//! 镜像操作模块
//!
//! 该模块封装了 Windows 系统镜像操作功能：
//! - 镜像释放/应用：使用 wimlib (libwim-15.dll)
//! - 镜像备份/捕获：使用 wimlib (libwim-15.dll)
//! - 离线驱动导入：优先使用当前 Windows/WinPE 自带的 dism.exe
//! - 离线 CAB 包导入：使用 dism.exe 命令行
//! - 镜像信息获取：使用 wimlib (libwim-15.dll) + WIM XML 解析
//! - 系统信息获取：使用 advapi32.dll (离线注册表)

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use crate::core::dism_cmd::DismCmd;
use crate::core::driver::DriverManager;
use crate::core::system_utils;
use crate::tr;
use lr_core::image_meta::{WimProgress, WIM_COMPRESS_LZMS, WIM_COMPRESS_LZX};
use lr_core::wimlib::WimlibManager;
use lr_core::WimEngineManager;

/// 操作进度
#[derive(Debug, Clone)]
pub struct DismProgress {
    pub percentage: u8,
    pub status: String,
}

/// 镜像分卷信息
#[derive(Debug, Clone)]
pub struct ImageInfo {
    pub index: u32,
    pub name: String,
    pub size_bytes: u64,
    /// WIM hard-link byte count used by the exact expanded-size budget.
    pub hard_link_bytes: u64,
    /// 安装类型，用于过滤 WindowsPE 等非系统镜像
    /// 值如: "Client", "WindowsPE", "Server" 等
    pub installation_type: String,
    /// Windows 主版本号 (如 10 表示 Win10/Win11)
    pub major_version: Option<u16>,
    /// Windows 次版本号 (如 Win7 为 1，对应版本 6.1)
    pub minor_version: Option<u16>,
    /// Windows 构建号（WIM XML VERSION/BUILD）。
    pub build: Option<u32>,
    /// WIM 架构代码（0=x86，9=amd64，12=arm64）。
    pub architecture: Option<u16>,
    /// 镜像类型 (标准安装/整盘备份/PE等)
    pub image_type: lr_core::image_meta::WimImageType,
    /// 是否已验证可安装
    pub verified_installable: bool,
}

/// Returns whether image inventory metadata describes a volume that the normal installer may
/// offer as an apply source.
///
/// `verified_installable` is not authoritative here: the wimlib XML inventory path initializes
/// that legacy field to `false` even after it has successfully enumerated a real WIM index.  DISM
/// applies a WIM by the index returned from image inventory, so all normal-system consumers share
/// this classification and let the real apply operation plus its integrity checks decide whether
/// the selected image can actually be applied.
pub fn is_installable_image(volume: &ImageInfo) -> bool {
    use lr_core::image_meta::WimImageType;

    match volume.image_type {
        WimImageType::StandardInstall | WimImageType::FullBackup => return true,
        WimImageType::WindowsPE => return false,
        WimImageType::Unknown => {}
    }
    let name = volume.name.to_lowercase();
    let install_type = volume.installation_type.to_lowercase();
    if install_type == "windowspe"
        || ["windows pe", "windows setup", "setup media", "winpe"]
            .iter()
            .any(|keyword| name.contains(keyword))
    {
        return false;
    }
    if install_type.is_empty() && volume.major_version.is_none() {
        return [
            "windows 10",
            "windows 11",
            "windows server",
            "windows 8",
            "windows 7",
            "backup",
            "备份",
            "系统镜像",
            "镜像",
        ]
        .iter()
        .any(|keyword| name.contains(keyword));
    }
    true
}

pub struct Dism {
    is_pe: bool,
}

impl Dism {
    pub fn new() -> Self {
        Self {
            is_pe: crate::core::system_info::SystemInfo::check_pe_environment(),
        }
    }

    /// 检查是否在 PE 环境
    pub fn is_pe_environment(&self) -> bool {
        self.is_pe
    }

    // ========================================================================
    // 镜像操作 - 使用 wimlib (libwim-15.dll)
    // ========================================================================

