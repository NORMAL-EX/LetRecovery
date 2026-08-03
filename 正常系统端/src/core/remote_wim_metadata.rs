//! Bounded HTTP Range reader for remote WIM/ESD XML metadata.
//!
//! Direct WIM/ESD URLs are read at offset zero. For Windows installation ISO
//! URLs, the ISO 9660/Joliet directory records are read first so the embedded
//! `sources/install.esd` or `sources/install.wim` byte extent can be addressed
//! without downloading the ISO. This module never downloads an image payload.
//! Servers must honor every exact byte range; a `200 OK` response is rejected
//! so a metadata probe cannot accidentally transfer a multi-gigabyte image.

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
const ISO_SECTOR_SIZE: u64 = 2_048;
const ISO_DESCRIPTOR_START_SECTOR: u64 = 16;
const MAX_ISO_DESCRIPTOR_SECTORS: u64 = 64;
const MAX_REMOTE_ISO_DIRECTORY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_REMOTE_WIM_XML_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug)]
struct EntityValidator {
    total_size: u64,
    if_range: Option<String>,
    resolved_url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IsoNameEncoding {
    Ascii,
    Joliet,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IsoEntry {
    name: String,
    offset: u64,
    length: u64,
    is_directory: bool,
    is_multi_extent: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IsoRoot {
    entry: IsoEntry,
    encoding: IsoNameEncoding,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IsoFile {
    name: String,
    extents: Vec<IsoEntry>,
    length: u64,
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
    let extension = extension.to_ascii_lowercase();
    if !matches!(extension.as_str(), "wim" | "esd" | "iso") {
        bail!("remote metadata probing supports only WIM, ESD, or ISO URLs");
    }

    let redirect_policy = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= 5 {
            return attempt.error("remote image exceeded the five-redirect limit");
        }
        if attempt
            .previous()
            .iter()
            .any(|previous| previous == attempt.url())
        {
            return attempt.error("remote image redirect loop detected");
        }
        if !redirect_target_is_allowed(attempt.url(), allow_insecure_http) {
            return attempt.error("remote image redirected to a disallowed URL");
        }
        attempt.follow()
    });
    let client = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .redirect(redirect_policy)
        .build()
        .context("could not create remote metadata HTTP client")?;
    if extension == "iso" {
        let descriptor_start = ISO_DESCRIPTOR_START_SECTOR
            .checked_mul(ISO_SECTOR_SIZE)
            .ok_or_else(|| anyhow!("ISO descriptor offset overflow"))?;
        let (_, validator) = fetch_exact_range(
            &client,
            url.as_str(),
            descriptor_start,
            descriptor_start + ISO_SECTOR_SIZE - 1,
            None,
        )?;
        let available = validator
            .total_size
            .checked_sub(descriptor_start)
            .ok_or_else(|| anyhow!("remote ISO is smaller than its volume descriptor area"))?;
        let descriptor_length = available.min(MAX_ISO_DESCRIPTOR_SECTORS * ISO_SECTOR_SIZE);
        if descriptor_length < ISO_SECTOR_SIZE {
            bail!("remote ISO has an incomplete volume descriptor");
        }
        let descriptor_end = descriptor_start
            .checked_add(descriptor_length - 1)
            .ok_or_else(|| anyhow!("ISO descriptor range overflow"))?;
        let (descriptors, descriptor_validator) = fetch_exact_range(
            &client,
            url.as_str(),
            descriptor_start,
            descriptor_end,
            Some(&validator),
        )?;
        let embedded = locate_iso_install_image(
            &descriptors,
            descriptor_validator.total_size,
            |start, end| {
                fetch_exact_range(
                    &client,
                    url.as_str(),
                    start,
                    end,
                    Some(&descriptor_validator),
                )
                .map(|(bytes, _)| bytes)
            },
        )?;
        return read_remote_wim_in_iso(&client, url.as_str(), &embedded, &descriptor_validator);
    }

    read_remote_wim_at(&client, url.as_str(), 0, None, None)
}

fn read_remote_wim_at(
    client: &Client,
    url: &str,
    base_offset: u64,
    embedded_size: Option<u64>,
    expected: Option<&EntityValidator>,
) -> Result<Vec<ImageInfo>> {
    if embedded_size.is_some_and(|size| size < WIM_HEADER_SIZE as u64) {
        bail!("remote embedded WIM/ESD is smaller than its header");
    }
    let header_end = base_offset
        .checked_add(WIM_HEADER_SIZE as u64 - 1)
        .ok_or_else(|| anyhow!("remote WIM header range overflow"))?;
    if let Some(size) = embedded_size {
        let image_end = base_offset
            .checked_add(size)
            .ok_or_else(|| anyhow!("remote embedded WIM/ESD extent overflow"))?;
        if header_end >= image_end {
            bail!("remote embedded WIM/ESD header escapes its ISO extent");
        }
    }
    let (header, validator) = fetch_exact_range(client, url, base_offset, header_end, expected)?;
    let image_size = embedded_size.unwrap_or(validator.total_size);
    let (relative_start, relative_end) = wim_xml_logical_range(&header, image_size)?;
    let start = base_offset
        .checked_add(relative_start)
        .ok_or_else(|| anyhow!("remote WIM XML absolute offset overflow"))?;
    let end = base_offset
        .checked_add(relative_end)
        .ok_or_else(|| anyhow!("remote WIM XML absolute range overflow"))?;
    let (xml_bytes, second_validator) =
        fetch_exact_range(client, url, start, end, Some(&validator))?;
    if second_validator.total_size != validator.total_size {
        bail!("remote image changed while metadata was being read");
    }
    decode_remote_wim_images(&xml_bytes)
}

fn read_remote_wim_in_iso(
    client: &Client,
    url: &str,
    image: &IsoFile,
    validator: &EntityValidator,
) -> Result<Vec<ImageInfo>> {
    if image.length < WIM_HEADER_SIZE as u64 {
        bail!("remote embedded WIM/ESD is smaller than its header");
    }
    let header = fetch_iso_file_range(image, 0, WIM_HEADER_SIZE as u64 - 1, |start, end| {
        fetch_exact_range(client, url, start, end, Some(validator)).map(|(bytes, _)| bytes)
    })?;
    let (xml_start, xml_end) = wim_xml_logical_range(&header, image.length)?;
    let xml_bytes = fetch_iso_file_range(image, xml_start, xml_end, |start, end| {
        fetch_exact_range(client, url, start, end, Some(validator)).map(|(bytes, _)| bytes)
    })?;
    decode_remote_wim_images(&xml_bytes)
}

fn wim_xml_logical_range(header: &[u8], image_size: u64) -> Result<(u64, u64)> {
    let resource = parse_wim_xml_resource(header, Some(image_size))
        .map_err(|error| anyhow!("invalid remote WIM header: {error}"))?;
    if resource.stored_size == 0 || resource.stored_size > MAX_REMOTE_WIM_XML_BYTES {
        bail!("remote WIM XML resource has an unsafe size");
    }
    let end_exclusive = resource
        .offset
        .checked_add(resource.stored_size)
        .ok_or_else(|| anyhow!("remote WIM XML range overflow"))?;
    if end_exclusive > image_size {
        bail!("remote WIM XML resource escapes the image extent");
    }
    Ok((resource.offset, end_exclusive - 1))
}

fn decode_remote_wim_images(xml_bytes: &[u8]) -> Result<Vec<ImageInfo>> {
    let xml = lr_core::image_meta::decode_wim_xml(xml_bytes)
        .map_err(|error| anyhow!("invalid remote WIM XML: {error}"))?;
    let images = parse_image_info_from_xml(&xml);
    if images.is_empty() {
        bail!("remote WIM XML contains no images");
    }
    Ok(images)
}

fn locate_iso_install_image<F>(descriptors: &[u8], total_size: u64, mut fetch: F) -> Result<IsoFile>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>>,
{
    let roots = parse_iso_roots(descriptors)?;
    let mut last_error = None;
    for root in roots {
        let result = (|| {
            let root_bytes = fetch_iso_directory(&root.entry, total_size, &mut fetch)?;
            let root_entries = parse_iso_directory(&root_bytes, root.encoding)?;
            let sources = root_entries
                .into_iter()
                .find(|entry| entry.is_directory && entry.name.eq_ignore_ascii_case("sources"))
                .ok_or_else(|| anyhow!("ISO root contains no sources directory"))?;
            let source_bytes = fetch_iso_directory(&sources, total_size, &mut fetch)?;
            let entries = parse_iso_directory(&source_bytes, root.encoding)?;
            for wanted in ["install.esd", "install.wim"] {
                if let Some(file) = collect_iso_file(&entries, wanted, total_size)? {
                    return Ok(file);
                }
            }
            bail!("ISO sources directory contains no install.esd or install.wim")
        })();
        match result {
            Ok(entry) => return Ok(entry),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("ISO has no usable ISO 9660 or Joliet volume")))
}

fn parse_iso_roots(descriptors: &[u8]) -> Result<Vec<IsoRoot>> {
    let mut primary = Vec::new();
    let mut joliet = Vec::new();
    for descriptor in descriptors.chunks_exact(ISO_SECTOR_SIZE as usize) {
        let descriptor_type = descriptor[0];
        if descriptor_type == 255 {
            break;
        }
        if &descriptor[1..6] != b"CD001" || descriptor[6] != 1 {
            continue;
        }
        let encoding = match descriptor_type {
            1 => IsoNameEncoding::Ascii,
            2 if matches!(&descriptor[88..91], b"%/@" | b"%/C" | b"%/E") => IsoNameEncoding::Joliet,
            _ => continue,
        };
        let root = parse_iso_record(&descriptor[156..], encoding)?
            .ok_or_else(|| anyhow!("ISO volume descriptor has no root directory record"))?;
        if !root.is_directory {
            bail!("ISO root record is not a directory");
        }
        let value = IsoRoot {
            entry: root,
            encoding,
        };
        match encoding {
            IsoNameEncoding::Ascii => primary.push(value),
            IsoNameEncoding::Joliet => joliet.push(value),
        }
    }
    joliet.extend(primary);
    if joliet.is_empty() {
        bail!("remote ISO has no ISO 9660/Joliet root descriptor");
    }
    Ok(joliet)
}

fn fetch_iso_directory<F>(entry: &IsoEntry, total_size: u64, fetch: &mut F) -> Result<Vec<u8>>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>>,
{
    if !entry.is_directory || entry.length == 0 {
        bail!("invalid ISO directory extent");
    }
    if entry.length > MAX_REMOTE_ISO_DIRECTORY_BYTES {
        bail!("ISO directory extent exceeds the metadata safety limit");
    }
    let end = checked_iso_file_extent(entry, total_size)?;
    fetch(entry.offset, end)
}

fn checked_iso_file_extent(entry: &IsoEntry, total_size: u64) -> Result<u64> {
    if entry.length == 0 {
        bail!("ISO extent is empty");
    }
    let end_exclusive = entry
        .offset
        .checked_add(entry.length)
        .ok_or_else(|| anyhow!("ISO extent overflow"))?;
    if end_exclusive > total_size {
        bail!("ISO extent escapes the remote file");
    }
    Ok(end_exclusive - 1)
}

fn collect_iso_file(
    entries: &[IsoEntry],
    wanted: &str,
    total_size: u64,
) -> Result<Option<IsoFile>> {
    let matching = entries
        .iter()
        .filter(|entry| !entry.is_directory && entry.name.eq_ignore_ascii_case(wanted))
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return Ok(None);
    }
    let mut extents = Vec::with_capacity(matching.len());
    let mut length = 0u64;
    for (index, entry) in matching.iter().enumerate() {
        checked_iso_file_extent(entry, total_size)?;
        let is_last = index + 1 == matching.len();
        if entry.is_multi_extent == is_last {
            bail!("ISO has an invalid multi-extent chain for {wanted}");
        }
        length = length
            .checked_add(entry.length)
            .ok_or_else(|| anyhow!("ISO logical file length overflow"))?;
        extents.push((*entry).clone());
    }
    Ok(Some(IsoFile {
        name: matching[0].name.clone(),
        extents,
        length,
    }))
}

fn fetch_iso_file_range<F>(
    file: &IsoFile,
    logical_start: u64,
    logical_end: u64,
    mut fetch: F,
) -> Result<Vec<u8>>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>>,
{
    if logical_start > logical_end || logical_end >= file.length {
        bail!("requested WIM metadata range escapes its ISO file extents");
    }
    let expected_length = logical_end
        .checked_sub(logical_start)
        .and_then(|length| length.checked_add(1))
        .ok_or_else(|| anyhow!("ISO logical metadata range overflow"))?;
    let capacity = usize::try_from(expected_length)
        .map_err(|_| anyhow!("ISO metadata range exceeds process address space"))?;
    let mut output = Vec::with_capacity(capacity);
    let mut logical_extent_start = 0u64;
    for extent in &file.extents {
        let logical_extent_end = logical_extent_start
            .checked_add(extent.length)
            .ok_or_else(|| anyhow!("ISO logical extent overflow"))?;
        let overlap_start = logical_start.max(logical_extent_start);
        let overlap_end_exclusive = (logical_end + 1).min(logical_extent_end);
        if overlap_start < overlap_end_exclusive {
            let within_extent = overlap_start - logical_extent_start;
            let physical_start = extent
                .offset
                .checked_add(within_extent)
                .ok_or_else(|| anyhow!("ISO physical metadata offset overflow"))?;
            let physical_end = physical_start
                .checked_add(overlap_end_exclusive - overlap_start - 1)
                .ok_or_else(|| anyhow!("ISO physical metadata range overflow"))?;
            output.extend_from_slice(&fetch(physical_start, physical_end)?);
        }
        logical_extent_start = logical_extent_end;
        if logical_extent_start > logical_end {
            break;
        }
    }
    if output.len() != capacity {
        bail!("ISO multi-extent metadata range was incomplete");
    }
    Ok(output)
}

fn parse_iso_directory(bytes: &[u8], encoding: IsoNameEncoding) -> Result<Vec<IsoEntry>> {
    let mut entries = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        let record_length = bytes[offset] as usize;
        if record_length == 0 {
            let next_sector = (offset / ISO_SECTOR_SIZE as usize + 1)
                .checked_mul(ISO_SECTOR_SIZE as usize)
                .ok_or_else(|| anyhow!("ISO directory sector offset overflow"))?;
            if next_sector <= offset {
                bail!("invalid ISO directory padding");
            }
            offset = next_sector;
            continue;
        }
        let end = offset
            .checked_add(record_length)
            .ok_or_else(|| anyhow!("ISO directory record overflow"))?;
        if end > bytes.len() {
            bail!("ISO directory record escapes its extent");
        }
        if let Some(entry) = parse_iso_record(&bytes[offset..end], encoding)? {
            entries.push(entry);
        }
        offset = end;
    }
    Ok(entries)
}

