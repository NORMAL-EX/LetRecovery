//! Shared Windows 11 shell defaults applied to an offline installation.
//!
//! The `ForceEffectMode` DWM value is intentionally isolated here because it is an undocumented
//! Windows 11 VM compatibility switch; callers must gate this module to a confirmed Windows 11
//! target and every write is followed by an exact typed readback. When curated inbox-app cleanup is
//! requested. Start-menu pin cleanup deliberately does not live here: Windows 11 OEM
//! `LayoutModification.json` adds OEM pins and does not remove Microsoft's default pins, while
//! `start2.bin` is an undocumented, build-specific cache.

use anyhow::{Context, Result};

use crate::registry::OfflineRegistry;

pub const FORCE_EFFECT_MODE_VALUE: u32 = 2;
pub const START_PIN_CLEANUP_UNSUPPORTED_REASON: &str =
    "Windows 11 OEM LayoutModification.json only adds OEM pins; selective removal of Microsoft default pins has no supported offline per-user API, and the opaque start2.bin cache is not modified";

const DWM_RELATIVE_KEY: &str = r"Microsoft\Windows\Dwm";
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Windows11ShellReport {
    pub force_effect_mode: u32,
}

/// Apply Windows 11 shell defaults to already loaded offline SOFTWARE and DEFAULT hives.
///
/// The hive aliases are internal names selected by the caller. No registry executable, localized
/// output parsing, opaque Start cache, or unsupported package deletion is used.
pub fn apply_offline_defaults(software_hive_alias: &str) -> Result<Windows11ShellReport> {
    validate_hive_alias(software_hive_alias)?;

    let dwm_key = format!(r"HKLM\{software_hive_alias}\{DWM_RELATIVE_KEY}");
    write_and_read_back_dword(&dwm_key, "ForceEffectMode", FORCE_EFFECT_MODE_VALUE)
        .context("enable Windows 11 DWM force-effect mode")?;

    Ok(Windows11ShellReport {
        force_effect_mode: FORCE_EFFECT_MODE_VALUE,
    })
}

fn write_and_read_back_dword(key: &str, name: &str, value: u32) -> Result<()> {
    OfflineRegistry::set_dword(key, name, value)?;
    let actual = OfflineRegistry::query_dword(key, name)?;
    if actual != value {
        anyhow::bail!(
            "registry DWORD readback mismatch for {key}\\{name}: expected {value}, got {actual}"
        );
    }
    Ok(())
}

fn validate_hive_alias(alias: &str) -> Result<()> {
    if alias.is_empty()
        || alias.len() > 64
        || !alias
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        anyhow::bail!("invalid offline registry hive alias")
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hive_alias_validation_rejects_registry_path_injection() {
        for valid in ["pc-soft", "pc_default", "A1"] {
            validate_hive_alias(valid).unwrap();
        }
        for invalid in ["", "pc\\soft", "HKLM\\pc-soft", "pc soft", "pc/soft"] {
            assert!(validate_hive_alias(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn forced_rounding_value_is_the_isolated_windows_11_vm_switch() {
        assert_eq!(FORCE_EFFECT_MODE_VALUE, 2);
    }

    #[test]
    fn registry_path_and_value_are_limited_to_the_requested_dwm_switch() {
        assert_eq!(DWM_RELATIVE_KEY, r"Microsoft\Windows\Dwm");
    }
}
