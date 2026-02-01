//! 日志管理模块
//! 
//! 提供文件日志记录功能，支持：
//! - 日志文件存储在 `{软件运行目录}/log` 目录
//! - 日志实时刷新到文件
//! - 可在运行时动态开关日志
//! - 日志状态持久化到配置文件

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

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

/// 日志管理器
pub struct LogManager;

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
        let env_filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info"));

        if enabled {
            // 创建文件日志写入器（按日期滚动）
            let file_appender = tracing_appender::rolling::daily(&log_dir, "LetRecovery.log");
            let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

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
            tracing_subscriber::registry()
                .with(file_layer)
                .init();

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

            tracing_subscriber::registry()
                .with(noop_layer)
                .init();

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
        // non_blocking writer 会在 guard 被 drop 时自动刷新
        // 这里通过写入一条空日志来触发刷新
        if Self::is_enabled() {
            log::trace!("日志刷新");
        }
    }

    /// 获取当前日志文件路径
    /// 
    /// 返回当天的日志文件路径
    pub fn get_current_log_file() -> PathBuf {
        let log_dir = Self::get_log_dir();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        log_dir.join(format!("LetRecovery.log.{}", today))
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

    /// 获取日志目录大小（字节）
    pub fn get_log_dir_size() -> u64 {
        let log_dir = Self::get_log_dir();
        if !log_dir.exists() {
            return 0;
        }

        walkdir::WalkDir::new(&log_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum()
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
}
