//! Windows 驱动管理模块
//!
//! 使用 Windows API 实现驱动的导出和导入功能：
//! - SetupAPI (setupapi.dll) - 驱动安装和枚举
//! - NewDev API (newdev.dll) - 驱动安装
//! - CfgMgr32 (cfgmgr32.dll) - 设备配置管理
//!
//! 离线驱动服务统一交给 Windows DISM；不再手工拼装 DriverStore 或离线注册表。

use std::ffi::{c_void, OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr::null_mut;

use anyhow::{bail, Context, Result};
use libloading::Library;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

#[cfg(windows)]
use windows::{
    core::GUID,
    Win32::Foundation::{GetLastError, BOOL, HWND},
    Win32::System::SystemInformation::{GetVersionExW, OSVERSIONINFOEXW},
};

// ============================================================================
// 常量定义
// ============================================================================

// SetupAPI 常量
const DIGCF_PRESENT: u32 = 0x0000_0002;
const DIGCF_ALLCLASSES: u32 = 0x0000_0004;

const SPDRP_HARDWAREID: u32 = 0x0000_0001;
const SPDRP_DEVICEDESC: u32 = 0x0000_0000;
const SPDRP_MFG: u32 = 0x0000_000B;
const SPDRP_CLASS: u32 = 0x0000_0007;
const SPDRP_CLASSGUID: u32 = 0x0000_0008;

const ERROR_NO_MORE_ITEMS: u32 = 259;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_INVALID_DATA: u32 = 13;
const ERROR_NOT_FOUND: u32 = 1168;
// `DiInstallDriverW` accepts zero or DIIRFLAG_FORCE_INF. These are not the similarly named
// INSTALLFLAG_* values used by older NewDev APIs.
const DIIRFLAG_FORCE_INF: u32 = 0x0000_0002;

const REG_SZ: u32 = 1;
const REG_MULTI_SZ: u32 = 7;
const DEVPROP_TYPE_STRING: u32 = 0x0000_0012;

#[repr(C)]
#[derive(Clone, Copy)]
struct DevPropKey {
    fmtid: GUID,
    pid: u32,
}

const DEVPKEY_DEVICE_DRIVER_INF_PATH: DevPropKey = DevPropKey {
    fmtid: GUID::from_u128(0xa8b865dd_2e3d_4094_ad97_e593a70c75d6),
    pid: 5,
};

// ============================================================================
// 类型定义
// ============================================================================

type HDevInfo = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct SpDevInfoData {
    cb_size: u32,
    class_guid: [u8; 16],
    dev_inst: u32,
    reserved: usize,
}

impl Default for SpDevInfoData {
    fn default() -> Self {
        Self {
            cb_size: std::mem::size_of::<Self>() as u32,
            class_guid: [0; 16],
            dev_inst: 0,
            reserved: 0,
        }
    }
}

// ============================================================================
// 函数指针类型
// ============================================================================

// SetupAPI
type FnSetupDiGetClassDevsW = unsafe extern "system" fn(
    class_guid: *const u8,
    enumerator: *const u16,
    hwnd_parent: HWND,
    flags: u32,
) -> HDevInfo;

type FnSetupDiEnumDeviceInfo = unsafe extern "system" fn(
    dev_info: HDevInfo,
    member_index: u32,
    device_info_data: *mut SpDevInfoData,
) -> BOOL;

type FnSetupDiGetDeviceRegistryPropertyW = unsafe extern "system" fn(
    dev_info: HDevInfo,
    device_info_data: *const SpDevInfoData,
    property: u32,
    property_reg_data_type: *mut u32,
    property_buffer: *mut u8,
    property_buffer_size: u32,
    required_size: *mut u32,
) -> BOOL;

type FnSetupDiGetDevicePropertyW = unsafe extern "system" fn(
    dev_info: HDevInfo,
    device_info_data: *const SpDevInfoData,
    property_key: *const DevPropKey,
    property_type: *mut u32,
    property_buffer: *mut u8,
    property_buffer_size: u32,
    required_size: *mut u32,
    flags: u32,
) -> BOOL;

type FnSetupDiDestroyDeviceInfoList = unsafe extern "system" fn(dev_info: HDevInfo) -> BOOL;

struct DeviceInfoSet {
    handle: HDevInfo,
    destroy: FnSetupDiDestroyDeviceInfoList,
}

impl Drop for DeviceInfoSet {
    fn drop(&mut self) {
        unsafe {
            let _ = (self.destroy)(self.handle);
        }
    }
}

type FnSetupCopyOEMInfW = unsafe extern "system" fn(
    source_inf_file_name: *const u16,
    oem_source_media_location: *const u16,
    oem_source_media_type: u32,
    copy_style: u32,
    destination_inf_file_name: *mut u16,
    destination_inf_file_name_size: u32,
    required_size: *mut u32,
    destination_inf_file_name_component: *mut *mut u16,
) -> BOOL;

type FnSetupGetInfDriverStoreLocationW = unsafe extern "system" fn(
    file_name: *const u16,
    alternate_platform_info: *const c_void,
    locale_name: *const u16,
    return_buffer: *mut u16,
    return_buffer_size: u32,
    required_size: *mut u32,
) -> BOOL;

// NewDev API
type FnDiInstallDriverW = unsafe extern "system" fn(
    hwnd_parent: HWND,
    inf_path: *const u16,
    flags: u32,
    need_reboot: *mut BOOL,
) -> BOOL;

// ============================================================================
// 辅助函数
// ============================================================================

/// 将 Rust 字符串转换为以 NUL 结尾的 UTF-16 Vec
fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

/// 将 Path 转换为以 NUL 结尾的 UTF-16 Vec
fn path_to_wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

/// 将 UTF-16 缓冲区转换为 Rust 字符串
fn wide_to_string(wide: &[u16]) -> String {
    let len = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    OsString::from_wide(&wide[..len])
        .to_string_lossy()
        .into_owned()
}

/// Allocates an aligned UTF-16 buffer for a SetupAPI byte count.
///
/// SetupAPI reports string-property sizes in bytes even though the payload is UTF-16.  A
/// `Vec<u8>` cannot be cast to `u16` safely because Rust does not promise two-byte alignment.
fn aligned_utf16_buffer(byte_count: u32, property_name: &str) -> Result<Vec<u16>> {
    if byte_count == 0 || !byte_count.is_multiple_of(2) {
        bail!("invalid UTF-16 byte size for {property_name}: {byte_count}");
    }
    Ok(vec![0u16; byte_count as usize / std::mem::size_of::<u16>()])
}

fn utf16_payload<'a>(buffer: &'a [u16], byte_count: u32, property_name: &str) -> Result<&'a [u16]> {
    if !byte_count.is_multiple_of(2) {
        bail!("odd UTF-16 byte size returned for {property_name}: {byte_count}");
    }
    let unit_count = byte_count as usize / std::mem::size_of::<u16>();
    if unit_count > buffer.len() {
        bail!(
            "oversized UTF-16 payload returned for {property_name}: {byte_count} bytes exceeds {}",
            std::mem::size_of_val(buffer)
        );
    }
    Ok(&buffer[..unit_count])
}

