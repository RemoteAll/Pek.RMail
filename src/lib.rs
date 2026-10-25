//! # Pek.RMail（crate `pek-rmail`）
//!
//! C# `Pek.Mail` 的 Rust 对应实现：**邮件发送库**（非服务、无面板），供生态内其他 Rust 项目以 path 依赖复用。
//!
//! ## 能力对照（C# → Rust）
//!
//! | C# | Rust |
//! |---|---|
//! | `MailSettings` / `MailData`（多账号、默认优先、失败轮换、邮箱后缀路由） | [`MailSettings`] / [`MailData`] |
//! | `EmailBox`（多收件人/抄送/密送/HTML/附件） | [`EmailBox`] |
//! | `System.Net.Mail.MailMessage` 高级发送 | [`MailMessage`] |
//! | `PhysicalFileAttachment` / `MemoryStreamAttachment` | [`Attachment::from_file`] / [`Attachment::from_bytes`] |
//! | `MailKitEmailSender` / `SmtpEmailSender` | [`SmtpEmailSender`]（lettre + rustls） |
//! | `NullEmailSender` | [`NullEmailSender`] |
//! | `MailQueueProvider` / `MailQueueService` / `MailQueueManager` | [`MailQueueProvider`] / [`MailQueueService`] / [`MailQueueManager`] |
//!
//! ## 快速开始
//!
//! ```no_run
//! use pek_rmail::{EmailBox, Mailer};
//!
//! let mailer = Mailer::open_in("."); // 读取/生成 Config/Mail.toml
//! for note in mailer.take_notes() {
//!     dhrust::info!("{note}");
//! }
//!
//! // 简单发送（使用配置中的默认账号，失败自动轮换）
//! mailer.send("user@example.com", "欢迎注册", "<h1>欢迎</h1>", true)?;
//!
//! // 富文本 + 抄送 + 附件
//! let mut box_ = EmailBox::new("user@example.com", "月度报表", "<p>请查收附件。</p>")
//!     .cc("boss@example.com");
//! box_ = box_.attach(pek_rmail::Attachment::from_bytes("report.csv", b"a,b\n1,2".to_vec()));
//! mailer.send_box(&box_)?;
//! # Ok::<(), pek_rmail::MailError>(())
//! ```
//!
//! ## 设计约束
//!
//! - 配置一律 TOML（`Config/Mail.toml`）：复用 `dhrust::config::TomlConfig`（缺失生成/补齐/容错/热加载/备份）；
//! - 不引入异步运行时：同步 API 为主；需要异步时用消费方线程池包装（[`Mailer::send_box_async`] 是后台线程）；
//! - 日志复用 `dhrust::logs`：只记录发送成功/失败/账号切换，不逐轮询打印；
//! - 基础校验复用 `dhrust::text`（`is_email` / `email_suffix` / `split_mail_addresses`）。

pub mod error;
pub mod mailer;
pub mod message;
pub mod queue;
pub mod sender;
pub mod settings;

pub use error::{AccountFailure, MailError, MailResult};
pub use mailer::Mailer;
pub use message::{Attachment, AttachmentSource, EmailBox, MailMessage, MailPriority};
pub use queue::{DEFAULT_SLEEP_INTERVAL_MS, MailQueueManager, MailQueueProvider, MailQueueService};
pub use sender::{EmailSender, NullEmailSender, SmtpEmailSender};
pub use settings::{
    MAIL_CONFIG_FILE, MAIL_CONFIG_TEMPLATE, MailData, MailSettings, MailSettingsFile,
    SecureSocketOption,
};
