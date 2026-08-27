#[cfg(all(windows, feature = "ci-automation"))]
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use lr_core::backup_handoff::BackupOutputPolicy;

use crate::app::WorkerMessage;
use crate::core::config::{
    AuthenticatedOperationConfig, AuthenticatedOperationTask, BackupConfig, BackupFormat,
};
use crate::core::dism::{Dism, DismProgress};
use crate::tr;
use crate::ui::progress::BackupStep;
use crate::utils::reboot_pe;

struct ReboundBackupPaths {
    source_root: String,
    target: PathBuf,
}

pub(crate) fn execute_backup_workflow(
    tx: Sender<WorkerMessage>,
    authenticated_handoff: crate::core::config::AuthenticatedOperationGuard,
) {
    log::info!("========== 开始严格 LRBK2 PE 备份流程 ==========");
    let authenticated_task = match authenticated_handoff.into_task() {
        Ok(task) => task,
        Err(error) => {
            let _ = tx.send(WorkerMessage::Failed(tr!("备份任务认证失效: {}", error)));
            return;
        }
    };
    if let Err(error) = run_backup_workflow(&tx, authenticated_task) {
        log::error!("[PE BACKUP] {error:#}");
        let _ = tx.send(WorkerMessage::Failed(tr!("备份失败: {}", error)));
    }
}

fn run_backup_workflow(
    tx: &Sender<WorkerMessage>,
    authenticated_task: AuthenticatedOperationTask,
) -> Result<()> {
    let _ = tx.send(WorkerMessage::SetBackupStep(BackupStep::ReadConfig));
    let _ = tx.send(WorkerMessage::SetStatus(tr!("正在读取并验证备份任务...")));
    let config = match authenticated_task.config() {
        AuthenticatedOperationConfig::Backup(config) => config.clone(),
        _ => anyhow::bail!("authenticated task is not a backup operation"),
    };
    log::info!("[PE BACKUP] authenticated language={}", config.language);
    let handoff = config
        .handoff
        .as_ref()
        .context("LRBK2 authorization is absent")?;
    #[cfg(feature = "ci-automation")]
    crate::register_ci_authenticated_backup_context(&config.name);
    if !matches!(config.format, BackupFormat::Wim | BackupFormat::Esd) {
        anyhow::bail!("only transactional WIM/ESD backup is enabled in WinPE");
    }
    lr_core::set_active_engine(lr_core::WimEngine::from_u8(config.wim_engine));
    let selected_engine = lr_core::active_engine();
    log::info!(
        "[PE BACKUP] configured WIM capture engine={}",
        selected_engine.name()
    );
    let bound = rebind_backup_paths(&config)?;
    log::info!(
        "[PE BACKUP] session={} source={} destination={} policy={}",
        handoff.session_id,
        bound.source_root,
        bound.target.display(),
        handoff.output_policy.as_str()
    );
    let _ = tx.send(WorkerMessage::SetProgress(100));
    let _ = tx.send(WorkerMessage::SetBackupStep(BackupStep::CaptureImage));
    let _ = tx.send(WorkerMessage::SetStatus(tr!(
        "正在使用 {} 捕获系统镜像...",
        selected_engine.name()
    )));

    // The one-shot WIM/SDI and BCD objects are required only to reach this already-running X:
    // environment. Remove the exact authenticated session before scanning the source volume. This
    // both closes the PE lifecycle and prevents the running Windows volume's private
    // LetRecovery_PE directory from entering the backup image.
    authenticated_task.verify_unchanged()?;
    crate::cleanup_persistent_pe_boot_payload(authenticated_task.guard())
        .context("clean the authenticated private PE boot payload before backup capture")?;
    require_private_pe_payload_absent_from_source(&bound.source_root)?;

    let (progress_tx, progress_rx) = channel::<DismProgress>();
    let progress_messages = tx.clone();
    let relay = thread::spawn(move || {
        while let Ok(progress) = progress_rx.recv() {
            let _ = progress_messages.send(WorkerMessage::SetProgress(progress.percentage));
        }
    });
    let capture_result = capture_and_publish(&config, &bound, Some(progress_tx));
    let _ = relay.join();
    capture_result?;

    let _ = tx.send(WorkerMessage::SetBackupStep(BackupStep::VerifyBackup));
    let _ = tx.send(WorkerMessage::SetStatus(tr!(
        "备份已经完整验证并由句柄 CAS 发布"
    )));
    // The completed staged bytes were semantically verified before publication, then hashed and
    // renamed by the same locked CAS handle. Reopening the target by pathname here would add a
    // post-commit failure/race window without strengthening that proof.
    authenticated_task.verify_unchanged()?;
    let _ = tx.send(WorkerMessage::SetProgress(100));

    let _ = tx.send(WorkerMessage::SetBackupStep(BackupStep::Cleanup));
    let _ = tx.send(WorkerMessage::SetStatus(tr!("正在清理本次备份会话文件...")));
    authenticated_task
        .cleanup_public_control_files()
        .context("delete the exact authenticated LRBK2 config and marker")?;
    let _ = tx.send(WorkerMessage::SetProgress(100));
    let _ = tx.send(WorkerMessage::SetBackupStep(BackupStep::Complete));
    let _ = tx.send(WorkerMessage::Completed);
    log::info!("========== LRBK2 PE 备份流程完成 ==========");
    thread::sleep(Duration::from_secs(3));
    reboot_pe();
    Ok(())
}

