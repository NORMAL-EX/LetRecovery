//! Microsoft Media Creation Tool product-catalogue boundary.
//!
//! Windows 11 25H2 no longer uses the legacy fixed `products.cab` fwlink. The
//! current MCT asks Microsoft Update Metadata Service for a short-lived CAB URL;
//! that CAB contains long-lived `CLIENTCONSUMER_RET` ESD URLs and their SHA-256
//! hashes. This module mirrors that read-only flow, verifies the CAB response,
//! extracts `products.xml` through the shared SetupAPI boundary, and publishes
//! only the Simplified-Chinese x64 consumer ESD.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT, CONTENT_ENCODING, CONTENT_TYPE};
use reqwest::redirect::{Action, Attempt, Policy};
use reqwest::Url;
use serde::Deserialize;

use crate::download::config::OnlineSystem;

const UPDATE_SEARCH_URL: &str =
    "https://fe3.delivery.mp.microsoft.com/UpdateMetadataService/updates/search/v1/bydeviceinfo/";
const WINDOWS_11_PRODUCTS: &str = "PN=Windows.Products.Cab.amd64&V=0.0.0.0";
const WINDOWS_11_DEVICE_ATTRIBUTES: &str = "DUScan=1;OSVersion=10.0.26100.1";
const WINDOWS_10_PRODUCTS_CAB: &str = "https://go.microsoft.com/fwlink/?LinkId=841361";
const PRODUCTS_XML: &str = "products.xml";
const MAX_REDIRECTS: usize = 5;
const MAX_METADATA_BYTES: usize = 512 * 1024;
const MAX_CAB_BYTES: usize = 1024 * 1024;
const MAX_XML_BYTES: usize = 8 * 1024 * 1024;
const MIN_CAB_BYTES: u64 = 1024;
const MAX_ESD_BYTES: u64 = 16 * 1024 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct UpdateSearchResult {
    #[serde(default)]
    update_ids: Vec<String>,
    #[serde(default)]
    file_locations: Vec<UpdateFileLocation>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct UpdateFileLocation {
    file_name: String,
    size: u64,
    #[serde(default)]
    digest: String,
    #[serde(default)]
    content_type: String,
    url: String,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
struct UpdateSearchRequest<'a> {
    products: &'a str,
    device_attributes: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProductImage {
    filename: String,
    download_url: String,
    size: u64,
    sha256: Option<String>,
    is_win11: bool,
    release: String,
}

/// Fetches the current official Simplified-Chinese x64 consumer ESD list.
///
/// Windows 11 is mandatory because the dynamic Microsoft catalogue is the
/// authoritative source requested by mode 1. Windows 10 is appended when its
/// legacy official fwlink remains available, but its failure must not hide a
/// valid current Windows 11 catalogue.
pub fn fetch_current_systems() -> Result<Vec<OnlineSystem>> {
    let client = build_client()?;
    let win11_location = fetch_windows_11_cab_location(&client)
        .context("query Microsoft Windows 11 products.cab metadata")?;
    let win11 = fetch_and_parse_products_cab(&client, &win11_location)
        .context("read Microsoft Windows 11 products.cab")?;
    if !win11.is_win11 {
        bail!("Microsoft Windows 11 catalogue returned a non-Windows-11 image");
    }

    let mut systems = vec![to_online_system(win11)];
    match fetch_legacy_windows_10(&client) {
        Ok(win10) if !win10.is_win11 => systems.push(to_online_system(win10)),
        Ok(_) => log::warn!("Microsoft Windows 10 catalogue unexpectedly returned Windows 11"),
        Err(error) => log::warn!(
            "Microsoft Windows 10 products.cab is temporarily unavailable; keeping the current Windows 11 entry: {error:#}"
        ),
    }
    Ok(systems)
}

fn build_client() -> Result<Client> {
    Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .user_agent("LetRecovery/official-microsoft-catalogue")
        .redirect(Policy::custom(official_redirect_policy))
        .build()
        .context("create Microsoft catalogue HTTP client")
}

fn official_redirect_policy(attempt: Attempt<'_>) -> Action {
    if attempt.previous().len() >= MAX_REDIRECTS {
        return attempt.error("too many Microsoft catalogue redirects");
    }
    if is_allowed_transport_url(attempt.url()) {
        attempt.follow()
    } else {
        attempt.error("Microsoft catalogue redirected outside an allowed host")
    }
}

fn fetch_windows_11_cab_location(client: &Client) -> Result<UpdateFileLocation> {
    let response = client
        .post(UPDATE_SEARCH_URL)
        .header(ACCEPT, "application/json")
        .json(&UpdateSearchRequest {
            products: WINDOWS_11_PRODUCTS,
            device_attributes: WINDOWS_11_DEVICE_ATTRIBUTES,
        })
        .send()
        .context("send Microsoft Update Metadata Service request")?;
    validate_final_url(&response, TransportRole::Metadata)?;
    let text = read_bounded(response, MAX_METADATA_BYTES)
        .context("read Microsoft Update Metadata Service response")?;
    let results: Vec<UpdateSearchResult> =
        serde_json::from_slice(&text).context("parse Microsoft update metadata JSON")?;

    let mut locations = Vec::new();
    for result in results {
        if result.update_ids.len() != 1 || !is_uuid(&result.update_ids[0]) {
            bail!("Microsoft update metadata returned an invalid update identity");
        }
        locations.extend(
            result
                .file_locations
                .into_iter()
                .filter(|location| location.file_name.eq_ignore_ascii_case("products.cab")),
        );
    }
    match locations.len() {
        1 => validate_dynamic_cab_location(locations.remove(0)),
        0 => bail!("Microsoft update metadata did not return products.cab"),
        _ => bail!("Microsoft update metadata returned multiple products.cab files"),
    }
}

fn validate_dynamic_cab_location(location: UpdateFileLocation) -> Result<UpdateFileLocation> {
    if !(MIN_CAB_BYTES..=MAX_CAB_BYTES as u64).contains(&location.size) {
        bail!("Microsoft products.cab declared an unsafe size");
    }
    if !location.content_type.eq_ignore_ascii_case("Metadata") {
        bail!("Microsoft products.cab has an unexpected content type");
    }
    let digest = BASE64_STANDARD
        .decode(location.digest.trim())
        .context("decode Microsoft products.cab SHA-256 digest")?;
    if digest.len() != 32 {
        bail!("Microsoft products.cab digest is not SHA-256");
    }
    let url = Url::parse(&location.url).context("parse Microsoft products.cab URL")?;
    if !is_dynamic_cab_url(&url) {
        bail!("Microsoft products.cab URL is outside the delivery service");
    }
    Ok(location)
}

fn fetch_and_parse_products_cab(
    client: &Client,
    location: &UpdateFileLocation,
) -> Result<ProductImage> {
    let response = client
        .get(&location.url)
        .header(
            ACCEPT,
            "application/vnd.ms-cab-compressed, application/octet-stream",
        )
        .header("Accept-Encoding", "identity")
        .send()
        .context("download Microsoft products.cab")?;
    validate_final_url(&response, TransportRole::Cabinet)?;
    let bytes = read_bounded(response, MAX_CAB_BYTES)?;
    if bytes.len() as u64 != location.size {
        bail!(
            "Microsoft products.cab size mismatch: expected {}, got {}",
            location.size,
            bytes.len()
        );
    }
    verify_cab_digest(&bytes, &location.digest)?;
    parse_cab_bytes(&bytes, ImageHashPolicy::RequireSha256)
}

fn fetch_legacy_windows_10(client: &Client) -> Result<ProductImage> {
    let response = client
        .get(WINDOWS_10_PRODUCTS_CAB)
        .header(
            ACCEPT,
            "application/vnd.ms-cab-compressed, application/octet-stream",
        )
        .header("Accept-Encoding", "identity")
        .send()
        .context("download Microsoft Windows 10 products.cab")?;
    validate_final_url(&response, TransportRole::LegacyCabinet)?;
    let bytes = read_bounded(response, MAX_CAB_BYTES)?;
    if bytes.len() < MIN_CAB_BYTES as usize {
        bail!("Microsoft Windows 10 products.cab is unexpectedly small");
    }
    parse_cab_bytes(&bytes, ImageHashPolicy::AllowLegacySha1)
}

fn verify_cab_digest(bytes: &[u8], expected_base64: &str) -> Result<()> {
    let expected = BASE64_STANDARD
        .decode(expected_base64.trim())
        .context("decode Microsoft products.cab digest")?;
    let actual_hex = lr_core::hash::sha256_bytes(bytes);
    let actual = hex_to_bytes(&actual_hex)?;
    if actual != expected {
        bail!("Microsoft products.cab SHA-256 mismatch");
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ImageHashPolicy {
    RequireSha256,
    AllowLegacySha1,
}

fn parse_cab_bytes(bytes: &[u8], hash_policy: ImageHashPolicy) -> Result<ProductImage> {
    if bytes.len() < MIN_CAB_BYTES as usize
        || bytes.len() > MAX_CAB_BYTES
        || !bytes.starts_with(b"MSCF")
    {
        bail!("Microsoft products.cab is not a bounded CAB file");
    }
    let temp = lr_core::scoped_temp_file::ScopedTempDir::create_in(
        &std::env::temp_dir(),
        "letrecovery-microsoft-products",
    )
    .context("create products.cab temporary directory")?;
    let cab_path = temp.path().join("products.cab");
    let mut cab = File::create(&cab_path).context("create temporary products.cab")?;
    cab.write_all(bytes)
        .context("write temporary products.cab")?;
    cab.flush().context("flush temporary products.cab")?;
    drop(cab);

    let extractor = lr_core::windows_cabinet::CabinetExtractor::new()?;
    let entries = extractor
        .list_contents(&cab_path)
        .context("enumerate Microsoft products.cab")?;
    if entries.len() != 1 || !entries[0].eq_ignore_ascii_case(PRODUCTS_XML) {
        bail!("Microsoft products.cab must contain only products.xml");
    }
    let output = temp.path().join("expanded");
    let files = extractor
        .extract(&cab_path, &output)
        .context("extract Microsoft products.xml with SetupAPI")?;
    if files.len() != 1 {
        bail!("Microsoft products.cab extraction returned an unexpected file count");
    }
    let xml_path = &files[0];
    if !xml_path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(PRODUCTS_XML))
    {
        bail!("Microsoft products.cab extracted an unexpected file");
    }
    let mut xml = Vec::new();
    File::open(xml_path)
        .context("open extracted Microsoft products.xml")?
        .take(MAX_XML_BYTES as u64 + 1)
        .read_to_end(&mut xml)
        .context("read extracted Microsoft products.xml")?;
    if xml.len() > MAX_XML_BYTES {
        bail!("Microsoft products.xml exceeds the bounded size limit");
    }
    let xml = std::str::from_utf8(&xml).context("Microsoft products.xml is not UTF-8")?;
    parse_products_xml(xml, hash_policy)
}

fn parse_products_xml(xml: &str, hash_policy: ImageHashPolicy) -> Result<ProductImage> {
    let document = roxmltree::Document::parse(xml).context("parse Microsoft products.xml")?;
    if document.root_element().tag_name().name() != "MCT" {
        bail!("Microsoft products.xml has an unexpected root element");
    }
    let mut images = BTreeMap::<String, ProductImage>::new();
    for node in document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "File")
    {
        let filename = child_text(node, "FileName").unwrap_or_default();
        let language = child_text(node, "LanguageCode").unwrap_or_default();
        let architecture = child_text(node, "Architecture").unwrap_or_default();
        if !language.eq_ignore_ascii_case("zh-cn")
            || !architecture.eq_ignore_ascii_case("x64")
            || !is_consumer_x64_zh_cn_esd(filename)
        {
            continue;
        }

        let url_text = child_text(node, "FilePath").context("consumer ESD has no FilePath")?;
        let url = validate_esd_url(url_text)?;
        let size = child_text(node, "Size")
            .context("consumer ESD has no Size")?
            .parse::<u64>()
            .context("consumer ESD has an invalid Size")?;
        if size == 0 || size > MAX_ESD_BYTES {
            bail!("consumer ESD declared an unsafe size");
        }
        let sha256 = match child_text(node, "Sha256") {
            Some(value) => {
                let value = value.trim().to_ascii_lowercase();
                if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    bail!("consumer ESD has an invalid SHA-256");
                }
                Some(value)
            }
            None if matches!(hash_policy, ImageHashPolicy::RequireSha256) => {
                bail!("consumer ESD has no SHA-256");
            }
            None => {
                // Microsoft's maintained Windows 10 22H2 products.cab still
                // uses the legacy Sha1 field. Validate the declaration but do
                // not promote SHA-1 into the SHA-256 integrity contract.
                let sha1 = child_text(node, "Sha1")
                    .context("legacy consumer ESD has no declared hash")?
                    .trim();
                if sha1.len() != 40 || !sha1.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    bail!("legacy consumer ESD has an invalid SHA-1");
                }
                None
            }
        };
        lr_core::download_integrity::validate_download_filename(filename)
            .context("consumer ESD has an unsafe filename")?;
        if url
            .path_segments()
            .and_then(Iterator::last)
            .is_none_or(|remote_name| remote_name != filename)
        {
            bail!("consumer ESD filename does not match its Microsoft URL");
        }
        let (is_win11, release) = classify_release(filename)?;
        let image = ProductImage {
            filename: filename.to_owned(),
            download_url: url.to_string(),
            size,
            sha256,
            is_win11,
            release,
        };
        let identity = image.download_url.to_ascii_lowercase();
        if let Some(existing) = images.get(&identity) {
            if existing != &image {
                bail!("Microsoft products.xml contains conflicting duplicate ESD metadata");
            }
        } else {
            images.insert(identity, image);
        }
    }
    match images.len() {
        1 => Ok(images.into_values().next().expect("one image exists")),
        0 => bail!("Microsoft products.xml has no Simplified-Chinese x64 consumer ESD"),
        _ => bail!("Microsoft products.xml contains multiple consumer ESD payloads"),
    }
}

