//! 系统工具模块
//!
//! 提供不依赖 DISM 的系统操作功能：
//! - 离线注册表读取 (advapi32.dll - RegLoadKey/RegUnLoadKey)
//! - 组件存储清理 (Task Scheduler API)
//! - 系统信息获取
//! - PE文件架构检测

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use anyhow::{bail, Result};

#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, LUID};
#[cfg(windows)]
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
#[cfg(windows)]
use windows::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES,
    SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
#[cfg(windows)]
use windows::Win32::System::Registry::{
    RegCloseKey, RegLoadKeyW, RegOpenKeyExW, RegQueryValueExW, RegUnLoadKeyW,
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_VALUE_TYPE,
};
#[cfg(windows)]
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

// ============================================================================
// 常量定义
// ============================================================================

const SE_RESTORE_NAME: &str = "SeRestorePrivilege";
const SE_BACKUP_NAME: &str = "SeBackupPrivilege";

/// 系统架构类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemArchitecture {
    X86,
    Amd64,
    Arm64,
    Unknown,
}

impl SystemArchitecture {
    /// 返回用于unattend.xml的processorArchitecture字符串
    pub fn as_unattend_str(&self) -> &'static str {
        match self {
            SystemArchitecture::X86 => "x86",
            SystemArchitecture::Amd64 => "amd64",
            SystemArchitecture::Arm64 => "arm64",
            SystemArchitecture::Unknown => "amd64", // 默认amd64
        }
    }
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 将字符串转换为以 NUL 结尾的 UTF-16 Vec
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
    String::from_utf16_lossy(&wide[..len])
}

/// 获取文件版本信息
#[cfg(windows)]
pub fn get_file_version(path: &Path) -> Option<(u16, u16, u16, u16)> {
    if !path.exists() {
        return None;
    }

    unsafe {
        let wide_path: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let mut handle: u32 = 0;
        let size = GetFileVersionInfoSizeW(PCWSTR::from_raw(wide_path.as_ptr()), Some(&mut handle));
        if size == 0 {
            return None;
        }

        let mut buffer = vec![0u8; size as usize];
        let result = GetFileVersionInfoW(
            PCWSTR::from_raw(wide_path.as_ptr()),
            0,
            size,
            buffer.as_mut_ptr() as *mut _,
        );
        if result.is_err() {
            return None;
        }

        let sub_block: Vec<u16> = "\\".encode_utf16().chain(std::iter::once(0)).collect();
        let mut info_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut info_len: u32 = 0;

        let result = VerQueryValueW(
            buffer.as_ptr() as *const _,
            PCWSTR::from_raw(sub_block.as_ptr()),
            &mut info_ptr,
            &mut info_len,
        );
        if !result.as_bool() || info_ptr.is_null() {
            return None;
        }

        #[repr(C)]
        struct VsFixedFileInfo {
            dw_signature: u32,
            dw_struc_version: u32,
            dw_file_version_ms: u32,
            dw_file_version_ls: u32,
            dw_product_version_ms: u32,
            dw_product_version_ls: u32,
        }

        let info = &*(info_ptr as *const VsFixedFileInfo);
        let major = (info.dw_file_version_ms >> 16) as u16;
        let minor = (info.dw_file_version_ms & 0xFFFF) as u16;
        let build = (info.dw_file_version_ls >> 16) as u16;
        let revision = (info.dw_file_version_ls & 0xFFFF) as u16;
        Some((major, minor, build, revision))
    }
}

#[cfg(not(windows))]
pub fn get_file_version(_path: &Path) -> Option<(u16, u16, u16, u16)> {
    None
}

