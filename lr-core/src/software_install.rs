//! Shared v4 software-install selection and silent-command parsing.
//!
//! The catalogue string is never passed to `cmd.exe` or another shell. It is
//! parsed into a program kind and an argv vector, then executed through the
//! parameterized command boundary. Only the downloaded installer itself or
//! Windows' `msiexec.exe` may be selected as the program.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use url::Url;

const INSTALLER_PLACEHOLDER: &str = "{installer}";
pub const FIRST_LOGON_INSTALLER_ARGUMENT_PLACEHOLDER: &str =
    "__LETRECOVERY_FIRST_LOGON_INSTALLER__";
const MAX_TEMPLATE_CHARS: usize = 2_048;
pub const MAX_SELECTED_SOFTWARE_PACKAGES: usize = 128;
pub const MAX_SELECTED_SOFTWARE_CONFIG_BYTES: usize = 1024 * 1024;
pub const STAGING_DIRECTORY_NAME: &str = "PreinstalledSoftware";

fn validation_installer_path(filename: &str) -> PathBuf {
    // This path is used only to parse and validate the server template. It is deliberately a
    // non-existent namespaced path rather than a guessed system drive; no filesystem access occurs.
    PathBuf::from(format!(
        r"\\?\LetRecoveryTemplate\LetRecovery_Scripts\PreinstalledSoftware\{filename}"
    ))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedSoftwarePackage {
    pub id: String,
    pub name: String,
    pub download_url: String,
    pub filename: String,
    pub silent_command: String,
    pub requires_admin: bool,
}

pub fn validate_selected_packages(packages: &[SelectedSoftwarePackage]) -> Result<()> {
    if packages.len() > MAX_SELECTED_SOFTWARE_PACKAGES {
        bail!("too many selected software packages");
    }
    let mut ids = BTreeSet::new();
    let mut filenames = BTreeSet::new();
    for package in packages {
        validate_token(&package.id, "software id", 128)?;
        validate_text(&package.name, "software name", 256)?;
        validate_filename(&package.filename)?;
        let url = Url::parse(&package.download_url).context("parse software download URL")?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.host_str().is_none()
        {
            bail!("software download URL is not an anonymous HTTP(S) URL");
        }
        let target = validation_installer_path(&package.filename);
        parse_silent_install_template(&package.silent_command, &target)
            .with_context(|| format!("validate silent command for {}", package.name))?;
        if !ids.insert(package.id.to_ascii_lowercase()) {
            bail!("selected software id appears more than once");
        }
        if !filenames.insert(package.filename.to_ascii_lowercase()) {
            bail!("selected software filename appears more than once");
        }
    }
    Ok(())
}

pub fn encode_selected_packages(packages: &[SelectedSoftwarePackage]) -> Result<String> {
    validate_selected_packages(packages)?;
    let json = serde_json::to_vec(packages).context("serialize selected software")?;
    if json.len() > MAX_SELECTED_SOFTWARE_CONFIG_BYTES {
        bail!("selected software configuration exceeds its byte limit");
    }
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json))
}

pub fn decode_selected_packages(encoded: &str) -> Result<Vec<SelectedSoftwarePackage>> {
    if encoded.len() > MAX_SELECTED_SOFTWARE_CONFIG_BYTES.saturating_mul(2) {
        bail!("encoded selected software configuration exceeds its byte limit");
    }
    let json = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded.trim())
        .context("decode selected software configuration")?;
    if json.len() > MAX_SELECTED_SOFTWARE_CONFIG_BYTES {
        bail!("selected software configuration exceeds its byte limit");
    }
    let packages: Vec<SelectedSoftwarePackage> =
        serde_json::from_slice(&json).context("parse selected software configuration")?;
    validate_selected_packages(&packages)?;
    Ok(packages)
}

fn validate_token(value: &str, label: &str, limit: usize) -> Result<()> {
    if value.is_empty()
        || value.len() > limit
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        bail!("{label} is malformed");
    }
    Ok(())
}

fn validate_text(value: &str, label: &str, limit: usize) -> Result<()> {
    if value.trim().is_empty()
        || value.chars().count() > limit
        || value.chars().any(|character| character.is_control())
    {
        bail!("{label} is malformed");
    }
    Ok(())
}

