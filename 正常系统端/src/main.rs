#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(dead_code)]

mod app;
mod core;
mod download;
mod ui;
mod utils;

use eframe::egui;
use std::sync::Arc;

/// 预加载的配置数据
pub struct PreloadedConfig {
    pub remote_config: Option<download::server_config::RemoteConfig>,
    pub system_info: Option<core::system_info::SystemInfo>,
    pub hardware_info: Option<core::hardware_info::HardwareInfo>,
    pub partitions: Vec<core::disk::Partition>,
}

fn main() -> eframe::Result<()> {
    // 初始化日志
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(Some(env_logger::TimestampPrecision::Millis))
        .init();

    log::info!("LetRecovery 启动中...");

    // 检查命令行参数，处理PE环境下的自动安装/备份
    let args: Vec<String> = std::env::args().collect();
    
    if args.contains(&"/PEINSTALL".to_string()) || args.contains(&"--pe-install".to_string()) {
        log::info!("检测到PE安装模式，执行自动安装...");
        return run_pe_install();
    }
    
    if args.contains(&"/PEBACKUP".to_string()) || args.contains(&"--pe-backup".to_string()) {
        log::info!("检测到PE备份模式，执行自动备份...");
        return run_pe_backup();
    }

    // 检查管理员权限
    if !utils::privilege::is_admin() {
        log::warn!("需要管理员权限，正在尝试提升权限...");
        if let Err(e) = utils::privilege::restart_as_admin() {
            log::error!("提升权限失败: {}", e);
            eprintln!("需要管理员权限运行此程序");
        }
        return Ok(());
    }

    log::info!("已获得管理员权限");

    // 检查是否为64位系统
    if !cfg!(target_arch = "x86_64") {
        log::error!("本程序仅支持64位系统");
        eprintln!("本程序仅支持64位系统");
        return Ok(());
    }

    // 检查依赖文件完整性
    if let Err(missing_files) = check_dependencies() {
        log::error!("依赖文件缺失: {:?}", missing_files);
        let message = format!(
            "程序文件不完整，无法正常运行。\n\n\
            缺少以下文件：\n{}\n\n\
            请重新下载完整安装包或修复程序文件。",
            missing_files.join("\n")
        );
        show_error_message(&message);
        return Ok(());
    }

    log::info!("依赖文件检查通过");

    // 检查系统核心组件（极限精简系统检测）
    if let Err(missing_components) = check_system_components() {
        log::error!("系统组件缺失: {:?}", missing_components);
        let message = format!(
            "很抱歉，该软件目前暂时不支持您所使用的极限精简系统使用。\n\n\
            缺少以下系统组件：\n{}",
            missing_components.join("\n")
        );
        show_error_message(&message);
        return Ok(());
    }

    log::info!("系统组件检查通过");

    // 防止重复运行
    let _mutex = match single_instance::SingleInstance::new("LetRecovery-mutex-2025") {
        Ok(m) => {
            if !m.is_single() {
                log::warn!("程序已在运行中");
                return Ok(());
            }
            m
        }
        Err(e) => {
            log::error!("创建互斥锁失败: {}", e);
            return Ok(());
        }
    };

    log::info!("正在预加载配置和系统信息...");

    // 在显示窗口前先加载服务器配置和系统信息
    let preloaded_config = preload_all_config();
    let preloaded_config = Arc::new(preloaded_config);

    log::info!("预加载完成，初始化 GUI...");

    // 加载图标
    let icon = load_icon();

    // 设置窗口选项
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([950.0, 680.0])
            .with_min_inner_size([800.0, 600.0])
            .with_icon(icon),
        ..Default::default()
    };

    // 运行应用，传入预加载的配置
    let config_clone = preloaded_config.clone();
    eframe::run_native(
        "LetRecovery - Windows系统一键重装工具",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new_with_preloaded(cc, &config_clone)))),
    )
}

