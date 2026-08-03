//! Bounded HTTP Range reader for remote WIM/ESD XML metadata.
//!
//! This module never downloads an image payload.  Servers must honor both
//! exact byte ranges; a `200 OK` response is rejected so a metadata probe
//! cannot accidentally transfer a multi-gigabyte image.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use lr_core::download_integrity::validate_download_url;
use lr_core::image_meta::{
    parse_image_info_from_xml, parse_wim_xml_resource, ImageInfo, WimImageType, WIM_HEADER_SIZE,
};
use reqwest::blocking::{Client, Response};
use reqwest::header::{
    ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_RANGE, ETAG, IF_RANGE, LAST_MODIFIED, RANGE,
};
use reqwest::StatusCode;

use crate::download::config::{EasyModeConfig, EasyModeSystem, EasyModeVolume};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug)]
struct EntityValidator {
    total_size: u64,
    if_range: Option<String>,
}

pub fn read_remote_image_info(url: &str, allow_insecure_http: bool) -> Result<Vec<ImageInfo>> {
    let url = validate_download_url(url, allow_insecure_http)
        .map_err(|error| anyhow!("invalid remote image URL: {error}"))?;
    let extension = url
        .as_str()
        .split(['?', '#'])
        .next()
        .and_then(|path| path.rsplit('/').next())
        .and_then(|name| name.rsplit_once('.').map(|(_, extension)| extension))
        .unwrap_or_default();
    if !matches!(extension.to_ascii_lowercase().as_str(), "wim" | "esd") {
        bail!("remote metadata probing supports only WIM or ESD URLs");
    }

    let client = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .context("could not create remote metadata HTTP client")?;
    let (header, validator) =
        fetch_exact_range(&client, url.as_str(), 0, WIM_HEADER_SIZE as u64 - 1, None)?;
    let resource = parse_wim_xml_resource(&header, Some(validator.total_size))
        .map_err(|error| anyhow!("invalid remote WIM header: {error}"))?;
    let end = resource
        .offset
        .checked_add(resource.stored_size - 1)
        .ok_or_else(|| anyhow!("remote WIM XML range overflow"))?;
    let (xml_bytes, second_validator) = fetch_exact_range(
        &client,
        url.as_str(),
        resource.offset,
        end,
        Some(&validator),
    )?;
    if second_validator.total_size != validator.total_size {
        bail!("remote image changed while metadata was being read");
    }
    let xml = lr_core::image_meta::decode_wim_xml(&xml_bytes)
        .map_err(|error| anyhow!("invalid remote WIM XML: {error}"))?;
    let images = parse_image_info_from_xml(&xml);
    if images.is_empty() {
        bail!("remote WIM XML contains no images");
    }
    Ok(images)
}

fn fetch_exact_range(
    client: &Client,
    url: &str,
    start: u64,
    end: u64,
    expected: Option<&EntityValidator>,
) -> Result<(Vec<u8>, EntityValidator)> {
    let mut request = client
        .get(url)
        .header(ACCEPT_ENCODING, "identity")
        .header(RANGE, format!("bytes={start}-{end}"));
    if let Some(value) = expected.and_then(|validator| validator.if_range.as_deref()) {
        request = request.header(IF_RANGE, value);
    }
    let response = request
        .send()
        .context("remote metadata range request failed")?;
    if response.status() != StatusCode::PARTIAL_CONTENT {
        bail!(
            "server did not honor the metadata byte range (HTTP {})",
            response.status()
        );
    }
    if response
        .headers()
        .get(CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| !value.eq_ignore_ascii_case("identity"))
    {
        bail!("server encoded the metadata byte range");
    }
    let validator = response_validator(&response, start, end)?;
    if let Some(expected) = expected {
        if expected.total_size != validator.total_size {
            bail!("remote image size changed between metadata requests");
        }
        if expected.if_range.is_some()
            && validator.if_range.is_some()
            && expected.if_range != validator.if_range
        {
            bail!("remote image validator changed between metadata requests");
        }
    }
    let expected_len = end
        .checked_sub(start)
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| anyhow!("invalid remote metadata range"))?;
    let bytes = response
        .bytes()
        .context("could not read remote metadata response")?;
    if bytes.len() as u64 != expected_len {
        bail!("server returned an incomplete metadata byte range");
    }
    Ok((bytes.to_vec(), validator))
}

