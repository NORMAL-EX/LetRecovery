//! Production side-effect backend for the native direct-install executor.
//!
//! The phase ordering and fail-closed gates live in `native_install_executor`;
//! this module reuses the established image, XP, boot and advanced-option
//! implementations. Desktop-to-PE staging includes both regular image files
//! and session-isolated XP/2003 text-mode source directories.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

#[cfg(feature = "ci-automation")]
use anyhow::Context;
use lr_core::cached_artifact::CachedArtifactStatus;
use lr_core::data_staging::StagingPayloadBudget;
#[cfg(any(feature = "ci-automation", test))]
use lr_core::data_staging::STAGING_OPERATIONAL_HEADROOM_BYTES;
use lr_core::pca_compat::PreparedPcaCompatPackage;

use super::disk::{DiskManager, Partition, PartitionStyle};
use super::native_install_compat::{
    self, DefaultUnattendOptions, PartitionIdentity, UnattendArchitecture,
};
use super::native_install_controller::{InstallMode, PcaCompatConfig, StartInstallIntent};
use super::native_install_executor::{
    InstallBackendError, InstallCancellation, InstallExecutionBackend, InstallExecutionContext,
    InstallExecutionEvent, InstallExecutionPhase, InstallExecutionReporter,
};
use super::ui_state::{BootModeSelection, DriverAction};

const UNSUPPORTED_PENDING: &str = "unsupported_pending";
const PREINSTALLED_SOFTWARE_DOWNLOAD_ATTEMPTS: usize = 3;
#[cfg(feature = "ci-automation")]
const CI_EXISTING_TARGET_DRIVER_FIXTURE_BUDGET_BYTES: u64 = 0;
#[cfg(not(test))]
const PREINSTALLED_SOFTWARE_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
#[cfg(test)]
const PREINSTALLED_SOFTWARE_RETRY_DELAY: std::time::Duration = std::time::Duration::ZERO;

#[cfg(feature = "ci-automation")]
fn ci_existing_target_driver_scenario_run_id() -> Option<String> {
    let value = std::env::var("LETRECOVERY_CI_DRIVER_SCENARIO").ok()?;
    let run_id = value.strip_prefix("existing_target_candidate:")?;
    (run_id.len() == 32
        && run_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then(|| run_id.to_owned())
}

#[cfg(feature = "ci-automation")]
fn ci_stale_disabled_driver_scenario_run_id() -> Option<String> {
    let value = std::env::var("LETRECOVERY_CI_HANDOFF_SCENARIO").ok()?;
    let run_id = value.strip_prefix("stale_disabled_driver:")?;
    (run_id.len() == 32
        && run_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then(|| run_id.to_owned())
}

#[cfg(feature = "ci-automation")]
fn stage_ci_stale_disabled_driver_fixture(data_dir: &Path, run_id: &str) -> anyhow::Result<()> {
    let root = data_dir.join("drivers");
    std::fs::create_dir_all(&root).context("create CI stale disabled-driver directory")?;
    let fixture_root = root.join(format!("lrci-stale-tree-{run_id}"));
    std::fs::create_dir(&fixture_root).context("create CI run-scoped stale driver tree")?;
    let fixture = fixture_root.join(format!("lrci-stale-empty-{run_id}.bin"));
    std::fs::write(&fixture, []).context("write CI zero-byte stale driver artifact")?;
    let empty_root = fixture_root.join("legacy-empty-set");
    std::fs::create_dir(&empty_root).context("create CI stale empty-file set")?;
    for index in 0..256_u16 {
        std::fs::write(empty_root.join(format!("empty-{index:03}.bin")), [])
            .context("write CI stale empty-file set member")?;
    }
    let mut deep_root = fixture_root.join("legacy-depth");
    for level in 0..8_u8 {
        deep_root = deep_root.join(format!("level-{level:02}-abcdefghijklmnop"));
    }
    std::fs::create_dir_all(&deep_root).context("create CI stale deep path")?;
    let long_component = format!("{}.bin", "l".repeat(120));
    let long_path = deep_root.join(&long_component);
    std::fs::write(&long_path, []).context("write CI stale long-component file")?;
    let long_path_utf16_units = long_path.to_string_lossy().encode_utf16().count();
    if long_path_utf16_units <= 260 {
        anyhow::bail!("CI stale long-path fixture did not exceed 260 UTF-16 code units");
    }
    std::fs::write(
        fixture_root.join(format!("legacy-{run_id}.inf")),
        b"; ignored historical driver package\r\n",
    )
    .context("write CI stale INF")?;
    std::fs::write(
        fixture_root.join(format!("legacy-{run_id}.sys")),
        b"ignored historical driver payload\r\n",
    )
    .context("write CI stale SYS")?;
    let large = fixture_root.join(format!("legacy-large-{run_id}.bin"));
    let large_file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&large)
        .context("create CI stale large logical file")?;
    large_file
        .set_len(64 * 1024 * 1024)
        .context("size CI stale large logical file")?;
    std::fs::write(
        root.join(format!("unrelated-history-{run_id}.dat")),
        b"unrelated stale directory noise must be tolerated\r\n",
    )
    .context("write CI unrelated stale driver noise")?;
    #[cfg(windows)]
    {
        let cycle = root.join(format!("lrci-stale-cycle-{run_id}"));
        std::os::windows::fs::symlink_dir(&root, &cycle)
            .context("create CI stale driver reparse cycle")?;
    }
    #[cfg(not(windows))]
    anyhow::bail!("CI stale disabled-driver reparse fixture requires Windows");
    log::warn!(
        "[CI HANDOFF REGRESSION] staged extreme stale disabled-driver tree run_id={} root={} file={} length=0 empty_set=256 long_component={} long_path_utf16_units={} large_logical_bytes={} sibling_noise=true reparse_cycle=true",
        run_id,
        fixture_root.display(),
        fixture.display(),
        long_component.len(),
        long_path_utf16_units,
        64 * 1024 * 1024
    );
    Ok(())
}

#[cfg(feature = "ci-automation")]
fn write_ci_stale_driver_manifest_receipt(
    run_id: &str,
    manifest_artifact_count: usize,
    preserved_driver_artifact_count: usize,
    run_fixture_artifact_count_any_role: usize,
) -> anyhow::Result<()> {
    let path = PathBuf::from(
        std::env::var_os("LETRECOVERY_CI_STALE_DRIVER_RECEIPT")
            .ok_or_else(|| anyhow::anyhow!("CI stale-driver product receipt path is missing"))?,
    );
    write_ci_stale_driver_manifest_receipt_to(
        &path,
        run_id,
        manifest_artifact_count,
        preserved_driver_artifact_count,
        run_fixture_artifact_count_any_role,
    )
}

#[cfg(feature = "ci-automation")]
fn write_ci_stale_driver_manifest_receipt_to(
    path: &Path,
    run_id: &str,
    manifest_artifact_count: usize,
    preserved_driver_artifact_count: usize,
    run_fixture_artifact_count_any_role: usize,
) -> anyhow::Result<()> {
    if path.file_name().and_then(|name| name.to_str())
        != Some("stale-disabled-driver-product-receipt.json")
    {
        anyhow::bail!("CI stale-driver product receipt has an unexpected filename");
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("CI stale-driver product receipt has no parent"))?;
    let parent_metadata =
        std::fs::symlink_metadata(parent).context("inspect CI stale-driver receipt parent")?;
    if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
        anyhow::bail!("CI stale-driver product receipt parent is not an ordinary directory");
    }
    if preserved_driver_artifact_count != 0 || run_fixture_artifact_count_any_role != 0 {
        anyhow::bail!(
            "disabled-driver handoff unexpectedly contains stale artifacts: preserved={} run_fixture_any_role={}",
            preserved_driver_artifact_count,
            run_fixture_artifact_count_any_role
        );
    }
    let bytes = format!(
        concat!(
            "{{\r\n",
            "  \"schema_version\": 1,\r\n",
            "  \"run_id\": \"{}\",\r\n",
            "  \"driver_export_requested\": false,\r\n",
            "  \"driver_action_mode\": 0,\r\n",
            "  \"restore_drivers\": false,\r\n",
            "  \"drivers_directory_skipped\": true,\r\n",
            "  \"manifest_artifact_count\": {},\r\n",
            "  \"preserved_driver_artifact_count\": {},\r\n",
            "  \"run_fixture_artifact_count_any_role\": {}\r\n",
            "}}\r\n"
        ),
        run_id,
        manifest_artifact_count,
        preserved_driver_artifact_count,
        run_fixture_artifact_count_any_role
    );
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .context("create CI stale-driver product receipt")?;
    output
        .write_all(bytes.as_bytes())
        .context("write CI stale-driver product receipt")?;
    output
        .sync_all()
        .context("flush CI stale-driver product receipt")?;
    Ok(())
}

#[cfg(feature = "ci-automation")]
fn ci_inf_model_id_is_safe(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 1024
        && id.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'\\' | b'&' | b'_' | b'-' | b'{' | b'}' | b'.')
        })
}

#[cfg(feature = "ci-automation")]
fn select_ci_storage_fixture_device(
    devices: Vec<lr_core::driver::StoragePathDevice>,
) -> Option<(lr_core::driver::StoragePathDevice, String)> {
    let mut candidates = devices
        .into_iter()
        .filter(lr_core::driver::StoragePathDevice::is_storage_controller)
        .filter_map(|device| {
            let model_id = device
                .hardware_ids
                .iter()
                .chain(device.compatible_ids.iter())
                .find(|id| ci_inf_model_id_is_safe(id))?
                .clone();
            Some((device, model_id))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.0
            .instance_id
            .to_ascii_lowercase()
            .cmp(&right.0.instance_id.to_ascii_lowercase())
    });
    candidates.into_iter().next()
}

#[cfg(feature = "ci-automation")]
fn stage_ci_existing_target_driver_fixture(
    destination: &Path,
    run_id: &str,
) -> Result<(), anyhow::Error> {
    let devices = lr_core::driver::list_current_windows_storage_path_devices()
        .context("enumerate current Windows storage path for CI driver fixture")?;
    let (device, model_id) = select_ci_storage_fixture_device(devices).ok_or_else(|| {
        anyhow::anyhow!("no storage-path controller with an INF-safe PnP ID was found")
    })?;

    let storage_root = destination.join("lr-ci-rejected-storage");
    let optional_root = destination.join("lr-ci-rejected-optional");
    if storage_root.exists() || optional_root.exists() {
        anyhow::bail!("CI driver fixture destination already exists");
    }
    std::fs::create_dir(&storage_root).context("create CI storage-driver fixture directory")?;
    std::fs::create_dir(&optional_root).context("create CI optional-driver fixture directory")?;

    let storage_inf = format!(
        concat!(
            "; LetRecovery disposable-VM CI fixture. It is deliberately unsigned and must be rejected.\r\n",
            "; RunId={run_id}\r\n",
            "[Version]\r\n",
            "Signature=\"$WINDOWS NT$\"\r\n",
            "Class=SCSIAdapter\r\n",
            "ClassGuid={{4D36E97B-E325-11CE-BFC1-08002BE10318}}\r\n",
            "Provider=%Provider%\r\n",
            "DriverVer=08/28/2026,99.99.9999.0\r\n",
            "CatalogFile=lrci_storage.cat\r\n\r\n",
            "[Manufacturer]\r\n",
            "%Provider%=Models,NTamd64\r\n\r\n",
            "[Models.NTamd64]\r\n",
            "%DeviceDesc%=Install,{model_id}\r\n\r\n",
            "[Install.NT]\r\n",
            "CopyFiles=DriverCopyFiles\r\n\r\n",
            "[Install.NT.Services]\r\n",
            "AddService=lrci_storage,0x00000002,Service\r\n\r\n",
            "[Service]\r\n",
            "DisplayName=%DeviceDesc%\r\n",
            "ServiceType=1\r\n",
            "StartType=0\r\n",
            "ErrorControl=1\r\n",
            "ServiceBinary=%12%\\lrci_storage.sys\r\n\r\n",
            "[DestinationDirs]\r\n",
            "DriverCopyFiles=12\r\n\r\n",
            "[DriverCopyFiles]\r\n",
            "lrci_storage.sys\r\n\r\n",
            "[Strings]\r\n",
            "Provider=\"LetRecovery CI\"\r\n",
            "DeviceDesc=\"LetRecovery CI rejected storage candidate\"\r\n"
        ),
        run_id = run_id,
        model_id = model_id,
    );
    let optional_inf = format!(
        concat!(
        "; LetRecovery disposable-VM CI fixture. It is deliberately unsigned, boot-start and unrelated.\r\n",
        "; RunId={run_id}\r\n",
        "[Version]\r\n",
        "Signature=\"$WINDOWS NT$\"\r\n",
        // Use a boot-storage class so x64 DISM deterministically enforces signing. The synthetic
        // ROOT model is unrelated to every real controller and is deliberately absent from the
        // topology-authenticated requirement manifest, so its rejection remains optional.
        "Class=SCSIAdapter\r\n",
        "ClassGuid={{4D36E97B-E325-11CE-BFC1-08002BE10318}}\r\n",
        "Provider=%Provider%\r\n",
        "DriverVer=08/28/2026,99.99.9999.0\r\n",
        "CatalogFile=lrci_optional.cat\r\n\r\n",
        "[Manufacturer]\r\n",
        "%Provider%=Models,NTamd64\r\n\r\n",
        "[Models.NTamd64]\r\n",
        "%DeviceDesc%=Install,ROOT\\LETRECOVERY_CI_OPTIONAL_{run_id}\r\n\r\n",
        "[Install.NT]\r\n",
        "CopyFiles=DriverCopyFiles\r\n\r\n",
        "[Install.NT.Services]\r\n",
        "AddService=lrci_optional,0x00000002,Service\r\n\r\n",
        "[Service]\r\n",
        "DisplayName=%DeviceDesc%\r\n",
        "ServiceType=1\r\n",
        // Boot-start is intentional in this disposable fixture. Together with SCSIAdapter it makes
        // x64 DISM deterministically reject the unsigned package. Its synthetic ROOT model is
        // absent from the storage manifest, proving a rejected unrelated package remains non-fatal.
        "StartType=0\r\n",
        "ErrorControl=1\r\n",
        "ServiceBinary=%12%\\lrci_optional.sys\r\n\r\n",
        "[DestinationDirs]\r\n",
        "DriverCopyFiles=12\r\n\r\n",
        "[DriverCopyFiles]\r\n",
        "lrci_optional.sys\r\n\r\n",
        "[Strings]\r\n",
        "Provider=\"LetRecovery CI\"\r\n",
        "DeviceDesc=\"LetRecovery CI rejected optional package\"\r\n"
    ),
        run_id = run_id
    );
    std::fs::write(storage_root.join("lrci_storage.inf"), storage_inf)
        .context("write CI storage-driver INF")?;
    std::fs::write(
        storage_root.join("lrci_storage.sys"),
        b"LR-CI-NOT-A-SIGNED-DRIVER\r\n",
    )
    .context("write CI storage-driver payload")?;
    std::fs::write(optional_root.join("lrci_optional.inf"), optional_inf)
        .context("write CI optional-driver INF")?;
    std::fs::write(
        optional_root.join("lrci_optional.sys"),
        b"LR-CI-NOT-A-SIGNED-DRIVER\r\n",
    )
    .context("write CI optional-driver payload")?;

    let mut requirements = lr_core::driver::load_storage_driver_requirements(destination)
        .context("load exported storage-driver manifest before CI fixture")?;
    if !requirements.is_empty() {
        anyhow::bail!(
            "CI existing-target scenario requires an inbox-only baseline storage path, found {} pre-existing OEM requirements",
            requirements.len()
        );
    }
    requirements.push(lr_core::driver::StorageDriverRequirement {
        description: format!(
            "LetRecovery CI target-existing candidate: {}",
            device.description
        ),
        source_inf: "lrci_storage.inf".to_owned(),
        hardware_ids: device.hardware_ids,
        compatible_ids: device.compatible_ids,
        device_instance_id: Some(device.instance_id.clone()),
    });
    lr_core::driver::write_storage_driver_requirements(destination, &requirements)
        .context("publish storage-driver manifest with CI target-existing requirement")?;
    log::info!(
        "[CI DRIVER SCENARIO] staged run_id={} storage_inf=lrci_storage.inf optional_inf=lrci_optional.inf device={} model_id={}",
        run_id,
        device.instance_id,
        model_id
    );
    Ok(())
}

#[derive(Debug)]
struct DownloadedSoftwareBatch {
    total_bytes: u64,
    packages: Vec<lr_core::software_install::SelectedSoftwarePackage>,
    failures: Vec<String>,
}

fn capture_nonempty_auxiliary_tree(
    lock: lr_core::install_source_lock::LockedInstallTree,
) -> Result<
    Option<(
        lr_core::install_source_lock::LockedInstallTree,
        Vec<lr_core::install_source_lock::LockedSourceArtifactIdentity>,
    )>,
    String,
> {
    let artifacts = lock.artifact_identities()?;
    Ok((!artifacts.is_empty()).then_some((lock, artifacts)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StagedImageSetKind {
    Single,
    Swm,
    Ghost,
}

#[derive(Debug, PartialEq, Eq)]
struct StagedImageSet {
    kind: StagedImageSetKind,
    main_name: String,
    volumes: Vec<PathBuf>,
}

fn extension_is(path: &Path, expected: &str) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
}

fn strip_prefix_ascii_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .get(..prefix.len())
        .filter(|candidate| candidate.eq_ignore_ascii_case(prefix))?;
    value.get(prefix.len()..)
}

fn enumerate_staged_image_set(source: &Path) -> Result<StagedImageSet, String> {
    let source = std::fs::canonicalize(source)
        .map_err(|error| format!("canonicalize split image source: {error}"))?;
    let source = source.as_path();
    let source_name = source
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "source image has no Unicode file name".to_string())?;
    if extension_is(source, "ghs") {
        return Err("select the primary .gho volume instead of a .ghs span".to_string());
    }
    if !extension_is(source, "swm") && !extension_is(source, "gho") {
        return Ok(StagedImageSet {
            kind: StagedImageSetKind::Single,
            main_name: source_name.to_string(),
            volumes: vec![source.to_path_buf()],
        });
    }

    let parent = source
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "split image source has no parent directory".to_string())?;
    let stem = source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .ok_or_else(|| "split image source has no Unicode stem".to_string())?;

    if extension_is(source, "swm") {
        let trimmed = stem.trim_end_matches(|value: char| value.is_ascii_digit());
        if trimmed.len() != stem.len()
            && !trimmed.is_empty()
            && parent.join(format!("{trimmed}.swm")).is_file()
        {
            return Err("select the primary SWM volume (for example install.swm)".to_string());
        }
        let mut indexed = BTreeMap::new();
        for entry in std::fs::read_dir(parent).map_err(|error| error.to_string())? {
            let path = entry.map_err(|error| error.to_string())?.path();
            if !extension_is(&path, "swm") {
                continue;
            }
            let Some(candidate) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            let index = if candidate.eq_ignore_ascii_case(stem) {
                Some(1_usize)
            } else if let Some(suffix) =
                strip_prefix_ascii_case(candidate, stem).filter(|suffix| !suffix.is_empty())
            {
                suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit())
                    .then(|| suffix.parse::<usize>().ok())
                    .flatten()
                    .filter(|index| *index >= 2 && suffix == index.to_string())
            } else {
                None
            };
            if let Some(index) = index {
                if indexed.insert(index, path).is_some() {
                    return Err(format!("duplicate SWM volume index {index}"));
                }
            }
        }
        if indexed.get(&1) != Some(&source.to_path_buf()) {
            return Err("the selected SWM path is not the primary volume".to_string());
        }
        for expected in 1..=indexed.keys().next_back().copied().unwrap_or(0) {
            if !indexed.contains_key(&expected) {
                return Err(format!("missing SWM volume {stem}{expected}.swm"));
            }
        }
        return Ok(StagedImageSet {
            kind: StagedImageSetKind::Swm,
            main_name: source_name.to_string(),
            volumes: indexed.into_values().collect(),
        });
    }

    let mut indexed = BTreeMap::new();
    indexed.insert(0_usize, source.to_path_buf());
    for entry in std::fs::read_dir(parent).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        if !extension_is(&path, "ghs") {
            continue;
        }
        let Some(candidate) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        let index = if candidate.eq_ignore_ascii_case(stem) {
            Some(1_usize)
        } else if let Some(suffix) =
            strip_prefix_ascii_case(candidate, stem).filter(|suffix| !suffix.is_empty())
        {
            suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit())
                .then(|| suffix.parse::<usize>().ok())
                .flatten()
                .filter(|index| {
                    *index >= 1 && (suffix == index.to_string() || suffix == format!("{index:03}"))
                })
        } else {
            None
        };
        if let Some(index) = index {
            if indexed.insert(index, path).is_some() {
                return Err(format!("duplicate Ghost span index {index}"));
            }
        }
    }
    let last = indexed.keys().next_back().copied().unwrap_or(0);
    for expected in 0..=last {
        if !indexed.contains_key(&expected) {
            return Err(format!("missing Ghost span index {expected}"));
        }
    }
    Ok(StagedImageSet {
        kind: StagedImageSetKind::Ghost,
        main_name: source_name.to_string(),
        volumes: indexed.into_values().collect(),
    })
}

fn partition_matches_stable_identity(
    partition: &Partition,
    identity: super::native_install_executor::StableTargetIdentity,
) -> bool {
    if !identity.matches_components(
        partition.disk_number,
        partition.partition_number,
        partition.disk_size_bytes,
        partition.partition_offset_bytes,
        partition.partition_size_bytes,
    ) {
        return false;
    }
    let Some(letter) = partition.letter.chars().next() else {
        return false;
    };
    match lr_core::windows_storage::stable_volume_identity(letter) {
        Ok(actual) => identity.matches_stable_volume(actual),
        Err(error) => {
            log::error!(
                "[NATIVE INSTALL TARGET] cannot re-probe stable identity for {}: {error}",
                partition.letter
            );
            false
        }
    }
}

fn dependency_drive_letter(original: &Path, resolved: &Path) -> Option<char> {
    lr_core::windows_storage::path_drive_letter(resolved)
        .or_else(|| lr_core::windows_storage::path_drive_letter(original))
}

fn dependency_kind_may_lack_local_extent(kind: lr_core::windows_storage::DriveKind) -> bool {
    matches!(
        kind,
        lr_core::windows_storage::DriveKind::Optical | lr_core::windows_storage::DriveKind::RamDisk
    )
}

fn image_format_dependencies(intent: &StartInstallIntent) -> Vec<(&'static str, &Path)> {
    let mut dependencies = vec![("source image", Path::new(&intent.image_path))];
    if !intent.image_backing_path.trim().is_empty() {
        dependencies.push(("backing ISO image", Path::new(&intent.image_backing_path)));
    }
    dependencies
}

fn direct_phase_requires_target_revalidation(phase: InstallExecutionPhase) -> bool {
    matches!(
        phase,
        InstallExecutionPhase::FormatTarget
            | InstallExecutionPhase::ApplyXpTextModeSource
            | InstallExecutionPhase::ApplyGhostImage
            | InstallExecutionPhase::ApplyWimImage
            | InstallExecutionPhase::ProcessDrivers
            | InstallExecutionPhase::RepairBoot
            | InstallExecutionPhase::StageDirectPreinstalledSoftware
            | InstallExecutionPhase::ApplyAdvancedOptions
            | InstallExecutionPhase::FinishDirectInstall
    )
}

/// Content-Length is advisory display data only. The downloaded file is still accepted solely
/// from the actual bytes written and the ordinary-file readback; a missing or inaccurate header
/// can make the display less smooth but can never admit, reject or truncate a package.
fn software_download_progress(
    package_index: usize,
    package_count: usize,
    written: u64,
    content_length: Option<u64>,
) -> u8 {
    let count = package_count.max(1) as u128;
    let within = content_length
        .filter(|length| *length != 0)
        .map(|length| (u128::from(written).saturating_mul(100) / u128::from(length)).min(100))
        .unwrap_or(0);
    ((package_index as u128)
        .saturating_mul(100)
        .saturating_add(within)
        / count)
        .min(99) as u8
}

/// Resolve the boot mode for a direct install without collapsing an unknown
/// target layout into Legacy.  The injected probe keeps the decision testable
/// and lets production use the shared, documented firmware API boundary.
fn resolve_direct_install_uefi_mode_with<E, F>(
    requested: BootModeSelection,
    target_style: PartitionStyle,
    detect_current_firmware: F,
) -> Result<bool, E>
where
    F: FnOnce() -> Result<bool, E>,
{
    match requested {
        BootModeSelection::UEFI => Ok(true),
        BootModeSelection::Legacy => Ok(false),
        BootModeSelection::Auto => match target_style {
            PartitionStyle::GPT => Ok(true),
            PartitionStyle::MBR => Ok(false),
            PartitionStyle::Unknown => detect_current_firmware(),
        },
    }
}

/// NT5 is a property of the validated install intent, not something inferred from a missing
/// directory after image application. A stripped Vista+ image or GHO may omit `Windows\Boot`;
/// that inventory observation is diagnostic only. The real BCDBoot/boot-repair result remains
/// the authoritative compatibility boundary.
fn missing_modern_boot_assets_warning(
    validated_is_nt5: bool,
    modern_boot_assets_present: bool,
) -> bool {
    !validated_is_nt5 && !modern_boot_assets_present
}

/// Validate payloads for explicitly selected Direct advanced options and
/// report whether the offline advanced-options transaction is actually
/// needed. Disabled options must not load offline hives or create files.
fn validate_direct_advanced_request(
    options: &super::advanced_options::AdvancedOptions,
    validated_is_nt5: bool,
) -> Result<bool, &'static str> {
    let migrate_wifi = options.migrate_wifi && !options.wifi_profile_xml.trim().is_empty();
    if options.migrate_wifi && !migrate_wifi {
        log::warn!(
            "[ADVANCED WIFI] status=skipped reason=missing_session_profile; installation continues"
        );
    }
    if options.run_script_during_deploy && options.deploy_script_path.trim().is_empty() {
        return Err("deployment script execution was selected without a script path");
    }
    if options.run_script_first_login && options.first_login_script_path.trim().is_empty() {
        return Err("first-login script execution was selected without a script path");
    }
    if options.import_custom_drivers && options.custom_drivers_path.trim().is_empty() {
        return Err("custom driver import was selected without a source path");
    }
    if options.import_registry_file && options.registry_file_path.trim().is_empty() {
        return Err("registry import was selected without a .reg path");
    }
    if options.import_custom_files && options.custom_files_path.trim().is_empty() {
        return Err("custom file import was selected without a source path");
    }
    if options.custom_username && options.username.trim().is_empty() {
        return Err("custom user name was selected without a user name");
    }
    if options.custom_volume_label && options.volume_label.trim().is_empty() {
        return Err("custom volume label was selected without a volume label");
    }

    Ok(validated_is_nt5
        || options.remove_shortcut_arrow
        || options.restore_classic_context_menu
        || options.bypass_nro
        || options.disable_windows_update
        || options.disable_windows_defender
        || options.disable_uac
        || options.disable_device_encryption
        || options.remove_uwp_apps
        || migrate_wifi
        || options.run_script_during_deploy
        || options.run_script_first_login
        || options.import_custom_drivers
        || options.import_storage_controller_drivers
        || options.import_registry_file
        || options.import_custom_files
        || options.custom_username
        || options.custom_volume_label
        || options.win7_inject_usb3_driver
        || options.win7_inject_nvme_driver)
}

fn run_requested_direct_operation<E, F>(requested: bool, operation: F) -> Result<(), E>
where
    F: FnOnce() -> Result<(), E>,
{
    if requested {
        operation()?;
    }
    Ok(())
}

