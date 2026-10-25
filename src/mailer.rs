//! 邮件外观（Facade）：配置 + SMTP 发送 + 热加载，一站式的推荐入口。
//!
//! 对应 C# `Pek.Mail` 的 DI 用法（`IMailKitEmailSender` + `MailSettings.Current`），
//! 在 Rust 里收敛为一个可 `Arc` 共享的 [`Mailer`]。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;

use crate::error::{MailError, MailResult};
use crate::message::{EmailBox, MailMessage};
use crate::sender::{EmailSender, SmtpEmailSender};
use crate::settings::{MailData, MailSettings, MailSettingsFile};

/// 邮件外观：持有配置文件与共享配置快照，内部使用 [`SmtpEmailSender`]。
///
/// ```no_run
/// use pek_rmail::Mailer;
/// let mailer = Mailer::open_in("."); // 读取/生成 Config/Mail.toml
/// mailer.send("user@example.com", "欢迎注册", "<h1>欢迎</h1>", true)?;
/// # Ok::<(), pek_rmail::MailError>(())
/// ```
pub struct Mailer {
    /// 配置文件句柄（`None` = 纯内存配置，不读写磁盘）
    file: Option<Mutex<MailSettingsFile>>,
    settings: Arc<RwLock<MailSettings>>,
    sender: Arc<SmtpEmailSender>,
}

impl Mailer {
    /// 打开指定配置文件（不存在时按模板生成）。
    pub fn open(path: impl AsRef<Path>) -> Self {
        let file = MailSettingsFile::load(path);
        let settings = Arc::new(RwLock::new(file.settings().clone()));
        let sender = Arc::new(SmtpEmailSender::from_shared(Arc::clone(&settings)));
        Self {
            file: Some(Mutex::new(file)),
            settings,
            sender,
        }
    }

    /// 打开程序基目录下的 `Config/Mail.toml`。
    pub fn open_in(base: impl AsRef<Path>) -> Self {
        Self::open(MailSettings::default_path(base))
    }

    /// 直接以内存配置构造（不读写磁盘；测试或动态配置场景）。
    pub fn with_settings(settings: MailSettings) -> Self {
        let settings = Arc::new(RwLock::new(settings));
        let sender = Arc::new(SmtpEmailSender::from_shared(Arc::clone(&settings)));
        Self {
            file: None,
            settings,
            sender,
        }
    }

    /// 配置文件路径（纯内存模式返回 `None`）。
    pub fn config_path(&self) -> Option<PathBuf> {
        self.file.as_ref().map(|file| {
            file.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .path()
                .to_path_buf()
        })
    }

    /// 当前配置快照。
    pub fn settings(&self) -> MailSettings {
        self.settings.read().map(|s| s.clone()).unwrap_or_default()
    }

    /// 发送器句柄（需要直接使用底层 API 时）。
    pub fn sender(&self) -> Arc<SmtpEmailSender> {
        Arc::clone(&self.sender)
    }

    /// 加载期提示（配置生成/补齐/迁移等；纯内存模式为空）。
    pub fn take_notes(&self) -> Vec<String> {
        self.file
            .as_ref()
            .map(|file| {
                file.lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take_notes()
            })
            .unwrap_or_default()
    }

    /// 外部修改配置后热加载；返回提示文本（空 = 未变化/纯内存模式）。
    pub fn reload_if_changed(&self) -> Vec<String> {
        let Some(file_handle) = self.file.as_ref() else {
            return Vec::new();
        };
        let mut file = file_handle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let notes = file.reload_if_changed();
        if !notes.is_empty() {
            self.replace_settings(file.settings().clone());
            for note in &notes {
                dhrust::info!("邮件配置：{note}");
            }
        }
        notes
    }

    /// 保存新配置（写回 TOML，含 `[[Data]]`）并立即生效；纯内存模式只更新内存。
    pub fn save_settings(&self, settings: MailSettings) -> MailResult<()> {
        if let Some(file_handle) = self.file.as_ref() {
            let mut file = file_handle
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *file.settings_mut() = settings.clone();
            file.save()?;
        }
        self.replace_settings(settings);
        Ok(())
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
    /// 说明：C# 库只提供 `FindByCode` 手动路由；这里按 `EmailSuffix` 自动匹配，便于业务直接使用。
    pub fn send_box_routed(&self, box_: &EmailBox) -> MailResult<String> {
        let settings = self.settings();
        let first_to = box_
            .to
            .first()
            .ok_or_else(|| MailError::Build("收件人为空".into()))?;
        let account = settings.route_for(first_to)?;
        self.sender.send_with_account(&box_.to_message(), account)
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
        std::thread::spawn(move || this.send_box(&box_))
    }

    fn replace_settings(&self, settings: MailSettings) {
        if let Ok(mut guard) = self.settings.write() {
            *guard = settings;
        }
    }
}
