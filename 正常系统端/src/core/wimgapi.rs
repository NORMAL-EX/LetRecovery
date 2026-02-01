//! wimgapi.dll 动态库封装
//!
//! 该模块封装了Windows自带的wimgapi.dll库的主要功能，用于WIM/ESD镜像的处理。
//! 相比DISM命令行工具，直接调用API具有更好的性能和更精确的进度控制。
//!
//! 支持的WIM类型：
//! - 标准Windows安装镜像 (install.wim/install.esd)
//! - 整盘备份型WIM (包含完整Windows目录结构)
//! - 自定义WIM镜像
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
pub const WIM_GENERIC_MOUNT: u32 = 0x2000_0000;

// 创建/打开模式
pub const WIM_CREATE_NEW: u32 = 1;
pub const WIM_CREATE_ALWAYS: u32 = 2;
pub const WIM_OPEN_EXISTING: u32 = 3;
pub const WIM_OPEN_ALWAYS: u32 = 4;

// 压缩类型
pub const WIM_COMPRESS_NONE: u32 = 0;
pub const WIM_COMPRESS_XPRESS: u32 = 1;
pub const WIM_COMPRESS_LZX: u32 = 2;
pub const WIM_COMPRESS_LZMS: u32 = 3;

// 操作标志
pub const WIM_FLAG_RESERVED: u32 = 0x0000_0001;
pub const WIM_FLAG_VERIFY: u32 = 0x0000_0002;
pub const WIM_FLAG_INDEX: u32 = 0x0000_0004;
pub const WIM_FLAG_NO_APPLY: u32 = 0x0000_0008;
pub const WIM_FLAG_NO_DIRACL: u32 = 0x0000_0010;
pub const WIM_FLAG_NO_FILEACL: u32 = 0x0000_0020;
pub const WIM_FLAG_SHARE_WRITE: u32 = 0x0000_0040;
pub const WIM_FLAG_FILEINFO: u32 = 0x0000_0080;
pub const WIM_FLAG_NO_RP_FIX: u32 = 0x0000_0100;
pub const WIM_FLAG_MOUNT_READONLY: u32 = 0x0000_0200;

// 引用文件标志
pub const WIM_REFERENCE_APPEND: u32 = 0x0001_0000;
pub const WIM_REFERENCE_REPLACE: u32 = 0x0002_0000;

// 提交标志
pub const WIM_COMMIT_FLAG_APPEND: u32 = 0x0000_0001;

// 消息类型
// WIM_MSG = WM_APP + 0x1476 = 0x8000 + 0x1476 = 0x9476
// WIM_MSG_TEXT = WIM_MSG + 1 = 0x9477
// WIM_MSG_PROGRESS = WIM_MSG + 2 = 0x9478
// 详见: https://github.com/jeffkl/ManagedWimgApi/blob/main/wimgapi.h
pub const WIM_MSG_PROGRESS: u32 = 0x00009478;
pub const WIM_MSG_PROCESS: u32 = 0x00009479;
pub const WIM_MSG_SCANNING: u32 = 0x0000947A;
pub const WIM_MSG_SETRANGE: u32 = 0x0000947B;
pub const WIM_MSG_SETPOS: u32 = 0x0000947C;
pub const WIM_MSG_STEPIT: u32 = 0x0000947D;
pub const WIM_MSG_COMPRESS: u32 = 0x0000947E;
pub const WIM_MSG_ERROR: u32 = 0x0000947F;
pub const WIM_MSG_ALIGNMENT: u32 = 0x00009480;
pub const WIM_MSG_RETRY: u32 = 0x00009481;
pub const WIM_MSG_SPLIT: u32 = 0x00009482;
pub const WIM_MSG_FILEINFO: u32 = 0x00009483;
pub const WIM_MSG_INFO: u32 = 0x00009484;
pub const WIM_MSG_WARNING: u32 = 0x00009485;
pub const WIM_MSG_CHK_PROCESS: u32 = 0x00009486;
pub const WIM_MSG_SUCCESS: u32 = 0x00000000;
pub const WIM_MSG_ABORT_IMAGE: u32 = 0xFFFFFFFF;

// 消息回调返回值
pub const WIM_MSG_DONE_NO_ERROR: u32 = 0;
pub const WIM_MSG_DONE_ERROR: u32 = 0xFFFFFFFF;

// 路径最大长度
pub const MAX_PATH: usize = 260;

// ============================================================================
// 类型别名
// ============================================================================

type Pcwstr = *const u16;
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

type FnWimSetReferenceFile = unsafe extern "system" fn(hWim: Handle, pszPath: Pcwstr, dwFlags: u32) -> i32;

type FnWimRegisterMessageCallback = unsafe extern "system" fn(
    hWim: Handle,
    fpMessageProc: Option<extern "system" fn(u32, usize, isize, *mut c_void) -> u32>,
    pvUserData: *mut c_void,
) -> u32;

type FnWimUnregisterMessageCallback = unsafe extern "system" fn(
    hWim: Handle,
    fpMessageProc: Option<extern "system" fn(u32, usize, isize, *mut c_void) -> u32>,
) -> i32;

type FnWimCommitImageHandle = unsafe extern "system" fn(
    hImage: Handle,
    dwCommitFlags: u32,
    phNewImageHandle: *mut Handle,
) -> i32;

type FnWimDeleteImage = unsafe extern "system" fn(hWim: Handle, dwImageIndex: u32) -> i32;

type FnWimExportImage = unsafe extern "system" fn(hImage: Handle, hWim: Handle, dwFlags: u32) -> i32;

type FnWimSetBootImage = unsafe extern "system" fn(hWim: Handle, dwImageIndex: u32) -> i32;

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

type FnWimMountImage = unsafe extern "system" fn(
    pszMountPath: Pwstr,
    pszWimFileName: Pwstr,
    dwImageIndex: u32,
    pszTempPath: Pwstr,
) -> i32;

type FnWimUnmountImage = unsafe extern "system" fn(
    pszMountPath: Pwstr,
    pszWimFileName: Pwstr,
    dwImageIndex: u32,
    bCommitChanges: i32,
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

/// WIM 镜像类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WimImageType {
    /// 标准Windows安装镜像 (有完整元数据)
    StandardInstall,
    /// 整盘备份型WIM (直接包含Windows目录)
    FullBackup,
    /// PE环境镜像
    WindowsPE,
    /// 未知类型
    Unknown,
}

