//! wimgapi.dll 动态库封装
//!
//! 该模块封装了Windows自带的wimgapi.dll库的主要功能，用于WIM/ESD镜像的处理。
//! 相比DISM命令行工具，直接调用API具有更好的性能和更精确的进度控制。
//!
//! 参考: https://learn.microsoft.com/zh-cn/windows-hardware/manufacture/desktop/wim/dd834950(v=msdn.10)?view=windows-11

#![allow(non_snake_case)]

use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use libloading::Library;

#[cfg(windows)]
use windows::Win32::Foundation::GetLastError;

// ============================================================================
// 错误类型定义
// ============================================================================

/// WIMGAPI 错误类型枚举
#[derive(Debug)]
pub enum WimApiError {
    /// Win32 API 错误
    Win32Error(u32),
    /// 库加载错误
    LibraryError(libloading::Error),
    /// 通用错误信息
    Message(String),
}

impl std::fmt::Display for WimApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WimApiError::Win32Error(code) => write!(f, "Win32 Error: {}", code),
            WimApiError::LibraryError(err) => write!(f, "Library Error: {}", err),
            WimApiError::Message(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for WimApiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WimApiError::LibraryError(err) => Some(err),
            _ => None,
        }
    }
}

impl From<libloading::Error> for WimApiError {
    fn from(err: libloading::Error) -> Self {
        WimApiError::LibraryError(err)
    }
}

// ============================================================================
// 常量定义
// ============================================================================

// 访问权限
pub const WIM_GENERIC_READ: u32 = 0x8000_0000;
pub const WIM_GENERIC_WRITE: u32 = 0x4000_0000;
#[allow(dead_code)]
pub const WIM_GENERIC_MOUNT: u32 = 0x2000_0000;

// 创建/打开模式
pub const WIM_CREATE_NEW: u32 = 1;
#[allow(dead_code)]
pub const WIM_CREATE_ALWAYS: u32 = 2;
pub const WIM_OPEN_EXISTING: u32 = 3;
#[allow(dead_code)]
pub const WIM_OPEN_ALWAYS: u32 = 4;

// 压缩类型
pub const WIM_COMPRESS_NONE: u32 = 0;
#[allow(dead_code)]
pub const WIM_COMPRESS_XPRESS: u32 = 1;
pub const WIM_COMPRESS_LZX: u32 = 2;
#[allow(dead_code)]
pub const WIM_COMPRESS_LZMS: u32 = 3;

// 消息类型
// WIM_MSG = WM_APP + 0x1476 = 0x8000 + 0x1476 = 0x9476
// WIM_MSG_TEXT = WIM_MSG + 1 = 0x9477
// WIM_MSG_PROGRESS = WIM_MSG + 2 = 0x9478
// 详见: https://github.com/jeffkl/ManagedWimgApi/blob/main/wimgapi.h
pub const WIM_MSG_PROGRESS: u32 = 0x00009478;
#[allow(dead_code)]
pub const WIM_MSG_PROCESS: u32 = 0x00009479;
pub const WIM_MSG_SCANNING: u32 = 0x0000947A;
#[allow(dead_code)]
pub const WIM_MSG_SETRANGE: u32 = 0x0000947B;
#[allow(dead_code)]
pub const WIM_MSG_SETPOS: u32 = 0x0000947C;
#[allow(dead_code)]
pub const WIM_MSG_STEPIT: u32 = 0x0000947D;
pub const WIM_MSG_COMPRESS: u32 = 0x0000947E;
pub const WIM_MSG_ERROR: u32 = 0x0000947F;
pub const WIM_MSG_SUCCESS: u32 = 0x00000000;
pub const WIM_MSG_ABORT_IMAGE: u32 = 0xFFFFFFFF;

// 路径最大长度
pub const MAX_PATH: usize = 260;

// ============================================================================
// 类型别名
// ============================================================================

type Pcwstr = *const u16;
#[allow(dead_code)]
type Pwstr = *mut u16;
type Handle = usize;

// ============================================================================
// 函数指针类型定义
// ============================================================================

type FnWimCreateFile = unsafe extern "system" fn(
    pszWimPath: Pcwstr,
    dwDesiredAccess: u32,
    dwCreationDisposition: u32,
    dwFlagsAndAttributes: u32,
    dwCompressionType: u32,
    pdwCreationResult: *mut u32,
) -> Handle;

