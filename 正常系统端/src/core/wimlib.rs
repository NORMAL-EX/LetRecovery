//! wimlib.dll 动态库封装
//!
//! 该模块封装了 wimlib.dll 的主要功能，用于 WIM/ESD 镜像的完整性校验。
//! wimlib 是一个开源的 WIM 处理库，提供了比微软官方 API 更快、更可靠的校验功能。
//!
//! # 特性
//! - 自动检测并加载 DLL（支持多种命名约定）
//! - 跨平台符号解析（标准/stdcall/下划线前缀）
//! - 线程安全的进度回调
//! - RAII 风格的资源管理
//!
//! # 参考
//! - https://wimlib.net/
//! - https://wimlib.net/apidoc/

#![allow(non_snake_case)]
#![allow(non_camel_case_types)]

use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

use libloading::{Library, Symbol};

// ============================================================================
// 日志宏定义
// ============================================================================

/// 条件编译的日志输出
/// 在 release 模式下，仅输出关键信息；在 debug 模式下输出详细日志
macro_rules! wimlib_log {
    (debug, $($arg:tt)*) => {
        #[cfg(debug_assertions)]
        eprintln!("[WIMLIB] {}", format!($($arg)*));
    };
    (info, $($arg:tt)*) => {
        println!("[WIMLIB] {}", format!($($arg)*));
    };
    (warn, $($arg:tt)*) => {
        eprintln!("[WIMLIB] ⚠ {}", format!($($arg)*));
    };
    (error, $($arg:tt)*) => {
        eprintln!("[WIMLIB] ✗ {}", format!($($arg)*));
    };
}

// ============================================================================
// 常量定义
// ============================================================================

/// wimlib 进度消息类型
mod progress_msg {
    pub const VERIFY_INTEGRITY: i32 = 6;
    pub const CALC_INTEGRITY: i32 = 7;
    pub const VERIFY_IMAGE: i32 = 25;
}

/// wimlib 错误码
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WimlibError {
    Success = 0,
    AlreadyLocked = 1,
    Decompression = 2,
    Fuse = 3,
    FsDaemonCrashed = 4,
    ImageCount = 5,
    ImageNameCollision = 6,
    Integrity = 7,
    InvalidCaptureConfig = 8,
    InvalidChunkSize = 9,
    InvalidCompressionType = 10,
    InvalidHeader = 11,
    InvalidImage = 12,
    InvalidIntegrityTable = 13,
    InvalidLookupTableEntry = 14,
    InvalidMetadataResource = 15,
    InvalidMultibyteString = 16,
    InvalidOverlay = 17,
    InvalidParam = 18,
    InvalidPartNumber = 19,
    InvalidPipableWim = 20,
    InvalidReparseData = 21,
    InvalidResourceHash = 22,
    InvalidMetadata = 23,
    InvalidUtf16String = 24,
    InvalidUtf8String = 25,
    IsDirectory = 26,
    IsSplitWim = 27,
    LibxmlUtf16HandlerNotRegistered = 28,
    Link = 29,
    MetadataNotFound = 30,
    Mkdir = 31,
    Mqueue = 32,
    Nomem = 33,
    Notdir = 34,
    Notempty = 35,
    NotARegularFile = 36,
    NotAWimFile = 37,
    NotPipable = 38,
    NoFilename = 39,
    Ntfs3g = 40,
    Open = 41,
    Opendir = 42,
    PathDoesNotExist = 43,
    Read = 44,
    Readlink = 45,
    Rename = 46,
    ReparsePointFixupFailed = 47,
    ResourceNotFound = 48,
    ResourceOrder = 49,
    SetAttributes = 50,
    SetReparseData = 51,
    SetSecurity = 52,
    SetShortName = 53,
    SetTimestamps = 54,
    SplitInvalid = 55,
    Stat = 56,
    UnexpectedEndOfFile = 57,
    UnicodeStringNotRepresentable = 58,
    UnknownVersion = 59,
    Unsupported = 60,
    UnsupportedFile = 61,
    WimIsReadonly = 62,
    Write = 63,
    Xml = 64,
    WimIsEncrypted = 65,
    WimlibIsUninitialized = 66,
    AesTruncatedInput = 67,
}

impl WimlibError {
    /// 从错误码创建枚举值
    pub fn from_code(code: i32) -> Option<Self> {
        if code >= 0 && code <= 67 {
            Some(unsafe { std::mem::transmute(code) })
        } else {
            None
        }
    }