fn child_text<'a, 'input>(node: roxmltree::Node<'a, 'input>, name: &str) -> Option<&'a str> {
    node.children()
        .find(|child| child.is_element() && child.tag_name().name() == name)
        .and_then(|child| child.text())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn is_consumer_x64_zh_cn_esd(filename: &str) -> bool {
    filename
        .to_ascii_lowercase()
        .ends_with("_clientconsumer_ret_x64fre_zh-cn.esd")
}

fn classify_release(filename: &str) -> Result<(bool, String)> {
    let build = filename
        .split('.')
        .next()
        .context("consumer ESD filename has no build")?
        .parse::<u32>()
        .context("consumer ESD filename has an invalid build")?;
    let is_win11 = match build {
        10_240..=21_999 => false,
        22_000..=99_999 => true,
        _ => bail!("consumer ESD build is outside the supported Windows 10/11 range"),
    };
    let release = filename
        .split(|character: char| !character.is_ascii_alphanumeric())
        .find(|part| {
            let lower = part.to_ascii_lowercase();
            lower.len() == 4
                && lower.as_bytes()[0].is_ascii_digit()
                && lower.as_bytes()[1].is_ascii_digit()
                && lower.as_bytes()[2] == b'h'
                && matches!(lower.as_bytes()[3], b'1' | b'2')
        })
        .map(str::to_ascii_uppercase)
        .context("consumer ESD filename has no release identifier")?;
    Ok((is_win11, release))
}

