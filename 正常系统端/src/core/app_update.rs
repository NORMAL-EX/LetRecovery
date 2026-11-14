//! Incremental self-update of the desktop release package.
//!
//! `check_for_update` downloads the fixed HTTPS manifest that the release workflow publishes
//! through the authenticated release callback and offers it only when its `build_epoch` is newer
//! than this executable. `install_update` then:
//!
//! 1. hashes the installed files and plans every path with `lr_core::update_manifest`;
//! 2. downloads only changed content from the content-addressed CDN (`<base><sha256>`), resuming
//!    interrupted transfers with HTTP Range and verifying size and SHA-256 again from disk;
//! 3. rebuilds an official bundled PE WIM from its per-file index — usually only
//!    `LetRecoveryPE.exe` and `PECMD.INI` change — and proves the result equals the official index
//!    and passes a full wimlib verification, falling back to the complete WIM otherwise. A PE the
//!    user customised (`bin/pe` is a user-managed boundary) is preserved;
//! 4. replaces files through a journaled rename transaction on the package volume with the main
//!    executable last. Any failure before the commit moves every file back.
//!
//! A transaction interrupted by a crash or power loss is resolved by
//! `recover_interrupted_update_at_startup` before the dependency check: rolled forward when every
//! rename already happened (the executable is replaced last, so this is exactly "the executable
//! was replaced"), otherwise rolled back.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use lr_core::update_manifest::{
    plan_image_patch, plan_obsolete_file, plan_package_file, validate_package_path,
    verify_image_listing, ImagePatchPlan, LocalFileState, ObjectRef, PackageAction, PackageFile,
    UpdateManifest, WimContentIndex, MAIN_EXECUTABLE, MAX_MANIFEST_BYTES, RELEASE_MANIFEST_PATH,
    RESTORABLE_IMAGE_ATTRIBUTES,
};
use serde::{Deserialize, Serialize};

const WORK_DIRECTORY: &str = "update";
const JOURNAL_FILE: &str = "journal.json";
const SESSION_PREFIX: &str = "session-";
const JOURNAL_SCHEMA_VERSION: u32 = 1;
const DOWNLOAD_ATTEMPTS: u32 = 4;
/// Per-operation network timeout (connect, response headers and every body read). It detects a
/// stalled transfer; the next attempt resumes the partial object with HTTP Range.
const NETWORK_STALL_TIMEOUT: Duration = Duration::from_secs(60);
const CLEANUP_ATTEMPTS: u32 = 60;
const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x20;

static RESTART_REQUESTED: AtomicBool = AtomicBool::new(false);
static RELAUNCH_EXECUTABLE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Whether this process may check for and install updates at all.
pub fn is_enabled() -> bool {
    lr_core::branding::feature::SELF_UPDATE
        && !crate::build_info::DEV
        && !cfg!(feature = "non-elevated-tests")
        && !crate::core::disk::DiskManager::is_pe_environment()
}

fn package_root() -> PathBuf {
    crate::utils::path::get_exe_dir()
}

fn package_path(root: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(root.to_path_buf(), |path, component| path.join(component))
}

fn work_directory(root: &Path) -> PathBuf {
    root.join(WORK_DIRECTORY)
}

fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow!("路径包含无法表示的字符: {}", path.display()))
}

fn read_installed_manifest(root: &Path) -> Result<UpdateManifest> {
    let path = package_path(root, RELEASE_MANIFEST_PATH);
    let bytes = fs::read(&path).with_context(|| format!("无法读取发布清单 {}", path.display()))?;
    UpdateManifest::parse(&bytes)
        .map_err(|error| anyhow!("发布清单 {} 无效: {error}", path.display()))
}

// ============================================================================
// Network
// ============================================================================

#[derive(Debug)]
struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

fn is_cancelled(error: &anyhow::Error) -> bool {
    error.downcast_ref::<Cancelled>().is_some()
}

fn build_client(direct: bool) -> Result<reqwest::blocking::Client> {
    let mut builder = reqwest::blocking::Client::builder()
        .user_agent(format!(
            "LetRecovery/{} (incremental-update)",
            env!("BUILD_VERSION")
        ))
        .connect_timeout(Duration::from_secs(20))
        .timeout(NETWORK_STALL_TIMEOUT);
    if direct {
        // Same narrowly scoped compatibility path as the fixed v4 catalogue: bypass a stale
        // machine proxy and use the bundled WebPKI roots without weakening certificate checks.
        builder = builder.use_rustls_tls().no_proxy();
    }
    builder.build().context("创建 HTTP 客户端失败")
}

fn fetch_small(url: &str, limit: usize) -> Result<Vec<u8>> {
    let attempt = |direct: bool| -> Result<Vec<u8>> {
        let response = build_client(direct)?
            .get(url)
            .send()
            .with_context(|| format!("请求 {url} 失败"))?;
        if !response.status().is_success() {
            bail!("{url} 返回 HTTP {}", response.status());
        }
        let mut bytes = Vec::new();
        response
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .with_context(|| format!("读取 {url} 失败"))?;
        if bytes.len() > limit {
            bail!("{url} 的响应超过 {limit} 字节上限");
        }
        Ok(bytes)
    };
    attempt(false).or_else(|native| {
        log::warn!("[UPDATE] 系统 TLS/代理路径请求失败，改用直连 Rustls 重试: {native:#}");
        attempt(true)
            .map_err(|direct| anyhow!("system TLS/proxy: {native:#}; direct rustls: {direct:#}"))
    })
}

fn object_url(sha256: &str) -> String {
    format!("{}{sha256}", lr_core::branding::UPDATE_OBJECT_BASE_URL)
}

fn verify_file(path: &Path, object: &ObjectRef) -> Result<()> {
    let length = fs::metadata(path)
        .with_context(|| format!("无法读取 {}", path.display()))?
        .len();
    if length != object.size {
        bail!(
            "{} 的大小为 {length} 字节，应为 {} 字节",
            path.display(),
            object.size
        );
    }
    let actual = lr_core::hash::sha256_file(path, |_| {})
        .with_context(|| format!("无法计算 {} 的 SHA-256", path.display()))?;
    if actual != object.sha256 {
        bail!(
            "{} 的 SHA-256 为 {actual}，应为 {}",
            path.display(),
            object.sha256
        );
    }
    Ok(())
}

