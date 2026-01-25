//! aria2下载管理器模块
//!
//! 优化点：
//! - 支持预启动aria2进程
//! - 更快的RPC连接（减少等待时间）
//! - 全局单例模式，避免重复启动

use anyhow::Result;
use aria2_ws::response::TaskStatus;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex as TokioMutex;

use crate::utils::cmd::create_command;
use crate::utils::path::get_bin_dir;

/// 全局aria2管理器（延迟初始化）
static GLOBAL_ARIA2: OnceLock<Arc<TokioMutex<Option<Aria2Manager>>>> = OnceLock::new();

/// aria2是否已预热
static ARIA2_WARMED_UP: AtomicBool = AtomicBool::new(false);

/// 下载进度信息
#[derive(Debug, Clone)]
pub struct DownloadProgress {
    pub gid: String,
    pub completed_length: u64,
    pub total_length: u64,
    pub download_speed: u64,
    pub percentage: f64,
    pub status: DownloadStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DownloadStatus {
    Waiting,
    Active,
    Paused,
    Complete,
    Error(String),
}

/// aria2 下载管理器
pub struct Aria2Manager {
    client: Option<Arc<aria2_ws::Client>>,
    aria2_process: Option<Child>,
}

impl Aria2Manager {
    /// 预热aria2（在后台启动进程并建立连接）
    /// 
    /// 可以在应用启动时或用户选择PE时调用，提前准备好aria2
    pub async fn warmup() -> Result<()> {
        if ARIA2_WARMED_UP.load(Ordering::SeqCst) {
            log::info!("[aria2] 已经预热过，跳过");
            return Ok(());
        }

        log::info!("[aria2] 开始预热...");
        
        // 获取或创建全局管理器
        let global = GLOBAL_ARIA2.get_or_init(|| Arc::new(TokioMutex::new(None)));
        let mut guard = global.lock().await;
        
        if guard.is_some() {
            log::info!("[aria2] 全局管理器已存在");
            ARIA2_WARMED_UP.store(true, Ordering::SeqCst);
            return Ok(());
        }
        
        // 启动新的管理器
        match Self::start_internal().await {
            Ok(manager) => {
                *guard = Some(manager);
                ARIA2_WARMED_UP.store(true, Ordering::SeqCst);
                log::info!("[aria2] 预热完成");
                Ok(())
            }
            Err(e) => {
                log::warn!("[aria2] 预热失败: {}", e);
                Err(e)
            }
        }
    }

    /// 获取全局aria2管理器（如果已预热）或创建新的
    pub async fn get_or_start() -> Result<Arc<TokioMutex<Option<Aria2Manager>>>> {
        let global = GLOBAL_ARIA2.get_or_init(|| Arc::new(TokioMutex::new(None)));
        
        {
            let mut guard = global.lock().await;
            if guard.is_none() {
                log::info!("[aria2] 全局管理器不存在，正在创建...");
                let manager = Self::start_internal().await?;
                *guard = Some(manager);
                ARIA2_WARMED_UP.store(true, Ordering::SeqCst);
            }
        }
        
        Ok(Arc::clone(global))
    }

    /// 内部启动方法
    async fn start_internal() -> Result<Self> {
        let bin_dir = get_bin_dir();
        let aria2c_path = bin_dir.join("aria2c.exe");

        if !aria2c_path.exists() {
            anyhow::bail!("aria2c.exe not found at {:?}", aria2c_path);
        }

        log::info!("[aria2] 正在启动 aria2c 进程...");
        let start_time = std::time::Instant::now();

        // 启动 aria2c 进程，启用 RPC
        let process = create_command(&aria2c_path)
            .args([
                "--daemon=true",
                "--enable-rpc=true",
                "--rpc-listen-port=6800",
                "--rpc-allow-origin-all=true",
                "--max-concurrent-downloads=5",
                "--split=32",
                "--max-connection-per-server=16",
                "--min-split-size=1M",
                "--file-allocation=none",
                "--continue=true",
                "--auto-file-renaming=false",
                "--allow-overwrite=true",
            ])
            .spawn()?;

        log::info!("[aria2] aria2c 进程已启动，正在等待 RPC 服务就绪...");

        // 优化：使用更短的初始等待时间和更快的重试
        // 第一次等待100ms，之后每次等待200ms，最多尝试20次（约4秒）
        let mut client = None;
        let mut last_error = String::new();

        // 先短暂等待进程启动
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        for i in 0..20 {
            match aria2_ws::Client::connect("ws://127.0.0.1:6800/jsonrpc", None).await {
                Ok(c) => {
                    client = Some(c);
                    let elapsed = start_time.elapsed();
                    log::info!("[aria2] RPC 连接成功 (第 {} 次尝试)，总耗时: {:?}", i + 1, elapsed);
                    break;
                }
                Err(e) => {
                    last_error = e.to_string();
                    if i < 5 {
                        // 前5次尝试，每200ms重试一次
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    } else {
                        // 之后每300ms重试一次
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    }
                }
            }
        }

        let client = client.ok_or_else(|| {
            anyhow::anyhow!("初始化aria2失败: {}", last_error)
        })?;

        Ok(Self {
            client: Some(Arc::new(client)),
            aria2_process: Some(process),
        })
    }

