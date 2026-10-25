//! 邮件发送器（对应 C# `IEmailSender` / `EmailSenderBase` / `NullEmailSender`）。

mod null;
mod smtp;

pub use null::NullEmailSender;
pub use smtp::SmtpEmailSender;

use crate::error::{AccountFailure, MailError, MailResult};
use crate::message::{EmailBox, MailMessage};
use crate::settings::MailData;

/// 邮件发送器接口（对应 C# `Pek.Mail.Abstractions.IEmailSender`）。
///
/// 同步 API 为主：库不引入异步运行时；需要异步的消费方用线程池包装
/// （例如 `tokio::task::spawn_blocking`）。
pub trait EmailSender: Send + Sync {
    /// 发送邮件：使用配置中所有已启用账号，默认账号优先，失败自动轮换；
    /// 全部失败返回 [`MailError::AllFailed`]（对齐 C# `AggregateException`）。
    fn send(&self, message: &MailMessage) -> MailResult<String>;

    /// 发送 [`EmailBox`]（便捷方法）。
    fn send_box(&self, mail_box: &EmailBox) -> MailResult<String> {
        self.send(&MailMessage::from_box(mail_box))
    }

    /// 使用指定账号发送（失败直接返回 [`MailError::Send`]）。
    fn send_with_account(&self, message: &MailMessage, account: &MailData) -> MailResult<String>;

    /// 按 `accounts` 顺序依次尝试，成功即止；全部失败返回 [`MailError::AllFailed`]。
    /// `accounts` 为空时退回使用配置中所有已启用账号（对齐 C# `Send(box, accounts)`）。
    fn send_with_accounts(
        &self,
        message: &MailMessage,
        accounts: &[MailData],
    ) -> MailResult<String> {
        if accounts.is_empty() {
            return self.send(message);
        }

        let mut failures = Vec::new();
        for account in accounts {
            match self.send_with_account(message, account) {
                Ok(response) => return Ok(response),
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
        Err(MailError::AllFailed {
            attempts: accounts.len(),
            failures,
        })
    }

    /// 发送 [`EmailBox`]（指定账号列表）。
    fn send_box_with_accounts(
        &self,
        mail_box: &EmailBox,
        accounts: &[MailData],
    ) -> MailResult<String> {
        self.send_with_accounts(&MailMessage::from_box(mail_box), accounts)
    }
}
