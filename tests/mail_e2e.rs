//! 端到端测试：本地假 SMTP 服务器 + 配置/队列/附件。
//!
//! 说明：不依赖外部邮件服务与网络，CI 可直接运行。

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use pek_rmail::{
    Attachment, EmailBox, EmailSender, MailData, MailError, MailQueueManager, MailQueueProvider,
    MailQueueService, MailSettings, MailSettingsFile, Mailer, SmtpEmailSender,
};

/// 假 SMTP 服务器句柄：地址、收到的 DATA 内容、命令列表、线程句柄。
type FakeSmtp = (
    SocketAddr,
    Arc<Mutex<Vec<u8>>>,
    Arc<Mutex<Vec<String>>>,
    thread::JoinHandle<()>,
);

/// 启动一个只够 lettre 用的假 SMTP 服务器。
fn start_fake_smtp() -> FakeSmtp {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定假 SMTP");
    let addr = listener.local_addr().expect("地址");
    let captured = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&captured);
    let commands = Arc::new(Mutex::new(Vec::new()));
    let command_sink = Arc::clone(&commands);

    let handle = thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let Ok(mut reader) = stream.try_clone().map(BufReader::new) else {
            return;
        };
        let _ = stream.write_all(b"220 fake.local ESMTP\r\n");

        let mut in_data = false;
        let mut data = Vec::new();
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }

            if in_data {
                if line == ".\r\n" {
                    in_data = false;
                    sink.lock().unwrap().extend_from_slice(&data);
                    data.clear();
                    let _ = stream.write_all(b"250 OK queued\r\n");
                } else {
                    data.extend_from_slice(line.as_bytes());
                }
                continue;
            }

            let upper = line.to_ascii_uppercase();
            if upper.starts_with("MAIL FROM") || upper.starts_with("RCPT TO") {
                command_sink.lock().unwrap().push(line.trim().to_string());
            }
            if upper.starts_with("EHLO") || upper.starts_with("HELO") {
                let _ = stream.write_all(b"250-fake.local\r\n250-SIZE 10485760\r\n250 HELP\r\n");
            } else if upper.starts_with("DATA") {
                in_data = true;
                let _ = stream.write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n");
            } else if upper.starts_with("QUIT") {
                let _ = stream.write_all(b"221 Bye\r\n");
                break;
            } else {
                let _ = stream.write_all(b"250 OK\r\n");
            }
        }
    });

    (addr, captured, commands, handle)
}

fn account(host: &str, port: u16, is_default: bool) -> MailData {
    MailData {
        code: format!("{host}:{port}"),
        host: host.to_string(),
        port,
        is_ssl: false,
        user_name: String::new(),
        password: String::new(),
        from: "sender@example.com".to_string(),
        from_name: "系统通知".to_string(),
        is_enabled: true,
        is_default,
        email_suffix: String::new(),
    }
}

fn plain_settings(port: u16) -> MailSettings {
    MailSettings {
        secure_socket_option: "None".to_string(),
        data: vec![account("127.0.0.1", port, true)],
        ..Default::default()
    }
}

#[test]
fn settings_selection_matches_csharp_rules() {
    let mut settings = MailSettings::default();
    let mut a = account("smtp-a", 25, false);
    a.code = "A".into();
    let mut b = account("smtp-b", 465, true);
    b.code = "B".into();
    let mut disabled = account("smtp-c", 25, true);
    disabled.code = "C".into();
    disabled.is_enabled = false;
    settings.data = vec![a, b, disabled];

    assert_eq!(settings.find_default().unwrap().code, "B");
    let enabled = settings.find_all_enabled();
    assert_eq!(enabled.len(), 2);
    assert_eq!(enabled[0].code, "B", "默认账号优先");
    assert_eq!(enabled[1].code, "A");
    assert_eq!(settings.find_by_code("a").unwrap().code, "A");
    assert!(settings.find_by_code("missing").is_none());

    let mut empty = MailSettings::default();
    assert!(matches!(empty.find_default(), Err(MailError::Config(_))));
    empty.data.push(account("smtp", 25, false));
    assert_eq!(empty.find_default().unwrap().host, "smtp");
}

