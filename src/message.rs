//! 邮件对象与附件（对应 C# `Pek.Mail.Core.EmailBox`、`Attachments.*`）。

use std::path::{Path, PathBuf};

use crate::error::{MailError, MailResult};

/// 默认附件大小上限（25MB）。
pub const DEFAULT_MAX_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;

/// 默认单封邮件附件总大小上限（50MB）。
///
/// 单附件上限之外再加总量上限，防止“多个 25MB 附件”叠加造成内存峰值；
/// 常见 SMTP 服务商（Gmail/QQ/企业邮箱）的总量限制通常为 25~50MB。
pub const DEFAULT_MAX_TOTAL_ATTACHMENT_BYTES: u64 = 50 * 1024 * 1024;

/// 附件来源（对应 C# `PhysicalFileAttachment` / `MemoryStreamAttachment`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentSource {
    /// 物理文件（发送时读取，构造时校验存在）
    File(PathBuf),
    /// 内存数据（动态生成的报表等）
    Memory(Vec<u8>),
}

/// 邮件附件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    name: String,
    source: AttachmentSource,
    content_type: String,
}

impl Attachment {
    /// 物理文件附件（对齐 C# `PhysicalFileAttachment`：文件不存在时立即报错）。
    pub fn from_file(path: impl AsRef<Path>) -> MailResult<Self> {
        let path = path.as_ref();
        if !path.is_file() {
            return Err(MailError::Attachment(format!(
                "文件未找到：{}",
                path.display()
            )));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "attachment".to_string());
        Ok(Self {
            name,
            source: AttachmentSource::File(path.to_path_buf()),
            content_type: "application/octet-stream".to_string(),
        })
    }

    /// 内存数据附件（对齐 C# `MemoryStreamAttachment`）。
    pub fn from_bytes(name: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name.into(),
            source: AttachmentSource::Memory(bytes.into()),
            content_type: "application/octet-stream".to_string(),
        }
    }

    /// 从读取器读入内存作为附件（适合动态生成内容；默认上限 25MB）。
    pub fn from_reader(name: impl Into<String>, reader: impl std::io::Read) -> MailResult<Self> {
        Self::from_reader_limited(name, reader, DEFAULT_MAX_ATTACHMENT_BYTES)
    }

    /// 从读取器读入内存作为附件，超过 `max_bytes` 报错。
    pub fn from_reader_limited(
        name: impl Into<String>,
        reader: impl std::io::Read,
        max_bytes: u64,
    ) -> MailResult<Self> {
        use std::io::Read;

        let mut bytes = Vec::new();
        reader
            .take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| MailError::Attachment(format!("读取附件数据失败：{e}")))?;
        if bytes.len() as u64 > max_bytes {
            return Err(MailError::Attachment(format!(
                "附件 {} 超过上限 {} 字节",
                name.into(),
                max_bytes
            )));
        }
        Ok(Self::from_bytes(name, bytes))
    }

    /// 指定 MIME 类型（如 `text/csv`、`application/pdf`）；默认 `application/octet-stream`。
    pub fn with_content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = content_type.into();
        self
    }

    /// 附件文件名。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// MIME 类型。
    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    /// 来源。
    pub fn source(&self) -> &AttachmentSource {
        &self.source
    }

    /// 读取附件内容（默认上限 25MB）。
    pub fn read(&self) -> MailResult<Vec<u8>> {
        self.read_limited(DEFAULT_MAX_ATTACHMENT_BYTES)
    }

    /// 附件大小（文件取元数据、内存取长度），供发送前总量校验。
    pub fn size(&self) -> MailResult<u64> {
        match &self.source {
            AttachmentSource::File(path) => {
                std::fs::metadata(path).map(|meta| meta.len()).map_err(|e| {
                    MailError::Attachment(format!("读取附件 {} 信息失败：{e}", path.display()))
                })
            }
            AttachmentSource::Memory(bytes) => Ok(bytes.len() as u64),
        }
    }

    /// 读取附件内容，超过 `max_bytes` 报错。
    pub fn read_limited(&self, max_bytes: u64) -> MailResult<Vec<u8>> {
        match &self.source {
            AttachmentSource::File(path) => {
                dhrust::io::read_file_limited(path, max_bytes).map_err(|e| {
                    MailError::Attachment(format!("读取附件 {} 失败：{e}", path.display()))
                })
            }
            AttachmentSource::Memory(bytes) => {
                if bytes.len() as u64 > max_bytes {
                    return Err(MailError::Attachment(format!(
                        "附件 {} 超过上限 {} 字节",
                        self.name, max_bytes
                    )));
                }
                Ok(bytes.clone())
            }
        }
    }
}

/// 邮件优先级（对应 C# `MailPriority`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MailPriority {
    /// 低
    Low,
    /// 普通（默认）
    #[default]
    Normal,
    /// 高
    High,
}

/// 电子邮件（对应 C# `Pek.Mail.Core.EmailBox`；推荐使用的邮件对象）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailBox {
    /// 附件列表
    pub attachments: Vec<Attachment>,
    /// 正文
    pub body: String,
    /// 抄送人
    pub cc: Vec<String>,
    /// 是否 HTML 内容（默认 true，对齐 C#）
    pub is_body_html: bool,
    /// 主题
    pub subject: String,
    /// 收件人
    pub to: Vec<String>,
    /// 密送人
    pub bcc: Vec<String>,
}

