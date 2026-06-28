//! wimgapi.dll（Windows 映像 API，WIMGAPI）动态封装
//!
//! 作为 wimlib 之外的**可选镜像引擎**，提供 apply（应用/释放）与 capture（捕获/备份）。
//! 通过 libloading 在运行时加载 `wimgapi.dll`（Windows 系统自带，WinPE 中通常也存在）。
//! 函数签名与常量严格对照 `wimgapi.h`。
//!
//! 设计上仅暴露与 [`crate::wimlib::WimlibManager`] 同名同形的 `apply_image` /
//! `capture_image`，便于 [`crate::wim_engine::WimEngineManager`] 在两者间切换并回退。

#![allow(non_snake_case)]
#![allow(non_camel_case_types)]
#![allow(dead_code)]

use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::mpsc::Sender;

use libloading::Library;

use crate::image_meta::WimProgress;

// ============================================================================
// 常量（对照 wimgapi.h）
// ============================================================================

const WIM_GENERIC_READ: u32 = 0x8000_0000;
const WIM_GENERIC_WRITE: u32 = 0x4000_0000;

// dwCreationDisposition
const WIM_CREATE_NEW: u32 = 1;
const WIM_CREATE_ALWAYS: u32 = 2;
const WIM_OPEN_EXISTING: u32 = 3;
const WIM_OPEN_ALWAYS: u32 = 4;

// 压缩类型（与 wimlib 取值一致：NONE=0 / XPRESS=1 / LZX=2 / LZMS=3）
// 由调用方以 u32 传入，直接作为 dwCompressionType。

// 消息常量：WIM_MSG = WM_APP = 0x8000
const WIM_MSG: u32 = 0x8000;
/// 进度消息：wParam = 完成百分比(0-100)，lParam = 预计剩余毫秒
const WIM_MSG_PROGRESS: u32 = WIM_MSG + 2;
/// 回调返回 ERROR_SUCCESS(0) 表示继续
const WIM_MSG_SUCCESS: u32 = 0;
/// WIMRegisterMessageCallback 失败返回值
const INVALID_CALLBACK_VALUE: u32 = 0xFFFF_FFFF;

type HANDLE = *mut c_void;
type DWORD = u32;
type BOOL = i32;

// 回调：DWORD WINAPI (DWORD msg, WPARAM, LPARAM, PVOID)
type WimMsgCallback = unsafe extern "system" fn(DWORD, usize, isize, *mut c_void) -> DWORD;

// ============================================================================
// FFI 函数类型（对照 wimgapi.h）
// ============================================================================

type FnWIMCreateFile =
    unsafe extern "system" fn(*const u16, DWORD, DWORD, DWORD, DWORD, *mut DWORD) -> HANDLE;
type FnWIMCloseHandle = unsafe extern "system" fn(HANDLE) -> BOOL;
type FnWIMSetTemporaryPath = unsafe extern "system" fn(HANDLE, *const u16) -> BOOL;
type FnWIMLoadImage = unsafe extern "system" fn(HANDLE, DWORD) -> HANDLE;
type FnWIMApplyImage = unsafe extern "system" fn(HANDLE, *const u16, DWORD) -> BOOL;
type FnWIMCaptureImage = unsafe extern "system" fn(HANDLE, *const u16, DWORD) -> HANDLE;
type FnWIMGetImageCount = unsafe extern "system" fn(HANDLE) -> DWORD;
type FnWIMSetImageInformation = unsafe extern "system" fn(HANDLE, *const c_void, DWORD) -> BOOL;
type FnWIMRegisterMessageCallback =
    unsafe extern "system" fn(HANDLE, Option<WimMsgCallback>, *mut c_void) -> DWORD;
type FnWIMUnregisterMessageCallback =
    unsafe extern "system" fn(HANDLE, Option<WimMsgCallback>) -> DWORD;

fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// 取最近一次 Win32 错误并拼成中文描述。
fn last_err(prefix: &str) -> String {
    let e = std::io::Error::last_os_error();
    format!("{}（{}）", prefix, e)
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
// 进度回调
// ============================================================================

struct ProgressCtx {
    tx: Option<Sender<WimProgress>>,
    last: u8,
    status_prefix: &'static str,
}

unsafe extern "system" fn message_callback(
    msg: DWORD,
    wparam: usize,
    _lparam: isize,
    ctx: *mut c_void,
) -> DWORD {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if msg == WIM_MSG_PROGRESS && !ctx.is_null() {
            let state = &mut *(ctx as *mut ProgressCtx);
            let percent = (wparam as u32).min(100) as u8;
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
    WIM_MSG_SUCCESS
}

// ============================================================================
// 封装
// ============================================================================

pub struct WimgapiManager {
    _lib: Library,
    create_file: FnWIMCreateFile,
    close_handle: FnWIMCloseHandle,
    set_temp_path: FnWIMSetTemporaryPath,
    load_image: FnWIMLoadImage,
    apply_image_fn: FnWIMApplyImage,
    capture_image_fn: FnWIMCaptureImage,
    get_image_count: FnWIMGetImageCount,
    set_image_information: FnWIMSetImageInformation,
    register_cb: FnWIMRegisterMessageCallback,
    unregister_cb: FnWIMUnregisterMessageCallback,
}

impl WimgapiManager {
    /// 加载 wimgapi.dll 并解析所需符号。任一步失败即返回 Err（上层据此回退 libwim）。
    pub fn new() -> Result<Self, String> {
        let lib = unsafe { Library::new("wimgapi.dll") }
            .map_err(|e| format!("加载 wimgapi.dll 失败: {}", e))?;

        let create_file = load_sym!(lib, b"WIMCreateFile\0", FnWIMCreateFile);
        let close_handle = load_sym!(lib, b"WIMCloseHandle\0", FnWIMCloseHandle);
        let set_temp_path = load_sym!(lib, b"WIMSetTemporaryPath\0", FnWIMSetTemporaryPath);
        let load_image = load_sym!(lib, b"WIMLoadImage\0", FnWIMLoadImage);
        let apply_image_fn = load_sym!(lib, b"WIMApplyImage\0", FnWIMApplyImage);
        let capture_image_fn = load_sym!(lib, b"WIMCaptureImage\0", FnWIMCaptureImage);
        let get_image_count = load_sym!(lib, b"WIMGetImageCount\0", FnWIMGetImageCount);
        let set_image_information =
            load_sym!(lib, b"WIMSetImageInformation\0", FnWIMSetImageInformation);
        let register_cb =
            load_sym!(lib, b"WIMRegisterMessageCallback\0", FnWIMRegisterMessageCallback);
        let unregister_cb = load_sym!(
            lib,
            b"WIMUnregisterMessageCallback\0",
            FnWIMUnregisterMessageCallback
        );

        Ok(Self {
            _lib: lib,
            create_file,
            close_handle,
            set_temp_path,
            load_image,
            apply_image_fn,
            capture_image_fn,
            get_image_count,
            set_image_information,
            register_cb,
            unregister_cb,
        })
    }

    fn set_temp_to_env(&self, h_wim: HANDLE) {
        let temp = std::env::temp_dir();
        let wtemp = to_wide(&temp.to_string_lossy());
        unsafe {
            (self.set_temp_path)(h_wim, wtemp.as_ptr());
        }
    }

    /// 释放/应用镜像到目录（与 `WimlibManager::apply_image` 等价）。
    pub fn apply_image(
        &self,
        image_file: &str,
        target_dir: &str,
        index: u32,
        progress_tx: Option<Sender<WimProgress>>,
    ) -> Result<(), String> {
        let wpath = to_wide(image_file);
        let mut disp: DWORD = 0;
        let h_wim = unsafe {
            (self.create_file)(
                wpath.as_ptr(),
                WIM_GENERIC_READ,
                WIM_OPEN_EXISTING,
                0,
                0,
                &mut disp,
            )
        };
        if h_wim.is_null() {
            return Err(last_err("WIMCreateFile（读取）失败"));
        }

        let result = (|| {
            self.set_temp_to_env(h_wim);

            let mut ctx = Box::new(ProgressCtx {
                tx: progress_tx,
                last: 255,
                status_prefix: "释放镜像中",
            });
            let cb_ok = unsafe {
                (self.register_cb)(
                    h_wim,
                    Some(message_callback),
                    &mut *ctx as *mut ProgressCtx as *mut c_void,
                )
            } != INVALID_CALLBACK_VALUE;

            let h_img = unsafe { (self.load_image)(h_wim, index) };
            let res = if h_img.is_null() {
                Err(last_err("WIMLoadImage 失败"))
            } else {
                let wtarget = to_wide(target_dir);
                let ok = unsafe { (self.apply_image_fn)(h_img, wtarget.as_ptr(), 0) } != 0;
                let r = if ok {
                    Ok(())
                } else {
                    Err(last_err("WIMApplyImage 失败"))
                };
                unsafe { (self.close_handle)(h_img) };
                r
            };

            if cb_ok {
                unsafe { (self.unregister_cb)(h_wim, Some(message_callback)) };
            }
            drop(ctx);
            res
        })();

        unsafe { (self.close_handle)(h_wim) };
        result
    }

    /// 捕获/备份目录到 WIM/ESD（compression：与 wimlib 取值一致；文件已存在则追加）。
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
        let (disposition, ctype) = if append {
            (WIM_OPEN_EXISTING, 0)
        } else {
            (WIM_CREATE_ALWAYS, compression)
        };

        let wpath = to_wide(image_file);
        let mut disp: DWORD = 0;
        let h_wim = unsafe {
            (self.create_file)(
                wpath.as_ptr(),
                WIM_GENERIC_WRITE,
                disposition,
                0,
                ctype,
                &mut disp,
            )
        };
        if h_wim.is_null() {
            return Err(last_err("WIMCreateFile（写入）失败"));
        }

        let result = (|| {
            self.set_temp_to_env(h_wim);

            let mut ctx = Box::new(ProgressCtx {
                tx: progress_tx,
                last: 255,
                status_prefix: "备份镜像中",
            });
            let cb_ok = unsafe {
                (self.register_cb)(
                    h_wim,
                    Some(message_callback),
                    &mut *ctx as *mut ProgressCtx as *mut c_void,
                )
            } != INVALID_CALLBACK_VALUE;

            let wsource = to_wide(source_dir);
            let h_img = unsafe { (self.capture_image_fn)(h_wim, wsource.as_ptr(), 0) };
            let res = if h_img.is_null() {
                Err(last_err("WIMCaptureImage 失败"))
            } else {
                // 设置镜像名称/描述（best-effort，失败不影响备份本身）
                if !name.is_empty() || !description.is_empty() {
                    if let Err(e) = self.set_image_info(h_img, name, description) {
                        log::warn!("wimgapi 设置镜像信息失败（忽略）：{}", e);
                    }
                }
                unsafe { (self.close_handle)(h_img) };
                Ok(())
            };

            if cb_ok {
                unsafe { (self.unregister_cb)(h_wim, Some(message_callback)) };
            }
            drop(ctx);
            res
        })();

        unsafe { (self.close_handle)(h_wim) };
        result
    }

    /// 通过 WIMSetImageInformation 写入镜像 NAME/DESCRIPTION（UTF-16 + BOM 的 XML）。
    fn set_image_info(&self, h_img: HANDLE, name: &str, description: &str) -> Result<(), String> {
        fn esc(s: &str) -> String {
            s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
        }
        let xml = format!(
            "<IMAGE><NAME>{}</NAME><DESCRIPTION>{}</DESCRIPTION></IMAGE>",
            esc(name),
            esc(description)
        );
        let mut buf: Vec<u16> = Vec::with_capacity(xml.len() + 2);
        buf.push(0xFEFF); // UTF-16 BOM
        buf.extend(xml.encode_utf16());
        let cb = (buf.len() * 2) as DWORD;
        let ok =
            unsafe { (self.set_image_information)(h_img, buf.as_ptr() as *const c_void, cb) } != 0;
        if ok {
            Ok(())
        } else {
            Err(last_err("WIMSetImageInformation 失败"))
        }
    }
}
