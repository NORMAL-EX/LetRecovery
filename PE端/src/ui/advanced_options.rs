use crate::core::config::InstallConfig;
use crate::core::dism::Dism;
use crate::core::registry::OfflineRegistry;
use crate::utils::path;
use std::path::{Path, PathBuf};

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

            // 先卸载注册表，因为驱动注入可能需要独占访问
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

    // ============ Win7 专用选项 ============

    // 12. Win7 注入 USB3 驱动
    if config.win7_inject_usb3_driver {
        log::info!("[ADVANCED] Win7: 开始注入USB3驱动");
        let usb3_dir = path::get_exe_dir().join("drivers").join("usb3");
        
        if usb3_dir.is_dir() {
            // 先卸载注册表
            let _ = OfflineRegistry::unload_hive("pc-soft");
            let _ = OfflineRegistry::unload_hive("pc-sys");
            if default_loaded {
                let _ = OfflineRegistry::unload_hive("pc-default");
            }
            
            // 处理驱动（包括解压.cab文件）
            match prepare_win7_drivers(&usb3_dir) {
                Ok(processed_path) => {
                    let dism = Dism::new();
                    let image_path = format!("{}\\", target_partition);
                    match dism.add_drivers_offline(&image_path, &processed_path.to_string_lossy()) {
                        Ok(_) => log::info!("[ADVANCED] Win7 USB3驱动注入成功"),
                        Err(e) => log::warn!("[ADVANCED] Win7 USB3驱动注入失败: {} (继续执行)", e),
                    }
                    
                    // 清理临时目录
                    if processed_path != usb3_dir {
                        let _ = std::fs::remove_dir_all(&processed_path);
                    }
                }
                Err(e) => log::warn!("[ADVANCED] Win7 USB3驱动准备失败: {}", e),
            }
            
            // 重新加载注册表
            let _ = OfflineRegistry::load_hive("pc-soft", &software_hive);
            let _ = OfflineRegistry::load_hive("pc-sys", &system_hive);
            if default_loaded {
                let _ = OfflineRegistry::load_hive("pc-default", &default_hive);
            }
        } else {
            log::warn!("[ADVANCED] Win7 USB3驱动目录不存在: {}", usb3_dir.display());
        }
    }

    // 13. Win7 注入 NVMe 驱动
    if config.win7_inject_nvme_driver {
        log::info!("[ADVANCED] Win7: 开始注入NVMe驱动");
        let nvme_dir = path::get_exe_dir().join("drivers").join("nvme");
        
        if nvme_dir.is_dir() {
            // 先卸载注册表
            let _ = OfflineRegistry::unload_hive("pc-soft");
            let _ = OfflineRegistry::unload_hive("pc-sys");
            if default_loaded {
                let _ = OfflineRegistry::unload_hive("pc-default");
            }
            
            // 使用新的处理函数
            match install_win7_nvme_drivers(&nvme_dir, target_partition) {
                Ok(_) => log::info!("[ADVANCED] Win7 NVMe驱动注入成功"),
                Err(e) => log::warn!("[ADVANCED] Win7 NVMe驱动注入失败: {} (继续执行)", e),
            }
            
            // 重新加载注册表
            let _ = OfflineRegistry::load_hive("pc-soft", &software_hive);
            let _ = OfflineRegistry::load_hive("pc-sys", &system_hive);
            if default_loaded {
                let _ = OfflineRegistry::load_hive("pc-default", &default_hive);
            }
        } else {
            log::warn!("[ADVANCED] Win7 NVMe驱动目录不存在: {}", nvme_dir.display());
        }
    }

    // 14. Win7 修复 ACPI_BIOS_ERROR (0xA5) 蓝屏
    if config.win7_fix_acpi_bsod {
        log::info!("[ADVANCED] Win7: 修复ACPI蓝屏问题");
        
        // 禁用 intelppm 服务 (Intel 电源管理)
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\intelppm",
            "Start",
            4, // 4 = Disabled
        );
        
        // 禁用 amdppm 服务 (AMD 电源管理)
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\amdppm",
            "Start",
            4,
        );
        
        // 禁用 Processor 服务
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\Processor",
            "Start",
            4,
        );
        
        // 同时设置 ControlSet002 (如果存在)
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\intelppm",
            "Start",
            4,
        );
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\amdppm",
            "Start",
            4,
        );
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\Processor",
            "Start",
            4,
        );
        
        log::info!("[ADVANCED] Win7 ACPI蓝屏修复设置完成");
    }

    // 15. Win7 修复 INACCESSIBLE_BOOT_DEVICE (0x7B) 蓝屏
    if config.win7_fix_storage_bsod {
        log::info!("[ADVANCED] Win7: 修复存储控制器蓝屏问题 (0x7B)");
        
        // ========== AHCI 相关驱动 ==========
        // msahci - Microsoft AHCI 驱动 (Win7原版自带但默认禁用)
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\msahci",
            "Start",
            0, // 0 = Boot
        );
        
        // iaStorV - Intel 存储驱动
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\iaStorV",
            "Start",
            0,
        );
        
        // iaStorAV - Intel AHCI 驱动
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\iaStorAV",
            "Start",
            0,
        );
        
        // iaStor - Intel SATA 驱动
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\iaStor",
            "Start",
            0,
        );
        
        // iaStorA - Intel AHCI Controller
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\iaStorA",
            "Start",
            0,
        );
        
        // ========== AMD/ATI 存储驱动 ==========
        // amd_sata - AMD SATA 驱动
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\amd_sata",
            "Start",
            0,
        );
        
        // amd_xata - AMD XATA 驱动
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\amd_xata",
            "Start",
            0,
        );
        
        // amdsata - AMD SATA 驱动 (另一个版本)
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\amdsata",
            "Start",
            0,
        );
        
        // amdxata - AMD XATA 驱动 (另一个版本)
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\amdxata",
            "Start",
            0,
        );
        
        // ========== NVMe 驱动 ==========
        // stornvme - Microsoft NVMe 驱动 (Win8+)
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\stornvme",
            "Start",
            0,
        );
        
        // ========== 标准 Windows 存储驱动 ==========
        // storahci - 标准 AHCI 驱动 (Win8+)
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\storahci",
            "Start",
            0,
        );
        
        // pciide - PCI IDE 控制器
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\pciide",
            "Start",
            0,
        );
        
        // intelide - Intel IDE 控制器
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\intelide",
            "Start",
            0,
        );
        
        // atapi - ATAPI 驱动
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\atapi",
            "Start",
            0,
        );
        
        // ========== 同时设置 ControlSet002 ==========
        // msahci
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\msahci",
            "Start",
            0,
        );
        // iaStorV
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\iaStorV",
            "Start",
            0,
        );
        // iaStorAV
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\iaStorAV",
            "Start",
            0,
        );
        // iaStor
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\iaStor",
            "Start",
            0,
        );
        // iaStorA
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\iaStorA",
            "Start",
            0,
        );
        // amd_sata
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\amd_sata",
            "Start",
            0,
        );
        // amd_xata
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\amd_xata",
            "Start",
            0,
        );
        // amdsata
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\amdsata",
            "Start",
            0,
        );
        // amdxata
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\amdxata",
            "Start",
            0,
        );
        // stornvme
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\stornvme",
            "Start",
            0,
        );
        // storahci
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\storahci",
            "Start",
            0,
        );
        // pciide
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\pciide",
            "Start",
            0,
        );
        // intelide
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\intelide",
            "Start",
            0,
        );
        // atapi
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\atapi",
            "Start",
            0,
        );
        
        log::info!("[ADVANCED] Win7 存储控制器蓝屏修复设置完成");
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