fn require_private_pe_payload_absent_from_source(source_root: &str) -> Result<()> {
    let path = PathBuf::from(source_root).join("LetRecovery_PE");
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("inspect private PE payload root {}", path.display())),
        Ok(_) => anyhow::bail!(
            "private PE payload root still exists before capture: {}",
            path.display()
        ),
    }
}

fn rebind_backup_paths(config: &BackupConfig) -> Result<ReboundBackupPaths> {
    let handoff = config.handoff.as_ref().context("missing LRBK2 handoff")?;
    let disk_numbers = lr_core::windows_storage::physical_disk_numbers()
        .context("enumerate complete physical disk inventory")?;
    let mut candidates = Vec::with_capacity(disk_numbers.len());
    for disk_number in disk_numbers {
        candidates.push((
            disk_number,
            lr_core::windows_storage::disk_layout_snapshot(disk_number)
                .with_context(|| format!("snapshot physical disk {disk_number}"))?,
        ));
    }
    let (source_disk, destination_disk) =
        lr_core::backup_handoff::bind_unique_backup_volumes(handoff, &candidates)?;
    let source_root = lr_core::windows_storage::volume_guid_path_for_partition(
        source_disk,
        handoff.source.partition_offset_bytes,
    )
    .context("resolve source volume GUID")?;
    let destination_root = lr_core::windows_storage::volume_guid_path_for_partition(
        destination_disk,
        handoff.destination.partition_offset_bytes,
    )
    .context("resolve destination volume GUID")?;
    require_not_encrypted(&source_root).context("source BitLocker gate")?;
    require_not_encrypted(&destination_root).context("destination BitLocker gate")?;
    Ok(ReboundBackupPaths {
        source_root,
        target: PathBuf::from(&destination_root).join(&handoff.destination_relative_path),
    })
}

fn require_not_encrypted(path: &str) -> Result<()> {
    use lr_core::fveapi::{FveError, FveProtectionStatus, FveVolumeStatus};

    let api = lr_core::fveapi::FveApi::instance()
        .map_err(|error| anyhow::anyhow!("load BitLocker status API: {error}"))?;
    match api.get_status_by_path(path) {
        Ok(info)
            if info.volume_status == FveVolumeStatus::FullyDecrypted
                && info.protection_status == FveProtectionStatus::Off =>
        {
            Ok(())
        }
        Err(FveError::NotEncrypted) | Err(FveError::NotBitLockerVolume) => Ok(()),
        Ok(info) => anyhow::bail!(
            "volume is not provably unencrypted (conversion={:?}, protection={:?}, lock={:?})",
            info.volume_status,
            info.protection_status,
            info.lock_status
        ),
        Err(error) => anyhow::bail!("cannot prove volume is unencrypted: {error}"),
    }
}

