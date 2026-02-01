//! Windows 驱动管理模块
//!
//! 使用多种方式实现驱动的导出和导入功能：
//! - 离线驱动导入：优先使用 dism.exe 命令行，回退到 Windows API
//! - 在线驱动操作：使用 SetupAPI/NewDev API
//!
//! Windows API 模块：
//! - SetupAPI (setupapi.dll) - 驱动安装和枚举
//! - NewDev API (newdev.dll) - 驱动安装
//! - CfgMgr32 (cfgmgr32.dll) - 设备配置管理
//!
//! DISM 命令行：
//! - 优先使用 {程序目录}\bin\Dism\dism.exe
//! - 回退到系统 dism.exe

use std::ffi::{c_void, OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr::null_mut;

use anyhow::{bail, Context, Result};
use libloading::Library;

#[cfg(windows)]
use windows::Win32::Foundation::{GetLastError, BOOL, HWND};

// ============================================================================
// 常量定义
// ============================================================================

// SetupAPI 常量
const DIGCF_PRESENT: u32 = 0x0000_0002;
const DIGCF_ALLCLASSES: u32 = 0x0000_0004;

const SPDRP_DRIVER: u32 = 0x0000_0009;
const SPDRP_INF_PATH: u32 = 0x0000_0010;
const SPDRP_HARDWAREID: u32 = 0x0000_0001;
const SPDRP_DEVICEDESC: u32 = 0x0000_0000;
const SPDRP_MFG: u32 = 0x0000_000B;
const SPDRP_CLASS: u32 = 0x0000_0007;
const SPDRP_CLASSGUID: u32 = 0x0000_0008;

const ERROR_NO_MORE_ITEMS: u32 = 259;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

const INSTALLFLAG_FORCE: u32 = 0x0000_0001;
const INSTALLFLAG_READONLY: u32 = 0x0000_0002;
const INSTALLFLAG_NONINTERACTIVE: u32 = 0x0000_0004;

const DRIVER_PACKAGE_REPAIR: u32 = 0x0000_0001;
const DRIVER_PACKAGE_FORCE: u32 = 0x0000_0004;
const DRIVER_PACKAGE_LEGACY_MODE: u32 = 0x0000_0010;

const SP_COPY_NOOVERWRITE: u32 = 0x0000_0008;
const SP_COPY_OEMINF_CATALOG_ONLY: u32 = 0x0004_0000;

const REG_SZ: u32 = 1;
const REG_MULTI_SZ: u32 = 7;

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

#[repr(C)]
#[derive(Clone, Copy)]
struct SpDrvInfoDataW {
    cb_size: u32,
    driver_type: u32,
    reserved: usize,
    description: [u16; 256],
    mfg_name: [u16; 256],
    provider_name: [u16; 256],
    driver_date: u64,
    driver_version: u64,
}

impl Default for SpDrvInfoDataW {
    fn default() -> Self {
        Self {
            cb_size: std::mem::size_of::<Self>() as u32,
            driver_type: 0,
            reserved: 0,
            description: [0; 256],
            mfg_name: [0; 256],
            provider_name: [0; 256],
            driver_date: 0,
            driver_version: 0,
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

type FnSetupDiDestroyDeviceInfoList = unsafe extern "system" fn(dev_info: HDevInfo) -> BOOL;

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

type FnSetupUninstallOEMInfW = unsafe extern "system" fn(
    inf_file_name: *const u16,
    flags: u32,
    reserved: *mut c_void,
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

type FnUpdateDriverForPlugAndPlayDevicesW = unsafe extern "system" fn(
    hwnd_parent: HWND,
    hardware_id: *const u16,
    full_inf_path: *const u16,
    install_flags: u32,
    b_reboot_required: *mut BOOL,
) -> BOOL;

// DIFx API (difxapi.dll) - 可选，用于驱动包管理
type FnDriverPackageInstallW = unsafe extern "system" fn(
    inf_path: *const u16,
    flags: u32,
    installer_info: *const c_void,
    need_reboot: *mut BOOL,
) -> u32;

type FnDriverPackageUninstallW = unsafe extern "system" fn(
    inf_path: *const u16,
    flags: u32,
    installer_info: *const c_void,
    need_reboot: *mut BOOL,
) -> u32;

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
    OsString::from_wide(&wide[..len]).to_string_lossy().into_owned()
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
    /// 设备类别
    pub device_class: String,
    /// 类别 GUID
    pub class_guid: String,
    /// 是否为第三方驱动 (OEM)
    pub is_oem: bool,
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
    destroy_device_info_list: FnSetupDiDestroyDeviceInfoList,
    copy_oem_inf: FnSetupCopyOEMInfW,
    uninstall_oem_inf: FnSetupUninstallOEMInfW,
    get_inf_driver_store_location: Option<FnSetupGetInfDriverStoreLocationW>,
}

impl SetupApi {
    fn new() -> Result<Self> {
        let lib = unsafe { Library::new("setupapi.dll") }
            .context("无法加载 setupapi.dll")?;

        unsafe {
            let get_class_devs: FnSetupDiGetClassDevsW = 
                *lib.get(b"SetupDiGetClassDevsW")?;
            let enum_device_info: FnSetupDiEnumDeviceInfo = 
                *lib.get(b"SetupDiEnumDeviceInfo")?;
            let get_device_registry_property: FnSetupDiGetDeviceRegistryPropertyW = 
                *lib.get(b"SetupDiGetDeviceRegistryPropertyW")?;
            let destroy_device_info_list: FnSetupDiDestroyDeviceInfoList = 
                *lib.get(b"SetupDiDestroyDeviceInfoList")?;
            let copy_oem_inf: FnSetupCopyOEMInfW = 
                *lib.get(b"SetupCopyOEMInfW")?;
            let uninstall_oem_inf: FnSetupUninstallOEMInfW = 
                *lib.get(b"SetupUninstallOEMInfW")?;
            
            // 这个函数在 Windows 8+ 才有
            let get_inf_driver_store_location = lib
                .get::<FnSetupGetInfDriverStoreLocationW>(b"SetupGetInfDriverStoreLocationW")
                .ok()
                .map(|f| *f);

            Ok(Self {
                _lib: lib,
                get_class_devs,
                enum_device_info,
                get_device_registry_property,
                destroy_device_info_list,
                copy_oem_inf,
                uninstall_oem_inf,
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
    ) -> Option<String> {
        let mut buffer = vec![0u8; 4096];
        let mut required_size: u32 = 0;
        let mut reg_type: u32 = 0;

        let result = unsafe {
            (self.get_device_registry_property)(
                dev_info,
                dev_info_data,
                property,
                &mut reg_type,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                &mut required_size,
            )
        };

        if result.0 == 0 {
            return None;
        }

        // 转换为字符串
        if reg_type == REG_SZ || reg_type == REG_MULTI_SZ {
            let wide_slice = unsafe {
                std::slice::from_raw_parts(
                    buffer.as_ptr() as *const u16,
                    required_size as usize / 2,
                )
            };
            Some(wide_to_string(wide_slice))
        } else {
            None
        }
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

        // 枚举每个设备
        let mut index = 0u32;
        loop {
            let mut dev_info_data = SpDevInfoData::default();
            
            let result = unsafe {
                (self.enum_device_info)(dev_info, index, &mut dev_info_data)
            };

            if result.0 == 0 {
                let err = get_last_error();
                if err == ERROR_NO_MORE_ITEMS {
                    break;
                }
                index += 1;
                continue;
            }

            // 获取 INF 路径
            if let Some(inf_path) = self.get_device_property_string(
                dev_info, &dev_info_data, SPDRP_INF_PATH
            ) {
                // 检查是否为 OEM 驱动
                let is_oem = inf_path.to_lowercase().starts_with("oem");

                let description = self
                    .get_device_property_string(dev_info, &dev_info_data, SPDRP_DEVICEDESC)
                    .unwrap_or_default();

                let manufacturer = self
                    .get_device_property_string(dev_info, &dev_info_data, SPDRP_MFG)
                    .unwrap_or_default();

                let hardware_id = self
                    .get_device_property_string(dev_info, &dev_info_data, SPDRP_HARDWAREID)
                    .unwrap_or_default();

                let device_class = self
                    .get_device_property_string(dev_info, &dev_info_data, SPDRP_CLASS)
                    .unwrap_or_default();

                let class_guid = self
                    .get_device_property_string(dev_info, &dev_info_data, SPDRP_CLASSGUID)
                    .unwrap_or_default();

                drivers.push(DriverInfo {
                    description,
                    manufacturer,
                    inf_path,
                    hardware_id,
                    device_class,
                    class_guid,
                    is_oem,
                });
            }

            index += 1;
        }

        // 清理
        unsafe {
            let _ = (self.destroy_device_info_list)(dev_info);
        }

        Ok(drivers)
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

    /// 卸载 OEM INF 文件
    fn uninstall_inf(&self, inf_name: &str) -> Result<()> {
        let wide_name = to_wide(inf_name);

        // SUOI_FORCEDELETE = 1
        let result = unsafe {
            (self.uninstall_oem_inf)(wide_name.as_ptr(), 1, null_mut())
        };

        if result.0 == 0 {
            let err = get_last_error();
            bail!("SetupUninstallOEMInf 失败: 错误码 {}", err);
        }

        Ok(())
    }

    /// 获取 INF 文件在驱动存储中的完整路径
    fn get_driver_store_path(&self, inf_name: &str) -> Option<PathBuf> {
        let func = self.get_inf_driver_store_location?;
        
        let wide_name = to_wide(inf_name);
        let mut buffer = vec![0u16; 520];
        let mut required_size: u32 = 0;

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

        if result.0 != 0 {
            Some(PathBuf::from(wide_to_string(&buffer)))
        } else {
            None
        }
    }
}

// ============================================================================
// NewDev API 封装
// ============================================================================

/// NewDev API 封装
struct NewDevApi {
    _lib: Library,
    di_install_driver: FnDiInstallDriverW,
    update_driver: FnUpdateDriverForPlugAndPlayDevicesW,
}

impl NewDevApi {
    fn new() -> Result<Self> {
        let lib = unsafe { Library::new("newdev.dll") }
            .context("无法加载 newdev.dll")?;

        unsafe {
            let di_install_driver: FnDiInstallDriverW = 
                *lib.get(b"DiInstallDriverW")?;
            let update_driver: FnUpdateDriverForPlugAndPlayDevicesW = 
                *lib.get(b"UpdateDriverForPlugAndPlayDevicesW")?;

            Ok(Self {
                _lib: lib,
                di_install_driver,
                update_driver,
            })
        }
    }

    /// 安装驱动
    fn install_driver(&self, inf_path: &Path, force: bool) -> Result<bool> {
        let wide_path = path_to_wide(inf_path);
        let mut need_reboot = BOOL::default();

        let flags = if force {
            INSTALLFLAG_FORCE | INSTALLFLAG_NONINTERACTIVE
        } else {
            INSTALLFLAG_NONINTERACTIVE
        };

        let result = unsafe {
            (self.di_install_driver)(
                HWND::default(),
                wide_path.as_ptr(),
                flags,
                &mut need_reboot,
            )
        };

        if result.0 == 0 {
            let err = get_last_error();
            bail!("DiInstallDriverW 失败: 错误码 {}", err);
        }

        Ok(need_reboot.0 != 0)
    }

    /// 更新即插即用设备的驱动
    fn update_pnp_driver(&self, hardware_id: &str, inf_path: &Path, force: bool) -> Result<bool> {
        let wide_hwid = to_wide(hardware_id);
        let wide_path = path_to_wide(inf_path);
        let mut need_reboot = BOOL::default();

        let flags = if force {
            INSTALLFLAG_FORCE
        } else {
            0
        };

        let result = unsafe {
            (self.update_driver)(
                HWND::default(),
                wide_hwid.as_ptr(),
                wide_path.as_ptr(),
                flags,
                &mut need_reboot,
            )
        };

        if result.0 == 0 {
            let err = get_last_error();
            // 错误码 0x0 表示成功但没有更新
            if err == 0 {
                return Ok(false);
            }
            bail!("UpdateDriverForPlugAndPlayDevicesW 失败: 错误码 {}", err);
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
        let newdev_api = NewDevApi::new().ok();

        Ok(Self {
            setup_api,
            newdev_api,
        })
    }

    /// 枚举系统中所有已安装的驱动
    pub fn enumerate_all_drivers(&self) -> Result<Vec<DriverInfo>> {
        self.setup_api.enumerate_drivers()
    }

    /// 枚举第三方 (OEM) 驱动
    pub fn enumerate_oem_drivers(&self) -> Result<Vec<DriverInfo>> {
        let all_drivers = self.setup_api.enumerate_drivers()?;
        Ok(all_drivers.into_iter().filter(|d| d.is_oem).collect())
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

        println!("[DriverManager] 找到 {} 个驱动需要导出", drivers.len());

        // 去重 INF 路径
        let mut exported_infs = std::collections::HashSet::new();
        let mut success_count = 0;

        for driver in &drivers {
            if exported_infs.contains(&driver.inf_path) {
                continue;
            }
            exported_infs.insert(driver.inf_path.clone());

            // 获取驱动存储中的完整路径
            let driver_store_path = if let Some(full_path) = 
                self.setup_api.get_driver_store_path(&driver.inf_path) 
            {
                full_path
            } else {
                // 尝试使用 Windows\INF 目录
                let windows_dir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_string());
                PathBuf::from(&windows_dir).join("INF").join(&driver.inf_path)
            };

            if !driver_store_path.exists() {
                println!(
                    "[DriverManager] 警告: 驱动文件不存在: {:?}",
                    driver_store_path
                );
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
                    println!(
                        "[DriverManager] 已导出: {} -> {:?}",
                        driver.description, dest_dir
                    );
                    success_count += 1;
                }
                Err(e) => {
                    println!(
                        "[DriverManager] 导出失败: {} - {}",
                        driver.description, e
                    );
                }
            }
        }

        println!("[DriverManager] 成功导出 {} 个驱动", success_count);
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
        println!("[DriverManager] 找到 {} 个 INF 文件", inf_files.len());

        for inf_path in inf_files {
            println!("[DriverManager] 正在安装: {:?}", inf_path);

            match self.install_single_driver(&inf_path, force) {
                Ok(reboot) => {
                    success_count += 1;
                    need_reboot = need_reboot || reboot;
                    println!("[DriverManager] 安装成功: {:?}", inf_path);
                }
                Err(e) => {
                    fail_count += 1;
                    println!("[DriverManager] 安装失败: {:?} - {}", inf_path, e);
                }
            }
        }

        println!(
            "[DriverManager] 驱动导入完成: 成功 {}, 失败 {}, 需要重启: {}",
            success_count, fail_count, need_reboot
        );

        Ok((success_count, fail_count, need_reboot))
    }

    /// 安装单个驱动
    fn install_single_driver(&self, inf_path: &Path, force: bool) -> Result<bool> {
        // 首先尝试使用 NewDev API
        if let Some(ref newdev) = self.newdev_api {
            match newdev.install_driver(inf_path, force) {
                Ok(reboot) => return Ok(reboot),
                Err(e) => {
                    println!("[DriverManager] DiInstallDriver 失败: {}, 尝试 SetupCopyOEMInf", e);
                }
            }
        }

        // 回退到 SetupCopyOEMInf（只添加到驱动存储，不实际安装）
        self.setup_api.install_inf(inf_path)?;
        Ok(false)
    }

    /// 递归查找目录中的所有 INF 文件
    fn find_inf_files(dir: &Path) -> Result<Vec<PathBuf>> {
        let mut inf_files = Vec::new();

        if !dir.is_dir() {
            bail!("{:?} 不是目录", dir);
        }

        for entry in walkdir::WalkDir::new(dir)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension() {
                    if ext.to_ascii_lowercase() == "inf" {
                        inf_files.push(path.to_path_buf());
                    }
                }
            }
        }

        Ok(inf_files)
    }

    /// 导入驱动到离线系统（PE环境下使用）
    /// 
    /// 使用 dism.exe 命令行进行离线驱动注入
    /// 优先使用 {程序目录}\bin\Dism\dism.exe
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
        use crate::core::dism_cmd::DismCmd;
        
        println!(
            "[DriverManager] 使用 dism.exe 离线导入驱动: {:?} -> {:?}",
            source_dir, offline_root
        );

        // 规范化路径
        let image_path = offline_root.to_string_lossy();
        let driver_path = source_dir.to_string_lossy();

        // 使用 dism.exe 命令行进行离线驱动注入
        let dism_cmd = DismCmd::new()
            .map_err(|e| anyhow::anyhow!("DISM 命令行初始化失败: {}", e))?;

        // 统计驱动文件数量
        let inf_files = Self::find_inf_files(source_dir)?;
        let inf_count = inf_files.len();

        // 使用智能导入（支持 INF 和 CAB）
        match dism_cmd.import_drivers_smart(&image_path, &driver_path, None) {
            Ok(_) => {
                println!(
                    "[DriverManager] dism.exe 离线驱动导入成功"
                );
                // DISM 成功时假设所有驱动都导入成功
                Ok((inf_count.max(1), 0))
            }
            Err(e) => {
                println!(
                    "[DriverManager] dism.exe 导入失败: {}, 尝试备用方法",
                    e
                );
                // 回退到传统方法
                self.import_drivers_offline_legacy(offline_root, source_dir)
            }
        }
    }

    /// 传统离线驱动导入方法（作为备用）
    /// 
    /// 完整的离线驱动注入，包括：
    /// 1. 复制驱动文件到 DriverStore\FileRepository
    /// 2. 复制 .sys 文件到 System32\drivers
    /// 3. 复制 INF 到 Windows\INF (命名为 oem*.inf)
    /// 4. 注册驱动服务到离线注册表
    fn import_drivers_offline_legacy(
        &self,
        offline_root: &Path,
        source_dir: &Path,
    ) -> Result<(usize, usize)> {
        let mut success_count = 0;
        let mut fail_count = 0;

        // 目标目录
        let driver_store = offline_root
            .join("Windows")
            .join("System32")
            .join("DriverStore")
            .join("FileRepository");
        let system_drivers = offline_root
            .join("Windows")
            .join("System32")
            .join("drivers");
        let inf_dir = offline_root
            .join("Windows")
            .join("INF");

        std::fs::create_dir_all(&driver_store)?;
        std::fs::create_dir_all(&system_drivers)?;
        std::fs::create_dir_all(&inf_dir)?;

        // 获取下一个可用的 OEM INF 编号
        let mut oem_index = Self::get_next_oem_index(&inf_dir);

        // 递归查找所有 INF 文件
        let inf_files = Self::find_inf_files(source_dir)?;
        println!(
            "[DriverManager] 离线安装: 找到 {} 个 INF 文件",
            inf_files.len()
        );

        for inf_path in inf_files {
            // 获取 INF 所在目录
            let inf_source_dir = inf_path.parent().unwrap_or(source_dir);
            let inf_name = inf_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown");
            let inf_filename = inf_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown.inf");

            // 1. 复制到 DriverStore\FileRepository
            let target_store_dir = driver_store.join(format!("{}.inf_amd64_offline{:08x}", inf_name, oem_index));
            if let Err(e) = Self::copy_dir_recursive(inf_source_dir, &target_store_dir) {
                println!("[DriverManager] 复制到DriverStore失败: {:?} - {}", inf_path, e);
                fail_count += 1;
                continue;
            }

            // 2. 解析 INF 文件并复制 .sys 文件到 System32\drivers
            if let Err(e) = Self::process_driver_files(&target_store_dir, &system_drivers) {
                println!("[DriverManager] 处理驱动文件失败: {:?} - {}", inf_path, e);
                // 继续，不算失败
            }

            // 3. 复制 INF 到 Windows\INF (命名为 oem{N}.inf)
            let oem_inf_name = format!("oem{}.inf", oem_index);
            let oem_inf_path = inf_dir.join(&oem_inf_name);
            let source_inf = target_store_dir.join(inf_filename);
            if source_inf.exists() {
                if let Err(e) = std::fs::copy(&source_inf, &oem_inf_path) {
                    println!("[DriverManager] 复制INF到Windows\\INF失败: {} - {}", oem_inf_name, e);
                }
            }

            // 4. 注册驱动服务到离线注册表
            if let Err(e) = Self::register_driver_to_offline_registry(
                offline_root,
                &target_store_dir,
                inf_filename,
                &oem_inf_name,
            ) {
                println!("[DriverManager] 注册驱动服务失败: {:?} - {}", inf_path, e);
                // 继续，不算失败（文件已复制，可能在启动时自动识别）
            }

            success_count += 1;
            oem_index += 1;
            println!("[DriverManager] 离线安装成功: {:?} -> {}", inf_path, oem_inf_name);
        }

        println!(
            "[DriverManager] 离线驱动导入完成: 成功 {}, 失败 {}",
            success_count, fail_count
        );

        Ok((success_count, fail_count))
    }

    /// 获取下一个可用的 OEM INF 编号
    fn get_next_oem_index(inf_dir: &Path) -> u32 {
        let mut max_index = 0u32;
        
        if let Ok(entries) = std::fs::read_dir(inf_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy().to_lowercase();
                
                if name_str.starts_with("oem") && name_str.ends_with(".inf") {
                    // 提取数字部分
                    let num_part = &name_str[3..name_str.len()-4];
                    if let Ok(num) = num_part.parse::<u32>() {
                        if num > max_index {
                            max_index = num;
                        }
                    }
                }
            }
        }
        
        max_index + 1
    }

    /// 处理驱动文件：复制 .sys 文件到 System32\drivers
    fn process_driver_files(driver_store_dir: &Path, system_drivers: &Path) -> Result<()> {
        // 查找目录中所有 .sys 文件
        for entry in std::fs::read_dir(driver_store_dir)? {
            let entry = entry?;
            let path = entry.path();
            
            if path.is_file() {
                if let Some(ext) = path.extension() {
                    if ext.to_string_lossy().to_lowercase() == "sys" {
                        // 复制到 System32\drivers
                        if let Some(filename) = path.file_name() {
                            let dest = system_drivers.join(filename);
                            if !dest.exists() {
                                if let Err(e) = std::fs::copy(&path, &dest) {
                                    println!("[DriverManager] 复制sys文件失败: {:?} - {}", filename, e);
                                } else {
                                    println!("[DriverManager] 已复制: {:?} -> {:?}", filename, dest);
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// 注册驱动服务到离线注册表
    fn register_driver_to_offline_registry(
        offline_root: &Path,
        driver_store_dir: &Path,
        inf_filename: &str,
        oem_inf_name: &str,
    ) -> Result<()> {
        use crate::core::registry::OfflineRegistry;
        
        // 查找 INF 文件
        let inf_path = driver_store_dir.join(inf_filename);
        if !inf_path.exists() {
            return Ok(()); // INF 不存在，跳过注册
        }

        // 读取并解析 INF 文件
        let inf_content = std::fs::read_to_string(&inf_path)
            .unwrap_or_default();
        
        // 解析服务信息
        let service_info = Self::parse_inf_service_info(&inf_content);
        
        if service_info.is_empty() {
            println!("[DriverManager] INF 中未找到服务定义: {}", inf_filename);
            return Ok(());
        }

        // 加载离线 SYSTEM 注册表
        let system_hive = offline_root
            .join("Windows")
            .join("System32")
            .join("config")
            .join("SYSTEM");
        
        if !system_hive.exists() {
            println!("[DriverManager] SYSTEM hive 不存在: {:?}", system_hive);
            return Ok(());
        }

        let hive_key = format!("drv_offline_{}", std::process::id());
        
        // 尝试加载注册表
        if let Err(e) = OfflineRegistry::load_hive(&hive_key, &system_hive.to_string_lossy()) {
            println!("[DriverManager] 加载SYSTEM hive失败: {}", e);
            return Ok(());
        }

        // 注册每个服务
        for (service_name, service_binary, service_type, start_type, error_control) in &service_info {
            let service_key = format!(
                "HKLM\\{}\\ControlSet001\\Services\\{}",
                hive_key, service_name
            );
            
            // 创建服务键
            let _ = OfflineRegistry::create_key(&service_key);
            
            // 设置服务属性
            let _ = OfflineRegistry::set_dword(&service_key, "Type", *service_type);
            let _ = OfflineRegistry::set_dword(&service_key, "Start", *start_type);
            let _ = OfflineRegistry::set_dword(&service_key, "ErrorControl", *error_control);
            
            // 设置 ImagePath (使用 REG_EXPAND_SZ)
            let image_path = if service_binary.contains('\\') || service_binary.contains('/') {
                service_binary.clone()
            } else {
                format!("System32\\drivers\\{}", service_binary)
            };
            let _ = OfflineRegistry::set_expand_string(&service_key, "ImagePath", &image_path);
            
            // 同时设置 ControlSet002 (如果存在)
            let service_key2 = format!(
                "HKLM\\{}\\ControlSet002\\Services\\{}",
                hive_key, service_name
            );
            let _ = OfflineRegistry::create_key(&service_key2);
            let _ = OfflineRegistry::set_dword(&service_key2, "Type", *service_type);
            let _ = OfflineRegistry::set_dword(&service_key2, "Start", *start_type);
            let _ = OfflineRegistry::set_dword(&service_key2, "ErrorControl", *error_control);
            let _ = OfflineRegistry::set_expand_string(&service_key2, "ImagePath", &image_path);
            
            println!(
                "[DriverManager] 已注册服务: {} (Type={}, Start={}, ImagePath={})",
                service_name, service_type, start_type, image_path
            );
        }

        // 卸载注册表
        let _ = OfflineRegistry::unload_hive(&hive_key);
        
        Ok(())
    }

    /// 解析 INF 文件中的服务信息
    /// 返回: Vec<(服务名, 二进制文件, 类型, 启动类型, 错误控制)>
    fn parse_inf_service_info(inf_content: &str) -> Vec<(String, String, u32, u32, u32)> {
        let mut services = Vec::new();
        let mut current_section = String::new();
        let mut service_install_sections: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        
        // 第一遍：找到 AddService 指令，获取服务名和安装段名
        for line in inf_content.lines() {
            let line = line.trim();
            
            // 跳过注释和空行
            if line.is_empty() || line.starts_with(';') {
                continue;
            }
            
            // 检查段名
            if line.starts_with('[') && line.ends_with(']') {
                current_section = line[1..line.len()-1].to_lowercase();
                continue;
            }
            
            // 查找 AddService 指令
            let lower_line = line.to_lowercase();
            if lower_line.starts_with("addservice") {
                // AddService = ServiceName, flags, InstallSection
                let parts: Vec<&str> = line.splitn(2, '=').collect();
                if parts.len() == 2 {
                    let args: Vec<&str> = parts[1].split(',').map(|s| s.trim()).collect();
                    if args.len() >= 3 {
                        let service_name = args[0].trim().to_string();
                        let install_section = args[2].trim().to_lowercase();
                        if !service_name.is_empty() && !install_section.is_empty() {
                            service_install_sections.insert(install_section, service_name);
                        }
                    }
                }
            }
        }
        
        // 第二遍：解析服务安装段
        current_section.clear();
        let mut service_type: u32 = 1; // SERVICE_KERNEL_DRIVER
        let mut start_type: u32 = 3;   // SERVICE_DEMAND_START
        let mut error_control: u32 = 1; // SERVICE_ERROR_NORMAL
        let mut service_binary = String::new();
        
        for line in inf_content.lines() {
            let line = line.trim();
            
            if line.is_empty() || line.starts_with(';') {
                continue;
            }
            
            if line.starts_with('[') && line.ends_with(']') {
                // 保存之前段的服务信息
                if let Some(service_name) = service_install_sections.get(&current_section) {
                    if !service_binary.is_empty() {
                        services.push((
                            service_name.clone(),
                            service_binary.clone(),
                            service_type,
                            start_type,
                            error_control,
                        ));
                    }
                }
                
                // 重置并切换到新段
                current_section = line[1..line.len()-1].to_lowercase();
                service_type = 1;
                start_type = 3;
                error_control = 1;
                service_binary.clear();
                continue;
            }
            
            // 解析服务段中的属性
            if service_install_sections.contains_key(&current_section) {
                let parts: Vec<&str> = line.splitn(2, '=').collect();
                if parts.len() == 2 {
                    let key = parts[0].trim().to_lowercase();
                    let value = parts[1].trim();
                    
                    match key.as_str() {
                        "servicetype" => {
                            service_type = Self::parse_inf_number(value);
                        }
                        "starttype" => {
                            start_type = Self::parse_inf_number(value);
                        }
                        "errorcontrol" => {
                            error_control = Self::parse_inf_number(value);
                        }
                        "servicebinary" => {
                            // %12%\xxx.sys 或 %dirid%\xxx.sys
                            service_binary = Self::resolve_inf_path(value);
                        }
                        _ => {}
                    }
                }
            }
        }
        
        // 保存最后一个段
        if let Some(service_name) = service_install_sections.get(&current_section) {
            if !service_binary.is_empty() {
                services.push((
                    service_name.clone(),
                    service_binary,
                    service_type,
                    start_type,
                    error_control,
                ));
            }
        }
        
        services
    }

    /// 解析 INF 文件中的数值（支持十进制和十六进制）
    fn parse_inf_number(value: &str) -> u32 {
        let value = value.split(';').next().unwrap_or("").trim();
        let value = value.split(',').next().unwrap_or("").trim();
        
        if value.to_lowercase().starts_with("0x") {
            u32::from_str_radix(&value[2..], 16).unwrap_or(0)
        } else {
            value.parse().unwrap_or(0)
        }
    }

    /// 解析 INF 路径，提取文件名
    fn resolve_inf_path(value: &str) -> String {
        // 移除注释
        let value = value.split(';').next().unwrap_or("").trim();
        
        // 提取文件名（去掉 %xx% 路径部分）
        // %12%\xxx.sys -> xxx.sys
        // %dirid%\path\xxx.sys -> xxx.sys
        let filename = value
            .rsplit(|c| c == '\\' || c == '/')
            .next()
            .unwrap_or(value)
            .trim();
        
        filename.to_string()
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

        let driver_store = system_root
            .join("Windows")
            .join("System32")
            .join("DriverStore")
            .join("FileRepository");

        if !driver_store.exists() {
            bail!("驱动存储目录不存在: {:?}", driver_store);
        }

        let mut success_count = 0;

        // 遍历 FileRepository 目录
        for entry in std::fs::read_dir(&driver_store)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                // 检查是否为 OEM 驱动（目录名包含 oem）
                let dir_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                
                // 只导出第三方驱动（包含 oem 或不在系统自带列表中）
                if Self::is_third_party_driver(dir_name) {
                    let dest_dir = destination.join(dir_name);
                    match Self::copy_dir_recursive(&path, &dest_dir) {
                        Ok(_) => {
                            success_count += 1;
                            println!("[DriverManager] 已导出: {}", dir_name);
                        }
                        Err(e) => {
                            println!("[DriverManager] 导出失败: {} - {}", dir_name, e);
                        }
                    }
                }
            }
        }

        println!("[DriverManager] 从系统导出 {} 个驱动", success_count);
        Ok(success_count)
    }

    /// 判断是否为第三方驱动
    fn is_third_party_driver(dir_name: &str) -> bool {
        let lower = dir_name.to_lowercase();
        
        // OEM 驱动
        if lower.contains("oem") {
            return true;
        }

        // 常见系统自带驱动前缀（跳过这些）
        let system_prefixes = [
            "acpi", "amd", "atiilhag", "basicdisplay", "basicrender",
            "compositebus", "disk", "display", "dual", "ehstorclass",
            "fdc", "floppy", "hdaudio", "hid", "i8042prt", "input",
            "kdnic", "keyboard", "ks", "monitor", "mouse", "mshdc",
            "msisadrv", "mssecflt", "netio", "ntfs", "nvraid", "pci",
            "pcmcia", "pdc", "portcls", "processr", "rdyboost", "sata",
            "scsi", "sd", "serial", "spaceport", "storage", "swenum",
            "sysaudio", "termdd", "uaspstor", "ufs", "umbus", "umdf",
            "umpnpmgr", "usb", "vdrvroot", "vga", "volmgr", "volsnap",
            "wdf", "wdma", "wfp", "win32k", "winhv", "wmilib", "wof",
            "ws2ifsl", "wudf",
        ];

        for prefix in &system_prefixes {
            if lower.starts_with(prefix) {
                return false;
            }
        }

        // 默认认为是第三方驱动
        true
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
    manager.export_drivers_from_system(
        Path::new(system_partition),
        Path::new(destination),
    )
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
    manager.import_drivers_offline(
        Path::new(offline_root),
        Path::new(driver_path),
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_third_party_detection() {
        assert!(DriverManager::is_third_party_driver("oem123.inf_amd64"));
        assert!(DriverManager::is_third_party_driver("nvlddmkm.inf_amd64"));
        assert!(!DriverManager::is_third_party_driver("usbport.inf_amd64"));
        assert!(!DriverManager::is_third_party_driver("pci.inf_amd64"));
    }
}