/// 安装 Win7 NVMe 驱动
/// 
/// 智能检测并处理两种类型的驱动包：
/// 1. Windows Update CAB包（如KB2990941、KB3087873）- 使用DISM API安装
/// 2. 普通驱动包（包含INF文件）- 使用驱动导入方式
/// 
/// # 参数
/// - `nvme_dir`: NVMe驱动目录
/// - `target_partition`: 目标分区（如 "D:"）
fn install_win7_nvme_drivers(nvme_dir: &Path, target_partition: &str) -> anyhow::Result<()> {
    // CabinetExtractor 已通过其他函数间接使用，无需直接导入
    
    log::info!("[NVME] 开始处理NVMe驱动目录: {}", nvme_dir.display());
    
    // 收集目录中的文件
    let mut cab_files: Vec<PathBuf> = Vec::new();
    let mut inf_files: Vec<PathBuf> = Vec::new();
    let mut has_subdirs = false;
    
    for entry in std::fs::read_dir(nvme_dir)? {
        let entry = entry?;
        let path = entry.path();
        
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let ext_lower = ext.to_lowercase();
                if ext_lower == "cab" {
                    cab_files.push(path);
                } else if ext_lower == "inf" {
                    inf_files.push(path);
                }
            }
        } else if path.is_dir() {
            has_subdirs = true;
        }
    }
    
    log::info!("[NVME] 发现: {} 个CAB文件, {} 个INF文件, 子目录={}", 
        cab_files.len(), inf_files.len(), has_subdirs);
    
    let mut success_count = 0;
    let mut fail_count = 0;
    
    // 处理CAB文件
    for cab_path in &cab_files {
        log::info!("[NVME] 处理CAB文件: {}", cab_path.display());
        
        // 检测CAB类型
        let cab_type = detect_cab_type(cab_path);
        
        match cab_type {
            CabType::WindowsUpdate => {
                // Windows Update包 - 使用dism.exe安装
                log::info!("[NVME] 检测到Windows Update包，使用dism.exe安装");
                let dism = Dism::new();
                let image_path = format!("{}\\", target_partition);
                match dism.add_package_offline(&image_path, &cab_path.to_string_lossy()) {
                    Ok(_) => {
                        log::info!("[NVME] Windows Update包安装成功: {}", cab_path.display());
                        success_count += 1;
                    }
                    Err(e) => {
                        log::warn!("[NVME] Windows Update包安装失败: {} - {}", cab_path.display(), e);
                        // 尝试备用方法：解压并手动复制驱动文件
                        if let Ok(_) = install_cab_as_driver_fallback(cab_path, target_partition) {
                            log::info!("[NVME] 备用方法安装成功");
                            success_count += 1;
                        } else {
                            fail_count += 1;
                        }
                    }
                }
            }
            CabType::DriverPackage => {
                // 驱动包 - 解压后使用驱动导入
                log::info!("[NVME] 检测到驱动包，解压后导入");
                match install_cab_as_driver(cab_path, target_partition) {
                    Ok(_) => {
                        success_count += 1;
                    }
                    Err(e) => {
                        log::warn!("[NVME] 驱动包安装失败: {} - {}", cab_path.display(), e);
                        fail_count += 1;
                    }
                }
            }
            CabType::Unknown => {
                // 未知类型 - 尝试两种方式
                log::info!("[NVME] CAB类型未知，尝试多种方法");
                
                // 先尝试dism.exe安装
                let dism = Dism::new();
                let image_path = format!("{}\\", target_partition);
                let dism_result = dism.add_package_offline(&image_path, &cab_path.to_string_lossy());
                if dism_result.is_ok() {
                    success_count += 1;
                    continue;
                }
                
                // 再尝试驱动导入
                match install_cab_as_driver(cab_path, target_partition) {
                    Ok(_) => success_count += 1,
                    Err(_) => fail_count += 1,
                }
            }
        }
    }
    
    // 处理直接的INF文件和子目录
    if !inf_files.is_empty() || has_subdirs {
        log::info!("[NVME] 处理INF文件和子目录");
        let dism = Dism::new();
        let image_path = format!("{}\\", target_partition);
        
        match dism.add_drivers_offline(&image_path, &nvme_dir.to_string_lossy()) {
            Ok(_) => {
                log::info!("[NVME] 驱动目录导入成功");
                success_count += 1;
            }
            Err(e) => {
                log::warn!("[NVME] 驱动目录导入失败: {}", e);
                fail_count += 1;
            }
        }
    }
    
    log::info!("[NVME] NVMe驱动处理完成: 成功={}, 失败={}", success_count, fail_count);
    
    if success_count == 0 && fail_count > 0 {
        anyhow::bail!("所有NVMe驱动安装失败");
    }
    
    Ok(())
}

