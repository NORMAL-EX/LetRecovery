//! Pure compatibility helpers extracted from the legacy direct-install UI.
//!
//! This module constructs unattended XML and typed mutation plans.  It never
//! starts DiskPart/format, writes a disk signature, changes an active flag,
//! injects a driver, or writes into an offline Windows directory.  Production
//! execution remains behind the native install backend; development tests can
//! therefore exercise every branch without touching the host.

use std::path::{Path, PathBuf};

use lr_core::command::CommandRequest;
use lr_core::format_command::{system_format_executable, FormatCommandError, FormatCommandSpec};
use lr_core::offline_international::OfflineInternationalSettings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowsFamily {
    Xp,
    Windows7,
    Windows8,
    Windows10,
    Windows11,
    Unsupported,
}

impl WindowsFamily {
    pub const fn driver_directory(self) -> Option<&'static str> {
        match self {
            Self::Windows7 => Some("win7"),
            Self::Windows8 => Some("win8"),
            Self::Windows10 => Some("win10"),
            Self::Windows11 => Some("win11"),
            // XP uses the dedicated AHCI/NVMe/USB3 integration path.
            Self::Xp | Self::Unsupported => None,
        }
    }
}

pub const fn classify_windows_version(major: u16, minor: u16, build: u16) -> WindowsFamily {
    match (major, minor) {
        (5, _) => WindowsFamily::Xp,
        (6, 1) => WindowsFamily::Windows7,
        (6, 2 | 3) => WindowsFamily::Windows8,
        (10, _) if build >= 22_000 => WindowsFamily::Windows11,
        (10, _) => WindowsFamily::Windows10,
        _ => WindowsFamily::Unsupported,
    }
}

