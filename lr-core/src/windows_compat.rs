//! Runtime-selected Windows API compatibility shims.
//!
//! Newer Windows releases keep using their native per-monitor DPI and firmware APIs. Windows 7
//! reaches only the documented older equivalents, which prevents missing imports from blocking the
//! process loader without weakening the Windows 10/11 path.

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::sync::OnceLock;

    use libloading::Library;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, ReleaseDC, LOGPIXELSX};
    use windows::Win32::System::WindowsProgramming::GetFirmwareEnvironmentVariableW;
    use windows::Win32::UI::WindowsAndMessaging::SetProcessDPIAware;

    type GetDpiForWindowFn = unsafe extern "system" fn(HWND) -> u32;
    type GetDpiForSystemFn = unsafe extern "system" fn() -> u32;
    type SetProcessDpiAwarenessContextFn = unsafe extern "system" fn(isize) -> i32;
    type GetFirmwareEnvironmentVariableExWFn =
        unsafe extern "system" fn(PCWSTR, PCWSTR, *mut c_void, u32, *mut u32) -> u32;
    static GET_DPI_FOR_WINDOW: OnceLock<Option<GetDpiForWindowFn>> = OnceLock::new();
    static GET_DPI_FOR_SYSTEM: OnceLock<Option<GetDpiForSystemFn>> = OnceLock::new();
    static SET_PROCESS_DPI_AWARENESS_CONTEXT: OnceLock<Option<SetProcessDpiAwarenessContextFn>> =
        OnceLock::new();
    static GET_FIRMWARE_ENVIRONMENT_VARIABLE_EX_W: OnceLock<
        Option<GetFirmwareEnvironmentVariableExWFn>,
    > = OnceLock::new();
    fn resolve<T: Copy>(library: &str, symbol: &[u8]) -> Option<T> {
        let library = unsafe { Library::new(library) }.ok()?;
        let function = unsafe { library.get::<T>(symbol) }.ok()?;
        let function = *function;
        // Keep the module loaded for the lifetime of the cached function pointer. This matters for
        // combase.dll on old systems where the process may not otherwise hold a module reference.
        std::mem::forget(library);
        Some(function)
    }

    // The dedicated Rust Win7 target deliberately supplies these three WinRT/COM symbols locally.
    // That removes loader-time dependencies which Windows 7 cannot satisfy. The normal Windows
    // target keeps windows-core's original imports unchanged, while the Win7 artifact forwards to
    // the real implementations whenever it later runs on Windows 8/10/11.
    #[cfg(target_vendor = "win7")]
    mod winrt_compat {
        use super::{c_void, resolve, OnceLock, PCWSTR};
        use windows::core::{GUID, HRESULT};
        use windows::Win32::Foundation::BOOL;

        type RoGetActivationFactoryFn =
            unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT;
        type CoIncrementMtaUsageFn = unsafe extern "system" fn(*mut *mut c_void) -> HRESULT;
        type RoOriginateErrorWFn = unsafe extern "system" fn(HRESULT, u32, PCWSTR) -> BOOL;

        static RO_GET_ACTIVATION_FACTORY: OnceLock<Option<RoGetActivationFactoryFn>> =
            OnceLock::new();
        static CO_INCREMENT_MTA_USAGE: OnceLock<Option<CoIncrementMtaUsageFn>> = OnceLock::new();
        static RO_ORIGINATE_ERROR_W: OnceLock<Option<RoOriginateErrorWFn>> = OnceLock::new();

        const E_NOTIMPL: HRESULT = HRESULT(0x8000_4001_u32 as i32);

        #[no_mangle]
        #[allow(non_snake_case)]
        pub unsafe extern "system" fn RoGetActivationFactory(
            activatable_class_id: *mut c_void,
            iid: *const GUID,
            factory: *mut *mut c_void,
        ) -> HRESULT {
            let function = RO_GET_ACTIVATION_FACTORY
                .get_or_init(|| resolve("combase.dll", b"RoGetActivationFactory\0"));
            if let Some(function) = function {
                return function(activatable_class_id, iid, factory);
            }
            if !factory.is_null() {
                *factory = std::ptr::null_mut();
            }
            E_NOTIMPL
        }

        #[no_mangle]
        #[allow(non_snake_case)]
        pub unsafe extern "system" fn CoIncrementMTAUsage(cookie: *mut *mut c_void) -> HRESULT {
            let function = CO_INCREMENT_MTA_USAGE
                .get_or_init(|| resolve("ole32.dll", b"CoIncrementMTAUsage\0"));
            if let Some(function) = function {
                return function(cookie);
            }
            if !cookie.is_null() {
                *cookie = std::ptr::null_mut();
            }
            E_NOTIMPL
        }

        #[no_mangle]
        #[allow(non_snake_case)]
        pub unsafe extern "system" fn RoOriginateErrorW(
            error: HRESULT,
            maximum_characters: u32,
            message: PCWSTR,
        ) -> BOOL {
            let function =
                RO_ORIGINATE_ERROR_W.get_or_init(|| resolve("combase.dll", b"RoOriginateErrorW\0"));
            function.map_or(BOOL(0), |function| {
                function(error, maximum_characters, message)
            })
        }
    }

    fn device_dpi(hwnd: HWND) -> u32 {
        let dc = unsafe { GetDC(hwnd) };
        if dc.is_invalid() {
            return 96;
        }
        let dpi = unsafe { GetDeviceCaps(dc, LOGPIXELSX) };
        unsafe {
            let _ = ReleaseDC(hwnd, dc);
        }
        u32::try_from(dpi)
            .ok()
            .filter(|value| *value != 0)
            .unwrap_or(96)
    }

    /// Returns the native per-window DPI on Windows 10 1607+, with the documented GDI DPI fallback
    /// used only when that API is absent.
    pub fn dpi_for_window(hwnd: HWND) -> u32 {
        let function =
            GET_DPI_FOR_WINDOW.get_or_init(|| resolve("user32.dll", b"GetDpiForWindow\0"));
        function
            .map(|function| unsafe { function(hwnd) })
            .filter(|dpi| *dpi != 0)
            .unwrap_or_else(|| device_dpi(hwnd))
    }

    /// Returns the native system DPI where available and otherwise queries the desktop DC.
    pub fn dpi_for_system() -> u32 {
        let function =
            GET_DPI_FOR_SYSTEM.get_or_init(|| resolve("user32.dll", b"GetDpiForSystem\0"));
        function
            .map(|function| unsafe { function() })
            .filter(|dpi| *dpi != 0)
            .unwrap_or_else(|| device_dpi(HWND::default()))
    }

    /// Enables per-monitor-v2 awareness on supported Windows 10/11 systems. Windows 7 falls back
    /// to its system-DPI-aware API instead of importing a missing modern procedure.
    pub fn enable_best_process_dpi_awareness() -> bool {
        let function = SET_PROCESS_DPI_AWARENESS_CONTEXT
            .get_or_init(|| resolve("user32.dll", b"SetProcessDpiAwarenessContext\0"));
        if let Some(function) = function {
            // DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2 is the documented pseudo-handle -4.
            return unsafe { function(-4) != 0 };
        }
        unsafe { SetProcessDPIAware().as_bool() }
    }

    /// Reads a UEFI variable with attributes on Windows 8+, and uses the value-only Windows 7 API
    /// when attributes are unavailable. Callers must treat attributes as optional.
    ///
    /// # Safety
    ///
    /// `name` and `guid` must point to valid NUL-terminated UTF-16 strings, and `buffer`
    /// must be writable for `size` bytes. These are the same requirements as
    /// `GetFirmwareEnvironmentVariableW` and `GetFirmwareEnvironmentVariableExW`.
    pub unsafe fn get_firmware_environment_variable(
        name: PCWSTR,
        guid: PCWSTR,
        buffer: *mut c_void,
        size: u32,
        attributes: Option<&mut u32>,
    ) -> u32 {
        let function = GET_FIRMWARE_ENVIRONMENT_VARIABLE_EX_W
            .get_or_init(|| resolve("kernel32.dll", b"GetFirmwareEnvironmentVariableExW\0"));
        if let Some(function) = function {
            return function(
                name,
                guid,
                buffer,
                size,
                attributes.map_or(std::ptr::null_mut(), |value| value),
            );
        }
        if let Some(attributes) = attributes {
            *attributes = 0;
        }
        GetFirmwareEnvironmentVariableW(name, guid, Some(buffer), size)
    }
}

#[cfg(windows)]
pub use imp::{
    dpi_for_system, dpi_for_window, enable_best_process_dpi_awareness,
    get_firmware_environment_variable,
};
