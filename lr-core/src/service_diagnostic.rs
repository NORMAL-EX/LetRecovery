//! Read-only SCM diagnostics for explaining supported Windows operations that fail after a
//! component-removal image deleted or disabled an operating-system service.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceAvailability {
    Present,
    Missing,
    Disabled,
    Unknown,
}

/// Inspect only service existence and configured start type. A stopped demand-start service is
/// normal and is reported as `Present`; callers must never turn that state into a preflight gate.
#[cfg(windows)]
pub fn query_service_availability(name: &str) -> ServiceAvailability {
    use std::mem::size_of;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        GetLastError, ERROR_INSUFFICIENT_BUFFER, ERROR_SERVICE_DOES_NOT_EXIST,
    };
    use windows::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceConfigW,
        QUERY_SERVICE_CONFIGW, SC_MANAGER_CONNECT, SERVICE_DISABLED, SERVICE_QUERY_CONFIG,
    };

    struct ServiceHandle(windows::Win32::System::Services::SC_HANDLE);
    impl Drop for ServiceHandle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseServiceHandle(self.0);
            }
        }
    }

    if name.is_empty() || name.encode_utf16().count() > 256 || name.contains(['\0', '\r', '\n']) {
        return ServiceAvailability::Unknown;
    }
    let name = name.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    unsafe {
        let manager = match OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) {
            Ok(handle) => ServiceHandle(handle),
            Err(_) => return ServiceAvailability::Unknown,
        };
        let service = match OpenServiceW(manager.0, PCWSTR(name.as_ptr()), SERVICE_QUERY_CONFIG) {
            Ok(handle) => ServiceHandle(handle),
            Err(_) if GetLastError() == ERROR_SERVICE_DOES_NOT_EXIST => {
                return ServiceAvailability::Missing;
            }
            Err(_) => return ServiceAvailability::Unknown,
        };

        let mut needed = 0_u32;
        if QueryServiceConfigW(service.0, None, 0, &mut needed).is_ok()
            || GetLastError() != ERROR_INSUFFICIENT_BUFFER
            || needed < size_of::<QUERY_SERVICE_CONFIGW>() as u32
        {
            return ServiceAvailability::Unknown;
        }
        let mut storage = vec![0_usize; (needed as usize).div_ceil(size_of::<usize>())];
        let config = storage.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>();
        if QueryServiceConfigW(service.0, Some(config), needed, &mut needed).is_err() {
            return ServiceAvailability::Unknown;
        }
        if (*config).dwStartType == SERVICE_DISABLED {
            ServiceAvailability::Disabled
        } else {
            ServiceAvailability::Present
        }
    }
}

#[cfg(not(windows))]
pub fn query_service_availability(_name: &str) -> ServiceAvailability {
    ServiceAvailability::Unknown
}

/// Add a plain-language defragsvc hint only after the real VDS Shrink operation returned the
/// documented ERROR_NOT_SUPPORTED HRESULT. The diagnostic never replaces the actual error.
pub fn explain_shrink_not_supported(
    hresult: i32,
    availability: ServiceAvailability,
) -> Option<String> {
    const HRESULT_FROM_WIN32_ERROR_NOT_SUPPORTED: i32 = 0x8007_0032_u32 as i32;
    if hresult != HRESULT_FROM_WIN32_ERROR_NOT_SUPPORTED {
        return None;
    }
    let detail = match availability {
        ServiceAvailability::Missing => {
            "系统中未找到“优化驱动器”(defragsvc)服务，可能被精简系统删除"
        }
        ServiceAvailability::Disabled => "“优化驱动器”(defragsvc)服务已被禁用",
        ServiceAvailability::Present | ServiceAvailability::Unknown => {
            "请检查“优化驱动器”(defragsvc)服务是否被精简或禁用"
        }
    };
    Some(format!(
        "Windows 无法执行缩卷（HRESULT 0x80070032 / ERROR_NOT_SUPPORTED）；{detail}。"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_error_not_supported_gets_the_defragsvc_hint() {
        assert!(explain_shrink_not_supported(
            0x8007_0032_u32 as i32,
            ServiceAvailability::Disabled
        )
        .unwrap()
        .contains("已被禁用"));
        assert!(explain_shrink_not_supported(-1, ServiceAvailability::Missing).is_none());
    }

    #[test]
    fn a_stopped_but_enabled_service_is_not_classified_as_damage() {
        let text =
            explain_shrink_not_supported(0x8007_0032_u32 as i32, ServiceAvailability::Present)
                .unwrap();
        assert!(text.contains("请检查"));
        assert!(!text.contains("未启动"));
    }
}
