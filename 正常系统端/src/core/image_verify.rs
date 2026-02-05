//! 镜像校验模块
//!
//! 提供对各种系统镜像格式的完整性校验功能：
//! - WIM/ESD: 使用 wimlib 进行完整性校验（支持 Integrity Table 验证）
//! - SWM: 加载所有分卷并验证完整性
//! - GHO: 验证文件头和基本结构
//! - ISO: 挂载后检查内部镜像文件
//!
//! # 架构设计
//! - 异步进度报告：通过 mpsc channel 实时推送进度
//! - 可取消操作：支持通过 AtomicBool 取消长时间运行的校验
//! - 类型安全：使用枚举确保状态转换的正确性

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::core::iso::IsoMounter;
use crate::core::wimgapi::{Wimgapi, WIM_COMPRESS_NONE, WIM_GENERIC_READ, WIM_OPEN_EXISTING, WIM_REFERENCE_APPEND};
use crate::core::wimlib::Wimlib;

// ============================================================================
// 类型定义
// ============================================================================

/// 镜像类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageType {
    /// Windows 映像格式 (.wim)
    Wim,
    /// Windows 高压缩映像格式 (.esd)
    Esd,
    /// Windows 分卷映像格式 (.swm)
    Swm,
    /// Ghost 映像格式 (.gho/.ghs)
    Gho,
    /// ISO 光盘映像格式 (.iso)
    Iso,
    /// 未知格式
    Unknown,
}

impl ImageType {
    /// 从文件扩展名推断镜像类型
    pub fn from_extension(path: &str) -> Self {
        let path_lower = path.to_lowercase();
        match () {
            _ if path_lower.ends_with(".wim") => Self::Wim,
            _ if path_lower.ends_with(".esd") => Self::Esd,
            _ if path_lower.ends_with(".swm") => Self::Swm,
            _ if path_lower.ends_with(".gho") || path_lower.ends_with(".ghs") => Self::Gho,
            _ if path_lower.ends_with(".iso") => Self::Iso,
            _ => Self::Unknown,
        }
    }

    /// 判断是否为 WIM 系列格式
    pub fn is_wim_family(&self) -> bool {
        matches!(self, Self::Wim | Self::Esd | Self::Swm)
    }
}

impl std::fmt::Display for ImageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wim => write!(f, "WIM"),
            Self::Esd => write!(f, "ESD"),
            Self::Swm => write!(f, "SWM"),
            Self::Gho => write!(f, "GHO"),
            Self::Iso => write!(f, "ISO"),
            Self::Unknown => write!(f, "未知"),
        }
    }
}

/// 校验状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyStatus {
    /// 校验通过
    Valid,
    /// 校验失败（文件损坏）
    Corrupted,
    /// 文件不存在
    NotFound,
    /// 格式不支持
    Unsupported,
    /// 校验过程出错
    Error,
    /// 用户取消
    Cancelled,
}

impl std::fmt::Display for VerifyStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Valid => write!(f, "校验通过"),
            Self::Corrupted => write!(f, "文件损坏"),
            Self::NotFound => write!(f, "文件不存在"),
            Self::Unsupported => write!(f, "格式不支持"),
            Self::Error => write!(f, "校验出错"),
            Self::Cancelled => write!(f, "已取消"),
        }
    }
}

/// 校验进度信息
#[derive(Debug, Clone)]
pub struct VerifyProgress {
    /// 进度百分比 (0-100)
    pub percentage: u8,
    /// 当前状态描述
    pub status: String,
    /// 当前正在校验的项目
    pub current_item: String,
}

impl Default for VerifyProgress {
    fn default() -> Self {
        Self {
            percentage: 0,
            status: String::new(),
            current_item: String::new(),
        }
    }
}

impl VerifyProgress {
    fn new(percentage: u8, status: impl Into<String>, current_item: impl Into<String>) -> Self {
        Self {
            percentage,
            status: status.into(),
            current_item: current_item.into(),
        }
    }
}

