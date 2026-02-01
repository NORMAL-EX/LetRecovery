//! DISM API 封装模块
//!
//! 该模块提供 Windows DISM API (dismapi.dll) 的 Rust 封装。
//! 主要用于离线镜像服务操作，如驱动注入、更新包安装等。
//!
//! 注意：DISM API 在某些 PE 环境中可能不可用或行为不一致，
//! 建议优先使用 dism_exe.rs 中的命令行方式。

use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Context, Result};
use libloading::Library;

// =============================================================================
// 常量定义
// =============================================================================

/// DISM 日志级别
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DismLogLevel {
    /// 错误级别
    Errors = 0,
    /// 错误和警告
    ErrorsWarnings = 1,
    /// 全部日志
    ErrorsWarningsInfo = 2,
}

/// DISM 会话选项
const DISM_ONLINE_IMAGE: *const u16 = 1 as *const u16; // 特殊常量表示在线镜像

/// DISM 包特性状态
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)]
pub enum DismPackageFeatureState {
    NotPresent = 0,
    UninstallPending = 1,
    Staged = 2,
    Removed = 3,
    Installed = 4,
    InstallPending = 5,
    Superseded = 6,
    PartiallyInstalled = 7,
}

// =============================================================================
// 类型定义
// =============================================================================

/// DISM 会话句柄
type DismSession = u32;

/// DISM 进度回调函数类型
type DismProgressCallback = Option<
    unsafe extern "system" fn(current: u32, total: u32, user_data: *mut c_void),
>;

/// DISM 包信息结构
#[repr(C)]
#[derive(Debug)]
pub struct DismPackageInfo {
    pub package_name: *const u16,
    pub package_state: u32,
    pub release_type: u32,
    pub install_time: u64,
}

/// DISM 驱动信息结构
#[repr(C)]
#[derive(Debug)]
pub struct DismDriverPackage {
    pub published_name: *const u16,
    pub original_file_name: *const u16,
    pub in_box: i32,
    pub catalog_file: *const u16,
    pub class_name: *const u16,
    pub class_description: *const u16,
    pub class_guid: *const u16,
    pub provider_name: *const u16,
    pub date: u64,
    pub major_version: u32,
    pub minor_version: u32,
    pub build: u32,
    pub revision: u32,
}

// =============================================================================
// 函数指针类型
// =============================================================================

type FnDismInitialize = unsafe extern "system" fn(
    log_level: u32,
    log_file_path: *const u16,
    scratch_directory: *const u16,
) -> i32;

type FnDismShutdown = unsafe extern "system" fn() -> i32;

type FnDismOpenSession = unsafe extern "system" fn(
    image_path: *const u16,
    windows_directory: *const u16,
    system_drive: *const u16,
    session: *mut DismSession,
) -> i32;

type FnDismCloseSession = unsafe extern "system" fn(session: DismSession) -> i32;

type FnDismAddDriver = unsafe extern "system" fn(
    session: DismSession,
    driver_path: *const u16,
    force_unsigned: i32,
) -> i32;

type FnDismAddPackage = unsafe extern "system" fn(
    session: DismSession,
    package_path: *const u16,
    ignore_check: i32,
    prevent_pending: i32,
    progress_callback: DismProgressCallback,
    user_data: *mut c_void,
) -> i32;

type FnDismGetPackages = unsafe extern "system" fn(
    session: DismSession,
    packages: *mut *mut DismPackageInfo,
    count: *mut u32,
) -> i32;

type FnDismGetDrivers = unsafe extern "system" fn(
    session: DismSession,
    all_drivers: i32,
    drivers: *mut *mut DismDriverPackage,
    count: *mut u32,
) -> i32;

type FnDismDelete = unsafe extern "system" fn(object: *mut c_void) -> i32;

type FnDismGetLastErrorMessage = unsafe extern "system" fn(error_message: *mut *mut u16) -> i32;

// =============================================================================
// 全局状态
// =============================================================================

static DISM_INITIALIZED: AtomicBool = AtomicBool::new(false);

// =============================================================================
// DISM API 封装
// =============================================================================

