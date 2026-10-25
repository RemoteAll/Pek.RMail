//! 邮件外观（Facade）：配置 + SMTP 发送 + 热加载，一站式的推荐入口。
//!
//! 对应 C# `Pek.Mail` 的 DI 用法（`IMailKitEmailSender` + `MailSettings.Current`），
//! 在 Rust 里收敛为一个可 `Arc` 共享的 [`Mailer`]。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

use dhrust::config::TomlConfigStore;

use crate::error::{MailError, MailResult};
use crate::message::{EmailBox, MailMessage};
use crate::sender::{EmailSender, SmtpEmailSender};
use crate::settings::{MAIL_CONFIG_TEMPLATE, MailData, MailSettings};

/// 邮件外观：持有配置文件与共享配置快照，内部使用 [`SmtpEmailSender`]。
///
/// ```no_run
/// use pek_rmail::Mailer;
/// let mailer = Mailer::open_in("."); // 读取/生成 Config/Mail.toml
/// mailer.send("user@example.com", "欢迎注册", "<h1>欢迎</h1>", true)?;
/// # Ok::<(), pek_rmail::MailError>(())
/// ```
pub struct Mailer {
    /// 配置文件句柄 + 共享配置快照（纯内存模式 = 不读写磁盘）
    store: TomlConfigStore<MailSettings>,
    sender: Arc<SmtpEmailSender>,
}

impl Mailer {
    /// 打开指定配置文件（不存在时按模板生成）。
    pub fn open(path: impl AsRef<Path>) -> Self {
        let store = TomlConfigStore::open(path, MAIL_CONFIG_TEMPLATE);
        let sender = Arc::new(SmtpEmailSender::from_shared(store.shared()));
        Self { store, sender }
    }

    /// 打开程序基目录下的 `Config/Mail.toml`。
    pub fn open_in(base: impl AsRef<Path>) -> Self {
        Self::open(MailSettings::default_path(base))
    }

    /// 直接以内存配置构造（不读写磁盘；测试或动态配置场景）。
    pub fn with_settings(settings: MailSettings) -> Self {
        let store = TomlConfigStore::in_memory(settings);
        let sender = Arc::new(SmtpEmailSender::from_shared(store.shared()));
        Self { store, sender }
    }

    /// 配置文件路径（纯内存模式返回 `None`）。
    pub fn config_path(&self) -> Option<PathBuf> {
        self.store.path().map(Path::to_path_buf)
    }

    /// 当前配置快照。
    pub fn settings(&self) -> MailSettings {
        self.store.value()
    }

    /// 发送器句柄（需要直接使用底层 API 时）。
    pub fn sender(&self) -> Arc<SmtpEmailSender> {
        Arc::clone(&self.sender)
    }

    /// 加载期提示（配置生成/补齐/迁移等；纯内存模式为空）。
    pub fn take_notes(&self) -> Vec<String> {
        self.store.take_notes()
    }

    /// 外部修改配置后热加载；返回提示文本（空 = 未变化/纯内存模式）。
    ///
    /// 注意：本方法每次都会读取文件元数据（Windows 实测约 49µs），请按秒级/文件监听调用，
    /// 不要放在每封邮件的发送路径上。
    pub fn reload_if_changed(&self) -> Vec<String> {
        let notes = self.store.reload_if_changed();
        for note in &notes {
            dhrust::info!("邮件配置：{note}");
        }
        notes
    }

    /// 保存新配置（写回 TOML，含 `[[Data]]`）并立即生效；纯内存模式只更新内存。
    pub fn save_settings(&self, settings: MailSettings) -> MailResult<()> {
        let entries: Vec<serde_json::Value> = settings
            .data
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<_, _>>()
            .map_err(|error| MailError::Config(format!("序列化账号失败：{error}")))?;
        self.store
            .save(settings, &[("Data", &entries)])
            .map_err(|error| MailError::Config(format!("保存邮件配置失败：{error}")))
    }

    /// 简单发送（HTML 正文）。
    pub fn send(
        &self,
        to: impl Into<String>,
        subject: impl Into<String>,
        body: impl Into<String>,
        is_body_html: bool,
    ) -> MailResult<String> {
        let message = MailMessage::new(to, subject, body, is_body_html);
        self.sender.send(&message)
    }