fn response_validator(response: &Response, start: u64, end: u64) -> Result<EntityValidator> {
    let content_range = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| anyhow!("partial response has no Content-Range"))?;
    let total_size = parse_content_range(content_range, start, end)?;
    let strong_etag = response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim_start().starts_with("W/"))
        .map(str::to_owned);
    let if_range = strong_etag.or_else(|| {
        response
            .headers()
            .get(LAST_MODIFIED)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    });
    Ok(EntityValidator {
        total_size,
        if_range,
    })
}

fn parse_content_range(value: &str, expected_start: u64, expected_end: u64) -> Result<u64> {
    let (unit, value) = value
        .split_once(' ')
        .ok_or_else(|| anyhow!("invalid Content-Range unit"))?;
    if !unit.eq_ignore_ascii_case("bytes") {
        bail!("invalid Content-Range unit");
    }
    let (range, total) = value
        .split_once('/')
        .ok_or_else(|| anyhow!("invalid Content-Range"))?;
    let (start, end) = range
        .split_once('-')
        .ok_or_else(|| anyhow!("invalid Content-Range interval"))?;
    let start = start
        .parse::<u64>()
        .context("invalid Content-Range start")?;
    let end = end.parse::<u64>().context("invalid Content-Range end")?;
    let total = total.parse::<u64>().context("invalid Content-Range size")?;
    if start != expected_start || end != expected_end || total == 0 || end >= total {
        bail!("Content-Range does not match the requested bytes");
    }
    Ok(total)
}

pub fn resolve_easy_mode_config(
    config: &EasyModeConfig,
    allow_insecure_http: bool,
) -> Result<EasyModeConfig> {
    resolve_easy_mode_config_with(config, allow_insecure_http, read_remote_image_info)
}

fn resolve_easy_mode_config_with<F>(
    config: &EasyModeConfig,
    allow_insecure_http: bool,
    mut read_image_info: F,
) -> Result<EasyModeConfig>
where
    F: FnMut(&str, bool) -> Result<Vec<ImageInfo>>,
{
    let mut resolved = Vec::new();
    for entry in &config.system {
        for (configured_name, system) in entry {
            if !system.volume.is_empty() {
                resolved.push(HashMap::from([(configured_name.clone(), system.clone())]));
                continue;
            }
            let images = read_image_info(&system.os_download, allow_insecure_http)
                .with_context(|| format!("could not read metadata for {configured_name}"))?;
            let mut groups: Vec<(String, Vec<EasyModeVolume>)> = Vec::new();
            for image in images.into_iter().filter(is_remote_installable) {
                let Some(family) = windows_family(&image) else {
                    continue;
                };
                if let Some((_, volumes)) = groups.iter_mut().find(|(name, _)| name == family) {
                    volumes.push(EasyModeVolume {
                        number: image.index,
                        name: image.name,
                        major_version: image.major_version,
                        minor_version: image.minor_version,
                        build: image.build,
                        architecture: image.architecture,
                        installation_type: Some(image.installation_type),
                    });
                } else {
                    groups.push((
                        family.to_owned(),
                        vec![EasyModeVolume {
                            number: image.index,
                            name: image.name,
                            major_version: image.major_version,
                            minor_version: image.minor_version,
                            build: image.build,
                            architecture: image.architecture,
                            installation_type: Some(image.installation_type),
                        }],
                    ));
                }
            }
            if groups.is_empty() {
                bail!("{configured_name} contains no supported installable Windows image");
            }
            for (family, volume) in groups {
                let os_logo = match family.as_str() {
                    "Windows 10" => "LOGO_WINDOWS10".to_owned(),
                    "Windows 11" => "LOGO_WINDOWS11".to_owned(),
                    _ => String::new(),
                };
                resolved.push(HashMap::from([(
                    family,
                    EasyModeSystem {
                        os_logo,
                        os_download: system.os_download.clone(),
                        volume,
                    },
                )]));
            }
        }
    }
    Ok(EasyModeConfig { system: resolved })
}

fn is_remote_installable(image: &ImageInfo) -> bool {
    image.image_type == WimImageType::StandardInstall
        && image.major_version.is_some()
        && matches!(
            image.installation_type.to_ascii_lowercase().as_str(),
            "client" | "server"
        )
}