fn capture_and_publish(
    config: &BackupConfig,
    bound: &ReboundBackupPaths,
    progress: Option<Sender<DismProgress>>,
) -> Result<()> {
    let parent = bound
        .target
        .parent()
        .context("backup destination has no parent")?;
    let metadata =
        std::fs::symlink_metadata(parent).context("inspect backup destination parent")?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        anyhow::bail!("backup destination parent is not a plain directory");
    }
    let handoff = config.handoff.as_ref().context("missing LRBK2 handoff")?;
    use lr_core::backup_atomic_publish::ExistingPublishKind;
    let existing_kind = match handoff.output_policy {
        BackupOutputPolicy::Create => None,
        BackupOutputPolicy::Replace => Some(ExistingPublishKind::Replace),
        BackupOutputPolicy::Append => Some(ExistingPublishKind::Append),
    };
    let session_id = lr_core::handoff_auth::SessionId::parse(&handoff.session_id)
        .context("validate backup publication session")?;
    let target_name = bound
        .target
        .file_name()
        .and_then(|value| value.to_str())
        .context("backup destination filename is not valid Unicode")?
        .to_owned();
    let staged_extension = bound
        .target
        .extension()
        .and_then(|value| value.to_str())
        .context("backup destination omits its image extension")?
        .to_ascii_lowercase();
    if let Some(recovery_session) =
        lr_core::backup_atomic_publish::SecurePublishSession::open_if_present(parent, &session_id)
            .context("inspect interrupted PE backup publication")?
    {
        let outcome = match existing_kind {
            Some(kind) => lr_core::backup_atomic_publish::recover_existing(
                &recovery_session,
                kind,
                &target_name,
                &staged_extension,
            ),
            None => lr_core::backup_atomic_publish::recover_create(
                &recovery_session,
                &target_name,
                &staged_extension,
            ),
        }
        .context("recover interrupted PE backup publication")?;
        if outcome == lr_core::backup_atomic_publish::RecoveryOutcome::Committed {
            if let Err(error) = recovery_session.remove_empty() {
                log::warn!(
                    "committed PE backup was recovered but its empty private session remains: {error}"
                );
            }
            return Ok(());
        }
        recovery_session
            .remove_empty()
            .context("remove rolled-back PE backup publication session")?;
    }

    if existing_kind.is_none() && bound.target.exists() {
        anyhow::bail!("create-only destination appeared before capture");
    }

    let mut publish_session =
        lr_core::backup_atomic_publish::SecurePublishSession::create(parent, &session_id)
            .context("create secure PE backup publication session")?;
    let staged = publish_session
        .path()
        .join(format!("staged.{staged_extension}"));
    #[cfg(all(windows, feature = "ci-automation"))]
    ci_probe_backup_namespace(publish_session.path(), parent, "before-capture");
    let (existing_preparation, append_base_catalog) = match existing_kind {
        Some(kind) => {
            let prepared = (|| -> Result<_> {
                let mut preparation = publish_session
                    .prepare_existing_copy(&target_name, &staged_extension)
                    .context("prepare locked existing PE backup copy")?;
                let expected = handoff
                    .base_file
                    .as_ref()
                    .context("existing PE backup authorization omits its base identity")?;
                let observed = preparation.old_expectation();
                if observed.length != expected.length_bytes || observed.sha256 != expected.sha256 {
                    anyhow::bail!(
                        "locked existing PE backup does not match its authenticated base identity"
                    );
                }
                let base_catalog = match kind {
                    ExistingPublishKind::Append => Some(
                        publish_session
                            .inspect_prepared_staged(&mut preparation, |path| {
                                Dism::read_verified_backup_catalog(path)
                            })
                            .context("inspect locked PE append base catalog")?,
                    ),
                    ExistingPublishKind::Replace => {
                        publish_session
                            .discard_copied_staged_for_replace(&mut preparation)
                            .context("reset copied PE staging for replacement")?;
                        None
                    }
                };
                Ok((preparation, base_catalog))
            })();
            match prepared {
                Ok((preparation, base_catalog)) => (Some(preparation), base_catalog),
                Err(error) => {
                    let cleanup = lr_core::backup_atomic_publish::recover_existing(
                        &publish_session,
                        kind,
                        &target_name,
                        &staged_extension,
                    )
                    .and_then(|_| publish_session.remove_empty());
                    return match cleanup {
                        Ok(()) => Err(error),
                        Err(cleanup_error) => Err(anyhow::anyhow!(
                            "{error:#}; exact existing-session rollback also failed: {cleanup_error:#}"
                        )),
                    };
                }
            }
        }
        None => (None, None),
    };
    let staged_text = staged.to_string_lossy();
    let dism = Dism::new();
    let capture_result = match config.format {
        BackupFormat::Wim => dism.capture_image_staged(
            &staged_text,
            &bound.source_root,
            &config.name,
            &config.description,
            false,
            progress,
        ),
        BackupFormat::Esd => dism.capture_image_staged(
            &staged_text,
            &bound.source_root,
            &config.name,
            &config.description,
            true,
            progress,
        ),
        _ => anyhow::bail!("only transactional WIM/ESD backup is enabled"),
    };
    let prepared = (|| -> Result<lr_core::backup_atomic_publish::FileExpectation> {
        capture_result?;
        #[cfg(all(windows, feature = "ci-automation"))]
        {
            ci_probe_backup_namespace(publish_session.path(), parent, "after-capture");
            ci_probe_backup_hard_link(&staged, parent, "after-capture");
        }
        let completed_catalog = Dism::read_verified_backup_catalog(&staged)
            .context("verify completed PE backup staging")?;
        #[cfg(all(windows, feature = "ci-automation"))]
        ci_probe_backup_hard_link(&staged, parent, "after-catalog-verification");
        match append_base_catalog.as_ref() {
            Some(base) => lr_core::backup_image_catalog::verify_append_catalog(
                base,
                &completed_catalog,
                &config.name,
                &config.description,
            ),
            None => lr_core::backup_image_catalog::verify_fresh_catalog(
                &completed_catalog,
                &config.name,
                &config.description,
            ),
        }
        .context("verify completed PE backup catalog semantics")?;
        lr_core::scoped_temp_file::restrict_to_system_and_administrators(&staged)
            .context("seal completed PE backup staging custody")?;
        let completed = publish_session
            .inspect_staged_file(&staged_extension)
            .context("lock and hash completed PE backup staging")?;
        #[cfg(all(windows, feature = "ci-automation"))]
        ci_probe_backup_hard_link(&staged, parent, "after-staged-inspection");
        Ok(completed)
    })();
    let completed = match prepared {
        Ok(completed) => completed,
        Err(error) => {
            drop(existing_preparation);
            let cleanup = match existing_kind {
                Some(kind) => lr_core::backup_atomic_publish::recover_existing(
                    &publish_session,
                    kind,
                    &target_name,
                    &staged_extension,
                ),
                None => lr_core::backup_atomic_publish::recover_create(
                    &publish_session,
                    &target_name,
                    &staged_extension,
                ),
            }
            .and_then(|_| publish_session.remove_empty());
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(anyhow::anyhow!(
                    "{error:#}; exact unprepared-session rollback also failed: {cleanup_error:#}"
                )),
            };
        }
    };
    match (existing_kind, existing_preparation) {
        (Some(kind), Some(preparation)) => lr_core::backup_atomic_publish::publish_existing(
            &mut publish_session,
            kind,
            preparation,
            completed,
        ),
        (None, None) => lr_core::backup_atomic_publish::publish_create(
            &mut publish_session,
            &target_name,
            &staged_extension,
            completed,
        ),
        _ => Err(anyhow::anyhow!(
            "PE backup publication preparation does not match its authenticated policy"
        )),
    }
    .context("publish completed PE backup with durable CAS")?;
    if let Err(error) = publish_session.remove_empty() {
        log::warn!("PE backup CAS committed but its empty private session remains: {error}");
    }
    Ok(())
}

