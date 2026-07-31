//! Pure validation for Windows volume-format requests.

use std::fmt;

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
            Self::ExFat => 11,
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
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDrive(value) => write!(formatter, "invalid drive letter: {value:?}"),
            Self::UnsupportedFileSystem(value) => {
                write!(formatter, "unsupported file system: {value:?}")
            }
            Self::InvalidLabelCharacter(character) => write!(
                formatter,
                "volume label contains an invalid character: {character:?}"
            ),
            Self::LabelTooLong { maximum } => write!(
                formatter,
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
                Some(label.to_owned())
            }
            None => None,
        };
        Ok(Self {
            drive,
            file_system,
            volume_label,
        })
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
        return Err(FormatCommandError::InvalidDrive(value.to_owned()));
    }
    Ok(format!("{}:", (bytes[0] as char).to_ascii_uppercase()))
}

fn parse_file_system(value: &str) -> Result<FileSystem, FormatCommandError> {
    match value.trim() {
        value if value.eq_ignore_ascii_case("NTFS") => Ok(FileSystem::Ntfs),
        value if value.eq_ignore_ascii_case("FAT") => Ok(FileSystem::Fat),
        value if value.eq_ignore_ascii_case("FAT32") => Ok(FileSystem::Fat32),
        value if value.eq_ignore_ascii_case("EXFAT") => Ok(FileSystem::ExFat),
        _ => Err(FormatCommandError::UnsupportedFileSystem(value.to_owned())),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_drive_and_preserves_validated_parameters() {
        let spec = FormatCommandSpec::new(" d:\\ ", "ntfs", Some("Data")).unwrap();
        assert_eq!(spec.drive(), "D:");
        assert_eq!(spec.file_system(), FileSystem::Ntfs);
        assert_eq!(spec.volume_label(), Some("Data"));
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
    fn preserves_spaces_unicode_and_non_path_metacharacters() {
        let spec = FormatCommandSpec::new("D:", "NTFS", Some("数据 & ^ % !")).unwrap();
        assert_eq!(spec.volume_label(), Some("数据 & ^ % !"));
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
        assert!(matches!(
            FormatCommandSpec::new("D:", "exFAT", Some("123456789012")),
            Err(FormatCommandError::LabelTooLong { maximum: 11 })
        ));
    }
}
