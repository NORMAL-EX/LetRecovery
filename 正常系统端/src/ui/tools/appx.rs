//! APPX应用管理模块
//!
//! 使用Windows Runtime API和文件系统操作管理APPX应用

use super::types::AppxPackageInfo;
use std::path::Path;
use std::collections::HashSet;

/// 判断是否为当前系统分区
#[cfg(windows)]
fn is_current_system(partition: &str) -> bool {
    if partition == "__CURRENT__" {
        return true;
    }
    
    if let Ok(system_root) = std::env::var("SystemRoot") {
        let system_drive = system_root.chars().next().unwrap_or('C');
        let target_drive = partition.chars().next().unwrap_or('?');
        return system_drive.eq_ignore_ascii_case(&target_drive);
    }
    false
}

/// 获取APPX包列表
pub fn get_appx_packages(target_partition: &str) -> Vec<AppxPackageInfo> {
    #[cfg(windows)]
    {
        if is_current_system(target_partition) {
            get_appx_packages_online()
        } else {
            get_appx_packages_offline(target_partition)
        }
    }
    
    #[cfg(not(windows))]
    {
        let _ = target_partition;
        Vec::new()
    }
}

/// 获取当前系统的APPX包（使用Windows Runtime API）
#[cfg(windows)]
fn get_appx_packages_online() -> Vec<AppxPackageInfo> {
    use windows::Management::Deployment::PackageManager;
    
    let mut packages = Vec::new();
    
    let pm = match PackageManager::new() {
        Ok(pm) => pm,
        Err(e) => {
            log::error!("创建PackageManager失败: {:?}", e);
            return packages;
        }
    };
    
    // 使用FindPackages获取所有包
    let all_packages = match pm.FindPackages() {
        Ok(pkgs) => pkgs,
        Err(e) => {
            log::error!("获取包列表失败: {:?}", e);
            return packages;
        }
    };
    
    // 获取迭代器
    let iterator = match all_packages.First() {
        Ok(iter) => iter,
        Err(e) => {
            log::error!("获取迭代器失败: {:?}", e);
            return packages;
        }
    };
    
    let mut seen_names: HashSet<String> = HashSet::new();
    
    // 使用迭代器遍历
    loop {
        if !iterator.HasCurrent().unwrap_or(false) {
            break;
        }
        
        if let Ok(pkg) = iterator.Current() {
            if let Ok(pkg_id) = pkg.Id() {
                if let Ok(full_name) = pkg_id.FullName() {
                    let package_full_name = full_name.to_string_lossy();
                    let package_name = pkg_id.Name()
                        .map(|n| n.to_string_lossy())
                        .unwrap_or_else(|_| package_full_name.clone());
                    
                    // 跳过框架包和资源包
                    let is_framework = pkg.IsFramework().unwrap_or(false);
                    let is_resource = pkg.IsResourcePackage().unwrap_or(false);
                    
                    if !is_framework && !is_resource && !is_system_critical_appx(&package_name) {
                        // 获取显示名称，如果是乱码/GUID则使用包名
                        let display_name = pkg.DisplayName()
                            .map(|n| {
                                let s = n.to_string_lossy();
                                if is_invalid_display_name(&s) {
                                    extract_friendly_name(&package_name)
                                } else {
                                    s
                                }
                            })
                            .unwrap_or_else(|_| extract_friendly_name(&package_name));
                        
                        // 如果最终显示名称还是无效，跳过
                        if is_invalid_display_name(&display_name) {
                            // 不添加到列表，继续下一个
                        } else {
                            // 避免重复
                            if !seen_names.contains(&display_name) {
                                seen_names.insert(display_name.clone());
                                packages.push(AppxPackageInfo {
                                    package_name: package_full_name,
                                    display_name,
                                });
                            }
                        }
                    }
                }
            }
        }
        
        // 移动到下一个
        if !iterator.MoveNext().unwrap_or(false) {
            break;
        }
    }
    
    packages.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    packages
}

