//! Read-only discovery and ephemeral capture of the currently connected Wi-Fi profile.
//!
//! The exported XML contains a clear-text key. It is returned only in memory and the temporary
//! export directory is removed by a guard on every exit path.

use anyhow::bail;
#[cfg(not(feature = "non-elevated-tests"))]
use anyhow::{anyhow, Context};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedWifiProfile {
    pub ssid: String,
    pub xml: String,
}

#[cfg(not(feature = "non-elevated-tests"))]
fn netsh_output(args: &[&str]) -> anyhow::Result<std::process::Output> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("netsh")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .context("failed to start netsh")
}

fn decode_console(bytes: &[u8]) -> String {
    match String::from_utf8(bytes.to_vec()) {
        Ok(value)
            if value
                .chars()
                .filter(|character| *character == '\u{fffd}')
                .count()
                < 3 =>
        {
            value
        }
        _ => encoding_rs::GBK.decode(bytes).0.into_owned(),
    }
}

fn connected_ssid(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let line = line.trim();
        if line.starts_with("BSSID") {
            return None;
        }
        let rest = line.strip_prefix("SSID")?;
        let (_, value) = rest.split_once(':')?;
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

#[cfg(feature = "non-elevated-tests")]
pub fn connected_wifi_available() -> anyhow::Result<bool> {
    Ok(false)
}

#[cfg(not(feature = "non-elevated-tests"))]
pub fn connected_wifi_available() -> anyhow::Result<bool> {
    let output = netsh_output(&["wlan", "show", "interfaces"])?;
    if !output.status.success() {
        bail!("netsh wlan show interfaces returned {}", output.status);
    }
    Ok(connected_ssid(&decode_console(&output.stdout)).is_some())
}

struct TempProfileDirectory(std::path::PathBuf);

impl Drop for TempProfileDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(not(feature = "non-elevated-tests"))]
fn temp_profile_directory() -> anyhow::Result<TempProfileDirectory> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    for _ in 0..32 {
        let suffix = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "LetRecovery-wifi-{}-{}-{suffix}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(TempProfileDirectory(path)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("failed to create Wi-Fi export directory"),
        }
    }
    bail!("failed to allocate a unique Wi-Fi export directory")
}

#[cfg(feature = "non-elevated-tests")]
pub fn capture_connected_wifi() -> anyhow::Result<CapturedWifiProfile> {
    bail!("Wi-Fi capture is disabled in the development build")
}

#[cfg(not(feature = "non-elevated-tests"))]
pub fn capture_connected_wifi() -> anyhow::Result<CapturedWifiProfile> {
    let interfaces = netsh_output(&["wlan", "show", "interfaces"])?;
    if !interfaces.status.success() {
        bail!("netsh wlan show interfaces returned {}", interfaces.status);
    }
    let ssid = connected_ssid(&decode_console(&interfaces.stdout))
        .ok_or_else(|| anyhow!("no connected Wi-Fi profile was found"))?;
    let directory = temp_profile_directory()?;
    let folder = directory.0.to_string_lossy().into_owned();
    let profile = format!("name={ssid}");
    let folder_argument = format!("folder={folder}");
    let exported = netsh_output(&[
        "wlan",
        "export",
        "profile",
        &profile,
        "key=clear",
        &folder_argument,
    ])?;
    if !exported.status.success() {
        bail!("netsh wlan export profile returned {}", exported.status);
    }
    let xml_path = std::fs::read_dir(&directory.0)
        .context("failed to read Wi-Fi export directory")?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("xml"))
        })
        .ok_or_else(|| anyhow!("netsh did not produce a Wi-Fi profile XML"))?;
    let xml = std::fs::read_to_string(xml_path).context("failed to read Wi-Fi profile XML")?;
    Ok(CapturedWifiProfile { ssid, xml })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ssid_without_confusing_bssid() {
        let text = "    BSSID : aa:bb:cc\r\n    SSID : Test Network\r\n";
        assert_eq!(connected_ssid(text).as_deref(), Some("Test Network"));
    }

    #[test]
    fn missing_ssid_is_not_connected() {
        assert_eq!(connected_ssid("State : disconnected"), None);
    }
}
