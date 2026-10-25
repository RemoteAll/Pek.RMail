//! 邮件发送队列（对应 C# `IMailQueueProvider` / `MailQueueService` / `MailQueueManagerBase`）。
//!
//! 与 C# 的差异（有意）：
//! - C# 队列线程空闲时 `Thread.Sleep(SleepInterval)`；本实现用 `Condvar` 等待，
//!   **入队立即唤醒**，空闲时不产生周期唤醒开销（对齐“非必要不做周期性全量劳作”的要求）；
//! - 队列为进程内内存队列（与 C# 一致），进程退出未发送的邮件不落盘。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::error::{MailError, MailResult};
use crate::message::EmailBox;
use crate::sender::EmailSender;
use crate::settings::MailSettings;

/// 默认空闲等待间隔（毫秒）。
pub const DEFAULT_SLEEP_INTERVAL_MS: u64 = 3000;

/// 邮件队列提供程序（线程安全，进程内）。
#[derive(Debug, Default)]
pub struct MailQueueProvider {
    queue: Mutex<VecDeque<EmailBox>>,
    ready: Condvar,
}

impl MailQueueProvider {
    /// 新建空队列。
    pub fn new() -> Self {
        Self::default()
    }

    /// 队列邮件数量。
    pub fn count(&self) -> usize {
        self.lock().len()
    }

    /// 队列是否为空。
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// 入队并唤醒等待线程。
    pub fn enqueue(&self, mail_box: EmailBox) {
        let mut queue = self.lock();
        queue.push_back(mail_box);
        drop(queue);
        self.ready.notify_one();
    }

    /// 尝试出队（不阻塞）。
    pub fn try_dequeue(&self) -> Option<EmailBox> {
        self.lock().pop_front()
    }

    /// 等待出队：最多等待 `timeout`；超时返回 `None`。
    ///
    /// 入队会立即唤醒，不会空等到超时（用于队列线程）。
    pub fn dequeue_timeout(&self, timeout: Duration) -> Option<EmailBox> {
        self.dequeue_timeout_while(timeout, || false)
    }

