use crate::core::config::InstallConfig;
use crate::core::dism::Dism;
use crate::core::registry::OfflineRegistry;
use crate::utils::path;
use anyhow::Context;
use std::path::Path;

/// 脚本目录名称（统一路径，与正常系统端保持一致）
const SCRIPTS_DIR: &str = "LetRecovery_Scripts";

struct OfflineHiveCleanup(Vec<&'static str>);

impl OfflineHiveCleanup {
    fn unload_all(&mut self) -> anyhow::Result<()> {
        for hive in self.0.iter().rev() {
            OfflineRegistry::unload_hive(hive)?;
        }
        self.0.clear();
        Ok(())
    }
}

impl Drop for OfflineHiveCleanup {
    fn drop(&mut self) {
        for hive in self.0.iter().rev() {
            if let Err(error) = OfflineRegistry::unload_hive(hive) {
                log::error!("[ADVANCED] emergency unload of offline hive {hive} failed: {error}");
            }
        }
    }
}

fn with_offline_hives_unloaded<T>(
    default_loaded: bool,
    software_hive: &str,
    system_hive: &str,
    default_hive: &str,
    operation: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    if default_loaded {
        OfflineRegistry::unload_hive("pc-default")?;
    }
    OfflineRegistry::unload_hive("pc-sys")?;
    OfflineRegistry::unload_hive("pc-soft")?;

    let operation_result = operation();
    let reload_result = (|| -> anyhow::Result<()> {
        OfflineRegistry::load_hive("pc-soft", software_hive)?;
        OfflineRegistry::load_hive("pc-sys", system_hive)?;
        if default_loaded {
            OfflineRegistry::load_hive("pc-default", default_hive)?;
        }
        Ok(())
    })();

    match (operation_result, reload_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(operation_error), Err(reload_error)) => anyhow::bail!(
            "offline operation failed: {operation_error}; additionally failed to reload registry hives: {reload_error}"
        ),
    }
}

/// 注入数据分区上 `user_drivers/<版本>` 的用户驱动（重装当前系统盘的 ViaPE 路径）。
/// 由正常端 start_pe_install_thread 把 `bin/drivers/<版本>` 复制到
/// `{data_dir}\user_drivers\<版本>`。win7/8/10/11 走 DISM 离线注入；
/// XP 由本文件的 XP 注入处理。目录不存在或无驱动则静默跳过、不打断安装。
pub fn inject_user_drivers_from_data(target_partition: &str, data_dir: &str) -> anyhow::Result<()> {
    let version = match detect_user_driver_version(target_partition) {
        Some(v) => v,
        None => return Ok(()),
    };
    let dir = format!("{}\\user_drivers\\{}", data_dir, version);
    if !Path::new(&dir).exists() {
        return Ok(());
    }
    log::info!(
        "[USER DRV] 注入 user_drivers/{} 到 {} ...",
        version,
        target_partition
    );
    lr_core::driver_trust::ensure_pe_driver_signing_trust()
        .context("初始化 PE 用户驱动签名信任链失败")?;
    let dism = Dism::new();
    let image_path = format!("{}\\", target_partition);
    dism.add_drivers_offline(&image_path, &dir)?;
    log::info!("[USER DRV] user_drivers/{} 注入成功", version);
    Ok(())
}

/// 按目标系统 `\Windows\System32\ntdll.dll` 版本识别用户驱动文件夹名。
/// 6.1=win7，6.2/6.3=win8/8.1，10.0 且 build<22000=win10，build>=22000=win11。
fn detect_user_driver_version(target_partition: &str) -> Option<&'static str> {
    let ntdll = Path::new(target_partition)
        .join("Windows")
        .join("System32")
        .join("ntdll.dll");
    let (major, minor, build, _) = crate::core::system_utils::get_file_version(&ntdll)?;
    match (major, minor) {
        (6, 1) => Some("win7"),
        (6, 2) | (6, 3) => Some("win8"),
        (10, _) => Some(if build >= 22000 { "win11" } else { "win10" }),
        _ => None,
    }
}

