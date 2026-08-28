use crate::core::config::InstallConfig;
use crate::core::dism::Dism;
use crate::core::registry::OfflineRegistry;
use crate::utils::path;
use anyhow::Context;
use std::path::{Path, PathBuf};

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

/// 注入 typed task 已认证的 `user_drivers/<版本>` INF。调用方传入的是 LRHM3 中的
/// exact file set；这里不得重新递归扫描公开数据目录，否则同目录迟到文件会绕过清单。
pub fn inject_user_drivers_from_authenticated_paths(
    target_partition: &str,
    authenticated_paths: &[std::path::PathBuf],
) -> anyhow::Result<()> {
    let version = match detect_user_driver_version(target_partition) {
        Some(v) => v,
        None => return Ok(()),
    };
    let inf_files = authenticated_paths
        .iter()
        .filter(|path| user_driver_path_matches_version(path, version))
        .filter(|path| {
            path.extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("inf"))
        })
        .cloned()
        .collect::<Vec<_>>();
    if inf_files.is_empty() {
        log::info!("[USER DRV] authenticated user_drivers/{version} has no INF; skipping");
        return Ok(());
    }
    log::info!(
        "[USER DRV] 注入 authenticated user_drivers/{} 到 {} ...",
        version,
        target_partition
    );
    let dism = Dism::new();
    let image_path = format!("{}\\", target_partition);
    let import_result =
        dism.add_preserved_driver_inf_files_offline_with_progress(&image_path, &inf_files, None)?;
    let summary = lr_core::bounded_failure_summary::summarize_failures(&import_result.failures, 3);
    if summary.is_empty() {
        log::info!("[USER DRV] user_drivers/{} 注入成功", version);
    } else {
        log::warn!("[USER DRV] 部分非启动存储用户驱动未导入，安装继续: {summary}");
    }
    Ok(())
}

fn user_driver_path_matches_version(path: &Path, version: &str) -> bool {
    let components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    components.windows(2).any(|pair| {
        pair[0].eq_ignore_ascii_case("user_drivers") && pair[1].eq_ignore_ascii_case(version)
    })
}

