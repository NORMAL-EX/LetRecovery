//! Shared process construction and execution boundaries for both endpoints.

use std::ffi::{OsStr, OsString};
use std::io;
use std::process::{Command, Output};
use std::sync::Mutex;

use crate::operation::OperationError;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Windows CREATE_NO_WINDOW 标志
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// 创建一个隐藏控制台窗口的 Command。
///
/// 在 Windows 上设置 CREATE_NO_WINDOW 防止弹出控制台窗口；其它平台返回普通 Command。
pub fn new_command<S: AsRef<std::ffi::OsStr>>(program: S) -> Command {
    let mut cmd = Command::new(program);

    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    cmd
}

/// A program and its arguments kept as separate OS strings.
///
/// This type deliberately has no shell-string constructor. Callers that must
/// retain a legacy shell wrapper still have to pass every wrapper argument
/// explicitly and document the compatibility reason at the call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRequest {
    program: OsString,
    args: Vec<OsString>,
}

impl CommandRequest {
    pub fn new<S: AsRef<OsStr>>(program: S) -> Self {
        Self {
            program: program.as_ref().to_os_string(),
            args: Vec::new(),
        }
    }

    pub fn arg<S: AsRef<OsStr>>(mut self, arg: S) -> Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_os_string()));
        self
    }

    pub fn program(&self) -> &OsStr {
        &self.program
    }

    pub fn arguments(&self) -> &[OsString] {
        &self.args
    }

    /// Render a diagnostic preview only. The returned text must never be fed
    /// back to a shell, and callers must avoid logging secrets.
    pub fn preview(&self) -> String {
        std::iter::once(self.program.as_os_str())
            .chain(self.args.iter().map(OsString::as_os_str))
            .map(|part| format!("{:?}", part.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Process result independent from `std::process::ExitStatus`, so mocks and
/// dry-run adapters do not need to manufacture platform-specific status data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    exit_code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl CommandOutcome {
    pub fn new(exit_code: Option<i32>, stdout: Vec<u8>, stderr: Vec<u8>) -> Self {
        Self {
            exit_code,
            stdout,
            stderr,
        }
    }

    pub fn success() -> Self {
        Self::new(Some(0), Vec::new(), Vec::new())
    }

    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }

    pub const fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }
}

impl From<Output> for CommandOutcome {
    fn from(output: Output) -> Self {
        Self::new(output.status.code(), output.stdout, output.stderr)
    }
}

pub trait CommandExecutor: Send + Sync {
    fn execute(&self, request: &CommandRequest) -> io::Result<CommandOutcome>;
}

/// Execute a request while preserving command-start failures as a shared,
/// serializable operation error. Exit and tool-specific text validation remain
/// the caller's responsibility because utilities such as DiskPart can report
/// failure despite a zero exit code.
pub fn execute_request<E>(
    executor: &E,
    request: &CommandRequest,
) -> Result<CommandOutcome, OperationError>
where
    E: CommandExecutor + ?Sized,
{
    executor.execute(request).map_err(|error| {
        OperationError::command_start(&request.program().to_string_lossy(), &error)
    })
}

/// Production executor. It preserves the shared `CREATE_NO_WINDOW` behavior.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemCommandExecutor;

impl CommandExecutor for SystemCommandExecutor {
    fn execute(&self, request: &CommandRequest) -> io::Result<CommandOutcome> {
        new_command(request.program())
            .args(request.arguments())
            .output()
            .map(CommandOutcome::from)
    }
}

/// Executor for tests and internal command previews. It records requests and
/// returns a configured result without ever starting a process.
#[derive(Debug)]
pub struct DryRunCommandExecutor {
    outcome: CommandOutcome,
    requests: Mutex<Vec<CommandRequest>>,
}

impl DryRunCommandExecutor {
    pub fn new(outcome: CommandOutcome) -> Self {
        Self {
            outcome,
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn requests(&self) -> io::Result<Vec<CommandRequest>> {
        self.requests
            .lock()
            .map(|requests| requests.clone())
            .map_err(|_| io::Error::other("dry-run command request lock was poisoned"))
    }
}

impl Default for DryRunCommandExecutor {
    fn default() -> Self {
        Self::new(CommandOutcome::success())
    }
}

impl CommandExecutor for DryRunCommandExecutor {
    fn execute(&self, request: &CommandRequest) -> io::Result<CommandOutcome> {
        self.requests
            .lock()
            .map_err(|_| io::Error::other("dry-run command request lock was poisoned"))?
            .push(request.clone());
        Ok(self.outcome.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_program_name() {
        let cmd = new_command("reg.exe");
        assert_eq!(cmd.get_program(), OsStr::new("reg.exe"));
    }

    #[test]
    fn request_keeps_spaces_unicode_and_metacharacters_as_one_argument() {
        let request = CommandRequest::new("format.com").args([
            "D:",
            "/FS:NTFS",
            "/V:数据 & ^ % !",
            "/Q",
            "/Y",
        ]);

        assert_eq!(request.program(), OsStr::new("format.com"));
        assert_eq!(request.arguments().len(), 5);
        assert_eq!(request.arguments()[2], OsStr::new("/V:数据 & ^ % !"));
        assert!(request.preview().contains("数据 & ^ % !"));
    }

    #[test]
    fn dry_run_records_without_starting_a_process() {
        let executor = DryRunCommandExecutor::default();
        let request = CommandRequest::new("this-program-must-not-exist.exe").arg("--write-disk");

        let outcome = executor.execute(&request).unwrap();

        assert!(outcome.succeeded());
        assert_eq!(executor.requests().unwrap(), vec![request]);
    }

    #[test]
    fn dry_run_can_model_a_failed_command() {
        let executor = DryRunCommandExecutor::new(CommandOutcome::new(
            Some(5),
            b"partial output".to_vec(),
            b"access denied".to_vec(),
        ));

        let outcome = executor
            .execute(&CommandRequest::new("format.com"))
            .unwrap();

        assert!(!outcome.succeeded());
        assert_eq!(outcome.exit_code(), Some(5));
        assert_eq!(outcome.stdout(), b"partial output");
        assert_eq!(outcome.stderr(), b"access denied");
    }

    struct StartFailureExecutor;

    impl CommandExecutor for StartFailureExecutor {
        fn execute(&self, _request: &CommandRequest) -> io::Result<CommandOutcome> {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "modeled access denied",
            ))
        }
    }

    #[test]
    fn typed_execution_distinguishes_command_start_failure() {
        let error = execute_request(&StartFailureExecutor, &CommandRequest::new("diskpart.exe"))
            .unwrap_err();

        assert_eq!(
            error.kind,
            crate::operation::OperationErrorKind::CommandStart
        );
        assert_eq!(error.code.as_deref(), Some("diskpart.exe"));
        assert!(!error.retryable);
    }
}