/// DISM API 管理器
///
/// 封装了 Windows DISM API 的主要功能，提供驱动注入和更新包安装等操作。
pub struct DismApi {
    _lib: Library,
    initialize: FnDismInitialize,
    shutdown: FnDismShutdown,
    open_session: FnDismOpenSession,
    close_session: FnDismCloseSession,
    add_driver: FnDismAddDriver,
    add_package: FnDismAddPackage,
    get_packages: Option<FnDismGetPackages>,
    get_drivers: Option<FnDismGetDrivers>,
    delete: FnDismDelete,
    get_last_error_message: Option<FnDismGetLastErrorMessage>,
}

impl DismApi {
    /// 创建 DISM API 实例
    ///
    /// 加载 dismapi.dll 并初始化 DISM 环境。
    /// 如果 DISM API 不可用，返回错误。
    pub fn new() -> Result<Self> {
        Self::with_log_level(DismLogLevel::Errors, None)
    }

    /// 创建带日志选项的 DISM API 实例
    pub fn with_log_level(log_level: DismLogLevel, log_file: Option<&str>) -> Result<Self> {
        // 尝试加载 dismapi.dll
        let lib = Self::load_dismapi_dll()?;

        // 获取函数指针
        let api = unsafe {
            let initialize: FnDismInitialize = *lib.get(b"DismInitialize")?;
            let shutdown: FnDismShutdown = *lib.get(b"DismShutdown")?;
            let open_session: FnDismOpenSession = *lib.get(b"DismOpenSession")?;
            let close_session: FnDismCloseSession = *lib.get(b"DismCloseSession")?;
            let add_driver: FnDismAddDriver = *lib.get(b"DismAddDriver")?;
            let add_package: FnDismAddPackage = *lib.get(b"DismAddPackage")?;
            let delete: FnDismDelete = *lib.get(b"DismDelete")?;

            // 可选函数
            let get_packages = lib.get(b"DismGetPackages").ok().map(|f| *f);
            let get_drivers = lib.get(b"DismGetDrivers").ok().map(|f| *f);
            let get_last_error_message = lib.get(b"DismGetLastErrorMessage").ok().map(|f| *f);

            DismApi {
                _lib: lib,
                initialize,
                shutdown,
                open_session,
                close_session,
                add_driver,
                add_package,
                get_packages,
                get_drivers,
                delete,
                get_last_error_message,
            }
        };

        // 初始化 DISM
        if !DISM_INITIALIZED.swap(true, Ordering::SeqCst) {
            let log_file_wide = log_file.map(|s| to_wide(s));
            let log_file_ptr = log_file_wide
                .as_ref()
                .map(|v| v.as_ptr())
                .unwrap_or(null_mut());

            let scratch_dir = Self::get_scratch_directory();
            let scratch_wide = to_wide(&scratch_dir);

            let hr = unsafe { (api.initialize)(log_level as u32, log_file_ptr, scratch_wide.as_ptr()) };

            if hr != 0 {
                DISM_INITIALIZED.store(false, Ordering::SeqCst);
                bail!("DISM 初始化失败: HRESULT = 0x{:08X}", hr as u32);
            }

            log::info!("[DISMAPI] DISM 初始化成功");
        }

        Ok(api)
    }

    /// 加载 dismapi.dll
    fn load_dismapi_dll() -> Result<Library> {
        // 尝试多个可能的路径
        let paths = [
            "dismapi.dll",
            r"X:\Windows\System32\dismapi.dll",
            r"C:\Windows\System32\dismapi.dll",
        ];

        for path in &paths {
            if let Ok(lib) = unsafe { Library::new(path) } {
                log::info!("[DISMAPI] 成功加载: {}", path);
                return Ok(lib);
            }
        }

        bail!("无法加载 dismapi.dll - DISM API 不可用");
    }

    /// 获取临时目录
    fn get_scratch_directory() -> String {
        // 优先使用 PE 环境的临时目录
        let candidates: [&str; 3] = [
            r"X:\Windows\TEMP",
            r"X:\TEMP",
            "",  // 占位符，后续特殊处理
        ];

        for (i, dir) in candidates.iter().enumerate() {
            // 最后一个是系统临时目录
            let dir_str = if i == 2 {
                std::env::temp_dir().to_string_lossy().to_string()
            } else {
                dir.to_string()
            };
            
            let path = Path::new(&dir_str);
            if path.exists() || std::fs::create_dir_all(path).is_ok() {
                return dir_str;
            }
        }

        // 默认返回临时目录
        std::env::temp_dir().to_string_lossy().to_string()
    }

