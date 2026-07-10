//! Shared DiskPart script execution for both endpoints.
//!
//! Scripts are always passed to DiskPart as a separate `/s` argument. The
//! generated temporary file is collision resistant and removed on every exit
//! path, including command-start failures.

use std::ffi::OsStr;
use std::io;
use std::path::Path;

use crate::command::{
    execute_request, CommandExecutor, CommandOutcome, CommandRequest, SystemCommandExecutor,
};
use crate::encoding::gbk_to_utf8;
use crate::operation::OperationError;
use crate::scoped_temp_file::ScopedTempFile;

/// Execute one DiskPart script with the production command executor.
pub fn execute_script<S: AsRef<OsStr>>(
    directory: &Path,
    prefix: &str,
    diskpart_program: S,
    script: &str,
) -> io::Result<CommandOutcome> {
    execute_script_with(
        &SystemCommandExecutor,
        directory,
        prefix,
        diskpart_program,
        script,
    )
}

/// Execute one DiskPart script through an injectable command boundary.
///
/// This is the testable safety boundary for destructive DiskPart operations:
/// tests can inspect the exact program and arguments without starting a
/// process or touching a disk.
pub fn execute_script_with<E, S>(
    executor: &E,
    directory: &Path,
    prefix: &str,
    diskpart_program: S,
    script: &str,
) -> io::Result<CommandOutcome>
where
    E: CommandExecutor + ?Sized,
    S: AsRef<OsStr>,
{
    let (_script_file, request) =
        prepare_script_request(directory, prefix, diskpart_program, script)?;

    executor.execute(&request)
}

/// Execute and validate a DiskPart script using the shared typed command
/// boundary. This keeps command-start, I/O, and tool-reported failures distinct.
pub fn execute_script_checked<S: AsRef<OsStr>>(
    directory: &Path,
    prefix: &str,
    diskpart_program: S,
    script: &str,
) -> Result<String, OperationError> {
    execute_script_checked_with(
        &SystemCommandExecutor,
        directory,
        prefix,
        diskpart_program,
        script,
    )
}

pub fn execute_script_checked_with<E, S>(
    executor: &E,
    directory: &Path,
    prefix: &str,
    diskpart_program: S,
    script: &str,
) -> Result<String, OperationError>
where
    E: CommandExecutor + ?Sized,
    S: AsRef<OsStr>,
{
    let (_script_file, request) =
        prepare_script_request(directory, prefix, diskpart_program, script)
            .map_err(|error| OperationError::io("prepare DiskPart script", &error))?;
    let outcome = execute_request(executor, &request)?;
    validated_stdout_typed(&outcome)
}

fn prepare_script_request<S: AsRef<OsStr>>(
    directory: &Path,
    prefix: &str,
    diskpart_program: S,
    script: &str,
) -> io::Result<(ScopedTempFile, CommandRequest)> {
    std::fs::create_dir_all(directory)?;
    let script_file = ScopedTempFile::create_in(directory, prefix, "txt", script.as_bytes())?;
    let request = CommandRequest::new(diskpart_program)
        .arg("/s")
        .arg(script_file.path().as_os_str());
    Ok((script_file, request))
}

/// DiskPart is known to report some failures in text while returning exit code
/// zero. Treat a failed status, any stderr, or a known error phrase as failure.
pub fn output_indicates_error(status_success: bool, stdout: &str, stderr: &str) -> bool {
    if !status_success || !stderr.trim().is_empty() {
        return true;
    }

    let output = stdout.to_lowercase();
    const ERROR_MARKERS: &[&str] = &[
        "diskpart has encountered an error",
        "virtual disk service error",
        "diskpart failed",
        "access is denied",
        "the arguments specified for this command are not valid",
        "there is no volume selected",
        "there is no disk selected",
        "the specified disk is not valid",
        "the specified volume is not valid",
        "the operation is not supported",
        "there is not enough usable space",
        "no usable free extent could be found",
        "the parameter is incorrect",
        "the media is write protected",
        "not enough space",
        "i/o device error",
        "cyclic redundancy check",
        "diskpart 遇到错误",
        "diskpart 无法",
        "diskpart 未能",
        "虚拟磁盘服务错误",
        "错误:",
        "错误：",
        "拒绝访问",
        "没有选择",
        "指定的磁盘无效",
        "指定的卷无效",
        "不支持此操作",
        "没有足够的可用空间",
        "找不到可用的空闲区域",
        "参数错误",
        "介质受写入保护",
        "循环冗余检查",
    ];

    ERROR_MARKERS.iter().any(|marker| output.contains(marker))
}