/// One transfer attempt into `partial`, resuming from its current length when the server honours
/// the exact Range. `on_bytes` receives the bytes of this object present so far.
fn download_attempt(
    object: &ObjectRef,
    partial: &Path,
    direct: bool,
    cancel: &AtomicBool,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<()> {
    let mut existing = fs::metadata(partial)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if existing >= object.size {
        let _ = fs::remove_file(partial);
        existing = 0;
    }
    let mut hasher = lr_core::hash::Sha256Stream::new();
    if existing > 0 {
        let mut file = File::open(partial)?;
        let mut buffer = vec![0u8; 1 << 20];
        let mut remaining = existing;
        while remaining > 0 {
            let wanted = remaining.min(buffer.len() as u64) as usize;
            file.read_exact(&mut buffer[..wanted])?;
            hasher.update(&buffer[..wanted]);
            remaining -= wanted as u64;
        }
    }
    let url = object_url(&object.sha256);
    let mut request = build_client(direct)?.get(&url);
    if existing > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={existing}-"));
    }
    let mut response = request.send().with_context(|| format!("请求 {url} 失败"))?;
    let status = response.status();
    let mut file = if existing > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT {
        let expected = format!("bytes {existing}-{}/{}", object.size - 1, object.size);
        let actual = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if actual != expected {
            let _ = fs::remove_file(partial);
            bail!("{url} 返回了意外的 Content-Range: {actual:?}，应为 {expected:?}");
        }
        OpenOptions::new().append(true).open(partial)?
    } else if status == reqwest::StatusCode::OK {
        existing = 0;
        hasher = lr_core::hash::Sha256Stream::new();
        File::create(partial)?
    } else {
        bail!("{url} 返回 HTTP {status}");
    };
    on_bytes(existing);
    let mut written = existing;
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(Cancelled.into());
        }
        let count = response
            .read(&mut buffer)
            .with_context(|| format!("读取 {url} 失败"))?;
        if count == 0 {
            break;
        }
        written += count as u64;
        if written > object.size {
            drop(file);
            let _ = fs::remove_file(partial);
            bail!("{url} 返回的数据超过声明的 {} 字节", object.size);
        }
        file.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
        on_bytes(written);
    }
    file.sync_all()?;
    drop(file);
    if written != object.size {
        bail!("{url} 的连接在 {written}/{} 字节处中断", object.size);
    }
    if hasher.finish_hex() != object.sha256 {
        let _ = fs::remove_file(partial);
        bail!("{url} 的 SHA-256 与发布清单不一致");
    }
    Ok(())
}

