//! wimlib (libwim-15.dll) 动态库封装
//!
//! 取代原先基于 wimgapi.dll 的镜像操作。提供：
//! - `Wimlib` / `WimHandle`：只读的完整性校验与信息读取（供 image_verify 使用）
//! - `WimlibManager`：apply（释放）/ capture（备份）/ split（SWM 分卷）/ 信息读取 /
//!   目录树遍历（替代挂载式的目录结构校验）
//!
//! 所有常量、结构体字段偏移、函数签名均严格对照 wimlib.h（1.14.x）。
//!
//! 参考: https://wimlib.net/apidoc/

#![allow(non_snake_case)]
#![allow(non_camel_case_types)]
#![allow(dead_code)]

use std::ffi::c_void;
use std::os::raw::{c_int, c_uint};
use std::os::windows::ffi::OsStrExt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::Sender;

use libloading::Library;

/// 只读校验路径用的全局进度（0-100），供 image_verify 的监控线程读取
static VERIFY_GLOBAL_PROGRESS: AtomicU8 = AtomicU8::new(0);

/// 进程级 wimlib_global_init 只执行一次（多个 Wimlib/WimlibManager 实例共享）。
/// 避免重复 init，以及在某实例 Drop 时调用 global_cleanup 影响其它仍在使用的实例。
static WIMLIB_INIT: std::sync::Once = std::sync::Once::new();
static WIMLIB_INIT_OK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 确保 wimlib 全局初始化只发生一次；返回是否初始化成功。
fn ensure_global_init(init: FnGlobalInit) -> bool {
    WIMLIB_INIT.call_once(|| {
        let rc = unsafe { init(0) };
        WIMLIB_INIT_OK.store(rc == WIMLIB_ERR_SUCCESS, Ordering::SeqCst);
    });
    WIMLIB_INIT_OK.load(Ordering::SeqCst)
}

use crate::image_meta::{parse_image_info_from_xml, ImageInfo, WimProgress};

// ============================================================================
// 常量（严格对照 wimlib.h）
// ============================================================================

/// 进度消息类型
mod progress_msg {
    pub const EXTRACT_STREAMS: i32 = 4;
    pub const WRITE_STREAMS: i32 = 12;
    pub const VERIFY_INTEGRITY: i32 = 16;
    /// wimlib_verify_wim() 校验文件数据时发送此消息（info 指向 verify_streams）
    pub const VERIFY_STREAMS: i32 = 29;
}

/// 进度回调返回值
const WIMLIB_PROGRESS_STATUS_CONTINUE: c_int = 0;
const WIMLIB_PROGRESS_STATUS_ABORT: c_int = 1;

/// 压缩类型（与 wimgapi 的 WIM_COMPRESS_* 取值一致：NONE=0/XPRESS=1/LZX=2/LZMS=3）
const WIMLIB_COMPRESSION_TYPE_NONE: c_int = 0;
const WIMLIB_COMPRESSION_TYPE_LZX: c_int = 2;
const WIMLIB_COMPRESSION_TYPE_LZMS: c_int = 3;

/// 特殊镜像索引
const WIMLIB_ALL_IMAGES: c_int = -1;

/// open / write / add / ref flags
const WIMLIB_REF_FLAG_GLOB_ENABLE: c_int = 0x0000_0001;
const WIMLIB_WRITE_FLAG_SOLID: c_int = 0x0000_1000;
const WIMLIB_WRITE_FLAG_REBUILD: c_int = 0x0000_0040;
const WIMLIB_ADD_FLAG_WINCONFIG: c_int = 0x0000_0800;
const WIMLIB_ITERATE_DIR_TREE_FLAG_RECURSIVE: c_int = 0x0000_0001;

/// 常用错误码（wimlib 真实取值，有跳号）
const WIMLIB_ERR_SUCCESS: c_int = 0;
pub const WIMLIB_ERR_INTEGRITY: c_int = 13;
const WIMLIB_ERR_PATH_DOES_NOT_EXIST: c_int = 49;

/// 把 wimlib 错误码转成中文描述
fn err_description(code: i32) -> &'static str {
    match code {
        0 => "操作成功",
        2 => "解压缩失败",
        13 => "完整性校验失败（镜像可能损坏）",
        17 => "无效的文件头",
        18 => "无效的镜像索引",
        19 => "无效的完整性表",
        21 => "无效的元数据资源",
        28 => "资源哈希校验失败",
        33 => "这是分卷 WIM，需要引入其余分卷",
        39 => "内存不足",
        43 => "不是有效的 WIM 文件",
        49 => "镜像中不存在该路径",
        50 => "读取文件失败",
        55 => "资源未找到（可能缺少分卷）",
        65 => "文件意外结束（可能被截断）",
        72 => "写入失败",
        74 => "WIM 文件已加密",
        _ => "未知错误",
    }
}

// ============================================================================
// FFI 类型
// ============================================================================

type WIMStruct = *mut c_void;

/// enum wimlib_progress_status (*)(enum wimlib_progress_msg, union*, void*)
type ProgressFunc =
    unsafe extern "C" fn(msg: c_int, info: *const c_void, ctx: *mut c_void) -> c_int;