    /// 应用系统镜像 (WIM/ESD)
    /// 使用 wimlib 实现
    pub fn apply_image(
        &self,
        image_file: &str,
        apply_dir: &str,
        index: u32,
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<()> {
        self.apply_image_cancellable(image_file, apply_dir, index, progress_tx, None)
    }

    pub fn apply_image_cancellable(
        &self,
        image_file: &str,
        apply_dir: &str,
        index: u32,
        progress_tx: Option<Sender<DismProgress>>,
        cancel: Option<Arc<AtomicBool>>,
    ) -> Result<()> {
        log::info!(
            "[Dism] 使用 wimlib 应用镜像: {} -> {}",
            image_file,
            apply_dir
        );

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
        let result =
            wim_manager.apply_image_cancellable(image_file, apply_dir, index, Some(wim_tx), cancel);

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
        log::info!(
            "[Dism] 使用 wimlib 捕获镜像: {} -> {}",
            capture_dir,
            image_file
        );

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

    pub(crate) fn verify_image_integrity(path: &Path) -> Result<i32> {
        let library = lr_core::wimlib::Wimlib::new()
            .map_err(|error| anyhow::anyhow!("captured image verifier unavailable: {error}"))?;
        let handle = library
            .open_wim(&path.to_string_lossy())
            .map_err(|error| anyhow::anyhow!("cannot reopen captured image: {error}"))?;
        handle
            .verify()
            .map_err(|error| anyhow::anyhow!("captured image verification failed: {error}"))?;
        let index = handle.get_image_count();
        if index <= 0 {
            anyhow::bail!("captured image contains no image");
        }
        Ok(index)
    }

    /// Capture directly into an already private, same-volume publication directory.
    /// The caller owns atomic publication and must verify/seal the completed file before CAS.
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

    pub(crate) fn read_verified_backup_catalog(
        path: &Path,
    ) -> Result<lr_core::backup_image_catalog::BackupImageCatalog> {
        lr_core::wimlib::read_verified_backup_catalog(path).map_err(anyhow::Error::msg)
    }

    // ========================================================================
    // 驱动操作 - 使用 setupapi.dll/newdev.dll
    // ========================================================================

    fn count_exported_inf_files(destination: &Path) -> Result<usize> {
        lr_core::driver::count_exported_driver_inf_files(destination)
    }

    fn require_exported_drivers(destination: &Path) -> Result<usize> {
        let count = Self::count_exported_inf_files(destination)?;
        if count == 0 {
            anyhow::bail!(
                "{}",
                tr!("驱动导出完成，但目标目录中没有找到任何 INF 驱动包")
            );
        }
        Ok(count)
    }

    fn remove_storage_driver_manifest(destination: &Path) -> Result<bool> {
        let manifest_path = destination.join(lr_core::driver::STORAGE_DRIVER_REQUIREMENTS_FILE);
        match std::fs::symlink_metadata(&manifest_path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("无法检查旧存储驱动清单: {}", manifest_path.display())
                })
            }
        }
        std::fs::remove_file(&manifest_path)
            .with_context(|| format!("无法清理旧存储驱动清单: {}", manifest_path.display()))?;
        Ok(true)
    }

    fn prepare_storage_driver_manifest(
        destination: &Path,
    ) -> Result<Vec<lr_core::driver::StorageDriverRequirement>> {
        Self::remove_storage_driver_manifest(destination)?;
        let requirements = lr_core::driver::list_present_oem_storage_driver_requirements()
            .context("无法枚举当前硬件已绑定的 OEM 启动存储控制器驱动，已拒绝继续导出")?;
        log::info!(
            "[Dism] 导出前确认 {} 个当前硬件已绑定的 OEM 启动存储驱动包",
            requirements.len()
        );
        Ok(requirements)
    }

    fn finalize_driver_export(
        destination: &Path,
        storage_requirements: &[lr_core::driver::StorageDriverRequirement],
        allow_verified_empty: bool,
    ) -> Result<usize> {
        let count = Self::count_exported_inf_files(destination)?;
        if count == 0 && !allow_verified_empty {
            Self::require_exported_drivers(destination)?;
        }
        lr_core::driver::write_storage_driver_requirements(destination, storage_requirements)
            .context("驱动导出完成，但启动存储驱动清单生成或覆盖验证失败")?;
        Ok(count)
    }

    /// Maps the selected drive's current physical storage ancestry to driver models that DISM says
    /// are actually staged in the offline source image. This avoids both unsafe source mixing
    /// (WinPE's bound INF is not the offline Windows binding) and the old all-controller heuristic.
    ///
    /// `None` means the read-only inventory was incomplete. Package export can still satisfy an
    /// explicit SaveOnly request, but AutoImport must not receive a fabricated empty manifest.
    fn offline_storage_driver_requirements(
        system_partition: &str,
    ) -> Result<Option<Vec<lr_core::driver::StorageDriverRequirement>>> {
        let source_root = Path::new(system_partition);
        let Some(drive_letter) = lr_core::windows_storage::path_drive_letter(source_root) else {
            anyhow::bail!(
                "离线 Windows 根目录没有可解析的绝对盘符: {}",
                source_root.display()
            );
        };
        let path_devices = match lr_core::driver::list_storage_path_devices_for_drive(drive_letter)
        {
            Ok(devices) => devices,
            Err(error) => {
                log::warn!(
                    "[Dism] 无法把离线系统卷映射到当前物理存储父链；仅保留驱动包，不发布启动存储清单: {error:#}"
                );
                return Ok(None);
            }
        };
        let controllers = path_devices
            .into_iter()
            .filter(lr_core::driver::StoragePathDevice::is_storage_controller)
            .collect::<Vec<_>>();

        let scratch_dir = std::env::temp_dir();
        let inventory_log = scratch_dir.join(format!(
            "LetRecovery-DismApi-offline-export-{}.log",
            std::process::id()
        ));
        let inventory = match lr_core::dism_driver_inventory::enumerate_offline_driver_candidates(
            source_root,
            &scratch_dir,
            &inventory_log,
        ) {
            Ok(inventory) => inventory,
            Err(error) => {
                log::warn!(
                    "[Dism] 无法读取离线源系统的 DISM 驱动库存；仅保留驱动包，不发布启动存储清单: {error:#}"
                );
                return Ok(None);
            }
        };

        let inventory_incomplete = !inventory.package_query_failures.is_empty()
            || inventory.omitted_package_query_failures != 0;
        let mut requirements = Vec::new();
        for controller in controllers {
            let mut device_ids = controller.hardware_ids.clone();
            device_ids.extend(controller.compatible_ids.iter().cloned());
            let matching = inventory
                .candidates
                .iter()
                .filter(|candidate| {
                    device_ids.iter().any(|device_id| {
                        !device_id.is_empty()
                            && candidate.hardware_id.eq_ignore_ascii_case(device_id)
                    })
                })
                .collect::<Vec<_>>();
            if matching.is_empty() {
                if inventory_incomplete {
                    log::warn!(
                        "[Dism] 离线 DISM 库存有未读取包，且存储路径设备 {} 尚无候选；仅保留驱动包，不发布不完整清单",
                        controller.instance_id
                    );
                    return Ok(None);
                }
                // No staged third-party candidate exists for this path device. The offline source
                // therefore contributes no OEM package to preserve for it.
                continue;
            }
            let Some(source_package) = matching
                .iter()
                .copied()
                .filter(|candidate| !candidate.in_box)
                .min_by(|left, right| {
                    left.published_name
                        .to_ascii_lowercase()
                        .cmp(&right.published_name.to_ascii_lowercase())
                })
            else {
                // The source image covers this device only with inbox candidates; DISM
                // /Export-Driver intentionally has no OEM package to preserve for it.
                continue;
            };
            requirements.push(lr_core::driver::StorageDriverRequirement {
                description: if controller.description.trim().is_empty() {
                    controller.instance_id.clone()
                } else {
                    controller.description.clone()
                },
                source_inf: source_package.published_name.clone(),
                hardware_ids: controller.hardware_ids,
                compatible_ids: controller.compatible_ids,
                device_instance_id: Some(controller.instance_id),
            });
        }
        Ok(Some(requirements))
    }

    fn finalize_offline_driver_export(
        destination: &Path,
        storage_requirements: Option<&[lr_core::driver::StorageDriverRequirement]>,
    ) -> Result<usize> {
        // Remove again after DISM returns so a stale file that appeared during a reused-directory
        // export cannot be mistaken for evidence about this offline source. This must happen even
        // when the exported payload is empty and finalization subsequently fails.
        Self::remove_storage_driver_manifest(destination)?;
        let count = Self::require_exported_drivers(destination)?;
        if let Some(requirements) = storage_requirements {
            lr_core::driver::write_storage_driver_requirements(destination, requirements)
                .context("离线驱动已导出，但真实存储父链清单无法写入或覆盖验证失败")?;
        }
        Ok(count)
    }

    /// 导出当前在线 Windows 的第三方驱动。
    /// 在正常环境下导出当前系统的第三方驱动（在线映像）
    pub fn export_drivers(&self, destination: &str) -> Result<usize> {
        self.export_drivers_with_empty_policy(destination, false)
    }

    /// Preserves host drivers for an automatic install. A completely enumerated zero-package
    /// result is valid only when the boot-storage manifest is also empty.
    pub fn export_drivers_for_automatic_restore(&self, destination: &str) -> Result<usize> {
        self.export_drivers_with_empty_policy(destination, true)
    }

    fn export_drivers_with_empty_policy(
        &self,
        destination: &str,
        allow_verified_empty: bool,
    ) -> Result<usize> {
        std::fs::create_dir_all(destination)?;

        let destination_path = Path::new(destination);
        let storage_requirements = Self::prepare_storage_driver_manifest(destination_path)?;

        if self.is_pe {
            anyhow::bail!("{}", tr!("PE环境下无法导出当前系统驱动，请使用 export_drivers_from_system 并指定目标系统分区"));
        }

        match DismCmd::new().and_then(|dism| dism.export_drivers_online(destination, None)) {
            Ok(()) => match Self::finalize_driver_export(
                destination_path,
                &storage_requirements,
                allow_verified_empty,
            ) {
                Ok(count) => {
                    log::info!(
                        "[Dism] DISM 在线导出完成，共 {} 个 INF；启动存储驱动覆盖验证通过",
                        count
                    );
                    return Ok(count);
                }
                Err(error) => {
                    log::warn!("[Dism] DISM 在线导出返回空目录: {error}，回退 SetupAPI");
                }
            },
            Err(error) => {
                log::warn!("[Dism] DISM 在线导出失败: {error}，回退 SetupAPI");
            }
        }

        log::info!(
            "[Dism] 使用 Windows API(SetupAPI) 导出驱动到: {}",
            destination
        );
        let manager = DriverManager::new()
            .map_err(|e| anyhow::anyhow!("{}", tr!("驱动管理器初始化失败: {}", e)))?;
        let count = manager.export_drivers(Path::new(destination), true)?;
        if count == 0 && !allow_verified_empty {
            anyhow::bail!("{}", tr!("未找到可导出的第三方驱动"));
        }
        let verified_count = Self::finalize_driver_export(
            destination_path,
            &storage_requirements,
            allow_verified_empty,
        )?;
        if verified_count == 0 {
            log::info!("[Dism] 当前系统没有第三方 OEM 驱动；已生成并回读空启动存储驱动清单");
            return Ok(0);
        }
        log::info!("[Dism] 成功导出 {} 个驱动", verified_count);
        Ok(verified_count)
    }

    /// 从指定系统分区导出驱动 (PE/正常环境均可)。
    /// 在线系统允许回退到 SetupAPI；离线系统只能使用 DISM 的受支持导出边界，禁止按
    /// FileRepository 目录名猜测第三方包。离线导出只在所选源卷的当前物理父链能够与
    /// 离线 DISM 驱动库存完整对应时发布版本 2 启动存储清单；证据不足仍可完成 SaveOnly，
    /// 但会删除旧清单，避免把当前 WinPE 或复用目录中的信息冒充为离线源绑定。
    pub fn export_drivers_from_system(
        &self,
        system_partition: &str,
        destination: &str,
    ) -> Result<usize> {
        std::fs::create_dir_all(destination)?;

        // 判断目标是否就是“当前运行系统”：非 PE 且盘符等于 GetWindowsDirectoryW 所在卷 → 用在线映像，
        // 否则按离线映像（PE 下对已部署系统，或对另一块系统盘）导出。
        let target_drive = system_partition
            .trim()
            .chars()
            .next()
            .map(|c| c.to_ascii_uppercase());
        let system_drive = lr_core::windows_storage::current_windows_drive_letter()
            .map_err(anyhow::Error::from)?;
        let is_online_target =
            !self.is_pe && target_drive.is_some_and(|drive| drive == system_drive);

        if is_online_target {
            return self.export_drivers(destination);
        }

        let destination_path = Path::new(destination);
        // A reused destination may contain a manifest produced by an earlier online export. Remove
        // it before DISM starts so every failure path also leaves no mixed-source critical claim.
        Self::remove_storage_driver_manifest(destination_path)?;

        if let Err(error) = DismCmd::new()
            .and_then(|dism| dism.export_drivers_offline(system_partition, destination, None))
        {
            anyhow::bail!(
                "DISM 离线驱动导出失败，已拒绝使用不完整的手工 DriverStore 回退: {error:#}"
            );
        }
        let storage_requirements = Self::offline_storage_driver_requirements(system_partition)?;
        let count = Self::finalize_offline_driver_export(
            destination_path,
            storage_requirements.as_deref(),
        )?;
        if let Some(requirements) = storage_requirements {
            log::info!(
                "[Dism] DISM 离线驱动导出完成，共 {} 个 INF；已用目标卷当前物理父链和离线镜像 DISM 库存生成 {} 个启动存储要求",
                count,
                requirements.len()
            );
        } else {
            log::warn!(
                "[Dism] DISM 离线驱动导出完成，共 {} 个 INF；证据不足，未生成启动存储清单。SaveOnly 备份可正常使用，AutoImport 必须将清单缺失分类为启动覆盖未确认",
                count
            );
        }
        Ok(count)
    }

    /// 导入驱动到离线系统 (PE和正常环境都可用)
    ///
    /// 使用 dism.exe 命令行进行离线驱动注入：
    /// - 支持普通驱动（.inf 文件）
    /// - 支持 CAB 包（Windows 更新）
    ///
    /// 优先使用当前 Windows/WinPE 自带的 DISM，随包副本只作兼容回退。
    pub fn add_drivers_offline(&self, image_path: &str, driver_path: &str) -> Result<()> {
        log::info!("[Dism] 离线导入驱动: {} -> {}", driver_path, image_path);

        // 规范化路径：移除尾部的反斜杠
        let image_path_clean = image_path.trim_end_matches('\\').trim_end_matches('/');

        // 使用 dism.exe 命令行进行离线驱动注入
        // 这将使用 DISM 的 /Add-Driver 和 /Add-Package 功能
        log::info!("[Dism] 使用 dism.exe 命令行进行离线驱动注入...");

        let dism_cmd = DismCmd::new()
            .map_err(|e| anyhow::anyhow!("{}", tr!("DISM 命令行初始化失败: {}", e)))?;

        // 智能导入：自动识别并处理驱动文件和 CAB 包
        dism_cmd
            .import_drivers_smart(image_path_clean, driver_path, None)
            .context("DISM 离线驱动导入失败，已拒绝不完整的手工注册表回退")?;
        log::info!("[Dism] 离线驱动注入完成");
        Ok(())
    }

    // ========================================================================
    // 镜像信息 - 使用 wimlib (libwim-15.dll) + WIM XML 解析
    // ========================================================================

    /// 获取 WIM/ESD 镜像信息（所有分卷）
    /// 使用 wimlib 或直接解析 WIM XML 元数据
    pub fn get_image_info(&self, image_file: &str) -> Result<Vec<ImageInfo>> {
        log::info!("[Dism] 开始获取镜像信息: {}", image_file);

        // 首先尝试使用 wimlib
        match WimlibManager::new() {
            Ok(wim_manager) => {
                log::info!("[Dism] wimlib 加载成功");
                match wim_manager.get_image_info(image_file) {
                    Ok(images) => {
                        log::info!("[Dism] 从 wimlib 成功获取 {} 个镜像信息", images.len());
                        return Ok(images
                            .into_iter()
                            .map(|img| ImageInfo {
                                index: img.index,
                                name: img.name,
                                size_bytes: img.size_bytes,
                                hard_link_bytes: img.hard_link_bytes,
                                installation_type: img.installation_type,
                                major_version: img.major_version,
                                minor_version: img.minor_version,
                                build: img.build,
                                architecture: img.architecture,
                                image_type: img.image_type,
                                verified_installable: img.verified_installable,
                            })
                            .collect());
                    }
                    Err(e) => {
                        log::warn!("[Dism] wimlib 获取镜像信息失败: {}", e);
                    }
                }
            }
            Err(e) => {
                log::warn!(
                    "[Dism] wimlib (libwim-15.dll) 加载失败: {} (PE 环境会自动释放内置 DLL)",
                    e
                );
            }
        }

        // 尝试直接解析 WIM XML 元数据（仅对WIM有效，ESD的元数据是压缩的）
        log::info!("[Dism] 尝试直接解析 WIM XML 元数据...");
        match Self::parse_wim_xml_metadata(image_file) {
            Ok(images) => {
                if !images.is_empty() {
                    log::info!("[Dism] 从 WIM XML 元数据成功解析出 {} 个镜像", images.len());
                    return Ok(images);
                } else {
                    log::warn!("[Dism] WIM XML 解析成功但未找到镜像信息");
                }
            }
            Err(e) => {
                log::warn!(
                    "[Dism] WIM XML 直接解析失败: {} (ESD 文件的元数据是压缩的，需要 wimlib)",
                    e
                );
            }
        }

        anyhow::bail!("{}", tr!("无法获取镜像信息：wimlib 打开文件失败。可能原因：1.镜像文件损坏 2.libwim-15.dll 缺失或版本过旧不支持此格式（程序会自动释放内置的 libwim-15.dll 到程序目录，请确认其存在）"))
    }

    /// 直接解析 WIM 文件的 XML 元数据
    fn parse_wim_xml_metadata(image_file: &str) -> Result<Vec<ImageInfo>> {
        let xml_string = Self::read_wim_xml_metadata(image_file)?;
        Self::parse_wim_xml(&xml_string)
    }

    fn get_ntdll_major_version(image_file: &str, index: u32) -> Result<u16> {
        // 用 wimlib 仅提取 \Windows\System32\ntdll.dll 到临时目录，再读其文件版本
        // （替代原先的 wimgapi 挂载方案——wimlib 在 Windows 上不支持挂载）
        let manager = WimlibManager::new()
            .map_err(|e| anyhow::anyhow!("{}", tr!("wimlib 初始化失败: {}", e)))?;

        let extract_dir = std::env::temp_dir().join(format!(
            "LetRecovery_WimExtract_{}_{}",
            std::process::id(),
            index
        ));
        if extract_dir.exists() {
            let _ = std::fs::remove_dir_all(&extract_dir);
        }
        std::fs::create_dir_all(&extract_dir).context(tr!("创建临时提取目录失败"))?;

        struct DirGuard(PathBuf);
        impl Drop for DirGuard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _guard = DirGuard(extract_dir.clone());

        let extract_dir_str = extract_dir.to_string_lossy().to_string();
        manager
            .extract_paths(
                image_file,
                index,
                &extract_dir_str,
                &["\\Windows\\System32\\ntdll.dll"],
            )
            .map_err(|e| anyhow::anyhow!("{}", tr!("提取 ntdll.dll 失败: {}", e)))?;

        let ntdll_path = extract_dir
            .join("Windows")
            .join("System32")
            .join("ntdll.dll");
        let (major, _minor, _build, _revision) = system_utils::get_file_version(&ntdll_path)
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("读取 ntdll.dll 版本失败")))?;
        Ok(major)
    }

    fn get_image_major_version_from_xml(image_file: &str, index: u32) -> Result<u16> {
        let xml_string = Self::read_wim_xml_metadata(image_file)?;
        let image_block = Self::extract_image_block(&xml_string, index)
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("未找到指定索引的镜像信息")))?;
        let version_block = Self::extract_xml_tag(&image_block, "VERSION").unwrap_or_default();
        let major_str = if !version_block.is_empty() {
            Self::extract_xml_tag(&version_block, "MAJOR")
        } else {
            Self::extract_xml_tag(&image_block, "MAJOR")
        };
        major_str
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("解析镜像版本失败")))
    }

    fn read_wim_xml_metadata(image_file: &str) -> Result<String> {
        use std::fs::File;
        use std::io::{Read, Seek, SeekFrom};

        log::debug!("[Dism] 尝试直接解析 WIM XML 元数据: {}", image_file);

        let mut file = File::open(image_file)?;
        let file_size = file.metadata()?.len();
        let mut header = [0u8; lr_core::image_meta::WIM_HEADER_SIZE];
        file.read_exact(&mut header)?;
        let resource = lr_core::image_meta::parse_wim_xml_resource(&header, Some(file_size))
            .map_err(|error| anyhow::anyhow!("{}: {error}", tr!("XML 元数据位置无效")))?;
        let xml_offset = resource.offset;
        let xml_size = resource.stored_size;

        log::debug!("[Dism] XML 偏移: {}, 大小: {}", xml_offset, xml_size);

        file.seek(SeekFrom::Start(xml_offset))?;
        let mut xml_data = vec![0u8; xml_size as usize];
        file.read_exact(&mut xml_data)?;

        lr_core::image_meta::decode_wim_xml(&xml_data)
            .map_err(|error| anyhow::anyhow!("{}: {error}", tr!("UTF-16 解码失败")))
    }

    fn extract_image_block(xml: &str, target_index: u32) -> Option<String> {
        let mut pos = 0;
        while let Some(start) = xml[pos..].find("<IMAGE INDEX=\"") {
            let abs_start = pos + start;
            let index_start = abs_start + 14;
            if let Some(index_end) = xml[index_start..].find('"') {
                let index_str = &xml[index_start..index_start + index_end];
                let index: u32 = index_str.parse().unwrap_or(0);
                if let Some(image_end) = xml[abs_start..].find("</IMAGE>") {
                    if index == target_index {
                        return Some(xml[abs_start..abs_start + image_end + 8].to_string());
                    }
                    pos = abs_start + image_end + 8;
                } else {
                    pos = abs_start + 14;
                }
            } else {
                pos = abs_start + 14;
            }
        }
        None
    }

    /// 解析 WIM XML 元数据字符串
    fn parse_wim_xml(xml: &str) -> Result<Vec<ImageInfo>> {
        let mut images = Vec::new();

        let mut pos = 0;
        while let Some(start) = xml[pos..].find("<IMAGE INDEX=\"") {
            let abs_start = pos + start;

            let index_start = abs_start + 14;
            if let Some(index_end) = xml[index_start..].find('"') {
                let index_str = &xml[index_start..index_start + index_end];
                let index: u32 = index_str.parse().unwrap_or(0);

                if let Some(image_end) = xml[abs_start..].find("</IMAGE>") {
                    let image_block = &xml[abs_start..abs_start + image_end + 8];

                    // 优先使用 DISPLAYNAME，其次使用 NAME，最后使用默认名称
                    let name = Self::extract_xml_tag(image_block, "DISPLAYNAME")
                        .or_else(|| Self::extract_xml_tag(image_block, "NAME"))
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| tr!("镜像 {}", index));

                    let size_bytes = Self::extract_xml_tag(image_block, "TOTALBYTES")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    let hard_link_bytes = Self::extract_xml_tag(image_block, "HARDLINKBYTES")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(0);

                    let installation_type =
                        Self::extract_xml_tag(image_block, "INSTALLATIONTYPE").unwrap_or_default();

                    // 提取版本信息 - 先尝试从 VERSION 块中获取，然后直接从 IMAGE 块获取
                    let major_version = Self::extract_xml_tag(image_block, "VERSION")
                        .and_then(|version_block| Self::extract_xml_tag(&version_block, "MAJOR"))
                        .or_else(|| Self::extract_xml_tag(image_block, "MAJOR"))
                        .and_then(|s| s.parse::<u16>().ok());

                    let minor_version = Self::extract_xml_tag(image_block, "VERSION")
                        .and_then(|version_block| Self::extract_xml_tag(&version_block, "MINOR"))
                        .or_else(|| Self::extract_xml_tag(image_block, "MINOR"))
                        .and_then(|s| s.parse::<u16>().ok());

                    // 确定镜像类型
                    let image_type = Self::determine_image_type_from_info(
                        &name,
                        &installation_type,
                        major_version,
                        size_bytes,
                    );

                    if index > 0 {
                        images.push(ImageInfo {
                            index,
                            name,
                            size_bytes,
                            hard_link_bytes,
                            installation_type,
                            major_version,
                            minor_version,
                            build: None,
                            architecture: None,
                            image_type,
                            verified_installable: false,
                        });
                    }

                    pos = abs_start + image_end + 8;
                } else {
                    pos = abs_start + 14;
                }
            } else {
                pos = abs_start + 14;
            }
        }

        if images.is_empty() {
            anyhow::bail!("{}", tr!("未找到有效的镜像信息"));
        }

        Ok(images)
    }

    /// 根据镜像信息确定镜像类型
    fn determine_image_type_from_info(
        name: &str,
        installation_type: &str,
        major_version: Option<u16>,
        size_bytes: u64,
    ) -> lr_core::image_meta::WimImageType {
        use lr_core::image_meta::WimImageType;

        let name_lower = name.to_lowercase();
        let install_type_lower = installation_type.to_lowercase();

        // 检测 PE 环境
        if install_type_lower == "windowspe"
            || name_lower.contains("windows pe")
            || name_lower.contains("winpe")
            || name_lower.contains("windows setup")
        {
            return WimImageType::WindowsPE;
        }

        // 检测标准安装镜像
        if !installation_type.is_empty()
            && major_version.is_some()
            && (install_type_lower == "client" || install_type_lower == "server")
        {
            return WimImageType::StandardInstall;
        }

        // 检测整盘备份型
        if installation_type.is_empty() && size_bytes > 1_000_000_000 {
            return WimImageType::FullBackup;
        }

        if name_lower.contains("backup")
            || name_lower.contains("备份")
            || name_lower.contains("ghost")
            || name_lower.contains("clone")
        {
            return WimImageType::FullBackup;
        }

        if major_version.is_some() && installation_type.is_empty() {
            return WimImageType::FullBackup;
        }

        WimImageType::Unknown
    }

    /// 从 XML 块中提取指定标签的内容
    fn extract_xml_tag(xml: &str, tag: &str) -> Option<String> {
        let open_tag = format!("<{}>", tag);
        let close_tag = format!("</{}>", tag);

        if let Some(start) = xml.find(&open_tag) {
            let content_start = start + open_tag.len();
            if let Some(end) = xml[content_start..].find(&close_tag) {
                let content = &xml[content_start..content_start + end];
                return Some(content.trim().to_string());
            }
        }
        None
    }

    // ========================================================================
    // 系统信息 - 使用离线注册表 API
    // ========================================================================

    /// 获取系统信息 (离线)
    /// 使用 advapi32.dll 的 RegLoadKey API 读取离线注册表
    pub fn get_offline_system_info(&self, image_path: &str) -> Result<String> {
        let info = system_utils::get_offline_system_info(image_path)?;

        let result = format!(
            "产品名称: {}\n版本: {}\n构建: {}\n版本ID: {}\n安装类型: {}",
            info.product_name,
            info.display_version,
            info.current_build,
            info.edition_id,
            info.installation_type
        );

        Ok(result)
    }
}

