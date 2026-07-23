//! Safe lookup and integrity verification for locally cached artifacts.

use std::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;

use crate::download_integrity::{
    select_expected_hash, validate_download_filename, DownloadFilenameError, HashAlgorithm,
    HashVerification, IntegrityConfigError, IntegrityRequirement,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachedArtifactVerification {
    /// Legacy metadata did not declare a checksum.
    NotProvided,
    /// The strongest declared checksum was calculated and matched.
    Passed { algorithm: HashAlgorithm },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CachedArtifactStatus {
    Missing,
    Ready {
        path: PathBuf,
        verification: CachedArtifactVerification,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CachedArtifactPresence {
    Missing,
    Present {
        path: PathBuf,
        /// `None` means legacy metadata did not declare a checksum.
        expected_algorithm: Option<HashAlgorithm>,
    },
}

#[derive(Debug)]
pub enum CachedArtifactError {
    InvalidFilename(DownloadFilenameError),
    InvalidChecksum(IntegrityConfigError),
    InspectPath {
        path: PathBuf,
        source: io::Error,
    },
    UnsafeFileType {
        path: PathBuf,
    },
    CalculateHash {
        path: PathBuf,
        algorithm: HashAlgorithm,
        source: io::Error,
    },
    HashMismatch {
        path: PathBuf,
        algorithm: HashAlgorithm,
        expected: String,
        actual: String,
    },
}

impl fmt::Display for CachedArtifactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFilename(error) => write!(f, "invalid cached filename: {error}"),
            Self::InvalidChecksum(error) => write!(f, "invalid checksum metadata: {error}"),
            Self::InspectPath { path, source } => {
                write!(f, "cannot inspect cached file {}: {source}", path.display())
            }
            Self::UnsafeFileType { path } => write!(
                f,
                "cached path is not a regular file and was rejected: {}",
                path.display()
            ),
            Self::CalculateHash {
                path,
                algorithm,
                source,
            } => write!(
                f,
                "cannot calculate {} for {}: {source}",
                algorithm.name(),
                path.display()
            ),
            Self::HashMismatch {
                path,
                algorithm,
                expected,
                actual,
            } => write!(
                f,
                "{} mismatch for {} (expected {expected}, actual {actual})",
                algorithm.name(),
                path.display()
            ),
        }
    }
}

impl std::error::Error for CachedArtifactError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidFilename(error) => Some(error),
            Self::InvalidChecksum(error) => Some(error),
            Self::InspectPath { source, .. } | Self::CalculateHash { source, .. } => Some(source),
            Self::UnsafeFileType { .. } | Self::HashMismatch { .. } => None,
        }
    }
}

/// Inspect a server-named artifact in trusted cache directories without
/// reading the file contents.
///
/// The filename and declared checksum metadata are validated first. Candidate
/// directories are then checked in order. Once a path exists, an unsafe file
/// type or metadata read failure fails closed instead of silently falling
/// through to a lower-priority copy.
pub fn inspect_cached_artifact(
    filename: &str,
    candidate_directories: &[PathBuf],
    sha256: Option<&str>,
    md5: Option<&str>,
) -> Result<CachedArtifactPresence, CachedArtifactError> {
    let (path, requirement) =
        inspect_with_requirement(filename, candidate_directories, sha256, md5)?;
    let Some(path) = path else {
        return Ok(CachedArtifactPresence::Missing);
    };
    let expected_algorithm = match requirement {
        IntegrityRequirement::NotProvided => None,
        IntegrityRequirement::Required(expected) => Some(expected.algorithm()),
    };
    Ok(CachedArtifactPresence::Present {
        path,
        expected_algorithm,
    })
}

/// Locate and fully verify a cached artifact before it is used. A read failure
/// or checksum mismatch fails closed without trying a lower-priority copy.
pub fn verify_cached_artifact(
    filename: &str,
    candidate_directories: &[PathBuf],
    sha256: Option<&str>,
    md5: Option<&str>,
) -> Result<CachedArtifactStatus, CachedArtifactError> {
    let (path, requirement) =
        inspect_with_requirement(filename, candidate_directories, sha256, md5)?;
    let Some(path) = path else {
        return Ok(CachedArtifactStatus::Missing);
    };

    let IntegrityRequirement::Required(expected) = requirement else {
        return Ok(CachedArtifactStatus::Ready {
            path,
            verification: CachedArtifactVerification::NotProvided,
        });
    };

    let algorithm = expected.algorithm();
    match crate::download_integrity::verify_file(&path, &expected).map_err(|source| {
        CachedArtifactError::CalculateHash {
            path: path.clone(),
            algorithm,
            source,
        }
    })? {
        HashVerification::Passed { .. } => Ok(CachedArtifactStatus::Ready {
            path,
            verification: CachedArtifactVerification::Passed { algorithm },
        }),
        HashVerification::Mismatch {
            expected, actual, ..
        } => Err(CachedArtifactError::HashMismatch {
            path,
            algorithm,
            expected,
            actual,
        }),
    }
}