fn parse_iso_record(record: &[u8], encoding: IsoNameEncoding) -> Result<Option<IsoEntry>> {
    let Some(&record_length) = record.first() else {
        return Ok(None);
    };
    if record_length == 0 {
        return Ok(None);
    }
    let record_length = record_length as usize;
    if record_length < 34 || record.len() < record_length {
        bail!("invalid ISO directory record length");
    }
    let identifier_length = record[32] as usize;
    let identifier_end = 33usize
        .checked_add(identifier_length)
        .ok_or_else(|| anyhow!("ISO identifier length overflow"))?;
    if identifier_length == 0 || identifier_end > record_length {
        bail!("invalid ISO directory identifier length");
    }
    let identifier = &record[33..identifier_end];
    if record[26] != 0 || record[27] != 0 {
        bail!("interleaved ISO files are not supported");
    }
    let extent_lba = u32::from_le_bytes(record[2..6].try_into().unwrap()) as u64;
    let extended_attribute_sectors = record[1] as u64;
    let data_lba = extent_lba
        .checked_add(extended_attribute_sectors)
        .ok_or_else(|| anyhow!("ISO extent LBA overflow"))?;
    let offset = data_lba
        .checked_mul(ISO_SECTOR_SIZE)
        .ok_or_else(|| anyhow!("ISO extent byte offset overflow"))?;
    let length = u32::from_le_bytes(record[10..14].try_into().unwrap()) as u64;
    let name = if identifier.len() == 1 && identifier[0] == 0 {
        ".".to_owned()
    } else if identifier.len() == 1 && identifier[0] == 1 {
        "..".to_owned()
    } else {
        decode_iso_name(identifier, encoding)?
    };
    Ok(Some(IsoEntry {
        name,
        offset,
        length,
        is_directory: record[25] & 0x02 != 0,
        is_multi_extent: record[25] & 0x80 != 0,
    }))
}