fn validate_filename(value: &str) -> Result<()> {
    validate_text(value, "software filename", 255)?;
    let path = Path::new(value);
    if path.file_name().and_then(|name| name.to_str()) != Some(value)
        || value == "."
        || value == ".."
        || value.contains(['/', '\\', ':'])
    {
        bail!("software filename is not a single safe path component");
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SilentInstallerProgram {
    DownloadedInstaller(PathBuf),
    WindowsInstaller,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedSilentInstall {
    pub program: SilentInstallerProgram,
    pub arguments: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirstLogonSoftwarePlanEntry {
    pub id: String,
    pub name: String,
    pub filename: String,
    pub program: String,
    pub arguments: Vec<String>,
}

pub fn first_logon_plan_bytes(packages: &[SelectedSoftwarePackage]) -> Result<Vec<u8>> {
    validate_selected_packages(packages)?;
    let entries = packages
        .iter()
        .map(|package| {
            let installer = validation_installer_path(&package.filename);
            let mut parsed = parse_silent_install_template(&package.silent_command, &installer)
                .with_context(|| format!("render silent command for {}", package.name))?;
            for argument in &mut parsed.arguments {
                if paths_equal(argument, &installer) {
                    *argument = FIRST_LOGON_INSTALLER_ARGUMENT_PLACEHOLDER.to_owned();
                }
            }
            let program = match parsed.program {
                SilentInstallerProgram::DownloadedInstaller(_) => "installer",
                SilentInstallerProgram::WindowsInstaller => "msiexec",
            };
            Ok(FirstLogonSoftwarePlanEntry {
                id: package.id.clone(),
                name: package.name.clone(),
                filename: package.filename.clone(),
                program: program.to_owned(),
                arguments: parsed.arguments,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let bytes = serde_json::to_vec(&entries).context("serialize first-logon software plan")?;
    if bytes.len() > MAX_SELECTED_SOFTWARE_CONFIG_BYTES {
        bail!("first-logon software plan exceeds its byte limit");
    }
    Ok(bytes)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SilentCommandError {
    Empty,
    TooLong,
    PlaceholderCount(usize),
    UnterminatedQuote,
    EmptyArgumentVector,
    UnsupportedProgram(String),
    InstallerArgumentMissing,
}

impl fmt::Display for SilentCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("silent command is empty"),
            Self::TooLong => formatter.write_str("silent command exceeds 2048 characters"),
            Self::PlaceholderCount(count) => write!(
                formatter,
                "silent command must contain exactly one {{installer}} placeholder, found {count}"
            ),
            Self::UnterminatedQuote => {
                formatter.write_str("silent command has an unterminated quote")
            }
            Self::EmptyArgumentVector => formatter.write_str("silent command has no program"),
            Self::UnsupportedProgram(program) => {
                write!(
                    formatter,
                    "silent command program is not supported: {program}"
                )
            }
            Self::InstallerArgumentMissing => formatter.write_str(
                "msiexec silent command does not contain the downloaded installer argument",
            ),
        }
    }
}

impl std::error::Error for SilentCommandError {}

pub fn parse_silent_install_template(
    template: &str,
    installer_path: &Path,
) -> Result<ParsedSilentInstall, SilentCommandError> {
    let template = template.trim();
    if template.is_empty() {
        return Err(SilentCommandError::Empty);
    }
    if template.chars().count() > MAX_TEMPLATE_CHARS {
        return Err(SilentCommandError::TooLong);
    }
    let placeholder_count = template.matches(INSTALLER_PLACEHOLDER).count();
    if placeholder_count != 1 {
        return Err(SilentCommandError::PlaceholderCount(placeholder_count));
    }

    let installer = installer_path.to_string_lossy();
    let rendered = template.replace(INSTALLER_PLACEHOLDER, &installer);
    let mut argv = split_windows_command_line(&rendered)?;
    if argv.is_empty() {
        return Err(SilentCommandError::EmptyArgumentVector);
    }
    let program_text = argv.remove(0);
    let program = if paths_equal(&program_text, installer_path) {
        SilentInstallerProgram::DownloadedInstaller(installer_path.to_path_buf())
    } else if program_text.eq_ignore_ascii_case("msiexec.exe")
        || program_text.eq_ignore_ascii_case("msiexec")
    {
        if !argv
            .iter()
            .any(|argument| paths_equal(argument, installer_path))
        {
            return Err(SilentCommandError::InstallerArgumentMissing);
        }
        SilentInstallerProgram::WindowsInstaller
    } else {
        return Err(SilentCommandError::UnsupportedProgram(program_text));
    };

    Ok(ParsedSilentInstall {
        program,
        arguments: argv,
    })
}

fn paths_equal(candidate: &str, expected: &Path) -> bool {
    candidate.eq_ignore_ascii_case(&expected.to_string_lossy())
}

/// Parses the Windows CRT command-line quoting rules into argv without invoking
/// a shell. An unmatched quote is rejected instead of being guessed.
fn split_windows_command_line(input: &str) -> Result<Vec<String>, SilentCommandError> {
    let chars: Vec<char> = input.chars().collect();
    let mut index = 0usize;
    let mut arguments = Vec::new();
    while index < chars.len() {
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        if index == chars.len() {
            break;
        }
        let mut argument = String::new();
        let mut quoted = false;
        while index < chars.len() {
            if !quoted && chars[index].is_whitespace() {
                break;
            }
            let mut slash_count = 0usize;
            while index < chars.len() && chars[index] == '\\' {
                slash_count += 1;
                index += 1;
            }
            if index < chars.len() && chars[index] == '"' {
                argument.extend(std::iter::repeat_n('\\', slash_count / 2));
                if slash_count.is_multiple_of(2) {
                    quoted = !quoted;
                } else {
                    argument.push('"');
                }
                index += 1;
                continue;
            }
            argument.extend(std::iter::repeat_n('\\', slash_count));
            if index < chars.len() && (!chars[index].is_whitespace() || quoted) {
                argument.push(chars[index]);
                index += 1;
            }
        }
        if quoted {
            return Err(SilentCommandError::UnterminatedQuote);
        }
        arguments.push(argument);
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
    }
    Ok(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_downloaded_executable_with_quoted_arguments() {
        let path = Path::new(r"C:\LetRecovery Scripts\AnyDesk.exe");
        let parsed = parse_silent_install_template(
            r#""{installer}" --install "C:\Program Files (x86)\AnyDesk" --silent"#,
            path,
        )
        .unwrap();
        assert_eq!(
            parsed.program,
            SilentInstallerProgram::DownloadedInstaller(path.to_path_buf())
        );
        assert_eq!(
            parsed.arguments,
            ["--install", r"C:\Program Files (x86)\AnyDesk", "--silent"]
        );
    }

    #[test]
    fn parses_windows_installer_without_shell_text() {
        let path = Path::new(r"C:\Packages\tool.msi");
        let parsed =
            parse_silent_install_template(r#"msiexec.exe /i "{installer}" /qn /norestart"#, path)
                .unwrap();
        assert_eq!(parsed.program, SilentInstallerProgram::WindowsInstaller);
        assert_eq!(
            parsed.arguments,
            ["/i", r"C:\Packages\tool.msi", "/qn", "/norestart"]
        );
    }

    #[test]
    fn parses_vmware_tools_nested_msi_arguments_without_a_shell() {
        let path = Path::new(r"C:\Packages\VMware-tools.exe");
        let parsed =
            parse_silent_install_template(r#""{installer}" /S /v"/qn REBOOT=R""#, path).unwrap();
        assert_eq!(
            parsed.program,
            SilentInstallerProgram::DownloadedInstaller(path.to_path_buf())
        );
        assert_eq!(parsed.arguments, ["/S", "/v/qn REBOOT=R"]);
    }

    #[test]
    fn rejects_other_programs_and_ambiguous_placeholders() {
        let path = Path::new(r"C:\Packages\tool.exe");
        assert!(matches!(
            parse_silent_install_template(r#"cmd.exe /c "{installer}" /S"#, path),
            Err(SilentCommandError::UnsupportedProgram(_))
        ));
        assert!(matches!(
            parse_silent_install_template(r#""{installer}" "{installer}""#, path),
            Err(SilentCommandError::PlaceholderCount(2))
        ));
    }

    #[test]
    fn selected_package_config_round_trips_and_rejects_unsafe_inputs() {
        let package = SelectedSoftwarePackage {
            id: "vmware-tools-x64".to_owned(),
            name: "VMware Tools".to_owned(),
            download_url: "https://packages.vmware.com/tools/tool.exe".to_owned(),
            filename: "VMware-tools.exe".to_owned(),
            silent_command: r#""{installer}" /S /v"/qn REBOOT=R""#.to_owned(),
            requires_admin: true,
        };
        let encoded = encode_selected_packages(std::slice::from_ref(&package)).unwrap();
        let decoded = decode_selected_packages(&encoded).unwrap();
        assert_eq!(decoded.as_slice(), std::slice::from_ref(&package));

        let mut unsafe_package = package;
        unsafe_package.filename = r"..\tool.exe".to_owned();
        assert!(validate_selected_packages(&[unsafe_package]).is_err());
    }

    #[test]
    fn duplicate_ids_or_filenames_are_rejected_case_insensitively() {
        let package = SelectedSoftwarePackage {
            id: "tool".to_owned(),
            name: "Tool".to_owned(),
            download_url: "https://example.com/tool.exe".to_owned(),
            filename: "Tool.exe".to_owned(),
            silent_command: r#""{installer}" /S"#.to_owned(),
            requires_admin: true,
        };
        let mut duplicate = package.clone();
        duplicate.id = "TOOL".to_owned();
        duplicate.filename = "other.exe".to_owned();
        assert!(validate_selected_packages(&[package.clone(), duplicate]).is_err());
        let mut duplicate = package.clone();
        duplicate.id = "other".to_owned();
        duplicate.filename = "tool.EXE".to_owned();
        assert!(validate_selected_packages(&[package, duplicate]).is_err());
    }

    #[test]
    fn first_logon_plan_contains_only_parameterized_program_kinds() {
        let package = SelectedSoftwarePackage {
            id: "tool".to_owned(),
            name: "Tool".to_owned(),
            download_url: "https://example.com/tool.msi".to_owned(),
            filename: "tool.msi".to_owned(),
            silent_command: r#"msiexec.exe /i "{installer}" /qn"#.to_owned(),
            requires_admin: true,
        };
        let bytes = first_logon_plan_bytes(&[package]).unwrap();
        let entries: Vec<FirstLogonSoftwarePlanEntry> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(entries[0].program, "msiexec");
        assert!(entries[0]
            .arguments
            .iter()
            .any(|argument| argument == FIRST_LOGON_INSTALLER_ARGUMENT_PLACEHOLDER));
        assert!(!String::from_utf8(bytes).unwrap().contains(r"C:\"));
    }
}