/// Disposable-VM-only phase probe for the WinPE sharing violation seen during backup publication.
///
/// `CreateHardLinkW` is one of the exact namespace operations that failed in the real VM.  A
/// short-lived link in the final target parent tells the CI log whether the conflicting handle was
/// introduced by capture, semantic verification, or the later CAS transaction.  Production builds
/// contain neither this probe nor its temporary public name.
#[cfg(all(windows, feature = "ci-automation"))]
fn ci_probe_backup_hard_link(source: &Path, target_parent: &Path, phase: &str) {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{CreateHardLinkW, DeleteFileW};

    let probe = target_parent.join(format!(
        ".lr-ci-hardlink-probe-{}-{phase}.tmp",
        std::process::id()
    ));
    let mut source_wide = source.as_os_str().encode_wide().collect::<Vec<_>>();
    let mut probe_wide = probe.as_os_str().encode_wide().collect::<Vec<_>>();
    if source_wide.contains(&0) || probe_wide.contains(&0) {
        log::error!("[CI BACKUP HANDLE PROBE] phase={phase} path contains embedded NUL");
        return;
    }
    source_wide.push(0);
    probe_wide.push(0);
    let hard_link_failed = match unsafe {
        CreateHardLinkW(
            PCWSTR(probe_wide.as_ptr()),
            PCWSTR(source_wide.as_ptr()),
            None,
        )
    } {
        Ok(()) => {
            match unsafe { DeleteFileW(PCWSTR(probe_wide.as_ptr())) } {
                Ok(()) => log::info!(
                    "[CI BACKUP HANDLE PROBE] phase={phase} hard-link create/delete succeeded"
                ),
                Err(error) => log::error!(
                    "[CI BACKUP HANDLE PROBE] phase={phase} hard-link created but cleanup failed: {error} (HRESULT 0x{:08x})",
                    error.code().0 as u32
                ),
            };
            false
        }
        Err(error) => {
            log::error!(
                "[CI BACKUP HANDLE PROBE] phase={phase} CreateHardLinkW failed: {error} (HRESULT 0x{:08x})",
                error.code().0 as u32
            );
            true
        }
    };

    if phase == "after-capture" && hard_link_failed {
        ci_log_restart_manager_holders(source, phase);
        ci_log_sysinternals_handle(source, phase);
        if let Some(session) = source.parent() {
            ci_log_sysinternals_handle(session, phase);
        }
        ci_log_sysinternals_handle(target_parent, phase);
    }
}

