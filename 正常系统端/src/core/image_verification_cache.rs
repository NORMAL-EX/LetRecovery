//! Persistent fingerprints for WIM/ESD files that already passed a full wimlib verification.
//!
//! A cache hit never trusts path metadata alone.  The source is opened without
//! write/delete sharing, BLAKE3 is recomputed over the locked byte stream, and
//! only an exact 256-bit fingerprint match may reuse the earlier full result.

use std::fs::{File, OpenOptions};
use std::io::{Error, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use memmap2::MmapOptions;
use serde::{Deserialize, Serialize};

const CACHE_VERSION: u32 = 1;
const MAX_CACHE_BYTES: u64 = 1024 * 1024;
const MAX_ENTRIES: usize = 64;
const HASH_CHUNK_BYTES: usize = 64 * 1024 * 1024;

static CACHE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug)]
pub(crate) struct CachedVerification {
    // Keep the verified bytes immutable until the caller has reopened the WIM
    // and read its current metadata.
    _locked_file: File,
}

#[derive(Debug)]
pub(crate) enum CacheProbe {
    Hit(CachedVerification),
    Uncached(FingerprintSource),
    Changed(PreparedFingerprint),
}

#[derive(Debug)]
pub(crate) struct FingerprintSource {
    key: String,
    file_size: u64,
    file: File,
}

#[derive(Debug)]
pub(crate) struct PreparedFingerprint {
    key: String,
    file_size: u64,
    fingerprint: String,
    // Keeping the original handle alive prevents write/delete opens between
    // fingerprinting and the full wimlib verification that authorizes caching.
    _locked_file: File,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct CacheFile {
    version: u32,
    entries: Vec<CacheEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CacheEntry {
    path: String,
    file_size: u64,
    fingerprint: String,
}

impl FingerprintSource {
    fn open(path: &Path) -> std::io::Result<Self> {
        let canonical_path = std::fs::canonicalize(path)?;
        let file = open_locked_source(&canonical_path)?;
        let file_size = file.metadata()?.len();
        let key = canonical_path.to_string_lossy().into_owned();
        Ok(Self {
            key,
            file_size,
            file,
        })
    }

    pub(crate) fn calculate(
        self,
        cancel: &AtomicBool,
        mut on_progress: impl FnMut(u64, u64),
    ) -> std::io::Result<PreparedFingerprint> {
        if cancel.load(Ordering::SeqCst) {
            return Err(Error::new(ErrorKind::Interrupted, "fingerprint cancelled"));
        }
        if self.file_size == 0 {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "cannot fingerprint an empty image",
            ));
        }

        // SAFETY: the mapping is read-only, and `_locked_file` denies write and
        // delete sharing for the entire lifetime of the mapping and result.
        let mapping = unsafe { MmapOptions::new().map(&self.file)? };
        if mapping.len() as u64 != self.file_size {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "mapped image length changed unexpectedly",
            ));
        }

        let mut hasher = blake3::Hasher::new();
        let mut completed = 0_u64;
        for chunk in mapping.chunks(HASH_CHUNK_BYTES) {
            if cancel.load(Ordering::SeqCst) {
                return Err(Error::new(ErrorKind::Interrupted, "fingerprint cancelled"));
            }
            hasher.update_rayon(chunk);
            completed = completed.saturating_add(chunk.len() as u64);
            on_progress(completed, self.file_size);
        }
        drop(mapping);

        Ok(PreparedFingerprint {
            key: self.key,
            file_size: self.file_size,
            fingerprint: hasher.finalize().to_hex().to_string(),
            _locked_file: self.file,
        })
    }
}

impl PreparedFingerprint {
    pub(crate) fn store(&self) -> std::io::Result<()> {
        let Some(cache_path) = default_cache_path() else {
            return Ok(());
        };
        store_at(
            &cache_path,
            CacheEntry {
                path: self.key.clone(),
                file_size: self.file_size,
                fingerprint: self.fingerprint.clone(),
            },
        )
    }
}

pub(crate) fn probe(
    path: &Path,
    cancel: &AtomicBool,
    on_progress: impl FnMut(u64, u64),
) -> std::io::Result<CacheProbe> {
    let Some(cache_path) = default_cache_path() else {
        return Ok(CacheProbe::Uncached(FingerprintSource::open(path)?));
    };
    probe_at(path, &cache_path, cancel, on_progress)
}

fn probe_at(
    path: &Path,
    cache_path: &Path,
    cancel: &AtomicBool,
    on_progress: impl FnMut(u64, u64),
) -> std::io::Result<CacheProbe> {
    let source = FingerprintSource::open(path)?;
    let entry = {
        let _guard = cache_mutex()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        load_at(cache_path)
            .entries
            .into_iter()
            .find(|entry| same_windows_path(&entry.path, &source.key))
    };
    let Some(entry) = entry else {
        return Ok(CacheProbe::Uncached(source));
    };
    if entry.file_size != source.file_size || !valid_entry(&entry) {
        return Ok(CacheProbe::Uncached(source));
    }

    let prepared = source.calculate(cancel, on_progress)?;
    if prepared.fingerprint == entry.fingerprint {
        Ok(CacheProbe::Hit(CachedVerification {
            _locked_file: prepared._locked_file,
        }))
    } else {
        Ok(CacheProbe::Changed(prepared))
    }
}

fn default_cache_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|directory| {
        directory
            .join("LetRecovery")
            .join("cache")
            .join("image-verification-v1.json")
    })
}

fn cache_mutex() -> &'static Mutex<()> {
    CACHE_LOCK.get_or_init(|| Mutex::new(()))
}