/// Distinguishes a verified manifest-only backup from a damaged or incomplete export. Automatic
/// preservation may safely no-op only when there are no INF packages and no captured boot-storage
/// requirements. Missing, malformed or contradictory manifests remain fail-closed.
fn automatic_driver_export_has_payload(driver_root: &Path) -> anyhow::Result<bool> {
    let inf_count = lr_core::driver::count_exported_driver_inf_files(driver_root)?;
    let requirements = lr_core::driver::load_storage_driver_requirements(driver_root)?;
    if inf_count != 0 {
        return Ok(true);
    }
    if !requirements.is_empty() {
        anyhow::bail!(
            "driver export contains no INF packages but declares {} boot-storage requirements",
            requirements.len()
        );
    }
    Ok(false)
}

fn should_include_preserved_driver_tree(
    export_requested: bool,
    driver_action_mode: u8,
    restore_drivers: bool,
) -> bool {
    export_requested && (driver_action_mode != 0 || restore_drivers)
}

/// Replaces the Driver Store preflight inventory with DISM's authoritative materialized result and
/// proves that the selected volume still has room for every unmaterialized payload plus the same
/// fixed 2 GiB operational headroom. A larger DISM tree is not itself an error; insufficient
/// current capacity is.
fn reconcile_exported_driver_budget(
    budget: &mut StagingPayloadBudget,
    actual_driver_bytes: u64,
    current_free_bytes: u64,
) -> Result<(u64, u64), InstallBackendError> {
    let planned_driver_bytes = budget.exported_driver_bytes;
    let mut reconciled = *budget;
    reconciled.exported_driver_bytes = actual_driver_bytes;
    let materialized_payload_bytes = actual_driver_bytes
        .checked_add(reconciled.pca_bytes)
        .ok_or_else(|| {
            InstallBackendError::new(
                "staging_materialized_size_overflow",
                "materialized PCA and driver bytes overflow u64",
            )
        })?;
    let remaining_required_bytes = reconciled
        .remaining_required_bytes_after(materialized_payload_bytes)
        .ok_or_else(|| {
            InstallBackendError::new(
                "staging_remaining_size_invalid",
                "reconciled staging budget cannot represent the remaining payload",
            )
        })?;
    if current_free_bytes < remaining_required_bytes {
        return Err(InstallBackendError::new(
            "staging_capacity_after_driver_export",
            format!(
                "DISM materialized {actual_driver_bytes} driver bytes (preflight {planned_driver_bytes}); the selected volume now has {current_free_bytes} free bytes but {remaining_required_bytes} bytes are still required for the remaining payload and fixed 2 GiB operational headroom"
            ),
        ));
    }
    *budget = reconciled;
    Ok((planned_driver_bytes, remaining_required_bytes))
}

#[cfg(feature = "ci-automation")]
fn write_ci_driver_budget_receipt(
    run_id: &str,
    planned_driver_bytes: u64,
    actual_driver_bytes: u64,
    current_free_bytes: u64,
    remaining_required_bytes: u64,
    reconciled_budget: StagingPayloadBudget,
) -> anyhow::Result<()> {
    let path = PathBuf::from(
        std::env::var_os("LETRECOVERY_CI_DRIVER_BUDGET_RECEIPT")
            .ok_or_else(|| anyhow::anyhow!("CI driver budget receipt path is missing"))?,
    );
    write_ci_driver_budget_receipt_to(
        &path,
        run_id,
        planned_driver_bytes,
        actual_driver_bytes,
        current_free_bytes,
        remaining_required_bytes,
        reconciled_budget,
    )
}

#[cfg(feature = "ci-automation")]
fn write_ci_driver_budget_receipt_to(
    path: &Path,
    run_id: &str,
    planned_driver_bytes: u64,
    actual_driver_bytes: u64,
    current_free_bytes: u64,
    remaining_required_bytes: u64,
    reconciled_budget: StagingPayloadBudget,
) -> anyhow::Result<()> {
    if path.file_name().and_then(|name| name.to_str()) != Some("driver-budget-reconciliation.json")
    {
        anyhow::bail!("CI driver budget receipt has an unexpected filename");
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("CI driver budget receipt has no parent"))?;
    let parent_metadata =
        std::fs::symlink_metadata(parent).context("inspect CI driver budget receipt parent")?;
    if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
        anyhow::bail!("CI driver budget receipt parent is not an ordinary directory");
    }
    let bytes = format!(
        concat!(
            "{{\r\n",
            "  \"schema_version\": 2,\r\n",
            "  \"run_id\": \"{}\",\r\n",
            "  \"planned_driver_bytes\": {},\r\n",
            "  \"actual_driver_bytes\": {},\r\n",
            "  \"current_free_bytes\": {},\r\n",
            "  \"remaining_required_bytes\": {},\r\n",
            "  \"image_bytes\": {},\r\n",
            "  \"materialized_pca_bytes\": {},\r\n",
            "  \"user_driver_bytes\": {},\r\n",
            "  \"uefiseven_bytes\": {},\r\n",
            "  \"preinstalled_software_bytes\": {},\r\n",
            "  \"operational_headroom_bytes\": {}\r\n",
            "}}\r\n"
        ),
        run_id,
        planned_driver_bytes,
        actual_driver_bytes,
        current_free_bytes,
        remaining_required_bytes,
        reconciled_budget.image_bytes,
        reconciled_budget.pca_bytes,
        reconciled_budget.user_driver_bytes,
        reconciled_budget.uefiseven_bytes,
        reconciled_budget.preinstalled_software_bytes,
        STAGING_OPERATIONAL_HEADROOM_BYTES
    );
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .context("create CI driver budget receipt")?;
    output
        .write_all(bytes.as_bytes())
        .context("write CI driver budget receipt")?;
    output
        .sync_all()
        .context("flush CI driver budget receipt")?;
    Ok(())
}

/// Stateful backend used for one executor run.
///
/// Target identity is resolved again from `disk:partition` before every write
/// branch that depends on a drive letter.  This prevents a DiskPart script or
/// WinPE drive-letter reassignment from redirecting the install.
pub struct ProductionInstallBackend {
    target: String,
    target_style: PartitionStyle,
    partitions: Vec<Partition>,
    pca_package: Option<PreparedPcaCompatPackage>,
    driver_backup: PathBuf,
    pe_path: Option<PathBuf>,
    pe_snapshot: Option<super::pe::LocalPeSnapshot>,
    pe_supports_source_image_verification_receipt: bool,
    pe_display_name: Option<String>,
    data_partition: Option<String>,
    staging_payload_budget: Option<StagingPayloadBudget>,
    prepared_software_directory: Option<lr_core::scoped_temp_file::ScopedTempDir>,
    prepared_software_bytes: u64,
    /// Exact subset whose installers were downloaded and read back successfully before staging.
    /// Package-host failures are optional-component warnings, including when every package fails;
    /// later staging and authenticated PE configuration must never reference a missing installer.
    prepared_software_packages: Option<Vec<lr_core::software_install::SelectedSoftwarePackage>>,
    /// Exact subset staged into an already-applied Direct target. In WinPE, transient network
    /// failures after the destructive boundary are isolated per package, so the first-logon plan
    /// must contain only installers whose non-empty files were read back successfully.
    direct_staged_software: Option<Vec<lr_core::software_install::SelectedSoftwarePackage>>,
    staged_image_name: Option<String>,
    /// Byte identity issued only by this run's uncached full wimlib verification plus copy and
    /// staged readback. The corresponding deny-write/delete guard is held in `pe_source_lock`.
    staged_source_image_receipt: Option<VerifiedStagedImageReceipt>,
    staged_xp_source_arch: Option<String>,
    bitlocker_decryption_volumes: Vec<char>,
    install_config_transaction: Option<super::install_config::InstallConfigTransaction>,
    pe_boot_transaction: Option<super::pe::PeBootTransaction>,
    handoff_auth_key: Option<lr_core::handoff_auth::SessionAuthKey>,
    direct_source_lock: Option<lr_core::install_source_lock::LockedInstallSourceSet>,
    direct_xp_source_lock: Option<lr_core::install_source_lock::LockedInstallTree>,
    pe_source_lock: Option<lr_core::install_source_lock::LockedInstallSourceSet>,
    pe_xp_source_lock: Option<lr_core::install_source_lock::LockedInstallTree>,
    pe_auxiliary_tree_locks: Vec<lr_core::install_source_lock::LockedInstallTree>,
    pe_auxiliary_file_locks: Vec<lr_core::install_source_lock::LockedPlainArtifact>,
    staging_transaction: Option<super::disk::PreparedStagingTransaction>,
    dual_boot_transaction: Option<super::disk::PreparedDualBootTransaction>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct VerifiedStagedImageReceipt {
    identity: lr_core::install_source_lock::LockedSourceArtifactIdentity,
}

/// Add the optional persistent data/staging volume to a UI-built dual-boot request. The normal
/// Windows transaction performs one Shrink for the combined Windows + data minimum and replaces
/// these requested offsets with the provider's actual readback before publishing the handoff.
fn dual_boot_plan_with_staging(
    plan: &lr_core::custom_install::DualBootPlan,
    staging_required_bytes: u64,
) -> Result<lr_core::custom_install::DualBootPlan, InstallBackendError> {
    if staging_required_bytes == 0 {
        return Err(InstallBackendError::new(
            "dual_boot_staging_empty",
            "dual-boot staging requirement must be non-zero",
        ));
    }
    if plan.data_offset_bytes.is_some() || plan.data_length_bytes != 0 {
        if plan.data_length_bytes < staging_required_bytes {
            return Err(InstallBackendError::new(
                "dual_boot_staging_too_small",
                format!(
                    "planned dual-boot data volume is {} bytes but staging requires at least {staging_required_bytes} bytes",
                    plan.data_length_bytes
                ),
            ));
        }
        return Ok(plan.clone());
    }
    let combined = plan
        .target_length_bytes
        .checked_add(staging_required_bytes)
        .ok_or_else(|| {
            InstallBackendError::new(
                "dual_boot_combined_size_overflow",
                "dual-boot Windows and staging sizes overflow u64",
            )
        })?;
    if combined >= plan.source_length_before_bytes {
        return Err(InstallBackendError::new(
            "dual_boot_source_too_small",
            "dual-boot Windows and staging volumes would consume the complete source volume",
        ));
    }
    let source_length_after_bytes = plan.source_length_before_bytes - combined;
    let target_offset_bytes = plan
        .source_offset_bytes
        .checked_add(source_length_after_bytes)
        .ok_or_else(|| {
            InstallBackendError::new(
                "dual_boot_target_offset_overflow",
                "dual-boot target offset overflows u64",
            )
        })?;
    let data_offset_bytes = target_offset_bytes
        .checked_add(plan.target_length_bytes)
        .ok_or_else(|| {
            InstallBackendError::new(
                "dual_boot_data_offset_overflow",
                "dual-boot data offset overflows u64",
            )
        })?;
    let prepared = lr_core::custom_install::DualBootPlan {
        source_length_after_bytes,
        target_offset_bytes,
        data_offset_bytes: Some(data_offset_bytes),
        data_length_bytes: staging_required_bytes,
        ..plan.clone()
    };
    lr_core::custom_install::validate_dual_boot_plan(&prepared).map_err(|error| {
        InstallBackendError::new(
            "invalid_dual_boot_staging_plan",
            format!("invalid combined dual-boot layout: {error}"),
        )
    })?;
    Ok(prepared)
}

impl Drop for ProductionInstallBackend {
    fn drop(&mut self) {
        if let Some(transaction) = self.pe_boot_transaction.take() {
            if let Err(error) = transaction.rollback() {
                log::error!("failed to roll back an uncommitted PE BCD handoff: {error}");
            }
        }
        // A dual-boot data volume can be the staging carrier. Release every locked artifact before
        // rolling back its config directory or deleting that task-created volume; otherwise our
        // own deny-delete handles would turn a recoverable failure into an incomplete rollback.
        self.pe_source_lock.take();
        self.pe_xp_source_lock.take();
        self.pe_auxiliary_tree_locks.clear();
        self.pe_auxiliary_file_locks.clear();
        if let Some(transaction) = self.install_config_transaction.take() {
            if let Err(error) = transaction.rollback() {
                log::error!("failed to roll back an uncommitted PE install handoff: {error}");
            }
        }
        if let Some(transaction) = self.staging_transaction.take() {
            drop(transaction);
        }
        if let Some(transaction) = self.dual_boot_transaction.take() {
            if let Err(error) = transaction.rollback() {
                log::error!("failed to roll back an uncommitted dual-boot preparation: {error:#}");
            }
        }
    }
}

impl ProductionInstallBackend {
    pub fn new(intent: &StartInstallIntent) -> Self {
        Self {
            target: intent.target_partition.clone(),
            target_style: PartitionStyle::Unknown,
            partitions: Vec::new(),
            pca_package: None,
            driver_backup: std::env::temp_dir().join("LetRecovery_DriverBackup"),
            pe_path: None,
            pe_snapshot: None,
            pe_supports_source_image_verification_receipt: false,
            pe_display_name: None,
            data_partition: None,
            staging_payload_budget: None,
            prepared_software_directory: None,
            prepared_software_bytes: 0,
            prepared_software_packages: None,
            direct_staged_software: None,
            staged_image_name: None,
            staged_source_image_receipt: None,
            staged_xp_source_arch: None,
            bitlocker_decryption_volumes: Vec::new(),
            install_config_transaction: None,
            pe_boot_transaction: None,
            handoff_auth_key: None,
            direct_source_lock: None,
            direct_xp_source_lock: None,
            pe_source_lock: None,
            pe_xp_source_lock: None,
            pe_auxiliary_tree_locks: Vec::new(),
            pe_auxiliary_file_locks: Vec::new(),
            staging_transaction: None,
            dual_boot_transaction: None,
        }
    }

    fn error(code: &'static str, error: impl std::fmt::Display) -> InstallBackendError {
        InstallBackendError::new(code, error.to_string())
    }

