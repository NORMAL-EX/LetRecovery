#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::Context;

mod app;
mod core;
#[cfg(target_os = "windows")]
pub mod native_ui;
mod ui;
mod utils;
mod workflow_journal;
mod workflows;

static ACTIVE_LOG_PATH: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
static ACTIVE_INSTALL_LOG_SESSION: std::sync::OnceLock<std::sync::Mutex<Option<String>>> =
    std::sync::OnceLock::new();
static LAST_PUBLISHED_INSTALL_LOG: std::sync::OnceLock<
    std::sync::Mutex<Option<std::path::PathBuf>>,
> = std::sync::OnceLock::new();

/// The packaged executable can run from a read-only ISO. Try writable WinPE locations in a stable
/// order and remember the exact file selected so the final handoff never guesses a different path.
fn log_file_candidates() -> Vec<std::path::PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            candidates.push(directory.join(lr_core::install_log_handoff::PE_LOG_FILE));
        }
    }
    candidates.push(std::path::PathBuf::from(format!(
        "X:\\{}",
        lr_core::install_log_handoff::PE_LOG_FILE
    )));
    candidates.push(std::env::temp_dir().join(lr_core::install_log_handoff::PE_LOG_FILE));
    if let Ok(directory) = std::env::current_dir() {
        candidates.push(directory.join(lr_core::install_log_handoff::PE_LOG_FILE));
    }
    candidates.dedup_by(|left, right| {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    });
    candidates
}

fn active_log_path() -> Option<&'static std::path::PathBuf> {
    ACTIVE_LOG_PATH.get()
}

fn runtime_log_directory() -> Option<std::path::PathBuf> {
    active_log_path()
        .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
        .or_else(pe_program_directory)
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
            let _ = f.sync_all();
        }
    }
}

/// 初始化日志：自实现的文件日志器（每条 flush）。文件打不开时静默跳过，不影响启动。
fn init_file_logger() {
    let mut selected = None;
    for path in log_file_candidates() {
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(file) => {
                selected = Some((path, file));
                break;
            }
            Err(_) => continue,
        }
    }
    let Some((path, file)) = selected else {
        return;
    };
    let logger = Box::new(FileLogger {
        file: std::sync::Mutex::new(file),
        level: log::LevelFilter::Info,
    });
    if log::set_boxed_logger(logger).is_ok() {
        let _ = ACTIVE_LOG_PATH.set(path);
        log::set_max_level(log::LevelFilter::Info);
    }
}

fn pe_program_directory() -> Option<std::path::PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
}

pub(crate) fn copy_desktop_install_log_into_pe(data_partition: &str, session_id: &str) {
    if let Ok(mut active) = ACTIVE_INSTALL_LOG_SESSION
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
    {
        *active = Some(session_id.to_owned());
    }
    let Some(pe_directory) = runtime_log_directory() else {
        log::warn!("[INSTALL LOG] 无法确定 PE 可写日志目录，跳过正常端日志中继");
        return;
    };
    let data_directory = crate::core::config::ConfigFileManager::get_data_dir(data_partition);
    match lr_core::install_log_handoff::copy_desktop_log_to_pe(
        std::path::Path::new(&data_directory),
        &pe_directory,
        session_id,
    ) {
        Ok(path) => log::info!(
            "[INSTALL LOG] 正常端日志已复制到 PE RAM 盘: {}",
            path.display()
        ),
        Err(error) => {
            log::warn!("[INSTALL LOG] 正常端日志中继失败；安装继续，不弹出提示: {error:#}")
        }
    }
}

fn remember_published_install_log(path: &std::path::Path) {
    if let Ok(mut published) = LAST_PUBLISHED_INSTALL_LOG
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
    {
        *published = Some(path.to_owned());
    }
}

fn publish_install_log_to_root(output_root: &std::path::Path, session_id: &str, label: &str) {
    let Some(pe_directory) = runtime_log_directory() else {
        log::warn!("[INSTALL LOG] 无法确定 PE 可写日志目录，跳过最终日志合并");
        return;
    };
    log::info!("[INSTALL LOG] 开始发布正常端与 PE 端合并日志");
    log::logger().flush();
    let desktop = pe_directory.join(format!("NormalEndpoint.{session_id}.log"));
    let pe_log = active_log_path()
        .cloned()
        .unwrap_or_else(|| pe_directory.join(lr_core::install_log_handoff::PE_LOG_FILE));
    match lr_core::install_log_handoff::publish_combined_install_log(
        desktop.is_file().then_some(desktop.as_path()),
        pe_log.is_file().then_some(pe_log.as_path()),
        output_root,
        session_id,
    ) {
        Ok(path) => {
            remember_published_install_log(&path);
            log::info!("[INSTALL LOG] 合并日志已发布到{label}: {}", path.display());
        }
        Err(error) => {
            log::warn!("[INSTALL LOG] 最终日志合并失败；安装完成状态和重启不受影响: {error:#}")
        }
    }
}

/// Returns a flush-complete normal+PE diagnostic for the terminal error prompt.
///
/// Destructive install paths normally already published the same combined file to the verified
/// target/data volume. Pre-write failures have no armed target finalizer, so they receive a
/// RAM-disk combined copy here. This diagnostic fallback must never change the workflow result.
pub(crate) fn prepare_failure_log_for_ui() -> Option<std::path::PathBuf> {
    log::logger().flush();
    if let Some(path) = LAST_PUBLISHED_INSTALL_LOG
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .ok()
        .and_then(|published| published.clone())
        .filter(|path| path.is_file())
    {
        return Some(path);
    }

    let pe_directory = runtime_log_directory()?;
    let session_id = ACTIVE_INSTALL_LOG_SESSION
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .ok()
        .and_then(|active| active.clone())
        .unwrap_or_else(|| format!("pe-runtime-{}", std::process::id()));
    let desktop = pe_directory.join(format!("NormalEndpoint.{session_id}.log"));
    let pe_log = active_log_path()
        .cloned()
        .unwrap_or_else(|| pe_directory.join(lr_core::install_log_handoff::PE_LOG_FILE));
    let output_root = pe_directory.join("FailureReport");
    match lr_core::install_log_handoff::publish_combined_install_log(
        desktop.is_file().then_some(desktop.as_path()),
        pe_log.is_file().then_some(pe_log.as_path()),
        &output_root,
        &session_id,
    ) {
        Ok(path) => {
            remember_published_install_log(&path);
            Some(path)
        }
        Err(error) => {
            log::error!("[INSTALL LOG] 无法为错误弹窗生成合并日志: {error:#}");
            pe_log.is_file().then_some(pe_log)
        }
    }
}

#[cfg(feature = "ci-automation")]
static CI_TERMINAL_FINALIZED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(feature = "ci-automation")]
static CI_AUTHENTICATED_RUN_CONTEXT: std::sync::OnceLock<CiRunContext> = std::sync::OnceLock::new();

#[cfg(feature = "ci-automation")]
#[derive(Clone, Debug, Eq, PartialEq)]
struct CiRunContext {
    run_id: String,
    started_utc: String,
    result_directory: std::path::PathBuf,
    fault_injection: Option<CiFaultInjection>,
}

#[cfg(feature = "ci-automation")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CiFaultInjection {
    BeforeTargetWrite,
    AfterTargetFormat,
}

#[cfg(feature = "ci-automation")]
impl CiFaultInjection {
    fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "before_target_write" => Ok(Self::BeforeTargetWrite),
            "after_target_format" => Ok(Self::AfterTargetFormat),
            _ => anyhow::bail!("unsupported CI fault injection: {value}"),
        }
    }
}

#[cfg(feature = "ci-automation")]
fn valid_ci_run_id(value: &str) -> bool {
    (32..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
}

#[cfg(all(feature = "ci-automation", windows))]
fn ci_metadata_is_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT.0
        != 0
}

#[cfg(all(feature = "ci-automation", not(windows)))]
fn ci_metadata_is_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(feature = "ci-automation")]
fn ci_plain_directory(path: &std::path::Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.is_dir() && !ci_metadata_is_reparse(&metadata))
        .unwrap_or(false)
}

#[cfg(feature = "ci-automation")]
fn find_ci_run_context_in_roots<I>(roots: I, expected_run_id: &str) -> anyhow::Result<CiRunContext>
where
    I: IntoIterator<Item = std::path::PathBuf>,
{
    if !valid_ci_run_id(expected_run_id) {
        anyhow::bail!("authenticated CI run id is invalid");
    }
    let mut matches = Vec::new();
    for root in roots {
        let ci_root = root.join("LR-CI");
        let state_root = ci_root.join("state");
        if !ci_plain_directory(&ci_root) || !ci_plain_directory(&state_root) {
            continue;
        }
        let active_path = state_root.join("active-run.json");
        let metadata = match std::fs::symlink_metadata(&active_path) {
            Ok(metadata)
                if metadata.is_file()
                    && !ci_metadata_is_reparse(&metadata)
                    && metadata.len() <= 64 * 1024 =>
            {
                metadata
            }
            _ => continue,
        };
        let _ = metadata;
        let bytes = match std::fs::read(&active_path) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let value: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            != Some(1)
            || value.get("phase").and_then(serde_json::Value::as_str) != Some("handoff_committed")
        {
            continue;
        }
        let Some(run_id) = value.get("run_id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if !valid_ci_run_id(run_id) {
            continue;
        }
        if !run_id.eq_ignore_ascii_case(expected_run_id) {
            continue;
        }
        let started_utc = value
            .get("updated_utc")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned();
        matches.push(CiRunContext {
            run_id: run_id.to_ascii_lowercase(),
            started_utc,
            result_directory: ci_root.join("results").join(run_id),
            fault_injection: None,
        });
    }
    if matches.len() != 1 {
        anyhow::bail!(
            "CI PE terminal publication requires one active handoff record, found {}",
            matches.len()
        );
    }
    Ok(matches.remove(0))
}