fn load_at(path: &Path) -> CacheFile {
    let Ok(metadata) = std::fs::metadata(path) else {
        return CacheFile {
            version: CACHE_VERSION,
            entries: Vec::new(),
        };
    };
    if metadata.len() > MAX_CACHE_BYTES {
        return CacheFile {
            version: CACHE_VERSION,
            entries: Vec::new(),
        };
    }
    let Ok(contents) = std::fs::read(path) else {
        return CacheFile {
            version: CACHE_VERSION,
            entries: Vec::new(),
        };
    };
    let Ok(mut cache) = serde_json::from_slice::<CacheFile>(&contents) else {
        return CacheFile {
            version: CACHE_VERSION,
            entries: Vec::new(),
        };
    };
    if cache.version != CACHE_VERSION {
        return CacheFile {
            version: CACHE_VERSION,
            entries: Vec::new(),
        };
    }
    cache.entries.retain(valid_entry);
    cache.entries.truncate(MAX_ENTRIES);
    cache
}

fn store_at(path: &Path, entry: CacheEntry) -> std::io::Result<()> {
    let _guard = cache_mutex()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut cache = load_at(path);
    cache.version = CACHE_VERSION;
    cache
        .entries
        .retain(|existing| !same_windows_path(&existing.path, &entry.path));
    cache.entries.insert(0, entry);
    cache.entries.truncate(MAX_ENTRIES);

    let directory = path
        .parent()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "cache path has no parent"))?;
    std::fs::create_dir_all(directory)?;
    let contents = loop {
        let contents = serde_json::to_vec(&cache)
            .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
        if contents.len() as u64 <= MAX_CACHE_BYTES {
            break contents;
        }
        if cache.entries.len() <= 1 {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "verification cache entry exceeds the size limit",
            ));
        }
        cache.entries.pop();
    };
    let (temporary, mut file) = lr_core::scoped_temp_file::ScopedTempFile::create_writer_in(
        directory,
        "image-verification",
        "json",
    )?;
    file.write_all(&contents)?;
    file.flush()?;
    file.sync_all()?;
    drop(file);
    temporary.persist_replace(path)
}

fn valid_entry(entry: &CacheEntry) -> bool {
    entry.path.len() <= 32 * 1024
        && entry.fingerprint.len() == 64
        && entry
            .fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
}

fn same_windows_path(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

#[cfg(windows)]
fn open_locked_source(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::{FILE_FLAG_SEQUENTIAL_SCAN, FILE_SHARE_READ};

    let mut options = OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .custom_flags(FILE_FLAG_SEQUENTIAL_SCAN.0);
    options.open(path)
}

#[cfg(not(windows))]
fn open_locked_source(path: &Path) -> std::io::Result<File> {
    File::open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(name: &str) -> Self {
            let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "letrecovery-verification-cache-{}-{name}-{id}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn unchanged_bytes_hit_but_same_size_changes_miss() {
        let directory = TestDirectory::new("changed");
        let image = directory.0.join("sample.wim");
        let cache = directory.0.join("cache.json");
        std::fs::write(&image, vec![0x5a; 4 * 1024 * 1024]).unwrap();
        let cancel = AtomicBool::new(false);

        let CacheProbe::Uncached(source) = probe_at(&image, &cache, &cancel, |_, _| {}).unwrap()
        else {
            panic!("new image must not hit");
        };
        let prepared = source.calculate(&cancel, |_, _| {}).unwrap();
        store_at(
            &cache,
            CacheEntry {
                path: prepared.key.clone(),
                file_size: prepared.file_size,
                fingerprint: prepared.fingerprint.clone(),
            },
        )
        .unwrap();
        drop(prepared);

        let CacheProbe::Hit(hit) = probe_at(&image, &cache, &cancel, |_, _| {}).unwrap() else {
            panic!("unchanged bytes must hit");
        };
        #[cfg(windows)]
        assert!(OpenOptions::new().write(true).open(&image).is_err());
        drop(hit);

        let mut changed = vec![0x5a; 4 * 1024 * 1024];
        let middle = changed.len() / 2;
        changed[middle] ^= 0xff;
        std::fs::write(&image, changed).unwrap();
        assert!(matches!(
            probe_at(&image, &cache, &cancel, |_, _| {}).unwrap(),
            CacheProbe::Changed(_)
        ));
    }

    #[test]
    fn malformed_cache_fails_closed_to_full_verification() {
        let directory = TestDirectory::new("malformed");
        let image = directory.0.join("sample.wim");
        let cache = directory.0.join("cache.json");
        std::fs::write(&image, b"not-empty").unwrap();
        std::fs::write(&cache, b"{broken").unwrap();
        assert!(matches!(
            probe_at(&image, &cache, &AtomicBool::new(false), |_, _| {}).unwrap(),
            CacheProbe::Uncached(_)
        ));
    }

    #[test]
    fn stored_cache_never_exceeds_the_read_limit() {
        let directory = TestDirectory::new("bounded");
        let cache = directory.0.join("cache.json");
        for index in 0..40 {
            store_at(
                &cache,
                CacheEntry {
                    path: format!("C:\\{}\\image-{index}.wim", "x".repeat(30 * 1024)),
                    file_size: 1,
                    fingerprint: "a".repeat(64),
                },
            )
            .unwrap();
        }
        assert!(std::fs::metadata(cache).unwrap().len() <= MAX_CACHE_BYTES);
    }

    #[test]
    fn cancellation_stops_before_publishing_a_fingerprint() {
        let directory = TestDirectory::new("cancelled");
        let image = directory.0.join("sample.wim");
        std::fs::write(&image, vec![0x33; 1024 * 1024]).unwrap();
        let source = FingerprintSource::open(&image).unwrap();
        let error = source
            .calculate(&AtomicBool::new(true), |_, _| {})
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Interrupted);
    }
}
