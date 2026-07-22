#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod core;
#[cfg(target_os = "windows")]
pub mod native_ui;
mod ui;
mod utils;
mod workflow_journal;
mod workflows;

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

/// Loads a hardware-matched Intel VMD package into the running WinPE before any volume scan.
/// Drvload is the Microsoft-supported runtime path; this does not persist a driver in the PE WIM.
fn load_matching_vmd_driver_into_running_pe() {
    let hardware_ids = match lr_core::driver::list_present_hardware_ids() {
        Ok(hardware_ids) => hardware_ids,
        Err(error) => {
            log::warn!("[VMD/PE] present-device enumeration failed: {error}");
            return;
        }
    };
    let packages = lr_core::storage_driver_match::select_builtin_storage_driver_packages(
        hardware_ids.iter().map(String::as_str),
    );
    if packages.is_empty() {
        log::info!("[VMD/PE] no supported Intel VMD controller is present");
        return;
    }

    let package_root = utils::path::get_exe_dir()
        .join("drivers")
        .join("storage_controller");
    for package in packages {
        let inf = package_root
            .join(package.directory_name())
            .join("iaStorVD.inf");
        let is_regular_file = inf
            .symlink_metadata()
            .map(|metadata| metadata.file_type().is_file())
            .unwrap_or(false);
        if !is_regular_file {
            log::error!("[VMD/PE] matched VMD INF is unavailable: {}", inf.display());
            continue;
        }

        let request = lr_core::command::CommandRequest::new("drvload.exe").arg(&inf);
        match lr_core::command::execute_request(&lr_core::command::SystemCommandExecutor, &request)
        {
            Ok(outcome) if outcome.succeeded() => {
                log::info!("[VMD/PE] runtime VMD driver loaded: {}", inf.display());
            }
            Ok(outcome) => {
                log::error!(
                    "[VMD/PE] drvload rejected {} (exit {:?}): {}",
                    inf.display(),
                    outcome.exit_code(),
                    String::from_utf8_lossy(outcome.stderr()).trim()
                );
            }
            Err(error) => {
                log::error!(
                    "[VMD/PE] failed to start drvload for {}: {error}",
                    inf.display()
                );
            }
        }
    }
}

/// 探测界面语言：从（正常系统端随重启写入的）配置文件读取 Language 字段。
/// 找不到数据分区或配置时返回空串（即简体中文内置）。
fn detect_ui_language() -> String {
    use core::config::{ConfigFileManager, OperationType};

    let operation_type = match ConfigFileManager::detect_operation_type() {
        Some(operation) => operation,
        None => return String::new(),
    };
    let data_partition = match ConfigFileManager::find_data_partition_for(operation_type) {
        Some(p) => p,
        None => return String::new(),
    };

    match operation_type {
        OperationType::Install => ConfigFileManager::read_install_config(&data_partition)
            .map(|c| c.language)
            .unwrap_or_default(),
        OperationType::Backup => ConfigFileManager::read_backup_config(&data_partition)
            .map(|c| c.language)
            .unwrap_or_default(),
        OperationType::Expand => ConfigFileManager::read_expand_config(&data_partition)
            .map(|c| c.language)
            .unwrap_or_default(),
    }
}