impl Default for Dism {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{is_installable_image, Dism, ImageInfo};

    fn image(image_type: lr_core::image_meta::WimImageType) -> ImageInfo {
        ImageInfo {
            index: 1,
            name: "Windows 11 Pro".to_owned(),
            size_bytes: 5_000_000_000,
            hard_link_bytes: 0,
            installation_type: "Client".to_owned(),
            major_version: Some(10),
            minor_version: Some(0),
            build: Some(26100),
            architecture: Some(9),
            image_type,
            // The production wimlib XML inventory currently reports this legacy flag as false.
            verified_installable: false,
        }
    }

    #[test]
    fn wimlib_inventory_does_not_require_the_legacy_verified_flag() {
        assert!(is_installable_image(&image(
            lr_core::image_meta::WimImageType::StandardInstall
        )));
        assert!(is_installable_image(&image(
            lr_core::image_meta::WimImageType::FullBackup
        )));
        assert!(!is_installable_image(&image(
            lr_core::image_meta::WimImageType::WindowsPE
        )));
    }

    #[test]
    fn exported_driver_validation_rejects_empty_and_counts_nested_inf_files() {
        let temporary =
            lr_core::scoped_temp_file::ScopedTempDir::create_in(&std::env::temp_dir(), "lr-dism")
                .expect("temporary driver directory");
        assert_eq!(Dism::count_exported_inf_files(temporary.path()).unwrap(), 0);
        let nested = temporary.path().join("driver");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(nested.join("OEM42.INF"), b"[Version]\r\n").unwrap();
        std::fs::write(nested.join("readme.txt"), b"ignored").unwrap();
        assert_eq!(Dism::count_exported_inf_files(temporary.path()).unwrap(), 1);
    }

