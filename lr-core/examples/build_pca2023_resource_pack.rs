//! Build and validate a minimal offline PCA2023 compatibility resource WIM.
//!
//! This developer tool captures an ordinary directory with the project's
//! bundled wimlib. It never mounts or services a Windows image.

#[cfg(windows)]
use std::collections::BTreeSet;
#[cfg(windows)]
use std::env;
#[cfg(windows)]
use std::fs;
#[cfg(windows)]
use std::path::{Path, PathBuf};

#[cfg(windows)]
use lr_core::boot_pca::{inspect_efi_architecture, inspect_efi_signature, PcaGeneration};
#[cfg(windows)]
use lr_core::hash::sha256_file;
#[cfg(windows)]
use lr_core::pca_compat::validate_offline_asset_package;
#[cfg(windows)]
use lr_core::wimlib::WimlibManager;

#[cfg(windows)]
const REQUIRED_FONTS: [&str; 16] = [
    "chs_boot_ex.ttf",
    "cht_boot_ex.ttf",
    "jpn_boot_ex.ttf",
    "kor_boot_ex.ttf",
    "malgunn_boot_ex.ttf",
    "malgun_boot_ex.ttf",
    "meiryon_boot_ex.ttf",
    "meiryo_boot_ex.ttf",
    "msjhn_boot_ex.ttf",
    "msjh_boot_ex.ttf",
    "msyhn_boot_ex.ttf",
    "msyh_boot_ex.ttf",
    "segmono_boot_ex.ttf",
    "segoen_slboot_ex.ttf",
    "segoe_slboot_ex.ttf",
    "wgl4_boot_ex.ttf",
];

#[cfg(windows)]
fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("error: PCA2023 resource pack building is supported only on Windows");
    std::process::exit(1);
}

#[cfg(windows)]
fn run() -> Result<(), String> {
    let mut arguments = env::args_os().skip(1);
    let source = arguments.next().map(PathBuf::from).ok_or_else(usage)?;
    let output = arguments.next().map(PathBuf::from).ok_or_else(usage)?;
    let architecture = arguments
        .next()
        .and_then(|value| value.to_str().map(str::to_owned))
        .ok_or_else(usage)?;
    let force = match arguments.next() {
        None => false,
        Some(value) if value == "--force" => true,
        Some(_) => return Err(usage()),
    };
    if arguments.next().is_some() {
        return Err(usage());
    }
    let expected_architecture = match architecture.as_str() {
        "x86" => 0,
        "amd64" => 9,
        _ => return Err("architecture must be x86 or amd64".to_string()),
    };

    let source = source
        .canonicalize()
        .map_err(|error| format!("cannot resolve source directory: {error}"))?;
    if !source.is_dir() {
        return Err(format!("source is not a directory: {}", source.display()));
    }
    let output = absolute_path(&output)?;
    if output.starts_with(&source) {
        return Err("output WIM must not be inside the captured source directory".to_string());
    }
    if !output
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("wim"))
    {
        return Err("output must use the .wim extension".to_string());
    }
    if output.exists() && !force {
        return Err(format!(
            "output already exists (pass --force to replace it): {}",
            output.display()
        ));
    }

    validate_source_tree(&source, expected_architecture)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create output directory: {error}"))?;
    }
    if output.exists() {
        fs::remove_file(&output)
            .map_err(|error| format!("cannot replace existing output: {error}"))?;
    }

    lr_core::ensure_dll_available();
    let manager = WimlibManager::new()?;
    let capture_result = manager.capture_image(
        &source.to_string_lossy(),
        &output.to_string_lossy(),
        &format!("LetRecovery PCA2023 {architecture}"),
        "Offline Microsoft BootEx compatibility resources",
        2,
        None,
    );
    if let Err(error) = capture_result {
        let _ = fs::remove_file(&output);
        return Err(format!("wimlib capture failed: {error}"));
    }
    if let Err(error) = validate_offline_asset_package(&output, expected_architecture) {
        let _ = fs::remove_file(&output);
        return Err(format!("generated package validation failed: {error}"));
    }

    let sha256 = sha256_file(&output, |_| {}).map_err(|error| error.to_string())?;
    println!("created: {}", output.display());
    println!("sha256: {sha256}");
    Ok(())
}