/// 检测PE文件的CPU架构
/// 
/// 通过读取PE文件头来判断目标系统是32位还是64位
/// 
/// # 参数
/// - `path`: PE文件路径（通常是ntdll.dll或kernel32.dll）
/// 
/// # 返回
/// - `SystemArchitecture`: 系统架构枚举
pub fn get_pe_architecture(path: &Path) -> SystemArchitecture {
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};
    
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return SystemArchitecture::Unknown,
    };
    
    // 读取DOS头，检查MZ签名
    let mut dos_header = [0u8; 64];
    if file.read_exact(&mut dos_header).is_err() {
        return SystemArchitecture::Unknown;
    }
    
    // 检查MZ签名
    if dos_header[0] != b'M' || dos_header[1] != b'Z' {
        return SystemArchitecture::Unknown;
    }
    
    // 获取PE头偏移（位于DOS头的0x3C处）
    let pe_offset = u32::from_le_bytes([
        dos_header[0x3C],
        dos_header[0x3D],
        dos_header[0x3E],
        dos_header[0x3F],
    ]) as u64;
    
    // 定位到PE头
    if file.seek(SeekFrom::Start(pe_offset)).is_err() {
        return SystemArchitecture::Unknown;
    }
    
    // 读取PE签名和COFF头
    let mut pe_header = [0u8; 6];
    if file.read_exact(&mut pe_header).is_err() {
        return SystemArchitecture::Unknown;
    }
    
    // 检查PE签名 "PE\0\0"
    if pe_header[0] != b'P' || pe_header[1] != b'E' || pe_header[2] != 0 || pe_header[3] != 0 {
        return SystemArchitecture::Unknown;
    }
    
    // 读取Machine字段（COFF头的前2字节）
    let machine = u16::from_le_bytes([pe_header[4], pe_header[5]]);
    
    match machine {
        0x014c => SystemArchitecture::X86,      // IMAGE_FILE_MACHINE_I386
        0x8664 => SystemArchitecture::Amd64,    // IMAGE_FILE_MACHINE_AMD64
        0xAA64 => SystemArchitecture::Arm64,    // IMAGE_FILE_MACHINE_ARM64
        _ => SystemArchitecture::Unknown,
    }
}

/// 检测目标系统的架构
/// 
/// 通过检测系统目录下的ntdll.dll或kernel32.dll来判断架构
/// 
/// # 参数
/// - `system_root`: 系统根目录（如 "C:\\"）
/// 
/// # 返回
/// - `SystemArchitecture`: 系统架构
pub fn get_system_architecture(system_root: &str) -> SystemArchitecture {
    let system_root_path = Path::new(system_root);
    
    // 优先检测 ntdll.dll
    let ntdll_path = system_root_path
        .join("Windows")
        .join("System32")
        .join("ntdll.dll");
    
    if ntdll_path.exists() {
        let arch = get_pe_architecture(&ntdll_path);
        if arch != SystemArchitecture::Unknown {
            return arch;
        }
    }
    
    // 备选：检测 kernel32.dll
    let kernel32_path = system_root_path
        .join("Windows")
        .join("System32")
        .join("kernel32.dll");
    
    if kernel32_path.exists() {
        let arch = get_pe_architecture(&kernel32_path);
        if arch != SystemArchitecture::Unknown {
            return arch;
        }
    }
    
    // 默认返回 amd64
    SystemArchitecture::Amd64
}

// ============================================================================
// 权限提升
// ============================================================================

/// 启用指定权限
#[cfg(windows)]
fn enable_privilege(privilege_name: &str) -> Result<()> {
    unsafe {
        let mut token_handle = HANDLE::default();
        let process = GetCurrentProcess();

        // OpenProcessToken 返回 Result
        if let Err(e) = OpenProcessToken(process, TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token_handle) {
            bail!("OpenProcessToken 失败: {}", e);
        }

        let wide_name = to_wide(privilege_name);
        let mut luid = LUID::default();

        // LookupPrivilegeValueW 返回 Result
        if let Err(e) = LookupPrivilegeValueW(PCWSTR::null(), PCWSTR(wide_name.as_ptr()), &mut luid) {
            let _ = CloseHandle(token_handle);
            bail!("LookupPrivilegeValueW 失败: {}", e);
        }

        let tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };

        // AdjustTokenPrivileges 返回 Result
        if let Err(e) = AdjustTokenPrivileges(
            token_handle,
            false,
            Some(&tp),
            0,
            None,
            None,
        ) {
            let _ = CloseHandle(token_handle);
            bail!("AdjustTokenPrivileges 失败: {}", e);
        }

        // 检查 GetLastError，AdjustTokenPrivileges 即使成功也可能设置错误码
        let last_error = GetLastError();
        let _ = CloseHandle(token_handle);
        
        if last_error.0 != 0 && last_error.0 != 1300 {
            // 1300 = ERROR_NOT_ALL_ASSIGNED，表示部分权限未能分配，可以忽略
            println!("[SystemUtils] AdjustTokenPrivileges 警告: 错误码 {}", last_error.0);
        }
    }

    Ok(())
}

#[cfg(not(windows))]
fn enable_privilege(_privilege_name: &str) -> Result<()> {
    Ok(())
}

// ============================================================================
// 离线注册表操作
// ============================================================================

