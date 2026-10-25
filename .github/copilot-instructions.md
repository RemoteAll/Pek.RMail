# Pek.RMail · 项目指令

> 全项目通用规则见用户级《跨项目接续协议》（本机 `%APPDATA%\Code\User\prompts\跨项目接续协议.instructions.md`；模板在 `tools/continuity/templates/`）。
> **开工先读（存在则读，按序）**：`.github/copilot-instructions.md` → `docs/项目接续指南.md` → `docs/用户档案.md` → `docs/项目记忆.md` → `docs/决策记录.md` 最新节。
> **机器自愈（有则快跑）**：`tools/continuity/install-global-instruction.ps1`（幂等；打开项目时 `.vscode/tasks.json` 也会自动跑）。

## 项目定位

`Pek.RMail`（crate `pek-rmail`）是 C# `Pek.Mail` 的 Rust 对应实现：**邮件发送库**（非服务、无面板），
供生态内其他 Rust 项目以 path 依赖复用。后续短信能力另起项目，不在本仓实现。

## 依赖约定（不要重复造轮子）

| 能力 | 复用来源 | 说明 |
|---|---|---|
| 配置（TOML 模板/补齐/热加载/备份） | `dhrust::config::TomlConfig<T>`（feature `toml`） | 不自己写读写与迁移 |
| TOML 数组表写回（`[[Data]]` 账号） | `TomlConfig::save_with_arrays`（底层 `dhrust::config::toml::sync_array_of_tables`） | 保留注释与排版 |
| 日志 | `dhrust::logs` | 标准日志 |
| 后台定时/线程 | `dhrust::threading` | 队列线程优先用标准线程 + 通道，不引入额外运行时 |
| 邮箱格式/后缀 | `dhrust::text::{is_email, email_suffix}` | 通用文本校验已下沉 |
| SMTP/MIME | `lettre`（rustls-tls + ring，纯 Rust 可 musl 交叉） | 不自研 MIME/认证 |
| 加密/密钥 | `dhrust::secret` | 需要保存密码时由消费方处理，本库不出配置项 |

## 硬约束

- **配置一律 TOML**（`Config/Mail.toml`），PascalCase 键名与 C# `Mail.config` 字段对齐；读写只走 `TomlConfig`。
- **不新增异步运行时**：本库提供同步 API；需要异步的消费方自行 `spawn_blocking`。
- **日志/审计只记有意义事件**：发送成功/失败、账号切换、队列积压；不逐轮询打日志。
- 公开 API 的文档注释必须说明与 C# `Pek.Mail` 的对应关系及差异。

## 每轮收尾（务必）

① `docs/决策记录.md` 新增一节（**含用户原话**）；② 运行 `tools/continuity/sync-memory.ps1` 同步记忆；③ 跑回归（`cargo test` + `cargo clippy`）并如实汇报；④ 提交并推送。
