//! BitLocker safety gate shared by the native install and backup flows.
//!
//! Planning and credential validation are pure. The only method allowed to create a
//! `BitLockerManager` is `execute_unlock`, which is disabled before any host I/O in
//! non-elevated development builds.

use super::bitlocker::{UnlockResult, VolumeStatus};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BitLockerVolumeSnapshot {
    pub drive: String,
    pub status: VolumeStatus,
}

impl From<&super::disk::Partition> for BitLockerVolumeSnapshot {
    fn from(partition: &super::disk::Partition) -> Self {
        Self {
            drive: partition.letter.clone(),
            status: partition.bitlocker_status,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitLockerCredentialKind {
    Password,
    RecoveryKey,
}

#[derive(Clone, PartialEq, Eq)]
pub enum BitLockerCredential {
    Password(String),
    RecoveryKey(String),
}

impl std::fmt::Debug for BitLockerCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple(match self {
                Self::Password(_) => "Password",
                Self::RecoveryKey(_) => "RecoveryKey",
            })
            .field(&"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ValidatedBitLockerCredential {
    kind: BitLockerCredentialKind,
    secret: String,
}

impl ValidatedBitLockerCredential {
    pub fn kind(&self) -> BitLockerCredentialKind {
        self.kind
    }
}

impl std::fmt::Debug for ValidatedBitLockerCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ValidatedBitLockerCredential")
            .field("kind", &self.kind)
            .field("secret", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeBitLockerGateError {
    InvalidDrive(String),
    EmptyPassword,
    InvalidRecoveryKey(String),
    DevelopmentBuildDenied,
}

impl std::fmt::Display for NativeBitLockerGateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidDrive(drive) => write!(formatter, "invalid drive letter: {drive:?}"),
            Self::EmptyPassword => formatter.write_str("BitLocker password cannot be empty"),
            Self::InvalidRecoveryKey(error) => formatter.write_str(error),
            Self::DevelopmentBuildDenied => formatter
                .write_str("BitLocker unlock is disabled in non-elevated development builds"),
        }
    }
}

impl std::error::Error for NativeBitLockerGateError {}

/// Validate a credential without touching the host. Password whitespace is preserved because it
/// can be significant; only a truly empty password is rejected.
pub fn validate_credential(
    credential: BitLockerCredential,
) -> Result<ValidatedBitLockerCredential, NativeBitLockerGateError> {
    match credential {
        BitLockerCredential::Password(password) => {
            if password.is_empty() {
                return Err(NativeBitLockerGateError::EmptyPassword);
            }
            Ok(ValidatedBitLockerCredential {
                kind: BitLockerCredentialKind::Password,
                secret: password,
            })
        }
        BitLockerCredential::RecoveryKey(recovery_key) => {
            let formatted = lr_core::fveapi::format_recovery_key(&recovery_key)
                .map_err(NativeBitLockerGateError::InvalidRecoveryKey)?;
            Ok(ValidatedBitLockerCredential {
                kind: BitLockerCredentialKind::RecoveryKey,
                secret: formatted,
            })
        }
    }
}

/// Preserve the legacy install ordering: the selected target first, followed by every other
/// locked data volume. X: is the PE runtime volume and is never offered for unlock.
pub fn plan_install_locked_volumes(
    target: &str,
    volumes: &[BitLockerVolumeSnapshot],
) -> Result<Vec<String>, NativeBitLockerGateError> {
    let target = canonical_drive(target)?;
    let mut locked = Vec::new();

    if volumes.iter().any(|volume| {
        canonical_drive(&volume.drive).as_deref() == Ok(target.as_str())
            && volume.status.needs_unlock()
    }) {
        locked.push(target.clone());
    }

    for volume in volumes {
        let drive = canonical_drive(&volume.drive)?;
        if drive == "X:" || !volume.status.needs_unlock() {
            continue;
        }
        if !locked
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&drive))
        {
            locked.push(drive);
        }
    }
    Ok(locked)
}

/// Backup only needs access to its selected source volume; unrelated locked volumes must not
/// block the operation or prompt for credentials.
pub fn plan_backup_locked_volumes(
    source: &str,
    volumes: &[BitLockerVolumeSnapshot],
) -> Result<Vec<String>, NativeBitLockerGateError> {
    let source = canonical_drive(source)?;
    for volume in volumes {
        let drive = canonical_drive(&volume.drive)?;
        if drive.eq_ignore_ascii_case(&source) && volume.status.needs_unlock() {
            return Ok(vec![source]);
        }
    }
    Ok(Vec::new())
}

