//! Windows版本检测模块
//!
//! 提供对离线Windows分区的精确版本检测

use std::path::Path;
use crate::utils::cmd::create_command;

/// Windows版本详细信息
#[derive(Debug, Clone)]
pub struct WindowsVersionInfo {
    pub product_name: String,
    pub display_version: Option<String>,
    pub current_build: Option<String>,
    pub edition_id: Option<String>,
}

impl WindowsVersionInfo {
    /// 格式化为显示字符串
    pub fn to_display_string(&self) -> String {
        // 首先尝试从ProductName中提取基本版本
        let base_version = extract_windows_version(&self.product_name);
        
        // 如果有DisplayVersion（如24H2、23H2等），附加上去
        if let Some(ref dv) = self.display_version {
            if !dv.is_empty() {
                return format!("{} {}", base_version, dv);
            }
        }
        
        // 如果没有DisplayVersion但有CurrentBuild，可以推断版本
        if let Some(ref build) = self.current_build {
            if let Some(version_from_build) = build_to_version(build) {
                // 判断是Win10还是Win11
                let win_version = if is_windows_11_build(build) {
                    "Windows 11"
                } else if base_version.contains("11") {
                    "Windows 11"
                } else if base_version.contains("10") {
                    "Windows 10"
                } else {
                    &base_version
                };
                return format!("{} {} ({})", win_version, version_from_build, build);
            }
        }
        
        base_version
    }
}

/// 从ProductName中提取Windows版本
fn extract_windows_version(product_name: &str) -> String {
    if product_name.contains("Windows 11") {
        "Windows 11".to_string()
    } else if product_name.contains("Windows 10") {
        "Windows 10".to_string()
    } else if product_name.contains("Windows 8.1") {
        "Windows 8.1".to_string()
    } else if product_name.contains("Windows 8") {
        "Windows 8".to_string()
    } else if product_name.contains("Windows 7") {
        "Windows 7".to_string()
    } else if product_name.contains("Windows Vista") {
        "Windows Vista".to_string()
    } else if product_name.contains("Windows XP") {
        "Windows XP".to_string()
    } else if product_name.contains("Windows Server 2022") {
        "Server 2022".to_string()
    } else if product_name.contains("Windows Server 2019") {
        "Server 2019".to_string()
    } else if product_name.contains("Windows Server 2016") {
        "Server 2016".to_string()
    } else if product_name.contains("Windows Server") {
        "Windows Server".to_string()
    } else if !product_name.is_empty() {
        product_name.to_string()
    } else {
        "Windows".to_string()
    }
}

/// 根据Build号判断是否为Windows 11
fn is_windows_11_build(build: &str) -> bool {
    if let Ok(build_num) = build.parse::<u32>() {
        // Windows 11的Build号从22000开始
        build_num >= 22000
    } else {
        false
    }
}

/// 根据Build号推断版本号
fn build_to_version(build: &str) -> Option<String> {
    let build_num: u32 = build.parse().ok()?;
    
    // Windows 11 版本映射
    if build_num >= 26100 {
        Some("24H2".to_string())
    } else if build_num >= 22631 {
        Some("23H2".to_string())
    } else if build_num >= 22621 {
        Some("22H2".to_string())
    } else if build_num >= 22000 {
        Some("21H2".to_string())
    }
    // Windows 10 版本映射
    else if build_num >= 19045 {
        Some("22H2".to_string())
    } else if build_num >= 19044 {
        Some("21H2".to_string())
    } else if build_num >= 19043 {
        Some("21H1".to_string())
    } else if build_num >= 19042 {
        Some("20H2".to_string())
    } else if build_num >= 19041 {
        Some("2004".to_string())
    } else if build_num >= 18363 {
        Some("1909".to_string())
    } else if build_num >= 18362 {
        Some("1903".to_string())
    } else if build_num >= 17763 {
        Some("1809".to_string())
    } else if build_num >= 17134 {
        Some("1803".to_string())
    } else if build_num >= 16299 {
        Some("1709".to_string())
    } else if build_num >= 15063 {
        Some("1703".to_string())
    } else if build_num >= 14393 {
        Some("1607".to_string())
    } else if build_num >= 10586 {
        Some("1511".to_string())
    } else if build_num >= 10240 {
        Some("1507".to_string())
    }
    // Windows 8.1 / 8 / 7
    else if build_num >= 9600 {
        Some("8.1".to_string())
    } else if build_num >= 9200 {
        Some("8".to_string())
    } else if build_num >= 7601 {
        Some("SP1".to_string())
    } else if build_num >= 7600 {
        Some("RTM".to_string())
    } else {
        None
    }
}