type FnGlobalInit = unsafe extern "C" fn(flags: c_int) -> c_int;
type FnGlobalCleanup = unsafe extern "C" fn();
type FnFree = unsafe extern "C" fn(wim: WIMStruct);
type FnGetErrorString = unsafe extern "C" fn(code: c_int) -> *const u16;
type FnOpenWimWithProgress = unsafe extern "C" fn(
    path: *const u16,
    open_flags: c_int,
    wim_ret: *mut WIMStruct,
    progfunc: Option<ProgressFunc>,
    progctx: *mut c_void,
) -> c_int;
type FnCreateNewWim = unsafe extern "C" fn(ctype: c_int, wim_ret: *mut WIMStruct) -> c_int;
type FnVerifyWim = unsafe extern "C" fn(wim: WIMStruct, flags: c_int) -> c_int;
type FnRegisterProgress =
    unsafe extern "C" fn(wim: WIMStruct, func: ProgressFunc, ctx: *mut c_void);
type FnExtractImage =
    unsafe extern "C" fn(wim: WIMStruct, image: c_int, target: *const u16, flags: c_int) -> c_int;
type FnExtractPaths = unsafe extern "C" fn(
    wim: WIMStruct,
    image: c_int,
    target: *const u16,
    paths: *const *const u16,
    num_paths: usize,
    flags: c_int,
) -> c_int;
type FnAddImage = unsafe extern "C" fn(
    wim: WIMStruct,
    source: *const u16,
    name: *const u16,
    config_file: *const u16,
    add_flags: c_int,
) -> c_int;
type FnWrite = unsafe extern "C" fn(
    wim: WIMStruct,
    path: *const u16,
    image: c_int,
    write_flags: c_int,
    num_threads: c_uint,
) -> c_int;
type FnOverwrite =
    unsafe extern "C" fn(wim: WIMStruct, write_flags: c_int, num_threads: c_uint) -> c_int;
type FnSetOutputCompression = unsafe extern "C" fn(wim: WIMStruct, ctype: c_int) -> c_int;
type FnSplit = unsafe extern "C" fn(
    wim: WIMStruct,
    swm_name: *const u16,
    part_size: u64,
    write_flags: c_int,
) -> c_int;
type FnReferenceResourceFiles = unsafe extern "C" fn(
    wim: WIMStruct,
    globs: *const *const u16,
    count: c_uint,
    ref_flags: c_int,
    open_flags: c_int,
) -> c_int;
type FnIterateDirTree = unsafe extern "C" fn(
    wim: WIMStruct,
    image: c_int,
    path: *const u16,
    flags: c_int,
    cb: unsafe extern "C" fn(dentry: *const c_void, ctx: *mut c_void) -> c_int,
    user_ctx: *mut c_void,
) -> c_int;
type FnGetXmlData =
    unsafe extern "C" fn(wim: WIMStruct, buf_ret: *mut *mut c_void, size_ret: *mut usize) -> c_int;
type FnGetWimInfo = unsafe extern "C" fn(wim: WIMStruct, info: *mut WimInfo) -> c_int;
type FnGetImageName = unsafe extern "C" fn(wim: WIMStruct, index: c_int) -> *const u16;
type FnGetImageDescription = unsafe extern "C" fn(wim: WIMStruct, index: c_int) -> *const u16;
type FnGetVersionString = unsafe extern "C" fn() -> *const u8;

type FnUpdateImage = unsafe extern "C" fn(
    wim: WIMStruct,
    image: c_int,
    cmds: *const WimlibUpdateCommandAdd,
    num_cmds: usize,
    update_flags: c_int,
) -> c_int;

/// wimlib_update_image 的 ADD 命令布局（仅使用 ADD 变体）。
///
/// C 定义为 `struct { enum wimlib_update_op op; union { add; delete; rename; } }`。
/// 64 位下 enum(4B) 之后需对齐到指针(8B)，联合体自偏移 8 起；下面 `#[repr(C)]`
/// 中 `op`(c_int) 之后会自动补 4 字节，使 `add` 落在偏移 8，布局与 C 端一致。
/// 由于 ADD 是联合体里最大的成员，把指针指向本结构传给 wimlib 完全合法。
#[repr(C)]
struct WimlibAddCommand {
    fs_source_path: *const u16,
    wim_target_path: *const u16,
    config_file: *const u16,
    add_flags: c_int,
}
#[repr(C)]
struct WimlibUpdateCommandAdd {
    op: c_int,
    add: WimlibAddCommand,
}
const WIMLIB_UPDATE_OP_ADD: c_int = 0;


/// struct wimlib_wim_info（前若干字段，足够取 image_count / 完整性表标志）
#[repr(C)]
#[derive(Clone, Copy)]
pub struct WimInfo {
    pub guid: [u8; 16],
    pub image_count: u32,
    pub boot_index: u32,
    pub wim_version: u32,
    pub chunk_size: u32,
    pub part_number: u16,
    pub total_parts: u16,
    pub compression_type: i32,
    pub total_bytes: u64,
    /// 位域区域（最低位 = has_integrity_table）
    pub flags: u32,
    pub reserved: [u32; 9],
}