#[cfg(windows)]
fn current_windows_is_pre_vista() -> Result<bool> {
    let mut version = OSVERSIONINFOEXW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOEXW>() as u32,
        ..Default::default()
    };
    unsafe {
        GetVersionExW((&mut version as *mut OSVERSIONINFOEXW).cast())
            .context("GetVersionExW failed while selecting the driver installation API")?;
    }
    Ok(newdev_fallback_allowed_for_major(version.dwMajorVersion))
}

const fn newdev_fallback_allowed_for_major(major_version: u32) -> bool {
    major_version < 6
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

// ============================================================================
// 驱动信息结构
// ============================================================================

/// 驱动信息
#[derive(Debug, Clone)]
pub struct DriverInfo {
    /// 设备描述
    pub description: String,
    /// 制造商
    pub manufacturer: String,
    /// INF 文件路径
    pub inf_path: String,
    /// 硬件 ID
    pub hardware_id: String,
    /// 完整硬件 ID 列表（SetupAPI `REG_MULTI_SZ`，按系统排名顺序）
    pub hardware_ids: Vec<String>,
    /// 设备类别
    pub device_class: String,
    /// 类别 GUID
    pub class_guid: String,
    /// 是否为第三方驱动 (OEM)
    pub is_oem: bool,
}

pub const STORAGE_DRIVER_REQUIREMENTS_FILE: &str = "LetRecovery-storage-drivers.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageDriverRequirement {
    pub description: String,
    pub source_inf: String,
    pub hardware_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct StorageDriverRequirementsManifest {
    version: u32,
    requirements: Vec<StorageDriverRequirement>,
}

// ============================================================================
// SetupAPI 封装
// ============================================================================

/// SetupAPI 封装结构
struct SetupApi {
    _lib: Library,
    get_class_devs: FnSetupDiGetClassDevsW,
    enum_device_info: FnSetupDiEnumDeviceInfo,
    get_device_registry_property: FnSetupDiGetDeviceRegistryPropertyW,
    get_device_property: Option<FnSetupDiGetDevicePropertyW>,
    destroy_device_info_list: FnSetupDiDestroyDeviceInfoList,
    copy_oem_inf: FnSetupCopyOEMInfW,
    get_inf_driver_store_location: Option<FnSetupGetInfDriverStoreLocationW>,
}

impl SetupApi {
    fn new() -> Result<Self> {
        let lib = unsafe { Library::new("setupapi.dll") }.context("无法加载 setupapi.dll")?;

        unsafe {
            let get_class_devs: FnSetupDiGetClassDevsW = *lib.get(b"SetupDiGetClassDevsW")?;
            let enum_device_info: FnSetupDiEnumDeviceInfo = *lib.get(b"SetupDiEnumDeviceInfo")?;
            let get_device_registry_property: FnSetupDiGetDeviceRegistryPropertyW =
                *lib.get(b"SetupDiGetDeviceRegistryPropertyW")?;
            let get_device_property = lib
                .get::<FnSetupDiGetDevicePropertyW>(b"SetupDiGetDevicePropertyW")
                .ok()
                .map(|function| *function);
            let destroy_device_info_list: FnSetupDiDestroyDeviceInfoList =
                *lib.get(b"SetupDiDestroyDeviceInfoList")?;
            let copy_oem_inf: FnSetupCopyOEMInfW = *lib.get(b"SetupCopyOEMInfW")?;

            // Available on Vista and later. Keep it dynamic so the shared library can still load
            // in older maintenance environments, but OEM export fails closed without it.
            let get_inf_driver_store_location = lib
                .get::<FnSetupGetInfDriverStoreLocationW>(b"SetupGetInfDriverStoreLocationW")
                .ok()
                .map(|f| *f);

            Ok(Self {
                _lib: lib,
                get_class_devs,
                enum_device_info,
                get_device_registry_property,
                get_device_property,
                destroy_device_info_list,
                copy_oem_inf,
                get_inf_driver_store_location,
            })
        }
    }

    /// 获取设备属性（字符串）
    fn get_device_property_string(
        &self,
        dev_info: HDevInfo,
        dev_info_data: &SpDevInfoData,
        property: u32,
    ) -> Result<Option<String>> {
        self.get_device_property_strings(dev_info, dev_info_data, property)
            .map(|values| values.into_iter().next())
    }

    /// Retrieves every string from a SetupAPI `REG_SZ`/`REG_MULTI_SZ` property.
    fn get_device_property_strings(
        &self,
        dev_info: HDevInfo,
        dev_info_data: &SpDevInfoData,
        property: u32,
    ) -> Result<Vec<String>> {
        let mut required_size: u32 = 0;
        let mut reg_type: u32 = 0;
        let probe = unsafe {
            (self.get_device_registry_property)(
                dev_info,
                dev_info_data,
                property,
                &mut reg_type,
                null_mut(),
                0,
                &mut required_size,
            )
        };
        if probe.0 == 0 {
            let error = get_last_error();
            if error == ERROR_INVALID_DATA || error == ERROR_NOT_FOUND {
                return Ok(Vec::new());
            }
            if required_size == 0 || error != ERROR_INSUFFICIENT_BUFFER {
                bail!("SetupDiGetDeviceRegistryPropertyW probe failed: {error}");
            }
        }
        if !(2..=1024 * 1024).contains(&required_size) {
            bail!("invalid SetupAPI property size: {required_size}");
        }

        let mut buffer = aligned_utf16_buffer(required_size, "SetupAPI registry property")?;
        let result = unsafe {
            (self.get_device_registry_property)(
                dev_info,
                dev_info_data,
                property,
                &mut reg_type,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * std::mem::size_of::<u16>()) as u32,
                &mut required_size,
            )
        };
        if result.0 == 0 {
            bail!(
                "SetupDiGetDeviceRegistryPropertyW read failed: {}",
                get_last_error()
            );
        }
        if reg_type != REG_SZ && reg_type != REG_MULTI_SZ {
            bail!("unexpected SetupAPI registry property type: {reg_type}");
        }

        let wide_slice = utf16_payload(&buffer, required_size, "SetupAPI registry property")?;
        Ok(wide_slice
            .split(|value| *value == 0)
            .take_while(|value| !value.is_empty())
            .map(|value| OsString::from_wide(value).to_string_lossy().into_owned())
            .filter(|value| !value.trim().is_empty())
            .collect())
    }

    /// Returns the published INF name (for example `oem42.inf`) that installed a device.
    ///
    /// `SetupDiGetDeviceRegistryPropertyW` has no `SPDRP_INF_PATH` selector. In particular,
    /// selector `0x10` is `SPDRP_UI_NUMBER` (a DWORD), which made the old fallback silently
    /// enumerate zero OEM drivers. Vista and newer expose the actual value through
    /// `DEVPKEY_Device_DriverInfPath`.
    fn get_device_driver_inf_path(
        &self,
        dev_info: HDevInfo,
        dev_info_data: &SpDevInfoData,
    ) -> Result<Option<String>> {
        let get_device_property = self
            .get_device_property
            .context("SetupDiGetDevicePropertyW is unavailable")?;
        let mut required_size = 0u32;
        let mut property_type = 0u32;
        let probe = unsafe {
            get_device_property(
                dev_info,
                dev_info_data,
                &DEVPKEY_DEVICE_DRIVER_INF_PATH,
                &mut property_type,
                null_mut(),
                0,
                &mut required_size,
                0,
            )
        };
        if probe.0 == 0 {
            let error = get_last_error();
            if error == ERROR_NOT_FOUND || error == ERROR_INVALID_DATA {
                return Ok(None);
            }
            if required_size == 0 || error != ERROR_INSUFFICIENT_BUFFER {
                bail!("SetupDiGetDevicePropertyW INF-path probe failed: {error}");
            }
        }
        if !(2..=1024 * 1024).contains(&required_size) {
            bail!("invalid driver INF-path property size: {required_size}");
        }
        let mut buffer = aligned_utf16_buffer(required_size, "driver INF-path property")?;
        let result = unsafe {
            get_device_property(
                dev_info,
                dev_info_data,
                &DEVPKEY_DEVICE_DRIVER_INF_PATH,
                &mut property_type,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * std::mem::size_of::<u16>()) as u32,
                &mut required_size,
                0,
            )
        };
        if result.0 == 0 {
            bail!(
                "SetupDiGetDevicePropertyW INF-path read failed: {}",
                get_last_error()
            );
        }
        if property_type != DEVPROP_TYPE_STRING {
            bail!("unexpected driver INF-path property type: {property_type}");
        }
        let wide_slice = utf16_payload(&buffer, required_size, "driver INF-path property")?;
        let value = wide_to_string(wide_slice);
        Ok((!value.trim().is_empty()).then_some(value))
    }

    /// 枚举所有设备的驱动信息
    fn enumerate_drivers(&self) -> Result<Vec<DriverInfo>> {
        let mut drivers = Vec::new();

        // 获取所有设备
        let dev_info = unsafe {
            (self.get_class_devs)(
                null_mut(),
                null_mut(),
                HWND::default(),
                DIGCF_PRESENT | DIGCF_ALLCLASSES,
            )
        };

        if dev_info.is_null() || dev_info == (-1isize as *mut c_void) {
            bail!("SetupDiGetClassDevsW 失败: {}", get_last_error());
        }
        let dev_info_set = DeviceInfoSet {
            handle: dev_info,
            destroy: self.destroy_device_info_list,
        };

        // 枚举每个设备
        let mut index = 0u32;
        loop {
            let mut dev_info_data = SpDevInfoData::default();

            let result = unsafe { (self.enum_device_info)(dev_info, index, &mut dev_info_data) };

            if result.0 == 0 {
                let err = get_last_error();
                if err == ERROR_NO_MORE_ITEMS {
                    break;
                }
                bail!("SetupDiEnumDeviceInfo failed at index {index}: {err}");
            }

            // 获取安装该设备的已发布 INF 名称。
            if let Some(inf_path) = self.get_device_driver_inf_path(dev_info, &dev_info_data)? {
                // 检查是否为 OEM 驱动
                let is_oem = inf_path.to_lowercase().starts_with("oem");

                let description = self
                    .get_device_property_string(dev_info, &dev_info_data, SPDRP_DEVICEDESC)?
                    .unwrap_or_default();

                let manufacturer = self
                    .get_device_property_string(dev_info, &dev_info_data, SPDRP_MFG)?
                    .unwrap_or_default();

                let hardware_ids =
                    self.get_device_property_strings(dev_info, &dev_info_data, SPDRP_HARDWAREID)?;
                let hardware_id = hardware_ids.first().cloned().unwrap_or_default();

                let device_class = self
                    .get_device_property_string(dev_info, &dev_info_data, SPDRP_CLASS)?
                    .unwrap_or_default();

                let class_guid = self
                    .get_device_property_string(dev_info, &dev_info_data, SPDRP_CLASSGUID)?
                    .unwrap_or_default();

                drivers.push(DriverInfo {
                    description,
                    manufacturer,
                    inf_path,
                    hardware_id,
                    hardware_ids,
                    device_class,
                    class_guid,
                    is_oem,
                });
            }

            index += 1;
        }

        drop(dev_info_set);
        Ok(drivers)
    }

    /// Enumerates hardware IDs for every present device, including devices that do not yet have
    /// an INF bound in the running Windows or WinPE environment.
    fn enumerate_present_hardware_ids(&self) -> Result<Vec<String>> {
        let mut hardware_ids = Vec::new();
        let dev_info = unsafe {
            (self.get_class_devs)(
                null_mut(),
                null_mut(),
                HWND::default(),
                DIGCF_PRESENT | DIGCF_ALLCLASSES,
            )
        };
        if dev_info.is_null() || dev_info == (-1isize as *mut c_void) {
            bail!("SetupDiGetClassDevsW 失败: {}", get_last_error());
        }
        let dev_info_set = DeviceInfoSet {
            handle: dev_info,
            destroy: self.destroy_device_info_list,
        };

        let mut index = 0u32;
        loop {
            let mut dev_info_data = SpDevInfoData::default();
            let result = unsafe { (self.enum_device_info)(dev_info, index, &mut dev_info_data) };
            if result.0 == 0 {
                let error = get_last_error();
                if error == ERROR_NO_MORE_ITEMS {
                    break;
                }
                bail!("SetupDiEnumDeviceInfo failed at index {index}: {error}");
            }

            for hardware_id in
                self.get_device_property_strings(dev_info, &dev_info_data, SPDRP_HARDWAREID)?
            {
                if !hardware_id.trim().is_empty() {
                    hardware_ids.push(hardware_id);
                }
            }
            index += 1;
        }

        drop(dev_info_set);
        Ok(hardware_ids)
    }

    /// 安装 INF 驱动文件到驱动存储
    fn install_inf(&self, inf_path: &Path) -> Result<String> {
        let wide_path = path_to_wide(inf_path);
        let mut dest_buffer = vec![0u16; 260];
        let mut required_size: u32 = 0;

        // SPOST_PATH = 1 表示从路径复制
        let result = unsafe {
            (self.copy_oem_inf)(
                wide_path.as_ptr(),
                null_mut(), // OEM source media location
                1,          // SPOST_PATH
                0,          // copy style
                dest_buffer.as_mut_ptr(),
                dest_buffer.len() as u32,
                &mut required_size,
                null_mut(),
            )
        };

        if result.0 == 0 {
            let err = get_last_error();
            bail!("SetupCopyOEMInf 失败: 错误码 {}", err);
        }

        Ok(wide_to_string(&dest_buffer))
    }

    /// 获取 INF 文件在驱动存储中的完整路径
    fn get_driver_store_path(&self, inf_name: &str) -> Option<PathBuf> {
        let func = self.get_inf_driver_store_location?;

        let wide_name = to_wide(inf_name);
        let mut required_size: u32 = 0;
        let probe = unsafe {
            func(
                wide_name.as_ptr(),
                null_mut(),
                null_mut(),
                null_mut(),
                0,
                &mut required_size,
            )
        };
        if probe.0 == 0 && (required_size == 0 || get_last_error() != ERROR_INSUFFICIENT_BUFFER) {
            return None;
        }
        if !(2..=32_768).contains(&required_size) {
            return None;
        }
        let mut buffer = vec![0u16; required_size as usize];
        let result = unsafe {
            func(
                wide_name.as_ptr(),
                null_mut(),
                null_mut(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                &mut required_size,
            )
        };
        (result.0 != 0).then(|| PathBuf::from(wide_to_string(&buffer)))
    }
}