#[cfg(feature = "ci-automation")]
fn find_ci_install_context_in_roots<I>(
    roots: I,
    expected_session_id: &str,
) -> anyhow::Result<Option<CiRunContext>>
where
    I: IntoIterator<Item = std::path::PathBuf>,
{
    lr_core::handoff_auth::validate_session_id(expected_session_id)?;
    let mut matches = Vec::new();
    for root in roots {
        let ci_root = root.join("LR-CI");
        let state_root = ci_root.join("state");
        if !ci_plain_directory(&ci_root) || !ci_plain_directory(&state_root) {
            continue;
        }
        let active_path = state_root.join("active-run.json");
        let metadata = match std::fs::symlink_metadata(&active_path) {
            Ok(metadata)
                if metadata.is_file()
                    && !ci_metadata_is_reparse(&metadata)
                    && metadata.len() <= 64 * 1024 =>
            {
                metadata
            }
            _ => continue,
        };
        let _ = metadata;
        let bytes = match std::fs::read(&active_path) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let value: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            != Some(1)
            || value.get("phase").and_then(serde_json::Value::as_str) != Some("handoff_committed")
            || value.get("session_id").and_then(serde_json::Value::as_str)
                != Some(expected_session_id)
        {
            continue;
        }
        let run_id = value
            .get("run_id")
            .and_then(serde_json::Value::as_str)
            .context("matching CI install fault record has no run id")?;
        if !valid_ci_run_id(run_id) {
            anyhow::bail!("matching CI install fault record has an invalid run id");
        }
        let fault_name = value
            .get("fault_injection")
            .and_then(serde_json::Value::as_str)
            .context("matching CI install record has no fault injection")?;
        let fault_injection = if fault_name == "none" {
            // A session-bound active record is also the durable identity for successful install
            // matrices (including driver recovery). `none` is the explicit no-fault value, not an
            // unknown fault name. Unique SessionId matching above remains the authentication gate.
            None
        } else {
            Some(CiFaultInjection::parse(fault_name)?)
        };
        let started_utc = value
            .get("updated_utc")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned();
        matches.push(CiRunContext {
            run_id: run_id.to_ascii_lowercase(),
            started_utc,
            result_directory: ci_root.join("results").join(run_id),
            fault_injection,
        });
    }
    if matches.len() > 1 {
        anyhow::bail!(
            "CI install fault injection requires at most one session-bound active record, found {}",
            matches.len()
        );
    }
    Ok(matches.pop())
}

#[cfg(feature = "ci-automation")]
pub(crate) fn register_ci_authenticated_backup_context(backup_name: &str) {
    let Some(run_id) = backup_name.strip_prefix("LR-CI-") else {
        return;
    };
    if !valid_ci_run_id(run_id) {
        log::error!("[CI AUTOMATION] authenticated backup name contains an invalid run id");
        return;
    }
    let context = (|| -> anyhow::Result<CiRunContext> {
        let roots = lr_core::windows_storage::volume_guid_paths()
            .context("enumerate volumes for authenticated CI run binding")?
            .into_iter()
            .map(std::path::PathBuf::from);
        find_ci_run_context_in_roots(roots, run_id)
    })();
    match context {
        Ok(context) => {
            if let Err(rejected) = CI_AUTHENTICATED_RUN_CONTEXT.set(context.clone()) {
                if CI_AUTHENTICATED_RUN_CONTEXT.get() != Some(&rejected) {
                    log::error!(
                        "[CI AUTOMATION] refusing to replace the authenticated CI run context"
                    );
                }
            } else {
                log::info!(
                    "[CI AUTOMATION] bound authenticated backup task to run_id={}",
                    context.run_id
                );
            }
        }
        Err(error) => {
            log::error!("[CI AUTOMATION] authenticated CI run binding failed: {error:#}")
        }
    }
}

#[cfg(feature = "ci-automation")]
pub(crate) fn register_ci_authenticated_install_context(session_id: &str) -> anyhow::Result<()> {
    let roots = lr_core::windows_storage::volume_guid_paths()?
        .into_iter()
        .map(std::path::PathBuf::from);
    let Some(context) = find_ci_install_context_in_roots(roots, session_id)? else {
        return Ok(());
    };
    if let Err(rejected) = CI_AUTHENTICATED_RUN_CONTEXT.set(context.clone()) {
        if CI_AUTHENTICATED_RUN_CONTEXT.get() != Some(&rejected) {
            anyhow::bail!("refusing to replace the authenticated CI run context");
        }
    } else if let Some(fault) = context.fault_injection {
        log::warn!(
            "[CI AUTOMATION] armed session-bound install fault {:?} for run_id={}",
            fault,
            context.run_id
        );
    } else {
        log::info!(
            "[CI AUTOMATION] bound authenticated successful install run_id={}",
            context.run_id
        );
    }
    Ok(())
}

#[cfg(feature = "ci-automation")]
pub(crate) fn inject_ci_failure_before_target_write() -> anyhow::Result<()> {
    if CI_AUTHENTICATED_RUN_CONTEXT
        .get()
        .and_then(|context| context.fault_injection)
        == Some(CiFaultInjection::BeforeTargetWrite)
    {
        anyhow::bail!(
            "CI fault injection before_target_write: authenticated inputs passed preflight; no target write has started and reversible cleanup is required"
        );
    }
    Ok(())
}

#[cfg(feature = "ci-automation")]
pub(crate) fn inject_ci_failure_after_target_format() -> anyhow::Result<()> {
    if CI_AUTHENTICATED_RUN_CONTEXT
        .get()
        .and_then(|context| context.fault_injection)
        == Some(CiFaultInjection::AfterTargetFormat)
    {
        anyhow::bail!(
            "CI fault injection after_target_format: target format completed; old-system rollback remains disabled"
        );
    }
    Ok(())
}

#[cfg(feature = "ci-automation")]
fn atomic_write_ci_file(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("CI result path has no parent"))?;
    let (temporary, mut file) =
        lr_core::scoped_temp_file::ScopedTempFile::create_writer_in(parent, "lr-ci", "tmp")?;
    file.write_all(contents)?;
    file.flush()?;
    file.sync_all()?;
    drop(file);
    temporary.persist_replace(path)
}

#[cfg(feature = "ci-automation")]
fn copy_ci_log(source: &std::path::Path, destination: &std::path::Path) -> std::io::Result<()> {
    use std::io::Write;
    let parent = destination
        .parent()
        .ok_or_else(|| std::io::Error::other("CI log path has no parent"))?;
    let mut input = std::fs::File::open(source)?;
    let (temporary, mut output) =
        lr_core::scoped_temp_file::ScopedTempFile::create_writer_in(parent, "pe-failure", "tmp")?;
    std::io::copy(&mut input, &mut output)?;
    output.flush()?;
    output.sync_all()?;
    drop(output);
    temporary.persist_replace(destination)
}

#[cfg(feature = "ci-automation")]
fn publish_ci_terminal_failure(
    error_message: &str,
    merged_log: Option<&std::path::Path>,
) -> anyhow::Result<std::path::PathBuf> {
    let context = CI_AUTHENTICATED_RUN_CONTEXT
        .get()
        .cloned()
        .context("no authenticated CI backup context was established")?;
    let results_root = context
        .result_directory
        .parent()
        .context("CI result directory has no results parent")?;
    if results_root.exists() && !ci_plain_directory(results_root) {
        anyhow::bail!("CI results parent is not a plain directory");
    }
    std::fs::create_dir_all(&context.result_directory).context("create CI PE result directory")?;
    if !ci_plain_directory(&context.result_directory) {
        anyhow::bail!("CI run result path is not a plain directory");
    }
    let mut warnings = Vec::new();
    if let Some(log_path) = merged_log {
        if let Err(error) = copy_ci_log(log_path, &context.result_directory.join("pe-failure.log"))
        {
            warnings.push(format!("copy PE failure log: {error}"));
        }
    } else {
        warnings.push("PE failure log was unavailable".to_owned());
    }
    let final_path = context.result_directory.join("final.json");
    let result = serde_json::json!({
        "schema_version": 1,
        "run_id": context.run_id,
        "terminal": true,
        "outcome": "product_failed",
        "stage": "pe",
        "started_utc": context.started_utc,
        "finished_utc": chrono::Utc::now().to_rfc3339(),
        "error": error_message,
        "backup": serde_json::Value::Null,
        "warnings": warnings,
        "shutdown_requested": true,
        "wim_engine": lr_core::active_engine().name(),
        "pe_build": "ci-automation"
    });
    let mut bytes = serde_json::to_vec_pretty(&result)?;
    bytes.push(b'\n');

    atomic_write_ci_file(&final_path, &bytes).context("atomically publish CI final.json")?;
    let readback: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&final_path).context("read back CI final.json")?)?;
    if readback.get("run_id").and_then(serde_json::Value::as_str)
        != result.get("run_id").and_then(serde_json::Value::as_str)
        || readback
            .get("terminal")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
    {
        anyhow::bail!("CI final.json round-trip mismatch");
    }
    Ok(final_path)
}

#[cfg(feature = "ci-automation")]
pub(crate) fn finalize_ci_failure(error_message: &str) {
    use std::sync::atomic::Ordering;
    if CI_TERMINAL_FINALIZED.swap(true, Ordering::SeqCst) {
        return;
    }
    log::error!("[CI AUTOMATION] terminal=failure error={error_message}");
    let merged_log = prepare_failure_log_for_ui();
    match publish_ci_terminal_failure(error_message, merged_log.as_deref()) {
        Ok(path) => log::info!("[CI AUTOMATION] final result published: {}", path.display()),
        Err(error) => log::error!("[CI AUTOMATION] terminal publication failed: {error:#}"),
    }
    log::logger().flush();
    match lr_core::windows_shutdown::schedule_shutdown(
        5,
        "LetRecovery PE CI reached a terminal failure; this disposable VM will power off.",
    ) {
        Ok(()) => log::info!("[CI AUTOMATION] power-off accepted timeout_seconds=5"),
        Err(error) => log::error!("[CI AUTOMATION] power-off request failed: {error:#}"),
    }
}
pub(crate) fn publish_final_install_log(target_partition: &str, session_id: &str) {
    publish_install_log_to_root(
        &std::path::PathBuf::from(format!("{}\\", target_partition)),
        session_id,
        "新系统",
    );
}

#[derive(Debug, Default)]
struct TerminalLogPublishGate {
    pending: bool,
}

impl TerminalLogPublishGate {
    fn armed() -> Self {
        Self { pending: true }
    }