fn decode_iso_name(identifier: &[u8], encoding: IsoNameEncoding) -> Result<String> {
    let decoded = match encoding {
        IsoNameEncoding::Ascii => String::from_utf8(identifier.to_vec())
            .context("ISO 9660 identifier is not valid ASCII/UTF-8")?,
        IsoNameEncoding::Joliet => {
            if !identifier.len().is_multiple_of(2) {
                bail!("Joliet identifier has an odd byte length");
            }
            let units = identifier
                .chunks_exact(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]));
            String::from_utf16(&units.collect::<Vec<_>>())
                .context("Joliet identifier is not valid UCS-2/UTF-16")?
        }
    };
    Ok(decoded
        .strip_suffix(";1")
        .unwrap_or(&decoded)
        .trim_end_matches('.')
        .to_owned())
}

fn fetch_exact_range(
    client: &Client,
    url: &str,
    start: u64,
    end: u64,
    expected: Option<&EntityValidator>,
) -> Result<(Vec<u8>, EntityValidator)> {
    let request_url = expected
        .map(|validator| validator.resolved_url.as_str())
        .unwrap_or(url);
    let mut request = client
        .get(request_url)
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
        if expected.resolved_url != validator.resolved_url {
            bail!("remote image redirect target changed between metadata requests");
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

fn redirect_target_is_allowed(url: &reqwest::Url, allow_insecure_http: bool) -> bool {
    url.host().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && (url.scheme() == "https" || (allow_insecure_http && url.scheme() == "http"))
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
        resolved_url: response.url().as_str().to_owned(),
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

    fn iso_record(identifier: &[u8], lba: u32, length: u32, flags: u8) -> Vec<u8> {
        let padding = usize::from(identifier.len().is_multiple_of(2));
        let record_length = 33 + identifier.len() + padding;
        let mut record = vec![0u8; record_length];
        record[0] = record_length as u8;
        record[2..6].copy_from_slice(&lba.to_le_bytes());
        record[6..10].copy_from_slice(&lba.to_be_bytes());
        record[10..14].copy_from_slice(&length.to_le_bytes());
        record[14..18].copy_from_slice(&length.to_be_bytes());
        record[25] = flags;
        record[28..30].copy_from_slice(&1u16.to_le_bytes());
        record[30..32].copy_from_slice(&1u16.to_be_bytes());
        record[32] = identifier.len() as u8;
        record[33..33 + identifier.len()].copy_from_slice(identifier);
        record
    }

    fn ascii_install_iso_fixture() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let root_record = iso_record(&[0], 40, ISO_SECTOR_SIZE as u32, 0x02);
        let mut descriptors = vec![0u8; ISO_SECTOR_SIZE as usize * 2];
        descriptors[0] = 1;
        descriptors[1..6].copy_from_slice(b"CD001");
        descriptors[6] = 1;
        descriptors[156..156 + root_record.len()].copy_from_slice(&root_record);
        let terminator = ISO_SECTOR_SIZE as usize;
        descriptors[terminator] = 255;
        descriptors[terminator + 1..terminator + 6].copy_from_slice(b"CD001");
        descriptors[terminator + 6] = 1;

        let sources_record = iso_record(b"SOURCES", 41, ISO_SECTOR_SIZE as u32, 0x02);
        let mut root = vec![0u8; ISO_SECTOR_SIZE as usize];
        root[..sources_record.len()].copy_from_slice(&sources_record);

        let install_record = iso_record(b"INSTALL.WIM;1", 42, 123_456, 0);
        let mut sources = vec![0u8; ISO_SECTOR_SIZE as usize];
        sources[..install_record.len()].copy_from_slice(&install_record);
        (descriptors, root, sources)
    }

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
    fn redirect_policy_preserves_transport_boundary_and_rejects_credentials() {
        let https = reqwest::Url::parse("https://cdn.example.com/install.wim").unwrap();
        let http = reqwest::Url::parse("http://cdn.example.com/install.wim").unwrap();
        let credentials =
            reqwest::Url::parse("https://user:secret@cdn.example.com/install.wim").unwrap();
        assert!(redirect_target_is_allowed(&https, false));
        assert!(!redirect_target_is_allowed(&http, false));
        assert!(redirect_target_is_allowed(&http, true));
        assert!(!redirect_target_is_allowed(&credentials, true));
    }

    #[test]
    fn locates_install_wim_inside_iso9660_without_reading_its_payload() {
        let (descriptors, root, sources) = ascii_install_iso_fixture();
        let root_start = 40 * ISO_SECTOR_SIZE;
        let source_start = 41 * ISO_SECTOR_SIZE;
        let image = locate_iso_install_image(&descriptors, 2_000_000, |start, end| {
            if start == root_start && end == root_start + ISO_SECTOR_SIZE - 1 {
                return Ok(root.clone());
            }
            if start == source_start && end == source_start + ISO_SECTOR_SIZE - 1 {
                return Ok(sources.clone());
            }
            bail!("unexpected fixture range {start}-{end}")
        })
        .unwrap();
        assert_eq!(image.name, "INSTALL.WIM");
        assert_eq!(image.extents.len(), 1);
        assert_eq!(image.extents[0].offset, 42 * ISO_SECTOR_SIZE);
        assert_eq!(image.length, 123_456);
    }

    #[test]
    fn reads_logical_metadata_across_iso_multi_extent_boundaries() {
        let first = IsoEntry {
            name: "INSTALL.WIM".into(),
            offset: 10_000,
            length: 4,
            is_directory: false,
            is_multi_extent: true,
        };
        let second = IsoEntry {
            name: "INSTALL.WIM".into(),
            offset: 20_000,
            length: 4,
            is_directory: false,
            is_multi_extent: false,
        };
        let file = collect_iso_file(&[first, second], "install.wim", 30_000)
            .unwrap()
            .unwrap();
        let bytes = fetch_iso_file_range(&file, 2, 5, |start, end| match (start, end) {
            (10_002, 10_003) => Ok(vec![2, 3]),
            (20_000, 20_001) => Ok(vec![4, 5]),
            range => bail!("unexpected fixture range {range:?}"),
        })
        .unwrap();
        assert_eq!(bytes, vec![2, 3, 4, 5]);
    }

    #[test]
    fn decodes_joliet_names_and_rejects_odd_byte_identifiers() {
        let encoded = "install.esd;1"
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect::<Vec<_>>();
        assert_eq!(
            decode_iso_name(&encoded, IsoNameEncoding::Joliet).unwrap(),
            "install.esd"
        );
        assert!(decode_iso_name(&[0, b'I', 0], IsoNameEncoding::Joliet).is_err());
    }

    #[test]
    fn rejects_iso_extents_that_escape_the_remote_entity() {
        let entry = IsoEntry {
            name: "install.wim".into(),
            offset: 9_000,
            length: 2_000,
            is_directory: false,
            is_multi_extent: false,
        };
        assert!(checked_iso_file_extent(&entry, 10_000).is_err());
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
