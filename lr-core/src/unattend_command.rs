//! Shared rendering and deterministic limits for Windows unattended commands.
//!
//! Microsoft documents `Microsoft-Windows-Deployment/RunSynchronousCommand/Path` as a
//! non-empty string with a maximum length of 259 characters and `Order` as 1 through 500.
//! Keeping that contract here prevents a well-formed XML document from passing local tests and
//! then being rejected by Windows SMI during the destructive post-apply `specialize` pass.
//!
//! Microsoft references:
//! <https://learn.microsoft.com/windows-hardware/customize/desktop/unattend/microsoft-windows-deployment-runsynchronous-runsynchronouscommand-path>
//! <https://learn.microsoft.com/windows-hardware/customize/desktop/unattend/microsoft-windows-deployment-runsynchronous-runsynchronouscommand-order>
//! <https://learn.microsoft.com/windows-hardware/customize/desktop/unattend/microsoft-windows-shell-setup-firstlogoncommands-synchronouscommand-commandline>

use anyhow::{bail, Result};

pub const RUN_SYNCHRONOUS_PATH_MAX_UTF16: usize = 259;
pub const RUN_SYNCHRONOUS_DESCRIPTION_MAX_UTF16: usize = 259;
pub const RUN_SYNCHRONOUS_ORDER_MAX: u32 = 500;
pub const FIRST_LOGON_COMMAND_LINE_MAX_UTF16: usize = 1024;
/// With `WillReboot=OnRequest`, 1 and 2 request documented reboot behavior. Required helpers
/// therefore normalize every failure to the first unambiguously failing exit code.
pub const REQUIRED_SPECIALIZE_FAILURE_EXIT_CODE: i32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequiredBuiltinUnattendError {
    UnattendedDisabled,
    CustomUnattend,
    UnsupportedSource,
}

/// Validate options whose selected result can only be proved by LetRecovery's built-in
/// specialize/first-logon hooks. Call this before the first destructive target write.
pub fn validate_required_builtin_unattend(
    required: bool,
    unattended: bool,
    has_custom_unattend: bool,
    unsupported_source: bool,
) -> std::result::Result<(), RequiredBuiltinUnattendError> {
    if !required {
        return Ok(());
    }
    if !unattended {
        return Err(RequiredBuiltinUnattendError::UnattendedDisabled);
    }
    if has_custom_unattend {
        return Err(RequiredBuiltinUnattendError::CustomUnattend);
    }
    if unsupported_source {
        return Err(RequiredBuiltinUnattendError::UnsupportedSource);
    }
    Ok(())
}

pub fn render_specialize_run_synchronous_command(
    order: u32,
    path: &str,
    description: &str,
) -> Result<String> {
    validate_specialize_run_synchronous_command(order, path, description)?;
    let path = xml_escape(path);
    let description = xml_escape(description);
    Ok(format!(
        r#"
                <RunSynchronousCommand wcm:action="add">
                    <Order>{order}</Order>
                    <Path>{path}</Path>
                    <Description>{description}</Description>
                </RunSynchronousCommand>"#
    ))
}

/// Render a load-bearing specialize command. With `WillReboot=OnRequest`, Windows Setup treats
/// exit codes other than 0, 1, or 2 as failure instead of silently continuing; callers must
/// normalize their own failures to 3 or greater.
pub fn render_required_specialize_run_synchronous_command(
    order: u32,
    path: &str,
    description: &str,
) -> Result<String> {
    validate_specialize_run_synchronous_command(order, path, description)?;
    let path = xml_escape(path);
    let description = xml_escape(description);
    Ok(format!(
        r#"
                <RunSynchronousCommand wcm:action="add">
                    <Order>{order}</Order>
                    <Path>{path}</Path>
                    <Description>{description}</Description>
                    <WillReboot>OnRequest</WillReboot>
                </RunSynchronousCommand>"#
    ))
}

pub fn render_first_logon_synchronous_command(
    order: u32,
    command_line: &str,
    description: &str,
) -> Result<String> {
    if !(1..=RUN_SYNCHRONOUS_ORDER_MAX).contains(&order) {
        bail!("first-logon command order must be between 1 and {RUN_SYNCHRONOUS_ORDER_MAX}");
    }
    validate_nonempty_utf16_field(
        "CommandLine",
        command_line,
        FIRST_LOGON_COMMAND_LINE_MAX_UTF16,
    )?;
    validate_nonempty_utf16_field(
        "Description",
        description,
        RUN_SYNCHRONOUS_DESCRIPTION_MAX_UTF16,
    )?;
    let command_line = xml_escape(command_line);
    let description = xml_escape(description);
    Ok(format!(
        r#"
                <SynchronousCommand wcm:action="add">
                    <Order>{order}</Order>
                    <CommandLine>{command_line}</CommandLine>
                    <Description>{description}</Description>
                </SynchronousCommand>"#
    ))
}

