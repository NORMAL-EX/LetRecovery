#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod core;
mod ui;
mod utils;

use eframe::egui;

/// 日志文件路径：优先 exe 同目录；取不到则退回当前目录。
fn log_file_path() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("LetRecoveryPE.log")))
        .unwrap_or_else(|| std::path::PathBuf::from("LetRecoveryPE.log"))
}

/// 文件日志器：每条日志**立即 flush 落盘**。
/// 之前用 env_logger 的 file pipe，GUI 进程长期不退出导致缓冲日志不落盘，
/// 安装流程的日志全丢失、无法排查；这里改为自实现、每条 flush。
struct FileLogger {
    file: std::sync::Mutex<std::fs::File>,
    level: log::LevelFilter,
}

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");
        if let Ok(mut f) = self.file.lock() {
            use std::io::Write;
            let _ = writeln!(
                f,
                "[{}] {} {} {}",
                ts,
                record.level(),
                record.target(),
                record.args()
            );
            let _ = f.flush(); // 关键：每条立即落盘，GUI 运行中也能实时看到
        }
    }

    fn flush(&self) {
        if let Ok(mut f) = self.file.lock() {
            use std::io::Write;
            let _ = f.flush();
        }
    }
}

/// 初始化日志：自实现的文件日志器（每条 flush）。文件打不开时静默跳过，不影响启动。
fn init_file_logger() {
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file_path())
    {
        Ok(f) => f,
        Err(_) => return,
    };
    let logger = Box::new(FileLogger {
        file: std::sync::Mutex::new(file),
        level: log::LevelFilter::Info,
    });
    if log::set_boxed_logger(logger).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}

/// 安装 panic 钩子，把线程 panic 的位置与信息写入日志（再调用默认钩子）。
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "未知位置".to_string());
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<非字符串 panic>".to_string());
        log::error!("[PANIC] 线程崩溃 @ {} : {}", location, msg);
        default_hook(info);
    }));
}

