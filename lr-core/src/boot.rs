//! XP/2003 Legacy 引导写入（两端共享）。
//!
//! - [`write_xp_boot`]：为已释放的 XP/2003 系统写入引导（ntldr/boot.ini + MBR，仅 Legacy）。

use std::path::{Path, PathBuf};

use crate::command::{execute_request, CommandExecutor, CommandRequest, SystemCommandExecutor};
use crate::encoding::gbk_to_utf8;

/// 为应用好的 XP/2003 系统写入引导（仅 Legacy/MBR）。
///
/// 步骤：`bootsect /nt52 <盘> /mbr` 写 XP 引导码 → 校验 ntldr/ntdetect.com →
/// 缺失 boot.ini 时写入一份默认（不覆盖镜像自带的）。返回执行日志。
pub fn write_xp_boot(bin_dir: &Path, win_partition: &str) -> Result<String, String> {
    write_xp_boot_with_executor(&SystemCommandExecutor, bin_dir, win_partition)
}

fn write_xp_boot_with_executor<E: CommandExecutor + ?Sized>(
    executor: &E,
    bin_dir: &Path,
    win_partition: &str,
) -> Result<String, String> {
    let win = win_partition.trim_end_matches('\\'); // 形如 "C:"
    if win.is_empty() {
        return Err("目标 Windows 分区为空".to_string());
    }
    let win_root = PathBuf::from(format!("{}\\", win));
    let mut log = String::new();

    // 写引导码前先完成所有只读前置检查，避免在已知镜像不可引导时修改磁盘。
    let bootsect = bin_dir.join("bootsect.exe");
    if !bootsect.is_file() {
        return Err(format!("未找到 bootsect.exe: {}", bootsect.display()));
    }

    let ntldr = win_root.join("ntldr");
    let ntdetect = win_root.join("ntdetect.com");
    let missing = [(&ntldr, "ntldr"), (&ntdetect, "ntdetect.com")]
        .into_iter()
        .filter_map(|(path, name)| (!path.is_file()).then_some(name))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(format!(
            "系统分区缺少 XP 引导关键文件: {}",
            missing.join(", ")
        ));
    }

    // boot.ini 仅在不存在时写入，写入失败不得继续修改引导扇区。
    let boot_ini = win_root.join("boot.ini");
    if !boot_ini.is_file() {
        let content = "[boot loader]\r\n\
timeout=10\r\n\
default=multi(0)disk(0)rdisk(0)partition(1)\\WINDOWS\r\n\
[operating systems]\r\n\
multi(0)disk(0)rdisk(0)partition(1)\\WINDOWS=\"Windows XP\" /noexecute=optin /fastdetect\r\n";
        std::fs::write(&boot_ini, content)
            .map_err(|error| format!("写入 {} 失败: {}", boot_ini.display(), error))?;
        if !boot_ini.is_file() {
            return Err(format!("写入后未找到 {}", boot_ini.display()));
        }
        log.push_str("已写入默认 boot.ini\n");
    } else {
        log.push_str("boot.ini 已存在，保留镜像自带配置\n");
    }

    log.push_str(&format!("执行: bootsect /nt52 {} /mbr\n", win));
    let request = CommandRequest::new(&bootsect).args(["/nt52", win, "/mbr"]);
    let outcome = execute_request(executor, &request)
        .map_err(|error| format!("bootsect 启动失败: {}", error))?;
    let stdout = gbk_to_utf8(outcome.stdout());
    let stderr = gbk_to_utf8(outcome.stderr());
    log.push_str(&stdout);
    log.push_str(&stderr);
    if !outcome.succeeded() {
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        return Err(format!(
            "bootsect 写入 XP 引导失败（退出码 {:?}）: {}",
            outcome.exit_code(),
            detail
        ));
    }

    Ok(log)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{CommandOutcome, DryRunCommandExecutor};
    use crate::scoped_temp_file::ScopedTempDir;
    use std::ffi::OsStr;

    fn xp_fixture() -> (ScopedTempDir, PathBuf, PathBuf) {
        let root = ScopedTempDir::create_in(&std::env::temp_dir(), "letrecovery-xp-boot-test")
            .expect("create isolated fixture");
        let bin = root.path().join("bin");
        let windows = root.path().join("windows-volume");
        std::fs::create_dir_all(&bin).expect("create bin");
        std::fs::create_dir_all(&windows).expect("create target volume");
        (root, bin, windows)
    }

    #[test]
    fn xp_boot_rejects_missing_bootsect_before_execution() {
        let (_root, bin, windows) = xp_fixture();
        std::fs::write(windows.join("ntldr"), b"ntldr").unwrap();
        std::fs::write(windows.join("ntdetect.com"), b"ntdetect").unwrap();
        let executor = DryRunCommandExecutor::default();

        let error =
            write_xp_boot_with_executor(&executor, &bin, windows.to_string_lossy().as_ref())
                .unwrap_err();

        assert!(error.contains("bootsect.exe"));
        assert!(executor.requests().unwrap().is_empty());
    }

    #[test]
    fn xp_boot_rejects_missing_critical_files_before_execution() {
        let (_root, bin, windows) = xp_fixture();
        std::fs::write(bin.join("bootsect.exe"), b"fixture").unwrap();
        std::fs::write(windows.join("ntldr"), b"ntldr").unwrap();
        let executor = DryRunCommandExecutor::default();

        let error =
            write_xp_boot_with_executor(&executor, &bin, windows.to_string_lossy().as_ref())
                .unwrap_err();

        assert!(error.contains("ntdetect.com"));
        assert!(executor.requests().unwrap().is_empty());
    }

    #[test]
    fn xp_boot_rejects_nonzero_bootsect_result() {
        let (_root, bin, windows) = xp_fixture();
        std::fs::write(bin.join("bootsect.exe"), b"fixture").unwrap();
        std::fs::write(windows.join("ntldr"), b"ntldr").unwrap();
        std::fs::write(windows.join("ntdetect.com"), b"ntdetect").unwrap();
        let executor = DryRunCommandExecutor::new(CommandOutcome::new(
            Some(5),
            Vec::new(),
            b"access denied".to_vec(),
        ));

        let error =
            write_xp_boot_with_executor(&executor, &bin, windows.to_string_lossy().as_ref())
                .unwrap_err();

        assert!(error.contains("Some(5)"));
        assert!(error.contains("access denied"));
    }

    #[test]
    fn xp_boot_success_keeps_arguments_separate_and_creates_boot_ini() {
        let (_root, bin, windows) = xp_fixture();
        let bootsect = bin.join("bootsect.exe");
        std::fs::write(&bootsect, b"fixture").unwrap();
        std::fs::write(windows.join("ntldr"), b"ntldr").unwrap();
        std::fs::write(windows.join("ntdetect.com"), b"ntdetect").unwrap();
        let executor = DryRunCommandExecutor::default();

        write_xp_boot_with_executor(&executor, &bin, windows.to_string_lossy().as_ref()).unwrap();

        assert!(windows.join("boot.ini").is_file());
        let requests = executor.requests().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].program(), bootsect.as_os_str());
        assert_eq!(requests[0].arguments()[0], OsStr::new("/nt52"));
        assert_eq!(requests[0].arguments()[2], OsStr::new("/mbr"));
    }
}