/// CAB文件类型
#[derive(Debug, Clone, Copy, PartialEq)]
enum CabType {
    /// Windows Update包（如KB2990941）
    WindowsUpdate,
    /// 普通驱动包（包含INF/SYS）
    DriverPackage,
    /// 未知类型
    Unknown,
}

/// 检测CAB文件类型
fn detect_cab_type(cab_path: &Path) -> CabType {
    use crate::core::cabinet::CabinetExtractor;
    
    // 先根据文件名判断
    let file_name = cab_path.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    
    // Windows Update包通常包含KB编号
    if file_name.contains("kb") || 
       file_name.contains("windows6") ||
       file_name.contains("windows8") ||
       file_name.contains("windows10") {
        return CabType::WindowsUpdate;
    }
    
    // 尝试解压并检查内容
    let temp_dir = std::env::temp_dir()
        .join(format!("LetRecovery_CabDetect_{}", std::process::id()));
    
    if let Err(_) = std::fs::create_dir_all(&temp_dir) {
        return CabType::Unknown;
    }
    
    let extractor = match CabinetExtractor::new() {
        Ok(e) => e,
        Err(_) => return CabType::Unknown,
    };
    
    // 只解压少量文件来检测
    match extractor.extract(cab_path, &temp_dir) {
        Ok(files) => {
            // 检查是否包含manifest文件（Windows Update特征）
            let has_manifest = files.iter().any(|p| {
                let name = p.to_string_lossy().to_lowercase();
                name.ends_with(".manifest") || 
                name.ends_with(".mum") ||
                name.contains("update.mum")
            });
            
            // 检查是否包含INF文件（驱动包特征）
            let has_inf = files.iter().any(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("inf"))
                    .unwrap_or(false)
            });
            
            // 检查是否包含嵌套cab
            let has_nested_cab = files.iter().any(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("cab"))
                    .unwrap_or(false)
            });
            
            // 清理临时目录
            let _ = std::fs::remove_dir_all(&temp_dir);
            
            if has_manifest || has_nested_cab {
                CabType::WindowsUpdate
            } else if has_inf {
                CabType::DriverPackage
            } else {
                CabType::Unknown
            }
        }
        Err(_) => {
            let _ = std::fs::remove_dir_all(&temp_dir);
            CabType::Unknown
        }
    }
}

