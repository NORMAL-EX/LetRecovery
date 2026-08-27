//! Advanced installation options and their offline application boundary.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::core::registry::OfflineRegistry;
use lr_core::unattend_account::BuiltInAdministratorOptions;
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

/// 系统安装高级选项
///
/// 容器级 `#[serde(default)]`：命令行安装允许只写需要的字段，缺省项自动取默认值。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AdvancedOptions {
    // 系统优化选项
    /// Current-install-only same-volume preservation. This is intentionally not persisted because
    /// it changes the reinstall's destructive boundary and must be selected again for each task.
    #[serde(skip)]
    pub preserve_personal_files: bool,
    pub remove_shortcut_arrow: bool,
    pub restore_classic_context_menu: bool,
    pub bypass_nro: bool,
    pub disable_windows_update: bool,
    /// Remove the Defender Antivirus engine and exactly target the Windows Security UI AppX;
    /// SecurityHealthService, wscsvc, mpssvc, and firewall services remain preserved.
    pub disable_windows_defender: bool,
    pub disable_reserved_storage: bool,
    pub disable_uac: bool,
    pub disable_device_encryption: bool,
    /// Remove the shared exact-PFN curated provisioned AppX list. Outlook and both AppX/Win32
    /// OneDrive are intentionally preserved. On Windows 11 the same option also configures the
    /// default user before first sign-in so Start recommendations and preinstalled content
    /// delivery do not recreate promotional entries.
    pub remove_uwp_apps: bool,
    /// Current-install-only v4 catalogue selections. The server response is revalidated before
    /// download and the value is carried in the authenticated PE handoff; it is never persisted
    /// as a long-lived preference.
    #[serde(skip)]
    pub preinstalled_software: Vec<lr_core::software_install::SelectedSoftwarePackage>,
    /// UI preference for the separately presented VMware Tools entry. The native window resolves
    /// it to an ordinary selected package only after positive VMware and live-catalogue checks.
    #[serde(skip)]
    pub install_vmware_tools: bool,
    /// 迁移当前 WiFi（重装后自动连接）：勾选时即抓取当前连接的 WiFi 配置
    pub migrate_wifi: bool,
    /// 抓取到的 WiFi 配置 XML（含明文密钥，故不持久化到 config.json）
    #[serde(skip)]
    pub wifi_profile_xml: String,
    /// 抓取到的 WiFi 名称（不持久化）
    #[serde(skip)]
    pub wifi_ssid: String,
    /// 当前系统是否检测到 WiFi（已连接无线网络）。None=尚未检测。
    /// 用于决定是否显示“迁移当前 WiFi”选项；无 WiFi（虚拟机/无无线网卡/未连接）时隐藏。
    #[serde(skip)]
    pub wifi_detected: Option<bool>,

    // 自定义脚本
    pub run_script_during_deploy: bool,
    pub deploy_script_path: String,
    pub run_script_first_login: bool,
    pub first_login_script_path: String,

    // 自定义内容
    pub import_custom_drivers: bool,
    pub custom_drivers_path: String,
    pub import_storage_controller_drivers: bool,
    pub import_registry_file: bool,
    pub registry_file_path: String,
    pub import_custom_files: bool,
    pub custom_files_path: String,

    // 用户设置
    pub custom_username: bool,
    pub username: String,
    /// 使用内置 RID-500 Administrator，而不是新建普通本地管理员。
    pub builtin_administrator: BuiltInAdministratorOptions,

    // 系统盘设置
    pub custom_volume_label: bool,
    pub volume_label: String,

    // Win7 专用选项
    pub win7_inject_usb3_driver: bool,
    pub win7_usb3_driver_path: String,
    pub win7_inject_nvme_driver: bool,
    pub win7_nvme_driver_path: String,
    pub win7_fix_acpi_bsod: bool,
    /// 修复0x7B蓝屏（INACCESSIBLE_BOOT_DEVICE）- 启用存储控制器驱动
    pub win7_fix_storage_bsod: bool,

    // Win7 UEFI 修补选项（仅在Win7 + UEFI模式下显示）
    pub win7_uefi_patch: bool,

    // XP 专用选项（仅检测到 XP/2003 镜像时显示）
    /// XP 注入 USB3(xHCI) 驱动（检测到 XP 时默认勾选）
    pub xp_inject_usb3_driver: bool,
    /// XP 注入 NVMe 驱动（检测到 XP 时默认勾选）
    pub xp_inject_nvme_driver: bool,
    /// XP 选项默认值是否已按「检测到 XP」初始化过（避免每帧重复覆盖用户的手动取消）
    #[serde(skip)]
    pub xp_defaults_applied: bool,
}

impl AdvancedOptions {
    /// 脚本目录名称（统一路径）
    const SCRIPTS_DIR: &'static str = "LetRecovery_Scripts";

    /// 获取程序运行目录（exe 所在目录）
    fn get_program_dir() -> Option<PathBuf> {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    }

    fn get_win7_drivers_root() -> Option<PathBuf> {
        Self::get_program_dir().map(|base| base.join("bin").join("drivers"))
    }

    fn target_win7_architecture(
        target_partition: &str,
    ) -> anyhow::Result<lr_core::win7_driver_package::Windows7TargetArchitecture> {
        match crate::core::system_utils::get_system_architecture(target_partition) {
            crate::core::system_utils::SystemArchitecture::X86 => {
                Ok(lr_core::win7_driver_package::Windows7TargetArchitecture::X86)
            }
            crate::core::system_utils::SystemArchitecture::Amd64 => {
                Ok(lr_core::win7_driver_package::Windows7TargetArchitecture::Amd64)
            }
            architecture => {
                anyhow::bail!("无法确认 Windows 7 目标系统架构，拒绝注入驱动: {architecture:?}")
            }
        }
    }

