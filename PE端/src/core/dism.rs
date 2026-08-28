//! 镜像操作模块
//!
//! 该模块封装了 Windows 系统镜像操作功能：
//! - 镜像释放/应用：使用 wimlib (libwim-15.dll)
//! - 镜像备份/捕获：使用 wimlib (libwim-15.dll)
//! - 驱动导入：使用 dism.exe 命令行（PE 环境兼容性最佳）
//! - CAB 包安装：使用 dism.exe 命令行

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use crate::core::dism_exe::{DismExe, DismExeProgress};
use crate::tr;
use lr_core::image_meta::{WimProgress, WIM_COMPRESS_LZMS, WIM_COMPRESS_LZX};
use lr_core::WimEngineManager;

/// 操作进度
#[derive(Debug, Clone)]
pub struct DismProgress {
    pub percentage: u8,
    pub status: String,
}

pub struct Dism;

const VERIFY_OUT_OF_MEMORY_MAX_ATTEMPTS: u8 = 2;

/// A current ViaPE handoff hashes every locked public artifact against its authenticated manifest.
/// Repeating full decompression in WinPE adds no new fact when the normal endpoint has also
/// authenticated that those exact bytes passed full verification. Legacy handoffs keep the old
/// behavior because the absent receipt defaults to false.
pub(crate) fn requires_pe_image_verification(
    source_image_verified: bool,
    is_gho: bool,
    is_xp_i386: bool,
) -> bool {
    !is_gho && !is_xp_i386 && !source_image_verified
}

fn should_retry_verify_error(error_code: i32, attempt: u8) -> bool {
    error_code == lr_core::wimlib::WIMLIB_ERR_NOMEM && attempt < VERIFY_OUT_OF_MEMORY_MAX_ATTEMPTS
}

#[derive(Debug, thiserror::Error)]
pub enum ImageVerificationError {
    #[error("可用内存不足，连续 {attempts} 次无法完成镜像校验：{detail}")]
    OutOfMemory { attempts: u8, detail: String },
    #[error("{0}")]
    Other(String),
}

impl ImageVerificationError {
    pub fn is_out_of_memory(&self) -> bool {
        matches!(self, Self::OutOfMemory { .. })
    }
}

impl Dism {
    pub fn new() -> Self {
        Self
    }

    // ========================================================================
    // 镜像操作 - 使用 wimlib (libwim-15.dll)
    // ========================================================================

    /// 校验镜像完整性（WIM/ESD）。会逐流解压并核对 SHA-1，能发现“解压到一半损坏”
    /// 这类块级损坏（即使 ESD 没有完整性表）。校验失败即说明镜像已损坏/不完整。
    pub fn verify_image(
        &self,
        image_file: &str,
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<(), ImageVerificationError> {
        use lr_core::wimlib::Wimlib;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::time::Duration;

        log::info!("[Dism] 校验镜像完整性: {}", image_file);

        let lib = Wimlib::new()
            .map_err(|error| ImageVerificationError::Other(tr!("wimlib 初始化失败: {}", error)))?;

        // 进度监控线程：读取 wimlib 全局校验进度并上报
        let done = Arc::new(AtomicBool::new(false));
        let done_mon = Arc::clone(&done);
        let tx = progress_tx.clone();
        let monitor = std::thread::spawn(move || {
            let mut last = 0u8;
            loop {
                if done_mon.load(Ordering::SeqCst) {
                    break;
                }
                let p = Wimlib::get_global_progress();
                if p > last {
                    last = p;
                    if let Some(ref t) = tx {
                        let _ = t.send(DismProgress {
                            percentage: p,
                            status: tr!("正在校验镜像 ({}%)...", p),
                        });
                    }
                }
                if p >= 100 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });

        let result = 'verify: {
            for attempt in 1..=VERIFY_OUT_OF_MEMORY_MAX_ATTEMPTS {
                let handle = lib.open_wim(image_file).map_err(|error| {
                    ImageVerificationError::Other(tr!("打开镜像失败: {}", error))
                })?;
                match handle.verify_detailed() {
                    Ok(()) => break 'verify Ok(()),
                    Err(error) if should_retry_verify_error(error.code(), attempt) => {
                        log::warn!(
                            "[Dism] 镜像校验遇到瞬时内存不足，将在释放句柄后重试 ({}/{}): {}",
                            attempt,
                            VERIFY_OUT_OF_MEMORY_MAX_ATTEMPTS,
                            error
                        );
                        if let Some(ref sender) = progress_tx {
                            let _ = sender.send(DismProgress {
                                percentage: 0,
                                status: tr!("可用内存不足，正在释放资源并重试镜像校验..."),
                            });
                        }
                        drop(handle);
                        std::thread::sleep(Duration::from_secs(2));
                    }
                    Err(error) if error.code() == lr_core::wimlib::WIMLIB_ERR_NOMEM => {
                        break 'verify Err(ImageVerificationError::OutOfMemory {
                            attempts: attempt,
                            detail: error.to_string(),
                        });
                    }
                    Err(error) => {
                        break 'verify Err(ImageVerificationError::Other(error.to_string()));
                    }
                }
            }
            unreachable!("bounded verification attempts always return on their final attempt");
        };
        done.store(true, Ordering::SeqCst);
        let _ = monitor.join();

