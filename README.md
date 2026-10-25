# Pek.RMail

C# [`Pek.Mail`](https://github.com/PeiKeSmart/Pek.AspNetCore) 的 Rust 实现：**邮件发送库**（非服务、无面板），
供生态内其他 Rust 项目以 path 依赖复用。

- crate：`pek-rmail`
- 参照源码：`G:\Code\Pek.FrameWork\Pek.Mail`
- 短信能力后续另起项目，**不在本仓实现**

---

## 1. 能力对照（C# → Rust）

| C# | Rust |
|---|---|
| `MailSettings` / `MailData` | `MailSettings` / `MailData` |
| 多账号：默认优先 + 失败轮换（`FindAllEnabled`） | `MailSettings::find_all_enabled` + `SmtpEmailSender` 自动 fallback |
| 邮箱后缀路由（`EmailSuffix` + `FindByCode`） | `MailSettings::find_by_suffix` / `route_for` / `Mailer::send_box_routed` |
| `EmailBox`（多收件人/抄送/密送/HTML/附件） | `EmailBox` |
| `System.Net.Mail.MailMessage` 高级发送 | `MailMessage`（From/ReplyTo/优先级/自定义头） |
| `PhysicalFileAttachment` / `MemoryStreamAttachment` | `Attachment::from_file` / `Attachment::from_bytes` / `from_reader` |
| `MailKitEmailSender` / `SmtpEmailSender` | `SmtpEmailSender`（`lettre` + rustls + ring） |
| `NullEmailSender` | `NullEmailSender` |
| `MailQueueProvider` / `MailQueueService` / `MailQueueManager` | 同名三件套（后台线程 + 入队即唤醒） |
| `SendMailController`（Extensions） | 不提供：由消费方面板/接口自行暴露；本库提供 `send_temporary` 供“测试发送”使用 |

## 2. 配置（`Config/Mail.toml`）

首次运行自动按内嵌模板生成（带中文注释）；字段名与 C# `Config/Mail.config` 保持一致：

```toml
UseDefaultCredentials = false
Domain = ""
SleepInterval = 3000
SecureSocketOption = ""   # 空=按账号 IsSSL 自动；Auto/None/SslOnConnect/StartTls

[[Data]]
Code = "account-001"
Host = "smtp.exmail.qq.com"
Port = 465
IsSSL = true
UserName = "noreply@example.com"
Password = "your_password"
From = "noreply@example.com"
FromName = "系统通知"
IsEnabled = true
IsDefault = true
EmailSuffix = ""
```

配置读写复用 `dhrust::config::TomlConfig`：缺失生成、缺键带注释补齐、类型容错、损坏备份重建、外部修改热加载；
`[[Data]]` 账号数组写回复用 `TomlConfig::save_with_arrays`（保留注释与排版）。
配置里密码为明文（与 C# 一致）；需要加密保存时由消费方用 `dhrust::secret` 处理。

## 3. 快速开始

```rust
use pek_rmail::{EmailBox, Mailer};

let mailer = Mailer::open_in("."); // 读取/生成 Config/Mail.toml
for note in mailer.take_notes() {
    dhrust::info!("{note}");
}

// 简单发送（HTML）
mailer.send("user@example.com", "欢迎注册", "<h1>欢迎</h1>", true)?;

// 富文本 + 抄送 + 附件
let mail_box = EmailBox::new("user@example.com", "月度报表", "<p>请查收附件。</p>")
    .cc("boss@example.com")
    .attach(pek_rmail::Attachment::from_bytes("report.csv", b"a,b\n1,2".to_vec()));
mailer.send_box(&mail_box)?;
```

## 4. 多账号与路由

- **默认优先**：`IsDefault=true && IsEnabled=true` 的账号先发；
- **失败轮换**：其余已启用账号按配置顺序依次尝试，全部失败返回 `MailError::AllFailed`（对齐 C# `AggregateException`）；
- **后缀路由**：`EmailSuffix`（多个用逗号分隔，如 `corp.cn,example.org`）命中的账号优先：
  `mailer.send_box_routed(&mail_box)`；也可用 `settings().route_for(address)` 手工选择；
- **指定账号**：`send_box_with_accounts(&box, &[account1, account2])`；
- **临时账号**：`send_temporary(&message, host, port, user, pass, ssl, fallback)`（对应 C# 的测试发送接口）。

## 5. 发送队列

```rust
use std::sync::Arc;
use pek_rmail::{EmailBox, MailQueueManager, MailQueueProvider, MailQueueService, SmtpEmailSender};

let provider = Arc::new(MailQueueProvider::new());
let service = MailQueueService::new(provider.clone());
let sender = Arc::new(SmtpEmailSender::new(mailer.settings()));
let manager = MailQueueManager::with_settings(provider, sender, &mailer.settings());
manager.start()?;

service.enqueue(EmailBox::new("user@example.com", "异步通知", "<p>稍后发送</p>"));
// ...
manager.stop();
```

与 C# 的差异（有意）：空闲时用 `Condvar` 等待而不是固定 `Thread.Sleep`，**入队立即唤醒**，
空闲时不产生周期唤醒开销（对齐“非必要不做周期性劳作”的资源约定）。

## 6. 与 C# 的差异（有意）

| 项目 | C# | Rust |
|---|---|---|
| 异步 API | `SendAsync` / MailKit 异步 | 同步为主；`Mailer::send_box_async` 用后台线程（库不引入 tokio 运行时） |
| `MailMessage` | `System.Net.Mail.MailMessage` | `MailMessage` 常用子集（From/ReplyTo/优先级/自定义头/附件） |
| 附件文件名编码 | 手动 GB18030 + RFC2047 | `lettre` 自动处理（UTF-8 / RFC 2231），更符合现代邮件客户端 |
| `UseDefaultCredentials` / `Domain` | Windows 域账号 | `UseDefaultCredentials=true` 时不做 SMTP AUTH；`Domain` 仅保留字段 |
| 队列空闲 | 固定睡眠 `SleepInterval` | 入队立即唤醒（空闲零周期开销） |
| 验证码业务（`SendMailController`） | `Pek.Mail.Extensions` | 不在库内：验证码生成/校验属于业务，消费方自行实现 |

## 7. 设计约束

- **不新增异步运行时**：库内同步；消费方需要异步时用 `spawn_blocking` 或 `Mailer::send_box_async`；
- **日志**：复用 `dhrust::logs`，只记发送成功/失败/账号切换，不逐轮询打印；
- **基础工具下沉 `DH.RustBase`**：
  - `dhrust::text::is_email`（对齐 C# `ValidFormatHelper.IsEmail`）
  - `dhrust::text::email_suffix`（对齐 C# `MailHelper.GetEmailSuffix`）
  - `dhrust::text::split_mail_addresses`（按 `,`/`;` 拆分）
  - `dhrust::config::TomlConfig::save_with_arrays`（同步 `[[数组表]]` 并保留注释）
- **SMTP/MIME**：使用 `lettre`（rustls + ring，纯 Rust，可 zig 交叉编 musl），不自研 MIME/认证。

## 8. 开发与回归

```powershell
cargo check
cargo test
cargo clippy --all-targets
```

测试不依赖外网：`tests/mail_e2e.rs` 内置假 SMTP 服务器，覆盖 HTML/附件/多账号 fallback/队列/配置回写。
