//! Shared unattended-account policy and XML fragments.
//!
//! The built-in Administrator is identified by RID 500. A localized display name must never be
//! used to guess that identity, and a requested rename must not be implemented by creating a new
//! local administrator with the same visible name.

use serde::{Deserialize, Serialize};
use std::fmt;
use zeroize::{Zeroize, Zeroizing};

pub const DEFAULT_ADMINISTRATOR_NAME: &str = "Administrator";
pub const PROTECTED_ADMINISTRATOR_SECRET_FILE_NAME: &str = "LR_AdministratorSecret.txt";
pub const PROTECTED_ADMINISTRATOR_SECRET_WIM_PATH: &str = "\\LR_AdministratorSecret.txt";
pub const PROTECTED_ADMINISTRATOR_SECRET_MAX_BYTES: u64 = 1024;
pub const TEMPORARY_OOBE_ACCOUNT_PREFIX: &str = "LrOOBE-";
const PROTECTED_ADMINISTRATOR_SECRET_MAGIC: &str = "LRAS1";
const MAX_LOCAL_ACCOUNT_NAME_UTF16: usize = 20;
const MAX_PASSWORD_UTF16: usize = 127;

/// Validation failures for an ordinary local account created by Windows Setup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalAccountNameValidationError {
    MissingAccountName,
    AccountNameTooLong,
    InvalidAccountName,
    ReservedAccountName,
}

impl fmt::Display for LocalAccountNameValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MissingAccountName => "local account name is required",
            Self::AccountNameTooLong => "local account name exceeds 20 UTF-16 code units",
            Self::InvalidAccountName => "local account name contains invalid text",
            Self::ReservedAccountName => "local account name is reserved by Windows",
        })
    }
}

impl std::error::Error for LocalAccountNameValidationError {}

/// Validates a normal local account name used by the built-in unattended file.
///
/// Besides the documented local-account syntax, the policy excludes Windows-owned identities.
/// The numbered DWM/UMFD virtual accounts are matched narrowly so ordinary names beginning with
/// the same letters remain valid.
pub fn validate_unattended_local_account_name(
    account_name: &str,
) -> Result<(), LocalAccountNameValidationError> {
    let name = account_name.trim();
    if name.is_empty() {
        return Err(LocalAccountNameValidationError::MissingAccountName);
    }
    if name.encode_utf16().count() > MAX_LOCAL_ACCOUNT_NAME_UTF16 {
        return Err(LocalAccountNameValidationError::AccountNameTooLong);
    }
    if name != account_name
        || name.ends_with('.')
        || name.chars().any(|character| {
            character.is_control()
                || !is_xml_1_0_character(character)
                || r#"\/"[]:|<>+=;,?*%@"#.contains(character)
        })
    {
        return Err(LocalAccountNameValidationError::InvalidAccountName);
    }
    if is_reserved_unattended_account_name(name) {
        return Err(LocalAccountNameValidationError::ReservedAccountName);
    }
    Ok(())
}

fn is_reserved_unattended_account_name(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        "NONE",
        "Administrator",
        "Guest",
        "DefaultAccount",
        "defaultuser0",
        "WDAGUtilityAccount",
        "WSIAccount",
        "DSMA",
        "HelpAssistant",
        "HomeGroupUser$",
        "krbtgt",
        "SYSTEM",
        "LocalSystem",
        "Local System",
        "LocalService",
        "Local Service",
        "NetworkService",
        "Network Service",
        "TrustedInstaller",
    ];
    RESERVED
        .iter()
        .any(|reserved| name.eq_ignore_ascii_case(reserved))
        || is_numbered_windows_virtual_account(name)
}