/// 获取指定分区的Windows版本和架构信息
pub fn get_windows_version_info(partition: &str) -> (String, String) {
    let partition_root = partition.trim_end_matches('\\').trim_end_matches(':');
    let partition_letter = format!("{}:", partition_root);
    
    // 首先尝试从注册表获取
    if let Some(version_info) = read_version_from_registry(&partition_letter) {
        let arch = detect_architecture(&partition_letter);
        return (version_info.to_display_string(), arch);
    }
    
    // 注册表方法失败，尝试从kernel32.dll获取版本
    if let Some(version_info) = read_version_from_kernel32(&partition_letter) {
        let arch = detect_architecture(&partition_letter);
        return (version_info.to_display_string(), arch);
    }
    
    // 所有方法都失败，使用文件系统特征检测
    detect_windows_from_filesystem(&partition_letter)
}

/// 从离线注册表读取Windows版本信息
fn read_version_from_registry(partition: &str) -> Option<WindowsVersionInfo> {
    let software_hive = format!("{}\\Windows\\System32\\config\\SOFTWARE", partition);
    
    if !Path::new(&software_hive).exists() {
        return None;
    }

    // 生成唯一的临时注册表加载点名称
    let partition_id = partition.trim_end_matches(':');
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let temp_key = format!("LR_VER_{}_{}", partition_id, timestamp % 10000);
    let reg_path = format!("HKLM\\{}\\Microsoft\\Windows NT\\CurrentVersion", temp_key);

    // 尝试加载注册表
    let load_result = create_command("reg.exe")
        .args(["load", &format!("HKLM\\{}", temp_key), &software_hive])
        .output();

    if load_result.is_err() {
        return None;
    }
    
    let load_output = load_result.unwrap();
    if !load_output.status.success() {
        // 注册表可能已被加载，尝试先卸载再加载
        let _ = create_command("reg.exe")
            .args(["unload", &format!("HKLM\\{}", temp_key)])
            .output();
        
        // 重试加载
        let retry_load = create_command("reg.exe")
            .args(["load", &format!("HKLM\\{}", temp_key), &software_hive])
            .output();
        
        if retry_load.is_err() || !retry_load.unwrap().status.success() {
            return None;
        }
    }

    // 查询注册表值
    let product_name = query_reg_value(&reg_path, "ProductName")
        .unwrap_or_else(|| "Windows".to_string());
    let display_version = query_reg_value(&reg_path, "DisplayVersion");
    let current_build = query_reg_value(&reg_path, "CurrentBuild")
        .or_else(|| query_reg_value(&reg_path, "CurrentBuildNumber"));
    let edition_id = query_reg_value(&reg_path, "EditionID");

    // 卸载注册表
    let _ = create_command("reg.exe")
        .args(["unload", &format!("HKLM\\{}", temp_key)])
        .output();

    Some(WindowsVersionInfo {
        product_name,
        display_version,
        current_build,
        edition_id,
    })
}

