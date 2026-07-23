//! Windows UEFI boot-manager signature selection and verification.
//!
//! Secure Boot firmware validates the EFI boot manager in the ESP. It does not
//! choose the signature generation based on `winload.efi`, so this module keeps
//! the decision scoped to firmware trust, the existing ESP, and the boot files
//! available in the offline Windows image.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::command::{new_command, CommandExecutor, CommandRequest, SystemCommandExecutor};
use crate::encoding::gbk_to_utf8;

/// Pick an unused drive letter for a temporary ESP mount.
///
/// `Path::exists` is not sufficient here because an empty optical drive can be
/// assigned while its root is inaccessible. `GetLogicalDrives` reports every
/// assigned local and network drive without probing the filesystem.
#[cfg(windows)]
pub fn find_available_drive_letter() -> Option<char> {
    use windows::Win32::Storage::FileSystem::GetLogicalDrives;

    let assigned = unsafe { GetLogicalDrives() };
    find_available_drive_letter_in_mask(assigned)
}

fn find_available_drive_letter_in_mask(assigned: u32) -> Option<char> {
    (3u8..=25)
        .rev()
        .find(|index| assigned & (1u32 << index) == 0)
        .map(|index| char::from(b'A' + index))
}

fn normalize_drive_letter(value: &str) -> Option<char> {
    let trimmed = value.trim().trim_end_matches([':', '\\', '/']);
    let mut chars = trimmed.chars();
    let letter = chars.next()?;
    if chars.next().is_some() || !letter.is_ascii_alphabetic() {
        return None;
    }
    Some(letter.to_ascii_uppercase())
}

/// Remove a temporary drive-letter mount created for an ESP.
pub fn unmount_esp(esp_letter: &str) -> Result<(), String> {
    let letter = normalize_drive_letter(esp_letter)
        .ok_or_else(|| format!("无效的 ESP 盘符: {esp_letter}"))?;
    let mounted = format!("{letter}:");
    let output = new_command("mountvol")
        .args([mounted.as_str(), "/d"])
        .output()
        .map_err(|error| format!("卸载 ESP {mounted} 失败: {error}"))?;
    if output.status.success() {
        return Ok(());
    }

    let stdout = gbk_to_utf8(&output.stdout);
    let stderr = gbk_to_utf8(&output.stderr);
    Err(format!(
        "卸载 ESP {mounted} 失败，退出码 {:?}: {}{}",
        output.status.code(),
        stdout.trim(),
        stderr.trim()
    ))
}

/// Owns a temporary ESP drive-letter mount and removes it on every exit path.
#[derive(Debug)]
pub struct TemporaryEspMountGuard {
    letter: Option<String>,
}

impl TemporaryEspMountGuard {
    pub fn new(esp_letter: &str) -> Result<Self, String> {
        let letter = normalize_drive_letter(esp_letter)
            .ok_or_else(|| format!("无效的 ESP 盘符: {esp_letter}"))?;
        Ok(Self {
            letter: Some(format!("{letter}:")),
        })
    }

    pub fn letter(&self) -> &str {
        self.letter
            .as_deref()
            .expect("ESP mount guard is active until it is dropped")
    }

    pub fn close(mut self) -> Result<(), String> {
        let letter = self
            .letter
            .take()
            .expect("ESP mount guard can only be closed once");
        unmount_esp(&letter)
    }
}

impl Drop for TemporaryEspMountGuard {
    fn drop(&mut self) {
        if let Some(letter) = self.letter.take() {
            if let Err(error) = unmount_esp(&letter) {
                log::warn!("[BOOT PCA] 卸载临时 ESP {} 失败: {}", letter, error);
            }
        }
    }
}

#[cfg(not(windows))]
pub fn find_available_drive_letter() -> Option<char> {
    None
}

/// User preference for the Windows EFI boot-manager signing generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BootPcaMode {
    #[default]
    Auto,
    Pca2011,
    Pca2023,
}

impl BootPcaMode {
    pub fn as_config_value(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Pca2011 => "pca2011",
            Self::Pca2023 => "pca2023",
        }
    }

    pub fn from_config_value(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "pca2011" | "2011" | "1" => Self::Pca2011,
            "pca2023" | "2023" | "2" => Self::Pca2023,
            _ => Self::Auto,
        }
    }
}

impl fmt::Display for BootPcaMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => f.write_str("Auto"),
            Self::Pca2011 => f.write_str("PCA2011"),
            Self::Pca2023 => f.write_str("PCA2023"),
        }
    }
}

/// Signing generation detected on an EFI executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PcaGeneration {
    Pca2011,
    Pca2023,
    #[default]
    Unknown,
}