/// 离线系统信息
#[derive(Debug, Clone, Default)]
pub struct OfflineSystemInfo {
    /// 产品名称 (如 "Windows 11 Pro")
    pub product_name: String,
    /// 版本号 (如 "10.0")
    pub current_version: String,
    /// 构建号 (如 "22631")
    pub current_build: String,
    /// 显示版本 (如 "23H2")
    pub display_version: String,
    /// 版本 ID (如 "Professional")
    pub edition_id: String,
    /// 安装类型 (如 "Client")
    pub installation_type: String,
    /// 注册的所有者
    pub registered_owner: String,
    /// 注册的组织
    pub registered_organization: String,
    /// 系统根目录
    pub system_root: String,
    /// 路径名称
    pub path_name: String,
}

/// 从离线系统读取系统信息
/// 
/// # 参数
/// - `system_root`: 系统根目录 (如 "D:\\")
/// 
/// # 返回
/// - `OfflineSystemInfo`: 系统信息结构
#[cfg(windows)]
pub fn get_offline_system_info(system_root: &str) -> Result<OfflineSystemInfo> {
    // 需要 SeRestorePrivilege 和 SeBackupPrivilege 权限
    enable_privilege(SE_RESTORE_NAME)?;
    enable_privilege(SE_BACKUP_NAME)?;

    let system_root_path = Path::new(system_root);
    let software_hive = system_root_path
        .join("Windows")
        .join("System32")
        .join("config")
        .join("SOFTWARE");

    if !software_hive.exists() {
        bail!("SOFTWARE hive 不存在: {:?}", software_hive);
    }

    // 生成唯一的临时键名
    let temp_key_name = format!("OFFLINE_SOFTWARE_{}", std::process::id());
    let wide_key_name = to_wide(&temp_key_name);
    let wide_hive_path = path_to_wide(&software_hive);

    println!("[SystemUtils] 加载离线注册表: {:?}", software_hive);

    // 加载 hive 到 HKEY_LOCAL_MACHINE
    let load_result = unsafe {
        RegLoadKeyW(HKEY_LOCAL_MACHINE, PCWSTR(wide_key_name.as_ptr()), PCWSTR(wide_hive_path.as_ptr()))
    };

    if load_result.0 != 0 {
        bail!("RegLoadKeyW 失败: 错误码 {}", load_result.0);
    }

    // 确保在函数退出时卸载 hive
    struct HiveGuard<'a> {
        key_name: &'a [u16],
    }

    impl<'a> Drop for HiveGuard<'a> {
        fn drop(&mut self) {
            unsafe {
                let _ = RegUnLoadKeyW(HKEY_LOCAL_MACHINE, PCWSTR(self.key_name.as_ptr()));
            }
        }
    }

    let _guard = HiveGuard {
        key_name: &wide_key_name,
    };

    // 打开 CurrentVersion 键
    let subkey_path = format!(
        "{}\\Microsoft\\Windows NT\\CurrentVersion",
        temp_key_name
    );
    let wide_subkey = to_wide(&subkey_path);

    let mut hkey = HKEY::default();
    let open_result = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(wide_subkey.as_ptr()),
            0,
            KEY_READ,
            &mut hkey,
        )
    };

    if open_result.0 != 0 {
        bail!("RegOpenKeyExW 失败: 错误码 {}", open_result.0);
    }

    // 读取值的辅助函数
    fn read_reg_string(hkey: HKEY, value_name: &str) -> Option<String> {
        let wide_name = to_wide(value_name);
        let mut buffer = vec![0u8; 1024];
        let mut size = buffer.len() as u32;
        let mut reg_type = REG_VALUE_TYPE::default();

        let result = unsafe {
            RegQueryValueExW(
                hkey,
                PCWSTR(wide_name.as_ptr()),
                None,
                Some(&mut reg_type),
                Some(buffer.as_mut_ptr()),
                Some(&mut size),
            )
        };

        if result.0 == 0 && (reg_type.0 == 1 || reg_type.0 == 2) {
            // REG_SZ (1) 或 REG_EXPAND_SZ (2)
            let wide_slice = unsafe {
                std::slice::from_raw_parts(
                    buffer.as_ptr() as *const u16,
                    (size as usize) / 2,
                )
            };
            Some(wide_to_string(wide_slice))
        } else {
            None
        }
    }

    // 读取所有信息
    let info = OfflineSystemInfo {
        product_name: read_reg_string(hkey, "ProductName").unwrap_or_default(),
        current_version: read_reg_string(hkey, "CurrentVersion").unwrap_or_default(),
        current_build: read_reg_string(hkey, "CurrentBuild").unwrap_or_default(),
        display_version: read_reg_string(hkey, "DisplayVersion").unwrap_or_default(),
        edition_id: read_reg_string(hkey, "EditionID").unwrap_or_default(),
        installation_type: read_reg_string(hkey, "InstallationType").unwrap_or_default(),
        registered_owner: read_reg_string(hkey, "RegisteredOwner").unwrap_or_default(),
        registered_organization: read_reg_string(hkey, "RegisteredOrganization").unwrap_or_default(),
        system_root: read_reg_string(hkey, "SystemRoot").unwrap_or_default(),
        path_name: read_reg_string(hkey, "PathName").unwrap_or_default(),
    };

    // 关闭键
    unsafe {
        let _ = RegCloseKey(hkey);
    }

    println!("[SystemUtils] 读取到系统信息: {:?}", info);
    Ok(info)
}