    fn take(&mut self) -> bool {
        std::mem::take(&mut self.pending)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalLogDestination {
    TargetSystem,
    VerifiedDataFallback,
}

fn terminal_log_destination(
    completed: bool,
    target_system_available: bool,
) -> TerminalLogDestination {
    if completed || target_system_available {
        TerminalLogDestination::TargetSystem
    } else {
        TerminalLogDestination::VerifiedDataFallback
    }
}

fn terminal_success_outcome(cleanup_verified: bool) -> &'static str {
    if cleanup_verified {
        "completed cleanup=verified"
    } else {
        "completed cleanup=incomplete"
    }
}

/// One-shot, best-effort terminal log publisher for the target-write phase.
///
/// Construct this immediately before formatting or the first target write. Any
/// later early return publishes a failure outcome from `Drop`; explicit terminal
/// paths publish a more precise outcome and disarm the fallback first.
pub(crate) struct InstallLogTerminalFinalizer {
    target_partition: String,
    data_volume_root: std::path::PathBuf,
    data_directory: std::path::PathBuf,
    expected_data_identity: Option<lr_core::windows_storage::StableVolumeIdentity>,
    session_id: String,
    target_system_available: bool,
    gate: TerminalLogPublishGate,
}

impl InstallLogTerminalFinalizer {
    pub(crate) fn armed(
        target_partition: &str,
        data_volume_root: &std::path::Path,
        data_directory: &std::path::Path,
        session_id: &str,
    ) -> Self {
        let expected_data_identity = match stable_log_volume_identity(data_volume_root) {
            Ok(identity) => Some(identity),
            Err(error) => {
                log::warn!(
                    "[INSTALL LOG] cannot bind failure-log fallback to a stable data volume; fallback publication will be skipped: {error}"
                );
                None
            }
        };
        Self {
            target_partition: target_partition.to_owned(),
            data_volume_root: data_volume_root.to_owned(),
            data_directory: data_directory.to_owned(),
            expected_data_identity,
            session_id: session_id.to_owned(),
            target_system_available: false,
            gate: TerminalLogPublishGate::armed(),
        }
    }

    /// Publish the first target-side checkpoint immediately after the new system exists.
    ///
    /// This is diagnostic-only and must never change the installation result. It closes the
    /// failure window where a later driver/boot/advanced-option error previously left the useful
    /// PE log only on the RAM disk or an unrelated data-volume fallback.
    pub(crate) fn mark_target_system_available(&mut self) {
        if self.target_system_available {
            return;
        }
        self.target_system_available = true;
        log::info!(
            "[INSTALL LOG] target system is available; publishing the first target-side checkpoint"
        );
        publish_final_install_log(&self.target_partition, &self.session_id);
    }

    fn publish_to_verified_data_fallback(&self, label: &str) {
        let actual = match stable_log_volume_identity(&self.data_volume_root) {
            Ok(identity) => Some(identity),
            Err(error) => {
                log::warn!(
                    "[INSTALL LOG] data-volume fallback identity query failed; no fallback log was written: {error}"
                );
                None
            }
        };
        if !lr_core::install_log_handoff::stable_log_destination_matches(
            self.expected_data_identity,
            actual,
        ) {
            log::warn!(
                "[INSTALL LOG] data-volume fallback is missing or no longer maps to the armed stable volume; no fallback log was written"
            );
            return;
        }
        publish_install_log_to_root(&self.data_directory, &self.session_id, label);
    }

    fn publish_once(&mut self, completed: bool, outcome: &str) {
        if !self.gate.take() {
            return;
        }
        if completed {
            log::info!("[PE INSTALL] terminal_outcome={outcome}");
        } else {
            log::error!("[PE INSTALL] terminal_outcome={outcome}");
        }
        if terminal_log_destination(completed, self.target_system_available)
            == TerminalLogDestination::TargetSystem
        {
            // Image and boot deployment have already completed. Re-querying mutable inventory
            // here cannot protect another destructive write and can only create a false failure.
            publish_final_install_log(&self.target_partition, &self.session_id);
        } else {
            // A formatting failure can leave the target unmountable or only partially changed.
            // The data partition is the already-validated diagnostic fallback and is deliberately
            // preserved on every failure path.
            self.publish_to_verified_data_fallback("数据分区失败诊断目录");
        }
    }

