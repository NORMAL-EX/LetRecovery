#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(dead_code)]

mod build_info;
mod core;
mod download;
mod native_ui;
mod utils;
mod win7_import_compat;

use std::sync::Arc;
use std::sync::{mpsc::Receiver, Mutex};

/// 预加载的配置数据
pub struct PreloadedConfig {
    pub app_config: core::app_config::AppConfig,
    pub remote_config: Option<download::server_config::RemoteConfig>,
    pub system_info: Option<core::system_info::SystemInfo>,
    pub hardware_info: Option<core::hardware_info::HardwareInfo>,
    pub partitions: Vec<core::disk::Partition>,
    /// PCA firmware probing starts alongside the other startup preloads. The native window takes
    /// this receiver after its HWND exists and forwards the already-running result into the normal
    /// message path without delaying first presentation.
    pub pca_firmware_receiver: Mutex<Option<Receiver<lr_core::boot_pca::FirmwarePcaInfo>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupRoute {
    Gui,
    PublicCli,
    InternalCompatibilityCli,
    InternalNativeHelper,
    RejectedLegacyCli,
    UnsupportedArguments,
}

fn classify_startup_route(args: &[String]) -> StartupRoute {
    let Some(first) = args.get(1).map(String::as_str) else {
        return StartupRoute::Gui;
    };
    #[cfg(feature = "non-elevated-tests")]
    if first == "--ui-personal-restore-progress-preview" && args.len() == 2 {
        return StartupRoute::InternalNativeHelper;
    }
    #[cfg(feature = "non-elevated-tests")]
    if matches!(
        first,
        "--ui-preview"
            | "--ui-error-preview"
            | "--ui-progress-preview"
            | "--ui-pe-maintenance-preview"
            | "--ui-about-preview"
    ) {
        return StartupRoute::Gui;
    }
    if matches!(
        first,
        "help" | "--help" | "-h" | "install" | "backup" | "config" | "inspect" | "update" | "tool"
    ) || first.eq_ignore_ascii_case("--install")
        || first.eq_ignore_ascii_case("/INSTALL")
    {
        return StartupRoute::PublicCli;
    }
    if is_restore_windows_update_arg(first) && args.len() == 2 {
        return StartupRoute::InternalCompatibilityCli;
    }
    if first == "--internal-prepare-local-rid" && args.len() == 4 {
        return StartupRoute::InternalNativeHelper;
    }
    if first == "--internal-store-builtin-administrator-secret" && args.len() == 2 {
        return StartupRoute::InternalNativeHelper;
    }
    if matches!(
        first,
        "--internal-begin-builtin-administrator-transition"
            | "--internal-finish-builtin-administrator-transition"
            | "--internal-retire-builtin-administrator-transition"
    ) && args.len() == 4
    {
        return StartupRoute::InternalNativeHelper;
    }
    if first == "--internal-begin-builtin-administrator-transition-with-personal-restore"
        && args.len() == 5
    {
        return StartupRoute::InternalNativeHelper;
    }
    if first == "--internal-delete-temporary-oobe-account" && args.len() == 3 {
        return StartupRoute::InternalNativeHelper;
    }
    if first == "--internal-cleanup-disabled-defaultuser0" && args.len() == 2 {
        return StartupRoute::InternalNativeHelper;
    }
    if first == "--internal-restore-personal-files" && args.len() == 3 {
        return StartupRoute::InternalNativeHelper;
    }
    if first == "--internal-restore-personal-files-at-shell" && args.len() == 3 {
        return StartupRoute::InternalNativeHelper;
    }
    if first == "--internal-personal-restore-progress-shell" && args.len() == 3 {
        return StartupRoute::InternalNativeHelper;
    }
    if matches!(
        first,
        "--internal-activate-personal-restore-shell-gate"
            | "--internal-begin-personal-restore-second-logon"
            | "--internal-restore-personal-files-before-shell"
            | "--internal-rearm-personal-restore-before-shell"
    ) && args.len() == 3
    {
        return StartupRoute::InternalNativeHelper;
    }
    if first == "--internal-restore-personal-files-after-shell" && args.len() == 4 {
        return StartupRoute::InternalNativeHelper;
    }
    if first == "--internal-register-personal-files-at-shell" && args.len() == 4 {
        return StartupRoute::InternalNativeHelper;
    }
    if (first.eq_ignore_ascii_case("/PEINSTALL")
        || first.eq_ignore_ascii_case("--pe-install")
        || first.eq_ignore_ascii_case("/PEBACKUP")
        || first.eq_ignore_ascii_case("--pe-backup"))
        && args.len() == 2
    {
        return StartupRoute::RejectedLegacyCli;
    }
    StartupRoute::UnsupportedArguments
}

const fn should_request_gui_elevation(
    route: StartupRoute,
    already_admin: bool,
    non_elevated_test_build: bool,
) -> bool {
    matches!(route, StartupRoute::Gui) && !already_admin && !non_elevated_test_build
}

fn main() -> anyhow::Result<()> {
    // Route arguments before loading configuration, initializing logging, or entering any GUI
    // elevation path. CLI and internal compatibility routes never self-elevate.
    let args: Vec<String> = std::env::args().collect();
    let startup_route = classify_startup_route(&args);
    if startup_route == StartupRoute::InternalNativeHelper {
        let required_specialize_account_prepare = matches!(
            args.get(1).map(String::as_str),
            Some("--internal-prepare-local-rid")
                | Some("--internal-store-builtin-administrator-secret")
        );
        #[cfg(feature = "non-elevated-tests")]
        let personal_restore_progress_preview =
            args.get(1).map(String::as_str) == Some("--ui-personal-restore-progress-preview");
        #[cfg(not(feature = "non-elevated-tests"))]
        let personal_restore_progress_preview = false;
        let result = if personal_restore_progress_preview {
            lr_core::first_logon::run_personal_restore_progress_preview()
        } else if args.get(1).map(String::as_str)
            == Some("--internal-store-builtin-administrator-secret")
        {
            lr_core::first_logon::protect_staged_builtin_administrator_secret()
        } else if args.get(1).map(String::as_str)
            == Some("--internal-personal-restore-progress-shell")
        {
            match args.get(2) {
                Some(session_id) => {
                    lr_core::first_logon::run_personal_restore_progress_shell(session_id)
                }
                None => Err(anyhow::anyhow!(
                    "missing personal-file progress Shell session id"
                )),
            }
        } else if matches!(
            args.get(1).map(String::as_str),
            Some("--internal-activate-personal-restore-shell-gate")
                | Some("--internal-begin-personal-restore-second-logon")
                | Some("--internal-rearm-personal-restore-before-shell")
        ) {
            match args.get(2) {
                Some(session_id) => match args.get(1).map(String::as_str) {
                    Some("--internal-activate-personal-restore-shell-gate") => {
                        lr_core::first_logon::activate_personal_restore_shell_gate(session_id)
                    }
                    Some("--internal-begin-personal-restore-second-logon") => {
                        lr_core::first_logon::begin_personal_restore_second_logon(session_id)
                    }
                    Some("--internal-rearm-personal-restore-before-shell") => {
                        lr_core::first_logon::rearm_personal_restore_before_shell(session_id)
                    }
                    _ => unreachable!("startup route already classified the helper switch"),
                },
                None => Err(anyhow::anyhow!(
                    "missing personal-file Shell gate session id"
                )),
            }
        } else if matches!(
            args.get(1).map(String::as_str),
            Some("--internal-restore-personal-files")
                | Some("--internal-restore-personal-files-at-shell")
                | Some("--internal-restore-personal-files-before-shell")
                | Some("--internal-restore-personal-files-after-shell")
        ) {
            match args.get(2) {
                Some(session_id) => {
                    let report = match args.get(1).map(String::as_str) {
                        Some("--internal-restore-personal-files-at-shell") => {
                            lr_core::first_logon::restore_personal_files_at_shell(session_id)?
                        }
                        Some("--internal-restore-personal-files-before-shell") => {
                            lr_core::first_logon::restore_personal_files_before_shell(session_id)?
                        }
                        Some("--internal-restore-personal-files-after-shell") => {
                            let automation = match args.get(3).map(String::as_str) {
                                Some("true") => true,
                                Some("false") => false,
                                _ => anyhow::bail!(
                                    "invalid personal-file Explorer-stage automation flag"
                                ),
                            };
                            lr_core::first_logon::restore_personal_files_after_shell(
                                session_id,
                                automation,
                            )?
                        }
                        _ => Some(
                            lr_core::personal_files::restore_preserved_personal_files_for_current_user(
                                session_id,
                            )?,
                        ),
                    };
                    if let Some(report) = report {
                        println!(
                            "completed profile={} sources={} directories={} files={} conflicts={}",
                            report.current_profile_root.display(),
                            report.source_profiles,
                            report.restored_directories,
                            report.restored_files,
                            report.renamed_conflicts
                        );
                        for (name, path) in [
                            "Desktop",
                            "Documents",
                            "Downloads",
                            "Pictures",
                            "Music",
                            "Videos",
                        ]
                        .into_iter()
                        .zip(report.personal_directories.iter())
                        {
                            println!(
                                "destination scope=personal name={name} path={}",
                                path.display()
                            );
                        }
                        for (name, path) in [
                            "Desktop",
                            "Documents",
                            "Downloads",
                            "Pictures",
                            "Music",
                            "Videos",
                        ]
                        .into_iter()
                        .zip(report.public_directories.iter())
                        {
                            println!(
                                "destination scope=public name={name} path={}",
                                path.display()
                            );
                        }
                    } else {
                        println!("completed cleanup-only receipt={session_id}");
                    }
                    Ok(())
                }
                None => Err(anyhow::anyhow!("missing personal-file restore session id")),
            }
        } else if args.get(1).map(String::as_str)
            == Some("--internal-register-personal-files-at-shell")
        {
            match (args.get(2), args.get(3)) {
                (Some(session_id), Some(launcher)) => {
                    lr_core::first_logon::register_personal_restore_at_shell(
                        session_id,
                        std::path::Path::new(launcher),
                    )
                }
                _ => Err(anyhow::anyhow!(
                    "missing personal-file Explorer-stage registration arguments"
                )),
            }
        } else if matches!(
            args.get(1).map(String::as_str),
            Some("--internal-begin-builtin-administrator-transition")
                | Some("--internal-begin-builtin-administrator-transition-with-personal-restore")
                | Some("--internal-finish-builtin-administrator-transition")
                | Some("--internal-retire-builtin-administrator-transition")
        ) {
            match (args.get(2), args.get(3)) {
                (Some(desired_name), Some(temporary_name)) => {
                    let desired_name =
                        lr_core::windows_accounts::decode_account_name_utf16_hex(desired_name)
                            .map_err(anyhow::Error::from)?;
                    let temporary_name =
                        lr_core::windows_accounts::decode_account_name_utf16_hex(temporary_name)
                            .map_err(anyhow::Error::from)?;
                    match args.get(1).map(String::as_str) {
                        Some("--internal-begin-builtin-administrator-transition") => {
                            lr_core::first_logon::begin_builtin_administrator_transition(
                                &desired_name,
                                &temporary_name,
                            )
                        }
                        Some(
                            "--internal-begin-builtin-administrator-transition-with-personal-restore",
                        ) => match args.get(4) {
                            Some(session_id) => lr_core::first_logon::begin_builtin_administrator_transition_with_personal_restore(
                                &desired_name,
                                &temporary_name,
                                session_id,
                            ),
                            None => Err(anyhow::anyhow!(
                                "missing built-in Administrator personal-file session id"
                            )),
                        },
                        Some("--internal-finish-builtin-administrator-transition") => {
                            lr_core::first_logon::finish_builtin_administrator_transition(
                                &desired_name,
                                &temporary_name,
                            )
                        }
                        Some("--internal-retire-builtin-administrator-transition") => {
                            lr_core::first_logon::retire_builtin_administrator_transition(
                                &desired_name,
                                &temporary_name,
                            )
                        }
                        _ => unreachable!("startup route already classified the helper switch"),
                    }
                }
                _ => Err(anyhow::anyhow!(
                    "missing built-in Administrator transition identities"
                )),
            }
        } else if args.get(1).map(String::as_str)
            == Some("--internal-cleanup-disabled-defaultuser0")
        {
            lr_core::windows_accounts::cleanup_disabled_default_oobe_account()
                .map(|removed| println!("completed removed={removed}"))
                .map_err(anyhow::Error::from)
        } else if args.get(1).map(String::as_str)
            == Some("--internal-delete-temporary-oobe-account")
        {
            match args.get(2) {
                Some(name) => lr_core::windows_accounts::decode_account_name_utf16_hex(name)
                    .and_then(|name| {
                        lr_core::unattend_account::validate_temporary_oobe_account_name(&name)
                            .map_err(|_| {
                                lr_core::windows_accounts::AccountUpdateError::InvalidAccount
                            })?;
                        lr_core::windows_accounts::delete_local_account(&name)
                    })
                    .map_err(anyhow::Error::from),
                None => Err(anyhow::anyhow!("missing temporary OOBE account identity")),
            }
        } else {
            match (args.get(2).map(String::as_str), args.get(3)) {
                (Some("500"), Some(name)) => {
                    lr_core::windows_accounts::decode_account_name_utf16_hex(name)
                        .and_then(|name| {
                            lr_core::windows_accounts::prepare_local_account_by_rid(500, &name)
                        })
                        .map_err(anyhow::Error::from)
                }
                _ => Err(anyhow::anyhow!("invalid internal account-helper arguments")),
            }
        };
        if let Err(error) = result {
            println!("failed: {error:#}");
            std::process::exit(if required_specialize_account_prepare {
                lr_core::unattend_command::REQUIRED_SPECIALIZE_FAILURE_EXIT_CODE
            } else {
                1
            });
        }
        std::process::exit(0);
    }
    if startup_route == StartupRoute::UnsupportedArguments {
        std::process::exit(core::cli::startup_usage_error(
            "unrecognized arguments; use 'help' for the public normal-Windows CLI",
        ));
    }
    if startup_route == StartupRoute::RejectedLegacyCli {
        std::process::exit(core::cli::startup_usage_error(
            "deprecated normal-endpoint PE install/backup switches are permanently rejected; use the public normal-Windows install/backup CLI",
        ));
    }
    let restore_windows_update = match parse_restore_windows_update_cli(&args) {
        Ok(value) => value,
        Err(error) => std::process::exit(core::cli::startup_usage_error(error)),
    };

    #[cfg(feature = "non-elevated-tests")]
    let non_elevated_test_build = true;
    #[cfg(not(feature = "non-elevated-tests"))]
    let non_elevated_test_build = false;

    if startup_route == StartupRoute::InternalCompatibilityCli {
        if non_elevated_test_build {
            std::process::exit(core::cli::development_run_denied());
        }
        if !utils::privilege::is_admin() {
            std::process::exit(core::cli::administrator_required_for(
                "the internal compatibility command",
            ));
        }
    }

    if should_request_gui_elevation(
        startup_route,
        utils::privilege::is_admin(),
        non_elevated_test_build,
    ) {
        if let Err(error) = utils::privilege::restart_gui_as_admin() {
            native_ui::enable_process_dpi_awareness();
            show_error_message(&format!("无法获取管理员权限：{error}"));
        }
        return Ok(());
    }

    // Suppress the system's modal "No disk" critical-error dialog before any background
    // preload probes drive letters. Empty/ejected optical drives are valid and must be skipped,
    // not allowed to block the whole process behind a system-owned retry dialog.
    unsafe {
        use windows::Win32::System::Diagnostics::Debug::{
            SetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOOPENFILEERRORBOX,
        };
        let required = SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX;
        let previous = SetErrorMode(required);
        let _ = SetErrorMode(previous | required);
    }

    // Startup validation and elevation failures can display MessageBoxW before the main window
    // exists. Establish PMv2 awareness first so those dialogs are rendered at the monitor's native
    // DPI instead of being bitmap-scaled and blurred by USER32.
    native_ui::enable_process_dpi_awareness();

    if startup_route == StartupRoute::PublicCli {
        if let Some(exit_code) = core::cli::execute_args(&args) {
            std::process::exit(exit_code);
        }
        #[cfg(feature = "non-elevated-tests")]
        if core::cli::is_destructive_run_request(&args) {
            std::process::exit(core::cli::development_run_denied());
        }
        #[cfg(not(feature = "non-elevated-tests"))]
        if core::cli::requires_administrator(&args) && !utils::privilege::is_admin() {
            std::process::exit(core::cli::administrator_required());
        }
    }

    // 加载应用配置（用于获取日志设置）
    let app_config = core::app_config::AppConfig::load();

    // 初始化日志系统
    if let Err(e) = utils::logger::LogManager::init(app_config.log_enabled) {
        if startup_route == StartupRoute::PublicCli {
            core::cli::emit_progress(serde_json::json!({
                "event": "warning",
                "code": "logger_initialization_failed",
                "message": e.to_string(),
            }));
        } else {
            eprintln!("日志系统初始化失败: {}", e);
        }
        // 即使日志初始化失败，程序也应该继续运行
    }

    // 清理旧日志文件
    if app_config.log_enabled {
        if let Err(e) = utils::logger::LogManager::cleanup_old_logs(app_config.log_retention_days) {
            log::warn!("清理旧日志失败: {}", e);
        }
    }

    // 初始化国际化系统
    utils::i18n::init(&app_config.language);

    // 应用 WIM 镜像引擎选择（libwim / wimgapi），供后续所有镜像操作使用
    app_config.apply_wim_engine();

    if startup_route == StartupRoute::PublicCli {
        if let Some(exit_code) = core::cli::execute_runtime_args(&args) {
            std::process::exit(exit_code);
        }
    }

    log::info!("LetRecovery 启动中...");
    log::info!(
        "[诊断环境] 软件版本: version={} | channel={} | arch={}",
        env!("BUILD_VERSION"),
        if crate::build_info::DEV {
            "dev-build"
        } else {
            "production"
        },
        std::env::consts::ARCH
    );

    #[cfg(feature = "non-elevated-tests")]
    if args.iter().any(|arg| arg == "--ui-error-preview") {
        show_error_message(
            "程序文件不完整，无法正常运行。\n\n\
             缺少以下文件：\n\
             bin/bcdedit.exe\n\
             bin/bcdboot.exe\n\
             bin/bootsect.exe\n\
             bin/aria2c.exe\n\
             bin/ghost/ghost64.exe\n\n\
             请重新下载完整安装包或修复程序文件。",
        );
        return Ok(());
    }

    #[cfg(not(feature = "non-elevated-tests"))]
    log::info!("已获得管理员权限，或正在执行不需提权的公开 CLI");

    if let Some(exit_code) = core::cli::execute_run_args(&args) {
        std::process::exit(exit_code);
    }

    if restore_windows_update {
        return run_restore_windows_update_cli();
    }

    // 记录本机配置信息，便于用户反馈问题时开发者排查
    #[cfg(not(feature = "non-elevated-tests"))]
    if app_config.log_enabled {
        log_machine_info();
    }

    // 检查是否为64位系统
    if !cfg!(target_arch = "x86_64") {
        log::error!("本程序仅支持64位系统");
        log::error!("本程序仅支持64位系统");
        return Ok(());
    }

    #[cfg(feature = "non-elevated-tests")]
    {
        let progress_preview = args.iter().any(|arg| arg == "--ui-progress-preview");
        let pe_maintenance_preview = args.iter().any(|arg| arg == "--ui-pe-maintenance-preview");
        let about_preview = args.iter().any(|arg| arg == "--ui-about-preview");
        if args.iter().any(|arg| arg == "--ui-preview")
            || progress_preview
            || pe_maintenance_preview
            || about_preview
            || std::env::var_os("LETRECOVERY_UI_SKIP_PRELOAD").is_some()
        {
            // Deterministic visual-regression entry: bypass single-instance state and vendor
            // WMI/SetupAPI providers, but retain the real config, native controls and message loop.
            // This branch is absent from release builds and the dangerous CLI guard has already run.
            // Visual/non-elevated runs must not change the host wallpaper or start packaged audio.
            // The production path below still synchronizes the easter egg for the selected language.
            let preview_config = Arc::new(PreloadedConfig {
                app_config: app_config.clone(),
                remote_config: None,
                system_info: None,
                hardware_info: None,
                partitions: Vec::new(),
                pca_firmware_receiver: Mutex::new(None),
            });
            let run_result = if progress_preview {
                native_ui::run_progress_preview(preview_config)
            } else if pe_maintenance_preview {
                native_ui::run_pe_maintenance_preview(preview_config)
            } else if about_preview {
                native_ui::run_about_preview(preview_config)
            } else {
                native_ui::run(preview_config)
            };
            utils::dprk_easter_egg::shutdown();
            run_result?;
            return Ok(());
        }
    }

    #[cfg(not(feature = "non-elevated-tests"))]
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

    #[cfg(not(feature = "non-elevated-tests"))]
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
    #[cfg(not(feature = "non-elevated-tests"))]
    let mutex_name = "LetRecovery-mutex-2025";
    #[cfg(feature = "non-elevated-tests")]
    let mutex_name = "LetRecovery-native-ui-preview-mutex";

    let _mutex = match single_instance::SingleInstance::new(mutex_name) {
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
    #[cfg(not(feature = "non-elevated-tests"))]
    let preloaded_config = {
        let pca_firmware_receiver = start_pca_firmware_probe();
        preload_all_config(app_config.clone(), pca_firmware_receiver)
    };

    // 原生 UI 开发预览不联网、不枚举安装目标分区；系统与硬件摘要仍在窗口
    // 显示前只读加载，确保无 UAC 的测试产物与正式版硬件页行为一致。
    #[cfg(feature = "non-elevated-tests")]
    let preloaded_config = {
        let system_info = std::thread::spawn(|| core::system_info::SystemInfo::collect().ok());
        let hardware_info =
            std::thread::spawn(|| core::hardware_info::HardwareInfo::collect().ok());
        PreloadedConfig {
            app_config: app_config.clone(),
            remote_config: None,
            system_info: system_info.join().ok().flatten(),
            hardware_info: hardware_info.join().ok().flatten(),
            partitions: Vec::new(),
            pca_firmware_receiver: Mutex::new(None),
        }
    };
    let preloaded_config = Arc::new(preloaded_config);
    if app_config.log_enabled {
        log_partition_environment(&preloaded_config.partitions);
    }

    log::info!("预加载完成，初始化 GUI...");

    log::info!("启动原生 Win32 窗口...");
    if let Err(error) = utils::dprk_easter_egg::sync_for_language(&app_config.language) {
        log::warn!("同步朝鲜文彩蛋失败: {error:#}");
    }
    let run_result = native_ui::run(preloaded_config);
    utils::dprk_easter_egg::shutdown();
    run_result?;
    Ok(())
}

/// 预加载所有配置和系统信息
fn start_pca_firmware_probe() -> Receiver<lr_core::boot_pca::FirmwarePcaInfo> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let firmware = lr_core::boot_pca::inspect_firmware_pca();
        let _ = sender.send(firmware);
    });
    receiver
}