/// 应用高级选项到目标系统
///
/// 此函数在PE环境中执行，负责将用户选择的高级选项应用到目标系统。
/// 通过离线修改注册表和生成必要的脚本来实现各项功能。
pub fn apply_advanced_options(
    target_partition: &str,
    config: &InstallConfig,
) -> anyhow::Result<()> {
    let windows_path = format!("{}\\Windows", target_partition);
    let software_hive = format!("{}\\System32\\config\\SOFTWARE", windows_path);
    let system_hive = format!("{}\\System32\\config\\SYSTEM", windows_path);
    let default_hive = format!("{}\\System32\\config\\DEFAULT", windows_path);

    log::info!("[ADVANCED] 开始应用高级选项到: {}", target_partition);

    // 加载离线注册表
    log::info!("[ADVANCED] 加载离线注册表...");
    OfflineRegistry::load_hive("pc-soft", &software_hive)?;
    let mut hive_cleanup = OfflineHiveCleanup(vec!["pc-soft"]);
    OfflineRegistry::load_hive("pc-sys", &system_hive)?;
    hive_cleanup.0.push("pc-sys");

    // DEFAULT hive 用于设置默认用户配置（如经典右键菜单）
    let default_loaded = OfflineRegistry::load_hive("pc-default", &default_hive).is_ok();
    if default_loaded {
        hive_cleanup.0.push("pc-default");
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
            "HKLM\\pc-soft\\Classes\\CLSID\\{86ca1aa0-34aa-4e8b-a509-50c905bae2a2}\\InprocServer32",
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

    // 4. 禁用Windows自动更新
    if config.disable_windows_update {
        log::info!("[ADVANCED] 通过策略禁用Windows自动更新");
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-soft\\Policies\\Microsoft\\Windows\\WindowsUpdate\\AU",
            "NoAutoUpdate",
            1,
        );
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-soft\\Policies\\Microsoft\\Windows\\WindowsUpdate\\AU",
            "AUOptions",
            1,
        );
    }

    // 5. 仅深度移除 Microsoft Defender Antivirus 引擎，保留安全中心等组件
    if config.disable_windows_defender {
        match lr_core::defender_removal::remove_offline_defender_engine(
            target_partition,
            "pc-soft",
            "pc-sys",
        ) {
            Ok(report) => log::info!(
                "[ADVANCED] Defender 杀毒引擎移除完成: disabled_services={}, deleted_service_keys={}, removed_paths={}, deleted_task_cache={}, deleted_task_records={}, deleted_engine_software_key={}",
                report.disabled_services,
                report.deleted_service_keys,
                report.removed_paths,
                report.deleted_task_cache,
                report.deleted_task_records,
                report.deleted_engine_software_key
            ),
            Err(error) => {
                let _ = OfflineRegistry::unload_hive("pc-soft");
                let _ = OfflineRegistry::unload_hive("pc-sys");
                if default_loaded {
                    let _ = OfflineRegistry::unload_hive("pc-default");
                }
                return Err(error);
            }
        }
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
        let _ =
            OfflineRegistry::set_dword("HKLM\\pc-soft\\Policies\\Microsoft\\FVE", "OSRecovery", 0);
        // 禁用 BitLocker 服务
        let _ =
            OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet001\\Services\\BDESVC", "Start", 4);
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
        let hardware_ids = lr_core::driver::list_present_hardware_ids().map_err(|error| {
            anyhow::anyhow!("storage-controller hardware enumeration failed: {error}")
        })?;
        let packages = lr_core::storage_driver_match::select_builtin_storage_driver_packages(
            hardware_ids.iter().map(String::as_str),
        )
        .map_err(anyhow::Error::new)?;
        let storage_drivers_dir = path::get_exe_dir()
            .join("drivers")
            .join("storage_controller");
        let verified_packages = packages
            .into_iter()
            .map(|package| {
                let directory = storage_drivers_dir.join(package.directory_name());
                lr_core::storage_driver_match::verify_builtin_storage_driver_package(
                    package, &directory,
                )
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        if verified_packages.is_empty() {
            log::info!(
                "[ADVANCED] no supported Intel VMD controller is present; built-in storage drivers were not staged"
            );
        } else {
            log::info!(
                "[ADVANCED] staging {} hardware-matched Intel VMD package(s)",
                verified_packages.len()
            );

            if default_loaded {
                OfflineRegistry::unload_hive("pc-default")?;
            }
            OfflineRegistry::unload_hive("pc-sys")?;
            OfflineRegistry::unload_hive("pc-soft")?;
            hive_cleanup.0.clear();

            let dism = Dism::new();
            let image_path = format!("{}\\", target_partition);
            let driver_store = Path::new(target_partition)
                .join("Windows")
                .join("System32")
                .join("DriverStore")
                .join("FileRepository");
            let stage_result = (|| -> anyhow::Result<()> {
                for verified in &verified_packages {
                    dism.add_drivers_offline(&image_path, &verified.directory().to_string_lossy())?;
                    for hardware_id in verified.package().controller_hardware_ids() {
                        if !lr_core::storage_driver_match::inf_tree_contains_hardware_id(
                            &driver_store,
                            hardware_id,
                        )? {
                            anyhow::bail!(
                                "offline DriverStore did not retain {} for {}",
                                hardware_id,
                                verified.directory().display()
                            );
                        }
                    }
                    log::info!(
                        "[ADVANCED] hardware-matched storage driver staged: {}",
                        verified.directory().display()
                    );
                }
                Ok(())
            })();

            let reload_result = (|| -> anyhow::Result<()> {
                OfflineRegistry::load_hive("pc-soft", &software_hive)?;
                hive_cleanup.0.push("pc-soft");
                OfflineRegistry::load_hive("pc-sys", &system_hive)?;
                hive_cleanup.0.push("pc-sys");
                if default_loaded {
                    OfflineRegistry::load_hive("pc-default", &default_hive)?;
                    hive_cleanup.0.push("pc-default");
                }
                Ok(())
            })();
            stage_result?;
            reload_result?;
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
        let drivers_root = path::get_exe_dir().join("drivers");
        let payload = lr_core::win7_driver_package::verify_windows7_driver_payload(&drivers_root)?;
        let architecture = target_win7_architecture(target_partition)?;
        let hardware_ids = lr_core::driver::list_present_hardware_ids()
            .context("枚举当前 USB 控制器硬件 ID 失败")?;
        let packages = payload.select_usb3_packages(&hardware_ids, architecture)?;
        with_offline_hives_unloaded(
            default_loaded,
            &software_hive,
            &system_hive,
            &default_hive,
            || {
                let dism = Dism::new();
                let image_path = format!("{}\\", target_partition);
                if packages.is_empty() {
                    log::warn!("[ADVANCED] 当前硬件没有匹配的内置 Win7 USB3 驱动包，安全跳过");
                }
                for package in &packages {
                    log::info!(
                        "[ADVANCED] Win7: 注入匹配的 USB3 驱动包: {}",
                        package.display()
                    );
                    dism.add_drivers_offline(&image_path, &package.to_string_lossy())?;
                }
                log::info!("[ADVANCED] Win7 USB3驱动注入成功");
                Ok(())
            },
        )?;
    }

    // 13. Win7 注入 NVMe 驱动
    if config.win7_inject_nvme_driver {
        log::info!("[ADVANCED] Win7: 开始注入NVMe驱动");
        let drivers_root = path::get_exe_dir().join("drivers");
        with_offline_hives_unloaded(
            default_loaded,
            &software_hive,
            &system_hive,
            &default_hive,
            || install_win7_nvme_drivers(&drivers_root, target_partition),
        )?;
        log::info!("[ADVANCED] Win7 NVMe驱动注入成功");
    }

    // 14. Win7 旧式处理器电源驱动兼容尝试。它不修补 ACPI 表，只在目标确认为
    // Windows 7 时按用户选择禁用历史处理器电源服务。
    if config.win7_fix_acpi_bsod && detect_user_driver_version(target_partition) == Some("win7") {
        log::info!("[ADVANCED] Win7: 尝试禁用旧式处理器电源驱动以提高启动兼容性");

        // 禁用 intelppm 服务 (Intel 电源管理)
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\intelppm",
            "Start",
            4, // 4 = Disabled
        );

        // 禁用 amdppm 服务 (AMD 电源管理)
        let _ =
            OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet001\\Services\\amdppm", "Start", 4);

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
        let _ =
            OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet002\\Services\\amdppm", "Start", 4);
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\Processor",
            "Start",
            4,
        );

        log::info!("[ADVANCED] Win7 旧式处理器电源驱动兼容设置完成");
    } else if config.win7_fix_acpi_bsod {
        log::warn!("[ADVANCED] 已选择旧式处理器电源驱动兼容尝试，但目标不是 Windows 7，安全跳过");
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
        let _ =
            OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet001\\Services\\iaStor", "Start", 0);

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
        let _ =
            OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet001\\Services\\pciide", "Start", 0);

        // intelide - Intel IDE 控制器
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\intelide",
            "Start",
            0,
        );

        // atapi - ATAPI 驱动
        let _ =
            OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet001\\Services\\atapi", "Start", 0);

        // ========== 同时设置 ControlSet002 ==========
        // msahci
        let _ =
            OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet002\\Services\\msahci", "Start", 0);
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
        let _ =
            OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet002\\Services\\iaStor", "Start", 0);
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
        let _ =
            OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet002\\Services\\pciide", "Start", 0);
        // intelide
        let _ = OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\intelide",
            "Start",
            0,
        );
        // atapi
        let _ =
            OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet002\\Services\\atapi", "Start", 0);

        log::info!("[ADVANCED] Win7 存储控制器蓝屏修复设置完成");
    }

    // ============ Windows XP 专用：离线注入存储/USB3 驱动 ============
    // XP(NT 5.x) 不能用 DISM 离线注入；这里走「拷贝 .sys/.inf + 在已加载的 SYSTEM
    // 配置单元(pc-sys)登记 boot-start 服务 + 写 CriticalDeviceDatabase」。
    // AHCI 始终注入；NVMe/USB3 按勾选。因为直接写已加载的 pc-sys（Win32 注册表 API），
    // 不需要 Win7 那套 DISM 前的卸载/重载。
    // XP 判定：配置标记 或 释放后系统缺少 \Windows\Boot（仅 Vista+ 才有），与引导步骤一致，
    // 使 CLI 安装（config.is_xp 可能为 false）也能触发注入。
    let xp_target = config.is_xp
        || !std::path::Path::new(&format!("{}\\Windows\\Boot", target_partition)).exists();
    if xp_target {
        // 驱动根目录：优先 exe\drivers\xp（与 Win7 PE 注入同源），回退 bin\drivers\xp
        let mut xp_dir = path::get_exe_dir().join("drivers").join("xp");
        if !xp_dir.is_dir() {
            let alt = path::get_bin_dir().join("drivers").join("xp");
            if alt.is_dir() {
                xp_dir = alt;
            }
        }
        if xp_dir.is_dir() {
            log::info!(
                "[ADVANCED] XP: 离线注入驱动 (AHCI 始终, NVMe={}, USB3={}) 源: {}",
                config.xp_inject_nvme_driver,
                config.xp_inject_usb3_driver,
                xp_dir.display()
            );
            let output = lr_core::xp::inject_xp_drivers(
                target_partition,
                &xp_dir,
                "pc-sys",
                config.xp_inject_nvme_driver,
                config.xp_inject_usb3_driver,
            )
            .map_err(anyhow::Error::msg)?;
            log::info!("[ADVANCED] XP 驱动注入完成:\n{}", output);
        } else {
            anyhow::bail!(
                "requested XP driver directory is missing: {}",
                xp_dir.display()
            );
        }
    }

    // 卸载注册表（确保正确卸载）
    log::info!("[ADVANCED] 卸载离线注册表...");
    std::thread::sleep(std::time::Duration::from_millis(500));
    hive_cleanup.unload_all()?;

    log::info!("[ADVANCED] 高级选项应用完成");
    Ok(())
}