/// 从kernel32.dll读取版本信息
fn read_version_from_kernel32(partition: &str) -> Option<WindowsVersionInfo> {
    #[cfg(windows)]
    {
        let kernel32_path = format!("{}\\Windows\\System32\\kernel32.dll", partition);
        
        if !Path::new(&kernel32_path).exists() {
            return None;
        }

        // 使用PowerShell获取文件版本信息
        let ps_script = format!(
            r#"
            $file = '{}'
            if (Test-Path $file) {{
                $ver = [System.Diagnostics.FileVersionInfo]::GetVersionInfo($file)
                Write-Output "$($ver.FileMajorPart).$($ver.FileMinorPart).$($ver.FileBuildPart).$($ver.FilePrivatePart)"
            }}
            "#,
            kernel32_path.replace('\'', "''")
        );

        let output = create_command("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                &ps_script,
            ])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let version_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let parts: Vec<&str> = version_str.split('.').collect();
        
        if parts.len() >= 3 {
            let major: u32 = parts[0].parse().unwrap_or(0);
            let minor: u32 = parts[1].parse().unwrap_or(0);
            let build: u32 = parts[2].parse().unwrap_or(0);
            
            // 根据主版本号和Build号确定Windows版本
            let product_name = if major == 10 && build >= 22000 {
                "Windows 11".to_string()
            } else if major == 10 {
                "Windows 10".to_string()
            } else if major == 6 && minor == 3 {
                "Windows 8.1".to_string()
            } else if major == 6 && minor == 2 {
                "Windows 8".to_string()
            } else if major == 6 && minor == 1 {
                "Windows 7".to_string()
            } else if major == 6 && minor == 0 {
                "Windows Vista".to_string()
            } else {
                format!("Windows {}.{}", major, minor)
            };
            
            return Some(WindowsVersionInfo {
                product_name,
                display_version: None,
                current_build: Some(build.to_string()),
                edition_id: None,
            });
        }
    }
    
    None
}

/// 查询注册表值
fn query_reg_value(key_path: &str, value_name: &str) -> Option<String> {
    let output = create_command("reg.exe")
        .args(["query", key_path, "/v", value_name])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = crate::utils::encoding::gbk_to_utf8(&output.stdout);
    
    // 解析输出，格式类似：
    //     ProductName    REG_SZ    Windows 11 Pro
    for line in stdout.lines() {
        let line_upper = line.to_uppercase();
        let value_upper = value_name.to_uppercase();
        
        if line_upper.contains(&value_upper) && line_upper.contains("REG_SZ") {
            // 找到REG_SZ后面的值
            if let Some(pos) = line.to_uppercase().find("REG_SZ") {
                let value_start = pos + 6; // "REG_SZ"的长度
                if value_start < line.len() {
                    let value = line[value_start..].trim();
                    if !value.is_empty() {
                        return Some(value.to_string());
                    }
                }
            }
        }
        
        // 也处理REG_DWORD的情况（对于某些数值）
        if line_upper.contains(&value_upper) && line_upper.contains("REG_DWORD") {
            if let Some(pos) = line.to_uppercase().find("REG_DWORD") {
                let value_start = pos + 9; // "REG_DWORD"的长度
                if value_start < line.len() {
                    let value = line[value_start..].trim();
                    // 转换十六进制值
                    if let Some(hex_val) = value.strip_prefix("0x") {
                        if let Ok(num) = u32::from_str_radix(hex_val, 16) {
                            return Some(num.to_string());
                        }
                    }
                    if !value.is_empty() {
                        return Some(value.to_string());
                    }
                }
            }
        }
    }

    None
}