type FnWimCloseHandle = unsafe extern "system" fn(hObject: Handle) -> i32;

type FnWimSetTemporaryPath = unsafe extern "system" fn(hWim: Handle, pszPath: Pcwstr) -> i32;

type FnWimLoadImage = unsafe extern "system" fn(hWim: Handle, dwImageIndex: u32) -> Handle;

type FnWimGetImageCount = unsafe extern "system" fn(hWim: Handle) -> u32;

type FnWimApplyImage = unsafe extern "system" fn(hImage: Handle, pszPath: Pcwstr, dwApplyFlags: u32) -> i32;

type FnWimCaptureImage = unsafe extern "system" fn(hWim: Handle, pszPath: Pcwstr, dwCaptureFlags: u32) -> Handle;

type FnWimGetImageInformation = unsafe extern "system" fn(
    hImage: Handle,
    ppvImageInfo: *mut *mut c_void,
    pcbImageInfo: *mut u32,
) -> i32;

type FnWimRegisterMessageCallback = unsafe extern "system" fn(
    hWim: Handle,
    fpMessageProc: Option<extern "system" fn(u32, usize, isize, *mut c_void) -> u32>,
    pvUserData: *mut c_void,
) -> u32;

type FnWimUnregisterMessageCallback = unsafe extern "system" fn(
    hWim: Handle,
    fpMessageProc: Option<extern "system" fn(u32, usize, isize, *mut c_void) -> u32>,
) -> i32;

type FnWimSetImageInformation = unsafe extern "system" fn(
    hImage: Handle,
    pvImageInfo: *const u8,
    cbImageInfo: u32,
) -> i32;

type FnWimGetAttributes = unsafe extern "system" fn(
    hWim: Handle,
    pWimInfo: *mut WimInfoRaw,
    cbWimInfo: u32,
) -> i32;

// ============================================================================
// 原始结构体定义
// ============================================================================

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct WimInfoRaw {
    wim_path: [u16; MAX_PATH],
    guid: [u8; 16],
    image_count: u32,
    compression_type: u32,
    part_number: u16,
    total_parts: u16,
    boot_index: u32,
    wim_attributes: u32,
    wim_flags_and_attr: u32,
}

impl Default for WimInfoRaw {
    fn default() -> Self {
        Self {
            wim_path: [0; MAX_PATH],
            guid: [0; 16],
            image_count: 0,
            compression_type: 0,
            part_number: 0,
            total_parts: 0,
            boot_index: 0,
            wim_attributes: 0,
            wim_flags_and_attr: 0,
        }
    }
}

// ============================================================================
// 公共结构体定义
// ============================================================================

/// WIM 文件信息
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct WimInfo {
    /// WIM 文件路径
    pub wim_path: String,
    /// 唯一标识符 GUID
    pub guid: [u8; 16],
    /// 镜像数量
    pub image_count: u32,
    /// 压缩类型
    pub compression_type: u32,
    /// 部件编号
    pub part_number: u16,
    /// 总部件数
    pub total_parts: u16,
    /// 引导镜像索引
    pub boot_index: u32,
    /// WIM 属性
    pub wim_attributes: u32,
    /// WIM 标志和属性
    pub wim_flags_and_attr: u32,
}

/// 镜像信息
#[derive(Debug, Clone)]
pub struct ImageInfo {
    /// 镜像索引
    pub index: u32,
    /// 镜像名称
    pub name: String,
    /// 镜像大小（字节）
    pub size_bytes: u64,
    /// 安装类型
    pub installation_type: String,
    /// 镜像描述
    pub description: String,
}

/// 操作进度
#[derive(Debug, Clone)]
pub struct WimProgress {
    /// 进度百分比 (0-100)
    pub percentage: u8,
    /// 状态描述
    pub status: String,
}

// ============================================================================
// 全局进度存储
// ============================================================================

static GLOBAL_PROGRESS: AtomicU8 = AtomicU8::new(0);