fn main() -> anyhow::Result<()> {
    // 初始化日志：写入到 exe 同目录的 LetRecoveryPE.log。
    // PE 下 GUI 程序没有控制台，stderr 会被直接丢弃，必须落盘才能事后排查“怎么死的”。
    init_file_logger();
    // 安装 panic 钩子：安装流程跑在工作线程里，线程 panic 会“静默死亡”导致界面卡住，
    // 必须把 panic 记到日志。
    install_panic_hook();

    log::info!("==================== LetRecovery PE 启动 ====================");
    log::info!(
        "版本: {} | 日志文件: {}",
        env!("CARGO_PKG_VERSION"),
        log_file_path().display()
    );

    // Deterministic, side-effect-free visual entry for the native PE progress shell, including the
    // same elapsed-time loading ring and paint timer used by the production page. It must run before
    // driver loading, BitLocker passthrough and task discovery so desktop QA cannot touch the host
    // storage stack. Release builds do not contain this branch.
    let args: Vec<String> = std::env::args().collect();
    #[cfg(feature = "non-elevated-tests")]
    if args.iter().any(|arg| arg == "--ui-progress-preview-failed") {
        utils::i18n::init("");
        native_ui::progress::run_failed_preview(core::config::OperationType::Install)
            .map_err(anyhow::Error::new)?;
        return Ok(());
    }
    #[cfg(feature = "non-elevated-tests")]
    if args.iter().any(|arg| arg == "--ui-progress-preview") {
        utils::i18n::init("");
        native_ui::progress::run_preview(core::config::OperationType::Install)
            .map_err(anyhow::Error::new)?;
        return Ok(());
    }

    // VMD storage must be visible before BitLocker passthrough, marker discovery or any partition
    // inventory. A matched package is loaded into this booted PE only; offline Windows receives
    // the same package later through the signed DISM boundary.
    load_matching_vmd_driver_into_running_pe();

    // 检查命令行参数
    log::info!("命令行参数: {:?}", args);

    // 【关键】BitLocker 密钥透传解锁必须在**任何**操作类型检测之前执行。
    // 安装标记文件(LetRecovery_Install.marker)位于目标系统卷上，若该卷被 BitLocker 加密，
    // 则 PE 启动后它处于锁定状态，detect_operation_type()/find_install_marker_partition()
    // 会读不到标记 → 返回 None → GUI 安装流程(execute_install_workflow)根本不会启动，
    // 而解锁逻辑原先恰好埋在 execute_install_workflow 里，形成“要解锁才能检测、要检测才会解锁”
    // 的死锁。这里提前到 main 最前面统一解锁，GUI/自动/命令行所有模式都覆盖，
    // 且无论是否加密都会在日志里留下解锁尝试记录。无密钥文件=未启用=安全空操作。
    unlock_bitlocker_passthrough();

    // 初始化多语言：从配置文件（正常系统端随重启写入 Language=）读取界面语言；空=简体中文（内置）。
    // 必须在任何 GUI/CLI 分支之前，确保所有模式下文案都按所选语言显示。
    let ui_language = detect_ui_language();
    utils::i18n::init(&ui_language);
    log::info!(
        "界面语言: {}",
        if ui_language.is_empty() {
            "zh-CN (默认)"
        } else {
            ui_language.as_str()
        }
    );

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
            Some(OperationType::Expand) => {
                log::info!("检测到扩容配置，启动GUI扩容界面...");
            }
            None => {
                log::warn!("未检测到配置文件，启动默认界面...");
                show_error_message(&tr!(
                    "未检测到安装或备份配置文件。\n\n请确保已正确准备配置文件后重试。"
                ));
                return Ok(());
            }
        }
    }

    let Some(operation_type) = core::config::ConfigFileManager::detect_operation_type() else {
        log::warn!("PE 原生界面未检测到安装、备份或扩容任务");
        show_error_message(&tr!(
            "未检测到安装或备份配置文件。\n\n请确保已正确准备配置文件后重试。"
        ));
        return Ok(());
    };

    log::info!("进入 PE 原生 Win32 进度界面");
    if let Err(error) = native_ui::progress::run(operation_type) {
        log::error!("PE 原生 Win32 进度界面运行失败: {error}");
        show_error_message(&tr!("启动失败: {} - {}", "LetRecovery PE", error));
    }
    Ok(())
}