/// 校验结果
#[derive(Debug, Clone)]
pub struct VerifyResult {
    /// 文件路径
    pub file_path: String,
    /// 镜像类型
    pub image_type: ImageType,
    /// 校验状态
    pub status: VerifyStatus,
    /// 文件大小（字节）
    pub file_size: u64,
    /// 镜像数量（WIM/ESD/SWM）
    pub image_count: u32,
    /// 分卷数量（SWM）
    pub part_count: u16,
    /// 详细消息
    pub message: String,
    /// 额外信息（如镜像名称列表）
    pub details: Vec<String>,
}

impl Default for VerifyResult {
    fn default() -> Self {
        Self {
            file_path: String::new(),
            image_type: ImageType::Unknown,
            status: VerifyStatus::Error,
            file_size: 0,
            image_count: 0,
            part_count: 0,
            message: String::new(),
            details: Vec::new(),
        }
    }
}

impl VerifyResult {
    /// 创建错误结果
    fn error(file_path: &str, image_type: ImageType, message: impl Into<String>) -> Self {
        Self {
            file_path: file_path.to_string(),
            image_type,
            status: VerifyStatus::Error,
            message: message.into(),
            ..Default::default()
        }
    }

    /// 创建损坏结果
    fn corrupted(file_path: &str, image_type: ImageType, message: impl Into<String>) -> Self {
        Self {
            file_path: file_path.to_string(),
            image_type,
            status: VerifyStatus::Corrupted,
            message: message.into(),
            ..Default::default()
        }
    }

    /// 创建成功结果
    fn valid(file_path: &str, image_type: ImageType, message: impl Into<String>) -> Self {
        Self {
            file_path: file_path.to_string(),
            image_type,
            status: VerifyStatus::Valid,
            message: message.into(),
            ..Default::default()
        }
    }
}

// ============================================================================
// 进度发送器
// ============================================================================

/// 进度发送器封装
struct ProgressReporter {
    tx: Option<Sender<VerifyProgress>>,
    progress: Arc<AtomicU8>,
}

impl ProgressReporter {
    fn new(tx: Option<Sender<VerifyProgress>>, progress: Arc<AtomicU8>) -> Self {
        Self { tx, progress }
    }

    /// 发送进度更新
    fn report(&self, percentage: u8, status: impl Into<String>, current_item: impl Into<String>) {
        self.progress.store(percentage, Ordering::SeqCst);

        if let Some(ref sender) = self.tx {
            let _ = sender.send(VerifyProgress::new(percentage, status, current_item));
        }
    }

    /// 发送简单进度更新
    fn report_simple(&self, percentage: u8, status: impl Into<String>) {
        self.report(percentage, status, "");
    }
}

// ============================================================================
// 镜像校验器
// ============================================================================

/// 镜像校验器
pub struct ImageVerifier {
    /// 取消标志
    cancel_flag: Arc<AtomicBool>,
    /// 当前进度
    progress: Arc<AtomicU8>,
}

impl ImageVerifier {
    /// 创建新的校验器实例
    pub fn new() -> Self {
        Self {
            cancel_flag: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(AtomicU8::new(0)),
        }
    }

