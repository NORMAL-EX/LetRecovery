//! 日志管理模块
//!
//! 提供文件日志记录功能，支持：
//! - 日志文件存储在 `{软件运行目录}/log` 目录
//! - 日志实时刷新到文件
//! - 可在运行时动态开关日志
//! - 日志状态持久化到配置文件

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter, Layer};

use super::path::get_exe_dir;

/// 全局日志启用状态
static LOG_ENABLED: AtomicBool = AtomicBool::new(true);

/// 全局日志守卫（保持文件写入器存活）
static LOG_GUARD: OnceLock<RwLock<Option<WorkerGuard>>> = OnceLock::new();
static LOG_BARRIER_ID: AtomicU64 = AtomicU64::new(0);
static LOG_BARRIER_NONCE: OnceLock<String> = OnceLock::new();

fn log_barrier_nonce() -> anyhow::Result<&'static str> {
    if let Some(value) = LOG_BARRIER_NONCE.get() {
        return Ok(value);
    }
    use windows::Win32::Security::Cryptography::{
        BCryptGenRandom, BCRYPT_ALG_HANDLE, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
    };
    let mut bytes = [0u8; 16];
    let status = unsafe {
        BCryptGenRandom(
            BCRYPT_ALG_HANDLE::default(),
            &mut bytes,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status.is_err() {
        anyhow::bail!("BCryptGenRandom failed while creating the log barrier nonce");
    }
    let generated = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let _ = LOG_BARRIER_NONCE.set(generated);
    LOG_BARRIER_NONCE
        .get()
        .map(String::as_str)
        .ok_or_else(|| anyhow::anyhow!("log barrier nonce was not initialized"))
}

/// 日志管理器
pub struct LogManager;

/// A flush-complete log object whose directory entry cannot be replaced while held on Windows.
///
/// Consumers that require a byte-accurate snapshot must read from `file()` instead of reopening
/// `path()`. The path is retained only for diagnostics.
pub struct LogBarrierSnapshot {
    path: PathBuf,
    file: std::fs::File,
}

impl LogBarrierSnapshot {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn file(&self) -> &std::fs::File {
        &self.file
    }
}

/// Converts tracing's platform-independent LF records to CRLF so the legacy Windows 7 Notepad
/// renders one record per line. Existing CRLF pairs are preserved, including pairs split across
/// separate writes.
struct CrLfWriter<W> {
    inner: W,
    previous_was_cr: bool,
}

impl<W> CrLfWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            previous_was_cr: false,
        }
    }
}