impl Default for EmailBox {
    fn default() -> Self {
        Self {
            attachments: Vec::new(),
            body: String::new(),
            cc: Vec::new(),
            is_body_html: true,
            subject: String::new(),
            to: Vec::new(),
            bcc: Vec::new(),
        }
    }
}

impl EmailBox {
    /// 新建（默认 HTML 正文，与 C# 一致）。
    pub fn new(to: impl Into<String>, subject: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            to: vec![to.into()],
            subject: subject.into(),
            body: body.into(),
            ..Self::default()
        }
    }

    /// 纯文本正文。
    pub fn plain(mut self) -> Self {
        self.is_body_html = false;
        self
    }

    /// 追加收件人。
    pub fn to(mut self, address: impl Into<String>) -> Self {
        self.to.push(address.into());
        self
    }

    /// 追加抄送。
    pub fn cc(mut self, address: impl Into<String>) -> Self {
        self.cc.push(address.into());
        self
    }

    /// 追加密送。
    pub fn bcc(mut self, address: impl Into<String>) -> Self {
        self.bcc.push(address.into());
        self
    }

    /// 追加附件。
    pub fn attach(mut self, attachment: Attachment) -> Self {
        self.attachments.push(attachment);
        self
    }

    /// 转为高级邮件对象（`MailMessage`）。
    pub fn to_message(&self) -> MailMessage {
        MailMessage::from_box(self)
    }
}

/// 高级邮件消息（对应 C# `System.Net.Mail.MailMessage` 的常用子集）。
///
/// 与 [`EmailBox`] 的差异：可指定发件人/回复地址/优先级/自定义头。
/// 使用配置账号发送时，发件人会被账号的 `From` 覆盖（对齐 C# `MailKitEmailSender` 行为）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailMessage {
    /// 发件人地址（为空时由账号或默认配置填充）
    pub from: String,
    /// 发件人昵称
    pub from_name: String,
    /// 回复地址（为空时不设置）
    pub reply_to: Vec<String>,
    /// 收件人
    pub to: Vec<String>,
    /// 抄送
    pub cc: Vec<String>,
    /// 密送
    pub bcc: Vec<String>,
    /// 主题
    pub subject: String,
    /// 正文
    pub body: String,
    /// 是否 HTML 正文
    pub is_body_html: bool,
    /// 优先级
    pub priority: MailPriority,
    /// 自定义头（原样设置，后写覆盖同名）
    pub headers: Vec<(String, String)>,
    /// 附件
    pub attachments: Vec<Attachment>,
}

impl Default for MailMessage {
    fn default() -> Self {
        Self {
            from: String::new(),
            from_name: String::new(),
            reply_to: Vec::new(),
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: String::new(),
            body: String::new(),
            is_body_html: true,
            priority: MailPriority::Normal,
            headers: Vec::new(),
            attachments: Vec::new(),
        }
    }
}

impl MailMessage {
    /// 简单构造（收件人 + 主题 + 正文）。
    pub fn new(
        to: impl Into<String>,
        subject: impl Into<String>,
        body: impl Into<String>,
        is_body_html: bool,
    ) -> Self {
        Self {
            to: vec![to.into()],
            subject: subject.into(),
            body: body.into(),
            is_body_html,
            ..Self::default()
        }
    }

    /// 指定发件人。
    pub fn with_from(mut self, from: impl Into<String>, from_name: impl Into<String>) -> Self {
        self.from = from.into();
        self.from_name = from_name.into();
        self
    }

    /// 追加收件人。
    pub fn add_to(mut self, address: impl Into<String>) -> Self {
        self.to.push(address.into());
        self
    }

    /// 追加抄送。
    pub fn add_cc(mut self, address: impl Into<String>) -> Self {
        self.cc.push(address.into());
        self
    }

    /// 追加密送。
    pub fn add_bcc(mut self, address: impl Into<String>) -> Self {
        self.bcc.push(address.into());
        self
    }

    /// 追加附件。
    pub fn add_attachment(mut self, attachment: Attachment) -> Self {
        self.attachments.push(attachment);
        self
    }

    /// 由 [`EmailBox`] 转换。
    pub fn from_box(mail_box: &EmailBox) -> Self {
        Self {
            to: mail_box.to.clone(),
            cc: mail_box.cc.clone(),
            bcc: mail_box.bcc.clone(),
            subject: mail_box.subject.clone(),
            body: mail_box.body.clone(),
            is_body_html: mail_box.is_body_html,
            attachments: mail_box.attachments.clone(),
            ..Self::default()
        }
    }

    /// 设置单个纯邮箱地址（不做显示名校验）。
    pub fn set_reply_to(mut self, address: impl Into<String>) -> Self {
        self.reply_to.push(address.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_size_limit_is_enforced() {
        let attachment = Attachment::from_bytes("a.bin", vec![1u8, 2, 3, 4]);
        assert_eq!(attachment.read_limited(4).unwrap(), vec![1, 2, 3, 4]);
        assert!(attachment.read_limited(3).is_err());

        let reader = std::io::Cursor::new(vec![1u8, 2, 3, 4]);
        assert!(Attachment::from_reader_limited("b.bin", reader, 3).is_err());
    }
}