#[cfg(not(windows))]
pub fn get_offline_system_info(_system_root: &str) -> Result<OfflineSystemInfo> {
    bail!("仅支持 Windows 平台")
}

/// 获取离线系统版本字符串（简化版）
pub fn get_offline_system_edition(system_root: &str) -> Result<String> {
    let info = get_offline_system_info(system_root)?;
    
    let version = if !info.display_version.is_empty() {
        format!(
            "{} {} (Build {})",
            info.product_name, info.display_version, info.current_build
        )
    } else {
        format!(
            "{} (Build {})",
            info.product_name, info.current_build
        )
    };

    Ok(version)
}

// ============================================================================
// 组件存储清理
// ============================================================================

/// 清理组件存储（WinSxS）
/// 
/// 使用 Task Scheduler 触发 StartComponentCleanup 任务
/// 这是 Microsoft 推荐的清理方式
#[cfg(windows)]
pub fn cleanup_component_store() -> Result<()> {
    use std::process::Command;

    println!("[SystemUtils] 触发组件存储清理任务...");

    // 方法1: 使用 schtasks.exe 触发已有任务
    let output = Command::new("schtasks.exe")
        .args(["/Run", "/TN", "\\Microsoft\\Windows\\Servicing\\StartComponentCleanup"])
        .output();

    match output {
        Ok(result) if result.status.success() => {
            println!("[SystemUtils] 组件清理任务已触发");
            return Ok(());
        }
        _ => {
            println!("[SystemUtils] schtasks 触发失败，尝试 cleanmgr...");
        }
    }

    // 方法2: 使用 cleanmgr.exe
    // /sagerun:1 使用预设配置
    let output = Command::new("cleanmgr.exe")
        .args(["/d", "C:", "/VERYLOWDISK"])
        .spawn();

    match output {
        Ok(_) => {
            println!("[SystemUtils] cleanmgr 已启动");
            Ok(())
        }
        Err(e) => {
            bail!("无法启动清理工具: {}", e);
        }
    }
}

#[cfg(not(windows))]
pub fn cleanup_component_store() -> Result<()> {
    bail!("仅支持 Windows 平台")
}

/// 清理离线系统的组件存储
/// 
/// 对于离线系统，清理以下临时目录：
/// - Windows\WinSxS\Temp
/// - Windows\Temp
/// - Windows\Prefetch
pub fn cleanup_offline_component_store(system_root: &str) -> Result<()> {
    let system_root_path = Path::new(system_root);

    // 需要清理的目录
    let cleanup_dirs = [
        system_root_path.join("Windows").join("WinSxS").join("Temp"),
        system_root_path.join("Windows").join("Temp"),
        system_root_path.join("Windows").join("Prefetch"),
        system_root_path.join("Windows").join("SoftwareDistribution").join("Download"),
    ];

    let mut cleaned_size: u64 = 0;

    for dir in &cleanup_dirs {
        if dir.exists() {
            println!("[SystemUtils] 清理目录: {:?}", dir);
            match cleanup_directory(dir) {
                Ok(size) => {
                    cleaned_size += size;
                }
                Err(e) => {
                    println!("[SystemUtils] 清理 {:?} 失败: {}", dir, e);
                }
            }
        }
    }

    println!(
        "[SystemUtils] 离线清理完成，释放空间: {:.2} MB",
        cleaned_size as f64 / 1024.0 / 1024.0
    );

    Ok(())
}

/// 清理目录中的文件（保留目录结构）
fn cleanup_directory(dir: &Path) -> Result<u64> {
    let mut total_size: u64 = 0;

    if !dir.exists() {
        return Ok(0);
    }

    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.is_file() {
            if let Ok(metadata) = path.metadata() {
                total_size += metadata.len();
            }
            let _ = std::fs::remove_file(path);
        }
    }

    Ok(total_size)
}