    fn target_is_windows_7(target_partition: &str) -> bool {
        let ntdll = Path::new(target_partition)
            .join("Windows")
            .join("System32")
            .join("ntdll.dll");
        matches!(
            crate::core::system_utils::get_file_version(&ntdll),
            Some((6, 1, _, _))
        )
    }

    fn target_is_windows_11(target_partition: &str) -> bool {
        let ntdll = Path::new(target_partition)
            .join("Windows")
            .join("System32")
            .join("ntdll.dll");
        matches!(
            crate::core::system_utils::get_file_version(&ntdll),
            Some((10, _, build, _)) if build >= 22_000
        )
    }

    /// 获取 XP 驱动目录（bin\drivers\xp\{usb3|nvme|ahci}）
    fn get_xp_driver_dirs() -> (Option<PathBuf>, Option<PathBuf>, Option<PathBuf>) {
        let base = Self::get_program_dir();
        let root = base
            .as_ref()
            .map(|b| b.join("bin").join("drivers").join("xp"));
        let usb3 = root.as_ref().map(|b| b.join("usb3"));
        let nvme = root.as_ref().map(|b| b.join("nvme"));
        let ahci = root.as_ref().map(|b| b.join("ahci"));
        (usb3, nvme, ahci)
    }

    /// 获取 UefiSeven 目录（bin\uefiseven）
    fn get_uefiseven_dir() -> Option<PathBuf> {
        Self::get_program_dir().map(|b| b.join("bin").join("uefiseven"))
    }

    /// 应用 UefiSeven 补丁到目标系统
    /// 此方法应在引导修复之后调用
    ///
    /// UefiSeven 是一个 EFI 加载器，用于模拟 Int10h 中断，使 Windows 7 能够在 UEFI Class 3 系统上启动。
    /// 它通过在 Windows 启动前安装一个最小的 Int10h 处理程序来工作。
    ///
    /// 参考: https://github.com/manatails/uefiseven
    pub fn apply_uefiseven_patch(&self, target_partition: &str) -> anyhow::Result<()> {
        if !self.win7_uefi_patch {
            log::info!("[UEFISEVEN] Win7 UEFI补丁未启用，跳过");
            return Ok(());
        }

        log::info!("[UEFISEVEN] 开始应用 UefiSeven 补丁");

        // 获取 UefiSeven 源文件目录
        let uefiseven_dir = match Self::get_uefiseven_dir() {
            Some(dir) if dir.exists() => dir,
            Some(dir) => {
                log::error!("[UEFISEVEN] UefiSeven 目录不存在: {}", dir.display());
                return Err(anyhow::anyhow!("UefiSeven 目录不存在: {}", dir.display()));
            }
            None => {
                log::error!("[UEFISEVEN] 无法获取程序运行目录");
                return Err(anyhow::anyhow!("无法获取程序运行目录"));
            }
        };

        lr_core::boot_pca::verify_uefiseven_package(&uefiseven_dir)
            .map_err(|error| anyhow::anyhow!("UefiSeven 固定资源校验失败: {error}"))?;

        // UefiSeven 必须跟随目标 Windows 所在磁盘，不能改写其它硬盘的 ESP。
        let boot_manager = crate::core::bcdedit::BootManager::new();
        let esp_mount = boot_manager
            .find_esp_on_same_disk(target_partition)
            .map_err(|error| anyhow::anyhow!("查找目标磁盘 EFI 分区失败: {}", error))?;
        let efi_mount_point = esp_mount.letter();
        log::info!("[UEFISEVEN] EFI 分区挂载点: {}", efi_mount_point);
        let operation = (|| -> anyhow::Result<()> {
            // Microsoft Boot 目录
            let ms_boot_dir =
                std::path::PathBuf::from(format!("{}\\EFI\\Microsoft\\Boot", efi_mount_point));
            let machine = lr_core::windows_hardware::collect_machine_identity();
            log::info!("[UEFISEVEN] machine environment: {:?}", machine.environment);
            for diagnostic in &machine.diagnostics {
                log::debug!("[UEFISEVEN] hardware probe: {diagnostic}");
            }
            if !lr_core::windows_hardware::should_install_uefiseven(machine.environment) {
                lr_core::boot_pca::restore_native_windows7_uefi_entries(&ms_boot_dir).map_err(
                    |error| anyhow::anyhow!("恢复 VMware 原生 Windows EFI 引导失败: {error}"),
                )?;
                log::info!(
                    "[UEFISEVEN] confirmed VMware guest; native Microsoft EFI entries restored"
                );
                return Ok(());
            }

            lr_core::boot_pca::install_uefiseven_package(&uefiseven_dir, &ms_boot_dir)
                .map_err(|error| anyhow::anyhow!("部署 UefiSeven 失败: {error}"))?;

            log::info!("[UEFISEVEN] UefiSeven 补丁应用成功");
            log::info!(
                "[UEFISEVEN] 启动流程: UEFI -> UefiSeven -> bootmgfw.original.efi -> Windows 7"
            );

            Ok(())
        })();
        crate::core::bcdedit::finish_with_esp_cleanup(operation, Some(esp_mount))
    }

    /// 当前系统是否检测到 WiFi（已连接到某个无线网络）。
    /// 虚拟机/无无线网卡/未连接 WiFi 时返回 false（用于隐藏“迁移 WiFi”选项）。
    #[cfg(windows)]
    fn system_has_wifi() -> bool {
        super::native_wifi::connected_wifi_available().unwrap_or(false)
    }

    #[cfg(not(windows))]
    fn system_has_wifi() -> bool {
        false
    }