// ============================================================================
// NewDev API 封装
// ============================================================================

/// NewDev API 封装
struct NewDevApi {
    _lib: Library,
    di_install_driver: FnDiInstallDriverW,
}

impl NewDevApi {
    fn new() -> Result<Self> {
        let lib = unsafe { Library::new("newdev.dll") }.context("无法加载 newdev.dll")?;

        unsafe {
            let di_install_driver: FnDiInstallDriverW = *lib.get(b"DiInstallDriverW")?;

            Ok(Self {
                _lib: lib,
                di_install_driver,
            })
        }
    }

    /// 安装驱动
    fn install_driver(&self, inf_path: &Path, force: bool) -> Result<bool> {
        let wide_path = path_to_wide(inf_path);
        let mut need_reboot = BOOL::default();

        let flags = if force { DIIRFLAG_FORCE_INF } else { 0 };

        let result = unsafe {
            (self.di_install_driver)(HWND::default(), wide_path.as_ptr(), flags, &mut need_reboot)
        };

        if result.0 == 0 {
            let err = get_last_error();
            bail!("DiInstallDriverW 失败: 错误码 {}", err);
        }

        Ok(need_reboot.0 != 0)
    }
}

// ============================================================================
// 驱动管理器
// ============================================================================

/// 驱动管理器
/// 提供驱动导出和导入的高级接口
pub struct DriverManager {
    setup_api: SetupApi,
    newdev_api: Option<NewDevApi>,
}