/// 获取离线系统的APPX包（直接读取文件系统）
#[cfg(windows)]
fn get_appx_packages_offline(target_partition: &str) -> Vec<AppxPackageInfo> {
    let mut packages = Vec::new();
    let mut seen_names: HashSet<String> = HashSet::new();
    
    let partition = target_partition.trim_end_matches('\\');
    let apps_path = format!("{}\\Program Files\\WindowsApps", partition);
    let apps_dir = Path::new(&apps_path);
    
    if !apps_dir.exists() {
        log::warn!("WindowsApps目录不存在: {}", apps_path);
        return packages;
    }
    
    let entries = match std::fs::read_dir(apps_dir) {
        Ok(e) => e,
        Err(e) => {
            log::error!("读取WindowsApps目录失败: {:?}", e);
            return packages;
        }
    };
    
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        
        let dir_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        
        // 跳过特殊目录
        if dir_name == "Deleted" || dir_name == "DeletedAllUserPackages" 
            || dir_name == "Merged" || dir_name == "Mutable" 
            || dir_name == "MutableBackup" || dir_name == "Projected"
            || dir_name == "MovedPackages" {
            continue;
        }
        
        // 解析包名格式: {Name}_{Version}_{Arch}_{PublisherId}
        // 或: {Name}_{Version}_{Arch}_split.{type}_{PublisherId}
        let parts: Vec<&str> = dir_name.split('_').collect();
        if parts.len() < 2 {
            continue;
        }
        
        let package_name = parts[0].to_string();
        
        // 跳过资源包（包含split.language-、split.scale-等）
        if dir_name.contains("_split.") || dir_name.contains("_neutral_~_") {
            continue;
        }
        
        // 过滤系统关键包
        if is_system_critical_appx(&package_name) {
            continue;
        }
        
        let display_name = extract_friendly_name(&package_name);
        
        // 过滤无效显示名称
        if is_invalid_display_name(&display_name) {
            continue;
        }
        
        // 避免重复（同一个包可能有多个版本/架构）
        if !seen_names.contains(&display_name) {
            seen_names.insert(display_name.clone());
            packages.push(AppxPackageInfo {
                package_name: dir_name,  // 使用完整目录名
                display_name,
            });
        }
    }
    
    packages.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    packages
}

/// 从包名称提取友好名称
#[cfg(windows)]
fn extract_friendly_name(package_name: &str) -> String {
    let name = package_name.replace('.', " ");
    
    let name = name
        .trim_start_matches("Microsoft ")
        .trim_start_matches("MicrosoftCorporationII ")
        .trim_start_matches("MicrosoftWindows ")
        .trim_start_matches("Windows ");
    
    name.to_string()
}

/// 检查显示名称是否无效（GUID、乱码、资源引用等）
#[cfg(windows)]
fn is_invalid_display_name(name: &str) -> bool {
    if name.is_empty() {
        return true;
    }
    
    // ms-resource: 开头的资源引用
    if name.starts_with("ms-resource:") {
        return true;
    }
    
    // GUID格式 (xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx) 或包含GUID
    if name.contains('-') {
        let parts: Vec<&str> = name.split('-').collect();
        if parts.len() >= 4 {
            let all_hex = parts.iter().all(|p| p.chars().all(|c| c.is_ascii_hexdigit()));
            if all_hex {
                return true;
            }
        }
    }
    
    // 以数字开头的乱码（如 "54792954 Filons", "58680125 Speion"）
    let first_char = name.chars().next().unwrap_or(' ');
    if first_char.is_ascii_digit() {
        return true;
    }
    
    // 包含大量十六进制字符的字符串（不包括空格）
    let name_no_space = name.replace(' ', "");
    let hex_chars = name_no_space.chars().filter(|c| c.is_ascii_hexdigit()).count();
    if name_no_space.len() > 10 && hex_chars as f32 / name_no_space.len() as f32 > 0.6 {
        return true;
    }
    
    // 名称太短（如单个字母）
    if name.len() < 3 {
        return true;
    }
    
    // 检测看起来像随机字符串的名称（如 "Filons", "Speion", "Voiess"）
    // 这些通常是短的、没有意义的字符组合
    let words: Vec<&str> = name.split_whitespace().collect();
    if words.len() == 1 && name.len() >= 5 && name.len() <= 8 {
        // 单个单词，5-8个字符，检查是否是已知的有意义的名称
        let known_names = ["Claude", "Cortana", "Spotify", "Discord", "Netflix", 
                          "Twitter", "Notepad", "Photos", "Camera", "Weather",
                          "Calendar", "Music", "Video", "Office", "Edge"];
        if !known_names.iter().any(|&k| name.eq_ignore_ascii_case(k)) {
            // 检查是否看起来像随机字符组合（辅音太多、元音太少）
            let vowels = name.to_lowercase().chars().filter(|c| "aeiou".contains(*c)).count();
            let consonants = name.to_lowercase().chars().filter(|c| c.is_ascii_alphabetic() && !"aeiou".contains(*c)).count();
            
            // 如果辅音/元音比例异常，可能是乱码
            if consonants > 0 && vowels > 0 {
                let ratio = consonants as f32 / vowels as f32;
                // 正常英文单词的辅音/元音比例通常在1.5-2.5之间
                // 如果比例异常且名称看起来不像正常单词，标记为无效
                if ratio > 4.0 || ratio < 0.3 {
                    return true;
                }
            }
        }
    }
    
    false
}

