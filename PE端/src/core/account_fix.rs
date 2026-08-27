//! Offline image account-state inspection.
//!
//! Windows documents `ImageState` as the supported way to distinguish a deployable image from an
//! installation whose specialize and oobeSystem passes have already completed. SAM inventory is
//! only supporting evidence: stock images already contain well-known built-in RIDs and may contain
//! setup-owned identities such as `defaultuser0` at RID 1000 or above. A documented OOBE-resealed
//! state is deployable only after SAM inventory succeeds and confirms there is no user-owned local
//! account. A SAM read failure is indeterminate and disables all account/unattended mutations.
//!
//! This module is deliberately read-only. Restoring a captured installation must preserve its
//! account database byte-for-byte; the install workflow must never clear passwords or enable
//! accounts merely to make an unattended file appear to work.

use anyhow::Result;

use crate::core::registry::OfflineRegistry;

const FIRST_NON_BUILTIN_LOCAL_RID: u32 = 1000;
const BUILTIN_ADMINISTRATOR_RID: u32 = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OfflineImageAccountMode {
    /// Windows Setup is explicitly resealed to OOBE and no ordinary local account exists.
    FreshDeployable,
    /// A completed/captured installation or an image containing an ordinary local account.
    PreserveExistingAccounts,
    /// Evidence was incomplete or contradictory. Installation may continue, but account and
    /// unattended mutations must remain disabled.
    Indeterminate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfflineImageAccountInspection {
    pub mode: OfflineImageAccountMode,
    pub image_state: Option<String>,
    pub local_account_count: usize,
    pub ordinary_local_account_count: usize,
    pub builtin_administrator_name: Option<String>,
    pub diagnostic: String,
}

impl OfflineImageAccountInspection {
    pub fn allows_new_install_unattended(&self) -> bool {
        self.mode == OfflineImageAccountMode::FreshDeployable
    }
}

fn with_loaded_hive<T>(name: &str, path: &str, action: impl FnOnce() -> Result<T>) -> Result<T> {
    OfflineRegistry::load_hive(name, path)?;
    let action_result = action();
    let unload_result = OfflineRegistry::unload_hive(name);
    match (action_result, unload_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(action_error), Err(unload_error)) => anyhow::bail!(
            "{}; additionally failed to unload offline hive {name}: {unload_error}",
            action_error
        ),
    }
}

fn read_offline_image_state(target_partition: &str) -> Result<String> {
    let software_hive = format!("{}\\Windows\\System32\\config\\SOFTWARE", target_partition);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let hive_name = format!("LR_STATE_{}_{}", std::process::id(), nonce);
    with_loaded_hive(&hive_name, &software_hive, || {
        OfflineRegistry::query_string(
            &format!(
                "HKLM\\{}\\Microsoft\\Windows\\CurrentVersion\\Setup\\State",
                hive_name
            ),
            "ImageState",
        )
    })
}

fn parse_rid(rid: &str) -> Option<u32> {
    (rid.len() == 8)
        .then(|| u32::from_str_radix(rid, 16).ok())
        .flatten()
}

fn is_user_owned_local_account(account: &lr_core::sam::SamAccount) -> bool {
    parse_rid(&account.rid).is_some_and(|rid| rid >= FIRST_NON_BUILTIN_LOCAL_RID)
        && !lr_core::unattend_account::is_windows_owned_local_account_name(&account.username)
}

fn is_resealed_to_oobe(image_state: &str) -> bool {
    matches!(
        image_state.trim().to_ascii_uppercase().as_str(),
        "IMAGE_STATE_GENERALIZE_RESEAL_TO_OOBE" | "IMAGE_STATE_SPECIALIZE_RESEAL_TO_OOBE"
    )
}

fn is_complete_installation(image_state: &str) -> bool {
    image_state
        .trim()
        .eq_ignore_ascii_case("IMAGE_STATE_COMPLETE")
}