impl Default for WimInfo {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

impl WimInfo {
    pub fn has_integrity_table(&self) -> bool {
        self.flags & 0x1 != 0
    }
}

// ============================================================================
// 工具函数
// ============================================================================

fn to_wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn path_to_wide(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

unsafe fn utf16_ptr_to_string(ptr: *const u16) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let mut len = 0usize;
    while *ptr.add(len) != 0 {
        len += 1;
        if len > 8192 {
            break;
        }
    }
    if len == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len)))
}

unsafe fn read_u64(base: *const c_void, off: usize) -> u64 {
    ((base as *const u8).add(off) as *const u64).read_unaligned()
}

/// wimlib 压缩用的最优线程数 = 逻辑 CPU 数；探测失败回退 0（由 wimlib 自动按在线处理器数选择）。
/// 显式传入核数以确保压缩阶段吃满全部核心。
fn optimal_threads() -> c_uint {
    std::thread::available_parallelism()
        .map(|n| n.get() as c_uint)
        .unwrap_or(0)
}

/// 在 wimlib 重负载操作（apply/capture/split/verify）期间，临时把**进程**优先级提升到
/// HIGH，结束后恢复原值。让 wimlib 内部的压缩/解压工作线程获得更激进的 CPU 调度，
/// 在与其它进程争用时也能顶到更高占用，提升吞吐。RAII：drop 时自动恢复。
#[cfg(windows)]
struct HighPriorityGuard {
    previous: u32,
    raised: bool,
}

#[cfg(windows)]
impl HighPriorityGuard {
    fn new() -> Self {
        use windows::Win32::System::Threading::{
            GetCurrentProcess, GetPriorityClass, SetPriorityClass, HIGH_PRIORITY_CLASS,
        };
        unsafe {
            let h = GetCurrentProcess();
            let previous = GetPriorityClass(h);
            let raised = SetPriorityClass(h, HIGH_PRIORITY_CLASS).is_ok();
            if raised {
                log::info!("wimlib 操作：已临时提升进程优先级为 HIGH（结束后恢复）");
            }
            HighPriorityGuard {
                previous,
                raised: raised && previous != 0,
            }
        }
    }
}

#[cfg(windows)]
impl Drop for HighPriorityGuard {
    fn drop(&mut self) {
        if self.raised {
            use windows::Win32::System::Threading::{
                GetCurrentProcess, SetPriorityClass, PROCESS_CREATION_FLAGS,
            };
            unsafe {
                let _ =
                    SetPriorityClass(GetCurrentProcess(), PROCESS_CREATION_FLAGS(self.previous));
            }
        }
    }
}

#[cfg(not(windows))]
struct HighPriorityGuard;
#[cfg(not(windows))]
impl HighPriorityGuard {
    fn new() -> Self {
        HighPriorityGuard
    }
}

// ============================================================================
// 进度回调
// ============================================================================

/// 通过 ctx 指针传递给回调的状态
struct ProgressCtx {
    tx: Option<Sender<WimProgress>>,
    last: u8,
    status_prefix: &'static str,
}

unsafe extern "C" fn progress_callback(
    msg: c_int,
    info: *const c_void,
    ctx: *mut c_void,
) -> c_int {
    let result = catch_unwind(AssertUnwindSafe(|| {
        if ctx.is_null() || info.is_null() {
            return;
        }
        let state = &mut *(ctx as *mut ProgressCtx);

        let (completed, total) = match msg {
            // extract: completed_bytes@48, total_bytes@40
            progress_msg::EXTRACT_STREAMS => (read_u64(info, 48), read_u64(info, 40)),
            // write_streams: total_bytes@0, completed_bytes@16
            progress_msg::WRITE_STREAMS => (read_u64(info, 16), read_u64(info, 0)),
            // verify integrity: total_bytes@0, completed_bytes@8
            progress_msg::VERIFY_INTEGRITY => (read_u64(info, 8), read_u64(info, 0)),
            _ => return,
        };

        if total > 0 {
            let percent = ((completed as f64 / total as f64) * 100.0).min(100.0) as u8;
            if percent != state.last {
                state.last = percent;
                if let Some(ref tx) = state.tx {
                    let _ = tx.send(WimProgress {
                        percentage: percent,
                        status: format!("{} {}%", state.status_prefix, percent),
                    });
                }
            }
        }
    }));

    match result {
        Ok(()) => WIMLIB_PROGRESS_STATUS_CONTINUE,
        Err(_) => WIMLIB_PROGRESS_STATUS_ABORT,
    }
}

/// iterate_dir_tree 用的空回调（只关心路径是否存在，由返回码判断）
unsafe extern "C" fn noop_iterate_cb(_dentry: *const c_void, _ctx: *mut c_void) -> c_int {
    0
}

