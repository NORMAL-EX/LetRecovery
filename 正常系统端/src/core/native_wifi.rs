//! Read-only discovery and in-memory capture of the currently connected Wi-Fi profile.
//!
//! The profile XML can contain a clear-text key. It is never written to a
//! temporary file and is returned only to the existing installation-session
//! configuration boundary.

#[cfg(feature = "non-elevated-tests")]
use anyhow::bail;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedWifiProfile {
    pub ssid: String,
    pub xml: String,
}

#[cfg(not(feature = "non-elevated-tests"))]
mod native {
    use std::ffi::c_void;
    use std::slice;

    use anyhow::{bail, Context};
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
    use windows::Win32::NetworkManagement::WiFi::{
        wlan_interface_state_connected, wlan_intf_opcode_current_connection, WlanCloseHandle,
        WlanEnumInterfaces, WlanFreeMemory, WlanGetProfile, WlanOpenHandle, WlanQueryInterface,
        WLAN_API_VERSION_2_0, WLAN_CONNECTION_ATTRIBUTES, WLAN_INTERFACE_INFO,
        WLAN_INTERFACE_INFO_LIST, WLAN_PROFILE_GET_PLAINTEXT_KEY,
    };

    use super::CapturedWifiProfile;

    struct WlanClient(HANDLE);

    impl Drop for WlanClient {
        fn drop(&mut self) {
            unsafe {
                let _ = WlanCloseHandle(self.0, None);
            }
        }
    }

    struct WlanMemory(*mut c_void);

    impl Drop for WlanMemory {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    WlanFreeMemory(self.0);
                }
            }
        }
    }

    fn status_error(operation: &str, status: u32) -> anyhow::Error {
        anyhow::anyhow!(
            "{operation} failed with Win32 error {status}: {}",
            std::io::Error::from_raw_os_error(status as i32)
        )
    }

    fn open_client() -> anyhow::Result<WlanClient> {
        let mut negotiated_version = 0_u32;
        let mut handle = HANDLE::default();
        let status = unsafe {
            WlanOpenHandle(
                WLAN_API_VERSION_2_0,
                None,
                &mut negotiated_version,
                &mut handle,
            )
        };
        if status != ERROR_SUCCESS.0 {
            return Err(status_error("WlanOpenHandle", status));
        }
        if handle.is_invalid() {
            bail!("WlanOpenHandle returned an invalid handle");
        }
        Ok(WlanClient(handle))
    }

    fn connected_interfaces(client: &WlanClient) -> anyhow::Result<Vec<WLAN_INTERFACE_INFO>> {
        let mut raw = std::ptr::null_mut::<WLAN_INTERFACE_INFO_LIST>();
        let status = unsafe { WlanEnumInterfaces(client.0, None, &mut raw) };
        if status != ERROR_SUCCESS.0 {
            return Err(status_error("WlanEnumInterfaces", status));
        }
        let memory = WlanMemory(raw.cast::<c_void>());
        if memory.0.is_null() {
            bail!("WlanEnumInterfaces returned a null list");
        }
        let list = unsafe { &*raw };
        let count = list.dwNumberOfItems as usize;
        let interfaces = unsafe { slice::from_raw_parts(list.InterfaceInfo.as_ptr(), count) };
        Ok(interfaces
            .iter()
            .copied()
            .filter(|interface| interface.isState == wlan_interface_state_connected)
            .collect())
    }

    unsafe fn connection_attributes(
        client: &WlanClient,
        interface: &WLAN_INTERFACE_INFO,
    ) -> anyhow::Result<(WlanMemory, WLAN_CONNECTION_ATTRIBUTES)> {
        let mut size = 0_u32;
        let mut raw = std::ptr::null_mut::<c_void>();
        let status = WlanQueryInterface(
            client.0,
            &interface.InterfaceGuid,
            wlan_intf_opcode_current_connection,
            None,
            &mut size,
            &mut raw,
            None,
        );
        if status != ERROR_SUCCESS.0 {
            return Err(status_error(
                "WlanQueryInterface(current connection)",
                status,
            ));
        }
        let memory = WlanMemory(raw);
        if memory.0.is_null() || size < std::mem::size_of::<WLAN_CONNECTION_ATTRIBUTES>() as u32 {
            bail!("WlanQueryInterface returned invalid connection attributes");
        }
        Ok((memory, *raw.cast::<WLAN_CONNECTION_ATTRIBUTES>()))
    }

    fn utf16_array(array: &[u16]) -> anyhow::Result<String> {
        let length = array
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(array.len());
        String::from_utf16(&array[..length]).context("Wi-Fi profile name is invalid UTF-16")
    }

    pub fn connected_wifi_available() -> anyhow::Result<bool> {
        let client = open_client()?;
        Ok(!connected_interfaces(&client)?.is_empty())
    }

    pub fn capture_connected_wifi() -> anyhow::Result<CapturedWifiProfile> {
        let client = open_client()?;
        let interfaces = connected_interfaces(&client)?;
        if interfaces.len() != 1 {
            bail!(
                "expected exactly one connected Wi-Fi interface, found {}",
                interfaces.len()
            );
        }
        let interface = &interfaces[0];
        let (_attributes_memory, attributes) =
            unsafe { connection_attributes(&client, interface)? };
        let profile_name = utf16_array(&attributes.strProfileName)?;
        if profile_name.is_empty() {
            bail!("connected Wi-Fi has no saved profile name");
        }
        let ssid_length = attributes
            .wlanAssociationAttributes
            .dot11Ssid
            .uSSIDLength
            .min(32) as usize;
        let ssid = String::from_utf8_lossy(
            &attributes.wlanAssociationAttributes.dot11Ssid.ucSSID[..ssid_length],
        )
        .into_owned();

        let profile_name_wide: Vec<u16> = profile_name.encode_utf16().chain(Some(0)).collect();
        let mut xml_pointer = PWSTR::null();
        let mut flags = WLAN_PROFILE_GET_PLAINTEXT_KEY;
        let mut granted_access = 0_u32;
        let status = unsafe {
            WlanGetProfile(
                client.0,
                &interface.InterfaceGuid,
                PCWSTR(profile_name_wide.as_ptr()),
                None,
                &mut xml_pointer,
                Some(&mut flags),
                Some(&mut granted_access),
            )
        };
        if status != ERROR_SUCCESS.0 {
            return Err(status_error(
                "WlanGetProfile(plaintext key requested)",
                status,
            ));
        }
        let xml_memory = WlanMemory(xml_pointer.0.cast::<c_void>());
        if xml_memory.0.is_null() {
            bail!("WlanGetProfile returned null profile XML");
        }
        let xml =
            unsafe { xml_pointer.to_string() }.context("Wi-Fi profile XML is invalid UTF-16")?;
        if xml.is_empty() {
            bail!("WlanGetProfile returned empty profile XML");
        }
        super::validate_portable_profile_xml(&xml)?;
        Ok(CapturedWifiProfile { ssid, xml })
    }
}

