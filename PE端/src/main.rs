#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod core;
mod ui;
mod utils;

use eframe::egui;

fn main() -> eframe::Result<()> {
    // 初始化日志
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(Some(env_logger::TimestampPrecision::Millis))
        .init();

    log::info!("LetRecovery PE 启动中...");

    // 检查命令行参数
    let args: Vec<String> = std::env::args().collect();

    // 命令行模式（无GUI）
    if args.contains(&"/PEINSTALL".to_string()) || args.contains(&"--pe-install".to_string()) {
        log::info!("检测到PE安装模式（命令行），执行自动安装...");
        return run_cli_mode(true);
    }

    if args.contains(&"/PEBACKUP".to_string()) || args.contains(&"--pe-backup".to_string()) {
        log::info!("检测到PE备份模式（命令行），执行自动备份...");
        return run_cli_mode(false);
    }

    // 自动检测模式
    if args.contains(&"/AUTO".to_string()) || args.contains(&"--auto".to_string()) {
        log::info!("检测到自动模式，检测操作类型...");

        use core::config::{ConfigFileManager, OperationType};

        match ConfigFileManager::detect_operation_type() {
            Some(OperationType::Install) => {
                log::info!("检测到安装配置，启动GUI安装界面...");
            }
            Some(OperationType::Backup) => {
                log::info!("检测到备份配置，启动GUI备份界面...");
            }
            None => {
                log::warn!("未检测到配置文件，启动默认界面...");
                show_error_message("未检测到安装或备份配置文件。\n\n请确保已正确准备配置文件后重试。");
                return Ok(());
            }
        }
    }

    log::info!("初始化 GUI...");

    // 加载图标
    let icon = load_icon();

    // 设置窗口选项 - 窗口不可关闭，不可调整大小
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([600.0, 500.0])
            .with_min_inner_size([600.0, 500.0])
            .with_max_inner_size([600.0, 500.0])
            .with_resizable(false)
            .with_maximize_button(false)
            .with_minimize_button(false)
            .with_close_button(false)
            .with_icon(icon),
        ..Default::default()
    };

    // 运行应用
    eframe::run_native(
        "LetRecovery PE",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}

/// 加载图标
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