/// 只读校验进度回调：把完整性校验进度写入全局变量
unsafe extern "C" fn verify_progress_callback(
    msg: c_int,
    info: *const c_void,
    _ctx: *mut c_void,
) -> c_int {
    let result = catch_unwind(AssertUnwindSafe(|| {
        if info.is_null() {
            return;
        }
        // 不同消息的 info 结构体布局不同：
        // - VERIFY_INTEGRITY (整完整性表)：integrity { total_bytes@0, completed_bytes@8 }
        //   仅在用 CHECK_INTEGRITY 打开时才会收到。
        // - VERIFY_STREAMS  (校验文件数据)：verify_streams {
        //     wimfile@0, total_streams@8, total_bytes@16, completed_streams@24, completed_bytes@32 }
        //   wimlib_verify_wim() 实际发送的是这个消息。
        let (total, completed) = match msg {
            progress_msg::VERIFY_INTEGRITY => (read_u64(info, 0), read_u64(info, 8)),
            progress_msg::VERIFY_STREAMS => (read_u64(info, 16), read_u64(info, 32)),
            _ => return,
        };
        if total > 0 {
            let percent = ((completed as f64 / total as f64) * 100.0).min(100.0) as u8;
            let cur = VERIFY_GLOBAL_PROGRESS.load(Ordering::SeqCst);
            if percent > cur {
                VERIFY_GLOBAL_PROGRESS.store(percent, Ordering::SeqCst);
            }
        }
    }));
    match result {
        Ok(()) => WIMLIB_PROGRESS_STATUS_CONTINUE,
        Err(_) => WIMLIB_PROGRESS_STATUS_ABORT,
    }
}

// ============================================================================
// DLL 加载（共享给 Wimlib 与 WimlibManager）
// ============================================================================

fn find_and_load_dll() -> Result<Library, String> {
    // 先确保 DLL 就位（PE 环境兜底，内置并释放），再尝试加载
    crate::ensure_dll_available();
    let names = ["libwim-15.dll", "wimlib-15.dll", "libwim.dll", "wimlib.dll"];
    let mut last = String::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for n in names {
                let p = dir.join(n);
                if p.exists() {
                    match unsafe { Library::new(&p) } {
                        Ok(l) => return Ok(l),
                        Err(e) => last = format!("{:?}: {}", p, e),
                    }
                }
            }
        }
    }
    for n in names {
        match unsafe { Library::new(n) } {
            Ok(l) => return Ok(l),
            Err(e) => last = format!("{}: {}", n, e),
        }
    }
    Err(format!("无法加载 wimlib DLL（libwim-15.dll 等）：{}", last))
}

macro_rules! load_sym {
    ($lib:expr, $name:literal, $ty:ty) => {{
        let s: libloading::Symbol<$ty> = unsafe {
            $lib.get($name)
                .map_err(|e| format!("符号 {} 解析失败: {}", String::from_utf8_lossy($name), e))?
        };
        *s
    }};
}

// ============================================================================
// 只读封装：Wimlib / WimHandle（供 image_verify 使用，保持原 API）
// ============================================================================

pub struct Wimlib {
    _lib: Library,
    global_cleanup: FnGlobalCleanup,
    open_wim: FnOpenWimWithProgress,
    free_wim: FnFree,
    verify_wim: FnVerifyWim,
    get_error_string: FnGetErrorString,
    get_wim_info: FnGetWimInfo,
    get_image_name: FnGetImageName,
    get_image_description: FnGetImageDescription,
    reference_resource_files: FnReferenceResourceFiles,
}

impl Wimlib {
    pub fn new() -> Result<Self, String> {
        let lib = find_and_load_dll()?;
        let global_init = load_sym!(lib, b"wimlib_global_init\0", FnGlobalInit);
        let global_cleanup = load_sym!(lib, b"wimlib_global_cleanup\0", FnGlobalCleanup);
        let open_wim = load_sym!(lib, b"wimlib_open_wim_with_progress\0", FnOpenWimWithProgress);
        let free_wim = load_sym!(lib, b"wimlib_free\0", FnFree);
        let verify_wim = load_sym!(lib, b"wimlib_verify_wim\0", FnVerifyWim);
        let get_error_string = load_sym!(lib, b"wimlib_get_error_string\0", FnGetErrorString);
        let get_wim_info = load_sym!(lib, b"wimlib_get_wim_info\0", FnGetWimInfo);
        let get_image_name = load_sym!(lib, b"wimlib_get_image_name\0", FnGetImageName);
        let get_image_description =
            load_sym!(lib, b"wimlib_get_image_description\0", FnGetImageDescription);
        let reference_resource_files =
            load_sym!(lib, b"wimlib_reference_resource_files\0", FnReferenceResourceFiles);

        if !ensure_global_init(global_init) {
            return Err("wimlib 全局初始化失败".to_string());
        }

        Ok(Self {
            _lib: lib,
            global_cleanup,
            open_wim,
            free_wim,
            verify_wim,
            get_error_string,
            get_wim_info,
            get_image_name,
            get_image_description,
            reference_resource_files,
        })
    }

    fn error_message(&self, code: c_int) -> String {
        let msg = unsafe {
            let p = (self.get_error_string)(code);
            utf16_ptr_to_string(p)
        };
        match msg {
            Some(m) if !m.is_empty() => format!("{}（{}，错误码 {}）", m, err_description(code), code),
            _ => format!("{}（错误码 {}）", err_description(code), code),
        }
    }