        match result {
            Ok(_) => {
                log::info!("[Dism] 镜像校验通过");
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// 应用系统镜像 (WIM/ESD)
    /// 使用 wimlib (libwim-15.dll) 实现
    pub fn apply_image_with_exact_swm_resources(
        &self,
        image_file: &str,
        exact_resource_files: &[PathBuf],
        apply_dir: &str,
        index: u32,
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<()> {
        self.apply_image_internal(
            image_file,
            Some(exact_resource_files),
            apply_dir,
            index,
            progress_tx,
        )
    }

    fn apply_image_internal(
        &self,
        image_file: &str,
        exact_resource_files: Option<&[PathBuf]>,
        apply_dir: &str,
        index: u32,
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<()> {
        log::info!("[Dism] 应用镜像: {} -> {}", image_file, apply_dir);

        let wim_manager = WimEngineManager::new_current()
            .map_err(|e| anyhow::anyhow!("{}", tr!("镜像引擎初始化失败: {}", e)))?;

        // 创建进度转换通道
        let (wim_tx, wim_rx) = std::sync::mpsc::channel::<WimProgress>();

        // 启动进度转发线程
        let progress_tx_clone = progress_tx.clone();
        let forward_thread = std::thread::spawn(move || {
            while let Ok(progress) = wim_rx.recv() {
                if let Some(ref tx) = progress_tx_clone {
                    let _ = tx.send(DismProgress {
                        percentage: progress.percentage,
                        status: progress.status,
                    });
                }
            }
        });

        // 应用镜像
        let result = match exact_resource_files {
            Some(resources) if image_file.to_ascii_lowercase().ends_with(".swm") => wim_manager
                .apply_image_with_exact_swm_resources(
                    image_file,
                    resources,
                    apply_dir,
                    index,
                    Some(wim_tx),
                ),
            _ => wim_manager.apply_image(image_file, apply_dir, index, Some(wim_tx)),
        };

        // 等待转发线程结束
        let _ = forward_thread.join();

        match result {
            Ok(_) => {
                log::info!("[Dism] 镜像应用成功");
                Ok(())
            }
            Err(e) => {
                anyhow::bail!("{}", tr!("镜像应用失败: {}", e))
            }
        }
    }

    fn capture_image_raw(
        &self,
        image_file: &str,
        capture_dir: &str,
        name: &str,
        description: &str,
        compression: u32,
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<()> {
        log::info!("[Dism] 捕获镜像: {} -> {}", capture_dir, image_file);

        let wim_manager = WimEngineManager::new_current()
            .map_err(|e| anyhow::anyhow!("{}", tr!("镜像引擎初始化失败: {}", e)))?;

        let (wim_tx, wim_rx) = std::sync::mpsc::channel::<WimProgress>();

        let progress_tx_clone = progress_tx.clone();
        let forward_thread = std::thread::spawn(move || {
            while let Ok(progress) = wim_rx.recv() {
                if let Some(ref tx) = progress_tx_clone {
                    let _ = tx.send(DismProgress {
                        percentage: progress.percentage,
                        status: progress.status,
                    });
                }
            }
        });

        let result = wim_manager.capture_image(
            capture_dir,
            image_file,
            name,
            description,
            compression,
            Some(wim_tx),
        );

        let _ = forward_thread.join();

        match result {
            Ok(_) => {
                log::info!("[Dism] 镜像捕获成功");
                Ok(())
            }
            Err(e) => {
                anyhow::bail!("{}", tr!("镜像捕获失败: {}", e))
            }
        }
    }

    /// Capture directly into an already private, same-volume publication directory.
    /// The backup workflow performs completed-image verification and handle-bound CAS.
    pub(crate) fn capture_image_staged(
        &self,
        image_file: &str,
        capture_dir: &str,
        name: &str,
        description: &str,
        esd: bool,
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<()> {
        self.capture_image_raw(
            image_file,
            capture_dir,
            name,
            description,
            if esd {
                WIM_COMPRESS_LZMS
            } else {
                WIM_COMPRESS_LZX
            },
            progress_tx,
        )
    }

    pub fn read_verified_backup_catalog(
        path: &Path,
    ) -> Result<lr_core::backup_image_catalog::BackupImageCatalog> {
        lr_core::wimlib::read_verified_backup_catalog(path).map_err(anyhow::Error::msg)
    }

    pub fn add_drivers_offline(&self, image_path: &str, driver_path: &str) -> Result<()> {
        log::info!(
            "[Dism] 使用 dism.exe 离线导入驱动: {} -> {}",
            driver_path,
            image_path
        );

        // 使用 dism.exe 命令行方式导入驱动
        let dism_exe =
            DismExe::new().map_err(|e| anyhow::anyhow!("{}", tr!("dism.exe 初始化失败: {}", e)))?;

        dism_exe.add_drivers_from_directory_resilient(image_path, driver_path, None)?;

        log::info!("[Dism] 离线驱动导入完成");
        Ok(())
    }

    pub fn add_preserved_driver_inf_files_offline_with_progress(
        &self,
        image_path: &str,
        inf_files: &[std::path::PathBuf],
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<super::dism_exe::PreservedDriverImportResult> {
        let dism_exe =
            DismExe::new().map_err(|e| anyhow::anyhow!("{}", tr!("dism.exe 初始化失败: {}", e)))?;
        let (exe_tx, exe_rx) = std::sync::mpsc::channel::<DismExeProgress>();
        let progress_tx_clone = progress_tx.clone();
        let forward_thread = std::thread::spawn(move || {
            while let Ok(progress) = exe_rx.recv() {
                if let Some(ref tx) = progress_tx_clone {
                    let _ = tx.send(DismProgress {
                        percentage: progress.percentage,
                        status: progress.status,
                    });
                }
            }
        });
        let result =
            dism_exe.add_preserved_driver_inf_files_resilient(image_path, inf_files, Some(exe_tx));
        let _ = forward_thread.join();
        result
    }

    /// 在一个 DISM servicing 会话中按依赖顺序添加多个 CAB 更新包。
    pub fn add_packages_offline_ordered(
        &self,
        image_path: &str,
        cab_paths: &[std::path::PathBuf],
    ) -> Result<()> {
        if cab_paths.is_empty() {
            anyhow::bail!("no ordered CAB packages were supplied");
        }
        log::info!(
            "[Dism] 在单个 servicing 会话中按顺序安装 {} 个 CAB 更新包 -> {}",
            cab_paths.len(),
            image_path
        );
        for (index, cab) in cab_paths.iter().enumerate() {
            log::info!(
                "[Dism] 有序 CAB {}/{}: {}",
                index + 1,
                cab_paths.len(),
                cab.display()
            );
        }
        let dism_exe =
            DismExe::new().map_err(|error| anyhow::anyhow!("dism.exe 初始化失败: {error}"))?;
        dism_exe.add_packages_offline_ordered(image_path, cab_paths, false, None)?;
        log::info!("[Dism] 有序 CAB servicing 会话完成");
        Ok(())
    }

    /// Install only the exact package paths retained by the authenticated task.
    /// No directory enumeration is performed here, so a late same-directory CAB cannot be
    /// incorporated into the servicing operation.
    pub fn add_optional_package_paths_offline(
        &self,
        image_path: &str,
        package_paths: &[std::path::PathBuf],
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<(usize, usize)> {
        if package_paths.is_empty() {
            return Ok((0, 0));
        }
        let dism_exe =
            DismExe::new().map_err(|e| anyhow::anyhow!("{}", tr!("dism.exe 初始化失败: {}", e)))?;
        let (exe_tx, exe_rx) = std::sync::mpsc::channel::<DismExeProgress>();
        let progress_tx_clone = progress_tx.clone();
        let forward_thread = std::thread::spawn(move || {
            while let Ok(progress) = exe_rx.recv() {
                if let Some(ref tx) = progress_tx_clone {
                    let _ = tx.send(DismProgress {
                        percentage: progress.percentage,
                        status: progress.status,
                    });
                }
            }
        });
        let result = dism_exe.add_packages_batch(image_path, package_paths, Some(exe_tx));
        let _ = forward_thread.join();
        result
    }
}

impl Default for Dism {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        requires_pe_image_verification, should_retry_verify_error,
        VERIFY_OUT_OF_MEMORY_MAX_ATTEMPTS,
    };

    #[test]
    fn authenticated_normal_endpoint_receipt_stops_duplicate_pe_verification() {
        assert!(!requires_pe_image_verification(true, false, false));
        assert!(requires_pe_image_verification(false, false, false));
    }

    #[test]
    fn non_wim_sources_never_enter_wimlib_verification() {
        assert!(!requires_pe_image_verification(false, true, false));
        assert!(!requires_pe_image_verification(false, false, true));
    }

    #[test]
    fn retries_only_the_first_explicit_out_of_memory_failure() {
        assert!(should_retry_verify_error(
            lr_core::wimlib::WIMLIB_ERR_NOMEM,
            1
        ));
        assert!(!should_retry_verify_error(
            lr_core::wimlib::WIMLIB_ERR_NOMEM,
            VERIFY_OUT_OF_MEMORY_MAX_ATTEMPTS
        ));
        assert!(!should_retry_verify_error(
            lr_core::wimlib::WIMLIB_ERR_INTEGRITY,
            1
        ));
    }
}