#[cfg(windows)]
fn validate_source_tree(source: &Path, expected_architecture: u16) -> Result<(), String> {
    let boot = source.join("Windows").join("Boot");
    let efi_ex = find_child_case_insensitive(&boot, "EFI_EX")?;
    let fonts_ex = find_child_case_insensitive(&boot, "FONTS_EX")?;
    let bootmgfw = find_child_case_insensitive(&efi_ex, "bootmgfw_EX.efi")?;
    let bootmgr = find_child_case_insensitive(&efi_ex, "bootmgr_EX.efi")?;

    let signature = inspect_efi_signature(&bootmgfw);
    if !signature.signature_valid || signature.generation != PcaGeneration::Pca2023 {
        return Err(format!(
            "bootmgfw_EX.efi is not valid PCA2023-signed Microsoft code: issuer={}, error={:?}",
            signature.issuer, signature.error
        ));
    }
    if inspect_efi_architecture(&bootmgfw) != Some(expected_architecture) {
        return Err(format!(
            "bootmgfw_EX.efi has the wrong architecture; expected WIM architecture {expected_architecture}"
        ));
    }
    let bootmgr_signature = inspect_efi_signature(&bootmgr);
    if !bootmgr_signature.signature_valid
        || !bootmgr_signature
            .issuer
            .to_ascii_lowercase()
            .contains("microsoft")
    {
        return Err(format!(
            "bootmgr_EX.efi does not have a valid Microsoft signature: issuer={}, error={:?}",
            bootmgr_signature.issuer, bootmgr_signature.error
        ));
    }
    if inspect_efi_architecture(&bootmgr) != Some(expected_architecture) {
        return Err(format!(
            "bootmgr_EX.efi has the wrong architecture; expected WIM architecture {expected_architecture}"
        ));
    }

    let boot_stl = boot.join("EFI").join("boot.stl");
    if boot_stl.exists() {
        let metadata = fs::symlink_metadata(&boot_stl)
            .map_err(|error| format!("cannot inspect boot.stl: {error}"))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 64 * 1024 {
            return Err("boot.stl must be a regular file no larger than 64 KiB".to_string());
        }
    }

    let fonts = fs::read_dir(&fonts_ex)
        .map_err(|error| format!("cannot read FONTS_EX: {error}"))?
        .map(|entry| {
            entry.map_err(|error| error.to_string()).and_then(|entry| {
                if !entry
                    .file_type()
                    .map_err(|error| error.to_string())?
                    .is_file()
                {
                    return Err(format!(
                        "FONTS_EX contains a non-file entry: {}",
                        entry.path().display()
                    ));
                }
                let path = entry.path();
                let bytes = fs::read(&path)
                    .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
                if !is_sfnt_font(&bytes) {
                    return Err(format!(
                        "FONTS_EX contains an invalid SFNT font: {}",
                        path.display()
                    ));
                }
                Ok(entry.file_name().to_string_lossy().to_ascii_lowercase())
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    let required = REQUIRED_FONTS
        .iter()
        .map(|name| (*name).to_string())
        .collect::<BTreeSet<_>>();
    if fonts != required {
        return Err(format!(
            "FONTS_EX must contain exactly the 16 approved BootEx fonts; found {fonts:?}"
        ));
    }

    let mut allowed = required
        .into_iter()
        .map(|name| format!("windows/boot/fonts_ex/{name}"))
        .collect::<BTreeSet<_>>();
    allowed.insert("windows/boot/efi_ex/bootmgfw_ex.efi".to_string());
    allowed.insert("windows/boot/efi_ex/bootmgr_ex.efi".to_string());
    allowed.insert("windows/boot/efi/boot.stl".to_string());

    for entry in walkdir::WalkDir::new(source).follow_links(false) {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry.file_type().is_symlink() {
            return Err(format!(
                "symbolic links are not allowed: {}",
                entry.path().display()
            ));
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(source)
            .map_err(|error| error.to_string())?
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        if !allowed.contains(&relative) {
            return Err(format!(
                "source contains a non-whitelisted file: {relative}"
            ));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn find_child_case_insensitive(parent: &Path, name: &str) -> Result<PathBuf, String> {
    for entry in fs::read_dir(parent)
        .map_err(|error| format!("cannot read {}: {error}", parent.display()))?
    {
        let entry =
            entry.map_err(|error| format!("cannot enumerate {}: {error}", parent.display()))?;
        if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(name)
        {
            return Ok(entry.path());
        }
    }
    Err(format!(
        "missing required resource: {}/{}",
        parent.display(),
        name
    ))
}

#[cfg(windows)]
fn is_sfnt_font(bytes: &[u8]) -> bool {
    bytes.get(..4).is_some_and(|magic| {
        magic == b"\0\x01\0\0" || magic == b"OTTO" || magic == b"ttcf" || magic == b"true"
    })
}

#[cfg(windows)]
fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        env::current_dir()
            .map(|current| current.join(path))
            .map_err(|error| format!("cannot resolve output path: {error}"))
    }
}

#[cfg(windows)]
fn usage() -> String {
    "usage: cargo run -p lr-core --example build_pca2023_resource_pack -- <source-directory> <output.wim> <x86|amd64> [--force]".to_string()
}
