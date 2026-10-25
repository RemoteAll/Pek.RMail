//! 示例：从 `Config/Mail.toml` 读取配置并发送一封邮件。
//!
//! 用法（PowerShell）：
//! ```powershell
//! cargo run --example send_mail -- user@example.com "主题" "<h1>正文</h1>"
//! ```

use std::env;

use pek_rmail::{EmailBox, Mailer};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let to = args
        .next()
        .unwrap_or_else(|| "user@example.com".to_string());
    let subject = args.next().unwrap_or_else(|| "Pek.RMail 测试".to_string());
    let body = args
        .next()
        .unwrap_or_else(|| "<h1>这是一封来自 Pek.RMail 的测试邮件</h1>".to_string());

    let mailer = Mailer::open_in(".");
    for note in mailer.take_notes() {
        println!("配置提示：{note}");
    }
    println!("配置文件：{:?}", mailer.config_path());
    println!(
        "可用账号：{}",
        mailer
            .settings()
            .find_all_enabled()
            .iter()
            .map(|a| a.label())
            .collect::<Vec<_>>()
            .join(", ")
    );

    let mail_box = EmailBox::new(to, subject, body);
    let response = mailer.send_box(&mail_box)?;
    println!("发送成功：{response}");
    Ok(())
}