/// 预加载所有配置和系统信息
fn preload_all_config() -> PreloadedConfig {
    // 并行加载各种信息以加快启动速度
    let remote_config_handle = std::thread::spawn(|| {
        log::info!("开始加载远程配置...");
        let config = download::server_config::RemoteConfig::load_from_server();
        log::info!("远程配置加载完成: loaded={}", config.loaded);
        config
    });

    let system_info_handle = std::thread::spawn(|| {
        log::info!("开始收集系统信息...");
        let info = core::system_info::SystemInfo::collect().ok();
        log::info!("系统信息收集完成");
        info
    });

    let hardware_info_handle = std::thread::spawn(|| {
        log::info!("开始收集硬件信息...");
        let info = core::hardware_info::HardwareInfo::collect().ok();
        log::info!("硬件信息收集完成");
        info
    });

    let partitions_handle = std::thread::spawn(|| {
        log::info!("开始获取分区信息...");
        let partitions = core::disk::DiskManager::get_partitions().unwrap_or_default();
        log::info!("分区信息获取完成: {} 个分区", partitions.len());
        partitions
    });

    // 等待所有线程完成
    let remote_config = remote_config_handle.join().ok();
    let system_info = system_info_handle.join().ok().flatten();
    let hardware_info = hardware_info_handle.join().ok().flatten();
    let partitions = partitions_handle.join().ok().unwrap_or_default();

    PreloadedConfig {
        remote_config,
        system_info,
        hardware_info,
        partitions,
    }
}

fn load_icon() -> egui::IconData {
    // 使用内嵌的图标数据（编译时嵌入）
    const ICON_BYTES: &[u8] = include_bytes!("../assets/icon.png");
    
    // 从内嵌的PNG数据加载图标
    if let Ok(image) = image::load_from_memory(ICON_BYTES) {
        let image = image.to_rgba8();
        let (width, height) = image.dimensions();
        return egui::IconData {
            rgba: image.into_raw(),
            width,
            height,
        };
    }

    // 如果解析失败，返回默认图标
    egui::IconData::default()
}

/// 检查程序依赖文件完整性
/// 返回 Ok(()) 表示所有文件存在，Err(Vec<String>) 包含缺失的文件列表
fn check_dependencies() -> Result<(), Vec<String>> {
    let exe_dir = utils::path::get_exe_dir();
    
    // 必需的依赖文件列表
    let required_files = [
        // bin 目录 - 核心工具
        "bin/bcdedit.exe",
        "bin/bcdboot.exe",
        "bin/bootsect.exe",
        "bin/format.com",
        "bin/aria2c.exe",
        "bin/ghost/ghost64.exe",
    ];
    
    let mut missing_files = Vec::new();
    
    for file in &required_files {
        let file_path = exe_dir.join(file);
        if !file_path.exists() {
            log::warn!("依赖文件缺失: {}", file);
            missing_files.push(file.to_string());
        }
    }
    
    if missing_files.is_empty() {
        Ok(())
    } else {
        Err(missing_files)
    }
}

/// 检查系统核心组件完整性（用于检测极限精简系统）
/// 返回 Ok(()) 表示所有组件存在，Err(Vec<String>) 包含缺失的组件列表
fn check_system_components() -> Result<(), Vec<String>> {
    // 获取系统盘路径 (通过 SYSTEMROOT 环境变量，通常为 C:\Windows)
    let system_root = std::env::var("SYSTEMROOT")
        .or_else(|_| std::env::var("WINDIR"))
        .unwrap_or_else(|_| "C:\\Windows".to_string());
    
    let system32_path = std::path::Path::new(&system_root).join("System32");
    
    // 必需的系统组件列表
    let required_components = [
        ("diskpart.exe", "磁盘分区工具"),
        ("wimgapi.dll", "WIM 镜像处理库"),
        ("advapi32.dll", "高级 Windows API 库"),
    ];
    
    let mut missing_components = Vec::new();
    
    for (file, description) in &required_components {
        let file_path = system32_path.join(file);
        if !file_path.exists() {
            log::warn!("系统组件缺失: {} ({})", file, description);
            missing_components.push(format!("{} - {}", file, description));
        }
    }
    
    if missing_components.is_empty() {
        Ok(())
    } else {
        Err(missing_components)
    }
}

