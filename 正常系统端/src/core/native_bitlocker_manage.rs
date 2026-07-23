//! Pure planning and read-only inventory boundary for the native BitLocker management dialog.
//!
//! UI state is never an execution authorization. Inventory display and intent construction are
//! separated, credentials reuse [`super::native_bitlocker_gate`] validation, and production code
//! must refresh the selected volume before forwarding a confirmed operation to the backend.

use super::bitlocker::VolumeStatus;
use super::native_bitlocker_gate::{
    validate_credential, BitLockerCredential, NativeBitLockerGateError,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BitLockerManageVolume {
    pub drive: String,
    pub label: String,
    pub total_size_mb: u64,
    pub status: VolumeStatus,
    pub protection_method: String,
    pub encryption_percentage: Option<u8>,
}

impl From<super::bitlocker::VolumeInfo> for BitLockerManageVolume {
    fn from(volume: super::bitlocker::VolumeInfo) -> Self {
        Self {
            drive: volume.letter,
            label: volume.label,
            total_size_mb: volume.total_size_mb,
            status: volume.status,
            protection_method: volume.protection_method,
            encryption_percentage: volume.encryption_percentage,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BitLockerUnlockMethod {
    #[default]
    Password,
    RecoveryKey,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BitLockerManageAction {
    #[default]
    Unlock,
    Decrypt,
    ReadRecoveryKey,
    SuspendProtection,
    ResumeProtection,
}

#[derive(Clone, PartialEq, Eq)]
pub enum BitLockerManageIntent {
    Unlock {
        volume: String,
        credential: BitLockerCredential,
    },
    Decrypt {
        volume: String,
    },
    ReadRecoveryKey {
        volume: String,
    },
    SuspendProtection {
        volume: String,
    },
    ResumeProtection {
        volume: String,
    },
}

impl std::fmt::Debug for BitLockerManageIntent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unlock { volume, credential } => formatter
                .debug_struct("Unlock")
                .field("volume", volume)
                .field("credential", credential)
                .finish(),
            Self::Decrypt { volume } => formatter
                .debug_struct("Decrypt")
                .field("volume", volume)
                .finish(),
            Self::ReadRecoveryKey { volume } => formatter
                .debug_struct("ReadRecoveryKey")
                .field("volume", volume)
                .finish(),
            Self::SuspendProtection { volume } => formatter
                .debug_struct("SuspendProtection")
                .field("volume", volume)
                .finish(),
            Self::ResumeProtection { volume } => formatter
                .debug_struct("ResumeProtection")
                .field("volume", volume)
                .finish(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BitLockerManageError {
    DevelopmentBuildDenied,
    InvalidDrive(String),
    VolumeNotAvailable(String),
    ActionUnavailable {
        volume: String,
        status: VolumeStatus,
        action: BitLockerManageAction,
    },
    InvalidCredential(NativeBitLockerGateError),
}

impl std::fmt::Display for BitLockerManageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DevelopmentBuildDenied => formatter
                .write_str("BitLocker management inventory is disabled in development-test builds"),
            Self::InvalidDrive(drive) => write!(formatter, "invalid BitLocker volume: {drive:?}"),
            Self::VolumeNotAvailable(drive) => {
                write!(
                    formatter,
                    "BitLocker volume is no longer available: {drive}"
                )
            }
            Self::ActionUnavailable {
                volume,
                status,
                action,
            } => write!(
                formatter,
                "BitLocker action {action:?} is unavailable for {volume} in state {status:?}"
            ),
            Self::InvalidCredential(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for BitLockerManageError {}

/// Reads only encrypted fixed-volume information. Development builds refuse before manager
/// construction, keeping UI tests isolated from the host.
pub fn read_inventory() -> Result<Vec<BitLockerManageVolume>, BitLockerManageError> {
    #[cfg(feature = "non-elevated-tests")]
    {
        Err(BitLockerManageError::DevelopmentBuildDenied)
    }
    #[cfg(not(feature = "non-elevated-tests"))]
    {
        Ok(super::bitlocker::BitLockerManager::new()
            .get_encrypted_volumes()
            .into_iter()
            .map(BitLockerManageVolume::from)
            .collect())
    }
}

pub fn build_intent(
    inventory: &[BitLockerManageVolume],
    selected_volume: &str,
    action: BitLockerManageAction,
    unlock_method: BitLockerUnlockMethod,
    secret: String,
) -> Result<BitLockerManageIntent, BitLockerManageError> {
    let volume = canonical_drive(selected_volume)?;
    let intent = match action {
        BitLockerManageAction::Unlock => {
            let credential = match unlock_method {
                BitLockerUnlockMethod::Password => BitLockerCredential::Password(secret),
                BitLockerUnlockMethod::RecoveryKey => BitLockerCredential::RecoveryKey(secret),
            };
            BitLockerManageIntent::Unlock { volume, credential }
        }
        BitLockerManageAction::Decrypt => BitLockerManageIntent::Decrypt { volume },
        BitLockerManageAction::ReadRecoveryKey => BitLockerManageIntent::ReadRecoveryKey { volume },
        BitLockerManageAction::SuspendProtection => {
            BitLockerManageIntent::SuspendProtection { volume }
        }
        BitLockerManageAction::ResumeProtection => {
            BitLockerManageIntent::ResumeProtection { volume }
        }
    };
    validate_intent(inventory, &intent)?;
    Ok(intent)
}

/// Reads the recovery protector for one freshly revalidated unlocked volume. This delegates to
/// [`execute_intent`] so the window cannot accidentally bypass the development-build denial or
/// the current-inventory check.
pub fn read_recovery_key(volume: &str) -> Result<String, String> {
    execute_intent(BitLockerManageIntent::ReadRecoveryKey {
        volume: volume.to_owned(),
    })
}

/// Executes one typed management intent on a background thread owned by the caller.
///
/// In development-test builds this returns before validating the drive, constructing a
/// `BitLockerManager`, or touching the host. Production execution constructs the manager once,
/// refreshes the encrypted-volume inventory, revalidates the operation and credential, and only
/// then calls the existing BitLocker implementation. Neither credentials nor recovery keys are
/// logged or included in errors/debug output by this boundary.
pub fn execute_intent(intent: BitLockerManageIntent) -> Result<String, String> {
    #[cfg(feature = "non-elevated-tests")]
    {
        let _ = intent;
        Err(crate::tr!("开发测试构建已禁用 BitLocker 管理操作。"))
    }
    #[cfg(not(feature = "non-elevated-tests"))]
    {
        let manager = super::bitlocker::BitLockerManager::new();
        execute_with_backend(&manager, intent)
    }
}

trait BitLockerManageBackend {
    fn inventory(&self) -> Vec<BitLockerManageVolume>;
    fn unlock(&self, volume: &str, credential: &BitLockerCredential) -> Result<String, String>;
    fn decrypt(&self, volume: &str) -> Result<String, String>;
    fn read_recovery_key(&self, volume: &str) -> Result<String, String>;
    fn suspend_protection(&self, volume: &str) -> Result<String, String>;
    fn resume_protection(&self, volume: &str) -> Result<String, String>;
}

#[cfg(not(feature = "non-elevated-tests"))]
impl BitLockerManageBackend for super::bitlocker::BitLockerManager {
    fn inventory(&self) -> Vec<BitLockerManageVolume> {
        self.get_encrypted_volumes()
            .into_iter()
            .map(BitLockerManageVolume::from)
            .collect()
    }

    fn unlock(&self, volume: &str, credential: &BitLockerCredential) -> Result<String, String> {
        let result = match credential {
            BitLockerCredential::Password(password) => self.unlock_with_password(volume, password),
            BitLockerCredential::RecoveryKey(recovery_key) => {
                self.unlock_with_recovery_key(volume, recovery_key)
            }
        };
        if result.success {
            Ok(result.message)
        } else {
            Err(result.message)
        }
    }

    fn decrypt(&self, volume: &str) -> Result<String, String> {
        let result = self.decrypt(volume);
        if result.success {
            Ok(result.message)
        } else {
            Err(result.message)
        }
    }

    fn read_recovery_key(&self, volume: &str) -> Result<String, String> {
        self.get_recovery_key(volume)
    }

    fn suspend_protection(&self, volume: &str) -> Result<String, String> {
        self.suspend_protection(volume)
    }

    fn resume_protection(&self, volume: &str) -> Result<String, String> {
        self.resume_protection(volume)
    }
}

fn execute_with_backend<B: BitLockerManageBackend>(
    backend: &B,
    intent: BitLockerManageIntent,
) -> Result<String, String> {
    let inventory = backend.inventory();
    validate_intent(&inventory, &intent).map_err(user_error)?;
    match intent {
        BitLockerManageIntent::Unlock { volume, credential } => {
            backend.unlock(&volume, &credential)
        }
        BitLockerManageIntent::Decrypt { volume } => backend.decrypt(&volume),
        BitLockerManageIntent::ReadRecoveryKey { volume } => backend.read_recovery_key(&volume),
        BitLockerManageIntent::SuspendProtection { volume } => backend.suspend_protection(&volume),
        BitLockerManageIntent::ResumeProtection { volume } => backend.resume_protection(&volume),
    }
}

fn validate_intent(
    inventory: &[BitLockerManageVolume],
    intent: &BitLockerManageIntent,
) -> Result<(), BitLockerManageError> {
    let (raw_volume, action) = match intent {
        BitLockerManageIntent::Unlock { volume, .. } => {
            (volume.as_str(), BitLockerManageAction::Unlock)
        }
        BitLockerManageIntent::Decrypt { volume } => {
            (volume.as_str(), BitLockerManageAction::Decrypt)
        }
        BitLockerManageIntent::ReadRecoveryKey { volume } => {
            (volume.as_str(), BitLockerManageAction::ReadRecoveryKey)
        }
        BitLockerManageIntent::SuspendProtection { volume } => {
            (volume.as_str(), BitLockerManageAction::SuspendProtection)
        }
        BitLockerManageIntent::ResumeProtection { volume } => {
            (volume.as_str(), BitLockerManageAction::ResumeProtection)
        }
    };
    let volume = canonical_drive(raw_volume)?;
    let status = inventory
        .iter()
        .find(|candidate| candidate.drive.eq_ignore_ascii_case(&volume))
        .map(|candidate| candidate.status)
        .ok_or_else(|| BitLockerManageError::VolumeNotAvailable(volume.clone()))?;
    let permitted = match status {
        VolumeStatus::EncryptedLocked => action == BitLockerManageAction::Unlock,
        VolumeStatus::EncryptedUnlocked => matches!(
            action,
            BitLockerManageAction::Decrypt
                | BitLockerManageAction::ReadRecoveryKey
                | BitLockerManageAction::SuspendProtection
                | BitLockerManageAction::ResumeProtection
        ),
        _ => false,
    };
    if !permitted {
        return Err(BitLockerManageError::ActionUnavailable {
            volume,
            status,
            action,
        });
    }
    if let BitLockerManageIntent::Unlock { credential, .. } = intent {
        validate_credential(credential.clone()).map_err(BitLockerManageError::InvalidCredential)?;
    }
    Ok(())
}

fn user_error(error: BitLockerManageError) -> String {
    match error {
        BitLockerManageError::DevelopmentBuildDenied => {
            crate::tr!("开发测试构建已禁用 BitLocker 管理操作。")
        }
        BitLockerManageError::InvalidDrive(_) => crate::tr!("BitLocker 卷无效。"),
        BitLockerManageError::VolumeNotAvailable(_) => {
            crate::tr!("所选 BitLocker 卷已不可用，请刷新后重试。")
        }
        BitLockerManageError::ActionUnavailable { .. } => {
            crate::tr!("当前卷状态不支持所选 BitLocker 操作。")
        }
        BitLockerManageError::InvalidCredential(error) => error.to_string(),
    }
}

fn canonical_drive(drive: &str) -> Result<String, BitLockerManageError> {
    match drive.as_bytes() {
        [letter, b':'] if letter.is_ascii_alphabetic() => {
            let drive = format!("{}:", (*letter as char).to_ascii_uppercase());
            if drive == "X:" {
                Err(BitLockerManageError::InvalidDrive(drive))
            } else {
                Ok(drive)
            }
        }
        _ => Err(BitLockerManageError::InvalidDrive(drive.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    fn volume(drive: &str, status: VolumeStatus) -> BitLockerManageVolume {
        BitLockerManageVolume {
            drive: drive.into(),
            label: "Data".into(),
            total_size_mb: 1024,
            status,
            protection_method: "Recovery Password".into(),
            encryption_percentage: None,
        }
    }

    #[test]
    fn locked_volume_only_accepts_valid_unlock_credentials() {
        let inventory = [volume("D:", VolumeStatus::EncryptedLocked)];
        assert!(matches!(
            build_intent(
                &inventory,
                "d:",
                BitLockerManageAction::Unlock,
                BitLockerUnlockMethod::Password,
                " password ".into()
            ),
            Ok(BitLockerManageIntent::Unlock { volume, .. }) if volume == "D:"
        ));
        assert!(matches!(
            build_intent(
                &inventory,
                "D:",
                BitLockerManageAction::Unlock,
                BitLockerUnlockMethod::RecoveryKey,
                "123".into()
            ),
            Err(BitLockerManageError::InvalidCredential(_))
        ));
        assert!(matches!(
            build_intent(
                &inventory,
                "D:",
                BitLockerManageAction::Decrypt,
                BitLockerUnlockMethod::Password,
                String::new()
            ),
            Err(BitLockerManageError::ActionUnavailable { .. })
        ));
    }

    #[test]
    fn unlocked_volume_restores_all_four_legacy_operations_without_credentials() {
        let inventory = [volume("E:", VolumeStatus::EncryptedUnlocked)];
        for action in [
            BitLockerManageAction::Decrypt,
            BitLockerManageAction::ReadRecoveryKey,
            BitLockerManageAction::SuspendProtection,
            BitLockerManageAction::ResumeProtection,
        ] {
            let intent = build_intent(
                &inventory,
                "E:",
                action,
                BitLockerUnlockMethod::Password,
                "must-not-be-forwarded".into(),
            )
            .unwrap();
            assert!(!format!("{intent:?}").contains("must-not-be-forwarded"));
        }
    }

    #[test]
    fn transient_states_and_stale_or_invalid_volumes_fail_closed() {
        for status in [VolumeStatus::Encrypting, VolumeStatus::Decrypting] {
            let inventory = [volume("F:", status)];
            assert!(matches!(
                build_intent(
                    &inventory,
                    "F:",
                    BitLockerManageAction::Decrypt,
                    BitLockerUnlockMethod::Password,
                    String::new()
                ),
                Err(BitLockerManageError::ActionUnavailable { .. })
            ));
        }
        assert!(matches!(
            build_intent(
                &[],
                "Z:",
                BitLockerManageAction::Decrypt,
                BitLockerUnlockMethod::Password,
                String::new()
            ),
            Err(BitLockerManageError::VolumeNotAvailable(_))
        ));
        assert!(matches!(
            build_intent(
                &[],
                "C:\\",
                BitLockerManageAction::Decrypt,
                BitLockerUnlockMethod::Password,
                String::new()
            ),
            Err(BitLockerManageError::InvalidDrive(_))
        ));
        assert!(matches!(
            build_intent(
                &[volume("X:", VolumeStatus::EncryptedUnlocked)],
                "X:",
                BitLockerManageAction::Decrypt,
                BitLockerUnlockMethod::Password,
                String::new()
            ),
            Err(BitLockerManageError::InvalidDrive(_))
        ));
    }

    #[test]
    fn debug_never_exposes_unlock_secret() {
        let inventory = [volume("D:", VolumeStatus::EncryptedLocked)];
        let intent = build_intent(
            &inventory,
            "D:",
            BitLockerManageAction::Unlock,
            BitLockerUnlockMethod::Password,
            "top-secret".into(),
        )
        .unwrap();
        assert!(!format!("{intent:?}").contains("top-secret"));
    }

    struct FakeBackend {
        inventory: Vec<BitLockerManageVolume>,
        calls: RefCell<Vec<&'static str>>,
    }

    impl FakeBackend {
        fn new(inventory: Vec<BitLockerManageVolume>) -> Self {
            Self {
                inventory,
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl BitLockerManageBackend for FakeBackend {
        fn inventory(&self) -> Vec<BitLockerManageVolume> {
            self.calls.borrow_mut().push("inventory");
            self.inventory.clone()
        }

        fn unlock(
            &self,
            _volume: &str,
            _credential: &BitLockerCredential,
        ) -> Result<String, String> {
            self.calls.borrow_mut().push("unlock");
            Ok("unlocked".into())
        }

        fn decrypt(&self, _volume: &str) -> Result<String, String> {
            self.calls.borrow_mut().push("decrypt");
            Ok("decrypting".into())
        }

        fn read_recovery_key(&self, _volume: &str) -> Result<String, String> {
            self.calls.borrow_mut().push("read");
            Ok("secret-key".into())
        }

        fn suspend_protection(&self, _volume: &str) -> Result<String, String> {
            self.calls.borrow_mut().push("suspend");
            Ok("suspended".into())
        }

        fn resume_protection(&self, _volume: &str) -> Result<String, String> {
            self.calls.borrow_mut().push("resume");
            Ok("resumed".into())
        }
    }

    #[test]
    fn typed_execution_revalidates_fresh_inventory_before_dispatch() {
        let backend = FakeBackend::new(vec![volume("D:", VolumeStatus::EncryptedLocked)]);
        let result = execute_with_backend(
            &backend,
            BitLockerManageIntent::Unlock {
                volume: "D:".into(),
                credential: BitLockerCredential::Password("secret".into()),
            },
        );
        assert_eq!(result.as_deref(), Ok("unlocked"));
        assert_eq!(&*backend.calls.borrow(), &["inventory", "unlock"]);

        backend.calls.borrow_mut().clear();
        let stale = execute_with_backend(
            &backend,
            BitLockerManageIntent::Decrypt {
                volume: "D:".into(),
            },
        );
        assert!(stale.is_err());
        assert_eq!(&*backend.calls.borrow(), &["inventory"]);
    }

    #[test]
    fn typed_execution_routes_every_unlocked_legacy_operation() {
        let cases = [
            (
                BitLockerManageIntent::Decrypt {
                    volume: "E:".into(),
                },
                "decrypt",
            ),
            (
                BitLockerManageIntent::ReadRecoveryKey {
                    volume: "E:".into(),
                },
                "read",
            ),
            (
                BitLockerManageIntent::SuspendProtection {
                    volume: "E:".into(),
                },
                "suspend",
            ),
            (
                BitLockerManageIntent::ResumeProtection {
                    volume: "E:".into(),
                },
                "resume",
            ),
        ];
        for (intent, expected) in cases {
            let backend = FakeBackend::new(vec![volume("E:", VolumeStatus::EncryptedUnlocked)]);
            assert!(execute_with_backend(&backend, intent).is_ok());
            assert_eq!(&*backend.calls.borrow(), &["inventory", expected]);
        }
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn development_inventory_denies_before_manager_or_host_io() {
        assert_eq!(
            read_inventory(),
            Err(BitLockerManageError::DevelopmentBuildDenied)
        );
        let error = execute_intent(BitLockerManageIntent::Unlock {
            volume: "not-a-drive".into(),
            credential: BitLockerCredential::Password("top-secret".into()),
        })
        .unwrap_err();
        assert_eq!(error, crate::tr!("开发测试构建已禁用 BitLocker 管理操作。"));
        assert!(!error.contains("top-secret"));
        assert_eq!(
            read_recovery_key("X:"),
            Err(crate::tr!("开发测试构建已禁用 BitLocker 管理操作。"))
        );
    }
}
