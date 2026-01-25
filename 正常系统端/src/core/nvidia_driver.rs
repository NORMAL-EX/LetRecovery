//! 英伟达显卡驱动卸载模块
//!
//! 使用 Windows SetupAPI 实现显卡驱动的检测和卸载功能。
//! 支持在线系统和离线系统 (PE环境) 的驱动卸载。
//!
//! # 功能
//! - 枚举系统中的所有显卡设备
//! - 检测英伟达显卡及其驱动
//! - 卸载英伟达显卡驱动
//! - 支持离线系统驱动卸载

#[cfg(windows)]
use std::ffi::{OsStr, OsString};
#[cfg(windows)]
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

use anyhow::{bail, Context, Result};

#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiCallClassInstaller, SetupDiDestroyDeviceInfoList,
    SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceInstanceIdW,
    SetupDiGetDeviceRegistryPropertyW, SetupDiRemoveDevice, SetupDiSetClassInstallParamsW,
    DIGCF_ALLCLASSES, DIGCF_PRESENT, DIF_PROPERTYCHANGE, DICS_DISABLE,
    DICS_FLAG_GLOBAL, HDEVINFO, SP_CLASSINSTALL_HEADER, SP_DEVINFO_DATA, SP_PROPCHANGE_PARAMS,
    SPDRP_CLASS, SPDRP_DEVICEDESC, SPDRP_DRIVER, SPDRP_FRIENDLYNAME,
    SPDRP_HARDWAREID, SPDRP_MFG, SETUP_DI_REGISTRY_PROPERTY,
};
#[cfg(windows)]
use windows::Win32::Foundation::{GetLastError, ERROR_NO_MORE_ITEMS, HWND};
#[cfg(windows)]
use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;

/// 显卡设备信息
#[derive(Debug, Clone, Default)]
pub struct GpuDeviceInfo {
    /// 设备描述/型号名称
    pub name: String,
    /// 友好名称
    pub friendly_name: String,
    /// 硬件 ID
    pub hardware_id: String,
    /// 设备实例 ID
    pub instance_id: String,
    /// 制造商
    pub manufacturer: String,
    /// 驱动键值
    pub driver_key: String,
    /// 设备类型
    pub device_class: String,
    /// 设备索引
    pub device_index: u32,
    /// 是否为英伟达设备
    pub is_nvidia: bool,
    /// 是否为 AMD 设备
    pub is_amd: bool,
    /// 是否为 Intel 设备
    pub is_intel: bool,
}

/// 系统硬件摘要信息（用于显示在卸载对话框中）
#[derive(Debug, Clone, Default)]
pub struct SystemHardwareSummary {
    /// 所有显卡设备
    pub gpu_devices: Vec<GpuDeviceInfo>,
    /// CPU 名称
    pub cpu_name: String,
    /// 内存大小 (bytes)
    pub memory_size: u64,
    /// 可用内存 (bytes)
    pub memory_available: u64,
}