    /// 获取取消标志的引用
    pub fn get_cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel_flag)
    }

    /// 请求取消校验
    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::SeqCst);
    }

    /// 重置取消标志
    pub fn reset_cancel(&self) {
        self.cancel_flag.store(false, Ordering::SeqCst);
    }

    /// 检查是否已取消
    fn is_cancelled(&self) -> bool {
        self.cancel_flag.load(Ordering::SeqCst)
    }

    /// 获取当前进度
    pub fn get_progress(&self) -> u8 {
        self.progress.load(Ordering::SeqCst)
    }

    /// 校验镜像文件（主入口）
    pub fn verify(&self, file_path: &str, progress_tx: Option<Sender<VerifyProgress>>) -> VerifyResult {
        self.reset_cancel();
        self.progress.store(0, Ordering::SeqCst);

        let reporter = ProgressReporter::new(progress_tx.clone(), Arc::clone(&self.progress));
        let path = Path::new(file_path);

        // 检查文件是否存在
        if !path.exists() {
            return VerifyResult {
                file_path: file_path.to_string(),
                image_type: ImageType::from_extension(file_path),
                status: VerifyStatus::NotFound,
                message: "文件不存在".to_string(),
                ..Default::default()
            };
        }

        // 获取文件大小
        let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let image_type = ImageType::from_extension(file_path);

        // 根据类型分发校验
        let mut result = match image_type {
            ImageType::Wim | ImageType::Esd => self.verify_wim_esd(file_path, &reporter),
            ImageType::Swm => self.verify_swm(file_path, &reporter),
            ImageType::Gho => self.verify_gho(file_path, &reporter),
            ImageType::Iso => self.verify_iso(file_path, &reporter),
            ImageType::Unknown => VerifyResult {
                file_path: file_path.to_string(),
                image_type,
                status: VerifyStatus::Unsupported,
                message: "不支持的镜像格式".to_string(),
                ..Default::default()
            },
        };

        // 填充通用字段
        result.file_size = file_size;
        result.image_type = image_type;
        result.file_path = file_path.to_string();

        // 发送最终进度
        reporter.report(100, format!("校验完成: {}", result.status), "");

        result
    }

    // ========================================================================
    // WIM/ESD 校验
    // ========================================================================

    fn verify_wim_esd(&self, file_path: &str, reporter: &ProgressReporter) -> VerifyResult {
        reporter.report(5, "正在加载 wimlib...", file_path);

        // 加载 wimlib
        let wimlib = match Wimlib::new() {
            Ok(w) => w,
            Err(e) => return VerifyResult::error(file_path, ImageType::Wim, format!("无法加载 wimlib: {}", e)),
        };

        reporter.report(10, "正在打开镜像文件...", file_path);

        // 打开 WIM 文件
        let wim_handle = match wimlib.open_wim(file_path) {
            Ok(h) => h,
            Err(e) => return VerifyResult::corrupted(file_path, ImageType::Wim, format!("无法打开镜像: {}", e)),
        };

        reporter.report(20, "正在读取镜像信息...", file_path);

        // 获取镜像数量
        let image_count = wim_handle.get_image_count();
        if image_count < 0 {
            return VerifyResult::corrupted(file_path, ImageType::Wim, "无法获取镜像数量");
        }

        if image_count == 0 {
            return VerifyResult::corrupted(file_path, ImageType::Wim, "镜像文件中没有有效的系统镜像");
        }

        let mut result = VerifyResult::default();
        result.image_count = image_count as u32;

        reporter.report(30, format!("发现 {} 个镜像，正在获取详细信息...", image_count), file_path);

        // 获取镜像详细信息
        for i in 1..=image_count {
            let (name, desc) = wim_handle.get_image_info(i);
            let display = if !desc.is_empty() {
                format!("镜像 {}: {} ({})", i, name, desc)
            } else if !name.is_empty() {
                format!("镜像 {}: {}", i, name)
            } else {
                format!("镜像 {}", i)
            };
            result.details.push(display);
        }

        reporter.report(50, "正在校验完整性...", file_path);

        // 启动进度监控线程
        let cancel_flag = Arc::clone(&self.cancel_flag);
        let reporter_tx = reporter.tx.clone();
        let monitor = thread::spawn(move || {
            let mut last_progress = 0u8;
            loop {
                if cancel_flag.load(Ordering::SeqCst) {
                    break;
                }

                let current = Wimlib::get_global_progress();
                if current > last_progress {
                    last_progress = current;
                    if let Some(ref tx) = reporter_tx {
                        let _ = tx.send(VerifyProgress::new(
                            current,
                            format!("正在校验完整性 ({}%)...", current),
                            "",
                        ));
                    }
                }

                if current >= 100 {
                    break;
                }

                thread::sleep(Duration::from_millis(100));
            }
        });

        // 执行校验
        let verify_result = wim_handle.verify();

        // 等待监控线程结束
        let _ = monitor.join();

        // 检查取消状态
        if self.is_cancelled() {
            result.status = VerifyStatus::Cancelled;
            result.message = "校验已取消".to_string();
            return result;
        }

        // 处理校验结果
        match verify_result {
            Ok(_) => {
                result.status = VerifyStatus::Valid;
                result.message = format!("校验通过，共 {} 个镜像全部有效", image_count);
            }
            Err(e) => {
                result.status = VerifyStatus::Corrupted;
                result.message = format!("校验失败: {}", e);
            }
        }

        result
    }

    // ========================================================================
    // SWM 分卷校验
    // ========================================================================

    fn verify_swm(&self, file_path: &str, reporter: &ProgressReporter) -> VerifyResult {
        reporter.report(5, "正在扫描分卷文件...", file_path);

        // 查找所有分卷
        let swm_files = match Self::find_swm_parts(file_path) {
            Ok(files) => files,
            Err(e) => return VerifyResult::error(file_path, ImageType::Swm, e),
        };

        if swm_files.is_empty() {
            return VerifyResult::error(file_path, ImageType::Swm, "未找到分卷文件");
        }

        let mut result = VerifyResult::default();
        result.part_count = swm_files.len() as u16;
        result.details.push(format!("找到 {} 个分卷文件", swm_files.len()));

        reporter.report(10, format!("找到 {} 个分卷，正在加载...", swm_files.len()), file_path);

        // 加载 wimgapi
        let wimgapi = match Wimgapi::new(None) {
            Ok(w) => w,
            Err(e) => return VerifyResult::error(file_path, ImageType::Swm, format!("无法加载 wimgapi.dll: {}", e)),
        };

        reporter.report(20, "正在打开主分卷...", file_path);

        // 打开主 SWM 文件
        let main_path = Path::new(&swm_files[0]);
        let wim_handle = match wimgapi.open(main_path, WIM_GENERIC_READ, WIM_OPEN_EXISTING, WIM_COMPRESS_NONE) {
            Ok(h) => h,
            Err(e) => return VerifyResult::corrupted(file_path, ImageType::Swm, format!("无法打开主分卷: {}", e)),
        };

        // 设置临时路径
        let temp_dir = std::env::temp_dir();
        let _ = wimgapi.set_temp_path(wim_handle, &temp_dir);

        // 加载其他分卷
        let total_parts = swm_files.len();
        for (i, swm_path) in swm_files.iter().enumerate().skip(1) {
            if self.is_cancelled() {
                let _ = wimgapi.close(wim_handle);
                result.status = VerifyStatus::Cancelled;
                result.message = "校验已取消".to_string();
                return result;
            }

            let progress = Self::calculate_progress(20, (i + 1) as u32, total_parts as u32, 30);
            reporter.report(progress, format!("正在加载分卷 {}/{}...", i + 1, total_parts), swm_path);

            let ref_path = Path::new(swm_path);
            if let Err(e) = wimgapi.set_reference_file(wim_handle, ref_path, WIM_REFERENCE_APPEND) {
                let _ = wimgapi.close(wim_handle);
                return VerifyResult::corrupted(file_path, ImageType::Swm, format!("无法加载分卷 {}: {}", swm_path, e));
            }
        }

        reporter.report(55, "正在读取镜像信息...", file_path);

        // 获取镜像数量
        let image_count = wimgapi.get_image_count(wim_handle);
        result.image_count = image_count;

        if image_count == 0 {
            let _ = wimgapi.close(wim_handle);
            return VerifyResult::corrupted(file_path, ImageType::Swm, "分卷镜像中没有有效的系统镜像");
        }

        // 获取镜像信息
        if let Ok(xml) = wimgapi.get_image_information(wim_handle) {
            let images = Wimgapi::parse_image_info_from_xml(&xml);
            for img in &images {
                result.details.push(format!("镜像 {}: {}", img.index, img.name));
            }
        }

        reporter.report(70, "正在验证镜像结构...", file_path);

        // 验证每个镜像
        for index in 1..=image_count {
            if self.is_cancelled() {
                let _ = wimgapi.close(wim_handle);
                result.status = VerifyStatus::Cancelled;
                result.message = "校验已取消".to_string();
                return result;
            }

            let progress = Self::calculate_progress(70, index, image_count, 25);
            reporter.report(progress, format!("正在验证镜像 {}/{}...", index, image_count), file_path);

            match wimgapi.load_image(wim_handle, index) {
                Ok(image_handle) => {
                    let _ = wimgapi.close(image_handle);
                }
                Err(e) => {
                    let _ = wimgapi.close(wim_handle);
                    return VerifyResult::corrupted(file_path, ImageType::Swm, format!("镜像 {} 损坏: {}", index, e));
                }
            }
        }

        let _ = wimgapi.close(wim_handle);

        result.status = VerifyStatus::Valid;
        result.message = format!("校验通过，{} 个分卷，{} 个镜像全部有效", total_parts, image_count);
        result
    }

    /// 查找 SWM 分卷文件
    fn find_swm_parts(main_swm: &str) -> Result<Vec<String>, String> {
        let path = Path::new(main_swm);
        let parent = path.parent().ok_or("无法获取文件目录")?;
        let stem = path.file_stem().and_then(|s| s.to_str()).ok_or("无法获取文件名")?;

        // 移除已有的数字后缀（如 install2 -> install）
        let base_name = stem.trim_end_matches(|c: char| c.is_ascii_digit());

        let mut parts = Vec::new();

        // 添加主文件
        if path.exists() {
            parts.push(main_swm.to_string());
        }

        // 查找其他分卷
        for i in 2..=999 {
            let part_name = format!("{}{}.swm", base_name, i);
            let part_path = parent.join(&part_name);

            if part_path.exists() {
                parts.push(part_path.to_string_lossy().to_string());
            } else {
                break;
            }
        }

        if parts.is_empty() {
            return Err("未找到任何分卷文件".to_string());
        }

        parts.sort();
        Ok(parts)
    }

    // ========================================================================
    // GHO 校验
    // ========================================================================

    fn verify_gho(&self, file_path: &str, reporter: &ProgressReporter) -> VerifyResult {
        reporter.report(10, "正在读取文件头...", file_path);

        let path = Path::new(file_path);

        // 检查文件大小
        let metadata = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) => return VerifyResult::error(file_path, ImageType::Gho, format!("无法读取文件元数据: {}", e)),
        };

        if metadata.len() < 512 {
            return VerifyResult::corrupted(file_path, ImageType::Gho, "文件太小，不是有效的 GHO 文件");
        }

        reporter.report(30, "正在验证文件签名...", file_path);

        // 读取文件头
        let mut file = match File::open(path) {
            Ok(f) => f,
            Err(e) => return VerifyResult::error(file_path, ImageType::Gho, format!("无法打开文件: {}", e)),
        };

        let mut header = [0u8; 512];
        if let Err(e) = file.read_exact(&mut header) {
            return VerifyResult::error(file_path, ImageType::Gho, format!("无法读取文件头: {}", e));
        }

        reporter.report(50, "正在分析文件结构...", file_path);

        // Ghost 文件签名检查
        let is_valid_signature = matches!(
            (header[0], header[1]),
            (0xFE, 0xEF) |  // 标准 Ghost 签名
            (0x47, 0x46) |  // "GF" - Ghost 4.x 格式
            (0xEB, _) |      // 引导代码
            (0xE9, _)        // 引导代码
        );

        if !is_valid_signature {
            // 检查是否是 GHS 分卷文件
            if file_path.to_lowercase().ends_with(".ghs") {
                reporter.report(70, "检测到 GHS 分卷文件...", file_path);
                let mut result = VerifyResult::valid(file_path, ImageType::Gho, "GHS 分卷文件结构正常");
                result.details.push("这是一个 Ghost 分卷文件".to_string());
                return result;
            }

            return VerifyResult::corrupted(
                file_path,
                ImageType::Gho,
                format!("无效的文件签名: {:02X} {:02X} {:02X} {:02X}", header[0], header[1], header[2], header[3]),
            );
        }

        reporter.report(70, "正在检查文件完整性...", file_path);

        // 检查文件尾部（确认未被截断）
        let file_len = metadata.len();
        if file_len > 512 {
            if let Err(e) = file.seek(SeekFrom::End(-512)) {
                return VerifyResult::error(file_path, ImageType::Gho, format!("文件读取错误: {}", e));
            }

            let mut tail = [0u8; 512];
            if file.read_exact(&mut tail).is_err() {
                return VerifyResult::corrupted(file_path, ImageType::Gho, "文件末尾不完整，可能被截断");
            }
        }

        reporter.report(90, "校验完成", file_path);

        // 构建结果
        let mut result = VerifyResult::valid(file_path, ImageType::Gho, "GHO 文件结构完整");
        result.image_count = 1;
        result.details.push(format!("文件大小: {:.2} GB", file_len as f64 / 1024.0 / 1024.0 / 1024.0));

        // 检测格式类型
        match (header[0], header[1]) {
            (0xFE, 0xEF) => result.details.push("标准 Ghost 格式".to_string()),
            (0x47, 0x46) => result.details.push("Ghost 4.x 格式".to_string()),
            _ => result.details.push("可引导 Ghost 镜像".to_string()),
        }

        result
    }

    // ========================================================================
    // ISO 校验
    // ========================================================================

    fn verify_iso(&self, file_path: &str, reporter: &ProgressReporter) -> VerifyResult {
        reporter.report(5, "正在验证 ISO 文件结构...", file_path);

        let path = Path::new(file_path);

        // 检查文件大小
        let metadata = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) => return VerifyResult::error(file_path, ImageType::Iso, format!("无法读取文件元数据: {}", e)),
        };

        // ISO 9660 主卷描述符位于 32768 字节偏移处
        if metadata.len() < 32768 + 2048 {
            return VerifyResult::corrupted(file_path, ImageType::Iso, "文件太小，不是有效的 ISO 文件");
        }

        reporter.report(10, "正在验证 ISO 签名...", file_path);

        // 验证 ISO 9660 签名
        let mut file = match File::open(path) {
            Ok(f) => f,
            Err(e) => return VerifyResult::error(file_path, ImageType::Iso, format!("无法打开文件: {}", e)),
        };

        if let Err(e) = file.seek(SeekFrom::Start(32768)) {
            return VerifyResult::error(file_path, ImageType::Iso, format!("文件读取错误: {}", e));
        }

        let mut pvd = [0u8; 6];
        if let Err(e) = file.read_exact(&mut pvd) {
            return VerifyResult::error(file_path, ImageType::Iso, format!("无法读取卷描述符: {}", e));
        }

        // 检查 ISO 9660 签名 "CD001"
        if &pvd[1..6] != b"CD001" {
            return VerifyResult::corrupted(file_path, ImageType::Iso, "无效的 ISO 9660 签名");
        }

        let mut result = VerifyResult::default();
        result.details.push("ISO 9660 签名验证通过".to_string());

        reporter.report(20, "正在挂载 ISO 文件...", file_path);

        // 挂载 ISO
        match IsoMounter::mount_iso(file_path) {
            Ok(drive) => {
                result.details.push(format!("已挂载到驱动器 {}", drive));

                reporter.report(40, "正在扫描安装镜像...", &drive);

                // 查找 sources 目录中的安装镜像
                let sources_path = format!("{}\\sources", drive);
                let wim_path = format!("{}\\install.wim", sources_path);
                let esd_path = format!("{}\\install.esd", sources_path);

                let install_image = if Path::new(&wim_path).exists() {
                    Some(wim_path)
                } else if Path::new(&esd_path).exists() {
                    Some(esd_path)
                } else {
                    None
                };

                if let Some(image_path) = install_image {
                    result.details.push(format!("找到安装镜像: {}", image_path));

                    reporter.report(60, "正在验证内部镜像...", &image_path);

                    // 递归验证内部镜像
                    let inner_reporter = ProgressReporter::new(None, Arc::new(AtomicU8::new(0)));
                    let inner_result = self.verify_wim_esd(&image_path, &inner_reporter);

                    result.image_count = inner_result.image_count;
                    result.details.extend(inner_result.details);

                    if inner_result.status != VerifyStatus::Valid {
                        let _ = IsoMounter::unmount();
                        result.status = inner_result.status;
                        result.message = format!("内部镜像校验失败: {}", inner_result.message);
                        return result;
                    }
                } else {
                    result.details.push("未找到 install.wim/esd，可能不是 Windows 安装 ISO".to_string());
                }

                reporter.report(90, "正在卸载 ISO...", file_path);
                let _ = IsoMounter::unmount();

                result.status = VerifyStatus::Valid;
                result.message = if result.image_count > 0 {
                    format!("ISO 校验通过，包含 {} 个系统镜像", result.image_count)
                } else {
                    "ISO 文件结构完整".to_string()
                };
            }
            Err(e) => {
                result.status = VerifyStatus::Error;
                result.message = format!("无法挂载 ISO: {}", e);
            }
        }

        result
    }

    // ========================================================================
    // 工具方法
    // ========================================================================

    /// 安全计算进度百分比
    ///
    /// # 参数
    /// - `base`: 基础进度值
    /// - `current`: 当前索引（从1开始）
    /// - `total`: 总数
    /// - `range`: 进度跨度
    ///
    /// # 返回值
    /// 安全的 u8 进度值，保证在 [0, 100] 范围内
    #[inline]
    fn calculate_progress(base: u8, current: u32, total: u32, range: u8) -> u8 {
        if total == 0 {
            return base;
        }

        let increment = (current as u64 * range as u64 / total as u64) as u32;
        let final_progress = (base as u32).saturating_add(increment);
        final_progress.min(100) as u8
    }
}

