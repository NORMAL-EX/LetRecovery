//! 镜像操作模块
//!
//! 该模块封装了 Windows 系统镜像操作功能：
//! - 镜像释放/应用：使用 wimlib (libwim-15.dll)
//! - 镜像备份/捕获：使用 wimlib (libwim-15.dll)
//! - 离线驱动导入：使用 dism.exe 命令行（优先使用 {程序目录}\bin\Dism\dism.exe）
//! - 离线 CAB 包导入：使用 dism.exe 命令行
//! - 镜像信息获取：使用 wimlib (libwim-15.dll) + WIM XML 解析
//! - 系统信息获取：使用 advapi32.dll (离线注册表)

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use crate::core::dism_cmd::DismCmd;
use crate::core::driver::DriverManager;
use crate::core::system_utils;
use crate::tr;
use lr_core::image_meta::{WimProgress, WIM_COMPRESS_LZX, WIM_COMPRESS_LZMS};
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
    /// 安装类型，用于过滤 WindowsPE 等非系统镜像
    /// 值如: "Client", "WindowsPE", "Server" 等
    pub installation_type: String,
    /// Windows 主版本号 (如 10 表示 Win10/Win11)
    pub major_version: Option<u16>,
    /// Windows 次版本号 (如 Win7 为 1，对应版本 6.1)
    pub minor_version: Option<u16>,
    /// 镜像类型 (标准安装/整盘备份/PE等)
    pub image_type: lr_core::image_meta::WimImageType,
    /// 是否已验证可安装
    pub verified_installable: bool,
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
        log::info!("[Dism] 使用 wimlib 应用镜像: {} -> {}", image_file, apply_dir);

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
        let result = wim_manager.apply_image(image_file, apply_dir, index, Some(wim_tx));

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

    /// 捕获系统镜像 (备份)
    /// 使用 wimlib 实现
    pub fn capture_image(
        &self,
        image_file: &str,
        capture_dir: &str,
        name: &str,
        description: &str,
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<()> {
        log::info!("[Dism] 使用 wimlib 捕获镜像: {} -> {}", capture_dir, image_file);

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
            WIM_COMPRESS_LZX,
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

    /// 增量备份镜像
    /// 使用 wimlib 实现
    pub fn append_image(
        &self,
        image_file: &str,
        capture_dir: &str,
        name: &str,
        description: &str,
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<()> {
        log::info!("[Dism] 使用 wimlib 追加镜像: {} -> {}", capture_dir, image_file);

        // 对于追加操作，WimManager 的 capture_image 在文件存在时会自动追加
        self.capture_image(image_file, capture_dir, name, description, progress_tx)
    }

    /// 捕获系统镜像为 ESD（LZMS solid 高压缩）。目标文件已存在则追加镜像。
    /// 与 PE 端 `Dism::capture_image_esd` 等价，供桌面 Direct 备份按格式分发使用。
    pub fn capture_image_esd(
        &self,
        image_file: &str,
        capture_dir: &str,
        name: &str,
        description: &str,
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<()> {
        log::info!("[Dism] 捕获 ESD 镜像(LZMS): {} -> {}", capture_dir, image_file);

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
            WIM_COMPRESS_LZMS,
            Some(wim_tx),
        );
        let _ = forward_thread.join();

        match result {
            Ok(_) => {
                log::info!("[Dism] ESD 镜像捕获成功");
                Ok(())
            }
            Err(e) => anyhow::bail!("{}", tr!("ESD 镜像捕获失败: {}", e)),
        }
    }

    /// 增量备份 ESD（文件存在时自动追加镜像）。
    pub fn append_image_esd(
        &self,
        image_file: &str,
        capture_dir: &str,
        name: &str,
        description: &str,
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<()> {
        log::info!("[Dism] 追加 ESD 镜像: {} -> {}", capture_dir, image_file);
        self.capture_image_esd(image_file, capture_dir, name, description, progress_tx)
    }

    /// 捕获为 SWM 分卷：先抓为临时 WIM(LZX)，再用 libwim 分割为 .swm。
    /// 与 PE 端 `Dism::capture_image_swm` 等价。
    pub fn capture_image_swm(
        &self,
        image_file: &str,
        capture_dir: &str,
        name: &str,
        description: &str,
        split_size_mb: u32,
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<()> {
        log::info!(
            "[Dism] 捕获 SWM 分卷: {} -> {} (分卷 {}MB)",
            capture_dir, image_file, split_size_mb
        );

        // 临时 WIM 与最终 .swm 同目录
        let temp_wim = format!("{}.tmp.wim", image_file.trim_end_matches(".swm"));

        if let Some(ref tx) = progress_tx {
            let _ = tx.send(DismProgress {
                percentage: 0,
                status: tr!("正在捕获镜像..."),
            });
        }

        let engine = WimEngineManager::new_current()
            .map_err(|e| anyhow::anyhow!("{}", tr!("镜像引擎初始化失败: {}", e)))?;

        let (wim_tx, wim_rx) = std::sync::mpsc::channel::<WimProgress>();
        let progress_tx_clone = progress_tx.clone();
        let forward_thread = std::thread::spawn(move || {
            while let Ok(progress) = wim_rx.recv() {
                if let Some(ref tx) = progress_tx_clone {
                    // 捕获阶段占 80% 进度，分割占后 20%
                    let _ = tx.send(DismProgress {
                        percentage: (progress.percentage as u32 * 80 / 100) as u8,
                        status: progress.status,
                    });
                }
            }
        });

        let result = engine.capture_image(
            capture_dir,
            &temp_wim,
            name,
            description,
            WIM_COMPRESS_LZX,
            Some(wim_tx),
        );
        let _ = forward_thread.join();

        if let Err(e) = result {
            let _ = std::fs::remove_file(&temp_wim);
            anyhow::bail!("{}", tr!("捕获镜像失败: {}", e));
        }

        if let Some(ref tx) = progress_tx {
            let _ = tx.send(DismProgress {
                percentage: 80,
                status: tr!("正在分割镜像..."),
            });
        }

        // 分卷由 libwim 执行（与生成引擎无关）。
        let wim_manager = WimlibManager::new()
            .map_err(|e| anyhow::anyhow!("{}", tr!("wimlib 初始化失败: {}", e)))?;
        let split_result = wim_manager.split_wim(&temp_wim, image_file, split_size_mb as u64);

        // 清理临时 WIM
        let _ = std::fs::remove_file(&temp_wim);

        match split_result {
            Ok(_) => {
                if let Some(ref tx) = progress_tx {
                    let _ = tx.send(DismProgress {
                        percentage: 100,
                        status: tr!("分卷完成"),
                    });
                }
                log::info!("[Dism] SWM 分卷镜像创建成功");
                Ok(())
            }
            Err(e) => anyhow::bail!("{}", tr!("分割镜像失败: {}", e)),
        }
    }

    // ========================================================================
    // 驱动操作 - 使用 setupapi.dll/newdev.dll
    // ========================================================================

    /// 导出驱动 - 优先 DISM API(DismExportDriver)，失败回退手工导出
    /// 在正常环境下导出当前系统的第三方驱动（在线映像）
    pub fn export_drivers(&self, destination: &str) -> Result<()> {
        std::fs::create_dir_all(destination)?;

        if self.is_pe {
            anyhow::bail!("{}", tr!("PE环境下无法导出当前系统驱动，请使用 export_drivers_from_system 并指定目标系统分区"));
        }

        // 优先：DISM API 在线导出（等价 dism /online /export-driver）
        match crate::core::dismapi::DismApi::load() {
            Ok(api) => match api.export_drivers_online(Path::new(destination), None) {
                Ok(count) => {
                    log::info!("[Dism] DismExportDriver(在线) 成功导出 {} 个驱动 -> {}", count, destination);
                    return Ok(());
                }
                Err(e) => {
                    log::warn!("[Dism] DismExportDriver(在线) 失败: {}，回退手工导出", e);
                }
            },
            Err(e) => {
                log::warn!("[Dism] 加载 dismapi.dll 失败: {}，回退手工导出", e);
            }
        }

        // 回退：SetupAPI 枚举 + 手工复制 DriverStore
        log::info!("[Dism] 使用 Windows API(SetupAPI) 导出驱动到: {}", destination);
        let manager = DriverManager::new()
            .map_err(|e| anyhow::anyhow!("{}", tr!("驱动管理器初始化失败: {}", e)))?;
        let count = manager.export_drivers(Path::new(destination), true)?;
        log::info!("[Dism] 成功导出 {} 个驱动", count);
        Ok(())
    }

    /// 从指定系统分区导出驱动 (PE/正常环境均可)
    /// 优先 DISM API(DismExportDriver)，失败回退手工遍历 DriverStore
    pub fn export_drivers_from_system(&self, system_partition: &str, destination: &str) -> Result<()> {
        std::fs::create_dir_all(destination)?;

        // 判断目标是否就是“当前运行系统”：非 PE 且盘符等于 %SystemDrive% → 用在线映像，
        // 否则按离线映像（PE 下对已部署系统，或对另一块系统盘）导出。
        let target_drive = system_partition
            .trim()
            .chars()
            .next()
            .map(|c| c.to_ascii_uppercase());
        let system_drive = std::env::var("SystemDrive")
            .ok()
            .and_then(|s| s.trim().chars().next())
            .map(|c| c.to_ascii_uppercase());
        let is_online_target = !self.is_pe && target_drive.is_some() && target_drive == system_drive;

        match crate::core::dismapi::DismApi::load() {
            Ok(api) => {
                let result = if is_online_target {
                    log::info!("[Dism] DismExportDriver: 目标为当前运行系统，使用在线映像导出 -> {}", destination);
                    api.export_drivers_online(Path::new(destination), None)
                } else {
                    log::info!("[Dism] DismExportDriver: 离线映像 {} -> {}", system_partition, destination);
                    api.export_drivers_offline(
                        Path::new(system_partition),
                        Path::new(destination),
                        None,
                    )
                };
                match result {
                    Ok(count) => {
                        log::info!("[Dism] DismExportDriver 成功导出 {} 个驱动", count);
                        return Ok(());
                    }
                    Err(e) => {
                        log::warn!("[Dism] DismExportDriver 失败: {}，回退手工导出", e);
                    }
                }
            }
            Err(e) => {
                log::warn!("[Dism] 加载 dismapi.dll 失败: {}，回退手工导出", e);
            }
        }

        // 回退：手工遍历 FileRepository
        log::info!("[Dism] 使用 Windows API 从 {} 导出驱动到: {}", system_partition, destination);
        let manager = DriverManager::new()
            .map_err(|e| anyhow::anyhow!("{}", tr!("驱动管理器初始化失败: {}", e)))?;
        let count = manager.export_drivers_from_system(
            Path::new(system_partition),
            Path::new(destination),
        )?;
        log::info!("[Dism] 成功导出 {} 个驱动", count);
        Ok(())
    }

    /// 导入驱动 - 使用 Windows API
    /// 在PE环境下，自动转为离线操作
    pub fn add_drivers(&self, target_path: &str, driver_path: &str) -> Result<()> {
        if self.is_pe {
            self.add_drivers_offline(target_path, driver_path)
        } else {
            self.add_drivers_online(driver_path)
        }
    }

    /// 导入驱动到在线系统 (仅在正常Windows环境下可用)
    /// 使用 Windows API (newdev.dll/setupapi.dll)
    pub fn add_drivers_online(&self, driver_path: &str) -> Result<()> {
        if self.is_pe {
            anyhow::bail!("{}", tr!("PE环境下无法使用在线方式添加驱动，请使用 add_drivers_offline"));
        }

        log::info!("[Dism] 使用 Windows API 导入驱动: {}", driver_path);

        let manager = DriverManager::new()
            .map_err(|e| anyhow::anyhow!("{}", tr!("驱动管理器初始化失败: {}", e)))?;

        let (success, fail, need_reboot) = manager.import_drivers(
            Path::new(driver_path),
            true, // force
        )?;

        log::info!(
            "[Dism] 驱动导入完成: 成功 {}, 失败 {}, 需要重启: {}",
            success, fail, need_reboot
        );

        if fail > 0 && success == 0 {
            anyhow::bail!("{}", tr!("所有驱动导入失败"));
        }
        Ok(())
    }

    /// 导入驱动到离线系统 (PE和正常环境都可用)
    /// 
    /// 使用 dism.exe 命令行进行离线驱动注入：
    /// - 支持普通驱动（.inf 文件）
    /// - 支持 CAB 包（Windows 更新）
    /// 
    /// 优先使用 {程序目录}\bin\Dism\dism.exe
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
        match dism_cmd.import_drivers_smart(image_path_clean, driver_path, None) {
            Ok(_) => {
                log::info!("[Dism] 离线驱动注入完成");
                Ok(())
            }
            Err(e) => {
                log::warn!("[Dism] dism.exe 导入失败: {}", e);

                // 尝试回退到 DriverManager（仅当 DISM 完全失败时）
                log::info!("[Dism] 尝试使用备用方法（DriverManager）...");
                
                let manager = DriverManager::new()
                    .map_err(|e| anyhow::anyhow!("{}", tr!("驱动管理器初始化失败: {}", e)))?;

                let (success, fail) = crate::core::driver::import_drivers_offline_dism_first(
                    &manager,
                    Path::new(image_path_clean),
                    Path::new(driver_path),
                )?;

                log::info!(
                    "[Dism] 备用方法完成: 成功 {}, 失败 {}",
                    success, fail
                );

                if fail > 0 && success == 0 {
                    anyhow::bail!("{}", tr!("所有驱动导入失败"));
                }
                Ok(())
            }
        }
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
                        return Ok(images.into_iter().map(|img| ImageInfo {
                            index: img.index,
                            name: img.name,
                            size_bytes: img.size_bytes,
                            installation_type: img.installation_type,
                            major_version: img.major_version,
                            minor_version: img.minor_version,
                            image_type: img.image_type,
                            verified_installable: img.verified_installable,
                        }).collect());
                    }
                    Err(e) => {
                        log::warn!("[Dism] wimlib 获取镜像信息失败: {}", e);
                    }
                }
            }
            Err(e) => {
                log::warn!("[Dism] wimlib (libwim-15.dll) 加载失败: {} (PE 环境会自动释放内置 DLL)", e);
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
                log::warn!("[Dism] WIM XML 直接解析失败: {} (ESD 文件的元数据是压缩的，需要 wimlib)", e);
            }
        }

        anyhow::bail!("{}", tr!("无法获取镜像信息：wimlib 打开文件失败。可能原因：1.镜像文件损坏 2.libwim-15.dll 缺失或版本过旧不支持此格式（程序会自动释放内置的 libwim-15.dll 到程序目录，请确认其存在）"))
    }

    /// 通过读取 ntdll.dll 文件版本判断是否为 Win10/11 镜像
    pub fn is_win10_or_11_image_by_ntdll(image_file: &str, index: u32) -> Result<bool> {
        let lower = image_file.to_lowercase();
        let is_wim = lower.ends_with(".wim");
        let is_esd = lower.ends_with(".esd");
        let is_swm = lower.ends_with(".swm");

        if !is_wim && !is_esd && !is_swm {
            anyhow::bail!("{}", tr!("仅支持 WIM/ESD/SWM 镜像"));
        }

        if is_wim || is_esd {
            if let Ok(major) = Self::get_ntdll_major_version(image_file, index) {
                return Ok(major >= 10);
            }
        }

        let major = Self::get_image_major_version_from_xml(image_file, index)?;
        Ok(major >= 10)
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
        let mut header = [0u8; 208];
        file.read_exact(&mut header)?;

        let signature = &header[0..8];
        if signature != b"MSWIM\0\0\0" {
            anyhow::bail!("{}", tr!("不是有效的 WIM 文件"));
        }

        let xml_offset = u64::from_le_bytes(header[48..56].try_into().unwrap());
        let xml_size = u64::from_le_bytes(header[56..64].try_into().unwrap());

        if xml_offset == 0 || xml_size == 0 || xml_size > 100_000_000 {
            anyhow::bail!("{}", tr!("XML 元数据位置无效"));
        }

        log::debug!("[Dism] XML 偏移: {}, 大小: {}", xml_offset, xml_size);

        file.seek(SeekFrom::Start(xml_offset))?;
        let mut xml_data = vec![0u8; xml_size as usize];
        file.read_exact(&mut xml_data)?;

        Self::decode_utf16le(&xml_data)
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
                        return Some(
                            xml[abs_start..abs_start + image_end + 8].to_string(),
                        );
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

    /// 将 UTF-16LE 编码的字节数组转换为 UTF-8 字符串
    fn decode_utf16le(data: &[u8]) -> Result<String> {
        if data.len() < 2 {
            anyhow::bail!("{}", tr!("数据太短"));
        }

        // 检查并跳过 BOM (0xFF 0xFE)
        let start = if data.len() >= 2 && data[0] == 0xFF && data[1] == 0xFE {
            2
        } else {
            0
        };

        let len = (data.len() - start) / 2;
        let mut utf16_data = Vec::with_capacity(len);
        
        for i in 0..len {
            let offset = start + i * 2;
            if offset + 1 < data.len() {
                let code_unit = u16::from_le_bytes([data[offset], data[offset + 1]]);
                utf16_data.push(code_unit);
            }
        }

        // 去除尾部的空字符
        while utf16_data.last() == Some(&0) {
            utf16_data.pop();
        }

        String::from_utf16(&utf16_data)
            .map_err(|e| anyhow::anyhow!("{}", tr!("UTF-16 解码失败: {}", e)))
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
                    
                    let installation_type = Self::extract_xml_tag(image_block, "INSTALLATIONTYPE")
                        .unwrap_or_default();

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
                        &name, &installation_type, major_version, size_bytes
                    );

                    if index > 0 {
                        images.push(ImageInfo {
                            index,
                            name,
                            size_bytes,
                            installation_type,
                            major_version,
                            minor_version,
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
        size_bytes: u64
    ) -> lr_core::image_meta::WimImageType {
        use lr_core::image_meta::WimImageType;
        
        let name_lower = name.to_lowercase();
        let install_type_lower = installation_type.to_lowercase();
        
        // 检测 PE 环境
        if install_type_lower == "windowspe" 
            || name_lower.contains("windows pe")
            || name_lower.contains("winpe")
            || name_lower.contains("windows setup") {
            return WimImageType::WindowsPE;
        }
        
        // 检测标准安装镜像
        if !installation_type.is_empty() 
            && major_version.is_some() 
            && (install_type_lower == "client" || install_type_lower == "server") {
            return WimImageType::StandardInstall;
        }
        
        // 检测整盘备份型
        if installation_type.is_empty() && size_bytes > 1_000_000_000 {
            return WimImageType::FullBackup;
        }
        
        if name_lower.contains("backup") 
            || name_lower.contains("备份")
            || name_lower.contains("ghost")
            || name_lower.contains("clone") {
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