/// 驱动卸载结果
#[derive(Debug, Clone, Default)]
pub struct UninstallResult {
    /// 是否成功
    pub success: bool,
    /// 消息
    pub message: String,
    /// 是否需要重启
    pub needs_reboot: bool,
    /// 成功卸载的驱动数量
    pub uninstalled_count: usize,
    /// 失败的驱动数量
    pub failed_count: usize,
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 将 Rust 字符串转换为以 NUL 结尾的 UTF-16 Vec
#[cfg(windows)]
fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

/// 将 UTF-16 缓冲区转换为 Rust 字符串
#[cfg(windows)]
fn wide_to_string(wide: &[u16]) -> String {
    let len = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    OsString::from_wide(&wide[..len])
        .to_string_lossy()
        .into_owned()
}

/// 检查是否为英伟达设备
fn is_nvidia_device(hardware_id: &str, manufacturer: &str, name: &str) -> bool {
    let hw_lower = hardware_id.to_lowercase();
    let mfg_lower = manufacturer.to_lowercase();
    let name_lower = name.to_lowercase();

    // 英伟达硬件 ID 以 PCI\VEN_10DE 开头
    hw_lower.contains("ven_10de")
        || mfg_lower.contains("nvidia")
        || name_lower.contains("nvidia")
        || name_lower.contains("geforce")
        || name_lower.contains("quadro")
        || name_lower.contains("tesla")
        || name_lower.contains("rtx")
        || name_lower.contains("gtx")
}

/// 检查是否为 AMD 设备
fn is_amd_device(hardware_id: &str, manufacturer: &str, name: &str) -> bool {
    let hw_lower = hardware_id.to_lowercase();
    let mfg_lower = manufacturer.to_lowercase();
    let name_lower = name.to_lowercase();

    // AMD 硬件 ID 以 PCI\VEN_1002 开头
    hw_lower.contains("ven_1002")
        || mfg_lower.contains("amd")
        || mfg_lower.contains("ati")
        || name_lower.contains("radeon")
        || name_lower.contains("amd")
}

/// 检查是否为 Intel 设备
fn is_intel_device(hardware_id: &str, manufacturer: &str, name: &str) -> bool {
    let hw_lower = hardware_id.to_lowercase();
    let mfg_lower = manufacturer.to_lowercase();
    let name_lower = name.to_lowercase();

    // Intel 硬件 ID 以 PCI\VEN_8086 开头
    hw_lower.contains("ven_8086")
        || mfg_lower.contains("intel")
        || name_lower.contains("intel")
        || name_lower.contains("uhd")
        || name_lower.contains("iris")
}

/// 美化 GPU 名称
pub fn beautify_gpu_name(name: &str) -> String {
    let mut result = name.to_string();

    // 替换制造商名称为中文
    if result.to_lowercase().contains("nvidia") {
        result = result
            .replace("NVIDIA", "英伟达")
            .replace("nvidia", "英伟达")
            .replace("Nvidia", "英伟达");
    }
    if result.to_lowercase().contains("intel") && !result.contains("英特尔") {
        result = result
            .replace("Intel", "英特尔")
            .replace("intel", "英特尔")
            .replace("INTEL", "英特尔");
    }
    if result.to_lowercase().contains("advanced micro devices")
        || result.to_lowercase().contains("amd")
    {
        result = result
            .replace("Advanced Micro Devices, Inc.", "AMD")
            .replace("Advanced Micro Devices", "AMD");
    }

    result
}

// ============================================================================
// WinAPI 实现
// ============================================================================

#[cfg(windows)]
/// 枚举所有显卡设备
pub fn enumerate_gpu_devices() -> Result<Vec<GpuDeviceInfo>> {
    use std::mem::size_of;

    let mut devices = Vec::new();

    unsafe {
        // 获取所有设备
        let dev_info = SetupDiGetClassDevsW(
            None,
            PCWSTR::null(),
            HWND::default(),
            DIGCF_PRESENT | DIGCF_ALLCLASSES,
        )?;

        if dev_info.is_invalid() {
            bail!("SetupDiGetClassDevsW 失败");
        }

        let mut dev_info_data = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };

        let mut index = 0u32;

        loop {
            // 使用 is_err() 替代 !.as_bool()
            if SetupDiEnumDeviceInfo(dev_info, index, &mut dev_info_data).is_err() {
                let err = GetLastError();
                if err.0 == ERROR_NO_MORE_ITEMS.0 as u32 {
                    break;
                }
                index += 1;
                continue;
            }

            // 获取设备类
            let device_class = get_device_registry_property_string(dev_info, &dev_info_data, SPDRP_CLASS)
                .unwrap_or_default();

            // 只处理显示适配器
            if device_class.to_lowercase() == "display" {
                let name = get_device_registry_property_string(dev_info, &dev_info_data, SPDRP_DEVICEDESC)
                    .unwrap_or_default();
                let friendly_name = get_device_registry_property_string(dev_info, &dev_info_data, SPDRP_FRIENDLYNAME)
                    .unwrap_or_default();
                let hardware_id = get_device_registry_property_string(dev_info, &dev_info_data, SPDRP_HARDWAREID)
                    .unwrap_or_default();
                let manufacturer = get_device_registry_property_string(dev_info, &dev_info_data, SPDRP_MFG)
                    .unwrap_or_default();
                let driver_key = get_device_registry_property_string(dev_info, &dev_info_data, SPDRP_DRIVER)
                    .unwrap_or_default();

                // 获取设备实例 ID
                let instance_id = get_device_instance_id(dev_info, &dev_info_data).unwrap_or_default();

                let display_name = if !friendly_name.is_empty() {
                    friendly_name.clone()
                } else {
                    name.clone()
                };

                let is_nvidia = is_nvidia_device(&hardware_id, &manufacturer, &display_name);
                let is_amd = is_amd_device(&hardware_id, &manufacturer, &display_name);
                let is_intel = is_intel_device(&hardware_id, &manufacturer, &display_name);

                devices.push(GpuDeviceInfo {
                    name,
                    friendly_name,
                    hardware_id,
                    instance_id,
                    manufacturer,
                    driver_key,
                    device_class,
                    device_index: devices.len() as u32,
                    is_nvidia,
                    is_amd,
                    is_intel,
                });
            }

            index += 1;
        }

        let _ = SetupDiDestroyDeviceInfoList(dev_info);
    }

