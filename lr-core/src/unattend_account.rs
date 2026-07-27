//! Shared unattended-account policy and XML fragments.
//!
//! The built-in Administrator is identified by RID 500. A localized display name must never be
//! used to guess that identity, and a requested rename must not be implemented by creating a new
//! local administrator with the same visible name.

use serde::{Deserialize, Serialize};
use std::fmt;

const DEFAULT_ADMINISTRATOR_NAME: &str = "Administrator";
const MAX_LOCAL_ACCOUNT_NAME_UTF16: usize = 20;
const MAX_PASSWORD_UTF16: usize = 127;

/// A runtime secret whose debug representation never exposes its contents.
///
/// This wrapper is intentionally not serializable. Callers must opt in explicitly when writing the
/// short-lived PE handoff or unattended answer file.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SensitiveString(String);

impl SensitiveString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }
}

impl From<String> for SensitiveString {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for SensitiveString {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl fmt::Debug for SensitiveString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BuiltInAdministratorOptions {
    pub enabled: bool,
    pub account_name: String,
    #[serde(skip)]
    pub password: SensitiveString,
    pub auto_logon: bool,
}

impl Default for BuiltInAdministratorOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            account_name: DEFAULT_ADMINISTRATOR_NAME.to_string(),
            password: SensitiveString::default(),
            auto_logon: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltInAdministratorValidationError {
    MissingAccountName,
    AccountNameTooLong,
    InvalidAccountName,
    ReservedAccountName,
    MissingPassword,
    PasswordTooLong,
    InvalidPassword,
}

impl fmt::Display for BuiltInAdministratorValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MissingAccountName => "built-in Administrator account name is required",
            Self::AccountNameTooLong => {
                "built-in Administrator account name exceeds 20 UTF-16 code units"
            }
            Self::InvalidAccountName => "built-in Administrator account name contains invalid text",
            Self::ReservedAccountName => "built-in Administrator account name is reserved",
            Self::MissingPassword => "built-in Administrator password is required",
            Self::PasswordTooLong => {
                "built-in Administrator password exceeds 127 UTF-16 code units"
            }
            Self::InvalidPassword => "built-in Administrator password contains invalid text",
        })
    }
}

impl std::error::Error for BuiltInAdministratorValidationError {}

