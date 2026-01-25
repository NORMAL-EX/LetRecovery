use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

use crate::core::dism::DismProgress;
use crate::core::disk::Partition;
use crate::utils::command::new_command;
use crate::utils::encoding::gbk_to_utf8;
use crate::utils::path::get_bin_dir;

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
    ghost_path: String,
    cancel_flag: Arc<AtomicBool>,
}

impl Ghost {
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

    /// 检查 Ghost 是否可用
    pub fn is_available(&self) -> bool {
        Path::new(&self.ghost_path).exists()
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
    pub fn validate_image(&self, gho_file: &str) -> Result<()> {
        let path = Path::new(gho_file);

        if !path.exists() {
            return Err(GhostError::ImageNotFound(gho_file.to_string()).into());
        }

        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        if extension != "gho" && extension != "ghs" {
            return Err(
                GhostError::InvalidImage(format!("不支持的文件格式: .{}", extension)).into(),
            );
        }

        let metadata = std::fs::metadata(path).context("无法读取文件元数据")?;

        if metadata.len() < 512 {
            return Err(
                GhostError::InvalidImage("文件太小，不是有效的 GHO 文件".to_string()).into(),
            );
        }

        let mut file = std::fs::File::open(path).context("无法打开文件")?;
        let mut header = [0u8; 4];
        file.read_exact(&mut header).context("无法读取文件头")?;

        let is_valid = (header[0] == 0xFE && header[1] == 0xEF)
            || (header[0] == 0x47 && header[1] == 0x46)
            || (header[0] == 0xEB || header[0] == 0xE9);

        if !is_valid {
            if extension == "ghs" {
                return Ok(());
            }
            return Err(GhostError::InvalidImage(format!(
                "文件头无效: {:02X} {:02X} {:02X} {:02X}",
                header[0], header[1], header[2], header[3]
            ))
            .into());
        }

        Ok(())
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
            return Err(GhostError::InvalidPartition(format!(
                "无效的分区参数: 磁盘={}, 分区={}",
                disk_number, partition_number
            ))
            .into());
        }

        let target_partition = format!("{}:{}", disk_number, partition_number);

        log::info!("========================================");
        log::info!("开始恢复 GHO 镜像");
        log::info!("镜像文件: {}", gho_file);
        log::info!(
            "目标分区: {} (磁盘 {} 分区 {})",
            target_partition,
            disk_number,
            partition_number
        );
        log::info!("========================================");

        let file_size = std::fs::metadata(gho_file)
            .map(|m| m.len())
            .unwrap_or(0);
        let estimated_size = file_size * 2;

        if let Some(ref tx) = progress_tx {
            let _ = tx.send(DismProgress {
                percentage: 0,
                status: "释放系统镜像".to_string(),
            });
        }

        let clone_param = format!("-clone,mode=pload,src={},dst={}", gho_file, target_partition);

        log::info!(
            "执行命令: {} {} -sure -fx -batch",
            self.ghost_path,
            clone_param
        );

        let mut child = new_command(&self.ghost_path)
            .args([&clone_param, "-sure", "-fx", "-batch"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("无法启动 Ghost 进程")?;

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
        partitions: &[Partition],
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<()> {
        let letter = target_letter
            .trim_end_matches(['\\', '/'])
            .to_uppercase();
        let letter = if letter.ends_with(':') {
            letter
        } else {
            format!("{}:", letter)
        };

        log::info!("解析目标盘符: {}", letter);

        let partition = partitions
            .iter()
            .find(|p| p.letter.eq_ignore_ascii_case(&letter))
            .ok_or_else(|| GhostError::InvalidPartition(format!("找不到分区 {}", letter)))?;

        log::info!(
            "找到分区信息: letter={}, disk={:?}, partition={:?}",
            partition.letter,
            partition.disk_number,
            partition.partition_number
        );

        let disk_number = partition.disk_number.ok_or_else(|| {
            GhostError::InvalidPartition(format!(
                "无法获取 {} 的磁盘号，请刷新分区列表",
                letter
            ))
        })?;

        let partition_number = partition.partition_number.ok_or_else(|| {
            GhostError::InvalidPartition(format!(
                "无法获取 {} 的分区号，请刷新分区列表",
                letter
            ))
        })?;

        // Ghost 磁盘号从1开始
        let ghost_disk = disk_number + 1;
        let ghost_partition = partition_number;

        log::info!("转换分区格式:");
        log::info!(
            "  Windows: Disk {} Partition {}",
            disk_number,
            partition_number
        );
        log::info!("  Ghost:   {}:{}", ghost_disk, ghost_partition);

        self.restore_image(gho_file, ghost_disk, ghost_partition, progress_tx)
    }

    /// 从盘符创建GHO镜像（备份）
    pub fn create_image_from_letter(
        &self,
        source_letter: &str,
        gho_file: &str,
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<()> {
        use crate::core::disk::DiskManager;
        
        self.reset_cancel();

        if !self.is_available() {
            return Err(GhostError::ExecutableNotFound(self.ghost_path.clone()).into());
        }

        let letter = source_letter
            .trim_end_matches(['\\', '/'])
            .to_uppercase();
        let letter = if letter.ends_with(':') {
            letter
        } else {
            format!("{}:", letter)
        };

        log::info!("解析源盘符: {}", letter);

        // 获取分区列表
        let partitions = DiskManager::get_partitions()
            .map_err(|e| GhostError::ExecutionFailed(format!("获取分区列表失败: {}", e)))?;

        let partition = partitions
            .iter()
            .find(|p| p.letter.eq_ignore_ascii_case(&letter))
            .ok_or_else(|| GhostError::InvalidPartition(format!("找不到分区 {}", letter)))?;

        let disk_number = partition.disk_number.ok_or_else(|| {
            GhostError::InvalidPartition(format!(
                "无法获取 {} 的磁盘号",
                letter
            ))
        })?;

        let partition_number = partition.partition_number.ok_or_else(|| {
            GhostError::InvalidPartition(format!(
                "无法获取 {} 的分区号",
                letter
            ))
        })?;

        // Ghost 磁盘号从1开始
        let ghost_disk = disk_number + 1;
        let ghost_partition = partition_number;
        let source_partition = format!("{}:{}", ghost_disk, ghost_partition);

        log::info!("========================================");
        log::info!("开始创建 GHO 镜像");
        log::info!("源分区: {} ({})", letter, source_partition);
        log::info!("目标文件: {}", gho_file);
        log::info!("========================================");

        // 确保目标目录存在
        if let Some(parent) = Path::new(gho_file).parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| GhostError::ExecutionFailed(format!("创建目录失败: {}", e)))?;
        }

        // 估算备份时间（基于分区大小）
        let estimated_size = partition.total_size_mb * 1024 * 1024;
        let estimated_seconds = (estimated_size / (100 * 1024 * 1024)).max(60) as u64;

        if let Some(ref tx) = progress_tx {
            let _ = tx.send(DismProgress {
                percentage: 0,
                status: "正在备份系统镜像".to_string(),
            });
        }

        // Ghost 备份命令: -clone,mode=pdump,src=1:1,dst=xxx.gho
        let clone_param = format!("-clone,mode=pdump,src={},dst={}", source_partition, gho_file);

        log::info!(
            "执行命令: {} {} -z9 -sure -fx -batch",
            self.ghost_path,
            clone_param
        );

        let mut child = new_command(&self.ghost_path)
            .args([&clone_param, "-z9", "-sure", "-fx", "-batch"]) // -z9 高压缩
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("无法启动 Ghost 进程")?;

        let result = self.monitor_ghost_backup(&mut child, progress_tx, estimated_seconds);

        let _ = child.kill();
        let _ = child.wait();

        result
    }

    /// 监控 Ghost 备份进程并报告进度
    fn monitor_ghost_backup(
        &self,
        child: &mut Child,
        progress_tx: Option<Sender<DismProgress>>,
        estimated_seconds: u64,
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
                    log::debug!("GHOST STDERR: {}", line_utf8);
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
        let estimated_duration = Duration::from_secs(estimated_seconds);

        log::info!("预计备份时间: {} 秒", estimated_seconds);

        let mut last_progress: u8 = 0;

        loop {
            if cancel_flag.load(Ordering::SeqCst) {
                log::info!("收到取消请求，终止进程");
                let _ = child.kill();
                return Err(GhostError::Cancelled.into());
            }

            match child.try_wait() {
                Ok(Some(status)) => {
                    log::info!("进程退出，状态码: {:?}", status.code());

                    if let Some(handle) = stdout_handle {
                        let _ = handle.join();
                    }

                    if let Some(handle) = stderr_handle {
                        let _ = handle.join();
                    }

                    let stderr_output = stderr_content
                        .lock()
                        .map(|s| s.clone())
                        .unwrap_or_default();

                    if let Some(ref tx) = progress_tx {
                        let _ = tx.send(DismProgress {
                            percentage: 100,
                            status: "备份系统镜像完成".to_string(),
                        });
                    }

                    if status.success() || status.code() == Some(0) {
                        log::info!("========================================");
                        log::info!("镜像备份成功!");
                        log::info!("========================================");
                        return Ok(());
                    } else {
                        let error_msg = if stderr_output.trim().is_empty() {
                            format!("Ghost 进程异常退出，退出码: {:?}", status.code())
                        } else {
                            format!("Ghost 错误: {}", stderr_output.trim())
                        };
                        log::error!("备份失败: {}", error_msg);
                        return Err(GhostError::ExecutionFailed(error_msg).into());
                    }
                }
                Ok(None) => {
                    let elapsed = start_time.elapsed();
                    let progress =
                        ((elapsed.as_secs_f64() / estimated_duration.as_secs_f64()) * 95.0)
                            .min(95.0) as u8;

                    if progress > last_progress {
                        last_progress = progress;
                        log::debug!(
                            "进度: {}% (已运行 {:.0} 秒)",
                            progress,
                            elapsed.as_secs_f64()
                        );

                        if let Some(ref tx) = progress_tx {
                            let _ = tx.send(DismProgress {
                                percentage: progress,
                                status: "正在备份系统镜像".to_string(),
                            });
                        }
                    }
                }
                Err(e) => {
                    return Err(
                        GhostError::ExecutionFailed(format!("检查进程状态失败: {}", e)).into(),
                    );
                }
            }

            std::thread::sleep(Duration::from_millis(500));
        }
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
                    log::debug!("GHOST STDERR: {}", line_utf8);
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

        log::info!("预计恢复时间: {} 秒", estimated_seconds);

        let mut last_progress: u8 = 0;

        loop {
            if cancel_flag.load(Ordering::SeqCst) {
                log::info!("收到取消请求，终止进程");
                let _ = child.kill();
                return Err(GhostError::Cancelled.into());
            }

            match child.try_wait() {
                Ok(Some(status)) => {
                    log::info!("进程退出，状态码: {:?}", status.code());

                    if let Some(handle) = stdout_handle {
                        let _ = handle.join();
                    }

                    if let Some(handle) = stderr_handle {
                        let _ = handle.join();
                    }

                    let stderr_output = stderr_content
                        .lock()
                        .map(|s| s.clone())
                        .unwrap_or_default();

                    if let Some(ref tx) = progress_tx {
                        let _ = tx.send(DismProgress {
                            percentage: 100,
                            status: "释放系统镜像".to_string(),
                        });
                    }

                    if status.success() || status.code() == Some(0) {
                        log::info!("========================================");
                        log::info!("镜像恢复成功!");
                        log::info!("========================================");
                        return Ok(());
                    } else {
                        let error_msg = if stderr_output.trim().is_empty() {
                            format!("Ghost 进程异常退出，退出码: {:?}", status.code())
                        } else {
                            format!("Ghost 错误: {}", stderr_output.trim())
                        };
                        log::error!("恢复失败: {}", error_msg);
                        return Err(GhostError::ExecutionFailed(error_msg).into());
                    }
                }
                Ok(None) => {
                    let elapsed = start_time.elapsed();
                    let progress =
                        ((elapsed.as_secs_f64() / estimated_duration.as_secs_f64()) * 95.0)
                            .min(95.0) as u8;

                    if progress > last_progress {
                        last_progress = progress;
                        log::debug!(
                            "进度: {}% (已运行 {:.0} 秒)",
                            progress,
                            elapsed.as_secs_f64()
                        );

                        if let Some(ref tx) = progress_tx {
                            let _ = tx.send(DismProgress {
                                percentage: progress,
                                status: "释放系统镜像".to_string(),
                            });
                        }
                    }
                }
                Err(e) => {
                    return Err(
                        GhostError::ExecutionFailed(format!("检查进程状态失败: {}", e)).into(),
                    );
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
                log::debug!("GHOST STDOUT: {}", line_utf8);
                lines.push(line_utf8);
            }
        }

        lines
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
