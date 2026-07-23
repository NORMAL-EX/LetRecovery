//! Pure policy and verification helpers for downloaded artifacts.

use std::fmt;
use std::path::Path;

use url::Url;

use crate::hash::{md5_bytes, md5_file, sha256_bytes, sha256_file};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAlgorithm {
    Sha256,
    Md5,
}

impl HashAlgorithm {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Sha256 => "SHA-256",
            Self::Md5 => "MD5",
        }
    }

    const fn hex_len(self) -> usize {
        match self {
            Self::Sha256 => 64,
            Self::Md5 => 32,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedHash {
    algorithm: HashAlgorithm,
    value: String,
}

impl ExpectedHash {
    pub const fn algorithm(&self) -> HashAlgorithm {
        self.algorithm
    }

    pub fn value(&self) -> &str {
        &self.value
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrityRequirement {
    NotProvided,
    Required(ExpectedHash),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityConfigError {
    pub algorithm: HashAlgorithm,
    pub value: String,
}

impl fmt::Display for IntegrityConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} checksum must contain exactly {} hexadecimal characters",
            self.algorithm.name(),
            self.algorithm.hex_len()
        )
    }
}

impl std::error::Error for IntegrityConfigError {}

/// Select the strongest declared checksum while preserving legacy MD5 data.
///
/// A non-empty SHA-256 declaration always wins. An invalid declared value is
/// an error and never falls back to a weaker checksum.
pub fn select_expected_hash(
    sha256: Option<&str>,
    md5: Option<&str>,
) -> Result<IntegrityRequirement, IntegrityConfigError> {
    if let Some(value) = non_empty(sha256) {
        return parse_expected_hash(HashAlgorithm::Sha256, value)
            .map(IntegrityRequirement::Required);
    }
    if let Some(value) = non_empty(md5) {
        return parse_expected_hash(HashAlgorithm::Md5, value).map(IntegrityRequirement::Required);
    }
    Ok(IntegrityRequirement::NotProvided)
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn parse_expected_hash(
    algorithm: HashAlgorithm,
    value: &str,
) -> Result<ExpectedHash, IntegrityConfigError> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.len() != algorithm.hex_len()
        || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(IntegrityConfigError {
            algorithm,
            value: value.to_string(),
        });
    }
    Ok(ExpectedHash {
        algorithm,
        value: normalized,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HashVerification {
    Passed {
        algorithm: HashAlgorithm,
        actual: String,
    },
    Mismatch {
        algorithm: HashAlgorithm,
        expected: String,
        actual: String,
    },
}

pub fn verify_file(
    path: impl AsRef<Path>,
    expected: &ExpectedHash,
) -> std::io::Result<HashVerification> {
    let actual = match expected.algorithm {
        HashAlgorithm::Sha256 => sha256_file(path, |_| {})?,
        HashAlgorithm::Md5 => md5_file(path, |_| {})?,
    };
    Ok(compare_hash(expected, actual))
}

pub fn verify_bytes(data: &[u8], expected: &ExpectedHash) -> HashVerification {
    let actual = match expected.algorithm {
        HashAlgorithm::Sha256 => sha256_bytes(data),
        HashAlgorithm::Md5 => md5_bytes(data),
    };
    compare_hash(expected, actual)
}

fn compare_hash(expected: &ExpectedHash, actual: String) -> HashVerification {
    if actual.eq_ignore_ascii_case(&expected.value) {
        HashVerification::Passed {
            algorithm: expected.algorithm,
            actual,
        }
    } else {
        HashVerification::Mismatch {
            algorithm: expected.algorithm,
            expected: expected.value.clone(),
            actual,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadTransport {
    Https,
    InsecureHttp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedDownloadUrl {
    url: Url,
    transport: DownloadTransport,
}

impl ValidatedDownloadUrl {
    pub fn as_str(&self) -> &str {
        self.url.as_str()
    }

    pub const fn transport(&self) -> DownloadTransport {
        self.transport
    }

    pub fn into_string(self) -> String {
        self.url.into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadUrlError {
    Invalid(String),
    MissingHost,
    EmbeddedCredentials,
    HttpRequiresOptIn,
    UnsupportedScheme(String),
}

impl fmt::Display for DownloadUrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "invalid download URL: {reason}"),
            Self::MissingHost => f.write_str("download URL has no host"),
            Self::EmbeddedCredentials => {
                f.write_str("download URL must not contain embedded credentials")
            }
            Self::HttpRequiresOptIn => f.write_str("HTTP download is disabled; HTTPS is required"),
            Self::UnsupportedScheme(scheme) => {
                write!(f, "unsupported download URL scheme: {scheme}")
            }
        }
    }
}

impl std::error::Error for DownloadUrlError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadFilenameError {
    Empty,
    TooLong,
    PathComponent,
    InvalidCharacter(char),
    ReservedDeviceName,
}

impl fmt::Display for DownloadFilenameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("download filename is empty"),
            Self::TooLong => f.write_str("download filename is longer than 255 UTF-16 units"),
            Self::PathComponent => {
                f.write_str("download filename must be a single safe path component")
            }
            Self::InvalidCharacter(character) => {
                write!(
                    f,
                    "download filename contains an invalid character: {character:?}"
                )
            }
            Self::ReservedDeviceName => {
                f.write_str("download filename uses a reserved Windows device name")
            }
        }
    }
}

impl std::error::Error for DownloadFilenameError {}

/// Validate a server-provided filename before it is joined to a local path.
pub fn validate_download_filename(value: &str) -> Result<(), DownloadFilenameError> {
    if value.is_empty() {
        return Err(DownloadFilenameError::Empty);
    }
    if value.encode_utf16().count() > 255 {
        return Err(DownloadFilenameError::TooLong);
    }
    if value == "."
        || value == ".."
        || value.ends_with(' ')
        || value.ends_with('.')
        || value.contains('/')
        || value.contains('\\')
    {
        return Err(DownloadFilenameError::PathComponent);
    }

    for character in value.chars() {
        if character.is_control() || matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
            return Err(DownloadFilenameError::InvalidCharacter(character));
        }
    }

    let stem = value
        .split('.')
        .next()
        .unwrap_or(value)
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|number| number.len() == 1 && matches!(number.as_bytes()[0], b'1'..=b'9'));
    if reserved {
        return Err(DownloadFilenameError::ReservedDeviceName);
    }

    Ok(())
}