impl BuiltInAdministratorOptions {
    pub fn validate(&self) -> Result<(), BuiltInAdministratorValidationError> {
        if !self.enabled {
            return Ok(());
        }
        let name = self.account_name.trim();
        if name.is_empty() {
            return Err(BuiltInAdministratorValidationError::MissingAccountName);
        }
        if name.encode_utf16().count() > MAX_LOCAL_ACCOUNT_NAME_UTF16 {
            return Err(BuiltInAdministratorValidationError::AccountNameTooLong);
        }
        if name != self.account_name
            || name == "."
            || name == ".."
            || name.chars().any(|character| {
                character.is_control()
                    || !is_xml_1_0_character(character)
                    || r#"\/"[]:|<>+=;,?*%@"#.contains(character)
            })
        {
            return Err(BuiltInAdministratorValidationError::InvalidAccountName);
        }
        if [
            "Guest",
            "DefaultAccount",
            "WDAGUtilityAccount",
            "defaultuser0",
        ]
        .iter()
        .any(|reserved| name.eq_ignore_ascii_case(reserved))
        {
            return Err(BuiltInAdministratorValidationError::ReservedAccountName);
        }

        let password = self.password.expose_secret();
        if password.is_empty() {
            return Err(BuiltInAdministratorValidationError::MissingPassword);
        }
        if password.encode_utf16().count() > MAX_PASSWORD_UTF16 {
            return Err(BuiltInAdministratorValidationError::PasswordTooLong);
        }
        if password.chars().any(|character| {
            matches!(character, '\r' | '\n' | '\0') || !is_xml_1_0_character(character)
        }) {
            return Err(BuiltInAdministratorValidationError::InvalidPassword);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuiltInAdministratorUnattend {
    /// Optional `Microsoft-Windows-Deployment/RunSynchronousCommand` used to rename RID 500.
    pub specialize_command: String,
    /// `Microsoft-Windows-Shell-Setup/UserAccounts` contents.
    pub user_accounts: String,
    /// Optional `Microsoft-Windows-Shell-Setup/AutoLogon` contents.
    pub auto_logon: String,
}

pub fn render_builtin_administrator_unattend(
    options: &BuiltInAdministratorOptions,
    specialize_order: u32,
) -> Result<Option<BuiltInAdministratorUnattend>, BuiltInAdministratorValidationError> {
    options.validate()?;
    if !options.enabled {
        return Ok(None);
    }

    let account_name = xml_escape(&options.account_name);
    let password = xml_escape(options.password.expose_secret());
    let specialize_command = if options
        .account_name
        .eq_ignore_ascii_case(DEFAULT_ADMINISTRATOR_NAME)
    {
        String::new()
    } else {
        let encoded = encoded_rename_rid_500_script(&options.account_name);
        format!(
            r#"
                <RunSynchronousCommand wcm:action="add">
                    <Order>{specialize_order}</Order>
                    <Path>powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand {encoded}</Path>
                    <Description>Rename built-in Administrator account</Description>
                </RunSynchronousCommand>"#
        )
    };
    let user_accounts = format!(
        r#"<UserAccounts>
                <AdministratorPassword>
                    <Value>{password}</Value>
                    <PlainText>true</PlainText>
                </AdministratorPassword>
            </UserAccounts>"#
    );
    let auto_logon = if options.auto_logon {
        format!(
            r#"<AutoLogon>
                <Password>
                    <Value>{password}</Value>
                    <PlainText>true</PlainText>
                </Password>
                <Enabled>true</Enabled>
                <LogonCount>1</LogonCount>
                <Username>{account_name}</Username>
            </AutoLogon>"#
        )
    } else {
        String::new()
    };

    Ok(Some(BuiltInAdministratorUnattend {
        specialize_command,
        user_accounts,
        auto_logon,
    }))
}

fn encoded_rename_rid_500_script(account_name: &str) -> String {
    // The requested name is itself Base64-encoded before being placed in the script. This keeps
    // every user-controlled character out of PowerShell syntax.
    let name_base64 = base64_encode(account_name.as_bytes());
    let script = format!(
        "$ErrorActionPreference='Stop';\
$name=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{name_base64}'));\
$account=Get-WmiObject -Class Win32_UserAccount -Filter 'LocalAccount=True' | Where-Object {{$_.SID -match '-500$'}} | Select-Object -First 1;\
if ($null -eq $account) {{ throw 'Built-in Administrator RID 500 was not found' }};\
if ($account.Name -ne $name) {{ $result=$account.Rename($name); if ([int]$result.ReturnValue -ne 0) {{ throw ('RID 500 rename failed: '+$result.ReturnValue) }} }}"
    );
    let utf16le = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    base64_encode(&utf16le)
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(TABLE[(first >> 2) as usize] as char);
        output.push(TABLE[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        output.push(if chunk.len() >= 2 {
            TABLE[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() == 3 {
            TABLE[(third & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    output
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
    matches!(character, '\u{9}' | '\u{A}' | '\u{D}')
        || ('\u{20}'..='\u{D7FF}').contains(&character)
        || ('\u{E000}'..='\u{FFFD}').contains(&character)
        || ('\u{10000}'..='\u{10FFFF}').contains(&character)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled(name: &str, password: &str, auto_logon: bool) -> BuiltInAdministratorOptions {
        BuiltInAdministratorOptions {
            enabled: true,
            account_name: name.to_string(),
            password: password.into(),
            auto_logon,
        }
    }

    #[test]
    fn defaults_are_safe_and_do_not_persist_a_password() {
        let options = BuiltInAdministratorOptions::default();
        assert!(!options.enabled);
        assert_eq!(options.account_name, "Administrator");
        assert!(options.password.is_empty());
        assert!(!options.auto_logon);

        let serialized = serde_json::to_string(&enabled("Ops", "Secret!", true)).unwrap();
        assert!(!serialized.contains("Secret!"));
        assert!(!format!("{:?}", enabled("Ops", "Secret!", true)).contains("Secret!"));
    }

    #[test]
    fn validation_rejects_unsafe_or_ambiguous_values() {
        assert_eq!(
            enabled("Ops/Admin", "Secret!", false).validate(),
            Err(BuiltInAdministratorValidationError::InvalidAccountName)
        );
        assert_eq!(
            enabled("Guest", "Secret!", false).validate(),
            Err(BuiltInAdministratorValidationError::ReservedAccountName)
        );
        assert_eq!(
            enabled("Ops", "", false).validate(),
            Err(BuiltInAdministratorValidationError::MissingPassword)
        );
        assert_eq!(
            enabled("Ops", "line1\nline2", false).validate(),
            Err(BuiltInAdministratorValidationError::InvalidPassword)
        );
    }

    #[test]
    fn default_name_needs_no_rename_command_and_can_auto_logon() {
        let rendered =
            render_builtin_administrator_unattend(&enabled("Administrator", "A<&B", true), 2)
                .unwrap()
                .unwrap();
        assert!(rendered.specialize_command.is_empty());
        assert!(rendered.user_accounts.contains("<AdministratorPassword>"));
        assert!(rendered
            .user_accounts
            .contains("<Value>A&lt;&amp;B</Value>"));
        assert!(rendered
            .auto_logon
            .contains("<Username>Administrator</Username>"));
        assert!(!rendered.user_accounts.contains("<LocalAccount"));
    }

    #[test]
    fn custom_name_uses_only_encoded_data_in_specialize_command() {
        let rendered =
            render_builtin_administrator_unattend(&enabled("运维 Admin", "Secret!", false), 7)
                .unwrap()
                .unwrap();
        assert!(rendered.specialize_command.contains("<Order>7</Order>"));
        assert!(rendered.specialize_command.contains("-EncodedCommand "));
        assert!(!rendered.specialize_command.contains("运维 Admin"));
        assert!(rendered.auto_logon.is_empty());
    }
}
