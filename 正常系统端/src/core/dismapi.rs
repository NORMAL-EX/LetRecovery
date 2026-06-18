//! DISM API (dismapi.dll) 封装 —— 第三方驱动导出 `DismExportDriver`。
//!
//! 旧方案是手工遍历 `DriverStore\FileRepository` 并用前缀启发式判断是否第三方驱动，
//! 既可能漏导、也可能误导系统自带驱动。这里改用微软官方 DISM API 的
//! `DismExportDriver`（等价于 `dism /export-driver`），由系统自己判定 OOB(out-of-box)
//! 驱动并连同关联文件一并导出，可靠性显著更高。在线（当前运行系统）与离线（PE 下对已
//! 部署系统的分区）均支持。
//!
//! `DismExportDriver` 本身不带进度回调，这里先用 `DismGetDrivers` 取得 OOB 驱动总数，
//! 再开一个看门狗线程轮询目标目录里已生成的子目录数 / 总数，回调上报实时进度。
//!
//! dismapi.dll 在现代 Windows 与 ADK 制作的 WinPE 中随系统提供；通过 libloading 动态
//! 加载，加载或调用失败时调用方应回退到旧的手工导出方案。

#![cfg(windows)]

use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use libloading::Library;

/// DISM 会话句柄（dismapi.h: `typedef UINT DismSession;`）
type DismSession = u32;

/// DismLogLevel: 0=Errors, 1=Errors+Warnings, 2=Errors+Warnings+Info
const DISM_LOG_ERRORS_WARNINGS_INFO: u32 = 2;

/// 在线映像句柄常量字符串（dismapi.h: `DISM_ONLINE_IMAGE`）
const DISM_ONLINE_IMAGE: &str = "DISM_{53BFAE52-B167-4E2F-A258-0A37B57FF845}";

/// DismInitialize 在已初始化时返回的 HRESULT：`DISMAPI_E_DISMAPI_ALREADY_INITIALIZED`
const DISMAPI_E_DISMAPI_ALREADY_INITIALIZED: i32 = 0xC004_0001u32 as i32;

// ---- dismapi.dll 导出函数签名（全部 stdcall，返回 HRESULT） ----
type FnDismInitialize =
    unsafe extern "system" fn(u32, *const u16, *const u16) -> i32;
type FnDismShutdown = unsafe extern "system" fn() -> i32;
type FnDismOpenSession =
    unsafe extern "system" fn(*const u16, *const u16, *const u16, *mut DismSession) -> i32;
type FnDismCloseSession = unsafe extern "system" fn(DismSession) -> i32;
type FnDismExportDriver = unsafe extern "system" fn(DismSession, *const u16) -> i32;
// HRESULT DismGetDrivers(DismSession, BOOL AllDrivers, DismDriverPackage** , UINT* Count)
type FnDismGetDrivers =
    unsafe extern "system" fn(DismSession, i32, *mut *mut c_void, *mut u32) -> i32;
// HRESULT DismDelete(VOID* DismStructure)
type FnDismDelete = unsafe extern "system" fn(*mut c_void) -> i32;

/// 将字符串转成以 NUL 结尾的 UTF-16
fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

/// 进度回调：`(已完成, 总数)`；总数为 0 表示未知（不确定进度）。
pub type ProgressCb<'a> = dyn Fn(u32, u32) + Send + Sync + 'a;

/// dismapi.dll 封装
pub struct DismApi {
    _lib: Library,
    initialize: FnDismInitialize,
    shutdown: FnDismShutdown,
    open_session: FnDismOpenSession,
    close_session: FnDismCloseSession,
    export_driver: FnDismExportDriver,
    get_drivers: Option<FnDismGetDrivers>,
    delete: Option<FnDismDelete>,
}

impl DismApi {
    /// 动态加载 dismapi.dll 并解析所需导出。
    pub fn load() -> Result<Self> {
        let lib = Self::load_library()?;
        unsafe {
            let initialize: FnDismInitialize = *lib
                .get::<FnDismInitialize>(b"DismInitialize\0")
                .context("dismapi.dll 缺少 DismInitialize")?;
            let shutdown: FnDismShutdown = *lib
                .get::<FnDismShutdown>(b"DismShutdown\0")
                .context("dismapi.dll 缺少 DismShutdown")?;
            let open_session: FnDismOpenSession = *lib
                .get::<FnDismOpenSession>(b"DismOpenSession\0")
                .context("dismapi.dll 缺少 DismOpenSession")?;
            let close_session: FnDismCloseSession = *lib
                .get::<FnDismCloseSession>(b"DismCloseSession\0")
                .context("dismapi.dll 缺少 DismCloseSession")?;
            let export_driver: FnDismExportDriver = *lib
                .get::<FnDismExportDriver>(b"DismExportDriver\0")
                .context("dismapi.dll 缺少 DismExportDriver")?;
            // 仅用于取总数报进度，缺失不致命
            let get_drivers = lib
                .get::<FnDismGetDrivers>(b"DismGetDrivers\0")
                .ok()
                .map(|s| *s);
            let delete = lib
                .get::<FnDismDelete>(b"DismDelete\0")
                .ok()
                .map(|s| *s);

            Ok(Self {
                _lib: lib,
                initialize,
                shutdown,
                open_session,
                close_session,
                export_driver,
                get_drivers,
                delete,
            })
        }
    }