impl DriverManager {
    /// 创建驱动管理器实例
    pub fn new() -> Result<Self> {
        let setup_api = SetupApi::new()?;
        let newdev_api = match NewDevApi::new() {
            Ok(api) => Some(api),
            Err(error) if current_windows_is_pre_vista()? => {
                log::warn!(
                    "[DriverManager] DiInstallDriverW is unavailable on this pre-Vista system; using SetupCopyOEMInfW staging compatibility: {error}"
                );
                None
            }
            Err(error) => {
                return Err(error.context(
                    "DiInstallDriverW is required on Windows Vista and newer; refusing to report package staging as driver installation",
                ));
            }
        };

        Ok(Self {
            setup_api,
            newdev_api,
        })
    }

    /// 枚举系统中所有已安装的驱动
    pub fn enumerate_all_drivers(&self) -> Result<Vec<DriverInfo>> {
        self.setup_api.enumerate_drivers()
    }

    /// Enumerates present-device hardware IDs even when a device has no installed driver yet.
    pub fn enumerate_present_hardware_ids(&self) -> Result<Vec<String>> {
        self.setup_api.enumerate_present_hardware_ids()
    }

    /// 枚举第三方 (OEM) 驱动
    pub fn enumerate_oem_drivers(&self) -> Result<Vec<DriverInfo>> {
        let all_drivers = self.setup_api.enumerate_drivers()?;
        Ok(all_drivers.into_iter().filter(|d| d.is_oem).collect())
    }

