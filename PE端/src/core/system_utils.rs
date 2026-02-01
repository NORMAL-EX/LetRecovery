//! 系统工具函数模块
//!
//! 提供各种系统级别的工具函数，包括：
//! - Windows 版本检测
//! - 系统架构检测
//! - 临时目录管理
//! - PE 环境检测

use std::path::{Path, PathBuf};

// =============================================================================
// Windows 版本信息
// =============================================================================

/// Windows 版本信息
#[derive(Debug, Clone)]
pub struct WindowsVersion {
    /// 主版本号 (如 Windows 10 = 10)
    pub major: u32,
    /// 次版本号 (如 Windows 10 = 0)
    pub minor: u32,
    /// 构建号 (如 19041)
    pub build: u32,
    /// 版本字符串
    pub version_string: String,
    /// 产品名称
    pub product_name: String,
}

impl Default for WindowsVersion {
    fn default() -> Self {
        Self {
            major: 0,
            minor: 0,
            build: 0,
            version_string: String::new(),
            product_name: String::new(),
        }
    }
}

impl WindowsVersion {
    /// 是否为 Windows 7
    pub fn is_win7(&self) -> bool {
        self.major == 6 && self.minor == 1
    }

    /// 是否为 Windows 8
    pub fn is_win8(&self) -> bool {
        self.major == 6 && self.minor == 2
    }

    /// 是否为 Windows 8.1
    pub fn is_win81(&self) -> bool {
        self.major == 6 && self.minor == 3
    }

    /// 是否为 Windows 10
    pub fn is_win10(&self) -> bool {
        self.major == 10 && self.build < 22000
    }

    /// 是否为 Windows 11
    pub fn is_win11(&self) -> bool {
        self.major == 10 && self.build >= 22000
    }

    /// 是否为 Windows 10 或更高版本
    pub fn is_win10_or_later(&self) -> bool {
        self.major >= 10
    }

    /// 获取简化的版本名称
    pub fn short_name(&self) -> &'static str {
        if self.is_win11() {
            "Windows 11"
        } else if self.is_win10() {
            "Windows 10"
        } else if self.is_win81() {
            "Windows 8.1"
        } else if self.is_win8() {
            "Windows 8"
        } else if self.is_win7() {
            "Windows 7"
        } else {
            "Windows"
        }
    }
}

/// 获取当前系统的 Windows 版本
#[cfg(windows)]
pub fn get_windows_version() -> WindowsVersion {
    use windows::Win32::System::SystemInformation::{
        GetVersionExW, OSVERSIONINFOEXW,
    };

    let mut version = WindowsVersion::default();

    unsafe {
        let mut osvi: OSVERSIONINFOEXW = std::mem::zeroed();
        osvi.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOEXW>() as u32;

        #[allow(deprecated)]
        if GetVersionExW(&mut osvi as *mut _ as *mut _).is_ok() {
            version.major = osvi.dwMajorVersion;
            version.minor = osvi.dwMinorVersion;
            version.build = osvi.dwBuildNumber;
        }
    }

    // 尝试从注册表获取更详细的信息
    if let Ok(output) = std::process::Command::new("reg")
        .args([
            "query",
            r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion",
            "/v",
            "ProductName",
        ])
        .output()
    {
        let output_str = String::from_utf8_lossy(&output.stdout);
        if let Some(line) = output_str.lines().find(|l| l.contains("ProductName")) {
            if let Some(value) = line.split("REG_SZ").nth(1) {
                version.product_name = value.trim().to_string();
            }
        }
    }

    version.version_string = format!(
        "{}.{}.{}",
        version.major, version.minor, version.build
    );

    version
}

#[cfg(not(windows))]
pub fn get_windows_version() -> WindowsVersion {
    WindowsVersion::default()
}