/// PE环境下自动执行安装
fn run_pe_install() -> eframe::Result<()> {
    use core::install_config::ConfigFileManager;
    
    println!("[PE INSTALL] ========== PE自动安装模式 ==========");
    
    // 查找配置文件所在分区
    let data_partition = match ConfigFileManager::find_data_partition() {
        Some(p) => p,
        None => {
            eprintln!("[PE INSTALL] 错误: 未找到安装配置文件");
            show_error_message("未找到安装配置文件，无法继续安装。");
            return Ok(());
        }
    };
    
    println!("[PE INSTALL] 数据分区: {}", data_partition);
    
    // 读取安装配置
    let config = match ConfigFileManager::read_install_config(&data_partition) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[PE INSTALL] 错误: 读取配置失败: {}", e);
            show_error_message(&format!("读取安装配置失败: {}", e));
            return Ok(());
        }
    };
    
    println!("[PE INSTALL] 目标分区: {}", config.target_partition);
    println!("[PE INSTALL] 镜像文件: {}", config.image_path);
    
    // 查找安装标记分区
    let target_partition = match ConfigFileManager::find_install_marker_partition() {
        Some(p) => p,
        None => config.target_partition.clone(),
    };
    
    // 构建完整镜像路径
    let data_dir = ConfigFileManager::get_data_dir(&data_partition);
    let image_path = format!("{}\\{}", data_dir, config.image_path);
    
    if !std::path::Path::new(&image_path).exists() {
        eprintln!("[PE INSTALL] 错误: 镜像文件不存在: {}", image_path);
        show_error_message(&format!("镜像文件不存在: {}", image_path));
        return Ok(());
    }
    
    println!("[PE INSTALL] 完整镜像路径: {}", image_path);
    
    // 执行安装
    let result = execute_pe_install(&target_partition, &image_path, &config, &data_dir);
    
    // 清理标记文件
    ConfigFileManager::cleanup_partition_markers(&target_partition);
    
    match result {
        Ok(_) => {
            println!("[PE INSTALL] 安装完成!");
            if config.auto_reboot {
                println!("[PE INSTALL] 即将重启...");
                let _ = utils::cmd::create_command("shutdown")
                    .args(["/r", "/t", "10", "/c", "LetRecovery 系统安装完成，即将重启..."])
                    .spawn();
            } else {
                show_success_message("系统安装完成！请手动重启计算机。");
            }
        }
        Err(e) => {
            eprintln!("[PE INSTALL] 安装失败: {}", e);
            show_error_message(&format!("系统安装失败: {}", e));
        }
    }
    
    Ok(())
}

/// PE环境下自动执行备份
fn run_pe_backup() -> eframe::Result<()> {
    use core::install_config::ConfigFileManager;
    
    println!("[PE BACKUP] ========== PE自动备份模式 ==========");
    
    // 查找配置文件所在分区
    let data_partition = match ConfigFileManager::find_data_partition() {
        Some(p) => p,
        None => {
            eprintln!("[PE BACKUP] 错误: 未找到备份配置文件");
            show_error_message("未找到备份配置文件，无法继续备份。");
            return Ok(());
        }
    };
    
    println!("[PE BACKUP] 数据分区: {}", data_partition);
    
    // 读取备份配置
    let config = match ConfigFileManager::read_backup_config(&data_partition) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[PE BACKUP] 错误: 读取配置失败: {}", e);
            show_error_message(&format!("读取备份配置失败: {}", e));
            return Ok(());
        }
    };
    
    println!("[PE BACKUP] 源分区: {}", config.source_partition);
    println!("[PE BACKUP] 保存路径: {}", config.save_path);
    
    // 查找备份标记分区
    let source_partition = match ConfigFileManager::find_backup_marker_partition() {
        Some(p) => p,
        None => config.source_partition.clone(),
    };
    
    // 执行备份
    let result = execute_pe_backup(&source_partition, &config);
    
    // 清理标记文件
    ConfigFileManager::cleanup_partition_markers(&source_partition);
    
    match result {
        Ok(_) => {
            println!("[PE BACKUP] 备份完成!");
            show_success_message(&format!("系统备份完成！\n保存位置: {}", config.save_path));
        }
        Err(e) => {
            eprintln!("[PE BACKUP] 备份失败: {}", e);
            show_error_message(&format!("系统备份失败: {}", e));
        }
    }
    
    Ok(())
}