/// Resolves `bin/drivers/<family>` without checking or creating the directory.
pub fn user_driver_source(root: &Path, family: WindowsFamily) -> Option<PathBuf> {
    family.driver_directory().map(|name| root.join(name))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnattendArchitecture {
    X86,
    Amd64,
}

impl UnattendArchitecture {
    const fn as_str(self) -> &'static str {
        match self {
            Self::X86 => "x86",
            Self::Amd64 => "amd64",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultUnattendOptions<'a> {
    pub architecture: UnattendArchitecture,
    pub family: WindowsFamily,
    pub username: Option<&'a str>,
    pub remove_uwp_apps: bool,
    pub international: Option<&'a OfflineInternationalSettings>,
}

/// Generates the same default answer file used by the old direct workflow.
///
/// The caller writes the returned text to Panther/Sysprep only after image
/// application succeeds. User text is XML-escaped before interpolation.
pub fn render_default_unattend(
    options: &DefaultUnattendOptions<'_>,
) -> Result<String, &'static str> {
    let username = xml_escape(
        options
            .username
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("User"),
    );
    let mut first_logon_commands = String::from(
        r#"
                <SynchronousCommand wcm:action="add">
                    <Order>1</Order>
                    <CommandLine>cmd /c if exist %SystemDrive%\LetRecovery_Scripts\firstlogon.bat call %SystemDrive%\LetRecovery_Scripts\firstlogon.bat</CommandLine>
                    <Description>Run first login script</Description>
                </SynchronousCommand>"#,
    );
    let mut order = 2;
    if options.remove_uwp_apps
        && matches!(
            options.family,
            WindowsFamily::Windows10 | WindowsFamily::Windows11
        )
    {
        first_logon_commands.push_str(&format!(
            r#"
                <SynchronousCommand wcm:action="add">
                    <Order>{order}</Order>
                    <CommandLine>powershell -ExecutionPolicy Bypass -File %SystemDrive%\LetRecovery_Scripts\remove_uwp.ps1</CommandLine>
                    <Description>Remove preinstalled UWP apps</Description>
                </SynchronousCommand>"#
        ));
        order += 1;
    }
    first_logon_commands.push_str(&format!(
        r#"
                <SynchronousCommand wcm:action="add">
                    <Order>{order}</Order>
                    <CommandLine>cmd /c rd /s /q %SystemDrive%\LetRecovery_Scripts</CommandLine>
                    <Description>Cleanup scripts directory</Description>
                </SynchronousCommand>"#
    ));

    let oobe = match options.family {
        WindowsFamily::Windows7 => {
            r#"<OOBE>
                <HideEULAPage>true</HideEULAPage>
                <ProtectYourPC>3</ProtectYourPC>
                <NetworkLocation>Home</NetworkLocation>
            </OOBE>"#
        }
        WindowsFamily::Windows8 => {
            r#"<OOBE>
                <HideEULAPage>true</HideEULAPage>
                <HideLocalAccountScreen>true</HideLocalAccountScreen>
                <ProtectYourPC>3</ProtectYourPC>
                <NetworkLocation>Home</NetworkLocation>
            </OOBE>"#
        }
        _ => {
            r#"<OOBE>
                <HideEULAPage>true</HideEULAPage>
                <HideOnlineAccountScreens>true</HideOnlineAccountScreens>
                <HideWirelessSetupInOOBE>true</HideWirelessSetupInOOBE>
                <ProtectYourPC>3</ProtectYourPC>
            </OOBE>"#
        }
    };
    let architecture = options.architecture.as_str();

    let (international_component, time_zone) = if matches!(
        options.family,
        WindowsFamily::Windows10 | WindowsFamily::Windows11
    ) {
        let international = options
            .international
            .ok_or("Windows 10/11 default unattend requires offline international settings")?;
        let input_locale = xml_escape(&international.input_locale);
        let system_locale = xml_escape(&international.system_locale);
        let ui_language = xml_escape(&international.ui_language);
        let user_locale = xml_escape(&international.user_locale);
        let time_zone = xml_escape(&international.time_zone);
        (
            format!(
                r#"        <component name="Microsoft-Windows-International-Core" processorArchitecture="{architecture}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
            <InputLocale>{input_locale}</InputLocale>
            <SystemLocale>{system_locale}</SystemLocale>
            <UILanguage>{ui_language}</UILanguage>
            <UserLocale>{user_locale}</UserLocale>
        </component>
"#
            ),
            format!("            <TimeZone>{time_zone}</TimeZone>\n"),
        )
    } else {
        (String::new(), String::new())
    };

    Ok(format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
    <settings pass="windowsPE">
        <component name="Microsoft-Windows-Setup" processorArchitecture="{architecture}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
            <UserData><ProductKey><WillShowUI>OnError</WillShowUI></ProductKey><AcceptEula>true</AcceptEula></UserData>
        </component>
    </settings>
    <settings pass="specialize">
        <component name="Microsoft-Windows-Shell-Setup" processorArchitecture="{architecture}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS"><ComputerName>*</ComputerName></component>
        <component name="Microsoft-Windows-Deployment" processorArchitecture="{architecture}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
            <RunSynchronous><RunSynchronousCommand wcm:action="add"><Order>1</Order><Path>cmd /c if exist %SystemDrive%\LetRecovery_Scripts\deploy.bat call %SystemDrive%\LetRecovery_Scripts\deploy.bat</Path><Description>Run custom deploy script</Description></RunSynchronousCommand></RunSynchronous>
        </component>
    </settings>
    <settings pass="oobeSystem">
{international_component}
        <component name="Microsoft-Windows-Shell-Setup" processorArchitecture="{architecture}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
{time_zone}
            {oobe}
            <UserAccounts><LocalAccounts><LocalAccount wcm:action="add"><Password><Value></Value><PlainText>true</PlainText></Password><Description>Local User</Description><DisplayName>{username}</DisplayName><Group>Administrators</Group><Name>{username}</Name></LocalAccount></LocalAccounts></UserAccounts>
            <AutoLogon><Password><Value></Value><PlainText>true</PlainText></Password><Enabled>true</Enabled><LogonCount>1</LogonCount><Username>{username}</Username></AutoLogon>
            <FirstLogonCommands>{first_logon_commands}
            </FirstLogonCommands>
        </component>
    </settings>
</unattend>"#
    ))
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MbrSignatureObservation {
    NonZero(String),
    Zero,
    NotMbrOrUnparseable,
}

/// Parses only an eight-hex ID on a line containing `ID`, preserving the old
/// conservative rule that a GUID/GPT or unfamiliar localized result is skipped.
pub fn parse_mbr_signature(output: &str) -> MbrSignatureObservation {
    for line in output.lines() {
        if !line.to_ascii_lowercase().contains("id") {
            continue;
        }
        // GPT reports a GUID. Never mistake its leading eight-hex group for
        // an MBR signature and issue `uniqueid disk id=<mbr-id>` against it.
        if line.contains('{') || line.contains('}') {
            continue;
        }
        if let Some(token) = line
            .split(|character: char| !character.is_ascii_alphanumeric())
            .find(|token| token.len() == 8 && token.chars().all(|c| c.is_ascii_hexdigit()))
        {
            let signature = token.to_ascii_uppercase();
            return if signature == "00000000" {
                MbrSignatureObservation::Zero
            } else {
                MbrSignatureObservation::NonZero(signature)
            };
        }
    }
    MbrSignatureObservation::NotMbrOrUnparseable
}

pub fn mbr_signature_read_script(disk_number: u32) -> String {
    format!("select disk {disk_number}\r\nuniqueid disk\r\nexit\r\n")
}

/// Builds a non-zero replacement ID from injected entropy for deterministic tests.
pub const fn replacement_mbr_signature(entropy: u32) -> u32 {
    entropy | 0x1000_0000
}

pub fn mbr_signature_write_script(disk_number: u32, signature: u32) -> Option<String> {
    (signature != 0).then(|| {
        format!("select disk {disk_number}\r\nuniqueid disk id={signature:08X}\r\nexit\r\n")
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartitionIdentity<'a> {
    pub letter: &'a str,
    pub disk_number: Option<u32>,
}

/// Creates best-effort `inactive` scripts for every other lettered partition
/// on the target MBR disk. The caller logs individual failures and continues.
pub fn sibling_inactive_scripts(
    target_letter: &str,
    partitions: &[PartitionIdentity<'_>],
) -> Vec<(String, String)> {
    let target = normalize_letter(target_letter);
    let Some(target_disk) = partitions
        .iter()
        .find(|partition| normalize_letter(partition.letter).eq_ignore_ascii_case(&target))
        .and_then(|partition| partition.disk_number)
    else {
        return Vec::new();
    };
    partitions
        .iter()
        .filter_map(|partition| {
            let letter = normalize_letter(partition.letter);
            (partition.disk_number == Some(target_disk)
                && !letter.is_empty()
                && !letter.eq_ignore_ascii_case(&target))
            .then(|| {
                (
                    letter.clone(),
                    format!("select volume {letter}\r\ninactive\r\nexit\r\n"),
                )
            })
        })
        .collect()
}

fn normalize_letter(value: &str) -> String {
    value
        .trim()
        .trim_end_matches(['\\', '/', ':'])
        .to_ascii_uppercase()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatCompatibilityPlan {
    pub drive: String,
    pub diskpart_script: String,
    pub fallback: CommandRequest,
}

/// Validates the drive/label once, then builds the DiskPart primary attempt and
/// direct `format.com` fallback with arguments kept separate.
pub fn build_format_plan(
    drive: &str,
    volume_label: Option<&str>,
) -> Result<FormatCompatibilityPlan, FormatCommandError> {
    let label = volume_label.filter(|label| !label.trim().is_empty());
    let primary = FormatCommandSpec::new(drive, "NTFS", label)?;
    let letter = primary.drive().trim_end_matches(':');
    let diskpart_format = match primary.volume_label() {
        Some(label) => format!("format fs=ntfs label=\"{label}\" quick override"),
        None => "format fs=ntfs quick override".to_string(),
    };
    let diskpart_script = format!("select volume {letter}\r\n{diskpart_format}\r\nexit\r\n");

    // The legacy fallback always supplies a label and forces dismount.
    let fallback_label = label.unwrap_or("本地磁盘");
    let fallback_spec = FormatCommandSpec::new(primary.drive(), "NTFS", Some(fallback_label))?
        .with_force_dismount(true);
    Ok(FormatCompatibilityPlan {
        drive: primary.drive().to_string(),
        diskpart_script,
        fallback: fallback_spec.command_request(system_format_executable()),
    })
}

pub fn diskpart_format_succeeded(stdout: &str) -> bool {
    let lower = stdout.to_ascii_lowercase();
    stdout.contains("成功格式化") || lower.contains("successfully formatted")
}

/// Evaluates the fallback with the shared localized output policy.
pub fn fallback_format_succeeded(exit_succeeded: bool, stdout: &str, stderr: &str) -> bool {
    lr_core::format_command::output_indicates_success(stdout)
        && !lr_core::format_command::output_indicates_error(exit_succeeded, stdout, stderr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn version_family_preserves_driver_matrix() {
        assert_eq!(classify_windows_version(5, 1, 2600), WindowsFamily::Xp);
        assert_eq!(
            classify_windows_version(6, 1, 7601),
            WindowsFamily::Windows7
        );
        assert_eq!(
            classify_windows_version(6, 3, 9600),
            WindowsFamily::Windows8
        );
        assert_eq!(
            classify_windows_version(10, 0, 19045),
            WindowsFamily::Windows10
        );
        assert_eq!(
            classify_windows_version(10, 0, 22621),
            WindowsFamily::Windows11
        );
        assert_eq!(
            user_driver_source(Path::new("bin/drivers"), WindowsFamily::Windows11),
            Some(PathBuf::from("bin/drivers/win11"))
        );
        assert_eq!(
            user_driver_source(Path::new("bin/drivers"), WindowsFamily::Xp),
            None
        );
    }

    #[test]
    fn unattend_varies_oobe_and_escapes_username() {
        let win7 = render_default_unattend(&DefaultUnattendOptions {
            architecture: UnattendArchitecture::Amd64,
            family: WindowsFamily::Windows7,
            username: Some("A&B<User>"),
            remove_uwp_apps: true,
            international: None,
        })
        .unwrap();
        assert!(win7.contains("processorArchitecture=\"amd64\""));
        assert!(win7.contains("A&amp;B&lt;User&gt;"));
        assert!(!win7.contains("HideOnlineAccountScreens"));
        assert!(!win7.contains("remove_uwp.ps1"));

        let international = OfflineInternationalSettings {
            ui_language: "zh-CN".to_string(),
            system_locale: "zh-CN".to_string(),
            user_locale: "zh-CN".to_string(),
            input_locale: "0804:00000804".to_string(),
            time_zone: "China Standard Time".to_string(),
        };
        let win11 = render_default_unattend(&DefaultUnattendOptions {
            architecture: UnattendArchitecture::X86,
            family: WindowsFamily::Windows11,
            username: None,
            remove_uwp_apps: true,
            international: Some(&international),
        })
        .unwrap();
        assert!(win11.contains("HideOnlineAccountScreens"));
        assert!(win11.contains("remove_uwp.ps1"));
        assert!(win11.contains("<Order>3</Order>"));
        assert!(win11.contains("<UILanguage>zh-CN</UILanguage>"));
        assert!(win11.contains("<InputLocale>0804:00000804</InputLocale>"));
        assert!(win11.contains("<TimeZone>China Standard Time</TimeZone>"));
        assert!(!win11.contains("HideLocalAccountScreen"));
    }

    #[test]
    fn windows_11_unattend_rejects_missing_international_settings() {
        let error = render_default_unattend(&DefaultUnattendOptions {
            architecture: UnattendArchitecture::Amd64,
            family: WindowsFamily::Windows11,
            username: None,
            remove_uwp_apps: false,
            international: None,
        })
        .unwrap_err();
        assert!(error.contains("requires offline international settings"));
    }

    #[test]
    fn mbr_signature_is_changed_only_when_exactly_zero() {
        assert_eq!(
            parse_mbr_signature("Disk ID: 00000000"),
            MbrSignatureObservation::Zero
        );
        assert_eq!(
            parse_mbr_signature("磁盘 ID: A1b2c3d4"),
            MbrSignatureObservation::NonZero("A1B2C3D4".to_string())
        );
        assert_eq!(
            parse_mbr_signature("Disk ID: {01234567-89AB-CDEF}"),
            MbrSignatureObservation::NotMbrOrUnparseable
        );
        assert_eq!(replacement_mbr_signature(0), 0x1000_0000);
        assert!(mbr_signature_write_script(2, 0).is_none());
        assert!(mbr_signature_write_script(2, 0x1234_5678)
            .unwrap()
            .contains("id=12345678"));
    }

    #[test]
    fn active_cleanup_is_limited_to_siblings_on_target_disk() {
        let partitions = [
            PartitionIdentity {
                letter: "C:",
                disk_number: Some(0),
            },
            PartitionIdentity {
                letter: "W:\\",
                disk_number: Some(0),
            },
            PartitionIdentity {
                letter: "D:",
                disk_number: Some(1),
            },
        ];
        let scripts = sibling_inactive_scripts("W:", &partitions);
        assert_eq!(scripts.len(), 1);
        assert_eq!(scripts[0].0, "C");
        assert!(scripts[0].1.contains("select volume C\r\ninactive"));
    }

    #[test]
    fn format_plan_preserves_diskpart_then_typed_fallback() {
        let plan = build_format_plan("e:\\", Some("Windows 11")).unwrap();
        assert_eq!(plan.drive, "E:");
        assert!(plan
            .diskpart_script
            .contains("format fs=ntfs label=\"Windows 11\" quick override"));
        assert!(plan.fallback.program() == system_format_executable().as_os_str());
        assert!(plan
            .fallback
            .arguments()
            .iter()
            .any(|argument| argument == OsStr::new("/V:Windows 11")));
        assert!(plan
            .fallback
            .arguments()
            .iter()
            .any(|argument| argument == OsStr::new("/X")));
    }

    #[test]
    fn format_success_requires_explicit_completion() {
        assert!(diskpart_format_succeeded(
            "DiskPart successfully formatted the volume."
        ));
        assert!(!diskpart_format_succeeded(" 50 percent completed"));
        assert!(fallback_format_succeeded(true, "Format complete.", ""));
        assert!(!fallback_format_succeeded(
            false,
            "Format complete.",
            "I/O error"
        ));
    }
}