    /// 应用选项到目标系统
    pub fn apply_to_system(&self, target_partition: &str, is_xp: bool) -> anyhow::Result<()> {
        log::info!(
            "[ADVANCED] 开始应用高级选项到: {} (is_xp={})",
            target_partition,
            is_xp
        );

        let windows_path = format!("{}\\Windows", target_partition);
        let software_hive = format!("{}\\System32\\config\\SOFTWARE", windows_path);
        let system_hive = format!("{}\\System32\\config\\SYSTEM", windows_path);
        let default_hive = format!("{}\\System32\\config\\DEFAULT", windows_path);

        // 加载离线注册表
        log::info!("[ADVANCED] 加载离线注册表...");
        OfflineRegistry::load_hive("pc-soft", &software_hive)?;
        let mut hive_cleanup = OfflineHiveCleanup(vec!["pc-soft"]);
        OfflineRegistry::load_hive("pc-sys", &system_hive)?;
        hive_cleanup.0.push("pc-sys");
        // DEFAULT 用于设置默认用户配置（如经典右键菜单）
        let default_loaded = OfflineRegistry::load_hive("pc-default", &default_hive).is_ok();
        if default_loaded {
            hive_cleanup.0.push("pc-default");
        }

        if Self::target_is_windows_11(target_partition) {
            match lr_core::windows11_shell::apply_offline_defaults("pc-soft") {
                Ok(report) => log::info!(
                    "[ADVANCED_WIN11_SHELL] status=completed force_effect_mode={}",
                    report.force_effect_mode
                ),
                Err(error) => log::warn!(
                    "[ADVANCED_WIN11_SHELL] status=warning detail={error:#}; installation continues"
                ),
            }
            if self.remove_uwp_apps {
                log::warn!(
                    "[ADVANCED_WIN11_START] status=not_supported detail={}; AppX package removal continues independently",
                    lr_core::windows11_shell::START_PIN_CLEANUP_UNSUPPORTED_REASON
                );
            }
        }

        if Self::target_is_windows_7(target_partition) {
            let control_sets =
                OfflineRegistry::disable_crash_auto_reboot_for_loaded_system("pc-sys")?;
            log::info!(
                "[WIN7 DIAGNOSTIC] 已关闭首次启动崩溃自动重启并回读验证，control_sets={:?}",
                control_sets
            );
        }

        // 创建脚本目录（用于存放自定义脚本）
        let scripts_dir = format!("{}\\{}", target_partition, Self::SCRIPTS_DIR);
        std::fs::create_dir_all(&scripts_dir)?;
        log::info!("[ADVANCED] 脚本目录: {}", scripts_dir);

        // ============ 系统优化选项 ============

        // 1. 移除快捷方式小箭头
        if self.remove_shortcut_arrow {
            self.apply_remove_shortcut_arrow()?;
        }

        // 2. Win11恢复经典右键菜单
        if self.restore_classic_context_menu {
            self.apply_restore_classic_context_menu(default_loaded)?;
        }

        // 3. OOBE绕过强制联网
        if self.bypass_nro {
            self.apply_bypass_nro()?;
        }

        // 4. 按目标系统家族移除 Windows Update 活动组件。
        if self.disable_windows_update {
            if let Err(error) = self.apply_disable_windows_update(target_partition) {
                log::warn!(
                    "[ADVANCED_UPDATE] action=remove_active_components status=warning detail={error:#}; installation continues"
                );
            }
        }

        // 5. 移除 Defender Antivirus 引擎并精确移除 Windows Security UI AppX；
        //    保留 SecurityHealthService、wscsvc、mpssvc 与防火墙服务。
        if self.disable_windows_defender {
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

        // 6. 系统保留空间只能由微软支持的在线 DISM 命令修改。
        // 内置 Win10 2004+ unattend 会在 specialize/SYSTEM 阶段执行并回读；此处不再写
        // ReserveManager 的内部离线注册表值。
        if self.disable_reserved_storage {
            log::info!(
                "[ADVANCED_RESERVED_STORAGE] phase=offline status=deferred reason=online_dism_only"
            );
        }

        // 7. 禁用UAC
        if self.disable_uac {
            self.apply_disable_uac()?;
        }

        // 8. 禁用自动设备加密 (BitLocker)
        if self.disable_device_encryption {
            self.apply_disable_device_encryption()?;
        }

        // 9. Curated AppX servicing is deferred until every externally loaded offline hive has
        // been unloaded. DISM must not service an image while LetRecovery still owns hive handles.
        // WiFi 迁移：只暂存已验证可迁移的 profile XML。内置无人值守文件会在首登时
        // 通过共享 finalizer 隐藏导入、检查退出码，并在所有收尾工作成功后删除脚本目录。
        if self.migrate_wifi && !self.wifi_profile_xml.is_empty() {
            self.apply_migrate_wifi(target_partition)?;
        }

        // ============ 自定义脚本 ============

        // 10. 系统部署中运行脚本
        if self.run_script_during_deploy && !self.deploy_script_path.is_empty() {
            self.apply_run_script_during_deploy(&scripts_dir)?;
        }

        // 11. 首次登录运行脚本
        if self.run_script_first_login && !self.first_login_script_path.is_empty() {
            self.apply_run_script_first_login(&scripts_dir)?;
        }

        // ============ 自定义内容 ============

        // 12. 导入自定义驱动 - 使用 DISM 实际安装
        if self.import_custom_drivers && !self.custom_drivers_path.is_empty() {
            self.apply_import_custom_drivers(
                target_partition,
                default_loaded,
                &software_hive,
                &system_hive,
                &default_hive,
            )?;
        }

        // 13. 导入磁盘控制器驱动（Win10/Win11 x64）
        if self.import_storage_controller_drivers {
            self.apply_import_storage_controller_drivers(
                target_partition,
                default_loaded,
                &software_hive,
                &system_hive,
                &default_hive,
            )?;
        }

        // 14. 导入注册表文件 - 实际导入到离线注册表
        if self.import_registry_file && !self.registry_file_path.is_empty() {
            self.apply_import_registry_file(&scripts_dir)?;
        }

        // 15. 导入自定义文件
        if self.import_custom_files && !self.custom_files_path.is_empty() {
            self.apply_import_custom_files(target_partition)?;
        }

        // 16. 自定义用户名 - 写入标记文件供无人值守使用
        if self.custom_username && !self.username.is_empty() {
            self.apply_custom_username(&scripts_dir)?;
        }

        // 17. 自定义系统盘卷标 - 写入标记文件供格式化时使用
        if self.custom_volume_label && !self.volume_label.is_empty() {
            self.apply_custom_volume_label(&scripts_dir)?;
        }

        // ============ Win7 专用选项 ============

        // 18. Win7 注入 USB3 驱动（固定读取程序运行目录下的 drivers\\usb3）
        // 支持 .cab 更新包文件和普通驱动文件夹
        if self.win7_inject_usb3_driver {
            self.apply_win7_inject_usb3_driver(
                target_partition,
                default_loaded,
                &software_hive,
                &system_hive,
                &default_hive,
            )?;
        }

        // 19. Win7 注入 NVMe 驱动（固定读取程序运行目录下的 drivers\\nvme）
        // 支持 .cab 更新包文件（如 KB2990941, KB3087873）和普通驱动文件夹
        if self.win7_inject_nvme_driver {
            self.apply_win7_inject_nvme_driver(
                target_partition,
                default_loaded,
                &software_hive,
                &system_hive,
                &default_hive,
            )?;
        }

        // 20. Win7 旧式处理器电源驱动兼容尝试。它不会修补 ACPI 表，只在用户明确
        // 选择时禁用历史上可能导致启动冲突的处理器电源服务。
        if self.win7_fix_acpi_bsod && Self::target_is_windows_7(target_partition) {
            self.apply_win7_fix_acpi_bsod()?;
        } else if self.win7_fix_acpi_bsod {
            log::warn!(
                "[ADVANCED] 已选择旧式处理器电源驱动兼容尝试，但目标不是 Windows 7，安全跳过"
            );
        }
        // 21. Win7 修复 INACCESSIBLE_BOOT_DEVICE (0x7B) 蓝屏
        // 这是Win7在现代硬件上最常见的蓝屏问题，原因是存储控制器驱动未启用
        // ============ Windows XP 专用：离线注入存储/USB3 驱动 ============
        // 直接写已加载的 SYSTEM 配置单元(pc-sys)，不走 DISM。AHCI 始终注入；NVMe/USB3 按勾选。
        if is_xp {
            self.apply_xp_inject_drivers(target_partition)?;
        }

        // 卸载注册表
        log::info!("[ADVANCED] 卸载离线注册表...");
        hive_cleanup.unload_all()?;

        // Curated provisioned-AppX servicing, like SecHealthUI servicing below, must run only
        // after the offline hive unload has completed successfully. It remains warning-only.
        if self.remove_uwp_apps {
            self.apply_remove_uwp_apps(target_partition);
        }

        // SecHealthUI uses supported AppX servicing only after every offline hive handle is
        // closed. Failure is an advanced-feature warning and must not block the installation.
        if self.disable_windows_defender {
            Self::apply_remove_sec_health_ui(target_partition);
        }

        log::info!("[ADVANCED] 高级选项应用完成");
        Ok(())
    }

    // ============ apply_to_system 各优化块的私有 helper（行为与内联版本逐字等价）============

    /// 1. 移除快捷方式小箭头
    fn apply_remove_shortcut_arrow(&self) -> anyhow::Result<()> {
        log::info!("[ADVANCED] 移除快捷方式小箭头");
        OfflineRegistry::set_string(
            "HKLM\\pc-soft\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Shell Icons",
            "29",
            "%systemroot%\\system32\\imageres.dll,197",
        )?;
        Ok(())
    }

    /// 2. Win11恢复经典右键菜单
    fn apply_restore_classic_context_menu(&self, default_loaded: bool) -> anyhow::Result<()> {
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
        Ok(())
    }

    /// 3. OOBE绕过强制联网
    fn apply_bypass_nro(&self) -> anyhow::Result<()> {
        log::info!("[ADVANCED] 设置OOBE绕过联网");
        OfflineRegistry::set_dword(
            "HKLM\\pc-soft\\Microsoft\\Windows\\CurrentVersion\\OOBE",
            "BypassNRO",
            1,
        )?;
        Ok(())
    }

    /// 4. 移除 Windows Update 活动组件并隐藏其设置入口。
    fn apply_disable_windows_update(&self, target_partition: &str) -> anyhow::Result<()> {
        log::info!("[ADVANCED_UPDATE] action=remove_active_components status=started");
        let report = lr_core::offline_windows_update_removal::remove_offline_windows_update(
            target_partition,
            "pc-soft",
            "pc-sys",
        )?;
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
        Ok(())
    }

    /// 7. 禁用UAC
    fn apply_disable_uac(&self) -> anyhow::Result<()> {
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
        Ok(())
    }

    /// 8. 禁用自动设备加密 (BitLocker)
    fn apply_disable_device_encryption(&self) -> anyhow::Result<()> {
        log::info!("[ADVANCED] 禁用自动设备加密");
        // 禁用 BitLocker 自动加密
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Control\\BitLocker",
            "PreventDeviceEncryption",
            1,
        )?;
        // 禁用 MBAM (Microsoft BitLocker Administration and Monitoring)
        OfflineRegistry::set_dword("HKLM\\pc-soft\\Policies\\Microsoft\\FVE", "OSRecovery", 0)?;
        // 禁用设备加密
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\BDESVC",
            "Start",
            4, // Disabled
        )?;
        Ok(())
    }

    /// Remove only the curated provisioned AppX identities from the offline image.
    fn apply_remove_uwp_apps(&self, target_partition: &str) {
        log::info!(
            "[ADVANCED_APPX] action=remove_curated_preinstalled_appx phase=offline_provisioning; exact all-user removal and final verification are required by the built-in unattend hook"
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

    /// WiFi 迁移：暂存 profile XML，交由共享首登 finalizer 隐藏导入并验证。
    fn apply_migrate_wifi(&self, target_partition: &str) -> anyhow::Result<()> {
        let scripts = format!("{}\\{}", target_partition, Self::SCRIPTS_DIR);
        std::fs::create_dir_all(&scripts)?;
        let xml_path = format!("{}\\LR_WiFi.xml", scripts);
        std::fs::write(&xml_path, self.wifi_profile_xml.as_bytes())?;
        if std::fs::read(&xml_path)? != self.wifi_profile_xml.as_bytes() {
            anyhow::bail!("staged Wi-Fi profile readback mismatch");
        }
        log::info!("[ADVANCED] 已暂存 WiFi 迁移配置: {}", self.wifi_ssid);
        Ok(())
    }

    /// 10. 系统部署中运行脚本
    fn apply_run_script_during_deploy(&self, scripts_dir: &str) -> anyhow::Result<()> {
        log::info!("[ADVANCED] 复制部署脚本: {}", self.deploy_script_path);
        let target_path = format!("{}\\deploy.bat", scripts_dir);
        std::fs::copy(&self.deploy_script_path, &target_path)?;
        log::info!("[ADVANCED] 部署脚本已复制到: {}", target_path);
        Ok(())
    }

    /// 11. 首次登录运行脚本
    fn apply_run_script_first_login(&self, scripts_dir: &str) -> anyhow::Result<()> {
        log::info!(
            "[ADVANCED] 复制首次登录脚本: {}",
            self.first_login_script_path
        );
        let target_path = format!("{}\\firstlogon.bat", scripts_dir);
        std::fs::copy(&self.first_login_script_path, &target_path)?;
        log::info!("[ADVANCED] 首次登录脚本已复制到: {}", target_path);
        Ok(())
    }

    /// 12. 导入自定义驱动 - 使用 DISM 实际安装
    fn apply_import_custom_drivers(
        &self,
        target_partition: &str,
        default_loaded: bool,
        software_hive: &str,
        system_hive: &str,
        default_hive: &str,
    ) -> anyhow::Result<()> {
        log::info!("[ADVANCED] 导入自定义驱动: {}", self.custom_drivers_path);

        // 先卸载注册表，因为 DISM 可能需要独占访问
        OfflineRegistry::unload_hive("pc-soft")?;
        OfflineRegistry::unload_hive("pc-sys")?;
        if default_loaded {
            OfflineRegistry::unload_hive("pc-default")?;
        }

        // 使用 DISM 添加驱动
        let dism = crate::core::dism::Dism::new();
        let image_path = format!("{}\\", target_partition);
        let import_result = dism.add_drivers_offline(&image_path, &self.custom_drivers_path);

        let reload_result = (|| -> anyhow::Result<()> {
            OfflineRegistry::load_hive("pc-soft", software_hive)?;
            OfflineRegistry::load_hive("pc-sys", system_hive)?;
            if default_loaded {
                OfflineRegistry::load_hive("pc-default", default_hive)?;
            }
            Ok(())
        })();
        import_result?;
        reload_result?;
        log::info!("[ADVANCED] 自定义驱动导入成功");
        Ok(())
    }

    /// 13. 导入磁盘控制器驱动（Win10/Win11 x64）
    fn apply_import_storage_controller_drivers(
        &self,
        target_partition: &str,
        default_loaded: bool,
        software_hive: &str,
        system_hive: &str,
        default_hive: &str,
    ) -> anyhow::Result<()> {
        let hardware_ids = lr_core::driver::list_present_hardware_ids().map_err(|error| {
            anyhow::anyhow!("storage-controller hardware enumeration failed: {error}")
        })?;
        let packages = lr_core::storage_driver_match::select_builtin_storage_driver_packages(
            hardware_ids.iter().map(String::as_str),
        )
        .map_err(anyhow::Error::new)?;
        if packages.is_empty() {
            log::info!(
                "[ADVANCED] no supported Intel VMD controller is present; built-in storage drivers were not staged"
            );
            return Ok(());
        }

        let storage_drivers_dir = crate::utils::path::get_drivers_dir().join("storage_controller");
        let verified_packages = packages
            .into_iter()
            .map(|package| {
                let directory = storage_drivers_dir.join(package.directory_name());
                lr_core::storage_driver_match::verify_builtin_storage_driver_package(
                    package, &directory,
                )
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        log::info!(
            "[ADVANCED] staging {} hardware-matched Intel VMD package(s)",
            verified_packages.len()
        );
        OfflineRegistry::unload_hive("pc-soft")?;
        OfflineRegistry::unload_hive("pc-sys")?;
        if default_loaded {
            OfflineRegistry::unload_hive("pc-default")?;
        }

        let dism = crate::core::dism::Dism::new();
        let image_path = format!("{}\\", target_partition);
        let driver_store = std::path::Path::new(target_partition)
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
            OfflineRegistry::load_hive("pc-soft", software_hive)?;
            OfflineRegistry::load_hive("pc-sys", system_hive)?;
            if default_loaded {
                OfflineRegistry::load_hive("pc-default", default_hive)?;
            }
            Ok(())
        })();
        stage_result?;
        reload_result?;
        Ok(())
    }

    /// 14. 导入注册表文件 - 实际导入到离线注册表
    fn apply_import_registry_file(&self, scripts_dir: &str) -> anyhow::Result<()> {
        log::info!("[ADVANCED] 导入注册表文件: {}", self.registry_file_path);

        // 读取原始 .reg 文件
        let reg_content = std::fs::read_to_string(&self.registry_file_path)?;
        // 转换路径：HKEY_LOCAL_MACHINE\SOFTWARE -> HKLM\pc-soft
        // 转换路径：HKEY_LOCAL_MACHINE\SYSTEM -> HKLM\pc-sys
        let converted = Self::convert_reg_file_for_offline(&reg_content);

        // `create_new` prevents concurrent installations from overwriting the
        // same import file; the guard cleans up on every return path.
        let temp_reg = lr_core::scoped_temp_file::ScopedTempFile::create_in(
            std::path::Path::new(scripts_dir),
            "lr-reg-import",
            "reg",
            converted.as_bytes(),
        )?;
        OfflineRegistry::import_reg_file(&temp_reg.to_string_lossy())?;
        log::info!("[ADVANCED] 注册表文件导入成功");
        Ok(())
    }

    /// 15. 导入自定义文件
    fn apply_import_custom_files(&self, target_partition: &str) -> anyhow::Result<()> {
        log::info!("[ADVANCED] 导入自定义文件: {}", self.custom_files_path);
        Self::copy_dir_all(&self.custom_files_path, target_partition)?;
        log::info!("[ADVANCED] 自定义文件导入成功");
        Ok(())
    }

    /// 16. 自定义用户名 - 写入标记文件供无人值守使用
    fn apply_custom_username(&self, scripts_dir: &str) -> anyhow::Result<()> {
        log::info!("[ADVANCED] 设置自定义用户名: {}", self.username);
        let username_file = format!("{}\\username.txt", scripts_dir);
        std::fs::write(&username_file, &self.username)?;
        Ok(())
    }

    /// 17. 自定义系统盘卷标 - 写入标记文件供格式化时使用
    fn apply_custom_volume_label(&self, scripts_dir: &str) -> anyhow::Result<()> {
        log::info!("[ADVANCED] 设置系统盘卷标: {}", self.volume_label);
        let volume_label_file = format!("{}\\volume_label.txt", scripts_dir);
        std::fs::write(&volume_label_file, &self.volume_label)?;
        Ok(())
    }

    /// 18. Win7 注入 USB3 驱动（固定读取程序运行目录下的 drivers\\usb3）
    ///
    /// 支持 .cab 更新包文件和普通驱动文件夹
    fn apply_win7_inject_usb3_driver(
        &self,
        target_partition: &str,
        default_loaded: bool,
        software_hive: &str,
        system_hive: &str,
        default_hive: &str,
    ) -> anyhow::Result<()> {
        // User-supplied Windows 7 compatibility paths are retired. Only the release-locked,
        // hash-verified payload may enter this boot-critical path; fields remain for config ABI.
        let custom_path: Option<PathBuf> = None;
        let architecture = Self::target_win7_architecture(target_partition)?;
        with_offline_hives_unloaded(
            default_loaded,
            software_hive,
            system_hive,
            default_hive,
            || {
                let dism = crate::core::dism::Dism::new();
                let image_path = format!("{}\\", target_partition);
                if let Some(path) = custom_path.as_ref() {
                    if !path.is_dir() {
                        anyhow::bail!("Win7 USB3驱动目录不存在: {}", path.display());
                    }
                    let processed = Self::prepare_win7_drivers(path)?;
                    let result =
                        dism.add_drivers_offline(&image_path, &processed.to_string_lossy());
                    if processed != *path {
                        let _ = std::fs::remove_dir_all(&processed);
                    }
                    result?;
                } else {
                    let root = Self::get_win7_drivers_root()
                        .ok_or_else(|| anyhow::anyhow!("无法获取内置 Win7 驱动目录"))?;
                    let payload =
                        lr_core::win7_driver_package::verify_windows7_driver_payload(&root)?;
                    let hardware_ids = lr_core::driver::list_present_hardware_ids()
                        .context("枚举当前 USB 控制器硬件 ID 失败")?;
                    let packages = payload.select_usb3_packages(&hardware_ids, architecture)?;
                    if packages.is_empty() {
                        log::warn!("[ADVANCED] 当前硬件没有匹配的内置 Win7 USB3 驱动包，安全跳过");
                    }
                    for package in packages {
                        log::info!(
                            "[ADVANCED] Win7: 注入匹配的 USB3 驱动包: {}",
                            package.display()
                        );
                        dism.add_drivers_offline(&image_path, &package.to_string_lossy())?;
                    }
                }
                log::info!("[ADVANCED] Win7 USB3驱动注入成功");
                Ok(())
            },
        )
    }

    /// 19. Win7 注入 NVMe 驱动（固定读取程序运行目录下的 drivers\\nvme）
    ///
    /// 支持 .cab 更新包文件（如 KB2990941, KB3087873）和普通驱动文件夹
    fn apply_win7_inject_nvme_driver(
        &self,
        target_partition: &str,
        default_loaded: bool,
        software_hive: &str,
        system_hive: &str,
        default_hive: &str,
    ) -> anyhow::Result<()> {
        let custom_path: Option<PathBuf> = None;
        let architecture = Self::target_win7_architecture(target_partition)?;
        with_offline_hives_unloaded(
            default_loaded,
            software_hive,
            system_hive,
            default_hive,
            || {
                let image_path = format!("{}\\", target_partition);
                if let Some(path) = custom_path.as_ref() {
                    if !path.is_dir() {
                        anyhow::bail!("Win7 NVMe驱动目录不存在: {}", path.display());
                    }
                    let processed = Self::prepare_win7_drivers(path)?;
                    let dism = crate::core::dism::Dism::new();
                    let result =
                        dism.add_drivers_offline(&image_path, &processed.to_string_lossy());
                    if processed != *path {
                        let _ = std::fs::remove_dir_all(&processed);
                    }
                    result?;
                } else {
                    let root = Self::get_win7_drivers_root()
                        .ok_or_else(|| anyhow::anyhow!("无法获取内置 Win7 驱动目录"))?;
                    let payload =
                        lr_core::win7_driver_package::verify_windows7_driver_payload(&root)?;
                    let cabs = payload.nvme_cabs(architecture)?;
                    let dism = crate::core::dism_cmd::DismCmd::new()
                        .context("初始化 DISM 命令边界失败")?;
                    for cab in cabs {
                        log::info!(
                            "[ADVANCED] Win7: 按依赖顺序安装 NVMe 更新: {}",
                            cab.display()
                        );
                        dism.add_package_offline_simple(&image_path, &cab.to_string_lossy(), None)?;
                    }
                }
                log::info!("[ADVANCED] Win7 NVMe驱动注入成功");
                Ok(())
            },
        )
    }

    /// 20. Win7 旧式处理器电源驱动兼容尝试。
    fn apply_win7_fix_acpi_bsod(&self) -> anyhow::Result<()> {
        log::info!("[ADVANCED] Win7: 尝试禁用旧式处理器电源驱动以提高启动兼容性");

        // 禁用 intelppm 服务 (Intel 电源管理)
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\intelppm",
            "Start",
            4, // 4 = Disabled
        )?;

        // 禁用 amdppm 服务 (AMD 电源管理)
        OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet001\\Services\\amdppm", "Start", 4)?;

        // 禁用 Processor 服务
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\Processor",
            "Start",
            4,
        )?;

        // 同时设置 ControlSet002 (如果存在)
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\intelppm",
            "Start",
            4,
        )?;
        OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet002\\Services\\amdppm", "Start", 4)?;
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\Processor",
            "Start",
            4,
        )?;

        log::info!("[ADVANCED] Win7 旧式处理器电源驱动兼容设置完成");
        Ok(())
    }

    /// 21. Win7 修复 INACCESSIBLE_BOOT_DEVICE (0x7B) 蓝屏
    ///
    /// 这是Win7在现代硬件上最常见的蓝屏问题，原因是存储控制器驱动未启用
    fn apply_win7_fix_storage_bsod(&self) -> anyhow::Result<()> {
        log::info!("[ADVANCED] Win7: 修复存储控制器蓝屏问题 (0x7B)");

        // ========== AHCI 相关驱动 ==========
        // msahci - Microsoft AHCI 驱动 (Win7原版自带但默认禁用)
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\msahci",
            "Start",
            0, // 0 = Boot (启动时加载)
        )?;
        // 同时设置 ControlSet002
        OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet002\\Services\\msahci", "Start", 0)?;

        // StorAHCI - 新版 AHCI 驱动 (Win8+)
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\storahci",
            "Start",
            0,
        )?;
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\storahci",
            "Start",
            0,
        )?;

        // ========== IDE 相关驱动 ==========
        // pciide - 标准 PCI IDE 控制器
        OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet001\\Services\\pciide", "Start", 0)?;
        OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet002\\Services\\pciide", "Start", 0)?;

        // intelide - Intel IDE 控制器
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\intelide",
            "Start",
            0,
        )?;
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\intelide",
            "Start",
            0,
        )?;

        // atapi - ATAPI/PATA 驱动
        OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet001\\Services\\atapi", "Start", 0)?;
        OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet002\\Services\\atapi", "Start", 0)?;

        // ========== Intel 存储驱动 ==========
        // iaStorV - Intel 快速存储技术 (RST)
        OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet001\\Services\\iaStorV", "Start", 0)?;
        OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet002\\Services\\iaStorV", "Start", 0)?;

        // iaStorAV - Intel AHCI 驱动
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\iaStorAV",
            "Start",
            0,
        )?;
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\iaStorAV",
            "Start",
            0,
        )?;

        // iaStor - 旧版 Intel 存储驱动
        OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet001\\Services\\iaStor", "Start", 0)?;
        OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet002\\Services\\iaStor", "Start", 0)?;

        // ========== NVMe 驱动 ==========
        // stornvme - Microsoft NVMe 驱动 (需要注入驱动文件才能生效)
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\stornvme",
            "Start",
            0,
        )?;
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\stornvme",
            "Start",
            0,
        )?;

        // ========== AMD 存储驱动 ==========
        // amd_sata - AMD SATA 驱动
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\amd_sata",
            "Start",
            0,
        )?;
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\amd_sata",
            "Start",
            0,
        )?;

        // amd_xata - AMD AHCI 驱动
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\amd_xata",
            "Start",
            0,
        )?;
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\amd_xata",
            "Start",
            0,
        )?;

        // amdsata - AMD SATA (另一版本)
        OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet001\\Services\\amdsata", "Start", 0)?;
        OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet002\\Services\\amdsata", "Start", 0)?;

        // ========== VMware/VirtualBox 虚拟机存储驱动 ==========
        // LSI_SAS - VMware 默认存储控制器
        OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet001\\Services\\LSI_SAS", "Start", 0)?;
        OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet002\\Services\\LSI_SAS", "Start", 0)?;

        // LSI_SAS2 - VMware LSI Logic SAS
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\LSI_SAS2",
            "Start",
            0,
        )?;
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\LSI_SAS2",
            "Start",
            0,
        )?;

        // LSI_SCSI - LSI SCSI 控制器
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet001\\Services\\LSI_SCSI",
            "Start",
            0,
        )?;
        OfflineRegistry::set_dword(
            "HKLM\\pc-sys\\ControlSet002\\Services\\LSI_SCSI",
            "Start",
            0,
        )?;

        // megasas - MegaRAID SAS 控制器
        OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet001\\Services\\megasas", "Start", 0)?;
        OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet002\\Services\\megasas", "Start", 0)?;

        // ========== 通用 SCSI 驱动 ==========
        // vhdmp - VHD Mini-Port 驱动
        OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet001\\Services\\vhdmp", "Start", 0)?;
        OfflineRegistry::set_dword("HKLM\\pc-sys\\ControlSet002\\Services\\vhdmp", "Start", 0)?;

        log::info!("[ADVANCED] Win7 存储控制器蓝屏修复设置完成");
        log::info!("[ADVANCED] 已启用: msahci, storahci, pciide, intelide, atapi, iaStorV, iaStorAV, iaStor, stornvme, amd_sata, amd_xata, amdsata, LSI_SAS, LSI_SAS2, LSI_SCSI, megasas, vhdmp");
        Ok(())
    }

    /// Windows XP 专用：离线注入存储/USB3 驱动
    /// 直接写已加载的 SYSTEM 配置单元(pc-sys)，不走 DISM。AHCI 始终注入；NVMe/USB3 按勾选。
    fn apply_xp_inject_drivers(&self, target_partition: &str) -> anyhow::Result<()> {
        let xp_dir = Self::get_program_dir().map(|b| b.join("bin").join("drivers").join("xp"));
        match xp_dir.as_ref() {
            Some(dir) if dir.is_dir() => {
                log::info!(
                    "[ADVANCED] XP: 离线注入驱动 (AHCI 始终, NVMe={}, USB3={}) 源: {}",
                    self.xp_inject_nvme_driver,
                    self.xp_inject_usb3_driver,
                    dir.display()
                );
                let output = lr_core::xp::inject_xp_drivers(
                    target_partition,
                    dir,
                    "pc-sys",
                    self.xp_inject_nvme_driver,
                    self.xp_inject_usb3_driver,
                )
                .map_err(anyhow::Error::msg)?;
                log::info!("[ADVANCED] XP 驱动注入完成:\n{}", output);
            }
            _ => anyhow::bail!(
                "requested XP driver directory is missing: {}",
                xp_dir
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "bin\\drivers\\xp".to_string())
            ),
        }
        Ok(())
    }

    /// 转换 .reg 文件内容以适配离线注册表
    fn convert_reg_file_for_offline(content: &str) -> String {
        content
            .replace(
                "HKEY_LOCAL_MACHINE\\SOFTWARE",
                "HKEY_LOCAL_MACHINE\\pc-soft",
            )
            .replace("HKEY_LOCAL_MACHINE\\SYSTEM", "HKEY_LOCAL_MACHINE\\pc-sys")
            .replace("HKEY_CURRENT_USER", "HKEY_LOCAL_MACHINE\\pc-default")
            .replace("[HKLM\\SOFTWARE", "[HKLM\\pc-soft")
            .replace("[HKLM\\SYSTEM", "[HKLM\\pc-sys")
    }

    fn copy_dir_all(src: &str, dst: &str) -> anyhow::Result<()> {
        std::fs::create_dir_all(dst)?;
        for entry in WalkDir::new(src) {
            let entry = entry?;
            let target = std::path::Path::new(dst).join(entry.path().strip_prefix(src)?);
            if entry.file_type().is_dir() {
                std::fs::create_dir_all(&target)?;
            } else {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(entry.path(), &target)?;
            }
        }
        Ok(())
    }

    /// 准备 Win7 驱动目录
    ///
    /// 此函数处理驱动目录，支持以下文件类型：
    /// - .cab 文件（Windows 更新包，如 KB2990941, KB3087873）
    /// - 普通驱动文件夹（包含 .inf 文件）
    ///
    /// 如果目录中存在 .cab 文件，会将它们解压到临时目录，
    /// 并将普通驱动文件也复制到该目录，返回合并后的路径。
    ///
    /// # 参数
    /// - `driver_dir`: 原始驱动目录
    ///
    /// # 返回
    /// - 处理后的驱动目录路径（可能是原目录或临时目录）
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

        let extractor = CabinetExtractor::new()?;
        let staging = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-win7-drivers",
        )?;
        let temp_dir = staging.path();

        for cab_path in &cab_files {
            let cab_name = cab_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown");

            let extract_dir = temp_dir.join(cab_name);

            log::info!(
                "[ADVANCED] 解压: {} -> {}",
                cab_path.display(),
                extract_dir.display()
            );

            let files = extractor.extract(cab_path, &extract_dir)?;
            log::info!("[ADVANCED] 成功解压 {} 个文件", files.len());
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
                    Self::copy_dir_recursive(&path, &dest)?;
                } else {
                    // 复制文件
                    std::fs::copy(&path, &dest)?;
                }
            }
        }

        log::info!("[ADVANCED] Win7 驱动准备完成: {}", temp_dir.display());

        Ok(staging.into_path())
    }

    /// 递归复制目录
    fn copy_dir_recursive(src: &PathBuf, dst: &PathBuf) -> anyhow::Result<()> {
        std::fs::create_dir_all(dst)?;

        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let path = entry.path();
            let dest = dst.join(entry.file_name());

            if path.is_dir() {
                Self::copy_dir_recursive(&path, &dest)?;
            } else {
                std::fs::copy(&path, &dest)?;
            }
        }

        Ok(())
    }
}