/// NT5 is an authenticated property of the selected source, never an inference from the applied
/// directory shape. Stripped Vista+ images and some GHO captures legitimately omit
/// `Windows\Boot`; treating that absence as XP would run the incompatible NT5 driver injector.
const fn authenticated_nt5_target(is_xp: bool, is_xp_i386: bool) -> bool {
    is_xp || is_xp_i386
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

fn disable_win7_processor_power_services(hive_name: &str) -> anyhow::Result<Vec<String>> {
    let mut updated = Vec::new();
    for control_set in 1..=4 {
        for service in ["intelppm", "amdppm", "Processor"] {
            let key = format!(
                "HKLM\\{}\\ControlSet{:03}\\Services\\{}",
                hive_name, control_set, service
            );
            if !OfflineRegistry::key_exists(&key)? {
                continue;
            }
            OfflineRegistry::set_dword(&key, "Start", 4)?;
            if OfflineRegistry::query_dword(&key, "Start")? != 4 {
                anyhow::bail!("processor power-service readback mismatch for {key}");
            }
            updated.push(format!("ControlSet{control_set:03}/{service}"));
        }
    }
    if updated.is_empty() {
        anyhow::bail!("no supported processor power service exists in the loaded SYSTEM hive");
    }
    Ok(updated)
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

    if detect_user_driver_version(target_partition) == Some("win11") {
        match lr_core::windows11_shell::apply_offline_defaults("pc-soft") {
            Ok(report) => log::info!(
                "[ADVANCED_WIN11_SHELL] status=completed force_effect_mode={}",
                report.force_effect_mode
            ),
            Err(error) => log::warn!(
                "[ADVANCED_WIN11_SHELL] status=warning detail={error:#}; installation continues"
            ),
        }
        if config.remove_uwp_apps {
            log::warn!(
                "[ADVANCED_WIN11_START] status=not_supported detail={}; AppX package removal continues independently",
                lr_core::windows11_shell::START_PIN_CLEANUP_UNSUPPORTED_REASON
            );
        }
    }

    if detect_user_driver_version(target_partition) == Some("win7") {
        let control_sets = OfflineRegistry::disable_crash_auto_reboot_for_loaded_system("pc-sys")?;
        log::info!(
            "[WIN7 DIAGNOSTIC] 已关闭首次启动崩溃自动重启并回读验证，control_sets={:?}",
            control_sets
        );
    }

    // ============ 系统优化选项 ============

    // 1. 移除快捷方式小箭头
    if config.remove_shortcut_arrow {
        log::info!("[ADVANCED] 移除快捷方式小箭头");
        OfflineRegistry::set_string(
            "HKLM\\pc-soft\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Shell Icons",
            "29",
            "%systemroot%\\system32\\imageres.dll,197",
        )?;
    }

    // 2. Win11恢复经典右键菜单
    if config.restore_classic_context_menu {
        log::info!("[ADVANCED] 恢复经典右键菜单");
        // 在 DEFAULT hive 中设置（影响所有新用户）
        if default_loaded {
            // 创建空的 InprocServer32 键，这会禁用新式右键菜单
            OfflineRegistry::create_key(
                "HKLM\\pc-default\\Software\\Classes\\CLSID\\{86ca1aa0-34aa-4e8b-a509-50c905bae2a2}\\InprocServer32"
            )?;
            // 设置默认值为空字符串
            OfflineRegistry::set_string(
                "HKLM\\pc-default\\Software\\Classes\\CLSID\\{86ca1aa0-34aa-4e8b-a509-50c905bae2a2}\\InprocServer32",
                "",
                "",
            )?;
        } else {
            anyhow::bail!("the default-user registry hive is unavailable");
        }
        // 同时在 SOFTWARE 中设置（系统级）
        OfflineRegistry::create_key(
            "HKLM\\pc-soft\\Classes\\CLSID\\{86ca1aa0-34aa-4e8b-a509-50c905bae2a2}\\InprocServer32",
        )?;
        OfflineRegistry::set_string(
            "HKLM\\pc-soft\\Classes\\CLSID\\{86ca1aa0-34aa-4e8b-a509-50c905bae2a2}\\InprocServer32",
            "",
            "",
        )?;
    }

    // 3. OOBE绕过强制联网
    if config.bypass_nro {
        log::info!("[ADVANCED] 设置OOBE绕过联网");
        OfflineRegistry::set_dword(
            "HKLM\\pc-soft\\Microsoft\\Windows\\CurrentVersion\\OOBE",
            "BypassNRO",
            1,
        )?;
    }

    // 4. 按目标系统家族移除 Windows Update 活动组件。
    if config.disable_windows_update {
        log::info!("[ADVANCED_UPDATE] action=remove_active_components status=started");
        match lr_core::offline_windows_update_removal::remove_offline_windows_update(
            target_partition,
            "pc-soft",
            "pc-sys",
        ) {
            Ok(report) => {
                let status = if report.warnings.is_empty() {
                    "completed"
                } else {
                    "warning"
                };
                log::info!(
                    "[ADVANCED_UPDATE] action=remove_active_components status={} profile={} build={} removed_paths={} removed_services={} removed_task_trees={} removed_task_records={} removed_registry_keys={} deleted_ubpm_values={} settings_visibility={:?} warning_count={}",
                    status,
                    report.profile,
                    report.target_build,
                    report.removed_paths,
                    report.removed_services,
                    report.removed_task_trees,
                    report.removed_task_records,
                    report.removed_registry_keys,
                    report.deleted_ubpm_values,
                    report.settings_page_visibility,
                    report.warnings.len()
                );
                for warning in &report.warnings {
                    log::warn!("[ADVANCED_UPDATE] detail={warning}");
                }
            }
            Err(error) => log::warn!(
                "[ADVANCED_UPDATE] action=remove_active_components status=warning detail={error:#}; installation continues"
            ),
        }
    }

    // 5. Windows Security UI is distinct from the preserved Security Health/Firewall services.
    // Remove the Defender Antivirus engine and exactly target the Windows Security UI AppX;
    // preserve SecurityHealthService, wscsvc, mpssvc, and firewall services.
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
            Err(error) => log::warn!(
                "[ADVANCED_DEFENDER] status=warning detail={error:#}; optional Defender removal was not completed; installation continues"
            ),
        }
    }

    // 6. 系统保留空间只允许通过 Win10 2004+ 的在线 DISM 接口修改。
    // 内置 unattend 会在 specialize/SYSTEM 阶段执行并回读；这里不再写 ReserveManager
    // 的内部离线注册表值。
    if config.disable_reserved_storage {
        log::info!(
            "[ADVANCED_RESERVED_STORAGE] phase=offline status=deferred reason=online_dism_only"
        );
    }

    // 7. 禁用UAC
    if config.disable_uac {
        log::info!("[ADVANCED] 禁用UAC");
        OfflineRegistry::set_dword(
            "HKLM\\pc-soft\\Microsoft\\Windows\\CurrentVersion\\Policies\\System",
            "EnableLUA",
            0,
        )?;
        OfflineRegistry::set_dword(
            "HKLM\\pc-soft\\Microsoft\\Windows\\CurrentVersion\\Policies\\System",
            "ConsentPromptBehaviorAdmin",
            0,
        )?;
    }

    // 8. 禁用自动设备加密 (BitLocker)
    if config.disable_device_encryption {
        log::info!("[ADVANCED] 禁用自动设备加密");
        // 禁用 BitLocker 自动加密
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Control\\BitLocker",
            "PreventDeviceEncryption",
            1,
        )?;
        // 禁用 MBAM (Microsoft BitLocker Administration and Monitoring)
        OfflineRegistry::set_dword("HKLM\\pc-soft\\Policies\\Microsoft\\FVE", "OSRecovery", 0)?;
        // 禁用 BitLocker 服务
        OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet001\\Services\\BDESVC", "Start", 4)?;
    }

    // 9. Curated AppX servicing is deferred until every externally loaded offline hive has
    // been unloaded. DISM must not service an image while LetRecovery still owns hive handles.

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
        let updated = disable_win7_processor_power_services("pc-sys")?;
        log::info!(
            "[ADVANCED] Win7 旧式处理器电源驱动兼容设置完成: {:?}",
            updated
        );
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
    // XP/2003 只能来自正常端已经验证并写入受认证配置的源类型。不得再因精简镜像或 GHO
    // 缺少 `Windows\Boot` 猜测 NT5；引导阶段与这里必须消费同一份配置事实。
    let xp_target = authenticated_nt5_target(config.is_xp, config.is_xp_i386);
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

    // Curated provisioned-AppX servicing, like SecHealthUI servicing below, must run only
    // after the offline hive unload has completed successfully. The required online hook later
    // performs exact all-user removal and fresh final verification; it is the authoritative gate.
    if config.remove_uwp_apps {
        apply_curated_appx_cleanup(target_partition);
    }

    // SecHealthUI servicing must run without mounted offline hives. The required online hook later
    // fails setup unless exact provisioning and all-user registration both read back as absent.
    if config.disable_windows_defender {
        apply_remove_sec_health_ui(target_partition);
    }

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
    log::info!(
        "[NVME] 在同一 servicing 会话中按依赖顺序安装 {} 个锁定的微软更新",
        cabs.len()
    );
    dism.add_packages_offline_ordered(&image_path, cabs)?;
    verify_win7_nvme_offline_versions(target_partition)
}