/// Downloads one content-addressed object to `destination` (verified size and SHA-256).
fn download_object(
    object: &ObjectRef,
    destination: &Path,
    cancel: &AtomicBool,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<()> {
    if verify_file(destination, object).is_ok() {
        on_bytes(object.size);
        return Ok(());
    }
    let partial = destination.with_extension("part");
    let mut direct = false;
    let mut last_error = None;
    for attempt in 1..=DOWNLOAD_ATTEMPTS {
        if cancel.load(Ordering::SeqCst) {
            return Err(Cancelled.into());
        }
        match download_attempt(object, &partial, direct, cancel, on_bytes) {
            Ok(()) => {
                verify_file(&partial, object)?;
                fs::rename(&partial, destination)
                    .with_context(|| format!("无法发布已下载的对象 {}", destination.display()))?;
                return Ok(());
            }
            Err(error) if is_cancelled(&error) => return Err(error),
            Err(error) => {
                log::warn!(
                    "[UPDATE] 下载 {} 第 {attempt}/{DOWNLOAD_ATTEMPTS} 次失败: {error:#}",
                    object.sha256
                );
                // A system proxy or Schannel failure is retried through the direct client.
                direct = !direct || attempt >= 2;
                last_error = Some(error);
                if attempt < DOWNLOAD_ATTEMPTS {
                    std::thread::sleep(Duration::from_secs(u64::from(attempt) * 2));
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("下载失败")))
}

// ============================================================================
// Update check
// ============================================================================

#[derive(Clone, Debug)]
pub struct UpdateOffer {
    pub manifest: UpdateManifest,
    pub current_version: String,
}

/// Returns an offer when the published release is newer than this executable.
pub fn check_for_update() -> Result<Option<UpdateOffer>> {
    let root = package_root();
    read_installed_manifest(&root).context("当前程序目录不是可增量升级的发布包")?;
    let bytes = fetch_small(lr_core::branding::UPDATE_MANIFEST_URL, MAX_MANIFEST_BYTES)?;
    let manifest =
        UpdateManifest::parse(&bytes).map_err(|error| anyhow!("服务器升级清单无效: {error}"))?;
    let local = crate::build_info::build_epoch();
    if !manifest.is_newer_than(local) {
        log::info!(
            "[UPDATE] 已是最新版本（本机 build_epoch={local}，服务器 {} build_epoch={}）",
            manifest.version,
            manifest.build_epoch
        );
        return Ok(None);
    }
    log::info!(
        "[UPDATE] 发现新版本 {}（build_epoch={}），本机 {}（build_epoch={local}）",
        manifest.version,
        manifest.build_epoch,
        env!("BUILD_VERSION")
    );
    Ok(Some(UpdateOffer {
        manifest,
        current_version: crate::build_info::display_version(),
    }))
}

// ============================================================================
// Progress and results
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdatePhase {
    Preparing,
    Downloading,
    BuildingImage,
    VerifyingImage,
    Applying,
    Finished,
}

impl UpdatePhase {
    fn from_index(index: u32) -> Self {
        match index {
            1 => Self::Downloading,
            2 => Self::BuildingImage,
            3 => Self::VerifyingImage,
            4 => Self::Applying,
            5 => Self::Finished,
            _ => Self::Preparing,
        }
    }

    fn index(self) -> u32 {
        match self {
            Self::Preparing => 0,
            Self::Downloading => 1,
            Self::BuildingImage => 2,
            Self::VerifyingImage => 3,
            Self::Applying => 4,
            Self::Finished => 5,
        }
    }
}

/// Lock-free progress shared between the worker and the dialog timer.
#[derive(Default)]
pub struct UpdateProgress {
    phase: AtomicU32,
    permille: AtomicU32,
    completed_bytes: AtomicU64,
    total_bytes: AtomicU64,
}

#[derive(Clone, Copy, Debug)]
pub struct UpdateProgressSnapshot {
    pub phase: UpdatePhase,
    pub permille: u32,
    pub completed_bytes: u64,
    pub total_bytes: u64,
}

impl UpdateProgress {
    fn set_phase(&self, phase: UpdatePhase, permille: u32) {
        self.phase.store(phase.index(), Ordering::SeqCst);
        self.set_permille(permille);
    }

    fn set_permille(&self, permille: u32) {
        // Overall progress never moves backwards, whatever order late updates arrive in.
        self.permille
            .fetch_max(permille.min(1000), Ordering::SeqCst);
    }

    fn set_bytes(&self, completed: u64, total: u64) {
        self.completed_bytes.store(completed, Ordering::SeqCst);
        self.total_bytes.store(total, Ordering::SeqCst);
    }

    pub fn snapshot(&self) -> UpdateProgressSnapshot {
        UpdateProgressSnapshot {
            phase: UpdatePhase::from_index(self.phase.load(Ordering::SeqCst)),
            permille: self.permille.load(Ordering::SeqCst),
            completed_bytes: self.completed_bytes.load(Ordering::SeqCst),
            total_bytes: self.total_bytes.load(Ordering::SeqCst),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RollbackStatus {
    /// Nothing on disk had been replaced yet.
    NotNeeded,
    RolledBack,
    /// Some files could not be moved back; the journal is kept for the next start.
    Incomplete(String),
}

#[derive(Debug)]
pub enum UpdateFailure {
    Cancelled,
    Failed {
        message: String,
        rollback: RollbackStatus,
    },
}

#[derive(Clone, Debug)]
pub struct UpdateSuccess {
    pub version: String,
    pub preserved_custom_pe: bool,
    pub downloaded_bytes: u64,
}

fn failure(context: String, error: anyhow::Error) -> UpdateFailure {
    if is_cancelled(&error) {
        return UpdateFailure::Cancelled;
    }
    log::error!("[UPDATE] {context}: {error:#}");
    UpdateFailure::Failed {
        message: format!("{context}\r\n{error:#}"),
        rollback: RollbackStatus::NotNeeded,
    }
}

// ============================================================================
// Journal and transaction
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JournalState {
    Applying,
    Committed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JournalOperation {
    /// Package-relative target.
    target: String,
    /// Session-relative new content; `None` deletes an obsolete file.
    staged: Option<String>,
    /// Session-relative place for the previous content.
    backup: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Journal {
    schema_version: u32,
    session: String,
    version: String,
    state: JournalState,
    operations: Vec<JournalOperation>,
}

fn is_session_name(name: &str) -> bool {
    name.strip_prefix(SESSION_PREFIX).is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix.len() <= 32
            && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn is_session_slot(value: &str, directory: &str) -> bool {
    value.strip_prefix(directory).is_some_and(|suffix| {
        suffix.len() == 7
            && suffix.starts_with('/')
            && suffix[1..].bytes().all(|byte| byte.is_ascii_digit())
    })
}

impl Journal {
    fn validate(&self) -> Result<()> {
        if self.schema_version != JOURNAL_SCHEMA_VERSION || !is_session_name(&self.session) {
            bail!("升级日志格式无效");
        }
        for operation in &self.operations {
            if !operation.target.eq_ignore_ascii_case(RELEASE_MANIFEST_PATH) {
                validate_package_path(&operation.target).map_err(|error| anyhow!(error))?;
            }
            if !is_session_slot(&operation.backup, "backup")
                || operation
                    .staged
                    .as_deref()
                    .is_some_and(|staged| !is_session_slot(staged, "staging"))
            {
                bail!("升级日志中的暂存路径无效");
            }
        }
        Ok(())
    }
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Same-volume rename that never replaces an existing file; the rename is flushed before return.
fn move_file(from: &Path, to: &Path) -> Result<()> {
    use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};
    let (source, destination) = (wide(from), wide(to));
    // SAFETY: both buffers are NUL-terminated UTF-16 paths that outlive the call.
    unsafe {
        MoveFileExW(
            windows::core::PCWSTR(source.as_ptr()),
            windows::core::PCWSTR(destination.as_ptr()),
            MOVEFILE_WRITE_THROUGH,
        )
    }
    .with_context(|| format!("无法把 {} 移动到 {}", from.display(), to.display()))
}

fn replace_file(from: &Path, to: &Path) -> Result<()> {
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let (source, destination) = (wide(from), wide(to));
    // SAFETY: both buffers are NUL-terminated UTF-16 paths that outlive the call.
    unsafe {
        MoveFileExW(
            windows::core::PCWSTR(source.as_ptr()),
            windows::core::PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .with_context(|| format!("无法替换 {}", to.display()))
}

fn set_file_attributes(path: &Path, attributes: u32) -> Result<()> {
    use windows::Win32::Storage::FileSystem::{SetFileAttributesW, FILE_FLAGS_AND_ATTRIBUTES};
    let path_wide = wide(path);
    // SAFETY: NUL-terminated UTF-16 path that outlives the call.
    unsafe {
        SetFileAttributesW(
            windows::core::PCWSTR(path_wide.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(attributes),
        )
    }
    .with_context(|| format!("无法设置 {} 的文件属性", path.display()))
}

fn write_durable(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = File::create(path).with_context(|| format!("无法创建 {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn write_journal(work: &Path, journal: &Journal) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(journal).context("序列化升级日志失败")?;
    let temporary = work.join(format!("{JOURNAL_FILE}.tmp"));
    write_durable(&temporary, &bytes)?;
    replace_file(&temporary, &work.join(JOURNAL_FILE))
}

fn load_journal(work: &Path) -> Result<Option<Journal>> {
    let path = work.join(JOURNAL_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("无法读取 {}", path.display())),
    };
    let journal: Journal = serde_json::from_slice(&bytes).context("升级日志无法解析")?;
    journal.validate()?;
    Ok(Some(journal))
}

fn session_path(session: &Path, slot: &str) -> PathBuf {
    package_path(session, slot)
}

/// Undoes the operations recorded in `progress` (backed up, installed) in reverse order.
fn rollback_operations(
    root: &Path,
    session: &Path,
    operations: &[JournalOperation],
    progress: &[(bool, bool)],
) -> RollbackStatus {
    let mut problems = Vec::new();
    for (operation, (backed_up, installed)) in operations.iter().zip(progress).rev() {
        let target = package_path(root, &operation.target);
        if *installed {
            if let Some(staged) = &operation.staged {
                if let Err(error) = move_file(&target, &session_path(session, staged)) {
                    problems.push(format!("{error:#}"));
                    continue;
                }
            }
        }
        if *backed_up {
            if let Err(error) = move_file(&session_path(session, &operation.backup), &target) {
                problems.push(format!("{error:#}"));
            }
        }
    }
    if problems.is_empty() {
        RollbackStatus::RolledBack
    } else {
        RollbackStatus::Incomplete(problems.join("；"))
    }
}

fn apply_operations(
    root: &Path,
    session: &Path,
    operations: &[JournalOperation],
) -> std::result::Result<(), (anyhow::Error, RollbackStatus)> {
    let mut progress: Vec<(bool, bool)> = Vec::with_capacity(operations.len());
    for operation in operations {
        let target = package_path(root, &operation.target);
        let mut state = (false, false);
        let mut step = || -> Result<()> {
            if exists(&target) {
                move_file(&target, &session_path(session, &operation.backup))?;
                state.0 = true;
            }
            if let Some(staged) = &operation.staged {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("无法创建 {}", parent.display()))?;
                }
                move_file(&session_path(session, staged), &target)?;
                state.1 = true;
            }
            Ok(())
        };
        let result = step();
        progress.push(state);
        if let Err(error) = result {
            let rollback = rollback_operations(root, session, operations, &progress);
            return Err((error, rollback));
        }
    }
    Ok(())
}

fn operation_applied(root: &Path, session: &Path, operation: &JournalOperation) -> bool {
    let target_exists = exists(&package_path(root, &operation.target));
    match &operation.staged {
        Some(staged) => target_exists && !exists(&session_path(session, staged)),
        None => !target_exists,
    }
}

enum RecoveryOutcome {
    RolledForward,
    RolledBack,
}

/// Resolves a transaction interrupted by a crash using only on-disk state.
fn recover_applying(root: &Path, work: &Path, journal: &Journal) -> Result<RecoveryOutcome> {
    let session = work.join(&journal.session);
    if journal
        .operations
        .iter()
        .all(|operation| operation_applied(root, &session, operation))
    {
        let mut committed = journal.clone();
        committed.state = JournalState::Committed;
        write_journal(work, &committed)?;
        return Ok(RecoveryOutcome::RolledForward);
    }
    let mut problems = Vec::new();
    for operation in journal.operations.iter().rev() {
        let target = package_path(root, &operation.target);
        let backup = session_path(&session, &operation.backup);
        if let Some(staged) = &operation.staged {
            let staged = session_path(&session, staged);
            if exists(&target) && !exists(&staged) {
                if let Err(error) = move_file(&target, &staged) {
                    problems.push(format!("{error:#}"));
                    continue;
                }
            }
        }
        if exists(&backup) && !exists(&target) {
            if let Err(error) = move_file(&backup, &target) {
                problems.push(format!("{error:#}"));
            }
        }
    }
    if !problems.is_empty() {
        bail!("{}", problems.join("；"));
    }
    let _ = fs::remove_dir_all(&session);
    fs::remove_file(work.join(JOURNAL_FILE)).context("无法删除已回滚的升级日志")?;
    Ok(RecoveryOutcome::RolledBack)
}

/// Removes session directories that no journal references (aborted downloads).
fn remove_unreferenced_sessions(work: &Path, keep: Option<&str>) {
    let Ok(entries) = fs::read_dir(work) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_session_name(&name) && Some(name.as_str()) != keep {
            if let Err(error) = fs::remove_dir_all(entry.path()) {
                log::warn!("[UPDATE] 无法清理旧的升级暂存目录 {name}: {error}");
            }
        }
    }
    let _ = fs::remove_file(work.join(format!("{JOURNAL_FILE}.tmp")));
}

/// Deletes the session (including the previous executable) and then its committed journal. The
/// journal is only removed while it still names this session.
fn cleanup_committed(work: &Path, session: &str) -> Result<()> {
    let directory = work.join(session);
    if exists(&directory) {
        fs::remove_dir_all(&directory)
            .with_context(|| format!("无法删除 {}", directory.display()))?;
    }
    if let Some(journal) = load_journal(work)? {
        if journal.session == session {
            fs::remove_file(work.join(JOURNAL_FILE)).context("无法删除升级日志")?;
        }
    }
    Ok(())
}

fn spawn_committed_cleanup(work: PathBuf, session: String) {
    std::thread::spawn(move || {
        // The previous process may still be exiting and keep its executable mapped.
        for attempt in 1..=CLEANUP_ATTEMPTS {
            match cleanup_committed(&work, &session) {
                Ok(()) => {
                    log::info!("[UPDATE] 已清理升级前的旧文件 {session}");
                    return;
                }
                Err(error) if attempt == CLEANUP_ATTEMPTS => {
                    log::warn!("[UPDATE] 暂时无法清理升级前的旧文件，下次启动再试: {error:#}");
                }
                Err(_) => std::thread::sleep(Duration::from_secs(1)),
            }
        }
    });
}

/// Startup hook, run while holding the single-instance mutex and before the dependency check.
pub fn recover_interrupted_update_at_startup() {
    let work = work_directory(&package_root());
    if !work.is_dir() {
        return;
    }
    match load_journal(&work) {
        Ok(None) => remove_unreferenced_sessions(&work, None),
        Ok(Some(journal)) => {
            remove_unreferenced_sessions(&work, Some(&journal.session));
            match journal.state {
                JournalState::Committed => spawn_committed_cleanup(work, journal.session),
                JournalState::Applying => {
                    match recover_applying(&package_root(), &work, &journal) {
                        Ok(RecoveryOutcome::RolledForward) => {
                            log::warn!(
                                "[UPDATE] 上次升级在全部文件替换后中断，已确认升级到 {} 完成",
                                journal.version
                            );
                            spawn_committed_cleanup(work, journal.session);
                        }
                        Ok(RecoveryOutcome::RolledBack) => log::warn!(
                            "[UPDATE] 上次升级到 {} 时中断，已恢复升级前的全部文件",
                            journal.version
                        ),
                        Err(error) => log::error!(
                            "[UPDATE] 上次升级中断且未能完全恢复，已保留升级日志供下次启动重试: {error:#}"
                        ),
                    }
                }
            }
        }
        Err(error) => log::error!("[UPDATE] 无法读取升级日志，已保留现场: {error:#}"),
    }
}

// ============================================================================
// Installation
// ============================================================================

struct PreparedPatch {
    target: PackageFile,
    index: WimContentIndex,
    plan: ImagePatchPlan,
}

fn new_session_name() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!(
        "{SESSION_PREFIX}{:08x}{:016x}",
        std::process::id(),
        nanos as u64
    )
}

fn local_state(
    root: &Path,
    relative: &str,
    cancel: &AtomicBool,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<Option<LocalFileState>> {
    let path = package_path(root, relative);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => bail!("{} 不是普通文件", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("无法读取 {}", path.display())),
    };
    let sha256 = lr_core::hash::sha256_file_cancellable(&path, |bytes| {
        if cancel.load(Ordering::SeqCst) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "cancelled",
            ));
        }
        on_bytes(bytes);
        Ok(())
    })
    .map_err(|error| {
        if error.kind() == std::io::ErrorKind::Interrupted {
            anyhow::Error::from(Cancelled)
        } else {
            anyhow!(error).context(format!("无法计算 {} 的 SHA-256", path.display()))
        }
    })?;
    Ok(Some(LocalFileState {
        size: metadata.len(),
        sha256,
    }))
}

fn prepare_patch(
    root: &Path,
    objects: &Path,
    target: &PackageFile,
    cancel: &AtomicBool,
) -> Result<PreparedPatch> {
    let index_ref = target
        .wim_index
        .as_ref()
        .ok_or_else(|| anyhow!("{} 没有逐文件索引", target.path))?;
    let index_path = objects.join(&index_ref.sha256);
    download_object(index_ref, &index_path, cancel, &mut |_| {})?;
    let index = WimContentIndex::parse(&fs::read(&index_path)?).map_err(|error| anyhow!(error))?;
    let manager = lr_core::wimlib::WimlibManager::new().map_err(|error| anyhow!(error))?;
    let local_wim = package_path(root, &target.path);
    let listing = manager
        .list_image_entries(path_text(&local_wim)?, index.image as i32)
        .map_err(|error| anyhow!(error))?;
    let plan = plan_image_patch(&listing.entries, &index);
    log::info!(
        "[UPDATE] {} 逐文件补丁：删除 {} 项，新建 {} 个目录，写入 {} 个文件",
        target.path,
        plan.delete.len(),
        plan.add_directories.len(),
        plan.add_files.len()
    );
    Ok(PreparedPatch {
        target: target.clone(),
        index,
        plan,
    })
}

/// Writes the patched image to `output`, proves it equals the official index and passes a full
/// wimlib verification, and returns its size and SHA-256.
fn build_patched_image(
    root: &Path,
    session: &Path,
    patch: &PreparedPatch,
    output: &Path,
    cancel: &Arc<AtomicBool>,
    progress: &UpdateProgress,
) -> Result<(u64, String)> {
    let objects = session.join("objects");
    let copies = session.join("files");
    let empty = session.join("empty");
    fs::create_dir_all(&empty)?;
    let mut sources = Vec::with_capacity(patch.plan.add_files.len());
    for (index, file) in patch.plan.add_files.iter().enumerate() {
        let object = objects.join(file.sha256());
        let attributes = file.attributes() & RESTORABLE_IMAGE_ATTRIBUTES;
        if attributes & !FILE_ATTRIBUTE_ARCHIVE == 0 {
            sources.push(object);
        } else {
            // wimlib captures the attributes of the source file; give HIDDEN/SYSTEM files their
            // official attributes on a private copy so shared objects keep theirs.
            let copy = copies.join(format!("{index:06}"));
            fs::copy(&object, &copy).with_context(|| format!("无法复制 {}", object.display()))?;
            set_file_attributes(&copy, attributes)?;
            sources.push(copy);
        }
    }
    let manager = lr_core::wimlib::WimlibManager::new().map_err(|error| anyhow!(error))?;
    let source_wim = package_path(root, &patch.target.path);
    let image = patch.index.image as i32;
    manager
        .write_patched_image(
            path_text(&source_wim)?,
            image,
            &patch.plan,
            &sources,
            &empty,
            path_text(output)?,
        )
        .map_err(|error| anyhow!(error))?;
    if cancel.load(Ordering::SeqCst) {
        return Err(Cancelled.into());
    }
    progress.set_phase(UpdatePhase::VerifyingImage, 880);
    let listing = manager
        .list_image_entries(path_text(output)?, image)
        .map_err(|error| anyhow!(error))?;
    verify_image_listing(&listing.entries, &patch.index)
        .map_err(|error| anyhow!("新镜像内容与官方清单不一致: {error}"))?;
    if listing.boot_index != patch.index.boot_index {
        bail!(
            "新镜像的启动索引为 {}，官方镜像为 {}",
            listing.boot_index,
            patch.index.boot_index
        );
    }
    let wimlib = lr_core::wimlib::Wimlib::new().map_err(|error| anyhow!(error))?;
    let handle = wimlib
        .open_wim_cancellable(path_text(output)?, Some(cancel.clone()))
        .map_err(|error| anyhow!(error))?;
    handle.verify().map_err(|error| {
        if cancel.load(Ordering::SeqCst) {
            anyhow::Error::from(Cancelled)
        } else {
            anyhow!("新镜像完整性校验失败: {error}")
        }
    })?;
    drop(handle);
    let size = fs::metadata(output)?.len();
    let sha256 = lr_core::hash::sha256_file(output, |_| {})?;
    Ok((size, sha256))
}

/// Settles what an earlier update left behind before a new one starts.
fn settle_previous_session(work: &Path) -> Result<()> {
    if !work.is_dir() {
        return Ok(());
    }
    if let Some(journal) = load_journal(work)? {
        if journal.state == JournalState::Applying
            || cleanup_committed(work, &journal.session).is_err()
        {
            bail!(
                "{}",
                crate::tr!("上一次升级仍在收尾，请重新启动 LetRecovery 后再试。")
            );
        }
    }
    remove_unreferenced_sessions(work, None);
    Ok(())
}

/// Installs `offer`. Runs on a worker thread; `cancel` is honoured until files start moving.
pub fn install_update(
    offer: &UpdateOffer,
    progress: &UpdateProgress,
    cancel: &Arc<AtomicBool>,
) -> std::result::Result<UpdateSuccess, UpdateFailure> {
    let root = package_root();
    let work = work_directory(&root);
    settle_previous_session(&work).map_err(|error| failure(crate::tr!("准备升级失败。"), error))?;
    let session_name = new_session_name();
    let session = work.join(&session_name);
    let result = install_in_session(&root, &work, &session_name, offer, progress, cancel);
    match &result {
        Ok(_) => {}
        Err(UpdateFailure::Failed {
            rollback: RollbackStatus::Incomplete(_),
            ..
        }) => {}
        Err(_) => {
            let _ = fs::remove_dir_all(&session);
        }
    }
    if let Err(UpdateFailure::Cancelled) = &result {
        log::info!("[UPDATE] 用户取消了升级，未修改任何程序文件");
    }
    result
}

fn install_in_session(
    root: &Path,
    work: &Path,
    session_name: &str,
    offer: &UpdateOffer,
    progress: &UpdateProgress,
    cancel: &Arc<AtomicBool>,
) -> std::result::Result<UpdateSuccess, UpdateFailure> {
    let manifest = &offer.manifest;
    let session = work.join(session_name);
    let staging = session.join("staging");
    let objects = session.join("objects");
    for directory in [
        &staging,
        &objects,
        &session.join("files"),
        &session.join("backup"),
    ] {
        fs::create_dir_all(directory)
            .with_context(|| format!("无法创建 {}", directory.display()))
            .map_err(|error| failure(crate::tr!("准备升级失败。"), error))?;
    }
    let installed = read_installed_manifest(root).map_err(|error| {
        failure(
            crate::tr!("无法读取本地发布清单，请重新下载完整安装包。"),
            error,
        )
    })?;
    progress.set_phase(UpdatePhase::Preparing, 0);

    // 1. Hash the installed files and plan every path.
    let obsolete_candidates: Vec<&PackageFile> = installed
        .files
        .iter()
        .filter(|file| manifest.file(&file.path).is_none())
        .collect();
    let hash_total: u64 = manifest
        .files
        .iter()
        .chain(obsolete_candidates.iter().copied())
        .filter_map(|file| fs::metadata(package_path(root, &file.path)).ok())
        .map(|metadata| metadata.len())
        .sum::<u64>()
        .max(1);
    let mut hashed_before = 0u64;
    let prepare_error = |error| failure(crate::tr!("检查本地文件失败。"), error);
    let mut planned: Vec<(PackageFile, PackageAction)> = Vec::new();
    let mut obsolete: Vec<PackageFile> = Vec::new();
    for (target, is_obsolete) in manifest
        .files
        .iter()
        .map(|file| (file, false))
        .chain(obsolete_candidates.iter().map(|file| (*file, true)))
    {
        let mut on_bytes = |bytes: u64| {
            progress.set_permille(((hashed_before + bytes) * 50 / hash_total) as u32);
        };
        let local =
            local_state(root, &target.path, cancel, &mut on_bytes).map_err(prepare_error)?;
        hashed_before += local.as_ref().map(|state| state.size).unwrap_or_default();
        if is_obsolete {
            if plan_obsolete_file(target, local.as_ref()) == PackageAction::DeleteObsolete {
                log::info!("[UPDATE] 删除新版本不再包含的文件 {}", target.path);
                obsolete.push(target.clone());
            }
            continue;
        }
        let action = plan_package_file(target, local.as_ref(), installed.file(&target.path));
        if action != PackageAction::Unchanged {
            log::info!("[UPDATE] {} -> {action:?}", target.path);
        }
        planned.push((target.clone(), action));
    }
    let preserved_custom_pe = planned
        .iter()
        .any(|(_, action)| *action == PackageAction::PreserveCustomizedImage);

    // 2. Prepare per-file patches of the official bundled PE.
    let mut patches = Vec::new();
    for (target, action) in planned.iter_mut() {
        if *action != PackageAction::PatchImage {
            continue;
        }
        match prepare_patch(root, &objects, target, cancel) {
            Ok(patch) => patches.push(patch),
            Err(error) if is_cancelled(&error) => return Err(UpdateFailure::Cancelled),
            Err(error) => {
                log::warn!(
                    "[UPDATE] 无法为 {} 生成逐文件补丁，改为下载完整镜像: {error:#}",
                    target.path
                );
                *action = PackageAction::Download;
            }
        }
    }

    // 3. Download every needed object once.
    let mut needed: Vec<ObjectRef> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let downloads = planned
        .iter()
        .filter(|(_, action)| *action == PackageAction::Download)
        .map(|(target, _)| ObjectRef {
            sha256: target.sha256.clone(),
            size: target.size,
        });
    for object in downloads.chain(
        patches
            .iter()
            .flat_map(|patch| patch.plan.required_objects()),
    ) {
        if seen.insert(object.sha256.clone()) {
            needed.push(object);
        }
    }
    let download_total: u64 = needed.iter().map(|object| object.size).sum();
    log::info!(
        "[UPDATE] 需要下载 {} 个对象，共 {download_total} 字节",
        needed.len()
    );
    progress.set_phase(UpdatePhase::Downloading, 50);
    progress.set_bytes(0, download_total);
    let mut completed = 0u64;
    for object in &needed {
        let mut on_bytes = |bytes: u64| {
            let done = completed + bytes.min(object.size);
            progress.set_bytes(done, download_total);
            progress.set_permille(50 + (done * 750 / download_total.max(1)) as u32);
        };
        download_object(object, &objects.join(&object.sha256), cancel, &mut on_bytes)
            .map_err(|error| failure(crate::tr!("下载更新文件失败。"), error))?;
        completed += object.size;
    }

    // 4. Stage complete files and rebuilt images.
    let mut staged: Vec<(String, PathBuf)> = Vec::new();
    let mut local_manifest = manifest.clone();
    let mut slot = 0usize;
    let mut next_slot = || {
        slot += 1;
        format!("staging/{slot:06}")
    };
    for patch in &patches {
        if cancel.load(Ordering::SeqCst) {
            return Err(UpdateFailure::Cancelled);
        }
        let local_wim = package_path(root, &patch.target.path);
        if patch.plan.is_empty() {
            // Same content as the official image, different bytes: keep it and remember it.
            let size = fs::metadata(&local_wim)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            let sha256 = lr_core::hash::sha256_file(&local_wim, |_| {}).unwrap_or_default();
            local_manifest.set_local_identity(&patch.target.path, size, &sha256);
            continue;
        }
        progress.set_phase(UpdatePhase::BuildingImage, 800);
        let slot_name = next_slot();
        let output = session_path(&session, &slot_name);
        match build_patched_image(root, &session, patch, &output, cancel, progress) {
            Ok((size, sha256)) => {
                local_manifest.set_local_identity(&patch.target.path, size, &sha256);
                staged.push((patch.target.path.clone(), output));
            }
            Err(error) if is_cancelled(&error) => return Err(UpdateFailure::Cancelled),
            Err(error) => {
                log::warn!(
                    "[UPDATE] 逐文件更新 {} 失败，改为下载完整镜像: {error:#}",
                    patch.target.path
                );
                let _ = fs::remove_file(&output);
                let object = ObjectRef {
                    sha256: patch.target.sha256.clone(),
                    size: patch.target.size,
                };
                let path = objects.join(&object.sha256);
                progress.set_phase(UpdatePhase::Downloading, 800);
                progress.set_bytes(0, object.size);
                download_object(&object, &path, cancel, &mut |bytes| {
                    progress.set_bytes(bytes, object.size)
                })
                .map_err(|error| failure(crate::tr!("下载更新文件失败。"), error))?;
                fs::copy(&path, &output)
                    .with_context(|| format!("无法暂存 {}", patch.target.path))
                    .and_then(|_| verify_file(&output, &object))
                    .map_err(|error| failure(crate::tr!("下载更新文件失败。"), error))?;
                staged.push((patch.target.path.clone(), output));
            }
        }
    }
    for (target, action) in &planned {
        if *action != PackageAction::Download {
            continue;
        }
        let object = ObjectRef {
            sha256: target.sha256.clone(),
            size: target.size,
        };
        let output = session_path(&session, &next_slot());
        fs::copy(objects.join(&object.sha256), &output)
            .with_context(|| format!("无法暂存 {}", target.path))
            .and_then(|_| verify_file(&output, &object))
            .map_err(|error| failure(crate::tr!("下载更新文件失败。"), error))?;
        staged.push((target.path.clone(), output));
    }
    let manifest_slot = next_slot();
    write_durable(
        &session_path(&session, &manifest_slot),
        local_manifest.to_pretty_json().as_bytes(),
    )
    .map_err(|error| failure(crate::tr!("准备升级失败。"), error))?;
    // Downloaded objects are no longer needed; free the space before files are swapped.
    let _ = fs::remove_dir_all(&objects);
    let _ = fs::remove_dir_all(session.join("files"));

    // 5. Journaled transaction: payload files, release manifest, obsolete files, executable.
    let staged_slot = |path: &Path| {
        path.strip_prefix(&session)
            .ok()
            .and_then(Path::to_str)
            .map(|slot| slot.replace('\\', "/"))
    };
    let mut ordered: Vec<(String, Option<String>)> = Vec::new();
    let mut executable = None;
    staged.sort_by(|left, right| left.0.to_lowercase().cmp(&right.0.to_lowercase()));
    for (target, path) in &staged {
        let slot = staged_slot(path);
        if target.eq_ignore_ascii_case(MAIN_EXECUTABLE) {
            executable = Some((target.clone(), slot));
        } else {
            ordered.push((target.clone(), slot));
        }
    }
    ordered.push((RELEASE_MANIFEST_PATH.to_owned(), Some(manifest_slot)));
    ordered.extend(obsolete.iter().map(|file| (file.path.clone(), None)));
    ordered.extend(executable);
    let operations: Vec<JournalOperation> = ordered
        .into_iter()
        .enumerate()
        .map(|(index, (target, staged))| JournalOperation {
            target,
            staged,
            backup: format!("backup/{:06}", index + 1),
        })
        .collect();
    let journal = Journal {
        schema_version: JOURNAL_SCHEMA_VERSION,
        session: session_name.to_owned(),
        version: manifest.version.clone(),
        state: JournalState::Applying,
        operations,
    };
    journal
        .validate()
        .map_err(|error| failure(crate::tr!("准备升级失败。"), error))?;
    if cancel.load(Ordering::SeqCst) {
        return Err(UpdateFailure::Cancelled);
    }
    progress.set_phase(UpdatePhase::Applying, 960);
    write_journal(work, &journal).map_err(|error| failure(crate::tr!("准备升级失败。"), error))?;
    if let Err((error, rollback)) = apply_operations(root, &session, &journal.operations) {
        log::error!("[UPDATE] 替换程序文件失败，回滚结果 {rollback:?}: {error:#}");
        if rollback == RollbackStatus::RolledBack {
            let _ = fs::remove_file(work.join(JOURNAL_FILE));
        }
        return Err(UpdateFailure::Failed {
            message: format!("{}\r\n{error:#}", crate::tr!("替换程序文件失败。")),
            rollback,
        });
    }
    let mut committed = journal;
    committed.state = JournalState::Committed;
    if let Err(error) = write_journal(work, &committed) {
        // Every rename already happened; startup recovery rolls this journal forward.
        log::warn!("[UPDATE] 无法写入升级完成标记，将在下次启动时确认: {error:#}");
    }
    let _ = fs::remove_dir_all(&staging);
    let _ = fs::remove_dir_all(session.join("empty"));
    progress.set_phase(UpdatePhase::Finished, 1000);
    if let Ok(mut relaunch) = RELAUNCH_EXECUTABLE.lock() {
        *relaunch = Some(package_path(root, MAIN_EXECUTABLE));
    }
    log::info!(
        "[UPDATE] 已升级到 {}，下载 {download_total} 字节；保留自定义 PE: {preserved_custom_pe}",
        manifest.version
    );
    Ok(UpdateSuccess {
        version: manifest.version.clone(),
        preserved_custom_pe,
        downloaded_bytes: download_total,
    })
}

// ============================================================================
// Restart
// ============================================================================

pub fn request_restart() {
    RESTART_REQUESTED.store(true, Ordering::SeqCst);
}

pub fn restart_requested() -> bool {
    RESTART_REQUESTED.load(Ordering::SeqCst)
}

/// Starts the updated executable. Called after the main window closed and the single-instance
/// mutex was released; the elevated token is inherited, so the argument-free GUI route does not
/// prompt for elevation again.
pub fn relaunch_updated_executable() {
    let executable = RELAUNCH_EXECUTABLE
        .lock()
        .ok()
        .and_then(|path| path.clone())
        .unwrap_or_else(|| package_path(&package_root(), MAIN_EXECUTABLE));
    let directory = executable
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(package_root);
    match std::process::Command::new(&executable)
        .current_dir(&directory)
        .spawn()
    {
        Ok(child) => log::info!(
            "[UPDATE] 已启动升级后的程序 {} (pid {})",
            executable.display(),
            child.id()
        ),
        Err(error) => log::error!(
            "[UPDATE] 无法启动升级后的程序 {}: {error}",
            executable.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "lr-app-update-{name}-{}-{}",
            std::process::id(),
            new_session_name()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn operation(target: &str, staged: Option<&str>, backup: usize) -> JournalOperation {
        JournalOperation {
            target: target.to_owned(),
            staged: staged.map(str::to_owned),
            backup: format!("backup/{backup:06}"),
        }
    }

    fn prepare(root: &Path, session: &Path) {
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(session.join("staging")).unwrap();
        fs::create_dir_all(session.join("backup")).unwrap();
        fs::write(root.join("LetRecovery.exe"), b"old exe").unwrap();
        fs::write(root.join("bin").join("a.dll"), b"old a").unwrap();
        fs::write(root.join("bin").join("gone.dll"), b"obsolete").unwrap();
        fs::write(session.join("staging").join("000001"), b"new a").unwrap();
        fs::write(session.join("staging").join("000002"), b"new b").unwrap();
        fs::write(session.join("staging").join("000003"), b"new exe").unwrap();
    }

    fn operations() -> Vec<JournalOperation> {
        vec![
            operation("bin/a.dll", Some("staging/000001"), 1),
            operation("bin/new/b.dll", Some("staging/000002"), 2),
            operation("bin/gone.dll", None, 3),
            operation("LetRecovery.exe", Some("staging/000003"), 4),
        ]
    }

    #[test]
    fn journal_names_and_slots_are_strict() {
        assert!(is_session_name("session-00ab12"));
        assert!(!is_session_name("session-"));
        assert!(!is_session_name("session-../x"));
        assert!(is_session_slot("backup/000001", "backup"));
        assert!(!is_session_slot("backup/../x", "backup"));
        assert!(!is_session_slot("staging/1", "staging"));
        let journal = Journal {
            schema_version: JOURNAL_SCHEMA_VERSION,
            session: "session-01".to_owned(),
            version: "v2026.11.13".to_owned(),
            state: JournalState::Applying,
            operations: vec![operation("config.json", Some("staging/000001"), 1)],
        };
        assert!(journal.validate().is_err());
    }

    #[test]
    fn transaction_applies_in_order_and_rolls_back_on_failure() {
        let root = temporary_root("apply");
        let session = root.join("update").join("session-01");
        prepare(&root, &session);
        let operations = operations();
        apply_operations(&root, &session, &operations).unwrap();
        assert_eq!(fs::read(root.join("bin/a.dll")).unwrap(), b"new a");
        assert_eq!(fs::read(root.join("bin/new/b.dll")).unwrap(), b"new b");
        assert!(!exists(&root.join("bin/gone.dll")));
        assert_eq!(fs::read(root.join("LetRecovery.exe")).unwrap(), b"new exe");
        assert_eq!(fs::read(session.join("backup/000004")).unwrap(), b"old exe");

        // A missing staged file fails the second operation: the first must be moved back.
        let root = temporary_root("rollback");
        let session = root.join("update").join("session-02");
        prepare(&root, &session);
        fs::remove_file(session.join("staging/000002")).unwrap();
        let (_, rollback) = apply_operations(&root, &session, &operations).unwrap_err();
        assert_eq!(rollback, RollbackStatus::RolledBack);
        assert_eq!(fs::read(root.join("bin/a.dll")).unwrap(), b"old a");
        assert_eq!(fs::read(root.join("bin/gone.dll")).unwrap(), b"obsolete");
        assert_eq!(fs::read(root.join("LetRecovery.exe")).unwrap(), b"old exe");
        assert_eq!(fs::read(session.join("staging/000001")).unwrap(), b"new a");
    }

    #[test]
    fn interrupted_transaction_rolls_back_unless_every_rename_happened() {
        let root = temporary_root("recover");
        let work = root.join("update");
        let session = work.join("session-03");
        prepare(&root, &session);
        let journal = Journal {
            schema_version: JOURNAL_SCHEMA_VERSION,
            session: "session-03".to_owned(),
            version: "v2026.11.13".to_owned(),
            state: JournalState::Applying,
            operations: operations(),
        };
        write_journal(&work, &journal).unwrap();
        // Simulate a crash after the first file and the backup half of the second operation.
        move_file(&root.join("bin/a.dll"), &session.join("backup/000001")).unwrap();
        move_file(&session.join("staging/000001"), &root.join("bin/a.dll")).unwrap();
        assert!(matches!(
            recover_applying(&root, &work, &journal).unwrap(),
            RecoveryOutcome::RolledBack
        ));
        assert_eq!(fs::read(root.join("bin/a.dll")).unwrap(), b"old a");
        assert_eq!(fs::read(root.join("LetRecovery.exe")).unwrap(), b"old exe");
        assert!(!exists(&work.join(JOURNAL_FILE)));
        assert!(!exists(&session));

        // Every rename happened but the commit marker was not written: roll forward.
        let root = temporary_root("forward");
        let work = root.join("update");
        let session = work.join("session-04");
        prepare(&root, &session);
        let journal = Journal {
            session: "session-04".to_owned(),
            ..journal
        };
        write_journal(&work, &journal).unwrap();
        apply_operations(&root, &session, &journal.operations).unwrap();
        assert!(matches!(
            recover_applying(&root, &work, &journal).unwrap(),
            RecoveryOutcome::RolledForward
        ));
        assert_eq!(
            load_journal(&work).unwrap().unwrap().state,
            JournalState::Committed
        );
        cleanup_committed(&work, "session-04").unwrap();
        assert!(!exists(&work.join(JOURNAL_FILE)));
        assert_eq!(fs::read(root.join("LetRecovery.exe")).unwrap(), b"new exe");
    }

    #[test]
    fn progress_never_moves_backwards() {
        let progress = UpdateProgress::default();
        progress.set_phase(UpdatePhase::Downloading, 400);
        progress.set_permille(100);
        assert_eq!(progress.snapshot().permille, 400);
        progress.set_phase(UpdatePhase::Applying, 960);
        assert_eq!(progress.snapshot().phase, UpdatePhase::Applying);
        assert_eq!(progress.snapshot().permille, 960);
    }
}