    /// 等待出队，且 `should_stop()` 为 true 时立即返回 `None`。
    ///
    /// 队列线程用：停止标志与等待在**同一把锁**下判定，避免“停止通知早于开始等待”
    /// 导致的丢失唤醒（否则要等满整个 `SleepInterval`）。
    pub fn dequeue_timeout_while(
        &self,
        timeout: Duration,
        should_stop: impl Fn() -> bool,
    ) -> Option<EmailBox> {
        let deadline = Instant::now() + timeout;
        let mut queue = self.lock();
        loop {
            if should_stop() {
                return None;
            }
            if let Some(box_) = queue.pop_front() {
                return Some(box_);
            }
            if timeout.is_zero() {
                return None;
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let (guard, _) = self
                .ready
                .wait_timeout(queue, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            queue = guard;
        }
    }

    /// 唤醒所有等待线程（停止队列时用）。
    pub fn notify_all(&self) {
        self.ready.notify_all();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<EmailBox>> {
        self.queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 邮件队列服务（对应 C# `IMailQueueService`）：仅负责入队。
#[derive(Debug, Clone)]
pub struct MailQueueService {
    provider: Arc<MailQueueProvider>,
}

impl MailQueueService {
    /// 以共享队列构造。
    pub fn new(provider: Arc<MailQueueProvider>) -> Self {
        Self { provider }
    }

    /// 共享队列句柄。
    pub fn provider(&self) -> Arc<MailQueueProvider> {
        Arc::clone(&self.provider)
    }

    /// 入队。
    pub fn enqueue(&self, box_: EmailBox) {
        self.provider.enqueue(box_);
    }

    /// 队列邮件数量。
    pub fn count(&self) -> usize {
        self.provider.count()
    }

    /// 队列是否为空。
    pub fn is_empty(&self) -> bool {
        self.provider.is_empty()
    }
}

/// 后台发送线程状态。
struct WorkerState {
    provider: Arc<MailQueueProvider>,
    sender: Arc<dyn EmailSender>,
    interval_ms: AtomicU64,
    running: AtomicBool,
    stop: AtomicBool,
}

impl WorkerState {
    fn interval(&self) -> Duration {
        let ms = self.interval_ms.load(Ordering::Relaxed);
        Duration::from_millis(if ms == 0 {
            DEFAULT_SLEEP_INTERVAL_MS
        } else {
            ms
        })
    }
}

/// 邮件队列管理器（对应 C# `MailQueueManagerBase` 的具体实现）。
///
/// 用法：
/// ```no_run
/// # use std::sync::Arc;
/// # use pek_rmail::{MailQueueManager, MailQueueProvider, MailQueueService, MailSettings, SmtpEmailSender};
/// let provider = Arc::new(MailQueueProvider::new());
/// let service = MailQueueService::new(provider.clone());
/// let sender = Arc::new(SmtpEmailSender::new(MailSettings::default()));
/// let manager = MailQueueManager::new(provider, sender, 3000);
/// manager.start().unwrap();
/// // service.enqueue(box);
/// ```
pub struct MailQueueManager {
    state: Arc<WorkerState>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl MailQueueManager {
    /// 构造（`sleep_interval_ms` 为 0 时按 3000）。
    pub fn new(
        provider: Arc<MailQueueProvider>,
        sender: Arc<dyn EmailSender>,
        sleep_interval_ms: u64,
    ) -> Self {
        Self {
            state: Arc::new(WorkerState {
                provider,
                sender,
                interval_ms: AtomicU64::new(sleep_interval_ms),
                running: AtomicBool::new(false),
                stop: AtomicBool::new(false),
            }),
            worker: Mutex::new(None),
        }
    }

    /// 用配置里的 `SleepInterval` 构造。
    pub fn with_settings(
        provider: Arc<MailQueueProvider>,
        sender: Arc<dyn EmailSender>,
        settings: &MailSettings,
    ) -> Self {
        Self::new(provider, sender, settings.sleep_interval_ms())
    }

    /// 是否正在运行（后台线程已异常退出时返回 false）。
    pub fn is_running(&self) -> bool {
        if !self.state.running.load(Ordering::Acquire) {
            return false;
        }
        let worker = self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        worker.as_ref().is_some_and(|handle| !handle.is_finished())
    }

    /// 队列邮件数量。
    pub fn count(&self) -> usize {
        self.state.provider.count()
    }

    /// 当前空闲等待间隔（毫秒；0 表示使用默认 3000）。
    pub fn sleep_interval_ms(&self) -> u64 {
        let ms = self.state.interval_ms.load(Ordering::Relaxed);
        if ms == 0 {
            DEFAULT_SLEEP_INTERVAL_MS
        } else {
            ms
        }
    }

    /// 调整空闲等待间隔（毫秒；运行时生效，下一次等待使用新值）。
    pub fn set_sleep_interval_ms(&self, ms: u64) {
        self.state.interval_ms.store(ms, Ordering::Relaxed);
    }

    /// 启动后台发送线程（幂等：已在运行则忽略）。
    pub fn start(&self) -> MailResult<()> {
        let mut worker = self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.state.running.load(Ordering::Acquire)
            && worker.as_ref().is_some_and(|handle| !handle.is_finished())
        {
            dhrust::warn!("邮件发送队列已在运行，忽略重复启动");
            return Ok(());
        }

        self.state.stop.store(false, Ordering::Release);
        self.state.running.store(true, Ordering::Release);

        let state = Arc::clone(&self.state);
        let thread_state = Arc::clone(&state);
        let handle = thread::Builder::new()
            .name("EmailQueue".to_string())
            .spawn(move || run_worker(&thread_state))
            .map_err(|e| {
                state.running.store(false, Ordering::Release);
                MailError::Queue(format!("启动邮件队列线程失败：{e}"))
            })?;
        *worker = Some(handle);
        dhrust::info!(
            "邮件发送队列已启动（空闲等待 {}ms）",
            self.state.interval().as_millis()
        );
        Ok(())
    }

    /// 停止后台线程并等待退出。
    pub fn stop(&self) {
        self.state.stop.store(true, Ordering::Release);
        self.state.provider.notify_all();
        let handle = self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
        self.state.running.store(false, Ordering::Release);
        dhrust::info!("邮件发送队列已停止");
    }
}

impl Drop for MailQueueManager {
    fn drop(&mut self) {
        // 防抖：未启动时 stop 也只做原子写
        if self.state.running.load(Ordering::Acquire) {
            self.stop();
        }
    }
}

fn run_worker(state: &WorkerState) {
    while !state.stop.load(Ordering::Acquire) {
        let Some(box_) = state
            .provider
            .dequeue_timeout_while(state.interval(), || state.stop.load(Ordering::Acquire))
        else {
            continue;
        };
        let started = Instant::now();
        let subject = box_.subject.clone();
        let first_to = box_.to.first().cloned().unwrap_or_default();
        dhrust::info!("开始发送邮件：主题={subject} 收件人={first_to}");
        match state.sender.send_box(&box_) {
            Ok(response) => dhrust::info!(
                "发送邮件结束：主题={subject} 收件人={first_to} 耗时={:.3}s 响应={response}",
                started.elapsed().as_secs_f64()
            ),
            Err(err) => dhrust::error!(
                "发送邮件失败：主题={subject} 收件人={first_to} 耗时={:.3}s 错误={err}",
                started.elapsed().as_secs_f64()
            ),
        }
    }
    state.running.store(false, Ordering::Release);
}