fn validate_esd_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("parse Microsoft consumer ESD URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str() != Some("dl.delivery.mp.microsoft.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.path().to_ascii_lowercase().ends_with(".esd")
    {
        bail!("consumer ESD URL is outside the long-lived Microsoft delivery boundary");
    }
    Ok(url)
}

fn to_online_system(image: ProductImage) -> OnlineSystem {
    let family = if image.is_win11 {
        "Windows 11"
    } else {
        "Windows 10"
    };
    OnlineSystem {
        download_url: image.download_url,
        display_name: format!(
            "{family} {} 官方原版（简体中文 x64 ESD，{}）",
            image.release,
            format_bytes(image.size)
        ),
        is_win11: image.is_win11,
        filename: Some(image.filename),
        md5: None,
        sha256: image.sha256,
    }
}

fn format_bytes(bytes: u64) -> String {
    format!("{:.2} GB", bytes as f64 / 1024_f64.powi(3))
}

fn read_bounded(response: Response, limit: usize) -> Result<Vec<u8>> {
    if response
        .headers()
        .get(CONTENT_ENCODING)
        .is_some_and(|value| {
            !value
                .to_str()
                .is_ok_and(|value| value.eq_ignore_ascii_case("identity"))
        })
    {
        bail!("Microsoft catalogue response used content encoding");
    }
    let response = response
        .error_for_status()
        .context("Microsoft catalogue returned an HTTP error")?;
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        bail!("Microsoft catalogue response exceeds the bounded size limit");
    }
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    response
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .context("read Microsoft catalogue response")?;
    if bytes.len() > limit {
        bail!("Microsoft catalogue response exceeds the bounded size limit");
    }
    Ok(bytes)
}