    /// Returns every currently bound third-party boot-storage package that must survive a
    /// reinstall. Intel VMD selection is independently checked so a lone 09AB dummy function
    /// cannot be mistaken for a generation-defining controller.
    pub fn present_oem_storage_requirements(&self) -> Result<Vec<StorageDriverRequirement>> {
        let present_ids = self.setup_api.enumerate_present_hardware_ids()?;
        crate::storage_driver_match::select_builtin_storage_driver_packages(
            present_ids.iter().map(String::as_str),
        )
        .map_err(anyhow::Error::new)?;

        let mut requirements =
            std::collections::BTreeMap::<String, StorageDriverRequirement>::new();
        for driver in self.setup_api.enumerate_drivers()? {
            if !driver.is_oem || !is_boot_storage_driver(&driver) {
                continue;
            }
            let ids = driver
                .hardware_ids
                .iter()
                .filter(|id| !id.trim().is_empty())
                .cloned()
                .collect::<Vec<_>>();
            if ids.is_empty() {
                bail!(
                    "bound OEM storage driver has no hardware ID: {} ({})",
                    driver.description,
                    driver.inf_path
                );
            }
            let key = driver.inf_path.to_ascii_lowercase();
            let requirement = requirements
                .entry(key)
                .or_insert_with(|| StorageDriverRequirement {
                    description: driver.description.clone(),
                    source_inf: driver.inf_path.clone(),
                    hardware_ids: Vec::new(),
                });
            for hardware_id in ids {
                if !requirement
                    .hardware_ids
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(&hardware_id))
                {
                    requirement.hardware_ids.push(hardware_id);
                }
            }
        }
        Ok(requirements.into_values().collect())
    }

    /// 导出第三方驱动到指定目录
    ///
    /// # 参数
    /// - `destination`: 目标目录
    /// - `oem_only`: 是否只导出第三方驱动
    ///
    /// # 返回
    /// - 成功导出的驱动数量
    pub fn export_drivers(&self, destination: &Path, oem_only: bool) -> Result<usize> {
        std::fs::create_dir_all(destination)?;

        let drivers = if oem_only {
            self.enumerate_oem_drivers()?
        } else {
            self.enumerate_all_drivers()?
        };

        log::info!("[DriverManager] 找到 {} 个驱动需要导出", drivers.len());

        // 去重 INF 路径
        let mut exported_infs = std::collections::HashSet::new();
        let mut success_count = 0;
        let mut failures = Vec::new();

        for driver in &drivers {
            if exported_infs.contains(&driver.inf_path) {
                continue;
            }
            exported_infs.insert(driver.inf_path.clone());

            // 获取驱动存储中的完整路径
            let Some(driver_store_path) = self.setup_api.get_driver_store_path(&driver.inf_path)
            else {
                failures.push(format!(
                    "SetupGetInfDriverStoreLocationW could not resolve {} ({})",
                    driver.inf_path, driver.description
                ));
                continue;
            };

            if !driver_store_path.exists() {
                log::warn!(
                    "[DriverManager] 警告: 驱动文件不存在: {:?}",
                    driver_store_path
                );
                failures.push(format!(
                    "driver store package is unavailable: {}",
                    driver_store_path.display()
                ));
                continue;
            }

            // 创建目标子目录（使用 INF 名称去掉扩展名）
            let inf_stem = Path::new(&driver.inf_path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&driver.inf_path);

            let dest_dir = destination.join(inf_stem);
            std::fs::create_dir_all(&dest_dir)?;

            // 复制驱动包
            match self.copy_driver_package(&driver_store_path, &dest_dir) {
                Ok(_) => {
                    log::info!(
                        "[DriverManager] 已导出: {} -> {:?}",
                        driver.description,
                        dest_dir
                    );
                    success_count += 1;
                }
                Err(e) => {
                    log::error!("[DriverManager] 导出失败: {} - {}", driver.description, e);
                    failures.push(format!("{}: {e}", driver.description));
                }
            }
        }

        if !failures.is_empty() {
            bail!(
                "{} driver package(s) failed to export; first failure: {}",
                failures.len(),
                failures[0]
            );
        }
        log::info!("[DriverManager] 成功导出 {} 个驱动", success_count);
        Ok(success_count)
    }

    /// 从驱动存储路径复制整个驱动包
    fn copy_driver_package(&self, inf_path: &Path, dest_dir: &Path) -> Result<()> {
        // 驱动存储格式: C:\Windows\System32\DriverStore\FileRepository\xxx.inf_xxx\
        // 需要复制整个目录

        let parent_dir = inf_path.parent().context("无法获取父目录")?;

        // 如果 INF 在 FileRepository 中
        if parent_dir.to_string_lossy().contains("FileRepository") {
            // 复制整个目录
            Self::copy_dir_recursive(parent_dir, dest_dir)?;
        } else {
            // 只复制 INF 文件本身（来自 Windows\INF）
            let dest_inf = dest_dir.join(inf_path.file_name().context("无文件名")?);
            std::fs::copy(inf_path, &dest_inf)?;

            // 尝试查找并复制关联的 .sys 文件
            self.try_copy_associated_files(inf_path, dest_dir)?;
        }

        Ok(())
    }

    /// 递归复制目录
    fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
        std::fs::create_dir_all(dst)?;

        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let src_path = entry.path();
            let dst_path = dst.join(entry.file_name());

            if src_path.is_dir() {
                Self::copy_dir_recursive(&src_path, &dst_path)?;
            } else {
                std::fs::copy(&src_path, &dst_path)?;
            }
        }

        Ok(())
    }

    /// 尝试复制 INF 关联的文件（通过解析 INF 文件）
    fn try_copy_associated_files(&self, inf_path: &Path, dest_dir: &Path) -> Result<()> {
        let inf_content = std::fs::read_to_string(inf_path)?;
        let windows_dir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_string());
        let system32_drivers = PathBuf::from(&windows_dir).join("System32").join("drivers");

        // 简单解析 INF 文件查找 .sys 文件
        for line in inf_content.lines() {
            let line = line.trim();

            // 查找 CopyFiles 引用的文件
            if line.ends_with(".sys") || line.ends_with(".dll") || line.ends_with(".cat") {
                let file_name = line.split(',').next().unwrap_or(line).trim();

                // 尝试从 System32\drivers 复制
                let src_file = system32_drivers.join(file_name);
                if src_file.exists() {
                    let dst_file = dest_dir.join(file_name);
                    let _ = std::fs::copy(&src_file, &dst_file);
                }
            }
        }

        Ok(())
    }

    /// 导入驱动（从目录递归安装所有 INF）
    ///
    /// # 参数
    /// - `source_dir`: 驱动目录
    /// - `force`: 是否强制安装（覆盖已有驱动）
    ///
    /// # 返回
    /// - (成功数, 失败数, 是否需要重启)
    pub fn import_drivers(&self, source_dir: &Path, force: bool) -> Result<(usize, usize, bool)> {
        let mut success_count = 0;
        let mut fail_count = 0;
        let mut need_reboot = false;

        // 递归查找所有 INF 文件
        let inf_files = Self::find_inf_files(source_dir)?;
        log::info!("[DriverManager] 找到 {} 个 INF 文件", inf_files.len());

        for inf_path in inf_files {
            log::info!("[DriverManager] 正在安装: {:?}", inf_path);

            match self.install_single_driver(&inf_path, force) {
                Ok(reboot) => {
                    success_count += 1;
                    need_reboot = need_reboot || reboot;
                    log::info!("[DriverManager] 安装成功: {:?}", inf_path);
                }
                Err(e) => {
                    fail_count += 1;
                    log::error!("[DriverManager] 安装失败: {:?} - {}", inf_path, e);
                }
            }
        }

        log::info!(
            "[DriverManager] 驱动导入完成: 成功 {}, 失败 {}, 需要重启: {}",
            success_count,
            fail_count,
            need_reboot
        );

        Ok((success_count, fail_count, need_reboot))
    }

    /// 安装单个驱动
    fn install_single_driver(&self, inf_path: &Path, force: bool) -> Result<bool> {
        // Vista+ uses DiInstallDriver, which both stages the package and binds it when applicable.
        // Do not turn a real DiInstallDriver failure into a reported success by merely staging the
        // INF through SetupCopyOEMInf.
        if let Some(ref newdev) = self.newdev_api {
            return newdev.install_driver(inf_path, force);
        }

        // Pre-Vista compatibility: SetupCopyOEMInf is the supported package-staging API. It does
        // not claim that a present device was rebound, and callers receive `need_reboot = false`.
        self.setup_api.install_inf(inf_path)?;
        Ok(false)
    }

    /// 递归查找目录中的所有 INF 文件（非目录会返回 Err）。
    /// pub: 供正常系统端的 dism-first 封装做导入前计数（保持与基线一致的早失败语义）。
    pub fn find_inf_files(dir: &Path) -> Result<Vec<PathBuf>> {
        let mut inf_files = Vec::new();

        if !dir.is_dir() {
            bail!("{dir:?} 不是目录");
        }

        let metadata = dir
            .symlink_metadata()
            .with_context(|| format!("driver directory is unavailable: {}", dir.display()))?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            bail!(
                "driver source is not a regular directory: {}",
                dir.display()
            );
        }
        for entry in walkdir::WalkDir::new(dir).follow_links(false) {
            let entry = entry.with_context(|| {
                format!("failed to enumerate driver directory: {}", dir.display())
            })?;
            let path = entry.path();
            if entry.file_type().is_file() {
                if let Some(ext) = path.extension() {
                    if ext.eq_ignore_ascii_case("inf") {
                        inf_files.push(path.to_path_buf());
                    }
                }
            }
        }

        Ok(inf_files)
    }

    /// 使用 Windows DISM 服务栈向离线系统导入驱动。
    ///
    /// # 参数
    /// - `offline_root`: 离线系统根目录 (如 "D:\\")
    /// - `source_dir`: 驱动目录
    ///
    /// # 返回
    /// - (成功数, 失败数)
    pub fn import_drivers_offline(
        &self,
        offline_root: &Path,
        source_dir: &Path,
    ) -> Result<(usize, usize)> {
        let windows_directory = offline_root.join("Windows");
        if !windows_directory.is_dir() {
            bail!(
                "offline Windows directory is unavailable: {}",
                windows_directory.display()
            );
        }
        let source_metadata = source_dir
            .symlink_metadata()
            .with_context(|| format!("driver source is unavailable: {}", source_dir.display()))?;
        if !source_metadata.file_type().is_dir() || source_metadata.file_type().is_symlink() {
            bail!(
                "driver source is not a regular directory: {}",
                source_dir.display()
            );
        }
        let inf_files = Self::find_inf_files(source_dir)?;
        if inf_files.is_empty() {
            bail!(
                "driver source contains no INF files: {}",
                source_dir.display()
            );
        }

        let request = crate::command::CommandRequest::new("dism.exe")
            .arg(format!("/Image:{}", offline_root.display()))
            .arg("/Add-Driver")
            .arg(format!("/Driver:{}", source_dir.display()))
            .arg("/Recurse");
        let outcome =
            crate::command::execute_request(&crate::command::SystemCommandExecutor, &request)
                .context("failed to start DISM offline driver import")?;
        if outcome.succeeded() {
            log::info!(
                "[DriverManager] DISM imported {} offline driver packages",
                inf_files.len()
            );
            return Ok((inf_files.len(), 0));
        }

        log::warn!(
            "[DriverManager] recursive DISM import failed; retrying exact INF packages: exit {:?}, stdout={}, stderr={}",
            outcome.exit_code(),
            String::from_utf8_lossy(outcome.stdout()).trim(),
            String::from_utf8_lossy(outcome.stderr()).trim()
        );

        for inf in &inf_files {
            let normal_request = crate::command::CommandRequest::new("dism.exe")
                .arg(format!("/Image:{}", offline_root.display()))
                .arg("/Add-Driver")
                .arg(format!("/Driver:{}", inf.display()));
            let normal_outcome = crate::command::execute_request(
                &crate::command::SystemCommandExecutor,
                &normal_request,
            )
            .with_context(|| format!("failed to start DISM for {}", inf.display()))?;
            if normal_outcome.succeeded() {
                continue;
            }

            let normal_error = format!(
                "exit {:?}: stdout={} stderr={}",
                normal_outcome.exit_code(),
                String::from_utf8_lossy(normal_outcome.stdout()).trim(),
                String::from_utf8_lossy(normal_outcome.stderr()).trim()
            );
            if !crate::driver_package_trust::is_known_dism_signature_false_negative(&normal_error) {
                bail!(
                    "DISM rejected driver package {}: {}",
                    inf.display(),
                    normal_error
                );
            }

            let verified =
                crate::driver_package_trust::verify_driver_package(inf).with_context(|| {
                    format!(
                        "driver package is not independently trusted: {}",
                        inf.display()
                    )
                })?;
            verified.revalidate()?;
            log::warn!(
                "[DriverManager] signature false-negative fallback for {} (signer: {})",
                inf.display(),
                verified.signer()
            );
            let fallback_request = crate::command::CommandRequest::new("dism.exe")
                .arg(format!("/Image:{}", offline_root.display()))
                .arg("/Add-Driver")
                .arg(format!("/Driver:{}", inf.display()))
                .arg("/ForceUnsigned");
            let fallback_outcome = crate::command::execute_request(
                &crate::command::SystemCommandExecutor,
                &fallback_request,
            )
            .with_context(|| format!("failed to start verified fallback for {}", inf.display()))?;
            if !fallback_outcome.succeeded() {
                bail!(
                    "verified driver fallback failed for {} (exit {:?}): stdout={} stderr={}",
                    inf.display(),
                    fallback_outcome.exit_code(),
                    String::from_utf8_lossy(fallback_outcome.stdout()).trim(),
                    String::from_utf8_lossy(fallback_outcome.stderr()).trim()
                );
            }
        }

        Ok((inf_files.len(), 0))
    }

    /// 从在线系统导出驱动（用于 PE 环境下导出目标系统的驱动）
    ///
    /// # 参数
    /// - `system_root`: 系统根目录 (如 "C:\\")
    /// - `destination`: 目标目录
    ///
    /// # 返回
    /// - 成功导出的驱动数量
    pub fn export_drivers_from_system(
        &self,
        system_root: &Path,
        destination: &Path,
    ) -> Result<usize> {
        std::fs::create_dir_all(destination)?;

        let windows_directory = system_root.join("Windows");
        if !windows_directory.is_dir() {
            bail!(
                "offline Windows directory is unavailable: {}",
                windows_directory.display()
            );
        }
        let image_argument = format!("/Image:{}", system_root.display());
        let destination_argument = format!("/Destination:{}", destination.display());
        let request = crate::command::CommandRequest::new("dism.exe")
            .arg(image_argument)
            .arg("/Export-Driver")
            .arg(destination_argument);
        let outcome =
            crate::command::execute_request(&crate::command::SystemCommandExecutor, &request)
                .context("failed to start DISM offline driver export")?;
        if !outcome.succeeded() {
            bail!(
                "DISM offline driver export failed (exit {:?}): stdout={} stderr={}",
                outcome.exit_code(),
                String::from_utf8_lossy(outcome.stdout()).trim(),
                String::from_utf8_lossy(outcome.stderr()).trim()
            );
        }
        let count = WalkDir::new(destination)
            .follow_links(false)
            .into_iter()
            .filter_map(std::result::Result::ok)
            .filter(|entry| {
                entry.file_type().is_file()
                    && entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("inf"))
            })
            .count();
        if count == 0 {
            bail!("DISM offline driver export completed without any INF package");
        }
        log::info!("[DriverManager] DISM exported {count} offline driver packages");
        Ok(count)
    }
}