    Ok(devices)
}

#[cfg(windows)]
/// 获取设备注册表属性（字符串）
fn get_device_registry_property_string(
    dev_info: HDEVINFO,
    dev_info_data: &SP_DEVINFO_DATA,
    property: SETUP_DI_REGISTRY_PROPERTY,
) -> Option<String> {
    unsafe {
        let mut buffer = vec![0u8; 4096];
        let mut required_size = 0u32;
        let mut reg_data_type = 0u32;

        let result = SetupDiGetDeviceRegistryPropertyW(
            dev_info,
            dev_info_data,
            property,
            Some(&mut reg_data_type),
            Some(&mut buffer),
            Some(&mut required_size),
        );

        // 使用 is_ok() 替代 as_bool()
        if result.is_ok() {
            // 字符串类型
            if reg_data_type == 1 || reg_data_type == 7 {
                // REG_SZ or REG_MULTI_SZ
                let wide: Vec<u16> = buffer[..required_size as usize]
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                return Some(wide_to_string(&wide));
            }
        }

        None
    }
}

#[cfg(windows)]
/// 获取设备实例 ID
fn get_device_instance_id(dev_info: HDEVINFO, dev_info_data: &SP_DEVINFO_DATA) -> Option<String> {
    unsafe {
        let mut buffer = vec![0u16; 512];
        let mut required_size = 0u32;

        let result = SetupDiGetDeviceInstanceIdW(
            dev_info,
            dev_info_data,
            Some(&mut buffer),
            Some(&mut required_size),
        );

        // 使用 is_ok() 替代 as_bool()
        if result.is_ok() {
            return Some(wide_to_string(&buffer));
        }

        None
    }
}

#[cfg(windows)]
/// 获取系统硬件摘要信息
pub fn get_system_hardware_summary() -> Result<SystemHardwareSummary> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let gpu_devices = enumerate_gpu_devices()?;

    // 获取 CPU 信息
    let cpu_name = get_cpu_name().unwrap_or_else(|| "未知CPU".to_string());

    // 获取内存信息
    let mut mem_info = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };

    let (memory_size, memory_available) = unsafe {
        // 使用 is_ok() 替代 as_bool()
        if GlobalMemoryStatusEx(&mut mem_info).is_ok() {
            (mem_info.ullTotalPhys, mem_info.ullAvailPhys)
        } else {
            (0, 0)
        }
    };

    Ok(SystemHardwareSummary {
        gpu_devices,
        cpu_name,
        memory_size,
        memory_available,
    })
}

#[cfg(windows)]
/// 获取 CPU 名称
fn get_cpu_name() -> Option<String> {
    use windows::Win32::System::Registry::{RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, KEY_READ, REG_VALUE_TYPE};

    unsafe {
        let subkey = to_wide(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0");
        let value_name = to_wide("ProcessorNameString");

        let mut key_handle: HKEY = HKEY::default();
        if RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            0,
            KEY_READ,
            &mut key_handle,
        )
        .is_err()
        {
            return None;
        }

        let mut buffer = vec![0u8; 256];
        let mut buffer_size = buffer.len() as u32;
        let mut value_type = REG_VALUE_TYPE(0);

        let result = RegQueryValueExW(
            key_handle,
            PCWSTR(value_name.as_ptr()),
            None,
            Some(&mut value_type),
            Some(buffer.as_mut_ptr()),
            Some(&mut buffer_size),
        );

        let _ = RegCloseKey(key_handle);

        if result.is_err() {
            return None;
        }

        // REG_SZ
        if value_type.0 == 1 {
            let wide: Vec<u16> = buffer[..buffer_size as usize]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            return Some(wide_to_string(&wide).trim().to_string());
        }

        None
    }
}