impl fmt::Display for PcaGeneration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pca2011 => f.write_str("PCA2011"),
            Self::Pca2023 => f.write_str("PCA2023"),
            Self::Unknown => f.write_str("Unknown"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct EfiSignatureInfo {
    pub generation: PcaGeneration,
    pub signature_valid: bool,
    pub issuer: String,
    pub path: PathBuf,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct FirmwarePcaInfo {
    pub secure_boot_enabled: Option<bool>,
    pub trusts_pca2011: Option<bool>,
    pub trusts_pca2023: Option<bool>,
    pub revokes_pca2011: Option<bool>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct WindowsBootSources {
    pub pca2011: Option<EfiSignatureInfo>,
    pub pca2023: Option<EfiSignatureInfo>,
}

impl WindowsBootSources {
    pub fn supports(&self, generation: PcaGeneration) -> bool {
        match generation {
            PcaGeneration::Pca2011 => self.pca2011.is_some(),
            PcaGeneration::Pca2023 => self.pca2023.is_some(),
            PcaGeneration::Unknown => false,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct BootPcaAssessment {
    pub firmware: FirmwarePcaInfo,
    pub existing_esp: PcaGeneration,
    pub sources: WindowsBootSources,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootPcaDecision {
    pub generation: PcaGeneration,
    pub reason: &'static str,
}

/// Resolve the requested mode without silently crossing signing generations.
pub fn resolve_pca_mode(
    requested: BootPcaMode,
    assessment: &BootPcaAssessment,
) -> Result<BootPcaDecision, String> {
    let firmware = &assessment.firmware;

    let ensure_compatible = |generation: PcaGeneration| -> Result<(), String> {
        if !assessment.sources.supports(generation) {
            return Err(match generation {
                PcaGeneration::Pca2011 => {
                    "目标 Windows 缺少有效的 PCA2011 EFI 引导文件".to_string()
                }
                PcaGeneration::Pca2023 => {
                    "目标 Windows 缺少有效的 PCA2023 BootEx 引导文件".to_string()
                }
                PcaGeneration::Unknown => "无法确定 EFI 引导签名".to_string(),
            });
        }

        if firmware.secure_boot_enabled == Some(true) {
            match generation {
                PcaGeneration::Pca2011 => {
                    if firmware.revokes_pca2011 == Some(true) {
                        return Err("固件已撤销 PCA2011，不能写入 PCA2011 引导".to_string());
                    }
                    if firmware.trusts_pca2011 == Some(false) {
                        return Err("固件不信任 PCA2011，不能写入 PCA2011 引导".to_string());
                    }
                }
                PcaGeneration::Pca2023 => {
                    if firmware.trusts_pca2023 == Some(false) {
                        return Err(
                            "固件不信任 Windows UEFI CA 2023，不能写入 PCA2023 引导".to_string()
                        );
                    }
                }
                PcaGeneration::Unknown => {}
            }
        }
        Ok(())
    };

    let explicit = match requested {
        BootPcaMode::Pca2011 => Some(PcaGeneration::Pca2011),
        BootPcaMode::Pca2023 => Some(PcaGeneration::Pca2023),
        BootPcaMode::Auto => None,
    };
    if let Some(generation) = explicit {
        ensure_compatible(generation)?;
        return Ok(BootPcaDecision {
            generation,
            reason: "用户选择",
        });
    }

    if firmware.secure_boot_enabled == Some(true) && firmware.revokes_pca2011 == Some(true) {
        ensure_compatible(PcaGeneration::Pca2023)?;
        if firmware.trusts_pca2023 != Some(true) {
            return Err(
                "固件已撤销 PCA2011，但无法确认固件信任 Windows UEFI CA 2023；请先完成固件证书更新，或手动选择 PCA2023 覆盖检测结果"
                    .to_string(),
            );
        }
        return Ok(BootPcaDecision {
            generation: PcaGeneration::Pca2023,
            reason: "固件已撤销 PCA2011",
        });
    }

    if firmware.secure_boot_enabled == Some(true)
        && firmware.trusts_pca2023 == Some(true)
        && assessment.sources.supports(PcaGeneration::Pca2023)
    {
        ensure_compatible(PcaGeneration::Pca2023)?;
        return Ok(BootPcaDecision {
            generation: PcaGeneration::Pca2023,
            reason: "安全启动已开启且固件信任 PCA2023",
        });
    }

    if assessment.existing_esp != PcaGeneration::Unknown
        && ensure_compatible(assessment.existing_esp).is_ok()
    {
        return Ok(BootPcaDecision {
            generation: assessment.existing_esp,
            reason: "沿用同盘 ESP 的现有签名",
        });
    }

    if firmware.trusts_pca2023 == Some(true) && assessment.sources.supports(PcaGeneration::Pca2023)
    {
        return Ok(BootPcaDecision {
            generation: PcaGeneration::Pca2023,
            reason: "固件支持 PCA2023 且目标 Windows 提供 BootEx",
        });
    }

    if ensure_compatible(PcaGeneration::Pca2011).is_ok() {
        return Ok(BootPcaDecision {
            generation: PcaGeneration::Pca2011,
            reason: "兼容性回退",
        });
    }

    ensure_compatible(PcaGeneration::Pca2023)?;
    if firmware.secure_boot_enabled == Some(false) {
        return Ok(BootPcaDecision {
            generation: PcaGeneration::Pca2023,
            reason: "安全启动关闭且仅 PCA2023 引导文件可用",
        });
    }

    Err(
        "自动模式无法确认固件信任 Windows UEFI CA 2023；请保留 PCA2011 引导文件、完成固件证书更新，或手动选择 PCA2023 覆盖检测结果"
            .to_string(),
    )
}

pub fn inspect_windows_boot_sources(windows_dir: &Path) -> WindowsBootSources {
    let normal = windows_dir.join("Boot").join("EFI").join("bootmgfw.efi");
    let bootex = windows_dir
        .join("Boot")
        .join("EFI_EX")
        .join("bootmgfw_EX.efi");

    let normal_info = inspect_efi_signature(&normal);
    let bootex_info = inspect_efi_signature(&bootex);
    let mut result = WindowsBootSources::default();

    if normal_info.signature_valid {
        match normal_info.generation {
            PcaGeneration::Pca2011 => result.pca2011 = Some(normal_info),
            PcaGeneration::Pca2023 => result.pca2023 = Some(normal_info),
            PcaGeneration::Unknown => {}
        }
    }
    if bootex_info.signature_valid {
        match bootex_info.generation {
            PcaGeneration::Pca2011 => {
                if result.pca2011.is_none() {
                    result.pca2011 = Some(bootex_info);
                }
            }
            PcaGeneration::Pca2023 => result.pca2023 = Some(bootex_info),
            PcaGeneration::Unknown => {}
        }
    }
    result
}

pub fn inspect_esp_generation(esp_root: &Path) -> EfiSignatureInfo {
    let primary = esp_root
        .join("EFI")
        .join("Microsoft")
        .join("Boot")
        .join("bootmgfw.efi");
    let fallback = esp_root.join("EFI").join("Boot").join("bootx64.efi");

    let primary_info = inspect_efi_signature(&primary);
    if primary_info.signature_valid && primary_info.generation != PcaGeneration::Unknown {
        return primary_info;
    }
    inspect_efi_signature(&fallback)
}

/// Write and verify the selected EFI boot-manager generation.
pub fn repair_uefi_boot(
    bcdboot_path: &Path,
    windows_partition: &str,
    esp_letter: &str,
    requested: BootPcaMode,
    firmware: FirmwarePcaInfo,
    existing_esp_hint: Option<PcaGeneration>,
) -> Result<BootPcaDecision, String> {
    let win = windows_partition.trim_end_matches(['\\', ':']);
    let windows_dir = PathBuf::from(format!("{}:\\Windows", win));
    let esp = esp_letter.trim_end_matches(['\\', ':']);
    let esp_root = PathBuf::from(format!("{}:\\", esp));

    if !windows_dir.is_dir() {
        return Err(format!("Windows 目录不存在: {}", windows_dir.display()));
    }
    if !esp_root.is_dir() {
        return Err(format!("ESP 未挂载: {}", esp_root.display()));
    }

    // A UEFI customization script may run before this function. When supplied,
    // this hint is the signature captured before that script touched the ESP;
    // using it prevents the script itself from biasing automatic selection.
    let existing_esp = existing_esp_hint.unwrap_or_else(|| {
        let existing = inspect_esp_generation(&esp_root);
        if existing.signature_valid {
            existing.generation
        } else {
            PcaGeneration::Unknown
        }
    });
    let sources = inspect_windows_boot_sources(&windows_dir);
    let assessment = BootPcaAssessment {
        firmware,
        existing_esp,
        sources,
    };
    let decision = resolve_pca_mode(requested, &assessment)?;

    let windows_arg = windows_dir.to_string_lossy().to_string();
    let esp_arg = format!("{}:", esp);
    log::info!(
        "[BOOT PCA] 执行 bcdboot，模式={}，原因={}，ESP={}",
        decision.generation,
        decision.reason,
        esp_arg
    );
    let write_mode = run_bcdboot(bcdboot_path, &windows_arg, &esp_arg, decision.generation)?;

    let primary = esp_root
        .join("EFI")
        .join("Microsoft")
        .join("Boot")
        .join("bootmgfw.efi");
    let bcd = esp_root
        .join("EFI")
        .join("Microsoft")
        .join("Boot")
        .join("BCD");
    if !bcd.is_file() {
        return Err(format!("bcdboot 返回成功，但未生成 BCD: {}", bcd.display()));
    }

    if write_mode == BcdbootWriteMode::ManualBootexCopy {
        let bootex_dir = windows_dir.join("Boot").join("EFI_EX");
        let bootex_source = bootex_dir.join("bootmgfw_EX.efi");
        let source_info = inspect_efi_signature(&bootex_source);
        validate_signature_generation(&source_info, PcaGeneration::Pca2023)?;

        let bootmgr_source = bootex_dir.join("bootmgr_EX.efi");
        let bootmgr_destination = esp_root
            .join("EFI")
            .join("Microsoft")
            .join("Boot")
            .join("bootmgr.efi");
        if bootmgr_source.is_file() {
            replace_file_with_signed_copy(&bootmgr_source, &bootmgr_destination, None)?;
        } else {
            log::info!("[BOOT PCA] 当前 BootEx 资源不含可选 bootmgr_EX.efi，保留现有文件");
        }

        deploy_bootex_fonts(
            &windows_dir.join("Boot").join("FONTS_EX"),
            &esp_root
                .join("EFI")
                .join("Microsoft")
                .join("Boot")
                .join("Fonts"),
        )?;

        let boot_stl_source = windows_dir.join("Boot").join("EFI").join("boot.stl");
        let boot_stl_destination = esp_root
            .join("EFI")
            .join("Microsoft")
            .join("Boot")
            .join("boot.stl");
        copy_optional_file_if_missing(&boot_stl_source, &boot_stl_destination)?;

        // Switch the firmware entry only after every supporting resource has
        // been prepared. A preceding failure therefore leaves the known-good
        // BCDBoot output as the active boot manager.
        replace_file_with_verified_copy(&bootex_source, &primary, PcaGeneration::Pca2023)?;
        log::info!("[BOOT PCA] 当前 bcdboot 不支持 /bootex，已部署并验证完整 BootEx 兼容资源");
    }

    let primary_info = inspect_efi_signature(&primary);
    validate_signature_generation(&primary_info, decision.generation)?;

    let fallback_dir = esp_root.join("EFI").join("Boot");
    std::fs::create_dir_all(&fallback_dir)
        .map_err(|e| format!("创建 EFI fallback 目录失败: {e}"))?;
    let fallback_name = efi_fallback_name(&primary)?;
    let fallback = fallback_dir.join(fallback_name);
    std::fs::copy(&primary, &fallback)
        .map_err(|e| format!("复制 bootmgfw.efi 到 {fallback_name} 失败: {e}"))?;
    let fallback_info = inspect_efi_signature(&fallback);
    validate_signature_generation(&fallback_info, decision.generation)?;

    Ok(decision)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BcdbootWriteMode {
    Native,
    ManualBootexCopy,
}

fn run_bcdboot(
    bcdboot_path: &Path,
    windows_arg: &str,
    esp_arg: &str,
    generation: PcaGeneration,
) -> Result<BcdbootWriteMode, String> {
    run_bcdboot_with_executor(
        &SystemCommandExecutor,
        bcdboot_path,
        windows_arg,
        esp_arg,
        generation,
    )
}

fn run_bcdboot_with_executor<E: CommandExecutor + ?Sized>(
    executor: &E,
    bcdboot_path: &Path,
    windows_arg: &str,
    esp_arg: &str,
    generation: PcaGeneration,
) -> Result<BcdbootWriteMode, String> {
    let mut offline_error = None;
    for offline in [true, false] {
        let args = bcdboot_args(windows_arg, esp_arg, generation, offline);
        let request = CommandRequest::new(bcdboot_path).args(&args);
        let output = executor
            .execute(&request)
            .map_err(|e| format!("启动 bcdboot 失败: {e}"))?;
        let stdout = gbk_to_utf8(output.stdout());
        let stderr = gbk_to_utf8(output.stderr());
        log::info!(
            "[BOOT PCA] bcdboot {} stdout: {}",
            if offline { "offline" } else { "compat" },
            stdout
        );
        log::info!(
            "[BOOT PCA] bcdboot {} stderr: {}",
            if offline { "offline" } else { "compat" },
            stderr
        );
        if output.succeeded() {
            return Ok(BcdbootWriteMode::Native);
        }

        let error = format!("退出码 {:?}: {}", output.exit_code(), stderr.trim());
        if offline {
            log::warn!(
                "[BOOT PCA] bcdboot /offline 失败，将保持 {} 代际并使用兼容命令重试: {}",
                generation,
                error
            );
            offline_error = Some(error);
        } else if generation != PcaGeneration::Pca2023 {
            return Err(format!(
                "bcdboot 写入 {} 引导失败（/offline: {}；兼容重试: {}）",
                generation,
                offline_error.as_deref().unwrap_or("未执行"),
                error
            ));
        } else {
            let standard_args = bcdboot_args(windows_arg, esp_arg, PcaGeneration::Pca2011, false);
            let standard_request = CommandRequest::new(bcdboot_path).args(&standard_args);
            let standard = executor
                .execute(&standard_request)
                .map_err(|e| format!("启动 bcdboot 兼容回退失败: {e}"))?;
            let standard_stdout = gbk_to_utf8(standard.stdout());
            let standard_stderr = gbk_to_utf8(standard.stderr());
            log::info!("[BOOT PCA] bcdboot 兼容回退 stdout: {}", standard_stdout);
            log::info!("[BOOT PCA] bcdboot 兼容回退 stderr: {}", standard_stderr);
            if standard.succeeded() {
                return Ok(BcdbootWriteMode::ManualBootexCopy);
            }
            return Err(format!(
                "bcdboot 写入 PCA2023 引导失败（/offline: {}；/bootex: {}；普通回退退出码 {:?}: {}）",
                offline_error.as_deref().unwrap_or("未执行"),
                error,
                standard.exit_code(),
                standard_stderr.trim()
            ));
        }
    }

    Err(format!("bcdboot 写入 {} 引导失败", generation))
}

fn replace_file_with_verified_copy(
    source: &Path,
    destination: &Path,
    expected: PcaGeneration,
) -> Result<(), String> {
    replace_file_with_signed_copy(source, destination, Some(expected))
}

fn replace_file_with_signed_copy(
    source: &Path,
    destination: &Path,
    expected: Option<PcaGeneration>,
) -> Result<(), String> {
    let parent = destination
        .parent()
        .ok_or_else(|| format!("目标 EFI 文件没有父目录: {}", destination.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("创建 EFI 目录失败: {e}"))?;
    let temporary = destination.with_extension("efi.lr-pca.tmp");
    let backup = destination.with_extension("efi.lr-pca.bak");
    let _ = std::fs::remove_file(&temporary);
    let _ = std::fs::remove_file(&backup);

    std::fs::copy(source, &temporary).map_err(|e| format!("暂存 BootEx 文件失败: {e}"))?;
    let temporary_info = inspect_efi_signature(&temporary);
    let validation = match expected {
        Some(generation) => validate_signature_generation(&temporary_info, generation),
        None if temporary_info.signature_valid => Ok(()),
        None => Err(format!(
            "EFI 文件签名无效: {}",
            temporary_info.error.as_deref().unwrap_or("签名验证失败")
        )),
    };
    if let Err(error) = validation {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }

    if destination.exists() {
        std::fs::rename(destination, &backup).map_err(|e| format!("备份现有 EFI 文件失败: {e}"))?;
    }
    if let Err(error) = std::fs::rename(&temporary, destination) {
        if backup.exists() {
            let _ = std::fs::rename(&backup, destination);
        }
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("替换 EFI 文件失败: {error}"));
    }
    let _ = std::fs::remove_file(&backup);
    Ok(())
}

fn deploy_bootex_fonts(source: &Path, destination: &Path) -> Result<(), String> {
    let entries =
        std::fs::read_dir(source).map_err(|error| format!("读取 BootEx 字体目录失败: {error}"))?;
    std::fs::create_dir_all(destination)
        .map_err(|error| format!("创建 ESP 字体目录失败: {error}"))?;
    let mut copied = 0usize;
    for entry in entries {
        let entry = entry.map_err(|error| format!("读取 BootEx 字体失败: {error}"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("读取 BootEx 字体类型失败: {error}"))?;
        if !file_type.is_file() || file_type.is_symlink() {
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "BootEx 字体文件名不是有效 Unicode".to_string())?;
        let Some(target_name) = bootex_font_destination_name(&name) else {
            continue;
        };
        std::fs::copy(entry.path(), destination.join(target_name))
            .map_err(|error| format!("复制 BootEx 字体 {name} 失败: {error}"))?;
        copied += 1;
    }
    if copied == 0 {
        return Err("BootEx 字体目录中没有 *_EX.ttf 文件".to_string());
    }
    Ok(())
}

fn bootex_font_destination_name(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    if !lower.ends_with("_ex.ttf") || name.len() <= "_EX.ttf".len() {
        return None;
    }
    Some(format!("{}.ttf", &name[..name.len() - "_EX.ttf".len()]))
}

fn copy_optional_file_if_missing(source: &Path, destination: &Path) -> Result<(), String> {
    if destination.is_file() {
        return Ok(());
    }
    if !source.is_file() {
        log::info!(
            "[BOOT PCA] 可选 BootEx 资源不存在，保留现有文件: {}",
            source.display()
        );
        return Ok(());
    }
    let parent = destination
        .parent()
        .ok_or_else(|| format!("BootEx 目标没有父目录: {}", destination.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建 BootEx 目标目录失败: {error}"))?;
    std::fs::copy(source, destination)
        .map_err(|error| format!("复制 {} 失败: {error}", source.display()))?;
    Ok(())
}

fn efi_fallback_name(path: &Path) -> Result<&'static str, String> {
    let architecture = inspect_efi_architecture(path)
        .ok_or_else(|| format!("无法识别 EFI 文件架构: {}", path.display()))?;
    match architecture {
        0 => Ok("bootia32.efi"),
        9 => Ok("bootx64.efi"),
        12 => Ok("bootaa64.efi"),
        _ => Err(format!("不支持的 EFI 文件架构: {architecture}")),
    }
}

/// Return the WIM architecture code used by an EFI PE image.
pub fn inspect_efi_architecture(path: &Path) -> Option<u16> {
    let data = std::fs::read(path).ok()?;
    efi_architecture_from_pe(&data)
}

fn efi_architecture_from_pe(data: &[u8]) -> Option<u16> {
    if data.len() < 0x40 || &data[..2] != b"MZ" {
        return None;
    }
    let pe_offset = u32::from_le_bytes(data.get(0x3c..0x40)?.try_into().ok()?) as usize;
    let header = data.get(pe_offset..pe_offset.checked_add(6)?)?;
    if &header[..4] != b"PE\0\0" {
        return None;
    }
    let machine = u16::from_le_bytes([header[4], header[5]]);
    match machine {
        0x014c => Some(0),
        0x8664 => Some(9),
        0xaa64 => Some(12),
        _ => None,
    }
}

fn bcdboot_args<'a>(
    windows_arg: &'a str,
    esp_arg: &'a str,
    generation: PcaGeneration,
    offline: bool,
) -> Vec<&'a str> {
    let mut args = vec![windows_arg, "/s", esp_arg, "/f", "UEFI", "/l", "zh-cn"];
    if offline {
        args.push("/offline");
    }
    if generation == PcaGeneration::Pca2023 {
        args.push("/bootex");
    }
    args
}

fn validate_signature_generation(
    info: &EfiSignatureInfo,
    expected: PcaGeneration,
) -> Result<(), String> {
    if !info.signature_valid {
        return Err(format!(
            "EFI 文件签名无效: {} ({})",
            info.path.display(),
            info.error.as_deref().unwrap_or("未知错误")
        ));
    }
    if info.generation != expected {
        return Err(format!(
            "EFI 签名代际不匹配: {}，期望 {}，实际 {}，签发者 {}",
            info.path.display(),
            expected,
            info.generation,
            info.issuer
        ));
    }
    Ok(())
}

fn generation_from_name(name: &str) -> PcaGeneration {
    let normalized = name.to_ascii_lowercase();
    if normalized.contains("windows uefi ca 2023")
        || normalized.contains("windows production pca 2023")
    {
        PcaGeneration::Pca2023
    } else if normalized.contains("windows production pca 2011") {
        PcaGeneration::Pca2011
    } else {
        PcaGeneration::Unknown
    }
}

#[cfg(windows)]
pub fn inspect_efi_signature(path: &Path) -> EfiSignatureInfo {
    match windows_impl::inspect_authenticode(path) {
        Ok((valid, issuer)) => EfiSignatureInfo {
            generation: generation_from_name(&issuer),
            signature_valid: valid,
            issuer,
            path: path.to_path_buf(),
            error: None,
        },
        Err(error) => EfiSignatureInfo {
            path: path.to_path_buf(),
            error: Some(error),
            ..Default::default()
        },
    }
}

#[cfg(not(windows))]
pub fn inspect_efi_signature(path: &Path) -> EfiSignatureInfo {
    EfiSignatureInfo {
        path: path.to_path_buf(),
        error: Some("EFI 签名检测仅支持 Windows".to_string()),
        ..Default::default()
    }
}

#[cfg(windows)]
pub fn inspect_firmware_pca() -> FirmwarePcaInfo {
    windows_impl::inspect_firmware_pca()
}

#[cfg(not(windows))]
pub fn inspect_firmware_pca() -> FirmwarePcaInfo {
    FirmwarePcaInfo {
        error: Some("UEFI 固件检测仅支持 Windows".to_string()),
        ..Default::default()
    }
}

#[cfg(windows)]
mod windows_impl {
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr::null_mut;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, SetLastError, ERROR_NOT_ALL_ASSIGNED, ERROR_SUCCESS, HANDLE,
    };
    use windows::Win32::Security::Cryptography::{
        CertCloseStore, CertCreateCertificateContext, CertFindCertificateInStore,
        CertFreeCertificateContext, CertGetNameStringW, CryptMsgClose, CryptMsgGetParam,
        CryptQueryObject, CERT_FIND_SUBJECT_CERT, CERT_INFO, CERT_NAME_ISSUER_FLAG,
        CERT_NAME_SIMPLE_DISPLAY_TYPE, CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED_EMBED,
        CERT_QUERY_FORMAT_FLAG_BINARY, CERT_QUERY_OBJECT_FILE, CMSG_SIGNER_INFO,
        CMSG_SIGNER_INFO_PARAM, HCERTSTORE, PKCS_7_ASN_ENCODING, X509_ASN_ENCODING,
    };
    use windows::Win32::Security::WinTrust::{
        WinVerifyTrust, WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0,
        WINTRUST_FILE_INFO, WTD_CHOICE_FILE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE,
        WTD_STATEACTION_VERIFY, WTD_UI_NONE,
    };
    use windows::Win32::Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
        SE_SYSTEM_ENVIRONMENT_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::Win32::System::WindowsProgramming::GetFirmwareEnvironmentVariableExW;

    use super::{generation_from_name, FirmwarePcaInfo, PcaGeneration};

    const EFI_GLOBAL_VARIABLE_GUID: &str = "{8BE4DF61-93CA-11D2-AA0D-00E098032B8C}";
    const EFI_IMAGE_SECURITY_DATABASE_GUID: &str = "{D719B2CB-3D3A-4596-A3BC-DAD00E67656F}";
    const EFI_CERT_X509_GUID_BYTES: [u8; 16] = [
        0xa1, 0x59, 0xc0, 0xa5, 0xe4, 0x94, 0xa7, 0x4a, 0x87, 0xb5, 0xab, 0x15, 0x5c, 0x2b, 0xf0,
        0x72,
    ];

    struct HandleGuard(HANDLE);
    impl Drop for HandleGuard {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    pub(super) fn inspect_authenticode(path: &Path) -> Result<(bool, String), String> {
        if !path.is_file() {
            return Err(format!("文件不存在: {}", path.display()));
        }
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();

        let mut file_info = WINTRUST_FILE_INFO {
            cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
            pcwszFilePath: PCWSTR(wide.as_ptr()),
            ..Default::default()
        };
        let mut trust_data = WINTRUST_DATA {
            cbStruct: size_of::<WINTRUST_DATA>() as u32,
            dwUIChoice: WTD_UI_NONE,
            fdwRevocationChecks: WTD_REVOKE_NONE,
            dwUnionChoice: WTD_CHOICE_FILE,
            Anonymous: WINTRUST_DATA_0 {
                pFile: &mut file_info,
            },
            dwStateAction: WTD_STATEACTION_VERIFY,
            ..Default::default()
        };
        let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
        let status =
            unsafe { WinVerifyTrust(None, &mut action, &mut trust_data as *mut _ as *mut c_void) };
        trust_data.dwStateAction = WTD_STATEACTION_CLOSE;
        unsafe {
            let _ = WinVerifyTrust(None, &mut action, &mut trust_data as *mut _ as *mut c_void);
        }
        if status != 0 {
            return Err(format!("WinVerifyTrust 返回 0x{:08X}", status as u32));
        }

        let issuer = signer_issuer(&wide)?;
        Ok((true, issuer))
    }

    fn signer_issuer(wide_path: &[u16]) -> Result<String, String> {
        let mut store = HCERTSTORE::default();
        let mut message: *mut c_void = null_mut();
        unsafe {
            CryptQueryObject(
                CERT_QUERY_OBJECT_FILE,
                wide_path.as_ptr() as *const c_void,
                CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED_EMBED,
                CERT_QUERY_FORMAT_FLAG_BINARY,
                0,
                None,
                None,
                None,
                Some(&mut store),
                Some(&mut message),
                None,
            )
            .map_err(|e| format!("CryptQueryObject 失败: {e}"))?;
        }

        let result = (|| {
            let mut size = 0u32;
            unsafe {
                CryptMsgGetParam(message, CMSG_SIGNER_INFO_PARAM, 0, None, &mut size)
                    .map_err(|e| format!("读取签名信息大小失败: {e}"))?;
            }
            if size < size_of::<CMSG_SIGNER_INFO>() as u32 {
                return Err("签名信息长度异常".to_string());
            }
            let mut buffer = vec![0u8; size as usize];
            unsafe {
                CryptMsgGetParam(
                    message,
                    CMSG_SIGNER_INFO_PARAM,
                    0,
                    Some(buffer.as_mut_ptr() as *mut c_void),
                    &mut size,
                )
                .map_err(|e| format!("读取签名信息失败: {e}"))?;
            }
            let signer = unsafe { &*(buffer.as_ptr() as *const CMSG_SIGNER_INFO) };
            let cert_info = CERT_INFO {
                Issuer: signer.Issuer,
                SerialNumber: signer.SerialNumber,
                ..Default::default()
            };
            let cert = unsafe {
                CertFindCertificateInStore(
                    store,
                    X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
                    0,
                    CERT_FIND_SUBJECT_CERT,
                    Some(&cert_info as *const _ as *const c_void),
                    None,
                )
            };
            if cert.is_null() {
                return Err("未找到 Authenticode 签名证书".to_string());
            }
            let name = certificate_name(cert, true);
            unsafe {
                let _ = CertFreeCertificateContext(Some(cert));
            }
            name
        })();

        unsafe {
            let _ = CryptMsgClose(Some(message));
            let _ = CertCloseStore(store, 0);
        }
        result
    }

    fn certificate_name(
        context: *const windows::Win32::Security::Cryptography::CERT_CONTEXT,
        issuer: bool,
    ) -> Result<String, String> {
        let flags = if issuer { CERT_NAME_ISSUER_FLAG } else { 0 };
        let size = unsafe {
            CertGetNameStringW(context, CERT_NAME_SIMPLE_DISPLAY_TYPE, flags, None, None)
        };
        if size <= 1 {
            return Err("证书名称为空".to_string());
        }
        let mut buffer = vec![0u16; size as usize];
        unsafe {
            CertGetNameStringW(
                context,
                CERT_NAME_SIMPLE_DISPLAY_TYPE,
                flags,
                None,
                Some(&mut buffer),
            );
        }
        buffer.truncate(buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len()));
        Ok(String::from_utf16_lossy(&buffer))
    }

    pub(super) fn inspect_firmware_pca() -> FirmwarePcaInfo {
        if let Err(error) = enable_system_environment_privilege() {
            return FirmwarePcaInfo {
                error: Some(error),
                ..Default::default()
            };
        }

        let secure_boot_result = read_firmware_variable("SecureBoot", EFI_GLOBAL_VARIABLE_GUID);
        let secure_boot = secure_boot_result
            .as_ref()
            .ok()
            .and_then(|value| value.first().copied())
            .map(|value| value != 0);
        let db = read_firmware_variable("db", EFI_IMAGE_SECURITY_DATABASE_GUID);
        let dbx = read_firmware_variable("dbx", EFI_IMAGE_SECURITY_DATABASE_GUID);

        let db_info = db
            .as_deref()
            .map(inspect_efi_certificate_database)
            .unwrap_or_default();
        let dbx_info = dbx
            .as_deref()
            .map(inspect_efi_certificate_database)
            .unwrap_or_default();

        let trusts_pca2011 = db_info.contains(PcaGeneration::Pca2011);
        let trusts_pca2023 = db_info.contains(PcaGeneration::Pca2023);
        let revokes_pca2011 = dbx_info.contains(PcaGeneration::Pca2011);

        let mut errors = Vec::new();
        if let Err(error) = secure_boot_result {
            errors.push(format!("读取 SecureBoot 失败: {error}"));
        }
        if let Err(error) = db {
            errors.push(format!("读取 db 失败: {error}"));
        }
        if let Err(error) = dbx {
            errors.push(format!("读取 dbx 失败: {error}"));
        }
        FirmwarePcaInfo {
            secure_boot_enabled: secure_boot,
            trusts_pca2011,
            trusts_pca2023,
            revokes_pca2011,
            error: (!errors.is_empty()).then(|| errors.join("; ")),
        }
    }

    fn enable_system_environment_privilege() -> Result<(), String> {
        let mut token = HANDLE::default();
        unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                &mut token,
            )
            .map_err(|e| format!("OpenProcessToken 失败: {e}"))?;
        }
        let _guard = HandleGuard(token);
        let mut luid = Default::default();
        unsafe {
            LookupPrivilegeValueW(PCWSTR::null(), SE_SYSTEM_ENVIRONMENT_NAME, &mut luid)
                .map_err(|e| format!("LookupPrivilegeValueW 失败: {e}"))?;
            let privileges = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES {
                    Luid: luid,
                    Attributes: SE_PRIVILEGE_ENABLED,
                }],
            };
            SetLastError(ERROR_SUCCESS);
            AdjustTokenPrivileges(token, false, Some(&privileges), 0, None, None)
                .map_err(|e| format!("AdjustTokenPrivileges 失败: {e}"))?;
            if GetLastError() == ERROR_NOT_ALL_ASSIGNED {
                return Err("当前进程没有 SeSystemEnvironmentPrivilege".to_string());
            }
        }
        Ok(())
    }

    fn read_firmware_variable(name: &str, guid: &str) -> Result<Vec<u8>, String> {
        let name_wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let guid_wide: Vec<u16> = guid.encode_utf16().chain(Some(0)).collect();
        let mut buffer = vec![0u8; 1024 * 1024];
        let mut attributes = 0u32;
        let size = unsafe {
            GetFirmwareEnvironmentVariableExW(
                PCWSTR(name_wide.as_ptr()),
                PCWSTR(guid_wide.as_ptr()),
                Some(buffer.as_mut_ptr() as *mut c_void),
                buffer.len() as u32,
                Some(&mut attributes),
            )
        };
        if size == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        buffer.truncate(size as usize);
        Ok(buffer)
    }

    #[derive(Default)]
    struct EfiCertificateDatabaseInfo {
        subjects: Vec<String>,
        complete_for_ca_detection: bool,
    }

    impl EfiCertificateDatabaseInfo {
        fn contains(&self, generation: PcaGeneration) -> Option<bool> {
            if self
                .subjects
                .iter()
                .any(|name| generation_from_name(name) == generation)
            {
                Some(true)
            } else {
                self.complete_for_ca_detection.then_some(false)
            }
        }
    }

    fn inspect_efi_certificate_database(data: &[u8]) -> EfiCertificateDatabaseInfo {
        let parsed = parse_efi_database(data);
        let mut result = EfiCertificateDatabaseInfo {
            complete_for_ca_detection: parsed.complete_for_ca_detection,
            ..Default::default()
        };
        for cert in parsed.x509_certificates {
            let context = unsafe {
                CertCreateCertificateContext(X509_ASN_ENCODING | PKCS_7_ASN_ENCODING, cert)
            };
            if context.is_null() {
                result.complete_for_ca_detection = false;
                continue;
            }
            if let Ok(name) = certificate_name(context, false) {
                result.subjects.push(name);
            } else {
                result.complete_for_ca_detection = false;
            }
            unsafe {
                let _ = CertFreeCertificateContext(Some(context));
            }
        }
        result
    }

    struct ParsedEfiDatabase<'a> {
        x509_certificates: Vec<&'a [u8]>,
        complete_for_ca_detection: bool,
    }

    fn parse_efi_database(data: &[u8]) -> ParsedEfiDatabase<'_> {
        let mut certificates = Vec::new();
        let mut offset = 0usize;
        let mut saw_list = false;
        let mut complete = !data.is_empty();
        while offset.checked_add(28).is_some_and(|end| end <= data.len()) {
            let signature_type = &data[offset..offset + 16];
            let list_size = read_u32(data, offset + 16) as usize;
            let header_size = read_u32(data, offset + 20) as usize;
            let signature_size = read_u32(data, offset + 24) as usize;
            if list_size < 28 || signature_size < 16 {
                complete = false;
                break;
            }
            let Some(list_end) = offset.checked_add(list_size) else {
                complete = false;
                break;
            };
            if list_end > data.len() {
                complete = false;
                break;
            }
            let Some(mut entry) = offset
                .checked_add(28)
                .and_then(|value| value.checked_add(header_size))
            else {
                complete = false;
                break;
            };
            if entry > list_end {
                complete = false;
                break;
            }
            saw_list = true;
            if signature_type == EFI_CERT_X509_GUID_BYTES {
                while entry
                    .checked_add(signature_size)
                    .is_some_and(|end| end <= list_end)
                {
                    certificates.push(&data[entry + 16..entry + signature_size]);
                    entry += signature_size;
                }
                if entry != list_end {
                    complete = false;
                }
            } else {
                // UEFI db/dbx may also contain RSA keys and image hashes. We
                // cannot infer CA absence from entries whose payload is not an
                // X.509 certificate, so preserve an unknown tri-state result.
                complete = false;
            }
            offset = list_end;
        }
        if !saw_list || offset != data.len() {
            complete = false;
        }
        ParsedEfiDatabase {
            x509_certificates: certificates,
            complete_for_ca_detection: complete,
        }
    }

    fn read_u32(data: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ])
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_x509_entries_and_rejects_truncated_lists() {
            let cert = [0x30, 0x03, 0x01, 0x02, 0x03];
            let signature_size = 16 + cert.len();
            let list_size = 28 + signature_size;
            let mut data = Vec::new();
            data.extend_from_slice(&EFI_CERT_X509_GUID_BYTES);
            data.extend_from_slice(&(list_size as u32).to_le_bytes());
            data.extend_from_slice(&0u32.to_le_bytes());
            data.extend_from_slice(&(signature_size as u32).to_le_bytes());
            data.extend_from_slice(&[0u8; 16]);
            data.extend_from_slice(&cert);
            let parsed = parse_efi_database(&data);
            assert_eq!(parsed.x509_certificates, vec![cert.as_slice()]);
            assert!(parsed.complete_for_ca_detection);

            data.pop();
            let parsed = parse_efi_database(&data);
            assert!(parsed.x509_certificates.is_empty());
            assert!(!parsed.complete_for_ca_detection);
        }

        #[test]
        fn unsupported_database_entries_keep_ca_absence_unknown() {
            let signature_size = 32usize;
            let list_size = 28 + signature_size;
            let mut data = vec![0x55; 16];
            data.extend_from_slice(&(list_size as u32).to_le_bytes());
            data.extend_from_slice(&0u32.to_le_bytes());
            data.extend_from_slice(&(signature_size as u32).to_le_bytes());
            data.extend_from_slice(&[0u8; 32]);

            let parsed = parse_efi_database(&data);
            assert!(parsed.x509_certificates.is_empty());
            assert!(!parsed.complete_for_ca_detection);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io;
    use std::sync::Mutex;

    use super::*;

    struct SequenceExecutor {
        outcomes: Mutex<VecDeque<crate::command::CommandOutcome>>,
        requests: Mutex<Vec<CommandRequest>>,
    }

    impl SequenceExecutor {
        fn new(outcomes: Vec<crate::command::CommandOutcome>) -> Self {
            Self {
                outcomes: Mutex::new(outcomes.into()),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<CommandRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl CommandExecutor for SequenceExecutor {
        fn execute(&self, request: &CommandRequest) -> io::Result<crate::command::CommandOutcome> {
            self.requests.lock().unwrap().push(request.clone());
            self.outcomes
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| io::Error::other("no modeled bcdboot outcome remains"))
        }
    }

    fn signature(generation: PcaGeneration) -> EfiSignatureInfo {
        EfiSignatureInfo {
            generation,
            signature_valid: true,
            ..Default::default()
        }
    }

    fn assessment() -> BootPcaAssessment {
        BootPcaAssessment {
            firmware: FirmwarePcaInfo {
                secure_boot_enabled: Some(true),
                trusts_pca2011: Some(true),
                trusts_pca2023: Some(true),
                revokes_pca2011: Some(false),
                error: None,
            },
            existing_esp: PcaGeneration::Pca2011,
            sources: WindowsBootSources {
                pca2011: Some(signature(PcaGeneration::Pca2011)),
                pca2023: Some(signature(PcaGeneration::Pca2023)),
            },
        }
    }

    #[test]
    fn auto_prefers_2023_when_secure_boot_firmware_trusts_both_generations() {
        let result = resolve_pca_mode(BootPcaMode::Auto, &assessment()).unwrap();
        assert_eq!(result.generation, PcaGeneration::Pca2023);
        assert_eq!(result.reason, "安全启动已开启且固件信任 PCA2023");
    }

    #[test]
    fn auto_preserves_compatible_existing_esp_when_secure_boot_is_disabled() {
        let mut input = assessment();
        input.firmware.secure_boot_enabled = Some(false);
        let result = resolve_pca_mode(BootPcaMode::Auto, &input).unwrap();
        assert_eq!(result.generation, PcaGeneration::Pca2011);
    }

    #[test]
    fn revoked_2011_forces_2023() {
        let mut input = assessment();
        input.firmware.revokes_pca2011 = Some(true);
        let result = resolve_pca_mode(BootPcaMode::Auto, &input).unwrap();
        assert_eq!(result.generation, PcaGeneration::Pca2023);
    }

    #[test]
    fn explicit_incompatible_mode_is_rejected() {
        let mut input = assessment();
        input.firmware.revokes_pca2011 = Some(true);
        let error = resolve_pca_mode(BootPcaMode::Pca2011, &input).unwrap_err();
        assert!(error.contains("撤销 PCA2011"));
    }

    #[test]
    fn old_firmware_rejects_2023_when_secure_boot_is_on() {
        let mut input = assessment();
        input.firmware.trusts_pca2023 = Some(false);
        let error = resolve_pca_mode(BootPcaMode::Pca2023, &input).unwrap_err();
        assert!(error.contains("不信任"));
    }

    #[test]
    fn config_values_are_backward_compatible() {
        assert_eq!(BootPcaMode::from_config_value(""), BootPcaMode::Auto);
        assert_eq!(BootPcaMode::from_config_value("2011"), BootPcaMode::Pca2011);
        assert_eq!(
            BootPcaMode::from_config_value("pca2023"),
            BootPcaMode::Pca2023
        );
    }

    #[test]
    fn drive_letter_selection_uses_highest_free_letter() {
        assert_eq!(find_available_drive_letter_in_mask(0), Some('Z'));
        assert_eq!(
            find_available_drive_letter_in_mask(1u32 << (b'Z' - b'A')),
            Some('Y')
        );
        assert_eq!(find_available_drive_letter_in_mask(u32::MAX), None);
        assert_eq!(normalize_drive_letter("z:\\"), Some('Z'));
        assert_eq!(normalize_drive_letter("S:"), Some('S'));
        assert_eq!(normalize_drive_letter("system"), None);
    }

    #[test]
    fn auto_prefers_2023_only_when_it_is_supported_and_no_existing_choice_survives() {
        let mut input = assessment();
        input.existing_esp = PcaGeneration::Unknown;
        let result = resolve_pca_mode(BootPcaMode::Auto, &input).unwrap();
        assert_eq!(result.generation, PcaGeneration::Pca2023);

        input.firmware.trusts_pca2023 = None;
        let result = resolve_pca_mode(BootPcaMode::Auto, &input).unwrap();
        assert_eq!(result.generation, PcaGeneration::Pca2011);
    }

    #[test]
    fn revoked_2011_without_a_2023_source_fails_closed() {
        let mut input = assessment();
        input.firmware.revokes_pca2011 = Some(true);
        input.sources.pca2023 = None;
        let error = resolve_pca_mode(BootPcaMode::Auto, &input).unwrap_err();
        assert!(error.contains("缺少有效的 PCA2023"));
    }

    #[test]
    fn auto_does_not_assume_unknown_2023_firmware_trust() {
        let mut input = assessment();
        input.existing_esp = PcaGeneration::Unknown;
        input.sources.pca2011 = None;
        input.firmware.trusts_pca2023 = None;

        let error = resolve_pca_mode(BootPcaMode::Auto, &input).unwrap_err();
        assert!(error.contains("无法确认固件信任"));

        input.firmware.secure_boot_enabled = Some(false);
        let result = resolve_pca_mode(BootPcaMode::Auto, &input).unwrap();
        assert_eq!(result.generation, PcaGeneration::Pca2023);
    }

    #[test]
    fn revocation_requires_confirmed_2023_trust_in_auto_mode() {
        let mut input = assessment();
        input.firmware.revokes_pca2011 = Some(true);
        input.firmware.trusts_pca2023 = None;

        let error = resolve_pca_mode(BootPcaMode::Auto, &input).unwrap_err();
        assert!(error.contains("已撤销 PCA2011"));
        assert!(error.contains("无法确认"));
    }

    #[test]
    fn compatibility_retry_keeps_the_requested_generation() {
        let pca2011 = bcdboot_args("W:\\Windows", "S:", PcaGeneration::Pca2011, false);
        assert!(!pca2011.contains(&"/offline"));
        assert!(!pca2011.contains(&"/bootex"));

        let pca2023 = bcdboot_args("W:\\Windows", "S:", PcaGeneration::Pca2023, false);
        assert!(!pca2023.contains(&"/offline"));
        assert!(pca2023.contains(&"/bootex"));

        let offline_2023 = bcdboot_args("W:\\Windows", "S:", PcaGeneration::Pca2023, true);
        assert!(offline_2023.contains(&"/offline"));
        assert!(offline_2023.contains(&"/bootex"));
    }

    #[test]
    fn maps_bootex_font_names_without_accepting_unrelated_files() {
        assert_eq!(
            bootex_font_destination_name("segoe_slboot_EX.ttf").as_deref(),
            Some("segoe_slboot.ttf")
        );
        assert!(bootex_font_destination_name("normal.ttf").is_none());
        assert!(bootex_font_destination_name("_EX.ttf").is_none());
    }

    #[test]
    fn derives_the_uefi_fallback_name_from_the_pe_machine() {
        fn image(machine: u16) -> Vec<u8> {
            let mut bytes = vec![0u8; 0x86];
            bytes[..2].copy_from_slice(b"MZ");
            bytes[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
            bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
            bytes[0x84..0x86].copy_from_slice(&machine.to_le_bytes());
            bytes
        }
        assert_eq!(efi_architecture_from_pe(&image(0x014c)), Some(0));
        assert_eq!(efi_architecture_from_pe(&image(0x8664)), Some(9));
        assert_eq!(efi_architecture_from_pe(&image(0xaa64)), Some(12));
        assert_eq!(efi_architecture_from_pe(&image(0xffff)), None);
    }

    #[test]
    fn old_bcdboot_falls_back_to_standard_layout_before_manual_bootex_copy() {
        let failure = || {
            crate::command::CommandOutcome::new(
                Some(87),
                Vec::new(),
                b"unsupported option".to_vec(),
            )
        };
        let executor = SequenceExecutor::new(vec![
            failure(),
            failure(),
            crate::command::CommandOutcome::success(),
        ]);

        let mode = run_bcdboot_with_executor(
            &executor,
            Path::new("bcdboot.exe"),
            "W:\\Windows",
            "S:",
            PcaGeneration::Pca2023,
        )
        .unwrap();

        assert_eq!(mode, BcdbootWriteMode::ManualBootexCopy);
        let requests = executor.requests();
        assert_eq!(requests.len(), 3);
        let arguments = |index: usize| {
            requests[index]
                .arguments()
                .iter()
                .map(|value| value.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        let first = arguments(0);
        assert!(first.iter().any(|arg| arg == "/offline"));
        assert!(first.iter().any(|arg| arg == "/bootex"));
        let second = arguments(1);
        assert!(!second.iter().any(|arg| arg == "/offline"));
        assert!(second.iter().any(|arg| arg == "/bootex"));
        let third = arguments(2);
        assert!(!third.iter().any(|arg| arg == "/offline"));
        assert!(!third.iter().any(|arg| arg == "/bootex"));
    }

    #[cfg(windows)]
    #[test]
    fn reads_the_installed_windows_boot_manager_signature() {
        let path = Path::new(r"C:\Windows\Boot\EFI\bootmgfw.efi");
        if !path.is_file() {
            return;
        }
        let info = inspect_efi_signature(path);
        assert!(info.signature_valid, "{:?}", info.error);
        assert_ne!(info.generation, PcaGeneration::Unknown, "{}", info.issuer);
    }

    #[cfg(windows)]
    #[test]
    fn firmware_inspection_never_silently_loses_secure_boot_state() {
        let info = inspect_firmware_pca();
        assert!(info.secure_boot_enabled.is_some() || info.error.is_some());
    }
}