    pub(crate) fn finish_success(&mut self, cleanup_verified: bool) {
        self.publish_once(true, terminal_success_outcome(cleanup_verified));
    }
}

fn stable_log_volume_identity(
    data_volume_root: &std::path::Path,
) -> Result<lr_core::windows_storage::StableVolumeIdentity, lr_core::windows_storage::StorageError>
{
    if let Some(letter) = lr_core::windows_storage::path_drive_letter(data_volume_root) {
        lr_core::windows_storage::stable_volume_identity(letter)
    } else {
        let volume_guid_root = data_volume_root.as_os_str().to_string_lossy();
        lr_core::windows_storage::stable_volume_identity_from_guid_path(&volume_guid_root)
    }
}

impl Drop for InstallLogTerminalFinalizer {
    fn drop(&mut self) {
        let outcome = if self.target_system_available {
            "failed stage=post_image_apply_early_return"
        } else {
            "failed stage=target_write_before_image_available"
        };
        self.publish_once(false, outcome);
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
fn load_matching_vmd_driver_into_running_pe() -> anyhow::Result<()> {
    let hardware_ids = lr_core::driver::list_present_hardware_ids()
        .map_err(|error| anyhow::anyhow!("present-device enumeration failed: {error}"))?;
    let packages = lr_core::storage_driver_match::select_builtin_storage_driver_packages(
        hardware_ids.iter().map(String::as_str),
    )
    .map_err(anyhow::Error::new)?;
    if packages.is_empty() {
        log::info!("[VMD/PE] no supported Intel VMD controller is present");
        return Ok(());
    }

    lr_core::driver_trust::ensure_pe_driver_signing_trust()
        .context("initialize WinPE trust before loading the matched VMD package")?;
    log::info!("[VMD/PE] PE driver signing trust is ready");

    let package_root = utils::path::get_exe_dir()
        .join("drivers")
        .join("storage_controller");
    for package in packages {
        let directory = package_root.join(package.directory_name());
        let verified = lr_core::storage_driver_match::verify_builtin_storage_driver_package(
            package, &directory,
        )?;
        let inf = verified.inf_path();

        let request = lr_core::command::CommandRequest::new("drvload.exe").arg(inf);
        match lr_core::command::execute_request(&lr_core::command::SystemCommandExecutor, &request)
        {
            Ok(outcome) if outcome.succeeded() => {
                log::info!("[VMD/PE] runtime VMD driver loaded: {}", inf.display());
            }
            Ok(outcome) => {
                let stdout = lr_core::encoding::decode_windows_console_output(outcome.stdout());
                let stderr = lr_core::encoding::decode_windows_console_output(outcome.stderr());
                let present = lr_core::driver::list_present_devices().map_err(|status_error| {
                    anyhow::anyhow!(
                        "drvload rejected {} (exit {:?}): stdout={} stderr={}; authoritative devnode status query also failed: {status_error}",
                        inf.display(),
                        outcome.exit_code(),
                        stdout.trim(),
                        stderr.trim()
                    )
                })?;
                let (usable, summary) = matched_vmd_controller_runtime_state(package, &present);
                if usable {
                    // DrvLoad returns a nonzero code when it has no driver to select, including an
                    // already-bound controller. The supported runtime fact is the current PnP
                    // devnode state: continue only when every exact generation-defining VMD node
                    // is already started and has no Configuration Manager problem.
                    log::warn!(
                        "[VMD/PE] drvload returned exit {:?}, but the exact matched controller is already operational; continuing: package={:?} state={} stdout={} stderr={}",
                        outcome.exit_code(),
                        package,
                        summary,
                        stdout.trim(),
                        stderr.trim()
                    );
                } else {
                    anyhow::bail!(
                        "drvload rejected {} (exit {:?}) and the matched VMD controller is not operational: state={} stdout={} stderr={}",
                        inf.display(),
                        outcome.exit_code(),
                        summary,
                        stdout.trim(),
                        stderr.trim()
                    );
                }
            }
            Err(error) => {
                anyhow::bail!("failed to start drvload for {}: {error}", inf.display());
            }
        }
    }
    Ok(())
}

fn matched_vmd_controller_runtime_state(
    package: lr_core::storage_driver_match::BuiltInStorageDriverPackage,
    devices: &[lr_core::driver::PresentDeviceState],
) -> (bool, String) {
    let matched = devices
        .iter()
        .filter(|device| package.matches_hardware_ids(&device.hardware_ids))
        .collect::<Vec<_>>();
    let summary = if matched.is_empty() {
        "no matching present devnode".to_owned()
    } else {
        matched
            .iter()
            .map(|device| {
                format!(
                    "ids=[{}] query_cr=0x{:08X} status=0x{:08X} problem={}",
                    device.hardware_ids.join(","),
                    device.status_query_cr,
                    device.devnode_status,
                    device
                        .problem_number
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "none".to_owned())
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    };
    (
        !matched.is_empty()
            && matched
                .iter()
                .all(|device| device.is_started_without_problem()),
        summary,
    )
}

/// 探测界面语言：从（正常系统端随重启写入的）配置文件读取 Language 字段。
/// 找不到数据分区或配置时返回空串（即简体中文内置）。
fn detect_ui_language(guard: &core::config::AuthenticatedOperationGuard) -> String {
    let Ok(text) = std::str::from_utf8(guard.exact_config_bytes()) else {
        return String::new();
    };
    let mut language = None;
    for line in text.lines() {
        let Some(value) = line.strip_prefix("Language=") else {
            continue;
        };
        if language.is_some()
            || value.len() > 32
            || !value
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-')
        {
            return String::new();
        }
        language = Some(value.to_owned());
    }
    language.unwrap_or_default()
}

fn unlock_maintenance_volumes_best_effort(
    guard: &core::config::AuthenticatedOperationGuard,
) -> anyhow::Result<(usize, usize)> {
    use lr_core::command::CommandExecutor as _;

    guard.verify_unchanged()?;
    let Some(secret) = guard.protected_bitlocker_secret_bytes() else {
        return Ok((0, 0));
    };
    let keys = lr_core::bl_passthrough::parse_keys(secret).map_err(anyhow::Error::msg)?;
    let mask = match lr_core::windows_storage::assigned_drive_letter_mask() {
        Ok(mask) => mask,
        Err(error) => {
            log::warn!("[PE MAINTENANCE] 无法枚举盘符，已跳过 BitLocker 解锁: {error}");
            return Ok((0, 0));
        }
    };
    let executor = lr_core::command::SystemCommandExecutor;
    let mut attempted_volumes = 0usize;
    let mut unlocked_volumes = 0usize;
    for index in 0..26_u32 {
        if mask & (1_u32 << index) == 0 {
            continue;
        }
        let drive = format!("{}:", char::from(b'A' + index as u8));
        if !maintenance_volume_may_need_unlock(&drive) {
            continue;
        }
        attempted_volumes += 1;
        for key in keys.iter() {
            // Microsoft documents this exact command as an unlock operation. It does not disable
            // protectors and does not start decryption; secret-bearing arguments are never logged.
            let request = lr_core::command::CommandRequest::new("manage-bde.exe").args([
                "-unlock",
                drive.as_str(),
                "-recoverypassword",
                key.as_str(),
            ]);
            match executor.execute(&request) {
                Ok(outcome) if outcome.succeeded() => {
                    unlocked_volumes += 1;
                    break;
                }
                Ok(_) => {}
                Err(error) => {
                    log::warn!(
                        "[PE MAINTENANCE] manage-bde 无法启动，停止后续自动解锁尝试: {error}"
                    );
                    guard.verify_unchanged()?;
                    return Ok((attempted_volumes, unlocked_volumes));
                }
            }
        }
    }
    guard.verify_unchanged()?;
    Ok((attempted_volumes, unlocked_volumes))
}

fn maintenance_volume_may_need_unlock(drive: &str) -> bool {
    use lr_core::fveapi::{FveApi, FveError, FveLockStatus};

    let Ok(api) = FveApi::instance() else {
        return true;
    };
    match api.get_status_by_path(drive) {
        Ok(info) => info.lock_status == FveLockStatus::Locked,
        Err(FveError::VolumeLocked | FveError::KeyRequired) => true,
        Err(FveError::NotEncrypted | FveError::NotBitLockerVolume | FveError::NotSupported) => {
            false
        }
        Err(_) => true,
    }
}

fn remain_in_hidden_pe_maintenance() -> ! {
    log::info!("[PE MAINTENANCE] 自动解锁阶段结束；LetRecovery 窗口保持隐藏，PE 桌面可供维护");
    loop {
        std::thread::park();
    }
}

fn is_removed_pe_cli_invocation(args: &[String]) -> bool {
    args.len() == 2
        && matches!(
            args[1].to_ascii_lowercase().as_str(),
            "/peinstall" | "--pe-install" | "/pebackup" | "--pe-backup"
        )
}

#[cfg(feature = "ci-automation")]
fn record_personal_restore_ci_probe(
    report: &lr_core::personal_files::PersonalFileRestoreReport,
) -> anyhow::Result<()> {
    use std::io::Write as _;

    let program_data = std::env::var_os("ProgramData")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("ProgramData is unavailable for the CI restore probe"))?;
    let log_directory = program_data.join("LetRecovery").join("Logs");
    std::fs::create_dir_all(&log_directory)?;
    let log_path = log_directory.join("FirstLogon-finalize.log");
    let expected = [
        "lr-preserve-desktop.txt",
        "lr-preserve-documents.txt",
        "lr-preserve-downloads.txt",
        "lr-preserve-pictures.txt",
        "lr-preserve-music.txt",
        "lr-preserve-videos.txt",
    ];
    let mut present = 0_u32;
    let mut observations = Vec::with_capacity(expected.len());
    for (directory, name) in report.personal_directories.iter().zip(expected) {
        let path = directory.join(name);
        let exists = std::fs::symlink_metadata(&path)
            .map(|metadata| metadata.is_file())
            .unwrap_or(false);
        if exists {
            present += 1;
        }
        observations.push(format!("{}={exists}", path.display()));
    }
    let desktop = report
        .personal_directories
        .first()
        .ok_or_else(|| anyhow::anyhow!("CI restore report has no Desktop destination"))?;
    let documents = report
        .personal_directories
        .get(1)
        .ok_or_else(|| anyhow::anyhow!("CI restore report has no Documents destination"))?;
    let inside_shortcut = desktop.join("LR-Preserve-Inside-Users.lnk");
    let canary = documents.join("LetRecovery-CI-post-restore-canary.txt");
    std::fs::write(&canary, b"created-after-personal-restore")?;
    let canary_readback = std::fs::read(&canary)? == b"created-after-personal-restore";
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    writeln!(
        log,
        "Personal files restore CI probe: immediate markers={present}/6 inside_shortcut={} canary={} details={}",
        inside_shortcut.is_file(),
        canary_readback,
        observations.join("|")
    )?;
    log.flush()?;
    Ok(())
}

#[cfg(feature = "ci-automation")]
fn record_personal_restore_source_ci_probe(session_id: &str) -> anyhow::Result<()> {
    use std::io::Write as _;
    use std::os::windows::fs::MetadataExt as _;

    let system_root = std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("SystemRoot is unavailable for the CI source probe"))?;
    let volume_root = system_root
        .parent()
        .ok_or_else(|| anyhow::anyhow!("SystemRoot has no volume root for the CI source probe"))?;
    let preserved = volume_root.join(format!("LetRecovery_Preserved_{session_id}"));
    let mut stack = vec![preserved.clone()];
    let mut files = Vec::new();
    let mut marker_count = 0_u32;
    let mut shortcut_count = 0_u32;
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let metadata = std::fs::symlink_metadata(entry.path())?;
            if metadata.is_dir() && metadata.file_attributes() & 0x0000_0400 == 0 {
                stack.push(entry.path());
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(&preserved)
                .map_err(|_| anyhow::anyhow!("CI source probe path escaped preservation root"))?
                .display()
                .to_string();
            let lower = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if lower.starts_with("lr-preserve-") && lower.ends_with(".txt") {
                marker_count += 1;
            }
            if lower == "lr-preserve-inside-users.lnk" {
                shortcut_count += 1;
            }
            if files.len() < 128 {
                files.push(relative);
            }
        }
    }
    files.sort();
    let program_data = std::env::var_os("ProgramData")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("ProgramData is unavailable for the CI source probe"))?;
    let log_directory = program_data.join("LetRecovery").join("Logs");
    std::fs::create_dir_all(&log_directory)?;
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_directory.join("FirstLogon-finalize.log"))?;
    writeln!(
        log,
        "Personal files restore CI source probe: markers={marker_count} inside_shortcut={shortcut_count} files={} details={}",
        files.len(),
        files.join("|")
    )?;
    log.flush()?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 3
        && matches!(
            args[1].as_str(),
            "--internal-activate-personal-restore-shell-gate"
                | "--internal-begin-personal-restore-second-logon"
                | "--internal-rearm-personal-restore-before-shell"
                | "--internal-personal-restore-progress-shell"
        )
    {
        let result = match args[1].as_str() {
            "--internal-activate-personal-restore-shell-gate" => {
                lr_core::first_logon::activate_personal_restore_shell_gate(&args[2])
            }
            "--internal-begin-personal-restore-second-logon" => {
                lr_core::first_logon::begin_personal_restore_second_logon(&args[2])
            }
            "--internal-rearm-personal-restore-before-shell" => {
                lr_core::first_logon::rearm_personal_restore_before_shell(&args[2])
            }
            "--internal-personal-restore-progress-shell" => {
                lr_core::first_logon::run_personal_restore_progress_shell(&args[2])
            }
            _ => unreachable!("private Shell-gate route was matched above"),
        };
        match result {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                println!("failed: {error:#}");
                std::process::exit(1);
            }
        }
    }
    if (args.len() == 3
        && matches!(
            args[1].as_str(),
            "--internal-restore-personal-files"
                | "--internal-restore-personal-files-at-shell"
                | "--internal-restore-personal-files-before-shell"
        ))
        || (args.len() == 4 && args[1] == "--internal-restore-personal-files-after-shell")
    {
        #[cfg(feature = "ci-automation")]
        if args[1] == "--internal-restore-personal-files-after-shell" {
            if let Err(error) = record_personal_restore_source_ci_probe(&args[2]) {
                eprintln!("LETRECOVERY_PERSONAL_RESTORE_CI_SOURCE_PROBE_FAILURE {error:#}");
            }
        }
        let result = match args[1].as_str() {
            "--internal-restore-personal-files-at-shell" => {
                lr_core::first_logon::restore_personal_files_at_shell(&args[2])
            }
            "--internal-restore-personal-files-before-shell" => {
                lr_core::first_logon::restore_personal_files_before_shell(&args[2])
            }
            "--internal-restore-personal-files-after-shell" => match args[3].as_str() {
                "true" => lr_core::first_logon::restore_personal_files_after_shell(&args[2], true),
                "false" => {
                    lr_core::first_logon::restore_personal_files_after_shell(&args[2], false)
                }
                _ => Err(anyhow::anyhow!(
                    "invalid personal-file Explorer-stage automation flag"
                )),
            },
            _ => {
                lr_core::personal_files::restore_preserved_personal_files_for_current_user(&args[2])
                    .map(Some)
            }
        };
        match result {
            Ok(Some(report)) => {
                #[cfg(feature = "ci-automation")]
                if let Err(error) = record_personal_restore_ci_probe(&report) {
                    eprintln!("LETRECOVERY_PERSONAL_RESTORE_CI_PROBE_FAILURE {error:#}");
                }
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
                std::process::exit(0);
            }
            Ok(None) => {
                println!("completed cleanup-only receipt={}", args[2]);
                std::process::exit(0);
            }
            Err(error) => {
                println!("failed: {error:#}");
                std::process::exit(1);
            }
        }
    }
    if args.len() == 4 && args[1] == "--internal-register-personal-files-at-shell" {
        let exit_code = match lr_core::first_logon::register_personal_restore_at_shell(
            &args[2],
            std::path::Path::new(&args[3]),
        ) {
            Ok(()) => 0,
            Err(error) => {
                println!("failed: {error:#}");
                1
            }
        };
        std::process::exit(exit_code);
    }
    if args.len() == 2 && args[1] == "--internal-store-builtin-administrator-secret" {
        let exit_code = match lr_core::first_logon::protect_staged_builtin_administrator_secret() {
            Ok(()) => 0,
            Err(error) => {
                println!("failed: {error:#}");
                lr_core::unattend_command::REQUIRED_SPECIALIZE_FAILURE_EXIT_CODE
            }
        };
        std::process::exit(exit_code);
    }
    if args.len() == 4 && args[1] == "--internal-prepare-local-rid" {
        let exit_code = if args[2] == "500" {
            match lr_core::windows_accounts::decode_account_name_utf16_hex(&args[3]).and_then(
                |name| lr_core::windows_accounts::prepare_local_account_by_rid(500, &name),
            ) {
                Ok(()) => 0,
                Err(_) => lr_core::unattend_command::REQUIRED_SPECIALIZE_FAILURE_EXIT_CODE,
            }
        } else {
            lr_core::unattend_command::REQUIRED_SPECIALIZE_FAILURE_EXIT_CODE
        };
        std::process::exit(exit_code);
    }
    if args.len() == 5
        && args[1] == "--internal-begin-builtin-administrator-transition-with-personal-restore"
    {
        let result = (|| -> anyhow::Result<()> {
            let desired_name = lr_core::windows_accounts::decode_account_name_utf16_hex(&args[2])?;
            let temporary_name =
                lr_core::windows_accounts::decode_account_name_utf16_hex(&args[3])?;
            lr_core::first_logon::begin_builtin_administrator_transition_with_personal_restore(
                &desired_name,
                &temporary_name,
                &args[4],
            )
        })();
        match result {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                println!("failed: {error:#}");
                std::process::exit(1);
            }
        }
    }
    if args.len() == 4
        && matches!(
            args[1].as_str(),
            "--internal-begin-builtin-administrator-transition"
                | "--internal-finish-builtin-administrator-transition"
                | "--internal-retire-builtin-administrator-transition"
        )
    {
        let result = (|| -> anyhow::Result<()> {
            let desired_name = lr_core::windows_accounts::decode_account_name_utf16_hex(&args[2])?;
            let temporary_name =
                lr_core::windows_accounts::decode_account_name_utf16_hex(&args[3])?;
            match args[1].as_str() {
                "--internal-begin-builtin-administrator-transition" => {
                    lr_core::first_logon::begin_builtin_administrator_transition(
                        &desired_name,
                        &temporary_name,
                    )
                }
                "--internal-finish-builtin-administrator-transition" => {
                    lr_core::first_logon::finish_builtin_administrator_transition(
                        &desired_name,
                        &temporary_name,
                    )
                }
                "--internal-retire-builtin-administrator-transition" => {
                    lr_core::first_logon::retire_builtin_administrator_transition(
                        &desired_name,
                        &temporary_name,
                    )
                }
                _ => unreachable!("private account transition route was matched above"),
            }
        })();
        match result {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                println!("failed: {error:#}");
                std::process::exit(1);
            }
        }
    }
    if args.len() == 2 && args[1] == "--internal-cleanup-disabled-defaultuser0" {
        let exit_code = match lr_core::windows_accounts::cleanup_disabled_default_oobe_account() {
            Ok(removed) => {
                println!("completed removed={removed}");
                0
            }
            Err(error) => {
                println!("failed: {error}");
                1
            }
        };
        std::process::exit(exit_code);
    }
    if args.len() == 3 && args[1] == "--internal-delete-temporary-oobe-account" {
        let exit_code = match lr_core::windows_accounts::decode_account_name_utf16_hex(&args[2])
            .and_then(|name| {
                lr_core::unattend_account::validate_temporary_oobe_account_name(&name)
                    .map_err(|_| lr_core::windows_accounts::AccountUpdateError::InvalidAccount)?;
                lr_core::windows_accounts::delete_local_account(&name)
            }) {
            Ok(()) => 0,
            Err(_) => 1,
        };
        std::process::exit(exit_code);
    }
    if is_removed_pe_cli_invocation(&args) {
        // PE has no destructive command-line mode. Reject a stale legacy entry before logging,
        // driver discovery, handoff authentication, UI initialization or any MessageBox.
        return Ok(());
    }
    // 初始化日志：优先写入程序目录；只读介质自动回退到 WinPE RAM 盘。
    // PE 下 GUI 程序没有控制台，stderr 会被直接丢弃，必须落盘才能事后排查“怎么死的”。
    init_file_logger();
    // 安装 panic 钩子：安装流程跑在工作线程里，线程 panic 会“静默死亡”导致界面卡住，
    // 必须把 panic 记到日志。
    install_panic_hook();

    log::info!("==================== LetRecovery PE 启动 ====================");
    log::info!(
        "版本: {} | 日志文件: {}",
        env!("BUILD_VERSION"),
        active_log_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "[unavailable]".to_string())
    );
    let firmware = lr_core::boot_pca::inspect_firmware_pca();
    log::info!(
        "[诊断环境] PE 程序: version={} | arch={}",
        env!("BUILD_VERSION"),
        std::env::consts::ARCH
    );
    log::info!(
        "[诊断环境] PE 固件: Secure Boot={} | PCA2011 revoked={} | PCA2023 trusted={} | probe_error={}",
        diagnostic_option(firmware.secure_boot_enabled),
        diagnostic_option(firmware.revokes_pca2011),
        diagnostic_option(firmware.trusts_pca2023),
        firmware.error.as_deref().unwrap_or("无")
    );

    // Deterministic, side-effect-free visual entry for the native PE progress shell, including the
    // same elapsed-time loading ring and paint timer used by the production page. It must run before
    // driver loading, BitLocker passthrough and task discovery so desktop QA cannot touch the host
    // storage stack. Release builds do not contain this branch.
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
    if let Err(error) = load_matching_vmd_driver_into_running_pe() {
        log::error!("[VMD/PE] boot-critical storage driver preparation failed: {error:#}");
        show_error_message(&tr!(
            "无法安全加载当前 Intel VMD 存储控制器驱动，已停止启动，未扫描或修改磁盘。\n\n{}",
            format_args!("{error:#}")
        ));
        return Ok(());
    }

    // 检查命令行参数
    log::info!("命令行参数: {:?}", args);

    // The fixed X: LRHC1 capsule is the sole operation authority. No disk task file, drive letter,
    // legacy INI, or unscoped recovery material is inspected before this succeeds.

    let authenticated_handoff = match core::config::AuthenticatedOperationGuard::discover() {
        Ok(guard) => guard,
        Err(error) => {
            log::error!("[HANDOFF AUTH] fixed X: LRHC1 payload rejected: {error:#}");
            show_error_message(&tr!(
                "PE 任务认证失败，尚未扫描或修改磁盘。请返回正常系统端重新创建任务。\r\n\r\n{}",
                format_args!("{error:#}")
            ));
            return Ok(());
        }
    };
    log::info!(
        "[HANDOFF AUTH] accepted fixed boot payload: purpose={:?}, session={}, config_bytes={}, artifacts={}",
        authenticated_handoff.purpose(),
        authenticated_handoff.session_id(),
        authenticated_handoff.exact_config_bytes().len(),
        authenticated_handoff.manifest().artifacts.len()
    );

    // 初始化多语言：从配置文件（正常系统端随重启写入 Language=）读取界面语言；空=简体中文（内置）。
    // 必须在任何 GUI/CLI 分支之前，确保所有模式下文案都按所选语言显示。
    let ui_language = detect_ui_language(&authenticated_handoff);
    utils::i18n::init(&ui_language);
    log::info!(
        "界面语言: {}",
        if ui_language.is_empty() {
            "zh-CN (默认)"
        } else {
            ui_language.as_str()
        }
    );

    if authenticated_handoff.purpose() == lr_core::handoff_auth::HandoffPurpose::Maintenance {
        match unlock_maintenance_volumes_best_effort(&authenticated_handoff) {
            Ok((attempted, unlocked)) => log::info!(
                "[PE MAINTENANCE] BitLocker 自动解锁完成: enumerated_volumes={attempted}, accepted_unlocks={unlocked}"
            ),
            Err(error) => log::warn!(
                "[PE MAINTENANCE] BitLocker 自动解锁材料不可用，维护环境继续: {error:#}"
            ),
        }
        remain_in_hidden_pe_maintenance();
    }
    let authenticated_operation = authenticated_handoff
        .operation_type()
        .context("non-maintenance handoff has no executable operation type")?;

    // 自动检测模式
    if args.contains(&"/AUTO".to_string()) || args.contains(&"--auto".to_string()) {
        log::info!("检测到自动模式，检测操作类型...");

        match authenticated_operation {
            core::config::OperationType::Install => {
                log::info!("检测到安装配置，启动GUI安装界面...");
            }
            core::config::OperationType::Backup => {
                log::info!("检测到备份配置，启动GUI备份界面...");
            }
            core::config::OperationType::Expand => {
                log::info!("检测到扩容配置，启动GUI扩容界面...");
            }
        }
    }

    let operation_type = authenticated_operation;
    authenticated_handoff.verify_unchanged()?;

    log::info!("进入 PE 原生 Win32 进度界面");
    if let Err(error) = native_ui::progress::run(operation_type, authenticated_handoff) {
        log::error!("PE 原生 Win32 进度界面运行失败: {error}");
        show_error_message(&tr!("启动失败: {} - {}", "LetRecovery PE", error));
    }
    Ok(())
}