#[test]
fn suffix_routing_prefers_matching_account() {
    let mut settings = MailSettings::default();
    let mut default = account("default", 25, true);
    default.code = "default".into();
    let mut corp = account("corp", 465, false);
    corp.code = "corp".into();
    corp.email_suffix = "corp.cn,example.org".into();
    settings.data = vec![default, corp];

    assert_eq!(settings.find_by_suffix("a@corp.cn").unwrap().code, "corp");
    assert_eq!(settings.route_for("a@example.org").unwrap().code, "corp");
    assert_eq!(settings.route_for("a@other.com").unwrap().code, "default");
}

#[test]
fn attachment_file_and_memory() {
    let dir = std::env::temp_dir().join(format!("pek-rmail-att-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("报表.txt");
    std::fs::write(&file, b"hello").unwrap();

    let from_file = Attachment::from_file(&file).unwrap();
    assert_eq!(from_file.name(), "报表.txt");
    assert_eq!(from_file.read().unwrap(), b"hello");

    let memory = Attachment::from_bytes("a.bin", vec![1, 2, 3]).with_content_type("text/plain");
    assert_eq!(memory.read().unwrap(), vec![1, 2, 3]);
    assert_eq!(memory.content_type(), "text/plain");

    assert!(Attachment::from_file(dir.join("missing.txt")).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn smtp_sends_html_with_attachment_to_fake_server() {
    let (addr, captured, commands, server) = start_fake_smtp();
    let sender = SmtpEmailSender::new(plain_settings(addr.port()));

    let mail_box = EmailBox::new("to@example.com", "测试主题", "<h1>你好</h1>")
        .cc("cc@example.com")
        .bcc("bcc@example.com")
        .attach(
            Attachment::from_bytes("a.txt", b"hello attachment".to_vec())
                .with_content_type("text/plain"),
        );

    let response = sender.send_box(&mail_box).expect("发送成功");
    assert!(response.contains("OK"), "响应：{response}");

    let _ = server.join();
    // Bcc 按协议不写入正文头，只在信封（RCPT TO）中出现
    let commands = commands.lock().unwrap().clone();
    assert!(
        commands
            .iter()
            .any(|c| c.starts_with("RCPT TO:") && c.contains("bcc@example.com")),
        "Bcc 应收件人出现在 RCPT TO：{commands:?}"
    );

    let raw = String::from_utf8_lossy(&captured.lock().unwrap()).to_string();
    assert!(raw.contains("Subject:"), "{raw}");
    assert!(raw.contains("to@example.com"), "{raw}");
    assert!(raw.contains("cc@example.com"), "{raw}");
    assert!(!raw.contains("Bcc:"), "Bcc 不应出现在正文头：{raw}");
    assert!(raw.contains("multipart/mixed"), "{raw}");
    assert!(raw.contains("text/html"), "{raw}");
    assert!(raw.contains("a.txt"), "{raw}");
    // 短文本附件按 7bit 原样传输；二进制附件 lettre 会自动 base64
    assert!(raw.contains("hello attachment"), "{raw}");
}

#[test]
fn smtp_falls_back_to_next_enabled_account() {
    // 先占一个端口再释放，保证该端口无人监听（连接会被拒绝）
    let dead = TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_port = dead.local_addr().unwrap().port();
    drop(dead);

    let (addr, captured, _commands, server) = start_fake_smtp();
    let mut settings = plain_settings(addr.port());
    settings.data = vec![
        account("127.0.0.1", dead_port, true),
        account("127.0.0.1", addr.port(), false),
    ];

    let sender = SmtpEmailSender::new(settings);
    let response = sender
        .send_box(&EmailBox::new("to@example.com", "fallback", "body"))
        .expect("应回退到第二个账号");
    assert!(response.contains("OK"), "{response}");

    let _ = server.join();
    assert!(!captured.lock().unwrap().is_empty());
}

#[test]
fn no_enabled_account_reports_config_error() {
    let mut settings = MailSettings::default();
    settings.data.push(MailData {
        is_enabled: false,
        ..account("127.0.0.1", 25, true)
    });
    let sender = SmtpEmailSender::new(settings);
    match sender.send_box(&EmailBox::new("to@example.com", "x", "y")) {
        Err(MailError::Config(msg)) => assert!(msg.contains("IsEnabled=true"), "{msg}"),
        other => panic!("期望配置错误，实际：{other:?}"),
    }
}

#[test]
fn invalid_recipient_is_reported() {
    let (addr, _captured, _commands, server) = start_fake_smtp();
    let sender = SmtpEmailSender::new(plain_settings(addr.port()));
    let err = sender
        .send_box(&EmailBox::new("not-an-email", "x", "y"))
        .expect_err("应报地址不合法");
    match err {
        MailError::AllFailed { failures, .. } => {
            assert!(failures[0].error.contains("不合法"), "{failures:?}");
        }
        other => panic!("期望 AllFailed，实际：{other:?}"),
    }
    // 服务器仍在等待连接；发送器未连上时它保持阻塞，直接丢弃监听即可
    drop(server);
}

/// 记录型发送器（队列测试）。
struct RecordingSender {
    sent: Arc<Mutex<Vec<String>>>,
    calls: AtomicUsize,
}

impl EmailSender for RecordingSender {
    fn send(&self, message: &pek_rmail::MailMessage) -> pek_rmail::MailResult<String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.sent.lock().unwrap().push(message.subject.clone());
        Ok("OK".to_string())
    }

    fn send_with_account(
        &self,
        _message: &pek_rmail::MailMessage,
        _account: &MailData,
    ) -> pek_rmail::MailResult<String> {
        Ok("OK".to_string())
    }
}

#[test]
fn queue_processes_enqueued_mail_and_wakes_immediately() {
    let provider = Arc::new(MailQueueProvider::new());
    let service = MailQueueService::new(Arc::clone(&provider));
    let sent = Arc::new(Mutex::new(Vec::new()));
    let sender = Arc::new(RecordingSender {
        sent: Arc::clone(&sent),
        calls: AtomicUsize::new(0),
    });

    let manager = MailQueueManager::new(Arc::clone(&provider), sender, 30_000);
    manager.start().unwrap();
    assert!(manager.is_running());

    service
        .enqueue(EmailBox::new("a@example.com", "queued-1", "body"))
        .unwrap();
    service
        .enqueue(EmailBox::new("b@example.com", "queued-2", "body"))
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(3);
    while sent.lock().unwrap().len() < 2 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    manager.stop();
    assert!(!manager.is_running());

    let sent = sent.lock().unwrap().clone();
    assert_eq!(sent, vec!["queued-1", "queued-2"]);
}

#[test]
fn mailer_open_save_and_send_via_config_file() {
    let (addr, captured, _commands, server) = start_fake_smtp();
    let dir = std::env::temp_dir().join(format!("pek-rmail-mailer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mailer = Mailer::open_in(&dir);
    assert!(
        mailer.config_path().unwrap().ends_with("Config\\Mail.toml")
            || mailer.config_path().unwrap().ends_with("Config/Mail.toml")
    );
    // 首次加载生成模板（带一条示例账号）
    assert_eq!(mailer.settings().data.len(), 1);

    let settings = MailSettings {
        secure_socket_option: "None".to_string(),
        data: vec![account("127.0.0.1", addr.port(), true)],
        ..Default::default()
    };
    mailer.save_settings(settings).expect("保存配置");
    let response = mailer
        .send_box(&EmailBox::new("to@example.com", "via-config", "<p>hi</p>"))
        .expect("通过 Mailer 发送");
    assert!(response.contains("OK"), "{response}");

    let _ = server.join();
    let raw = String::from_utf8_lossy(&captured.lock().unwrap()).to_string();
    assert!(
        raw.contains("via-config") || raw.contains("Subject:"),
        "{raw}"
    );
    let text = std::fs::read_to_string(mailer.config_path().unwrap()).unwrap();
    assert!(text.contains(&format!("Port = {}", addr.port())), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn config_file_round_trip_keeps_comments_and_syncs_accounts() {
    let dir = std::env::temp_dir().join(format!("pek-rmail-cfg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Mail.toml");

    let mut file = MailSettingsFile::load(&path);
    assert!(file.is_new());
    assert_eq!(file.settings().data.len(), 1, "模板自带示例账号");

    let mut settings = file.settings().clone();
    settings.data = vec![account("smtp-a", 465, true), account("smtp-b", 25, false)];
    *file.settings_mut() = settings;
    file.save().expect("保存");

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("# Pek.RMail 邮件配置"), "模板注释应保留");
    assert_eq!(text.matches("[[Data]]").count(), 2, "{text}");
    assert!(text.contains("smtp-a") && text.contains("smtp-b"), "{text}");

    let mut reloaded = MailSettings::load(&path);
    assert_eq!(reloaded.value().data.len(), 2);
    assert_eq!(reloaded.value().find_default().unwrap().host, "smtp-a");
    assert!(
        reloaded
            .take_notes()
            .iter()
            .all(|n| !n.contains("解析失败"))
    );
    let _ = std::fs::remove_dir_all(&dir);
}
