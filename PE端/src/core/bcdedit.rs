use anyhow::Result;
use std::path::Path;
use std::sync::Mutex;

use crate::tr;
use crate::utils::command::new_command;
use crate::utils::encoding::gbk_to_utf8;
use crate::utils::path::get_bin_dir;
use lr_core::boot_pca::BootPcaMode;

static ESP_MOUNT_LOCK: Mutex<()> = Mutex::new(());

pub(crate) fn finish_with_esp_cleanup<T>(
    operation: Result<T>,
    mount: Option<lr_core::boot_pca::TemporaryEspMountGuard>,
) -> Result<T> {
    let cleanup = mount
        .map(lr_core::boot_pca::TemporaryEspMountGuard::close)
        .unwrap_or(Ok(()));
    match (operation, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(cleanup)) => anyhow::bail!("temporary ESP cleanup failed: {cleanup}"),
        (Err(operation), Ok(())) => Err(operation),
        (Err(operation), Err(cleanup)) => Err(anyhow::anyhow!(
            "{operation:#}; additionally temporary ESP cleanup failed: {cleanup}"
        )),
    }
}

#[cfg(test)]
mod trusted_cleanup_tests {
    use super::*;
    use std::collections::VecDeque;

    const LOADER: &str = "{11111111-1111-1111-1111-111111111111}";
    const RAMDISK: &str = "{22222222-2222-2222-2222-222222222222}";