    /// 加载 dismapi.dll：优先程序目录随附的 `bin\Dism\dismapi.dll`（与 dism.exe 同一套），
    /// 回退到系统 dismapi.dll。这样在精简 WinPE（可能没有完整 DISM API/维护栈）下也能用上
    /// 随产品打包的那一套，与 `DismCmd` 优先用 `bin\Dism\dism.exe` 的约定保持一致。
    fn load_library() -> Result<Library> {
        use libloading::os::windows::{Library as WinLib, LOAD_WITH_ALTERED_SEARCH_PATH};

        // 1) 随附的 bin\Dism\dismapi.dll —— 用 LOAD_WITH_ALTERED_SEARCH_PATH 让其依赖
        //    (dismcore.dll / providers 等)从该 DLL 所在目录解析，而不是仅从系统目录。
        let bundled = crate::utils::path::get_exe_dir()
            .join("bin")
            .join("Dism")
            .join("dismapi.dll");
        if bundled.exists() {
            match unsafe { WinLib::load_with_flags(&bundled, LOAD_WITH_ALTERED_SEARCH_PATH) } {
                Ok(l) => {
                    log::info!("[DISM] 使用随附 DISM API: {}", bundled.display());
                    return Ok(l.into());
                }
                Err(e) => {
                    log::warn!(
                        "[DISM] 加载随附 dismapi.dll 失败({}): {}，改用系统 DISM API",
                        bundled.display(),
                        e
                    );
                }
            }
        }

        // 2) 回退：系统 dismapi.dll（System32 / PATH 标准搜索顺序）
        let lib = unsafe { Library::new("dismapi.dll") }
            .context("无法加载 dismapi.dll（系统/PE 可能未包含 DISM API）")?;
        log::info!("[DISM] 使用系统 DISM API (dismapi.dll)");
        Ok(lib)
    }

    /// 导出当前运行系统的第三方驱动（在线映像）。
    pub fn export_drivers_online(
        &self,
        destination: &Path,
        progress: Option<&ProgressCb<'_>>,
    ) -> Result<usize> {
        self.export_drivers_impl(None, destination, progress)
    }

    /// 导出离线系统分区的第三方驱动（PE 下对已部署系统）。
    /// `image_root` 形如 `C:\`，即离线 Windows 映像根目录。
    pub fn export_drivers_offline(
        &self,
        image_root: &Path,
        destination: &Path,
        progress: Option<&ProgressCb<'_>>,
    ) -> Result<usize> {
        self.export_drivers_impl(Some(image_root), destination, progress)
    }