/// 执行PE安装
fn execute_pe_install(
    target_partition: &str,
    image_path: &str,
    config: &core::install_config::InstallConfig,
    data_dir: &str,
) -> anyhow::Result<()> {
    use anyhow::Context;
    
    println!("[PE INSTALL] Step 1: 格式化分区");
    // 格式化目标分区
    let output = utils::cmd::create_command("cmd")
        .args(["/c", &format!("format {} /FS:NTFS /Q /Y", target_partition)])
        .output()
        .context("执行格式化命令失败")?;
    
    if !output.status.success() {
        let stderr = utils::encoding::gbk_to_utf8(&output.stderr);
        anyhow::bail!("格式化分区失败: {}", stderr);
    }
    
    println!("[PE INSTALL] Step 2: 释放镜像");
    // 释放镜像
    let apply_dir = format!("{}\\", target_partition);
    
    if config.is_gho {
        // GHO镜像使用Ghost
        let ghost = core::ghost::Ghost::new();
        if !ghost.is_available() {
            anyhow::bail!("Ghost工具不可用");
        }
        
        let partitions = core::disk::DiskManager::get_partitions().unwrap_or_default();
        ghost.restore_image_to_letter(image_path, target_partition, &partitions, None)?;
    } else {
        // WIM/ESD使用DISM
        let dism = core::dism::Dism::new();
        dism.apply_image(image_path, &apply_dir, config.volume_index, None)?;
    }
    
    println!("[PE INSTALL] Step 3: 导入驱动");
    // 导入驱动
    if config.restore_drivers {
        let driver_path = format!("{}\\drivers", data_dir);
        if std::path::Path::new(&driver_path).exists() {
            let dism = core::dism::Dism::new();
            let _ = dism.add_drivers_offline(&apply_dir, &driver_path);
        }
    }
    
    println!("[PE INSTALL] Step 4: 修复引导");
    // 修复引导
    let boot_manager = core::bcdedit::BootManager::new();
    let use_uefi = detect_uefi_mode();
    boot_manager.repair_boot_advanced(target_partition, use_uefi)?;
    
    println!("[PE INSTALL] Step 5: 应用高级选项");
    // 应用高级选项
    let mut advanced_options = ui::advanced_options::AdvancedOptions::default();
    advanced_options.remove_shortcut_arrow = config.remove_shortcut_arrow;
    advanced_options.restore_classic_context_menu = config.restore_classic_context_menu;
    advanced_options.bypass_nro = config.bypass_nro;
    advanced_options.disable_windows_update = config.disable_windows_update;
    advanced_options.disable_windows_defender = config.disable_windows_defender;
    advanced_options.disable_reserved_storage = config.disable_reserved_storage;
    advanced_options.disable_uac = config.disable_uac;
    advanced_options.disable_device_encryption = config.disable_device_encryption;
    advanced_options.remove_uwp_apps = config.remove_uwp_apps;
    advanced_options.import_storage_controller_drivers = config.import_storage_controller_drivers;
    advanced_options.custom_username = !config.custom_username.is_empty();
    advanced_options.username = config.custom_username.clone();
    
    let _ = advanced_options.apply_to_system(target_partition);
    
    // 生成无人值守配置
    if config.unattended {
        let _ = generate_unattend_xml_pe(target_partition, &config.custom_username);
    }
    
    println!("[PE INSTALL] Step 6: 清理临时文件");
    // 清理数据目录
    let _ = std::fs::remove_dir_all(data_dir);
    
    Ok(())
}

/// 执行PE备份
fn execute_pe_backup(
    source_partition: &str,
    config: &core::install_config::BackupConfig,
) -> anyhow::Result<()> {
    let dism = core::dism::Dism::new();
    let capture_dir = format!("{}\\", source_partition);
    
    if config.incremental && std::path::Path::new(&config.save_path).exists() {
        dism.append_image(
            &config.save_path,
            &capture_dir,
            &config.name,
            &config.description,
            None,
        )
    } else {
        dism.capture_image(
            &config.save_path,
            &capture_dir,
            &config.name,
            &config.description,
            None,
        )
    }
}

/// 检测UEFI模式（使用 Windows API）
fn detect_uefi_mode() -> bool {
    // 检查EFI系统分区
    for letter in ['S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z'] {
        let efi_path = format!("{}:\\EFI\\Microsoft\\Boot", letter);
        if std::path::Path::new(&efi_path).exists() {
            return true;
        }
    }
    
    // 使用 Windows API 检测固件类型
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetFirmwareEnvironmentVariableW(
                lpName: *const u16,
                lpGuid: *const u16,
                pBuffer: *mut u8,
                nSize: u32,
            ) -> u32;
        }

        unsafe {
            let name: Vec<u16> = "".encode_utf16().chain(std::iter::once(0)).collect();
            let guid: Vec<u16> = "{00000000-0000-0000-0000-000000000000}"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let mut buffer = [0u8; 1];

            let result = GetFirmwareEnvironmentVariableW(
                name.as_ptr(),
                guid.as_ptr(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
            );

            if result == 0 {
                let error = std::io::Error::last_os_error();
                let raw_error = error.raw_os_error().unwrap_or(0) as u32;
                
                // ERROR_INVALID_FUNCTION (1) 表示是 Legacy BIOS
                if raw_error == 1 {
                    return false;
                }
            }
            // 其他情况都认为是 UEFI
            return true;
        }
    }
    
    #[cfg(not(windows))]
    false
}