/// 移除APPX包
pub fn remove_appx_packages(target_partition: &str, packages: &[String]) -> (usize, usize) {
    #[cfg(windows)]
    {
        if is_current_system(target_partition) {
            remove_appx_packages_online(packages)
        } else {
            remove_appx_packages_offline(target_partition, packages)
        }
    }
    
    #[cfg(not(windows))]
    {
        let _ = (target_partition, packages);
        (0, 0)
    }
}

/// 移除当前系统的APPX包（使用Windows Runtime API）
#[cfg(windows)]
fn remove_appx_packages_online(packages: &[String]) -> (usize, usize) {
    use windows::Management::Deployment::{PackageManager, RemovalOptions};
    
    let mut success = 0;
    let mut fail = 0;
    
    let pm = match PackageManager::new() {
        Ok(pm) => pm,
        Err(e) => {
            log::error!("创建PackageManager失败: {:?}", e);
            return (0, packages.len());
        }
    };
    
    for package_name in packages {
        let hstring_name = windows::core::HSTRING::from(package_name.as_str());
        
        match pm.RemovePackageAsync(&hstring_name) {
            Ok(operation) => {
                match operation.get() {
                    Ok(_) => {
                        log::info!("成功移除包: {}", package_name);
                        success += 1;
                    }
                    Err(e) => {
                        log::warn!("移除包失败 {}: {:?}", package_name, e);
                        // 尝试保留数据移除
                        if let Ok(op2) = pm.RemovePackageWithOptionsAsync(&hstring_name, RemovalOptions::PreserveApplicationData) {
                            if op2.get().is_ok() {
                                success += 1;
                                continue;
                            }
                        }
                        fail += 1;
                    }
                }
            }
            Err(e) => {
                log::warn!("启动移除操作失败 {}: {:?}", package_name, e);
                fail += 1;
            }
        }
    }
    
    (success, fail)
}

/// 移除离线系统的APPX包（直接删除目录）
#[cfg(windows)]
fn remove_appx_packages_offline(target_partition: &str, packages: &[String]) -> (usize, usize) {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueW, 
        SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::core::PCWSTR;
    
    let mut success = 0;
    let mut fail = 0;
    
    let partition = target_partition.trim_end_matches('\\');
    let apps_path = format!("{}\\Program Files\\WindowsApps", partition);
    
    // 尝试启用必要权限
    unsafe {
        let mut token: HANDLE = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token).is_ok() {
            let priv_names = [
                windows::core::w!("SeBackupPrivilege"),
                windows::core::w!("SeRestorePrivilege"),
                windows::core::w!("SeTakeOwnershipPrivilege"),
            ];
            
            for priv_name in &priv_names {
                let mut luid = windows::Win32::Foundation::LUID::default();
                if LookupPrivilegeValueW(PCWSTR::null(), *priv_name, &mut luid).is_ok() {
                    let mut tp = TOKEN_PRIVILEGES {
                        PrivilegeCount: 1,
                        Privileges: [windows::Win32::Security::LUID_AND_ATTRIBUTES {
                            Luid: luid,
                            Attributes: SE_PRIVILEGE_ENABLED,
                        }],
                    };
                    let _ = AdjustTokenPrivileges(token, false, Some(&mut tp), 0, None, None);
                }
            }
            let _ = windows::Win32::Foundation::CloseHandle(token);
        }
    }
    
    for package_name in packages {
        // 获取包的基础名称（用于匹配相关目录）
        let base_name = package_name.split('_').next().unwrap_or(package_name);
        
        let entries = match std::fs::read_dir(&apps_path) {
            Ok(e) => e,
            Err(e) => {
                log::error!("无法读取WindowsApps目录: {:?}", e);
                fail += 1;
                continue;
            }
        };
        
        let mut removed_count = 0;
        
        // 删除所有以此包名开头的目录（包括不同版本、架构、资源包）
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            
            let dir_name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };
            
            // 匹配：精确匹配或以 base_name_ 开头
            if dir_name == package_name || dir_name.starts_with(&format!("{}_", base_name)) {
                match remove_dir_with_acl(&path) {
                    Ok(_) => {
                        log::info!("成功删除: {}", dir_name);
                        removed_count += 1;
                    }
                    Err(e) => {
                        log::warn!("删除失败 {}: {:?}", dir_name, e);
                    }
                }
            }
        }
        
        if removed_count > 0 {
            success += 1;
        } else {
            fail += 1;
        }
    }
    
    (success, fail)
}