/// Determine whether WinPE rejects namespace changes for the whole authenticated destination or
/// only for the completed WIM object.  Both links use a newly created, closed, tiny ordinary file:
/// one stays inside the private session and the other crosses into the public target parent.  This
/// is CI-only evidence and never changes the production decision.
#[cfg(all(windows, feature = "ci-automation"))]
fn ci_probe_backup_namespace(session: &Path, target_parent: &Path, phase: &str) {
    use std::io::Write;
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{CreateHardLinkW, DeleteFileW};

    let stem = format!(".lr-ci-namespace-{}-{phase}", std::process::id());
    let source = session.join(format!("{stem}.source"));
    let private_link = session.join(format!("{stem}.private-link"));
    let public_link = target_parent.join(format!("{stem}.public-link"));

    let create = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&source)?;
        file.write_all(b"LetRecovery CI namespace probe\n")?;
        file.flush()?;
        file.sync_all()
    })();
    if let Err(error) = create {
        log::error!(
            "[CI BACKUP NAMESPACE PROBE] phase={phase} create closed source failed: {error} raw={:?}",
            error.raw_os_error()
        );
        return;
    }

    let link = |destination: &Path, role: &str| {
        let mut source_wide = source.as_os_str().encode_wide().collect::<Vec<_>>();
        let mut destination_wide = destination.as_os_str().encode_wide().collect::<Vec<_>>();
        source_wide.push(0);
        destination_wide.push(0);
        match unsafe {
            CreateHardLinkW(
                PCWSTR(destination_wide.as_ptr()),
                PCWSTR(source_wide.as_ptr()),
                None,
            )
        } {
            Ok(()) => {
                log::info!(
                    "[CI BACKUP NAMESPACE PROBE] phase={phase} role={role} hard-link succeeded"
                );
                if let Err(error) = unsafe { DeleteFileW(PCWSTR(destination_wide.as_ptr())) } {
                    log::error!(
                        "[CI BACKUP NAMESPACE PROBE] phase={phase} role={role} cleanup failed: {error} (HRESULT 0x{:08x})",
                        error.code().0 as u32
                    );
                }
            }
            Err(error) => log::error!(
                "[CI BACKUP NAMESPACE PROBE] phase={phase} role={role} hard-link failed: {error} (HRESULT 0x{:08x})",
                error.code().0 as u32
            ),
        }
    };
    link(&private_link, "private-session");
    link(&public_link, "public-parent");
    if let Err(error) = std::fs::remove_file(&source) {
        log::error!(
            "[CI BACKUP NAMESPACE PROBE] phase={phase} source cleanup failed: {error} raw={:?}",
            error.raw_os_error()
        );
    }
}

