//! Ghost 镜像操作模块
//! 
//! 提供 Ghost (.gho) 镜像文件的恢复功能，支持进度回调和取消操作。
//! 
//! # 功能
//! - 验证 GHO 文件有效性
//! - 获取 GHO 镜像信息
//! - 恢复 GHO 镜像到指定分区（支持进度回调）
//! - 支持取消正在进行的操作

use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

use crate::core::dism::DismProgress;
use crate::tr;
use crate::utils::cmd::create_command;
use crate::utils::encoding::gbk_to_utf8;
use crate::utils::path::get_bin_dir;

/// Ghost 进度信息
#[derive(Debug, Clone)]
pub struct GhostProgress {
    /// 当前进度百分比 (0-100)
    pub percentage: u8,
    /// 当前状态描述
    pub status: String,
    /// 已处理的数据量（字节）
    pub bytes_processed: u64,
    /// 总数据量（字节）
    pub bytes_total: u64,
    /// 当前速度（字节/秒）
    pub speed: u64,
}

impl Default for GhostProgress {
    fn default() -> Self {
        Self {
            percentage: 0,
            status: String::new(),
            bytes_processed: 0,
            bytes_total: 0,
            speed: 0,
        }
    }
}

impl From<GhostProgress> for DismProgress {
    fn from(gp: GhostProgress) -> Self {
        DismProgress {
            percentage: gp.percentage,
            status: gp.status,
        }
    }
}

/// GHO 镜像信息
#[derive(Debug, Clone)]
pub struct GhoImageInfo {
    /// 文件路径
    pub file_path: String,
    /// 文件大小（字节）
    pub file_size: u64,
    /// 镜像描述
    pub description: String,
    /// 原始分区大小估算（字节）
    pub original_size: u64,
    /// 压缩比估算
    pub compression_ratio: f32,
}

/// Ghost 错误类型
#[derive(Debug, thiserror::Error)]
pub enum GhostError {
    #[error("Ghost 可执行文件不存在: {0}")]
    ExecutableNotFound(String),
    
    #[error("GHO 文件不存在: {0}")]
    ImageNotFound(String),
    
    #[error("GHO 文件无效或损坏: {0}")]
    InvalidImage(String),
    
    #[error("目标分区无效: {0}")]
    InvalidPartition(String),
    
    #[error("Ghost 执行失败: {0}")]
    ExecutionFailed(String),
    
    #[error("操作被用户取消")]
    Cancelled,
    