#[cfg(windows)]
/// 卸载英伟达显卡驱动（在线系统）
pub fn uninstall_nvidia_drivers_online() -> Result<UninstallResult> {
    use std::mem::size_of;

    let mut result = UninstallResult::default();
    let mut uninstalled = 0usize;
    let mut failed = 0usize;

    unsafe {
        let dev_info = SetupDiGetClassDevsW(
            None,
            PCWSTR::null(),
            HWND::default(),
            DIGCF_PRESENT | DIGCF_ALLCLASSES,
        )?;

        if dev_info.is_invalid() {
            bail!("SetupDiGetClassDevsW 失败");
        }

        let mut dev_info_data = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };

        let mut index = 0u32;
        let mut nvidia_devices: Vec<SP_DEVINFO_DATA> = Vec::new();

        // 首先收集所有英伟达设备
        loop {
            // 使用 is_err() 替代 !.as_bool()
            if SetupDiEnumDeviceInfo(dev_info, index, &mut dev_info_data).is_err() {
                let err = GetLastError();
                if err.0 == ERROR_NO_MORE_ITEMS.0 as u32 {
                    break;
                }
                index += 1;
                continue;
            }

            let device_class = get_device_registry_property_string(dev_info, &dev_info_data, SPDRP_CLASS)
                .unwrap_or_default();

            if device_class.to_lowercase() == "display" {
                let hardware_id = get_device_registry_property_string(dev_info, &dev_info_data, SPDRP_HARDWAREID)
                    .unwrap_or_default();
                let manufacturer = get_device_registry_property_string(dev_info, &dev_info_data, SPDRP_MFG)
                    .unwrap_or_default();
                let name = get_device_registry_property_string(dev_info, &dev_info_data, SPDRP_DEVICEDESC)
                    .unwrap_or_default();

                if is_nvidia_device(&hardware_id, &manufacturer, &name) {
                    nvidia_devices.push(dev_info_data);
                    println!("[NvidiaUninstall] 找到英伟达设备: {}", name);
                }
            }

            index += 1;
        }

        if nvidia_devices.is_empty() {
            let _ = SetupDiDestroyDeviceInfoList(dev_info);
            result.success = true;
            result.message = "未找到英伟达显卡设备".to_string();
            return Ok(result);
        }

        // 卸载每个英伟达设备
        for mut device_data in nvidia_devices {
            let name = get_device_registry_property_string(dev_info, &device_data, SPDRP_DEVICEDESC)
                .unwrap_or_else(|| "未知设备".to_string());

            println!("[NvidiaUninstall] 正在卸载: {}", name);

            // 方法1：尝试使用 SetupDiRemoveDevice - 返回 BOOL 类型
            if SetupDiRemoveDevice(dev_info, &mut device_data).as_bool() {
                println!("[NvidiaUninstall] 成功卸载: {}", name);
                uninstalled += 1;
                result.needs_reboot = true;
            } else {
                // 方法2：尝试禁用设备
                let params = SP_PROPCHANGE_PARAMS {
                    ClassInstallHeader: SP_CLASSINSTALL_HEADER {
                        cbSize: size_of::<SP_CLASSINSTALL_HEADER>() as u32,
                        InstallFunction: DIF_PROPERTYCHANGE,
                    },
                    StateChange: DICS_DISABLE,
                    Scope: DICS_FLAG_GLOBAL,
                    HwProfile: 0,
                };

                let params_size = size_of::<SP_PROPCHANGE_PARAMS>() as u32;

                // 使用 is_ok() 替代 as_bool()
                if SetupDiSetClassInstallParamsW(
                    dev_info,
                    Some(&device_data),
                    Some(&params.ClassInstallHeader),
                    params_size,
                )
                .is_ok()
                {
                    // 使用 is_ok() 替代 as_bool()
                    if SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, dev_info, Some(&device_data))
                        .is_ok()
                    {
                        println!("[NvidiaUninstall] 已禁用设备: {}", name);
                        uninstalled += 1;
                        result.needs_reboot = true;
                    } else {
                        println!(
                            "[NvidiaUninstall] 禁用失败: {} (错误: {:?})",
                            name,
                            GetLastError()
                        );
                        failed += 1;
                    }
                } else {
                    println!(
                        "[NvidiaUninstall] 设置参数失败: {} (错误: {:?})",
                        name,
                        GetLastError()
                    );
                    failed += 1;
                }
            }
        }

        let _ = SetupDiDestroyDeviceInfoList(dev_info);
    }

    result.uninstalled_count = uninstalled;
    result.failed_count = failed;
    result.success = uninstalled > 0;

    if uninstalled > 0 && failed == 0 {
        result.message = format!("成功卸载 {} 个英伟达驱动", uninstalled);
    } else if uninstalled > 0 && failed > 0 {
        result.message = format!("卸载完成: 成功 {}, 失败 {}", uninstalled, failed);
    } else {
        result.message = "卸载失败，请尝试手动卸载".to_string();
    }

    Ok(result)
}