/// 从离线系统获取 Windows 版本
///
/// # 参数
/// - `system_root`: 系统根目录 (如 "D:\\")
pub fn get_offline_windows_version(system_root: &Path) -> Option<WindowsVersion> {
    // 通过读取 ntoskrnl.exe 的版本信息来检测
    let kernel_path = system_root
        .join("Windows")
        .join("System32")
        .join("ntoskrnl.exe");

    if !kernel_path.exists() {
        return None;
    }

    // 尝试使用 PowerShell 获取版本信息
    let ps_script = format!(
        "(Get-Item '{}').VersionInfo | ConvertTo-Json",
        kernel_path.to_string_lossy()
    );

    if let Ok(output) = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &ps_script])
        .output()
    {
        let output_str = String::from_utf8_lossy(&output.stdout);
        
        // 简单解析 JSON
        let mut version = WindowsVersion::default();
        
        for line in output_str.lines() {
            let line = line.trim();
            if line.contains("\"FileMajorPart\"") {
                if let Some(val) = extract_json_number(line) {
                    version.major = val;
                }
            } else if line.contains("\"FileMinorPart\"") {
                if let Some(val) = extract_json_number(line) {
                    version.minor = val;
                }
            } else if line.contains("\"FileBuildPart\"") {
                if let Some(val) = extract_json_number(line) {
                    version.build = val;
                }
            } else if line.contains("\"ProductName\"") {
                if let Some(val) = extract_json_string(line) {
                    version.product_name = val;
                }
            }
        }

        if version.major > 0 {
            version.version_string = format!(
                "{}.{}.{}",
                version.major, version.minor, version.build
            );
            return Some(version);
        }
    }

    None
}

/// 获取文件的版本信息
/// 
/// 返回 (major, minor, build, revision) 元组
/// 
/// # 参数
/// - `path`: 文件路径
pub fn get_file_version(path: &Path) -> Option<(u32, u32, u32, u32)> {
    // 方法1: 尝试使用 PowerShell 获取文件版本
    if let Some(version) = get_file_version_via_powershell(path) {
        return Some(version);
    }
    
    // 方法2: 直接从 PE 资源段读取版本信息
    get_file_version_from_pe(path)
}

/// 通过 PowerShell 获取文件版本
fn get_file_version_via_powershell(path: &Path) -> Option<(u32, u32, u32, u32)> {
    let ps_script = format!(
        "$v = (Get-Item '{}' -ErrorAction SilentlyContinue).VersionInfo; if($v) {{ Write-Output \"$($v.FileMajorPart),$($v.FileMinorPart),$($v.FileBuildPart),$($v.FilePrivatePart)\" }}",
        path.to_string_lossy().replace('\'', "''")
    );
    
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps_script])
        .output()
        .ok()?;
    
    if !output.status.success() {
        return None;
    }
    
    let output_str = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = output_str.trim().split(',').collect();
    
    if parts.len() == 4 {
        let major = parts[0].parse().ok()?;
        let minor = parts[1].parse().ok()?;
        let build = parts[2].parse().ok()?;
        let revision = parts[3].parse().ok()?;
        return Some((major, minor, build, revision));
    }
    
    None
}