    fn supports_fused_verify_copy(path: &Path) -> bool {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("wim") || extension.eq_ignore_ascii_case("esd")
            })
    }

    fn supports_authenticated_verify_receipt(path: &Path) -> bool {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(extension.to_ascii_lowercase().as_str(), "wim" | "esd")
            })
    }

    fn verification_result_can_issue_receipt(result: &super::image_verify::VerifyResult) -> bool {
        result.status == super::image_verify::VerifyStatus::Valid
            && result.full_wimlib_verification_performed
    }

    fn lock_published_verified_image(
        &mut self,
        destination: &Path,
        expected_length: u64,
        copied_sha256: &str,
        staged_readback_sha256: &str,
    ) -> Result<(), InstallBackendError> {
        // This is deliberately the first operation after atomic publication. The guard denies
        // write/delete sharing and remains owned by the backend through manifest publication.
        let lock = lr_core::install_source_lock::LockedInstallSourceSet::acquire_pinned_original(
            destination,
        )
        .map_err(|error| Self::error("lock_published_verified_image", error))?;
        let identities = lock
            .artifact_identities()
            .map_err(|error| Self::error("identify_published_verified_image", error))?;
        let copied_sha256 =
            lr_core::install_handoff::decode_hex_array::<32>(copied_sha256, "copied image SHA-256")
                .map_err(|error| Self::error("decode_copied_image_sha256", error))?;
        let staged_readback_sha256 = lr_core::install_handoff::decode_hex_array::<32>(
            staged_readback_sha256,
            "staged image readback SHA-256",
        )
        .map_err(|error| Self::error("decode_staged_image_sha256", error))?;
        let identity = identities.as_slice();
        if identity.len() != 1
            || identity[0].length_bytes != expected_length
            || identity[0].sha256 != copied_sha256
            || identity[0].sha256 != staged_readback_sha256
        {
            return Err(InstallBackendError::new(
                "published_verified_image_identity_mismatch",
                "the locked published image differs from copy-stream or staged-readback evidence",
            ));
        }
        self.staged_source_image_receipt = Some(VerifiedStagedImageReceipt {
            identity: identity[0].clone(),
        });
        self.pe_source_lock = Some(lock);
        Ok(())
    }

    fn receipt_matches_manifest_identities(
        receipt: Option<&VerifiedStagedImageReceipt>,
        staged_path: &Path,
        config: &super::install_config::InstallConfig,
        identities: &[lr_core::install_source_lock::LockedSourceArtifactIdentity],
    ) -> Result<bool, InstallBackendError> {
        let Some(receipt) = receipt else {
            return Ok(false);
        };
        if !Self::supports_authenticated_verify_receipt(staged_path)
            || config.is_gho
            || config.is_xp
            || config.is_xp_i386
        {
            return Err(InstallBackendError::new(
                "invalid_source_verification_receipt_combination",
                "a source verification receipt is valid only for one-file WIM/ESD handoff",
            ));
        }
        if identities != std::slice::from_ref(&receipt.identity) {
            return Err(InstallBackendError::new(
                "source_verification_receipt_identity_mismatch",
                "manifest image identities do not exactly match the verification receipt",
            ));
        }
        Ok(true)
    }

    fn source_verification_is_deferred_to_copy(intent: &StartInstallIntent) -> bool {
        intent.mode == InstallMode::ViaPe
            && Self::supports_fused_verify_copy(Path::new(&intent.image_path))
    }

    #[cfg(windows)]
    fn open_locked_source(path: &Path) -> std::io::Result<File> {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::{FILE_FLAG_SEQUENTIAL_SCAN, FILE_SHARE_READ};

        let mut options = OpenOptions::new();
        options
            .read(true)
            .share_mode(FILE_SHARE_READ.0)
            .custom_flags(FILE_FLAG_SEQUENTIAL_SCAN.0);
        options.open(path)
    }

    fn lock_direct_source_set(
        &mut self,
        intent: &StartInstallIntent,
    ) -> Result<(), InstallBackendError> {
        self.direct_source_lock = None;
        self.direct_xp_source_lock = None;
        if intent.mode != InstallMode::Direct {
            return Ok(());
        }
        if intent.options.is_xp_i386 {
            self.direct_xp_source_lock = Some(
                lr_core::install_source_lock::LockedInstallTree::acquire(Path::new(
                    &intent.image_path,
                ))
                .map_err(|error| Self::error("lock_direct_xp_source_tree", error))?,
            );
            return Ok(());
        }
        self.direct_source_lock = Some(
            lr_core::install_source_lock::LockedInstallSourceSet::acquire(Path::new(
                &intent.image_path,
            ))
            .map_err(|error| Self::error("lock_direct_source_set", error))?,
        );
        Ok(())
    }

    fn verify_direct_source_set_unchanged(
        &self,
        intent: &StartInstallIntent,
    ) -> Result<(), InstallBackendError> {
        if intent.mode != InstallMode::Direct {
            return Ok(());
        }
        if intent.options.is_xp_i386 {
            return self
                .direct_xp_source_lock
                .as_ref()
                .ok_or_else(|| {
                    InstallBackendError::new(
                        "direct_xp_source_lock_missing",
                        "XP apply reached without its verification tree manifest",
                    )
                })?
                .verify_unchanged()
                .map_err(|error| Self::error("direct_xp_source_changed_after_verify", error));
        }
        self.direct_source_lock
            .as_ref()
            .ok_or_else(|| {
                InstallBackendError::new(
                    "direct_source_lock_missing",
                    "direct image apply reached without the verification source lock",
                )
            })?
            .verify_unchanged()
            .map_err(|error| Self::error("direct_source_changed_after_verify", error))
    }

    fn immutable_image_path(&self, intent: &StartInstallIntent) -> String {
        self.direct_source_lock
            .as_ref()
            .map(|locked| locked.selected_path().to_string_lossy().into_owned())
            .unwrap_or_else(|| intent.image_path.clone())
    }

    fn immutable_xp_source_path(&self, intent: &StartInstallIntent) -> String {
        self.direct_xp_source_lock
            .as_ref()
            .map(|locked| locked.selected_path().to_string_lossy().into_owned())
            .unwrap_or_else(|| intent.image_path.clone())
    }

    const fn supports_direct_phase(phase: InstallExecutionPhase) -> bool {
        matches!(
            phase,
            InstallExecutionPhase::InspectBitLocker
                | InstallExecutionPhase::AwaitBitLockerDecryption
                | InstallExecutionPhase::VerifyPcaBeforeDiskWrite
                | InstallExecutionPhase::ResolveStableTarget
                | InstallExecutionPhase::RunDiskpartScripts
                | InstallExecutionPhase::ResolveTargetAfterDiskpart
                | InstallExecutionPhase::VerifySourceImage
                | InstallExecutionPhase::PreparePreinstalledSoftware
                | InstallExecutionPhase::FormatTarget
                | InstallExecutionPhase::ExportHostDrivers
                | InstallExecutionPhase::ApplyXpTextModeSource
                | InstallExecutionPhase::ApplyGhostImage
                | InstallExecutionPhase::ApplyWimImage
                | InstallExecutionPhase::ProcessDrivers
                | InstallExecutionPhase::RepairBoot
                | InstallExecutionPhase::StageDirectPreinstalledSoftware
                | InstallExecutionPhase::ApplyAdvancedOptions
                | InstallExecutionPhase::FinishDirectInstall
        )
    }

    const fn supports_via_pe_phase(phase: InstallExecutionPhase) -> bool {
        matches!(
            phase,
            InstallExecutionPhase::InspectBitLocker
                | InstallExecutionPhase::AwaitBitLockerDecryption
                | InstallExecutionPhase::VerifyPcaBeforeDiskWrite
                | InstallExecutionPhase::VerifyPeEnvironment
                | InstallExecutionPhase::InstallPeBootEntry
                | InstallExecutionPhase::SelectDataPartition
                | InstallExecutionPhase::PersistPcaCompatibilityPackage
                | InstallExecutionPhase::ExportDriversToPeData
                | InstallExecutionPhase::VerifySourceImage
                | InstallExecutionPhase::CopySourceImage
                | InstallExecutionPhase::PreparePreinstalledSoftware
                | InstallExecutionPhase::StagePreinstalledSoftware
                | InstallExecutionPhase::StageUefiSeven
                | InstallExecutionPhase::StageUserDrivers
                | InstallExecutionPhase::WritePeInstallConfig
                | InstallExecutionPhase::ReadyToRebootIntoPe
        )
    }

    fn data_partition(&self) -> Result<&str, InstallBackendError> {
        self.data_partition.as_deref().ok_or_else(|| {
            InstallBackendError::new(
                "data_partition_missing",
                "PE data partition is not selected",
            )
        })
    }

    #[cfg(not(feature = "non-elevated-tests"))]
    fn begin_bitlocker_fallback_decryption(&mut self) -> Result<(), InstallBackendError> {
        self.partitions = DiskManager::get_partitions()
            .map_err(|error| Self::error("bitlocker_inventory", error))?;
        self.bitlocker_decryption_volumes.clear();
        let manager = super::bitlocker::BitLockerManager::new();
        for partition in &self.partitions {
            let Some(letter) = partition.letter.chars().next() else {
                continue;
            };
            let drive = format!("{}:", letter.to_ascii_uppercase());
            match manager.get_status(letter) {
                super::bitlocker::VolumeStatus::NotEncrypted => {}
                super::bitlocker::VolumeStatus::Decrypting => {
                    self.bitlocker_decryption_volumes
                        .push(letter.to_ascii_uppercase());
                }
                super::bitlocker::VolumeStatus::EncryptedUnlocked => {
                    let result = manager.decrypt(&drive);
                    if !result.success {
                        return Err(InstallBackendError::new(
                            "bitlocker_decrypt_start",
                            format!("{drive}: {}", result.message),
                        ));
                    }
                    self.bitlocker_decryption_volumes
                        .push(letter.to_ascii_uppercase());
                }
                super::bitlocker::VolumeStatus::EncryptedLocked => {
                    log::warn!(
                        "[NATIVE INSTALL] skipping locked non-target BitLocker volume {drive} during fallback decryption"
                    );
                }
                super::bitlocker::VolumeStatus::Encrypting => {
                    return Err(InstallBackendError::new(
                        "bitlocker_encrypting",
                        format!("{drive} is still encrypting"),
                    ));
                }
                super::bitlocker::VolumeStatus::Unknown => {
                    return Err(InstallBackendError::new(
                        "bitlocker_status_unknown",
                        format!("cannot determine BitLocker status for {drive}"),
                    ));
                }
            }
        }
        Ok(())
    }

    #[cfg(not(feature = "non-elevated-tests"))]
    fn await_bitlocker_fallback_decryption(
        &mut self,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<(), InstallBackendError> {
        let manager = super::bitlocker::BitLockerManager::new();
        loop {
            if cancellation.is_cancelled() {
                return Err(InstallBackendError::new(
                    "cancelled",
                    "installation cancelled while waiting for BitLocker decryption",
                ));
            }
            let mut all_decrypted = true;
            let mut highest_encrypted = 0.0_f32;
            for &letter in &self.bitlocker_decryption_volumes {
                let drive = format!("{}:", letter.to_ascii_uppercase());
                let (status, encrypted_percentage) = manager.get_status_with_percentage(letter);
                match status {
                    super::bitlocker::VolumeStatus::NotEncrypted => {}
                    super::bitlocker::VolumeStatus::Decrypting
                    | super::bitlocker::VolumeStatus::EncryptedUnlocked => {
                        all_decrypted = false;
                        highest_encrypted = highest_encrypted.max(encrypted_percentage);
                    }
                    super::bitlocker::VolumeStatus::EncryptedLocked => {
                        return Err(InstallBackendError::new(
                            "bitlocker_relocked",
                            format!("{drive} became locked while decrypting"),
                        ));
                    }
                    super::bitlocker::VolumeStatus::Encrypting => {
                        return Err(InstallBackendError::new(
                            "bitlocker_encrypting",
                            format!("{drive} is encrypting instead of decrypting"),
                        ));
                    }
                    super::bitlocker::VolumeStatus::Unknown => {
                        return Err(InstallBackendError::new(
                            "bitlocker_status_unknown",
                            format!("cannot determine BitLocker status for {drive}"),
                        ));
                    }
                }
            }
            if all_decrypted {
                return Ok(());
            }
            reporter.report(InstallExecutionEvent::Progress {
                phase: InstallExecutionPhase::AwaitBitLockerDecryption,
                percentage: (100.0 - highest_encrypted).clamp(0.0, 100.0) as u8,
                detail: crate::tr!("正在等待 BitLocker 完全解密..."),
            });
            for _ in 0..8 {
                if cancellation.is_cancelled() {
                    return Err(InstallBackendError::new(
                        "cancelled",
                        "installation cancelled while waiting for BitLocker decryption",
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        }
    }

    #[cfg(not(feature = "non-elevated-tests"))]
    fn inspect_bitlocker_fresh(
        &mut self,
        intent: &StartInstallIntent,
        context: &InstallExecutionContext,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<(), InstallBackendError> {
        let partitions = DiskManager::get_partitions()
            .map_err(|error| Self::error("bitlocker_inventory", error))?;
        let target = if let Some(identity) = context.stable_target {
            partitions
                .iter()
                .find(|partition| partition_matches_stable_identity(partition, identity))
        } else {
            partitions.iter().find(|partition| {
                partition
                    .letter
                    .eq_ignore_ascii_case(&intent.target_partition)
            })
        }
        .ok_or_else(|| {
            InstallBackendError::new(
                "bitlocker_target_missing",
                "the verified installation target is no longer present",
            )
        })?;
        let letter = target.letter.chars().next().ok_or_else(|| {
            InstallBackendError::new(
                "bitlocker_target_letter_missing",
                "the verified installation target has no drive letter",
            )
        })?;
        let drive = format!("{}:", letter.to_ascii_uppercase());
        let manager = super::bitlocker::BitLockerManager::new();
        match manager.get_status(letter) {
            super::bitlocker::VolumeStatus::NotEncrypted => Ok(()),
            super::bitlocker::VolumeStatus::EncryptedLocked => Err(InstallBackendError::new(
                "bitlocker_target_locked",
                format!("{drive} is locked; unlock it before installation"),
            )),
            super::bitlocker::VolumeStatus::Unknown => Err(InstallBackendError::new(
                "bitlocker_status_unknown",
                format!("cannot determine BitLocker status for {drive}"),
            )),
            super::bitlocker::VolumeStatus::Encrypting => Err(InstallBackendError::new(
                "bitlocker_target_encrypting",
                format!("{drive} is currently encrypting"),
            )),
            super::bitlocker::VolumeStatus::Decrypting => {
                self.begin_bitlocker_fallback_decryption()?;
                self.await_bitlocker_fallback_decryption(reporter, cancellation)
            }
            super::bitlocker::VolumeStatus::EncryptedUnlocked => {
                if manager.get_recovery_key(&drive).is_ok() {
                    Ok(())
                } else {
                    self.begin_bitlocker_fallback_decryption()?;
                    self.await_bitlocker_fallback_decryption(reporter, cancellation)
                }
            }
        }
    }

    fn data_dir(&self) -> Result<String, InstallBackendError> {
        Ok(super::install_config::ConfigFileManager::get_data_dir(
            self.data_partition()?,
        ))
    }

    fn download_software_packages(
        destination: &Path,
        packages: &[lr_core::software_install::SelectedSoftwarePackage],
        progress_phase: InstallExecutionPhase,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<DownloadedSoftwareBatch, InstallBackendError> {
        lr_core::software_install::validate_selected_packages(packages)
            .map_err(|error| Self::error("validate_preinstalled_software", error))?;
        std::fs::create_dir_all(destination)
            .map_err(|error| Self::error("create_preinstalled_software_directory", error))?;
        let metadata = std::fs::symlink_metadata(destination)
            .map_err(|error| Self::error("inspect_preinstalled_software_directory", error))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(InstallBackendError::new(
                "unsafe_preinstalled_software_directory",
                format!("{} is not an ordinary directory", destination.display()),
            ));
        }

        let client = reqwest::blocking::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .timeout(std::time::Duration::from_secs(60 * 60))
            .build()
            .map_err(|error| Self::error("build_preinstalled_software_client", error))?;
        let mut total = 0_u64;
        let mut downloaded = Vec::with_capacity(packages.len());
        let mut failures = Vec::new();
        for (index, package) in packages.iter().enumerate() {
            if cancellation.is_cancelled() {
                return Err(InstallBackendError::new(
                    "cancelled",
                    "preinstalled software download was cancelled",
                ));
            }
            let percentage = u8::try_from(index.saturating_mul(100) / packages.len().max(1))
                .unwrap_or(99)
                .min(99);
            reporter.report(InstallExecutionEvent::Progress {
                phase: progress_phase,
                percentage,
                detail: format!("正在下载 {}", package.name),
            });
            let path = destination.join(&package.filename);
            let mut final_result = None;
            for attempt in 1..=PREINSTALLED_SOFTWARE_DOWNLOAD_ATTEMPTS {
                if cancellation.is_cancelled() {
                    return Err(InstallBackendError::new(
                        "cancelled",
                        "preinstalled software download was cancelled",
                    ));
                }
                let result = (|| {
                    let mut output = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)
                        .map_err(|error| Self::error("create_preinstalled_software_file", error))?;
                    let mut response = client
                        .get(&package.download_url)
                        .send()
                        .and_then(reqwest::blocking::Response::error_for_status)
                        .map_err(|error| Self::error("download_preinstalled_software", error))?;
                    let content_length = response.content_length();
                    let mut buffer = vec![0_u8; 1024 * 1024];
                    let mut written = 0_u64;
                    let mut last_percentage = percentage;
                    loop {
                        if cancellation.is_cancelled() {
                            return Err(InstallBackendError::new(
                                "cancelled",
                                "preinstalled software download was cancelled",
                            ));
                        }
                        let count = response.read(&mut buffer).map_err(|error| {
                            Self::error("read_preinstalled_software_response", error)
                        })?;
                        if count == 0 {
                            break;
                        }
                        output.write_all(&buffer[..count]).map_err(|error| {
                            Self::error("write_preinstalled_software_file", error)
                        })?;
                        written = written.checked_add(count as u64).ok_or_else(|| {
                            InstallBackendError::new(
                                "preinstalled_software_size_overflow",
                                "downloaded software sizes overflow u64",
                            )
                        })?;
                        let percentage = software_download_progress(
                            index,
                            packages.len(),
                            written,
                            content_length,
                        );
                        if percentage > last_percentage {
                            last_percentage = percentage;
                            reporter.report(InstallExecutionEvent::Progress {
                                phase: progress_phase,
                                percentage,
                                detail: format!("正在下载 {}", package.name),
                            });
                        }
                    }
                    if written == 0 {
                        return Err(InstallBackendError::new(
                            "empty_preinstalled_software_download",
                            format!("{} returned an empty installer", package.name),
                        ));
                    }
                    output
                        .flush()
                        .and_then(|_| output.sync_all())
                        .map_err(|error| Self::error("flush_preinstalled_software_file", error))?;
                    let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
                        Self::error("inspect_preinstalled_software_file", error)
                    })?;
                    if !metadata.is_file()
                        || metadata.file_type().is_symlink()
                        || metadata.len() != written
                    {
                        return Err(InstallBackendError::new(
                            "preinstalled_software_readback_mismatch",
                            format!("downloaded installer readback failed for {}", package.name),
                        ));
                    }
                    Ok(written)
                })();
                match result {
                    Ok(written) => {
                        final_result = Some(Ok(written));
                        break;
                    }
                    Err(error) => {
                        if let Err(cleanup) = std::fs::remove_file(&path) {
                            if cleanup.kind() != std::io::ErrorKind::NotFound {
                                log::warn!(
                                    "[PREINSTALL SOFTWARE] partial download cleanup failed for {}: {cleanup}",
                                    path.display()
                                );
                            }
                        }
                        if error.code == "cancelled" {
                            return Err(error);
                        }
                        if attempt < PREINSTALLED_SOFTWARE_DOWNLOAD_ATTEMPTS {
                            log::warn!(
                                "[PREINSTALL SOFTWARE] package={} attempt={}/{} status=retry code={} detail={}",
                                package.id,
                                attempt,
                                PREINSTALLED_SOFTWARE_DOWNLOAD_ATTEMPTS,
                                error.code,
                                error.detail
                            );
                            reporter.report(InstallExecutionEvent::Progress {
                                phase: progress_phase,
                                percentage,
                                detail: format!(
                                    "下载 {} 失败，正在重试 ({}/{})",
                                    package.name,
                                    attempt + 1,
                                    PREINSTALLED_SOFTWARE_DOWNLOAD_ATTEMPTS
                                ),
                            });
                            std::thread::sleep(PREINSTALLED_SOFTWARE_RETRY_DELAY);
                        } else {
                            final_result = Some(Err(error));
                        }
                    }
                }
            }
            let written = match final_result.expect("bounded download loop always completes") {
                Ok(written) => written,
                Err(error) => {
                    let failure = format!("{}:{}:{}", package.id, error.code, error.detail);
                    log::warn!(
                        "[PREINSTALL SOFTWARE] package={} status=skipped_after_retries attempts={} code={} detail={}",
                        package.id,
                        PREINSTALLED_SOFTWARE_DOWNLOAD_ATTEMPTS,
                        error.code,
                        error.detail
                    );
                    failures.push(failure);
                    let completed_percentage =
                        u8::try_from((index + 1).saturating_mul(100) / packages.len().max(1))
                            .unwrap_or(99)
                            .min(99);
                    reporter.report(InstallExecutionEvent::Progress {
                        phase: progress_phase,
                        percentage: completed_percentage,
                        detail: format!("已跳过下载失败的软件 {}", package.name),
                    });
                    continue;
                }
            };
            total = total.checked_add(written).ok_or_else(|| {
                InstallBackendError::new(
                    "preinstalled_software_size_overflow",
                    "downloaded software sizes overflow u64",
                )
            })?;
            downloaded.push(package.clone());
            let completed_percentage =
                u8::try_from((index + 1).saturating_mul(100) / packages.len().max(1))
                    .unwrap_or(99)
                    .min(99);
            reporter.report(InstallExecutionEvent::Progress {
                phase: progress_phase,
                percentage: completed_percentage,
                detail: format!("已下载 {}", package.name),
            });
        }
        reporter.report(InstallExecutionEvent::Progress {
            phase: progress_phase,
            percentage: 100,
            detail: format!(
                "预装软件下载完成：成功 {} 个，失败 {} 个",
                downloaded.len(),
                failures.len()
            ),
        });
        Ok(DownloadedSoftwareBatch {
            total_bytes: total,
            packages: downloaded,
            failures,
        })
    }

    fn prepare_preinstalled_software(
        &mut self,
        intent: &StartInstallIntent,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<(), InstallBackendError> {
        let packages = &intent.options.advanced_options.preinstalled_software;
        lr_core::software_install::validate_selected_packages(packages)
            .map_err(|error| Self::error("validate_preinstalled_software", error))?;
        if !intent.options.unattended_install {
            return Err(InstallBackendError::new(
                "preinstalled_software_requires_unattended",
                "preinstalled software requires LetRecovery unattended installation",
            ));
        }
        if !intent.options.custom_unattend_path.trim().is_empty() {
            return Err(InstallBackendError::new(
                "preinstalled_software_conflicts_with_custom_unattend",
                "preinstalled software requires LetRecovery's built-in unattended file",
            ));
        }
        self.prepared_software_directory = None;
        self.prepared_software_bytes = 0;
        self.prepared_software_packages = None;
        let temp = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "LetRecovery-preinstalled-software",
        )
        .map_err(|error| Self::error("create_preinstalled_software_temp", error))?;
        let batch = Self::download_software_packages(
            temp.path(),
            packages,
            InstallExecutionPhase::PreparePreinstalledSoftware,
            reporter,
            cancellation,
        )?;
        if !batch.failures.is_empty() {
            // Preinstalled applications are optional to the core Windows result. A package host
            // may be temporarily unavailable (or every selected host may be unavailable), so a
            // bounded download failure must not stop the system installation. Persist only the
            // exact successful subset; structural/path/authentication errors still return above.
            log::warn!(
                "[PREINSTALL SOFTWARE] phase=pre_destructive status=warning selected={} downloaded={} skipped={} detail={}",
                packages.len(),
                batch.packages.len(),
                batch.failures.len(),
                batch.failures.join("; ")
            );
        }
        self.prepared_software_bytes = batch.total_bytes;
        self.prepared_software_packages = Some(batch.packages);
        self.prepared_software_directory = Some(temp);
        Ok(())
    }

    fn copy_prepared_software_to(
        &self,
        destination: &Path,
        packages: &[lr_core::software_install::SelectedSoftwarePackage],
    ) -> Result<u64, InstallBackendError> {
        let prepared = self.prepared_software_directory.as_ref().ok_or_else(|| {
            InstallBackendError::new(
                "preinstalled_software_not_prepared",
                "preinstalled software download directory is missing",
            )
        })?;
        if destination.exists() {
            std::fs::remove_dir_all(destination)
                .map_err(|error| Self::error("clear_preinstalled_software_destination", error))?;
        }
        // A completely unavailable optional package set is a valid empty result. Do not create an
        // empty PE artifact directory: the authenticated handoff should describe only installers
        // that were actually downloaded and copied.
        if packages.is_empty() {
            return Ok(0);
        }
        std::fs::create_dir_all(destination)
            .map_err(|error| Self::error("create_preinstalled_software_destination", error))?;
        let mut total = 0_u64;
        for package in packages {
            let source = prepared.path().join(&package.filename);
            let target = destination.join(&package.filename);
            let source_metadata = std::fs::symlink_metadata(&source)
                .map_err(|error| Self::error("inspect_prepared_software", error))?;
            if !source_metadata.is_file()
                || source_metadata.file_type().is_symlink()
                || source_metadata.len() == 0
            {
                return Err(InstallBackendError::new(
                    "unsafe_prepared_software_file",
                    format!("{} is not an ordinary non-empty file", source.display()),
                ));
            }
            let mut input = File::open(&source)
                .map_err(|error| Self::error("open_prepared_software", error))?;
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|error| Self::error("create_staged_software", error))?;
            let copied = std::io::copy(&mut input, &mut output)
                .map_err(|error| Self::error("copy_preinstalled_software", error))?;
            output
                .flush()
                .and_then(|_| output.sync_all())
                .map_err(|error| Self::error("flush_staged_software", error))?;
            let target_metadata = std::fs::symlink_metadata(&target)
                .map_err(|error| Self::error("inspect_staged_software", error))?;
            if copied != source_metadata.len()
                || !target_metadata.is_file()
                || target_metadata.file_type().is_symlink()
                || target_metadata.len() != source_metadata.len()
            {
                return Err(InstallBackendError::new(
                    "staged_software_readback_mismatch",
                    format!("staged installer readback failed for {}", package.name),
                ));
            }
            total = total.checked_add(copied).ok_or_else(|| {
                InstallBackendError::new(
                    "preinstalled_software_size_overflow",
                    "staged software sizes overflow u64",
                )
            })?;
        }
        Ok(total)
    }

    fn stage_preinstalled_software_for_pe(
        &mut self,
        _intent: &StartInstallIntent,
    ) -> Result<(), InstallBackendError> {
        let destination = Path::new(&self.data_dir()?).join("preinstalled_software");
        let packages = self.prepared_software_packages.as_deref().ok_or_else(|| {
            InstallBackendError::new(
                "preinstalled_software_subset_missing",
                "preinstalled software download result is missing",
            )
        })?;
        let actual = self.copy_prepared_software_to(&destination, packages)?;
        if actual != self.prepared_software_bytes {
            return Err(InstallBackendError::new(
                "staged_software_budget_mismatch",
                format!(
                    "downloaded {} bytes but staged {actual} bytes",
                    self.prepared_software_bytes
                ),
            ));
        }
        self.prepared_software_directory = None;
        Ok(())
    }

    fn stage_preinstalled_software_for_direct(
        &mut self,
        intent: &StartInstallIntent,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<(), InstallBackendError> {
        let packages = &intent.options.advanced_options.preinstalled_software;
        if packages.is_empty() {
            self.direct_staged_software = Some(Vec::new());
            return Ok(());
        }
        let scripts = Path::new(&self.target).join("LetRecovery_Scripts");
        let destination = scripts.join(lr_core::software_install::STAGING_DIRECTORY_NAME);
        if self.prepared_software_directory.is_some() {
            let prepared_packages =
                self.prepared_software_packages.as_deref().ok_or_else(|| {
                    InstallBackendError::new(
                        "preinstalled_software_subset_missing",
                        "preinstalled software download result is missing",
                    )
                })?;
            let actual = self.copy_prepared_software_to(&destination, prepared_packages)?;
            if actual != self.prepared_software_bytes {
                return Err(InstallBackendError::new(
                    "direct_staged_software_size_mismatch",
                    format!(
                        "downloaded {} bytes but copied {actual} bytes",
                        self.prepared_software_bytes
                    ),
                ));
            }
            self.prepared_software_directory = None;
            self.direct_staged_software = Some(prepared_packages.to_vec());
        } else {
            if destination.exists() {
                std::fs::remove_dir_all(&destination).map_err(|error| {
                    Self::error("clear_direct_preinstalled_software_destination", error)
                })?;
            }
            let batch = Self::download_software_packages(
                &destination,
                packages,
                InstallExecutionPhase::StageDirectPreinstalledSoftware,
                reporter,
                cancellation,
            )?;
            if !batch.failures.is_empty() {
                // The Windows image and boot files already exist. Transient package-host failures
                // are isolated after bounded retries and must not convert a bootable installation
                // into a total failure. The exact successful subset is embedded into first logon.
                log::warn!(
                    "[PREINSTALL SOFTWARE] phase=post_destructive status=warning selected={} staged={} skipped={} detail={}",
                    packages.len(),
                    batch.packages.len(),
                    batch.failures.len(),
                    batch.failures.join("; ")
                );
            }
            self.prepared_software_bytes = batch.total_bytes;
            if batch.packages.is_empty() {
                match std::fs::remove_dir(&destination) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => log::warn!(
                        "[PREINSTALL SOFTWARE] optional empty staging directory cleanup failed for {}: {error}",
                        destination.display()
                    ),
                }
            }
            self.direct_staged_software = Some(batch.packages);
        }
        Ok(())
    }

    fn require_cached_pe(
        status: CachedArtifactStatus,
        filename: &str,
    ) -> Result<PathBuf, InstallBackendError> {
        match status {
            CachedArtifactStatus::Ready { path, .. } => Ok(path),
            CachedArtifactStatus::Missing => Err(InstallBackendError::new(
                "pe_download_required",
                format!(
                    "PE file {filename} is missing; schedule the existing verified download workflow first"
                ),
            )),
        }
    }

    fn verify_pe_environment(
        &mut self,
        intent: &StartInstallIntent,
    ) -> Result<(), InstallBackendError> {
        let pe_index = intent.pe_index.ok_or_else(|| {
            InstallBackendError::new("missing_pe_index", "automatic PE index is missing")
        })?;
        let entries = crate::download::config::PeCache::load_strict()
            .map_err(|error| Self::error("pe_catalog_invalid", error))?
            .ok_or_else(|| {
                InstallBackendError::new(
                    "pe_catalog_missing",
                    "the cached PE catalog is unavailable; refresh the online PE list",
                )
            })?;
        let pe = entries.get(pe_index).ok_or_else(|| {
            InstallBackendError::new("invalid_pe_index", "automatic PE index is no longer valid")
        })?;
        let status = super::pe::PeManager::check_cached_pe(
            &pe.filename,
            pe.sha256.as_deref(),
            pe.md5.as_deref(),
        )
        .map_err(|error| Self::error("pe_cache_rejected", error))?;
        let verified_path = Self::require_cached_pe(status, &pe.filename)?;
        let snapshot = super::pe::snapshot_local_pe(&verified_path, &pe.filename)
            .map_err(|error| Self::error("snapshot_local_pe", error))?;
        let snapshot_path = snapshot.path.clone();
        self.pe_supports_source_image_verification_receipt =
            super::pe::supports_source_image_verification_receipt(&snapshot_path);
        log::info!(
            "[PE] authenticated source verification receipt capability: {}",
            self.pe_supports_source_image_verification_receipt
        );
        self.pe_path = Some(snapshot_path);
        self.pe_snapshot = Some(snapshot);
        self.pe_display_name = Some(pe.display_name.clone());
        Ok(())
    }

    fn install_pe_boot_entry(&mut self) -> Result<(), InstallBackendError> {
        let path = self.pe_path.as_ref().ok_or_else(|| {
            InstallBackendError::new("pe_not_verified", "PE cache verification has not completed")
        })?;
        let display_name = self.pe_display_name.as_deref().ok_or_else(|| {
            InstallBackendError::new("pe_name_missing", "PE display name is unavailable")
        })?;
        let session_id = self
            .install_config_transaction
            .as_ref()
            .map(|transaction| transaction.session_id())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                InstallBackendError::new(
                    "pe_session_missing",
                    "PE boot journal cannot be created before the install handoff session",
                )
            })?
            .to_owned();
        let config_bytes = self
            .install_config_transaction
            .as_mut()
            .ok_or_else(|| {
                InstallBackendError::new(
                    "pe_config_transaction_missing",
                    "authenticated PE install config is unavailable",
                )
            })?
            .take_boot_config_bytes()
            .map_err(|error| Self::error("take_authenticated_install_config", error))?;
        let manifest_bytes = self
            .install_config_transaction
            .as_mut()
            .ok_or_else(|| {
                InstallBackendError::new(
                    "pe_config_transaction_missing",
                    "authenticated PE install manifest is unavailable",
                )
            })?
            .take_boot_manifest_bytes()
            .map_err(|error| Self::error("take_authenticated_install_manifest", error))?;
        let auth_key = self.handoff_auth_key.take().ok_or_else(|| {
            InstallBackendError::new(
                "pe_handoff_auth_missing",
                "authenticated PE handoff key is unavailable",
            )
        })?;
        let private_wifi_profile = self
            .install_config_transaction
            .as_mut()
            .and_then(|transaction| transaction.take_private_wifi_profile());
        let protected_administrator_secret = self
            .install_config_transaction
            .as_mut()
            .and_then(|transaction| transaction.take_protected_administrator_secret());
        let payload = super::pe::HandoffBootPayload::new(
            auth_key,
            lr_core::handoff_auth::HandoffPurpose::Install,
            &session_id,
            config_bytes,
            manifest_bytes,
            None,
            private_wifi_profile,
        )
        .map_err(|error| Self::error("build_authenticated_install_boot_payload", error))?;
        let payload = match protected_administrator_secret {
            Some(secret) => payload
                .with_administrator_secret(secret)
                .map_err(|error| Self::error("bind_protected_administrator_boot_secret", error))?,
            None => payload,
        };
        let result = super::pe::PeManager::new()
            .boot_to_pe_for_install(&path.to_string_lossy(), display_name, payload)
            .map_err(|error| Self::error("install_pe_boot_entry", error));
        match result {
            Ok(transaction) => {
                self.pe_boot_transaction = Some(transaction);
                Ok(())
            }
            Err(boot_error) => {
                let rollback = self
                    .install_config_transaction
                    .take()
                    .map(|transaction| transaction.rollback());
                match rollback {
                    None | Some(Ok(())) => Err(boot_error),
                    Some(Err(rollback_error)) => Err(InstallBackendError::new(
                        "install_pe_boot_entry_and_config_rollback",
                        format!(
                            "{}: {}; additionally failed to roll back PE handoff: {rollback_error}",
                            boot_error.code, boot_error.detail
                        ),
                    )),
                }
            }
        }
    }

    fn commit_pe_handoff(&mut self) -> Result<(), InstallBackendError> {
        let log_handoff = self.install_config_transaction.as_ref().map(|transaction| {
            (
                transaction.data_directory().to_path_buf(),
                transaction.session_id().to_owned(),
            )
        });
        let boot = self.pe_boot_transaction.take().ok_or_else(|| {
            InstallBackendError::new(
                "pe_boot_transaction_missing",
                "PE boot transaction is unavailable at commit",
            )
        })?;
        let config = self.install_config_transaction.take().ok_or_else(|| {
            InstallBackendError::new(
                "pe_config_transaction_missing",
                "PE install config transaction is unavailable at commit",
            )
        })?;
        if let Err(error) = boot.commit() {
            let rollback = config.rollback();
            return match rollback {
                Ok(()) => Err(Self::error("commit_pe_boot_transaction", error)),
                Err(rollback) => Err(InstallBackendError::new(
                    "commit_pe_boot_and_config_rollback",
                    format!("{error}; additionally failed to roll back PE config: {rollback}"),
                )),
            };
        }
        config.commit();
        if let Some(transaction) = self.staging_transaction.take() {
            transaction.commit();
        }
        if let Some(transaction) = self.dual_boot_transaction.take() {
            transaction.commit();
        }
        // The diagnostic directory is intentionally created only after both handoff transactions
        // have committed. A BCD failure or a dropped config transaction therefore cannot leave an
        // unowned logs/<session> directory that defeats rollback cleanup.
        if let Some((data_directory, session_id)) = log_handoff {
            log::info!(
                "[INSTALL LOG] PE 配置与启动事务已提交，开始截取重启前正常端日志: session={session_id}"
            );
            let staged = crate::utils::logger::LogManager::flush_barrier().and_then(|snapshot| {
                lr_core::install_log_handoff::stage_desktop_log_from_file(
                    snapshot.file(),
                    &data_directory,
                    &session_id,
                    env!("BUILD_VERSION"),
                )
            });
            match staged {
                Ok(manifest) => log::info!(
                    "[INSTALL LOG] 正常端日志已暂存到 PE 数据分区: session={}, bytes={}, sha256={}",
                    manifest.session_id,
                    manifest.bytes,
                    manifest.sha256
                ),
                Err(error) => log::warn!(
                    "[INSTALL LOG] 正常端日志暂存失败；安装交接继续，不弹出提示: {error:#}"
                ),
            }
        }
        Ok(())
    }

    fn source_payload_bytes(intent: &StartInstallIntent) -> Result<u64, InstallBackendError> {
        if intent.options.is_xp_i386 {
            let source = Path::new(&intent.image_path);
            let arch = lr_core::xp_i386::validate_i386_source(source)
                .map_err(|error| Self::error("invalid_xp_source", error))?;
            let mut size = Self::directory_size_checked(source)?;
            if arch == "AMD64" {
                let sibling = source
                    .parent()
                    .map(|parent| parent.join("I386"))
                    .filter(|path| path.is_dir());
                if let Some(sibling) = sibling {
                    size = size
                        .checked_add(Self::directory_size_checked(&sibling)?)
                        .ok_or_else(|| {
                            InstallBackendError::new(
                                "xp_source_size_overflow",
                                "XP source tree sizes overflow u64",
                            )
                        })?;
                }
            }
            Ok(size)
        } else {
            let image_set = enumerate_staged_image_set(Path::new(&intent.image_path))
                .map_err(|error| InstallBackendError::new("enumerate_source_image_set", error))?;
            image_set.volumes.iter().try_fold(0_u64, |total, path| {
                let metadata = std::fs::symlink_metadata(path)
                    .map_err(|error| Self::error("inspect_source_image_volume", error))?;
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(InstallBackendError::new(
                        "source_image_volume_not_regular",
                        format!(
                            "source image volume is not a regular file: {}",
                            path.display()
                        ),
                    ));
                }
                total.checked_add(metadata.len()).ok_or_else(|| {
                    InstallBackendError::new(
                        "source_image_set_size_overflow",
                        "source image volume sizes overflow u64",
                    )
                })
            })
        }
    }

    fn planned_user_driver_bytes() -> Result<u64, InstallBackendError> {
        let mut total = 0_u64;
        for version in ["win7", "win8", "win10", "win11"] {
            let source = crate::utils::path::get_drivers_dir().join(version);
            let has_inf = match Self::directory_has_inf_checked(&source) {
                Ok(value) => value,
                Err(error) => {
                    log::warn!(
                        "[DATA CAPACITY] optional user-driver directory {version} is unavailable and will be skipped: {}",
                        error.detail
                    );
                    continue;
                }
            };
            if !has_inf {
                continue;
            }
            let size = match Self::directory_size_checked(&source) {
                Ok(value) => value,
                Err(error) => {
                    log::warn!(
                        "[DATA CAPACITY] optional user-driver directory {version} cannot be measured and will be skipped: {}",
                        error.detail
                    );
                    continue;
                }
            };
            total = total.checked_add(size).ok_or_else(|| {
                InstallBackendError::new(
                    "user_driver_size_overflow",
                    "versioned user driver sizes overflow u64",
                )
            })?;
        }
        Ok(total)
    }

    fn planned_uefiseven_bytes(intent: &StartInstallIntent) -> Result<u64, InstallBackendError> {
        if !(intent.options.repair_boot && intent.options.advanced_options.win7_uefi_patch) {
            return Ok(0);
        }
        let source = crate::utils::path::get_uefiseven_dir();
        lr_core::boot_pca::verify_uefiseven_package(&source)
            .map_err(|error| Self::error("verify_uefiseven_source_for_capacity", error))?;
        ["bootx64.efi", "UefiSeven.ini"]
            .iter()
            .try_fold(0_u64, |total, name| {
                let metadata = std::fs::symlink_metadata(source.join(name))
                    .map_err(|error| Self::error("measure_uefiseven_source", error))?;
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(InstallBackendError::new(
                        "unsafe_uefiseven_source",
                        format!("UefiSeven input is not an ordinary file: {name}"),
                    ));
                }
                total.checked_add(metadata.len()).ok_or_else(|| {
                    InstallBackendError::new(
                        "uefiseven_size_overflow",
                        "UefiSeven source sizes overflow u64",
                    )
                })
            })
    }

    fn build_staging_payload_budget(
        &self,
        intent: &StartInstallIntent,
    ) -> Result<StagingPayloadBudget, InstallBackendError> {
        let image_bytes = Self::source_payload_bytes(intent)?;
        let exported_driver_bytes = if intent.options.export_drivers {
            lr_core::driver::estimate_online_oem_driver_export()
                .map_err(|error| Self::error("measure_oem_driver_export", error))?
                .bytes
        } else {
            0
        };
        #[cfg(feature = "ci-automation")]
        let exported_driver_bytes = if ci_existing_target_driver_scenario_run_id().is_some() {
            // The existing-target VM lane intentionally adds its authenticated fixture only after
            // DISM completes. Keeping the fixture out of preflight proves the production
            // reconciliation path with actual > planned instead of allowing the test to bypass
            // the real-world failure mode.
            exported_driver_bytes
                .checked_add(CI_EXISTING_TARGET_DRIVER_FIXTURE_BUDGET_BYTES)
                .ok_or_else(|| {
                    InstallBackendError::new(
                        "ci_driver_fixture_budget_overflow",
                        "CI driver fixture capacity budget overflows u64",
                    )
                })?
        } else {
            exported_driver_bytes
        };
        let pca_bytes = self
            .pca_package
            .as_ref()
            .map(|package| {
                std::fs::symlink_metadata(package.path())
                    .map_err(|error| Self::error("measure_pca_package", error))
                    .and_then(|metadata| {
                        if metadata.is_file() && !metadata.file_type().is_symlink() {
                            Ok(metadata.len())
                        } else {
                            Err(InstallBackendError::new(
                                "unsafe_pca_package",
                                "prepared PCA package is not an ordinary file",
                            ))
                        }
                    })
            })
            .transpose()?
            .unwrap_or(0);
        let user_driver_bytes = Self::planned_user_driver_bytes()?;
        let uefiseven_bytes = Self::planned_uefiseven_bytes(intent)?;
        let budget = StagingPayloadBudget {
            image_bytes,
            exported_driver_bytes,
            pca_bytes,
            user_driver_bytes,
            uefiseven_bytes,
            preinstalled_software_bytes: self.prepared_software_bytes,
        };
        let payload = budget.payload_bytes().ok_or_else(|| {
            InstallBackendError::new(
                "staging_payload_size_overflow",
                "staging payload component sizes overflow u64",
            )
        })?;
        let required = budget.required_bytes().ok_or_else(|| {
            InstallBackendError::new(
                "staging_required_size_overflow",
                "staging payload plus 2 GiB headroom overflows u64",
            )
        })?;
        log::info!(
            "[DATA CAPACITY] exact payload: image={} drivers={} pca={} user_drivers={} uefiseven={} preinstalled_software={} payload={} headroom={} required={}",
            budget.image_bytes,
            budget.exported_driver_bytes,
            budget.pca_bytes,
            budget.user_driver_bytes,
            budget.uefiseven_bytes,
            budget.preinstalled_software_bytes,
            payload,
            lr_core::data_staging::STAGING_OPERATIONAL_HEADROOM_BYTES,
            required
        );
        Ok(budget)
    }

    fn select_data_partition(
        &mut self,
        intent: &StartInstallIntent,
    ) -> Result<(), InstallBackendError> {
        let budget = self.build_staging_payload_budget(intent)?;
        let payload_bytes = budget.payload_bytes().ok_or_else(|| {
            InstallBackendError::new(
                "staging_payload_size_overflow",
                "staging payload component sizes overflow u64",
            )
        })?;
        let required_bytes = budget.required_bytes().ok_or_else(|| {
            InstallBackendError::new(
                "staging_required_size_overflow",
                "staging payload plus 2 GiB headroom overflows u64",
            )
        })?;
        let allow_target_shrink = !matches!(
            intent.options.custom_install_plan,
            lr_core::custom_install::CustomInstallPlan::DualBoot(_)
        );
        #[cfg(feature = "ci-automation")]
        let force_target_shrink = super::pe::ci_force_auto_staging_requested();
        #[cfg(not(feature = "ci-automation"))]
        let force_target_shrink = false;
        let selected = DiskManager::find_suitable_data_partition(
            &intent.target_partition,
            payload_bytes,
            allow_target_shrink,
            force_target_shrink,
        )
        .map_err(|error| Self::error("select_data_partition", error))?;
        let selected = if let Some(selected) = selected {
            selected
        } else if let lr_core::custom_install::CustomInstallPlan::DualBoot(plan) =
            &intent.options.custom_install_plan
        {
            let plan = dual_boot_plan_with_staging(plan, required_bytes)?;
            let transaction = DiskManager::prepare_dual_boot_target(&plan)
                .map_err(|error| Self::error("prepare_dual_boot_staging", error))?;
            let data_partition = transaction.data_partition().ok_or_else(|| {
                InstallBackendError::new(
                    "dual_boot_staging_missing",
                    "the single dual-boot shrink transaction did not create its data/staging volume",
                )
            })?;
            self.dual_boot_transaction = Some(transaction);
            (data_partition, None)
        } else {
            return Err(InstallBackendError::new(
                "no_data_partition",
                format!(
                    "no data partition has enough space; exact payload is {payload_bytes} bytes and the required total with 2 GiB headroom is {required_bytes} bytes ({:.2} GiB)",
                    required_bytes as f64 / 1024.0 / 1024.0 / 1024.0
                ),
            ));
        };
        self.staging_payload_budget = Some(budget);
        self.staging_transaction = selected.1;
        self.data_partition = Some(selected.0);
        std::fs::create_dir_all(self.data_dir()?)
            .map_err(|error| Self::error("create_data_directory", error))?;
        #[cfg(feature = "ci-automation")]
        if super::pe::ci_force_auto_staging_requested() {
            let transaction = self.staging_transaction.as_ref().ok_or_else(|| {
                InstallBackendError::new(
                    "ci_auto_staging_not_created",
                    "CI requested a target-volume staging transaction but no new staging partition was created",
                )
            })?;
            log::warn!(
                "[CI AUTOMATION] forced auto-staging transaction created: {}",
                transaction.ci_rollback_receipt()
            );
        }
        #[cfg(feature = "ci-automation")]
        if super::pe::ci_after_auto_staging_fault_requested() {
            let transaction = self.staging_transaction.as_ref().ok_or_else(|| {
                InstallBackendError::new(
                    "ci_auto_staging_not_created",
                    "CI fault after_auto_staging requires a newly created staging transaction",
                )
            })?;
            return Err(InstallBackendError::new(
                "ci_fault_after_auto_staging",
                format!(
                    "CI fault injection after_auto_staging: {}; exact rollback remains armed",
                    transaction.ci_rollback_receipt()
                ),
            ));
        }
        Ok(())
    }

    fn persist_pca_package(&self) -> Result<(), InstallBackendError> {
        let Some(package) = self.pca_package.as_ref() else {
            return Ok(());
        };
        let destination =
            Path::new(&self.data_dir()?).join(lr_core::pca_compat::STAGED_PACKAGE_RELATIVE_PATH);
        package
            .persist_to(&destination)
            .map_err(|error| Self::error("persist_pca_package", error))?;
        let actual = std::fs::symlink_metadata(&destination)
            .map_err(|error| Self::error("measure_staged_pca_package", error))?;
        let expected = self
            .staging_payload_budget
            .as_ref()
            .ok_or_else(|| {
                InstallBackendError::new(
                    "staging_budget_missing",
                    "data capacity budget is missing before PCA staging",
                )
            })?
            .pca_bytes;
        if !actual.is_file() || actual.file_type().is_symlink() || actual.len() != expected {
            return Err(InstallBackendError::new(
                "staged_pca_size_changed",
                format!("planned {expected} bytes, staged {} bytes", actual.len()),
            ));
        }
        Ok(())
    }

    fn verify_source_image(
        &mut self,
        intent: &StartInstallIntent,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<(), InstallBackendError> {
        use super::image_verify::{ImageVerifier, VerifyStatus};

        if cancellation.is_cancelled() {
            return Err(InstallBackendError::new(
                "cancelled",
                "source image verification was cancelled before it started",
            ));
        }

        // Hold every member of a WIM/ESD/SWM/GHO set without FILE_SHARE_WRITE or
        // FILE_SHARE_DELETE from before verification until the destructive apply phase ends.
        // The apply engines may reopen by path, but Windows cannot replace or mutate those paths
        // while these handles remain alive.
        self.lock_direct_source_set(intent)?;

        if intent.options.is_xp_i386 {
            lr_core::xp_i386::validate_i386_source(Path::new(&intent.image_path))
                .map_err(|error| Self::error("invalid_xp_source", error))?;
            Self::report(
                reporter,
                InstallExecutionPhase::VerifySourceImage,
                100,
                crate::tr!("XP/2003 安装源校验通过"),
            );
            return Ok(());
        }

        if Self::source_verification_is_deferred_to_copy(intent) {
            Self::report(
                reporter,
                InstallExecutionPhase::VerifySourceImage,
                100,
                Path::new(&intent.image_path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default(),
            );
            return Ok(());
        }

        let (progress_tx, progress_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let image = self.immutable_image_path(intent);
        let verify_cancel = Arc::new(AtomicBool::new(false));
        let verify_cancel_for_worker = Arc::clone(&verify_cancel);
        std::thread::spawn(move || {
            let result = ImageVerifier::with_cancel_flag(verify_cancel_for_worker)
                .verify(&image, Some(progress_tx));
            let _ = result_tx.send(result);
        });
        let mut cancellation_reported = false;
        let result = loop {
            while let Ok(progress) = progress_rx.try_recv() {
                Self::report(
                    reporter,
                    InstallExecutionPhase::VerifySourceImage,
                    progress.percentage,
                    progress.status,
                );
            }
            match result_rx.try_recv() {
                Ok(result) => break result,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(InstallBackendError::new(
                        "source_verify_worker_disconnected",
                        "the source verification worker ended without a result",
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if cancellation.is_cancelled() && !cancellation_reported {
                cancellation_reported = true;
                verify_cancel.store(true, Ordering::SeqCst);
                Self::report(
                    reporter,
                    InstallExecutionPhase::VerifySourceImage,
                    0,
                    crate::tr!("已请求取消；校验将在安全点停止。"),
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        };
        if result.status == VerifyStatus::Cancelled || cancellation.is_cancelled() {
            return Err(InstallBackendError::new(
                "cancelled",
                "source image verification was cancelled",
            ));
        }
        if result.status == VerifyStatus::Valid {
            Ok(())
        } else {
            Err(InstallBackendError::new(
                "source_image_verification_failed",
                format!("{}: {}", result.status, result.message),
            ))
        }
    }

    fn copy_source_image(
        &mut self,
        intent: &StartInstallIntent,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<(), InstallBackendError> {
        self.staged_source_image_receipt = None;
        if intent.options.is_xp_i386 {
            return self.copy_xp_source(intent, reporter, cancellation);
        }
        self.pe_source_lock = None;
        let image_set = enumerate_staged_image_set(Path::new(&intent.image_path))
            .map_err(|error| InstallBackendError::new("enumerate_source_image_set", error))?;
        if image_set.volumes.len() > 1 {
            return self.copy_split_source_image(&image_set, reporter, cancellation);
        }
        let file_name = Path::new(&intent.image_path)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                InstallBackendError::new("invalid_image_name", "source image has no file name")
            })?
            .to_string();
        let destination = Path::new(&self.data_dir()?).join(&file_name);
        let source_path = Path::new(&intent.image_path);
        let fused_verify_copy = Self::supports_fused_verify_copy(source_path);
        let source_identity = std::fs::canonicalize(source_path)
            .map_err(|error| Self::error("canonicalize_source_image", error))?;
        if destination.exists() {
            let destination_identity = std::fs::canonicalize(&destination)
                .map_err(|error| Self::error("canonicalize_staged_image", error))?;
            if source_identity == destination_identity {
                self.verify_staged_image(&destination, reporter, cancellation, 0, 99)?;
                Self::report(
                    reporter,
                    InstallExecutionPhase::CopySourceImage,
                    100,
                    file_name.clone(),
                );
                self.staged_image_name = Some(file_name);
                return Ok(());
            }
        }
        let source = if fused_verify_copy {
            Self::open_locked_source(&source_identity)
                .map_err(|error| Self::error("lock_source_image", error))?
        } else {
            File::open(&source_identity).map_err(|error| Self::error("open_source_image", error))?
        };
        let source_metadata = source
            .metadata()
            .map_err(|error| Self::error("inspect_source_image", error))?;
        if !source_metadata.is_file() {
            return Err(InstallBackendError::new(
                "source_image_not_regular_file",
                "source image is not a regular file",
            ));
        }
        let total = source_metadata.len();
        let extension = source_path
            .extension()
            .and_then(|value| value.to_str())
            .filter(|value| {
                !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
            .ok_or_else(|| {
                InstallBackendError::new("invalid_image_extension", "unsafe image extension")
            })?;
        let destination_directory = destination.parent().ok_or_else(|| {
            InstallBackendError::new(
                "invalid_staged_image_path",
                "staged image has no parent directory",
            )
        })?;
        let (temporary, destination_file) =
            lr_core::scoped_temp_file::ScopedTempFile::create_writer_in(
                destination_directory,
                "staged-image",
                extension,
            )
            .map_err(|error| Self::error("create_staged_image", error))?;
        let mut reader = BufReader::with_capacity(1024 * 1024, source);
        let mut writer = BufWriter::with_capacity(1024 * 1024, destination_file);

        let verify_cancel = Arc::new(AtomicBool::new(false));
        let mut verify_progress_rx = None;
        let mut verify_result_rx = None;
        if fused_verify_copy {
            use super::image_verify::ImageVerifier;

            let (progress_tx, progress_rx) = mpsc::channel();
            let (result_tx, result_rx) = mpsc::channel();
            let verify_cancel_for_worker = Arc::clone(&verify_cancel);
            let image = source_identity.to_string_lossy().into_owned();
            std::thread::spawn(move || {
                let result = ImageVerifier::with_cancel_flag_without_persistent_cache(
                    verify_cancel_for_worker,
                )
                .verify(&image, Some(progress_tx));
                let _ = result_tx.send(result);
            });
            verify_progress_rx = Some(progress_rx);
            verify_result_rx = Some(result_rx);
        }

        let mut copy_progress = 0_u8;
        let mut verify_progress = 0_u8;
        let copy_result = lr_core::hash::copy_and_sha256(&mut reader, &mut writer, |copied| {
            if cancellation.is_cancelled() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "image copy was cancelled",
                ));
            }
            copy_progress = if total == 0 {
                100
            } else {
                ((copied.saturating_mul(100) / total).min(100)) as u8
            };
            if let Some(progress_rx) = verify_progress_rx.as_ref() {
                while let Ok(progress) = progress_rx.try_recv() {
                    verify_progress = progress.percentage;
                }
            }
            let percentage = if fused_verify_copy {
                ((u16::from(copy_progress) + u16::from(verify_progress)) * 65 / 200) as u8
            } else {
                (u16::from(copy_progress) * 65 / 100) as u8
            };
            Self::report(
                reporter,
                InstallExecutionPhase::CopySourceImage,
                percentage,
                file_name.clone(),
            );
            Ok(())
        });
        if copy_result.is_err() {
            verify_cancel.store(true, Ordering::SeqCst);
        }

        let source_verification = if let Some(result_rx) = verify_result_rx.as_ref() {
            let result = loop {
                if let Some(progress_rx) = verify_progress_rx.as_ref() {
                    while let Ok(progress) = progress_rx.try_recv() {
                        verify_progress = progress.percentage;
                        let percentage = ((u16::from(copy_progress) + u16::from(verify_progress))
                            * 65
                            / 200) as u8;
                        Self::report(
                            reporter,
                            InstallExecutionPhase::CopySourceImage,
                            percentage,
                            progress.status,
                        );
                    }
                }
                match result_rx.try_recv() {
                    Ok(result) => break result,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        return Err(InstallBackendError::new(
                            "source_verify_worker_disconnected",
                            "the fused source verification worker ended without a result",
                        ));
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                }
                if cancellation.is_cancelled() {
                    verify_cancel.store(true, Ordering::SeqCst);
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            };
            Some(result)
        } else {
            None
        };

        let (copied, copied_sha256) = copy_result.map_err(|error| {
            if error.kind() == std::io::ErrorKind::Interrupted && cancellation.is_cancelled() {
                InstallBackendError::new("cancelled", "image copy was cancelled")
            } else {
                Self::error("copy_staged_image", error)
            }
        })?;

        let full_wimlib_verification_performed = if let Some(result) = source_verification {
            use super::image_verify::VerifyStatus;

            if result.status == VerifyStatus::Cancelled || cancellation.is_cancelled() {
                return Err(InstallBackendError::new(
                    "cancelled",
                    "fused source image verification was cancelled",
                ));
            }
            if result.status != VerifyStatus::Valid {
                return Err(InstallBackendError::new(
                    "source_image_verification_failed",
                    format!("{}: {}", result.status, result.message),
                ));
            }
            if !Self::verification_result_can_issue_receipt(&result) {
                return Err(InstallBackendError::new(
                    "fresh_full_source_verification_missing",
                    "authenticated handoff requires a full uncached wimlib verification from this run",
                ));
            }
            true
        } else {
            false
        };
        drop(reader);
        if cancellation.is_cancelled() {
            return Err(InstallBackendError::new(
                "cancelled",
                "image copy was cancelled",
            ));
        }
        writer
            .flush()
            .map_err(|error| Self::error("flush_staged_image", error))?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|error| Self::error("sync_staged_image", error))?;
        drop(writer);
        Self::report(
            reporter,
            InstallExecutionPhase::CopySourceImage,
            66,
            file_name.clone(),
        );
        let staged_size = std::fs::metadata(temporary.path())
            .map_err(|error| Self::error("inspect_staged_image", error))?
            .len();
        if copied != total || staged_size != total {
            return Err(InstallBackendError::new(
                "staged_image_size_mismatch",
                format!(
                    "expected {total} bytes, copied {copied} bytes, staged {staged_size} bytes"
                ),
            ));
        }
        let staged_hash_span = if fused_verify_copy { 33_u64 } else { 16_u64 };
        let staged_sha256 = lr_core::hash::sha256_file_cancellable(temporary.path(), |hashed| {
            if cancellation.is_cancelled() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "staged image hashing was cancelled",
                ));
            }
            let percentage = if total == 0 {
                66_u8.saturating_add(staged_hash_span as u8)
            } else {
                66 + ((hashed.saturating_mul(staged_hash_span) / total).min(staged_hash_span)) as u8
            };
            Self::report(
                reporter,
                InstallExecutionPhase::CopySourceImage,
                percentage,
                file_name.clone(),
            );
            Ok(())
        })
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::Interrupted && cancellation.is_cancelled() {
                InstallBackendError::new("cancelled", "staged image hashing was cancelled")
            } else {
                Self::error("hash_staged_image", error)
            }
        })?;
        if staged_sha256 != copied_sha256 {
            return Err(InstallBackendError::new(
                "staged_image_hash_mismatch",
                "staged image differs from the source byte stream",
            ));
        }
        if !fused_verify_copy {
            self.verify_staged_image(temporary.path(), reporter, cancellation, 82, 17)?;
        }
        if fused_verify_copy && !full_wimlib_verification_performed {
            return Err(InstallBackendError::new(
                "fresh_full_source_verification_missing",
                "WIM/ESD publication has no fresh full wimlib verification evidence",
            ));
        }
        temporary
            .persist_replace(&destination)
            .map_err(|error| Self::error("commit_staged_image", error))?;
        if fused_verify_copy {
            self.lock_published_verified_image(
                &destination,
                copied,
                &copied_sha256,
                &staged_sha256,
            )?;
        }
        Self::report(
            reporter,
            InstallExecutionPhase::CopySourceImage,
            100,
            file_name.clone(),
        );
        self.staged_image_name = Some(file_name);
        Ok(())
    }

    fn copy_split_source_image(
        &mut self,
        image_set: &StagedImageSet,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<(), InstallBackendError> {
        if !matches!(
            image_set.kind,
            StagedImageSetKind::Swm | StagedImageSetKind::Ghost
        ) {
            return Err(InstallBackendError::new(
                "invalid_split_image_kind",
                "multi-volume staging is limited to SWM and GHO/GHS sets",
            ));
        }
        let source = image_set.volumes.first().ok_or_else(|| {
            InstallBackendError::new("empty_split_image_set", "split image set is empty")
        })?;
        let source_set =
            lr_core::install_source_lock::LockedInstallSourceSet::acquire_pinned_original(source)
                .map_err(|error| Self::error("lock_split_image_set", error))?;
        let identities = source_set
            .artifact_identities()
            .map_err(|error| Self::error("capture_split_image_set", error))?;
        if identities.len() != image_set.volumes.len() {
            return Err(InstallBackendError::new(
                "split_image_inventory_changed",
                "split image inventory changed before protected staging",
            ));
        }
        let total = identities.iter().try_fold(0_u64, |total, identity| {
            total.checked_add(identity.length_bytes).ok_or_else(|| {
                InstallBackendError::new(
                    "split_image_set_size_overflow",
                    "split image volume sizes overflow u64",
                )
            })
        })?;

        let data_dir = PathBuf::from(self.data_dir()?);
        let stage = lr_core::scoped_temp_file::ScopedTempDir::create_system_administrators_in(
            &data_dir,
            "install-image-set",
        )
        .map_err(|error| Self::error("create_protected_split_image_stage", error))?;
        let mut completed = 0_u64;
        for (ordinal, identity) in identities.iter().enumerate() {
            if cancellation.is_cancelled() {
                return Err(InstallBackendError::new(
                    "cancelled",
                    "split image staging was cancelled",
                ));
            }
            let file_name = identity
                .path
                .file_name()
                .and_then(|value| value.to_str())
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    InstallBackendError::new(
                        "invalid_split_image_name",
                        "split image span has no Unicode file name",
                    )
                })?;
            let destination = stage.path().join(file_name);
            let mut output =
                lr_core::scoped_temp_file::create_system_administrators_file_new(&destination)
                    .map_err(|error| Self::error("create_protected_split_image_span", error))?;
            let completed_before = completed;
            source_set
                .copy_artifact_to_verified_writer_with_progress(ordinal, &mut output, |copied| {
                    if cancellation.is_cancelled() {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "split image staging was cancelled",
                        ));
                    }
                    let aggregate = completed_before.saturating_add(copied);
                    let percentage = if total == 0 {
                        90
                    } else {
                        ((aggregate.saturating_mul(90) / total).min(90)) as u8
                    };
                    Self::report(
                        reporter,
                        InstallExecutionPhase::CopySourceImage,
                        percentage,
                        file_name,
                    );
                    Ok(())
                })
                .map_err(|error| {
                    if cancellation.is_cancelled() {
                        InstallBackendError::new("cancelled", "split image staging was cancelled")
                    } else {
                        Self::error("copy_protected_split_image_span", error)
                    }
                })?;
            lr_core::scoped_temp_file::verify_system_administrators_file_custody(&output)
                .map_err(|error| Self::error("verify_protected_split_image_span", error))?;
            completed = completed
                .checked_add(identity.length_bytes)
                .ok_or_else(|| {
                    InstallBackendError::new(
                        "split_image_set_size_overflow",
                        "split image copy progress overflow",
                    )
                })?;
        }
        source_set
            .verify_unchanged()
            .map_err(|error| Self::error("split_image_source_changed", error))?;
        stage
            .verify_system_administrators_custody()
            .map_err(|error| Self::error("verify_protected_split_image_stage", error))?;
        let staged_primary = stage.path().join(&image_set.main_name);
        let staged_lock =
            lr_core::install_source_lock::LockedInstallSourceSet::acquire_pinned_original(
                &staged_primary,
            )
            .map_err(|error| Self::error("lock_staged_split_image_set", error))?;
        let staged_identities = staged_lock
            .artifact_identities()
            .map_err(|error| Self::error("verify_staged_split_image_set", error))?;
        if staged_identities
            .iter()
            .zip(&identities)
            .any(|(staged, source)| {
                staged.length_bytes != source.length_bytes || staged.sha256 != source.sha256
            })
            || staged_identities.len() != identities.len()
        {
            return Err(InstallBackendError::new(
                "staged_split_image_mismatch",
                "protected split image set differs from the held source handles",
            ));
        }
        self.verify_staged_image(&staged_primary, reporter, cancellation, 90, 9)?;
        staged_lock
            .verify_unchanged()
            .map_err(|error| Self::error("staged_split_image_changed", error))?;
        let stage_path = stage.into_path();
        let stage_name = stage_path
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                InstallBackendError::new(
                    "invalid_split_image_stage_name",
                    "protected split image stage has no Unicode directory name",
                )
            })?;
        self.staged_image_name = Some(format!("{stage_name}\\{}", image_set.main_name));
        self.pe_source_lock = Some(staged_lock);
        Self::report(
            reporter,
            InstallExecutionPhase::CopySourceImage,
            100,
            image_set.main_name.clone(),
        );
        Ok(())
    }

    fn verify_staged_image(
        &self,
        path: &Path,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
        progress_base: u8,
        progress_span: u8,
    ) -> Result<(), InstallBackendError> {
        use super::image_verify::{ImageVerifier, VerifyStatus};

        if cancellation.is_cancelled() {
            return Err(InstallBackendError::new(
                "cancelled",
                "staged image verification was cancelled",
            ));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_worker = Arc::clone(&cancel);
        let path = path.to_string_lossy().into_owned();
        let (progress_tx, progress_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result =
                ImageVerifier::with_cancel_flag(cancel_for_worker).verify(&path, Some(progress_tx));
            let _ = result_tx.send(result);
        });
        let result = loop {
            while let Ok(progress) = progress_rx.try_recv() {
                let mapped = progress_base.saturating_add(
                    ((u16::from(progress.percentage) * u16::from(progress_span)) / 100) as u8,
                );
                Self::report(
                    reporter,
                    InstallExecutionPhase::CopySourceImage,
                    mapped.min(progress_base.saturating_add(progress_span)),
                    progress.status,
                );
            }
            match result_rx.try_recv() {
                Ok(result) => break result,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(InstallBackendError::new(
                        "staged_verify_worker_disconnected",
                        "the staged image verification worker ended without a result",
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if cancellation.is_cancelled() {
                cancel.store(true, Ordering::SeqCst);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        if result.status == VerifyStatus::Cancelled || cancellation.is_cancelled() {
            return Err(InstallBackendError::new(
                "cancelled",
                "staged image verification was cancelled",
            ));
        }
        if result.status == VerifyStatus::Valid {
            Ok(())
        } else {
            Err(InstallBackendError::new(
                "staged_image_verification_failed",
                format!("{}: {}", result.status, result.message),
            ))
        }
    }

    fn directory_size_checked(source: &Path) -> Result<u64, InstallBackendError> {
        let metadata = std::fs::symlink_metadata(source)
            .map_err(|error| Self::error("inspect_xp_source", error))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(InstallBackendError::new(
                "unsafe_xp_source_entry",
                format!(
                    "XP source is not an ordinary directory: {}",
                    source.display()
                ),
            ));
        }
        let mut size = 0_u64;
        for entry in
            std::fs::read_dir(source).map_err(|error| Self::error("read_xp_source", error))?
        {
            let entry = entry.map_err(|error| Self::error("read_xp_source_entry", error))?;
            let metadata = entry
                .metadata()
                .map_err(|error| Self::error("inspect_xp_source_entry", error))?;
            let file_type = entry
                .file_type()
                .map_err(|error| Self::error("inspect_xp_source_entry", error))?;
            if file_type.is_symlink() {
                return Err(InstallBackendError::new(
                    "unsafe_xp_source_entry",
                    format!("XP source contains a link: {}", entry.path().display()),
                ));
            }
            if file_type.is_dir() {
                size = size
                    .checked_add(Self::directory_size_checked(&entry.path())?)
                    .ok_or_else(|| {
                        InstallBackendError::new(
                            "directory_size_overflow",
                            format!("directory size overflows u64: {}", source.display()),
                        )
                    })?;
            } else if file_type.is_file() {
                size = size.checked_add(metadata.len()).ok_or_else(|| {
                    InstallBackendError::new(
                        "directory_size_overflow",
                        format!("directory size overflows u64: {}", source.display()),
                    )
                })?;
            } else {
                return Err(InstallBackendError::new(
                    "unsafe_xp_source_entry",
                    format!(
                        "XP source contains a special entry: {}",
                        entry.path().display()
                    ),
                ));
            }
        }
        Ok(size)
    }

    fn copy_xp_tree(
        source: &Path,
        destination: &Path,
        total: u64,
        copied: &mut u64,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<(), InstallBackendError> {
        std::fs::create_dir_all(destination)
            .map_err(|error| Self::error("create_staged_xp_directory", error))?;
        for entry in
            std::fs::read_dir(source).map_err(|error| Self::error("read_xp_source", error))?
        {
            if cancellation.is_cancelled() {
                return Err(InstallBackendError::new(
                    "cancelled",
                    "XP source copy was cancelled",
                ));
            }
            let entry = entry.map_err(|error| Self::error("read_xp_source_entry", error))?;
            let file_type = entry
                .file_type()
                .map_err(|error| Self::error("inspect_xp_source_entry", error))?;
            let target = destination.join(entry.file_name());
            if file_type.is_symlink() {
                return Err(InstallBackendError::new(
                    "unsafe_xp_source_entry",
                    format!("XP source contains a link: {}", entry.path().display()),
                ));
            }
            if file_type.is_dir() {
                Self::copy_xp_tree(
                    &entry.path(),
                    &target,
                    total,
                    copied,
                    reporter,
                    cancellation,
                )?;
            } else if file_type.is_file() {
                let bytes = std::fs::copy(entry.path(), &target)
                    .map_err(|error| Self::error("copy_xp_source_file", error))?;
                *copied = copied.saturating_add(bytes);
                let percentage = if total == 0 {
                    100
                } else {
                    ((copied.saturating_mul(100) / total).min(100)) as u8
                };
                Self::report(
                    reporter,
                    InstallExecutionPhase::CopySourceImage,
                    percentage,
                    crate::tr!("正在暂存 XP/2003 安装源..."),
                );
            } else {
                return Err(InstallBackendError::new(
                    "unsafe_xp_source_entry",
                    format!(
                        "XP source contains a special entry: {}",
                        entry.path().display()
                    ),
                ));
            }
        }
        Ok(())
    }

    fn copy_xp_source(
        &mut self,
        intent: &StartInstallIntent,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<(), InstallBackendError> {
        let source = Path::new(&intent.image_path);
        let arch = lr_core::xp_i386::validate_i386_source(source)
            .map_err(|error| Self::error("invalid_xp_source", error))?;
        let sibling_i386 = (arch == "AMD64")
            .then(|| source.parent().map(|parent| parent.join("I386")))
            .flatten()
            .filter(|path| path.is_dir());
        let mut total = Self::directory_size_checked(source)?;
        if let Some(sibling) = sibling_i386.as_ref() {
            total = total.saturating_add(Self::directory_size_checked(sibling)?);
        }

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| Self::error("stage_xp_clock", error))?
            .as_nanos();
        let root_name = format!("xp-source-{}-{nonce}", std::process::id());
        let data_dir = PathBuf::from(self.data_dir()?);
        let final_root = data_dir.join(&root_name);
        let temporary_root = data_dir.join(format!(".{root_name}.partial"));
        let mut copied = 0_u64;
        let result = (|| {
            Self::copy_xp_tree(
                source,
                &temporary_root.join(arch),
                total,
                &mut copied,
                reporter,
                cancellation,
            )?;
            if let Some(sibling) = sibling_i386.as_ref() {
                Self::copy_xp_tree(
                    sibling,
                    &temporary_root.join("I386"),
                    total,
                    &mut copied,
                    reporter,
                    cancellation,
                )?;
            }
            if copied != total {
                return Err(InstallBackendError::new(
                    "staged_xp_source_size_mismatch",
                    format!("expected {total} bytes, copied {copied} bytes"),
                ));
            }
            lr_core::xp_i386::validate_i386_source(&temporary_root.join(arch))
                .map_err(|error| Self::error("staged_xp_source_invalid", error))?;
            std::fs::rename(&temporary_root, &final_root)
                .map_err(|error| Self::error("commit_staged_xp_source", error))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&temporary_root);
        }
        result?;
        self.staged_image_name = Some(root_name);
        self.staged_xp_source_arch = Some(arch.to_string());
        Ok(())
    }

    fn verify_staged_source_payload_size(
        &self,
        intent: &StartInstallIntent,
    ) -> Result<(), InstallBackendError> {
        let expected = self
            .staging_payload_budget
            .as_ref()
            .ok_or_else(|| {
                InstallBackendError::new(
                    "staging_budget_missing",
                    "data capacity budget is missing after source staging",
                )
            })?
            .image_bytes;
        let staged_name = self.staged_image_name.as_deref().ok_or_else(|| {
            InstallBackendError::new("staged_image_missing", "staged image name is missing")
        })?;
        let staged = Path::new(&self.data_dir()?).join(staged_name);
        let actual = if intent.options.is_xp_i386 {
            Self::directory_size_checked(&staged)?
        } else if let Some(lock) = self.pe_source_lock.as_ref() {
            lock.artifact_identities()
                .map_err(|error| Self::error("measure_staged_image_set", error))?
                .into_iter()
                .try_fold(0_u64, |total, identity| {
                    total.checked_add(identity.length_bytes).ok_or_else(|| {
                        InstallBackendError::new(
                            "staged_image_size_overflow",
                            "staged image set sizes overflow u64",
                        )
                    })
                })?
        } else {
            let metadata = std::fs::symlink_metadata(&staged)
                .map_err(|error| Self::error("measure_staged_image", error))?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(InstallBackendError::new(
                    "unsafe_staged_image",
                    format!("staged image is not an ordinary file: {}", staged.display()),
                ));
            }
            metadata.len()
        };
        if actual != expected {
            return Err(InstallBackendError::new(
                "staged_source_size_changed",
                format!("planned {expected} bytes, staged {actual} bytes"),
            ));
        }
        Ok(())
    }

    fn verify_late_payloads_fit_plan(
        &self,
        intent: &StartInstallIntent,
    ) -> Result<(), InstallBackendError> {
        let planned = self.staging_payload_budget.as_ref().ok_or_else(|| {
            InstallBackendError::new(
                "staging_budget_missing",
                "data capacity budget is missing before image copy",
            )
        })?;
        let user_driver_bytes = Self::planned_user_driver_bytes()?;
        let uefiseven_bytes = Self::planned_uefiseven_bytes(intent)?;
        if user_driver_bytes > planned.user_driver_bytes {
            return Err(InstallBackendError::new(
                "user_driver_payload_grew_after_plan",
                format!(
                    "versioned user drivers grew after capacity planning: planned {} bytes, current {} bytes",
                    planned.user_driver_bytes, user_driver_bytes
                ),
            ));
        }
        if uefiseven_bytes > planned.uefiseven_bytes {
            return Err(InstallBackendError::new(
                "uefiseven_payload_grew_after_plan",
                format!(
                    "UefiSeven payload grew after capacity planning: planned {} bytes, current {} bytes",
                    planned.uefiseven_bytes, uefiseven_bytes
                ),
            ));
        }
        Ok(())
    }

    fn stage_uefiseven(&self) -> Result<(), InstallBackendError> {
        let source = crate::utils::path::get_uefiseven_dir();
        let destination = Path::new(&self.data_dir()?).join("uefiseven");
        lr_core::boot_pca::verify_uefiseven_package(&source)
            .map_err(|error| Self::error("verify_uefiseven_source", error))?;
        if destination.exists() {
            std::fs::remove_dir_all(&destination)
                .map_err(|error| Self::error("clear_uefiseven_stage", error))?;
        }
        std::fs::create_dir_all(&destination)
            .map_err(|error| Self::error("create_uefiseven_stage", error))?;
        for name in ["bootx64.efi", "UefiSeven.ini"] {
            let from = source.join(name);
            std::fs::copy(&from, destination.join(name))
                .map_err(|error| Self::error("copy_uefiseven_stage", error))?;
        }
        lr_core::boot_pca::verify_uefiseven_package(&destination)
            .map_err(|error| Self::error("verify_staged_uefiseven", error))?;
        let actual = lr_core::driver::measure_plain_tree_logical_bytes(&destination)
            .map_err(|error| Self::error("measure_staged_uefiseven", error))?;
        let expected = self
            .staging_payload_budget
            .as_ref()
            .ok_or_else(|| {
                InstallBackendError::new(
                    "staging_budget_missing",
                    "data capacity budget is missing before UefiSeven staging",
                )
            })?
            .uefiseven_bytes;
        if actual != expected {
            return Err(InstallBackendError::new(
                "staged_uefiseven_size_changed",
                format!("planned {expected} bytes, staged {actual} bytes"),
            ));
        }
        Ok(())
    }

    fn directory_has_inf_checked(directory: &Path) -> Result<bool, InstallBackendError> {
        if !directory.exists() {
            return Ok(false);
        }
        let mut pending = vec![directory.to_path_buf()];
        while let Some(path) = pending.pop() {
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|error| Self::error("inspect_user_driver_directory", error))?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(InstallBackendError::new(
                    "unsafe_user_driver_directory",
                    format!(
                        "user driver path is not an ordinary directory: {}",
                        path.display()
                    ),
                ));
            }
            let entries = std::fs::read_dir(&path)
                .map_err(|error| Self::error("read_user_driver_directory", error))?;
            for entry in entries {
                let entry = entry
                    .map_err(|error| Self::error("read_user_driver_directory_entry", error))?;
                let path = entry.path();
                let file_type = entry
                    .file_type()
                    .map_err(|error| Self::error("inspect_user_driver_entry", error))?;
                if file_type.is_symlink() {
                    return Err(InstallBackendError::new(
                        "unsafe_user_driver_entry",
                        format!("user driver tree contains a link: {}", path.display()),
                    ));
                }
                if file_type.is_dir() {
                    pending.push(path);
                } else if file_type.is_file()
                    && path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("inf"))
                {
                    return Ok(true);
                } else if !file_type.is_file() {
                    return Err(InstallBackendError::new(
                        "unsafe_user_driver_entry",
                        format!(
                            "user driver tree contains a special entry: {}",
                            path.display()
                        ),
                    ));
                }
            }
        }
        Ok(false)
    }

    fn stage_user_drivers(&self) -> Result<(), InstallBackendError> {
        let root = Path::new(&self.data_dir()?).join("user_drivers");
        if root.exists() {
            std::fs::remove_dir_all(&root)
                .map_err(|error| Self::error("clear_user_driver_stage", error))?;
        }
        for version in ["win7", "win8", "win10", "win11"] {
            let source = crate::utils::path::get_drivers_dir().join(version);
            let has_inf = match Self::directory_has_inf_checked(&source) {
                Ok(value) => value,
                Err(error) => {
                    log::warn!(
                        "[Driver] optional user-driver directory {version} is unavailable and was skipped: {}",
                        error.detail
                    );
                    continue;
                }
            };
            if !has_inf {
                continue;
            }
            if let Err(error) = Self::copy_directory(&source, &root.join(version)) {
                log::warn!(
                    "[Driver] optional user-driver directory {version} could not be staged and was skipped: {error}"
                );
            }
        }
        let actual = if root.exists() {
            lr_core::driver::measure_plain_tree_logical_bytes(&root)
                .map_err(|error| Self::error("measure_staged_user_drivers", error))?
        } else {
            0
        };
        let expected = self
            .staging_payload_budget
            .as_ref()
            .ok_or_else(|| {
                InstallBackendError::new(
                    "staging_budget_missing",
                    "data capacity budget is missing before user-driver staging",
                )
            })?
            .user_driver_bytes;
        if actual > expected {
            return Err(InstallBackendError::new(
                "staged_user_driver_size_exceeded_plan",
                format!("planned {expected} bytes, staged {actual} bytes"),
            ));
        }
        if actual != expected {
            log::warn!(
                "[DATA CAPACITY] optional user drivers used fewer bytes than planned: planned={}, actual={}",
                expected,
                actual
            );
        }
        Ok(())
    }

    fn write_pe_install_config(
        &mut self,
        intent: &StartInstallIntent,
    ) -> Result<(), InstallBackendError> {
        let staged_name = self.staged_image_name.as_deref().ok_or_else(|| {
            InstallBackendError::new("staged_image_missing", "source image has not been staged")
        })?;
        let pca = self.pca_package.as_ref().map(|package| PcaCompatConfig {
            package: lr_core::pca_compat::STAGED_PACKAGE_RELATIVE_PATH.to_string(),
            sha256: package.sha256().to_string(),
            image_index: package.image_index(),
            target_build: package.target().build,
            target_architecture: package.target().architecture,
        });
        let mut prepared_dual_boot_plan = None;
        let effective_target = if let lr_core::custom_install::CustomInstallPlan::DualBoot(plan) =
            &intent.options.custom_install_plan
        {
            if let Some(transaction) = self.dual_boot_transaction.as_ref() {
                prepared_dual_boot_plan = Some(transaction.plan().clone());
                transaction.target_partition()
            } else {
                let transaction = super::disk::DiskManager::prepare_dual_boot_target(plan)
                    .map_err(|error| Self::error("prepare_dual_boot_target", error))?;
                let target = transaction.target_partition();
                prepared_dual_boot_plan = Some(transaction.plan().clone());
                self.dual_boot_transaction = Some(transaction);
                target
            }
        } else {
            intent.target_partition.clone()
        };
        let mut config =
            intent.to_install_config(staged_name, lr_core::active_engine().as_u8(), pca.as_ref());
        let staged_software = self
            .prepared_software_packages
            .as_deref()
            .unwrap_or(&intent.options.advanced_options.preinstalled_software);
        config.preinstalled_software_config = if staged_software.is_empty() {
            String::new()
        } else {
            lr_core::software_install::encode_selected_packages(staged_software)
                .map_err(|error| Self::error("encode_preinstalled_software_config", error))?
        };
        config.target_partition.clone_from(&effective_target);
        if let Some(plan) = prepared_dual_boot_plan {
            config.custom_install_plan = lr_core::custom_install::CustomInstallPlan::DualBoot(plan);
        }
        if let lr_core::custom_install::CustomInstallPlan::RepartitionAllDisks(plan) =
            &mut config.custom_install_plan
        {
            let data_letter = lr_core::windows_storage::path_drive_letter(Path::new(&format!(
                "{}\\",
                self.data_partition()?.trim_end_matches(['\\', '/'])
            )))
            .ok_or_else(|| {
                InstallBackendError::new(
                    "invalid_full_disk_staging_partition",
                    "full-disk staging partition has no drive letter",
                )
            })?;
            let staging = lr_core::windows_storage::volume_identity(data_letter)
                .map_err(|error| Self::error("capture_full_disk_staging_extent", error))?;
            plan.preserved_staging = plan
                .disks
                .iter()
                .find(|disk| disk.diagnostic_disk_number == staging.disk_number)
                .map(|disk| lr_core::custom_install::PreservedStagingExtent {
                    disk_locator_token: disk.locator_token.clone(),
                    offset_bytes: staging.offset_bytes,
                    length_bytes: staging.extent_length_bytes,
                });
            lr_core::custom_install::validate_full_disk_plan(plan)
                .map_err(|error| Self::error("validate_full_disk_staging_plan", error))?;
        }
        if intent.options.export_drivers && intent.options.driver_action == DriverAction::AutoImport
        {
            let driver_root = Path::new(&self.data_dir()?).join("drivers");
            if !automatic_driver_export_has_payload(&driver_root)
                .map_err(|error| Self::error("verify_empty_pe_driver_backup", error))?
            {
                // Older PE packages treat any existing driver directory as importable. Encode the
                // verified empty result explicitly so they never invoke DISM on a manifest-only
                // directory.
                config.restore_drivers = false;
                config.driver_action_mode = 0;
                log::info!("[Driver] 当前系统没有第三方 OEM 驱动；PE 配置已明确跳过驱动导入");
            }
        }
        if intent.options.is_xp_i386 {
            config.xp_source_arch = self.staged_xp_source_arch.clone().ok_or_else(|| {
                InstallBackendError::new(
                    "staged_xp_arch_missing",
                    "XP source architecture has not been staged",
                )
            })?;
        }
        let auth_key = lr_core::handoff_auth::SessionAuthKey::generate()
            .map_err(|error| Self::error("generate_pe_handoff_auth", error))?;
        let staged_root = Path::new(&self.data_dir()?).join(staged_name);
        let identities = if intent.options.is_xp_i386 {
            let source_arch = self.staged_xp_source_arch.as_deref().ok_or_else(|| {
                InstallBackendError::new(
                    "staged_xp_arch_missing",
                    "XP source architecture has not been staged",
                )
            })?;
            let lock = lr_core::install_source_lock::LockedInstallTree::acquire(
                &staged_root.join(source_arch),
            )
            .map_err(|error| Self::error("lock_pe_xp_source_manifest", error))?;
            let identities = lock
                .artifact_identities()
                .map_err(|error| Self::error("capture_pe_xp_source_manifest", error))?;
            self.pe_xp_source_lock = Some(lock);
            identities
        } else if let Some(lock) = self.pe_source_lock.as_ref() {
            let expected = std::fs::canonicalize(&staged_root)
                .map_err(|error| Self::error("canonicalize_pe_install_source", error))?;
            if lock.selected_path() != expected {
                return Err(InstallBackendError::new(
                    "staged_install_source_lock_mismatch",
                    "held PE source lock does not identify the configured staged image",
                ));
            }
            lock.artifact_identities()
                .map_err(|error| Self::error("capture_pe_install_source_manifest", error))?
        } else {
            let lock =
                lr_core::install_source_lock::LockedInstallSourceSet::acquire_pinned_original(
                    &staged_root,
                )
                .map_err(|error| Self::error("lock_pe_install_source_manifest", error))?;
            let identities = lock
                .artifact_identities()
                .map_err(|error| Self::error("capture_pe_install_source_manifest", error))?;
            self.pe_source_lock = Some(lock);
            identities
        };
        let receipt_matches = Self::receipt_matches_manifest_identities(
            self.staged_source_image_receipt.as_ref(),
            &staged_root,
            &config,
            &identities,
        )?;
        config.source_image_verified =
            receipt_matches && self.pe_supports_source_image_verification_receipt;
        if receipt_matches && !config.source_image_verified {
            log::info!(
                "[IMAGE VERIFY] authenticated PE has no receipt capability; omitting the optional field and retaining legacy PE full verification"
            );
        }
        let role = if intent.options.is_xp_i386 {
            lr_core::handoff_manifest::ArtifactRole::XpSourceFile
        } else {
            lr_core::handoff_manifest::ArtifactRole::InstallImageSpan
        };
        let mut source_artifacts = identities
            .iter()
            .enumerate()
            .map(|(ordinal, identity)| {
                let ordinal = u32::try_from(ordinal).map_err(|_| {
                    Self::error(
                        "build_pe_install_source_manifest",
                        "install artifact ordinal overflow",
                    )
                })?;
                super::install_config::ConfigFileManager::public_artifact_record(
                    self.data_partition()?,
                    identity,
                    role,
                    ordinal,
                )
                .map_err(|error| Self::error("build_pe_install_source_manifest", error))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let data_dir = PathBuf::from(self.data_dir()?);
        #[cfg(feature = "ci-automation")]
        if let Some(run_id) = ci_stale_disabled_driver_scenario_run_id() {
            if intent.options.export_drivers {
                return Err(InstallBackendError::new(
                    "ci_stale_driver_scenario_mismatch",
                    "CI stale disabled-driver scenario requires driver export to be disabled",
                ));
            }
            stage_ci_stale_disabled_driver_fixture(&data_dir, &run_id)
                .map_err(|error| Self::error("stage_ci_stale_disabled_driver_fixture", error))?;
        }
        if !config.pca_compat_package.is_empty() {
            let lock = lr_core::install_source_lock::LockedPlainArtifact::acquire(
                &data_dir.join(&config.pca_compat_package),
            )
            .map_err(|error| Self::error("lock_pe_pca_manifest", error))?;
            source_artifacts.push(
                super::install_config::ConfigFileManager::public_artifact_record(
                    self.data_partition()?,
                    lock.identity(),
                    lr_core::handoff_manifest::ArtifactRole::PcaPackage,
                    0,
                )
                .map_err(|error| Self::error("build_pe_pca_manifest", error))?,
            );
            self.pe_auxiliary_file_locks.push(lock);
        }
        let include_preserved_drivers = should_include_preserved_driver_tree(
            intent.options.export_drivers,
            config.driver_action_mode,
            config.restore_drivers,
        );
        for (relative, role) in [
            (
                "drivers",
                lr_core::handoff_manifest::ArtifactRole::PreservedDriver,
            ),
            (
                "user_drivers",
                lr_core::handoff_manifest::ArtifactRole::UserDriver,
            ),
            (
                "uefiseven",
                lr_core::handoff_manifest::ArtifactRole::UefiSevenFile,
            ),
            (
                "preinstalled_software",
                lr_core::handoff_manifest::ArtifactRole::PreinstalledSoftware,
            ),
        ] {
            if relative == "drivers" && !include_preserved_drivers {
                if data_dir.join(relative).exists() {
                    log::info!(
                        "[PE HANDOFF] ignoring stale preserved-driver directory because the current task has no driver payload"
                    );
                }
                continue;
            }
            let root = data_dir.join(relative);
            if !root.is_dir() {
                continue;
            }
            let lock = lr_core::install_source_lock::LockedInstallTree::acquire(&root)
                .map_err(|error| Self::error("lock_pe_auxiliary_manifest", error))?;
            let Some((lock, artifacts)) = capture_nonempty_auxiliary_tree(lock)
                .map_err(|error| Self::error("capture_pe_auxiliary_manifest", error))?
            else {
                // Optional downloads and optional driver groups may legitimately yield no files.
                // Their producing phase already enforces any feature-specific mandatory result;
                // an undeclared empty directory is not an authenticated artifact and must not
                // convert a usable Windows installation into a total failure.
                log::info!(
                    "[PE HANDOFF] ignoring empty optional auxiliary directory: {}",
                    root.display()
                );
                continue;
            };
            for (ordinal, identity) in artifacts.iter().enumerate() {
                let ordinal = u32::try_from(ordinal).map_err(|_| {
                    Self::error(
                        "build_pe_auxiliary_manifest",
                        "auxiliary artifact ordinal overflow",
                    )
                })?;
                source_artifacts.push(
                    super::install_config::ConfigFileManager::public_artifact_record(
                        self.data_partition()?,
                        identity,
                        role,
                        ordinal,
                    )
                    .map_err(|error| Self::error("build_pe_auxiliary_manifest", error))?,
                );
            }
            self.pe_auxiliary_tree_locks.push(lock);
        }
        #[cfg(feature = "ci-automation")]
        let ci_stale_driver_manifest_receipt =
            if let Some(run_id) = ci_stale_disabled_driver_scenario_run_id() {
                if intent.options.export_drivers
                    || config.driver_action_mode != 0
                    || config.restore_drivers
                    || include_preserved_drivers
                {
                    return Err(InstallBackendError::new(
                    "ci_stale_driver_scenario_mismatch",
                    "CI stale disabled-driver scenario did not retain the disabled driver policy",
                ));
                }
                let preserved_driver_artifact_count = source_artifacts
                    .iter()
                    .filter(|artifact| {
                        artifact.role == lr_core::handoff_manifest::ArtifactRole::PreservedDriver
                    })
                    .count();
                let run_fixture_artifact_count_any_role = source_artifacts
                    .iter()
                    .filter(|artifact| artifact.relative_path.contains(&run_id))
                    .count();
                Some((
                    run_id,
                    source_artifacts.len(),
                    preserved_driver_artifact_count,
                    run_fixture_artifact_count_any_role,
                ))
            } else {
                None
            };
        let private_wifi_profile = intent
            .options
            .advanced_options
            .migrate_wifi
            .then_some(intent.options.advanced_options.wifi_profile_xml.as_bytes());
        let auto_staging_source_length_before_bytes = self
            .staging_transaction
            .as_ref()
            .map(super::disk::PreparedStagingTransaction::source_length_before_bytes);
        let transaction = super::install_config::ConfigFileManager::write_install_config_transactional_with_private_wifi(
                &effective_target,
                self.data_partition()?,
                &config,
                &auth_key,
                source_artifacts,
                private_wifi_profile,
                auto_staging_source_length_before_bytes,
            )
            .map_err(|error| Self::error("write_pe_install_config", error))?;
        #[cfg(feature = "ci-automation")]
        if let Some((run_id, total, preserved, run_fixture)) = ci_stale_driver_manifest_receipt {
            write_ci_stale_driver_manifest_receipt(&run_id, total, preserved, run_fixture)
                .map_err(|error| Self::error("write_ci_stale_driver_manifest_receipt", error))?;
        }
        self.handoff_auth_key = Some(auth_key);
        self.install_config_transaction = Some(transaction);
        Ok(())
    }

    fn refresh_target(
        &mut self,
        context: &InstallExecutionContext,
    ) -> Result<(), InstallBackendError> {
        let identity = context.stable_target.ok_or_else(|| {
            InstallBackendError::new("missing_stable_target", "stable target identity is absent")
        })?;
        let partitions = DiskManager::get_partitions()
            .map_err(|error| Self::error("enumerate_partitions", error))?;
        let target = partitions
            .iter()
            .find(|partition| partition_matches_stable_identity(partition, identity))
            .ok_or_else(|| {
                InstallBackendError::new(
                    "target_identity_changed",
                    format!(
                        "disk {} partition {} no longer exists or has no usable drive letter",
                        identity.disk_number, identity.partition_number
                    ),
                )
            })?;
        if !target.install_target_eligible {
            return Err(InstallBackendError::new(
                "target_is_hidden_or_service_partition",
                "the current canonical disk layout no longer identifies the selected extent as an ordinary installable user-data partition",
            ));
        }
        if target.letter.trim().is_empty() {
            return Err(InstallBackendError::new(
                "target_has_no_letter",
                "the verified target partition has no drive letter",
            ));
        }
        self.target.clone_from(&target.letter);
        self.target_style = target.partition_style;
        self.partitions = partitions;
        Ok(())
    }

    #[cfg(not(feature = "non-elevated-tests"))]
    fn refresh_target_after_partition_scripts(
        &mut self,
        context: &InstallExecutionContext,
    ) -> Result<(), InstallBackendError> {
        let identity = context.stable_target.ok_or_else(|| {
            InstallBackendError::new("missing_stable_target", "stable target identity is absent")
        })?;
        for attempt in 0..4 {
            std::thread::sleep(std::time::Duration::from_millis(if attempt == 0 {
                800
            } else {
                500
            }));
            if self.refresh_target(context).is_ok() {
                return Ok(());
            }
        }

        let free = DiskManager::find_available_drive_letter().ok_or_else(|| {
            InstallBackendError::new(
                "no_free_drive_letter",
                "no free drive letter is available for the verified target partition",
            )
        })?;
        let offset = super::quick_partition::get_physical_disks()
            .into_iter()
            .find(|disk| disk.disk_number == identity.disk_number)
            .and_then(|disk| {
                disk.partitions
                    .into_iter()
                    .find(|partition| partition.partition_number == identity.partition_number)
            })
            .map(|partition| partition.offset_bytes)
            .ok_or_else(|| {
                InstallBackendError::new(
                    "target_identity_changed",
                    "verified target partition disappeared before drive-letter assignment",
                )
            })?;
        let expected_layout = lr_core::windows_storage::disk_layout_snapshot(identity.disk_number)
            .map_err(|error| Self::error("target_layout_snapshot", error))?;
        lr_core::windows_storage::assign_partition_drive_letter_checked(
            identity.disk_number,
            offset,
            free,
            &expected_layout,
        )
        .map_err(|error| Self::error("assign_target_letter", error))?;
        log::info!(
            "[NATIVE INSTALL] assigned drive {free}: to disk {} partition {} through VDS",
            identity.disk_number,
            identity.partition_number,
        );

        for attempt in 0..4 {
            std::thread::sleep(std::time::Duration::from_millis(if attempt == 0 {
                800
            } else {
                500
            }));
            if self.refresh_target(context).is_ok() {
                return Ok(());
            }
        }
        Err(InstallBackendError::new(
            "target_identity_changed",
            format!(
                "disk {} partition {} no longer exists or could not be assigned a drive letter",
                identity.disk_number, identity.partition_number,
            ),
        ))
    }

    fn report(
        reporter: &mut dyn InstallExecutionReporter,
        phase: InstallExecutionPhase,
        percentage: u8,
        detail: impl Into<String>,
    ) {
        reporter.report(InstallExecutionEvent::Progress {
            phase,
            percentage,
            detail: detail.into(),
        });
    }

    fn apply_wim(
        &self,
        intent: &StartInstallIntent,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<(), InstallBackendError> {
        if cancellation.is_cancelled() {
            return Err(InstallBackendError::new(
                "cancelled",
                "WIM apply was cancelled before it started",
            ));
        }
        let (progress_tx, progress_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let image = self.immutable_image_path(intent);
        let target = format!("{}\\", self.target);
        let volume_index = intent.volume_index;
        Self::report(
            reporter,
            InstallExecutionPhase::ApplyWimImage,
            0,
            crate::tr!("正在启动镜像释放引擎..."),
        );
        let apply_cancel = Arc::new(AtomicBool::new(false));
        let apply_cancel_for_worker = Arc::clone(&apply_cancel);
        std::thread::spawn(move || {
            let result = super::dism::Dism::new().apply_image_cancellable(
                &image,
                &target,
                volume_index,
                Some(progress_tx),
                Some(apply_cancel_for_worker),
            );
            let _ = result_tx.send(result);
        });
        let mut cancellation_reported = false;
        loop {
            while let Ok(progress) = progress_rx.try_recv() {
                Self::report(
                    reporter,
                    InstallExecutionPhase::ApplyWimImage,
                    progress.percentage,
                    progress.status,
                );
            }
            match result_rx.try_recv() {
                Ok(result) => {
                    while let Ok(progress) = progress_rx.try_recv() {
                        Self::report(
                            reporter,
                            InstallExecutionPhase::ApplyWimImage,
                            progress.percentage,
                            progress.status,
                        );
                    }
                    if cancellation.is_cancelled() {
                        return Err(InstallBackendError::new(
                            "cancelled",
                            "WIM apply was cancelled",
                        ));
                    }
                    return result.map_err(|error| Self::error("apply_wim", error));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(InstallBackendError::new(
                        "apply_wim_worker_disconnected",
                        "the WIM apply worker ended without a result",
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if cancellation.is_cancelled() && !cancellation_reported {
                cancellation_reported = true;
                apply_cancel.store(true, Ordering::SeqCst);
                Self::report(
                    reporter,
                    InstallExecutionPhase::ApplyWimImage,
                    0,
                    crate::tr!("已请求取消；镜像引擎将在安全点停止。"),
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    fn apply_ghost(
        &self,
        intent: &StartInstallIntent,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<(), InstallBackendError> {
        let ghost = super::ghost::Ghost::new();
        if !ghost.is_available() {
            return Err(InstallBackendError::new(
                "ghost_unavailable",
                "Ghost executable is unavailable",
            ));
        }
        let cancel_flag = ghost.get_cancel_flag();
        let (progress_tx, progress_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let image = self.immutable_image_path(intent);
        let target = self.target.clone();
        let partitions = self.partitions.clone();
        std::thread::spawn(move || {
            let result =
                ghost.restore_image_to_letter(&image, &target, &partitions, Some(progress_tx));
            let _ = result_tx.send(result);
        });
        loop {
            while let Ok(progress) = progress_rx.try_recv() {
                Self::report(
                    reporter,
                    InstallExecutionPhase::ApplyGhostImage,
                    progress.percentage,
                    progress.status,
                );
            }
            match result_rx.try_recv() {
                Ok(result) => {
                    return result.map_err(|error| Self::error("apply_ghost", error));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(InstallBackendError::new(
                        "apply_ghost_worker_disconnected",
                        "the Ghost worker ended without a result",
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if cancellation.is_cancelled() {
                cancel_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    fn legacy_advanced(intent: &StartInstallIntent) -> super::advanced_options::AdvancedOptions {
        (&intent.options.advanced_options).into()
    }

    fn verify_storage_driver_preflight(
        intent: &StartInstallIntent,
    ) -> Result<(), InstallBackendError> {
        if !intent
            .options
            .advanced_options
            .import_storage_controller_drivers
        {
            return Ok(());
        }
        let hardware_ids = lr_core::driver::list_present_hardware_ids()
            .map_err(|error| Self::error("storage_driver_hardware_enumeration", error))?;
        let packages = lr_core::storage_driver_match::select_builtin_storage_driver_packages(
            hardware_ids.iter().map(String::as_str),
        )
        .map_err(|error| Self::error("storage_driver_selection", error))?;
        let root = crate::utils::path::get_drivers_dir().join("storage_controller");
        for package in packages {
            let directory = root.join(package.directory_name());
            lr_core::storage_driver_match::verify_builtin_storage_driver_package(
                package, &directory,
            )
            .map_err(|error| Self::error("storage_driver_package_verification", error))?;
        }
        Ok(())
    }

    fn copy_directory(source: &Path, destination: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(destination)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            let destination = destination.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                Self::copy_directory(&entry.path(), &destination)?;
            } else {
                std::fs::copy(entry.path(), destination)?;
            }
        }
        Ok(())
    }

    fn process_drivers(&self, intent: &StartInstallIntent) -> Result<(), InstallBackendError> {
        if !self.driver_backup.exists() {
            return if intent.options.export_drivers
                && intent.options.driver_action != DriverAction::None
            {
                Err(InstallBackendError::new(
                    "driver_backup_missing",
                    "the requested driver backup directory is missing",
                ))
            } else {
                Ok(())
            };
        }
        let backup = self.driver_backup.to_string_lossy();
        match intent.options.driver_action {
            DriverAction::AutoImport => {
                if !automatic_driver_export_has_payload(&self.driver_backup)
                    .map_err(|error| Self::error("verify_empty_driver_backup", error))?
                {
                    std::fs::remove_dir_all(&self.driver_backup)
                        .map_err(|error| Self::error("clear_empty_driver_backup", error))?;
                    log::info!(
                        "[Driver] 当前系统没有第三方 OEM 驱动；直接安装路径安全跳过驱动导入"
                    );
                    return Ok(());
                }
                super::dism::Dism::new()
                    .add_drivers_offline(&format!("{}\\", self.target), &backup)
                    .map_err(|error| Self::error("import_preserved_drivers", error))?;
                lr_core::driver::verify_offline_storage_driver_requirements(
                    Path::new(&self.target),
                    &self.driver_backup,
                )
                .map_err(|error| Self::error("verify_preserved_storage_drivers", error))?;
                std::fs::remove_dir_all(&self.driver_backup)
                    .map_err(|error| Self::error("clear_imported_driver_backup", error))?;
            }
            DriverAction::SaveOnly => {
                let destination = PathBuf::from(format!("{}\\LetRecovery_Drivers", self.target));
                Self::copy_directory(&self.driver_backup, &destination)
                    .map_err(|error| Self::error("preserve_driver_backup", error))?;
                std::fs::remove_dir_all(&self.driver_backup)
                    .map_err(|error| Self::error("clear_preserved_driver_backup", error))?;
            }
            DriverAction::None => {}
        }
        Ok(())
    }

    fn reject_format_dependency_on_target(
        target_letter: char,
        target_identity: lr_core::windows_storage::VolumeIdentity,
        name: &str,
        path: &Path,
    ) -> Result<(), InstallBackendError> {
        let resolved = std::fs::canonicalize(path)
            .map_err(|error| Self::error("resolve_format_dependency", error))?;
        let Some(source_letter) = dependency_drive_letter(path, &resolved) else {
            return Err(InstallBackendError::new(
                "unverifiable_format_dependency",
                format!(
                    "cannot prove that {name} is separate from the local target because it has no drive-letter identity"
                ),
            ));
        };
        if source_letter.eq_ignore_ascii_case(&target_letter) {
            return Err(InstallBackendError::new(
                "format_dependency_on_target",
                format!("{name} is stored on the target volume {target_letter}:"),
            ));
        }
        if matches!(
            lr_core::windows_storage::drive_kind(source_letter),
            Ok(lr_core::windows_storage::DriveKind::Remote)
        ) {
            return Err(InstallBackendError::new(
                "unverifiable_network_format_dependency",
                format!(
                    "cannot prove that network-mapped {name} is not backed by a loopback share on target {target_letter}:"
                ),
            ));
        }
        match lr_core::windows_storage::volume_identity(source_letter) {
            Ok(source_identity)
                if source_identity.disk_number == target_identity.disk_number
                    && source_identity.offset_bytes == target_identity.offset_bytes =>
            {
                Err(InstallBackendError::new(
                    "format_dependency_on_target",
                    format!("{name} resolves to the target volume {target_letter}:"),
                ))
            }
            Ok(_) => Ok(()),
            Err(error) => {
                if matches!(
                    lr_core::windows_storage::drive_kind(source_letter),
                    Ok(kind) if dependency_kind_may_lack_local_extent(kind)
                ) {
                    // A mounted read-only ISO cannot alias the selected writable local target. A
                    // WinPE RAM disk also cannot be the selected local-disk target once the drive
                    // letters differ; it commonly has no physical disk extent for the IOCTL.
                    // Network mappings remain fail-closed because a loopback share can reside on
                    // the target volume even though GetDriveTypeW reports DRIVE_REMOTE.
                    log::debug!(
                        "[NATIVE INSTALL] physical identity unavailable for {name} on {source_letter}: {error}"
                    );
                    Ok(())
                } else {
                    Err(InstallBackendError::new(
                        "resolve_format_dependency_identity",
                        format!(
                            "cannot prove that {name} on {source_letter}: is separate from target {target_letter}: {error}"
                        ),
                    ))
                }
            }
        }
    }

    fn validate_direct_target_dependencies(
        &self,
        intent: &StartInstallIntent,
        context: &InstallExecutionContext,
    ) -> Result<(), InstallBackendError> {
        let plan = Self::format_plan_for_intent(&self.target, intent)?;
        let letter =
            plan.drive.chars().next().ok_or_else(|| {
                InstallBackendError::new("format_target", "target drive is empty")
            })?;
        let stable = context.stable_target.ok_or_else(|| {
            InstallBackendError::new("missing_stable_target", "stable target identity is absent")
        })?;
        if !lr_core::windows_storage::same_stable_volume_identity(
            stable.stable_volume,
            intent.target_stable_identity,
        ) {
            return Err(InstallBackendError::new(
                "stable_target_token_mismatch",
                "execution context and install intent authorize different stable volumes",
            ));
        }
        let target_identity = lr_core::windows_storage::VolumeIdentity {
            disk_number: stable.disk_number,
            offset_bytes: stable.partition_offset_bytes,
            extent_length_bytes: stable.partition_size_bytes,
        };
        let running_windows = lr_core::windows_storage::current_windows_drive_letter()
            .map_err(|error| Self::error("resolve_running_windows_volume", error))?;
        if running_windows.eq_ignore_ascii_case(&letter) {
            return Err(InstallBackendError::new(
                "format_running_windows_volume",
                format!("target {letter}: is the current running Windows volume"),
            ));
        }
        match lr_core::windows_storage::volume_identity(running_windows) {
            Ok(running_identity)
                if running_identity.disk_number == target_identity.disk_number
                    && running_identity.offset_bytes == target_identity.offset_bytes =>
            {
                return Err(InstallBackendError::new(
                    "format_running_windows_volume",
                    format!(
                        "target {letter}: resolves to the current running Windows physical volume"
                    ),
                ));
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    lr_core::windows_storage::drive_kind(running_windows),
                    Ok(lr_core::windows_storage::DriveKind::RamDisk)
                ) =>
            {
                log::debug!(
                    "[NATIVE INSTALL] running Windows is on RAM disk {running_windows}: physical extents unavailable: {error}"
                );
            }
            Err(error) => {
                return Err(Self::error("resolve_running_windows_identity", error));
            }
        }
        for (name, path) in image_format_dependencies(intent) {
            Self::reject_format_dependency_on_target(letter, target_identity, name, path)?;
        }
        let executable = std::env::current_exe()
            .map_err(|error| Self::error("resolve_running_executable", error))?;
        Self::reject_format_dependency_on_target(
            letter,
            target_identity,
            "running executable and log directory",
            &executable,
        )?;
        if !intent.options.custom_unattend_path.trim().is_empty() {
            Self::reject_format_dependency_on_target(
                letter,
                target_identity,
                "custom unattended file",
                Path::new(&intent.options.custom_unattend_path),
            )?;
        }
        let advanced = &intent.options.advanced_options;
        if advanced.import_custom_drivers && !advanced.custom_drivers_path.trim().is_empty() {
            Self::reject_format_dependency_on_target(
                letter,
                target_identity,
                "custom driver directory",
                Path::new(&advanced.custom_drivers_path),
            )?;
        }
        if advanced.run_script_during_deploy && !advanced.deploy_script_path.trim().is_empty() {
            Self::reject_format_dependency_on_target(
                letter,
                target_identity,
                "deployment script",
                Path::new(&advanced.deploy_script_path),
            )?;
        }
        if advanced.run_script_first_login && !advanced.first_login_script_path.trim().is_empty() {
            Self::reject_format_dependency_on_target(
                letter,
                target_identity,
                "first-login script",
                Path::new(&advanced.first_login_script_path),
            )?;
        }
        if advanced.import_registry_file && !advanced.registry_file_path.trim().is_empty() {
            Self::reject_format_dependency_on_target(
                letter,
                target_identity,
                "registry file",
                Path::new(&advanced.registry_file_path),
            )?;
        }
        if advanced.import_custom_files && !advanced.custom_files_path.trim().is_empty() {
            Self::reject_format_dependency_on_target(
                letter,
                target_identity,
                "custom file directory",
                Path::new(&advanced.custom_files_path),
            )?;
        }
        if self.driver_backup.exists() {
            Self::reject_format_dependency_on_target(
                letter,
                target_identity,
                "exported driver backup",
                &self.driver_backup,
            )?;
        }
        Ok(())
    }

    fn format_target_compat(
        &self,
        intent: &StartInstallIntent,
        context: &InstallExecutionContext,
    ) -> Result<(), InstallBackendError> {
        let plan = Self::format_plan_for_intent(&self.target, intent)?;
        let letter =
            plan.drive.chars().next().ok_or_else(|| {
                InstallBackendError::new("format_target", "target drive is empty")
            })?;
        let stable = context.stable_target.ok_or_else(|| {
            InstallBackendError::new("missing_stable_target", "stable target identity is absent")
        })?;
        if !lr_core::windows_storage::same_stable_volume_identity(
            stable.stable_volume,
            intent.target_stable_identity,
        ) {
            return Err(InstallBackendError::new(
                "stable_target_token_mismatch",
                "execution context and install intent authorize different stable volumes",
            ));
        }
        lr_core::windows_storage::format_drive_with_options_stable_checked(
            letter,
            intent.target_stable_identity,
            &lr_core::windows_storage::FormatOptions {
                file_system: lr_core::windows_storage::FileSystem::Ntfs,
                label: plan.volume_label,
                allocation_unit_size: 0,
                quick: true,
                force_dismount: true,
            },
        )
        .map_err(|error| Self::error("format_target", error))
    }

    fn format_plan_for_intent(
        target: &str,
        intent: &StartInstallIntent,
    ) -> Result<native_install_compat::FormatCompatibilityPlan, InstallBackendError> {
        let advanced = &intent.options.advanced_options;
        let label = (advanced.custom_volume_label && !advanced.volume_label.trim().is_empty())
            .then_some(advanced.volume_label.as_str());
        native_install_compat::build_format_plan(target, label)
            .map_err(|error| Self::error("invalid_format_plan", error))
    }

    fn deactivate_xp_sibling_partitions(&self) -> Result<(), InstallBackendError> {
        let identities = self
            .partitions
            .iter()
            .map(|partition| PartitionIdentity {
                letter: partition.letter.as_str(),
                disk_number: partition.disk_number,
            })
            .collect::<Vec<_>>();
        let inventory = super::quick_partition::get_physical_disks();
        let mut changed = Vec::new();
        for letter in native_install_compat::sibling_inactive_letters(&self.target, &identities) {
            let letter_char = letter
                .chars()
                .next()
                .map(|value| value.to_ascii_uppercase());
            let partition = inventory.iter().find_map(|disk| {
                disk.partitions
                    .iter()
                    .find(|partition| {
                        partition
                            .drive_letter
                            .map(|value| value.to_ascii_uppercase())
                            == letter_char
                    })
                    .map(|partition| {
                        (
                            disk.disk_number,
                            partition.offset_bytes,
                            partition.is_active,
                        )
                    })
            });
            let result = partition
                .ok_or_else(|| anyhow::anyhow!("cannot resolve sibling partition {letter}:"))
                .and_then(|(disk_number, offset, was_active)| {
                    let expected_layout =
                        lr_core::windows_storage::disk_layout_snapshot(disk_number)?;
                    lr_core::windows_storage::set_mbr_active_checked(
                        disk_number,
                        offset,
                        false,
                        &expected_layout,
                    )
                    .map_err(anyhow::Error::from)?;
                    changed.push((disk_number, offset, was_active));
                    Ok(())
                });
            if let Err(error) = result {
                let mut rollback_errors = Vec::new();
                for (disk_number, offset, was_active) in changed.into_iter().rev() {
                    let rollback = lr_core::windows_storage::disk_layout_snapshot(disk_number)
                        .and_then(|layout| {
                            lr_core::windows_storage::set_mbr_active_checked(
                                disk_number,
                                offset,
                                was_active,
                                &layout,
                            )
                        });
                    if let Err(rollback) = rollback {
                        rollback_errors.push(rollback.to_string());
                    }
                }
                let detail = if rollback_errors.is_empty() {
                    error.to_string()
                } else {
                    format!(
                        "{error}; additionally failed to restore sibling active flags: {}",
                        rollback_errors.join("; ")
                    )
                };
                return Err(Self::error("deactivate_xp_sibling_partition", detail));
            }
        }
        Ok(())
    }

    fn ensure_mbr_signature(&self, disk_number: u32) -> Result<(), InstallBackendError> {
        match lr_core::windows_storage::mbr_signature(disk_number)
            .map_err(|error| Self::error("read_mbr_signature", error))?
        {
            Some(signature) if signature != 0 => {
                log::info!(
                    "[NATIVE INSTALL] disk {disk_number} keeps MBR signature {signature:08X}"
                );
                Ok(())
            }
            None => {
                // Legacy BCDBoot/Bootsect and VDS bootIndicator are MBR-only operations. A GPT
                // readback here is not an optional signature condition: continuing would write
                // BIOS files and then fail (or partially mutate state) while setting active.
                Err(InstallBackendError::new(
                    "legacy_boot_requires_mbr",
                    format!(
                        "disk {disk_number} is not MBR; refusing the Legacy boot and active-partition path"
                    ),
                ))
            }
            Some(0) => {
                // A zero-signature disk cannot produce the stable MBR identity required by the
                // direct-install authorization. Do not reopen a possibly reused PhysicalDriveN
                // and invent an identity during boot repair.
                Err(InstallBackendError::new(
                    "unstable_zero_mbr_signature",
                    format!(
                        "disk {disk_number} has a zero MBR signature and cannot be safely rebound; initialize its signature in the checked partitioning workflow and restart installation"
                    ),
                ))
            }
            Some(_) => unreachable!("the non-zero signature guard covers every other u32"),
        }
    }

    fn inject_versioned_user_drivers(&self, is_xp: bool) -> Result<(), InstallBackendError> {
        if is_xp {
            return Ok(());
        }
        let ntdll = Path::new(&self.target)
            .join("Windows")
            .join("System32")
            .join("ntdll.dll");
        let Some((major, minor, build, _)) = super::system_utils::get_file_version(&ntdll) else {
            log::warn!("[NATIVE INSTALL] cannot identify target version for user drivers");
            return Ok(());
        };
        let family = native_install_compat::classify_windows_version(major, minor, build);
        let Some(source) = native_install_compat::user_driver_source(
            &crate::utils::path::get_drivers_dir(),
            family,
        ) else {
            return Ok(());
        };
        if !match Self::directory_has_inf_checked(&source) {
            Ok(value) => value,
            Err(error) => {
                log::warn!(
                    "[NATIVE INSTALL] optional user drivers were skipped: {}",
                    error.detail
                );
                false
            }
        } {
            return Ok(());
        }
        if let Err(error) = super::dism::Dism::new()
            .add_drivers_offline(&format!("{}\\", self.target), &source.to_string_lossy())
        {
            log::warn!("[NATIVE INSTALL] optional user drivers were skipped: {error}");
        }
        Ok(())
    }

    fn write_unattend(&self, intent: &StartInstallIntent) -> Result<(), InstallBackendError> {
        let panther = Path::new(&self.target).join("Windows").join("Panther");
        std::fs::create_dir_all(&panther).map_err(|error| Self::error("create_panther", error))?;
        let destination = panther.join("unattend.xml");
        if !intent.options.custom_unattend_path.trim().is_empty() {
            if intent.options.advanced_options.disable_windows_defender {
                return Err(InstallBackendError::new(
                    "required_security_ui_hook_unavailable",
                    "Windows Security UI removal requires LetRecovery's built-in unattended file",
                ));
            }
            if intent.options.advanced_options.disable_reserved_storage {
                log::warn!(
                    "[ADVANCED_RESERVED_STORAGE] phase=online_hook status=skipped reason=custom_unattend_not_modified"
                );
            }
            if intent.options.advanced_options.remove_uwp_apps {
                return Err(InstallBackendError::new(
                    "required_appx_hook_unavailable",
                    "preinstalled application removal requires LetRecovery's built-in unattended file",
                ));
            }
            std::fs::copy(&intent.options.custom_unattend_path, &destination)
                .map_err(|error| Self::error("copy_custom_unattend", error))?;
            return Ok(());
        }

        let architecture = match super::system_utils::get_system_architecture(&self.target) {
            super::system_utils::SystemArchitecture::X86 => UnattendArchitecture::X86,
            super::system_utils::SystemArchitecture::Amd64 => UnattendArchitecture::Amd64,
            unexpected => {
                return Err(InstallBackendError::new(
                    "unsupported_unattend_architecture",
                    format!("unsupported target architecture: {unexpected:?}"),
                ));
            }
        };
        let first_logon_software = self
            .direct_staged_software
            .as_deref()
            .unwrap_or(&intent.options.advanced_options.preinstalled_software);
        let temporary_oobe_account = if intent
            .options
            .advanced_options
            .builtin_administrator
            .enabled
        {
            let session = lr_core::handoff_auth::generate_session_id()
                .map_err(|error| Self::error("generate_temporary_oobe_session", error))?;
            Some(
                lr_core::unattend_account::temporary_oobe_account_name(session.as_str()).map_err(
                    |error| InstallBackendError::new("generate_temporary_oobe_account", error),
                )?,
            )
        } else {
            None
        };
        lr_core::first_logon::stage_with_software_shutdown_and_personal_restore_and_builtin(
            &self.target,
            first_logon_software,
            intent.options.automation_shutdown_on_terminal,
            None,
            temporary_oobe_account.as_deref().map(|temporary_name| {
                lr_core::first_logon::BuiltinAdministratorTransitionAccounts {
                    desired_name: intent
                        .options
                        .advanced_options
                        .builtin_administrator
                        .account_name
                        .as_str(),
                    temporary_name,
                    password: &intent
                        .options
                        .advanced_options
                        .builtin_administrator
                        .password,
                }
            }),
        )
        .map_err(|error| Self::error("stage_first_logon_finalizer", error))?;
        // Windows Setup can leave a disabled `defaultuser0` account even for an ordinary
        // unattended local-account install. The first-logon finalizer always owns that bounded
        // cleanup, so its native NetAPI/Profile helper must be staged for every install rather
        // than only for the optional built-in Administrator transition.
        lr_core::first_logon::stage_account_helper(&self.target)
            .map_err(|error| Self::error("stage_account_helper", error))?;
        let ntdll = Path::new(&self.target)
            .join("Windows")
            .join("System32")
            .join("ntdll.dll");
        let target_version = super::system_utils::get_file_version(&ntdll);
        let family = target_version
            .map(|(major, minor, build, _)| {
                native_install_compat::classify_windows_version(major, minor, build)
            })
            .unwrap_or(native_install_compat::WindowsFamily::Unsupported);
        let international = if matches!(
            family,
            native_install_compat::WindowsFamily::Windows10
                | native_install_compat::WindowsFamily::Windows11
        ) {
            Some(
                lr_core::offline_international::read_offline_international_settings(&self.target)
                    .map_err(|error| Self::error("read_offline_international", error))?,
            )
        } else {
            None
        };
        let advanced = &intent.options.advanced_options;
        let remove_security_ui = if advanced.disable_windows_defender
            && matches!(
                family,
                native_install_compat::WindowsFamily::Windows10
                    | native_install_compat::WindowsFamily::Windows11
            ) {
            match lr_core::sec_health_ui::stage_online_removal_script(&self.target) {
                Ok(path) => match lr_core::sec_health_ui::online_script_is_staged(&self.target) {
                    Ok(true) => {
                        log::info!(
                            "[ADVANCED_SEC_HEALTH_UI] phase=online_hook status=staged path={:?}",
                            path
                        );
                        true
                    }
                    Ok(false) => {
                        return Err(InstallBackendError::new(
                            "security_ui_script_readback_mismatch",
                            "Windows Security UI removal script readback mismatch",
                        ))
                    }
                    Err(error) => return Err(Self::error("security_ui_script_readback", error)),
                },
                Err(error) => return Err(Self::error("stage_security_ui_script", error)),
            }
        } else {
            false
        };
        let remove_curated_appx = if advanced.remove_uwp_apps
            && matches!(
                family,
                native_install_compat::WindowsFamily::Windows10
                    | native_install_compat::WindowsFamily::Windows11
            ) {
            let path = lr_core::offline_appx::stage_curated_online_removal_script(&self.target)
                .map_err(|error| Self::error("stage_curated_appx_script", error))?;
            if !lr_core::offline_appx::curated_online_script_is_staged(&self.target)
                .map_err(|error| Self::error("curated_appx_script_readback", error))?
            {
                return Err(InstallBackendError::new(
                    "curated_appx_script_readback_mismatch",
                    "preinstalled application removal script readback mismatch",
                ));
            }
            log::info!(
                "[ADVANCED_APPX] phase=online_hook status=staged path={:?}",
                path
            );
            true
        } else {
            false
        };
        let reserved_storage_support = if advanced.disable_reserved_storage {
            match target_version {
                Some((major, minor, build, _)) => {
                    match lr_core::reserved_storage::SupportedTargetVersion::new(
                        major.into(),
                        minor.into(),
                        build.into(),
                    ) {
                        Some(support) => {
                            match lr_core::reserved_storage::stage_online_disable_script(
                                &self.target,
                            ) {
                                Ok(path) => {
                                    log::info!(
                                "[ADVANCED_RESERVED_STORAGE] phase=online_hook status=staged build={} path={:?}",
                                build,
                                path
                            );
                                    Some(support)
                                }
                                Err(error) => {
                                    log::warn!(
                                "[ADVANCED_RESERVED_STORAGE] phase=online_hook status=skipped reason=script_stage_failed detail={:?}",
                                error.to_string()
                            );
                                    None
                                }
                            }
                        }
                        None => {
                            log::warn!(
                            "[ADVANCED_RESERVED_STORAGE] phase=online_hook status=skipped reason=unsupported_target_version version={}.{}.{} minimum_build=19041",
                            major,
                            minor,
                            build
                        );
                            None
                        }
                    }
                }
                None => {
                    log::warn!(
                        "[ADVANCED_RESERVED_STORAGE] phase=online_hook status=skipped reason=target_version_unconfirmed"
                    );
                    None
                }
            }
        } else {
            None
        };
        let xml = native_install_compat::render_default_unattend(&DefaultUnattendOptions {
            architecture,
            family,
            username: advanced
                .custom_username
                .then_some(advanced.username.as_str()),
            builtin_administrator: advanced
                .builtin_administrator
                .enabled
                .then_some(&advanced.builtin_administrator),
            temporary_oobe_account_name: temporary_oobe_account.as_deref(),
            remove_uwp_apps: remove_curated_appx,
            run_deploy_script: advanced.run_script_during_deploy,
            remove_security_ui,
            reserved_storage_support,
            international: international.as_ref(),
        })
        .map_err(|error| Self::error("render_default_unattend", error))?;
        std::fs::write(&destination, &xml)
            .map_err(|error| Self::error("write_default_unattend", error))?;
        let sysprep = Path::new(&self.target)
            .join("Windows")
            .join("System32")
            .join("Sysprep");
        if sysprep.is_dir() {
            if let Err(error) = std::fs::write(sysprep.join("unattend.xml"), xml) {
                log::warn!("[NATIVE INSTALL] writing Sysprep unattend failed: {error}");
            }
        }
        Ok(())
    }

    fn repair_boot(&mut self, intent: &StartInstallIntent) -> Result<(), InstallBackendError> {
        let is_xp = intent.options.is_xp || intent.options.is_xp_i386;
        let modern_boot_assets_present = Path::new(&self.target)
            .join("Windows")
            .join("Boot")
            .is_dir();
        if missing_modern_boot_assets_warning(is_xp, modern_boot_assets_present) {
            log::warn!(
                "[NATIVE INSTALL] target is validated as Vista+ but Windows\\Boot is absent; treating the directory shape as advisory and continuing to the authoritative boot-repair operation"
            );
        }

        let use_uefi = resolve_direct_install_uefi_mode_with(
            intent.options.boot_mode,
            self.target_style,
            || {
                let firmware = lr_core::windows_firmware::detect_firmware_type()
                    .map_err(|error| Self::error("detect_firmware_mode", error))?;
                Ok(matches!(
                    firmware,
                    lr_core::windows_firmware::FirmwareType::Uefi
                ))
            },
        )?;
        if intent.options.boot_mode == BootModeSelection::Auto
            && self.target_style == PartitionStyle::Unknown
        {
            log::warn!(
                "[NATIVE INSTALL] target partition style is unknown; Auto boot mode used the current firmware probe: {}",
                if use_uefi { "UEFI" } else { "Legacy" }
            );
        }

        if let Some(package) = self.pca_package.as_ref() {
            package
                .inject_into_offline_windows(Path::new(&format!("{}\\", self.target)))
                .map_err(|error| Self::error("inject_pca2023", error))?;
        }
        let manager = super::bcdedit::BootManager::new();
        if !use_uefi {
            if let Some(disk_number) = self
                .partitions
                .iter()
                .find(|partition| partition.letter.eq_ignore_ascii_case(&self.target))
                .and_then(|partition| partition.disk_number)
            {
                // A zero or unreadable MBR identity cannot be repaired safely by reopening only
                // a mutable disk number here. The checked partitioning workflow must establish a
                // stable identity first; boot repair fails closed instead of guessing a disk.
                self.ensure_mbr_signature(disk_number)?;
            }
        }
        if is_xp {
            if use_uefi {
                manager
                    .write_xp_uefi_gpt_boot(&self.target)
                    .map_err(|error| Self::error("repair_xp_uefi_boot", error))?;
            } else {
                manager
                    .write_xp_boot(&self.target)
                    .map_err(|error| Self::error("repair_xp_boot", error))?;
            }
        } else {
            manager
                .repair_boot_advanced(&self.target, use_uefi, intent.options.boot_pca_mode)
                .map_err(|error| Self::error("repair_boot", error))?;
        }

        if use_uefi && intent.options.advanced_options.win7_uefi_patch {
            let advanced = Self::legacy_advanced(intent);
            advanced
                .apply_uefiseven_patch(&self.target)
                .map_err(|error| Self::error("apply_win7_uefi_patch", error))?;
        }
        Ok(())
    }
}

impl InstallExecutionBackend for ProductionInstallBackend {
    fn execute_phase(
        &mut self,
        intent: &StartInstallIntent,
        context: &InstallExecutionContext,
        phase: InstallExecutionPhase,
        reporter: &mut dyn InstallExecutionReporter,
        cancellation: &dyn InstallCancellation,
    ) -> Result<(), InstallBackendError> {
        #[cfg(feature = "non-elevated-tests")]
        {
            let _ = (intent, context, phase, reporter, cancellation);
            Err(InstallBackendError::new(
                "development_build_denied",
                "production install backend is disabled in non-elevated development builds",
            ))
        }

        #[cfg(not(feature = "non-elevated-tests"))]
        {
            let supported = match intent.mode {
                InstallMode::Direct => Self::supports_direct_phase(phase),
                InstallMode::ViaPe => Self::supports_via_pe_phase(phase),
            };
            if !supported {
                return Err(InstallBackendError::new(
                    UNSUPPORTED_PENDING,
                    format!("phase {phase:?} does not belong to the selected install mode"),
                ));
            }
            if cancellation.is_cancelled()
                && !(intent.mode == InstallMode::ViaPe && phase.is_via_pe_commit_phase())
            {
                return Err(InstallBackendError::new(
                    "cancelled",
                    "installation cancelled",
                ));
            }
            if intent.mode == InstallMode::Direct
                && direct_phase_requires_target_revalidation(phase)
            {
                self.refresh_target(context)?;
            }
            if intent.mode == InstallMode::Direct
                && matches!(
                    phase,
                    InstallExecutionPhase::ApplyXpTextModeSource
                        | InstallExecutionPhase::ApplyGhostImage
                        | InstallExecutionPhase::ApplyWimImage
                )
            {
                self.verify_direct_source_set_unchanged(intent)?;
            }
            match phase {
                InstallExecutionPhase::InspectBitLocker => {
                    self.inspect_bitlocker_fresh(intent, context, reporter, cancellation)
                }
                InstallExecutionPhase::AwaitBitLockerDecryption => {
                    self.await_bitlocker_fallback_decryption(reporter, cancellation)
                }
                InstallExecutionPhase::VerifyPcaBeforeDiskWrite => {
                    Self::verify_storage_driver_preflight(intent)?;
                    let may_use_uefi = intent.options.boot_mode != BootModeSelection::Legacy;
                    self.pca_package = super::pca_preflight::verify_before_disk_write(
                        &intent.image_path,
                        intent.volume_index,
                        intent.is_gho,
                        intent.options.is_xp || intent.options.is_xp_i386,
                        may_use_uefi,
                        intent.options.boot_pca_mode,
                    )
                    .map_err(|error| Self::error("pca_preflight", error))?;
                    Ok(())
                }
                InstallExecutionPhase::ResolveStableTarget => self.refresh_target(context),
                InstallExecutionPhase::ResolveTargetAfterDiskpart => {
                    self.refresh_target_after_partition_scripts(context)
                }
                InstallExecutionPhase::RunDiskpartScripts => {
                    let directory = crate::utils::path::get_diskpart_scripts_dir();
                    lr_core::diskpart::run_scripts_in_dir(&directory)
                        .map(|_| ())
                        .map_err(|error| Self::error("legacy_partition_scripts_disabled", error))
                }
                InstallExecutionPhase::FormatTarget => {
                    // This guard protects every later input, even when the user deliberately keeps
                    // the existing file system. Applying an image over the volume that carries its
                    // own source or a later script/driver is just as unsafe as formatting it.
                    self.validate_direct_target_dependencies(intent, context)?;
                    if !intent.options.format_partition {
                        return Ok(());
                    }
                    self.format_target_compat(intent, context)
                }
                InstallExecutionPhase::ExportHostDrivers => {
                    if self.driver_backup.exists() {
                        std::fs::remove_dir_all(&self.driver_backup)
                            .map_err(|error| Self::error("clear_driver_backup", error))?;
                    }
                    let dism = super::dism::Dism::new();
                    if dism.is_pe_environment() {
                        dism.export_drivers_from_system(
                            &format!("{}\\", self.target),
                            &self.driver_backup.to_string_lossy(),
                        )
                    } else if intent.options.driver_action == DriverAction::AutoImport {
                        dism.export_drivers_for_automatic_restore(
                            &self.driver_backup.to_string_lossy(),
                        )
                    } else {
                        dism.export_drivers(&self.driver_backup.to_string_lossy())
                    }
                    .map(|_| ())
                    .map_err(|error| Self::error("export_host_drivers", error))
                }
                InstallExecutionPhase::ApplyXpTextModeSource => {
                    self.deactivate_xp_sibling_partitions()?;
                    let custom = (!intent.options.custom_unattend_path.trim().is_empty())
                        .then(|| Path::new(&intent.options.custom_unattend_path));
                    let locked = self.direct_xp_source_lock.as_ref().ok_or_else(|| {
                        InstallBackendError::new(
                            "direct_xp_source_lock_missing",
                            "XP apply reached without its verification tree manifest",
                        )
                    })?;
                    lr_core::xp_i386::install_from_i386_locked(
                        locked,
                        &self.target,
                        &crate::utils::path::get_bin_dir(),
                        custom,
                    )
                    .map(|_| ())
                    .map_err(|error| Self::error("apply_xp_i386", error))
                }
                InstallExecutionPhase::ApplyGhostImage => {
                    self.apply_ghost(intent, reporter, cancellation)
                }
                InstallExecutionPhase::ApplyWimImage => {
                    self.apply_wim(intent, reporter, cancellation)
                }
                InstallExecutionPhase::ProcessDrivers => self.process_drivers(intent),
                InstallExecutionPhase::RepairBoot => self.repair_boot(intent),
                InstallExecutionPhase::StageDirectPreinstalledSoftware => {
                    // Desktop Windows copies the installers downloaded before destructive work.
                    // When the normal endpoint runs in WinPE, this phase downloads them directly
                    // into the already applied target instead of wasting RAM-disk space on X:.
                    self.stage_preinstalled_software_for_direct(intent, reporter, cancellation)
                }
                InstallExecutionPhase::ApplyAdvancedOptions => {
                    let advanced = Self::legacy_advanced(intent);
                    let is_nt5 = intent.options.is_xp || intent.options.is_xp_i386;
                    let advanced_requested = validate_direct_advanced_request(&advanced, is_nt5)
                        .map_err(|error| Self::error("invalid_advanced_option", error))?;
                    if let Err(error) = run_requested_direct_operation(advanced_requested, || {
                        advanced.apply_to_system(&self.target, is_nt5)
                    }) {
                        // The target image and boot files already exist at this phase. Advanced
                        // customizations are optional and must not turn a usable installation into
                        // a failed one. Preserve the exact cause as a warning and continue.
                        log::warn!(
                            "[ADVANCED] status=warning detail={error:#}; optional advanced options were not fully applied; installation continues"
                        );
                    }
                    if advanced.disable_windows_defender && !intent.options.unattended_install {
                        return Err(InstallBackendError::new(
                            "required_security_ui_hook_unavailable",
                            "Windows Security UI removal requires unattended installation",
                        ));
                    }
                    if advanced.disable_reserved_storage && !intent.options.unattended_install {
                        log::warn!(
                            "[ADVANCED_RESERVED_STORAGE] phase=online_hook status=skipped reason=unattended_install_disabled"
                        );
                    }
                    if advanced.remove_uwp_apps && !intent.options.unattended_install {
                        return Err(InstallBackendError::new(
                            "required_appx_hook_unavailable",
                            "preinstalled application removal requires unattended installation",
                        ));
                    }
                    self.inject_versioned_user_drivers(is_nt5)?;
                    if intent.options.unattended_install {
                        self.write_unattend(intent)?;
                    }
                    Ok(())
                }
                InstallExecutionPhase::FinishDirectInstall => Ok(()),
                InstallExecutionPhase::VerifyPeEnvironment => self.verify_pe_environment(intent),
                InstallExecutionPhase::InstallPeBootEntry => self.install_pe_boot_entry(),
                InstallExecutionPhase::SelectDataPartition => self.select_data_partition(intent),
                InstallExecutionPhase::PersistPcaCompatibilityPackage => self.persist_pca_package(),
                InstallExecutionPhase::ExportDriversToPeData => {
                    let destination = Path::new(&self.data_dir()?).join("drivers");
                    if destination.exists() {
                        std::fs::remove_dir_all(&destination)
                            .map_err(|error| Self::error("clear_pe_driver_backup", error))?;
                    }
                    let dism = super::dism::Dism::new();
                    let result = if intent.options.driver_action == DriverAction::AutoImport {
                        dism.export_drivers_for_automatic_restore(&destination.to_string_lossy())
                    } else {
                        dism.export_drivers(&destination.to_string_lossy())
                    };
                    result.map_err(|error| Self::error("export_drivers_to_pe_data", error))?;
                    #[cfg(feature = "ci-automation")]
                    if let Some(run_id) = ci_existing_target_driver_scenario_run_id() {
                        stage_ci_existing_target_driver_fixture(&destination, &run_id)
                            .map_err(|error| Self::error("stage_ci_driver_fixture", error))?;
                    }
                    let actual = lr_core::driver::measure_plain_tree_logical_bytes(&destination)
                        .map_err(|error| Self::error("measure_exported_drivers", error))?;
                    let current_free = DiskManager::current_directory_free_bytes(&destination)
                        .map_err(|error| Self::error("recheck_driver_export_capacity", error))?;
                    let reconciled_budget =
                        self.staging_payload_budget.as_mut().ok_or_else(|| {
                            InstallBackendError::new(
                                "staging_budget_missing",
                                "data capacity budget is missing before driver export",
                            )
                        })?;
                    let (planned, remaining_required) =
                        reconcile_exported_driver_budget(reconciled_budget, actual, current_free)?;
                    #[cfg(feature = "ci-automation")]
                    let reconciled_budget = *reconciled_budget;
                    #[cfg(feature = "ci-automation")]
                    if let Some(run_id) = ci_existing_target_driver_scenario_run_id() {
                        write_ci_driver_budget_receipt(
                            &run_id,
                            planned,
                            actual,
                            current_free,
                            remaining_required,
                            reconciled_budget,
                        )
                        .map_err(|error| Self::error("write_ci_driver_budget_receipt", error))?;
                    }
                    if actual != planned {
                        log::warn!(
                            "[DATA CAPACITY] reconciled Driver Store preflight to authoritative DISM export: planned={}, actual={}, current_free={}, remaining_required={}",
                            planned,
                            actual,
                            current_free,
                            remaining_required
                        );
                    }
                    Ok(())
                }
                InstallExecutionPhase::VerifySourceImage => {
                    self.verify_source_image(intent, reporter, cancellation)
                }
                InstallExecutionPhase::PreparePreinstalledSoftware => {
                    self.prepare_preinstalled_software(intent, reporter, cancellation)
                }
                InstallExecutionPhase::CopySourceImage => {
                    self.verify_late_payloads_fit_plan(intent)?;
                    self.copy_source_image(intent, reporter, cancellation)?;
                    self.verify_staged_source_payload_size(intent)
                }
                InstallExecutionPhase::StagePreinstalledSoftware => {
                    self.stage_preinstalled_software_for_pe(intent)
                }
                InstallExecutionPhase::StageUefiSeven => self.stage_uefiseven(),
                InstallExecutionPhase::StageUserDrivers => self.stage_user_drivers(),
                InstallExecutionPhase::WritePeInstallConfig => self.write_pe_install_config(intent),
                // Deliberately does not call shutdown/reboot. The UI owns the
                // explicit user confirmation after ReadyToReboot is reported.
                InstallExecutionPhase::ReadyToRebootIntoPe => self.commit_pe_handoff(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::native_install_controller::{InstallOptions, StartInstallIntent};
    use crate::core::ui_state::AdvancedOptionsData;
    use lr_core::boot_pca::BootPcaMode;

    #[test]
    fn authenticated_verification_receipt_is_limited_to_single_file_wim_family() {
        assert!(
            ProductionInstallBackend::supports_authenticated_verify_receipt(Path::new(
                r"D:\install.WIM"
            ))
        );
        assert!(
            ProductionInstallBackend::supports_authenticated_verify_receipt(Path::new(
                r"D:\install.esd"
            ))
        );
        assert!(
            !ProductionInstallBackend::supports_authenticated_verify_receipt(Path::new(
                r"D:\install.swm"
            ))
        );
        assert!(
            !ProductionInstallBackend::supports_authenticated_verify_receipt(Path::new(
                r"D:\backup.gho"
            ))
        );
    }

    #[test]
    fn cached_valid_result_cannot_issue_a_handoff_receipt() {
        let cached_valid = super::super::image_verify::VerifyResult {
            status: super::super::image_verify::VerifyStatus::Valid,
            full_wimlib_verification_performed: false,
            ..super::super::image_verify::VerifyResult::default()
        };
        assert!(!ProductionInstallBackend::verification_result_can_issue_receipt(&cached_valid));

        let fresh_full = super::super::image_verify::VerifyResult {
            full_wimlib_verification_performed: true,
            ..cached_valid
        };
        assert!(ProductionInstallBackend::verification_result_can_issue_receipt(&fresh_full));
    }

    #[test]
    fn receipt_requires_exact_manifest_identity_and_legal_format() {
        let identity = lr_core::install_source_lock::LockedSourceArtifactIdentity {
            path: PathBuf::from(r"D:\LetRecovery_Data\install.wim"),
            length_bytes: 123,
            sha256: [7; 32],
        };
        let receipt = VerifiedStagedImageReceipt {
            identity: identity.clone(),
        };
        let config = super::super::install_config::InstallConfig {
            image_path: "install.wim".into(),
            ..super::super::install_config::InstallConfig::default()
        };
        assert!(
            ProductionInstallBackend::receipt_matches_manifest_identities(
                Some(&receipt),
                Path::new(r"D:\LetRecovery_Data\install.wim"),
                &config,
                std::slice::from_ref(&identity),
            )
            .unwrap()
        );

        let mut changed = identity.clone();
        changed.sha256[0] ^= 1;
        assert!(
            ProductionInstallBackend::receipt_matches_manifest_identities(
                Some(&receipt),
                Path::new(r"D:\LetRecovery_Data\install.wim"),
                &config,
                &[changed],
            )
            .is_err()
        );
        assert!(
            ProductionInstallBackend::receipt_matches_manifest_identities(
                Some(&receipt),
                Path::new(r"D:\LetRecovery_Data\install.swm"),
                &config,
                std::slice::from_ref(&identity),
            )
            .is_err()
        );
    }

    #[cfg(windows)]
    #[test]
    fn publication_tamper_window_cannot_create_a_receipt() {
        let temp = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-published-image-receipt-tamper",
        )
        .expect("create temp directory");
        let image = temp.path().join("install.wim");
        let expected = b"copy-stream bytes";
        std::fs::write(&image, b"bytes changed before publication lock")
            .expect("write tampered publication");
        let expected_sha256 = lr_core::hash::sha256_bytes(expected);
        let mut backend = ProductionInstallBackend::new(&intent(InstallMode::ViaPe));

        let error = backend
            .lock_published_verified_image(
                &image,
                expected.len() as u64,
                &expected_sha256,
                &expected_sha256,
            )
            .expect_err("changed bytes must not produce a verification receipt");
        assert_eq!(error.code, "published_verified_image_identity_mismatch");
        assert!(backend.staged_source_image_receipt.is_none());
        assert!(backend.pe_source_lock.is_none());
    }

    #[cfg(feature = "ci-automation")]
    #[test]
    fn ci_driver_fixture_selects_a_deterministic_real_storage_controller_id() {
        let device = |instance_id: &str, class: &str, hardware_id: &str| {
            lr_core::driver::StoragePathDevice {
                instance_id: instance_id.to_owned(),
                description: instance_id.to_owned(),
                device_class: class.to_owned(),
                class_guid: String::new(),
                hardware_ids: vec![hardware_id.to_owned()],
                compatible_ids: Vec::new(),
                bound_inf: Some("storvsc.inf".to_owned()),
            }
        };
        let selected = select_ci_storage_fixture_device(vec![
            device("z-controller", "SCSIAdapter", "VMBUS\\{BBBB-BBBB}"),
            device("unrelated", "Net", "ROOT\\UNRELATED"),
            device("a-controller", "SCSIAdapter", "VMBUS\\{AAAA-AAAA}"),
            device("bad-id", "SCSIAdapter", "PCI\\VEN_1234,%Unsafe%"),
        ])
        .expect("one safe storage controller should be selected");
        assert_eq!(selected.0.instance_id, "a-controller");
        assert_eq!(selected.1, "VMBUS\\{AAAA-AAAA}");
    }

    #[test]
    fn single_source_dual_boot_adds_staging_to_the_same_shrink_plan() {
        let gib = lr_core::custom_install::GIB;
        let source_offset = 1_048_576_u64;
        let source_length = 500 * gib;
        let windows_length = 100 * gib;
        let request = lr_core::custom_install::DualBootPlan {
            source_drive_letter: 'C',
            source_offset_bytes: source_offset,
            source_length_before_bytes: source_length,
            source_length_after_bytes: source_length - windows_length,
            target_offset_bytes: source_offset + source_length - windows_length,
            target_length_bytes: windows_length,
            data_offset_bytes: None,
            data_length_bytes: 0,
        };
        // Preserve the exact payload + 2 GiB minimum; it need not be an integral MiB because the
        // provider's actual create/readback is authoritative.
        let staging = 18 * gib + 12_345;
        let combined = dual_boot_plan_with_staging(&request, staging).unwrap();
        assert_eq!(combined.data_length_bytes, staging);
        assert_eq!(
            combined.source_length_after_bytes,
            source_length - windows_length - staging
        );
        assert_eq!(
            combined.target_offset_bytes,
            source_offset + combined.source_length_after_bytes
        );
        assert_eq!(
            combined.data_offset_bytes,
            Some(combined.target_offset_bytes + windows_length)
        );
        lr_core::custom_install::validate_dual_boot_plan(&combined).unwrap();
    }

    #[test]
    fn swm_volume_enumeration_requires_a_contiguous_primary_led_set() {
        let directory = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-swm-volume-set",
        )
        .unwrap();
        for name in ["install.swm", "install2.swm", "install3.swm"] {
            std::fs::write(directory.path().join(name), name.as_bytes()).unwrap();
        }

        let set = enumerate_staged_image_set(&directory.path().join("install.swm")).unwrap();
        assert_eq!(set.kind, StagedImageSetKind::Swm);
        assert_eq!(set.main_name, "install.swm");
        assert_eq!(set.volumes.len(), 3);
        assert!(set.volumes[0].ends_with("install.swm"));
        assert!(set.volumes[1].ends_with("install2.swm"));
        assert!(set.volumes[2].ends_with("install3.swm"));

        std::fs::remove_file(directory.path().join("install2.swm")).unwrap();
        assert!(enumerate_staged_image_set(&directory.path().join("install.swm")).is_err());
    }

    #[test]
    fn ghost_volume_enumeration_uses_primary_gho_and_contiguous_ghs_spans() {
        let directory = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-ghost-volume-set",
        )
        .unwrap();
        for name in ["system.gho", "system001.ghs", "system002.ghs"] {
            std::fs::write(directory.path().join(name), name.as_bytes()).unwrap();
        }

        let set = enumerate_staged_image_set(&directory.path().join("system.gho")).unwrap();
        assert_eq!(set.kind, StagedImageSetKind::Ghost);
        assert_eq!(set.volumes.len(), 3);
        assert!(set.volumes[1].ends_with("system001.ghs"));
        assert!(enumerate_staged_image_set(&directory.path().join("system001.ghs")).is_err());

        std::fs::remove_file(directory.path().join("system001.ghs")).unwrap();
        assert!(enumerate_staged_image_set(&directory.path().join("system.gho")).is_err());
    }

    #[test]
    fn every_direct_target_write_phase_requires_fresh_identity() {
        for phase in [
            InstallExecutionPhase::FormatTarget,
            InstallExecutionPhase::ApplyXpTextModeSource,
            InstallExecutionPhase::ApplyGhostImage,
            InstallExecutionPhase::ApplyWimImage,
            InstallExecutionPhase::ProcessDrivers,
            InstallExecutionPhase::RepairBoot,
            InstallExecutionPhase::ApplyAdvancedOptions,
            InstallExecutionPhase::FinishDirectInstall,
        ] {
            assert!(
                direct_phase_requires_target_revalidation(phase),
                "{phase:?}"
            );
        }
        assert!(!direct_phase_requires_target_revalidation(
            InstallExecutionPhase::VerifySourceImage
        ));
    }

    #[test]
    fn raw_unc_dependencies_fail_closed_but_mapped_drives_keep_their_identity() {
        assert_eq!(
            dependency_drive_letter(
                Path::new(r"\\server\share\install.wim"),
                Path::new(r"\\server\share\install.wim")
            ),
            None
        );
        assert_eq!(
            dependency_drive_letter(
                Path::new(r"Z:\install.wim"),
                Path::new(r"\\?\UNC\server\share\install.wim")
            ),
            Some('Z')
        );
        assert!(dependency_kind_may_lack_local_extent(
            lr_core::windows_storage::DriveKind::RamDisk
        ));
        assert!(dependency_kind_may_lack_local_extent(
            lr_core::windows_storage::DriveKind::Optical
        ));
        assert!(!dependency_kind_may_lack_local_extent(
            lr_core::windows_storage::DriveKind::Remote
        ));
    }

    #[test]
    fn fused_verification_is_limited_to_single_file_wim_formats() {
        assert!(ProductionInstallBackend::supports_fused_verify_copy(
            Path::new("install.wim")
        ));
        assert!(ProductionInstallBackend::supports_fused_verify_copy(
            Path::new("INSTALL.ESD")
        ));
        assert!(!ProductionInstallBackend::supports_fused_verify_copy(
            Path::new("install.swm")
        ));
        assert!(!ProductionInstallBackend::supports_fused_verify_copy(
            Path::new("system.gho")
        ));
    }

    #[cfg(windows)]
    #[test]
    fn fused_source_lock_denies_writers_until_released() {
        let temp = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-fused-source-lock-test",
        )
        .expect("create temp directory");
        let image = temp.path().join("install.wim");
        std::fs::write(&image, b"test image bytes").expect("write test image");

        let locked =
            ProductionInstallBackend::open_locked_source(&image).expect("lock source image");
        let write_while_locked = OpenOptions::new().write(true).open(&image);
        assert!(
            write_while_locked.is_err(),
            "a writer must not open while verification and copying share the source"
        );

        drop(locked);
        OpenOptions::new()
            .write(true)
            .open(&image)
            .expect("writer should open after the source lock is released");
    }

    #[cfg(windows)]
    #[test]
    fn fused_copy_publishes_only_an_exact_fully_verified_wim() {
        let temp = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-fused-copy-test",
        )
        .expect("create temp directory");
        let capture_source = temp.path().join("capture-source");
        std::fs::create_dir_all(&capture_source).expect("create capture source");
        std::fs::write(
            capture_source.join("payload.bin"),
            vec![0x5a_u8; 2 * 1024 * 1024],
        )
        .expect("write capture payload");
        let source_wim = temp.path().join("source.wim");
        let manager = lr_core::wimlib::WimlibManager::new().expect("load embedded wimlib");
        manager
            .capture_image(
                &capture_source.to_string_lossy(),
                &source_wim.to_string_lossy(),
                "test",
                "fused verification test",
                0,
                None,
            )
            .expect("capture test WIM");

        let data_dir = Path::new(
            &super::super::install_config::ConfigFileManager::get_data_dir(
                &temp.path().to_string_lossy(),
            ),
        )
        .to_path_buf();
        std::fs::create_dir_all(&data_dir).expect("create staged data directory");
        let destination = data_dir.join("source.wim");
        let mut install_intent = intent(InstallMode::ViaPe);
        install_intent.image_path = source_wim.to_string_lossy().into_owned();
        let mut backend = ProductionInstallBackend::new(&install_intent);
        backend.data_partition = Some(temp.path().to_string_lossy().into_owned());
        let mut reporter = |_event: InstallExecutionEvent| {};
        let cancellation = || false;

        backend
            .verify_source_image(&install_intent, &mut reporter, &cancellation)
            .expect("defer full verification into copy phase");
        backend
            .copy_source_image(&install_intent, &mut reporter, &cancellation)
            .expect("copy and verify valid WIM");
        assert!(backend.staged_source_image_receipt.is_some());
        assert_eq!(
            std::fs::read(&source_wim).expect("read source WIM"),
            std::fs::read(&destination).expect("read staged WIM")
        );

        backend.pe_source_lock = None;
        backend.staged_source_image_receipt = None;
        let mut same_file_intent = install_intent.clone();
        same_file_intent.image_path = destination.to_string_lossy().into_owned();
        let mut same_file_backend = ProductionInstallBackend::new(&same_file_intent);
        same_file_backend.data_partition = Some(temp.path().to_string_lossy().into_owned());
        same_file_backend
            .copy_source_image(&same_file_intent, &mut reporter, &cancellation)
            .expect("verify an already-staged source without issuing a receipt");
        assert!(same_file_backend.staged_source_image_receipt.is_none());
        same_file_backend.pe_source_lock = None;

        std::fs::remove_file(&destination).expect("remove first staged result");
        let original_len = std::fs::metadata(&source_wim)
            .expect("inspect source WIM")
            .len();
        OpenOptions::new()
            .write(true)
            .open(&source_wim)
            .expect("open source WIM for corruption")
            .set_len(original_len / 2)
            .expect("truncate source WIM");
        let error = backend
            .copy_source_image(&install_intent, &mut reporter, &cancellation)
            .expect_err("a truncated WIM must not be published");
        assert_eq!(error.code, "source_image_verification_failed");
        assert!(!destination.exists());
    }

    fn intent(mode: InstallMode) -> StartInstallIntent {
        StartInstallIntent {
            mode,
            running_in_pe: false,
            target_partition: "E:".into(),
            target_disk_number: 1,
            target_partition_number: 2,
            target_disk_size_bytes: 1_000_000_000_000,
            target_partition_offset_bytes: 1_048_576,
            target_partition_size_bytes: 500_000_000_000,
            target_stable_identity: lr_core::windows_storage::StableVolumeIdentity {
                extent: lr_core::windows_storage::VolumeIdentity {
                    disk_number: 1,
                    offset_bytes: 1_048_576,
                    extent_length_bytes: 500_000_000_000,
                },
                disk: lr_core::windows_storage::StableDiskIdentity::Gpt { disk_id: [1; 16] },
                partition: lr_core::windows_storage::StablePartitionIdentity::Gpt {
                    partition_id: [2; 16],
                },
                device_id_hash: Some([3; 32]),
            },
            image_path: "D:\\install.wim".into(),
            image_backing_path: String::new(),
            volume_index: 1,
            is_system_partition: false,
            pe_index: None,
            is_gho: false,
            options: InstallOptions {
                format_partition: false,
                repair_boot: false,
                unattended_install: false,
                export_drivers: false,
                auto_reboot: false,
                automation_shutdown_on_terminal: false,
                boot_mode: BootModeSelection::Auto,
                boot_pca_mode: BootPcaMode::Auto,
                advanced_options: AdvancedOptionsData::default(),
                driver_action: DriverAction::None,
                custom_unattend_path: String::new(),
                is_xp: false,
                is_xp_i386: false,
                run_diskpart_scripts: false,
                custom_install_plan: lr_core::custom_install::CustomInstallPlan::default(),
            },
        }
    }

    #[test]
    fn direct_wim_verification_is_not_deferred_to_a_copy_phase_that_does_not_exist() {
        assert!(
            !ProductionInstallBackend::source_verification_is_deferred_to_copy(&intent(
                InstallMode::Direct
            ))
        );
        assert!(
            ProductionInstallBackend::source_verification_is_deferred_to_copy(&intent(
                InstallMode::ViaPe
            ))
        );
    }

    #[test]
    fn mounted_optical_source_keeps_its_backing_iso_as_a_format_dependency() {
        let mut value = intent(InstallMode::Direct);
        value.target_partition = "F:".into();
        value.image_path = r"E:\sources\install.wim".into();
        value.image_backing_path = r"F:\images\windows.iso".into();

        let dependencies = image_format_dependencies(&value);
        assert_eq!(dependencies.len(), 2);
        assert_eq!(dependencies[0].1, Path::new(r"E:\sources\install.wim"));
        assert_eq!(dependencies[1].0, "backing ISO image");
        assert_eq!(dependencies[1].1, Path::new(r"F:\images\windows.iso"));
        assert_eq!(
            lr_core::windows_storage::path_drive_letter(dependencies[1].1),
            value.target_partition.chars().next()
        );
    }

    #[test]
    fn advanced_state_round_trips_to_established_business_type() {
        let mut value = intent(InstallMode::Direct);
        value.options.advanced_options.disable_uac = true;
        value.options.advanced_options.username = "LetRecovery".into();
        value.options.advanced_options.migrate_wifi = true;
        value.options.advanced_options.wifi_ssid = "Test Wi-Fi".into();
        value.options.advanced_options.wifi_profile_xml = "<WLANProfile />".into();
        let converted = ProductionInstallBackend::legacy_advanced(&value);
        assert!(converted.disable_uac);
        assert_eq!(converted.username, "LetRecovery");
        assert!(converted.migrate_wifi);
        assert_eq!(converted.wifi_ssid, "Test Wi-Fi");
        assert_eq!(converted.wifi_profile_xml, "<WLANProfile />");
    }

    #[test]
    fn automatic_driver_restore_accepts_only_a_verified_empty_manifest() {
        let temporary = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-empty-driver-export",
        )
        .expect("temporary driver export");

        assert!(automatic_driver_export_has_payload(temporary.path()).is_err());
        lr_core::driver::write_storage_driver_requirements(temporary.path(), &[])
            .expect("empty manifest");
        assert!(!automatic_driver_export_has_payload(temporary.path()).unwrap());

        std::fs::write(temporary.path().join("oem1.inf"), b"[Version]\r\n").expect("driver INF");
        assert!(automatic_driver_export_has_payload(temporary.path()).unwrap());
    }

    #[test]
    fn via_pe_plan_is_fully_dispatched_and_never_contains_reboot_io() {
        use crate::core::native_install_executor::NativeInstallExecutor;

        let plan = NativeInstallExecutor::build_plan(
            &intent(InstallMode::ViaPe),
            &InstallExecutionContext::default(),
        )
        .expect("ViaPE plan");
        assert!(plan
            .iter()
            .copied()
            .all(ProductionInstallBackend::supports_via_pe_phase));
        assert_eq!(
            plan.last(),
            Some(&InstallExecutionPhase::ReadyToRebootIntoPe)
        );
        assert!(!plan.contains(&InstallExecutionPhase::FinishDirectInstall));
    }

    #[test]
    fn missing_pe_is_returned_as_download_preparation_boundary() {
        let error = ProductionInstallBackend::require_cached_pe(
            CachedArtifactStatus::Missing,
            "LetRecovery_PE.wim",
        )
        .expect_err("missing PE must not be accepted");
        assert_eq!(error.code, "pe_download_required");
        assert!(error.detail.contains("LetRecovery_PE.wim"));
    }

    #[test]
    fn every_direct_executor_phase_has_a_production_dispatch_branch() {
        use crate::core::native_install_executor::{
            BitLockerRequirement, NativeInstallExecutor, StableTargetIdentity,
        };

        let context = InstallExecutionContext {
            stable_target: Some(StableTargetIdentity {
                disk_number: 2,
                partition_number: 3,
                disk_size_bytes: 2_000_000_000_000,
                partition_offset_bytes: 1_048_576,
                partition_size_bytes: 1_000_000_000_000,
                stable_volume: lr_core::windows_storage::StableVolumeIdentity {
                    extent: lr_core::windows_storage::VolumeIdentity {
                        disk_number: 2,
                        offset_bytes: 1_048_576,
                        extent_length_bytes: 1_000_000_000_000,
                    },
                    disk: lr_core::windows_storage::StableDiskIdentity::Gpt { disk_id: [1; 16] },
                    partition: lr_core::windows_storage::StablePartitionIdentity::Gpt {
                        partition_id: [2; 16],
                    },
                    device_id_hash: Some([3; 32]),
                },
            }),
            bitlocker: BitLockerRequirement::Ready,
        };
        let plan = NativeInstallExecutor::build_plan(&intent(InstallMode::Direct), &context)
            .expect("direct plan");
        assert!(plan
            .into_iter()
            .all(ProductionInstallBackend::supports_direct_phase));
        assert!(!ProductionInstallBackend::supports_direct_phase(
            InstallExecutionPhase::InstallPeBootEntry
        ));
    }

    #[test]
    fn direct_format_stage_validates_the_custom_label_for_winapi() {
        let mut value = intent(InstallMode::Direct);
        value.options.format_partition = true;
        value.options.advanced_options.custom_volume_label = true;
        value.options.advanced_options.volume_label = "Windows 11".into();
        let plan = ProductionInstallBackend::format_plan_for_intent("E:", &value).unwrap();
        assert_eq!(plan.drive, "E:");
        assert_eq!(plan.volume_label, "Windows 11");
    }

    #[test]
    fn software_download_progress_uses_length_only_for_smooth_display() {
        assert_eq!(software_download_progress(0, 1, 0, Some(1_000)), 0);
        assert_eq!(software_download_progress(0, 1, 500, Some(1_000)), 50);
        assert_eq!(software_download_progress(0, 1, 1_000, Some(1_000)), 99);
        assert_eq!(software_download_progress(1, 2, 0, None), 50);
        assert_eq!(software_download_progress(1, 2, 1_000, Some(1)), 99);
        assert_eq!(software_download_progress(0, 1, u64::MAX, Some(1)), 99);
    }

    #[test]
    fn software_download_retries_and_isolates_one_package_failure() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback test server");
        let address = listener.local_addr().expect("read loopback address");
        let server = std::thread::spawn(move || {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().expect("accept package request");
                let mut request = [0_u8; 2048];
                let count = stream.read(&mut request).expect("read package request");
                let request = String::from_utf8_lossy(&request[..count]);
                let response = if request.contains("GET /ok.exe ") {
                    "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                } else {
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                };
                stream
                    .write_all(response.as_bytes())
                    .expect("write package response");
            }
        });
        let packages = vec![
            lr_core::software_install::SelectedSoftwarePackage {
                id: "unavailable".into(),
                name: "Unavailable".into(),
                download_url: format!("http://{address}/missing.exe"),
                filename: "missing.exe".into(),
                silent_command: "\"{installer}\" /S".into(),
                requires_admin: true,
            },
            lr_core::software_install::SelectedSoftwarePackage {
                id: "available".into(),
                name: "Available".into(),
                download_url: format!("http://{address}/ok.exe"),
                filename: "ok.exe".into(),
                silent_command: "\"{installer}\" /S".into(),
                requires_admin: true,
            },
        ];
        let destination = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-software-download-isolation",
        )
        .expect("create software test directory");
        let mut events = Vec::new();
        let mut reporter = |event| events.push(event);
        let cancellation = || false;

        let batch = ProductionInstallBackend::download_software_packages(
            destination.path(),
            &packages,
            InstallExecutionPhase::StageDirectPreinstalledSoftware,
            &mut reporter,
            &cancellation,
        )
        .expect("one failed package must not abort the remaining package");
        server.join().expect("join package test server");

        assert_eq!(batch.total_bytes, 2);
        assert_eq!(batch.packages, vec![packages[1].clone()]);
        assert_eq!(batch.failures.len(), 1);
        assert!(batch.failures[0].contains("unavailable"));
        assert!(!destination.path().join("missing.exe").exists());
        assert_eq!(
            std::fs::read(destination.path().join("ok.exe")).expect("read successful package"),
            b"ok"
        );
        assert!(events.iter().any(|event| matches!(
            event,
            InstallExecutionEvent::Progress { detail, .. }
                if detail.contains("成功 1 个，失败 1 个")
        )));
    }

    #[test]
    fn all_software_download_failures_leave_an_empty_nonfatal_plan() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback test server");
        let address = listener.local_addr().expect("read loopback address");
        let server = std::thread::spawn(move || {
            for _ in 0..PREINSTALLED_SOFTWARE_DOWNLOAD_ATTEMPTS {
                let (mut stream, _) = listener.accept().expect("accept package request");
                let mut request = [0_u8; 2048];
                let request_bytes = stream.read(&mut request).expect("read package request");
                assert!(request_bytes > 0, "package request must not be empty");
                stream
                    .write_all(
                        b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .expect("write package response");
            }
        });
        let package = lr_core::software_install::SelectedSoftwarePackage {
            id: "unavailable".into(),
            name: "Unavailable".into(),
            download_url: format!("http://{address}/missing.exe"),
            filename: "missing.exe".into(),
            silent_command: "\"{installer}\" /S".into(),
            requires_admin: true,
        };
        let mut install_intent = intent(InstallMode::ViaPe);
        install_intent.options.unattended_install = true;
        install_intent
            .options
            .advanced_options
            .preinstalled_software
            .push(package);
        let mut backend = ProductionInstallBackend::new(&install_intent);
        let mut events = Vec::new();
        let mut reporter = |event| events.push(event);
        let cancellation = || false;

        backend
            .prepare_preinstalled_software(&install_intent, &mut reporter, &cancellation)
            .expect("all package-host failures must remain nonfatal");
        server.join().expect("join package test server");

        assert_eq!(backend.prepared_software_bytes, 0);
        assert_eq!(backend.prepared_software_packages, Some(Vec::new()));
        assert!(backend
            .prepared_software_directory
            .as_ref()
            .is_some_and(|directory| directory.path().is_dir()));
        let staged = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-empty-software-stage",
        )
        .expect("create empty software staging root");
        let destination = staged.path().join("preinstalled_software");
        std::fs::create_dir_all(&destination).expect("create stale empty staging directory");
        assert_eq!(
            backend
                .copy_prepared_software_to(&destination, &[])
                .expect("an empty optional result must stage successfully"),
            0
        );
        assert!(
            !destination.exists(),
            "an empty optional result must not leave a PE artifact directory"
        );
        assert!(events.iter().any(|event| matches!(
            event,
            InstallExecutionEvent::Progress { detail, .. }
                if detail.contains("成功 0 个，失败 1 个")
        )));
    }

    #[test]
    fn empty_optional_auxiliary_tree_is_omitted_from_handoff() {
        let root = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-empty-auxiliary-tree",
        )
        .expect("create empty auxiliary tree");
        let lock = lr_core::install_source_lock::LockedInstallTree::acquire(root.path())
            .expect("lock empty optional tree");

        assert!(
            capture_nonempty_auxiliary_tree(lock)
                .expect("inspect empty optional tree")
                .is_none(),
            "empty optional trees must not become manifest artifacts or fatal errors"
        );
    }

    #[test]
    fn zero_byte_file_remains_an_authenticated_auxiliary_tree_member() {
        let root = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-zero-byte-auxiliary-tree",
        )
        .expect("create auxiliary tree");
        std::fs::write(root.path().join("empty.dll"), []).expect("write empty ordinary file");
        let lock = lr_core::install_source_lock::LockedInstallTree::acquire(root.path())
            .expect("lock auxiliary tree");
        let (_, artifacts) = capture_nonempty_auxiliary_tree(lock)
            .expect("capture auxiliary tree")
            .expect("a zero-byte file is still a real artifact");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].length_bytes, 0);
    }

    #[test]
    fn disabled_driver_task_never_includes_a_stale_driver_tree() {
        assert!(!should_include_preserved_driver_tree(false, 0, false));
        assert!(!should_include_preserved_driver_tree(false, 2, true));
        assert!(!should_include_preserved_driver_tree(true, 0, false));
        assert!(should_include_preserved_driver_tree(true, 1, false));
        assert!(should_include_preserved_driver_tree(true, 2, true));
    }

    #[test]
    fn dism_export_rebases_one_budget_and_preserves_full_headroom() {
        let gib = 1024_u64 * 1024 * 1024;
        let mut budget = StagingPayloadBudget {
            image_bytes: 5 * gib,
            exported_driver_bytes: 3 * gib,
            pca_bytes: 100,
            user_driver_bytes: 200,
            uefiseven_bytes: 300,
            preinstalled_software_bytes: 400,
        };
        let actual = 3 * gib + 263_055_629;
        let expected_remaining = 7 * gib + 900;
        let (planned, remaining) =
            reconcile_exported_driver_budget(&mut budget, actual, expected_remaining).unwrap();
        assert_eq!(planned, 3 * gib);
        assert_eq!(remaining, expected_remaining);
        assert_eq!(budget.exported_driver_bytes, actual);

        let mut insufficient = budget;
        let before_failure = insufficient;
        let error =
            reconcile_exported_driver_budget(&mut insufficient, actual, expected_remaining - 1)
                .unwrap_err();
        assert_eq!(error.code, "staging_capacity_after_driver_export");
        assert_eq!(insufficient, before_failure);
    }

    #[test]
    fn dism_export_reconciliation_covers_smaller_equal_zero_and_overflow_edges() {
        let base = StagingPayloadBudget {
            image_bytes: 10,
            exported_driver_bytes: 100,
            pca_bytes: 20,
            user_driver_bytes: 30,
            uefiseven_bytes: 40,
            preinstalled_software_bytes: 50,
        };
        let remaining = STAGING_OPERATIONAL_HEADROOM_BYTES + 10 + 30 + 40 + 50;
        for actual in [50_u64, 100] {
            let mut budget = base;
            let (planned, observed_remaining) =
                reconcile_exported_driver_budget(&mut budget, actual, remaining).unwrap();
            assert_eq!(planned, 100);
            assert_eq!(observed_remaining, remaining);
            assert_eq!(budget.exported_driver_bytes, actual);
        }

        let mut zero = StagingPayloadBudget::default();
        assert_eq!(
            reconcile_exported_driver_budget(&mut zero, 0, STAGING_OPERATIONAL_HEADROOM_BYTES)
                .unwrap(),
            (0, STAGING_OPERATIONAL_HEADROOM_BYTES)
        );

        let mut materialized_overflow = StagingPayloadBudget {
            pca_bytes: u64::MAX,
            ..StagingPayloadBudget::default()
        };
        let unchanged = materialized_overflow;
        let error =
            reconcile_exported_driver_budget(&mut materialized_overflow, 1, u64::MAX).unwrap_err();
        assert_eq!(error.code, "staging_materialized_size_overflow");
        assert_eq!(materialized_overflow, unchanged);

        let mut total_overflow = StagingPayloadBudget {
            image_bytes: u64::MAX,
            ..StagingPayloadBudget::default()
        };
        let unchanged = total_overflow;
        let error = reconcile_exported_driver_budget(&mut total_overflow, 0, u64::MAX).unwrap_err();
        assert_eq!(error.code, "staging_remaining_size_invalid");
        assert_eq!(total_overflow, unchanged);
    }

    #[cfg(feature = "ci-automation")]
    #[test]
    fn ci_driver_budget_receipt_is_create_new_run_bound_and_exact() {
        let temp = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-ci-driver-budget-receipt",
        )
        .expect("create receipt directory");
        let path = temp.path().join("driver-budget-reconciliation.json");
        let budget = StagingPayloadBudget {
            image_bytes: 4,
            exported_driver_bytes: 67_108_864,
            pca_bytes: 5,
            user_driver_bytes: 6,
            uefiseven_bytes: 7,
            preinstalled_software_bytes: 8,
        };
        let remaining = 4 + 6 + 7 + 8 + STAGING_OPERATIONAL_HEADROOM_BYTES;
        write_ci_driver_budget_receipt_to(
            &path,
            "00112233445566778899aabbccddeeff",
            0,
            67_108_864,
            9_000_000_000,
            remaining,
            budget,
        )
        .expect("write receipt");
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read receipt"))
                .expect("parse receipt");
        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["run_id"], "00112233445566778899aabbccddeeff");
        assert_eq!(value["planned_driver_bytes"], 0);
        assert_eq!(value["actual_driver_bytes"], 67_108_864_u64);
        assert_eq!(value["current_free_bytes"], 9_000_000_000_u64);
        assert_eq!(value["remaining_required_bytes"], remaining);
        assert_eq!(value["image_bytes"], 4);
        assert_eq!(value["materialized_pca_bytes"], 5);
        assert_eq!(value["user_driver_bytes"], 6);
        assert_eq!(value["uefiseven_bytes"], 7);
        assert_eq!(value["preinstalled_software_bytes"], 8);
        assert_eq!(
            value["operational_headroom_bytes"],
            STAGING_OPERATIONAL_HEADROOM_BYTES
        );
        assert!(write_ci_driver_budget_receipt_to(
            &path,
            "00112233445566778899aabbccddeeff",
            0,
            1,
            2,
            1,
            budget,
        )
        .is_err());
    }

    #[cfg(feature = "ci-automation")]
    #[test]
    fn ci_stale_driver_product_receipt_proves_manifest_exclusion() {
        let temp = lr_core::scoped_temp_file::ScopedTempDir::create_in(
            &std::env::temp_dir(),
            "lr-ci-stale-driver-product-receipt",
        )
        .expect("create receipt directory");
        let path = temp
            .path()
            .join("stale-disabled-driver-product-receipt.json");
        write_ci_stale_driver_manifest_receipt_to(
            &path,
            "00112233445566778899aabbccddeeff",
            3,
            0,
            0,
        )
        .expect("write stale-driver product receipt");
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read receipt"))
                .expect("parse receipt");
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["run_id"], "00112233445566778899aabbccddeeff");
        assert_eq!(value["drivers_directory_skipped"], true);
        assert_eq!(value["manifest_artifact_count"], 3);
        assert_eq!(value["preserved_driver_artifact_count"], 0);
        assert_eq!(value["run_fixture_artifact_count_any_role"], 0);
        assert!(write_ci_stale_driver_manifest_receipt_to(
            &temp.path().join("unexpected.json"),
            "00112233445566778899aabbccddeeff",
            3,
            0,
            0,
        )
        .is_err());
        let rejected = temp.path().join("rejected");
        std::fs::create_dir(&rejected).expect("create rejected receipt directory");
        assert!(write_ci_stale_driver_manifest_receipt_to(
            &rejected.join("stale-disabled-driver-product-receipt.json"),
            "00112233445566778899aabbccddeeff",
            3,
            1,
            0,
        )
        .is_err());
    }

    #[test]
    fn direct_auto_boot_mode_uses_target_layout_when_known() {
        let detector_must_not_run = || -> Result<bool, &'static str> {
            panic!("known target layout must not query current firmware")
        };
        assert_eq!(
            resolve_direct_install_uefi_mode_with(
                BootModeSelection::Auto,
                PartitionStyle::GPT,
                detector_must_not_run,
            ),
            Ok(true)
        );
        assert_eq!(
            resolve_direct_install_uefi_mode_with(
                BootModeSelection::Auto,
                PartitionStyle::MBR,
                detector_must_not_run,
            ),
            Ok(false)
        );
    }

    #[test]
    fn direct_auto_boot_mode_probes_firmware_when_target_layout_is_unknown() {
        assert_eq!(
            resolve_direct_install_uefi_mode_with(
                BootModeSelection::Auto,
                PartitionStyle::Unknown,
                || Ok::<_, &'static str>(true),
            ),
            Ok(true)
        );
        assert_eq!(
            resolve_direct_install_uefi_mode_with(
                BootModeSelection::Auto,
                PartitionStyle::Unknown,
                || Ok::<_, &'static str>(false),
            ),
            Ok(false)
        );
        assert_eq!(
            resolve_direct_install_uefi_mode_with(
                BootModeSelection::Auto,
                PartitionStyle::Unknown,
                || Err("firmware probe failed"),
            ),
            Err("firmware probe failed")
        );
    }

    #[test]
    fn explicit_direct_boot_mode_never_queries_firmware() {
        let detector_must_not_run = || -> Result<bool, &'static str> {
            panic!("explicit boot mode must not query current firmware")
        };
        assert_eq!(
            resolve_direct_install_uefi_mode_with(
                BootModeSelection::UEFI,
                PartitionStyle::Unknown,
                detector_must_not_run,
            ),
            Ok(true)
        );
        assert_eq!(
            resolve_direct_install_uefi_mode_with(
                BootModeSelection::Legacy,
                PartitionStyle::Unknown,
                detector_must_not_run,
            ),
            Ok(false)
        );
    }

    #[test]
    fn missing_boot_directory_never_reclassifies_a_validated_modern_source_as_nt5() {
        assert!(!missing_modern_boot_assets_warning(true, false));
        assert!(!missing_modern_boot_assets_warning(false, true));
        assert!(missing_modern_boot_assets_warning(false, false));
    }

    #[test]
    fn disabled_direct_advanced_options_do_not_start_an_offline_transaction() {
        use std::cell::Cell;

        let options = super::super::advanced_options::AdvancedOptions::default();
        assert_eq!(validate_direct_advanced_request(&options, false), Ok(false));

        let called = Cell::new(false);
        run_requested_direct_operation(false, || -> Result<(), &'static str> {
            called.set(true);
            Err("must not run")
        })
        .expect("disabled options must not run their transaction");
        assert!(!called.get());
    }

    #[test]
    fn selected_direct_advanced_operation_preserves_error_for_outer_warning_policy() {
        let mut options = super::super::advanced_options::AdvancedOptions {
            disable_uac: true,
            ..Default::default()
        };
        assert_eq!(validate_direct_advanced_request(&options, false), Ok(true));
        assert_eq!(
            run_requested_direct_operation(true, || Err("offline write failed")),
            Err("offline write failed")
        );

        options.disable_uac = false;
        options.migrate_wifi = true;
        assert_eq!(validate_direct_advanced_request(&options, false), Ok(false));

        options.wifi_profile_xml = "<WLANProfile />".to_string();
        assert_eq!(validate_direct_advanced_request(&options, false), Ok(true));
    }

    #[test]
    fn explicitly_selected_direct_file_inputs_cannot_be_empty() {
        let cases = [
            (
                "deploy",
                "deployment script execution was selected without a script path",
            ),
            (
                "first_login",
                "first-login script execution was selected without a script path",
            ),
            (
                "drivers",
                "custom driver import was selected without a source path",
            ),
            (
                "registry",
                "registry import was selected without a .reg path",
            ),
            (
                "files",
                "custom file import was selected without a source path",
            ),
        ];

        for (case, expected) in cases {
            let mut options = super::super::advanced_options::AdvancedOptions::default();
            match case {
                "deploy" => options.run_script_during_deploy = true,
                "first_login" => options.run_script_first_login = true,
                "drivers" => options.import_custom_drivers = true,
                "registry" => options.import_registry_file = true,
                "files" => options.import_custom_files = true,
                _ => unreachable!(),
            }
            assert_eq!(
                validate_direct_advanced_request(&options, false),
                Err(expected),
                "case {case} must fail before the Direct transaction"
            );
        }
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn development_backend_refuses_before_any_io() {
        let intent = intent(InstallMode::Direct);
        let mut backend = ProductionInstallBackend::new(&intent);
        let mut reporter = |_: InstallExecutionEvent| {};
        let cancelled = || false;
        let error = backend
            .execute_phase(
                &intent,
                &InstallExecutionContext::default(),
                InstallExecutionPhase::FormatTarget,
                &mut reporter,
                &cancelled,
            )
            .unwrap_err();
        assert_eq!(error.code, "development_build_denied");
    }
}