    #[error("IO 错误: {0}")]
    IoError(#[from] std::io::Error),
}

/// Ghost 镜像操作管理器
pub struct Ghost {
    /// Ghost64.exe 路径
    ghost_path: String,
    /// 取消标志
    cancel_flag: Arc<AtomicBool>,
}

impl Ghost {
    /// 创建新的 Ghost 实例
    pub fn new() -> Self {
        let bin_dir = get_bin_dir();
        Self {
            ghost_path: bin_dir
                .join("ghost")
                .join("ghost64.exe")
                .to_string_lossy()
                .to_string(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 使用自定义路径创建 Ghost 实例
    pub fn with_path(ghost_path: &str) -> Self {
        Self {
            ghost_path: ghost_path.to_string(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 检查 Ghost 是否可用
    pub fn is_available(&self) -> bool {
        Path::new(&self.ghost_path).exists()
    }

    /// 获取 Ghost 可执行文件路径
    pub fn get_ghost_path(&self) -> &str {
        &self.ghost_path
    }

    /// 获取取消标志的克隆（用于外部控制取消）
    pub fn get_cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel_flag)
    }

    /// 请求取消当前操作
    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::SeqCst);
    }

    /// 重置取消标志
    pub fn reset_cancel(&self) {
        self.cancel_flag.store(false, Ordering::SeqCst);
    }

    /// 验证 GHO 文件
    /// 
    /// 检查文件是否存在、是否可读、以及是否具有有效的 GHO 文件头
    pub fn validate_image(&self, gho_file: &str) -> Result<()> {
        let path = Path::new(gho_file);
        
        // 检查文件存在
        if !path.exists() {
            return Err(GhostError::ImageNotFound(gho_file.to_string()).into());
        }

        // 检查文件扩展名
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();
        
        if extension != "gho" && extension != "ghs" {
            return Err(GhostError::InvalidImage(
                tr!("不支持的文件格式: .{}", extension)
            ).into());
        }

        // 检查文件大小（GHO 文件至少应该有头部信息）
        let metadata = std::fs::metadata(path)
            .context(tr!("无法读取文件元数据"))?;

        if metadata.len() < 512 {
            return Err(GhostError::InvalidImage(
                tr!("文件太小，不是有效的 GHO 文件")
            ).into());
        }

        // 读取并验证 GHO 文件头
        let mut file = std::fs::File::open(path)
            .context(tr!("无法打开文件"))?;

        let mut header = [0u8; 4];
        file.read_exact(&mut header)
            .context(tr!("无法读取文件头"))?;

        // Ghost 文件签名检查
        let is_valid = (header[0] == 0xFE && header[1] == 0xEF)
            || (header[0] == 0x47 && header[1] == 0x46)
            || (header[0] == 0xEB || header[0] == 0xE9);

        if !is_valid {
            if extension == "ghs" {
                return Ok(());
            }
            return Err(GhostError::InvalidImage(
                tr!("文件头无效: {} {} {} {}",
                    format!("{:02X}", header[0]),
                    format!("{:02X}", header[1]),
                    format!("{:02X}", header[2]),
                    format!("{:02X}", header[3]))
            ).into());
        }

        Ok(())
    }

    /// 获取 GHO 镜像信息
    pub fn get_image_info(&self, gho_file: &str) -> Result<GhoImageInfo> {
        self.validate_image(gho_file)?;

        let path = Path::new(gho_file);
        let metadata = std::fs::metadata(path)?;
        let file_size = metadata.len();

        let mut info = GhoImageInfo {
            file_path: gho_file.to_string(),
            file_size,
            description: String::new(),
            original_size: 0,
            compression_ratio: 0.0,
        };

        info.original_size = file_size * 2;
        info.compression_ratio = 0.5;
        info.description = tr!("GHO 镜像 - {} GB (压缩后)", format!("{:.1}", file_size as f64 / 1024.0 / 1024.0 / 1024.0));

        Ok(info)
    }

    /// 恢复 GHO 镜像到指定分区
    pub fn restore_image(
        &self,
        gho_file: &str,
        disk_number: u32,
        partition_number: u32,
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<()> {
        self.reset_cancel();

        if !self.is_available() {
            return Err(GhostError::ExecutableNotFound(self.ghost_path.clone()).into());
        }

        self.validate_image(gho_file)?;

        if disk_number == 0 || partition_number == 0 {
            return Err(GhostError::InvalidPartition(
                tr!("无效的分区参数: 磁盘={}, 分区={}", disk_number, partition_number)
            ).into());
        }

        let target_partition = format!("{}:{}", disk_number, partition_number);
        
        log::info!("[GHOST] ========================================");
        log::info!("[GHOST] 开始恢复 GHO 镜像");
        log::info!("[GHOST] 镜像文件: {}", gho_file);
        log::info!("[GHOST] 目标分区: {} (磁盘 {} 分区 {})", target_partition, disk_number, partition_number);
        log::info!("[GHOST] Ghost 路径: {}", self.ghost_path);
        log::info!("[GHOST] ========================================");

        let image_info = self.get_image_info(gho_file).ok();
        let estimated_size = image_info.as_ref().map(|i| i.original_size).unwrap_or(0);

        if let Some(ref tx) = progress_tx {
            let _ = tx.send(DismProgress {
                percentage: 0,
                status: format!("STEP:3:{}", tr!("释放系统镜像")),
            });
        }

        let clone_param = format!(
            "-clone,mode=pload,src={},dst={}",
            gho_file, target_partition
        );

        log::info!("[GHOST] 执行命令: {} {} -sure -fx -batch", self.ghost_path, clone_param);

        let mut child = create_command(&self.ghost_path)
            .args([&clone_param, "-sure", "-fx", "-batch"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context(tr!("无法启动 Ghost 进程"))?;

        let result = self.monitor_ghost_process(&mut child, progress_tx, estimated_size);

        let _ = child.kill();
        let _ = child.wait();

        result
    }

    /// 使用盘符恢复 GHO 镜像
    pub fn restore_image_to_letter(
        &self,
        gho_file: &str,
        target_letter: &str,
        partitions: &[crate::core::disk::Partition],
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<()> {
        let letter = target_letter.trim_end_matches(['\\', '/']).to_uppercase();
        let letter = if letter.ends_with(':') {
            letter
        } else {
            format!("{}:", letter)
        };

        log::info!("[GHOST] 解析目标盘符: {}", letter);

        let partition = partitions
            .iter()
            .find(|p| p.letter.eq_ignore_ascii_case(&letter))
            .ok_or_else(|| GhostError::InvalidPartition(
                tr!("找不到分区 {}", letter)
            ))?;

        log::info!("[GHOST] 找到分区信息: letter={}, disk={:?}, partition={:?}",
            partition.letter, partition.disk_number, partition.partition_number);

        let disk_number = partition.disk_number.ok_or_else(|| {
            GhostError::InvalidPartition(tr!("无法获取 {} 的磁盘号，请刷新分区列表", letter))
        })?;

        let partition_number = partition.partition_number.ok_or_else(|| {
            GhostError::InvalidPartition(tr!("无法获取 {} 的分区号，请刷新分区列表", letter))
        })?;

        let ghost_disk = disk_number + 1;
        let ghost_partition = partition_number;

        log::info!("[GHOST] 转换分区格式:");
        log::info!("[GHOST]   Windows: Disk {} Partition {}", disk_number, partition_number);
        log::info!("[GHOST]   Ghost:   {}:{}", ghost_disk, ghost_partition);

        self.restore_image(gho_file, ghost_disk, ghost_partition, progress_tx)
    }

    /// 监控 Ghost 进程并报告进度
    fn monitor_ghost_process(
        &self,
        child: &mut Child,
        progress_tx: Option<Sender<DismProgress>>,
        estimated_size: u64,
    ) -> Result<()> {
        let cancel_flag = Arc::clone(&self.cancel_flag);
        
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let stdout_handle = if let Some(stdout) = stdout {
            let cancel = Arc::clone(&cancel_flag);
            Some(std::thread::spawn(move || {
                Self::read_ghost_output(stdout, cancel)
            }))
        } else {
            None
        };

        let stderr_content = Arc::new(std::sync::Mutex::new(String::new()));
        let stderr_content_clone = Arc::clone(&stderr_content);
        
        let stderr_handle = if let Some(stderr) = stderr {
            Some(std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    let line_utf8 = gbk_to_utf8(line.as_bytes());
                    log::debug!("[GHOST STDERR] {}", line_utf8);
                    if let Ok(mut content) = stderr_content_clone.lock() {
                        content.push_str(&line_utf8);
                        content.push('\n');
                    }
                }
            }))
        } else {
            None
        };

        let start_time = std::time::Instant::now();
        
        let estimated_seconds = if estimated_size > 0 {
            (estimated_size / (100 * 1024 * 1024)).max(60) as u64
        } else {
            300
        };
        let estimated_duration = Duration::from_secs(estimated_seconds);
        
        log::info!("[GHOST] 预计恢复时间: {} 秒", estimated_seconds);
        
        let mut last_progress: u8 = 0;

        loop {
            if cancel_flag.load(Ordering::SeqCst) {
                log::info!("[GHOST] 收到取消请求，终止进程");
                let _ = child.kill();
                return Err(GhostError::Cancelled.into());
            }

            match child.try_wait() {
                Ok(Some(status)) => {
                    log::info!("[GHOST] 进程退出，状态码: {:?}", status.code());
                    
                    if let Some(handle) = stdout_handle {
                        let _ = handle.join();
                    }
                    
                    if let Some(handle) = stderr_handle {
                        let _ = handle.join();
                    }
                    
                    let stderr_output = stderr_content.lock()
                        .map(|s| s.clone())
                        .unwrap_or_default();

                    if let Some(ref tx) = progress_tx {
                        let _ = tx.send(DismProgress {
                            percentage: 100,
                            status: format!("STEP:3:{}", tr!("释放系统镜像")),
                        });
                    }

                    if status.success() || status.code() == Some(0) {
                        log::info!("[GHOST] ========================================");
                        log::info!("[GHOST] 镜像恢复成功!");
                        log::info!("[GHOST] ========================================");
                        return Ok(());
                    } else {
                        let error_msg = if stderr_output.trim().is_empty() {
                            tr!("Ghost 进程异常退出，退出码: {}", format!("{:?}", status.code()))
                        } else {
                            tr!("Ghost 错误: {}", stderr_output.trim())
                        };
                        log::error!("[GHOST] 恢复失败: {}", error_msg);
                        return Err(GhostError::ExecutionFailed(error_msg).into());
                    }
                }
                Ok(None) => {
                    let elapsed = start_time.elapsed();
                    let progress = ((elapsed.as_secs_f64() / estimated_duration.as_secs_f64()) * 95.0)
                        .min(95.0) as u8;
                    
                    if progress > last_progress {
                        last_progress = progress;
                        log::info!("[GHOST] 进度: {}% (已运行 {:.0} 秒)", progress, elapsed.as_secs_f64());
                        
                        if let Some(ref tx) = progress_tx {
                            let _ = tx.send(DismProgress {
                                percentage: progress,
                                status: format!("STEP:3:{}", tr!("释放系统镜像")),
                            });
                        }
                    }
                }
                Err(e) => {
                    return Err(GhostError::ExecutionFailed(
                        tr!("检查进程状态失败: {}", e)
                    ).into());
                }
            }

            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// 读取 Ghost 输出
    fn read_ghost_output<R: Read>(reader: R, cancel_flag: Arc<AtomicBool>) -> Vec<String> {
        let reader = BufReader::new(reader);
        let mut lines = Vec::new();
        
        for line in reader.lines() {
            if cancel_flag.load(Ordering::SeqCst) {
                break;
            }

            if let Ok(line) = line {
                let line_utf8 = gbk_to_utf8(line.as_bytes());
                log::debug!("[GHOST STDOUT] {}", line_utf8);
                lines.push(line_utf8);
            }
        }
        
        lines
    }

    /// 创建 GHO 镜像（备份功能）
    pub fn create_image(
        &self,
        disk_number: u32,
        partition_number: u32,
        gho_file: &str,
        compression: u8,
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<()> {
        self.reset_cancel();

        if !self.is_available() {
            return Err(GhostError::ExecutableNotFound(self.ghost_path.clone()).into());
        }

        if disk_number == 0 || partition_number == 0 {
            return Err(GhostError::InvalidPartition(
                tr!("无效的分区参数: 磁盘={}, 分区={}", disk_number, partition_number)
            ).into());
        }

        if let Some(parent) = Path::new(gho_file).parent() {
            std::fs::create_dir_all(parent)
                .context(tr!("无法创建输出目录"))?;
        }

        let source_partition = format!("{}:{}", disk_number, partition_number);
        
        log::info!("[GHOST] ========================================");
        log::info!("[GHOST] 开始创建 GHO 镜像");
        log::info!("[GHOST] 源分区: {} (磁盘 {} 分区 {})", source_partition, disk_number, partition_number);
        log::info!("[GHOST] 输出文件: {}", gho_file);
        log::info!("[GHOST] 压缩级别: {}", compression);
        log::info!("[GHOST] ========================================");

        if let Some(ref tx) = progress_tx {
            let _ = tx.send(DismProgress {
                percentage: 0,
                status: tr!("正在准备备份..."),
            });
        }

        let compression = compression.clamp(1, 9);

        let clone_param = format!(
            "-clone,mode=pdump,src={},dst={}",
            source_partition, gho_file
        );

        let mut child = create_command(&self.ghost_path)
            .args([&clone_param, "-sure", "-fx", "-batch", &format!("-z{}", compression)])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context(tr!("无法启动 Ghost 进程"))?;

        let result = self.monitor_ghost_process(&mut child, progress_tx, 0);

        let _ = child.kill();
        let _ = child.wait();

        result
    }

    /// 从盘符创建 GHO 镜像（备份）：解析盘符 → 磁盘/分区号后调用 create_image。
    /// 与 PE 端 `Ghost::create_image_from_letter` 等价，供桌面 Direct 备份按格式分发使用。
    pub fn create_image_from_letter(
        &self,
        source_letter: &str,
        gho_file: &str,
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<()> {
        let letter = source_letter.trim_end_matches(['\\', '/']).to_uppercase();
        let letter = if letter.ends_with(':') {
            letter
        } else {
            format!("{}:", letter)
        };

        let partitions = crate::core::disk::DiskManager::get_partitions()
            .map_err(|e| anyhow::anyhow!("{}", tr!("获取分区列表失败: {}", e)))?;
        let partition = partitions
            .iter()
            .find(|p| p.letter.eq_ignore_ascii_case(&letter))
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("找不到分区 {}", letter)))?;

        let disk_number = partition
            .disk_number
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("无法获取 {} 的磁盘号，请刷新分区列表", letter)))?;
        let partition_number = partition
            .partition_number
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("无法获取 {} 的分区号，请刷新分区列表", letter)))?;

        // Ghost 磁盘号从 1 开始（create_image 内部直接把它当作 ghost src 的磁盘号）
        self.create_image(disk_number + 1, partition_number, gho_file, 9, progress_tx)
    }
}

impl Default for Ghost {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Ghost {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ghost_new() {
        let ghost = Ghost::new();
        assert!(ghost.ghost_path.contains("ghost64.exe"));
    }

    #[test]
    fn test_cancel_flag() {
        let ghost = Ghost::new();
        assert!(!ghost.cancel_flag.load(Ordering::SeqCst));
        
        ghost.cancel();
        assert!(ghost.cancel_flag.load(Ordering::SeqCst));
        
        ghost.reset_cancel();
        assert!(!ghost.cancel_flag.load(Ordering::SeqCst));
    }
}