/// Use the Microsoft-signed Sysinternals Handle utility in the disposable CI image when stripped
/// WinPE does not expose Restart Manager.  The command only searches; no handle-closing switch is
/// ever passed. Its test-only binary is supplied and hash-verified by the disposable VM harness;
/// it never enters production WIMs.
#[cfg(all(windows, feature = "ci-automation"))]
fn ci_log_sysinternals_handle(path: &Path, phase: &str) {
    let tool = match std::env::current_exe().ok().and_then(|value| {
        value
            .parent()
            .map(|parent| parent.join("lr-ci-handle64.exe"))
    }) {
        Some(tool) if tool.is_file() => tool,
        Some(tool) => {
            log::warn!(
                "[CI BACKUP HANDLE OWNER] phase={phase} Sysinternals Handle is absent at {}",
                tool.display()
            );
            return;
        }
        None => {
            log::warn!(
                "[CI BACKUP HANDLE OWNER] phase={phase} cannot resolve the CI executable directory"
            );
            return;
        }
    };
    let fragment = match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => match parent.file_name() {
            Some(directory) => PathBuf::from(directory).join(name),
            None => PathBuf::from(name),
        },
        _ => path.to_path_buf(),
    };
    let output = match std::process::Command::new(&tool)
        .arg("-accepteula")
        .arg("-nobanner")
        .arg("-a")
        .arg(&fragment)
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            log::warn!(
                "[CI BACKUP HANDLE OWNER] phase={phase} failed to start Microsoft Sysinternals Handle: {error}"
            );
            return;
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    log::warn!(
        "[CI BACKUP HANDLE OWNER] phase={phase} Sysinternals Handle exit={:?} query={} stdout={:?} stderr={:?}",
        output.status.code(),
        fragment.display(),
        stdout.trim(),
        stderr.trim()
    );
}

