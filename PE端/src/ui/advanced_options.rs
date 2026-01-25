use crate::core::config::InstallConfig;
use crate::core::dism::Dism;
use crate::core::registry::OfflineRegistry;
use crate::utils::path;

/// 脚本目录名称（统一路径，与正常系统端保持一致）
const SCRIPTS_DIR: &str = "LetRecovery_Scripts";

/// 应用高级选项到目标系统
/// 
/// 此函数在PE环境中执行，负责将用户选择的高级选项应用到目标系统。
/// 通过离线修改注册表和生成必要的脚本来实现各项功能。
pub fn apply_advanced_options(target_partition: &str, config: &InstallConfig) -> anyhow::Result<()> {
    let windows_path = format!("{}\\Windows", target_partition);
    let software_hive = format!("{}\\System32\\config\\SOFTWARE", windows_path);
    let system_hive = format!("{}\\System32\\config\\SYSTEM", windows_path);
    let default_hive = format!("{}\\System32\\config\\DEFAULT", windows_path);

    log::info!("[ADVANCED] 开始应用高级选项到: {}", target_partition);

    // 加载离线注册表
    log::info!("[ADVANCED] 加载离线注册表...");
    OfflineRegistry::load_hive("pc-soft", &software_hive)?;
    OfflineRegistry::load_hive("pc-sys", &system_hive)?;
    
    // DEFAULT hive 用于设置默认用户配置（如经典右键菜单）
    let default_loaded = OfflineRegistry::load_hive("pc-default", &default_hive).is_ok();
    if default_loaded {
        log::info!("[ADVANCED] DEFAULT hive 加载成功");
    } else {
        log::warn!("[ADVANCED] DEFAULT hive 加载失败，部分用户级设置可能无法应用");
    }

    // 创建脚本目录（用于存放自定义脚本）
    let scripts_dir = format!("{}\\{}", target_partition, SCRIPTS_DIR);
    std::fs::create_dir_all(&scripts_dir)?;
    log::info!("[ADVANCED] 脚本目录: {}", scripts_dir);

    // ============ 系统优化选项 ============

    // 1. 移除快捷方式小箭头
    if config.remove_shortcut_arrow {
        log::info!("[ADVANCED] 移除快捷方式小箭头");
        let _ = OfflineRegistry::set_string(
            "HKLM\\pc-soft\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Shell Icons",
            "29",
            "%systemroot%\\system32\\imageres.dll,197",
        );
    }

    // 2. Win11恢复经典右键菜单
    if config.restore_classic_context_menu {
        log::info!("[ADVANCED] 恢复经典右键菜单");
        // 在 DEFAULT hive 中设置（影响所有新用户）
        if default_loaded {
            // 创建空的 InprocServer32 键，这会禁用新式右键菜单
            let _ = OfflineRegistry::create_key(
                "HKLM\\pc-default\\Software\\Classes\\CLSID\\{86ca1aa0-34aa-4e8b-a509-50c905bae2a2}\\InprocServer32"
            );
            // 设置默认值为空字符串
            let _ = OfflineRegistry::set_string(
                "HKLM\\pc-default\\Software\\Classes\\CLSID\\{86ca1aa0-34aa-4e8b-a509-50c905bae2a2}\\InprocServer32",
                "",
                "",
            );
        }
        // 同时在 SOFTWARE 中设置（系统级）
        let _ = OfflineRegistry::create_key(
            "HKLM\\pc-soft\\Classes\\CLSID\\{86ca1aa0-34aa-4e8b-a509-50c905bae2a2}\\InprocServer32"
        );
        let _ = OfflineRegistry::set_string(
            "HKLM\\pc-soft\\Classes\\CLSID\\{86ca1aa0-34aa-4e8b-a509-50c905bae2a2}\\InprocServer32",
            "",
            "",
        );
    }

    // 3. OOBE绕过强制联网
    if config.bypass_nro {
        log::info!("[ADVANCED] 设置OOBE绕过联网");
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-soft\\Microsoft\\Windows\\CurrentVersion\\OOBE",
            "BypassNRO",
            1,
        );
    }

    // 4. 禁用Windows更新
    if config.disable_windows_update {
        log::info!("[ADVANCED] 禁用Windows更新服务");
        // 禁用 Windows Update 服务 (Start=4 表示禁用)
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\wuauserv",
            "Start",
            4,
        );
        // 禁用 Update Orchestrator Service
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\UsoSvc",
            "Start",
            4,
        );
        // 设置策略禁用自动更新
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-soft\\Policies\\Microsoft\\Windows\\WindowsUpdate\\AU",
            "NoAutoUpdate",
            1,
        );
    }

    // 5. 禁用Windows安全中心/Defender
    if config.disable_windows_defender {
        log::info!("[ADVANCED] 禁用Windows Defender");
        // 禁用反间谍软件（Defender主开关）
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-soft\\Policies\\Microsoft\\Windows Defender",
            "DisableAntiSpyware",
            1,
        );
        // 禁用实时保护
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-soft\\Policies\\Microsoft\\Windows Defender\\Real-Time Protection",
            "DisableRealtimeMonitoring",
            1,
        );
        // 禁用 Windows Defender 服务 (Start=4 表示禁用)
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\WinDefend",
            "Start",
            4,
        );
        // 禁用 Defender 网络检查服务
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\WdNisSvc",
            "Start",
            4,
        );
        // 禁用安全健康服务
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\SecurityHealthService",
            "Start",
            4,
        );
    }

    // 6. 禁用系统保留空间
    if config.disable_reserved_storage {
        log::info!("[ADVANCED] 禁用系统保留空间");
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-soft\\Microsoft\\Windows\\CurrentVersion\\ReserveManager",
            "ShippedWithReserves",
            0,
        );
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-soft\\Microsoft\\Windows\\CurrentVersion\\ReserveManager",
            "PassedPolicy",
            0,
        );
    }

    // 7. 禁用UAC
    if config.disable_uac {
        log::info!("[ADVANCED] 禁用UAC");
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-soft\\Microsoft\\Windows\\CurrentVersion\\Policies\\System",
            "EnableLUA",
            0,
        );
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-soft\\Microsoft\\Windows\\CurrentVersion\\Policies\\System",
            "ConsentPromptBehaviorAdmin",
            0,
        );
    }

    // 8. 禁用自动设备加密 (BitLocker)
    if config.disable_device_encryption {
        log::info!("[ADVANCED] 禁用自动设备加密");
        // 禁用 BitLocker 自动加密
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Control\\BitLocker",
            "PreventDeviceEncryption",
            1,
        );
        // 禁用 MBAM (Microsoft BitLocker Administration and Monitoring)
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-soft\\Policies\\Microsoft\\FVE",
            "OSRecovery",
            0,
        );
        // 禁用 BitLocker 服务
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\BDESVC",
            "Start",
            4,
        );
    }

    // 9. 删除预装UWP应用 - 生成PowerShell脚本
    if config.remove_uwp_apps {
        log::info!("[ADVANCED] 配置删除预装UWP应用");
        // 创建首次登录脚本来删除UWP应用
        let remove_uwp_script = generate_remove_uwp_script();
        let uwp_script_path = format!("{}\\remove_uwp.ps1", scripts_dir);
        std::fs::write(&uwp_script_path, &remove_uwp_script)?;
        log::info!("[ADVANCED] UWP删除脚本已写入: {}", uwp_script_path);
    }

    // 10. 导入磁盘控制器驱动（Win10/Win11 x64）
    if config.import_storage_controller_drivers {
        let storage_drivers_dir = path::get_exe_dir()
            .join("drivers")
            .join("storage_controller");
        if storage_drivers_dir.is_dir() {
            log::info!(
                "[ADVANCED] 导入磁盘控制器驱动: {}",
                storage_drivers_dir.display()
            );

            // 先卸载注册表，因为 DISM 可能需要独占访问
            let _ = OfflineRegistry::unload_hive("pc-soft");
            let _ = OfflineRegistry::unload_hive("pc-sys");
            if default_loaded {
                let _ = OfflineRegistry::unload_hive("pc-default");
            }

            let dism = Dism::new();
            let image_path = format!("{}\\", target_partition);
            let storage_drivers_path = storage_drivers_dir.to_string_lossy().to_string();
            match dism.add_drivers_offline(&image_path, &storage_drivers_path) {
                Ok(_) => log::info!("[ADVANCED] 磁盘控制器驱动导入成功"),
                Err(e) => log::warn!("[ADVANCED] 磁盘控制器驱动导入失败: {}", e),
            }

            // 重新加载注册表
            let _ = OfflineRegistry::load_hive("pc-soft", &software_hive);
            let _ = OfflineRegistry::load_hive("pc-sys", &system_hive);
            if default_loaded {
                let _ = OfflineRegistry::load_hive("pc-default", &default_hive);
            }
        } else {
            log::warn!(
                "[ADVANCED] 未找到磁盘控制器驱动目录: {}",
                storage_drivers_dir.display()
            );
        }
    }

    // 11. 自定义用户名 - 写入标记文件供无人值守使用
    if !config.custom_username.is_empty() {
        log::info!("[ADVANCED] 设置自定义用户名: {}", config.custom_username);
        let username_file = format!("{}\\username.txt", scripts_dir);
        std::fs::write(&username_file, &config.custom_username)?;
    }

    // 卸载注册表（确保正确卸载）
    log::info!("[ADVANCED] 卸载离线注册表...");
    std::thread::sleep(std::time::Duration::from_millis(500));
    let _ = OfflineRegistry::unload_hive("pc-soft");
    let _ = OfflineRegistry::unload_hive("pc-sys");
    if default_loaded {
        let _ = OfflineRegistry::unload_hive("pc-default");
    }

    log::info!("[ADVANCED] 高级选项应用完成");
    Ok(())
}