// ============================================================================
// 公共接口函数
// ============================================================================

/// 导出系统驱动到指定目录
///
/// # 参数
/// - `destination`: 目标目录
///
/// # 返回
/// - 成功导出的驱动数量
pub fn export_drivers(destination: &str) -> Result<usize> {
    let manager = DriverManager::new()?;
    manager.export_drivers(Path::new(destination), true)
}

/// Counts exported INF packages without following or accepting reparse points.
///
/// A zero result is meaningful: an online Windows installation can legitimately contain only
/// inbox drivers. Callers decide whether that verified empty set is a no-op (automatic preserve)
/// or a user-visible failure (an explicit "save drivers" request).
pub fn count_exported_driver_inf_files(driver_tree: &Path) -> Result<usize> {
    let root_metadata = driver_tree.symlink_metadata().with_context(|| {
        format!(
            "failed to inspect exported driver directory: {}",
            driver_tree.display()
        )
    })?;
    if !root_metadata.file_type().is_dir() || metadata_is_reparse_point(&root_metadata) {
        bail!(
            "exported driver root is not a plain directory: {}",
            driver_tree.display()
        );
    }

    let mut count = 0usize;
    for entry in WalkDir::new(driver_tree).follow_links(false) {
        let entry = entry.with_context(|| {
            format!(
                "failed to enumerate exported driver directory: {}",
                driver_tree.display()
            )
        })?;
        let metadata = entry.path().symlink_metadata().with_context(|| {
            format!(
                "failed to inspect exported driver entry: {}",
                entry.path().display()
            )
        })?;
        if metadata_is_reparse_point(&metadata) {
            bail!(
                "exported driver tree contains a reparse point: {}",
                entry.path().display()
            );
        }
        if metadata.file_type().is_file()
            && entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("inf"))
        {
            count = count.saturating_add(1);
        }
    }
    Ok(count)
}