/// 生成无人值守XML (PE版本)
fn generate_unattend_xml_pe(target_partition: &str, username: &str) -> anyhow::Result<()> {
    let username = if username.is_empty() { "User" } else { username };
    
    let xml_content = format!(r#"<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
    <settings pass="windowsPE">
        <component name="Microsoft-Windows-Setup" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
            <UserData>
                <ProductKey>
                    <WillShowUI>OnError</WillShowUI>
                </ProductKey>
                <AcceptEula>true</AcceptEula>
            </UserData>
        </component>
    </settings>
    <settings pass="oobeSystem">
        <component name="Microsoft-Windows-Shell-Setup" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
            <OOBE>
                <HideEULAPage>true</HideEULAPage>
                <HideLocalAccountScreen>true</HideLocalAccountScreen>
                <HideOEMRegistrationScreen>true</HideOEMRegistrationScreen>
                <HideOnlineAccountScreens>true</HideOnlineAccountScreens>
                <HideWirelessSetupInOOBE>true</HideWirelessSetupInOOBE>
                <ProtectYourPC>3</ProtectYourPC>
                <SkipMachineOOBE>true</SkipMachineOOBE>
                <SkipUserOOBE>true</SkipUserOOBE>
            </OOBE>
            <UserAccounts>
                <LocalAccounts>
                    <LocalAccount wcm:action="add">
                        <Password>
                            <Value></Value>
                            <PlainText>true</PlainText>
                        </Password>
                        <Description>Local User</Description>
                        <DisplayName>{}</DisplayName>
                        <Group>Administrators</Group>
                        <Name>{}</Name>
                    </LocalAccount>
                </LocalAccounts>
            </UserAccounts>
            <AutoLogon>
                <Password>
                    <Value></Value>
                    <PlainText>true</PlainText>
                </Password>
                <Enabled>true</Enabled>
                <Username>{}</Username>
            </AutoLogon>
        </component>
    </settings>
</unattend>"#, username, username, username);

    let panther_dir = format!("{}\\Windows\\Panther", target_partition);
    std::fs::create_dir_all(&panther_dir)?;
    
    let unattend_path = format!("{}\\unattend.xml", panther_dir);
    std::fs::write(&unattend_path, &xml_content)?;
    
    Ok(())
}

/// 显示错误消息框
fn show_error_message(message: &str) {
    #[cfg(windows)]
    {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        use std::ptr::null_mut;
        
        let wide_message: Vec<u16> = OsStr::new(message)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let wide_title: Vec<u16> = OsStr::new("LetRecovery 错误")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        
        unsafe {
            #[link(name = "user32")]
            extern "system" {
                fn MessageBoxW(hwnd: *mut std::ffi::c_void, text: *const u16, caption: *const u16, utype: u32) -> i32;
            }
            MessageBoxW(null_mut(), wide_message.as_ptr(), wide_title.as_ptr(), 0x10); // MB_ICONERROR
        }
    }
    
    #[cfg(not(windows))]
    {
        eprintln!("错误: {}", message);
    }
}

/// 显示成功消息框
fn show_success_message(message: &str) {
    #[cfg(windows)]
    {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        use std::ptr::null_mut;
        
        let wide_message: Vec<u16> = OsStr::new(message)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let wide_title: Vec<u16> = OsStr::new("LetRecovery")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        
        unsafe {
            #[link(name = "user32")]
            extern "system" {
                fn MessageBoxW(hwnd: *mut std::ffi::c_void, text: *const u16, caption: *const u16, utype: u32) -> i32;
            }
            MessageBoxW(null_mut(), wide_message.as_ptr(), wide_title.as_ptr(), 0x40); // MB_ICONINFORMATION
        }
    }
    
    #[cfg(not(windows))]
    {
        println!("成功: {}", message);
    }
}
