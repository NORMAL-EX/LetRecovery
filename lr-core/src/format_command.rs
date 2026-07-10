//! Pure construction and validation for Windows `format.com` invocations.

use std::fmt;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSystem {
    Ntfs,
    Fat,
    Fat32,
    ExFat,
}

impl FileSystem {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ntfs => "NTFS",
            Self::Fat => "FAT",
            Self::Fat32 => "FAT32",
            Self::ExFat => "exFAT",
        }
    }

    const fn max_label_utf16_units(self) -> usize {
        match self {
            Self::Ntfs => 32,
            Self::Fat | Self::Fat32 => 11,
            Self::ExFat => 15,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatCommandError {
    InvalidDrive(String),
    UnsupportedFileSystem(String),
    InvalidLabelCharacter(char),
    LabelTooLong { maximum: usize },
}

impl fmt::Display for FormatCommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDrive(value) => write!(f, "invalid drive letter: {value:?}"),
            Self::UnsupportedFileSystem(value) => {
                write!(f, "unsupported file system: {value:?}")
            }
            Self::InvalidLabelCharacter(character) => {
                write!(
                    f,
                    "volume label contains an invalid character: {character:?}"
                )
            }
            Self::LabelTooLong { maximum } => write!(
                f,
                "volume label exceeds the {maximum} UTF-16 unit limit for this file system"
            ),
        }
    }
}

impl std::error::Error for FormatCommandError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatCommandSpec {
    drive: String,
    file_system: FileSystem,
    volume_label: Option<String>,
    force_dismount: bool,
}

impl FormatCommandSpec {
    pub fn new(
        drive: &str,
        file_system: &str,
        volume_label: Option<&str>,
    ) -> Result<Self, FormatCommandError> {
        let drive = normalize_drive(drive)?;
        let file_system = parse_file_system(file_system)?;
        let volume_label = match volume_label.filter(|label| !label.is_empty()) {
            Some(label) => {
                validate_volume_label(label, file_system)?;
                Some(label.to_string())
            }
            None => None,
        };
        Ok(Self {
            drive,
            file_system,
            volume_label,
            force_dismount: false,
        })
    }

    pub const fn with_force_dismount(mut self, enabled: bool) -> Self {
        self.force_dismount = enabled;
        self
    }

    pub fn drive(&self) -> &str {
        &self.drive
    }

    pub const fn file_system(&self) -> FileSystem {
        self.file_system
    }

    pub fn volume_label(&self) -> Option<&str> {
        self.volume_label.as_deref()
    }

    /// Build one process argument per item. No shell parsing is required.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            self.drive.clone(),
            format!("/FS:{}", self.file_system.as_str()),
        ];
        if let Some(label) = &self.volume_label {
            args.push(format!("/V:{label}"));
        }
        args.push("/Q".to_string());
        if self.force_dismount {
            args.push("/X".to_string());
        }
        args.push("/Y".to_string());
        args
    }
}

fn normalize_drive(value: &str) -> Result<String, FormatCommandError> {
    let trimmed = value.trim();
    let bytes = trimmed.as_bytes();
    let valid = match bytes {
        [letter] => letter.is_ascii_alphabetic(),
        [letter, b':'] => letter.is_ascii_alphabetic(),
        [letter, b':', slash] => letter.is_ascii_alphabetic() && matches!(slash, b'\\' | b'/'),
        _ => false,
    };
    if !valid {
        return Err(FormatCommandError::InvalidDrive(value.to_string()));
    }
    Ok(format!("{}:", (bytes[0] as char).to_ascii_uppercase()))
}

fn parse_file_system(value: &str) -> Result<FileSystem, FormatCommandError> {
    match value.trim() {
        value if value.eq_ignore_ascii_case("NTFS") => Ok(FileSystem::Ntfs),
        value if value.eq_ignore_ascii_case("FAT") => Ok(FileSystem::Fat),
        value if value.eq_ignore_ascii_case("FAT32") => Ok(FileSystem::Fat32),
        value if value.eq_ignore_ascii_case("EXFAT") => Ok(FileSystem::ExFat),
        _ => Err(FormatCommandError::UnsupportedFileSystem(value.to_string())),
    }
}

fn validate_volume_label(label: &str, file_system: FileSystem) -> Result<(), FormatCommandError> {
    for character in label.chars() {
        if character.is_control()
            || matches!(
                character,
                '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
            )
        {
            return Err(FormatCommandError::InvalidLabelCharacter(character));
        }
    }
    let maximum = file_system.max_label_utf16_units();
    if label.encode_utf16().count() > maximum {
        return Err(FormatCommandError::LabelTooLong { maximum });
    }
    Ok(())
}

/// `cmd.exe /c` remains necessary in one legacy WinPE fallback. Reject every
/// character that can change command structure before using that wrapper.
pub fn validate_cmd_wrapper_label(label: &str) -> Result<(), FormatCommandError> {
    if let Some(character) = label
        .chars()
        .find(|character| matches!(character, '&' | '^' | '%' | '!' | '(' | ')' | ';'))
    {
        return Err(FormatCommandError::InvalidLabelCharacter(character));
    }
    Ok(())
}

/// Resolve the system `format.com` without assuming Windows is installed on C:.
pub fn system_format_executable() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .map(|root| root.join("System32").join("format.com"))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("format.com"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_drive_and_builds_stable_arguments() {
        let spec = FormatCommandSpec::new(" d:\\ ", "ntfs", Some("Data"))
            .unwrap()
            .with_force_dismount(true);
        assert_eq!(spec.drive(), "D:");
        assert_eq!(spec.args(), ["D:", "/FS:NTFS", "/V:Data", "/Q", "/X", "/Y"]);
    }

    #[test]
    fn rejects_drive_and_file_system_injection() {
        assert!(matches!(
            FormatCommandSpec::new("D: & format C:", "NTFS", None),
            Err(FormatCommandError::InvalidDrive(_))
        ));
        assert!(matches!(
            FormatCommandSpec::new("D:", "NTFS & whoami", None),
            Err(FormatCommandError::UnsupportedFileSystem(_))
        ));
    }

    #[test]
    fn direct_invocation_keeps_spaces_unicode_and_metacharacters_in_one_argument() {
        let spec = FormatCommandSpec::new("D:", "NTFS", Some("数据 & ^ % !")).unwrap();
        let args = spec.args();
        assert_eq!(args[2], "/V:数据 & ^ % !");
        assert_eq!(args.len(), 5);
    }

    #[test]
    fn cmd_wrapper_rejects_shell_metacharacters() {
        assert!(validate_cmd_wrapper_label("Data & whoami").is_err());
        assert!(validate_cmd_wrapper_label("普通卷标").is_ok());
    }

    #[test]
    fn rejects_invalid_or_too_long_labels() {
        assert!(matches!(
            FormatCommandSpec::new("D:", "NTFS", Some("bad/label")),
            Err(FormatCommandError::InvalidLabelCharacter('/'))
        ));
        assert!(matches!(
            FormatCommandSpec::new("D:", "FAT32", Some("123456789012")),
            Err(FormatCommandError::LabelTooLong { maximum: 11 })
        ));
    }
}