/// Decode a DiskPart outcome and return stdout only when it is trustworthy.
///
/// The error text includes both streams when available because localized
/// DiskPart builds do not consistently choose stdout or stderr for failures.
pub fn validated_stdout(outcome: &CommandOutcome) -> Result<String, String> {
    validated_stdout_typed(outcome).map_err(|error| {
        error
            .message
            .strip_prefix("DiskPart failed: ")
            .unwrap_or(&error.message)
            .to_string()
    })
}

/// Typed counterpart of [`validated_stdout`] for new command paths.
pub fn validated_stdout_typed(outcome: &CommandOutcome) -> Result<String, OperationError> {
    let stdout = gbk_to_utf8(outcome.stdout());
    let stderr = gbk_to_utf8(outcome.stderr());
    if !output_indicates_error(outcome.succeeded(), &stdout, &stderr) {
        return Ok(stdout);
    }

    let stdout = stdout.trim();
    let stderr = stderr.trim();
    let detail = match (stdout.is_empty(), stderr.is_empty()) {
        (false, false) => format!("{stdout}\n{stderr}"),
        (false, true) => stdout.to_string(),
        (true, false) => stderr.to_string(),
        (true, true) => format!(
            "DiskPart exited without usable output (exit code: {:?})",
            outcome.exit_code()
        ),
    };
    Err(OperationError::command_exit(
        "DiskPart",
        outcome.exit_code(),
        detail,
    ))
}

/// Run supported scripts in a directory with the production executor.
///
/// - `.cmd` / `.bat` are retained for backwards compatibility and run through
///   `cmd /c`.
/// - `.txt` files run through `diskpart /s`.
/// - Other extensions are ignored and files are processed in name order.
pub fn run_scripts_in_dir(dir: &Path) -> Result<String, String> {
    run_scripts_in_dir_with_executor(&SystemCommandExecutor, dir)
}

