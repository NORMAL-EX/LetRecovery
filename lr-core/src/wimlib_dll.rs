//! wimlib DLL 兜底：内置 libwim-15.dll，运行时确保其在 exe 同目录可用。
//!
//! 背景：迁移到 wimlib 后，镜像操作依赖 `libwim-15.dll`。PE 环境默认**不含**该 DLL
//! （旧版用的 wimgapi.dll 才是 PE 自带的）。若 PE 打包未带上该 DLL，会导致备份/安装
//! 等所有镜像操作在加载阶段失败。这里把 DLL 编译进二进制，加载前把同目录副本原子同步
//! 为本次构建内嵌的版本，从根本上消除“PE 缺 DLL”或旧 DLL 遮蔽新功能的故障。

use crate::scoped_temp_file::ScopedTempFile;
use std::path::Path;

/// 编译期嵌入的 libwim-15.dll
static EMBEDDED_WIMLIB_DLL: &[u8] = include_bytes!("../vendor/libwim-15.dll");

fn installed_dll_matches(path: &Path) -> bool {
    std::fs::read(path).is_ok_and(|bytes| bytes == EMBEDDED_WIMLIB_DLL)
}

fn sync_embedded_dll(dir: &Path) -> std::io::Result<bool> {
    let dst = dir.join("libwim-15.dll");
    if installed_dll_matches(&dst) {
        return Ok(false);
    }

    let staged = ScopedTempFile::create_in(dir, "libwim-15", "tmp", EMBEDDED_WIMLIB_DLL)?;
    if !installed_dll_matches(staged.path()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "staged wimlib DLL differs from the embedded bytes",
        ));
    }
    staged.persist_replace(&dst)?;
    Ok(true)
}

/// 确保 exe 同目录的 libwim-15.dll 与本次构建的内嵌版本逐字节一致。幂等。
///
/// 临时文件与目标位于同一目录，完整写入并回读后才通过原子替换发布，避免程序崩溃时
/// 留下半个 DLL。该函数在 DLL 首次加载前调用，因此不会替换已载入的模块。
pub fn ensure_dll_available() {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let dst = dir.join("libwim-15.dll");
            match sync_embedded_dll(dir) {
                Ok(true) => log::info!("已同步内置 libwim-15.dll 到 {}", dst.display()),
                Ok(false) => {}
                Err(error) => {
                    log::warn!("同步内置 libwim-15.dll 失败 {}: {}", dst.display(), error)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scoped_temp_file::ScopedTempDir;

    #[test]
    fn installed_copy_must_match_every_embedded_byte() {
        let temp = ScopedTempDir::create_in(&std::env::temp_dir(), "lr-wimlib-dll-test")
            .expect("create temp directory");
        let dll = temp.path().join("libwim-15.dll");

        assert!(!installed_dll_matches(&dll));
        std::fs::write(&dll, b"stale DLL").expect("write stale DLL");
        assert!(!installed_dll_matches(&dll));
        std::fs::write(&dll, EMBEDDED_WIMLIB_DLL).expect("write embedded DLL");
        assert!(installed_dll_matches(&dll));
    }

    #[test]
    fn synchronizes_embedded_dll_through_a_valid_temporary_name() {
        let temp = ScopedTempDir::create_in(&std::env::temp_dir(), "lr-wimlib-sync-test")
            .expect("create temp directory");
        let dll = temp.path().join("libwim-15.dll");
        std::fs::write(&dll, b"stale DLL").expect("write stale DLL");

        assert!(sync_embedded_dll(temp.path()).expect("synchronize embedded DLL"));
        assert!(!sync_embedded_dll(temp.path()).expect("reuse matching embedded DLL"));

        assert!(installed_dll_matches(&dll));
        let remaining_files = std::fs::read_dir(temp.path())
            .expect("read temp directory")
            .map(|entry| entry.expect("read directory entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(
            remaining_files,
            vec![std::ffi::OsString::from("libwim-15.dll")]
        );
    }
}