    /// 实际执行：初始化 → 开会话 → 取总数 → 起进度看门狗 → 导出 → 清理。
    fn export_drivers_impl(
        &self,
        image_root: Option<&Path>,
        destination: &Path,
        progress: Option<&ProgressCb<'_>>,
    ) -> Result<usize> {
        std::fs::create_dir_all(destination)
            .with_context(|| format!("创建驱动导出目录失败: {:?}", destination))?;

        // 1) 初始化 DISM API（已初始化则复用，不再重复 Shutdown）
        let mut owns_init = true;
        unsafe {
            let hr = (self.initialize)(DISM_LOG_ERRORS_WARNINGS_INFO, std::ptr::null(), std::ptr::null());
            if hr == DISMAPI_E_DISMAPI_ALREADY_INITIALIZED {
                owns_init = false;
            } else if hr < 0 {
                bail!("DismInitialize 失败: HRESULT 0x{:08X}", hr as u32);
            }
        }
        // 确保任何提前返回都能 Shutdown
        let _shutdown_guard = ShutdownGuard {
            shutdown: self.shutdown,
            active: owns_init,
        };

        // 2) 打开会话（在线/离线）
        let image_path_w = match image_root {
            Some(root) => {
                // 规范成带尾部反斜杠的根路径，DISM 对离线映像根更稳
                let s = root.to_string_lossy();
                let s = s.trim_end_matches(|c| c == '\\' || c == '/');
                to_wide(&format!("{}\\", s))
            }
            None => to_wide(DISM_ONLINE_IMAGE),
        };

        let mut session: DismSession = 0;
        unsafe {
            let hr = (self.open_session)(
                image_path_w.as_ptr(),
                std::ptr::null(), // WindowsDirectory: 离线默认 "Windows"
                std::ptr::null(), // SystemDrive
                &mut session,
            );
            if hr < 0 {
                bail!(
                    "DismOpenSession 失败: HRESULT 0x{:08X}（映像: {}）",
                    hr as u32,
                    image_root.map(|p| p.display().to_string()).unwrap_or_else(|| "在线".into())
                );
            }
        }
        let _session_guard = SessionGuard {
            close: self.close_session,
            session,
        };

        // 3) 取 OOB 驱动总数（best-effort，仅用于进度）
        let total = self.count_oob_drivers(session).unwrap_or(0);
        if total > 0 {
            log::info!("[DISM] 待导出第三方驱动: {} 个", total);
        }
        if let Some(cb) = progress {
            cb(0, total);
        }

        // 4) 用 scope 起看门狗线程：DismExportDriver 是阻塞调用、且不带进度回调，
        //    看门狗轮询目标目录已生成的子目录数（每个驱动一个子目录）/ 总数，实时上报进度。
        //    scope 保证看门狗对借用的 progress 回调引用始终有效。
        let stop = Arc::new(AtomicBool::new(false));
        let export_hr = std::thread::scope(|s| {
            let watcher = progress.map(|cb| {
                let stop_c = stop.clone();
                let dest = destination;
                s.spawn(move || {
                    while !stop_c.load(Ordering::Relaxed) {
                        let n = count_subdirs(dest) as u32;
                        cb(n.min(total.max(n)), total);
                        std::thread::sleep(std::time::Duration::from_millis(400));
                    }
                })
            });

            // 5) 执行导出（阻塞）
            let dest_w = to_wide(&destination.to_string_lossy());
            let hr = unsafe { (self.export_driver)(session, dest_w.as_ptr()) };

            // 6) 停看门狗
            stop.store(true, Ordering::Relaxed);
            if let Some(h) = watcher {
                let _ = h.join();
            }
            hr
        });

        if export_hr < 0 {
            bail!("DismExportDriver 失败: HRESULT 0x{:08X}", export_hr as u32);
        }

        // 以目标目录实际生成的子目录数为导出结果计数（总数未知时用它）
        let exported = count_subdirs(destination);
        if let Some(cb) = progress {
            let total = if total == 0 { exported as u32 } else { total };
            cb(exported as u32, total);
        }
        log::info!("[DISM] DismExportDriver 完成，已导出 {} 个驱动到 {:?}", exported, destination);
        Ok(exported)
    }

    /// 用 DismGetDrivers(AllDrivers=FALSE) 取第三方驱动数量。
    fn count_oob_drivers(&self, session: DismSession) -> Option<u32> {
        let get = self.get_drivers?;
        let mut buf: *mut c_void = std::ptr::null_mut();
        let mut count: u32 = 0;
        let hr = unsafe { get(session, 0 /* AllDrivers=FALSE → 仅 OOB */, &mut buf, &mut count) };
        if hr < 0 {
            return None;
        }
        // 释放 DISM 分配的缓冲
        if !buf.is_null() {
            if let Some(del) = self.delete {
                unsafe {
                    let _ = del(buf);
                }
            }
        }
        Some(count)
    }
}

/// 统计目录下的一级子目录数量（DismExportDriver 每个驱动建一个子目录）。
fn count_subdirs(dir: &Path) -> usize {
    match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .filter(|e| e.path().is_dir())
            .count(),
        Err(_) => 0,
    }
}

/// 会话 RAII：作用域结束自动 DismCloseSession。
struct SessionGuard {
    close: FnDismCloseSession,
    session: DismSession,
}
impl Drop for SessionGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = (self.close)(self.session);
        }
    }
}

/// 初始化 RAII：仅当本调用拥有初始化时，作用域结束自动 DismShutdown。
struct ShutdownGuard {
    shutdown: FnDismShutdown,
    active: bool,
}
impl Drop for ShutdownGuard {
    fn drop(&mut self) {
        if self.active {
            unsafe {
                let _ = (self.shutdown)();
            }
        }
    }
}
