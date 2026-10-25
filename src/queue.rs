//! 邮件发送队列（对应 C# `IMailQueueProvider` / `MailQueueService` / `MailQueueManagerBase`）。
//!
//! 与 C# 的差异（有意）：
//! - C# 队列线程空闲时 `Thread.Sleep(SleepInterval)`；本实现用 `Condvar` 等待，
//!   **入队立即唤醒**，空闲时不产生周期唤醒开销（对齐“非必要不做周期性全量劳作”的要求）；
//! - 队列为进程内内存队列（与 C# 一致），进程退出未发送的邮件不落盘。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dhrust::threading::BlockingQueue;

use crate::error::{MailError, MailResult};
use crate::message::EmailBox;
use crate::sender::EmailSender;
use crate::settings::MailSettings;

/// 默认空闲等待间隔（毫秒）。
pub const DEFAULT_SLEEP_INTERVAL_MS: u64 = 3000;

/// 默认停止等待上限（等当前发送结束）。
pub const DEFAULT_STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// 邮件队列提供程序（线程安全，进程内）。
#[derive(Debug, Default)]
pub struct MailQueueProvider {
    queue: BlockingQueue<EmailBox>,
}

impl MailQueueProvider {
    /// 新建空队列（无容量上限）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 新建带容量上限的队列（满时 `try_enqueue` 返回原邮件，供调用方背压）。
    pub fn with_capacity(max_queue: usize) -> Self {
        Self {
            queue: BlockingQueue::with_capacity(max_queue),
        }
    }

    /// 队列邮件数量。
    pub fn count(&self) -> usize {
        self.queue.len()
    }

    /// 队列是否为空。
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// 尝试入队（满/关闭时返回原邮件，不阻塞）。
    pub fn try_enqueue(&self, mail_box: EmailBox) -> Result<(), Box<EmailBox>> {
        self.queue.try_enqueue(mail_box).map_err(Box::new)
    }

    /// 入队并唤醒等待线程；队列已满返回 [`MailError::Queue`]。
    pub fn enqueue(&self, mail_box: EmailBox) -> MailResult<()> {
        self.try_enqueue(mail_box)
            .map_err(|_| MailError::Queue("邮件队列已满，入队失败".to_string()))
    }

    /// 尝试出队（不阻塞）。
    pub fn try_dequeue(&self) -> Option<EmailBox> {
        self.queue.try_dequeue()
    }

    /// 等待出队：最多等待 `timeout`；超时返回 `None`。
    pub fn dequeue_timeout(&self, timeout: Duration) -> Option<EmailBox> {
        self.queue.dequeue_timeout(timeout)
    }

    /// 等待出队，且 `should_stop()` 为 true 时立即返回 `None`。
    pub fn dequeue_timeout_while(
        &self,
        timeout: Duration,
        should_stop: impl Fn() -> bool,
    ) -> Option<EmailBox> {
        self.queue.dequeue_timeout_while(timeout, should_stop)
    }

    /// 唤醒所有等待线程（停止队列时用）。
    pub fn notify_all(&self) {
        self.queue.notify_all();
    }