fn preload_all_config(
    app_config: core::app_config::AppConfig,
    pca_firmware_receiver: Receiver<lr_core::boot_pca::FirmwarePcaInfo>,
) -> PreloadedConfig {
    use std::time::Instant;

    // 窗口显示前并行读取分区、系统和硬件信息。硬件页首次出现时必须已经
    // 有确定的成功或失败状态，不能要求用户再点击一次“刷新”才开始读取。

    let partitions_handle = std::thread::spawn(|| {
        log::info!("开始获取分区信息...");
        let partitions = core::disk::DiskManager::get_install_partitions().unwrap_or_default();
        log::info!("可安装分区信息获取完成: {} 个分区", partitions.len());
        partitions
    });

    let system_info_handle = std::thread::spawn(|| {
        log::info!("开始获取系统信息...");
        let info = core::system_info::SystemInfo::collect().ok();
        log::info!("系统信息获取完成: success={}", info.is_some());
        info
    });

    let hardware_info_handle = std::thread::spawn(|| {
        log::info!("开始获取硬件信息...");
        let info = core::hardware_info::HardwareInfo::collect().ok();
        log::info!("硬件信息获取完成: success={}", info.is_some());
        info
    });

    let start = Instant::now();

    // 等待分区信息（这个通常很快）
    log::info!("等待分区信息...");
    let partitions = partitions_handle.join().ok().unwrap_or_default();
    let system_info = system_info_handle.join().ok().flatten();
    let hardware_info = hardware_info_handle.join().ok().flatten();

    log::info!("预加载完成，耗时: {:?}", start.elapsed());

    PreloadedConfig {
        app_config,
        // 网络目录由原生窗口在创建后异步加载。这样超时/错误能够回到页面，且不会
        // 留下一个主线程已经放弃接收、但仍在后台运行的预加载线程。
        remote_config: None,
        system_info,
        hardware_info,
        partitions,
        pca_firmware_receiver: Mutex::new(Some(pca_firmware_receiver)),
    }
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

/// 收集并记录本机配置信息到日志（便于用户反馈问题时排查）
fn log_machine_info() {
    log::info!("========== 本机配置信息 ==========");
    let sys_info = core::system_info::SystemInfo::collect().ok();
    match core::hardware_info::HardwareInfo::collect() {
        Ok(hw) => {
            log::info!(
                "[诊断环境] 源系统: {} | version={} | build={} | arch={}",
                hw.os.name,
                hw.os.version,
                hw.os.build_number,
                hw.os.architecture
            );
            log::info!("[诊断环境] 运行平台: {}", hw.machine_environment_summary());
            log::info!(
                "[诊断环境] 磁盘结构: 物理磁盘={} | 物理分区总数={}",
                hw.disks.len(),
                hw.disks
                    .iter()
                    .map(|disk| u64::from(disk.partitions))
                    .fold(0u64, u64::saturating_add)
            );
            log::info!(
                "[诊断环境] 系统卷 BitLocker: {:?}",
                hw.system_bitlocker_status
            );
            if let Some(si) = &sys_info {
                log::info!(
                    "[诊断环境] 固件: {} | Secure Boot: {}",
                    si.boot_mode,
                    if si.secure_boot { "开" } else { "关" }
                );
            } else {
                log::warn!("[诊断环境] 固件: 未知 | Secure Boot: 未知（系统信息采集失败）");
            }
            let text = hw.to_formatted_text(sys_info.as_ref());
            for line in text.lines() {
                if !line.trim().is_empty() {
                    log::info!("{}", line);
                }
            }
        }
        Err(e) => {
            log::warn!("采集硬件信息失败: {}", e);
            if let Some(si) = &sys_info {
                log::info!(
                    "启动模式: {} | 安全启动: {} | TPM: {} | 64位: {}",
                    si.boot_mode,
                    si.secure_boot,
                    si.tpm_enabled,
                    si.is_64bit
                );
            }
        }
    }
    log::info!("==================================");
}

fn log_partition_environment(partitions: &[core::disk::Partition]) {
    use std::collections::BTreeSet;

    let disk_numbers = partitions
        .iter()
        .filter_map(|partition| partition.disk_number)
        .collect::<BTreeSet<_>>();
    let unresolved = partitions
        .iter()
        .filter(|partition| partition.disk_number.is_none())
        .count();
    log::info!(
        "[诊断环境] 固定卷库存: 已映射物理磁盘={} | 可见固定卷={} | 磁盘号未知卷={}",
        disk_numbers.len(),
        partitions.len(),
        unresolved
    );
    for partition in partitions {
        log::info!(
            "[诊断环境] 卷 {}: disk={} | partition={} | style={} | size={}MB | free={}MB | system={} | windows={} | BitLocker={}",
            partition.letter,
            partition
                .disk_number
                .map(|number| number.to_string())
                .unwrap_or_else(|| "未知".to_owned()),
            partition
                .partition_number
                .map(|number| number.to_string())
                .unwrap_or_else(|| "未知".to_owned()),
            partition.partition_style,
            partition.total_size_mb,
            partition.free_size_mb,
            partition.is_system_partition,
            partition.has_windows,
            partition.bitlocker_status.as_str()
        );
    }
}

/// 检查系统核心组件完整性（用于检测极限精简系统）
/// 返回 Ok(()) 表示所有组件存在，Err(Vec<String>) 包含缺失的组件列表
fn check_system_components() -> Result<(), Vec<String>> {
    let system32_path = match lr_core::windows_compat::system_directory() {
        Ok(path) => path,
        Err(error) => {
            return Err(vec![format!("无法通过 Windows API 定位系统目录 - {error}")]);
        }
    };

    // 必需的系统组件列表
    // 注：WIM 处理已改用内置的 libwim-15.dll，不再依赖系统 wimgapi.dll
    let required_components = [("advapi32.dll", "高级 Windows API 库")];

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

fn is_restore_windows_update_arg(value: &str) -> bool {
    value.eq_ignore_ascii_case("--restore-windows-update")
        || value.eq_ignore_ascii_case("/RESTORE-WINDOWS-UPDATE")
}

/// Parse the fixed, parameterless Windows Update restore maintenance command.
/// Other application arguments retain their existing behavior, but once this command is present
/// it must be the only argument so a caller cannot accidentally combine restoration with install.
fn parse_restore_windows_update_cli(args: &[String]) -> anyhow::Result<bool> {
    let matches = args
        .iter()
        .skip(1)
        .filter(|arg| is_restore_windows_update_arg(arg))
        .count();
    if matches == 0 {
        return Ok(false);
    }
    if matches != 1 || args.len() != 2 {
        anyhow::bail!(
            "--restore-windows-update is parameterless and cannot be combined with other commands"
        );
    }
    Ok(true)
}

fn run_restore_windows_update_cli() -> anyhow::Result<()> {
    let report = core::cli_update::restore_current_windows_update()?;
    if !report.warnings.is_empty() || !report.missing_services.is_empty() {
        log::error!(
            "[UPDATE_RESTORE] compatibility_exit=partial restored={} missing_services={} warning_count={}",
            report.applied_values,
            report.missing_services.len(),
            report.warnings.len()
        );
        utils::logger::LogManager::flush();
        anyhow::bail!("Windows Update restore completed only partially");
    }
    utils::logger::LogManager::flush();
    Ok(())
}

/// 检测UEFI模式（使用 Windows API）
fn detect_uefi_mode() -> anyhow::Result<bool> {
    match lr_core::windows_firmware::detect_firmware_type()? {
        lr_core::windows_firmware::FirmwareType::Uefi => Ok(true),
        lr_core::windows_firmware::FirmwareType::Bios => Ok(false),
    }
}

/// 生成无人值守XML (PE版本)
fn generate_unattend_xml_pe(
    target_partition: &str,
    config: &core::install_config::InstallConfig,
) -> anyhow::Result<()> {
    use crate::core::system_utils::{get_file_version, get_system_architecture};
    use anyhow::Context;
    use std::path::Path;

    let username = if config.custom_username.is_empty() {
        "User"
    } else {
        &config.custom_username
    };
    let username = escape_xml_text(username);
    let temporary_oobe_account = config
        .builtin_administrator
        .enabled
        .then(|| lr_core::unattend_account::temporary_oobe_account_name(&config.session_id))
        .transpose()
        .map_err(|error| anyhow::anyhow!("无法生成临时 OOBE 账户: {error}"))?;
    let builtin = lr_core::unattend_account::render_builtin_administrator_unattend(
        &config.builtin_administrator,
        1,
        temporary_oobe_account.as_deref().unwrap_or_default(),
    )
    .map_err(|error| anyhow::anyhow!("内置 Administrator 配置无效: {error}"))?;
    let (specialize_settings, user_accounts, auto_logon) = if let Some(builtin) = builtin {
        let specialize_settings = if builtin.specialize_command.is_empty() {
            String::new()
        } else {
            format!(
                r#"    <settings pass="specialize">
        <component name="Microsoft-Windows-Deployment" processorArchitecture="{{arch}}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
            <RunSynchronous>{}</RunSynchronous>
        </component>
    </settings>
"#,
                builtin.specialize_command
            )
        };
        (
            specialize_settings,
            builtin.user_accounts,
            builtin.auto_logon,
        )
    } else {
        (
            String::new(),
            format!(
                r#"<UserAccounts><LocalAccounts><LocalAccount wcm:action="add"><Password><Value></Value><PlainText>true</PlainText></Password><Description>Local User</Description><DisplayName>{username}</DisplayName><Group>Administrators</Group><Name>{username}</Name></LocalAccount></LocalAccounts></UserAccounts>"#
            ),
            format!(
                r#"<AutoLogon><Password><Value></Value><PlainText>true</PlainText></Password><Enabled>true</Enabled><LogonCount>1</LogonCount><Username>{username}</Username></AutoLogon>"#
            ),
        )
    };

    // 检测目标系统架构
    let arch = get_system_architecture(target_partition);
    let arch_str = arch.as_unattend_str();

    // 通过 ntdll.dll 文件版本检测目标系统版本
    let ntdll_path = Path::new(target_partition)
        .join("Windows")
        .join("System32")
        .join("ntdll.dll");
    let (is_win7, is_win8) = match get_file_version(&ntdll_path) {
        Some((major, minor, _, _)) => {
            let is_win7 = major == 6 && minor == 1;
            let is_win8 = major == 6 && (minor == 2 || minor == 3);
            (is_win7, is_win8)
        }
        None => (false, false),
    };

    let international = if is_win7 || is_win8 {
        None
    } else {
        Some(
            lr_core::offline_international::read_offline_international_settings(target_partition)
                .context("读取目标系统国际化设置失败")?,
        )
    };

    // 根据系统版本生成不同的OOBE配置
    // Win7: 移除HideOEMRegistrationScreen（家庭版不支持）
    let oobe_section = if is_win7 {
        r#"<OOBE>
                <HideEULAPage>true</HideEULAPage>
                <ProtectYourPC>3</ProtectYourPC>
                <NetworkLocation>Home</NetworkLocation>
            </OOBE>"#
    } else if is_win8 {
        r#"<OOBE>
                <HideEULAPage>true</HideEULAPage>
                <HideLocalAccountScreen>true</HideLocalAccountScreen>
                <ProtectYourPC>3</ProtectYourPC>
                <NetworkLocation>Home</NetworkLocation>
            </OOBE>"#
    } else {
        r#"<OOBE>
                <HideEULAPage>true</HideEULAPage>
                <HideOnlineAccountScreens>true</HideOnlineAccountScreens>
                <HideWirelessSetupInOOBE>true</HideWirelessSetupInOOBE>
                <ProtectYourPC>3</ProtectYourPC>
            </OOBE>"#
    };

    let (international_component, time_zone) = if let Some(settings) = international.as_ref() {
        let input_locale = escape_xml_text(&settings.input_locale);
        let system_locale = escape_xml_text(&settings.system_locale);
        let ui_language = escape_xml_text(&settings.ui_language);
        let user_locale = escape_xml_text(&settings.user_locale);
        let time_zone = escape_xml_text(&settings.time_zone);
        (
            format!(
                r#"        <component name="Microsoft-Windows-International-Core" processorArchitecture="{arch}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
            <InputLocale>{input_locale}</InputLocale>
            <SystemLocale>{system_locale}</SystemLocale>
            <UILanguage>{ui_language}</UILanguage>
            <UserLocale>{user_locale}</UserLocale>
        </component>
"#,
                arch = arch_str,
            ),
            format!("            <TimeZone>{time_zone}</TimeZone>\n"),
        )
    } else {
        (String::new(), String::new())
    };

    let xml_content = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
    <settings pass="windowsPE">
        <component name="Microsoft-Windows-Setup" processorArchitecture="{arch}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
            <UserData>
                <ProductKey>
                    <WillShowUI>OnError</WillShowUI>
                </ProductKey>
                <AcceptEula>true</AcceptEula>
            </UserData>
        </component>
    </settings>
{specialize_settings}
    <settings pass="oobeSystem">
{international_component}
        <component name="Microsoft-Windows-Shell-Setup" processorArchitecture="{arch}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
{time_zone}
            {oobe}
            {user_accounts}
            {auto_logon}
        </component>
    </settings>
</unattend>"#,
        arch = arch_str,
        international_component = international_component,
        time_zone = time_zone,
        oobe = oobe_section,
        specialize_settings = specialize_settings.replace("{arch}", arch_str),
        user_accounts = user_accounts,
        auto_logon = auto_logon
    );

    let panther_dir = format!("{}\\Windows\\Panther", target_partition);
    std::fs::create_dir_all(&panther_dir)?;

    let unattend_path = format!("{}\\unattend.xml", panther_dir);
    std::fs::write(&unattend_path, &xml_content)?;

    Ok(())
}

fn escape_xml_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
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
        let wide_title: Vec<u16> = OsStr::new("LetRecovery")
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

#[cfg(test)]
mod tests {
    use super::{
        classify_startup_route, parse_restore_windows_update_cli, should_request_gui_elevation,
        StartupRoute,
    };

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn restore_windows_update_cli_is_fixed_and_parameterless() {
        assert!(
            !parse_restore_windows_update_cli(&args(&["LetRecovery.exe"]))
                .expect("ordinary startup should parse")
        );
        assert!(parse_restore_windows_update_cli(&args(&[
            "LetRecovery.exe",
            "--restore-windows-update",
        ]))
        .expect("fixed maintenance command should parse"));
        assert!(parse_restore_windows_update_cli(&args(&[
            "LetRecovery.exe",
            "/restore-windows-update",
        ]))
        .expect("Windows-style spelling should be case-insensitive"));
    }

    #[test]
    fn restore_windows_update_cli_rejects_duplicates_and_combinations() {
        assert!(parse_restore_windows_update_cli(&args(&[
            "LetRecovery.exe",
            "--restore-windows-update",
            "--restore-windows-update",
        ]))
        .is_err());
        assert!(parse_restore_windows_update_cli(&args(&[
            "LetRecovery.exe",
            "--restore-windows-update",
            "--install",
        ]))
        .is_err());
    }

    #[test]
    fn startup_router_never_treats_cli_or_internal_handoffs_as_gui() {
        for values in [
            &["LetRecovery.exe", "help"][..],
            &["LetRecovery.exe", "install", "run"][..],
            &["LetRecovery.exe", "config", "generate"][..],
            &["LetRecovery.exe", "update", "restore"][..],
            &["LetRecovery.exe", "tool", "network-info", "inspect"][..],
            &["LetRecovery.exe", "--restore-windows-update"][..],
            &[
                "LetRecovery.exe",
                "--internal-restore-personal-files",
                "0123456789abcdef0123456789abcdef",
            ][..],
            &[
                "LetRecovery.exe",
                "--internal-restore-personal-files-at-shell",
                "0123456789abcdef0123456789abcdef",
            ][..],
            &[
                "LetRecovery.exe",
                "--internal-restore-personal-files-before-shell",
                "0123456789abcdef0123456789abcdef",
            ][..],
            &[
                "LetRecovery.exe",
                "--internal-personal-restore-progress-shell",
                "0123456789abcdef0123456789abcdef",
            ][..],
            &[
                "LetRecovery.exe",
                "--internal-activate-personal-restore-shell-gate",
                "0123456789abcdef0123456789abcdef",
            ][..],
            &[
                "LetRecovery.exe",
                "--internal-begin-personal-restore-second-logon",
                "0123456789abcdef0123456789abcdef",
            ][..],
            &[
                "LetRecovery.exe",
                "--internal-rearm-personal-restore-before-shell",
                "0123456789abcdef0123456789abcdef",
            ][..],
            &[
                "LetRecovery.exe",
                "--internal-restore-personal-files-after-shell",
                "0123456789abcdef0123456789abcdef",
                "true",
            ][..],
            &[
                "LetRecovery.exe",
                "--internal-register-personal-files-at-shell",
                "0123456789abcdef0123456789abcdef",
                r"C:\LetRecovery-first-logon.cmd",
            ][..],
            &[
                "LetRecovery.exe",
                "--internal-prepare-local-rid",
                "500",
                "004c005200410064006d0069006e00310031",
            ][..],
            &[
                "LetRecovery.exe",
                "--internal-store-builtin-administrator-secret",
            ][..],
            &[
                "LetRecovery.exe",
                "--internal-begin-builtin-administrator-transition-with-personal-restore",
                "004c005200410064006d0069006e00310031",
                "004c0072004f004f00420045002d003000310032003300340035003600370038003900610062",
                "0123456789abcdef0123456789abcdef",
            ][..],
            &[
                "LetRecovery.exe",
                "--internal-delete-temporary-oobe-account",
                "004c0072004f004f00420045002d003000310032003300340035003600370038003900610062",
            ][..],
            &[
                "LetRecovery.exe",
                "--internal-cleanup-disabled-defaultuser0",
            ][..],
            &["LetRecovery.exe", "/PEBACKUP"][..],
        ] {
            let route = classify_startup_route(&args(values));
            assert_ne!(route, StartupRoute::Gui);
            assert!(!should_request_gui_elevation(route, false, false));
        }
    }

    #[test]
    fn personal_restore_helper_route_requires_exact_private_arity() {
        assert_eq!(
            classify_startup_route(&args(&[
                "LetRecovery.exe",
                "--internal-restore-personal-files",
                "0123456789abcdef0123456789abcdef",
            ])),
            StartupRoute::InternalNativeHelper
        );
        assert_eq!(
            classify_startup_route(&args(&[
                "LetRecovery.exe",
                "--internal-restore-personal-files",
            ])),
            StartupRoute::UnsupportedArguments
        );
        assert_eq!(
            classify_startup_route(&args(&[
                "LetRecovery.exe",
                "--internal-restore-personal-files-at-shell",
                "0123456789abcdef0123456789abcdef",
            ])),
            StartupRoute::InternalNativeHelper
        );
        for switch in [
            "--internal-restore-personal-files-before-shell",
            "--internal-activate-personal-restore-shell-gate",
            "--internal-begin-personal-restore-second-logon",
            "--internal-rearm-personal-restore-before-shell",
        ] {
            assert_eq!(
                classify_startup_route(&args(&[
                    "LetRecovery.exe",
                    switch,
                    "0123456789abcdef0123456789abcdef",
                ])),
                StartupRoute::InternalNativeHelper
            );
            assert_eq!(
                classify_startup_route(&args(&["LetRecovery.exe", switch])),
                StartupRoute::UnsupportedArguments
            );
        }
        assert_eq!(
            classify_startup_route(&args(&[
                "LetRecovery.exe",
                "--internal-register-personal-files-at-shell",
                "0123456789abcdef0123456789abcdef",
                r"C:\LetRecovery-first-logon.cmd",
            ])),
            StartupRoute::InternalNativeHelper
        );
        assert_eq!(
            classify_startup_route(&args(&[
                "LetRecovery.exe",
                "--internal-restore-personal-files-after-shell",
                "0123456789abcdef0123456789abcdef",
                "false",
            ])),
            StartupRoute::InternalNativeHelper
        );
        assert_eq!(
            classify_startup_route(&args(&[
                "LetRecovery.exe",
                "--internal-restore-personal-files-after-shell",
                "0123456789abcdef0123456789abcdef",
            ])),
            StartupRoute::UnsupportedArguments
        );
        assert_eq!(
            classify_startup_route(&args(&[
                "LetRecovery.exe",
                "--internal-register-personal-files-at-shell",
                "0123456789abcdef0123456789abcdef",
            ])),
            StartupRoute::UnsupportedArguments
        );
    }

    #[test]
    fn built_in_account_prepare_route_requires_exact_private_arity() {
        assert_eq!(
            classify_startup_route(&args(&[
                "LetRecovery.exe",
                "--internal-prepare-local-rid",
                "500",
                "004c005200410064006d0069006e00310031",
            ])),
            StartupRoute::InternalNativeHelper
        );
        assert_eq!(
            classify_startup_route(&args(&[
                "LetRecovery.exe",
                "--internal-prepare-local-rid",
                "500",
            ])),
            StartupRoute::UnsupportedArguments
        );
    }

    #[test]
    fn built_in_secret_store_route_requires_exact_private_arity() {
        assert_eq!(
            classify_startup_route(&args(&[
                "LetRecovery.exe",
                "--internal-store-builtin-administrator-secret",
            ])),
            StartupRoute::InternalNativeHelper
        );
        assert_eq!(
            classify_startup_route(&args(&[
                "LetRecovery.exe",
                "--internal-store-builtin-administrator-secret",
                "unexpected",
            ])),
            StartupRoute::UnsupportedArguments
        );
    }

    #[test]
    fn built_in_account_transition_routes_require_exact_private_arity() {
        for switch in [
            "--internal-begin-builtin-administrator-transition",
            "--internal-finish-builtin-administrator-transition",
            "--internal-retire-builtin-administrator-transition",
        ] {
            assert_eq!(
                classify_startup_route(&args(&[
                    "LetRecovery.exe",
                    switch,
                    "004c005200410064006d0069006e00310031",
                    "004c0072004f004f00420045002d003000310032003300340035003600370038003900610062",
                ])),
                StartupRoute::InternalNativeHelper
            );
            assert_eq!(
                classify_startup_route(&args(&[
                    "LetRecovery.exe",
                    switch,
                    "004c005200410064006d0069006e00310031",
                ])),
                StartupRoute::UnsupportedArguments
            );
        }
        assert_eq!(
            classify_startup_route(&args(&[
                "LetRecovery.exe",
                "--internal-begin-builtin-administrator-transition-with-personal-restore",
                "004c005200410064006d0069006e00310031",
                "004c0072004f004f00420045002d003000310032003300340035003600370038003900610062",
                "0123456789abcdef0123456789abcdef",
            ])),
            StartupRoute::InternalNativeHelper
        );
        assert_eq!(
            classify_startup_route(&args(&[
                "LetRecovery.exe",
                "--internal-begin-builtin-administrator-transition-with-personal-restore",
                "004c005200410064006d0069006e00310031",
                "004c0072004f004f00420045002d003000310032003300340035003600370038003900610062",
            ])),
            StartupRoute::UnsupportedArguments
        );
    }

    #[test]
    fn temporary_oobe_cleanup_route_requires_exact_private_arity() {
        assert_eq!(
            classify_startup_route(&args(&[
                "LetRecovery.exe",
                "--internal-cleanup-disabled-defaultuser0",
            ])),
            StartupRoute::InternalNativeHelper
        );
        assert_eq!(
            classify_startup_route(&args(&[
                "LetRecovery.exe",
                "--internal-cleanup-disabled-defaultuser0",
                "unexpected",
            ])),
            StartupRoute::UnsupportedArguments
        );
        assert_eq!(
            classify_startup_route(&args(&[
                "LetRecovery.exe",
                "--internal-delete-temporary-oobe-account",
                "004c0072004f004f00420045002d003000310032003300340035003600370038003900610062",
            ])),
            StartupRoute::InternalNativeHelper
        );
        assert_eq!(
            classify_startup_route(&args(&[
                "LetRecovery.exe",
                "--internal-delete-temporary-oobe-account",
            ])),
            StartupRoute::UnsupportedArguments
        );
    }

    #[test]
    fn retired_pe_switches_are_case_insensitive_rejections_and_require_exact_arity() {
        assert_eq!(
            classify_startup_route(&args(&["LetRecovery.exe", "/pebackup"])),
            StartupRoute::RejectedLegacyCli
        );
        assert_eq!(
            classify_startup_route(&args(&["LetRecovery.exe", "/peinstall", "extra"])),
            StartupRoute::UnsupportedArguments
        );
    }

    #[test]
    fn only_plain_production_gui_launch_requests_elevation() {
        let gui = classify_startup_route(&args(&["LetRecovery.exe"]));
        assert_eq!(gui, StartupRoute::Gui);
        assert!(should_request_gui_elevation(gui, false, false));
        assert!(!should_request_gui_elevation(gui, true, false));
        assert!(!should_request_gui_elevation(gui, false, true));
        let unknown = classify_startup_route(&args(&["LetRecovery.exe", "--unknown"]));
        assert_eq!(unknown, StartupRoute::UnsupportedArguments);
        assert!(!should_request_gui_elevation(unknown, false, false));
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn personal_restore_progress_preview_is_a_non_elevated_native_route() {
        let route = classify_startup_route(&args(&[
            "LetRecovery.exe",
            "--ui-personal-restore-progress-preview",
        ]));
        assert_eq!(route, StartupRoute::InternalNativeHelper);
        assert!(!should_request_gui_elevation(route, false, true));
        assert_eq!(
            classify_startup_route(&args(&[
                "LetRecovery.exe",
                "--ui-personal-restore-progress-preview",
                "extra",
            ])),
            StartupRoute::UnsupportedArguments
        );
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn progress_preview_is_an_explicit_non_elevated_gui_route() {
        let route = classify_startup_route(&args(&["LetRecovery.exe", "--ui-progress-preview"]));
        assert_eq!(route, StartupRoute::Gui);
        assert!(!should_request_gui_elevation(route, false, true));
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn pe_maintenance_preview_is_an_explicit_non_elevated_gui_route() {
        let route =
            classify_startup_route(&args(&["LetRecovery.exe", "--ui-pe-maintenance-preview"]));
        assert_eq!(route, StartupRoute::Gui);
        assert!(!should_request_gui_elevation(route, false, true));
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn about_preview_is_an_explicit_non_elevated_gui_route() {
        let route = classify_startup_route(&args(&["LetRecovery.exe", "--ui-about-preview"]));
        assert_eq!(route, StartupRoute::Gui);
        assert!(!should_request_gui_elevation(route, false, true));
    }
}