fn classify_evidence(
    image_state: Result<String>,
    accounts: Result<Vec<lr_core::sam::SamAccount>>,
    source_is_capture: bool,
) -> OfflineImageAccountInspection {
    let state_error = image_state.as_ref().err().map(ToString::to_string);
    let account_error = accounts.as_ref().err().map(ToString::to_string);
    let state = image_state.ok();
    let accounts = accounts.ok();
    let local_account_count = accounts.as_ref().map_or(0, Vec::len);
    let ordinary_local_account_count = accounts.as_ref().map_or(0, |items| {
        items
            .iter()
            .filter(|account| is_user_owned_local_account(account))
            .count()
    });
    let builtin_administrator_name = accounts.as_ref().and_then(|items| {
        items
            .iter()
            .find(|account| parse_rid(&account.rid) == Some(BUILTIN_ADMINISTRATOR_RID))
            .map(|account| account.username.clone())
    });

    let (mode, diagnostic) = if source_is_capture {
        (
            OfflineImageAccountMode::PreserveExistingAccounts,
            "the selected source format is a captured/restored installation".to_string(),
        )
    } else if ordinary_local_account_count != 0 {
        (
            OfflineImageAccountMode::PreserveExistingAccounts,
            format!("SAM contains {ordinary_local_account_count} user-owned local account(s)"),
        )
    } else if state.as_deref().is_some_and(is_resealed_to_oobe) && accounts.is_some() {
        (
            OfflineImageAccountMode::FreshDeployable,
            "ImageState is resealed to OOBE and verified SAM inventory contains no user-owned local account"
                .to_string(),
        )
    } else if state.as_deref().is_some_and(is_complete_installation) {
        (
            OfflineImageAccountMode::PreserveExistingAccounts,
            "ImageState is IMAGE_STATE_COMPLETE".to_string(),
        )
    } else {
        let mut evidence = Vec::new();
        if let Some(state) = &state {
            evidence.push(format!("unsupported ImageState={state}"));
        }
        if let Some(error) = state_error {
            evidence.push(format!("ImageState unavailable: {error}"));
        }
        if let Some(error) = account_error {
            evidence.push(format!("SAM inventory unavailable: {error}"));
        }
        if evidence.is_empty() {
            evidence.push("offline account evidence is incomplete".to_string());
        }
        (OfflineImageAccountMode::Indeterminate, evidence.join("; "))
    };

    OfflineImageAccountInspection {
        mode,
        image_state: state,
        local_account_count,
        ordinary_local_account_count,
        builtin_administrator_name,
        diagnostic,
    }
}