    /// 获取错误描述（中文）
    pub fn description(&self) -> &'static str {
        match self {
            Self::Success => "操作成功",
            Self::Decompression => "解压缩失败",
            Self::Integrity => "完整性校验失败",
            Self::InvalidHeader => "无效的文件头",
            Self::InvalidImage => "无效的镜像",
            Self::InvalidIntegrityTable => "无效的完整性表",
            Self::InvalidResourceHash => "资源哈希校验失败",
            Self::InvalidMetadata => "无效的元数据",
            Self::NotAWimFile => "不是有效的 WIM 文件",
            Self::IsSplitWim => "这是分卷 WIM 文件",
            Self::UnexpectedEndOfFile => "文件意外结束（可能被截断）",
            Self::WimIsEncrypted => "WIM 文件已加密",
            Self::Open => "无法打开文件",
            Self::Read => "读取文件失败",
            _ => "未知错误",
        }
    }
}

// ============================================================================
// FFI 类型定义
// ============================================================================

type WIMStruct = *mut c_void;
type ProgressFunc = unsafe extern "C" fn(msg: i32, info: *const c_void, ctx: *mut c_void) -> i32;

/// 完整性校验进度信息
#[repr(C)]
struct ProgressInfoVerifyIntegrity {
    total_bytes: u64,
    completed_bytes: u64,
    total_chunks: u32,
    completed_chunks: u32,
    chunk_size: u32,
    filename: *const u16,
}

/// WIM 文件信息结构体
/// 
/// 该结构体严格按照 wimlib 的 C 头文件定义布局
/// 参考: https://wimlib.net/apidoc/structwimlib__wim__info.html
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct WimInfo {
    /// WIM 文件的 GUID
    pub guid: [u8; 16],
    /// 镜像数量
    pub image_count: u32,
    /// 启动镜像索引
    pub boot_index: u32,
    /// WIM 版本
    pub wim_version: u32,
    /// 块大小
    pub chunk_size: u32,
    /// 分卷编号
    pub part_number: u16,
    /// 总分卷数
    pub total_parts: u16,
    /// 压缩类型
    pub compression_type: i32,
    /// 总大小（字节）
    pub total_bytes: u64,
    /// 是否有完整性表
    pub has_integrity_table: u32,
    /// 是否已打开为可写
    pub opened_for_write: u32,
    /// 是否只读
    pub is_readonly: u32,
    /// 是否有引用的资源
    pub has_rpfix: u32,
    /// 是否为管道格式
    pub is_pipable: u32,
    /// 是否为固实 WIM
    pub is_solid: u32,
    /// 保留字段，确保结构体大小正确
    _reserved: [u8; 48],
}

impl Default for WimInfo {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

// ============================================================================
// 函数指针类型
// ============================================================================

type FnGlobalInit = unsafe extern "C" fn(flags: i32) -> i32;
type FnGlobalCleanup = unsafe extern "C" fn();
type FnOpenWim = unsafe extern "C" fn(path: *const u16, flags: i32, wim: *mut WIMStruct, progress: Option<ProgressFunc>) -> i32;
type FnFree = unsafe extern "C" fn(wim: WIMStruct);
type FnVerifyWim = unsafe extern "C" fn(wim: WIMStruct, flags: i32) -> i32;
type FnRegisterProgressFunction = unsafe extern "C" fn(wim: WIMStruct, func: ProgressFunc, ctx: *mut c_void);
type FnGetErrorString = unsafe extern "C" fn(code: i32) -> *const u16;
type FnGetWimInfo = unsafe extern "C" fn(wim: WIMStruct, info: *mut WimInfo) -> i32;
type FnGetImageName = unsafe extern "C" fn(wim: WIMStruct, index: i32) -> *const u16;
type FnGetImageDescription = unsafe extern "C" fn(wim: WIMStruct, index: i32) -> *const u16;

// ============================================================================
// 全局状态
// ============================================================================

/// 全局进度值（0-100）
static GLOBAL_PROGRESS: AtomicU8 = AtomicU8::new(0);

/// 取消标志
static CANCEL_FLAG: AtomicBool = AtomicBool::new(false);

/// 重置全局状态
fn reset_global_state() {
    GLOBAL_PROGRESS.store(0, Ordering::SeqCst);
    CANCEL_FLAG.store(false, Ordering::SeqCst);
}

/// 进度回调函数
extern "C" fn progress_callback(msg: i32, info: *const c_void, _ctx: *mut c_void) -> i32 {
    // 检查取消标志
    if CANCEL_FLAG.load(Ordering::SeqCst) {
        return 1; // WIMLIB_PROGRESS_STATUS_ABORT
    }

    if msg == progress_msg::VERIFY_INTEGRITY && !info.is_null() {
        let verify_info = unsafe { &*(info as *const ProgressInfoVerifyIntegrity) };
        if verify_info.total_bytes > 0 {
            let percent = ((verify_info.completed_bytes as f64 / verify_info.total_bytes as f64) * 100.0) as u8;
            let current = GLOBAL_PROGRESS.load(Ordering::SeqCst);
            // 只更新更大的进度值（避免回退）
            if percent > current {
                GLOBAL_PROGRESS.store(percent, Ordering::SeqCst);
            }
        }
    }

    0 // WIMLIB_PROGRESS_STATUS_CONTINUE
}

// ============================================================================
// 符号加载器
// ============================================================================

/// 符号变体类型
#[derive(Debug, Clone, Copy)]
enum SymbolVariant {
    /// 标准名称: wimlib_xxx
    Standard,
    /// 下划线前缀: _wimlib_xxx
    Underscore,
    /// stdcall 格式: _wimlib_xxx@N
    Stdcall(usize),
    /// stdcall 无前缀: wimlib_xxx@N
    StdcallNoPrefix(usize),
}

/// 符号加载器
struct SymbolLoader<'a> {
    lib: &'a Library,
}