/// 将CAB作为驱动包安装（解压后导入INF）
fn install_cab_as_driver(cab_path: &Path, target_partition: &str) -> anyhow::Result<()> {
    use crate::core::cabinet::CabinetExtractor;
    
    log::info!("[NVME] 解压驱动CAB: {}", cab_path.display());
    
    let temp_dir = std::env::temp_dir()
        .join(format!("LetRecovery_Driver_{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir)?;
    
    let extractor = CabinetExtractor::new()?;
    let _files = extractor.extract(cab_path, &temp_dir)?;
    
    // 检查是否有嵌套cab
    process_nested_cabs_for_drivers(&temp_dir)?;
    
    // 使用Dism导入驱动
    let dism = Dism::new();
    let image_path = format!("{}\\", target_partition);
    let result = dism.add_drivers_offline(&image_path, &temp_dir.to_string_lossy());
    
    // 清理
    let _ = std::fs::remove_dir_all(&temp_dir);
    
    result
}

/// 处理嵌套的CAB文件
fn process_nested_cabs_for_drivers(dir: &Path) -> anyhow::Result<()> {
    use crate::core::cabinet::CabinetExtractor;
    
    // 查找嵌套cab
    let mut nested_cabs = Vec::new();
    find_nested_cabs(dir, &mut nested_cabs);
    
    if nested_cabs.is_empty() {
        return Ok(());
    }
    
    log::info!("[NVME] 处理 {} 个嵌套CAB", nested_cabs.len());
    
    let extractor = CabinetExtractor::new()?;
    
    for cab in nested_cabs {
        let extract_dir = cab.with_extension("extracted");
        if let Ok(_) = extractor.extract(&cab, &extract_dir) {
            // 递归处理
            let _ = process_nested_cabs_for_drivers(&extract_dir);
        }
    }
    
    Ok(())
}

/// 递归查找嵌套CAB
fn find_nested_cabs(dir: &Path, cabs: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                find_nested_cabs(&path, cabs);
            } else if path.is_file() {
                if let Some(ext) = path.extension() {
                    if ext.to_string_lossy().to_lowercase() == "cab" {
                        cabs.push(path);
                    }
                }
            }
        }
    }
}