#[cfg(windows)]
/// 卸载英伟达显卡驱动（离线系统）
pub fn uninstall_nvidia_drivers_offline(target_partition: &str) -> Result<UninstallResult> {
    let mut result = UninstallResult::default();

    let partition = target_partition.trim_end_matches('\\');

    // 离线卸载通过删除驱动存储中的文件实现
    let driver_store = format!(
        "{}\\Windows\\System32\\DriverStore\\FileRepository",
        partition
    );
    let driver_store_path = Path::new(&driver_store);

    if !driver_store_path.exists() {
        result.message = format!("驱动存储目录不存在: {}", driver_store);
        return Ok(result);
    }

    let mut removed_count = 0usize;
    let mut failed_count = 0usize;

    // 英伟达驱动相关的目录名模式
    let nvidia_patterns = [
        "nv",          // nvlddmkm, nvdisplay 等
        "nvidia",      // nvidia 驱动
        "nvd",         // nvd 开头的驱动
        "nvmodules",   // nvmodules
        "nvcontainer", // nvcontainer
    ];

    if let Ok(entries) = std::fs::read_dir(driver_store_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let dir_name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_lowercase(),
                None => continue,
            };

            // 检查是否为英伟达驱动目录
            let is_nvidia = nvidia_patterns.iter().any(|p| dir_name.starts_with(p))
                || dir_name.contains("nvidia")
                || dir_name.contains("nvlddmkm")
                || dir_name.contains("nvdisplay");

            if is_nvidia {
                println!("[NvidiaUninstall] 删除离线驱动目录: {}", dir_name);

                match remove_directory_recursive(&path) {
                    Ok(_) => {
                        removed_count += 1;
                        println!("[NvidiaUninstall] 成功删除: {}", dir_name);
                    }
                    Err(e) => {
                        failed_count += 1;
                        println!("[NvidiaUninstall] 删除失败: {} - {}", dir_name, e);
                    }
                }
            }
        }
    }

    // 同时清理 INF 目录中的英伟达 INF 文件
    let inf_dir = format!("{}\\Windows\\INF", partition);
    let inf_path = Path::new(&inf_dir);

    if inf_path.exists() {
        if let Ok(entries) = std::fs::read_dir(inf_path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }

                if let Some(ext) = path.extension() {
                    if ext.to_ascii_lowercase() == "inf" {
                        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                            let name_lower = name.to_lowercase();
                            // 检查是否为英伟达 INF 文件
                            if name_lower.starts_with("nv") || name_lower.contains("nvidia") {
                                // 尝试读取文件内容确认
                                if is_nvidia_inf_file(&path) {
                                    println!("[NvidiaUninstall] 删除INF文件: {}", name);
                                    if std::fs::remove_file(&path).is_ok() {
                                        // 同时删除对应的 PNF 文件
                                        let pnf_path = path.with_extension("pnf");
                                        let _ = std::fs::remove_file(&pnf_path);
                                        removed_count += 1;
                                    } else {
                                        failed_count += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    result.uninstalled_count = removed_count;
    result.failed_count = failed_count;
    result.success = removed_count > 0;

    if removed_count > 0 && failed_count == 0 {
        result.message = format!("成功删除 {} 个英伟达驱动文件/目录", removed_count);
    } else if removed_count > 0 && failed_count > 0 {
        result.message = format!("删除完成: 成功 {}, 失败 {}", removed_count, failed_count);
    } else if failed_count > 0 {
        result.message = format!("删除失败: {} 个文件无法删除", failed_count);
    } else {
        result.message = "未找到英伟达驱动文件".to_string();
    }

    Ok(result)
}

/// 检查 INF 文件是否为英伟达驱动
fn is_nvidia_inf_file(path: &Path) -> bool {
    if let Ok(content) = std::fs::read_to_string(path) {
        let content_lower = content.to_lowercase();
        return content_lower.contains("nvidia")
            || content_lower.contains("ven_10de")
            || content_lower.contains("nvlddmkm")
            || content_lower.contains("nvdisplay")
            || content_lower.contains("geforce")
            || content_lower.contains("quadro");
    }
    false
}

/// 递归删除目录
fn remove_directory_recursive(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    // 首先尝试直接删除
    if std::fs::remove_dir_all(path).is_ok() {
        return Ok(());
    }

    // 如果失败，尝试修改权限后删除
    fn make_writable(path: &Path) -> std::io::Result<()> {
        if path.is_dir() {
            for entry in std::fs::read_dir(path)? {
                let entry = entry?;
                make_writable(&entry.path())?;
            }
        }

        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_readonly(false);
        std::fs::set_permissions(path, perms)?;
        Ok(())
    }

    let _ = make_writable(path);

    // 再次尝试删除
    std::fs::remove_dir_all(path).context("无法删除目录")
}

// ============================================================================
// 非 Windows 平台的存根实现
// ============================================================================

#[cfg(not(windows))]
pub fn enumerate_gpu_devices() -> Result<Vec<GpuDeviceInfo>> {
    Ok(Vec::new())
}

#[cfg(not(windows))]
pub fn get_system_hardware_summary() -> Result<SystemHardwareSummary> {
    Ok(SystemHardwareSummary::default())
}

#[cfg(not(windows))]
pub fn uninstall_nvidia_drivers_online() -> Result<UninstallResult> {
    Ok(UninstallResult {
        success: false,
        message: "此功能仅支持 Windows 系统".to_string(),
        ..Default::default()
    })
}

#[cfg(not(windows))]
pub fn uninstall_nvidia_drivers_offline(_target_partition: &str) -> Result<UninstallResult> {
    Ok(UninstallResult {
        success: false,
        message: "此功能仅支持 Windows 系统".to_string(),
        ..Default::default()
    })
}

/// 格式化显示系统硬件摘要
pub fn format_hardware_summary(summary: &SystemHardwareSummary) -> String {
    let mut output = String::new();

    // 显卡信息
    for (i, gpu) in summary.gpu_devices.iter().enumerate() {
        let display_name = if !gpu.friendly_name.is_empty() {
            beautify_gpu_name(&gpu.friendly_name)
        } else {
            beautify_gpu_name(&gpu.name)
        };

        output.push_str(&format!("显卡{}型号: {}\n", i + 1, display_name));
        output.push_str(&format!("显卡{}硬件ID: {}\n", i + 1, gpu.hardware_id));
    }

    // 分隔线
    output.push_str("---------------------------------------------------------------------\n");

    // CPU 信息
    output.push_str(&format!("{}\n", summary.cpu_name));

    // 分隔线
    output.push_str("---------------------------------------------------------------------\n");

    // 内存信息
    let total_gb = summary.memory_size as f64 / (1024.0 * 1024.0 * 1024.0);
    let avail_gb = summary.memory_available as f64 / (1024.0 * 1024.0 * 1024.0);
    output.push_str(&format!(
        "内存大小: {:.0} GB ({:.1} GB可用)\n",
        total_gb.ceil(),
        avail_gb
    ));

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_nvidia_device() {
        assert!(is_nvidia_device("PCI\\VEN_10DE&DEV_1234", "", ""));
        assert!(is_nvidia_device("", "NVIDIA Corporation", ""));
        assert!(is_nvidia_device("", "", "NVIDIA GeForce RTX 4090"));
        assert!(!is_nvidia_device("PCI\\VEN_1002&DEV_1234", "", ""));
        assert!(!is_nvidia_device("", "AMD", "Radeon RX 7900"));
    }

    #[test]
    fn test_beautify_gpu_name() {
        assert_eq!(
            beautify_gpu_name("NVIDIA GeForce RTX 4090"),
            "英伟达 GeForce RTX 4090"
        );
        assert_eq!(beautify_gpu_name("Intel UHD Graphics"), "英特尔 UHD Graphics");
    }
}