fn diagnostic_option(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "是",
        Some(false) => "否",
        None => "未知",
    }
}

fn validate_persistent_pe_payload(path: &std::path::Path, extension: &str) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("PE payload path has no parent"))?;
    let parent_name = parent
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("PE payload parent name is invalid"))?;
    if !path.is_absolute() || !parent_name.eq_ignore_ascii_case("LetRecovery_PE") {
        anyhow::bail!("PE payload path is outside the persistent PE directory");
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("PE payload filename is not valid Unicode"))?;
    if !name.starts_with("boot-") || !name.ends_with(extension) {
        anyhow::bail!("PE payload filename is not session-scoped");
    }
    Ok(())
}

#[cfg(test)]
fn persistent_pe_payloads_from_journal(contents: &str) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let lines: Vec<&str> = contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.is_empty() {
        anyhow::bail!("active PE BCD journal is empty");
    }
    if lines.len() == 2
        && lines
            .iter()
            .all(|line| line.starts_with('{') && line.ends_with('}'))
    {
        return Ok(vec![
            std::path::PathBuf::from(r"C:\LetRecovery_PE\boot.wim"),
            std::path::PathBuf::from(r"C:\LetRecovery_PE\boot.sdi"),
        ]);
    }
    let mut payloads = Vec::new();
    for line in lines {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 5 || fields[0] != "LRPE2" {
            anyhow::bail!("invalid PE BCD journal record");
        }
        let wim = std::path::PathBuf::from(fields[3]);
        let sdi = std::path::PathBuf::from(fields[4]);
        validate_persistent_pe_payload(&wim, ".wim")?;
        validate_persistent_pe_payload(&sdi, ".sdi")?;
        payloads.push(wim);
        payloads.push(sdi);
    }
    Ok(payloads)
}

fn remove_persistent_payload(path: &std::path::Path) -> anyhow::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect persistent PE payload {}", path.display()));
        }
    };
    #[cfg(windows)]
    let is_reparse = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x0000_0400 != 0
    };
    #[cfg(not(windows))]
    let is_reparse = metadata.file_type().is_symlink();
    if is_reparse {
        anyhow::bail!(
            "refusing to remove a linked persistent PE payload: {}",
            path.display()
        );
    }
    std::fs::remove_file(path)
        .with_context(|| format!("remove persistent PE payload {}", path.display()))?;
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            log::info!("已删除持久 PE 会话载荷: {}", path.display());
            Ok(())
        }
        Err(error) => Err(error)
            .with_context(|| format!("read back persistent PE payload {}", path.display())),
        Ok(_) => anyhow::bail!(
            "persistent PE payload still exists after removal: {}",
            path.display()
        ),
    }
}