    /// 指定发件人发送（发件人仍会被配置账号覆盖，见 [`MailMessage`] 说明）。
    pub fn send_from(
        &self,
        from: impl Into<String>,
        from_name: impl Into<String>,
        to: impl Into<String>,
        subject: impl Into<String>,
        body: impl Into<String>,
        is_body_html: bool,
    ) -> MailResult<String> {
        let message = MailMessage::new(to, subject, body, is_body_html).with_from(from, from_name);
        self.sender.send(&message)
    }

    /// 发送 [`EmailBox`]（使用全部已启用账号，默认优先 + 失败轮换）。
    pub fn send_box(&self, box_: &EmailBox) -> MailResult<String> {
        self.sender.send_box(box_)
    }

    /// 发送 [`EmailBox`]（指定账号列表，空列表 = 全部已启用）。
    pub fn send_box_with_accounts(
        &self,
        box_: &EmailBox,
        accounts: &[MailData],
    ) -> MailResult<String> {
        self.sender.send_box_with_accounts(box_, accounts)
    }

    /// 按收件人后缀路由发送：`EmailSuffix` 命中优先，否则默认账号。
    ///
    /// 仅当**所有收件人邮箱后缀一致**时才自动路由；多后缀混发请用
    /// [`Self::send_box`] 或 [`Self::send_box_with_accounts`] 明确账号，避免误用单账号。
    /// 说明：C# 库只提供 `FindByCode` 手动路由；这里按 `EmailSuffix` 自动匹配，便于业务直接使用。
    pub fn send_box_routed(&self, box_: &EmailBox) -> MailResult<String> {
        let first_to = box_
            .to
            .first()
            .ok_or_else(|| MailError::Build("收件人为空".into()))?;
        let suffix = dhrust::text::email_suffix(first_to);
        if !box_
            .to
            .iter()
            .all(|to| dhrust::text::email_suffix(to) == suffix)
        {
            return Err(MailError::Build(
                "收件人邮箱后缀不一致，无法按后缀路由；请使用 send_box 或指定账号".into(),
            ));
        }
        self.sender.send_routed(&box_.to_message(), first_to)
    }

    /// 临时 SMTP 参数发送（对应 C# `Send(mail, Host, Port, ...)`）。
    #[allow(clippy::too_many_arguments)]
    pub fn send_temporary(
        &self,
        message: &MailMessage,
        host: &str,
        port: u16,
        user_name: &str,
        password: &str,
        enable_ssl: bool,
        fallback: bool,
    ) -> MailResult<String> {
        self.sender.send_temporary(
            message, host, port, user_name, password, enable_ssl, fallback,
        )
    }

    /// 异步发送 [`EmailBox`]：在后台线程执行同步发送（库不引入异步运行时）。
    pub fn send_box_async(self: &Arc<Self>, box_: EmailBox) -> JoinHandle<MailResult<String>> {
        let this = Arc::clone(self);
        dhrust::threading::spawn_named("pek-rmail-send", move || this.send_box(&box_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_if_changed_refreshes_shared_settings() {
        let dir = std::env::temp_dir().join(format!("pek-rmail-reload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mailer = Mailer::open_in(&dir);
        let path = mailer.config_path().expect("文件模式");
        let text = std::fs::read_to_string(&path).expect("读配置");
        let updated = text.replace("SleepInterval = 3000", "SleepInterval = 43210");
        assert_ne!(text, updated, "模板应包含 SleepInterval");
        std::fs::write(&path, updated).expect("写外部修改");

        let notes = mailer.reload_if_changed();
        assert!(
            notes.iter().any(|note| note.contains("热加载")),
            "{notes:?}"
        );
        assert_eq!(mailer.settings().sleep_interval, 43210);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn routed_send_rejects_mixed_suffixes() {
        let mailer = Mailer::with_settings(MailSettings::default());
        let mail_box = EmailBox::new("a@example.com", "s", "b").to("b@other.com");
        let error = mailer.send_box_routed(&mail_box).unwrap_err();
        assert!(matches!(error, MailError::Build(_)), "{error}");
    }
}