/// 从 PE 文件资源段直接读取版本信息
fn get_file_version_from_pe(path: &Path) -> Option<(u32, u32, u32, u32)> {
    let data = std::fs::read(path).ok()?;
    
    // 验证 DOS 头
    if data.len() < 0x40 || &data[0..2] != b"MZ" {
        return None;
    }
    
    // 获取 PE 头偏移
    let pe_offset = u32::from_le_bytes([data[0x3C], data[0x3D], data[0x3E], data[0x3F]]) as usize;
    
    if data.len() < pe_offset + 4 || &data[pe_offset..pe_offset + 4] != b"PE\0\0" {
        return None;
    }
    
    // COFF 文件头
    let coff_header_offset = pe_offset + 4;
    if data.len() < coff_header_offset + 20 {
        return None;
    }
    
    let num_sections = u16::from_le_bytes([data[coff_header_offset + 2], data[coff_header_offset + 3]]) as usize;
    let optional_header_size = u16::from_le_bytes([data[coff_header_offset + 16], data[coff_header_offset + 17]]) as usize;
    
    // 可选头
    let optional_header_offset = coff_header_offset + 20;
    if data.len() < optional_header_offset + optional_header_size {
        return None;
    }
    
    // 判断是 PE32 还是 PE32+
    let magic = u16::from_le_bytes([data[optional_header_offset], data[optional_header_offset + 1]]);
    let (data_dir_offset, num_data_dirs) = match magic {
        0x10b => (optional_header_offset + 96, 16usize),  // PE32
        0x20b => (optional_header_offset + 112, 16usize), // PE32+
        _ => return None,
    };
    
    // 资源目录是数据目录的第3项 (索引2)
    if num_data_dirs < 3 {
        return None;
    }
    
    let resource_dir_rva_offset = data_dir_offset + 2 * 8;
    if data.len() < resource_dir_rva_offset + 8 {
        return None;
    }
    
    let resource_rva = u32::from_le_bytes([
        data[resource_dir_rva_offset],
        data[resource_dir_rva_offset + 1],
        data[resource_dir_rva_offset + 2],
        data[resource_dir_rva_offset + 3],
    ]) as usize;
    
    if resource_rva == 0 {
        return None;
    }
    
    // 读取节表找到资源节
    let section_table_offset = optional_header_offset + optional_header_size;
    
    for i in 0..num_sections {
        let section_offset = section_table_offset + i * 40;
        if data.len() < section_offset + 40 {
            continue;
        }
        
        let virtual_address = u32::from_le_bytes([
            data[section_offset + 12],
            data[section_offset + 13],
            data[section_offset + 14],
            data[section_offset + 15],
        ]) as usize;
        
        let virtual_size = u32::from_le_bytes([
            data[section_offset + 8],
            data[section_offset + 9],
            data[section_offset + 10],
            data[section_offset + 11],
        ]) as usize;
        
        let raw_data_ptr = u32::from_le_bytes([
            data[section_offset + 20],
            data[section_offset + 21],
            data[section_offset + 22],
            data[section_offset + 23],
        ]) as usize;
        
        // 检查资源 RVA 是否在这个节内
        if resource_rva >= virtual_address && resource_rva < virtual_address + virtual_size {
            let resource_file_offset = raw_data_ptr + (resource_rva - virtual_address);
            return parse_version_resource(&data, resource_file_offset, raw_data_ptr, virtual_address);
        }
    }
    
    None
}

/// 解析版本资源
fn parse_version_resource(data: &[u8], resource_offset: usize, section_raw: usize, section_rva: usize) -> Option<(u32, u32, u32, u32)> {
    // 遍历资源目录查找 VS_VERSION_INFO (类型 16)
    if data.len() < resource_offset + 16 {
        return None;
    }
    
    let num_named_entries = u16::from_le_bytes([data[resource_offset + 12], data[resource_offset + 13]]) as usize;
    let num_id_entries = u16::from_le_bytes([data[resource_offset + 14], data[resource_offset + 15]]) as usize;
    
    let entries_offset = resource_offset + 16;
    
    for i in 0..(num_named_entries + num_id_entries) {
        let entry_offset = entries_offset + i * 8;
        if data.len() < entry_offset + 8 {
            continue;
        }
        
        let id = u32::from_le_bytes([
            data[entry_offset],
            data[entry_offset + 1],
            data[entry_offset + 2],
            data[entry_offset + 3],
        ]);
        
        let offset_or_dir = u32::from_le_bytes([
            data[entry_offset + 4],
            data[entry_offset + 5],
            data[entry_offset + 6],
            data[entry_offset + 7],
        ]);
        
        // RT_VERSION = 16
        if id == 16 && (offset_or_dir & 0x80000000) != 0 {
            let sub_dir_offset = resource_offset.wrapping_add((offset_or_dir & 0x7FFFFFFF) as usize);
            if let Some(version) = find_version_in_subdir(data, sub_dir_offset, resource_offset, section_raw, section_rva) {
                return Some(version);
            }
        }
    }
    
    None
}