/// Testable variant of [`run_scripts_in_dir`].
pub fn run_scripts_in_dir_with_executor<E>(executor: &E, dir: &Path) -> Result<String, String>
where
    E: CommandExecutor + ?Sized,
{
    if !dir.exists() {
        return Ok(format!("diskpart 脚本目录不存在，跳过：{}", dir.display()));
    }

    let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .map_err(|error| format!("读取脚本目录失败：{error}"))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_file())
        .collect();
    entries.sort();

    let mut log = String::new();
    let mut any = false;

    for path in entries {
        let extension = path
            .extension()
            .and_then(OsStr::to_str)
            .unwrap_or_default()
            .to_lowercase();
        let (request, is_diskpart) = match extension.as_str() {
            "cmd" | "bat" => (
                CommandRequest::new("cmd").arg("/c").arg(path.as_os_str()),
                false,
            ),
            "txt" => (
                CommandRequest::new("diskpart")
                    .arg("/s")
                    .arg(path.as_os_str()),
                true,
            ),
            _ => continue,
        };

        any = true;
        log.push_str(&format!("\n>>> 执行脚本: {}\n", path.display()));
        match executor.execute(&request) {
            Ok(outcome) => {
                let stdout = gbk_to_utf8(outcome.stdout());
                let stderr = gbk_to_utf8(outcome.stderr());
                if !stdout.trim().is_empty() {
                    log.push_str(stdout.trim());
                    log.push('\n');
                }
                if !stderr.trim().is_empty() {
                    log.push_str(stderr.trim());
                    log.push('\n');
                }

                let failed = if is_diskpart {
                    output_indicates_error(outcome.succeeded(), &stdout, &stderr)
                } else {
                    !outcome.succeeded()
                };
                if failed {
                    log.push_str(&format!("[脚本执行失败] {}\n", path.display()));
                    return Err(log);
                }
            }
            Err(error) => {
                log.push_str(&format!(
                    "[无法启动 {}] {error}\n",
                    request.program().to_string_lossy()
                ));
                return Err(log);
            }
        }
    }

    if !any {
        log.push_str(&format!(
            "目录中没有可执行脚本(.cmd/.bat/.txt)：{}",
            dir.display()
        ));
    }
    Ok(log)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    use super::*;
    use crate::command::DryRunCommandExecutor;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    fn test_directory() -> PathBuf {
        let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("lr-core-diskpart-test-{}-{id}", std::process::id()))
    }

    #[test]
    fn dry_run_keeps_program_and_script_path_as_separate_arguments() {
        let root = test_directory();
        let directory = root.join("含 空格 & ^");
        let executor = DryRunCommandExecutor::default();

        let outcome = execute_script_with(
            &executor,
            &directory,
            "diskpart",
            "not-real diskpart.exe",
            "select disk 0\n",
        )
        .unwrap();

        assert!(outcome.succeeded());
        let requests = executor.requests().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].program(), OsStr::new("not-real diskpart.exe"));
        assert_eq!(requests[0].arguments().len(), 2);
        assert_eq!(requests[0].arguments()[0], OsStr::new("/s"));
        let script_path = PathBuf::from(&requests[0].arguments()[1]);
        assert!(script_path.starts_with(&directory));
        assert!(!script_path.exists());

        std::fs::remove_dir(directory).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    struct InspectingExecutor {
        expected_script: &'static [u8],
        observed_path: Mutex<Option<PathBuf>>,
        fail: bool,
    }

    impl CommandExecutor for InspectingExecutor {
        fn execute(&self, request: &CommandRequest) -> io::Result<CommandOutcome> {
            let script_path = PathBuf::from(&request.arguments()[1]);
            assert_eq!(std::fs::read(&script_path)?, self.expected_script);
            *self.observed_path.lock().unwrap() = Some(script_path);
            if self.fail {
                Err(io::Error::other("modeled command-start failure"))
            } else {
                Ok(CommandOutcome::success())
            }
        }
    }

    #[test]
    fn temporary_script_exists_during_execution_and_is_cleaned_after_error() {
        let directory = test_directory();
        let executor = InspectingExecutor {
            expected_script: b"list disk\n",
            observed_path: Mutex::new(None),
            fail: true,
        };

        let error = execute_script_with(
            &executor,
            &directory,
            "diskpart",
            "diskpart.exe",
            "list disk\n",
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Other);
        let path = executor.observed_path.lock().unwrap().clone().unwrap();
        assert!(!path.exists());
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn detects_exit_stderr_and_localized_diskpart_failures() {
        assert!(output_indicates_error(false, "", ""));
        assert!(output_indicates_error(true, "", "access denied"));
        assert!(output_indicates_error(
            true,
            "Virtual Disk Service error: operation is not supported",
            ""
        ));
        assert!(output_indicates_error(
            true,
            "DiskPart 遇到错误: 拒绝访问。",
            ""
        ));
        assert!(!output_indicates_error(
            true,
            "DiskPart successfully formatted the volume.",
            ""
        ));
        assert!(!output_indicates_error(
            true,
            "No errors were found while checking metadata.",
            ""
        ));
        assert!(!output_indicates_error(
            true,
            "  Volume 2     D   invalid data   NTFS\n  卷 3        E   无法启动失败案例  NTFS",
            ""
        ));
    }

    #[test]
    fn validated_stdout_preserves_success_and_combines_failure_details() {
        let success = CommandOutcome::new(
            Some(0),
            b"DiskPart successfully completed the operation.".to_vec(),
            Vec::new(),
        );
        assert_eq!(
            validated_stdout(&success).unwrap(),
            "DiskPart successfully completed the operation."
        );

        let failure = CommandOutcome::new(
            Some(0),
            b"DiskPart has encountered an error".to_vec(),
            b"Access is denied".to_vec(),
        );
        let error = validated_stdout(&failure).unwrap_err();
        assert!(error.contains("encountered an error"));
        assert!(error.contains("Access is denied"));
    }

    #[test]
    fn directory_runner_rejects_diskpart_text_errors_with_zero_exit_code() {
        let directory = test_directory();
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("01-prepare.txt"), "list disk\n").unwrap();
        let executor = DryRunCommandExecutor::new(CommandOutcome::new(
            Some(0),
            b"DiskPart has encountered an error: Access is denied.".to_vec(),
            Vec::new(),
        ));

        let error = run_scripts_in_dir_with_executor(&executor, &directory).unwrap_err();

        assert!(error.contains("脚本执行失败"));
        let request = executor.requests().unwrap().pop().unwrap();
        assert_eq!(request.program(), OsStr::new("diskpart"));
        assert_eq!(request.arguments()[0], OsStr::new("/s"));
        std::fs::remove_file(directory.join("01-prepare.txt")).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn checked_execution_classifies_zero_exit_text_failure() {
        let directory = test_directory();
        let executor = DryRunCommandExecutor::new(CommandOutcome::new(
            Some(0),
            b"DiskPart has encountered an error: Access is denied.".to_vec(),
            Vec::new(),
        ));

        let error = execute_script_checked_with(
            &executor,
            &directory,
            "typed",
            "diskpart.exe",
            "list disk\n",
        )
        .unwrap_err();

        assert_eq!(
            error.kind,
            crate::operation::OperationErrorKind::CommandExit
        );
        assert!(error.message.contains("Access is denied"));
        std::fs::remove_dir(directory).unwrap();
    }
}