#[derive(Clone, Copy)]
enum TransportRole {
    Metadata,
    Cabinet,
    LegacyCabinet,
}

fn validate_final_url(response: &Response, role: TransportRole) -> Result<()> {
    let valid = match role {
        TransportRole::Metadata => {
            response.url().scheme() == "https" && response.url().as_str() == UPDATE_SEARCH_URL
        }
        TransportRole::Cabinet => is_cabinet_download_url(response.url()),
        TransportRole::LegacyCabinet => {
            response.url().scheme() == "https"
                && response.url().host_str().is_some_and(|host| {
                    host == "download.microsoft.com" || host.ends_with(".download.microsoft.com")
                })
        }
    };
    if !valid {
        bail!("Microsoft catalogue response used an unexpected final URL");
    }
    if matches!(role, TransportRole::Metadata)
        && !response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().starts_with("application/json"))
    {
        bail!("Microsoft metadata response is not JSON");
    }
    Ok(())
}

fn is_allowed_transport_url(url: &Url) -> bool {
    (url.scheme() == "https"
        && matches!(
            url.host_str(),
            Some("go.microsoft.com")
                | Some("download.microsoft.com")
                | Some("fe3.delivery.mp.microsoft.com")
        ))
        || is_cabinet_download_url(url)
}

fn is_dynamic_cab_url(url: &Url) -> bool {
    is_cabinet_download_url(url) && url.host_str() == Some("tlu.dl.delivery.mp.microsoft.com")
}