    /// 启动 aria2c 进程并连接（公开接口，向后兼容）
    pub async fn start() -> Result<Self> {
        Self::start_internal().await
    }

    /// 添加下载任务
    pub async fn add_download(
        &self,
        url: &str,
        save_dir: &str,
        filename: Option<&str>,
    ) -> Result<String> {
        self.add_download_with_headers(url, save_dir, filename, None).await
    }

    /// 添加下载任务（支持自定义headers）
    pub async fn add_download_with_headers(
        &self,
        url: &str,
        save_dir: &str,
        filename: Option<&str>,
        headers: Option<Vec<String>>,
    ) -> Result<String> {
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("aria2 client not connected"))?;

        let mut options = aria2_ws::TaskOptions::default();
        options.dir = Some(save_dir.to_string());
        options.split = Some(32);
        options.max_connection_per_server = Some(16);

        if let Some(name) = filename {
            options.out = Some(name.to_string());
        }

        // 设置自定义headers
        if let Some(hdrs) = headers {
            if !hdrs.is_empty() {
                log::info!("[aria2] 设置自定义headers到请求选项，数量: {}", hdrs.len());
                for (i, h) in hdrs.iter().enumerate() {
                    let header_name = h.split(':').next().unwrap_or("Unknown");
                    log::info!("[aria2] 设置Header[{}]: {}", i, header_name);
                }
                options.header = Some(hdrs);
            } else {
                log::warn!("[aria2] 收到空的headers列表");
            }
        } else {
            log::info!("[aria2] 未提供自定义headers");
        }

        let gid = client
            .add_uri(vec![url.to_string()], Some(options), None, None)
            .await?;

        Ok(gid)
    }

    /// 获取下载状态
    pub async fn get_status(&self, gid: &str) -> Result<DownloadProgress> {
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("aria2 client not connected"))?;

        let status = client.tell_status(gid).await?;

        let completed = status.completed_length;
        let total = status.total_length;
        let speed = status.download_speed;

        let percentage = if total > 0 {
            (completed as f64 / total as f64) * 100.0
        } else {
            0.0
        };

        let download_status = match status.status {
            TaskStatus::Waiting => DownloadStatus::Waiting,
            TaskStatus::Active => DownloadStatus::Active,
            TaskStatus::Paused => DownloadStatus::Paused,
            TaskStatus::Complete => DownloadStatus::Complete,
            TaskStatus::Error => DownloadStatus::Error(status.error_message.unwrap_or_default()),
            TaskStatus::Removed => DownloadStatus::Error("已移除".to_string()),
        };

        Ok(DownloadProgress {
            gid: gid.to_string(),
            completed_length: completed,
            total_length: total,
            download_speed: speed,
            percentage,
            status: download_status,
        })
    }

    /// 暂停下载
    pub async fn pause(&self, gid: &str) -> Result<()> {
        if let Some(client) = &self.client {
            client.pause(gid).await?;
        }
        Ok(())
    }

    /// 恢复下载
    pub async fn resume(&self, gid: &str) -> Result<()> {
        if let Some(client) = &self.client {
            client.unpause(gid).await?;
        }
        Ok(())
    }

    /// 取消下载
    pub async fn cancel(&self, gid: &str) -> Result<()> {
        if let Some(client) = &self.client {
            client.remove(gid).await?;
        }
        Ok(())
    }

    /// 获取全局状态
    pub async fn get_global_stat(&self) -> Result<(u64, u64)> {
        if let Some(client) = &self.client {
            let stat = client.get_global_stat().await?;
            return Ok((stat.download_speed, stat.num_active as u64));
        }
        Ok((0, 0))
    }

    /// 关闭 aria2c
    pub async fn shutdown(&mut self) -> Result<()> {
        if let Some(client) = self.client.take() {
            let _ = client.shutdown().await;
        }
        if let Some(mut process) = self.aria2_process.take() {
            let _ = process.kill();
        }
        Ok(())
    }
}

impl Drop for Aria2Manager {
    fn drop(&mut self) {
        if let Some(mut process) = self.aria2_process.take() {
            let _ = process.kill();
        }
    }
}

/// 清理全局aria2管理器
pub async fn cleanup_global_aria2() {
    if let Some(global) = GLOBAL_ARIA2.get() {
        let mut guard = global.lock().await;
        if let Some(mut manager) = guard.take() {
            let _ = manager.shutdown().await;
        }
        ARIA2_WARMED_UP.store(false, Ordering::SeqCst);
    }
}
