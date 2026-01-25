use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::core::hardware_info::HardwareInfo;
use crate::core::registry::OfflineRegistry;

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
}

impl AdvancedOptions {
    /// 脚本目录名称（统一路径）
    const SCRIPTS_DIR: &'static str = "LetRecovery_Scripts";

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

    /// 显示高级选项界面
    pub fn show_ui(&mut self, ui: &mut egui::Ui, hardware_info: Option<&HardwareInfo>) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("系统优化选项");
            ui.separator();

            ui.checkbox(&mut self.remove_shortcut_arrow, "移除快捷方式小箭头");
            ui.checkbox(&mut self.restore_classic_context_menu, "Win11恢复经典右键菜单");
            ui.checkbox(&mut self.bypass_nro, "OOBE绕过强制联网");
            ui.checkbox(&mut self.disable_windows_update, "禁用Windows更新");
            ui.checkbox(&mut self.disable_windows_defender, "禁用Windows安全中心");
            ui.checkbox(&mut self.disable_reserved_storage, "禁用系统保留空间");
            ui.checkbox(&mut self.disable_uac, "禁用用户账户控制(UAC)");
            ui.checkbox(&mut self.disable_device_encryption, "禁用自动设备加密");
            ui.checkbox(&mut self.remove_uwp_apps, "删除预装UWP应用");

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
                ui.checkbox(&mut self.custom_username, "自定义用户名");
                if self.custom_username {
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