fn inspect_with_requirement(
    filename: &str,
    candidate_directories: &[PathBuf],
    sha256: Option<&str>,
    md5: Option<&str>,
) -> Result<(Option<PathBuf>, IntegrityRequirement), CachedArtifactError> {
    validate_download_filename(filename).map_err(CachedArtifactError::InvalidFilename)?;
    let requirement =
        select_expected_hash(sha256, md5).map_err(CachedArtifactError::InvalidChecksum)?;
    let path = find_regular_file(filename, candidate_directories)?;
    Ok((path, requirement))
}

fn find_regular_file(
    filename: &str,
    candidate_directories: &[PathBuf],
) -> Result<Option<PathBuf>, CachedArtifactError> {
    for directory in candidate_directories {
        let path = directory.join(filename);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => return Ok(Some(path)),
            Ok(_) => return Err(CachedArtifactError::UnsafeFileType { path }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => return Err(CachedArtifactError::InspectPath { path, source }),
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    const SHA256_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const MD5_ABC: &str = "900150983cd24fb0d6963f7d28e17f72";

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock must be after the Unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "letrecovery-cached-artifact-{label}-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create isolated test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_file_is_distinct_from_an_unverified_file() {
        let cache = TestDirectory::new("missing");
        let candidates = vec![cache.path().to_path_buf()];
        assert_eq!(
            verify_cached_artifact("pe.wim", &candidates, None, None).unwrap(),
            CachedArtifactStatus::Missing
        );

        fs::write(cache.path().join("pe.wim"), b"abc").unwrap();
        assert!(matches!(
            inspect_cached_artifact("pe.wim", &candidates, None, None).unwrap(),
            CachedArtifactPresence::Present {
                expected_algorithm: None,
                ..
            }
        ));
        assert!(matches!(
            verify_cached_artifact("pe.wim", &candidates, None, None).unwrap(),
            CachedArtifactStatus::Ready {
                verification: CachedArtifactVerification::NotProvided,
                ..
            }
        ));
    }

    #[test]
    fn sha256_is_verified_for_a_cached_file() {
        let cache = TestDirectory::new("sha256");
        fs::write(cache.path().join("LetRecovery PE.wim"), b"abc").unwrap();
        let status = verify_cached_artifact(
            "LetRecovery PE.wim",
            &[cache.path().to_path_buf()],
            Some(SHA256_ABC),
            Some(MD5_ABC),
        )
        .unwrap();
        assert!(matches!(
            status,
            CachedArtifactStatus::Ready {
                verification: CachedArtifactVerification::Passed {
                    algorithm: HashAlgorithm::Sha256
                },
                ..
            }
        ));
    }

    #[test]
    fn legacy_md5_is_verified_for_a_cached_file() {
        let cache = TestDirectory::new("md5");
        fs::write(cache.path().join("pe.wim"), b"abc").unwrap();
        let status =
            verify_cached_artifact("pe.wim", &[cache.path().to_path_buf()], None, Some(MD5_ABC))
                .unwrap();
        assert!(matches!(
            status,
            CachedArtifactStatus::Ready {
                verification: CachedArtifactVerification::Passed {
                    algorithm: HashAlgorithm::Md5
                },
                ..
            }
        ));
    }

    #[test]
    fn a_mismatch_fails_closed_without_using_a_lower_priority_copy() {
        let first = TestDirectory::new("first");
        let second = TestDirectory::new("second");
        fs::write(first.path().join("pe.wim"), b"damaged").unwrap();
        fs::write(second.path().join("pe.wim"), b"abc").unwrap();

        assert!(matches!(
            inspect_cached_artifact(
                "pe.wim",
                &[first.path().to_path_buf(), second.path().to_path_buf()],
                Some(SHA256_ABC),
                None,
            )
            .unwrap(),
            CachedArtifactPresence::Present {
                expected_algorithm: Some(HashAlgorithm::Sha256),
                ..
            }
        ));

        let error = verify_cached_artifact(
            "pe.wim",
            &[first.path().to_path_buf(), second.path().to_path_buf()],
            Some(SHA256_ABC),
            None,
        )
        .unwrap_err();
        assert!(matches!(error, CachedArtifactError::HashMismatch { .. }));
    }

    #[test]
    fn invalid_metadata_is_rejected_even_when_the_file_is_missing() {
        let cache = TestDirectory::new("metadata");
        let error = verify_cached_artifact(
            "pe.wim",
            &[cache.path().to_path_buf()],
            Some("not-a-sha256"),
            Some(MD5_ABC),
        )
        .unwrap_err();
        assert!(matches!(error, CachedArtifactError::InvalidChecksum(_)));
    }

    #[test]
    fn path_traversal_and_non_regular_cache_entries_are_rejected() {
        let cache = TestDirectory::new("path");
        assert!(matches!(
            verify_cached_artifact("..\\pe.wim", &[cache.path().to_path_buf()], None, None),
            Err(CachedArtifactError::InvalidFilename(_))
        ));

        fs::create_dir(cache.path().join("pe.wim")).unwrap();
        assert!(matches!(
            verify_cached_artifact("pe.wim", &[cache.path().to_path_buf()], None, None),
            Err(CachedArtifactError::UnsafeFileType { .. })
        ));
    }
}