    pub fn open_wim(&self, path: &str) -> Result<WimHandle<'_>, String> {
        VERIFY_GLOBAL_PROGRESS.store(0, Ordering::SeqCst);
        let wpath = to_wide(path);
        let mut wim: WIMStruct = null_mut();
        // 注册校验进度回调（用于后续 verify 的进度上报）
        let rc = unsafe {
            (self.open_wim)(
                wpath.as_ptr(),
                0,
                &mut wim,
                Some(verify_progress_callback),
                null_mut(),
            )
        };
        if rc != WIMLIB_ERR_SUCCESS {
            return Err(self.error_message(rc));
        }
        if wim.is_null() {
            return Err("打开 WIM 失败：返回空句柄".to_string());
        }
        Ok(WimHandle { wim, lib: self })
    }

    /// 当前完整性校验进度（0-100）
    pub fn get_global_progress() -> u8 {
        VERIFY_GLOBAL_PROGRESS.load(Ordering::SeqCst)
    }
}

// 不在 Drop 中调用 wimlib_global_cleanup：
// 它是进程级全局操作，多个实例并存时提前 cleanup 会影响其它仍在使用的实例。
// wimlib 文档说明 cleanup 非必需，进程退出即可释放。

pub struct WimHandle<'a> {
    wim: WIMStruct,
    lib: &'a Wimlib,
}

impl<'a> WimHandle<'a> {
    /// 校验完整性（无完整性表时 wimlib 直接返回成功）
    pub fn verify(&self) -> Result<(), String> {
        let _prio = HighPriorityGuard::new();
        let rc = unsafe { (self.lib.verify_wim)(self.wim, 0) };
        if rc != WIMLIB_ERR_SUCCESS {
            return Err(self.lib.error_message(rc));
        }
        Ok(())
    }

    /// 为分卷 WIM（SWM）引入其余分卷。`globs` 为通配路径（如 "dir/install*.swm"）。
    pub fn reference_resource_globs(&self, globs: &[&str]) -> Result<(), String> {
        let wide: Vec<Vec<u16>> = globs.iter().map(|g| to_wide(g)).collect();
        let ptrs: Vec<*const u16> = wide.iter().map(|v| v.as_ptr()).collect();
        let rc = unsafe {
            (self.lib.reference_resource_files)(
                self.wim,
                ptrs.as_ptr(),
                ptrs.len() as c_uint,
                WIMLIB_REF_FLAG_GLOB_ENABLE,
                0,
            )
        };
        if rc != WIMLIB_ERR_SUCCESS {
            return Err(self.lib.error_message(rc));
        }
        Ok(())
    }

    pub fn get_info(&self) -> Option<WimInfo> {
        let mut info = WimInfo::default();
        let rc = unsafe { (self.lib.get_wim_info)(self.wim, &mut info) };
        if rc == WIMLIB_ERR_SUCCESS {
            Some(info)
        } else {
            None
        }
    }

    pub fn get_image_count(&self) -> i32 {
        self.get_info().map(|i| i.image_count as i32).unwrap_or(-1)
    }

    pub fn get_image_name(&self, index: i32) -> Option<String> {
        unsafe { utf16_ptr_to_string((self.lib.get_image_name)(self.wim, index)) }
    }

    pub fn get_image_description(&self, index: i32) -> Option<String> {
        unsafe { utf16_ptr_to_string((self.lib.get_image_description)(self.wim, index)) }
    }

    pub fn get_image_info(&self, index: i32) -> (String, String) {
        (
            self.get_image_name(index).unwrap_or_default(),
            self.get_image_description(index).unwrap_or_default(),
        )
    }
}

impl<'a> Drop for WimHandle<'a> {
    fn drop(&mut self) {
        if !self.wim.is_null() {
            unsafe { (self.lib.free_wim)(self.wim) };
        }
    }
}

// ============================================================================
// 读写封装：WimlibManager（替代 wimgapi 的 WimManager）
// ============================================================================

pub struct WimlibManager {
    _lib: Library,
    global_cleanup: FnGlobalCleanup,
    open_wim: FnOpenWimWithProgress,
    create_new_wim: FnCreateNewWim,
    free_wim: FnFree,
    get_error_string: FnGetErrorString,
    register_progress: FnRegisterProgress,
    extract_image: FnExtractImage,
    extract_paths: FnExtractPaths,
    add_image: FnAddImage,
    write: FnWrite,
    overwrite: FnOverwrite,
    set_output_compression: FnSetOutputCompression,
    split: FnSplit,
    reference_resource_files: FnReferenceResourceFiles,
    iterate_dir_tree: FnIterateDirTree,
    get_xml_data: FnGetXmlData,
    update_image: FnUpdateImage,
}