    /// 打开离线镜像会话
    pub fn open_offline_session(&self, image_path: &str) -> Result<DismSession> {
        let image_wide = to_wide(image_path);
        let mut session: DismSession = 0;

        let hr = unsafe {
            (self.open_session)(
                image_wide.as_ptr(),
                null_mut(),
                null_mut(),
                &mut session,
            )
        };

        if hr != 0 {
            bail!(
                "打开离线镜像会话失败: {} - HRESULT = 0x{:08X}",
                image_path,
                hr as u32
            );
        }

        log::info!("[DISMAPI] 打开会话: {} (Session: {})", image_path, session);
        Ok(session)
    }

    /// 关闭会话
    pub fn close_session(&self, session: DismSession) -> Result<()> {
        let hr = unsafe { (self.close_session)(session) };

        if hr != 0 {
            log::warn!("[DISMAPI] 关闭会话失败: HRESULT = 0x{:08X}", hr as u32);
        } else {
            log::info!("[DISMAPI] 关闭会话: {}", session);
        }

        Ok(())
    }

    /// 添加驱动到离线镜像
    ///
    /// # 参数
    /// - `session`: DISM 会话句柄
    /// - `driver_path`: 驱动 INF 文件或目录路径
    /// - `force_unsigned`: 是否强制安装未签名驱动
    pub fn add_driver(
        &self,
        session: DismSession,
        driver_path: &str,
        force_unsigned: bool,
    ) -> Result<()> {
        let driver_wide = to_wide(driver_path);
        let force = if force_unsigned { 1 } else { 0 };

        log::info!("[DISMAPI] 添加驱动: {}", driver_path);

        let hr = unsafe { (self.add_driver)(session, driver_wide.as_ptr(), force) };

        if hr != 0 {
            let error_msg = self.get_last_error();
            bail!(
                "添加驱动失败: {} - HRESULT = 0x{:08X} - {}",
                driver_path,
                hr as u32,
                error_msg.unwrap_or_default()
            );
        }

        log::info!("[DISMAPI] 驱动添加成功: {}", driver_path);
        Ok(())
    }

    /// 添加更新包到离线镜像
    ///
    /// # 参数
    /// - `session`: DISM 会话句柄
    /// - `package_path`: CAB 或 MSU 包路径
    /// - `ignore_check`: 忽略适用性检查
    /// - `prevent_pending`: 阻止挂起操作
    pub fn add_package(
        &self,
        session: DismSession,
        package_path: &str,
        ignore_check: bool,
        prevent_pending: bool,
    ) -> Result<()> {
        let package_wide = to_wide(package_path);

        log::info!("[DISMAPI] 添加更新包: {}", package_path);

        let hr = unsafe {
            (self.add_package)(
                session,
                package_wide.as_ptr(),
                if ignore_check { 1 } else { 0 },
                if prevent_pending { 1 } else { 0 },
                None,
                null_mut(),
            )
        };

        if hr != 0 {
            let error_msg = self.get_last_error();
            bail!(
                "添加更新包失败: {} - HRESULT = 0x{:08X} - {}",
                package_path,
                hr as u32,
                error_msg.unwrap_or_default()
            );
        }

        log::info!("[DISMAPI] 更新包添加成功: {}", package_path);
        Ok(())
    }

    /// 获取最后的错误消息
    fn get_last_error(&self) -> Option<String> {
        let get_error = self.get_last_error_message?;
        
        let mut error_ptr: *mut u16 = null_mut();
        let hr = unsafe { (get_error)(&mut error_ptr) };

        if hr == 0 && !error_ptr.is_null() {
            let error_str = wide_to_string(error_ptr);
            unsafe { (self.delete)(error_ptr as *mut c_void) };
            Some(error_str)
        } else {
            None
        }
    }

    /// 检查 DISM API 是否可用
    pub fn is_available() -> bool {
        Self::load_dismapi_dll().is_ok()
    }
}