fn verify_win7_nvme_offline_versions(target_partition: &str) -> anyhow::Result<()> {
    use lr_core::windows_file_version::query_file_version;

    let target_root = PathBuf::from(format!(
        "{}\\",
        target_partition.trim_end_matches(['\\', '/'])
    ));
    let drivers = target_root.join("Windows").join("System32").join("drivers");
    let expected = [("stornvme.sys", 18_615_u16), ("storport.sys", 18_969_u16)];
    for (file_name, minimum_revision) in expected {
        let path = drivers.join(file_name);
        let version = query_file_version(&path).with_context(|| {
            format!("回读 Windows 7 NVMe 热修补文件版本失败: {}", path.display())
        })?;
        if !is_supported_win7_nvme_file_version(version, minimum_revision) {
            anyhow::bail!(
                "Windows 7 NVMe 热修补文件版本不满足要求: {} = {}, 要求 6.1.7601.{} 或更高",
                path.display(),
                version,
                minimum_revision
            );
        }
        log::info!("[NVME] 已回读验证 {} = {}", path.display(), version);
    }
    Ok(())
}

fn is_supported_win7_nvme_file_version(
    version: lr_core::windows_file_version::FileVersion,
    minimum_revision: u16,
) -> bool {
    version.major == 6
        && version.minor == 1
        && version.build == 7601
        && version.revision >= minimum_revision
}