/// Returns whether a high-RID local-account name is created and owned by Windows itself.
///
/// Offline inspection handles built-in identities by RID first. This deliberately narrow helper
/// covers only identities Windows can create at RID 1000 or above; names that are merely forbidden
/// for a new unattended account are not enough to turn an existing high-RID SAM record into a
/// system identity.
pub fn is_windows_owned_local_account_name(name: &str) -> bool {
    const WINDOWS_OWNED_HIGH_RID: &[&str] = &[
        "DefaultAccount",
        "defaultuser0",
        "WDAGUtilityAccount",
        "WSIAccount",
        "DSMA",
        "HelpAssistant",
        "HomeGroupUser$",
    ];
    WINDOWS_OWNED_HIGH_RID
        .iter()
        .any(|reserved| name.eq_ignore_ascii_case(reserved))
        || is_numbered_windows_virtual_account(name)
}

fn is_numbered_windows_virtual_account(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ["dwm-", "umfd-"].iter().any(|prefix| {
        lower.strip_prefix(prefix).is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
    })
}

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
        self.0.zeroize();
    }
}

impl Drop for SensitiveString {
    fn drop(&mut self) {
        self.0.zeroize();
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
    /// Retained for configuration compatibility. The built-in account must perform one first
    /// logon so Windows creates its requested profile and runs the finalizer; the separate
    /// session-bound temporary account satisfies OOBE's cross-version account requirement.
    /// Enabled installations therefore normalize this to `true` at rendering and UI boundaries.
    pub auto_logon: bool,
}

impl Default for BuiltInAdministratorOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            account_name: DEFAULT_ADMINISTRATOR_NAME.to_string(),
            password: SensitiveString::default(),
            auto_logon: true,
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
    InvalidSpecializeCommand,
    InvalidTemporaryAccount,
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
            Self::InvalidSpecializeCommand => {
                "built-in Administrator rename command violates the Windows unattend contract"
            }
            Self::InvalidTemporaryAccount => {
                "temporary OOBE account identity is invalid or conflicts with Administrator"
            }
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

        validate_builtin_administrator_password(self.password.expose_secret())
    }
}