impl<'a> SymbolLoader<'a> {
    fn new(lib: &'a Library) -> Self {
        Self { lib }
    }

    /// 尝试加载符号，支持多种变体
    unsafe fn load<T>(&self, name: &str, stdcall_size: usize) -> Result<Symbol<'a, T>, String> {
        let variants = [
            SymbolVariant::Standard,
            SymbolVariant::Underscore,
            SymbolVariant::Stdcall(stdcall_size),
            SymbolVariant::StdcallNoPrefix(stdcall_size),
        ];

        for variant in &variants {
            let symbol_name = self.format_symbol_name(name, *variant);
            if let Ok(symbol) = self.lib.get::<T>(symbol_name.as_bytes()) {
                wimlib_log!(debug, "符号 '{}' -> '{}'", name, symbol_name);
                return Ok(symbol);
            }
        }

        Err(format!("无法找到符号 '{}'", name))
    }

    /// 尝试加载可选符号
    unsafe fn load_optional<T>(&self, name: &str, stdcall_size: usize) -> Option<Symbol<'a, T>> {
        self.load(name, stdcall_size).ok()
    }

    fn format_symbol_name(&self, name: &str, variant: SymbolVariant) -> String {
        match variant {
            SymbolVariant::Standard => name.to_string(),
            SymbolVariant::Underscore => format!("_{}", name),
            SymbolVariant::Stdcall(size) => format!("_{}@{}", name, size),
            SymbolVariant::StdcallNoPrefix(size) => format!("{}@{}", name, size),
        }
    }
}

// ============================================================================
// Wimlib 主结构体
// ============================================================================

/// Wimlib DLL 封装
pub struct Wimlib {
    _lib: Arc<Library>,
    global_init: FnGlobalInit,
    global_cleanup: FnGlobalCleanup,
    open_wim: FnOpenWim,
    free_wim: FnFree,
    verify_wim: FnVerifyWim,
    register_progress_function: FnRegisterProgressFunction,
    get_error_string: FnGetErrorString,
    get_wim_info: Option<FnGetWimInfo>,
    get_image_name: Option<FnGetImageName>,
    get_image_description: Option<FnGetImageDescription>,
}