impl std::fmt::Display for WimImageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WimImageType::StandardInstall => write!(f, "标准安装镜像"),
            WimImageType::FullBackup => write!(f, "整盘备份镜像"),
            WimImageType::WindowsPE => write!(f, "PE环境镜像"),
            WimImageType::Unknown => write!(f, "未知类型"),
        }
    }
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
    /// Windows 主版本号 (如 10 表示 Win10/Win11)
    pub major_version: Option<u16>,
    /// Windows 次版本号 (如 Win7 为 1，对应版本 6.1)
    pub minor_version: Option<u16>,
    /// 镜像类型 (标准安装/整盘备份/PE等)
    pub image_type: WimImageType,
    /// 是否已验证可安装 (通过目录结构检测)
    pub verified_installable: bool,
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
        WIM_MSG_PROCESS => {
            // 文件处理消息，静默处理
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
    wim_set_reference_file: FnWimSetReferenceFile,
    wim_register_message_callback: FnWimRegisterMessageCallback,
    wim_unregister_message_callback: FnWimUnregisterMessageCallback,
    wim_commit_image_handle: FnWimCommitImageHandle,
    wim_delete_image: FnWimDeleteImage,
    wim_export_image: FnWimExportImage,
    wim_set_boot_image: FnWimSetBootImage,
    wim_set_image_information: FnWimSetImageInformation,
    wim_get_attributes: FnWimGetAttributes,
    wim_mount_image: FnWimMountImage,
    wim_unmount_image: FnWimUnmountImage,
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
    ///
    /// 加载顺序：
    /// 1. 程序目录下的 wimgapi.dll（自带新版本，兼容老系统）
    /// 2. 系统目录的 wimgapi.dll
    ///
    /// # 参数
    /// - `path`: 可选的 wimgapi.dll 路径，默认按上述顺序查找
    ///
    /// # 返回值
    /// - `Ok(Self)`: 成功加载
    /// - `Err(WimApiError)`: 加载失败
    pub fn new(path: Option<PathBuf>) -> Result<Self, WimApiError> {
        let lib_path = if let Some(p) = path {
            p
        } else {
            // 优先尝试加载程序目录下的 wimgapi.dll
            let exe_dir_dll = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("wimgapi.dll")));
            
            if let Some(ref local_dll) = exe_dir_dll {
                if local_dll.exists() {
                    println!("[WIMGAPI] 使用程序目录下的 wimgapi.dll: {:?}", local_dll);
                    local_dll.clone()
                } else {
                    println!("[WIMGAPI] 程序目录下未找到 wimgapi.dll，使用系统版本");
                    PathBuf::from("wimgapi.dll")
                }
            } else {
                PathBuf::from("wimgapi.dll")
            }
        };
        
        println!("[WIMGAPI] 加载 DLL: {:?}", lib_path);
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
                wim_set_reference_file: *lib.get(b"WIMSetReferenceFile")?,
                wim_register_message_callback: *lib.get(b"WIMRegisterMessageCallback")?,
                wim_unregister_message_callback: *lib.get(b"WIMUnregisterMessageCallback")?,
                wim_commit_image_handle: *lib.get(b"WIMCommitImageHandle")?,
                wim_delete_image: *lib.get(b"WIMDeleteImage")?,
                wim_export_image: *lib.get(b"WIMExportImage")?,
                wim_set_boot_image: *lib.get(b"WIMSetBootImage")?,
                wim_set_image_information: *lib.get(b"WIMSetImageInformation")?,
                wim_get_attributes: *lib.get(b"WIMGetAttributes")?,
                wim_mount_image: *lib.get(b"WIMMountImage")?,
                wim_unmount_image: *lib.get(b"WIMUnmountImage")?,
                _lib: lib,
            })
        }
    }

    /// 打开 WIM 文件
    ///
    /// # 参数
    /// - `path`: WIM 文件路径
    /// - `access`: 访问权限 (WIM_GENERIC_READ, WIM_GENERIC_WRITE 等)
    /// - `disposition`: 打开方式 (WIM_OPEN_EXISTING, WIM_CREATE_NEW 等)
    /// - `compression`: 压缩类型 (仅创建新文件时有效)
    ///
    /// # 返回值
    /// - `Ok(Handle)`: WIM 文件句柄
    /// - `Err(WimApiError)`: 打开失败
    pub fn open(
        &self,
        path: &Path,
        access: u32,
        disposition: u32,
        compression: u32,
    ) -> Result<Handle, WimApiError> {
        let wide_path = path_to_wide(path);
        let mut creation_result: u32 = 0;

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

        if handle == 0 {
            return Err(WimApiError::Win32Error(get_last_error()));
        }

        Ok(handle)
    }

    /// 关闭句柄
    ///
    /// # 参数
    /// - `handle`: 要关闭的句柄
    pub fn close(&self, handle: Handle) -> Result<(), WimApiError> {
        let result = unsafe { (self.wim_close_handle)(handle) };
        if result == 0 {
            return Err(WimApiError::Win32Error(get_last_error()));
        }
        Ok(())
    }

    /// 设置临时文件路径
    ///
    /// # 参数
    /// - `handle`: WIM 文件句柄
    /// - `path`: 临时目录路径
    pub fn set_temp_path(&self, handle: Handle, path: &Path) -> Result<(), WimApiError> {
        let wide_path = path_to_wide(path);
        let result = unsafe { (self.wim_set_temporary_path)(handle, wide_path.as_ptr()) };
        if result == 0 {
            return Err(WimApiError::Win32Error(get_last_error()));
        }
        Ok(())
    }

    /// 加载镜像
    ///
    /// # 参数
    /// - `handle`: WIM 文件句柄
    /// - `index`: 镜像索引 (从1开始)
    ///
    /// # 返回值
    /// - `Ok(Handle)`: 镜像句柄
    pub fn load_image(&self, handle: Handle, index: u32) -> Result<Handle, WimApiError> {
        let image_handle = unsafe { (self.wim_load_image)(handle, index) };
        if image_handle == 0 {
            return Err(WimApiError::Win32Error(get_last_error()));
        }
        Ok(image_handle)
    }

    /// 获取镜像数量
    ///
    /// # 参数
    /// - `handle`: WIM 文件句柄
    pub fn get_image_count(&self, handle: Handle) -> u32 {
        unsafe { (self.wim_get_image_count)(handle) }
    }

    /// 注册消息回调
    /// 返回注册结果，INVALID_CALLBACK_VALUE (0xFFFFFFFF) 表示失败
    ///
    /// # 参数
    /// - `handle`: WIM 文件句柄
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
    ///
    /// # 参数
    /// - `handle`: WIM 文件句柄
    pub fn unregister_callback(&self, handle: Handle) {
        unsafe {
            (self.wim_unregister_message_callback)(handle, Some(progress_callback));
        }
    }

    /// 获取当前进度
    pub fn get_progress(&self) -> u8 {
        GLOBAL_PROGRESS.load(Ordering::SeqCst)
    }

    /// 应用/释放镜像到指定目录
    ///
    /// # 参数
    /// - `image_handle`: 镜像句柄 (通过 load_image 获取)
    /// - `target_path`: 目标目录路径
    /// - `flags`: 操作标志
    ///
    /// # 返回值
    /// - `Ok(())`: 成功
    pub fn apply_image(
        &self,
        image_handle: Handle,
        target_path: &Path,
        flags: u32,
    ) -> Result<(), WimApiError> {
        let wide_path = path_to_wide(target_path);
        let result = unsafe { (self.wim_apply_image)(image_handle, wide_path.as_ptr(), flags) };
        if result == 0 {
            return Err(WimApiError::Win32Error(get_last_error()));
        }
        Ok(())
    }

    /// 捕获/备份目录到 WIM 文件
    ///
    /// # 参数
    /// - `handle`: WIM 文件句柄
    /// - `source_path`: 源目录路径
    /// - `flags`: 捕获标志
    ///
    /// # 返回值
    /// - `Ok(Handle)`: 新创建的镜像句柄
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
    ///
    /// # 参数
    /// - `handle`: WIM 或镜像句柄
    ///
    /// # 返回值
    /// - `Ok(String)`: XML 格式的镜像信息
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

    /// 设置引用文件 (用于 split WIM)
    ///
    /// # 参数
    /// - `handle`: WIM 文件句柄
    /// - `ref_path`: 引用文件路径
    /// - `flags`: 标志 (WIM_REFERENCE_APPEND 或 WIM_REFERENCE_REPLACE)
    pub fn set_reference_file(
        &self,
        handle: Handle,
        ref_path: &Path,
        flags: u32,
    ) -> Result<(), WimApiError> {
        let wide_path = path_to_wide(ref_path);
        let result = unsafe { (self.wim_set_reference_file)(handle, wide_path.as_ptr(), flags) };
        if result == 0 {
            return Err(WimApiError::Win32Error(get_last_error()));
        }
        Ok(())
    }

    /// 提交镜像更改
    ///
    /// # 参数
    /// - `image_handle`: 镜像句柄
    /// - `flags`: 提交标志
    pub fn commit_image(&self, image_handle: Handle, flags: u32) -> Result<(), WimApiError> {
        let mut new_handle: Handle = 0;
        let result = unsafe {
            (self.wim_commit_image_handle)(image_handle, flags, &mut new_handle)
        };
        if result == 0 {
            return Err(WimApiError::Win32Error(get_last_error()));
        }
        Ok(())
    }

    /// 删除镜像
    ///
    /// # 参数
    /// - `handle`: WIM 文件句柄
    /// - `index`: 要删除的镜像索引
    pub fn delete_image(&self, handle: Handle, index: u32) -> Result<(), WimApiError> {
        let result = unsafe { (self.wim_delete_image)(handle, index) };
        if result == 0 {
            return Err(WimApiError::Win32Error(get_last_error()));
        }
        Ok(())
    }

    /// 导出镜像
    ///
    /// # 参数
    /// - `src_image_handle`: 源镜像句柄
    /// - `dst_wim_handle`: 目标 WIM 文件句柄
    /// - `flags`: 导出标志
    pub fn export_image(
        &self,
        src_image_handle: Handle,
        dst_wim_handle: Handle,
        flags: u32,
    ) -> Result<(), WimApiError> {
        let result = unsafe { (self.wim_export_image)(src_image_handle, dst_wim_handle, flags) };
        if result == 0 {
            return Err(WimApiError::Win32Error(get_last_error()));
        }
        Ok(())
    }

    /// 设置引导镜像
    ///
    /// # 参数
    /// - `handle`: WIM 文件句柄
    /// - `index`: 引导镜像索引 (0 表示取消引导镜像)
    pub fn set_boot_image(&self, handle: Handle, index: u32) -> Result<(), WimApiError> {
        let result = unsafe { (self.wim_set_boot_image)(handle, index) };
        if result == 0 {
            return Err(WimApiError::Win32Error(get_last_error()));
        }
        Ok(())
    }

    /// 设置镜像信息
    ///
    /// # 参数
    /// - `handle`: 镜像句柄
    /// - `xml_info`: XML 格式的镜像信息
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
    ///
    /// # 参数
    /// - `handle`: WIM 文件句柄
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

    /// 挂载镜像
    ///
    /// # 参数
    /// - `mount_path`: 挂载目录
    /// - `wim_path`: WIM 文件路径
    /// - `index`: 镜像索引
    /// - `temp_path`: 临时目录 (None 表示只读挂载)
    pub fn mount_image(
        &self,
        mount_path: &Path,
        wim_path: &Path,
        index: u32,
        temp_path: Option<&Path>,
    ) -> Result<(), WimApiError> {
        let mut wide_mount = path_to_wide(mount_path);
        let mut wide_wim = path_to_wide(wim_path);
        let wide_temp = temp_path.map(path_to_wide);

        let temp_ptr = match wide_temp {
            Some(mut t) => t.as_mut_ptr(),
            None => null_mut(),
        };

        let result = unsafe {
            (self.wim_mount_image)(
                wide_mount.as_mut_ptr(),
                wide_wim.as_mut_ptr(),
                index,
                temp_ptr,
            )
        };

        if result == 0 {
            return Err(WimApiError::Win32Error(get_last_error()));
        }
        Ok(())
    }

    /// 卸载镜像
    ///
    /// # 参数
    /// - `mount_path`: 挂载目录
    /// - `wim_path`: WIM 文件路径
    /// - `index`: 镜像索引
    /// - `commit`: 是否提交更改
    pub fn unmount_image(
        &self,
        mount_path: &Path,
        wim_path: &Path,
        index: u32,
        commit: bool,
    ) -> Result<(), WimApiError> {
        let mut wide_mount = path_to_wide(mount_path);
        let mut wide_wim = path_to_wide(wim_path);

        let result = unsafe {
            (self.wim_unmount_image)(
                wide_mount.as_mut_ptr(),
                wide_wim.as_mut_ptr(),
                index,
                if commit { 1 } else { 0 },
            )
        };

        if result == 0 {
            return Err(WimApiError::Win32Error(get_last_error()));
        }
        Ok(())
    }

    /// 解析镜像 XML 获取镜像信息列表
    ///
    /// 支持多种WIM格式：
    /// - 标准Windows安装镜像 (带完整元数据)
    /// - 整盘备份型WIM (可能元数据不完整)
    /// - 各种非标准格式
    ///
    /// # 参数
    /// - `xml`: XML 字符串
    pub fn parse_image_info_from_xml(xml: &str) -> Vec<ImageInfo> {
        let mut images = Vec::new();
        let mut pos = 0;

        // 首先尝试标准格式解析 (INDEX="...")
        while let Some(start) = xml[pos..].find("<IMAGE INDEX=\"") {
            let abs_start = pos + start;
            let index_start = abs_start + 14;

            if let Some(index_end) = xml[index_start..].find('"') {
                let index_str = &xml[index_start..index_start + index_end];
                let index: u32 = index_str.parse().unwrap_or(0);

                if let Some(image_end) = xml[abs_start..].find("</IMAGE>") {
                    let image_block = &xml[abs_start..abs_start + image_end + 8];
                    
                    if let Some(info) = Self::parse_single_image_block(image_block, index) {
                        images.push(info);
                    }

                    pos = abs_start + image_end + 8;
                } else {
                    pos = abs_start + 14;
                }
            } else {
                pos = abs_start + 14;
            }
        }
        
        // 如果标准格式解析失败，尝试备用解析策略
        if images.is_empty() {
            images = Self::parse_image_info_fallback(xml);
        }

        // 对解析结果进行后处理，确定镜像类型
        for img in &mut images {
            img.image_type = Self::determine_image_type(img);
        }

        images
    }

    /// 解析单个IMAGE块
    fn parse_single_image_block(image_block: &str, index: u32) -> Option<ImageInfo> {
        if index == 0 {
            return None;
        }

        let size_bytes = Self::extract_xml_tag(image_block, "TOTALBYTES")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let installation_type = Self::extract_xml_tag(image_block, "INSTALLATIONTYPE")
            .unwrap_or_default();

        let description = Self::extract_xml_tag(image_block, "DESCRIPTION")
            .unwrap_or_default();

        // 提取版本信息 - 多种格式支持
        let major_version = Self::extract_version_number(image_block, "MAJOR");
        let minor_version = Self::extract_version_number(image_block, "MINOR");

        // 智能构建镜像名称
        let name = Self::build_image_name(image_block, &description, index);

        Some(ImageInfo {
            index,
            name,
            size_bytes,
            installation_type,
            description,
            major_version,
            minor_version,
            image_type: WimImageType::Unknown, // 后续会更新
            verified_installable: false,       // 后续会验证
        })
    }

    /// 智能构建镜像名称
    /// 
    /// 按优先级尝试以下来源：
    /// 1. DISPLAYNAME 标签
    /// 2. NAME 标签
    /// 3. WINDOWS 块中的 PRODUCTNAME + EDITIONID 组合
    /// 4. DESCRIPTION + FLAGS 组合
    /// 5. 仅 DESCRIPTION
    /// 6. 仅 FLAGS
    /// 7. 默认 "镜像 {index}"
    fn build_image_name(image_block: &str, description: &str, index: u32) -> String {
        // 1. 优先使用 DISPLAYNAME
        if let Some(display_name) = Self::extract_xml_tag(image_block, "DISPLAYNAME") {
            if !display_name.is_empty() {
                return display_name;
            }
        }

        // 2. 其次使用 NAME
        if let Some(name) = Self::extract_xml_tag(image_block, "NAME") {
            if !name.is_empty() {
                return name;
            }
        }

        // 3. 尝试从 WINDOWS 块中组合 PRODUCTNAME + EDITIONID
        if let Some(windows_block) = Self::extract_xml_tag(image_block, "WINDOWS") {
            let product_name = Self::extract_xml_tag(&windows_block, "PRODUCTNAME");
            let edition_id = Self::extract_xml_tag(&windows_block, "EDITIONID");

            match (product_name, edition_id) {
                (Some(prod), Some(ed)) if !prod.is_empty() && !ed.is_empty() => {
                    // 避免重复：如果 PRODUCTNAME 已经包含 EDITIONID，直接返回
                    if prod.to_lowercase().contains(&ed.to_lowercase()) {
                        return prod;
                    }
                    return format!("{} {}", prod, ed);
                }
                (Some(prod), _) if !prod.is_empty() => return prod,
                (_, Some(ed)) if !ed.is_empty() => return format!("Windows {}", ed),
                _ => {}
            }
        }

        // 4. 尝试使用 DESCRIPTION + FLAGS 组合
        let flags = Self::extract_xml_tag(image_block, "FLAGS").unwrap_or_default();
        
        if !description.is_empty() && !flags.is_empty() {
            // 避免重复：如果 DESCRIPTION 已经包含 FLAGS 内容
            if description.to_lowercase().contains(&flags.to_lowercase()) {
                return description.to_string();
            }
            return format!("{} {}", description, flags);
        }

        // 5. 仅使用 DESCRIPTION
        if !description.is_empty() {
            return description.to_string();
        }

        // 6. 仅使用 FLAGS
        if !flags.is_empty() {
            return format!("Windows {}", flags);
        }

        // 7. 最后使用默认名称
        format!("镜像 {}", index)
    }

    /// 提取版本号 (支持多种XML结构)
    fn extract_version_number(image_block: &str, tag: &str) -> Option<u16> {
        // 先尝试从 VERSION 块中获取
        Self::extract_xml_tag(image_block, "VERSION")
            .and_then(|version_block| Self::extract_xml_tag(&version_block, tag))
            .or_else(|| {
                // 然后尝试从 WINDOWS/VERSION 路径获取
                Self::extract_xml_tag(image_block, "WINDOWS")
                    .and_then(|win_block| Self::extract_xml_tag(&win_block, "VERSION"))
                    .and_then(|ver_block| Self::extract_xml_tag(&ver_block, tag))
            })
            .or_else(|| {
                // 最后直接从 IMAGE 块获取
                Self::extract_xml_tag(image_block, tag)
            })
            .and_then(|s| s.parse::<u16>().ok())
    }

    /// 备用解析策略 - 处理非标准格式的WIM
    fn parse_image_info_fallback(xml: &str) -> Vec<ImageInfo> {
        let mut images = Vec::new();
        
        // 检查是否有 IMAGE 标签但格式不同
        let image_count = xml.matches("<IMAGE ").count();
        if image_count == 0 {
            return images;
        }

        println!("[WIMGAPI] XML中发现 {} 个 IMAGE 标签，使用备用解析策略", image_count);
        
        let mut backup_pos = 0;
        let mut backup_index = 1u32;
        
        while let Some(img_start) = xml[backup_pos..].find("<IMAGE ") {
            let abs_img_start = backup_pos + img_start;
            
            // 查找IMAGE块的结束位置
            let block_end = xml[abs_img_start..].find("</IMAGE>")
                .map(|e| abs_img_start + e + 8)
                .or_else(|| {
                    // 查找下一个 <IMAGE 或 </WIM>
                    xml[abs_img_start + 7..].find("<IMAGE ")
                        .map(|e| abs_img_start + 7 + e)
                        .or_else(|| xml[abs_img_start..].find("</WIM>").map(|e| abs_img_start + e))
                })
                .unwrap_or(xml.len());
            
            let image_block = &xml[abs_img_start..block_end];
            
            // 尝试从属性中提取索引
            let parsed_index = Self::extract_index_from_attributes(image_block)
                .unwrap_or(backup_index);
            
            let size_bytes = Self::extract_xml_tag(image_block, "TOTALBYTES")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            
            let installation_type = Self::extract_xml_tag(image_block, "INSTALLATIONTYPE")
                .unwrap_or_default();
            
            let description = Self::extract_xml_tag(image_block, "DESCRIPTION")
                .unwrap_or_default();
            
            let major_version = Self::extract_version_number(image_block, "MAJOR");
            let minor_version = Self::extract_version_number(image_block, "MINOR");
            
            // 使用智能名称构建
            let name = Self::build_image_name(image_block, &description, parsed_index);
            
            images.push(ImageInfo {
                index: parsed_index,
                name,
                size_bytes,
                installation_type,
                description,
                major_version,
                minor_version,
                image_type: WimImageType::Unknown,
                verified_installable: false,
            });
            
            backup_index += 1;
            backup_pos = block_end;
        }

        images
    }

    /// 从IMAGE标签的属性中提取索引
    fn extract_index_from_attributes(image_block: &str) -> Option<u32> {
        // 尝试 INDEX="N" 格式
        if let Some(idx_pos) = image_block.find("INDEX=\"") {
            let idx_start = idx_pos + 7;
            if let Some(idx_end) = image_block[idx_start..].find('"') {
                return image_block[idx_start..idx_start + idx_end].parse().ok();
            }
        }
        
        // 尝试 INDEX='N' 格式 (单引号)
        if let Some(idx_pos) = image_block.find("INDEX='") {
            let idx_start = idx_pos + 7;
            if let Some(idx_end) = image_block[idx_start..].find('\'') {
                return image_block[idx_start..idx_start + idx_end].parse().ok();
            }
        }
        
        // 尝试 INDEX=N 格式 (无引号)
        if let Some(idx_pos) = image_block.find("INDEX=") {
            let idx_start = idx_pos + 6;
            let idx_str: String = image_block[idx_start..].chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if !idx_str.is_empty() {
                return idx_str.parse().ok();
            }
        }
        
        None
    }

    /// 根据镜像信息确定镜像类型
    fn determine_image_type(info: &ImageInfo) -> WimImageType {
        let name_lower = info.name.to_lowercase();
        let install_type_lower = info.installation_type.to_lowercase();
        
        // 检测 PE 环境
        if install_type_lower == "windowspe" 
            || name_lower.contains("windows pe")
            || name_lower.contains("winpe")
            || name_lower.contains("windows setup") {
            return WimImageType::WindowsPE;
        }
        
        // 检测标准安装镜像 (有完整的安装类型和版本信息)
        if !info.installation_type.is_empty() 
            && info.major_version.is_some() 
            && (install_type_lower == "client" || install_type_lower == "server") {
            return WimImageType::StandardInstall;
        }
        
        // 检测整盘备份型 (通常缺少installation_type或名称特殊)
        if info.installation_type.is_empty() && info.size_bytes > 1_000_000_000 {
            // 大于1GB且缺少安装类型，很可能是整盘备份
            return WimImageType::FullBackup;
        }
        
        // 名称暗示是备份
        if name_lower.contains("backup") 
            || name_lower.contains("备份")
            || name_lower.contains("ghost")
            || name_lower.contains("clone") {
            return WimImageType::FullBackup;
        }
        
        // 有版本信息但缺少安装类型，可能是非标准安装镜像或备份
        if info.major_version.is_some() && info.installation_type.is_empty() {
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
    ///
    /// # 参数
    /// - `image_file`: WIM/ESD 文件路径
    /// - `target_dir`: 目标目录
    /// - `index`: 镜像索引 (从1开始)
    /// - `progress_tx`: 进度发送器 (可选)
    ///
    /// # 返回值
    /// - `Ok(())`: 成功
    pub fn apply_image(
        &self,
        image_file: &str,
        target_dir: &str,
        index: u32,
        progress_tx: Option<std::sync::mpsc::Sender<WimProgress>>,
    ) -> Result<(), WimApiError> {
        let image_path = Path::new(image_file);
        let target_path = Path::new(target_dir);
        let temp_dir = std::env::temp_dir();

        println!("[WIMGAPI] 开始释放镜像: {} -> {}", image_file, target_dir);
        println!("[WIMGAPI] 镜像索引: {}", index);

        // 打开 WIM 文件
        let wim_handle = self.wimgapi.open(
            image_path,
            WIM_GENERIC_READ,
            WIM_OPEN_EXISTING,
            WIM_COMPRESS_NONE,
        )?;

        // 设置临时路径
        self.wimgapi.set_temp_path(wim_handle, &temp_dir)?;

        // 注册进度回调
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
                            status: format!("释放镜像中 {}%", current),
                        });
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        });

        // 加载镜像
        let image_handle = match self.wimgapi.load_image(wim_handle, index) {
            Ok(h) => h,
            Err(e) => {
                monitor_running.store(false, Ordering::SeqCst);
                let _ = monitor_thread.join();
                self.wimgapi.unregister_callback(wim_handle);
                self.wimgapi.close(wim_handle)?;
                return Err(e);
            }
        };

        // 应用镜像
        let apply_result = self.wimgapi.apply_image(image_handle, target_path, 0);

        // 停止进度监控
        monitor_running.store(false, Ordering::SeqCst);
        let _ = monitor_thread.join();

        // 清理
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
            println!("[WIMGAPI] 镜像释放完成");
        }

        apply_result
    }

    /// 捕获/备份目录到 WIM 文件
    ///
    /// # 参数
    /// - `source_dir`: 源目录
    /// - `image_file`: 目标 WIM 文件路径
    /// - `name`: 镜像名称
    /// - `description`: 镜像描述
    /// - `compression`: 压缩类型
    /// - `progress_tx`: 进度发送器 (可选)
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
        let temp_dir = std::env::temp_dir();

        println!("[WIMGAPI] 开始捕获镜像: {} -> {}", source_dir, image_file);

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
        let capture_result = self.wimgapi.capture_image(wim_handle, source_path, 0);

        let image_handle = match capture_result {
            Ok(h) => h,
            Err(e) => {
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

        println!("[WIMGAPI] 镜像捕获完成");
        Ok(())
    }

    /// 获取 WIM 文件中的镜像信息列表
    ///
    /// 支持多种WIM格式：
    /// - 标准Windows安装镜像
    /// - 整盘备份型WIM
    /// - 自定义WIM镜像
    ///
    /// # 参数
    /// - `image_file`: WIM/ESD 文件路径
    pub fn get_image_info(&self, image_file: &str) -> Result<Vec<ImageInfo>, WimApiError> {
        let image_path = Path::new(image_file);
        
        // 尝试多个临时目录，PE环境中 %TEMP% 可能无效
        let temp_candidates = [
            std::env::temp_dir(),
            PathBuf::from("X:\\Windows\\Temp"),
            PathBuf::from("C:\\Windows\\Temp"),
            PathBuf::from("X:\\Temp"),
            PathBuf::from("C:\\Temp"),
        ];
        
        let temp_dir = temp_candidates.iter()
            .find(|p| p.exists() || std::fs::create_dir_all(p).is_ok())
            .cloned()
            .unwrap_or_else(std::env::temp_dir);
        
        println!("[WIMGAPI] 使用临时目录: {:?}", temp_dir);
        println!("[WIMGAPI] 打开镜像文件: {}", image_file);

        let wim_handle = self.wimgapi.open(
            image_path,
            WIM_GENERIC_READ,
            WIM_OPEN_EXISTING,
            WIM_COMPRESS_NONE,
        ).map_err(|e| {
            println!("[WIMGAPI] 打开WIM文件失败: {}", e);
            e
        })?;

        if let Err(e) = self.wimgapi.set_temp_path(wim_handle, &temp_dir) {
            println!("[WIMGAPI] 设置临时目录失败: {} (继续尝试)", e);
            // 不直接返回错误，某些情况下即使设置失败也能继续
        }
        
        // 先获取镜像数量作为后备
        let image_count = self.wimgapi.get_image_count(wim_handle);
        println!("[WIMGAPI] WIM文件包含 {} 个镜像", image_count);

        let xml = self.wimgapi.get_image_information(wim_handle).map_err(|e| {
            println!("[WIMGAPI] 获取镜像信息失败: {}", e);
            let _ = self.wimgapi.close(wim_handle);
            e
        })?;
        
        println!("[WIMGAPI] 成功获取XML元数据，长度: {} 字符", xml.len());
        
        // 打印XML内容用于调试（仅前2000字符）
        if xml.len() > 0 {
            let preview_len = std::cmp::min(2000, xml.len());
            println!("[WIMGAPI] XML预览:\n{}", &xml[..preview_len]);
        }
        
        let mut images = Wimgapi::parse_image_info_from_xml(&xml);
        println!("[WIMGAPI] 解析出 {} 个镜像", images.len());
        
        // 如果 XML 解析失败但 WIM 确实有镜像，使用 image_count 创建基本信息
        if images.is_empty() && image_count > 0 {
            println!("[WIMGAPI] XML解析失败，使用镜像数量创建基本信息 (可能是整盘备份型WIM)");
            for i in 1..=image_count {
                images.push(ImageInfo {
                    index: i,
                    name: format!("系统镜像 {}", i),
                    size_bytes: 0,
                    installation_type: String::new(),
                    description: String::new(),
                    major_version: None,
                    minor_version: None,
                    image_type: WimImageType::FullBackup, // 默认标记为整盘备份
                    verified_installable: false,
                });
            }
        }

        self.wimgapi.close(wim_handle)?;

        // 输出每个镜像的详细信息
        for img in &images {
            println!("[WIMGAPI] 镜像 {}: {} (类型: {}, 安装类型: {}, 版本: {:?}.{:?})", 
                img.index, img.name, img.image_type, 
                if img.installation_type.is_empty() { "<无>" } else { &img.installation_type },
                img.major_version, img.minor_version);
        }

        Ok(images)
    }

    /// 验证WIM镜像是否包含有效的Windows系统
    /// 
    /// 通过挂载镜像并检查目录结构来判断
    /// 
    /// # 参数
    /// - `image_file`: WIM/ESD 文件路径
    /// - `index`: 镜像索引
    /// 
    /// # 返回
    /// - `Ok(true)`: 包含有效的Windows系统
    /// - `Ok(false)`: 不是有效的Windows系统镜像
    /// - `Err`: 验证过程出错
    pub fn verify_windows_image(&self, image_file: &str, index: u32) -> Result<bool, WimApiError> {
        let image_path = Path::new(image_file);
        
        // 创建临时挂载目录
        let mount_dir = std::env::temp_dir()
            .join(format!("WimVerify_{}_{}", std::process::id(), index));
        
        if mount_dir.exists() {
            let _ = std::fs::remove_dir_all(&mount_dir);
        }
        
        if std::fs::create_dir_all(&mount_dir).is_err() {
            return Err(WimApiError::Message("创建临时挂载目录失败".to_string()));
        }

        println!("[WIMGAPI] 开始验证镜像: {} 索引 {}", image_file, index);
        println!("[WIMGAPI] 挂载目录: {:?}", mount_dir);

        // 挂载镜像
        let mount_result = self.wimgapi.mount_image(&mount_dir, image_path, index, None);
        
        if let Err(e) = mount_result {
            let _ = std::fs::remove_dir_all(&mount_dir);
            println!("[WIMGAPI] 挂载失败: {}", e);
            return Err(e);
        }

        // 检查目录结构
        let is_valid = Self::check_windows_directory_structure(&mount_dir);
        
        // 卸载镜像
        let _ = self.wimgapi.unmount_image(&mount_dir, image_path, index, false);
        let _ = std::fs::remove_dir_all(&mount_dir);

        println!("[WIMGAPI] 镜像验证结果: {}", if is_valid { "有效" } else { "无效" });
        Ok(is_valid)
    }

    /// 检查目录结构是否包含有效的Windows系统
    fn check_windows_directory_structure(mount_dir: &Path) -> bool {
        // Windows系统必须包含的核心目录和文件
        let required_dirs = [
            "Windows",
            "Windows\\System32",
        ];
        
        let required_files = [
            "Windows\\System32\\ntdll.dll",
        ];
        
        // 可选但有助于确认的目录
        let optional_dirs = [
            "Program Files",
            "Users",
            "ProgramData",
        ];

        // 检查必需目录
        for dir in &required_dirs {
            let path = mount_dir.join(dir);
            if !path.exists() || !path.is_dir() {
                println!("[WIMGAPI] 缺少必需目录: {}", dir);
                return false;
            }
        }

        // 检查必需文件
        for file in &required_files {
            let path = mount_dir.join(file);
            if !path.exists() || !path.is_file() {
                println!("[WIMGAPI] 缺少必需文件: {}", file);
                return false;
            }
        }

        // 统计可选目录存在数量
        let optional_count: usize = optional_dirs.iter()
            .filter(|dir| mount_dir.join(dir).exists())
            .count();
        
        println!("[WIMGAPI] 可选目录存在: {}/{}", optional_count, optional_dirs.len());

        true
    }

    /// 快速检测WIM文件类型（不挂载，仅通过XML分析）
    /// 
    /// # 参数
    /// - `image_file`: WIM/ESD 文件路径
    /// 
    /// # 返回
    /// 镜像类型列表，与镜像索引对应
    pub fn detect_wim_type(&self, image_file: &str) -> Result<Vec<WimImageType>, WimApiError> {
        let images = self.get_image_info(image_file)?;
        Ok(images.into_iter().map(|img| img.image_type).collect())
    }

    /// 获取 WIM 文件属性
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
}