/// 在子目录中查找版本信息
fn find_version_in_subdir(data: &[u8], dir_offset: usize, resource_base: usize, section_raw: usize, section_rva: usize) -> Option<(u32, u32, u32, u32)> {
    if data.len() < dir_offset + 16 {
        return None;
    }
    
    let num_named = u16::from_le_bytes([data[dir_offset + 12], data[dir_offset + 13]]) as usize;
    let num_id = u16::from_le_bytes([data[dir_offset + 14], data[dir_offset + 15]]) as usize;
    
    for i in 0..(num_named + num_id) {
        let entry_offset = dir_offset + 16 + i * 8;
        if data.len() < entry_offset + 8 {
            continue;
        }
        
        let offset_or_dir = u32::from_le_bytes([
            data[entry_offset + 4],
            data[entry_offset + 5],
            data[entry_offset + 6],
            data[entry_offset + 7],
        ]);
        
        if (offset_or_dir & 0x80000000) != 0 {
            // 还是目录，继续递归
            let sub_offset = resource_base.wrapping_add((offset_or_dir & 0x7FFFFFFF) as usize);
            if let Some(v) = find_version_in_subdir(data, sub_offset, resource_base, section_raw, section_rva) {
                return Some(v);
            }
        } else {
            // 数据入口
            let data_entry_offset = resource_base.wrapping_add(offset_or_dir as usize);
            if data.len() < data_entry_offset + 16 {
                continue;
            }
            
            let data_rva = u32::from_le_bytes([
                data[data_entry_offset],
                data[data_entry_offset + 1],
                data[data_entry_offset + 2],
                data[data_entry_offset + 3],
            ]) as usize;
            
            let data_size = u32::from_le_bytes([
                data[data_entry_offset + 4],
                data[data_entry_offset + 5],
                data[data_entry_offset + 6],
                data[data_entry_offset + 7],
            ]) as usize;
            
            // 转换 RVA 到文件偏移
            let data_file_offset = section_raw + (data_rva - section_rva);
            
            if data.len() >= data_file_offset + data_size && data_size >= 52 {
                // 解析 VS_FIXEDFILEINFO
                // 跳过 VS_VERSION_INFO 头部, 查找 VS_FIXEDFILEINFO 签名 0xFEEF04BD
                for offset in (0..data_size.saturating_sub(52)).step_by(4) {
                    let pos = data_file_offset + offset;
                    if data.len() < pos + 52 {
                        break;
                    }
                    
                    let signature = u32::from_le_bytes([
                        data[pos], data[pos + 1], data[pos + 2], data[pos + 3]
                    ]);
                    
                    if signature == 0xFEEF04BD {
                        // 找到 VS_FIXEDFILEINFO
                        let file_version_ms = u32::from_le_bytes([
                            data[pos + 8], data[pos + 9], data[pos + 10], data[pos + 11]
                        ]);
                        let file_version_ls = u32::from_le_bytes([
                            data[pos + 12], data[pos + 13], data[pos + 14], data[pos + 15]
                        ]);
                        
                        let major = (file_version_ms >> 16) & 0xFFFF;
                        let minor = file_version_ms & 0xFFFF;
                        let build = (file_version_ls >> 16) & 0xFFFF;
                        let revision = file_version_ls & 0xFFFF;
                        
                        return Some((major, minor, build, revision));
                    }
                }
            }
        }
    }
    
    None
}

/// 从 JSON 行中提取数字值
fn extract_json_number(line: &str) -> Option<u32> {
    line.split(':')
        .nth(1)?
        .trim()
        .trim_end_matches(',')
        .parse()
        .ok()
}

/// 从 JSON 行中提取字符串值
fn extract_json_string(line: &str) -> Option<String> {
    let value = line.split(':').nth(1)?.trim();
    let value = value.trim_start_matches('"').trim_end_matches('"');
    let value = value.trim_end_matches(',');
    let value = value.trim_end_matches('"');
    Some(value.to_string())
}

// =============================================================================
// 系统架构
// =============================================================================

/// 系统架构
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemArchitecture {
    X86,
    X64,
    Arm64,
    Unknown,
}