/// Inspect the applied offline Windows instance without modifying SOFTWARE, SYSTEM, or SAM.
pub fn inspect_offline_image_accounts(
    target_partition: &str,
    source_is_capture: bool,
) -> OfflineImageAccountInspection {
    classify_evidence(
        read_offline_image_state(target_partition),
        lr_core::sam::list_accounts(target_partition),
        source_is_capture,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(rid: &str, username: &str) -> lr_core::sam::SamAccount {
        lr_core::sam::SamAccount {
            username: username.to_string(),
            rid: rid.to_string(),
            disabled: false,
        }
    }

    #[test]
    fn stock_builtin_accounts_do_not_turn_a_resealed_image_into_a_backup() {
        let inspection = classify_evidence(
            Ok("IMAGE_STATE_GENERALIZE_RESEAL_TO_OOBE".to_string()),
            Ok(vec![
                account("000001F4", "Administrator"),
                account("000001F5", "Guest"),
                account("000001F7", "DefaultAccount"),
                account("000003E8", "defaultuser0"),
            ]),
            false,
        );
        assert_eq!(inspection.mode, OfflineImageAccountMode::FreshDeployable);
        assert_eq!(
            inspection.builtin_administrator_name.as_deref(),
            Some("Administrator")
        );
        assert_eq!(inspection.ordinary_local_account_count, 0);
    }

    #[test]
    fn microsoft_oobe_resealed_state_requires_successful_sam_inventory() {
        // Exact ImageState observed in Microsoft's 28000.2113 zh-CN Client Pro ISO supplied for
        // this regression. Microsoft documents this state as generalized and ready to continue to
        // OOBE. The image-state evidence is valid, but this policy still requires a successful SAM
        // inventory before enabling account and unattended mutations.
        let inspection = classify_evidence(
            Ok("IMAGE_STATE_GENERALIZE_RESEAL_TO_OOBE".to_string()),
            Err(anyhow::anyhow!("offline SAM inventory unavailable")),
            false,
        );
        assert_eq!(inspection.mode, OfflineImageAccountMode::Indeterminate);
        assert!(inspection.diagnostic.contains("SAM inventory unavailable"));
    }

    #[test]
    fn genuine_user_account_overrides_oobe_resealed_state() {
        let inspection = classify_evidence(
            Ok("IMAGE_STATE_GENERALIZE_RESEAL_TO_OOBE".to_string()),
            Ok(vec![account("000003E8", "ExistingUser")]),
            false,
        );
        assert_eq!(
            inspection.mode,
            OfflineImageAccountMode::PreserveExistingAccounts
        );
        assert_eq!(inspection.ordinary_local_account_count, 1);
    }

    #[test]
    fn rid_and_windows_owned_name_jointly_distinguish_system_and_user_accounts() {
        for system in [
            account("000001F4", "LocalizedAdministrator"),
            account("000001F5", "LocalizedGuest"),
            account("000003E8", "defaultuser0"),
            account("000003E9", "WDAGUtilityAccount"),
            account("000003EA", "DWM-12"),
            account("000003EB", "UMFD-0"),
        ] {
            assert!(!is_user_owned_local_account(&system), "{}", system.username);
        }
        assert!(is_user_owned_local_account(&account(
            "000003E8",
            "ActualUser"
        )));
        assert!(is_user_owned_local_account(&account(
            "000003E8",
            "Administrator"
        )));
        assert!(is_user_owned_local_account(&account("000003E9", "NONE")));
    }

    #[test]
    fn complete_image_preserves_accounts_even_when_sam_inventory_is_unavailable() {
        let inspection = classify_evidence(
            Ok("IMAGE_STATE_COMPLETE".to_string()),
            Err(anyhow::anyhow!("locked SAM")),
            false,
        );
        assert_eq!(
            inspection.mode,
            OfflineImageAccountMode::PreserveExistingAccounts
        );
    }

    #[test]
    fn ordinary_local_account_preserves_accounts_even_if_setup_state_is_unavailable() {
        let inspection = classify_evidence(
            Err(anyhow::anyhow!("missing state")),
            Ok(vec![account("000003E8", "ExistingUser")]),
            false,
        );
        assert_eq!(
            inspection.mode,
            OfflineImageAccountMode::PreserveExistingAccounts
        );
        assert_eq!(inspection.ordinary_local_account_count, 1);
    }

    #[test]
    fn incomplete_or_conflicting_evidence_disables_unattended_without_claiming_backup() {
        let inspection = classify_evidence(
            Ok("IMAGE_STATE_UNDEPLOYABLE".to_string()),
            Ok(vec![account("000001F4", "Administrator")]),
            false,
        );
        assert_eq!(inspection.mode, OfflineImageAccountMode::Indeterminate);
        assert!(!inspection.allows_new_install_unattended());
    }

    #[test]
    fn captured_source_format_always_preserves_the_existing_account_database() {
        let inspection = classify_evidence(
            Ok("IMAGE_STATE_GENERALIZE_RESEAL_TO_OOBE".to_string()),
            Ok(vec![]),
            true,
        );
        assert_eq!(
            inspection.mode,
            OfflineImageAccountMode::PreserveExistingAccounts
        );
    }
}