/// 备用方法：直接复制驱动文件
fn install_cab_as_driver_fallback(cab_path: &Path, target_partition: &str) -> anyhow::Result<()> {
    use crate::core::cabinet::CabinetExtractor;
    
    log::info!("[NVME] 使用备用方法处理: {}", cab_path.display());
    
    let temp_dir = std::env::temp_dir()
        .join(format!("LetRecovery_Fallback_{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir)?;
    
    let extractor = CabinetExtractor::new()?;
    let _ = extractor.extract(cab_path, &temp_dir)?;
    
    // 处理嵌套cab
    let _ = process_nested_cabs_for_drivers(&temp_dir);
    
    // 目标目录
    let system32_drivers = PathBuf::from(target_partition)
        .join("Windows")
        .join("System32")
        .join("drivers");
    let inf_dir = PathBuf::from(target_partition)
        .join("Windows")
        .join("INF");
    
    std::fs::create_dir_all(&system32_drivers)?;
    std::fs::create_dir_all(&inf_dir)?;
    
    // 复制所有驱动文件
    copy_driver_files_recursive(&temp_dir, &system32_drivers, &inf_dir)?;
    
    // 注册驱动服务
    register_nvme_driver_services(target_partition)?;
    
    // 清理
    let _ = std::fs::remove_dir_all(&temp_dir);
    
    Ok(())
}

/// 递归复制驱动文件
fn copy_driver_files_recursive(
    source: &Path,
    drivers_dir: &Path,
    inf_dir: &Path,
) -> anyhow::Result<usize> {
    let mut count = 0;
    
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        
        if path.is_dir() {
            count += copy_driver_files_recursive(&path, drivers_dir, inf_dir)?;
        } else if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let ext_lower = ext.to_lowercase();
                let file_name = entry.file_name();
                
                match ext_lower.as_str() {
                    "sys" => {
                        let dest = drivers_dir.join(&file_name);
                        if !dest.exists() {
                            std::fs::copy(&path, &dest)?;
                            log::info!("[NVME] 复制驱动: {:?}", file_name);
                            count += 1;
                        }
                    }
                    "inf" => {
                        let dest = inf_dir.join(&file_name);
                        if !dest.exists() {
                            std::fs::copy(&path, &dest)?;
                            log::info!("[NVME] 复制INF: {:?}", file_name);
                            count += 1;
                        }
                    }
                    "cat" => {
                        let dest = inf_dir.join(&file_name);
                        if !dest.exists() {
                            let _ = std::fs::copy(&path, &dest);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    
    Ok(count)
}

/// 注册NVMe驱动服务到离线注册表
fn register_nvme_driver_services(target_partition: &str) -> anyhow::Result<()> {
    let system_hive = format!("{}\\Windows\\System32\\config\\SYSTEM", target_partition);
    
    if !std::path::Path::new(&system_hive).exists() {
        log::warn!("[NVME] SYSTEM hive不存在，跳过服务注册");
        return Ok(());
    }
    
    let hive_key = format!("nvme_drv_{}", std::process::id());
    
    if OfflineRegistry::load_hive(&hive_key, &system_hive).is_err() {
        log::warn!("[NVME] 无法加载SYSTEM hive，跳过服务注册");
        return Ok(());
    }
    
    // 注册stornvme服务（NVMe标准驱动）
    let services = [
        ("stornvme", "stornvme.sys", 0u32, 0u32), // Boot start
        ("storahci", "storahci.sys", 0, 0),
        ("msahci", "msahci.sys", 0, 0),
    ];
    
    for (service_name, binary, service_type, start_type) in &services {
        let key_path = format!("HKLM\\{}\\ControlSet001\\Services\\{}", hive_key, service_name);
        
        let _ = OfflineRegistry::create_key(&key_path);
        let _ = OfflineRegistry::set_dword(&key_path, "Type", *service_type);
        let _ = OfflineRegistry::set_dword(&key_path, "Start", *start_type);
        let _ = OfflineRegistry::set_dword(&key_path, "ErrorControl", 1);
        let _ = OfflineRegistry::set_expand_string(
            &key_path, 
            "ImagePath", 
            &format!("System32\\drivers\\{}", binary)
        );
        
        // 同时设置ControlSet002
        let key_path2 = format!("HKLM\\{}\\ControlSet002\\Services\\{}", hive_key, service_name);
        let _ = OfflineRegistry::create_key(&key_path2);
        let _ = OfflineRegistry::set_dword(&key_path2, "Type", *service_type);
        let _ = OfflineRegistry::set_dword(&key_path2, "Start", *start_type);
        let _ = OfflineRegistry::set_dword(&key_path2, "ErrorControl", 1);
        let _ = OfflineRegistry::set_expand_string(
            &key_path2, 
            "ImagePath", 
            &format!("System32\\drivers\\{}", binary)
        );
    }
    
    let _ = OfflineRegistry::unload_hive(&hive_key);
    
    log::info!("[NVME] NVMe服务注册完成");
    Ok(())
}

/// 准备 Win7 驱动目录
/// 
/// 如果目录中包含 .cab 文件，会将其解压到临时目录。
/// 支持 Windows 更新包格式（如 KB2990941、KB3087873）。
fn prepare_win7_drivers(driver_dir: &PathBuf) -> anyhow::Result<PathBuf> {
    use crate::core::cabinet::CabinetExtractor;
    
    // 检查目录中是否有 .cab 文件
    let mut cab_files: Vec<PathBuf> = Vec::new();
    let mut has_inf_files = false;
    let mut has_subdirs = false;
    
    for entry in std::fs::read_dir(driver_dir)? {
        let entry = entry?;
        let path = entry.path();
        
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let ext_lower = ext.to_lowercase();
                if ext_lower == "cab" {
                    cab_files.push(path);
                } else if ext_lower == "inf" {
                    has_inf_files = true;
                }
            }
        } else if path.is_dir() {
            has_subdirs = true;
        }
    }
    
    // 如果没有 .cab 文件，直接返回原目录
    if cab_files.is_empty() {
        log::info!("[ADVANCED] 目录中没有 .cab 文件，直接使用原目录");
        return Ok(driver_dir.clone());
    }
    
    log::info!("[ADVANCED] 发现 {} 个 .cab 文件，开始解压", cab_files.len());
    
    // 尝试创建 Cabinet 解压器
    let extractor = match CabinetExtractor::new() {
        Ok(e) => e,
        Err(e) => {
            log::warn!("[ADVANCED] 无法创建 Cabinet 解压器: {} (将使用原目录)", e);
            return Ok(driver_dir.clone());
        }
    };
    
    // 创建临时目录
    let temp_dir = std::env::temp_dir()
        .join(format!("LetRecovery_Win7Drivers_{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir)?;
    
    // 解压所有 .cab 文件
    let mut extract_success_count = 0;
    
    for cab_path in &cab_files {
        let cab_name = cab_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        
        let extract_dir = temp_dir.join(cab_name);
        
        log::info!("[ADVANCED] 解压: {} -> {}", 
            cab_path.display(), extract_dir.display());
        
        match extractor.extract(cab_path, &extract_dir) {
            Ok(files) => {
                log::info!("[ADVANCED] 成功解压 {} 个文件", files.len());
                extract_success_count += 1;
            }
            Err(e) => {
                log::warn!("[ADVANCED] 解压 {} 失败: {} (跳过)", cab_path.display(), e);
            }
        }
    }
    
    // 如果所有 cab 文件都解压失败，清理临时目录并返回原目录
    if extract_success_count == 0 {
        log::warn!("[ADVANCED] 所有 .cab 文件解压失败，使用原目录");
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Ok(driver_dir.clone());
    }
    
    // 如果原目录有普通驱动文件或子目录，也复制到临时目录
    if has_inf_files || has_subdirs {
        log::info!("[ADVANCED] 复制原目录中的其他驱动文件");
        
        for entry in std::fs::read_dir(driver_dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_name = entry.file_name();
            
            // 跳过 .cab 文件（已处理）
            if path.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if ext.to_lowercase() == "cab" {
                        continue;
                    }
                }
            }
            
            let dest = temp_dir.join(&file_name);
            
            if path.is_dir() {
                // 递归复制子目录
                copy_dir_recursive(&path, &dest)?;
            } else {
                // 复制文件
                std::fs::copy(&path, &dest)?;
            }
        }
    }
    
    log::info!("[ADVANCED] Win7 驱动准备完成: {}", temp_dir.display());
    
    Ok(temp_dir)
}

/// 递归复制目录
fn copy_dir_recursive(src: &PathBuf, dst: &PathBuf) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dest = dst.join(entry.file_name());
        
        if path.is_dir() {
            copy_dir_recursive(&path, &dest)?;
        } else {
            std::fs::copy(&path, &dest)?;
        }
    }
    
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

/// 应用 UefiSeven 补丁到目标系统（PE环境版本）
/// 
/// 此方法应在引导修复之后调用。
/// UefiSeven 是一个 EFI 加载器，用于模拟 Int10h 中断，使 Windows 7 能够在 UEFI Class 3 系统上启动。
/// 
/// 参考: https://github.com/manatails/uefiseven
pub fn apply_uefiseven_patch(data_partition: &str, _target_partition: &str) -> anyhow::Result<()> {
    use crate::core::bcdedit::BootManager;
    use std::path::Path;
    
    log::info!("[UEFISEVEN] 开始应用 UefiSeven 补丁");
    
    // 从数据分区查找 UefiSeven 文件
    let data_dir = crate::core::config::ConfigFileManager::get_data_dir(data_partition);
    let uefiseven_dir = format!("{}\\uefiseven", data_dir);
    let uefiseven_efi = format!("{}\\bootx64.efi", uefiseven_dir);
    let uefiseven_ini = format!("{}\\UefiSeven.ini", uefiseven_dir);
    
    if !Path::new(&uefiseven_efi).exists() {
        log::warn!("[UEFISEVEN] UefiSeven bootx64.efi 不存在: {}", uefiseven_efi);
        return Err(anyhow::anyhow!("UefiSeven bootx64.efi 不存在: {}", uefiseven_efi));
    }
    
    log::info!("[UEFISEVEN] 找到 UefiSeven 文件: {}", uefiseven_efi);
    
    // 查找并挂载 EFI 分区
    let boot_manager = BootManager::new();
    let esp_letter = boot_manager.find_and_mount_esp()
        .map_err(|e| anyhow::anyhow!("查找 EFI 分区失败: {}", e))?;
    
    log::info!("[UEFISEVEN] EFI 分区: {}", esp_letter);
    
    // Microsoft Boot 目录
    let ms_boot_dir = format!("{}\\EFI\\Microsoft\\Boot", esp_letter);
    let bootmgfw_path = format!("{}\\bootmgfw.efi", ms_boot_dir);
    let bootmgfw_original = format!("{}\\bootmgfw.original.efi", ms_boot_dir);
    let uefiseven_target = format!("{}\\bootmgfw.efi", ms_boot_dir);
    let uefiseven_ini_target = format!("{}\\UefiSeven.ini", ms_boot_dir);
    
    // 检查原始 bootmgfw.efi 是否存在
    if !Path::new(&bootmgfw_path).exists() {
        log::warn!("[UEFISEVEN] bootmgfw.efi 不存在: {}", bootmgfw_path);
        return Err(anyhow::anyhow!("bootmgfw.efi 不存在，请确保引导修复已完成"));
    }
    
    // 备份原始 bootmgfw.efi（如果尚未备份）
    if !Path::new(&bootmgfw_original).exists() {
        log::info!("[UEFISEVEN] 备份原始 bootmgfw.efi 到 bootmgfw.original.efi");
        std::fs::copy(&bootmgfw_path, &bootmgfw_original)?;
    } else {
        log::info!("[UEFISEVEN] bootmgfw.original.efi 已存在，跳过备份");
    }
    
    // 复制 UefiSeven 到 bootmgfw.efi（替换原来的）
    log::info!("[UEFISEVEN] 部署 UefiSeven bootx64.efi -> bootmgfw.efi");
    std::fs::copy(&uefiseven_efi, &uefiseven_target)?;
    
    // 复制配置文件（如果存在）
    if Path::new(&uefiseven_ini).exists() {
        log::info!("[UEFISEVEN] 部署 UefiSeven.ini 配置文件");
        std::fs::copy(&uefiseven_ini, &uefiseven_ini_target)?;
    } else {
        // 创建默认配置文件
        log::info!("[UEFISEVEN] 创建默认 UefiSeven.ini 配置");
        let default_config = r#"[uefiseven]
; Skip any warnings and errors during boot
skiperrors=0
; Enable verbose logging (set to 1 for debugging)
verbose=0
; Log output to file (requires verbose=1)
log=0
"#;
        std::fs::write(&uefiseven_ini_target, default_config)?;
    }
    
    log::info!("[UEFISEVEN] UefiSeven 补丁应用成功");
    log::info!("[UEFISEVEN] 启动流程: UEFI -> UefiSeven -> bootmgfw.original.efi -> Windows 7");
    
    Ok(())
}