fn validate_builtin_administrator_password(
    password: &str,
) -> Result<(), BuiltInAdministratorValidationError> {
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

/// Serialize the only secret-bearing Administrator field for the private boot WIM.
///
/// The public data-volume INI retains the enabled flag and account name, but its password value is
/// cleared before publication. The manifest binds these exact canonical bytes as a
/// `ProtectedAdministratorSecret` artifact.
pub fn serialize_protected_administrator_secret(
    password: &SensitiveString,
) -> Result<Zeroizing<Vec<u8>>, BuiltInAdministratorValidationError> {
    let password = password.expose_secret();
    validate_builtin_administrator_password(password)?;
    let header = format!(
        "{PROTECTED_ADMINISTRATOR_SECRET_MAGIC}\r\nUtf8Length={}\r\n\r\n",
        password.len()
    );
    let mut bytes = Zeroizing::new(Vec::with_capacity(header.len() + password.len()));
    bytes.extend_from_slice(header.as_bytes());
    bytes.extend_from_slice(password.as_bytes());
    debug_assert!(bytes.len() as u64 <= PROTECTED_ADMINISTRATOR_SECRET_MAX_BYTES);
    Ok(bytes)
}

pub fn parse_protected_administrator_secret(content: &[u8]) -> Result<Zeroizing<String>, String> {
    if content.is_empty() || content.len() as u64 > PROTECTED_ADMINISTRATOR_SECRET_MAX_BYTES {
        return Err("protected Administrator secret length is outside its limit".into());
    }
    let separator = b"\r\n\r\n";
    let split = content
        .windows(separator.len())
        .position(|window| window == separator)
        .ok_or_else(|| "protected Administrator secret header is incomplete".to_string())?;
    let header = std::str::from_utf8(&content[..split])
        .map_err(|_| "protected Administrator secret header is not UTF-8".to_string())?;
    let mut lines = header.split("\r\n");
    if lines.next() != Some(PROTECTED_ADMINISTRATOR_SECRET_MAGIC) {
        return Err("unsupported protected Administrator secret".into());
    }
    let declared = lines
        .next()
        .and_then(|line| line.strip_prefix("Utf8Length="))
        .ok_or_else(|| "protected Administrator secret has no byte length".to_string())?
        .parse::<usize>()
        .map_err(|_| "protected Administrator secret byte length is invalid".to_string())?;
    if lines.next().is_some() {
        return Err("protected Administrator secret header has trailing fields".into());
    }
    let password_bytes = &content[split + separator.len()..];
    if password_bytes.len() != declared {
        return Err("protected Administrator secret byte length does not match".into());
    }
    let password = std::str::from_utf8(password_bytes)
        .map_err(|_| "protected Administrator password is not UTF-8".to_string())?;
    validate_builtin_administrator_password(password).map_err(|error| error.to_string())?;
    let parsed = Zeroizing::new(password.to_owned());
    let canonical =
        serialize_protected_administrator_secret(&SensitiveString::new(parsed.as_str()))
            .map_err(|error| error.to_string())?;
    if canonical.as_slice() != content {
        return Err("protected Administrator secret is not canonical".into());
    }
    Ok(parsed)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuiltInAdministratorUnattend {
    /// Kept so callers can compose one template for both account modes. RID 500 is intentionally
    /// prepared only after OOBE under the session-bound temporary account.
    pub specialize_command: String,
    /// `Microsoft-Windows-Shell-Setup/UserAccounts` contents.
    pub user_accounts: String,
    /// Optional `Microsoft-Windows-Shell-Setup/AutoLogon` contents.
    pub auto_logon: String,
}

/// Derive the short-lived local account that makes OOBE's account-creation postcondition
/// explicit. Microsoft documents that AutoLogon may target an existing account, but also
/// recommends creating at least one administrator through `UserAccounts` so OOBE can complete.
/// A session-derived name avoids deleting an unrelated account from a customized source image.
pub fn temporary_oobe_account_name(session_id: &str) -> Result<String, String> {
    crate::handoff_auth::validate_session_id(session_id).map_err(|error| error.to_string())?;
    Ok(format!(
        "{TEMPORARY_OOBE_ACCOUNT_PREFIX}{}",
        &session_id[..12]
    ))
}

pub fn validate_temporary_oobe_account_name(
    account_name: &str,
) -> Result<(), BuiltInAdministratorValidationError> {
    let Some(suffix) = account_name.strip_prefix(TEMPORARY_OOBE_ACCOUNT_PREFIX) else {
        return Err(BuiltInAdministratorValidationError::InvalidTemporaryAccount);
    };
    if suffix.len() != 12
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || crate::windows_accounts::validate_new_account_name(account_name).is_err()
    {
        return Err(BuiltInAdministratorValidationError::InvalidTemporaryAccount);
    }
    Ok(())
}

pub fn render_builtin_administrator_unattend(
    options: &BuiltInAdministratorOptions,
    specialize_order: u32,
    temporary_oobe_account: &str,
) -> Result<Option<BuiltInAdministratorUnattend>, BuiltInAdministratorValidationError> {
    options.validate()?;
    if !options.enabled {
        return Ok(None);
    }
    validate_temporary_oobe_account_name(temporary_oobe_account)?;
    if options
        .account_name
        .eq_ignore_ascii_case(temporary_oobe_account)
    {
        return Err(BuiltInAdministratorValidationError::InvalidTemporaryAccount);
    }

    let temporary_oobe_account = xml_escape(temporary_oobe_account);
    let password = xml_escape(options.password.expose_secret());
    // A real Windows 11 install proved that OOBE can reverse a successful specialize-time RID-500
    // rename and consumes Winlogon's plaintext DefaultPassword during the temporary first logon.
    // Preserve only the authenticated handoff secret as local-only encrypted LSA private data;
    // the helper deletes its plaintext staging file before specialize completes.
    let specialize_command =
        crate::unattend_command::render_required_specialize_run_synchronous_command(
            specialize_order,
            r#""%SystemDrive%\LetRecovery_Scripts\LetRecovery-account-helper.exe" --internal-store-builtin-administrator-secret"#,
            "Protect built-in Administrator transition secret",
        )
        .map_err(|_| BuiltInAdministratorValidationError::InvalidSpecializeCommand)?;
    let user_accounts = format!(
        r#"<UserAccounts>
                <AdministratorPassword>
                    <Value>{password}</Value>
                    <PlainText>true</PlainText>
                </AdministratorPassword>
                <LocalAccounts>
                    <LocalAccount wcm:action="add">
                        <Password><Value>{password}</Value><PlainText>true</PlainText></Password>
                        <Description>Temporary LetRecovery OOBE administrator</Description>
                        <DisplayName>{temporary_oobe_account}</DisplayName>
                        <Group>Administrators</Group>
                        <Name>{temporary_oobe_account}</Name>
                    </LocalAccount>
                </LocalAccounts>
            </UserAccounts>"#
    );
    // Microsoft's documented LogonCount +1 behavior gives this value two automatic logons. The
    // temporary account completes OOBE first; the finalizer switches the remaining logon to the
    // requested RID-500 name. Both accounts receive the same authenticated password, so no secret
    // is placed on a helper command line.
    let auto_logon = format!(
        r#"<AutoLogon>
                <Password>
                    <Value>{password}</Value>
                    <PlainText>true</PlainText>
                </Password>
                <Enabled>true</Enabled>
                <LogonCount>1</LogonCount>
                <Username>{temporary_oobe_account}</Username>
            </AutoLogon>"#
    );

    Ok(Some(BuiltInAdministratorUnattend {
        specialize_command,
        user_accounts,
        auto_logon,
    }))
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
        assert!(options.auto_logon);

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
    fn ordinary_unattended_accounts_reject_windows_owned_identities_without_broad_prefixes() {
        for reserved in [
            "SYSTEM",
            "system",
            "Local Service",
            "NetworkService",
            "TrustedInstaller",
            "Administrator",
            "Guest",
            "DefaultAccount",
            "defaultuser0",
            "WDAGUtilityAccount",
            "WSIAccount",
            "NONE",
            "DWM-1",
            "umfd-0",
        ] {
            assert_eq!(
                validate_unattended_local_account_name(reserved),
                Err(LocalAccountNameValidationError::ReservedAccountName),
                "{reserved}"
            );
        }

        for ordinary in ["Tom", "Terry", "SystemBuilder", "Alice", "张三"] {
            assert_eq!(validate_unattended_local_account_name(ordinary), Ok(()));
            assert!(!is_windows_owned_local_account_name(ordinary));
        }

        assert!(is_windows_owned_local_account_name("defaultuser0"));
        assert!(is_windows_owned_local_account_name("DWM-12"));
        assert!(!is_windows_owned_local_account_name("Administrator"));
        assert!(!is_windows_owned_local_account_name("NONE"));
        assert!(!is_windows_owned_local_account_name("DWM-Admin"));
    }

    #[test]
    fn ordinary_unattended_accounts_follow_windows_name_syntax() {
        assert_eq!(
            validate_unattended_local_account_name(""),
            Err(LocalAccountNameValidationError::MissingAccountName)
        );
        assert_eq!(
            validate_unattended_local_account_name("bad/name"),
            Err(LocalAccountNameValidationError::InvalidAccountName)
        );
        assert_eq!(
            validate_unattended_local_account_name("trailing."),
            Err(LocalAccountNameValidationError::InvalidAccountName)
        );
        assert_eq!(
            validate_unattended_local_account_name("123456789012345678901"),
            Err(LocalAccountNameValidationError::AccountNameTooLong)
        );
    }

    #[test]
    fn default_name_uses_the_temporary_oobe_account_for_two_logons() {
        let temporary = temporary_oobe_account_name("0123456789abcdef0123456789abcdef").unwrap();
        let rendered = render_builtin_administrator_unattend(
            &enabled("Administrator", "A<&B", true),
            2,
            &temporary,
        )
        .unwrap()
        .unwrap();
        assert!(rendered
            .specialize_command
            .contains("--internal-store-builtin-administrator-secret"));
        assert!(rendered
            .specialize_command
            .contains("<WillReboot>OnRequest</WillReboot>"));
        assert!(rendered.user_accounts.contains("<AdministratorPassword>"));
        assert!(rendered
            .user_accounts
            .contains("<Value>A&lt;&amp;B</Value>"));
        assert!(rendered
            .auto_logon
            .contains("<Username>LrOOBE-0123456789ab</Username>"));
        assert!(rendered.auto_logon.contains("<LogonCount>1</LogonCount>"));
        assert!(rendered.user_accounts.contains("<LocalAccounts>"));
        assert!(rendered
            .user_accounts
            .contains("<Name>LrOOBE-0123456789ab</Name>"));
    }

    #[test]
    fn custom_name_is_deferred_until_after_oobe() {
        let temporary = temporary_oobe_account_name("abcdef0123456789abcdef0123456789").unwrap();
        let rendered = render_builtin_administrator_unattend(
            &enabled("O'Brien", "Secret!", false),
            7,
            &temporary,
        )
        .unwrap()
        .unwrap();
        assert!(rendered
            .specialize_command
            .contains("--internal-store-builtin-administrator-secret"));
        assert!(rendered
            .auto_logon
            .contains("<Username>LrOOBE-abcdef012345</Username>"));
        assert!(rendered.auto_logon.contains("<LogonCount>1</LogonCount>"));

        let maximum_name = "x".repeat(MAX_LOCAL_ACCOUNT_NAME_UTF16);
        let maximum = render_builtin_administrator_unattend(
            &enabled(&maximum_name, "Secret!", false),
            500,
            &temporary,
        )
        .unwrap()
        .unwrap();
        assert!(maximum
            .specialize_command
            .contains("--internal-store-builtin-administrator-secret"));
        assert!(maximum
            .auto_logon
            .contains("<Username>LrOOBE-abcdef012345</Username>"));
    }

    #[test]
    fn temporary_oobe_account_is_session_bound_and_cannot_replace_rid_500() {
        assert_eq!(
            temporary_oobe_account_name("0123456789abcdef0123456789abcdef").unwrap(),
            "LrOOBE-0123456789ab"
        );
        assert!(temporary_oobe_account_name("ABC").is_err());
        assert_eq!(
            render_builtin_administrator_unattend(
                &enabled("LrOOBE-0123456789ab", "Secret!", true),
                2,
                "LrOOBE-0123456789ab",
            ),
            Err(BuiltInAdministratorValidationError::InvalidTemporaryAccount)
        );
        assert_eq!(
            render_builtin_administrator_unattend(
                &enabled("Administrator", "Secret!", true),
                2,
                "LrOOBE-not-hex-value",
            ),
            Err(BuiltInAdministratorValidationError::InvalidTemporaryAccount)
        );
    }

    #[test]
    fn protected_administrator_secret_round_trips_canonically() {
        let original = SensitiveString::new("A<&密码!\u{1f512}");
        let bytes = serialize_protected_administrator_secret(&original).unwrap();
        let parsed = parse_protected_administrator_secret(&bytes).unwrap();

        assert_eq!(parsed.as_str(), original.expose_secret());
        assert!(!std::str::from_utf8(&bytes).unwrap().ends_with("\r\n"));
        assert!(parse_protected_administrator_secret(
            format!("{}\r\n", std::str::from_utf8(&bytes).unwrap()).as_bytes()
        )
        .is_err());
    }

    #[test]
    fn protected_administrator_secret_rejects_length_and_header_tampering() {
        assert!(
            parse_protected_administrator_secret(b"LRAS1\r\nUtf8Length=99\r\n\r\nsecret").is_err()
        );
        assert!(
            parse_protected_administrator_secret(b"LRAS2\r\nUtf8Length=6\r\n\r\nsecret").is_err()
        );
        assert!(parse_protected_administrator_secret(b"LRAS1\nUtf8Length=6\n\nsecret").is_err());
    }
}