    #[test]
    fn finalized_export_always_publishes_a_storage_manifest() {
        let temporary =
            lr_core::scoped_temp_file::ScopedTempDir::create_in(&std::env::temp_dir(), "lr-dism")
                .expect("temporary driver directory");
        let nested = temporary.path().join("driver");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(nested.join("OEM42.INF"), b"[Version]\r\n").unwrap();

        assert_eq!(
            Dism::finalize_driver_export(temporary.path(), &[], false).unwrap(),
            1
        );
        let manifest = temporary
            .path()
            .join(lr_core::driver::STORAGE_DRIVER_REQUIREMENTS_FILE);
        assert!(manifest.is_file());
        assert_eq!(
            lr_core::driver::verify_offline_storage_driver_requirements(
                temporary.path(),
                temporary.path(),
            )
            .unwrap(),
            Vec::<lr_core::driver::StorageDriverRequirement>::new()
        );
    }

    #[test]
    fn automatic_export_accepts_only_a_manifest_backed_empty_driver_set() {
        let temporary =
            lr_core::scoped_temp_file::ScopedTempDir::create_in(&std::env::temp_dir(), "lr-dism")
                .expect("temporary driver directory");

        assert!(Dism::finalize_driver_export(temporary.path(), &[], false).is_err());
        assert_eq!(
            Dism::finalize_driver_export(temporary.path(), &[], true).unwrap(),
            0
        );
        assert!(temporary
            .path()
            .join(lr_core::driver::STORAGE_DRIVER_REQUIREMENTS_FILE)
            .is_file());
    }