fn target_win7_architecture(
    target_partition: &str,
) -> anyhow::Result<lr_core::win7_driver_package::Windows7TargetArchitecture> {
    match crate::core::system_utils::get_offline_system_architecture(Path::new(target_partition)) {
        crate::core::system_utils::SystemArchitecture::X86 => {
            Ok(lr_core::win7_driver_package::Windows7TargetArchitecture::X86)
        }
        crate::core::system_utils::SystemArchitecture::X64 => {
            Ok(lr_core::win7_driver_package::Windows7TargetArchitecture::Amd64)
        }
        architecture => {
            anyhow::bail!("无法确认 Windows 7 目标系统架构，拒绝注入驱动: {architecture:?}")
        }
    }
}

/// Installs the locked Microsoft NVMe hotfix pair in dependency order.
fn install_win7_nvme_drivers(drivers_root: &Path, target_partition: &str) -> anyhow::Result<()> {
    let payload = lr_core::win7_driver_package::verify_windows7_driver_payload(drivers_root)?;
    let architecture = target_win7_architecture(target_partition)?;
    let cabs = payload.nvme_cabs(architecture)?;
    let dism = Dism::new();
    let image_path = format!("{}\\", target_partition);
    for cab in cabs {
        log::info!("[NVME] 按依赖顺序安装锁定的微软更新: {}", cab.display());
        dism.add_package_offline(&image_path, &cab.to_string_lossy())?;
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
pub fn apply_uefiseven_patch(data_partition: &str, target_partition: &str) -> anyhow::Result<()> {
    use crate::core::bcdedit::BootManager;
    use std::path::Path;

    log::info!("[UEFISEVEN] 开始应用 UefiSeven 补丁");

    // 从数据分区查找 UefiSeven 文件
    let data_dir = crate::core::config::ConfigFileManager::get_data_dir(data_partition);
    let uefiseven_dir = format!("{}\\uefiseven", data_dir);
    let uefiseven_efi = format!("{}\\bootx64.efi", uefiseven_dir);
    let uefiseven_ini = format!("{}\\UefiSeven.ini", uefiseven_dir);

    if !Path::new(&uefiseven_efi).exists() {
        log::warn!(
            "[UEFISEVEN] UefiSeven bootx64.efi 不存在: {}",
            uefiseven_efi
        );
        return Err(anyhow::anyhow!(
            "UefiSeven bootx64.efi 不存在: {}",
            uefiseven_efi
        ));
    }

    log::info!("[UEFISEVEN] 找到 UefiSeven 文件: {}", uefiseven_efi);

    // 只挂载目标 Windows 所在磁盘的 ESP，不能改写其它硬盘的引导。
    let boot_manager = BootManager::new();
    let esp_mount = boot_manager
        .find_esp_on_same_disk(target_partition)
        .map_err(|e| anyhow::anyhow!("查找目标磁盘 EFI 分区失败: {}", e))?;
    let esp_letter = esp_mount.letter();

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