/// Validate both metadata URLs and URLs produced by redirect resolution.
pub fn validate_download_url(
    value: &str,
    allow_insecure_http: bool,
) -> Result<ValidatedDownloadUrl, DownloadUrlError> {
    let url = Url::parse(value).map_err(|error| DownloadUrlError::Invalid(error.to_string()))?;
    if url.host().is_none() {
        return Err(DownloadUrlError::MissingHost);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(DownloadUrlError::EmbeddedCredentials);
    }

    let transport = match url.scheme() {
        "https" => DownloadTransport::Https,
        "http" if allow_insecure_http => DownloadTransport::InsecureHttp,
        "http" => return Err(DownloadUrlError::HttpRequiresOptIn),
        scheme => return Err(DownloadUrlError::UnsupportedScheme(scheme.to_string())),
    };

    Ok(ValidatedDownloadUrl { url, transport })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA256_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const MD5_ABC: &str = "900150983cd24fb0d6963f7d28e17f72";

    #[test]
    fn sha256_takes_priority_over_legacy_md5() {
        let selected = select_expected_hash(Some(SHA256_ABC), Some(MD5_ABC)).unwrap();
        let IntegrityRequirement::Required(expected) = selected else {
            panic!("a declared SHA-256 must require verification");
        };
        assert_eq!(expected.algorithm(), HashAlgorithm::Sha256);
        assert_eq!(expected.value(), SHA256_ABC);
    }

    #[test]
    fn legacy_md5_remains_supported() {
        let selected = select_expected_hash(None, Some(MD5_ABC)).unwrap();
        let IntegrityRequirement::Required(expected) = selected else {
            panic!("a declared MD5 must require verification");
        };
        assert_eq!(expected.algorithm(), HashAlgorithm::Md5);
        assert!(matches!(
            verify_bytes(b"abc", &expected),
            HashVerification::Passed { .. }
        ));
    }

    #[test]
    fn missing_checksums_are_not_reported_as_passed() {
        assert_eq!(
            select_expected_hash(Some("  "), None).unwrap(),
            IntegrityRequirement::NotProvided
        );
    }

    #[test]
    fn malformed_declared_sha256_is_an_error_without_md5_fallback() {
        let error = select_expected_hash(Some("not-a-sha256"), Some(MD5_ABC)).unwrap_err();
        assert_eq!(error.algorithm, HashAlgorithm::Sha256);
    }

    #[test]
    fn mismatch_preserves_expected_and_actual_values() {
        let IntegrityRequirement::Required(expected) =
            select_expected_hash(Some(SHA256_ABC), None).unwrap()
        else {
            unreachable!();
        };
        let result = verify_bytes(b"different", &expected);
        assert!(matches!(result, HashVerification::Mismatch { .. }));
    }

    #[test]
    fn calculation_errors_are_distinct_from_missing_metadata() {
        let IntegrityRequirement::Required(expected) =
            select_expected_hash(Some(SHA256_ABC), None).unwrap()
        else {
            unreachable!();
        };
        let result = verify_file("this-file-must-not-exist-6e55d09f.wim", &expected);
        assert!(result.is_err());
    }

    #[test]
    fn https_is_allowed_by_default() {
        let validated = validate_download_url("https://example.com/pe.wim", false).unwrap();
        assert_eq!(validated.transport(), DownloadTransport::Https);
    }

    #[test]
    fn http_requires_explicit_compatibility_opt_in() {
        assert_eq!(
            validate_download_url("http://example.com/pe.wim", false).unwrap_err(),
            DownloadUrlError::HttpRequiresOptIn
        );
        let validated = validate_download_url("http://example.com/pe.wim", true).unwrap();
        assert_eq!(validated.transport(), DownloadTransport::InsecureHttp);
    }

    #[test]
    fn credentials_and_non_web_schemes_are_rejected() {
        assert_eq!(
            validate_download_url("https://user:secret@example.com/pe.wim", false).unwrap_err(),
            DownloadUrlError::EmbeddedCredentials
        );
        assert!(matches!(
            validate_download_url("file:///C:/pe.wim", false),
            Err(DownloadUrlError::MissingHost)
        ));
    }

    #[test]
    fn download_filename_accepts_spaces_unicode_and_shell_metacharacters() {
        validate_download_filename("LetRecovery PE 中文 & ^ %.wim").unwrap();
    }

    #[test]
    fn download_filename_rejects_traversal_and_windows_reserved_names() {
        assert_eq!(
            validate_download_filename("..\\outside.wim").unwrap_err(),
            DownloadFilenameError::PathComponent
        );
        assert_eq!(
            validate_download_filename("CON.wim").unwrap_err(),
            DownloadFilenameError::ReservedDeviceName
        );
        assert!(matches!(
            validate_download_filename("C:pe.wim"),
            Err(DownloadFilenameError::InvalidCharacter(':'))
        ));
    }
}