/// 生成删除预装UWP应用的PowerShell脚本
fn generate_remove_uwp_script() -> String {
    r#"# LetRecovery - 删除预装UWP应用脚本
# 此脚本会删除大部分预装的UWP应用，保留必要的系统组件

$AppsToRemove = @(
    "Microsoft.3DBuilder"
    "Microsoft.BingFinance"
    "Microsoft.BingNews"
    "Microsoft.BingSports"
    "Microsoft.BingWeather"
    "Microsoft.Getstarted"
    "Microsoft.MicrosoftOfficeHub"
    "Microsoft.MicrosoftSolitaireCollection"
    "Microsoft.Office.OneNote"
    "Microsoft.People"
    "Microsoft.SkypeApp"
    "Microsoft.Windows.Photos"
    "Microsoft.WindowsAlarms"
    "Microsoft.WindowsCamera"
    "Microsoft.WindowsFeedbackHub"
    "Microsoft.WindowsMaps"
    "Microsoft.WindowsSoundRecorder"
    "Microsoft.Xbox.TCUI"
    "Microsoft.XboxApp"
    "Microsoft.XboxGameOverlay"
    "Microsoft.XboxGamingOverlay"
    "Microsoft.XboxIdentityProvider"
    "Microsoft.XboxSpeechToTextOverlay"
    "Microsoft.YourPhone"
    "Microsoft.ZuneMusic"
    "Microsoft.ZuneVideo"
    "Microsoft.GetHelp"
    "Microsoft.Messaging"
    "Microsoft.Print3D"
    "Microsoft.MixedReality.Portal"
    "Microsoft.OneConnect"
    "Microsoft.Wallet"
    "Microsoft.WindowsCommunicationsApps"
    "Microsoft.BingTranslator"
    "Microsoft.DesktopAppInstaller"
    "Microsoft.Advertising.Xaml"
    "Microsoft.549981C3F5F10"
    "Clipchamp.Clipchamp"
    "Disney.37853FC22B2CE"
    "MicrosoftCorporationII.QuickAssist"
    "MicrosoftTeams"
    "SpotifyAB.SpotifyMusic"
)

foreach ($App in $AppsToRemove) {
    Write-Host "正在删除: $App"
    Get-AppxPackage -Name $App -AllUsers | Remove-AppxPackage -AllUsers -ErrorAction SilentlyContinue
    Get-AppxProvisionedPackage -Online | Where-Object {$_.PackageName -like "*$App*"} | Remove-AppxProvisionedPackage -Online -ErrorAction SilentlyContinue
}

Write-Host "UWP应用清理完成"
"#.to_string()
}

/// 获取脚本目录名称
pub fn get_scripts_dir_name() -> &'static str {
    SCRIPTS_DIR
}