#[cfg(windows)]
fn lock_persistent_pe_root(path: &std::path::Path) -> anyhow::Result<std::fs::File> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::MetadataExt;
    use std::os::windows::io::FromRawHandle;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };

    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }
    .context("open persistent PE root without delete sharing")?;
    let file = unsafe { std::fs::File::from_raw_handle(handle.0) };
    let metadata = file
        .metadata()
        .context("inspect locked persistent PE root")?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        anyhow::bail!("persistent PE root is a reparse point or not a directory");
    }
    Ok(file)
}

#[cfg(not(windows))]
fn lock_persistent_pe_root(path: &std::path::Path) -> anyhow::Result<std::fs::File> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        anyhow::bail!("persistent PE root is a symlink or not a directory");
    }
    Ok(std::fs::File::open(path)?)
}

const MAX_PERSISTENT_JOURNAL_BYTES: u64 = 256 * 1024;

struct LockedPersistentJournal {
    file: std::fs::File,
    contents: String,
}

impl LockedPersistentJournal {
    fn verify_unchanged(&mut self) -> anyhow::Result<()> {
        use std::io::{Read, Seek};

        let length = self.file.metadata()?.len();
        if length > MAX_PERSISTENT_JOURNAL_BYTES {
            anyhow::bail!("persistent PE journal grew beyond its bounded size");
        }
        self.file.rewind()?;
        let mut bytes = Vec::with_capacity(length as usize);
        self.file
            .by_ref()
            .take(MAX_PERSISTENT_JOURNAL_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_PERSISTENT_JOURNAL_BYTES
            || bytes.as_slice() != self.contents.as_bytes()
        {
            anyhow::bail!("persistent PE journal changed after authentication");
        }
        Ok(())
    }
}

fn read_persistent_journal(
    path: &std::path::Path,
) -> anyhow::Result<Option<LockedPersistentJournal>> {
    use std::io::Read;

    #[cfg(windows)]
    let mut file = {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Foundation::GENERIC_READ;
        use windows::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ};

        match std::fs::OpenOptions::new()
            .read(true)
            .access_mode(GENERIC_READ.0)
            // Keep the exact journal object stable while matching and deleting only this session's
            // BCD/payload objects. Denying write/delete sharing avoids a path reopen race.
            .share_mode(FILE_SHARE_READ.0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).with_context(|| format!("open {}", path.display())),
        }
    };
    #[cfg(not(windows))]
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("open {}", path.display())),
    };
    let metadata = file
        .metadata()
        .with_context(|| format!("inspect locked {}", path.display()))?;
    #[cfg(windows)]
    let is_reparse = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x0000_0400 != 0
    };
    #[cfg(not(windows))]
    let is_reparse = metadata.file_type().is_symlink();
    if !metadata.is_file() || is_reparse {
        anyhow::bail!(
            "persistent PE journal is not a regular file: {}",
            path.display()
        );
    }
    if metadata.len() > MAX_PERSISTENT_JOURNAL_BYTES {
        anyhow::bail!("persistent PE journal exceeds its bounded size");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(MAX_PERSISTENT_JOURNAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read locked persistent PE journal {}", path.display()))?;
    if bytes.len() as u64 > MAX_PERSISTENT_JOURNAL_BYTES {
        anyhow::bail!("persistent PE journal exceeds its bounded size");
    }
    let contents = String::from_utf8(bytes)
        .with_context(|| format!("decode persistent PE journal {}", path.display()))?;
    Ok(Some(LockedPersistentJournal { file, contents }))
}

#[cfg(test)]
fn merge_persistent_pe_payload_journals(
    active: Option<&str>,
    pending: Option<&str>,
) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let mut payloads = Vec::new();
    for (role, contents) in [("active", active), ("pending", pending)] {
        if let Some(contents) = contents {
            if role == "pending"
                && (contents
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .count()
                    != 1
                    || !contents.trim().starts_with("LRPE2\t"))
            {
                anyhow::bail!("pending PE BCD journal must contain exactly one LRPE2 record");
            }
            let parsed = persistent_pe_payloads_from_journal(contents)
                .with_context(|| format!("parse {role} PE BCD journal"))?;
            if role == "pending" && parsed.len() != 2 {
                anyhow::bail!("pending PE BCD journal must contain exactly one LRPE2 record");
            }
            for path in parsed {
                if !payloads.iter().any(|existing: &std::path::PathBuf| {
                    existing
                        .to_string_lossy()
                        .eq_ignore_ascii_case(&path.to_string_lossy())
                }) {
                    payloads.push(path);
                }
            }
        }
    }
    Ok(payloads)
}

#[derive(Clone, Debug)]
struct TrustedPersistentPeRecord {
    ramdisk_guid: String,
    loader_guid: String,
    session_id: String,
    wim_name: String,
    sdi_name: String,
    root_identity: lr_core::install_handoff::CanonicalInstallTargetV2,
    purpose: lr_core::handoff_auth::HandoffPurpose,
    capsule_sha256: [u8; 32],
}

fn parse_trusted_persistent_pe_record(line: &str) -> anyhow::Result<TrustedPersistentPeRecord> {
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.len() != 14 || fields[0] != "LRPE4" {
        anyhow::bail!("not an LRPE4 persistent PE journal record");
    }
    let session_id = fields[5];
    lr_core::handoff_auth::validate_session_id(session_id).context("invalid LRPE4 SessionId")?;
    let wim = std::path::PathBuf::from(fields[3]);
    let sdi = std::path::PathBuf::from(fields[4]);
    validate_persistent_pe_payload(&wim, ".wim")?;
    validate_persistent_pe_payload(&sdi, ".sdi")?;
    let identity = lr_core::install_handoff::canonical_target_from_fields(
        Some(lr_core::install_handoff::CANONICAL_TARGET_VERSION),
        Some(fields[6]),
        Some(fields[7].parse().context("parse LRPE4 root offset")?),
        Some(fields[8].parse().context("parse LRPE4 root length")?),
        Some(fields[9]),
        Some(fields[10]),
        (fields[11] != "none").then_some(fields[11]),
    )?
    .context("LRPE4 root identity is absent")?;
    Ok(TrustedPersistentPeRecord {
        ramdisk_guid: fields[1].to_string(),
        loader_guid: fields[2].to_string(),
        session_id: session_id.to_string(),
        wim_name: wim
            .file_name()
            .and_then(|value| value.to_str())
            .context("LRPE4 WIM filename is invalid")?
            .to_string(),
        sdi_name: sdi
            .file_name()
            .and_then(|value| value.to_str())
            .context("LRPE4 SDI filename is invalid")?
            .to_string(),
        root_identity: identity,
        purpose: lr_core::handoff_auth::HandoffPurpose::parse(fields[12])?,
        capsule_sha256: lr_core::install_handoff::decode_hex_array::<32>(
            fields[13],
            "LRPE4 capsule SHA-256",
        )?,
    })
}

fn rewrite_persistent_journal_without_line(
    root: &std::path::Path,
    journal: &std::path::Path,
    original: &str,
    removed_line: &str,
) -> anyhow::Result<()> {
    let remaining = original
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && *line != removed_line)
        .collect::<Vec<_>>();
    if remaining.is_empty() {
        remove_persistent_payload(journal)?;
        return Ok(());
    }
    let bytes = format!("{}\r\n", remaining.join("\r\n")).into_bytes();
    let temporary = lr_core::scoped_temp_file::ScopedTempFile::create_in(
        root,
        "pe-journal-cleanup",
        "tmp",
        &bytes,
    )?;
    temporary.persist_replace(journal)?;
    let actual = std::fs::read(journal)?;
    if actual != bytes {
        anyhow::bail!("persistent PE journal readback mismatch after exact cleanup");
    }
    Ok(())
}

fn persistent_record_matches_running(
    record: &TrustedPersistentPeRecord,
    session_id: &str,
    purpose: lr_core::handoff_auth::HandoffPurpose,
    capsule_sha256: &[u8; 32],
) -> bool {
    record.session_id == session_id
        && record.purpose == purpose
        && record.capsule_sha256 == *capsule_sha256
}

fn persistent_pe_root_for_volume(volume_guid_root: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(volume_guid_root).join("LetRecovery_PE")
}

fn remove_empty_private_pe_root(root: &std::path::Path) -> anyhow::Result<bool> {
    if std::fs::read_dir(root)?.next().is_some() {
        return Ok(false);
    }
    match std::fs::remove_dir(root) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("remove empty private PE payload root {}", root.display())
            });
        }
    }
    match std::fs::symlink_metadata(root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error)
            .with_context(|| format!("read back private PE payload root {}", root.display())),
        Ok(_) => anyhow::bail!(
            "private PE payload root still exists after exact removal: {}",
            root.display()
        ),
    }
}