/// 从文件系统检测Windows版本
fn detect_windows_from_filesystem(partition: &str) -> (String, String) {
    let arch = detect_architecture(partition);
    
    // 检查ntoskrnl.exe存在来确认是Windows
    let ntoskrnl = format!("{}\\Windows\\System32\\ntoskrnl.exe", partition);
    if !Path::new(&ntoskrnl).exists() {
        return ("Unknown".to_string(), arch);
    }
    
    // 使用文件特征来判断Windows版本
    let system32 = format!("{}\\Windows\\System32", partition);
    let system_apps = format!("{}\\Windows\\SystemApps", partition);
    
    // Windows 11 特征：新的开始菜单组件
    let win11_start = format!(
        "{}\\Microsoft.Windows.StartMenuExperienceHost_cw5n1h2txyewy",
        system_apps
    );
    let win11_widgets = format!(
        "{}\\MicrosoftWindows.Client.WebExperience_cw5n1h2txyewy",
        system_apps
    );
    
    // Windows 10 特征
    let win10_cortana = format!(
        "{}\\Microsoft.Windows.Cortana_cw5n1h2txyewy",
        system_apps
    );
    let win10_edge_legacy = format!(
        "{}\\Microsoft.MicrosoftEdge_8wekyb3d8bbwe",
        system_apps
    );
    
    // 检测Windows 11
    // Windows 11有新的Widgets应用并且StartMenuExperienceHost有特定版本
    if Path::new(&win11_widgets).exists() 
        || (Path::new(&win11_start).exists() && has_win11_start_menu_features(partition)) {
        return ("Windows 11".to_string(), arch);
    }
    
    // 检测Windows 10
    if Path::new(&system_apps).exists() {
        // 有SystemApps目录，至少是Windows 10
        if Path::new(&win10_cortana).exists() || Path::new(&win10_edge_legacy).exists() {
            return ("Windows 10".to_string(), arch);
        }
        // 有SystemApps但没有特定应用，可能是精简版Win10或早期版本
        return ("Windows 10".to_string(), arch);
    }
    
    // 检查servicing目录（Vista及以上）
    let servicing = format!("{}\\Windows\\servicing", partition);
    let winsxs = format!("{}\\Windows\\WinSxS", partition);
    
    if Path::new(&servicing).exists() && Path::new(&winsxs).exists() {
        // Vista/7/8系列
        // 检查Modern UI特征（Windows 8+）
        let immersive_shell = format!("{}\\twinui.dll", system32);
        if Path::new(&immersive_shell).exists() {
            // Windows 8或8.1
            let win81_feature = format!("{}\\Windows\\WinStore", partition);
            if Path::new(&win81_feature).exists() {
                return ("Windows 8.1".to_string(), arch);
            }
            return ("Windows 8".to_string(), arch);
        }
        
        // 没有Modern UI，是Windows 7或Vista
        let win7_feature = format!("{}\\Windows\\System32\\drivers\\wimmount.sys", partition);
        if Path::new(&win7_feature).exists() {
            return ("Windows 7".to_string(), arch);
        }
        
        return ("Windows Vista".to_string(), arch);
    }
    
    // XP及更早版本
    let xp_feature = format!("{}\\Windows\\System32\\config\\software", partition);
    if Path::new(&xp_feature).exists() {
        return ("Windows XP".to_string(), arch);
    }
    
    ("Windows".to_string(), arch)
}

/// 检查是否有Windows 11的开始菜单特征
fn has_win11_start_menu_features(partition: &str) -> bool {
    // Windows 11的开始菜单有新的XAML布局文件
    let start_layout = format!(
        "{}\\Windows\\SystemApps\\Microsoft.Windows.StartMenuExperienceHost_cw5n1h2txyewy\\StartMenuExperienceHost.exe",
        partition
    );
    
    if !Path::new(&start_layout).exists() {
        return false;
    }
    
    // 检查文件大小或创建时间等特征来区分Win10和Win11
    // Windows 11的StartMenuExperienceHost.exe通常更大
    if let Ok(metadata) = std::fs::metadata(&start_layout) {
        // Windows 11的开始菜单EXE通常超过2MB
        return metadata.len() > 2_000_000;
    }
    
    false
}

/// 检测系统架构
pub fn detect_architecture(partition: &str) -> String {
    // 检查SysWOW64目录是否存在来判断是否为64位系统
    let syswow64 = format!("{}\\Windows\\SysWOW64", partition);
    if Path::new(&syswow64).exists() {
        // 进一步检查是否为ARM64
        let arm64_folder = format!("{}\\Windows\\System32\\DriverStore\\FileRepository", partition);
        if Path::new(&arm64_folder).exists() {
            // 检查是否有ARM64驱动
            if let Ok(entries) = std::fs::read_dir(&arm64_folder) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_lowercase();
                    if name.contains("arm64") {
                        return "ARM64".to_string();
                    }
                }
            }
        }
        "x64".to_string()
    } else {
        "x86".to_string()
    }
}

/// 获取Windows分区信息列表（用于下拉框显示）
pub fn get_windows_partition_infos(partitions: &[crate::core::disk::Partition]) -> Vec<super::types::WindowsPartitionInfo> {
    partitions
        .iter()
        .filter(|p| p.has_windows && p.letter.to_uppercase() != "X:")
        .map(|p| {
            let (version, arch) = get_windows_version_info(&p.letter);
            super::types::WindowsPartitionInfo {
                letter: p.letter.clone(),
                windows_version: version,
                architecture: arch,
            }
        })
        .collect()
}