impl WimlibManager {
    pub fn new() -> Result<Self, String> {
        let lib = find_and_load_dll()?;
        let global_init = load_sym!(lib, b"wimlib_global_init\0", FnGlobalInit);
        let global_cleanup = load_sym!(lib, b"wimlib_global_cleanup\0", FnGlobalCleanup);
        let open_wim = load_sym!(lib, b"wimlib_open_wim_with_progress\0", FnOpenWimWithProgress);
        let create_new_wim = load_sym!(lib, b"wimlib_create_new_wim\0", FnCreateNewWim);
        let free_wim = load_sym!(lib, b"wimlib_free\0", FnFree);
        let get_error_string = load_sym!(lib, b"wimlib_get_error_string\0", FnGetErrorString);
        let register_progress =
            load_sym!(lib, b"wimlib_register_progress_function\0", FnRegisterProgress);
        let extract_image = load_sym!(lib, b"wimlib_extract_image\0", FnExtractImage);
        let extract_paths = load_sym!(lib, b"wimlib_extract_paths\0", FnExtractPaths);
        let add_image = load_sym!(lib, b"wimlib_add_image\0", FnAddImage);
        let write = load_sym!(lib, b"wimlib_write\0", FnWrite);
        let overwrite = load_sym!(lib, b"wimlib_overwrite\0", FnOverwrite);
        let set_output_compression =
            load_sym!(lib, b"wimlib_set_output_compression_type\0", FnSetOutputCompression);
        let split = load_sym!(lib, b"wimlib_split\0", FnSplit);
        let reference_resource_files =
            load_sym!(lib, b"wimlib_reference_resource_files\0", FnReferenceResourceFiles);
        let iterate_dir_tree = load_sym!(lib, b"wimlib_iterate_dir_tree\0", FnIterateDirTree);
        let get_xml_data = load_sym!(lib, b"wimlib_get_xml_data\0", FnGetXmlData);
        let update_image = load_sym!(lib, b"wimlib_update_image\0", FnUpdateImage);

        if !ensure_global_init(global_init) {
            return Err("wimlib 全局初始化失败".to_string());
        }

        Ok(Self {
            _lib: lib,
            global_cleanup,
            open_wim,
            create_new_wim,
            free_wim,
            get_error_string,
            register_progress,
            extract_image,
            extract_paths,
            add_image,
            write,
            overwrite,
            set_output_compression,
            split,
            reference_resource_files,
            iterate_dir_tree,
            get_xml_data,
            update_image,
        })
    }

    fn error_message(&self, code: c_int) -> String {
        let msg = unsafe { utf16_ptr_to_string((self.get_error_string)(code)) };
        match msg {
            Some(m) if !m.is_empty() => format!("{}（{}，错误码 {}）", m, err_description(code), code),
            _ => format!("{}（错误码 {}）", err_description(code), code),
        }
    }

    /// 打开 WIM（不带进度）
    fn open(&self, path: &str) -> Result<WIMStruct, String> {
        let wpath = to_wide(path);
        let mut wim: WIMStruct = null_mut();
        let rc = unsafe { (self.open_wim)(wpath.as_ptr(), 0, &mut wim, None, null_mut()) };
        if rc != WIMLIB_ERR_SUCCESS {
            return Err(self.error_message(rc));
        }
        if wim.is_null() {
            return Err("打开 WIM 失败：空句柄".to_string());
        }
        Ok(wim)
    }

    /// 释放/应用镜像到目录（与 wimgapi::WimManager::apply_image 等价）
    pub fn apply_image(
        &self,
        image_file: &str,
        target_dir: &str,
        index: u32,
        progress_tx: Option<Sender<WimProgress>>,
    ) -> Result<(), String> {
        let wim = self.open(image_file)?;
        let _prio = HighPriorityGuard::new();

        // 安装进度回调
        let mut ctx = Box::new(ProgressCtx {
            tx: progress_tx,
            last: 255,
            status_prefix: "释放镜像中",
        });
        unsafe {
            (self.register_progress)(
                wim,
                progress_callback,
                &mut *ctx as *mut ProgressCtx as *mut c_void,
            );
        }

        // SWM：引入同目录其余分卷
        if image_file.to_lowercase().ends_with(".swm") {
            if let Err(e) = self.reference_swm(wim, image_file) {
                unsafe { (self.free_wim)(wim) };
                return Err(e);
            }
        }

        let wtarget = to_wide(target_dir);
        let rc = unsafe { (self.extract_image)(wim, index as c_int, wtarget.as_ptr(), 0) };
        unsafe { (self.free_wim)(wim) };
        drop(ctx);

        if rc != WIMLIB_ERR_SUCCESS {
            return Err(self.error_message(rc));
        }
        Ok(())
    }

    /// 引入 SWM 其余分卷：glob = 同目录 <stem 去尾数字>*.swm
    fn reference_swm(&self, wim: WIMStruct, first_part: &str) -> Result<(), String> {
        let p = Path::new(first_part);
        let dir = p.parent().filter(|d| !d.as_os_str().is_empty());
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let base = stem.trim_end_matches(|c: char| c.is_ascii_digit());
        let base = if base.is_empty() { stem } else { base };
        let pattern = format!("{}*.swm", base);
        let glob = match dir {
            Some(d) => d.join(pattern).to_string_lossy().into_owned(),
            None => pattern,
        };
        let wglob = to_wide(&glob);
        let globs: [*const u16; 1] = [wglob.as_ptr()];
        let rc = unsafe {
            (self.reference_resource_files)(wim, globs.as_ptr(), 1, WIMLIB_REF_FLAG_GLOB_ENABLE, 0)
        };
        if rc != WIMLIB_ERR_SUCCESS {
            return Err(self.error_message(rc));
        }
        Ok(())
    }