    fn ok(stdout: impl Into<String>) -> BcdCommandOutput {
        BcdCommandOutput {
            success: true,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    #[test]
    fn missing_loader_does_not_prevent_ramdisk_cleanup() {
        let mut outputs = VecDeque::from([
            ok(format!("identifier {RAMDISK}")),
            ok(format!("identifier {RAMDISK}")),
            ok("deleted"),
            ok("no trusted objects"),
        ]);
        let mut commands = Vec::new();
        delete_trusted_pe_boot_objects_with(LOADER, RAMDISK, |args| {
            commands.push(args.to_vec());
            Ok(outputs.pop_front().expect("unexpected bcdedit command"))
        })
        .unwrap();
        assert!(outputs.is_empty());
        assert_eq!(
            commands
                .iter()
                .filter(|args| args.first().is_some_and(|arg| arg == "/delete"))
                .collect::<Vec<_>>(),
            vec![&vec![
                "/delete".to_string(),
                RAMDISK.to_string(),
                "/f".to_string()
            ]]
        );
    }

    #[test]
    fn delete_failure_is_idempotent_only_after_fresh_absence_proof() {
        let mut outputs = VecDeque::from([
            ok(format!("identifier {LOADER}")),
            BcdCommandOutput {
                success: false,
                stdout: String::new(),
                stderr: "not found".into(),
            },
            ok("object disappeared"),
            ok("ramdisk already absent"),
            ok("no trusted objects"),
        ]);
        delete_trusted_pe_boot_objects_with(LOADER, RAMDISK, |_| {
            Ok(outputs.pop_front().expect("unexpected bcdedit command"))
        })
        .unwrap();
        assert!(outputs.is_empty());
    }
}

pub struct BootManager {
    bcdedit_path: String,
    bcdboot_path: String,
}

#[derive(Debug)]
struct BcdCommandOutput {
    success: bool,
    stdout: String,
    stderr: String,
}

fn valid_bcd_guid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 38
        && bytes[0] == b'{'
        && bytes[37] == b'}'
        && bytes[1..37].iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn inventory_contains(inventory: &BcdCommandOutput, guid: &str) -> bool {
    format!("{}\n{}", inventory.stdout, inventory.stderr)
        .to_ascii_lowercase()
        .contains(&guid.to_ascii_lowercase())
}

fn delete_trusted_pe_boot_objects_with(
    loader_guid: &str,
    ramdisk_guid: &str,
    mut run: impl FnMut(&[String]) -> Result<BcdCommandOutput>,
) -> Result<()> {
    if !valid_bcd_guid(loader_guid)
        || !valid_bcd_guid(ramdisk_guid)
        || loader_guid.eq_ignore_ascii_case(ramdisk_guid)
    {
        anyhow::bail!("trusted PE boot journal contains invalid or duplicate BCD GUIDs");
    }

    let inventory_args = ["/enum".to_string(), "all".to_string()];
    for guid in [loader_guid, ramdisk_guid] {
        let before = run(&inventory_args)?;
        if !before.success {
            anyhow::bail!(
                "bcdedit could not inventory trusted PE objects before cleanup: stdout={}; stderr={}",
                before.stdout,
                before.stderr
            );
        }
        if !inventory_contains(&before, guid) {
            continue;
        }

        let delete_args = ["/delete".to_string(), guid.to_string(), "/f".to_string()];
        let deleted = run(&delete_args)?;
        if !deleted.success {
            // A previous run or a concurrent, equally trusted cleanup may have removed this
            // object after our inventory. Only a fresh successful full inventory may turn that
            // nonzero delete into idempotent success.
            let after_failure = run(&inventory_args)?;
            if !after_failure.success || inventory_contains(&after_failure, guid) {
                anyhow::bail!(
                    "bcdedit failed to delete trusted PE object {guid}: stdout={}; stderr={}; fresh_inventory_stdout={}; fresh_inventory_stderr={}",
                    deleted.stdout,
                    deleted.stderr,
                    after_failure.stdout,
                    after_failure.stderr
                );
            }
        }
    }

    let final_inventory = run(&inventory_args)?;
    if !final_inventory.success {
        anyhow::bail!(
            "bcdedit could not verify trusted PE object deletion: {}",
            final_inventory.stderr
        );
    }
    for guid in [loader_guid, ramdisk_guid] {
        if inventory_contains(&final_inventory, guid) {
            anyhow::bail!("trusted PE BCD object remains after deletion: {guid}");
        }
    }
    Ok(())
}

impl BootManager {
    pub fn new() -> Self {
        let bin_dir = get_bin_dir();
        Self {
            bcdedit_path: bin_dir.join("bcdedit.exe").to_string_lossy().to_string(),
            bcdboot_path: bin_dir.join("bcdboot.exe").to_string_lossy().to_string(),
        }
    }

    /// Delete only the two BCD objects named by a trusted, session-bound normal-endpoint journal.
    pub fn delete_trusted_pe_boot_objects(
        &self,
        loader_guid: &str,
        ramdisk_guid: &str,
    ) -> Result<()> {
        delete_trusted_pe_boot_objects_with(loader_guid, ramdisk_guid, |args| {
            let output = new_command(&self.bcdedit_path).args(args).output()?;
            Ok(BcdCommandOutput {
                success: output.status.success(),
                stdout: gbk_to_utf8(&output.stdout),
                stderr: gbk_to_utf8(&output.stderr),
            })
        })
    }

    /// 查找目标 Windows 分区所在磁盘的 ESP 分区
    pub fn find_esp_on_same_disk(
        &self,
        windows_partition: &str,
    ) -> Result<lr_core::boot_pca::TemporaryEspMountGuard> {
        let _mount_lock = ESP_MOUNT_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        log::info!("查找 {} 所在磁盘的 ESP 分区...", windows_partition);

        let drive_letter = windows_partition
            .trim_end_matches(':')
            .trim_end_matches('\\')
            .chars()
            .next()
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("无法确定分区所在磁盘")))?;
        let target = lr_core::windows_storage::volume_identity(drive_letter)?;
        let disk_num = target.disk_number;
        log::info!("目标分区在磁盘 {}", disk_num);

        let esp = lr_core::windows_storage::partitions(disk_num)?
            .into_iter()
            .find(|partition| partition.kind == lr_core::windows_storage::PartitionKind::EfiSystem)
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("未找到 ESP 分区")))?;
        log::info!("找到 ESP: 分区 {}", esp.partition_number);

        let existing_letters = lr_core::windows_storage::assigned_drive_letters_for_partition(
            disk_num,
            esp.offset_bytes,
        )?;
        if let Some(letter) = existing_letters.first().copied() {
            log::info!("ESP 已有盘符 {}:，复用且不会在操作后移除", letter);
            return lr_core::boot_pca::TemporaryEspMountGuard::existing(&letter.to_string())
                .map_err(anyhow::Error::msg);
        }

        // Step 3: 使用真正空闲的盘符挂载 ESP，不能覆盖用户已有的 S: 等盘符。
        let mount_letter = lr_core::boot_pca::find_available_drive_letter()
            .ok_or_else(|| anyhow::anyhow!("{}", tr!("没有空闲盘符可挂载 ESP")))?;

        let expected_layout = lr_core::windows_storage::disk_layout_snapshot(disk_num)?;
        lr_core::windows_storage::assign_partition_drive_letter_checked(
            disk_num,
            esp.offset_bytes,
            mount_letter,
            &expected_layout,
        )?;
        let mount_guard = lr_core::boot_pca::TemporaryEspMountGuard::new(
            &mount_letter.to_string(),
            lr_core::windows_storage::VolumeIdentity {
                disk_number: disk_num,
                offset_bytes: esp.offset_bytes,
                extent_length_bytes: esp.size_bytes,
            },
            expected_layout,
        )
        .map_err(anyhow::Error::msg)?;

        let mount_root = format!("{}:\\", mount_letter);
        for _ in 0..20 {
            if Path::new(&mount_root).exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if Path::new(&mount_root).is_dir() {
            let mounted = format!("{}:", mount_letter);
            log::info!("ESP 已挂载到 {}", mounted);
            Ok(mount_guard)
        } else {
            let cleanup = mount_guard.close();
            match cleanup {
                Ok(()) => anyhow::bail!("{}", tr!("ESP 盘符分配失败")),
                Err(error) => {
                    anyhow::bail!("{}", tr!("ESP 盘符分配失败，且临时盘符卸载失败: {}", error))
                }
            }
        }
    }

    /// Select the exact system partition for a BIOS installation. BCDBoot `/s` means the system
    /// partition, which can be a separate active partition and must not be assumed to be the
    /// Windows volume. A temporary access path is assigned only to that verified same-disk extent.
    fn prepare_legacy_boot_partition(
        &self,
        windows_partition: &str,
    ) -> Result<(
        String,
        lr_core::windows_storage::VolumeIdentity,
        Option<lr_core::boot_pca::TemporaryEspMountGuard>,
    )> {
        let windows_letter = windows_partition
            .trim_end_matches([':', '\\'])
            .chars()
            .next()
            .ok_or_else(|| anyhow::anyhow!("cannot resolve Windows partition drive letter"))?;
        let windows = lr_core::windows_storage::volume_identity(windows_letter)?;
        let partitions = lr_core::windows_storage::partitions(windows.disk_number)?;
        let selected = partitions
            .iter()
            .find(|partition| partition.active)
            .filter(|partition| partition.offset_bytes != windows.offset_bytes)
            .cloned();

        let Some(selected) = selected else {
            return Ok((windows_partition.to_string(), windows, None));
        };
        let selected_identity = lr_core::windows_storage::VolumeIdentity {
            disk_number: windows.disk_number,
            offset_bytes: selected.offset_bytes,
            extent_length_bytes: selected.size_bytes,
        };
        let existing = lr_core::windows_storage::assigned_drive_letters_for_partition(
            windows.disk_number,
            selected.offset_bytes,
        )?;
        if let Some(letter) = existing.first().copied() {
            return Ok((format!("{}:", letter), selected_identity, None));
        }

        let letter = lr_core::boot_pca::find_available_drive_letter().ok_or_else(|| {
            anyhow::anyhow!("no free drive letter for the active system partition")
        })?;
        let expected_layout = lr_core::windows_storage::disk_layout_snapshot(windows.disk_number)?;
        lr_core::windows_storage::assign_partition_drive_letter_checked(
            windows.disk_number,
            selected.offset_bytes,
            letter,
            &expected_layout,
        )?;
        let guard = lr_core::boot_pca::TemporaryEspMountGuard::new(
            &letter.to_string(),
            selected_identity,
            expected_layout,
        )
        .map_err(anyhow::Error::msg)?;
        Ok((format!("{}:", letter), selected_identity, Some(guard)))
    }

    /// 修复指定分区的引导（高级版本，支持指定引导模式）
    pub fn repair_boot_advanced(
        &self,
        windows_partition: &str,
        use_uefi: bool,
        pca_mode: BootPcaMode,
    ) -> Result<()> {
        let windows_path = format!("{}\\Windows", windows_partition);

        log::info!("========== 修复引导 ==========");
        log::info!("Windows 路径: {}", windows_path);
        log::info!(
            "引导模式: {}",
            if use_uefi { "UEFI" } else { "Legacy/BIOS" }
        );

        // 验证 Windows 目录存在
        if !Path::new(&windows_path).exists() {
            anyhow::bail!("{}", tr!("Windows 目录不存在: {}", windows_path));
        }

        let mounted_esp = if use_uefi {
            log::info!("UEFI 模式：查找目标磁盘 ESP 分区");
            Some(
                self.find_esp_on_same_disk(windows_partition)
                    .map_err(|error| {
                        anyhow::anyhow!("{}", tr!("目标系统所在磁盘没有可用的 ESP: {}", error))
                    })?,
            )
        } else {
            None
        };
        let (legacy_boot_partition, legacy_boot_identity, legacy_mount) = if use_uefi {
            (String::new(), None, None)
        } else {
            let (partition, identity, mount) =
                self.prepare_legacy_boot_partition(windows_partition)?;
            (partition, Some(identity), mount)
        };
        let operation = (|| -> Result<()> {
            let existing_esp_hint = mounted_esp.as_ref().map(|mount| {
                let esp_letter = mount.letter();
                let esp_root = format!("{}\\", esp_letter.trim_end_matches('\\'));
                let info = lr_core::boot_pca::inspect_esp_generation(Path::new(&esp_root));
                if info.signature_valid {
                    info.generation
                } else {
                    lr_core::boot_pca::PcaGeneration::Unknown
                }
            });

            if use_uefi {
                let esp_letter = mounted_esp
                    .as_ref()
                    .map(lr_core::boot_pca::TemporaryEspMountGuard::letter)
                    .expect("UEFI repair always mounts the target-disk ESP first");
                let (version, family) =
                    lr_core::boot_pca::inspect_installed_windows_boot_family(windows_partition)
                        .map_err(|error| {
                            anyhow::anyhow!("{}", tr!("无法确认目标系统引导版本: {}", error))
                        })?;
                log::info!(
                    "[BOOT] 目标 Windows 版本: {}，引导族: {:?}",
                    version,
                    family
                );
                match family {
                    lr_core::boot_pca::InstalledWindowsBootFamily::LegacyUefi => {
                        lr_core::boot_pca::repair_legacy_windows_uefi_boot(
                            Path::new(&self.bcdboot_path),
                            windows_partition,
                            esp_letter,
                        )
                        .map_err(|error| {
                            anyhow::anyhow!("{}", tr!("旧版 Windows UEFI 引导修复失败: {}", error))
                        })?;
                        log::info!("旧版 Windows 标准 UEFI 引导修复成功");
                    }
                    lr_core::boot_pca::InstalledWindowsBootFamily::ModernPca => {
                        let firmware = lr_core::boot_pca::inspect_firmware_pca();
                        log::info!("[BOOT PCA] 固件检测: {:?}", firmware);
                        let decision = lr_core::boot_pca::repair_uefi_boot(
                            Path::new(&self.bcdboot_path),
                            windows_partition,
                            esp_letter,
                            pca_mode,
                            firmware,
                            existing_esp_hint,
                        )
                        .map_err(|error| {
                            anyhow::anyhow!("{}", tr!("UEFI 引导修复失败: {}", error))
                        })?;
                        log::info!(
                            "UEFI 引导修复成功: {} ({})",
                            decision.generation,
                            decision.reason
                        );
                    }
                    lr_core::boot_pca::InstalledWindowsBootFamily::Nt5 => {
                        anyhow::bail!("{}", tr!("NT5 系统必须使用 XP/2003 专用引导写入路径"));
                    }
                }
            } else {
                // Legacy/BIOS 模式
                log::info!("Legacy 模式：写入 MBR 引导");

                let bootsect_path = get_bin_dir().join("bootsect.exe");
                if !bootsect_path.is_file() {
                    anyhow::bail!(
                        "{}",
                        tr!(
                            "Legacy 引导修复缺少 bootsect.exe：{}",
                            bootsect_path.display()
                        )
                    );
                }

                // BCDBoot's /s argument is the only supported way to bind the write to an
                // explicitly selected system partition. Never retry without /s: on a multi-disk
                // machine that would allow firmware enumeration to redirect the write elsewhere.
                let output = new_command(&self.bcdboot_path)
                    .args([
                        &windows_path,
                        "/s",
                        &legacy_boot_partition,
                        "/f",
                        "BIOS",
                        "/l",
                        "zh-cn",
                    ])
                    .output()?;

                let stdout = gbk_to_utf8(&output.stdout);
                let stderr = gbk_to_utf8(&output.stderr);

                log::debug!("bcdboot stdout: {}", stdout);
                log::debug!("bcdboot stderr: {}", stderr);

                if !output.status.success() {
                    // Windows 7 BCDBoot may not understand /f. The compatibility retry retains
                    // the same already-verified /s target and removes only that optional switch.
                    let output = new_command(&self.bcdboot_path)
                        .args([&windows_path, "/s", &legacy_boot_partition, "/l", "zh-cn"])
                        .output()?;

                    let stderr = gbk_to_utf8(&output.stderr);
                    if !output.status.success() {
                        anyhow::bail!("{}", tr!("Legacy 引导修复失败: {}", stderr));
                    }
                }

                log::info!("使用 bootsect 写入目标分区引导扇区和同盘 MBR");
                let output = new_command(&bootsect_path)
                    .args(["/nt60", &legacy_boot_partition, "/force", "/mbr"])
                    .output()?;
                let stdout = gbk_to_utf8(&output.stdout);
                let stderr = gbk_to_utf8(&output.stderr);
                log::debug!("bootsect stdout: {}", stdout);
                log::debug!("bootsect stderr: {}", stderr);
                if !output.status.success() {
                    anyhow::bail!(
                        "{}",
                        tr!(
                            "bootsect 写入引导扇区失败（退出码 {}）：{}",
                            format!("{:?}", output.status.code()),
                            if stderr.trim().is_empty() {
                                stdout.trim()
                            } else {
                                stderr.trim()
                            }
                        )
                    );
                }
                let boot_identity = legacy_boot_identity
                    .ok_or_else(|| anyhow::anyhow!("legacy boot identity is missing"))?;
                let expected_layout =
                    lr_core::windows_storage::disk_layout_snapshot(boot_identity.disk_number)?;
                lr_core::windows_storage::set_mbr_active_checked(
                    boot_identity.disk_number,
                    boot_identity.offset_bytes,
                    true,
                    &expected_layout,
                )?;
                log::info!("Legacy 引导修复成功");
            }

            log::info!("========== 引导修复完成 ==========");
            Ok(())
        })();
        let mount = if use_uefi { mounted_esp } else { legacy_mount };
        finish_with_esp_cleanup(operation, mount)
    }

    /// 为已释放的 XP/2003 系统写入引导（ntldr/boot.ini + MBR，仅 Legacy）。
    pub fn write_xp_boot(&self, windows_partition: &str) -> Result<()> {
        log::info!("========== 写入 XP 引导 ==========");
        match lr_core::boot::write_xp_boot(&get_bin_dir(), windows_partition) {
            Ok(out) => {
                log::info!("XP 引导写入完成:\n{}", out);
                log::info!("========== XP 引导完成 ==========");
                Ok(())
            }
            Err(e) => anyhow::bail!("{}", tr!("XP 引导写入失败: {}", e)),
        }
    }

    /// 为已释放的「UEFI 化」XP/2003 系统写入 UEFI/GPT 引导。
    ///
    /// 查找同盘 ESP 并挂载，再用映像自带的 `WINDOWS\Boot\EFI`（bootxp64.efi + BCC）复刻
    /// 社区方案的 UEFI 引导写入。映像若不含这些文件，返回 Err，调用方应回退 Legacy。
    pub fn write_xp_uefi_gpt_boot(&self, windows_partition: &str) -> Result<()> {
        log::info!("========== 写入 XP UEFI/GPT 引导 ==========");
        let esp = self
            .find_esp_on_same_disk(windows_partition)
            .map_err(|e| anyhow::anyhow!("{}", tr!("未找到 ESP，无法写 UEFI 引导: {}", e)))?;
        log::info!("使用 ESP: {}", esp.letter());

        let operation = (|| -> Result<()> {
            match lr_core::xp::write_xp_uefi_gpt_boot(
                windows_partition,
                esp.letter(),
                Path::new(&self.bcdedit_path),
            ) {
                Ok(out) => {
                    log::info!("XP UEFI 引导写入完成:\n{}", out);
                    log::info!("========== XP UEFI 引导完成 ==========");
                    Ok(())
                }
                Err(e) => anyhow::bail!("{}", tr!("XP UEFI 引导写入失败: {}", e)),
            }
        })();
        finish_with_esp_cleanup(operation, Some(esp))
    }
}

impl Default for BootManager {
    fn default() -> Self {
        Self::new()
    }
}
