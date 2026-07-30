//! Compatibility guard for the removed DiskPart script feature.
//!
//! LetRecovery no longer starts `diskpart.exe`, `cmd.exe`, or batch files from this boundary.
//! Built-in storage operations use [`crate::windows_storage`] and typed parameters instead.
//! Existing configurations keep their historical flag and directory so upgrades remain
//! parse-compatible, but a directory containing legacy scripts fails closed with a precise list.

use std::ffi::OsStr;
use std::path::Path;

/// Validate that no legacy partition scripts would have been executed.
///
/// Missing or empty directories are a successful no-op. Any `.txt`, `.cmd`, or `.bat` file is
/// rejected because arbitrary shell/DiskPart text cannot be translated into typed WinAPI calls
/// without changing its semantics. Callers must surface the error and stop the installation.
pub fn run_scripts_in_dir(dir: &Path) -> Result<String, String> {
    if !dir.exists() {
        return Ok(format!("旧分区脚本目录不存在，跳过：{}", dir.display()));
    }
    if !dir.is_dir() {
        return Err(format!("旧分区脚本路径不是目录：{}", dir.display()));
    }

    let mut scripts = std::fs::read_dir(dir)
        .map_err(|error| format!("读取旧分区脚本目录失败：{error}"))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_file() && is_legacy_partition_script(path))
        .collect::<Vec<_>>();
    scripts.sort();

    if scripts.is_empty() {
        return Ok(format!("目录中没有旧分区脚本：{}", dir.display()));
    }

    let names = scripts
        .iter()
        .map(|path| {
            path.file_name()
                .unwrap_or_else(|| OsStr::new("<unknown>"))
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "检测到已停用的任意分区脚本：{names}。LetRecovery 已改用参数化 WinAPI 存储操作，\
         无法安全、等价地自动转换任意 .txt/.cmd/.bat 脚本；请移除这些脚本并改用内置分区功能。"
    ))
}

fn is_legacy_partition_script(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "txt" | "cmd" | "bat"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    fn test_directory() -> PathBuf {
        let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "lr-core-legacy-partition-script-test-{}-{id}",
            std::process::id()
        ))
    }

    #[test]
    fn missing_directory_is_a_safe_noop() {
        let directory = test_directory();
        assert!(run_scripts_in_dir(&directory).unwrap().contains("不存在"));
    }

    #[test]
    fn ignores_unrelated_files_and_rejects_every_legacy_script_extension() {
        let directory = test_directory();
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("notes.json"), "{}").unwrap();
        assert!(run_scripts_in_dir(&directory).unwrap().contains("没有"));

        for name in ["01.txt", "02.CMD", "03.bat"] {
            std::fs::write(directory.join(name), "select disk 0").unwrap();
        }
        let error = run_scripts_in_dir(&directory).unwrap_err();
        assert!(error.contains("01.txt"));
        assert!(error.contains("02.CMD"));
        assert!(error.contains("03.bat"));
        assert!(error.contains("WinAPI"));

        for entry in std::fs::read_dir(&directory).unwrap() {
            std::fs::remove_file(entry.unwrap().path()).unwrap();
        }
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn rejects_a_file_where_a_directory_is_required() {
        let path = test_directory();
        std::fs::write(&path, "not a directory").unwrap();
        let error = run_scripts_in_dir(&path).unwrap_err();
        assert!(error.contains("不是目录"));
        std::fs::remove_file(path).unwrap();
    }
}