fn apply_curated_appx_cleanup(target_partition: &str) {
    log::warn!(
        "[ADVANCED_APPX] action=remove_curated_preinstalled_appx status=scope_warning provisioning=covered existing_registered_users=not_covered gho_or_existing_profile_full_removal=not_claimed future_reprovisioning=not_blocked"
    );
    log::info!(
        "[ADVANCED_APPX] action=remove_curated_preinstalled_appx status=started target={:?}",
        target_partition
    );
    match lr_core::offline_appx::remove_curated_preinstalled_appx(target_partition) {
        Ok(report) => {
            for item in &report.items {
                match item.status {
                    lr_core::offline_appx::CuratedAppxStatus::Warning => log::warn!(
                        "[ADVANCED_APPX] action=remove_curated_preinstalled_appx item={} package={:?} status={} reason={:?}",
                        item.id,
                        item.package_full_name,
                        item.status.as_str(),
                        item.reason
                    ),
                    _ => log::info!(
                        "[ADVANCED_APPX] action=remove_curated_preinstalled_appx item={} package={:?} status={} reason={:?}",
                        item.id,
                        item.package_full_name,
                        item.status.as_str(),
                        item.reason
                    ),
                }
            }
            if report.warnings == 0 {
                log::info!(
                    "[ADVANCED_APPX] action=remove_curated_preinstalled_appx status=completed removed={} not_present={} warnings=0",
                    report.removed,
                    report.not_present
                );
            } else {
                log::warn!(
                    "[ADVANCED_APPX] action=remove_curated_preinstalled_appx status=completed_with_warnings removed={} not_present={} warnings={}",
                    report.removed,
                    report.not_present,
                    report.warnings
                );
            }
        }
        Err(error) => log::warn!(
            "[ADVANCED_APPX] action=remove_curated_preinstalled_appx status=warning reason={:?}",
            error.to_string()
        ),
    }
}

fn apply_remove_sec_health_ui(target_partition: &str) {
    match lr_core::sec_health_ui::remove_offline_provisioning(target_partition) {
        Ok(report) => {
            for item in &report.items {
                let message = format!(
                    "[ADVANCED_SEC_HEALTH_UI] phase=offline_provisioning item={} package={:?} status={} reason={:?}",
                    item.id,
                    item.package_full_name,
                    item.status.as_str(),
                    item.reason
                );
                if item.status == lr_core::offline_appx::CuratedAppxStatus::Warning {
                    log::warn!("{}", message);
                } else {
                    log::info!("{}", message);
                }
            }
            if report.warnings == 0 {
                log::info!(
                    "[ADVANCED_SEC_HEALTH_UI] phase=offline_provisioning status=completed removed={} not_present={} warnings=0",
                    report.removed,
                    report.not_present
                );
            } else {
                log::warn!(
                    "[ADVANCED_SEC_HEALTH_UI] phase=offline_provisioning status=completed_with_warnings removed={} not_present={} warnings={}",
                    report.removed,
                    report.not_present,
                    report.warnings
                );
            }
        }
        Err(error) => log::warn!(
            "[ADVANCED_SEC_HEALTH_UI] phase=offline_provisioning status=warning reason={:?}",
            error.to_string()
        ),
    }
}