    /// 捕获/备份目录到 WIM/ESD（compression：2=LZX 普通 WIM；3=LZMS 走 solid=ESD）
    /// 若目标文件已存在则追加镜像（overwrite）。
    pub fn capture_image(
        &self,
        source_dir: &str,
        image_file: &str,
        name: &str,
        description: &str,
        compression: u32,
        progress_tx: Option<Sender<WimProgress>>,
    ) -> Result<(), String> {
        let append = Path::new(image_file).exists();
        let _prio = HighPriorityGuard::new();

        let wim = if append {
            self.open(image_file)?
        } else {
            let mut w: WIMStruct = null_mut();
            let ctype = if compression == 3 {
                WIMLIB_COMPRESSION_TYPE_LZMS
            } else if compression == 0 {
                WIMLIB_COMPRESSION_TYPE_NONE
            } else {
                WIMLIB_COMPRESSION_TYPE_LZX
            };
            let rc = unsafe { (self.create_new_wim)(ctype, &mut w) };
            if rc != WIMLIB_ERR_SUCCESS {
                return Err(self.error_message(rc));
            }
            w
        };

        let mut ctx = Box::new(ProgressCtx {
            tx: progress_tx,
            last: 255,
            status_prefix: "备份镜像中",
        });
        unsafe {
            (self.register_progress)(
                wim,
                progress_callback,
                &mut *ctx as *mut ProgressCtx as *mut c_void,
            );
        }

        let result = (|| {
            // 添加镜像（使用 Windows 默认捕获配置排除 pagefile 等）
            let wsource = to_wide(source_dir);
            let wname = to_wide(name);
            let rc = unsafe {
                (self.add_image)(
                    wim,
                    wsource.as_ptr(),
                    if name.is_empty() { null_mut() } else { wname.as_ptr() },
                    null_mut(),
                    WIMLIB_ADD_FLAG_WINCONFIG,
                )
            };
            if rc != WIMLIB_ERR_SUCCESS {
                return Err(self.error_message(rc));
            }
            let _ = description; // 描述可后续通过 set_image_property 设置，这里从简

            let solid = compression == 3;
            if append {
                let flags = if solid { WIMLIB_WRITE_FLAG_SOLID } else { 0 };
                let rc = unsafe { (self.overwrite)(wim, flags, optimal_threads()) };
                if rc != WIMLIB_ERR_SUCCESS {
                    return Err(self.error_message(rc));
                }
            } else {
                let wpath = to_wide(image_file);
                let flags = if solid {
                    WIMLIB_WRITE_FLAG_SOLID | WIMLIB_WRITE_FLAG_REBUILD
                } else {
                    0
                };
                let rc = unsafe {
                    (self.write)(wim, wpath.as_ptr(), WIMLIB_ALL_IMAGES, flags, optimal_threads())
                };
                if rc != WIMLIB_ERR_SUCCESS {
                    return Err(self.error_message(rc));
                }
            }
            Ok(())
        })();

        unsafe { (self.free_wim)(wim) };
        drop(ctx);
        result
    }

    /// 往**已存在** WIM 的第 `image` 个镜像注入一个本地文件，再覆盖写回（wimlib_overwrite）。
    ///
    /// - `wim_path`：要修改的 WIM 文件路径
    /// - `image`：镜像序号（1 起；PE 的 boot.wim 通常注入第 1 个可引导镜像）
    /// - `src_file`：本地源文件路径
    /// - `dest_in_wim`：镜像内目标绝对路径（如 `"\\LR_BitLockerKeys.json"`）
    ///
    /// 用途：把 BitLocker 恢复密钥文件打包进 PE 的 boot.wim，使 PE 启动后可直接读取。
    pub fn add_file_to_image(
        &self,
        wim_path: &str,
        image: i32,
        src_file: &str,
        dest_in_wim: &str,
    ) -> Result<(), String> {
        let wim = self.open(wim_path)?;
        let result = (|| {
            let wsrc = to_wide(src_file);
            let wdest = to_wide(dest_in_wim);
            let cmd = WimlibUpdateCommandAdd {
                op: WIMLIB_UPDATE_OP_ADD,
                add: WimlibAddCommand {
                    fs_source_path: wsrc.as_ptr(),
                    wim_target_path: wdest.as_ptr(),
                    config_file: std::ptr::null(),
                    add_flags: 0,
                },
            };
            let rc = unsafe { (self.update_image)(wim, image as c_int, &cmd, 1, 0) };
            if rc != WIMLIB_ERR_SUCCESS {
                return Err(self.error_message(rc));
            }
            let rc = unsafe { (self.overwrite)(wim, 0, optimal_threads()) };
            if rc != WIMLIB_ERR_SUCCESS {
                return Err(self.error_message(rc));
            }
            Ok(())
        })();
        unsafe { (self.free_wim)(wim) };
        result
    }

    /// 把已有 WIM 分割为 SWM 分卷
    pub fn split_wim(&self, wim_path: &str, swm_path: &str, part_size_mb: u64) -> Result<(), String> {
        let wim = self.open(wim_path)?;
        let _prio = HighPriorityGuard::new();
        let wswm = to_wide(swm_path);
        let part_size = part_size_mb.saturating_mul(1024 * 1024);
        let rc = unsafe { (self.split)(wim, wswm.as_ptr(), part_size, 0) };
        unsafe { (self.free_wim)(wim) };
        if rc != WIMLIB_ERR_SUCCESS {
            return Err(self.error_message(rc));
        }
        Ok(())
    }

