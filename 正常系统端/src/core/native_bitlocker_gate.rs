//! BitLocker safety gate shared by the native install and backup flows.
//!
//! Planning and credential validation are pure. The only method allowed to create a
//! `BitLockerManager` is `execute_unlock`, which is disabled before any host I/O in
//! non-elevated development builds.

use super::bitlocker::{UnlockResult, VolumeStatus};
use zeroize::Zeroize;

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

impl Drop for BitLockerCredential {
    fn drop(&mut self) {
        match self {
            Self::Password(secret) | Self::RecoveryKey(secret) => secret.zeroize(),
        }
    }
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

impl Drop for ValidatedBitLockerCredential {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
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
    mut credential: BitLockerCredential,
) -> Result<ValidatedBitLockerCredential, NativeBitLockerGateError> {
    match &mut credential {
        BitLockerCredential::Password(password) => {
            if password.is_empty() {
                return Err(NativeBitLockerGateError::EmptyPassword);
            }
            Ok(ValidatedBitLockerCredential {
                kind: BitLockerCredentialKind::Password,
                secret: std::mem::take(password),
            })
        }
        BitLockerCredential::RecoveryKey(recovery_key) => {
            let formatted = lr_core::fveapi::format_recovery_key(recovery_key.as_str())
                .map_err(NativeBitLockerGateError::InvalidRecoveryKey)?;
            Ok(ValidatedBitLockerCredential {
                kind: BitLockerCredentialKind::RecoveryKey,
                secret: formatted,
            })
        }
    }
}

/// Install only needs access to its selected target volume (same policy as backup). Unrelated
/// locked volumes are never written and must not block the operation or prompt for credentials.
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
        canonical_drive(&volume.drive)?;
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

/// 解锁时不再让用户选择“密码”还是“恢复密钥”，按输入内容自动判断：
/// 只由数字、空格和连字符组成且恰好 48 位数字时按恢复密钥处理；同样只含数字和分隔符但位数
/// 不对，几乎一定是抄错了恢复密钥，直接提示而不是拿它去试密码；其它输入一律按密码处理。
pub fn classify_unlock_secret(secret: &str) -> Result<(BitLockerCredentialKind, String), String> {
    if secret.is_empty() {
        return Err(crate::tr!("请输入 BitLocker 解锁密码或恢复密钥"));
    }
    // 密码原样使用（首尾空格也可能是密码的一部分），只在识别恢复密钥时去掉首尾空白。
    let trimmed = secret.trim();
    let key_characters_only = !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|c| c.is_ascii_digit() || c == '-' || c.is_whitespace());
    let digits = trimmed.chars().filter(char::is_ascii_digit).count();
    let has_separator = trimmed.chars().any(|c| c == '-' || c.is_whitespace());
    if key_characters_only && digits == 48 {
        let formatted = lr_core::fveapi::format_recovery_key(trimmed).map_err(|_| {
            crate::tr!("恢复密钥应为 48 位数字（8 组，每组 6 位），请核对后重新输入")
        })?;
        return Ok((BitLockerCredentialKind::RecoveryKey, formatted));
    }
    if key_characters_only && has_separator {
        return Err(crate::tr!(
            "恢复密钥应为 48 位数字（8 组，每组 6 位），请核对后重新输入"
        ));
    }
    Ok((BitLockerCredentialKind::Password, secret.to_string()))
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
    fn install_only_plans_its_locked_target() {
        let volumes = vec![
            volume("D:", VolumeStatus::EncryptedLocked),
            volume("C:", VolumeStatus::EncryptedLocked),
            volume("X:", VolumeStatus::EncryptedLocked),
            volume("E:", VolumeStatus::EncryptedUnlocked),
        ];
        assert_eq!(
            plan_install_locked_volumes("c:", &volumes).unwrap(),
            vec!["C:"]
        );
        assert!(plan_install_locked_volumes("E:", &volumes)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn unlock_secret_kind_is_detected_from_the_input() {
        let key = "111111-222222-333333-444444-555555-666666-777777-888888";
        assert_eq!(
            classify_unlock_secret(key).unwrap().0,
            BitLockerCredentialKind::RecoveryKey
        );
        assert_eq!(
            classify_unlock_secret(&key.replace('-', " ")).unwrap().0,
            BitLockerCredentialKind::RecoveryKey
        );
        assert_eq!(
            classify_unlock_secret(" pass word ").unwrap(),
            (BitLockerCredentialKind::Password, " pass word ".to_string())
        );
        assert_eq!(
            classify_unlock_secret("12345678").unwrap().0,
            BitLockerCredentialKind::Password
        );
        assert!(classify_unlock_secret("111111-222222").is_err());
        assert!(classify_unlock_secret("").is_err());
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