pub(crate) fn cleanup_persistent_pe_boot_payload(
    authenticated_handoff: &core::config::AuthenticatedOperationGuard,
) -> anyhow::Result<()> {
    authenticated_handoff.verify_unchanged()?;
    let session_id = authenticated_handoff.session_id();
    let purpose = authenticated_handoff.purpose();
    let capsule_sha256 = authenticated_handoff.capsule_sha256()?;
    let mut matches = Vec::new();
    for volume_root in lr_core::windows_storage::volume_guid_paths()? {
        let root = persistent_pe_root_for_volume(&volume_root);
        if !root.is_dir() {
            continue;
        }
        for name in ["pe_guid.txt", "pe_pending.txt"] {
            let journal = root.join(name);
            let Ok(Some(locked_journal)) = read_persistent_journal(&journal) else {
                // Unreadable, linked, non-file or malformed same-name journals cannot match the
                // complete authenticated tuple. Ignore them so they cannot hide a valid record on
                // another current volume; zero exact matches remains a failure below.
                continue;
            };
            let lines = locked_journal
                .contents
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>();
            let mut journal_matches = Vec::new();
            for line in &lines {
                if !line.starts_with("LRPE4\t") {
                    continue;
                }
                if line.split('\t').nth(5) != Some(session_id) {
                    continue;
                }
                let Ok(record) = parse_trusted_persistent_pe_record(line) else {
                    // A same-name stale, truncated or otherwise malformed journal is environment
                    // noise. It cannot match the complete authenticated tuple and must not hide a
                    // valid record on another volume.
                    continue;
                };
                if !persistent_record_matches_running(&record, session_id, purpose, &capsule_sha256)
                {
                    continue;
                }
                if name == "pe_pending.txt" && lines.len() != 1 {
                    continue;
                }
                // This journal authorizes only deletion of this session's bounded BCD objects and
                // private payload filenames. The running private WIM already supplies the exact
                // SessionId, purpose and capsule digest. Requiring the normal-Windows disk GUID,
                // layout digest, disk number or historical extent to match again provides no
                // additional authenticity, but can falsely block cleanup after legitimate WinPE
                // enumeration or topology changes. Keep root_identity parseable for journal
                // compatibility/diagnostics; never use it as a cross-reboot gate.
                journal_matches.push(((*line).to_string(), record));
            }
            if journal_matches.len() > 1 {
                anyhow::bail!(
                    "expected exactly one stable/session-bound LRPE4 journal, found multiple exact records in {}",
                    journal.display()
                );
            }
            if let Some((line, record)) = journal_matches.pop() {
                matches.push((root.clone(), journal, line, record, locked_journal));
            }
        }
    }
    if matches.len() != 1 {
        anyhow::bail!(
            "expected exactly one stable/session-bound LRPE4 journal, found {}",
            matches.len()
        );
    }
    let (root, journal, line, record, mut locked_journal) = matches.pop().unwrap();
    let root_lock = lock_persistent_pe_root(&root)?;
    locked_journal.verify_unchanged()?;
    log::info!(
        "[PE HANDOFF] matched LRPE4 by current session/purpose/capsule; historical root geometry is diagnostic only: offset={} length={} style={:?}",
        record.root_identity.partition_offset_bytes,
        record.root_identity.partition_length_bytes,
        record.root_identity.style
    );
    crate::core::bcdedit::BootManager::new()
        .delete_trusted_pe_boot_objects(&record.loader_guid, &record.ramdisk_guid)?;
    remove_persistent_payload(&root.join(&record.wim_name))?;
    remove_persistent_payload(&root.join(&record.sdi_name))?;
    // Keep the exact ordinary, non-reparse journal object locked until every operation it
    // authorizes has completed. The protected root remains locked while the handle is released and
    // the journal is atomically rewritten without this one exact record.
    locked_journal.verify_unchanged()?;
    let contents = locked_journal.contents.clone();
    drop(locked_journal);
    rewrite_persistent_journal_without_line(&root, &journal, &contents, &line)?;
    if journal.exists()
        && std::fs::read_to_string(journal)?
            .lines()
            .any(|candidate| candidate.trim() == line)
    {
        anyhow::bail!("trusted LRPE4 journal record remains after cleanup");
    }
    drop(root_lock);
    if remove_empty_private_pe_root(&root)? {
        // This directory is not a mounted image; it is the private on-disk RAM-boot payload root.
        // Remove only the now-empty exact directory (never recursively) so a subsequent system
        // backup cannot archive a product-generated placeholder.
        log::info!(
            "[PE HANDOFF] removed empty private PE payload root: {}",
            root.display()
        );
    } else {
        log::warn!(
            "[PE HANDOFF] private PE payload root still contains unrelated entries and was retained: {}",
            root.display()
        );
    }
    Ok(())
}

pub(crate) fn save_only_driver_destination(
    target_partition: &str,
    session_id: &str,
) -> anyhow::Result<std::path::PathBuf> {
    let token = session_id.trim().trim_matches(['{', '}']);
    if token.is_empty()
        || token.len() > 64
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    {
        anyhow::bail!("SaveOnly requires a valid non-empty installation SessionId");
    }
    Ok(std::path::PathBuf::from(format!(
        "{}\\LetRecovery_Drivers\\session-{}",
        target_partition,
        token.to_ascii_lowercase()
    )))
}

/// 显示错误消息框
fn show_error_message(message: &str) {
    #[cfg(feature = "ci-automation")]
    {
        log::error!("PE startup error: {message}");
        finalize_ci_failure(message);
    }
    #[cfg(all(windows, not(feature = "ci-automation")))]
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

    #[cfg(all(not(windows), not(feature = "ci-automation")))]
    {
        log::error!("错误: {}", message);
    }
}

#[cfg(test)]
mod persistent_payload_tests {
    use super::{
        is_removed_pe_cli_invocation, matched_vmd_controller_runtime_state,
        merge_persistent_pe_payload_journals, parse_trusted_persistent_pe_record,
        persistent_pe_payloads_from_journal, persistent_pe_root_for_volume,
        persistent_record_matches_running, remove_empty_private_pe_root,
        save_only_driver_destination, terminal_log_destination, terminal_success_outcome,
        TerminalLogDestination, TerminalLogPublishGate,
    };

    #[test]
    fn private_pe_root_is_removed_only_when_empty() {
        let workspace = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "pe-root-cleanup-test",
        )
        .unwrap();
        let empty = workspace.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        assert!(remove_empty_private_pe_root(&empty).unwrap());
        assert!(!empty.exists());