fn windows_family(image: &ImageInfo) -> Option<&'static str> {
    let name = image.name.to_ascii_lowercase();
    if name.contains("windows 11") {
        return Some("Windows 11");
    }
    if name.contains("windows 10") {
        return Some("Windows 10");
    }
    if name.contains("windows 8.1") {
        return Some("Windows 8.1");
    }
    if name.contains("windows 8") {
        return Some("Windows 8");
    }
    if name.contains("windows 7") {
        return Some("Windows 7");
    }
    if image.installation_type.eq_ignore_ascii_case("server") {
        return Some("Windows Server");
    }
    match (image.major_version, image.minor_version) {
        (Some(6), Some(1)) => Some("Windows 7"),
        (Some(6), Some(2)) => Some("Windows 8"),
        (Some(6), Some(3)) => Some("Windows 8.1"),
        (Some(10), Some(0)) if image.build.is_some_and(|build| build >= 22_000) => {
            Some("Windows 11")
        }
        (Some(10), Some(0)) => Some("Windows 10"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_exact_content_range() {
        assert_eq!(
            parse_content_range("bytes 0-207/5703333631", 0, 207).unwrap(),
            5_703_333_631
        );
        assert!(parse_content_range("bytes 0-206/5703333631", 0, 207).is_err());
        assert!(parse_content_range("bytes */5703333631", 0, 207).is_err());
        assert_eq!(
            parse_content_range("BYTES 0-207/5703333631", 0, 207).unwrap(),
            5_703_333_631
        );
    }

    #[test]
    fn classifies_supported_client_generations_without_build_only_guessing() {
        let image = |name: &str, major, minor, build| ImageInfo {
            index: 1,
            name: name.to_owned(),
            size_bytes: 1,
            installation_type: "Client".to_owned(),
            description: String::new(),
            major_version: major,
            minor_version: minor,
            build,
            architecture: Some(9),
            image_type: WimImageType::StandardInstall,
            verified_installable: false,
        };
        assert_eq!(
            windows_family(&image("Professional", Some(6), Some(1), Some(7601))),
            Some("Windows 7")
        );
        assert_eq!(
            windows_family(&image("Professional", Some(6), Some(3), Some(9600))),
            Some("Windows 8.1")
        );
        assert_eq!(
            windows_family(&image("Professional", Some(10), Some(0), Some(19045))),
            Some("Windows 10")
        );
        assert_eq!(
            windows_family(&image("Professional", Some(10), Some(0), Some(26100))),
            Some("Windows 11")
        );
    }

    #[test]
    fn url_only_catalogue_uses_detected_windows_family_and_filters_pe() {
        let config = EasyModeConfig {
            system: vec![HashMap::from([(
                "Windows 10/11".to_owned(),
                EasyModeSystem {
                    os_logo: "LOGO_WINDOWS11".to_owned(),
                    os_download: "https://example.com/install.wim".to_owned(),
                    volume: Vec::new(),
                },
            )])],
        };
        let image = |index, name: &str, installation_type: &str, image_type| ImageInfo {
            index,
            name: name.to_owned(),
            size_bytes: 1,
            installation_type: installation_type.to_owned(),
            description: String::new(),
            major_version: Some(6),
            minor_version: Some(1),
            build: Some(7601),
            architecture: Some(9),
            image_type,
            verified_installable: false,
        };
        let resolved = resolve_easy_mode_config_with(&config, false, |_, _| {
            Ok(vec![
                image(1, "Windows PE", "WindowsPE", WimImageType::WindowsPE),
                image(
                    2,
                    "Windows 7 Professional",
                    "Client",
                    WimImageType::StandardInstall,
                ),
            ])
        })
        .unwrap();
        let systems = resolved.get_systems();
        assert_eq!(systems.len(), 1);
        assert_eq!(systems[0].0, "Windows 7");
        assert_eq!(systems[0].1.volume.len(), 1);
        assert_eq!(systems[0].1.volume[0].number, 2);
    }

    #[test]
    #[ignore = "requires an explicitly supplied remote image URL"]
    fn probes_remote_image_from_environment() {
        let url = std::env::var("LETRECOVERY_REMOTE_WIM_TEST_URL").unwrap();
        let images = read_remote_image_info(&url, true).unwrap();
        assert!(images.iter().any(is_remote_installable));
    }
}