impl<W: Write> Write for CrLfWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut converted = Vec::with_capacity(buf.len().saturating_add(16));
        for &byte in buf {
            if byte == b'\n' && !self.previous_was_cr {
                converted.push(b'\r');
            }
            converted.push(byte);
            self.previous_was_cr = byte == b'\r';
        }
        self.inner.write_all(&converted)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl LogManager {
    /// 获取日志目录路径
    pub fn get_log_dir() -> PathBuf {
        get_exe_dir().join("log")
    }

    /// 初始化日志系统
    ///
    /// # Arguments
    /// * `enabled` - 是否启用日志记录
    ///
    /// # Returns
    /// 如果初始化成功返回 Ok(())
    pub fn init(enabled: bool) -> anyhow::Result<()> {
        LOG_ENABLED.store(enabled, Ordering::SeqCst);

        // 创建日志目录
        let log_dir = Self::get_log_dir();
        if enabled {
            std::fs::create_dir_all(&log_dir)?;
        }

        // 配置环境过滤器
        let env_filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

        if enabled {
            // 创建文件日志写入器（按日期滚动）
            // 用 Builder 设置 .log 后缀，文件名形如 LetRecovery.2026-06-06.log
            // （直接用 rolling::daily 会得到 LetRecovery.log.2026-06-06，后缀是日期而非 .log）
            let file_appender = tracing_appender::rolling::Builder::new()
                .filename_prefix("LetRecovery")
                .filename_suffix("log")
                .rotation(tracing_appender::rolling::Rotation::DAILY)
                .build(&log_dir)
                .map_err(|e| anyhow::anyhow!("创建日志文件写入器失败: {}", e))?;
            let (non_blocking, guard) =
                tracing_appender::non_blocking(CrLfWriter::new(file_appender));

            // 文件日志格式层
            let file_layer = fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_target(true)
                .with_thread_ids(false)
                .with_thread_names(false)
                .with_file(true)
                .with_line_number(true)
                .with_filter(env_filter);

            // 初始化 tracing 订阅器
            tracing_subscriber::registry().with(file_layer).init();

            // 保存守卫以保持日志文件打开
            let lock = LOG_GUARD.get_or_init(|| RwLock::new(None));
            *lock.write() = Some(guard);

            // 兼容 log crate 的宏
            Self::setup_log_compat();

            log::info!("日志系统初始化完成，日志目录: {}", log_dir.display());
        } else {
            // 日志禁用时，使用空订阅器
            let noop_layer = fmt::layer()
                .with_writer(std::io::sink)
                .with_filter(EnvFilter::new("off"));

            tracing_subscriber::registry().with(noop_layer).init();

            // 仍然设置 log 兼容层（但输出会被过滤）
            Self::setup_log_compat();
        }

        Ok(())
    }

    /// 设置 log crate 兼容层
    fn setup_log_compat() {
        // tracing-log 桥接已经通过 tracing-subscriber 自动处理
        // 这里不需要额外操作，tracing-subscriber 默认支持 log crate
    }

    /// 检查日志是否启用
    pub fn is_enabled() -> bool {
        LOG_ENABLED.load(Ordering::SeqCst)
    }

    /// 设置日志启用状态
    ///
    /// 注意：此方法仅更新状态标志，不会动态重新初始化日志系统
    /// 新状态将在下次程序启动时生效
    pub fn set_enabled(enabled: bool) {
        LOG_ENABLED.store(enabled, Ordering::SeqCst);

        if enabled {
            log::info!("日志记录已启用（将在重启后完全生效）");
        }
    }

    /// 刷新日志缓冲区
    ///
    /// 强制将所有缓冲的日志写入文件
    pub fn flush() {
        if let Err(error) = Self::flush_barrier() {
            log::warn!("日志落盘屏障失败: {error:#}");
        }
    }

    /// Wait until an observable marker has passed through tracing-appender's
    /// FIFO worker, then flush the containing file through the filesystem.
    ///
    /// tracing-appender 0.2.x implements `NonBlocking::flush()` as a no-op, so
    /// emitting an ordinary record is not a persistence barrier. Observing a
    /// unique marker in the file proves that every earlier queued record was
    /// processed. Failure is returned to the caller instead of being presented
    /// as a successful snapshot boundary.
    pub fn flush_barrier() -> anyhow::Result<LogBarrierSnapshot> {
        use anyhow::bail;

        if !Self::is_enabled() {
            bail!("logging is disabled");
        }
        let id = LOG_BARRIER_ID.fetch_add(1, Ordering::Relaxed);
        let marker = format!(
            "LR_LOG_BARRIER_{}_{}_{}",
            std::process::id(),
            log_barrier_nonce()?,
            id
        );
        log::info!("{marker}");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut last_candidate_error = None;
        loop {
            // Refresh the bounded set on every poll. The appender can rotate at midnight between
            // marker emission and its asynchronous write, creating a path which did not exist on
            // the first poll.
            let candidates = Self::barrier_log_candidates(8);
            if let Some(snapshot) = Self::find_log_barrier_candidate(
                &candidates,
                marker.as_bytes(),
                &mut last_candidate_error,
            )? {
                return Ok(snapshot);
            }
            if Instant::now() >= deadline {
                match last_candidate_error {
                    Some(error) => bail!(
                        "timed out waiting for the asynchronous log writer; last candidate error: {error}"
                    ),
                    None => bail!("timed out waiting for the asynchronous log writer"),
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn barrier_log_candidates(limit: usize) -> Vec<PathBuf> {
        if limit == 0 {
            return Vec::new();
        }
        let current = Self::get_current_log_file();
        let mut candidates = Vec::with_capacity(limit);
        if current.exists() {
            candidates.push(current.clone());
        }
        for path in Self::latest_log_files(limit) {
            if candidates.len() >= limit {
                break;
            }
            if path != current {
                candidates.push(path);
            }
        }
        candidates
    }

    fn find_log_barrier_candidate(
        candidates: &[PathBuf],
        marker: &[u8],
        last_candidate_error: &mut Option<String>,
    ) -> anyhow::Result<Option<LogBarrierSnapshot>> {
        use anyhow::Context;

        for path in candidates {
            let mut file = match Self::open_log_barrier_candidate(path) {
                Ok(file) => file,
                Err(error) => {
                    *last_candidate_error = Some(format!("{}: {error:#}", path.display()));
                    continue;
                }
            };
            let contains = match Self::log_tail_contains(&mut file, path, marker) {
                Ok(contains) => contains,
                Err(error) => {
                    *last_candidate_error = Some(format!("{}: {error:#}", path.display()));
                    continue;
                }
            };
            if contains {
                file.sync_all()
                    .with_context(|| format!("flush log to storage: {}", path.display()))?;
                return Ok(Some(LogBarrierSnapshot {
                    path: path.clone(),
                    file,
                }));
            }
        }
        Ok(None)
    }

    fn open_log_barrier_candidate(path: &Path) -> anyhow::Result<std::fs::File> {
        use anyhow::Context;

        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows::Win32::Storage::FileSystem::{
                FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
            };

            // Keeping this handle open without FILE_SHARE_DELETE prevents a daily log entry from
            // being renamed or replaced between marker observation and snapshot consumption.
            options
                .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE).0)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
        }
        options
            .open(path)
            .with_context(|| format!("open log barrier candidate: {}", path.display()))
    }

    fn log_tail_contains(
        file: &mut std::fs::File,
        path: &Path,
        needle: &[u8],
    ) -> anyhow::Result<bool> {
        use anyhow::Context;

        const TAIL_LIMIT: u64 = 256 * 1024;
        let length = file
            .metadata()
            .with_context(|| format!("read log barrier metadata: {}", path.display()))?
            .len();
        let start = length.saturating_sub(TAIL_LIMIT);
        file.seek(SeekFrom::Start(start))
            .with_context(|| format!("seek log barrier candidate: {}", path.display()))?;
        let mut tail = Vec::with_capacity((length - start) as usize);
        file.take(length - start)
            .read_to_end(&mut tail)
            .with_context(|| format!("read log barrier candidate: {}", path.display()))?;
        Ok(tail.windows(needle.len()).any(|window| window == needle))
    }

    /// 获取当前日志文件路径
    ///
    /// 返回当天的日志文件路径
    pub fn get_current_log_file() -> PathBuf {
        let log_dir = Self::get_log_dir();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        log_dir.join(format!("LetRecovery.{}.log", today))
    }

    fn latest_log_files(limit: usize) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(Self::get_log_dir()) else {
            return Vec::new();
        };
        let mut files: Vec<_> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                let is_log = path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("log"));
                if !is_log {
                    return None;
                }
                let modified = entry.metadata().ok()?.modified().ok()?;
                Some((path, modified))
            })
            .collect();
        files.sort_by(|left, right| right.1.cmp(&left.1));
        files
            .into_iter()
            .take(limit)
            .map(|(path, _)| path)
            .collect()
    }

    /// 清理旧日志文件
    ///
    /// 删除指定天数之前的日志文件
    ///
    /// # Arguments
    /// * `days` - 保留最近多少天的日志
    pub fn cleanup_old_logs(days: u32) -> anyhow::Result<()> {
        let log_dir = Self::get_log_dir();
        if !log_dir.exists() {
            return Ok(());
        }

        let cutoff = chrono::Local::now() - chrono::Duration::days(days as i64);

        for entry in std::fs::read_dir(&log_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                if let Ok(metadata) = path.metadata() {
                    if let Ok(modified) = metadata.modified() {
                        let modified: chrono::DateTime<chrono::Local> = modified.into();
                        if modified < cutoff {
                            if let Err(e) = std::fs::remove_file(&path) {
                                log::warn!("删除旧日志文件失败: {} - {}", path.display(), e);
                            } else {
                                log::info!("已删除旧日志文件: {}", path.display());
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// 格式化文件大小为人类可读格式
    pub fn format_size(bytes: u64) -> String {
        const KB: u64 = 1024;
        const MB: u64 = KB * 1024;
        const GB: u64 = MB * 1024;

        if bytes >= GB {
            format!("{:.2} GB", bytes as f64 / GB as f64)
        } else if bytes >= MB {
            format!("{:.2} MB", bytes as f64 / MB as f64)
        } else if bytes >= KB {
            format!("{:.2} KB", bytes as f64 / KB as f64)
        } else {
            format!("{} B", bytes)
        }
    }
}

/// 日志记录宏的包装，添加启用状态检查
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        if $crate::utils::logger::LogManager::is_enabled() {
            log::info!($($arg)*);
        }
    };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {
        if $crate::utils::logger::LogManager::is_enabled() {
            log::warn!($($arg)*);
        }
    };
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {
        if $crate::utils::logger::LogManager::is_enabled() {
            log::error!($($arg)*);
        }
    };
}

#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => {
        if $crate::utils::logger::LogManager::is_enabled() {
            log::debug!($($arg)*);
        }
    };
}

#[macro_export]
macro_rules! log_trace {
    ($($arg:tt)*) => {
        if $crate::utils::logger::LogManager::is_enabled() {
            log::trace!($($arg)*);
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_writer_converts_lone_newlines_and_preserves_crlf() {
        let mut output = Vec::new();
        {
            let mut writer = CrLfWriter::new(&mut output);
            writer.write_all(b"first\nsecond\r\nthird").unwrap();
            writer.flush().unwrap();
        }
        assert_eq!(output, b"first\r\nsecond\r\nthird");
    }

    #[test]
    fn crlf_writer_preserves_a_crlf_split_across_writes() {
        let mut output = Vec::new();
        {
            let mut writer = CrLfWriter::new(&mut output);
            writer.write_all(b"first\r").unwrap();
            writer.write_all(b"\nsecond\n").unwrap();
        }
        assert_eq!(output, b"first\r\nsecond\r\n");
    }

    #[test]
    fn test_format_size() {
        assert_eq!(LogManager::format_size(0), "0 B");
        assert_eq!(LogManager::format_size(512), "512 B");
        assert_eq!(LogManager::format_size(1024), "1.00 KB");
        assert_eq!(LogManager::format_size(1536), "1.50 KB");
        assert_eq!(LogManager::format_size(1048576), "1.00 MB");
        assert_eq!(LogManager::format_size(1073741824), "1.00 GB");
    }

    #[test]
    fn test_log_dir_path() {
        let log_dir = LogManager::get_log_dir();
        assert!(log_dir.ends_with("log"));
    }

    #[cfg(windows)]
    #[test]
    fn barrier_candidate_handle_prevents_path_replacement() {
        let root = std::env::temp_dir().join(format!(
            "lr-log-barrier-share-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("candidate.log");
        let moved = root.join("candidate.moved.log");
        std::fs::write(&source, b"before\nunique-marker\nafter\n").unwrap();

        let mut held = LogManager::open_log_barrier_candidate(&source).unwrap();
        assert!(LogManager::log_tail_contains(&mut held, &source, b"unique-marker").unwrap());
        assert!(std::fs::rename(&source, &moved).is_err());
        assert!(std::fs::remove_file(&source).is_err());

        drop(held);
        std::fs::rename(&source, &moved).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bad_barrier_candidate_does_not_hide_later_matching_log() {
        let root = std::env::temp_dir().join(format!(
            "lr-log-barrier-skip-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let missing = root.join("missing.log");
        let valid = root.join("valid.log");
        std::fs::write(&valid, b"before\nunique-marker\nafter\n").unwrap();

        let mut diagnostic = None;
        let snapshot = LogManager::find_log_barrier_candidate(
            &[missing, valid.clone()],
            b"unique-marker",
            &mut diagnostic,
        )
        .unwrap()
        .expect("the valid second candidate must still be inspected");
        assert_eq!(snapshot.path(), valid);
        assert!(diagnostic.is_some());

        drop(snapshot);
        std::fs::remove_dir_all(root).unwrap();
    }
}