#[cfg(windows)]
fn metadata_is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

/// 从指定系统分区导出驱动（PE环境下使用）
///
/// # 参数
/// - `system_partition`: 系统分区根目录 (如 "C:\\")
/// - `destination`: 目标目录
///
/// # 返回
/// - 成功导出的驱动数量
pub fn export_drivers_from_system(system_partition: &str, destination: &str) -> Result<usize> {
    let manager = DriverManager::new()?;
    manager.export_drivers_from_system(Path::new(system_partition), Path::new(destination))
}

/// 导入驱动
///
/// # 参数
/// - `driver_path`: 驱动目录
/// - `force`: 是否强制安装
///
/// # 返回
/// - (成功数, 失败数, 是否需要重启)
pub fn import_drivers(driver_path: &str, force: bool) -> Result<(usize, usize, bool)> {
    let manager = DriverManager::new()?;
    manager.import_drivers(Path::new(driver_path), force)
}

/// 导入驱动到离线系统（PE环境下使用）
///
/// # 参数
/// - `offline_root`: 离线系统根目录 (如 "D:\\")
/// - `driver_path`: 驱动目录
///
/// # 返回
/// - (成功数, 失败数)
pub fn import_drivers_offline(offline_root: &str, driver_path: &str) -> Result<(usize, usize)> {
    let manager = DriverManager::new()?;
    manager.import_drivers_offline(Path::new(offline_root), Path::new(driver_path))
}

/// 枚举所有 OEM 驱动
pub fn list_oem_drivers() -> Result<Vec<DriverInfo>> {
    let manager = DriverManager::new()?;
    manager.enumerate_oem_drivers()
}

/// 枚举所有驱动
pub fn list_all_drivers() -> Result<Vec<DriverInfo>> {
    let manager = DriverManager::new()?;
    manager.enumerate_all_drivers()
}

/// 枚举当前存在设备的硬件 ID，包括尚未绑定 INF 的设备。
pub fn list_present_hardware_ids() -> Result<Vec<String>> {
    let manager = DriverManager::new()?;
    manager.enumerate_present_hardware_ids()
}

/// Enumerates third-party packages currently bound to boot-storage controller classes.
pub fn list_present_oem_storage_driver_requirements() -> Result<Vec<StorageDriverRequirement>> {
    let manager = DriverManager::new()?;
    manager.present_oem_storage_requirements()
}

/// Verifies an exported tree and atomically records the exact storage coverage PE must preserve.
pub fn write_storage_driver_requirements(
    exported_root: &Path,
    requirements: &[StorageDriverRequirement],
) -> Result<()> {
    validate_storage_driver_requirements(exported_root, requirements)?;
    validate_requirement_values(requirements)?;
    let manifest = StorageDriverRequirementsManifest {
        version: 1,
        requirements: requirements.to_vec(),
    };
    let bytes = serde_json::to_vec_pretty(&manifest)?;
    let destination = exported_root.join(STORAGE_DRIVER_REQUIREMENTS_FILE);
    let temporary = crate::scoped_temp_file::ScopedTempFile::create_in(
        exported_root,
        "storage-driver-requirements",
        "json",
        &bytes,
    )
    .with_context(|| {
        format!(
            "failed to write storage driver manifest beside {}",
            destination.display()
        )
    })?;
    temporary.persist_replace(&destination).with_context(|| {
        format!(
            "failed to publish storage driver manifest: {}",
            destination.display()
        )
    })?;
    Ok(())
}

/// Verifies that an offline Windows DriverStore covers every boot-storage requirement captured
/// before rebooting into PE.
pub fn verify_offline_storage_driver_requirements(
    offline_root: &Path,
    exported_root: &Path,
) -> Result<Vec<StorageDriverRequirement>> {
    let requirements = load_storage_driver_requirements(exported_root)?;
    let driver_store = offline_root
        .join("Windows")
        .join("System32")
        .join("DriverStore")
        .join("FileRepository");
    validate_storage_driver_requirements(&driver_store, &requirements).with_context(|| {
        format!(
            "offline DriverStore does not preserve all boot-storage drivers: {}",
            driver_store.display()
        )
    })?;
    Ok(requirements)
}

pub fn validate_storage_driver_requirements(
    driver_tree: &Path,
    requirements: &[StorageDriverRequirement],
) -> Result<()> {
    validate_requirement_values(requirements)?;
    for requirement in requirements {
        for hardware_id in &requirement.hardware_ids {
            if !crate::storage_driver_match::inf_tree_contains_hardware_id(
                driver_tree,
                hardware_id,
            )? {
                bail!(
                    "missing exported boot-storage driver coverage: {} ({}, hardware ID: {})",
                    requirement.description,
                    requirement.source_inf,
                    hardware_id
                );
            }
        }
    }
    Ok(())
}

pub fn load_storage_driver_requirements(
    exported_root: &Path,
) -> Result<Vec<StorageDriverRequirement>> {
    let manifest_path = exported_root.join(STORAGE_DRIVER_REQUIREMENTS_FILE);
    let metadata = manifest_path.symlink_metadata().with_context(|| {
        format!(
            "storage driver manifest is unavailable: {}",
            manifest_path.display()
        )
    })?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > 1024 * 1024
    {
        bail!(
            "storage driver manifest is not a bounded regular file: {}",
            manifest_path.display()
        );
    }
    let manifest: StorageDriverRequirementsManifest =
        serde_json::from_slice(&std::fs::read(&manifest_path).with_context(|| {
            format!(
                "failed to read storage driver manifest: {}",
                manifest_path.display()
            )
        })?)
        .with_context(|| {
            format!(
                "failed to parse storage driver manifest: {}",
                manifest_path.display()
            )
        })?;
    if manifest.version != 1 {
        bail!(
            "unsupported storage driver manifest version: {}",
            manifest.version
        );
    }
    validate_requirement_values(&manifest.requirements)?;
    Ok(manifest.requirements)
}

/// Returns true only when every captured boot-storage requirement is an Intel VMD controller for
/// which disabling VMD/RST in firmware is a valid recovery route after a failed import.
pub fn requirements_are_only_intel_vmd(requirements: &[StorageDriverRequirement]) -> bool {
    const VMD_IDS: [&str; 6] = ["09AB", "9A0B", "467F", "A77F", "7D0B", "AD0B"];
    let hardware_ids = requirements
        .iter()
        .flat_map(|requirement| requirement.hardware_ids.iter().map(String::as_str))
        .collect::<Vec<_>>();
    !hardware_ids.is_empty()
        && crate::storage_driver_match::select_builtin_storage_driver_packages(
            hardware_ids.iter().copied(),
        )
        .map(|packages| !packages.is_empty())
        .unwrap_or(false)
        && hardware_ids.iter().all(|hardware_id| {
            let normalized = hardware_id.to_ascii_uppercase();
            VMD_IDS
                .iter()
                .any(|device| normalized.contains(&format!("PCI\\VEN_8086&DEV_{device}")))
        })
}

