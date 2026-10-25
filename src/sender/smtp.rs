//! 基于 SMTP 的邮件发送器（对应 C# `Pek.Mail.MailKit.MailKitEmailSender`）。
//!
//! - 使用 [`lettre`]（rustls + ring，纯 Rust、可 musl 交叉）；
//! - 每次发送按账号新建连接（与 C# 每次 `using var client` 一致）；
//! - 多账号：默认账号优先，失败自动切换下一个，全部失败返回聚合错误。

use std::sync::{Arc, RwLock};
use std::time::Duration;

use lettre::message::header::{ContentType, HeaderName, HeaderValue};
use lettre::message::{Attachment as LettreAttachment, Mailbox, Message, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{Address, SmtpTransport, Transport};

use crate::error::{AccountFailure, MailError, MailResult};
use crate::message::{DEFAULT_MAX_TOTAL_ATTACHMENT_BYTES, MailMessage, MailPriority};
use crate::sender::EmailSender;
use crate::settings::{MailData, MailSettings, SecureSocketOption};

/// 默认网络超时（连接/读/写）。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// SMTP 邮件发送器。
///
/// 配置通过 `Arc<RwLock<MailSettings>>` 共享：外部修改（面板保存/热加载）后无需重建发送器。
pub struct SmtpEmailSender {
    settings: Arc<RwLock<MailSettings>>,
    timeout: Duration,
}

impl SmtpEmailSender {
    /// 以配置构造（内部创建共享句柄）。
    pub fn new(settings: MailSettings) -> Self {
        Self {
            settings: Arc::new(RwLock::new(settings)),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// 以共享配置构造（与 [`crate::Mailer`] 共用同一份配置）。
    pub fn from_shared(settings: Arc<RwLock<MailSettings>>) -> Self {
        Self {
            settings,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// 设置网络超时（连接/读/写）。
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// 共享配置句柄（供热加载更新）。
    pub fn settings_handle(&self) -> Arc<RwLock<MailSettings>> {
        Arc::clone(&self.settings)
    }

    /// 当前配置快照。
    pub fn settings_snapshot(&self) -> MailSettings {
        match self.settings.read() {
            Ok(settings) => settings.clone(),
            Err(poisoned) => {
                static WARNED: std::sync::Once = std::sync::Once::new();
                WARNED.call_once(|| {
                    dhrust::warn!("邮件配置读锁已毒化，已恢复读取（请检查此前的 panic）");
                });
                poisoned.into_inner().clone()
            }
        }
    }

    /// 使用临时 SMTP 参数发送（对应 C# `Send(mail, Host, Port, UserName, Password, EnableSsl, fallback)`）。
    ///
    /// `fallback=true` 时：临时账号失败后自动改用配置中的已启用账号轮换发送。
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
        let settings = self.settings_snapshot();
        let mut from = message.from.clone();
        let mut from_name = message.from_name.clone();
        if from.trim().is_empty()
            && let Ok(default) = settings.find_default()
        {
            from = default.sender_address().to_string();
            from_name = default.from_name.clone();
        }

        let account = MailData {
            code: host.to_string(),
            host: host.to_string(),
            port,
            is_ssl: enable_ssl,
            user_name: user_name.to_string(),
            password: password.to_string(),
            from,
            from_name,
            is_enabled: true,
            is_default: false,
            email_suffix: String::new(),
        };

        match self.send_message(message, &account, &settings) {
            Ok(response) => Ok(response),
            Err(err) if fallback => {
                dhrust::warn!("临时 SMTP 账号发送失败，改用配置账号轮换：{err}");
                self.send_with_settings(message, settings.find_all_enabled(), &settings)
            }
            Err(err) => Err(err),
        }
    }

    /// 构建 MIME 消息；`from` / `from_name` / `reply_to` 由调用方覆盖，
    /// 避免为了改三个字段而深拷贝整封邮件（附件可能是数十 MB）。
    fn build_message(
        message: &MailMessage,
        from: &str,
        from_name: &str,
        reply_to: &[String],
        max_total_attachment_bytes: u64,
    ) -> MailResult<Message> {
        let from_address = from.trim();
        if from_address.is_empty() {
            return Err(MailError::Build(
                "发件人地址为空（请配置 From 或 UserName）".into(),
            ));
        }
        let from = parse_mailbox(from_address, from_name)?;

        let to = collect_mailboxes(&message.to, "收件人")?;
        if to.is_empty() {
            return Err(MailError::Build("收件人为空".into()));
        }
        let cc = collect_mailboxes(&message.cc, "抄送")?;
        let bcc = collect_mailboxes(&message.bcc, "密送")?;
        let reply_to = collect_mailboxes(reply_to, "回复地址")?;

        let mut builder = Message::builder()
            .from(from)
            .subject(message.subject.clone());
        for item in to {
            builder = builder.to(item);
        }
        for item in cc {
            builder = builder.cc(item);
        }
        for item in bcc {
            builder = builder.bcc(item);
        }
        for item in reply_to {
            builder = builder.reply_to(item);
        }

        for (name, value) in &message.headers {
            let header_name = HeaderName::new_from_ascii(name.to_string())
                .map_err(|_| MailError::Build(format!("自定义头名称不合法：{name}")))?;
            builder = builder.raw_header(HeaderValue::new(header_name, value.clone()));
        }

        // 优先级头（对齐 C# `MailMessage.Priority` → MailKit 头）
        match message.priority {
            MailPriority::Normal => {}
            MailPriority::High => {
                builder = add_raw_header(builder, "X-Priority", "2 (High)")?;
                builder = add_raw_header(builder, "Importance", "high")?;
            }
            MailPriority::Low => {
                builder = add_raw_header(builder, "X-Priority", "4 (Low)")?;
                builder = add_raw_header(builder, "Importance", "low")?;
            }
        }

        let body_part = if message.is_body_html {
            SinglePart::html(message.body.clone())
        } else {
            SinglePart::plain(message.body.clone())
        };

        if message.attachments.is_empty() {
            return builder
                .singlepart(body_part)
                .map_err(|e| MailError::Build(e.to_string()));
        }

        // 先按元数据/长度校验总量，避免多个附件叠加造成内存峰值；
        // 单附件仍由 Attachment::read 的 25MB 上限保护。
        let total_bytes = message.attachments.iter().try_fold(0u64, |total, item| {
            item.size().map(|size| total.saturating_add(size))
        })?;
        if total_bytes > max_total_attachment_bytes {
            return Err(MailError::Attachment(format!(
                "附件总大小 {} 字节超过上限 {} 字节",
                total_bytes, max_total_attachment_bytes
            )));
        }

        let mut mixed = MultiPart::mixed().singlepart(body_part);
        for attachment in &message.attachments {
            let bytes = attachment.read()?;
            let content_type = ContentType::parse(attachment.content_type()).unwrap_or_else(|_| {
                ContentType::parse("application/octet-stream").expect("固定 MIME")
            });
            mixed = mixed.singlepart(
                LettreAttachment::new(attachment.name().to_string()).body(bytes, content_type),
            );
        }
        builder
            .multipart(mixed)
            .map_err(|e| MailError::Build(e.to_string()))
    }

    /// 构建 SMTP 传输。
    fn build_transport(
        &self,
        settings: &MailSettings,
        account: &MailData,
    ) -> MailResult<SmtpTransport> {
        let host = account.host.trim();
        if host.is_empty() {
            return Err(MailError::Config("SMTP 服务器地址（Host）为空".into()));
        }
        if account.port == 0 {
            return Err(MailError::Config("SMTP 端口（Port）为 0".into()));
        }

        let builder = SmtpTransport::builder_dangerous(host)
            .port(account.port)
            .timeout(Some(self.timeout));

        let option = settings.socket_option();
        let tls_params = || {
            TlsParameters::new(host.to_string())
                .map_err(|e| MailError::Build(format!("TLS 参数构建失败：{e}")))
        };
        let builder = match option {
            SecureSocketOption::Auto if account.is_ssl => builder.tls(Tls::Wrapper(tls_params()?)),
            SecureSocketOption::Auto => builder.tls(Tls::Opportunistic(tls_params()?)),
            SecureSocketOption::None => builder.tls(Tls::None),
            SecureSocketOption::SslOnConnect => builder.tls(Tls::Wrapper(tls_params()?)),
            SecureSocketOption::StartTls => builder.tls(Tls::Required(tls_params()?)),
        };

        let builder = if !settings.use_default_credentials && !account.user_name.trim().is_empty() {
            builder.credentials(Credentials::new(
                account.user_name.clone(),
                account.password.clone(),
            ))
        } else {
            builder
        };

        Ok(builder.build())
    }

    /// 用指定账号发送（内部：已按账号设置发件人）。
    fn send_message(
        &self,
        message: &MailMessage,
        account: &MailData,
        settings: &MailSettings,
    ) -> MailResult<String> {
        // 与 C# 一致：每次用账号的 From 覆盖，并把 Reply-To 设为发件人。
        // 不克隆整封邮件：附件数据只在 build_message 里读取一次。
        let sender = account.sender_address().trim();
        if sender.is_empty() {
            return Err(MailError::Send {
                account: account.label().to_string(),
                error: "发件人地址为空（From / UserName 均未配置）".into(),
            });
        }
        let reply_to = [sender.to_string()];

        let email = Self::build_message(
            message,
            sender,
            &account.from_name,
            &reply_to,
            DEFAULT_MAX_TOTAL_ATTACHMENT_BYTES,
        )
        .map_err(|e| MailError::Send {
            account: account.label().to_string(),
            error: e.to_string(),
        })?;
        let transport = self
            .build_transport(settings, account)
            .map_err(|e| MailError::Send {
                account: account.label().to_string(),
                error: e.to_string(),
            })?;
        let response = transport.send(&email).map_err(|e| MailError::Send {
            account: account.label().to_string(),
            error: e.to_string(),
        })?;
        Ok(response.message().collect::<Vec<_>>().join("\n"))
    }

    /// 用同一份配置快照依次尝试账号（`send` / `send_with_accounts` 共用）。
    fn send_with_settings<'a, I>(
        &self,
        message: &MailMessage,
        accounts: I,
        settings: &MailSettings,
    ) -> MailResult<String>
    where
        I: IntoIterator<Item = &'a MailData>,
    {
        let mut failures = Vec::new();
        let mut attempts = 0usize;
        for account in accounts {
            attempts += 1;
            match self.send_message(message, account, settings) {
                Ok(response) => {
                    dhrust::info!(
                        "邮件发送成功：账号={} 主题={} 收件人={}",
                        account.label(),
                        message.subject,
                        message.to.first().map(String::as_str).unwrap_or("")
                    );
                    return Ok(response);
                }
                Err(err) => {
                    dhrust::warn!("{err}");
                    let error = match &err {
                        MailError::Send { error, .. } => error.clone(),
                        other => other.to_string(),
                    };
                    failures.push(AccountFailure {
                        code: account.label().to_string(),
                        host: account.host.clone(),
                        error,
                    });
                }
            }
        }

        if attempts == 0 {
            return Err(MailError::Config(
                "没有找到可用的邮箱配置，请检查 Mail.toml 中是否存在 IsEnabled=true 的邮箱账号"
                    .to_string(),
            ));
        }
        Err(MailError::AllFailed { attempts, failures })
    }

    /// 按收件人后缀路由，只快照一次配置（供 `Mailer::send_box_routed` 使用）。
    pub(crate) fn send_routed(&self, message: &MailMessage, address: &str) -> MailResult<String> {
        let settings = self.settings_snapshot();
        let account = settings.route_for(address)?;
        self.send_message(message, account, &settings)
    }
}

impl EmailSender for SmtpEmailSender {
    fn send(&self, message: &MailMessage) -> MailResult<String> {
        let settings = self.settings_snapshot();
        let accounts = settings.find_all_enabled();
        self.send_with_settings(message, accounts, &settings)
    }

    fn send_with_accounts(
        &self,
        message: &MailMessage,
        accounts: &[MailData],
    ) -> MailResult<String> {
        if accounts.is_empty() {
            return self.send(message);
        }
        let settings = self.settings_snapshot();
        self.send_with_settings(message, accounts.iter(), &settings)
    }

    fn send_with_account(&self, message: &MailMessage, account: &MailData) -> MailResult<String> {
        let settings = self.settings_snapshot();
        self.send_message(message, account, &settings)
    }
}

/// 解析单个邮箱地址（支持 `"昵称 <a@b.com>"` 形式）。
fn parse_mailbox(raw: &str, default_name: &str) -> MailResult<Mailbox> {
    let raw = raw.trim();
    let (name, address) = match (raw.rfind('<'), raw.ends_with('>')) {
        (Some(index), true) => {
            let name = raw[..index].trim().trim_matches('"').trim();
            let address = raw[index + 1..raw.len() - 1].trim();
            (Some(name).filter(|n| !n.is_empty()), address)
        }
        _ => (None, raw),
    };

    if !dhrust::text::is_email(address) {
        return Err(MailError::InvalidAddress(raw.to_string()));
    }
    let address: Address = address
        .parse()
        .map_err(|_| MailError::InvalidAddress(raw.to_string()))?;
    let name = name
        .map(str::to_string)
        .or_else(|| Some(default_name.trim().to_string()).filter(|n| !n.is_empty()));
    Ok(Mailbox::new(name, address))
}

/// 解析地址列表（每项允许用 `,` / `;` 继续分割，便于配置里一行写多个）。
fn collect_mailboxes(list: &[String], field: &str) -> MailResult<Vec<Mailbox>> {
    let mut out = Vec::new();
    for item in list {
        for part in dhrust::text::split_mail_addresses(item) {
            out.push(parse_mailbox(part, "").map_err(|e| match e {
                MailError::InvalidAddress(addr) => {
                    MailError::InvalidAddress(format!("{field}地址不合法：{addr}"))
                }
                other => other,
            })?);
        }
    }
    Ok(out)
}

fn add_raw_header(
    builder: lettre::message::MessageBuilder,
    name: &'static str,
    value: &'static str,
) -> MailResult<lettre::message::MessageBuilder> {
    let header_name = HeaderName::new_from_ascii(name.to_string())
        .map_err(|_| MailError::Build(format!("内置头名称不合法：{name}")))?;
    Ok(builder.raw_header(HeaderValue::new(header_name, value.to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Attachment;

    #[test]
    fn total_attachment_budget_is_enforced() {
        let message = MailMessage::new("to@example.com", "s", "b", false)
            .add_attachment(Attachment::from_bytes("a.txt", vec![0u8; 4]))
            .add_attachment(Attachment::from_bytes("b.txt", vec![0u8; 4]));
        let reply_to = ["from@example.com".to_string()];

        let error = SmtpEmailSender::build_message(&message, "from@example.com", "", &reply_to, 7)
            .unwrap_err();
        assert!(matches!(error, MailError::Attachment(_)), "{error}");

        assert!(
            SmtpEmailSender::build_message(&message, "from@example.com", "", &reply_to, 8,).is_ok()
        );
    }
}