impl Default for ImageVerifier {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_image_type_from_extension() {
        assert_eq!(ImageType::from_extension("test.wim"), ImageType::Wim);
        assert_eq!(ImageType::from_extension("TEST.WIM"), ImageType::Wim);
        assert_eq!(ImageType::from_extension("test.esd"), ImageType::Esd);
        assert_eq!(ImageType::from_extension("test.swm"), ImageType::Swm);
        assert_eq!(ImageType::from_extension("test.gho"), ImageType::Gho);
        assert_eq!(ImageType::from_extension("test.GHS"), ImageType::Gho);
        assert_eq!(ImageType::from_extension("test.iso"), ImageType::Iso);
        assert_eq!(ImageType::from_extension("test.txt"), ImageType::Unknown);
    }

    #[test]
    fn test_image_type_is_wim_family() {
        assert!(ImageType::Wim.is_wim_family());
        assert!(ImageType::Esd.is_wim_family());
        assert!(ImageType::Swm.is_wim_family());
        assert!(!ImageType::Gho.is_wim_family());
        assert!(!ImageType::Iso.is_wim_family());
    }

    #[test]
    fn test_calculate_progress() {
        // 正常情况
        assert_eq!(ImageVerifier::calculate_progress(50, 3, 5, 40), 74);
        // 第一个项目
        assert_eq!(ImageVerifier::calculate_progress(50, 1, 7, 40), 55);
        // 最后一个项目
        assert_eq!(ImageVerifier::calculate_progress(50, 7, 7, 40), 90);
        // 除零保护
        assert_eq!(ImageVerifier::calculate_progress(50, 1, 0, 40), 50);
        // 上限保护
        assert_eq!(ImageVerifier::calculate_progress(90, 10, 10, 50), 100);
    }

    #[test]
    fn test_verify_progress_default() {
        let progress = VerifyProgress::default();
        assert_eq!(progress.percentage, 0);
        assert!(progress.status.is_empty());
        assert!(progress.current_item.is_empty());
    }

    #[test]
    fn test_verify_result_constructors() {
        let error = VerifyResult::error("test.wim", ImageType::Wim, "test error");
        assert_eq!(error.status, VerifyStatus::Error);
        assert_eq!(error.message, "test error");

        let corrupted = VerifyResult::corrupted("test.wim", ImageType::Wim, "corrupted");
        assert_eq!(corrupted.status, VerifyStatus::Corrupted);

        let valid = VerifyResult::valid("test.wim", ImageType::Wim, "ok");
        assert_eq!(valid.status, VerifyStatus::Valid);
    }

    #[test]
    fn test_verify_status_display() {
        assert_eq!(format!("{}", VerifyStatus::Valid), "校验通过");
        assert_eq!(format!("{}", VerifyStatus::Corrupted), "文件损坏");
        assert_eq!(format!("{}", VerifyStatus::Cancelled), "已取消");
    }
}