/// 进度回调函数
/// 
/// 根据 Microsoft 文档，WIM_MSG_PROGRESS 消息中：
/// - wParam: 进度百分比 (0-100)
/// - lParam: 预计剩余时间（毫秒）
/// 
/// 参考: https://learn.microsoft.com/en-us/windows-hardware/manufacture/desktop/wim/dd834944
extern "system" fn progress_callback(
    msg_id: u32,
    wparam: usize,
    _lparam: isize,
    _user_data: *mut c_void,
) -> u32 {
    match msg_id {
        WIM_MSG_PROGRESS => {
            // wParam 直接是 DWORD 百分比值 (0-100)
            // 使用 min(100) 防止异常值
            let percent = (wparam as u32).min(100) as u8;
            let old_progress = GLOBAL_PROGRESS.swap(percent, Ordering::SeqCst);
            
            // 只在进度变化时记录日志，避免日志过多
            if percent != old_progress && (percent % 5 == 0 || percent == 100) {
                log::info!("[WIMGAPI] 镜像操作进度: {}%", percent);
            }
        }
        WIM_MSG_SCANNING => {
            log::info!("[WIMGAPI] 正在扫描文件...");
        }
        WIM_MSG_COMPRESS => {
            log::info!("[WIMGAPI] 正在压缩数据...");
        }
        WIM_MSG_ERROR => {
            log::error!("[WIMGAPI] WIM操作发生错误 (msg_id={:#x})", msg_id);
            return WIM_MSG_ABORT_IMAGE;
        }
        _ => {
            // 记录未知消息类型，便于调试
            if msg_id >= 0x9476 && msg_id <= 0x94A0 {
                log::trace!("[WIMGAPI] 收到WIM消息: {:#x}, wparam={}", msg_id, wparam);
            }
        }
    }
    WIM_MSG_SUCCESS
}

// ============================================================================
// Wimgapi 主结构体
// ============================================================================

/// WIMGAPI 封装结构体
pub struct Wimgapi {
    _lib: Library,
    wim_create_file: FnWimCreateFile,
    wim_close_handle: FnWimCloseHandle,
    wim_set_temporary_path: FnWimSetTemporaryPath,
    wim_load_image: FnWimLoadImage,
    wim_get_image_count: FnWimGetImageCount,
    wim_apply_image: FnWimApplyImage,
    wim_capture_image: FnWimCaptureImage,
    wim_get_image_information: FnWimGetImageInformation,
    wim_register_message_callback: FnWimRegisterMessageCallback,
    wim_unregister_message_callback: FnWimUnregisterMessageCallback,
    wim_set_image_information: FnWimSetImageInformation,
    wim_get_attributes: FnWimGetAttributes,
}

/// 将字符串转换为以 NUL 结尾的 UTF-16 Vec
fn to_wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

/// 将路径转换为以 NUL 结尾的 UTF-16 Vec
fn path_to_wide(path: &Path) -> Vec<u16> {
    to_wide(path.as_os_str())
}

/// 将 UTF-16 指针转换为 Rust 字符串
fn utf16_ptr_to_string(ptr: *const u16, max_len: usize) -> String {
    if ptr.is_null() || max_len == 0 {
        return String::new();
    }
    unsafe {
        let slice = std::slice::from_raw_parts(ptr, max_len);
        let mut len = max_len;
        while len > 0 && slice[len - 1] == 0 {
            len -= 1;
        }
        String::from_utf16_lossy(&slice[..len])
    }
}

/// 获取最后的 Win32 错误码
#[cfg(windows)]
fn get_last_error() -> u32 {
    unsafe { GetLastError().0 }
}

#[cfg(not(windows))]
fn get_last_error() -> u32 {
    0
}

impl Wimgapi {
    /// 加载 wimgapi.dll 并解析所需函数
    pub fn new(path: Option<PathBuf>) -> Result<Self, WimApiError> {
        // 优先级：
        // 1. 用户指定的路径
        // 2. 程序目录下的 wimgapi.dll（用户可放置新版本）
        // 3. 程序 bin 目录下的 wimgapi.dll
        // 4. PE 系统目录 X:\Windows\System32\wimgapi.dll
        // 5. 默认搜索路径
        let lib_path = if let Some(p) = path {
            p
        } else {
            // 获取程序所在目录
            let exe_dir = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.to_path_buf()));
            