impl Default for WimManager {
    fn default() -> Self {
        Self::new().expect("Failed to create WimManager")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_xml_parsing_standard() {
        let xml = r#"
        <WIM>
            <IMAGE INDEX="1">
                <NAME>Windows 11 Pro</NAME>
                <DESCRIPTION>Windows 11 Professional</DESCRIPTION>
                <TOTALBYTES>15000000000</TOTALBYTES>
                <INSTALLATIONTYPE>Client</INSTALLATIONTYPE>
                <VERSION>
                    <MAJOR>10</MAJOR>
                    <MINOR>0</MINOR>
                </VERSION>
            </IMAGE>
            <IMAGE INDEX="2">
                <NAME>Windows 11 Home</NAME>
                <DESCRIPTION>Windows 11 Home Edition</DESCRIPTION>
                <TOTALBYTES>14000000000</TOTALBYTES>
                <INSTALLATIONTYPE>Client</INSTALLATIONTYPE>
                <VERSION>
                    <MAJOR>10</MAJOR>
                    <MINOR>0</MINOR>
                </VERSION>
            </IMAGE>
        </WIM>
        "#;

        let images = Wimgapi::parse_image_info_from_xml(xml);
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].index, 1);
        assert_eq!(images[0].name, "Windows 11 Pro");
        assert_eq!(images[0].image_type, WimImageType::StandardInstall);
        assert_eq!(images[1].index, 2);
        assert_eq!(images[1].name, "Windows 11 Home");
    }

    #[test]
    fn test_xml_parsing_backup_wim() {
        // 整盘备份型WIM通常缺少INSTALLATIONTYPE
        let xml = r#"
        <WIM>
            <IMAGE INDEX="1">
                <NAME>System Backup 2024-01-15</NAME>
                <TOTALBYTES>50000000000</TOTALBYTES>
            </IMAGE>
        </WIM>
        "#;

        let images = Wimgapi::parse_image_info_from_xml(xml);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].index, 1);
        assert_eq!(images[0].name, "System Backup 2024-01-15");
        assert_eq!(images[0].image_type, WimImageType::FullBackup);
    }

    #[test]
    fn test_xml_parsing_pe_image() {
        let xml = r#"
        <WIM>
            <IMAGE INDEX="1">
                <NAME>Windows PE</NAME>
                <INSTALLATIONTYPE>WindowsPE</INSTALLATIONTYPE>
                <TOTALBYTES>500000000</TOTALBYTES>
            </IMAGE>
        </WIM>
        "#;

        let images = Wimgapi::parse_image_info_from_xml(xml);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].image_type, WimImageType::WindowsPE);
    }

    #[test]
    fn test_xml_parsing_empty_name_with_windows_block() {
        // 测试场景：NAME为空但有WINDOWS块包含PRODUCTNAME和EDITIONID
        let xml = r#"
        <WIM>
            <IMAGE INDEX="1">
                <NAME></NAME>
                <DESCRIPTION>Windows 7</DESCRIPTION>
                <FLAGS>Ultimate</FLAGS>
                <WINDOWS>
                    <PRODUCTNAME>Windows 7</PRODUCTNAME>
                    <EDITIONID>Ultimate</EDITIONID>
                </WINDOWS>
                <TOTALBYTES>19679656523</TOTALBYTES>
                <VERSION>
                    <MAJOR>6</MAJOR>
                    <MINOR>1</MINOR>
                </VERSION>
            </IMAGE>
        </WIM>
        "#;

        let images = Wimgapi::parse_image_info_from_xml(xml);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].index, 1);
        // 应该从 WINDOWS 块中提取 PRODUCTNAME + EDITIONID
        assert_eq!(images[0].name, "Windows 7 Ultimate");
        assert_eq!(images[0].description, "Windows 7");
    }

    #[test]
    fn test_xml_parsing_empty_name_with_description_and_flags() {
        // 测试场景：NAME为空，无WINDOWS块，但有DESCRIPTION和FLAGS
        let xml = r#"
        <WIM>
            <IMAGE INDEX="1">
                <NAME></NAME>
                <DESCRIPTION>Windows 10</DESCRIPTION>
                <FLAGS>Professional</FLAGS>
                <TOTALBYTES>15000000000</TOTALBYTES>
            </IMAGE>
        </WIM>
        "#;

        let images = Wimgapi::parse_image_info_from_xml(xml);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].index, 1);
        // 应该组合 DESCRIPTION + FLAGS
        assert_eq!(images[0].name, "Windows 10 Professional");
    }

    #[test]
    fn test_xml_parsing_empty_name_description_only() {
        // 测试场景：NAME为空，只有DESCRIPTION
        let xml = r#"
        <WIM>
            <IMAGE INDEX="1">
                <NAME></NAME>
                <DESCRIPTION>Custom Windows Image</DESCRIPTION>
                <TOTALBYTES>20000000000</TOTALBYTES>
            </IMAGE>
        </WIM>
        "#;

        let images = Wimgapi::parse_image_info_from_xml(xml);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].index, 1);
        assert_eq!(images[0].name, "Custom Windows Image");
    }

    #[test]
    fn test_xml_parsing_empty_name_flags_only() {
        // 测试场景：NAME为空，只有FLAGS
        let xml = r#"
        <WIM>
            <IMAGE INDEX="1">
                <NAME></NAME>
                <FLAGS>Enterprise</FLAGS>
                <TOTALBYTES>18000000000</TOTALBYTES>
            </IMAGE>
        </WIM>
        "#;

        let images = Wimgapi::parse_image_info_from_xml(xml);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].index, 1);
        assert_eq!(images[0].name, "Windows Enterprise");
    }

    #[test]
    fn test_xml_parsing_no_name_fallback() {
        // 测试场景：没有任何名称相关字段
        let xml = r#"
        <WIM>
            <IMAGE INDEX="1">
                <TOTALBYTES>10000000000</TOTALBYTES>
            </IMAGE>
        </WIM>
        "#;

        let images = Wimgapi::parse_image_info_from_xml(xml);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].index, 1);
        assert_eq!(images[0].name, "镜像 1");
    }

    #[test]
    fn test_xml_parsing_productname_contains_edition() {
        // 测试场景：PRODUCTNAME已经包含版本信息，避免重复
        let xml = r#"
        <WIM>
            <IMAGE INDEX="1">
                <NAME></NAME>
                <WINDOWS>
                    <PRODUCTNAME>Windows 11 Pro</PRODUCTNAME>
                    <EDITIONID>Pro</EDITIONID>
                </WINDOWS>
                <TOTALBYTES>15000000000</TOTALBYTES>
            </IMAGE>
        </WIM>
        "#;

        let images = Wimgapi::parse_image_info_from_xml(xml);
        assert_eq!(images.len(), 1);
        // 应该避免重复，只返回 PRODUCTNAME
        assert_eq!(images[0].name, "Windows 11 Pro");
    }

    #[test]
    fn test_determine_image_type() {
        let info = ImageInfo {
            index: 1,
            name: "Windows 11 Pro".to_string(),
            size_bytes: 15_000_000_000,
            installation_type: "Client".to_string(),
            description: String::new(),
            major_version: Some(10),
            minor_version: Some(0),
            image_type: WimImageType::Unknown,
            verified_installable: false,
        };
        let detected_type = Wimgapi::determine_image_type(&info);
        assert_eq!(detected_type, WimImageType::StandardInstall);

        let pe_info = ImageInfo {
            index: 1,
            name: "Windows PE".to_string(),
            size_bytes: 500_000_000,
            installation_type: "WindowsPE".to_string(),
            description: String::new(),
            major_version: Some(10),
            minor_version: Some(0),
            image_type: WimImageType::Unknown,
            verified_installable: false,
        };
        let pe_detected_type = Wimgapi::determine_image_type(&pe_info);
        assert_eq!(pe_detected_type, WimImageType::WindowsPE);

        let backup_info = ImageInfo {
            index: 1,
            name: "My Backup".to_string(),
            size_bytes: 50_000_000_000,
            installation_type: String::new(),
            description: String::new(),
            major_version: None,
            minor_version: None,
            image_type: WimImageType::Unknown,
            verified_installable: false,
        };
        let backup_detected_type = Wimgapi::determine_image_type(&backup_info);
        assert_eq!(backup_detected_type, WimImageType::FullBackup);
    }
}