fn is_cabinet_download_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && matches!(
            url.host_str(),
            Some("tlu.dl.delivery.mp.microsoft.com") | Some("dl.delivery.mp.microsoft.com")
        )
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

fn hex_to_bytes(value: &str) -> Result<Vec<u8>> {
    if value.len() % 2 != 0 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid hexadecimal digest");
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).expect("hexadecimal bytes are ASCII");
            u8::from_str_radix(text, 16).context("decode hexadecimal digest")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CURRENT_25H2_XML: &str = r#"
      <MCT><Catalogs><Catalog><PublishedMedia><Files>
        <File>
          <FileName>26200.8875.260711-1836.25h2_ge_release_svc_refresh_CLIENTCONSUMER_RET_x64FRE_zh-cn.esd</FileName>
          <LanguageCode>zh-cn</LanguageCode><Architecture>x64</Architecture>
          <Edition>Professional</Edition><Size>6167539878</Size>
          <Sha256>9af007141812ab958a957d12378e79c29da712ed979697e182548dcdb5cb3d78</Sha256>
          <FilePath>http://dl.delivery.mp.microsoft.com/filestreamingservice/files/06947912-0f3c-4098-b6d3-05f51d27373a/26200.8875.260711-1836.25h2_ge_release_svc_refresh_CLIENTCONSUMER_RET_x64FRE_zh-cn.esd</FilePath>
        </File>
        <File>
          <FileName>26200.8875.260711-1836.25h2_ge_release_svc_refresh_CLIENTCONSUMER_RET_x64FRE_zh-cn.esd</FileName>
          <LanguageCode>zh-cn</LanguageCode><Architecture>x64</Architecture>
          <Edition>Core</Edition><Size>6167539878</Size>
          <Sha256>9af007141812ab958a957d12378e79c29da712ed979697e182548dcdb5cb3d78</Sha256>
          <FilePath>http://dl.delivery.mp.microsoft.com/filestreamingservice/files/06947912-0f3c-4098-b6d3-05f51d27373a/26200.8875.260711-1836.25h2_ge_release_svc_refresh_CLIENTCONSUMER_RET_x64FRE_zh-cn.esd</FilePath>
        </File>
      </Files></PublishedMedia></Catalog></Catalogs></MCT>
    "#;

    const LEGACY_22H2_XML: &str = r#"
      <MCT><Catalogs><Catalog><PublishedMedia><Files>
        <File>
          <FileName>19045.3803.231204-0204.22h2_release_svc_refresh_CLIENTCONSUMER_RET_x64FRE_zh-cn.esd</FileName>
          <LanguageCode>zh-cn</LanguageCode><Architecture>x64</Architecture>
          <Edition>Professional</Edition><Size>4185785021</Size>
          <Sha1>b4c440e96bd81efc6245d5fcc4682440d738e51a</Sha1>
          <FilePath>http://dl.delivery.mp.microsoft.com/filestreamingservice/files/04dcfb20-5583-43e7-8be1-30501b0a1c6e/19045.3803.231204-0204.22h2_release_svc_refresh_CLIENTCONSUMER_RET_x64FRE_zh-cn.esd</FilePath>
        </File>
      </Files></PublishedMedia></Catalog></Catalogs></MCT>
    "#;

    #[test]
    fn parses_and_deduplicates_current_25h2_consumer_esd() {
        let image = parse_products_xml(CURRENT_25H2_XML, ImageHashPolicy::RequireSha256).unwrap();
        assert!(image.is_win11);
        assert_eq!(image.release, "25H2");
        assert_eq!(image.size, 6_167_539_878);
        assert_eq!(image.sha256.as_deref().map(str::len), Some(64));
        assert!(image
            .download_url
            .starts_with("http://dl.delivery.mp.microsoft.com/filestreamingservice/files/"));
    }

    #[test]
    fn accepts_the_official_legacy_windows_10_sha1_without_claiming_sha256() {
        let image = parse_products_xml(LEGACY_22H2_XML, ImageHashPolicy::AllowLegacySha1).unwrap();
        assert!(!image.is_win11);
        assert_eq!(image.release, "22H2");
        assert!(image.sha256.is_none());
        assert!(parse_products_xml(LEGACY_22H2_XML, ImageHashPolicy::RequireSha256).is_err());
    }

    #[test]
    fn rejects_non_delivery_and_signed_query_esd_urls() {
        assert!(validate_esd_url("https://example.com/windows.esd").is_err());
        assert!(validate_esd_url(
            "http://dl.delivery.mp.microsoft.com/files/windows.esd?temporary=true"
        )
        .is_err());
    }

    #[test]
    fn update_metadata_requires_one_products_cab_and_sha256() {
        let valid = UpdateFileLocation {
            file_name: "products.cab".into(),
            size: 43_917,
            digest: "LmOE+TgNCX7leapHdQRWa+F/EYrIBvx7NS6D4O+SFJw=".into(),
            content_type: "Metadata".into(),
            url: "http://tlu.dl.delivery.mp.microsoft.com/filestreamingservice/files/id?P1=1"
                .into(),
        };
        assert!(validate_dynamic_cab_location(valid).is_ok());
    }

    #[test]
    fn transport_allowlist_rejects_lookalike_hosts() {
        assert!(is_dynamic_cab_url(
            &Url::parse("http://tlu.dl.delivery.mp.microsoft.com/files/id?P1=1").unwrap()
        ));
        assert!(!is_dynamic_cab_url(
            &Url::parse("http://tlu.dl.delivery.mp.microsoft.com.example/files/id").unwrap()
        ));
    }

    #[test]
    fn formats_official_list_entry_without_exposing_volume_numbers() {
        let image = parse_products_xml(CURRENT_25H2_XML, ImageHashPolicy::RequireSha256).unwrap();
        let system = to_online_system(image);
        assert!(system.display_name.starts_with("Windows 11 25H2 官方原版"));
        assert!(!system.display_name.starts_with("1."));
        assert_eq!(system.sha256.as_deref().map(str::len), Some(64));
    }

    #[test]
    #[ignore = "requires the live Microsoft Update Metadata Service"]
    fn live_catalogue_returns_a_long_lived_25h2_esd() {
        let systems = fetch_current_systems().unwrap();
        let win11 = systems.iter().find(|system| system.is_win11).unwrap();
        assert!(win11.display_name.contains("25H2"));
        assert!(win11
            .download_url
            .starts_with("http://dl.delivery.mp.microsoft.com/filestreamingservice/files/"));
        assert!(!win11.download_url.contains('?'));
        assert_eq!(win11.sha256.as_deref().map(str::len), Some(64));

        let win10 = systems.iter().find(|system| !system.is_win11).unwrap();
        assert!(win10.display_name.contains("22H2"));
        assert!(win10.download_url.contains("CLIENTCONSUMER_RET"));
        assert!(win10.sha256.is_none());
    }
}