// ============================================================================
// 分析组件存储
// ============================================================================

/// 组件存储分析结果
#[derive(Debug, Clone, Default)]
pub struct ComponentStoreAnalysis {
    /// 实际大小 (字节)
    pub actual_size: u64,
    /// 共享文件大小 (字节)
    pub shared_with_windows: u64,
    /// 备份和已禁用功能大小 (字节)
    pub backups_and_disabled: u64,
    /// 缓存和临时数据大小 (字节)
    pub cache_and_temp: u64,
    /// 是否建议清理
    pub cleanup_recommended: bool,
}

/// 分析组件存储
/// 
/// 直接读取目录大小，而不是依赖 DISM
pub fn analyze_component_store(system_root: &str) -> Result<ComponentStoreAnalysis> {
    let system_root_path = Path::new(system_root);
    let winsxs_path = system_root_path.join("Windows").join("WinSxS");

    if !winsxs_path.exists() {
        bail!("WinSxS 目录不存在");
    }

    let temp_path = winsxs_path.join("Temp");
    let backup_path = winsxs_path.join("Backup");

    // 计算总大小
    let actual_size = get_dir_size(&winsxs_path)?;
    let cache_and_temp = get_dir_size(&temp_path).unwrap_or(0);
    let backups = get_dir_size(&backup_path).unwrap_or(0);

    // 如果临时文件超过 1GB，建议清理
    let cleanup_recommended = cache_and_temp > 1024 * 1024 * 1024;

    Ok(ComponentStoreAnalysis {
        actual_size,
        shared_with_windows: actual_size - cache_and_temp - backups,
        backups_and_disabled: backups,
        cache_and_temp,
        cleanup_recommended,
    })
}

/// 获取目录大小（递归）
fn get_dir_size(path: &Path) -> Result<u64> {
    let mut total: u64 = 0;

    if !path.exists() {
        return Ok(0);
    }

    for entry in walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.path().is_file() {
            if let Ok(metadata) = entry.metadata() {
                total += metadata.len();
            }
        }
    }

    Ok(total)
}

// ============================================================================
// 系统修复
// ============================================================================

/// 检查系统文件完整性
/// 
/// 使用 SFC (System File Checker) 扫描
#[cfg(windows)]
pub fn check_system_files() -> Result<bool> {
    use std::process::Command;

    println!("[SystemUtils] 运行系统文件检查...");

    let output = Command::new("sfc")
        .args(["/scannow"])
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    
    // 检查输出中是否包含 "did not find any integrity violations"
    let no_issues = stdout.contains("did not find any integrity violations")
        || stdout.contains("未发现完整性冲突")
        || stdout.contains("未發現完整性衝突");

    Ok(no_issues)
}

#[cfg(not(windows))]
pub fn check_system_files() -> Result<bool> {
    bail!("仅支持 Windows 平台")
}

/// 检查离线系统文件完整性
/// 
/// 扫描离线系统的关键文件是否存在
pub fn check_offline_system_files(system_root: &str) -> Result<bool> {
    let system_root_path = Path::new(system_root);

    // 关键文件列表
    let critical_files = [
        "Windows\\System32\\ntoskrnl.exe",
        "Windows\\System32\\winload.exe",
        "Windows\\System32\\config\\SYSTEM",
        "Windows\\System32\\config\\SOFTWARE",
        "Windows\\System32\\config\\SAM",
        "Windows\\System32\\config\\SECURITY",
        "Windows\\System32\\config\\DEFAULT",
        "Windows\\explorer.exe",
        "Windows\\System32\\kernel32.dll",
        "Windows\\System32\\user32.dll",
        "Windows\\System32\\ntdll.dll",
    ];

    let mut missing_files = Vec::new();

    for file in &critical_files {
        let full_path = system_root_path.join(file);
        if !full_path.exists() {
            missing_files.push(file.to_string());
        }
    }

    if !missing_files.is_empty() {
        println!("[SystemUtils] 缺失关键文件: {:?}", missing_files);
        return Ok(false);
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wide_conversion() {
        let s = "Hello World";
        let wide = to_wide(s);
        assert!(wide.ends_with(&[0]));
    }

    #[test]
    fn test_wide_to_string() {
        let wide = [72, 101, 108, 108, 111, 0]; // "Hello\0"
        let s = wide_to_string(&wide);
        assert_eq!(s, "Hello");
    }
}
