//! 邮件配置（对应 C# `Pek.Mail.MailSettings` / `MailData`）。
//!
//! - 配置一律 TOML：`Config/Mail.toml`，字段名与 C# `Config/Mail.config` 保持一致；
//! - 读写复用 [`dhrust::config::TomlConfig`]：缺失生成、缺键带注释补齐、类型容错、损坏备份重建、外部改动热加载；
//! - 数组表 `[[Data]]` 的写回复用 [`dhrust::config::toml::sync_array_of_tables`]（保留注释与排版）。

use std::path::{Path, PathBuf};

use dhrust::config::TomlConfig;
use serde::{Deserialize, Serialize};

use crate::error::{MailError, MailResult};

/// 默认配置相对路径（相对程序基目录）。
pub const MAIL_CONFIG_FILE: &str = "Config/Mail.toml";

/// 配置模板（内嵌；带全部字段中文注释）。
pub const MAIL_CONFIG_TEMPLATE: &str = include_str!("../res/Mail.toml.template");

/// 安全套接字选项（对应 MailKit `SecureSocketOptions` 的常用子集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SecureSocketOption {
    /// 自动：按账号 `IsSSL` 决定（true → 直接 SSL，false → STARTTLS 可用则升级）
    #[default]
    Auto,
    /// 不加密（仅信任的内网中继）
    None,
    /// 连接即 SSL（通常端口 465）
    SslOnConnect,
    /// 先明文连接再 STARTTLS 升级（通常端口 587）
    StartTls,
}

impl SecureSocketOption {
    /// 解析配置文本（空串按 [`SecureSocketOption::Auto`]；大小写与连字符不敏感）。
    pub fn parse(value: &str) -> Option<Self> {
        let text = value.trim().to_ascii_lowercase();
        match text.replace(['-', '_'], "").as_str() {
            "" | "auto" => Some(Self::Auto),
            "none" => Some(Self::None),
            "sslonconnect" | "ssl" => Some(Self::SslOnConnect),
            "starttls" => Some(Self::StartTls),
            _ => None,
        }
    }

    /// 转配置文本。
    pub fn as_str(self) -> &'static str {
        match self {
            SecureSocketOption::Auto => "Auto",
            SecureSocketOption::None => "None",
            SecureSocketOption::SslOnConnect => "SslOnConnect",
            SecureSocketOption::StartTls => "StartTls",
        }
    }
}

/// 邮箱账号（对应 C# `Pek.Mail.MailData`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "PascalCase")]
pub struct MailData {
    /// 惟一标识（未填写时日志用 Host 代替）
    pub code: String,
    /// SMTP 服务器
    pub host: String,
    /// SMTP 端口（默认 25）
    pub port: u16,
    /// 是否直接 SSL 连接（端口 465 通常为 true）
    #[serde(rename = "IsSSL")]
    pub is_ssl: bool,
    /// 身份验证用户名
    pub user_name: String,
    /// 身份验证密码（163/QQ 企业邮箱常用授权码）
    pub password: String,
    /// 发件人邮件地址（为空时回退到 `UserName`）
    pub from: String,
    /// 发件人昵称
    pub from_name: String,
    /// 是否启用（未启用账号不参与发送与 fallback）
    pub is_enabled: bool,
    /// 是否默认账号（默认优先，失败后按顺序轮换其余已启用账号）
    pub is_default: bool,
    /// 收件人邮箱后缀路由（多个用逗号分隔；命中时优先使用该账号）
    pub email_suffix: String,
}

impl Default for MailData {
    fn default() -> Self {
        Self {
            code: String::new(),
            host: String::new(),
            port: 25,
            is_ssl: false,
            user_name: String::new(),
            password: String::new(),
            from: String::new(),
            from_name: String::new(),
            is_enabled: false,
            is_default: false,
            email_suffix: String::new(),
        }
    }
}

impl MailData {
    /// 实际发件人地址（`From` 为空时回退 `UserName`）。
    pub fn sender_address(&self) -> &str {
        if !self.from.trim().is_empty() {
            &self.from
        } else {
            &self.user_name
        }
    }

    /// 日志/错误展示用的账号标识。
    pub fn label(&self) -> &str {
        if !self.code.trim().is_empty() {
            &self.code
        } else {
            &self.host
        }
    }

    /// 该账号的邮箱后缀是否命中给定收件人（大小写不敏感；`EmailSuffix` 多个用逗号/分号分隔）。
    pub fn matches_suffix(&self, address: &str) -> bool {
        let Some(suffix) = dhrust::text::email_suffix(address) else {
            return false;
        };
        let suffix = suffix.to_ascii_lowercase();
        self.email_suffix
            .split([',', ';', ' '])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .any(|s| s.trim_start_matches('@').eq_ignore_ascii_case(&suffix))
    }
}

/// 邮箱设置（对应 C# `Pek.Mail.MailSettings`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "PascalCase")]
pub struct MailSettings {
    /// 是否使用系统默认凭据（Rust 版为 true 时不做 SMTP AUTH）
    pub use_default_credentials: bool,
    /// 域名（Windows 域认证时填写）
    pub domain: String,
    /// 队列空闲等待间隔（毫秒；默认 3000）
    pub sleep_interval: u64,
    /// 安全套接字选项文本（空 / Auto / None / SslOnConnect / StartTls）
    pub secure_socket_option: String,
    /// 邮箱账号列表
    pub data: Vec<MailData>,
}

impl Default for MailSettings {
    fn default() -> Self {
        Self {
            use_default_credentials: false,
            domain: String::new(),
            sleep_interval: 3000,
            secure_socket_option: String::new(),
            data: Vec::new(),
        }
    }
}

