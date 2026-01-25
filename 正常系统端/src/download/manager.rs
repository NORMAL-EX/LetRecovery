use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use super::aria2::{Aria2Manager, DownloadProgress, DownloadStatus};

/// 下载任务
#[derive(Debug, Clone)]
pub struct DownloadTask {
    pub gid: String,
    pub url: String,
    pub filename: String,
    pub save_path: String,
    pub progress: DownloadProgress,
}

/// 下载管理器
pub struct DownloadManager {
    aria2: Arc<Mutex<Option<Aria2Manager>>>,
    tasks: Arc<Mutex<HashMap<String, DownloadTask>>>,
}

impl DownloadManager {
    pub fn new() -> Self {
        Self {
            aria2: Arc::new(Mutex::new(None)),
            tasks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 初始化 aria2
    pub async fn init(&self) -> Result<()> {
        let manager = Aria2Manager::start().await?;
        *self.aria2.lock().await = Some(manager);
        Ok(())
    }

    /// 添加下载任务
    pub async fn add_task(
        &self,
        url: &str,
        save_dir: &str,
        filename: Option<&str>,
    ) -> Result<String> {
        let aria2 = self.aria2.lock().await;
        let aria2 = aria2
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("aria2 not initialized"))?;

        let gid = aria2.add_download(url, save_dir, filename).await?;

        let task = DownloadTask {
            gid: gid.clone(),
            url: url.to_string(),
            filename: filename.unwrap_or("").to_string(),
            save_path: save_dir.to_string(),
            progress: DownloadProgress {
                gid: gid.clone(),
                completed_length: 0,
                total_length: 0,
                download_speed: 0,
                percentage: 0.0,
                status: DownloadStatus::Waiting,
            },
        };

        self.tasks.lock().await.insert(gid.clone(), task);
        Ok(gid)
    }

    /// 获取任务进度
    pub async fn get_progress(&self, gid: &str) -> Result<DownloadProgress> {
        let aria2 = self.aria2.lock().await;
        let aria2 = aria2
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("aria2 not initialized"))?;

        let progress = aria2.get_status(gid).await?;

        // 更新任务信息
        if let Some(task) = self.tasks.lock().await.get_mut(gid) {
            task.progress = progress.clone();
        }

        Ok(progress)
    }

    /// 暂停任务
    pub async fn pause_task(&self, gid: &str) -> Result<()> {
        let aria2 = self.aria2.lock().await;
        if let Some(aria2) = aria2.as_ref() {
            aria2.pause(gid).await?;
        }
        Ok(())
    }

    /// 恢复任务
    pub async fn resume_task(&self, gid: &str) -> Result<()> {
        let aria2 = self.aria2.lock().await;
        if let Some(aria2) = aria2.as_ref() {
            aria2.resume(gid).await?;
        }
        Ok(())
    }

    /// 取消任务
    pub async fn cancel_task(&self, gid: &str) -> Result<()> {
        let aria2 = self.aria2.lock().await;
        if let Some(aria2) = aria2.as_ref() {
            aria2.cancel(gid).await?;
        }
        self.tasks.lock().await.remove(gid);
        Ok(())
    }

    /// 获取所有任务
    pub async fn get_all_tasks(&self) -> Vec<DownloadTask> {
        self.tasks.lock().await.values().cloned().collect()
    }

    /// 关闭
    pub async fn shutdown(&self) -> Result<()> {
        let mut aria2 = self.aria2.lock().await;
        if let Some(mut manager) = aria2.take() {
            manager.shutdown().await?;
        }
        Ok(())
    }

    /// 检查是否已初始化
    pub async fn is_initialized(&self) -> bool {
        self.aria2.lock().await.is_some()
    }
}

impl Default for DownloadManager {
    fn default() -> Self {
        Self::new()
    }
}