fn validate_portable_profile_xml(xml: &str) -> anyhow::Result<()> {
    let document = roxmltree::Document::parse(xml)?;
    let authentication = document
        .descendants()
        .find(|node| node.tag_name().name() == "authentication")
        .and_then(|node| node.text())
        .unwrap_or("")
        .trim()
        .to_ascii_uppercase();
    let shared_key = document
        .descendants()
        .find(|node| node.tag_name().name() == "sharedKey");
    if let Some(shared_key) = shared_key {
        let protected = shared_key
            .descendants()
            .find(|node| node.tag_name().name() == "protected")
            .and_then(|node| node.text())
            .unwrap_or("")
            .trim();
        let key_material = shared_key
            .descendants()
            .find(|node| node.tag_name().name() == "keyMaterial")
            .and_then(|node| node.text())
            .unwrap_or("")
            .trim();
        if !protected.eq_ignore_ascii_case("false") || key_material.is_empty() {
            anyhow::bail!("Wi-Fi profile key material was not returned in portable plaintext form");
        }
    } else if !matches!(authentication.as_str(), "OPEN" | "OWE") {
        anyhow::bail!(
            "Wi-Fi profile uses credentials that are not embedded in the portable profile XML"
        );
    }
    Ok(())
}

#[cfg(feature = "non-elevated-tests")]
pub fn connected_wifi_available() -> anyhow::Result<bool> {
    Ok(false)
}

#[cfg(not(feature = "non-elevated-tests"))]
pub use native::connected_wifi_available;

#[cfg(feature = "non-elevated-tests")]
pub fn capture_connected_wifi() -> anyhow::Result<CapturedWifiProfile> {
    bail!("Wi-Fi capture is disabled in the development build")
}

#[cfg(not(feature = "non-elevated-tests"))]
pub use native::capture_connected_wifi;

#[cfg(test)]
mod tests {
    use super::{validate_portable_profile_xml, CapturedWifiProfile};

    #[test]
    fn captured_profile_keeps_ssid_and_xml_separate() {
        let profile = CapturedWifiProfile {
            ssid: "Test Network".to_owned(),
            xml: "<WLANProfile />".to_owned(),
        };
        assert_eq!(profile.ssid, "Test Network");
        assert_eq!(profile.xml, "<WLANProfile />");
    }

    #[test]
    fn portable_profile_requires_plaintext_key_or_an_open_authentication_mode() {
        let personal = r#"<WLANProfile><MSM><security><authEncryption><authentication>WPA2PSK</authentication></authEncryption><sharedKey><protected>false</protected><keyMaterial>secret</keyMaterial></sharedKey></security></MSM></WLANProfile>"#;
        assert!(validate_portable_profile_xml(personal).is_ok());
        let encrypted = personal.replace("<protected>false", "<protected>true");
        assert!(validate_portable_profile_xml(&encrypted).is_err());
        let enterprise = r#"<WLANProfile><MSM><security><authEncryption><authentication>WPA2</authentication></authEncryption></security></MSM></WLANProfile>"#;
        assert!(validate_portable_profile_xml(enterprise).is_err());
        let open = r#"<WLANProfile><MSM><security><authEncryption><authentication>open</authentication></authEncryption></security></MSM></WLANProfile>"#;
        assert!(validate_portable_profile_xml(open).is_ok());
    }
}
