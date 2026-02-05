//! 镜像操作模块
//!
//! 该模块封装了 Windows 系统镜像操作功能：
//! - 镜像释放/应用：使用 wimgapi.dll
//! - 镜像备份/捕获：使用 wimgapi.dll
//! - 离线驱动导入：使用 dism.exe 命令行（优先使用 {程序目录}\bin\Dism\dism.exe）
//! - 离线 CAB 包导入：使用 dism.exe 命令行
//! - 镜像信息获取：使用 wimgapi.dll + WIM XML 解析
//! - 系统信息获取：使用 advapi32.dll (离线注册表)

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use crate::core::dism_cmd::DismCmd;
use crate::core::driver::DriverManager;
use crate::core::system_utils;
use crate::core::wimgapi::{WimManager, WimProgress, WIM_COMPRESS_LZX, Wimgapi};

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
    pub image_type: crate::core::wimgapi::WimImageType,
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
    // 镜像操作 - 使用 wimgapi.dll
    // ========================================================================

    /// 应用系统镜像 (WIM/ESD)
    /// 使用 wimgapi.dll 实现
    pub fn apply_image(
        &self,
        image_file: &str,
        apply_dir: &str,
        index: u32,
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<()> {
        println!("[Dism] 使用 wimgapi 应用镜像: {} -> {}", image_file, apply_dir);

        let wim_manager = WimManager::new()
            .map_err(|e| anyhow::anyhow!("wimgapi 初始化失败: {}", e))?;

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
                println!("[Dism] 镜像应用成功");
                Ok(())
            }
            Err(e) => {
                anyhow::bail!("镜像应用失败: {}", e)
            }
        }
    }

    /// 捕获系统镜像 (备份)
    /// 使用 wimgapi.dll 实现
    pub fn capture_image(
        &self,
        image_file: &str,
        capture_dir: &str,
        name: &str,
        description: &str,
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<()> {
        println!("[Dism] 使用 wimgapi 捕获镜像: {} -> {}", capture_dir, image_file);

        let wim_manager = WimManager::new()
            .map_err(|e| anyhow::anyhow!("wimgapi 初始化失败: {}", e))?;

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
                println!("[Dism] 镜像捕获成功");
                Ok(())
            }
            Err(e) => {
                anyhow::bail!("镜像捕获失败: {}", e)
            }
        }
    }

    /// 增量备份镜像
    /// 使用 wimgapi.dll 实现
    pub fn append_image(
        &self,
        image_file: &str,
        capture_dir: &str,
        name: &str,
        description: &str,
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<()> {
        println!("[Dism] 使用 wimgapi 追加镜像: {} -> {}", capture_dir, image_file);

        // 对于追加操作，WimManager 的 capture_image 在文件存在时会自动追加
        self.capture_image(image_file, capture_dir, name, description, progress_tx)
    }

    // ========================================================================
    // 驱动操作 - 使用 setupapi.dll/newdev.dll
    // ========================================================================

    /// 导出驱动 - 使用 Windows API
    /// 在正常环境下导出当前系统的第三方驱动
    pub fn export_drivers(&self, destination: &str) -> Result<()> {
        std::fs::create_dir_all(destination)?;

        if self.is_pe {
            anyhow::bail!("PE环境下无法导出当前系统驱动，请使用 export_drivers_from_system 并指定目标系统分区");
        }

        println!("[Dism] 使用 Windows API 导出驱动到: {}", destination);

        let manager = DriverManager::new()
            .map_err(|e| anyhow::anyhow!("驱动管理器初始化失败: {}", e))?;

        let count = manager.export_drivers(Path::new(destination), true)?;
        println!("[Dism] 成功导出 {} 个驱动", count);
        Ok(())
    }

    /// 从指定系统分区导出驱动 (PE环境下使用)
    /// 使用 Windows API 直接读取驱动存储
    pub fn export_drivers_from_system(&self, system_partition: &str, destination: &str) -> Result<()> {
        std::fs::create_dir_all(destination)?;

        println!("[Dism] 使用 Windows API 从 {} 导出驱动到: {}", system_partition, destination);

        let manager = DriverManager::new()
            .map_err(|e| anyhow::anyhow!("驱动管理器初始化失败: {}", e))?;

        let count = manager.export_drivers_from_system(
            Path::new(system_partition),
            Path::new(destination),
        )?;
        println!("[Dism] 成功导出 {} 个驱动", count);
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
            anyhow::bail!("PE环境下无法使用在线方式添加驱动，请使用 add_drivers_offline");
        }

        println!("[Dism] 使用 Windows API 导入驱动: {}", driver_path);

        let manager = DriverManager::new()
            .map_err(|e| anyhow::anyhow!("驱动管理器初始化失败: {}", e))?;

        let (success, fail, need_reboot) = manager.import_drivers(
            Path::new(driver_path),
            true, // force
        )?;

        println!(
            "[Dism] 驱动导入完成: 成功 {}, 失败 {}, 需要重启: {}",
            success, fail, need_reboot
        );

        if fail > 0 && success == 0 {
            anyhow::bail!("所有驱动导入失败");
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
        println!("[Dism] 离线导入驱动: {} -> {}", driver_path, image_path);

        // 规范化路径：移除尾部的反斜杠
        let image_path_clean = image_path.trim_end_matches('\\').trim_end_matches('/');
        
        // 使用 dism.exe 命令行进行离线驱动注入
        // 这将使用 DISM 的 /Add-Driver 和 /Add-Package 功能
        println!("[Dism] 使用 dism.exe 命令行进行离线驱动注入...");
        
        let dism_cmd = DismCmd::new()
            .map_err(|e| anyhow::anyhow!("DISM 命令行初始化失败: {}", e))?;

        // 智能导入：自动识别并处理驱动文件和 CAB 包
        match dism_cmd.import_drivers_smart(image_path_clean, driver_path, None) {
            Ok(_) => {
                println!("[Dism] 离线驱动注入完成");
                Ok(())
            }
            Err(e) => {
                println!("[Dism] dism.exe 导入失败: {}", e);
                
                // 尝试回退到 DriverManager（仅当 DISM 完全失败时）
                println!("[Dism] 尝试使用备用方法（DriverManager）...");
                
                let manager = DriverManager::new()
                    .map_err(|e| anyhow::anyhow!("驱动管理器初始化失败: {}", e))?;

                let (success, fail) = manager.import_drivers_offline(
                    Path::new(image_path_clean),
                    Path::new(driver_path),
                )?;

                println!(
                    "[Dism] 备用方法完成: 成功 {}, 失败 {}",
                    success, fail
                );

                if fail > 0 && success == 0 {
                    anyhow::bail!("所有驱动导入失败");
                }
                Ok(())
            }
        }
    }

    // ========================================================================
    // 镜像信息 - 使用 wimgapi.dll + WIM XML 解析
    // ========================================================================

    /// 获取 WIM/ESD 镜像信息（所有分卷）
    /// 使用 wimgapi.dll 或直接解析 WIM XML 元数据
    pub fn get_image_info(&self, image_file: &str) -> Result<Vec<ImageInfo>> {
        println!("[Dism] 开始获取镜像信息: {}", image_file);
        
        // 首先尝试使用 wimgapi
        match WimManager::new() {
            Ok(wim_manager) => {
                println!("[Dism] wimgapi.dll 加载成功");
                match wim_manager.get_image_info(image_file) {
                    Ok(images) => {
                        println!("[Dism] 从 wimgapi 成功获取 {} 个镜像信息", images.len());
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
                        println!("[Dism] wimgapi 获取镜像信息失败: {}", e);
                    }
                }
            }
            Err(e) => {
                println!("[Dism] wimgapi.dll 加载失败: {} (这可能是PE环境缺少该DLL)", e);
            }
        }

        // 尝试直接解析 WIM XML 元数据（仅对WIM有效，ESD的元数据是压缩的）
        println!("[Dism] 尝试直接解析 WIM XML 元数据...");
        match Self::parse_wim_xml_metadata(image_file) {
            Ok(images) => {
                if !images.is_empty() {
                    println!("[Dism] 从 WIM XML 元数据成功解析出 {} 个镜像", images.len());
                    return Ok(images);
                } else {
                    println!("[Dism] WIM XML 解析成功但未找到镜像信息");
                }
            }
            Err(e) => {
                println!("[Dism] WIM XML 直接解析失败: {} (ESD文件的元数据是压缩的，需要wimgapi)", e);
            }
        }

        anyhow::bail!("无法获取镜像信息：wimgapi 打开文件失败。可能原因：1.镜像文件损坏 2.系统 wimgapi.dll 版本过旧不支持此ESD格式，请将新版 wimgapi.dll 放到程序目录")
    }

    /// 通过读取 ntdll.dll 文件版本判断是否为 Win10/11 镜像
    pub fn is_win10_or_11_image_by_ntdll(image_file: &str, index: u32) -> Result<bool> {
        let lower = image_file.to_lowercase();
        let is_wim = lower.ends_with(".wim");
        let is_esd = lower.ends_with(".esd");
        let is_swm = lower.ends_with(".swm");

        if !is_wim && !is_esd && !is_swm {
            anyhow::bail!("仅支持 WIM/ESD/SWM 镜像");
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
        let wimgapi = Wimgapi::new(None)
            .map_err(|e| anyhow::anyhow!("wimgapi 初始化失败: {}", e))?;
        let wim_path = Path::new(image_file);
        let mount_dir = std::env::temp_dir().join(format!(
            "LetRecovery_WimMount_{}_{}",
            std::process::id(),
            index
        ));

        if mount_dir.exists() {
            let _ = std::fs::remove_dir_all(&mount_dir);
        }
        std::fs::create_dir_all(&mount_dir).context("创建临时挂载目录失败")?;

        let temp_dir = mount_dir.join("temp");
        let _ = std::fs::create_dir_all(&temp_dir);

        wimgapi
            .mount_image(&mount_dir, wim_path, index, Some(&temp_dir))
            .map_err(|e| anyhow::anyhow!("挂载镜像失败: {}", e))?;

        struct MountGuard<'a> {
            wimgapi: &'a Wimgapi,
            mount_dir: PathBuf,
            wim_path: PathBuf,
            index: u32,
        }

        impl<'a> Drop for MountGuard<'a> {
            fn drop(&mut self) {
                let _ = self
                    .wimgapi
                    .unmount_image(&self.mount_dir, &self.wim_path, self.index, false);
                let _ = std::fs::remove_dir_all(&self.mount_dir);
            }
        }

        let _guard = MountGuard {
            wimgapi: &wimgapi,
            mount_dir: mount_dir.clone(),
            wim_path: wim_path.to_path_buf(),
            index,
        };

        let ntdll_path = mount_dir
            .join("Windows")
            .join("System32")
            .join("ntdll.dll");
        let (major, _minor, _build, _revision) = system_utils::get_file_version(&ntdll_path)
            .ok_or_else(|| anyhow::anyhow!("读取 ntdll.dll 版本失败"))?;
        Ok(major)
    }

    fn get_image_major_version_from_xml(image_file: &str, index: u32) -> Result<u16> {
        let xml_string = Self::read_wim_xml_metadata(image_file)?;
        let image_block = Self::extract_image_block(&xml_string, index)
            .ok_or_else(|| anyhow::anyhow!("未找到指定索引的镜像信息"))?;
        let version_block = Self::extract_xml_tag(&image_block, "VERSION").unwrap_or_default();
        let major_str = if !version_block.is_empty() {
            Self::extract_xml_tag(&version_block, "MAJOR")
        } else {
            Self::extract_xml_tag(&image_block, "MAJOR")
        };
        major_str
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| anyhow::anyhow!("解析镜像版本失败"))
    }

    fn read_wim_xml_metadata(image_file: &str) -> Result<String> {
        use std::fs::File;
        use std::io::{Read, Seek, SeekFrom};

        println!("[Dism] 尝试直接解析 WIM XML 元数据: {}", image_file);

        let mut file = File::open(image_file)?;
        let mut header = [0u8; 208];
        file.read_exact(&mut header)?;

        let signature = &header[0..8];
        if signature != b"MSWIM\0\0\0" {
            anyhow::bail!("不是有效的 WIM 文件");
        }

        let xml_offset = u64::from_le_bytes(header[48..56].try_into().unwrap());
        let xml_size = u64::from_le_bytes(header[56..64].try_into().unwrap());

        if xml_offset == 0 || xml_size == 0 || xml_size > 100_000_000 {
            anyhow::bail!("XML 元数据位置无效");
        }

        println!("[Dism] XML 偏移: {}, 大小: {}", xml_offset, xml_size);

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
            anyhow::bail!("数据太短");
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
            .map_err(|e| anyhow::anyhow!("UTF-16 解码失败: {}", e))
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
                        .unwrap_or_else(|| format!("镜像 {}", index));
                    
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
            anyhow::bail!("未找到有效的镜像信息");
        }

        Ok(images)
    }

    /// 根据镜像信息确定镜像类型
    fn determine_image_type_from_info(
        name: &str,
        installation_type: &str,
        major_version: Option<u16>,
        size_bytes: u64
    ) -> crate::core::wimgapi::WimImageType {
        use crate::core::wimgapi::WimImageType;
        
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
