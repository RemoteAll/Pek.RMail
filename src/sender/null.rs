//! 空邮件发送器（对应 C# `Pek.Mail.Core.NullEmailSender`）。
//!
//! 用于测试/禁用邮件能力：不连接任何服务器，同步发送返回 `"Fail"`。

use crate::error::MailResult;
use crate::message::MailMessage;
use crate::sender::EmailSender;
use crate::settings::MailData;

/// 空邮件发送器。
#[derive(Debug, Default, Clone, Copy)]
pub struct NullEmailSender;

impl EmailSender for NullEmailSender {
    fn send(&self, _message: &MailMessage) -> MailResult<String> {
        Ok("Fail".to_string())
    }

    fn send_with_account(&self, _message: &MailMessage, _account: &MailData) -> MailResult<String> {
        Ok("Fail".to_string())
    }
}
