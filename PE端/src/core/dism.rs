//! 镜像操作模块
//!
//! 该模块封装了 Windows 系统镜像操作功能：
//! - 镜像释放/应用：使用 wimgapi.dll
//! - 镜像备份/捕获：使用 wimgapi.dll
//! - 驱动导入：使用 dism.exe 命令行（PE 环境兼容性最佳）
//! - CAB 包安装：使用 dism.exe 命令行

use anyhow::Result;
use std::path::Path;
use std::sync::mpsc::Sender;

use crate::core::dism_exe::{DismExe, DismExeProgress};
use crate::core::driver::DriverManager;
use crate::core::wimgapi::{WimManager, WimProgress, WIM_COMPRESS_LZX, WIM_COMPRESS_LZMS};

/// 操作进度
#[derive(Debug, Clone)]
pub struct DismProgress {
    pub percentage: u8,
    pub status: String,
}

/// 镜像分卷信息
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ImageInfo {
    pub index: u32,
    pub name: String,
    pub size_bytes: u64,
    /// 安装类型，用于过滤 WindowsPE 等非系统镜像
    pub installation_type: String,
}

pub struct Dism;

impl Dism {
    pub fn new() -> Self {
        Self
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
        log::info!("[Dism] 使用 wimgapi 应用镜像: {} -> {}", image_file, apply_dir);

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
                log::info!("[Dism] 镜像应用成功");
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
        log::info!("[Dism] 使用 wimgapi 捕获镜像: {} -> {}", capture_dir, image_file);

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
                log::info!("[Dism] 镜像捕获成功");
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
        log::info!("[Dism] 使用 wimgapi 追加镜像: {} -> {}", capture_dir, image_file);

        // 对于追加操作，WimManager 的 capture_image 在文件存在时会自动追加
        self.capture_image(image_file, capture_dir, name, description, progress_tx)
    }

    /// 捕获系统镜像为ESD格式（高压缩）
    /// 使用 wimgapi.dll + LZMS 压缩
    pub fn capture_image_esd(
        &self,
        image_file: &str,
        capture_dir: &str,
        name: &str,
        description: &str,
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<()> {
        log::info!("[Dism] 使用 wimgapi 捕获ESD镜像: {} -> {}", capture_dir, image_file);

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
            WIM_COMPRESS_LZMS,
            Some(wim_tx),
        );

        let _ = forward_thread.join();

        match result {
            Ok(_) => {
                log::info!("[Dism] ESD镜像捕获成功");
                Ok(())
            }
            Err(e) => {
                anyhow::bail!("ESD镜像捕获失败: {}", e)
            }
        }
    }