/// BitLocker 密钥透传解锁。
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
            log::info!(
                "[实验] 未发现密钥透传文件 {}，跳过解锁（未启用透传/无加密卷）",
                keys_path
            );
            return;
        }
    };
    let keys = lr_core::bl_passthrough::parse_keys(&content);
    if keys.is_empty() {
        log::warn!(
            "[实验] 密钥透传文件存在但未解析出任何恢复密钥: {}",
            keys_path
        );
        return;
    }
    let fveapi_ok = lr_core::fveapi::FveApi::instance().is_ok();
    log::info!(
        "[实验] BitLocker 密钥透传：解析到 {} 个恢复密钥，fveapi.dll={}，开始逐盘尝试解锁…",
        keys.len(),
        if fveapi_ok {
            "可用(优先)"
        } else {
            "不可用(仅用 manage-bde)"
        }
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
                log::info!(
                    "[实验] {} 经 fveapi 用第 {} 个恢复密钥解锁成功",
                    drive,
                    i + 1
                );
                any_unlocked = true;
                break;
            }
            if try_unlock_manage_bde(&drive, key) {
                log::info!(
                    "[实验] {} 经 manage-bde 用第 {} 个恢复密钥解锁成功",
                    drive,
                    i + 1
                );
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
fn run_cli_mode(is_install: bool) -> anyhow::Result<()> {
    use core::bcdedit::BootManager;
    use core::config::ConfigFileManager;
    use core::disk::DiskManager;
    use core::dism::Dism;
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
        log::info!("[PE INSTALL] ========== PE自动安装模式 ==========");
        // 注：BitLocker 透传解锁已在 main() 最前面统一执行，这里不再重复。

        // 查找配置文件所在分区
        let (data_partition, target_partition, config) =
            match ConfigFileManager::find_install_task() {
                Ok(task) => task,
                Err(e) => {
                    log::error!("[PE INSTALL] 错误: 读取安装任务失败: {}", e);
                    show_error_message(&tr!("读取安装任务失败: {}", e));
                    return Ok(());
                }
            };

        log::info!("[PE INSTALL] 数据分区: {}", data_partition);

        // 切换到正常系统端选定的镜像引擎（随重启传入）
        lr_core::set_active_engine(lr_core::WimEngine::from_u8(config.wim_engine));

        log::info!("[PE INSTALL] 目标分区: {}", config.target_partition);
        log::info!("[PE INSTALL] 镜像文件: {}", config.image_path);

        let data_dir = ConfigFileManager::get_data_dir(&data_partition);
        let resolved_source = if config.is_xp_i386 {
            ConfigFileManager::resolve_staged_xp_source(
                &data_dir,
                &config.image_path,
                &config.xp_source_arch,
            )
        } else {
            ConfigFileManager::resolve_staged_file(&data_dir, &config.image_path)
        };
        let image_path = match resolved_source {
            Ok(path) => path.to_string_lossy().into_owned(),
            Err(error) => {
                log::error!("[PE INSTALL] 错误: {error}");
                show_error_message(&tr!("安装配置中的镜像文件名无效: {}", error));
                return Ok(());
            }
        };

        if !std::path::Path::new(&image_path).exists() {
            log::error!("[PE INSTALL] 错误: 镜像文件不存在: {}", image_path);
            show_error_message(&tr!("镜像文件不存在: {}", image_path));
            return Ok(());
        }

        log::info!("[PE INSTALL] 完整镜像路径: {}", image_path);

        // Step 0: 校验镜像完整性（WIM/ESD；GHO 跳过）——放在格式化之前，坏镜像不糟蹋目标盘
        let xp_custom_sif = if config.is_xp_i386 && !config.custom_unattend_file.is_empty() {
            match ConfigFileManager::resolve_staged_file(&data_dir, &config.custom_unattend_file) {
                Ok(path) => Some(path),
                Err(error) => {
                    show_error_message(&tr!("自定义 XP 应答文件名无效: {}", error));
                    return Ok(());
                }
            }
        } else {
            None
        };
        if config.is_xp_i386 {
            if let Err(error) =
                lr_core::xp_i386::validate_i386_source(std::path::Path::new(&image_path))
            {
                show_error_message(&tr!("XP/2003 安装源校验失败: {}", error));
                return Ok(());
            }
        }

        if config.is_gho {
            let ghost = core::ghost::Ghost::new();
            if !ghost.is_available() {
                show_error_message(&tr!("Ghost工具不可用"));
                return Ok(());
            }
            if let Err(error) = ghost.verify_image_integrity(&image_path) {
                show_error_message(&tr!("GHO 镜像预检失败: {}", error));
                return Ok(());
            }
            log::info!("[PE安装/CLI] GHO 镜像预检通过，尚未修改目标分区");
        } else if !config.is_xp_i386 {
            log::info!("[PE INSTALL] Step 0: 校验镜像完整性");
            log::info!("[PE安装/CLI] 开始校验镜像: {}", image_path);
            let dism = Dism::new();
            if let Err(e) = dism.verify_image(&image_path, None) {
                log::error!("[PE INSTALL] 镜像校验失败: {}", e);
                log::error!("[PE安装/CLI] 镜像校验失败: {}", e);
                show_error_message(&tr!(
                    "镜像校验失败：镜像可能已损坏或不完整（{}）。请重新获取镜像后重试。",
                    e
                ));
                return Ok(());
            }
            log::info!("[PE安装/CLI] 镜像校验通过");
        }

        // Keep CLI installs on the same fail-closed path as the PE GUI. This
        // check is read-only and runs before formatting the selected volume.
        let staged_pca_compat =
            match core::pca_preflight::staged_config(&config, std::path::Path::new(&data_dir)) {
                Ok(staged) => staged,
                Err(error) => {
                    show_error_message(&error);
                    return Ok(());
                }
            };
        let pca_compat_package = if config.is_xp_i386 {
            None
        } else {
            match core::pca_preflight::verify_before_disk_write(
                &image_path,
                config.volume_index,
                config.is_gho,
                config.is_xp,
                config.boot_mode != 2,
                config.boot_pca_mode,
                staged_pca_compat.as_ref(),
            ) {
                Ok(package) => package,
                Err(error) => {
                    show_error_message(&error);
                    return Ok(());
                }
            }
        };

        if config.run_diskpart_scripts {
            log::info!("[PE INSTALL] Step 0.5: 运行 Diskpart 脚本");
            let scripts_dir = std::path::Path::new(&data_dir).join("diskpart");
            if let Err(error) = lr_core::diskpart::run_scripts_in_dir(&scripts_dir) {
                log::error!("[PE INSTALL] Diskpart 脚本执行失败: {error}");
                show_error_message(&tr!("Diskpart 脚本执行失败: {}", error));
                return Ok(());
            }
        }

        // Step 1: 格式化分区
        log::info!("[PE INSTALL] Step 1: 格式化分区");
        let volume_label =
            (!config.volume_label.is_empty()).then_some(config.volume_label.as_str());
        if let Err(e) = DiskManager::format_partition_with_label(&target_partition, volume_label) {
            log::error!("[PE INSTALL] 格式化失败: {}", e);
            show_error_message(&tr!("格式化分区失败: {}", e));
            return Ok(());
        }

        // Step 2: 释放镜像
        log::info!("[PE INSTALL] Step 2: 释放镜像");
        if config.is_xp_i386 {
            match lr_core::xp_i386::install_from_i386(
                std::path::Path::new(&image_path),
                &target_partition,
                &utils::path::get_bin_dir(),
                xp_custom_sif.as_deref(),
            ) {
                Ok(log_output) => log::info!("[PE INSTALL/XP TEXTMODE] {log_output}"),
                Err(error) => {
                    show_error_message(&tr!("准备 XP/2003 文本模式安装失败: {}", error));
                    return Ok(());
                }
            }
            if let Err(error) =
                DiskManager::cleanup_auto_created_partition_and_extend(&target_partition)
            {
                log::error!("[PE INSTALL/XP TEXTMODE] cleanup failed: {error}");
                show_error_message(&tr!("清理安装临时分区并合并空间失败: {}", error));
                return Ok(());
            }
            ConfigFileManager::cleanup_all(&data_partition, &target_partition);
            if config.auto_reboot {
                let _ = utils::command::new_command("shutdown")
                    .args(["/r", "/t", "10", "/c", "LetRecovery XP/2003 setup is ready"])
                    .spawn();
            } else {
                show_success_message(&tr!(
                    "XP/2003 文本模式安装已准备完成，请重启计算机继续安装。"
                ));
            }
            return Ok(());
        }

        let apply_dir = format!("{}\\", target_partition);

        let apply_result = if config.is_gho {
            let ghost = Ghost::new();
            if !ghost.is_available() {
                show_error_message(&tr!("Ghost工具不可用"));
                return Ok(());
            }
            let partitions = DiskManager::get_partitions().unwrap_or_default();
            ghost.restore_image_to_letter(&image_path, &target_partition, &partitions, None)
        } else {
            let dism = Dism::new();
            dism.apply_image(&image_path, &apply_dir, config.volume_index, None)
        };

        if let Err(e) = apply_result {
            log::error!("[PE INSTALL] 释放镜像失败: {}", e);
            show_error_message(&tr!("释放镜像失败: {}", e));
            return Ok(());
        }

        // Step 3: 导入驱动
        log::info!("[PE INSTALL] Step 3: 导入驱动");
        let driver_path = format!("{}\\drivers", data_dir);
        let driver_path_exists = std::path::Path::new(&driver_path).exists();

        if config.should_import_drivers() && driver_path_exists {
            let dism = Dism::new();
            match dism.add_drivers_offline_with_progress(&apply_dir, &driver_path, None) {
                Ok(_) => log::info!("[PE INSTALL] 驱动导入成功"),
                Err(e) => {
                    log::warn!("[PE INSTALL] 警告: 驱动导入失败: {} (继续安装)", e);
                    log::warn!("驱动导入失败: {}", e);
                }
            }

            // 同时检查驱动目录中是否有 CAB 文件并安装
            let cab_files = find_cab_files_in_dir(&driver_path);
            if !cab_files.is_empty() {
                log::info!(
                    "[PE INSTALL] 在驱动目录中发现 {} 个 CAB 文件，一并安装",
                    cab_files.len()
                );
                match dism.add_packages_offline_from_dir(&apply_dir, &driver_path, None) {
                    Ok((success, fail)) => {
                        log::info!(
                            "[PE INSTALL] 驱动目录中的CAB安装完成: {} 成功, {} 失败",
                            success,
                            fail
                        );
                    }
                    Err(e) => {
                        log::warn!(
                            "[PE INSTALL] 警告: 驱动目录中的CAB安装失败: {} (继续安装)",
                            e
                        );
                        log::warn!("驱动目录中的CAB安装失败: {}", e);
                    }
                }
            }
        } else if config.should_import_drivers() && !driver_path_exists {
            log::info!("[PE INSTALL] 驱动目录不存在，跳过驱动导入");
        } else {
            log::info!("[PE INSTALL] 跳过驱动导入");
        }

        // Step 4: 安装CAB更新包
        log::info!("[PE INSTALL] Step 4: 安装CAB更新包");
        if config.install_cab_packages {
            let cab_path = format!("{}\\updates", data_dir);
            if std::path::Path::new(&cab_path).exists() {
                let dism = Dism::new();
                match dism.add_packages_offline_from_dir(&apply_dir, &cab_path, None) {
                    Ok((success, fail)) => {
                        log::info!(
                            "[PE INSTALL] CAB更新包安装完成: {} 成功, {} 失败",
                            success,
                            fail
                        );
                    }
                    Err(e) => {
                        log::warn!("[PE INSTALL] 警告: CAB更新包安装失败: {} (继续安装)", e);
                        log::warn!("CAB更新包安装失败: {}", e);
                    }
                }
            } else {
                log::info!("[PE INSTALL] 更新包目录不存在，跳过CAB安装");
            }
        } else {
            log::info!("[PE INSTALL] 跳过CAB更新包安装");
        }

        if let Some(package) = pca_compat_package.as_ref() {
            log::info!(
                "[PE INSTALL] 为 Windows build {} / architecture {} 注入 PCA2023 BootEx",
                package.target().build,
                package.target().architecture
            );
            if let Err(error) =
                package.inject_into_offline_windows(std::path::Path::new(&apply_dir))
            {
                log::error!("[PE INSTALL] PCA2023 兼容包注入失败: {error}");
                show_error_message(&tr!("升级 PCA2023 引导文件失败：{}", error));
                return Ok(());
            }
        }

        // Step 5: 修复引导
        log::info!("[PE INSTALL] Step 5: 修复引导");
        let boot_manager = BootManager::new();
        let use_uefi = DiskManager::resolve_install_uefi_mode(config.boot_mode, &target_partition);

        // XP/2003：写 XP 引导（UEFI 化映像走 UEFI/GPT，否则 ntldr）；其余走 bcdboot。
        let win_boot_dir = format!("{}\\Windows\\Boot", target_partition);
        let is_xp = config.is_xp || !std::path::Path::new(&win_boot_dir).exists();
        let boot_result = if is_xp {
            if use_uefi {
                log::info!("[PE INSTALL] 识别为 XP/2003 + UEFI，写入 XP UEFI/GPT 引导");
                boot_manager.write_xp_uefi_gpt_boot(&target_partition)
            } else {
                log::info!("[PE INSTALL] 识别为 XP/2003(Legacy)，写入 XP 引导(ntldr/boot.ini)");
                boot_manager.write_xp_boot(&target_partition)
            }
        } else {
            boot_manager.repair_boot_advanced(&target_partition, use_uefi, config.boot_pca_mode)
        };
        if let Err(e) = boot_result {
            log::error!("[PE INSTALL] 修复引导失败: {}", e);
            show_error_message(&tr!("修复引导失败: {}", e));
            return Ok(());
        }

        // Step 5.5: 如果启用了 Win7 UEFI 补丁，应用 UefiSeven
        if use_uefi && config.win7_uefi_patch {
            log::info!("[PE INSTALL] Step 5.5: 应用 Win7 UEFI 补丁 (UefiSeven)");
            match ui::advanced_options::apply_uefiseven_patch(&data_partition, &target_partition) {
                Ok(_) => log::info!("[PE INSTALL] UefiSeven 补丁应用成功"),
                Err(e) => {
                    // UefiSeven 补丁失败不中断安装，只记录警告
                    log::warn!(
                        "[PE INSTALL] 警告: UefiSeven 补丁应用失败: {} (继续安装)",
                        e
                    );
                    log::warn!("UefiSeven 补丁应用失败: {}", e);
                }
            }
        }

        // Step 6: 应用高级选项
        log::info!("[PE INSTALL] Step 6: 应用高级选项");
        if let Err(error) = apply_advanced_options(&target_partition, &config) {
            log::error!("[PE INSTALL] 应用高级选项失败，安装停止: {}", error);
            show_error_message(&tr!("应用高级选项失败，未继续安装: {}", error));
            return Ok(());
        }
        // 注入数据分区上的用户驱动（bin/drivers/<版本> 由正常端复制而来）
        if let Err(error) =
            ui::advanced_options::inject_user_drivers_from_data(&target_partition, &data_dir)
        {
            show_error_message(&tr!("注入用户驱动失败: {}", error));
            return Ok(());
        }

        // Step 7: 生成无人值守配置
        if config.unattended {
            log::info!("[PE INSTALL] Step 7: 生成无人值守配置");
            if !config.custom_unattend_file.is_empty() {
                // 用户自定义无人值守文件：直接复制到目标系统
                let data_dir = ConfigFileManager::get_data_dir(&data_partition);
                let src = match ConfigFileManager::resolve_staged_file(
                    &data_dir,
                    &config.custom_unattend_file,
                ) {
                    Ok(path) => path,
                    Err(error) => {
                        log::error!("[PE INSTALL] 自定义无人值守文件名无效: {error}");
                        show_error_message(&tr!("自定义无人值守文件名无效: {}", error));
                        return Ok(());
                    }
                };
                if let Err(e) =
                    app::apply_custom_unattend(&target_partition, &src.to_string_lossy())
                {
                    log::error!("[PE INSTALL] 应用自定义无人值守文件失败: {}", e);
                    show_error_message(&tr!("应用自定义无人值守文件失败: {}", e));
                    return Ok(());
                }
                log::info!("[PE INSTALL] 已应用自定义无人值守文件: {}", src.display());
            } else if let Err(error) = app::generate_unattend_xml(&target_partition, &config) {
                log::error!("[PE INSTALL] 生成无人值守配置失败: {error}");
                show_error_message(&tr!("生成无人值守配置失败: {}", error));
                return Ok(());
            }
        }

        // Step 7.5: 离线登录兜底（放开空密码策略 + 已知用户名时配置自动登录）
        if let Err(e) = core::account_fix::ensure_offline_login(
            &target_partition,
            &config.custom_username,
            config.is_gho || config.is_xp,
        ) {
            log::warn!("[PE INSTALL] 离线登录兜底设置失败（不影响安装）: {}", e);
        } else {
            log::info!("[PE INSTALL] 已应用离线登录兜底设置");
        }

        // Step 8: 清理
        log::info!("[PE INSTALL] Step 8: 清理临时文件");
        // Step 9: 清理自动创建的数据分区并扩展目标分区
        log::info!("[PE INSTALL] Step 9: 清理自动创建的分区");
        if let Err(e) = DiskManager::cleanup_auto_created_partition_and_extend(&target_partition) {
            log::error!("[PE INSTALL] 清理自动创建分区失败: {}", e);
            show_error_message(&tr!("清理安装临时分区并合并空间失败: {}", e));
            return Ok(());
        }
        log::info!("[PE INSTALL] 自动创建分区清理完成");
        ConfigFileManager::cleanup_all(&data_partition, &target_partition);

        log::info!("[PE INSTALL] 安装完成!");

        if config.auto_reboot {
            log::info!("[PE INSTALL] 即将重启...");
            let _ = utils::command::new_command("shutdown")
                .args([
                    "/r",
                    "/t",
                    "10",
                    "/c",
                    "LetRecovery 系统安装完成，即将重启...",
                ])
                .spawn();
        } else {
            show_success_message(&tr!("系统安装完成！请手动重启计算机。"));
        }
    } else {
        // 备份模式
        log::info!("[PE BACKUP] ========== PE自动备份模式 ==========");

        // 查找配置文件所在分区
        let data_partition = match ConfigFileManager::find_data_partition_for(
            crate::core::config::OperationType::Backup,
        ) {
            Some(p) => p,
            None => {
                log::error!("[PE BACKUP] 错误: 未找到备份配置文件");
                show_error_message(&tr!("未找到备份配置文件，无法继续备份。"));
                return Ok(());
            }
        };

        log::info!("[PE BACKUP] 数据分区: {}", data_partition);

        // 读取备份配置
        let config = match ConfigFileManager::read_backup_config(&data_partition) {
            Ok(c) => c,
            Err(e) => {
                log::error!("[PE BACKUP] 错误: 读取配置失败: {}", e);
                show_error_message(&tr!("读取备份配置失败: {}", e));
                return Ok(());
            }
        };

        // 切换到正常系统端选定的镜像引擎（随重启传入）
        lr_core::set_active_engine(lr_core::WimEngine::from_u8(config.wim_engine));

        log::info!("[PE BACKUP] 源分区: {}", config.source_partition);
        log::info!("[PE BACKUP] 保存路径: {}", config.save_path);

        // 查找备份标记分区
        let source_partition = ConfigFileManager::find_backup_marker_partition()
            .unwrap_or_else(|| config.source_partition.clone());

        // 执行备份（按格式分发，与 PE GUI worker 一致）。
        // 此前恒走 LZX WIM，忽略 config.format/swm —— ESD/SWM/GHO 都会产出错误文件。
        let dism = Dism::new();
        let capture_dir = format!("{}\\", source_partition);

        use crate::core::config::BackupFormat;
        let backup_result = match config.format {
            BackupFormat::Gho => {
                let ghost = core::ghost::Ghost::new();
                if !ghost.is_available() {
                    log::error!("[PE BACKUP] Ghost 工具不可用");
                    show_error_message(&tr!("系统备份失败: Ghost 工具不可用"));
                    return Ok(());
                }
                ghost.create_image_from_letter(&source_partition, &config.save_path, None)
            }
            BackupFormat::Esd => {
                if config.incremental && std::path::Path::new(&config.save_path).exists() {
                    dism.append_image_esd(
                        &config.save_path,
                        &capture_dir,
                        &config.name,
                        &config.description,
                        None,
                    )
                } else {
                    dism.capture_image_esd(
                        &config.save_path,
                        &capture_dir,
                        &config.name,
                        &config.description,
                        None,
                    )
                }
            }
            BackupFormat::Swm => dism.capture_image_swm(
                &config.save_path,
                &capture_dir,
                &config.name,
                &config.description,
                config.swm_split_size,
                None,
            ),
            BackupFormat::Wim => {
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
        };

        if let Err(e) = backup_result {
            log::error!("[PE BACKUP] 备份失败: {}", e);
            show_error_message(&tr!("系统备份失败: {}", e));
            return Ok(());
        }

        let verify_result = match config.format {
            BackupFormat::Gho => {
                core::ghost::Ghost::new().verify_image_integrity(&config.save_path)
            }
            BackupFormat::Wim | BackupFormat::Esd | BackupFormat::Swm => {
                Dism::verify_captured_image(
                    std::path::Path::new(&config.save_path),
                    &config.name,
                    &config.description,
                )
            }
        };
        if let Err(error) = verify_result {
            log::error!("[PE BACKUP] 备份产物验证失败: {}", error);
            show_error_message(&tr!("备份文件验证失败: {}", error));
            return Ok(());
        }

        // 删除PE引导项
        let boot_manager = BootManager::new();
        if let Err(error) = boot_manager.delete_current_boot_entry() {
            log::error!("[PE BACKUP] 删除 PE 引导项失败: {}", error);
            show_error_message(&tr!("删除 PE 引导项失败: {}", error));
            return Ok(());
        }

        // 清理
        ConfigFileManager::cleanup_partition_markers(&source_partition);
        ConfigFileManager::cleanup_data_dir(&data_partition);
        ConfigFileManager::cleanup_pe_dir(&data_partition);

        log::info!("[PE BACKUP] 备份完成!");
        show_success_message(&tr!("系统备份完成！\n保存位置: {}", config.save_path));

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
            MessageBoxW(null_mut(), wide_message.as_ptr(), wide_title.as_ptr(), 0x10);
            // MB_ICONERROR
        }
    }

    #[cfg(not(windows))]
    {
        log::error!("错误: {}", message);
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
            MessageBoxW(null_mut(), wide_message.as_ptr(), wide_title.as_ptr(), 0x40);
            // MB_ICONINFORMATION
        }
    }

    #[cfg(not(windows))]
    {
        log::info!("成功: {}", message);
    }
}
