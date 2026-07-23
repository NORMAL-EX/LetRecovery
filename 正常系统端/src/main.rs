#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(dead_code)]

mod build_info;
mod core;
mod download;
mod native_ui;
mod utils;

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

fn main() -> anyhow::Result<()> {
    // 加载应用配置（用于获取日志设置）
    let app_config = core::app_config::AppConfig::load();

    // 初始化日志系统
    if let Err(e) = utils::logger::LogManager::init(app_config.log_enabled) {
        eprintln!("日志系统初始化失败: {}", e);
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

    log::info!("LetRecovery 启动中...");

    // 检查命令行参数，处理PE环境下的自动安装/备份
    let args: Vec<String> = std::env::args().collect();

    // 该 feature 只用于无副作用的 UI/单元测试。即使调用者传入正式操作参数，
    // 也不能在非管理员开发构建中进入安装、备份或 PE 工作流。
    #[cfg(feature = "non-elevated-tests")]
    if args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "/INSTALL" | "--install" | "/PEINSTALL" | "--pe-install" | "/PEBACKUP" | "--pe-backup"
        )
    }) {
        log::error!("开发 UI 测试构建拒绝执行安装、备份或 PE 命令行入口");
        return Ok(());
    }

    // 开发 UI 测试构建必须保持 asInvoker，避免每次视觉迭代都弹 UAC。
    // build.rs 已拒绝 release + non-elevated-tests，因此正式产物仍强制管理员权限。
    #[cfg(not(feature = "non-elevated-tests"))]
    {
        if !utils::privilege::is_admin() {
            log::warn!("需要管理员权限，正在尝试提升权限...");
            if let Err(e) = utils::privilege::restart_as_admin() {
                log::error!("提升权限失败: {}", e);
                log::error!("需要管理员权限运行此程序");
                show_error_message(&format!("无法获取管理员权限：{e}"));
            }
            return Ok(());
        }
        log::info!("已获得管理员权限");
    }

    #[cfg(feature = "non-elevated-tests")]
    log::warn!("开发 UI 测试构建：已跳过管理员检测和自动提权");

    // Legacy PE automation remains supported, but it must obey the same elevation boundary as
    // every other disk-mutating command-line entry.
    if args.contains(&"/PEINSTALL".to_string()) || args.contains(&"--pe-install".to_string()) {
        log::info!("检测到PE安装模式，执行自动安装...");
        return run_pe_install();
    }

    if args.contains(&"/PEBACKUP".to_string()) || args.contains(&"--pe-backup".to_string()) {
        log::info!("检测到PE备份模式，执行自动备份...");
        return run_pe_backup();
    }

    // 命令行无人值守安装：--install --config <install.json> [--advanced <advanced.json>]
    // 放在确认管理员权限之后、GUI 初始化之前；不进 GUI，准备好后（默认）重启进 PE 完成安装。
    if args.contains(&"/INSTALL".to_string()) || args.contains(&"--install".to_string()) {
        let config = arg_value(&args, &["--config", "/CONFIG"]);
        let advanced = arg_value(&args, &["--advanced", "/ADVANCED"]);
        return run_cli_install_entry(config.as_deref(), advanced.as_deref());
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
    if args.iter().any(|arg| arg == "--ui-preview")
        || std::env::var_os("LETRECOVERY_UI_SKIP_PRELOAD").is_some()
    {
        // Deterministic visual-regression entry: bypass single-instance state and vendor
        // WMI/SetupAPI providers, but retain the real config, native controls and message loop.
        // This branch is absent from release builds and the dangerous CLI guard has already run.
        if let Err(error) = utils::dprk_easter_egg::sync_for_language(&app_config.language) {
            log::warn!("同步朝鲜文彩蛋失败: {error:#}");
        }
        let run_result = native_ui::run(Arc::new(PreloadedConfig {
            app_config: app_config.clone(),
            remote_config: None,
            system_info: None,
            hardware_info: None,
            partitions: Vec::new(),
            pca_firmware_receiver: Mutex::new(None),
        }));
        utils::dprk_easter_egg::shutdown();
        run_result?;
        return Ok(());
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
        let partitions = core::disk::DiskManager::get_partitions().unwrap_or_default();
        log::info!("分区信息获取完成: {} 个分区", partitions.len());
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

/// 收集并记录本机配置信息到日志（便于用户反馈问题时排查）
fn log_machine_info() {
    log::info!("========== 本机配置信息 ==========");
    let sys_info = core::system_info::SystemInfo::collect().ok();
    match core::hardware_info::HardwareInfo::collect() {
        Ok(hw) => {
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

/// 检查系统核心组件完整性（用于检测极限精简系统）
/// 返回 Ok(()) 表示所有组件存在，Err(Vec<String>) 包含缺失的组件列表
fn check_system_components() -> Result<(), Vec<String>> {
    // 获取系统盘路径 (通过 SYSTEMROOT 环境变量，通常为 C:\Windows)
    let system_root = std::env::var("SYSTEMROOT")
        .or_else(|_| std::env::var("WINDIR"))
        .unwrap_or_else(|_| "C:\\Windows".to_string());

    let system32_path = std::path::Path::new(&system_root).join("System32");

    // 必需的系统组件列表
    // 注：WIM 处理已改用内置的 libwim-15.dll，不再依赖系统 wimgapi.dll
    let required_components = [
        ("diskpart.exe", "磁盘分区工具"),
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
/// 从参数列表取某个带值参数的值，支持 `--name value` 与 `--name=value`（名称大小写不敏感）。
fn arg_value(args: &[String], names: &[&str]) -> Option<String> {
    for (i, a) in args.iter().enumerate() {
        for name in names {
            if a.eq_ignore_ascii_case(name) {
                return args.get(i + 1).cloned();
            }
            let prefix = format!("{}=", name);
            if a.len() >= prefix.len() && a[..prefix.len()].eq_ignore_ascii_case(&prefix) {
                return Some(a[prefix.len()..].to_string());
            }
        }
    }
    None
}

/// `--install` 入口：校验参数后调用命令行无人值守安装。
fn run_cli_install_entry(config: Option<&str>, advanced: Option<&str>) -> anyhow::Result<()> {
    let config = match config {
        Some(c) if !c.is_empty() => c,
        _ => {
            log::error!("[CLI INSTALL] 缺少 --config <install.json>");
            log::error!(
                "用法: LetRecovery.exe --install --config <install.json> [--advanced <advanced.json>]"
            );
            return Ok(());
        }
    };
    if let Err(e) = core::cli_install::run_cli_install(config, advanced) {
        log::error!("[CLI INSTALL] 失败: {:#}", e);
    }
    Ok(())
}

fn run_pe_install() -> anyhow::Result<()> {
    use core::install_config::ConfigFileManager;

    log::info!("[PE INSTALL] ========== PE自动安装模式 ==========");

    let (data_partition, target_partition, config) = match ConfigFileManager::find_install_task() {
        Ok(task) => task,
        Err(error) => {
            log::error!("[PE INSTALL] 无法确认安装任务: {error}");
            show_error_message(&format!("无法确认本次安装任务: {error}"));
            return Ok(());
        }
    };

    log::info!("[PE INSTALL] 数据分区: {}", data_partition);

    log::info!("[PE INSTALL] 目标分区: {}", config.target_partition);
    log::info!("[PE INSTALL] 镜像文件: {}", config.image_path);

    // The staged image is a single file name. Reject absolute paths and traversal from a
    // modified/stale INI before any image verification or target-volume write can begin.
    if let Err(error) = lr_core::download_integrity::validate_download_filename(&config.image_path)
    {
        log::error!("[PE INSTALL] 错误: 无效的镜像文件名: {error}");
        show_error_message(&format!("安装配置中的镜像文件名无效: {error}"));
        return Ok(());
    }

    // 构建完整镜像路径
    let data_dir = ConfigFileManager::get_data_dir(&data_partition);
    let image_path = std::path::Path::new(&data_dir)
        .join(&config.image_path)
        .to_string_lossy()
        .into_owned();

    if !std::path::Path::new(&image_path).exists() {
        log::error!("[PE INSTALL] 错误: 镜像文件不存在: {}", image_path);
        show_error_message(&format!("镜像文件不存在: {}", image_path));
        return Ok(());
    }

    log::info!("[PE INSTALL] 完整镜像路径: {}", image_path);

    // 执行安装
    let result = execute_pe_install(&target_partition, &image_path, &config, &data_dir);

    // 清理标记文件
    ConfigFileManager::cleanup_partition_markers(&target_partition);

    match result {
        Ok(_) => {
            log::info!("[PE INSTALL] 安装完成!");
            if config.auto_reboot {
                log::info!("[PE INSTALL] 即将重启...");
                let _ = utils::cmd::create_command("shutdown")
                    .args([
                        "/r",
                        "/t",
                        "10",
                        "/c",
                        "LetRecovery 系统安装完成，即将重启...",
                    ])
                    .spawn();
            } else {
                show_success_message("系统安装完成！请手动重启计算机。");
            }
        }
        Err(e) => {
            log::error!("[PE INSTALL] 安装失败: {}", e);
            show_error_message(&format!("系统安装失败: {}", e));
        }
    }

    Ok(())
}

/// PE环境下自动执行备份
fn run_pe_backup() -> anyhow::Result<()> {
    use core::install_config::ConfigFileManager;

    log::info!("[PE BACKUP] ========== PE自动备份模式 ==========");

    // 查找配置文件所在分区
    let data_partition = match ConfigFileManager::find_data_partition() {
        Some(p) => p,
        None => {
            log::error!("[PE BACKUP] 错误: 未找到备份配置文件");
            show_error_message("未找到备份配置文件，无法继续备份。");
            return Ok(());
        }
    };

    log::info!("[PE BACKUP] 数据分区: {}", data_partition);

    // 读取备份配置
    let config = match ConfigFileManager::read_backup_config(&data_partition) {
        Ok(c) => c,
        Err(e) => {
            log::error!("[PE BACKUP] 错误: 读取配置失败: {}", e);
            show_error_message(&format!("读取备份配置失败: {}", e));
            return Ok(());
        }
    };

    log::info!("[PE BACKUP] 源分区: {}", config.source_partition);
    log::info!("[PE BACKUP] 保存路径: {}", config.save_path);

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
            log::info!("[PE BACKUP] 备份完成!");
            show_success_message(&format!("系统备份完成！\n保存位置: {}", config.save_path));
        }
        Err(e) => {
            log::error!("[PE BACKUP] 备份失败: {}", e);
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
    use lr_core::command::{CommandExecutor, CommandRequest, SystemCommandExecutor};

    log::info!("[PE INSTALL] Step 0: 格式化前校验镜像");
    let verification = core::image_verify::ImageVerifier::new().verify(image_path, None);
    if verification.status != core::image_verify::VerifyStatus::Valid {
        anyhow::bail!("镜像校验失败，未格式化目标分区: {}", verification.message);
    }

    log::info!("[PE INSTALL] Step 1: 格式化分区");
    // 格式化目标分区
    let spec = lr_core::format_command::FormatCommandSpec::new(target_partition, "NTFS", None)
        .map_err(|error| anyhow::anyhow!("无效的格式化参数: {error}"))?;
    let args = spec.args();
    let mut cmd_args = vec!["/d", "/s", "/c", "format.com"];
    cmd_args.extend(args.iter().map(String::as_str));
    // WinPE compatibility path: retain cmd.exe because some PE format.com builds
    // do not exit when invoked directly with CREATE_NO_WINDOW.
    let request = CommandRequest::new("cmd").args(&cmd_args);
    let output = SystemCommandExecutor
        .execute(&request)
        .context("执行格式化命令失败")?;
    let stdout = utils::encoding::gbk_to_utf8(output.stdout());
    let stderr = utils::encoding::gbk_to_utf8(output.stderr());

    if lr_core::format_command::output_indicates_error(output.succeeded(), &stdout, &stderr) {
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        anyhow::bail!("格式化分区失败: {detail}");
    }

    log::info!("[PE INSTALL] Step 2: 释放镜像");
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

    log::info!("[PE INSTALL] Step 3: 导入驱动");
    // 导入驱动
    if config.restore_drivers {
        let driver_path = format!("{}\\drivers", data_dir);
        if std::path::Path::new(&driver_path).exists() {
            let dism = core::dism::Dism::new();
            let _ = dism.add_drivers_offline(&apply_dir, &driver_path);
        }
    }

    log::info!("[PE INSTALL] Step 4: 修复引导");
    // 修复引导
    let boot_manager = core::bcdedit::BootManager::new();
    let use_uefi = match config.boot_mode {
        1 => true,
        2 => false,
        _ => {
            let target_style =
                core::disk::DiskManager::get_partitions()
                    .ok()
                    .and_then(|partitions| {
                        partitions
                            .into_iter()
                            .find(|partition| {
                                partition.letter.eq_ignore_ascii_case(target_partition)
                            })
                            .map(|partition| partition.partition_style)
                    });
            match target_style {
                Some(core::disk::PartitionStyle::GPT) => true,
                Some(core::disk::PartitionStyle::MBR) => false,
                _ => detect_uefi_mode(),
            }
        }
    };
    // XP/2003 判定：配置标记 或 释放后缺少 \Windows\Boot（仅 Vista+ 才有）
    let is_xp = config.is_xp
        || !std::path::Path::new(&format!("{}\\Windows\\Boot", target_partition)).exists();
    if is_xp {
        if use_uefi {
            log::info!("[PE INSTALL] XP/2003 + UEFI，写入 XP UEFI/GPT 引导");
            if let Err(e) = boot_manager.write_xp_uefi_gpt_boot(target_partition) {
                log::warn!("[PE INSTALL] XP UEFI 引导失败({})，回退 Legacy(ntldr)", e);
                boot_manager.write_xp_boot(target_partition)?;
            }
        } else {
            log::info!("[PE INSTALL] XP/2003(Legacy)，写入 XP 引导(ntldr/boot.ini)");
            boot_manager.write_xp_boot(target_partition)?;
        }
    } else {
        boot_manager.repair_boot_advanced(target_partition, use_uefi, config.boot_pca_mode)?;
    }

    log::info!("[PE INSTALL] Step 5: 应用高级选项");
    // 应用高级选项
    let advanced_options = core::advanced_options::AdvancedOptions {
        remove_shortcut_arrow: config.remove_shortcut_arrow,
        restore_classic_context_menu: config.restore_classic_context_menu,
        bypass_nro: config.bypass_nro,
        disable_windows_update: config.disable_windows_update,
        disable_windows_defender: config.disable_windows_defender,
        disable_reserved_storage: config.disable_reserved_storage,
        disable_uac: config.disable_uac,
        disable_device_encryption: config.disable_device_encryption,
        remove_uwp_apps: config.remove_uwp_apps,
        import_storage_controller_drivers: config.import_storage_controller_drivers,
        custom_username: !config.custom_username.is_empty(),
        username: config.custom_username.clone(),
        xp_inject_usb3_driver: config.xp_inject_usb3_driver,
        xp_inject_nvme_driver: config.xp_inject_nvme_driver,
        ..core::advanced_options::AdvancedOptions::default()
    };

    if let Err(error) = advanced_options.apply_to_system(target_partition, is_xp) {
        if config.disable_windows_defender {
            return Err(error);
        }
        log::warn!("[PE INSTALL] 应用高级选项失败: {}", error);
    }

    // 生成无人值守配置
    if config.unattended {
        generate_unattend_xml_pe(target_partition, &config.custom_username)
            .context("[PE INSTALL] 生成无人值守配置失败")?;
    }

    log::info!("[PE INSTALL] Step 6: 清理临时文件");
    // 清理数据目录
    let _ = std::fs::remove_dir_all(data_dir);

    Ok(())
}

/// 执行PE备份（按格式分发：0=WIM,1=ESD,2=SWM,3=GHO）。
/// 此前恒走 LZX WIM，忽略 format/swm —— ESD/SWM/GHO 都会产出错误文件。
fn execute_pe_backup(
    source_partition: &str,
    config: &core::install_config::BackupConfig,
) -> anyhow::Result<()> {
    let capture_dir = format!("{}\\", source_partition);

    match config.format {
        3 => {
            let ghost = core::ghost::Ghost::new();
            if !ghost.is_available() {
                anyhow::bail!("Ghost 工具不可用");
            }
            ghost.create_image_from_letter(source_partition, &config.save_path, None)
        }
        1 => {
            let dism = core::dism::Dism::new();
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
        2 => {
            let dism = core::dism::Dism::new();
            dism.capture_image_swm(
                &config.save_path,
                &capture_dir,
                &config.name,
                &config.description,
                config.swm_split_size,
                None,
            )
        }
        _ => {
            let dism = core::dism::Dism::new();
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
            true
        }
    }

    #[cfg(not(windows))]
    false
}

/// 生成无人值守XML (PE版本)
fn generate_unattend_xml_pe(target_partition: &str, username: &str) -> anyhow::Result<()> {
    use crate::core::system_utils::{get_file_version, get_system_architecture};
    use anyhow::Context;
    use std::path::Path;

    let username = if username.is_empty() {
        "User"
    } else {
        username
    };
    let username = escape_xml_text(username);

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
    <settings pass="oobeSystem">
{international_component}
        <component name="Microsoft-Windows-Shell-Setup" processorArchitecture="{arch}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
{time_zone}
            {oobe}
            <UserAccounts>
                <LocalAccounts>
                    <LocalAccount wcm:action="add">
                        <Password>
                            <Value></Value>
                            <PlainText>true</PlainText>
                        </Password>
                        <Description>Local User</Description>
                        <DisplayName>{user}</DisplayName>
                        <Group>Administrators</Group>
                        <Name>{user}</Name>
                    </LocalAccount>
                </LocalAccounts>
            </UserAccounts>
            <AutoLogon>
                <Password>
                    <Value></Value>
                    <PlainText>true</PlainText>
                </Password>
                <Enabled>true</Enabled>
                <Username>{user}</Username>
            </AutoLogon>
        </component>
    </settings>
</unattend>"#,
        arch = arch_str,
        international_component = international_component,
        time_zone = time_zone,
        oobe = oobe_section,
        user = username
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