impl Wimlib {
    /// 加载并初始化 wimlib
    ///
    /// 按以下顺序查找 DLL：
    /// 1. 程序所在目录
    /// 2. 系统 PATH
    ///
    /// 支持的 DLL 名称：libwim-15.dll, wimlib-15.dll, wimlib.dll
    pub fn new() -> Result<Self, String> {
        let dll_names = ["libwim-15.dll", "wimlib-15.dll", "wimlib.dll"];
        
        // 查找并加载 DLL
        let lib = Self::find_and_load_dll(&dll_names)?;
        let lib_arc = Arc::new(lib);

        unsafe {
            let loader = SymbolLoader::new(&lib_arc);

            // 加载必需符号
            let global_init = *loader.load::<FnGlobalInit>("wimlib_global_init", 4)
                .map_err(|e| format!("加载 wimlib_global_init 失败: {}", e))?;
            let global_cleanup = *loader.load::<FnGlobalCleanup>("wimlib_global_cleanup", 0)
                .map_err(|e| format!("加载 wimlib_global_cleanup 失败: {}", e))?;
            let open_wim = *loader.load::<FnOpenWim>("wimlib_open_wim", 16)
                .map_err(|e| format!("加载 wimlib_open_wim 失败: {}", e))?;
            let free_wim = *loader.load::<FnFree>("wimlib_free", 4)
                .map_err(|e| format!("加载 wimlib_free 失败: {}", e))?;
            let verify_wim = *loader.load::<FnVerifyWim>("wimlib_verify_wim", 8)
                .map_err(|e| format!("加载 wimlib_verify_wim 失败: {}", e))?;
            let register_progress_function = *loader.load::<FnRegisterProgressFunction>("wimlib_register_progress_function", 12)
                .map_err(|e| format!("加载 wimlib_register_progress_function 失败: {}", e))?;
            let get_error_string = *loader.load::<FnGetErrorString>("wimlib_get_error_string", 4)
                .map_err(|e| format!("加载 wimlib_get_error_string 失败: {}", e))?;

            // 加载可选符号
            let get_wim_info = loader.load_optional::<FnGetWimInfo>("wimlib_get_wim_info", 8).map(|s| *s);
            let get_image_name = loader.load_optional::<FnGetImageName>("wimlib_get_image_name", 8).map(|s| *s);
            let get_image_description = loader.load_optional::<FnGetImageDescription>("wimlib_get_image_description", 8).map(|s| *s);

            // 初始化库
            let init_result = global_init(0);
            if init_result != 0 {
                return Err(format!("wimlib 初始化失败，错误码: {}", init_result));
            }

            wimlib_log!(info, "初始化完成");

            Ok(Self {
                _lib: lib_arc,
                global_init,
                global_cleanup,
                open_wim,
                free_wim,
                verify_wim,
                register_progress_function,
                get_error_string,
                get_wim_info,
                get_image_name,
                get_image_description,
            })
        }
    }

    /// 查找并加载 DLL
    fn find_and_load_dll(names: &[&str]) -> Result<Library, String> {
        let mut last_error = String::new();

        // 1. 尝试程序目录
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(exe_dir) = exe_path.parent() {
                for name in names {
                    let dll_path = exe_dir.join(name);
                    if dll_path.exists() {
                        match unsafe { Library::new(&dll_path) } {
                            Ok(lib) => {
                                wimlib_log!(info, "已加载: {:?}", dll_path);
                                return Ok(lib);
                            }
                            Err(e) => {
                                last_error = format!("{:?}: {}", dll_path, e);
                            }
                        }
                    }
                }
            }
        }

        // 2. 尝试系统 PATH
        for name in names {
            match unsafe { Library::new(*name) } {
                Ok(lib) => {
                    wimlib_log!(info, "已从系统路径加载: {}", name);
                    return Ok(lib);
                }
                Err(e) => {
                    last_error = format!("{}: {}", name, e);
                }
            }
        }

        Err(format!("无法加载 wimlib DLL: {}", last_error))
    }

    /// 打开 WIM 文件
    pub fn open_wim(&self, path: &str) -> Result<WimHandle<'_>, String> {
        let path_utf16: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let mut wim: WIMStruct = null_mut();

        let ret = unsafe { (self.open_wim)(path_utf16.as_ptr(), 0, &mut wim, None) };

        if ret != 0 {
            return Err(self.get_error_message(ret));
        }

        if wim.is_null() {
            return Err("打开 WIM 失败：返回空句柄".to_string());
        }

        Ok(WimHandle { wim, lib: self })
    }

    /// 获取错误信息
    fn get_error_message(&self, code: i32) -> String {
        // 首先尝试获取 wimlib 的错误描述
        let wimlib_msg = unsafe {
            let ptr = (self.get_error_string)(code);
            if !ptr.is_null() {
                Self::utf16_ptr_to_string(ptr)
            } else {
                None
            }
        };

        // 组合错误信息
        let code_desc = WimlibError::from_code(code)
            .map(|e| e.description())
            .unwrap_or("未知错误");

        match wimlib_msg {
            Some(msg) if !msg.is_empty() => format!("{} ({})", msg, code_desc),
            _ => format!("{} (错误码: {})", code_desc, code),
        }
    }

    /// 将 UTF-16 指针转换为 String
    unsafe fn utf16_ptr_to_string(ptr: *const u16) -> Option<String> {
        if ptr.is_null() {
            return None;
        }

        let mut len = 0;
        while *ptr.add(len) != 0 {
            len += 1;
            if len > 4096 {
                return None; // 安全限制
            }
        }

        if len == 0 {
            return None;
        }

        let slice = std::slice::from_raw_parts(ptr, len);
        Some(String::from_utf16_lossy(slice))
    }

    /// 获取当前全局进度
    pub fn get_global_progress() -> u8 {
        GLOBAL_PROGRESS.load(Ordering::SeqCst)
    }

    /// 设置取消标志
    pub fn request_cancel() {
        CANCEL_FLAG.store(true, Ordering::SeqCst);
    }

    /// 检查是否已取消
    pub fn is_cancelled() -> bool {
        CANCEL_FLAG.load(Ordering::SeqCst)
    }
}