fn validate_requirement_values(requirements: &[StorageDriverRequirement]) -> Result<()> {
    if requirements.len() > 128 {
        bail!("too many boot-storage driver requirements");
    }
    for requirement in requirements {
        if requirement.description.len() > 1024
            || requirement.source_inf.len() > 260
            || requirement.hardware_ids.is_empty()
            || requirement.hardware_ids.len() > 64
        {
            bail!(
                "invalid boot-storage driver requirement: {}",
                requirement.source_inf
            );
        }
        let source = Path::new(&requirement.source_inf);
        if source.file_name() != Some(source.as_os_str())
            || !requirement
                .source_inf
                .to_ascii_lowercase()
                .ends_with(".inf")
        {
            bail!(
                "invalid published storage INF name: {}",
                requirement.source_inf
            );
        }
        if requirement
            .hardware_ids
            .iter()
            .any(|id| id.is_empty() || id.len() > 1024 || id.contains(['\r', '\n', '\0']))
        {
            bail!(
                "invalid storage controller hardware ID in {}",
                requirement.source_inf
            );
        }
    }
    Ok(())
}

fn is_boot_storage_driver(driver: &DriverInfo) -> bool {
    const STORAGE_CLASS_GUIDS: [&str; 2] = [
        "{4D36E97B-E325-11CE-BFC1-08002BE10318}", // SCSIAdapter
        "{4D36E96A-E325-11CE-BFC1-08002BE10318}", // HDC
    ];
    const VMD_IDS: [&str; 6] = ["09AB", "9A0B", "467F", "A77F", "7D0B", "AD0B"];
    driver.device_class.eq_ignore_ascii_case("SCSIAdapter")
        || driver.device_class.eq_ignore_ascii_case("HDC")
        || STORAGE_CLASS_GUIDS
            .iter()
            .any(|guid| driver.class_guid.eq_ignore_ascii_case(guid))
        || driver.hardware_ids.iter().any(|id| {
            let normalized = id.to_ascii_uppercase();
            VMD_IDS
                .iter()
                .any(|device| normalized.contains(&format!("PCI\\VEN_8086&DEV_{device}")))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "letrecovery-driver-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn identifies_bound_oem_scsi_and_vmd_packages() {
        assert_eq!(DIIRFLAG_FORCE_INF, 0x0000_0002);
        let scsi = DriverInfo {
            description: "OEM storage".into(),
            manufacturer: "Vendor".into(),
            inf_path: "oem1.inf".into(),
            hardware_id: "PCI\\VEN_1234&DEV_5678".into(),
            hardware_ids: vec!["PCI\\VEN_1234&DEV_5678".into()],
            device_class: "SCSIAdapter".into(),
            class_guid: String::new(),
            is_oem: true,
        };
        assert!(is_boot_storage_driver(&scsi));

        let mut vmd = scsi.clone();
        vmd.device_class = "System".into();
        vmd.hardware_ids = vec!["PCI\\VEN_8086&DEV_A77F".into()];
        assert!(is_boot_storage_driver(&vmd));

        let mut network = scsi;
        network.device_class = "Net".into();
        network.hardware_ids = vec!["PCI\\VEN_1234&DEV_5678".into()];
        assert!(!is_boot_storage_driver(&network));
    }

    #[test]
    fn setupapi_utf16_buffers_are_aligned_and_byte_bounded() {
        let buffer = aligned_utf16_buffer(6, "test property").unwrap();
        assert_eq!(buffer.len(), 3);
        assert_eq!((buffer.as_ptr() as usize) % std::mem::align_of::<u16>(), 0);
        assert_eq!(utf16_payload(&buffer, 4, "test property").unwrap().len(), 2);
        assert!(aligned_utf16_buffer(3, "test property").is_err());
        assert!(utf16_payload(&buffer, 8, "test property").is_err());
    }

    #[test]
    fn newdev_staging_fallback_is_limited_to_pre_vista_versions() {
        assert!(newdev_fallback_allowed_for_major(5));
        assert!(!newdev_fallback_allowed_for_major(6));
        assert!(!newdev_fallback_allowed_for_major(10));
    }

    #[test]
    fn storage_manifest_requires_and_rechecks_offline_inf_coverage() {
        let exported = TestDirectory::new("exported");
        let package = exported.0.join("iastorvd");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("iaStorVD.inf"),
            b"%VMD%=Install, PCI\\VEN_8086&DEV_A77F\r\n",
        )
        .unwrap();
        let requirement = StorageDriverRequirement {
            description: "Intel VMD".into(),
            source_inf: "oem42.inf".into(),
            hardware_ids: vec!["PCI\\VEN_8086&DEV_A77F&SUBSYS_12341043".into()],
        };
        write_storage_driver_requirements(&exported.0, std::slice::from_ref(&requirement)).unwrap();

        let offline = TestDirectory::new("offline");
        let repository = offline
            .0
            .join("Windows/System32/DriverStore/FileRepository/iaStorVD.inf_test");
        std::fs::create_dir_all(&repository).unwrap();
        std::fs::copy(
            package.join("iaStorVD.inf"),
            repository.join("iaStorVD.inf"),
        )
        .unwrap();
        assert_eq!(
            verify_offline_storage_driver_requirements(&offline.0, &exported.0).unwrap(),
            vec![requirement]
        );

        let incomplete = StorageDriverRequirement {
            description: "Two storage controllers".into(),
            source_inf: "oem43.inf".into(),
            hardware_ids: vec![
                "PCI\\VEN_8086&DEV_A77F".into(),
                "PCI\\VEN_1234&DEV_5678".into(),
            ],
        };
        assert!(validate_storage_driver_requirements(
            &offline
                .0
                .join("Windows/System32/DriverStore/FileRepository"),
            &[incomplete]
        )
        .is_err());
    }

    #[test]
    fn only_exact_intel_vmd_requirements_have_a_firmware_recovery_route() {
        let vmd = StorageDriverRequirement {
            description: "Intel VMD".into(),
            source_inf: "iastorvd.inf".into(),
            hardware_ids: vec!["PCI\\VEN_8086&DEV_A77F&SUBSYS_12341043".into()],
        };
        assert!(requirements_are_only_intel_vmd(std::slice::from_ref(&vmd)));
        let mut vmd_with_managed_function = vmd.clone();
        vmd_with_managed_function
            .hardware_ids
            .push("PCI\\VEN_8086&DEV_09AB".into());
        assert!(requirements_are_only_intel_vmd(&[
            vmd_with_managed_function
        ]));
        assert!(!requirements_are_only_intel_vmd(&[]));

        let nvme = StorageDriverRequirement {
            description: "NVMe".into(),
            source_inf: "stornvme.inf".into(),
            hardware_ids: vec!["PCI\\VEN_144D&DEV_A80A".into()],
        };
        assert!(!requirements_are_only_intel_vmd(&[vmd, nvme]));
    }
}