    #[test]
    fn offline_export_finalization_removes_stale_manifest_without_publishing_empty_claim() {
        let temporary =
            lr_core::scoped_temp_file::ScopedTempDir::create_in(&std::env::temp_dir(), "lr-dism")
                .expect("temporary driver directory");
        let nested = temporary.path().join("driver");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(nested.join("OEM42.INF"), b"[Version]\r\n").unwrap();
        let manifest = temporary
            .path()
            .join(lr_core::driver::STORAGE_DRIVER_REQUIREMENTS_FILE);
        std::fs::write(&manifest, b"stale online-source manifest").unwrap();

        assert_eq!(
            Dism::finalize_offline_driver_export(temporary.path(), None).unwrap(),
            1
        );
        assert!(!manifest.exists());
    }

    #[test]
    fn offline_export_finalization_rejects_empty_payload_and_removes_stale_manifest() {
        let temporary =
            lr_core::scoped_temp_file::ScopedTempDir::create_in(&std::env::temp_dir(), "lr-dism")
                .expect("temporary driver directory");
        let manifest = temporary
            .path()
            .join(lr_core::driver::STORAGE_DRIVER_REQUIREMENTS_FILE);
        std::fs::write(&manifest, b"stale online-source manifest").unwrap();

        assert!(Dism::finalize_offline_driver_export(temporary.path(), None).is_err());
        assert!(!manifest.exists());
    }
}
