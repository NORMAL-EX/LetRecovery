use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::core::hardware_info::HardwareInfo;
use crate::core::registry::OfflineRegistry;
use std::path::PathBuf;

/// 系统安装高级选项
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AdvancedOptions {
    // 系统优化选项
    pub remove_shortcut_arrow: bool,
    pub restore_classic_context_menu: bool,
    pub bypass_nro: bool,
    pub disable_windows_update: bool,
    pub disable_windows_defender: bool,
    pub disable_reserved_storage: bool,
    pub disable_uac: bool,
    pub disable_device_encryption: bool,
    pub remove_uwp_apps: bool,

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

    /// 获取 Win7 驱动目录（程序运行目录下的 drivers\{usb3|nvme}）
    fn get_win7_driver_dirs() -> (Option<PathBuf>, Option<PathBuf>) {
        let base = Self::get_program_dir();
        let usb3 = base.as_ref().map(|b| b.join("drivers").join("usb3"));
        let nvme = base.as_ref().map(|b| b.join("drivers").join("nvme"));
        (usb3, nvme)
    }
    
    /// 获取 UefiSeven 目录（程序运行目录下的 uefiseven）
    fn get_uefiseven_dir() -> Option<PathBuf> {
        Self::get_program_dir().map(|b| b.join("uefiseven"))
    }
    
    /// 显示依赖无人值守的复选框
    /// 如果无人值守被禁用，该复选框也会被禁用并显示提示
    fn show_unattend_dependent_checkbox(
        ui: &mut egui::Ui,
        value: &mut bool,
        label: &str,
        unattend_disabled: bool,
        tooltip: &str,
    ) {
        if unattend_disabled {
            // 禁用状态：强制取消勾选并显示禁用的复选框
            *value = false;
            ui.add_enabled(false, egui::Checkbox::new(value, label))
                .on_disabled_hover_text(tooltip);
        } else {
            // 正常状态
            ui.checkbox(value, label);
        }
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
            println!("[UEFISEVEN] Win7 UEFI补丁未启用，跳过");
            return Ok(());
        }
        
        println!("[UEFISEVEN] 开始应用 UefiSeven 补丁");
        
        // 获取 UefiSeven 源文件目录
        let uefiseven_dir = match Self::get_uefiseven_dir() {
            Some(dir) if dir.exists() => dir,
            Some(dir) => {
                println!("[UEFISEVEN] UefiSeven 目录不存在: {}", dir.display());
                return Err(anyhow::anyhow!("UefiSeven 目录不存在: {}", dir.display()));
            }
            None => {
                println!("[UEFISEVEN] 无法获取程序运行目录");
                return Err(anyhow::anyhow!("无法获取程序运行目录"));
            }
        };
        
        // 检查 UefiSeven 文件
        let uefiseven_efi = uefiseven_dir.join("bootx64.efi");
        let uefiseven_ini = uefiseven_dir.join("UefiSeven.ini");
        
        if !uefiseven_efi.exists() {
            println!("[UEFISEVEN] UefiSeven bootx64.efi 不存在: {}", uefiseven_efi.display());
            return Err(anyhow::anyhow!("UefiSeven bootx64.efi 不存在"));
        }
        
        // 查找 EFI 系统分区
        let efi_partition = Self::find_efi_partition()?;
        println!("[UEFISEVEN] 找到 EFI 分区: {}", efi_partition);
        
        // 确保 EFI 分区已挂载
        let efi_mount_point = Self::ensure_efi_mounted(&efi_partition)?;
        println!("[UEFISEVEN] EFI 分区挂载点: {}", efi_mount_point);
        
        // Microsoft Boot 目录
        let ms_boot_dir = format!("{}\\EFI\\Microsoft\\Boot", efi_mount_point);
        let bootmgfw_path = format!("{}\\bootmgfw.efi", ms_boot_dir);
        let bootmgfw_original = format!("{}\\bootmgfw.original.efi", ms_boot_dir);
        let uefiseven_target = format!("{}\\bootmgfw.efi", ms_boot_dir);
        let uefiseven_ini_target = format!("{}\\UefiSeven.ini", ms_boot_dir);
        
        // 检查原始 bootmgfw.efi 是否存在
        if !std::path::Path::new(&bootmgfw_path).exists() {
            println!("[UEFISEVEN] bootmgfw.efi 不存在: {}", bootmgfw_path);
            return Err(anyhow::anyhow!("bootmgfw.efi 不存在，请确保引导修复已完成"));
        }
        
        // 备份原始 bootmgfw.efi（如果尚未备份）
        if !std::path::Path::new(&bootmgfw_original).exists() {
            println!("[UEFISEVEN] 备份原始 bootmgfw.efi 到 bootmgfw.original.efi");
            std::fs::copy(&bootmgfw_path, &bootmgfw_original)?;
        } else {
            println!("[UEFISEVEN] bootmgfw.original.efi 已存在，跳过备份");
        }
        
        // 复制 UefiSeven 到 bootmgfw.efi（替换原来的）
        println!("[UEFISEVEN] 部署 UefiSeven bootx64.efi -> bootmgfw.efi");
        std::fs::copy(&uefiseven_efi, &uefiseven_target)?;
        
        // 复制配置文件（如果存在）
        if uefiseven_ini.exists() {
            println!("[UEFISEVEN] 部署 UefiSeven.ini 配置文件");
            std::fs::copy(&uefiseven_ini, &uefiseven_ini_target)?;
        } else {
            // 创建默认配置文件
            println!("[UEFISEVEN] 创建默认 UefiSeven.ini 配置");
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
        
        println!("[UEFISEVEN] UefiSeven 补丁应用成功");
        println!("[UEFISEVEN] 启动流程: UEFI -> UefiSeven -> bootmgfw.original.efi -> Windows 7");
        
        Ok(())
    }
    
    /// 查找 EFI 系统分区
    fn find_efi_partition() -> anyhow::Result<String> {
        use std::process::Command;
        
        // 使用 PowerShell 查找 EFI 分区
        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                r#"
                $efiPart = Get-Partition | Where-Object { $_.GptType -eq '{c12a7328-f81f-11d2-ba4b-00a0c93ec93b}' } | Select-Object -First 1
                if ($efiPart) {
                    $efiPart.DiskNumber.ToString() + ':' + $efiPart.PartitionNumber.ToString()
                }
                "#
            ])
            .output()?;
        
        let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
        
        if result.is_empty() {
            return Err(anyhow::anyhow!("未找到 EFI 系统分区"));
        }
        
        Ok(result)
    }
    
    /// 确保 EFI 分区已挂载，返回挂载点
    fn ensure_efi_mounted(efi_partition: &str) -> anyhow::Result<String> {
        use std::process::Command;
        
        // 解析磁盘号和分区号
        let parts: Vec<&str> = efi_partition.split(':').collect();
        if parts.len() != 2 {
            return Err(anyhow::anyhow!("无效的 EFI 分区标识: {}", efi_partition));
        }
        
        let disk_num = parts[0];
        let part_num = parts[1];
        
        // 检查是否已经有挂载点
        let check_output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(r#"
                $vol = Get-Partition -DiskNumber {} -PartitionNumber {} | Get-Volume -ErrorAction SilentlyContinue
                if ($vol -and $vol.DriveLetter) {{
                    $vol.DriveLetter + ':'
                }}
                "#, disk_num, part_num)
            ])
            .output()?;
        
        let existing_mount = String::from_utf8_lossy(&check_output.stdout).trim().to_string();
        
        if !existing_mount.is_empty() && existing_mount.len() == 2 {
            return Ok(existing_mount);
        }
        
        // 查找可用盘符
        let find_letter = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                r#"
                $used = (Get-Volume).DriveLetter
                $available = 90..65 | ForEach-Object { [char]$_ } | Where-Object { $_ -notin $used }
                if ($available) { $available[0] }
                "#
            ])
            .output()?;
        
        let letter = String::from_utf8_lossy(&find_letter.stdout).trim().to_string();
        
        if letter.is_empty() {
            return Err(anyhow::anyhow!("没有可用的盘符"));
        }
        
        // 使用 mountvol 挂载 EFI 分区
        let mount_result = Command::new("cmd")
            .args([
                "/c",
                &format!("mountvol {}:\\ /s", letter)
            ])
            .output();
        
        match mount_result {
            Ok(output) if output.status.success() => {
                Ok(format!("{}:", letter))
            }
            Ok(output) => {
                // mountvol /s 可能失败，尝试使用 diskpart
                let diskpart_script = format!(
                    "select disk {}\nselect partition {}\nassign letter={}\n",
                    disk_num, part_num, letter
                );
                
                let temp_script = std::env::temp_dir().join("efi_mount.txt");
                std::fs::write(&temp_script, &diskpart_script)?;
                
                let diskpart_result = Command::new("diskpart")
                    .args(["/s", &temp_script.to_string_lossy()])
                    .output();
                
                let _ = std::fs::remove_file(&temp_script);
                
                match diskpart_result {
                    Ok(dp_output) if dp_output.status.success() => {
                        Ok(format!("{}:", letter))
                    }
                    _ => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        Err(anyhow::anyhow!("挂载 EFI 分区失败: {}", stderr))
                    }
                }
            }
            Err(e) => Err(anyhow::anyhow!("执行挂载命令失败: {}", e))
        }
    }

    /// 应用选项到目标系统
    pub fn apply_to_system(&self, target_partition: &str) -> anyhow::Result<()> {
        println!("[ADVANCED] 开始应用高级选项到: {}", target_partition);
        
        let windows_path = format!("{}\\Windows", target_partition);
        let software_hive = format!("{}\\System32\\config\\SOFTWARE", windows_path);
        let system_hive = format!("{}\\System32\\config\\SYSTEM", windows_path);
        let default_hive = format!("{}\\System32\\config\\DEFAULT", windows_path);

        // 加载离线注册表
        println!("[ADVANCED] 加载离线注册表...");
        OfflineRegistry::load_hive("pc-soft", &software_hive)?;
        OfflineRegistry::load_hive("pc-sys", &system_hive)?;
        // DEFAULT 用于设置默认用户配置（如经典右键菜单）
        let default_loaded = OfflineRegistry::load_hive("pc-default", &default_hive).is_ok();

        // 创建脚本目录（用于存放自定义脚本）
        let scripts_dir = format!("{}\\{}", target_partition, Self::SCRIPTS_DIR);
        std::fs::create_dir_all(&scripts_dir)?;
        println!("[ADVANCED] 脚本目录: {}", scripts_dir);

        // ============ 系统优化选项 ============

        // 1. 移除快捷方式小箭头
        if self.remove_shortcut_arrow {
            println!("[ADVANCED] 移除快捷方式小箭头");
            let _ = OfflineRegistry::set_string(
                "HKLM\\pc-soft\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Shell Icons",
                "29",
                "%systemroot%\\system32\\imageres.dll,197",
            );
        }

        // 2. Win11恢复经典右键菜单
        if self.restore_classic_context_menu {
            println!("[ADVANCED] 恢复经典右键菜单");
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
        if self.bypass_nro {
            println!("[ADVANCED] 设置OOBE绕过联网");
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-soft\\Microsoft\\Windows\\CurrentVersion\\OOBE",
                "BypassNRO",
                1,
            );
        }

        // 4. 禁用Windows更新
        if self.disable_windows_update {
            println!("[ADVANCED] 禁用Windows更新服务");
            // 禁用 Windows Update 服务
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet001\\Services\\wuauserv",
                "Start",
                4, // 4 = Disabled
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
        if self.disable_windows_defender {
            println!("[ADVANCED] 禁用Windows Defender");
            // 禁用实时保护
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-soft\\Policies\\Microsoft\\Windows Defender",
                "DisableAntiSpyware",
                1,
            );
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-soft\\Policies\\Microsoft\\Windows Defender\\Real-Time Protection",
                "DisableRealtimeMonitoring",
                1,
            );
            // 禁用服务
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet001\\Services\\WinDefend",
                "Start",
                4, // Disabled
            );
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet001\\Services\\WdNisSvc",
                "Start",
                4,
            );
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet001\\Services\\SecurityHealthService",
                "Start",
                4,
            );
        }

        // 6. 禁用系统保留空间
        if self.disable_reserved_storage {
            println!("[ADVANCED] 禁用系统保留空间");
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
        if self.disable_uac {
            println!("[ADVANCED] 禁用UAC");
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
        if self.disable_device_encryption {
            println!("[ADVANCED] 禁用自动设备加密");
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
            // 禁用设备加密
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet001\\Services\\BDESVC",
                "Start",
                4, // Disabled
            );
        }

        // 9. 删除预装UWP应用 - 通过删除 AppxProvisioned 配置
        if self.remove_uwp_apps {
            println!("[ADVANCED] 配置删除预装UWP应用");
            // 创建首次登录脚本来删除UWP应用
            let remove_uwp_script = Self::generate_remove_uwp_script();
            let uwp_script_path = format!("{}\\remove_uwp.ps1", scripts_dir);
            std::fs::write(&uwp_script_path, &remove_uwp_script)?;
            println!("[ADVANCED] UWP删除脚本已写入: {}", uwp_script_path);
        }

        // ============ 自定义脚本 ============

        // 10. 系统部署中运行脚本
        if self.run_script_during_deploy && !self.deploy_script_path.is_empty() {
            println!("[ADVANCED] 复制部署脚本: {}", self.deploy_script_path);
            let target_path = format!("{}\\deploy.bat", scripts_dir);
            std::fs::copy(&self.deploy_script_path, &target_path)?;
            println!("[ADVANCED] 部署脚本已复制到: {}", target_path);
        }

        // 11. 首次登录运行脚本
        if self.run_script_first_login && !self.first_login_script_path.is_empty() {
            println!("[ADVANCED] 复制首次登录脚本: {}", self.first_login_script_path);
            let target_path = format!("{}\\firstlogon.bat", scripts_dir);
            std::fs::copy(&self.first_login_script_path, &target_path)?;
            println!("[ADVANCED] 首次登录脚本已复制到: {}", target_path);
        }

        // ============ 自定义内容 ============

        // 12. 导入自定义驱动 - 使用 DISM 实际安装
        if self.import_custom_drivers && !self.custom_drivers_path.is_empty() {
            println!("[ADVANCED] 导入自定义驱动: {}", self.custom_drivers_path);
            
            // 先卸载注册表，因为 DISM 可能需要独占访问
            let _ = OfflineRegistry::unload_hive("pc-soft");
            let _ = OfflineRegistry::unload_hive("pc-sys");
            if default_loaded {
                let _ = OfflineRegistry::unload_hive("pc-default");
            }
            
            // 使用 DISM 添加驱动
            let dism = crate::core::dism::Dism::new();
            let image_path = format!("{}\\", target_partition);
            match dism.add_drivers_offline(&image_path, &self.custom_drivers_path) {
                Ok(_) => println!("[ADVANCED] 自定义驱动导入成功"),
                Err(e) => println!("[ADVANCED] 自定义驱动导入失败: {} (继续执行)", e),
            }
            
            // 重新加载注册表
            let _ = OfflineRegistry::load_hive("pc-soft", &software_hive);
            let _ = OfflineRegistry::load_hive("pc-sys", &system_hive);
        }

        // 13. 导入磁盘控制器驱动（Win10/Win11 x64）
        if self.import_storage_controller_drivers {
            let storage_drivers_dir = crate::utils::path::get_exe_dir()
                .join("drivers")
                .join("storage_controller");
            if storage_drivers_dir.is_dir() {
                println!(
                    "[ADVANCED] 导入磁盘控制器驱动: {}",
                    storage_drivers_dir.display()
                );

                // 先卸载注册表，因为 DISM 可能需要独占访问
                let _ = OfflineRegistry::unload_hive("pc-soft");
                let _ = OfflineRegistry::unload_hive("pc-sys");
                if default_loaded {
                    let _ = OfflineRegistry::unload_hive("pc-default");
                }

                let dism = crate::core::dism::Dism::new();
                let image_path = format!("{}\\", target_partition);
                let storage_drivers_path = storage_drivers_dir.to_string_lossy().to_string();
                match dism.add_drivers_offline(&image_path, &storage_drivers_path) {
                    Ok(_) => println!("[ADVANCED] 磁盘控制器驱动导入成功"),
                    Err(e) => println!("[ADVANCED] 磁盘控制器驱动导入失败: {} (继续执行)", e),
                }

                // 重新加载注册表
                let _ = OfflineRegistry::load_hive("pc-soft", &software_hive);
                let _ = OfflineRegistry::load_hive("pc-sys", &system_hive);
            } else {
                println!(
                    "[ADVANCED] 未找到磁盘控制器驱动目录: {}",
                    storage_drivers_dir.display()
                );
            }
        }

        // 14. 导入注册表文件 - 实际导入到离线注册表
        if self.import_registry_file && !self.registry_file_path.is_empty() {
            println!("[ADVANCED] 导入注册表文件: {}", self.registry_file_path);
            
            // 读取原始 .reg 文件
            if let Ok(reg_content) = std::fs::read_to_string(&self.registry_file_path) {
                // 转换路径：HKEY_LOCAL_MACHINE\SOFTWARE -> HKLM\pc-soft
                // 转换路径：HKEY_LOCAL_MACHINE\SYSTEM -> HKLM\pc-sys
                let converted = Self::convert_reg_file_for_offline(&reg_content);
                
                // 写入临时文件
                let temp_reg = format!("{}\\temp_import.reg", scripts_dir);
                std::fs::write(&temp_reg, &converted)?;
                
                // 导入注册表
                match OfflineRegistry::import_reg_file(&temp_reg) {
                    Ok(_) => println!("[ADVANCED] 注册表文件导入成功"),
                    Err(e) => println!("[ADVANCED] 注册表文件导入失败: {} (继续执行)", e),
                }
                
                // 删除临时文件
                let _ = std::fs::remove_file(&temp_reg);
            }
        }

        // 15. 导入自定义文件
        if self.import_custom_files && !self.custom_files_path.is_empty() {
            println!("[ADVANCED] 导入自定义文件: {}", self.custom_files_path);
            match Self::copy_dir_all(&self.custom_files_path, target_partition) {
                Ok(_) => println!("[ADVANCED] 自定义文件导入成功"),
                Err(e) => println!("[ADVANCED] 自定义文件导入失败: {} (继续执行)", e),
            }
        }

        // 16. 自定义用户名 - 写入标记文件供无人值守使用
        if self.custom_username && !self.username.is_empty() {
            println!("[ADVANCED] 设置自定义用户名: {}", self.username);
            let username_file = format!("{}\\username.txt", scripts_dir);
            std::fs::write(&username_file, &self.username)?;
        }

        // 17. 自定义系统盘卷标 - 写入标记文件供格式化时使用
        if self.custom_volume_label && !self.volume_label.is_empty() {
            println!("[ADVANCED] 设置系统盘卷标: {}", self.volume_label);
            let volume_label_file = format!("{}\\volume_label.txt", scripts_dir);
            std::fs::write(&volume_label_file, &self.volume_label)?;
        }

        // ============ Win7 专用选项 ============
        
        // 18. Win7 注入 USB3 驱动（固定读取程序运行目录下的 drivers\\usb3）
        // 支持 .cab 更新包文件和普通驱动文件夹
        if self.win7_inject_usb3_driver {
            let usb3_path = if !self.win7_usb3_driver_path.is_empty() {
                Some(PathBuf::from(&self.win7_usb3_driver_path))
            } else {
                let (usb3_dir, _) = Self::get_win7_driver_dirs();
                usb3_dir
            };

            let usb3_path = match usb3_path {
                Some(p) if p.exists() => p,
                Some(p) => {
                    println!("[ADVANCED] Win7 USB3驱动目录不存在，跳过: {}", p.to_string_lossy());
                    PathBuf::new()
                }
                None => {
                    println!("[ADVANCED] 无法获取 Win7 USB3驱动目录，跳过");
                    PathBuf::new()
                }
            };

            if usb3_path.as_os_str().is_empty() {
                // 目录不可用，直接跳过
            } else {
                println!("[ADVANCED] Win7: 处理USB3驱动目录: {}", usb3_path.to_string_lossy());
                
                // 先卸载注册表
                let _ = OfflineRegistry::unload_hive("pc-soft");
                let _ = OfflineRegistry::unload_hive("pc-sys");
                if default_loaded {
                    let _ = OfflineRegistry::unload_hive("pc-default");
                }
                
                // 处理目录中的驱动（包括 .cab 文件）
                let processed_path = Self::prepare_win7_drivers(&usb3_path)?;
                
                let dism = crate::core::dism::Dism::new();
                let image_path = format!("{}\\", target_partition);
                match dism.add_drivers_offline(&image_path, &processed_path.to_string_lossy()) {
                    Ok(_) => println!("[ADVANCED] Win7 USB3驱动注入成功"),
                    Err(e) => println!("[ADVANCED] Win7 USB3驱动注入失败: {} (继续执行)", e),
                }
                
                // 清理临时目录（如果使用了临时目录）
                if processed_path != usb3_path {
                    let _ = std::fs::remove_dir_all(&processed_path);
                }
                
                // 重新加载注册表
                let _ = OfflineRegistry::load_hive("pc-soft", &software_hive);
                let _ = OfflineRegistry::load_hive("pc-sys", &system_hive);
            }
        }
        
        // 19. Win7 注入 NVMe 驱动（固定读取程序运行目录下的 drivers\\nvme）
        // 支持 .cab 更新包文件（如 KB2990941, KB3087873）和普通驱动文件夹
        if self.win7_inject_nvme_driver {
            let nvme_path = if !self.win7_nvme_driver_path.is_empty() {
                Some(PathBuf::from(&self.win7_nvme_driver_path))
            } else {
                let (_, nvme_dir) = Self::get_win7_driver_dirs();
                nvme_dir
            };

            let nvme_path = match nvme_path {
                Some(p) if p.exists() => p,
                Some(p) => {
                    println!("[ADVANCED] Win7 NVMe驱动目录不存在，跳过: {}", p.to_string_lossy());
                    PathBuf::new()
                }
                None => {
                    println!("[ADVANCED] 无法获取 Win7 NVMe驱动目录，跳过");
                    PathBuf::new()
                }
            };

            if nvme_path.as_os_str().is_empty() {
                // 目录不可用，直接跳过
            } else {
                println!("[ADVANCED] Win7: 处理NVMe驱动目录: {}", nvme_path.to_string_lossy());
                
                // 先卸载注册表
                let _ = OfflineRegistry::unload_hive("pc-soft");
                let _ = OfflineRegistry::unload_hive("pc-sys");
                if default_loaded {
                    let _ = OfflineRegistry::unload_hive("pc-default");
                }
                
                // 处理目录中的驱动（包括 .cab 文件）
                let processed_path = Self::prepare_win7_drivers(&nvme_path)?;
                
                let dism = crate::core::dism::Dism::new();
                let image_path = format!("{}\\", target_partition);
                match dism.add_drivers_offline(&image_path, &processed_path.to_string_lossy()) {
                    Ok(_) => println!("[ADVANCED] Win7 NVMe驱动注入成功"),
                    Err(e) => println!("[ADVANCED] Win7 NVMe驱动注入失败: {} (继续执行)", e),
                }
                
                // 清理临时目录（如果使用了临时目录）
                if processed_path != nvme_path {
                    let _ = std::fs::remove_dir_all(&processed_path);
                }
                
                // 重新加载注册表
                let _ = OfflineRegistry::load_hive("pc-soft", &software_hive);
                let _ = OfflineRegistry::load_hive("pc-sys", &system_hive);
            }
        }
        
        // 20. Win7 修复 ACPI_BIOS_ERROR (0xA5) 蓝屏
        if self.win7_fix_acpi_bsod {
            println!("[ADVANCED] Win7: 修复ACPI蓝屏问题");
            
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
            
            println!("[ADVANCED] Win7 ACPI蓝屏修复设置完成");
        }

        // 21. Win7 修复 INACCESSIBLE_BOOT_DEVICE (0x7B) 蓝屏
        // 这是Win7在现代硬件上最常见的蓝屏问题，原因是存储控制器驱动未启用
        if self.win7_fix_storage_bsod {
            println!("[ADVANCED] Win7: 修复存储控制器蓝屏问题 (0x7B)");
            
            // ========== AHCI 相关驱动 ==========
            // msahci - Microsoft AHCI 驱动 (Win7原版自带但默认禁用)
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet001\\Services\\msahci",
                "Start",
                0, // 0 = Boot (启动时加载)
            );
            // 同时设置 ControlSet002
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet002\\Services\\msahci",
                "Start",
                0,
            );
            
            // StorAHCI - 新版 AHCI 驱动 (Win8+)
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet001\\Services\\storahci",
                "Start",
                0,
            );
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet002\\Services\\storahci",
                "Start",
                0,
            );
            
            // ========== IDE 相关驱动 ==========
            // pciide - 标准 PCI IDE 控制器
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet001\\Services\\pciide",
                "Start",
                0,
            );
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet002\\Services\\pciide",
                "Start",
                0,
            );
            
            // intelide - Intel IDE 控制器
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet001\\Services\\intelide",
                "Start",
                0,
            );
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet002\\Services\\intelide",
                "Start",
                0,
            );
            
            // atapi - ATAPI/PATA 驱动
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet001\\Services\\atapi",
                "Start",
                0,
            );
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet002\\Services\\atapi",
                "Start",
                0,
            );
            
            // ========== Intel 存储驱动 ==========
            // iaStorV - Intel 快速存储技术 (RST)
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet001\\Services\\iaStorV",
                "Start",
                0,
            );
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet002\\Services\\iaStorV",
                "Start",
                0,
            );
            
            // iaStorAV - Intel AHCI 驱动
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet001\\Services\\iaStorAV",
                "Start",
                0,
            );
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet002\\Services\\iaStorAV",
                "Start",
                0,
            );
            
            // iaStor - 旧版 Intel 存储驱动
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet001\\Services\\iaStor",
                "Start",
                0,
            );
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet002\\Services\\iaStor",
                "Start",
                0,
            );
            
            // ========== NVMe 驱动 ==========
            // stornvme - Microsoft NVMe 驱动 (需要注入驱动文件才能生效)
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet001\\Services\\stornvme",
                "Start",
                0,
            );
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet002\\Services\\stornvme",
                "Start",
                0,
            );
            
            // ========== AMD 存储驱动 ==========
            // amd_sata - AMD SATA 驱动
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet001\\Services\\amd_sata",
                "Start",
                0,
            );
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet002\\Services\\amd_sata",
                "Start",
                0,
            );
            
            // amd_xata - AMD AHCI 驱动
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet001\\Services\\amd_xata",
                "Start",
                0,
            );
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet002\\Services\\amd_xata",
                "Start",
                0,
            );
            
            // amdsata - AMD SATA (另一版本)
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet001\\Services\\amdsata",
                "Start",
                0,
            );
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet002\\Services\\amdsata",
                "Start",
                0,
            );
            
            // ========== VMware/VirtualBox 虚拟机存储驱动 ==========
            // LSI_SAS - VMware 默认存储控制器
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet001\\Services\\LSI_SAS",
                "Start",
                0,
            );
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet002\\Services\\LSI_SAS",
                "Start",
                0,
            );
            
            // LSI_SAS2 - VMware LSI Logic SAS
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet001\\Services\\LSI_SAS2",
                "Start",
                0,
            );
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet002\\Services\\LSI_SAS2",
                "Start",
                0,
            );
            
            // LSI_SCSI - LSI SCSI 控制器
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet001\\Services\\LSI_SCSI",
                "Start",
                0,
            );
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet002\\Services\\LSI_SCSI",
                "Start",
                0,
            );
            
            // megasas - MegaRAID SAS 控制器
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet001\\Services\\megasas",
                "Start",
                0,
            );
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet002\\Services\\megasas",
                "Start",
                0,
            );
            
            // ========== 通用 SCSI 驱动 ==========
            // vhdmp - VHD Mini-Port 驱动
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet001\\Services\\vhdmp",
                "Start",
                0,
            );
            let _ = OfflineRegistry::set_dword(
                "HKLM\\pc-sys\\ControlSet002\\Services\\vhdmp",
                "Start",
                0,
            );
            
            println!("[ADVANCED] Win7 存储控制器蓝屏修复设置完成");
            println!("[ADVANCED] 已启用: msahci, storahci, pciide, intelide, atapi, iaStorV, iaStorAV, iaStor, stornvme, amd_sata, amd_xata, amdsata, LSI_SAS, LSI_SAS2, LSI_SCSI, megasas, vhdmp");
        }

        // 卸载注册表
        println!("[ADVANCED] 卸载离线注册表...");
        let _ = OfflineRegistry::unload_hive("pc-soft");
        let _ = OfflineRegistry::unload_hive("pc-sys");
        if default_loaded {
            let _ = OfflineRegistry::unload_hive("pc-default");
        }

        println!("[ADVANCED] 高级选项应用完成");
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

    /// 转换 .reg 文件内容以适配离线注册表
    fn convert_reg_file_for_offline(content: &str) -> String {
        content
            .replace("HKEY_LOCAL_MACHINE\\SOFTWARE", "HKEY_LOCAL_MACHINE\\pc-soft")
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
            println!("[ADVANCED] 目录中没有 .cab 文件，直接使用原目录");
            return Ok(driver_dir.clone());
        }
        
        println!("[ADVANCED] 发现 {} 个 .cab 文件，开始解压", cab_files.len());
        
        // 尝试创建 Cabinet 解压器
        let extractor = match CabinetExtractor::new() {
            Ok(e) => e,
            Err(e) => {
                println!("[ADVANCED] 无法创建 Cabinet 解压器: {} (将使用原目录)", e);
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
            
            println!("[ADVANCED] 解压: {} -> {}", 
                cab_path.display(), extract_dir.display());
            
            match extractor.extract(cab_path, &extract_dir) {
                Ok(files) => {
                    println!("[ADVANCED] 成功解压 {} 个文件", files.len());
                    extract_success_count += 1;
                }
                Err(e) => {
                    println!("[ADVANCED] 解压 {} 失败: {} (跳过)", cab_path.display(), e);
                }
            }
        }
        
        // 如果所有 cab 文件都解压失败，清理临时目录并返回原目录
        if extract_success_count == 0 {
            println!("[ADVANCED] 所有 .cab 文件解压失败，使用原目录");
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Ok(driver_dir.clone());
        }
        
        // 如果原目录有普通驱动文件或子目录，也复制到临时目录
        if has_inf_files || has_subdirs {
            println!("[ADVANCED] 复制原目录中的其他驱动文件");
            
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
        
        println!("[ADVANCED] Win7 驱动准备完成: {}", temp_dir.display());
        
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
                Self::copy_dir_recursive(&path, &dest)?;
            } else {
                std::fs::copy(&path, &dest)?;
            }
        }
        
        Ok(())
    }

    /// 显示高级选项界面
    /// 
    /// # 参数
    /// - `unattend_disabled`: 无人值守选项是否被禁用（由于目标分区已存在配置文件）
    /// - `is_win7`: 当前选择的镜像是否为 Windows 7
    /// - `is_uefi_mode`: 当前安装模式是否为 UEFI
    pub fn show_ui(&mut self, ui: &mut egui::Ui, hardware_info: Option<&HardwareInfo>, unattend_disabled: bool, is_win7: bool, is_uefi_mode: bool) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            // ============ Win7 专用选项（仅当选择Win7镜像时显示）============
            if is_win7 {
                ui.heading("Windows 7 专用选项");
                ui.separator();
                
                ui.colored_label(
                    egui::Color32::from_rgb(255, 165, 0),
                    "⚠ 以下选项仅适用于 Windows 7 x64 安装",
                );
                ui.add_space(5.0);
                
                let (usb3_dir, nvme_dir) = Self::get_win7_driver_dirs();

                // USB3 驱动注入（固定读取程序运行目录下的 drivers\usb3）
                ui.vertical(|ui| {
                    ui.checkbox(&mut self.win7_inject_usb3_driver, "注入USB3.0驱动");
                    if self.win7_inject_usb3_driver {
                        if let Some(dir) = &usb3_dir {
                            self.win7_usb3_driver_path = dir.to_string_lossy().to_string();
                            if !dir.exists() {
                                ui.colored_label(
                                    egui::Color32::from_rgb(255, 165, 0),
                                    "未找到该驱动目录，将跳过 USB3 驱动注入",
                                );
                            }
                        } else {
                            self.win7_usb3_driver_path.clear();
                            ui.colored_label(
                                egui::Color32::from_rgb(255, 165, 0),
                                "无法获取程序运行目录，将跳过 USB3 驱动注入",
                            );
                        }
                    }
                });
                if self.win7_inject_usb3_driver {
                    ui.label(
                        egui::RichText::new("Win7原生不支持USB3.0，安装时键鼠可能无法使用")
                            .small()
                            .color(egui::Color32::GRAY),
                    );
                }
                
                // NVMe 驱动注入（固定读取程序运行目录下的 drivers\nvme）
                ui.vertical(|ui| {
                    ui.checkbox(&mut self.win7_inject_nvme_driver, "注入NVMe驱动");
                    if self.win7_inject_nvme_driver {
                        if let Some(dir) = &nvme_dir {
                            self.win7_nvme_driver_path = dir.to_string_lossy().to_string();
                            if !dir.exists() {
                                ui.colored_label(
                                    egui::Color32::from_rgb(255, 165, 0),
                                    "未找到该驱动目录，将跳过 NVMe 驱动注入",
                                );
                            }
                        } else {
                            self.win7_nvme_driver_path.clear();
                            ui.colored_label(
                                egui::Color32::from_rgb(255, 165, 0),
                                "无法获取程序运行目录，将跳过 NVMe 驱动注入",
                            );
                        }
                    }
                });
                if self.win7_inject_nvme_driver {
                    ui.label(
                        egui::RichText::new("Win7原生不支持NVMe SSD，需要注入驱动才能识别硬盘")
                            .small()
                            .color(egui::Color32::GRAY),
                    );
                }
                
                // A5 蓝屏修复
                ui.checkbox(&mut self.win7_fix_acpi_bsod, "修复ACPI_BIOS_ERROR蓝屏(0xA5)");
                if self.win7_fix_acpi_bsod {
                    ui.label(
                        egui::RichText::new("禁用intelppm/amdppm服务，解决新平台ACPI兼容性问题")
                            .small()
                            .color(egui::Color32::GRAY),
                    );
                }
                
                // 7B 蓝屏修复 (存储控制器)
                ui.checkbox(&mut self.win7_fix_storage_bsod, "修复INACCESSIBLE_BOOT_DEVICE蓝屏(0x7B)");
                if self.win7_fix_storage_bsod {
                    ui.label(
                        egui::RichText::new(
                            "启用AHCI/IDE/NVMe/SCSI等存储控制器驱动，解决硬盘无法识别问题\n\
                             适用于：VMware NVMe、现代AHCI控制器、LSI SAS控制器等"
                        )
                            .small()
                            .color(egui::Color32::GRAY),
                    );
                }
                
                // Win7 UEFI 修补选项（仅在UEFI模式下显示）
                if is_uefi_mode {
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(5.0);
                    
                    ui.colored_label(
                        egui::Color32::from_rgb(100, 181, 246),
                        "🔧 UEFI 启动修补 (UefiSeven)",
                    );
                    ui.add_space(5.0);
                    
                    ui.checkbox(&mut self.win7_uefi_patch, "应用Win7 UEFI启动修补");
                    
                    ui.label(
                        egui::RichText::new(
                            "使用开源项目 UefiSeven 修补 Win7 UEFI 启动问题。\n\
                             Win7 的引导程序不完全支持 UEFI Class 3 系统，可能导致：\n\
                             • 启动时卡在 \"Starting Windows\" 界面\n\
                             • 出现错误代码 0xc000000d\n\
                             此选项会在安装完成后自动部署 UefiSeven 引导加载器。"
                        )
                        .small()
                        .color(egui::Color32::GRAY),
                    );
                    
                    // 检查 UefiSeven 文件是否存在
                    let uefiseven_dir = Self::get_uefiseven_dir();
                    if let Some(dir) = &uefiseven_dir {
                        if !dir.exists() {
                            ui.add_space(3.0);
                            ui.colored_label(
                                egui::Color32::from_rgb(255, 165, 0),
                                "⚠ 未找到 UefiSeven 文件，请将 UefiSeven 文件放置在程序目录的 uefiseven 文件夹中",
                            );
                        }
                    }
                }
                
                ui.add_space(15.0);
            }
            
            ui.heading("系统优化选项");
            ui.separator();

            ui.checkbox(&mut self.remove_shortcut_arrow, "移除快捷方式小箭头");
            ui.checkbox(&mut self.restore_classic_context_menu, "Win11恢复经典右键菜单");
            
            // OOBE绕过强制联网 - 依赖无人值守
            Self::show_unattend_dependent_checkbox(
                ui, 
                &mut self.bypass_nro, 
                "OOBE绕过强制联网",
                unattend_disabled,
                "此选项依赖无人值守配置，由于目标分区已存在配置文件而被禁用"
            );
            
            ui.checkbox(&mut self.disable_windows_update, "禁用Windows更新");
            ui.checkbox(&mut self.disable_windows_defender, "禁用Windows安全中心");
            ui.checkbox(&mut self.disable_reserved_storage, "禁用系统保留空间");
            ui.checkbox(&mut self.disable_uac, "禁用用户账户控制(UAC)");
            ui.checkbox(&mut self.disable_device_encryption, "禁用自动设备加密");
            
            // 删除预装UWP应用 - 依赖无人值守
            Self::show_unattend_dependent_checkbox(
                ui, 
                &mut self.remove_uwp_apps, 
                "删除预装UWP应用",
                unattend_disabled,
                "此选项依赖无人值守配置，由于目标分区已存在配置文件而被禁用"
            );

            ui.add_space(15.0);
            ui.heading("自定义脚本");
            ui.separator();

            ui.horizontal(|ui| {
                ui.checkbox(&mut self.run_script_during_deploy, "系统部署中运行脚本");
                if self.run_script_during_deploy {
                    ui.text_edit_singleline(&mut self.deploy_script_path);
                    if ui.button("浏览...").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("批处理文件", &["bat", "cmd"])
                            .pick_file()
                        {
                            self.deploy_script_path = path.to_string_lossy().to_string();
                        }
                    }
                }
            });

            ui.horizontal(|ui| {
                ui.checkbox(&mut self.run_script_first_login, "首次登录运行脚本");
                if self.run_script_first_login {
                    ui.text_edit_singleline(&mut self.first_login_script_path);
                    if ui.button("浏览...").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("批处理文件", &["bat", "cmd"])
                            .pick_file()
                        {
                            self.first_login_script_path = path.to_string_lossy().to_string();
                        }
                    }
                }
            });

            ui.add_space(15.0);
            ui.heading("自定义内容");
            ui.separator();

            ui.horizontal(|ui| {
                ui.checkbox(&mut self.import_custom_drivers, "导入自定义驱动");
                if self.import_custom_drivers {
                    ui.text_edit_singleline(&mut self.custom_drivers_path);
                    if ui.button("浏览...").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.custom_drivers_path = path.to_string_lossy().to_string();
                        }
                    }
                }
            });

            ui.horizontal(|ui| {
                ui.checkbox(
                    &mut self.import_storage_controller_drivers,
                    "导入磁盘控制器驱动[Win11/Win10 X64]",
                );
            });
            ui.label(
                egui::RichText::new(
                    "导入 Win10/Win11 的英特尔 VMD / 苹果 SSD / Visior 硬盘控制器驱动，如已集成无需勾选",
                )
                .small(),
            );

            ui.horizontal(|ui| {
                ui.checkbox(&mut self.import_registry_file, "导入注册表文件");
                if self.import_registry_file {
                    ui.text_edit_singleline(&mut self.registry_file_path);
                    if ui.button("浏览...").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("注册表文件", &["reg"])
                            .pick_file()
                        {
                            self.registry_file_path = path.to_string_lossy().to_string();
                        }
                    }
                }
            });

            ui.horizontal(|ui| {
                ui.checkbox(&mut self.import_custom_files, "导入自定义文件");
                if self.import_custom_files {
                    ui.text_edit_singleline(&mut self.custom_files_path);
                    if ui.button("浏览...").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.custom_files_path = path.to_string_lossy().to_string();
                        }
                    }
                }
            });

            ui.add_space(15.0);
            ui.heading("用户设置");
            ui.separator();

            ui.horizontal(|ui| {
                // 自定义用户名 - 依赖无人值守
                let was_enabled = self.custom_username;
                Self::show_unattend_dependent_checkbox(
                    ui,
                    &mut self.custom_username,
                    "自定义用户名",
                    unattend_disabled,
                    "此选项依赖无人值守配置，由于目标分区已存在配置文件而被禁用"
                );
                
                // 只有在启用且非禁用状态时才显示输入框
                if self.custom_username && !unattend_disabled {
                    ui.text_edit_singleline(&mut self.username);
                    let model_name = detect_computer_model_name(hardware_info);
                    let button = ui.add_enabled(
                        model_name.is_some(),
                        egui::Button::new("识别电脑型号"),
                    );
                    if button.clicked() {
                        if let Some(name) = model_name {
                            self.username = name;
                        }
                    }
                }
                
                // 如果因禁用而取消勾选，重置状态
                if was_enabled && unattend_disabled {
                    self.custom_username = false;
                }
            });

            ui.add_space(15.0);
            ui.heading("系统盘设置");
            ui.separator();

            ui.horizontal(|ui| {
                ui.checkbox(&mut self.custom_volume_label, "自定义系统盘卷标");
                if self.custom_volume_label {
                    ui.add(egui::TextEdit::singleline(&mut self.volume_label)
                        .desired_width(150.0)
                        .hint_text("例如: Windows"));
                }
            });
            if self.custom_volume_label {
                ui.label("提示: 卷标将在格式化分区时应用");
            }
        });
    }
}

use egui;

fn detect_computer_model_name(hardware_info: Option<&HardwareInfo>) -> Option<String> {
    let info = hardware_info?;
    let model_token = extract_primary_token(&info.computer_model);
    let manufacturer_token = extract_primary_token(&info.computer_manufacturer);

    match (model_token, manufacturer_token) {
        (Some(model), Some(manufacturer)) => {
            if model.len() <= manufacturer.len() {
                Some(model)
            } else {
                Some(manufacturer)
            }
        }
        (Some(model), None) => Some(model),
        (None, Some(manufacturer)) => Some(manufacturer),
        (None, None) => None,
    }
}

fn extract_primary_token(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let token = trimmed
        .split(|c: char| {
            c.is_whitespace() || matches!(c, '_' | '-' | ',' | ';' | '/' | '\\')
        })
        .find(|part| !part.is_empty())?;
    let token = token.trim_matches(|c: char| c.is_ascii_punctuation());
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}