/// Ask Windows Restart Manager which processes or services currently use the captured file.
///
/// The DLL is loaded dynamically because stripped WinPE images are allowed to omit Restart
/// Manager.  This diagnostic is CI-only, never shuts down an application, and never changes the
/// production decision: absence or failure merely leaves an explicit error code in the VM log.
#[cfg(all(windows, feature = "ci-automation"))]
fn ci_log_restart_manager_holders(path: &Path, phase: &str) {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::System::RestartManager::RM_PROCESS_INFO;

    type RmStartSession = unsafe extern "system" fn(*mut u32, u32, PWSTR) -> u32;
    type RmRegisterResources = unsafe extern "system" fn(
        u32,
        u32,
        *const PCWSTR,
        u32,
        *const c_void,
        u32,
        *const PCWSTR,
    ) -> u32;
    type RmGetList =
        unsafe extern "system" fn(u32, *mut u32, *mut u32, *mut RM_PROCESS_INFO, *mut u32) -> u32;
    type RmEndSession = unsafe extern "system" fn(u32) -> u32;

    const ERROR_SUCCESS: u32 = 0;
    const ERROR_MORE_DATA: u32 = 234;
    const MAX_REPORTED_HOLDERS: u32 = 1024;

    let library = match unsafe { libloading::Library::new("rstrtmgr.dll") } {
        Ok(library) => library,
        Err(error) => {
            log::warn!(
                "[CI BACKUP HANDLE OWNER] phase={phase} Restart Manager unavailable: {error}"
            );
            return;
        }
    };
    let start = match unsafe { library.get::<RmStartSession>(b"RmStartSession\0") } {
        Ok(symbol) => symbol,
        Err(error) => {
            log::warn!("[CI BACKUP HANDLE OWNER] phase={phase} RmStartSession missing: {error}");
            return;
        }
    };
    let register = match unsafe { library.get::<RmRegisterResources>(b"RmRegisterResources\0") } {
        Ok(symbol) => symbol,
        Err(error) => {
            log::warn!(
                "[CI BACKUP HANDLE OWNER] phase={phase} RmRegisterResources missing: {error}"
            );
            return;
        }
    };
    let get_list = match unsafe { library.get::<RmGetList>(b"RmGetList\0") } {
        Ok(symbol) => symbol,
        Err(error) => {
            log::warn!("[CI BACKUP HANDLE OWNER] phase={phase} RmGetList missing: {error}");
            return;
        }
    };
    let end = match unsafe { library.get::<RmEndSession>(b"RmEndSession\0") } {
        Ok(symbol) => symbol,
        Err(error) => {
            log::warn!("[CI BACKUP HANDLE OWNER] phase={phase} RmEndSession missing: {error}");
            return;
        }
    };

    let mut session = 0_u32;
    let mut session_key = [0_u16; 33];
    let start_code = unsafe { start(&mut session, 0, PWSTR(session_key.as_mut_ptr())) };
    if start_code != ERROR_SUCCESS {
        log::warn!(
            "[CI BACKUP HANDLE OWNER] phase={phase} RmStartSession failed: Win32={start_code}"
        );
        return;
    }

    (|| {
        let mut path_wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
        if path_wide.contains(&0) {
            log::warn!("[CI BACKUP HANDLE OWNER] phase={phase} path contains embedded NUL");
            return;
        }
        path_wide.push(0);
        let resources = [PCWSTR(path_wide.as_ptr())];
        let register_code = unsafe {
            register(
                session,
                resources.len() as u32,
                resources.as_ptr(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
            )
        };
        if register_code != ERROR_SUCCESS {
            log::warn!(
                "[CI BACKUP HANDLE OWNER] phase={phase} RmRegisterResources failed: Win32={register_code}"
            );
            return;
        }

        let mut needed = 0_u32;
        let mut count = 0_u32;
        let mut reboot_reasons = 0_u32;
        let first_code = unsafe {
            get_list(
                session,
                &mut needed,
                &mut count,
                std::ptr::null_mut(),
                &mut reboot_reasons,
            )
        };
        if first_code == ERROR_SUCCESS && needed == 0 {
            log::warn!(
                "[CI BACKUP HANDLE OWNER] phase={phase} Restart Manager reported no user-mode holder; reboot_reasons=0x{reboot_reasons:08x}"
            );
            return;
        }
        if first_code != ERROR_MORE_DATA || needed == 0 || needed > MAX_REPORTED_HOLDERS {
            log::warn!(
                "[CI BACKUP HANDLE OWNER] phase={phase} initial RmGetList failed or returned an invalid count: Win32={first_code}, needed={needed}, reboot_reasons=0x{reboot_reasons:08x}"
            );
            return;
        }

        for attempt in 1..=3 {
            let mut entries = vec![RM_PROCESS_INFO::default(); needed as usize];
            count = needed;
            let code = unsafe {
                get_list(
                    session,
                    &mut needed,
                    &mut count,
                    entries.as_mut_ptr(),
                    &mut reboot_reasons,
                )
            };
            if code == ERROR_MORE_DATA && needed <= MAX_REPORTED_HOLDERS {
                continue;
            }
            if code != ERROR_SUCCESS {
                log::warn!(
                    "[CI BACKUP HANDLE OWNER] phase={phase} RmGetList attempt={attempt} failed: Win32={code}, needed={needed}, capacity={count}, reboot_reasons=0x{reboot_reasons:08x}"
                );
                return;
            }
            entries.truncate(count as usize);
            log::warn!(
                "[CI BACKUP HANDLE OWNER] phase={phase} holders={} reboot_reasons=0x{reboot_reasons:08x}",
                entries.len()
            );
            for entry in entries {
                let app_end = entry
                    .strAppName
                    .iter()
                    .position(|value| *value == 0)
                    .unwrap_or(entry.strAppName.len());
                let service_end = entry
                    .strServiceShortName
                    .iter()
                    .position(|value| *value == 0)
                    .unwrap_or(entry.strServiceShortName.len());
                log::warn!(
                    "[CI BACKUP HANDLE OWNER] phase={phase} pid={} app={} service={} type={} status=0x{:08x} ts_session={} restartable={}",
                    entry.Process.dwProcessId,
                    String::from_utf16_lossy(&entry.strAppName[..app_end]),
                    String::from_utf16_lossy(&entry.strServiceShortName[..service_end]),
                    entry.ApplicationType.0,
                    entry.AppStatus,
                    entry.TSSessionId,
                    entry.bRestartable.as_bool()
                );
            }
            return;
        }
        log::warn!(
            "[CI BACKUP HANDLE OWNER] phase={phase} holder list changed on all bounded retries"
        );
    })();

    let end_code = unsafe { end(session) };
    if end_code != ERROR_SUCCESS {
        log::warn!("[CI BACKUP HANDLE OWNER] phase={phase} RmEndSession failed: Win32={end_code}");
    }
}

#[cfg(test)]
mod tests {
    use super::require_private_pe_payload_absent_from_source;

    #[test]
    fn backup_refuses_to_scan_a_source_that_still_contains_private_pe_payload() {
        let source = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "backup-pe-exclusion-test",
        )
        .unwrap();
        let root = source.path().to_string_lossy();
        assert!(require_private_pe_payload_absent_from_source(&root).is_ok());
        std::fs::create_dir(source.path().join("LetRecovery_PE")).unwrap();
        assert!(require_private_pe_payload_absent_from_source(&root).is_err());
    }
}