impl Drop for DismApi {
    fn drop(&mut self) {
        // 不在 Drop 中调用 DismShutdown，因为可能有多个实例
        // 应该由调用者在应用退出时显式调用 shutdown
    }
}

/// 显式关闭 DISM
///
/// 应在应用退出前调用此函数释放 DISM 资源。
pub fn shutdown_dism() {
    if DISM_INITIALIZED.swap(false, Ordering::SeqCst) {
        // DISM 关闭逻辑已在 Drop 中处理
        log::info!("[DISMAPI] DISM 已关闭");
    }
}

// =============================================================================
// 便捷函数
// =============================================================================

/// 添加驱动到离线镜像（便捷函数）
///
/// # 参数
/// - `image_path`: 离线镜像根目录 (如 "D:\\")
/// - `driver_path`: 驱动路径 (INF 文件或目录)
///
/// # 注意
/// 建议使用 dism_exe.rs 中的命令行方式，兼容性更好。
pub fn add_driver_offline(image_path: &str, driver_path: &str) -> Result<()> {
    let api = DismApi::new().context("DISM API 不可用，请使用命令行方式")?;
    
    let session = api.open_offline_session(image_path)?;
    let result = api.add_driver(session, driver_path, false);
    let _ = api.close_session(session);
    
    result
}

/// 添加更新包到离线镜像（便捷函数）
///
/// # 参数
/// - `image_path`: 离线镜像根目录 (如 "D:\\")
/// - `package_path`: 更新包路径 (CAB 或 MSU)
///
/// # 注意
/// 建议使用 dism_exe.rs 中的命令行方式，兼容性更好。
pub fn add_package_offline(image_path: &str, package_path: &str) -> Result<()> {
    let api = DismApi::new().context("DISM API 不可用，请使用命令行方式")?;
    
    let session = api.open_offline_session(image_path)?;
    let result = api.add_package(session, package_path, false, false);
    let _ = api.close_session(session);
    
    result
}

/// 安装更新包到离线系统（便捷函数）
///
/// 这是 `add_package_offline` 的别名，用于兼容旧代码。
/// 如果 DISM API 不可用，会自动使用 dism.exe 命令行方式。
///
/// # 参数
/// - `image_path`: 离线镜像根目录 (如 "D:\\")
/// - `cab_path`: CAB 更新包路径
/// - `_progress_callback`: 进度回调（暂未实现）
pub fn install_update_package(
    image_path: &str, 
    cab_path: &std::path::Path, 
    _progress_callback: Option<()>
) -> Result<()> {
    let cab_str = cab_path.to_string_lossy();
    
    // 首先尝试 DISM API
    if let Ok(api) = DismApi::new() {
        let normalized_image = if image_path.ends_with('\\') {
            image_path.to_string()
        } else {
            format!("{}\\", image_path)
        };
        
        if let Ok(session) = api.open_offline_session(&normalized_image) {
            let result = api.add_package(session, &cab_str, true, false);
            let _ = api.close_session(session);
            
            if result.is_ok() {
                log::info!("[DISMAPI] 更新包安装成功 (API): {}", cab_str);
                return result;
            }
            
            log::warn!("[DISMAPI] DISM API 安装失败，尝试命令行方式");
        }
    }
    
    // 如果 DISM API 失败，使用 dism.exe 命令行方式
    log::info!("[DISMAPI] 使用 dism.exe 命令行方式安装更新包");
    
    use crate::core::dism_exe::DismExe;
    
    let dism_exe = DismExe::new().context("dism.exe 不可用")?;
    dism_exe.add_package_offline(image_path, &cab_str, true, None)?;
    
    log::info!("[DISMAPI] 更新包安装成功 (命令行): {}", cab_str);
    Ok(())
}

// =============================================================================
// 辅助函数
// =============================================================================

/// 将 Rust 字符串转换为以 NUL 结尾的 UTF-16 Vec
fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

/// 将 UTF-16 指针转换为 Rust 字符串
fn wide_to_string(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }

    unsafe {
        let mut len = 0;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(ptr, len);
        String::from_utf16_lossy(slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_wide() {
        let wide = to_wide("test");
        assert_eq!(wide, vec!['t' as u16, 'e' as u16, 's' as u16, 't' as u16, 0]);
    }
}
