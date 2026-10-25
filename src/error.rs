//! 错误类型（对齐 C# `Pek.Mail` 的异常口径，但用 `Result` 返回）。

use std::fmt;

/// 单个账号的发送失败信息（聚合错误用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountFailure {
    /// 账号 Code（未填写时用 Host 代替）
    pub code: String,
    /// SMTP 服务器
    pub host: String,
    /// 失败原因
    pub error: String,
}

impl fmt::Display for AccountFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}（{}）：{}", self.code, self.host, self.error)
    }
}

/// 邮件错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailError {
    /// 配置问题（无可用账号、配置读写失败等）
    Config(String),
    /// 邮箱地址格式不合法
    InvalidAddress(String),
    /// 附件读取/不存在
    Attachment(String),
    /// 构建邮件失败（MIME/头部等）
    Build(String),
    /// 单个指定账号发送失败
    Send { account: String, error: String },
    /// 所有已启用账号均发送失败（对齐 C# `AggregateException`）
    AllFailed {
        attempts: usize,
        failures: Vec<AccountFailure>,
    },
    /// 队列/线程状态问题
    Queue(String),
}

impl fmt::Display for MailError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MailError::Config(msg) => write!(f, "邮件配置错误：{msg}"),
            MailError::InvalidAddress(addr) => write!(f, "邮箱地址不合法：{addr}"),
            MailError::Attachment(msg) => write!(f, "附件错误：{msg}"),
            MailError::Build(msg) => write!(f, "邮件构建失败：{msg}"),
            MailError::Send { account, error } => write!(f, "邮箱账号 {account} 发送失败：{error}"),
            MailError::AllFailed { attempts, failures } => {
                write!(f, "所有 {attempts} 个启用的邮箱账号均发送失败")?;
                for (i, item) in failures.iter().enumerate() {
                    write!(f, "{}#{i}: {item}", if i == 0 { "：" } else { "；" })?;
                }
                Ok(())
            }
            MailError::Queue(msg) => write!(f, "邮件队列错误：{msg}"),
        }
    }
}

impl std::error::Error for MailError {}

/// 邮件操作结果。
pub type MailResult<T> = Result<T, MailError>;