/// 命令行模式执行
fn run_cli_mode(is_install: bool) -> eframe::Result<()> {
    use core::bcdedit::BootManager;
    use core::config::ConfigFileManager;
    use core::dism::Dism;
    use core::disk::DiskManager;
    use core::ghost::Ghost;
    use ui::advanced_options::apply_advanced_options;

    if is_install {
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
        let target_partition = ConfigFileManager::find_install_marker_partition()
            .unwrap_or_else(|| config.target_partition.clone());

        // 构建完整镜像路径
        let data_dir = ConfigFileManager::get_data_dir(&data_partition);
        let image_path = format!("{}\\{}", data_dir, config.image_path);

        if !std::path::Path::new(&image_path).exists() {
            eprintln!("[PE INSTALL] 错误: 镜像文件不存在: {}", image_path);
            show_error_message(&format!("镜像文件不存在: {}", image_path));
            return Ok(());
        }

        println!("[PE INSTALL] 完整镜像路径: {}", image_path);

        // Step 1: 格式化分区
        println!("[PE INSTALL] Step 1: 格式化分区");
        if let Err(e) = DiskManager::format_partition(&target_partition) {
            eprintln!("[PE INSTALL] 格式化失败: {}", e);
            show_error_message(&format!("格式化分区失败: {}", e));
            return Ok(());
        }

        // Step 2: 释放镜像
        println!("[PE INSTALL] Step 2: 释放镜像");
        let apply_dir = format!("{}\\", target_partition);

        let apply_result = if config.is_gho {
            let ghost = Ghost::new();
            if !ghost.is_available() {
                show_error_message("Ghost工具不可用");
                return Ok(());
            }
            let partitions = DiskManager::get_partitions().unwrap_or_default();
            ghost.restore_image_to_letter(&image_path, &target_partition, &partitions, None)
        } else {
            let dism = Dism::new();
            dism.apply_image(&image_path, &apply_dir, config.volume_index, None)
        };

        if let Err(e) = apply_result {
            eprintln!("[PE INSTALL] 释放镜像失败: {}", e);
            show_error_message(&format!("释放镜像失败: {}", e));
            return Ok(());
        }

        // Step 3: 导入驱动
        println!("[PE INSTALL] Step 3: 导入驱动");
        if config.restore_drivers {
            let driver_path = format!("{}\\drivers", data_dir);
            if std::path::Path::new(&driver_path).exists() {
                let dism = Dism::new();
                let _ = dism.add_drivers_offline(&apply_dir, &driver_path);
            }
        }

        // Step 4: 修复引导
        println!("[PE INSTALL] Step 4: 修复引导");
        let boot_manager = BootManager::new();
        let use_uefi = DiskManager::detect_uefi_mode();

        if let Err(e) = boot_manager.repair_boot_advanced(&target_partition, use_uefi) {
            eprintln!("[PE INSTALL] 修复引导失败: {}", e);
            show_error_message(&format!("修复引导失败: {}", e));
            return Ok(());
        }

        // Step 5: 应用高级选项
        println!("[PE INSTALL] Step 5: 应用高级选项");
        let _ = apply_advanced_options(&target_partition, &config);

        // Step 6: 生成无人值守配置
        if config.unattended {
            println!("[PE INSTALL] Step 6: 生成无人值守配置");
            let _ = generate_unattend_xml(&target_partition, &config.custom_username);
        }

        // Step 7: 清理
        println!("[PE INSTALL] Step 7: 清理临时文件");
        ConfigFileManager::cleanup_all(&data_partition, &target_partition);

        // Step 8: 清理自动创建的数据分区并扩展目标分区
        println!("[PE INSTALL] Step 8: 清理自动创建的分区");
        match DiskManager::cleanup_auto_created_partition_and_extend(&target_partition) {
            Ok(_) => println!("[PE INSTALL] 自动创建分区清理完成"),
            Err(e) => {
                // 不中断安装流程，只记录警告
                eprintln!("[PE INSTALL] 警告: 清理自动创建分区失败: {}", e);
                log::warn!("清理自动创建分区失败: {}", e);
            }
        }

        println!("[PE INSTALL] 安装完成!");

        if config.auto_reboot {
            println!("[PE INSTALL] 即将重启...");
            let _ = utils::command::new_command("shutdown")
                .args(["/r", "/t", "10", "/c", "LetRecovery 系统安装完成，即将重启..."])
                .spawn();
        } else {
            show_success_message("系统安装完成！请手动重启计算机。");
        }
    } else {
        // 备份模式
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
        let source_partition = ConfigFileManager::find_backup_marker_partition()
            .unwrap_or_else(|| config.source_partition.clone());

        // 执行备份
        let dism = Dism::new();
        let capture_dir = format!("{}\\", source_partition);

        let backup_result =
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
            };

        if let Err(e) = backup_result {
            eprintln!("[PE BACKUP] 备份失败: {}", e);
            show_error_message(&format!("系统备份失败: {}", e));
            return Ok(());
        }

        // 删除PE引导项
        let boot_manager = BootManager::new();
        let _ = boot_manager.delete_current_boot_entry();

        // 清理
        ConfigFileManager::cleanup_partition_markers(&source_partition);
        ConfigFileManager::cleanup_data_dir(&data_partition);
        ConfigFileManager::cleanup_pe_dir(&data_partition);

        println!("[PE BACKUP] 备份完成!");
        show_success_message(&format!(
            "系统备份完成！\n保存位置: {}",
            config.save_path
        ));

        // 自动重启
        let _ = utils::command::new_command("shutdown")
            .args([
                "/r",
                "/t",
                "10",
                "/c",
                "LetRecovery 系统备份完成，即将重启...",
            ])
            .spawn();
    }

    Ok(())
}

/// 生成无人值守XML
fn generate_unattend_xml(target_partition: &str, username: &str) -> anyhow::Result<()> {
    let username = if username.is_empty() { "User" } else { username };

    let xml_content = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
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
</unattend>"#,
        username, username, username
    );

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
        let wide_title: Vec<u16> = OsStr::new("LetRecovery PE 错误")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            #[link(name = "user32")]
            extern "system" {
                fn MessageBoxW(
                    hwnd: *mut std::ffi::c_void,
                    text: *const u16,
                    caption: *const u16,
                    utype: u32,
                ) -> i32;
            }
            MessageBoxW(
                null_mut(),
                wide_message.as_ptr(),
                wide_title.as_ptr(),
                0x10,
            ); // MB_ICONERROR
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
        let wide_title: Vec<u16> = OsStr::new("LetRecovery PE")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            #[link(name = "user32")]
            extern "system" {
                fn MessageBoxW(
                    hwnd: *mut std::ffi::c_void,
                    text: *const u16,
                    caption: *const u16,
                    utype: u32,
                ) -> i32;
            }
            MessageBoxW(
                null_mut(),
                wide_message.as_ptr(),
                wide_title.as_ptr(),
                0x40,
            ); // MB_ICONINFORMATION
        }
    }

    #[cfg(not(windows))]
    {
        println!("成功: {}", message);
    }
}