        let occupied = workspace.path().join("occupied");
        std::fs::create_dir(&occupied).unwrap();
        std::fs::write(occupied.join("foreign.bin"), b"keep").unwrap();
        assert!(!remove_empty_private_pe_root(&occupied).unwrap());
        assert_eq!(
            std::fs::read(occupied.join("foreign.bin")).unwrap(),
            b"keep"
        );
    }

    #[test]
    fn failed_drvload_is_nonfatal_only_for_an_exact_operational_controller() {
        use lr_core::driver::PresentDeviceState;
        use lr_core::storage_driver_match::BuiltInStorageDriverPackage;

        let operational = PresentDeviceState {
            hardware_ids: vec!["PCI\\VEN_8086&DEV_467F&SUBSYS_00000000".to_owned()],
            status_query_cr: 0,
            devnode_status: 0x0000_0008,
            problem_number: None,
        };
        assert!(
            matched_vmd_controller_runtime_state(
                BuiltInStorageDriverPackage::IntelVmdCurrent,
                std::slice::from_ref(&operational),
            )
            .0
        );

        let failed_start = PresentDeviceState {
            devnode_status: 0x0000_0400,
            problem_number: Some(10),
            ..operational.clone()
        };
        assert!(
            !matched_vmd_controller_runtime_state(
                BuiltInStorageDriverPackage::IntelVmdCurrent,
                &[failed_start],
            )
            .0
        );

        let unrelated_status_failure = PresentDeviceState {
            hardware_ids: vec!["PCI\\VEN_1234&DEV_5678".to_owned()],
            status_query_cr: 0x0000_000D,
            devnode_status: 0,
            problem_number: None,
        };
        assert!(
            matched_vmd_controller_runtime_state(
                BuiltInStorageDriverPackage::IntelVmdCurrent,
                &[operational.clone(), unrelated_status_failure],
            )
            .0
        );

        let matched_status_failure = PresentDeviceState {
            status_query_cr: 0x0000_000D,
            devnode_status: 0,
            problem_number: None,
            ..operational.clone()
        };
        assert!(
            !matched_vmd_controller_runtime_state(
                BuiltInStorageDriverPackage::IntelVmdCurrent,
                &[matched_status_failure],
            )
            .0
        );
        assert!(
            !matched_vmd_controller_runtime_state(
                BuiltInStorageDriverPackage::IntelVmd11th,
                &[operational],
            )
            .0
        );
    }

    #[test]
    fn removed_pe_cli_is_case_insensitive_and_requires_exact_arity() {
        let program = "LetRecoveryPE.exe".to_owned();
        for argument in ["/PEINSTALL", "--Pe-Install", "/pebackup", "--PE-BACKUP"] {
            assert!(is_removed_pe_cli_invocation(&[
                program.clone(),
                argument.to_owned()
            ]));
        }
        assert!(!is_removed_pe_cli_invocation(std::slice::from_ref(
            &program
        )));
        assert!(!is_removed_pe_cli_invocation(&[
            program,
            "/PEINSTALL".to_owned(),
            "unexpected".to_owned(),
        ]));
    }

    #[test]
    fn lrpe4_parser_preserves_session_guids_stable_root_and_capsule_binding() {
        let record = parse_trusted_persistent_pe_record(concat!(
            "LRPE4\t{11111111-1111-1111-1111-111111111111}\t",
            "{22222222-2222-2222-2222-222222222222}\t",
            "C:\\LetRecovery_PE\\boot-session.wim\t",
            "C:\\LetRecovery_PE\\boot-session.sdi\t",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\t",
            "1111111111111111111111111111111111111111111111111111111111111111\t",
            "1048576\t8000000\tGPT\t33333333333333333333333333333333\t",
            "2222222222222222222222222222222222222222222222222222222222222222\t",
            "install\t4444444444444444444444444444444444444444444444444444444444444444"
        ))
        .unwrap();
        assert_eq!(record.session_id, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(record.wim_name, "boot-session.wim");
        assert_eq!(record.root_identity.partition_offset_bytes, 1_048_576);
        assert_eq!(
            record.purpose,
            lr_core::handoff_auth::HandoffPurpose::Install
        );
        assert_eq!(record.capsule_sha256, [0x44; 32]);
    }

    #[test]
    fn lrpe4_parser_accepts_private_payload_on_non_c_system_volume() {
        let record = parse_trusted_persistent_pe_record(concat!(
            "LRPE4\t{11111111-1111-1111-1111-111111111111}\t",
            "{22222222-2222-2222-2222-222222222222}\t",
            "D:\\LetRecovery_PE\\boot-session.wim\t",
            "D:\\LetRecovery_PE\\boot-session.sdi\t",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\t",
            "1111111111111111111111111111111111111111111111111111111111111111\t",
            "1048576\t8000000\tGPT\t33333333333333333333333333333333\t",
            "2222222222222222222222222222222222222222222222222222222222222222\t",
            "backup\t4444444444444444444444444444444444444444444444444444444444444444"
        ))
        .unwrap();
        assert_eq!(record.wim_name, "boot-session.wim");
        assert_eq!(record.sdi_name, "boot-session.sdi");
        assert_eq!(
            record.purpose,
            lr_core::handoff_auth::HandoffPurpose::Backup
        );
    }

    #[test]
    fn persistent_cleanup_binding_ignores_historical_disk_inventory() {
        let record = parse_trusted_persistent_pe_record(concat!(
            "LRPE4\t{11111111-1111-1111-1111-111111111111}\t",
            "{22222222-2222-2222-2222-222222222222}\t",
            "C:\\LetRecovery_PE\\boot-session.wim\t",
            "C:\\LetRecovery_PE\\boot-session.sdi\t",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\t",
            // Deliberately unrelated historical disk/layout values. They remain parseable for
            // journal compatibility but are not part of the running-session match.
            "9999999999999999999999999999999999999999999999999999999999999999\t",
            "4608\t8000512\tGPT\t33333333333333333333333333333333\t",
            "2222222222222222222222222222222222222222222222222222222222222222\t",
            "install\t4444444444444444444444444444444444444444444444444444444444444444"
        ))
        .unwrap();
        assert!(persistent_record_matches_running(
            &record,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            lr_core::handoff_auth::HandoffPurpose::Install,
            &[0x44; 32],
        ));
        assert!(!persistent_record_matches_running(
            &record,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            lr_core::handoff_auth::HandoffPurpose::Install,
            &[0x44; 32],
        ));
        assert!(!persistent_record_matches_running(
            &record,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            lr_core::handoff_auth::HandoffPurpose::Install,
            &[0x55; 32],
        ));
    }

    #[test]
    fn persistent_cleanup_scans_a_volume_guid_root_without_a_drive_letter() {
        assert_eq!(
            persistent_pe_root_for_volume(r"\\?\Volume{12345678-1234-1234-1234-123456789abc}\"),
            std::path::PathBuf::from(
                r"\\?\Volume{12345678-1234-1234-1234-123456789abc}\LetRecovery_PE"
            )
        );
    }

    #[test]
    fn terminal_log_publish_gate_is_one_shot() {
        let mut gate = TerminalLogPublishGate::armed();
        assert!(gate.take());
        assert!(!gate.take());

        let mut unarmed = TerminalLogPublishGate::default();
        assert!(!unarmed.take());
    }

    #[test]
    fn terminal_log_moves_to_target_as_soon_as_the_image_exists() {
        assert_eq!(
            terminal_log_destination(false, false),
            TerminalLogDestination::VerifiedDataFallback
        );
        assert_eq!(
            terminal_log_destination(false, true),
            TerminalLogDestination::TargetSystem
        );
        assert_eq!(
            terminal_log_destination(true, true),
            TerminalLogDestination::TargetSystem
        );
    }

    #[test]
    fn terminal_success_never_claims_verified_cleanup_after_a_cleanup_warning() {
        assert_eq!(terminal_success_outcome(true), "completed cleanup=verified");
        assert_eq!(
            terminal_success_outcome(false),
            "completed cleanup=incomplete"
        );
    }

    #[test]
    fn lrpe2_journal_resolves_only_session_scoped_payloads() {
        let payloads = persistent_pe_payloads_from_journal(concat!(
            "LRPE2\t{11111111-1111-1111-1111-111111111111}\t",
            "{22222222-2222-2222-2222-222222222222}\t",
            "C:\\LetRecovery_PE\\boot-session.wim\t",
            "C:\\LetRecovery_PE\\boot-session.sdi\r\n"
        ))
        .unwrap();
        assert_eq!(payloads.len(), 2);
        assert!(payloads[0].ends_with("boot-session.wim"));
        assert!(payloads[1].ends_with("boot-session.sdi"));
    }

    #[test]
    fn persistent_payload_journal_rejects_empty_and_escaped_paths() {
        assert!(persistent_pe_payloads_from_journal("").is_err());
        assert!(persistent_pe_payloads_from_journal(concat!(
            "LRPE2\t{11111111-1111-1111-1111-111111111111}\t",
            "{22222222-2222-2222-2222-222222222222}\t",
            "C:\\Windows\\boot-session.wim\t",
            "C:\\LetRecovery_PE\\boot-session.sdi\r\n"
        ))
        .is_err());
    }

    #[test]
    fn pending_only_crash_window_still_resolves_session_payloads() {
        let pending = concat!(
            "LRPE2\t{11111111-1111-1111-1111-111111111111}\t",
            "{22222222-2222-2222-2222-222222222222}\t",
            "C:\\LetRecovery_PE\\boot-pending.wim\t",
            "C:\\LetRecovery_PE\\boot-pending.sdi\r\n"
        );

        let payloads = merge_persistent_pe_payload_journals(None, Some(pending)).unwrap();

        assert_eq!(payloads.len(), 2);
        assert!(payloads[0].ends_with("boot-pending.wim"));
        assert!(payloads[1].ends_with("boot-pending.sdi"));
    }

    #[test]
    fn pending_journal_rejects_multiple_or_legacy_records() {
        let record = concat!(
            "LRPE2\t{11111111-1111-1111-1111-111111111111}\t",
            "{22222222-2222-2222-2222-222222222222}\t",
            "C:\\LetRecovery_PE\\boot-pending.wim\t",
            "C:\\LetRecovery_PE\\boot-pending.sdi\r\n"
        );
        assert!(
            merge_persistent_pe_payload_journals(None, Some(&format!("{record}{record}"))).is_err()
        );
        assert!(merge_persistent_pe_payload_journals(
            None,
            Some("{11111111-1111-1111-1111-111111111111}\r\n{22222222-2222-2222-2222-222222222222}\r\n")
        )
        .is_err());
    }

    #[test]
    fn save_only_destination_is_session_scoped_and_rejects_path_syntax() {
        let destination =
            save_only_driver_destination("C:", "{11111111-1111-1111-1111-111111111111}").unwrap();
        assert!(destination
            .ends_with("LetRecovery_Drivers\\session-11111111-1111-1111-1111-111111111111"));
        assert!(save_only_driver_destination("C:", "../escape").is_err());
        assert!(save_only_driver_destination("C:", "").is_err());
    }
}
#[cfg(all(test, feature = "ci-automation"))]
mod ci_automation_tests {
    use super::{
        atomic_write_ci_file, find_ci_install_context_in_roots, find_ci_run_context_in_roots,
        valid_ci_run_id, CiFaultInjection,
    };

    #[test]
    fn ci_fault_parser_accepts_only_supported_boundaries() {
        assert_eq!(
            CiFaultInjection::parse("before_target_write").unwrap(),
            CiFaultInjection::BeforeTargetWrite
        );
        assert_eq!(
            CiFaultInjection::parse("after_target_format").unwrap(),
            CiFaultInjection::AfterTargetFormat
        );
        assert!(CiFaultInjection::parse("before_target_format").is_err());
    }

    fn write_active(root: &std::path::Path, run_id: &str, phase: &str) {
        let state = root.join("LR-CI").join("state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(
            state.join("active-run.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "run_id": run_id,
                "phase": phase,
                "updated_utc": "2026-08-20T00:00:00Z"
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn ci_run_id_and_active_record_selection_are_strict_and_unique() {
        assert!(valid_ci_run_id("0123456789abcdef0123456789abcdef"));
        assert!(!valid_ci_run_id("short"));
        assert!(!valid_ci_run_id("0123456789abcdef0123456789abcdeg"));
        let first = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-ci-context-first",
        )
        .unwrap();
        let second = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-ci-context-second",
        )
        .unwrap();
        let run_id = "0123456789abcdef0123456789abcdef";
        write_active(first.path(), run_id, "handoff_committed");
        let selected = find_ci_run_context_in_roots(vec![first.path().to_owned()], run_id).unwrap();
        assert_eq!(selected.run_id, run_id);
        let stale_run_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        write_active(second.path(), stale_run_id, "handoff_committed");
        let selected = find_ci_run_context_in_roots(
            vec![first.path().to_owned(), second.path().to_owned()],
            run_id,
        )
        .unwrap();
        assert_eq!(selected.run_id, run_id);
        write_active(second.path(), run_id, "handoff_committed");
        assert!(find_ci_run_context_in_roots(
            vec![first.path().to_owned(), second.path().to_owned()],
            run_id
        )
        .is_err());
    }

    #[test]
    fn ci_final_file_replaces_atomically_and_round_trips() {
        let directory = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-ci-final",
        )
        .unwrap();
        let path = directory.path().join("final.json");
        atomic_write_ci_file(&path, br#"{"terminal":false}"#).unwrap();
        atomic_write_ci_file(&path, br#"{"terminal":true}"#).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(value["terminal"], true);
    }

    #[test]
    fn install_fault_record_requires_exact_session_and_unique_plain_record() {
        let first = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-ci-install-fault-first",
        )
        .unwrap();
        let second = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-ci-install-fault-second",
        )
        .unwrap();
        let run_id = "0123456789abcdef0123456789abcdef";
        let session_id = "11111111111111111111111111111111";
        let write_fault =
            |root: &std::path::Path, session: &str, fault: &str, preserve_personal_files: bool| {
                let state = root.join("LR-CI").join("state");
                std::fs::create_dir_all(&state).unwrap();
                std::fs::write(
                    state.join("active-run.json"),
                    serde_json::to_vec(&serde_json::json!({
                        "schema_version": 1,
                        "run_id": run_id,
                        "phase": "handoff_committed",
                        "session_id": session,
                        "fault_injection": fault,
                        "preserve_personal_files": preserve_personal_files,
                        "updated_utc": "2026-08-23T00:00:00Z"
                    }))
                    .unwrap(),
                )
                .unwrap();
            };
        write_fault(first.path(), session_id, "after_target_format", false);
        let selected = find_ci_install_context_in_roots(vec![first.path().to_owned()], session_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            selected.fault_injection,
            Some(CiFaultInjection::AfterTargetFormat)
        );
        assert!(find_ci_install_context_in_roots(
            vec![first.path().to_owned()],
            "22222222222222222222222222222222"
        )
        .unwrap()
        .is_none());
        write_fault(second.path(), session_id, "after_target_format", false);
        assert!(find_ci_install_context_in_roots(
            vec![first.path().to_owned(), second.path().to_owned()],
            session_id
        )
        .is_err());
        write_fault(second.path(), session_id, "unknown", false);
        assert!(
            find_ci_install_context_in_roots(vec![second.path().to_owned()], session_id).is_err()
        );
        write_fault(second.path(), session_id, "none", true);
        let preservation =
            find_ci_install_context_in_roots(vec![second.path().to_owned()], session_id)
                .unwrap()
                .unwrap();
        assert_eq!(preservation.fault_injection, None);
        write_fault(second.path(), session_id, "none", false);
        let ordinary_success =
            find_ci_install_context_in_roots(vec![second.path().to_owned()], session_id)
                .unwrap()
                .unwrap();
        assert_eq!(ordinary_success.fault_injection, None);
    }
}