/// 删除目录（带ACL处理）
#[cfg(windows)]
fn remove_dir_with_acl(path: &Path) -> std::io::Result<()> {
    // 首先尝试直接删除
    if std::fs::remove_dir_all(path).is_ok() {
        return Ok(());
    }
    
    // 如果失败，尝试修改权限后删除
    // 递归设置所有文件为可写
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
    std::fs::remove_dir_all(path)
}

/// 检查是否为系统关键APPX（不可移除）
pub fn is_system_critical_appx(package_name: &str) -> bool {
    let critical_packages = [
        // 核心系统组件
        "Microsoft.WindowsStore",
        "Microsoft.StorePurchaseApp",
        "Microsoft.DesktopAppInstaller",
        // 基础工具
        "Microsoft.WindowsTerminal",
        "Microsoft.WindowsCalculator",
        "Microsoft.Windows.Photos",
        "Microsoft.WindowsNotepad",
        "Microsoft.Paint",
        "Microsoft.ScreenSketch",
        "Microsoft.WindowsAlarms",
        "Microsoft.WindowsSoundRecorder",
        "Microsoft.WindowsCamera",
        // 媒体扩展
        "Microsoft.HEIFImageExtension",
        "Microsoft.HEVCVideoExtension",
        "Microsoft.VP9VideoExtensions",
        "Microsoft.WebMediaExtensions",
        "Microsoft.WebpImageExtension",
        "Microsoft.RawImageExtension",
        "Microsoft.AV1VideoExtension",
        "Microsoft.AVCEncoderVideoExtension",
        "Microsoft.MPEG2VideoExtension",
        // 运行时组件
        "Microsoft.VCLibs",
        "Microsoft.NET.Native",
        "Microsoft.UI.Xaml",
        "Microsoft.WindowsAppRuntime",
        "Microsoft.WinAppRuntime",
        "MicrosoftCorporationII.WinAppRuntime",
        "Microsoft.Services.Store.Engagement",
        // 安全相关
        "Microsoft.SecHealthUI",
        "Microsoft.Windows.SecHealthUI",
        // Shell体验
        "Windows.CBSPreview",
        "MicrosoftWindows.Client",
        "Microsoft.Windows.ShellExperienceHost",
        "Microsoft.Windows.StartMenuExperienceHost",
        "Microsoft.StartExperiencesApp",
        // 认证和账户
        "Microsoft.AAD.BrokerPlugin",
        "Microsoft.AccountsControl",
        // 系统服务
        "Microsoft.AsyncTextService",
        "Microsoft.BioEnrollment",
        "Microsoft.CredDialogHost",
        "Microsoft.ECApp",
        "Microsoft.LockApp",
        // Edge相关
        "Microsoft.MicrosoftEdge",
        "Microsoft.MicrosoftEdgeDevToolsClient",
        "Microsoft.Win32WebViewHost",
        "Microsoft.Edge.GameAssist",
        // Windows核心体验
        "Microsoft.Windows.Apprep.ChxApp",
        "Microsoft.Windows.AssignedAccessLockApp",
        "Microsoft.Windows.CapturePicker",
        "Microsoft.Windows.CloudExperienceHost",
        "Microsoft.Windows.ContentDeliveryManager",
        "Microsoft.Windows.NarratorQuickStart",
        "Microsoft.Windows.OOBENetworkCaptivePortal",
        "Microsoft.Windows.OOBENetworkConnectionFlow",
        "Microsoft.Windows.ParentalControls",
        "Microsoft.Windows.PeopleExperienceHost",
        "Microsoft.Windows.PinningConfirmationDialog",
        "Microsoft.Windows.Search",
        "Microsoft.Windows.SecureAssessmentBrowser",
        "Microsoft.Windows.XGpuEjectDialog",
        // Xbox相关（部分系统集成）
        "Microsoft.XboxGameCallableUI",
        "Microsoft.Xbox.TCUI",
        "Microsoft.XboxIdentityProvider",
        // 网络相关
        "NcsiUwpApp",
        // 控制面板
        "windows.immersivecontrolpanel",
        // 打印
        "Windows.PrintDialog",
        // 语言包
        "Microsoft.LanguageExperiencePack",
        // Widgets
        "Microsoft.WidgetsPlatformRuntime",
        // OneDrive
        "Microsoft.OneDriveSync",
        // 应用兼容性
        "Microsoft.ApplicationCompatibilityEnhancements",
        // Winget源
        "Microsoft.Winget.Source",
        // AI管理器
        "aimgr",
    ];

    let lower_name = package_name.to_lowercase();
    for critical in &critical_packages {
        if lower_name.contains(&critical.to_lowercase()) {
            return true;
        }
    }

    false
}