fn main() -> eframe::Result<()> {
    // 初始化日志：写入到 exe 同目录的 LetRecoveryPE.log。
    // PE 下 GUI 程序没有控制台，stderr 会被直接丢弃，必须落盘才能事后排查“怎么死的”。
    init_file_logger();
    // 安装 panic 钩子：安装流程跑在工作线程里，线程 panic 会“静默死亡”导致界面卡住，
    // 必须把 panic 记到日志。
    install_panic_hook();

    log::info!("==================== LetRecovery PE 启动 ====================");
    log::info!("版本: {} | 日志文件: {}", env!("CARGO_PKG_VERSION"), log_file_path().display());

    // 检查命令行参数
    let args: Vec<String> = std::env::args().collect();
    log::info!("命令行参数: {:?}", args);

    // 【关键】BitLocker 密钥透传解锁必须在**任何**操作类型检测之前执行。
    // 安装标记文件(LetRecovery_Install.marker)位于目标系统卷上，若该卷被 BitLocker 加密，
    // 则 PE 启动后它处于锁定状态，detect_operation_type()/find_install_marker_partition()
    // 会读不到标记 → 返回 None → GUI 安装流程(execute_install_workflow)根本不会启动，
    // 而解锁逻辑原先恰好埋在 execute_install_workflow 里，形成“要解锁才能检测、要检测才会解锁”
    // 的死锁。这里提前到 main 最前面统一解锁，GUI/自动/命令行所有模式都覆盖，
    // 且无论是否加密都会在日志里留下解锁尝试记录。无密钥文件=未启用=安全空操作。
    unlock_bitlocker_passthrough();

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

/// 【实验性】BitLocker 密钥透传解锁。
///
/// 若正常系统端在注入引导时把恢复密钥文件打包进了 boot.wim，则 PE 启动后该文件位于
/// `X:\LR_BitLockerKeys.txt`。读取其中的恢复密钥，对 A–Z 各盘逐一尝试解锁。
///
/// 解锁优先用 fveapi，失败再回退 `manage-bde -unlock`（精简 WinPE 可能缺其一）。
/// 全程写**日志**（GUI 无控制台，必须落盘到 LetRecoveryPE.log 才能排查），失败原因也记录。
/// best-effort：无文件/无锁定卷/解锁失败都不致命。
fn unlock_bitlocker_passthrough() {
    let keys_path = format!("X:\\{}", lr_core::bl_passthrough::KEYS_FILE_NAME);
    let content = match std::fs::read_to_string(&keys_path) {
        Ok(c) => c,
        Err(_) => {
            log::info!("[实验] 未发现密钥透传文件 {}，跳过解锁（未启用透传/无加密卷）", keys_path);
            return;
        }
    };
    let keys = lr_core::bl_passthrough::parse_keys(&content);
    if keys.is_empty() {
        log::warn!("[实验] 密钥透传文件存在但未解析出任何恢复密钥: {}", keys_path);
        return;
    }
    let fveapi_ok = lr_core::fveapi::FveApi::instance().is_ok();
    log::info!(
        "[实验] BitLocker 密钥透传：解析到 {} 个恢复密钥，fveapi.dll={}，开始逐盘尝试解锁…",
        keys.len(),
        if fveapi_ok { "可用(优先)" } else { "不可用(仅用 manage-bde)" }
    );

    let mut any_unlocked = false;
    for byte in b'A'..=b'Z' {
        let letter = byte as char;
        if letter == 'X' {
            continue; // 跳过 PE 系统盘
        }
        let drive = format!("{}:", letter);
        for (i, key) in keys.iter().enumerate() {
            if try_unlock_fveapi(letter, key) {
                log::info!("[实验] {} 经 fveapi 用第 {} 个恢复密钥解锁成功", drive, i + 1);
                any_unlocked = true;
                break;
            }
            if try_unlock_manage_bde(&drive, key) {
                log::info!("[实验] {} 经 manage-bde 用第 {} 个恢复密钥解锁成功", drive, i + 1);
                any_unlocked = true;
                break;
            }
        }
    }
    if !any_unlocked {
        log::warn!("[实验] 未解锁任何卷（若有锁定卷，请看上方各盘的 fveapi/manage-bde 失败原因）");
    }
    log::info!("[实验] BitLocker 密钥透传解锁流程结束");
}

/// 用 fveapi 对单个卷尝试恢复密钥解锁。返回是否成功；失败原因写日志。
fn try_unlock_fveapi(drive_letter: char, recovery_key: &str) -> bool {
    let api = match lr_core::fveapi::FveApi::instance() {
        Ok(a) => a,
        Err(_) => return false, // fveapi.dll 不可用（上层已记录一次）
    };
    let formatted = match lr_core::fveapi::format_recovery_key(recovery_key) {
        Ok(f) => f,
        Err(e) => {
            log::warn!("[实验] 恢复密钥格式化失败: {}", e);
            return false;
        }
    };
    let path = format!("{}:", drive_letter);
    match api.open_volume(&path) {
        Ok(handle) => match handle.unlock_with_recovery_key(&formatted) {
            Ok(_) => true,
            Err(e) => {
                // 开卷成功（通常即加密卷，含锁定卷）但本密钥解锁失败：记录具体错误便于定位
                log::info!("[实验] {} fveapi 解锁失败: {:?}", path, e);
                false
            }
        },
        Err(e) => {
            // 非 BitLocker / 未加密卷会在此返回，属正常，debug 级
            log::debug!("[实验] {} fveapi 开卷失败/非加密: {:?}", path, e);
            false
        }
    }
}

/// 回退：manage-bde 解锁（WinPE 可能未含该工具）。失败原因写日志。
fn try_unlock_manage_bde(drive: &str, recovery_key: &str) -> bool {
    match std::process::Command::new("manage-bde")
        .args(["-unlock", drive, "-RecoveryPassword", recovery_key])
        .output()
    {
        Ok(o) => {
            if o.status.success() {
                true
            } else {
                let out = String::from_utf8_lossy(&o.stdout);
                let err = String::from_utf8_lossy(&o.stderr);
                log::debug!(
                    "[实验] {} manage-bde 解锁未成功: {} {}",
                    drive,
                    out.trim(),
                    err.trim()
                );
                false
            }
        }
        Err(e) => {
            log::debug!("[实验] {} manage-bde 不可用: {}", drive, e);
            false
        }
    }
}

/// 命令行模式执行
fn run_cli_mode(is_install: bool) -> eframe::Result<()> {
    use core::bcdedit::BootManager;
    use core::config::ConfigFileManager;
    use core::dism::Dism;
    use core::disk::DiskManager;
    use core::ghost::Ghost;
    use ui::advanced_options::apply_advanced_options;

    /// 递归查找目录中的所有 CAB 文件
    fn find_cab_files_in_dir(dir: &str) -> Vec<std::path::PathBuf> {
        fn find_recursive(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() {
                        if let Some(ext) = path.extension() {
                            if ext.to_string_lossy().to_lowercase() == "cab" {
                                files.push(path);
                            }
                        }
                    } else if path.is_dir() {
                        find_recursive(&path, files);
                    }
                }
            }
        }
        let mut files = Vec::new();
        find_recursive(std::path::Path::new(dir), &mut files);
        files
    }

    if is_install {
        println!("[PE INSTALL] ========== PE自动安装模式 ==========");
        // 注：BitLocker 透传解锁已在 main() 最前面统一执行，这里不再重复。

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

        // Step 0: 校验镜像完整性（WIM/ESD；GHO 跳过）——放在格式化之前，坏镜像不糟蹋目标盘
        if !config.is_gho {
            println!("[PE INSTALL] Step 0: 校验镜像完整性");
            log::info!("[PE安装/CLI] 开始校验镜像: {}", image_path);
            let dism = Dism::new();
            if let Err(e) = dism.verify_image(&image_path, None) {
                eprintln!("[PE INSTALL] 镜像校验失败: {}", e);
                log::error!("[PE安装/CLI] 镜像校验失败: {}", e);
                show_error_message(&format!(
                    "镜像校验失败：镜像可能已损坏或不完整（{}）。请重新获取镜像后重试。",
                    e
                ));
                return Ok(());
            }
            log::info!("[PE安装/CLI] 镜像校验通过");
        }

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
        let driver_path = format!("{}\\drivers", data_dir);
        let driver_path_exists = std::path::Path::new(&driver_path).exists();
        
        if config.should_import_drivers() && driver_path_exists {
            let dism = Dism::new();
            match dism.add_drivers_offline_with_progress(&apply_dir, &driver_path, None) {
                Ok(_) => println!("[PE INSTALL] 驱动导入成功"),
                Err(e) => {
                    eprintln!("[PE INSTALL] 警告: 驱动导入失败: {} (继续安装)", e);
                    log::warn!("驱动导入失败: {}", e);
                }
            }
            
            // 同时检查驱动目录中是否有 CAB 文件并安装
            let cab_files = find_cab_files_in_dir(&driver_path);
            if !cab_files.is_empty() {
                println!("[PE INSTALL] 在驱动目录中发现 {} 个 CAB 文件，一并安装", cab_files.len());
                match dism.add_packages_offline_from_dir(&apply_dir, &driver_path, None) {
                    Ok((success, fail)) => {
                        println!("[PE INSTALL] 驱动目录中的CAB安装完成: {} 成功, {} 失败", success, fail);
                    }
                    Err(e) => {
                        eprintln!("[PE INSTALL] 警告: 驱动目录中的CAB安装失败: {} (继续安装)", e);
                        log::warn!("驱动目录中的CAB安装失败: {}", e);
                    }
                }
            }
        } else if config.should_import_drivers() && !driver_path_exists {
            println!("[PE INSTALL] 驱动目录不存在，跳过驱动导入");
        } else {
            println!("[PE INSTALL] 跳过驱动导入");
        }

        // Step 4: 安装CAB更新包
        println!("[PE INSTALL] Step 4: 安装CAB更新包");
        if config.install_cab_packages {
            let cab_path = format!("{}\\updates", data_dir);
            if std::path::Path::new(&cab_path).exists() {
                let dism = Dism::new();
                match dism.add_packages_offline_from_dir(&apply_dir, &cab_path, None) {
                    Ok((success, fail)) => {
                        println!("[PE INSTALL] CAB更新包安装完成: {} 成功, {} 失败", success, fail);
                    }
                    Err(e) => {
                        eprintln!("[PE INSTALL] 警告: CAB更新包安装失败: {} (继续安装)", e);
                        log::warn!("CAB更新包安装失败: {}", e);
                    }
                }
            } else {
                println!("[PE INSTALL] 更新包目录不存在，跳过CAB安装");
            }
        } else {
            println!("[PE INSTALL] 跳过CAB更新包安装");
        }

        // Step 5: 修复引导
        println!("[PE INSTALL] Step 5: 修复引导");
        let boot_manager = BootManager::new();
        let use_uefi = DiskManager::detect_uefi_mode();

        if let Err(e) = boot_manager.repair_boot_advanced(&target_partition, use_uefi) {
            eprintln!("[PE INSTALL] 修复引导失败: {}", e);
            show_error_message(&format!("修复引导失败: {}", e));
            return Ok(());
        }

        // Step 5.5: 如果启用了 Win7 UEFI 补丁，应用 UefiSeven
        if use_uefi && config.win7_uefi_patch {
            println!("[PE INSTALL] Step 5.5: 应用 Win7 UEFI 补丁 (UefiSeven)");
            match ui::advanced_options::apply_uefiseven_patch(&data_partition, &target_partition) {
                Ok(_) => println!("[PE INSTALL] UefiSeven 补丁应用成功"),
                Err(e) => {
                    // UefiSeven 补丁失败不中断安装，只记录警告
                    eprintln!("[PE INSTALL] 警告: UefiSeven 补丁应用失败: {} (继续安装)", e);
                    log::warn!("UefiSeven 补丁应用失败: {}", e);
                }
            }
        }

        // Step 6: 应用高级选项
        println!("[PE INSTALL] Step 6: 应用高级选项");
        let _ = apply_advanced_options(&target_partition, &config);

        // Step 7: 生成无人值守配置
        if config.unattended {
            println!("[PE INSTALL] Step 7: 生成无人值守配置");
            if !config.custom_unattend_file.is_empty() {
                // 用户自定义无人值守文件：直接复制到目标系统
                let data_dir = ConfigFileManager::get_data_dir(&data_partition);
                let src = format!("{}\\{}", data_dir, config.custom_unattend_file);
                match std::fs::read(&src) {
                    Ok(content) => {
                        let panther_dir = format!("{}\\Windows\\Panther", target_partition);
                        let _ = std::fs::create_dir_all(&panther_dir);
                        let _ = std::fs::write(format!("{}\\unattend.xml", panther_dir), &content);
                        let sysprep_dir =
                            format!("{}\\Windows\\System32\\Sysprep", target_partition);
                        if std::path::Path::new(&sysprep_dir).exists() {
                            let _ = std::fs::write(format!("{}\\unattend.xml", sysprep_dir), &content);
                        }
                        println!("[PE INSTALL] 已应用自定义无人值守文件: {}", src);
                    }
                    Err(e) => eprintln!("[PE INSTALL] 读取自定义无人值守文件失败: {}", e),
                }
            } else {
                let _ = generate_unattend_xml(&target_partition, &config.custom_username);
            }
        }

        // Step 7.5: 离线登录兜底（放开空密码策略 + 已知用户名时配置自动登录）
        if let Err(e) =
            core::account_fix::ensure_offline_login(&target_partition, &config.custom_username)
        {
            eprintln!("[PE INSTALL] 离线登录兜底设置失败（不影响安装）: {}", e);
        } else {
            println!("[PE INSTALL] 已应用离线登录兜底设置");
        }

        // Step 8: 清理
        println!("[PE INSTALL] Step 8: 清理临时文件");
        ConfigFileManager::cleanup_all(&data_partition, &target_partition);

        // Step 9: 清理自动创建的数据分区并扩展目标分区
        println!("[PE INSTALL] Step 9: 清理自动创建的分区");
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