            // 尝试程序目录
            if let Some(ref dir) = exe_dir {
                let local_path = dir.join("wimgapi.dll");
                if local_path.exists() {
                    log::info!("[WIMGAPI] 使用程序目录 wimgapi.dll: {:?}", local_path);
                    local_path
                } else {
                    // 尝试 bin 子目录
                    let bin_path = dir.join("bin").join("wimgapi.dll");
                    if bin_path.exists() {
                        log::info!("[WIMGAPI] 使用 bin 目录 wimgapi.dll: {:?}", bin_path);
                        bin_path
                    } else {
                        // 尝试 PE 系统目录
                        let pe_system_path = PathBuf::from("X:\\Windows\\System32\\wimgapi.dll");
                        if pe_system_path.exists() {
                            log::info!("[WIMGAPI] 使用 PE 系统 wimgapi.dll: {:?}", pe_system_path);
                            pe_system_path
                        } else {
                            log::info!("[WIMGAPI] 使用默认 wimgapi.dll");
                            PathBuf::from("wimgapi.dll")
                        }
                    }
                }
            } else {
                // 无法获取程序目录，使用默认
                log::info!("[WIMGAPI] 使用默认 wimgapi.dll");
                PathBuf::from("wimgapi.dll")
            }
        };
        
        log::info!("[WIMGAPI] 加载 wimgapi.dll: {:?}", lib_path);
        let lib = unsafe { Library::new(&lib_path) }?;

        unsafe {
            Ok(Self {
                wim_create_file: *lib.get(b"WIMCreateFile")?,
                wim_close_handle: *lib.get(b"WIMCloseHandle")?,
                wim_set_temporary_path: *lib.get(b"WIMSetTemporaryPath")?,
                wim_load_image: *lib.get(b"WIMLoadImage")?,
                wim_get_image_count: *lib.get(b"WIMGetImageCount")?,
                wim_apply_image: *lib.get(b"WIMApplyImage")?,
                wim_capture_image: *lib.get(b"WIMCaptureImage")?,
                wim_get_image_information: *lib.get(b"WIMGetImageInformation")?,
                wim_register_message_callback: *lib.get(b"WIMRegisterMessageCallback")?,
                wim_unregister_message_callback: *lib.get(b"WIMUnregisterMessageCallback")?,
                wim_set_image_information: *lib.get(b"WIMSetImageInformation")?,
                wim_get_attributes: *lib.get(b"WIMGetAttributes")?,
                _lib: lib,
            })
        }
    }

    /// 打开 WIM 文件
    pub fn open(
        &self,
        path: &Path,
        access: u32,
        disposition: u32,
        compression: u32,
    ) -> Result<Handle, WimApiError> {
        log::info!("[WIMGAPI] open: 准备打开文件: {:?}", path);
        log::info!("[WIMGAPI] open: access={:#x}, disposition={}, compression={}", access, disposition, compression);
        
        let wide_path = path_to_wide(path);
        let mut creation_result: u32 = 0;

        log::info!("[WIMGAPI] open: 即将调用 WIMCreateFile...");

        let handle = unsafe {
            (self.wim_create_file)(
                wide_path.as_ptr(),
                access,
                disposition,
                0,
                compression,
                &mut creation_result,
            )
        };

        log::info!("[WIMGAPI] open: WIMCreateFile 返回, handle={}, creation_result={}", handle, creation_result);

        if handle == 0 {
            let err = get_last_error();
            log::error!("[WIMGAPI] open: 打开失败, 错误码={}", err);
            return Err(WimApiError::Win32Error(err));
        }

        log::info!("[WIMGAPI] open: 文件打开成功");
        Ok(handle)
    }

    /// 关闭句柄
    pub fn close(&self, handle: Handle) -> Result<(), WimApiError> {
        let result = unsafe { (self.wim_close_handle)(handle) };
        if result == 0 {
            return Err(WimApiError::Win32Error(get_last_error()));
        }
        Ok(())
    }

    /// 设置临时文件路径
    pub fn set_temp_path(&self, handle: Handle, path: &Path) -> Result<(), WimApiError> {
        log::info!("[WIMGAPI] set_temp_path: 即将调用 WIMSetTemporaryPath...");
        let wide_path = path_to_wide(path);
        let result = unsafe { (self.wim_set_temporary_path)(handle, wide_path.as_ptr()) };
        log::info!("[WIMGAPI] set_temp_path: 返回 result={}", result);
        if result == 0 {
            let err = get_last_error();
            log::error!("[WIMGAPI] set_temp_path: 失败, 错误码={}", err);
            return Err(WimApiError::Win32Error(err));
        }
        Ok(())
    }

    /// 加载镜像
    pub fn load_image(&self, handle: Handle, index: u32) -> Result<Handle, WimApiError> {
        log::info!("[WIMGAPI] load_image: 即将调用 WIMLoadImage(index={})...", index);
        let image_handle = unsafe { (self.wim_load_image)(handle, index) };
        log::info!("[WIMGAPI] load_image: 返回 handle={}", image_handle);
        if image_handle == 0 {
            let err = get_last_error();
            log::error!("[WIMGAPI] load_image: 失败, 错误码={}", err);
            return Err(WimApiError::Win32Error(err));
        }
        Ok(image_handle)
    }

    /// 获取镜像数量
    #[allow(dead_code)]
    pub fn get_image_count(&self, handle: Handle) -> u32 {
        unsafe { (self.wim_get_image_count)(handle) }
    }

    /// 注册消息回调
    /// 返回注册结果，INVALID_CALLBACK_VALUE (0xFFFFFFFF) 表示失败
    pub fn register_callback(&self, handle: Handle) -> u32 {
        // 重置全局进度为0
        GLOBAL_PROGRESS.store(0, Ordering::SeqCst);
        
        let result = unsafe {
            (self.wim_register_message_callback)(handle, Some(progress_callback), null_mut())
        };
        
        // 检查注册结果
        if result == 0xFFFFFFFF {
            let err = get_last_error();
            log::error!("[WIMGAPI] 回调注册失败, 错误码={}", err);
        } else {
            log::info!("[WIMGAPI] 回调注册成功, callback_id={}", result);
        }
        
        result
    }

    /// 取消注册消息回调
    pub fn unregister_callback(&self, handle: Handle) {
        unsafe {
            (self.wim_unregister_message_callback)(handle, Some(progress_callback));
        }
    }

    /// 应用/释放镜像到指定目录
    pub fn apply_image(
        &self,
        image_handle: Handle,
        target_path: &Path,
        flags: u32,
    ) -> Result<(), WimApiError> {
        log::info!("[WIMGAPI] apply_image: 即将调用 WIMApplyImage(target={:?}, flags={})...", target_path, flags);
        let wide_path = path_to_wide(target_path);
        let result = unsafe { (self.wim_apply_image)(image_handle, wide_path.as_ptr(), flags) };
        log::info!("[WIMGAPI] apply_image: 返回 result={}", result);
        if result == 0 {
            let err = get_last_error();
            log::error!("[WIMGAPI] apply_image: 失败, 错误码={}", err);
            return Err(WimApiError::Win32Error(err));
        }
        Ok(())
    }

    /// 捕获/备份目录到 WIM 文件
    pub fn capture_image(
        &self,
        handle: Handle,
        source_path: &Path,
        flags: u32,
    ) -> Result<Handle, WimApiError> {
        let wide_path = path_to_wide(source_path);
        let image_handle = unsafe { (self.wim_capture_image)(handle, wide_path.as_ptr(), flags) };
        if image_handle == 0 {
            return Err(WimApiError::Win32Error(get_last_error()));
        }
        Ok(image_handle)
    }

    /// 获取镜像 XML 信息
    pub fn get_image_information(&self, handle: Handle) -> Result<String, WimApiError> {
        let mut pv: *mut c_void = null_mut();
        let mut size: u32 = 0;

        let result = unsafe {
            (self.wim_get_image_information)(handle, &mut pv, &mut size)
        };

        if result == 0 {
            return Err(WimApiError::Win32Error(get_last_error()));
        }

        let xml_string = utf16_ptr_to_string(pv as *const u16, (size as usize) / 2);
        Ok(xml_string)
    }

    /// 设置镜像信息
    #[allow(dead_code)]
    pub fn set_image_information(
        &self,
        handle: Handle,
        xml_info: &str,
    ) -> Result<(), WimApiError> {
        let utf16_chars: Vec<u16> = xml_info.encode_utf16().collect();
        let buffer_size = (utf16_chars.len() * std::mem::size_of::<u16>()) as u32;

        let result = unsafe {
            (self.wim_set_image_information)(handle, utf16_chars.as_ptr() as *const u8, buffer_size)
        };

        if result == 0 {
            return Err(WimApiError::Win32Error(get_last_error()));
        }
        Ok(())
    }

    /// 获取 WIM 文件属性
    #[allow(dead_code)]
    pub fn get_attributes(&self, handle: Handle) -> Result<WimInfo, WimApiError> {
        let mut raw = WimInfoRaw::default();
        let size = std::mem::size_of::<WimInfoRaw>() as u32;

        let result = unsafe { (self.wim_get_attributes)(handle, &mut raw, size) };
        if result == 0 {
            return Err(WimApiError::Win32Error(get_last_error()));
        }

        Ok(WimInfo {
            wim_path: utf16_ptr_to_string(raw.wim_path.as_ptr(), MAX_PATH),
            guid: raw.guid,
            image_count: raw.image_count,
            compression_type: raw.compression_type,
            part_number: raw.part_number,
            total_parts: raw.total_parts,
            boot_index: raw.boot_index,
            wim_attributes: raw.wim_attributes,
            wim_flags_and_attr: raw.wim_flags_and_attr,
        })
    }

    /// 解析镜像 XML 获取镜像信息列表
    pub fn parse_image_info_from_xml(xml: &str) -> Vec<ImageInfo> {
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

                    // 优先使用 DISPLAYNAME，其次使用 NAME
                    let name = Self::extract_xml_tag(image_block, "DISPLAYNAME")
                        .or_else(|| Self::extract_xml_tag(image_block, "NAME"))
                        .unwrap_or_default();

                    let size_bytes = Self::extract_xml_tag(image_block, "TOTALBYTES")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);

                    let installation_type = Self::extract_xml_tag(image_block, "INSTALLATIONTYPE")
                        .unwrap_or_default();

                    let description = Self::extract_xml_tag(image_block, "DESCRIPTION")
                        .unwrap_or_default();

                    if index > 0 {
                        images.push(ImageInfo {
                            index,
                            name,
                            size_bytes,
                            installation_type,
                            description,
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

        images
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

// ============================================================================
// 高级封装接口
// ============================================================================

/// WIM 镜像管理器
/// 提供更易用的高级接口
pub struct WimManager {
    wimgapi: Wimgapi,
}

impl WimManager {
    /// 创建 WIM 管理器实例
    pub fn new() -> Result<Self, WimApiError> {
        Ok(Self {
            wimgapi: Wimgapi::new(None)?,
        })
    }

    /// 释放/应用 WIM/ESD 镜像到目标目录
    pub fn apply_image(
        &self,
        image_file: &str,
        target_dir: &str,
        index: u32,
        progress_tx: Option<std::sync::mpsc::Sender<WimProgress>>,
    ) -> Result<(), WimApiError> {
        let image_path = Path::new(image_file);
        let target_path = Path::new(target_dir);
        
        // PE环境下使用可靠的临时目录
        // 优先级: X:\Windows\Temp -> 目标分区根目录
        let temp_dir = {
            let pe_temp = PathBuf::from("X:\\Windows\\Temp");
            if pe_temp.exists() {
                pe_temp
            } else {
                // 如果X盘temp不存在，使用目标分区
                let target_temp = Path::new(target_dir).join("$WIM_TEMP$");
                let _ = std::fs::create_dir_all(&target_temp);
                target_temp
            }
        };

        log::info!("[WIMGAPI] 开始释放镜像: {} -> {}", image_file, target_dir);
        log::info!("[WIMGAPI] 镜像索引: {}", index);

        // 打开 WIM 文件
        let wim_handle = self.wimgapi.open(
            image_path,
            WIM_GENERIC_READ,
            WIM_OPEN_EXISTING,
            WIM_COMPRESS_NONE,
        )?;

        // 设置临时路径
        log::info!("[WIMGAPI] 设置临时路径: {:?}", temp_dir);
        self.wimgapi.set_temp_path(wim_handle, &temp_dir)?;
        log::info!("[WIMGAPI] 临时路径设置成功");

        // 注册进度回调
        log::info!("[WIMGAPI] 注册进度回调...");
        self.wimgapi.register_callback(wim_handle);
        log::info!("[WIMGAPI] 进度回调注册成功");

        // 启动进度监控线程
        let progress_tx_clone = progress_tx.clone();
        let monitor_running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let monitor_running_clone = monitor_running.clone();

        let monitor_thread = std::thread::spawn(move || {
            let mut last_progress: u8 = 0;
            while monitor_running_clone.load(Ordering::SeqCst) {
                let current = GLOBAL_PROGRESS.load(Ordering::SeqCst);
                if current != last_progress {
                    last_progress = current;
                    if let Some(ref tx) = progress_tx_clone {
                        let _ = tx.send(WimProgress {
                            percentage: current,
                            status: format!("释放镜像中 {}%", current),
                        });
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        });

        // 加载镜像
        log::info!("[WIMGAPI] 加载镜像索引 {}...", index);
        let image_handle = match self.wimgapi.load_image(wim_handle, index) {
            Ok(h) => {
                log::info!("[WIMGAPI] 镜像加载成功, handle={}", h);
                h
            }
            Err(e) => {
                log::error!("[WIMGAPI] 镜像加载失败: {}", e);
                monitor_running.store(false, Ordering::SeqCst);
                let _ = monitor_thread.join();
                self.wimgapi.unregister_callback(wim_handle);
                self.wimgapi.close(wim_handle)?;
                return Err(e);
            }
        };

        // 应用镜像
        log::info!("[WIMGAPI] 开始应用镜像到: {:?}", target_path);
        let apply_result = self.wimgapi.apply_image(image_handle, target_path, 0);
        log::info!("[WIMGAPI] 应用镜像完成, 结果: {:?}", apply_result.is_ok());

        // 停止进度监控
        monitor_running.store(false, Ordering::SeqCst);
        let _ = monitor_thread.join();

        // 清理
        log::info!("[WIMGAPI] 清理资源...");
        self.wimgapi.unregister_callback(wim_handle);
        self.wimgapi.close(image_handle)?;
        self.wimgapi.close(wim_handle)?;

        // 发送完成消息
        if apply_result.is_ok() {
            if let Some(tx) = progress_tx {
                let _ = tx.send(WimProgress {
                    percentage: 100,
                    status: "释放完成".to_string(),
                });
            }
            log::info!("[WIMGAPI] 镜像释放完成");
        }

        apply_result
    }

    /// 捕获/备份目录到 WIM 文件
    pub fn capture_image(
        &self,
        source_dir: &str,
        image_file: &str,
        name: &str,
        description: &str,
        compression: u32,
        progress_tx: Option<std::sync::mpsc::Sender<WimProgress>>,
    ) -> Result<(), WimApiError> {
        let source_path = Path::new(source_dir);
        let image_path = Path::new(image_file);
        
        // PE环境下使用可靠的临时目录
        // 优先级: X:\Windows\Temp -> 系统临时目录
        let temp_dir = {
            let pe_temp = PathBuf::from("X:\\Windows\\Temp");
            if pe_temp.exists() {
                pe_temp
            } else {
                std::env::temp_dir()
            }
        };

        log::info!("[WIMGAPI] 开始捕获镜像: {} -> {}", source_dir, image_file);
        log::info!("[WIMGAPI] 临时目录: {:?}", temp_dir);

        // 确定是创建新文件还是追加
        let disposition = if image_path.exists() {
            WIM_OPEN_EXISTING
        } else {
            WIM_CREATE_NEW
        };

        // 打开/创建 WIM 文件
        let wim_handle = self.wimgapi.open(
            image_path,
            WIM_GENERIC_WRITE | WIM_GENERIC_READ,
            disposition,
            compression,
        )?;

        // 设置临时路径
        self.wimgapi.set_temp_path(wim_handle, &temp_dir)?;

        // 注册进度回调
        log::info!("[WIMGAPI] 注册进度回调...");
        self.wimgapi.register_callback(wim_handle);

        // 启动进度监控线程
        let progress_tx_clone = progress_tx.clone();
        let monitor_running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let monitor_running_clone = monitor_running.clone();

        let monitor_thread = std::thread::spawn(move || {
            let mut last_progress: u8 = 0;
            while monitor_running_clone.load(Ordering::SeqCst) {
                let current = GLOBAL_PROGRESS.load(Ordering::SeqCst);
                if current != last_progress {
                    last_progress = current;
                    if let Some(ref tx) = progress_tx_clone {
                        let _ = tx.send(WimProgress {
                            percentage: current,
                            status: format!("捕获镜像中 {}%", current),
                        });
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        });

        // 捕获镜像
        log::info!("[WIMGAPI] 开始捕获...");
        let capture_result = self.wimgapi.capture_image(wim_handle, source_path, 0);

        let image_handle = match capture_result {
            Ok(h) => {
                log::info!("[WIMGAPI] 捕获成功, handle={}", h);
                h
            }
            Err(e) => {
                log::error!("[WIMGAPI] 捕获失败: {}", e);
                monitor_running.store(false, Ordering::SeqCst);
                let _ = monitor_thread.join();
                self.wimgapi.unregister_callback(wim_handle);
                self.wimgapi.close(wim_handle)?;
                return Err(e);
            }
        };

        // 设置镜像信息
        let xml_info = format!(
            r#"<IMAGE><NAME>{}</NAME><DESCRIPTION>{}</DESCRIPTION></IMAGE>"#,
            name, description
        );
        let _ = self.wimgapi.set_image_information(image_handle, &xml_info);

        // 停止进度监控
        monitor_running.store(false, Ordering::SeqCst);
        let _ = monitor_thread.join();

        // 清理
        log::info!("[WIMGAPI] 清理资源...");
        self.wimgapi.unregister_callback(wim_handle);
        self.wimgapi.close(image_handle)?;
        self.wimgapi.close(wim_handle)?;

        // 发送完成消息
        if let Some(tx) = progress_tx {
            let _ = tx.send(WimProgress {
                percentage: 100,
                status: "捕获完成".to_string(),
            });
        }

        log::info!("[WIMGAPI] 镜像捕获完成");
        Ok(())
    }

    /// 获取 WIM 文件中的镜像信息列表
    pub fn get_image_info(&self, image_file: &str) -> Result<Vec<ImageInfo>, WimApiError> {
        let image_path = Path::new(image_file);
        let temp_dir = std::env::temp_dir();

        let wim_handle = self.wimgapi.open(
            image_path,
            WIM_GENERIC_READ,
            WIM_OPEN_EXISTING,
            WIM_COMPRESS_NONE,
        )?;

        self.wimgapi.set_temp_path(wim_handle, &temp_dir)?;

        let xml = self.wimgapi.get_image_information(wim_handle)?;
        let images = Wimgapi::parse_image_info_from_xml(&xml);

        self.wimgapi.close(wim_handle)?;

        Ok(images)
    }

    /// 获取 WIM 文件属性
    #[allow(dead_code)]
    pub fn get_wim_info(&self, image_file: &str) -> Result<WimInfo, WimApiError> {
        let image_path = Path::new(image_file);

        let wim_handle = self.wimgapi.open(
            image_path,
            WIM_GENERIC_READ,
            WIM_OPEN_EXISTING,
            WIM_COMPRESS_NONE,
        )?;

        let info = self.wimgapi.get_attributes(wim_handle)?;
        self.wimgapi.close(wim_handle)?;

        Ok(info)
    }
    
    /// 分割WIM文件为SWM分卷
    /// 使用DISM命令行进行分割（wimgapi不直接支持分割）
    pub fn split_wim(&self, wim_file: &str, swm_file: &str, split_size_mb: u64) -> Result<(), WimApiError> {
        use crate::utils::command::new_command;
        use crate::utils::encoding::gbk_to_utf8;
        
        log::info!("[WIMGAPI] 分割WIM文件: {} -> {} (每卷 {}MB)", wim_file, swm_file, split_size_mb);
        
        // 使用DISM命令进行分割
        // dism /Split-Image /ImageFile:xxx.wim /SWMFile:xxx.swm /FileSize:size
        let output = new_command("dism.exe")
            .args([
                "/Split-Image",
                &format!("/ImageFile:{}", wim_file),
                &format!("/SWMFile:{}", swm_file),
                &format!("/FileSize:{}", split_size_mb),
            ])
            .output()
            .map_err(|e| WimApiError::Message(format!("执行DISM失败: {}", e)))?;
        
        let stdout = gbk_to_utf8(&output.stdout);
        let stderr = gbk_to_utf8(&output.stderr);
        
        log::info!("[WIMGAPI] DISM分割输出: {}", stdout);
        if !stderr.is_empty() {
            log::warn!("[WIMGAPI] DISM分割错误: {}", stderr);
        }
        
        if !output.status.success() {
            return Err(WimApiError::Message(format!(
                "DISM分割失败: {}",
                if stderr.is_empty() { &stdout } else { &stderr }
            )));
        }
        
        // 验证分卷文件是否创建
        if !Path::new(swm_file).exists() {
            return Err(WimApiError::Message("分卷文件未创建".to_string()));
        }
        
        log::info!("[WIMGAPI] WIM分割完成");
        Ok(())
    }
}

impl Default for WimManager {
    fn default() -> Self {
        Self::new().expect("Failed to create WimManager")
    }
}