    /// 增量备份ESD镜像
    pub fn append_image_esd(
        &self,
        image_file: &str,
        capture_dir: &str,
        name: &str,
        description: &str,
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<()> {
        log::info!("[Dism] 使用 wimgapi 追加ESD镜像: {} -> {}", capture_dir, image_file);
        self.capture_image_esd(image_file, capture_dir, name, description, progress_tx)
    }

    /// 捕获系统镜像为SWM分卷格式
    /// 先创建WIM，然后分割
    pub fn capture_image_swm(
        &self,
        image_file: &str,
        capture_dir: &str,
        name: &str,
        description: &str,
        split_size_mb: u32,
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<()> {
        log::info!("[Dism] 捕获SWM分卷镜像: {} -> {} (分卷大小: {}MB)", capture_dir, image_file, split_size_mb);

        // 先创建临时WIM文件
        let temp_wim = format!("{}.tmp.wim", image_file.trim_end_matches(".swm"));
        
        // Step 1: 捕获为WIM
        if let Some(ref tx) = progress_tx {
            let _ = tx.send(DismProgress {
                percentage: 0,
                status: "正在捕获镜像...".to_string(),
            });
        }

        let wim_manager = WimManager::new()
            .map_err(|e| anyhow::anyhow!("wimgapi 初始化失败: {}", e))?;

        let (wim_tx, wim_rx) = std::sync::mpsc::channel::<WimProgress>();

        let progress_tx_clone = progress_tx.clone();
        let forward_thread = std::thread::spawn(move || {
            while let Ok(progress) = wim_rx.recv() {
                if let Some(ref tx) = progress_tx_clone {
                    // 捕获阶段占80%进度
                    let _ = tx.send(DismProgress {
                        percentage: (progress.percentage as u32 * 80 / 100) as u8,
                        status: progress.status,
                    });
                }
            }
        });

        let result = wim_manager.capture_image(
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
            anyhow::bail!("捕获镜像失败: {}", e);
        }

        // Step 2: 分割WIM为SWM
        if let Some(ref tx) = progress_tx {
            let _ = tx.send(DismProgress {
                percentage: 80,
                status: "正在分割镜像...".to_string(),
            });
        }

        let split_result = wim_manager.split_wim(&temp_wim, image_file, split_size_mb as u64);

        // 清理临时WIM
        let _ = std::fs::remove_file(&temp_wim);

        match split_result {
            Ok(_) => {
                if let Some(ref tx) = progress_tx {
                    let _ = tx.send(DismProgress {
                        percentage: 100,
                        status: "分卷完成".to_string(),
                    });
                }
                log::info!("[Dism] SWM分卷镜像创建成功");
                Ok(())
            }
            Err(e) => {
                anyhow::bail!("分割镜像失败: {}", e)
            }
        }
    }

    // ========================================================================
    // 驱动操作 - 使用 dism.exe 命令行
    // ========================================================================

    /// 导入驱动到离线系统 (PE环境下使用)
    /// 使用 dism.exe 命令行实现，在 PE 环境下兼容性最佳
    pub fn add_drivers_offline(&self, image_path: &str, driver_path: &str) -> Result<()> {
        log::info!(
            "[Dism] 使用 dism.exe 离线导入驱动: {} -> {}",
            driver_path,
            image_path
        );

        // 使用 dism.exe 命令行方式导入驱动
        let dism_exe = DismExe::new()
            .map_err(|e| anyhow::anyhow!("dism.exe 初始化失败: {}", e))?;

        dism_exe.add_driver_offline(image_path, driver_path, true, false, None)?;

        log::info!("[Dism] 离线驱动导入完成");
        Ok(())
    }

    /// 导入驱动到离线系统（带进度回调）
    pub fn add_drivers_offline_with_progress(
        &self,
        image_path: &str,
        driver_path: &str,
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<()> {
        log::info!(
            "[Dism] 使用 dism.exe 离线导入驱动（带进度）: {} -> {}",
            driver_path,
            image_path
        );

        let dism_exe = DismExe::new()
            .map_err(|e| anyhow::anyhow!("dism.exe 初始化失败: {}", e))?;

        // 创建进度转换通道
        let (exe_tx, exe_rx) = std::sync::mpsc::channel::<DismExeProgress>();

        // 启动进度转发线程
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

        let result = dism_exe.add_driver_offline(image_path, driver_path, true, false, Some(exe_tx));

        // 等待转发线程结束
        let _ = forward_thread.join();

        match result {
            Ok(_) => {
                log::info!("[Dism] 离线驱动导入成功");
                Ok(())
            }
            Err(e) => {
                anyhow::bail!("离线驱动导入失败: {}", e)
            }
        }
    }

    /// 添加 CAB 更新包到离线系统
    /// 使用 dism.exe 命令行实现
    pub fn add_package_offline(&self, image_path: &str, cab_path: &str) -> Result<()> {
        log::info!(
            "[Dism] 使用 dism.exe 安装 CAB 更新包: {} -> {}",
            cab_path,
            image_path
        );

        let dism_exe = DismExe::new()
            .map_err(|e| anyhow::anyhow!("dism.exe 初始化失败: {}", e))?;

        dism_exe.add_package_offline(image_path, cab_path, false, None)?;

        log::info!("[Dism] CAB 更新包安装完成");
        Ok(())
    }

    /// 批量添加 CAB 更新包到离线系统
    pub fn add_packages_offline_from_dir(
        &self,
        image_path: &str,
        cab_dir: &str,
        progress_tx: Option<Sender<DismProgress>>,
    ) -> Result<(usize, usize)> {
        log::info!(
            "[Dism] 使用 dism.exe 批量安装 CAB 更新包: {} -> {}",
            cab_dir,
            image_path
        );

        let dism_exe = DismExe::new()
            .map_err(|e| anyhow::anyhow!("dism.exe 初始化失败: {}", e))?;

        // 创建进度转换通道
        let (exe_tx, exe_rx) = std::sync::mpsc::channel::<DismExeProgress>();

        // 启动进度转发线程
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

        let result = dism_exe.add_packages_from_directory(
            image_path,
            Path::new(cab_dir),
            Some(exe_tx),
        );

        // 等待转发线程结束
        let _ = forward_thread.join();

        match result {
            Ok((success, fail)) => {
                log::info!(
                    "[Dism] 批量 CAB 更新包安装完成: 成功 {}, 失败 {}",
                    success,
                    fail
                );
                Ok((success, fail))
            }
            Err(e) => {
                anyhow::bail!("批量 CAB 更新包安装失败: {}", e)
            }
        }
    }

    /// 检测当前是否在 PE 环境中运行
    pub fn is_pe_environment(&self) -> bool {
        // 检查 X: 盘是否存在（PE 环境的典型特征）
        if Path::new("X:\\Windows\\System32").exists() {
            return true;
        }

        // 检查系统盘符是否为 X:
        if let Ok(system_root) = std::env::var("SystemRoot") {
            if system_root.to_uppercase().starts_with("X:") {
                return true;
            }
        }

        // 检查是否存在 PE 特有的文件
        let pe_markers = [
            "X:\\Windows\\System32\\winpeshl.exe",
            "X:\\Windows\\System32\\wpeinit.exe",
            "X:\\sources\\boot.wim",
        ];

        for marker in &pe_markers {
            if Path::new(marker).exists() {
                return true;
            }
        }

        false
    }

    // ========================================================================
    // 驱动导出操作 - 使用 Windows API (导出不需要 dism.exe)
    // ========================================================================

    /// 从指定系统分区导出驱动 (PE环境下使用)
    /// 使用 Windows API 直接读取驱动存储
    pub fn export_drivers_from_system(&self, system_partition: &str, destination: &str) -> Result<()> {
        std::fs::create_dir_all(destination)?;

        log::info!(
            "[Dism] 使用 Windows API 从 {} 导出驱动到: {}",
            system_partition,
            destination
        );

        let manager = DriverManager::new()
            .map_err(|e| anyhow::anyhow!("驱动管理器初始化失败: {}", e))?;

        let count = manager.export_drivers_from_system(
            Path::new(system_partition),
            Path::new(destination),
        )?;
        log::info!("[Dism] 成功导出 {} 个驱动", count);
        Ok(())
    }

    /// 导出当前系统驱动
    /// 使用 Windows API 导出在线系统的驱动
    pub fn export_drivers(&self, destination: &str) -> Result<()> {
        std::fs::create_dir_all(destination)?;

        log::info!(
            "[Dism] 使用 Windows API 导出当前系统驱动到: {}",
            destination
        );

        let manager = DriverManager::new()
            .map_err(|e| anyhow::anyhow!("驱动管理器初始化失败: {}", e))?;

        let count = manager.export_drivers(Path::new(destination), true)?;
        log::info!("[Dism] 成功导出 {} 个驱动", count);
        Ok(())
    }

    // ========================================================================
    // 镜像信息 - 使用 wimgapi.dll + WIM XML 解析
    // ========================================================================

    /// 获取 WIM/ESD 镜像信息（所有分卷）
    /// 使用 wimgapi.dll 或直接解析 WIM XML 元数据
    #[allow(dead_code)]
    pub fn get_image_info(&self, image_file: &str) -> Result<Vec<ImageInfo>> {
        // 首先尝试使用 wimgapi
        if let Ok(wim_manager) = WimManager::new() {
            if let Ok(images) = wim_manager.get_image_info(image_file) {
                log::info!("[Dism] 从 wimgapi 成功获取 {} 个镜像信息", images.len());
                return Ok(images.into_iter().map(|img| ImageInfo {
                    index: img.index,
                    name: img.name,
                    size_bytes: img.size_bytes,
                    installation_type: img.installation_type,
                }).collect());
            }
        }

        // 尝试直接解析 WIM XML 元数据
        if let Ok(images) = Self::parse_wim_xml_metadata(image_file) {
            if !images.is_empty() {
                log::info!("[Dism] 从 WIM XML 元数据成功解析出 {} 个镜像", images.len());
                return Ok(images);
            }
        }

        anyhow::bail!("无法获取镜像信息")
    }

    /// 直接解析 WIM 文件的 XML 元数据
    fn parse_wim_xml_metadata(image_file: &str) -> Result<Vec<ImageInfo>> {
        use std::fs::File;
        use std::io::{Read, Seek, SeekFrom};

        log::info!("[Dism] 尝试直接解析 WIM XML 元数据: {}", image_file);

        let mut file = File::open(image_file)?;
        
        // 读取 WIM 文件头（208 字节）
        let mut header = [0u8; 208];
        file.read_exact(&mut header)?;

        // 验证 WIM 签名 "MSWIM\0\0\0"
        let signature = &header[0..8];
        if signature != b"MSWIM\0\0\0" {
            anyhow::bail!("不是有效的 WIM 文件");
        }

        // 从头部读取 XML 数据的偏移量和大小
        let xml_offset = u64::from_le_bytes(header[48..56].try_into().unwrap());
        let xml_size = u64::from_le_bytes(header[56..64].try_into().unwrap());

        if xml_offset == 0 || xml_size == 0 || xml_size > 100_000_000 {
            anyhow::bail!("XML 元数据位置无效");
        }

        log::info!("[Dism] XML 偏移: {}, 大小: {}", xml_offset, xml_size);

        // 读取 XML 数据
        file.seek(SeekFrom::Start(xml_offset))?;
        let mut xml_data = vec![0u8; xml_size as usize];
        file.read_exact(&mut xml_data)?;

        // XML 数据是 UTF-16LE 编码
        let xml_string = Self::decode_utf16le(&xml_data)?;
        
        // 解析 XML
        Self::parse_wim_xml(&xml_string)
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

                    if index > 0 {
                        images.push(ImageInfo {
                            index,
                            name,
                            size_bytes,
                            installation_type,
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
}

impl Default for Dism {
    fn default() -> Self {
        Self::new()
    }
}