pub fn execute_unlock(
    drive: &str,
    credential: &ValidatedBitLockerCredential,
) -> Result<UnlockResult, NativeBitLockerGateError> {
    #[cfg(feature = "non-elevated-tests")]
    {
        let _ = (drive, credential);
        Err(NativeBitLockerGateError::DevelopmentBuildDenied)
    }

    #[cfg(not(feature = "non-elevated-tests"))]
    {
        let drive = canonical_drive(drive)?;
        let manager = super::bitlocker::BitLockerManager::new();
        Ok(match credential.kind {
            BitLockerCredentialKind::Password => {
                manager.unlock_with_password(&drive, &credential.secret)
            }
            BitLockerCredentialKind::RecoveryKey => {
                manager.unlock_with_recovery_key(&drive, &credential.secret)
            }
        })
    }
}

fn canonical_drive(drive: &str) -> Result<String, NativeBitLockerGateError> {
    let bytes = drive.as_bytes();
    if bytes.len() != 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return Err(NativeBitLockerGateError::InvalidDrive(drive.to_owned()));
    }
    Ok(format!("{}:", (bytes[0] as char).to_ascii_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(drive: &str, status: VolumeStatus) -> BitLockerVolumeSnapshot {
        BitLockerVolumeSnapshot {
            drive: drive.into(),
            status,
        }
    }

    #[test]
    fn drive_validation_is_strict_and_canonical() {
        assert_eq!(canonical_drive("c:").unwrap(), "C:");
        for invalid in ["", "C", "C:\\", " C:", "CC:", "1:", "C: & whoami"] {
            assert!(matches!(
                canonical_drive(invalid),
                Err(NativeBitLockerGateError::InvalidDrive(_))
            ));
        }
    }

    #[test]
    fn install_puts_target_first_excludes_x_and_deduplicates() {
        let volumes = vec![
            volume("D:", VolumeStatus::EncryptedLocked),
            volume("c:", VolumeStatus::EncryptedLocked),
            volume("C:", VolumeStatus::EncryptedLocked),
            volume("X:", VolumeStatus::EncryptedLocked),
            volume("E:", VolumeStatus::EncryptedUnlocked),
        ];
        assert_eq!(
            plan_install_locked_volumes("c:", &volumes).unwrap(),
            vec!["C:", "D:"]
        );
    }

    #[test]
    fn backup_only_plans_its_locked_source() {
        let volumes = vec![
            volume("C:", VolumeStatus::EncryptedLocked),
            volume("D:", VolumeStatus::EncryptedLocked),
        ];
        assert_eq!(
            plan_backup_locked_volumes("D:", &volumes).unwrap(),
            vec!["D:"]
        );
        assert!(plan_backup_locked_volumes("E:", &volumes)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn password_must_be_nonempty_but_is_not_trimmed() {
        assert_eq!(
            validate_credential(BitLockerCredential::Password(String::new())),
            Err(NativeBitLockerGateError::EmptyPassword)
        );
        let validated =
            validate_credential(BitLockerCredential::Password(" password ".into())).unwrap();
        assert_eq!(validated.kind(), BitLockerCredentialKind::Password);
        assert_eq!(validated.secret, " password ");
    }

    #[test]
    fn recovery_key_is_normalized_by_shared_fve_validator() {
        let raw = "111111222222333333444444555555666666777777888888";
        let validated = validate_credential(BitLockerCredential::RecoveryKey(raw.into())).unwrap();
        assert_eq!(validated.kind(), BitLockerCredentialKind::RecoveryKey);
        assert_eq!(
            validated.secret,
            "111111-222222-333333-444444-555555-666666-777777-888888"
        );
        assert!(matches!(
            validate_credential(BitLockerCredential::RecoveryKey("123".into())),
            Err(NativeBitLockerGateError::InvalidRecoveryKey(_))
        ));
    }

    #[test]
    fn debug_output_never_contains_secrets() {
        let input = BitLockerCredential::Password("top-secret".into());
        let input_debug = format!("{input:?}");
        assert!(!input_debug.contains("top-secret"));

        let validated = validate_credential(input).unwrap();
        let validated_debug = format!("{validated:?}");
        assert!(!validated_debug.contains("top-secret"));
        assert!(validated_debug.contains("<redacted>"));
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn development_build_denies_before_drive_or_manager_access() {
        let credential =
            validate_credential(BitLockerCredential::Password("secret".into())).unwrap();
        assert!(matches!(
            execute_unlock("not a drive", &credential),
            Err(NativeBitLockerGateError::DevelopmentBuildDenied)
        ));
    }
}