impl SystemArchitecture {
    /// 获取架构名称
    pub fn name(&self) -> &'static str {
        match self {
            SystemArchitecture::X86 => "x86",
            SystemArchitecture::X64 => "amd64",
            SystemArchitecture::Arm64 => "arm64",
            SystemArchitecture::Unknown => "unknown",
        }
    }

    /// 获取处理器架构字符串 (用于 unattend.xml)
    pub fn processor_architecture(&self) -> &'static str {
        match self {
            SystemArchitecture::X86 => "x86",
            SystemArchitecture::X64 => "amd64",
            SystemArchitecture::Arm64 => "arm64",
            SystemArchitecture::Unknown => "amd64",
        }
    }

    /// 获取用于 unattend.xml 的架构字符串
    /// 这是 processor_architecture 的别名，提供更明确的命名
    #[inline]
    pub fn as_unattend_str(&self) -> &'static str {
        self.processor_architecture()
    }
}

/// 获取当前系统架构
pub fn get_system_architecture() -> SystemArchitecture {
    #[cfg(target_arch = "x86_64")]
    {
        SystemArchitecture::X64
    }

    #[cfg(target_arch = "x86")]
    {
        // 检查是否在 WoW64 下运行
        if is_wow64() {
            SystemArchitecture::X64
        } else {
            SystemArchitecture::X86
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        SystemArchitecture::Arm64
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64")))]
    {
        SystemArchitecture::Unknown
    }
}

/// 检测离线系统的架构
pub fn get_offline_system_architecture(system_root: &Path) -> SystemArchitecture {
    // 检查 System32 目录下的 kernel32.dll 是 32 位还是 64 位
    let kernel32_path = system_root
        .join("Windows")
        .join("System32")
        .join("kernel32.dll");

    if let Ok(data) = std::fs::read(&kernel32_path) {
        // PE 文件头检测
        if data.len() > 0x40 {
            // DOS 头的 e_lfanew 字段在偏移 0x3C
            let pe_offset = u32::from_le_bytes([data[0x3C], data[0x3D], data[0x3E], data[0x3F]]) as usize;
            
            if data.len() > pe_offset + 6 {
                // PE 签名后的 Machine 字段
                let machine = u16::from_le_bytes([data[pe_offset + 4], data[pe_offset + 5]]);
                
                return match machine {
                    0x014c => SystemArchitecture::X86,  // IMAGE_FILE_MACHINE_I386
                    0x8664 => SystemArchitecture::X64,  // IMAGE_FILE_MACHINE_AMD64
                    0xAA64 => SystemArchitecture::Arm64, // IMAGE_FILE_MACHINE_ARM64
                    _ => SystemArchitecture::Unknown,
                };
            }
        }
    }

    // 如果无法检测，检查是否存在 SysWOW64 目录
    if system_root.join("Windows").join("SysWOW64").exists() {
        SystemArchitecture::X64
    } else {
        SystemArchitecture::X86
    }
}

/// 检查是否在 WoW64 下运行
#[cfg(windows)]
fn is_wow64() -> bool {
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::System::Threading::GetCurrentProcess;

    #[link(name = "kernel32")]
    extern "system" {
        fn IsWow64Process(hProcess: *mut std::ffi::c_void, Wow64Process: *mut BOOL) -> BOOL;
    }

    unsafe {
        let mut is_wow64 = BOOL::default();
        let process = GetCurrentProcess();
        if IsWow64Process(process.0, &mut is_wow64).as_bool() {
            is_wow64.as_bool()
        } else {
            false
        }
    }
}

#[cfg(not(windows))]
fn is_wow64() -> bool {
    false
}

// =============================================================================
// PE 环境检测
// =============================================================================

/// 检测当前是否在 PE 环境中运行
pub fn is_pe_environment() -> bool {
    // 检查 X: 盘是否存在（PE 环境的典型特征）
    if Path::new("X:\\Windows\\System32").exists() {
        return true;
    }

    // 检查系统盘符是否为 X:
    if let Ok(system_root) = std::env::var("SystemRoot") {
        if system_root.to_uppercase().starts_with("X:") {
            return true;
        }
    }

    // 检查是否存在 PE 特有的文件
    let pe_markers = [
        "X:\\Windows\\System32\\winpeshl.exe",
        "X:\\Windows\\System32\\wpeinit.exe",
        "X:\\sources\\boot.wim",
    ];

    for marker in &pe_markers {
        if Path::new(marker).exists() {
            return true;
        }
    }

    false
}

/// 获取 PE 环境的系统盘符
pub fn get_pe_system_drive() -> Option<String> {
    // 检查常见的 PE 系统盘符
    for letter in ['X', 'Y', 'Z', 'W'] {
        let path = format!("{}:\\Windows\\System32", letter);
        if Path::new(&path).exists() {
            return Some(format!("{}:", letter));
        }
    }

    // 从环境变量获取
    if let Ok(system_root) = std::env::var("SystemRoot") {
        if let Some(drive) = system_root.chars().next() {
            return Some(format!("{}:", drive));
        }
    }

    None
}

// =============================================================================
// 临时目录管理
// =============================================================================

/// 获取适合的临时目录
///
/// 在 PE 环境中优先使用 X:\Windows\TEMP，
/// 否则使用系统默认临时目录。
pub fn get_temp_directory() -> PathBuf {
    // PE 环境优先
    let pe_temps = [
        "X:\\Windows\\TEMP",
        "X:\\TEMP",
        "Y:\\TEMP",
    ];

    for temp in &pe_temps {
        let path = Path::new(temp);
        if path.exists() {
            return path.to_path_buf();
        }
    }

    // 尝试创建 PE 临时目录
    if is_pe_environment() {
        let pe_temp = Path::new("X:\\Windows\\TEMP");
        if std::fs::create_dir_all(pe_temp).is_ok() {
            return pe_temp.to_path_buf();
        }
    }

    // 使用系统默认临时目录
    std::env::temp_dir()
}

/// 创建唯一的临时目录
pub fn create_temp_directory(prefix: &str) -> std::io::Result<PathBuf> {
    let base = get_temp_directory();
    let unique_name = format!(
        "{}_{}_{}",
        prefix,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    );
    
    let path = base.join(unique_name);
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

/// 确保临时目录存在
///
/// 用于确保 DISM 等工具的 scratchdir 可用。
pub fn ensure_scratch_directory() -> PathBuf {
    let scratch = get_temp_directory();
    let _ = std::fs::create_dir_all(&scratch);
    scratch
}

// =============================================================================
// 其他工具函数
// =============================================================================

/// 格式化文件大小
pub fn format_file_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;

    if bytes >= TB {
        format!("{:.2} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// 格式化持续时间
pub fn format_duration(seconds: u64) -> String {
    if seconds < 60 {
        format!("{}秒", seconds)
    } else if seconds < 3600 {
        let minutes = seconds / 60;
        let secs = seconds % 60;
        format!("{}分{}秒", minutes, secs)
    } else {
        let hours = seconds / 3600;
        let minutes = (seconds % 3600) / 60;
        format!("{}小时{}分", hours, minutes)
    }
}

/// 检查路径是否存在且可访问
pub fn path_accessible(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }

    // 尝试读取目录或文件
    if path.is_dir() {
        std::fs::read_dir(path).is_ok()
    } else {
        std::fs::metadata(path).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_file_size() {
        assert_eq!(format_file_size(500), "500 B");
        assert_eq!(format_file_size(1024), "1.00 KB");
        assert_eq!(format_file_size(1024 * 1024), "1.00 MB");
        assert_eq!(format_file_size(1024 * 1024 * 1024), "1.00 GB");
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(30), "30秒");
        assert_eq!(format_duration(90), "1分30秒");
        assert_eq!(format_duration(3661), "1小时1分");
    }

    #[test]
    fn test_system_architecture() {
        let arch = get_system_architecture();
        assert!(matches!(
            arch,
            SystemArchitecture::X86 | SystemArchitecture::X64 | SystemArchitecture::Arm64
        ));
    }
}