pub fn validate_specialize_run_synchronous_command(
    order: u32,
    path: &str,
    description: &str,
) -> Result<()> {
    if !(1..=RUN_SYNCHRONOUS_ORDER_MAX).contains(&order) {
        bail!("specialize command order must be between 1 and {RUN_SYNCHRONOUS_ORDER_MAX}");
    }
    validate_nonempty_utf16_field("Path", path, RUN_SYNCHRONOUS_PATH_MAX_UTF16)?;
    validate_nonempty_utf16_field(
        "Description",
        description,
        RUN_SYNCHRONOUS_DESCRIPTION_MAX_UTF16,
    )?;
    Ok(())
}

fn validate_nonempty_utf16_field(name: &str, value: &str, maximum: usize) -> Result<()> {
    if value.is_empty() {
        bail!("specialize command {name} must not be empty");
    }
    if value.chars().any(|character| {
        matches!(character, '\0' | '\r' | '\n') || !is_xml_1_0_character(character)
    }) {
        bail!("specialize command {name} contains invalid text");
    }
    let length = value.encode_utf16().count();
    if length > maximum {
        bail!(
            "specialize command {name} is {length} UTF-16 code units; Windows allows at most {maximum}"
        );
    }
    Ok(())
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn is_xml_1_0_character(character: char) -> bool {
    matches!(character, '\u{9}')
        || ('\u{20}'..='\u{D7FF}').contains(&character)
        || ('\u{E000}'..='\u{FFFD}').contains(&character)
        || ('\u{10000}'..='\u{10FFFF}').contains(&character)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_decoded_path_and_enforces_microsoft_limits() {
        let path = r#"cmd /d /c echo one & echo two > nul"#;
        let fragment = render_specialize_run_synchronous_command(500, path, "Run & verify")
            .expect("documented boundary is valid");
        let document = format!(
            r#"<root xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">{fragment}</root>"#
        );
        let parsed = roxmltree::Document::parse(&document).unwrap();
        let decoded = parsed
            .descendants()
            .find(|node| node.tag_name().name() == "Path")
            .and_then(|node| node.text())
            .unwrap();
        assert_eq!(decoded, path);

        assert!(render_specialize_run_synchronous_command(0, "cmd", "x").is_err());
        assert!(render_specialize_run_synchronous_command(501, "cmd", "x").is_err());
        assert!(render_specialize_run_synchronous_command(1, "", "x").is_err());
        assert!(render_specialize_run_synchronous_command(
            1,
            &"a".repeat(RUN_SYNCHRONOUS_PATH_MAX_UTF16),
            "x"
        )
        .is_ok());
        assert!(render_specialize_run_synchronous_command(
            1,
            &"a".repeat(RUN_SYNCHRONOUS_PATH_MAX_UTF16 + 1),
            "x"
        )
        .is_err());
    }

    #[test]
    fn required_hooks_are_rejected_before_destructive_work_when_unavailable() {
        use RequiredBuiltinUnattendError as Error;

        assert_eq!(
            validate_required_builtin_unattend(true, false, false, false),
            Err(Error::UnattendedDisabled)
        );
        assert_eq!(
            validate_required_builtin_unattend(true, true, true, false),
            Err(Error::CustomUnattend)
        );
        assert_eq!(
            validate_required_builtin_unattend(true, true, false, true),
            Err(Error::UnsupportedSource)
        );
        assert!(validate_required_builtin_unattend(true, true, false, false).is_ok());
        assert!(validate_required_builtin_unattend(false, false, true, true).is_ok());
    }

    #[test]
    fn first_logon_command_line_uses_its_documented_limit() {
        assert!(render_first_logon_synchronous_command(
            1,
            &"a".repeat(FIRST_LOGON_COMMAND_LINE_MAX_UTF16),
            "x"
        )
        .is_ok());
        assert!(render_first_logon_synchronous_command(
            1,
            &"a".repeat(FIRST_LOGON_COMMAND_LINE_MAX_UTF16 + 1),
            "x"
        )
        .is_err());
    }

    #[test]
    fn required_specialize_command_declares_on_request_exit_contract() {
        let fragment = render_required_specialize_run_synchronous_command(
            7,
            "cmd /d /c exit /b 3",
            "Required action",
        )
        .unwrap();
        assert!(fragment.contains("<WillReboot>OnRequest</WillReboot>"));
        assert_eq!(
            std::hint::black_box(REQUIRED_SPECIALIZE_FAILURE_EXIT_CODE),
            3
        );
        assert!(fragment.contains("<Order>7</Order>"));
    }
}