    /// 关闭队列（拒绝后续入队；已入队邮件仍可取出）。
    pub fn close(&self) {
        self.queue.close();
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
    pub fn enqueue(&self, box_: EmailBox) -> MailResult<()> {
        self.provider.enqueue(box_)
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
    /// 运行代次：每次 start/stop 递增，旧线程据此退出且不再改写 `running`。
    epoch: AtomicU64,
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

    fn is_current(&self, epoch: u64) -> bool {
        self.epoch.load(Ordering::Acquire) == epoch
    }
}

/// 邮件队列管理器（对应 C# `MailQueueManagerBase` 的具体实现）。
///
/// 用法：
/// ```no_run
/// # use std::sync::Arc;
/// # use pek_rmail::{EmailBox, MailQueueManager, MailQueueProvider, MailQueueService, MailSettings, SmtpEmailSender};
/// let provider = Arc::new(MailQueueProvider::new());
/// let service = MailQueueService::new(provider.clone());
/// let sender = Arc::new(SmtpEmailSender::new(MailSettings::default()));
/// let manager = MailQueueManager::new(provider, sender, 3000);
/// manager.start().unwrap();
/// service.enqueue(EmailBox::new("user@example.com", "subject", "body")).unwrap();
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
                epoch: AtomicU64::new(0),
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

        // 先递增代次再置 running，确保旧线程退出时不会清掉新线程的运行标记。
        let epoch = self.state.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.state.running.store(true, Ordering::Release);

        let state = Arc::clone(&self.state);
        let thread_state = Arc::clone(&state);
        let handle = thread::Builder::new()
            .name("EmailQueue".to_string())
            .spawn(move || run_worker(&thread_state, epoch))
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

    /// 停止后台线程并等待退出（最多等 30 秒）。
    pub fn stop(&self) {
        let _ = self.stop_timeout(DEFAULT_STOP_TIMEOUT);
    }

    /// 停止后台线程并最多等待 `timeout`；超时返回 `false`（线程被分离，稍后自行退出）。
    pub fn stop_timeout(&self, timeout: Duration) -> bool {
        let was_running = self.state.running.load(Ordering::Acquire);
        // 递增代次 = 停止信号：旧线程退出后不会再把新线程的 running 置 false。
        self.state.epoch.fetch_add(1, Ordering::AcqRel);
        self.state.provider.notify_all();
        let handle = self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();

        let stopped = if let Some(handle) = handle {
            let deadline = Instant::now() + timeout;
            while !handle.is_finished() {
                if Instant::now() >= deadline {
                    self.state.running.store(false, Ordering::Release);
                    return false;
                }
                thread::sleep(Duration::from_millis(10));
            }
            let _ = handle.join();
            true
        } else {
            true
        };

        self.state.running.store(false, Ordering::Release);
        if was_running {
            dhrust::info!("邮件发送队列已停止");
        }
        stopped
    }
}

impl Drop for MailQueueManager {
    fn drop(&mut self) {
        // 防抖：未启动时不做任何事；运行中最多等 2 秒，避免拖住进程退出
        if self.state.running.load(Ordering::Acquire) {
            let _ = self.stop_timeout(Duration::from_secs(2));
        }
    }
}

fn run_worker(state: &WorkerState, epoch: u64) {
    while state.is_current(epoch) {
        let Some(box_) = state
            .provider
            .dequeue_timeout_while(state.interval(), || !state.is_current(epoch))
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
    if state.is_current(epoch) {
        state.running.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::MailMessage;
    use crate::settings::MailData;

    struct BlockingSender {
        started: std::sync::mpsc::Sender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl EmailSender for BlockingSender {
        fn send(&self, _message: &MailMessage) -> MailResult<String> {
            let _ = self.started.send(());
            let _ = self.release.lock().unwrap().recv();
            Ok("OK".to_string())
        }

        fn send_with_account(
            &self,
            _message: &MailMessage,
            _account: &MailData,
        ) -> MailResult<String> {
            Ok("OK".to_string())
        }
    }

    #[test]
    fn provider_capacity_rejects_overflow() {
        let provider = MailQueueProvider::with_capacity(1);
        assert!(
            provider
                .try_enqueue(EmailBox::new("a@example.com", "s1", "b"))
                .is_ok()
        );
        assert!(
            provider
                .try_enqueue(EmailBox::new("b@example.com", "s2", "b"))
                .is_err()
        );
        assert_eq!(provider.count(), 1);
        assert!(
            provider
                .enqueue(EmailBox::new("c@example.com", "s3", "b"))
                .is_err()
        );
    }

    #[test]
    fn stale_worker_does_not_clear_running_after_restart() {
        let provider = Arc::new(MailQueueProvider::new());
        provider
            .enqueue(EmailBox::new("a@example.com", "s1", "b"))
            .unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let sender = Arc::new(BlockingSender {
            started: started_tx,
            release: Mutex::new(release_rx),
        });
        let state = Arc::new(WorkerState {
            provider,
            sender,
            interval_ms: AtomicU64::new(30_000),
            running: AtomicBool::new(true),
            epoch: AtomicU64::new(1),
        });

        let thread_state = Arc::clone(&state);
        let handle = thread::spawn(move || run_worker(&thread_state, 1));
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("worker should start sending");

        // 模拟 stop_timeout 后立刻 start：新代次 + running 重新置 true。
        state.epoch.store(2, Ordering::Release);
        state.running.store(true, Ordering::Release);
        release_tx.send(()).unwrap();
        handle.join().unwrap();

        assert!(
            state.running.load(Ordering::Acquire),
            "旧线程退出时不应清掉新线程的运行标记"
        );
    }
}