impl MailSettings {
    /// 默认配置路径（`base/Config/Mail.toml`）。
    pub fn default_path(base: impl AsRef<Path>) -> PathBuf {
        base.as_ref().join(MAIL_CONFIG_FILE)
    }

    /// 加载配置（缺失时按内嵌模板生成，含一条示例账号）。
    pub fn load(path: impl AsRef<Path>) -> TomlConfig<MailSettings> {
        TomlConfig::load(path, MAIL_CONFIG_TEMPLATE)
    }

    /// 从程序基目录加载 `Config/Mail.toml`。
    pub fn load_from_base(base: impl AsRef<Path>) -> TomlConfig<MailSettings> {
        Self::load(Self::default_path(base))
    }

    /// 获取默认账号：默认且已启用 → 任意已启用 → 报错（对齐 C# `FindDefault`）。
    pub fn find_default(&self) -> MailResult<&MailData> {
        self.data
            .iter()
            .find(|e| e.is_default && e.is_enabled)
            .or_else(|| self.data.iter().find(|e| e.is_enabled))
            .ok_or_else(|| {
                MailError::Config(
                    "没有找到可用的邮箱配置，请检查 Mail.toml 中是否存在 IsEnabled=true 的邮箱账号"
                        .to_string(),
                )
            })
    }

    /// 所有已启用账号，默认账号排首位（对齐 C# `FindAllEnabled`）。
    pub fn find_all_enabled(&self) -> Vec<&MailData> {
        let mut list: Vec<&MailData> = self.data.iter().filter(|e| e.is_enabled).collect();
        list.sort_by_key(|e| !e.is_default); // stable：默认账号在前，其余保持原顺序
        list
    }

    /// 按 Code 查找账号（对齐 C# `FindByCode`；大小写不敏感）。
    pub fn find_by_code(&self, code: &str) -> Option<&MailData> {
        self.data.iter().find(|e| e.code.eq_ignore_ascii_case(code))
    }

    /// 按收件人邮箱后缀路由：返回第一个已启用且 `EmailSuffix` 命中的账号
    /// （C# 由调用方 `FindByCode` 手动路由；本库提供自动匹配便利方法）。
    pub fn find_by_suffix(&self, address: &str) -> Option<&MailData> {
        self.data
            .iter()
            .filter(|e| e.is_enabled)
            .find(|e| e.matches_suffix(address))
    }

    /// 路由账号：后缀命中优先，否则默认账号。
    pub fn route_for(&self, address: &str) -> MailResult<&MailData> {
        self.find_by_suffix(address)
            .or_else(|| self.find_default().ok())
            .ok_or_else(|| {
                MailError::Config(
                    "没有找到可用的邮箱配置，请检查 Mail.toml 中是否存在 IsEnabled=true 的邮箱账号"
                        .to_string(),
                )
            })
    }

    /// 解析安全套接字选项（非法值回退 Auto 并在日志中告警）。
    pub fn socket_option(&self) -> SecureSocketOption {
        match SecureSocketOption::parse(&self.secure_socket_option) {
            Some(option) => option,
            None => {
                dhrust::warn!(
                    "邮件配置 SecureSocketOption 无法识别：{}（按 Auto 处理）",
                    self.secure_socket_option
                );
                SecureSocketOption::Auto
            }
        }
    }

    /// 队列空闲等待间隔（毫秒）：0 或未配置按 3000（对齐 C# 默认）。
    pub fn sleep_interval_ms(&self) -> u64 {
        if self.sleep_interval == 0 {
            3000
        } else {
            self.sleep_interval
        }
    }
}

/// 配置文件句柄：持有 [`TomlConfig<MailSettings>`]，支持保存（含 `[[Data]]` 数组表）与热加载。
///
/// 用法：
/// ```no_run
/// use pek_rmail::MailSettingsFile;
/// let mut file = MailSettingsFile::load_from_base(".");
/// for note in file.take_notes() { dhrust::info!("{note}"); }
/// file.reload_if_changed();
/// ```
pub struct MailSettingsFile {
    config: TomlConfig<MailSettings>,
}

impl MailSettingsFile {
    /// 按路径加载。
    pub fn load(path: impl AsRef<Path>) -> Self {
        Self {
            config: MailSettings::load(path),
        }
    }

    /// 从程序基目录加载 `Config/Mail.toml`。
    pub fn load_from_base(base: impl AsRef<Path>) -> Self {
        Self {
            config: MailSettings::load_from_base(base),
        }
    }

    /// 配置路径。
    pub fn path(&self) -> &Path {
        self.config.path()
    }

    /// 当前配置。
    pub fn settings(&self) -> &MailSettings {
        self.config.value()
    }

    /// 当前配置（可变；修改后调用 [`Self::save`]）。
    pub fn settings_mut(&mut self) -> &mut MailSettings {
        self.config.value_mut()
    }

    /// 是否首次生成（或损坏后重建）。
    pub fn is_new(&self) -> bool {
        self.config.is_new()
    }

    /// 取走加载/保存期间的提示（含已生成/已补齐/类型修正等）。
    pub fn take_notes(&mut self) -> Vec<String> {
        self.config.take_notes()
    }

    /// 外部修改时热加载；返回提示文本（空 = 未变化）。
    pub fn reload_if_changed(&mut self) -> Vec<String> {
        self.config.reload_if_changed()
    }

    /// 保存配置（标量 + `[[Data]]` 数组表；保留注释与排版）。
    pub fn save(&mut self) -> MailResult<()> {
        let entries: Vec<serde_json::Value> = self
            .config
            .value()
            .data
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<_, _>>()
            .map_err(|e| MailError::Config(format!("序列化账号失败：{e}")))?;
        self.config
            .save_with_arrays(&[("Data", &entries)])
            .map_err(|e| MailError::Config(format!("保存邮件配置失败：{e}")))
    }
}