impl Drop for Wimlib {
    fn drop(&mut self) {
        unsafe {
            (self.global_cleanup)();
        }
        wimlib_log!(debug, "已清理");
    }
}

// ============================================================================
// WIM 句柄
// ============================================================================

/// WIM 文件句柄（RAII）
pub struct WimHandle<'a> {
    wim: WIMStruct,
    lib: &'a Wimlib,
}

impl<'a> WimHandle<'a> {
    /// 验证 WIM 完整性
    pub fn verify(&self) -> Result<(), String> {
        // 重置全局状态
        reset_global_state();

        // 注册进度回调
        unsafe {
            (self.lib.register_progress_function)(self.wim, progress_callback, null_mut());
        }

        // 执行校验
        let ret = unsafe { (self.lib.verify_wim)(self.wim, 0) };

        if ret != 0 {
            return Err(self.lib.get_error_message(ret));
        }

        Ok(())
    }

    /// 获取 WIM 信息
    pub fn get_info(&self) -> Option<WimInfo> {
        let func = self.lib.get_wim_info?;
        let mut info = WimInfo::default();

        let ret = unsafe { func(self.wim, &mut info) };
        if ret == 0 {
            Some(info)
        } else {
            None
        }
    }

    /// 获取镜像数量
    pub fn get_image_count(&self) -> i32 {
        if let Some(info) = self.get_info() {
            info.image_count as i32
        } else {
            -1
        }
    }

    /// 获取镜像名称
    pub fn get_image_name(&self, index: i32) -> Option<String> {
        let func = self.lib.get_image_name?;
        unsafe {
            let ptr = func(self.wim, index);
            Wimlib::utf16_ptr_to_string(ptr)
        }
    }

    /// 获取镜像描述
    pub fn get_image_description(&self, index: i32) -> Option<String> {
        let func = self.lib.get_image_description?;
        unsafe {
            let ptr = func(self.wim, index);
            Wimlib::utf16_ptr_to_string(ptr)
        }
    }

    /// 获取镜像信息（名称和描述）
    pub fn get_image_info(&self, index: i32) -> (String, String) {
        let name = self.get_image_name(index).unwrap_or_default();
        let desc = self.get_image_description(index).unwrap_or_default();
        (name, desc)
    }

    /// 获取当前校验进度
    pub fn get_verify_progress(&self) -> u8 {
        Wimlib::get_global_progress()
    }
}

impl<'a> Drop for WimHandle<'a> {
    fn drop(&mut self) {
        if !self.wim.is_null() {
            unsafe {
                (self.lib.free_wim)(self.wim);
            }
        }
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_codes() {
        assert_eq!(WimlibError::from_code(0), Some(WimlibError::Success));
        assert_eq!(WimlibError::from_code(7), Some(WimlibError::Integrity));
        assert_eq!(WimlibError::from_code(37), Some(WimlibError::NotAWimFile));
        assert_eq!(WimlibError::from_code(-1), None);
        assert_eq!(WimlibError::from_code(100), None);
    }

    #[test]
    fn test_error_descriptions() {
        assert_eq!(WimlibError::Success.description(), "操作成功");
        assert_eq!(WimlibError::Integrity.description(), "完整性校验失败");
        assert_eq!(WimlibError::NotAWimFile.description(), "不是有效的 WIM 文件");
    }

    #[test]
    fn test_wim_info_default() {
        let info = WimInfo::default();
        assert_eq!(info.image_count, 0);
        assert_eq!(info.total_bytes, 0);
    }

    #[test]
    fn test_global_progress() {
        reset_global_state();
        assert_eq!(Wimlib::get_global_progress(), 0);
        
        GLOBAL_PROGRESS.store(50, Ordering::SeqCst);
        assert_eq!(Wimlib::get_global_progress(), 50);
        
        reset_global_state();
        assert_eq!(Wimlib::get_global_progress(), 0);
    }

    #[test]
    fn test_cancel_flag() {
        reset_global_state();
        assert!(!Wimlib::is_cancelled());
        
        Wimlib::request_cancel();
        assert!(Wimlib::is_cancelled());
        
        reset_global_state();
        assert!(!Wimlib::is_cancelled());
    }
}