/// 应用 UefiSeven 补丁到目标系统（PE环境版本）
///
/// 此方法应在引导修复之后调用。
/// UefiSeven 是一个 EFI 加载器，用于模拟 Int10h 中断，使 Windows 7 能够在 UEFI Class 3 系统上启动。
///
/// 参考: https://github.com/manatails/uefiseven
pub fn apply_uefiseven_patch(uefiseven_dir: &Path, target_partition: &str) -> anyhow::Result<()> {
    use crate::core::bcdedit::BootManager;
    log::info!("[UEFISEVEN] 开始应用 UefiSeven 补丁");

    lr_core::boot_pca::verify_uefiseven_package(uefiseven_dir)
        .map_err(|error| anyhow::anyhow!("UefiSeven 固定资源校验失败: {error}"))?;

    // 只挂载目标 Windows 所在磁盘的 ESP，不能改写其它硬盘的引导。
    let boot_manager = BootManager::new();
    let esp_mount = boot_manager
        .find_esp_on_same_disk(target_partition)
        .map_err(|e| anyhow::anyhow!("查找目标磁盘 EFI 分区失败: {}", e))?;
    let esp_letter = esp_mount.letter();

    log::info!("[UEFISEVEN] EFI 分区: {}", esp_letter);
    let operation = (|| -> anyhow::Result<()> {
        // Microsoft Boot 目录
        let ms_boot_dir = PathBuf::from(format!("{}\\EFI\\Microsoft\\Boot", esp_letter));
        let machine = lr_core::windows_hardware::collect_machine_identity();
        log::info!("[UEFISEVEN] machine environment: {:?}", machine.environment);
        for diagnostic in &machine.diagnostics {
            log::debug!("[UEFISEVEN] hardware probe: {diagnostic}");
        }
        if !lr_core::windows_hardware::should_install_uefiseven(machine.environment) {
            lr_core::boot_pca::restore_native_windows7_uefi_entries(&ms_boot_dir).map_err(
                |error| anyhow::anyhow!("恢复 VMware 原生 Windows EFI 引导失败: {error}"),
            )?;
            log::info!("[UEFISEVEN] confirmed VMware guest; native Microsoft EFI entries restored");
            return Ok(());
        }

        lr_core::boot_pca::install_uefiseven_package(uefiseven_dir, &ms_boot_dir)
            .map_err(|error| anyhow::anyhow!("部署 UefiSeven 失败: {error}"))?;

        log::info!("[UEFISEVEN] UefiSeven 补丁应用成功");
        log::info!("[UEFISEVEN] 启动流程: UEFI -> UefiSeven -> bootmgfw.original.efi -> Windows 7");

        Ok(())
    })();
    crate::core::bcdedit::finish_with_esp_cleanup(operation, Some(esp_mount))
}

#[cfg(test)]
mod tests {
    use super::{
        authenticated_nt5_target, is_supported_win7_nvme_file_version,
        user_driver_path_matches_version,
    };
    use lr_core::windows_file_version::FileVersion;

    #[test]
    fn nvme_hotfix_version_gate_accepts_gdr_ldr_and_newer_revisions() {
        for revision in [18_615, 22_823, 24_001] {
            assert!(is_supported_win7_nvme_file_version(
                FileVersion {
                    major: 6,
                    minor: 1,
                    build: 7601,
                    revision,
                },
                18_615,
            ));
        }
    }

    #[test]
    fn nvme_hotfix_version_gate_rejects_wrong_os_and_old_payloads() {
        assert!(!is_supported_win7_nvme_file_version(
            FileVersion {
                major: 6,
                minor: 1,
                build: 7601,
                revision: 18_614,
            },
            18_615,
        ));
        assert!(!is_supported_win7_nvme_file_version(
            FileVersion {
                major: 10,
                minor: 0,
                build: 7601,
                revision: 24_001,
            },
            18_615,
        ));
    }

    #[test]
    fn user_driver_filter_uses_only_the_authenticated_version_subtree() {
        assert!(user_driver_path_matches_version(
            std::path::Path::new(r"R:\LetRecovery_Data\user_drivers\win11\net.inf"),
            "win11"
        ));
        assert!(!user_driver_path_matches_version(
            std::path::Path::new(r"R:\LetRecovery_Data\user_drivers\win10\net.inf"),
            "win11"
        ));
        assert!(!user_driver_path_matches_version(
            std::path::Path::new(r"R:\other\win11\net.inf"),
            "win11"
        ));
    }

    #[test]
    fn nt5_driver_injection_uses_only_authenticated_source_flags() {
        assert!(authenticated_nt5_target(true, false));
        assert!(authenticated_nt5_target(false, true));
        assert!(!authenticated_nt5_target(false, false));
    }
}