    /// 读取镜像信息（解析 wimlib 提供的 XML）
    pub fn get_image_info(&self, image_file: &str) -> Result<Vec<ImageInfo>, String> {
        let wim = self.open(image_file)?;
        let mut buf: *mut c_void = null_mut();
        let mut size: usize = 0;
        let rc = unsafe { (self.get_xml_data)(wim, &mut buf, &mut size) };
        if rc != WIMLIB_ERR_SUCCESS || buf.is_null() || size == 0 {
            unsafe { (self.free_wim)(wim) };
            return Err(self.error_message(rc));
        }
        // XML 为 UTF-16LE（带 BOM），size 为字节数
        let xml = unsafe {
            let bytes = std::slice::from_raw_parts(buf as *const u8, size);
            decode_utf16le(bytes)
        };
        unsafe { (self.free_wim)(wim) };

        let images = parse_image_info_from_xml(&xml);
        if images.is_empty() {
            return Err("未解析到镜像信息".to_string());
        }
        Ok(images)
    }

    /// 判断镜像某卷是否包含指定路径（替代挂载查目录结构）。
    /// 用 iterate_dir_tree 的返回码判断：成功=存在，PATH_DOES_NOT_EXIST=不存在。
    pub fn image_contains_path(&self, image_file: &str, index: u32, path_in_image: &str) -> Result<bool, String> {
        let wim = self.open(image_file)?;
        let wpath = to_wide(path_in_image);
        let rc = unsafe {
            (self.iterate_dir_tree)(wim, index as c_int, wpath.as_ptr(), 0, noop_iterate_cb, null_mut())
        };
        unsafe { (self.free_wim)(wim) };
        if rc == WIMLIB_ERR_SUCCESS {
            Ok(true)
        } else if rc == WIMLIB_ERR_PATH_DOES_NOT_EXIST {
            Ok(false)
        } else {
            Err(self.error_message(rc))
        }
    }

    /// 校验镜像是否为有效 Windows 系统（检查 \Windows\System32\ntdll.dll 是否存在）
    pub fn verify_windows_image(&self, image_file: &str, index: u32) -> Result<bool, String> {
        self.image_contains_path(image_file, index, "\\Windows\\System32\\ntdll.dll")
    }

    /// 一次打开、批量判断镜像某卷是否包含其中任意一条路径（任一命中即 true）。
    /// 仅读元数据资源、不挂载，适合廉价探测内置应答文件等，避免多次开关 WIM。
    pub fn image_contains_any_path(
        &self,
        image_file: &str,
        index: u32,
        paths: &[&str],
    ) -> Result<bool, String> {
        let wim = self.open(image_file)?;
        let mut found = false;
        for p in paths {
            let wpath = to_wide(p);
            let rc = unsafe {
                (self.iterate_dir_tree)(wim, index as c_int, wpath.as_ptr(), 0, noop_iterate_cb, null_mut())
            };
            if rc == WIMLIB_ERR_SUCCESS {
                found = true;
                break;
            } else if rc == WIMLIB_ERR_PATH_DOES_NOT_EXIST {
                continue;
            } else {
                unsafe { (self.free_wim)(wim) };
                return Err(self.error_message(rc));
            }
        }
        unsafe { (self.free_wim)(wim) };
        Ok(found)
    }

    /// 从镜像中仅提取若干路径到目标目录（用于离线读取 ntdll.dll 版本等）
    pub fn extract_paths(
        &self,
        image_file: &str,
        index: u32,
        target_dir: &str,
        paths: &[&str],
    ) -> Result<(), String> {
        let wim = self.open(image_file)?;
        let wtarget = to_wide(target_dir);
        let wpaths: Vec<Vec<u16>> = paths.iter().map(|p| to_wide(p)).collect();
        let ptrs: Vec<*const u16> = wpaths.iter().map(|v| v.as_ptr()).collect();
        let rc = unsafe {
            (self.extract_paths)(
                wim,
                index as c_int,
                wtarget.as_ptr(),
                ptrs.as_ptr(),
                ptrs.len(),
                0,
            )
        };
        unsafe { (self.free_wim)(wim) };
        if rc != WIMLIB_ERR_SUCCESS {
            return Err(self.error_message(rc));
        }
        Ok(())
    }
}

// 同上：不在 Drop 中调用 wimlib_global_cleanup。

/// UTF-16LE 字节数组（可能带 BOM）解码为 String
fn decode_utf16le(data: &[u8]) -> String {
    let start = if data.len() >= 2 && data[0] == 0xFF && data[1] == 0xFE {
        2
    } else {
        0
    };
    let mut units = Vec::with_capacity((data.len() - start) / 2);
    let mut i = start;
    while i + 1 < data.len() {
        units.push(u16::from_le_bytes([data[i], data[i + 1]]));
        i += 2;
    }
    while units.last() == Some(&0) {
        units.pop();
    }
    String::from_utf16_lossy(&units)
}
