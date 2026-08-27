//! Authentication primitives for cross-reboot handoff files.
//!
//! A public checksum can detect accidental corruption, but cannot prove that a writable data
//! volume still contains the task authorized by the normal endpoint. This module binds the exact
//! raw configuration bytes and session identifier to a secret embedded only in the protected,
//! private PE boot payload. Filesystem publication, key custody and bounded reads remain endpoint
//! responsibilities.

use std::fmt;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, Zeroizing};

pub const SESSION_AUTH_KEY_BYTES: usize = 32;
pub const SESSION_AUTH_TAG_BYTES: usize = 32;
pub const SESSION_AUTH_HEX_CHARS: usize = SESSION_AUTH_TAG_BYTES * 2;
pub const SESSION_ID_BYTES: usize = 16;
pub const SESSION_ID_HEX_CHARS: usize = SESSION_ID_BYTES * 2;
pub const LOCATOR_TOKEN_BYTES: usize = 32;
pub const LOCATOR_TOKEN_HEX_CHARS: usize = LOCATOR_TOKEN_BYTES * 2;
pub const AUTH_MARKER_MAGIC: &str = "LRHA1";
pub const AUTH_MARKER_MAX_BYTES: usize = 4 * 1024;
pub const AUTH_CAPSULE_MAGIC: &str = "LRHC1";
pub const AUTH_CAPSULE_MAX_BYTES: usize = 4 * 1024;
pub const AUTH_CONFIG_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Returns whether a plain file name belongs exclusively to LetRecovery's private PE staging
/// namespace and can therefore be removed as an orphan after all journal-authorized BCD cleanup
/// has completed. Journals are intentionally excluded and must be handled by their transaction
/// parser. Arbitrary names, directories and reparse points never become eligible through this
/// helper.
pub fn is_orphaned_private_pe_file_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if matches!(lower.as_str(), "boot.wim" | "boot.sdi") {
        return true;
    }
    if let Some(token) = lower.strip_prefix("boot-").and_then(|value| {
        value
            .strip_suffix(".wim")
            .or_else(|| value.strip_suffix(".sdi"))
    }) {
        return !token.is_empty()
            && token.len() <= 64
            && token
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-');
    }

    const TEMP_PATTERNS: &[(&str, &str)] = &[
        ("pe-bcd-journal-", ".tmp"),
        ("pe-payload-", ".tmp"),
        ("handoff-capsule-", ".txt"),
        ("handoff-config-", ".ini"),
        ("handoff-manifest-", ".txt"),
        ("handoff-unattend-", ".xml"),
        ("handoff-wifi-", ".xml"),
    ];
    TEMP_PATTERNS.iter().any(|(prefix, suffix)| {
        let Some(token) = lower
            .strip_prefix(prefix)
            .and_then(|value| value.strip_suffix(suffix))
        else {
            return false;
        };
        let Some((process_id, unique_id)) = token.split_once('-') else {
            return false;
        };
        !process_id.is_empty()
            && !unique_id.is_empty()
            && process_id.len() <= 10
            && unique_id.len() <= 20
            && process_id.bytes().all(|byte| byte.is_ascii_digit())
            && unique_id.bytes().all(|byte| byte.is_ascii_digit())
    })
}

const HMAC_BLOCK_BYTES: usize = 64;
const PROTOCOL_LABEL: &[u8] = b"LetRecovery\0cross-reboot-handoff\0HMAC-SHA256\0v1";
const SESSION_FIELD_LABEL: &[u8] = b"session";
const CONFIG_FIELD_LABEL: &[u8] = b"config";

/// A destructive operation domain. Tags from one domain are invalid in every other domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandoffPurpose {
    Install,
    Backup,
    Expand,
    /// Authenticated, non-deployment PE boot used only for interactive maintenance.
    Maintenance,
}

/// Security-document terminology retained as an explicit alias for endpoint APIs.
pub type OperationDomain = HandoffPurpose;

/// Canonical, non-secret cross-reboot session identifier (16 CSPRNG bytes as 32 lowercase hex).
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct SessionId(String);

impl SessionId {
    pub fn parse(value: &str) -> Result<Self> {
        if value.len() != SESSION_ID_HEX_CHARS
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            bail!("session identifier must contain exactly 32 lowercase hexadecimal characters");
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[cfg(windows)]
    pub fn generate() -> Result<Self> {
        let mut bytes = [0_u8; SESSION_ID_BYTES];
        fill_system_random(&mut bytes, "session identifier")?;
        let value = Self(encode_lower_hex(&bytes));
        bytes.zeroize();
        Ok(value)
    }
}

impl fmt::Debug for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("SessionId").field(&self.0).finish()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

pub fn validate_session_id(value: &str) -> Result<SessionId> {
    SessionId::parse(value)
}

#[cfg(windows)]
pub fn generate_session_id() -> Result<SessionId> {
    SessionId::generate()
}

/// A non-secret 256-bit locator used only for exact volume-marker matching after reboot.
///
/// The token is authenticated by the private LRHC1 payload. It deliberately carries no disk,
/// partition, GUID, capacity or layout information: those mutable inventory fields are not part
/// of cross-reboot discovery.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct LocatorToken(String);

impl LocatorToken {
    pub fn parse(value: &str) -> Result<Self> {
        if value.len() != LOCATOR_TOKEN_HEX_CHARS
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            bail!("locator token must contain exactly 64 lowercase hexadecimal characters");
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[cfg(windows)]
    pub fn generate() -> Result<Self> {
        let mut bytes = [0_u8; LOCATOR_TOKEN_BYTES];
        fill_system_random(&mut bytes, "volume locator token")?;
        let value = Self(encode_lower_hex(&bytes));
        bytes.zeroize();
        Ok(value)
    }
}

impl fmt::Debug for LocatorToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("LocatorToken")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for LocatorToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

pub fn validate_locator_token(value: &str) -> Result<LocatorToken> {
    LocatorToken::parse(value)
}

#[cfg(windows)]
pub fn generate_locator_token() -> Result<LocatorToken> {
    LocatorToken::generate()
}

impl HandoffPurpose {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Backup => "backup",
            Self::Expand => "expand",
            Self::Maintenance => "maintenance",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "install" => Ok(Self::Install),
            "backup" => Ok(Self::Backup),
            "expand" => Ok(Self::Expand),
            "maintenance" => Ok(Self::Maintenance),
            _ => bail!("invalid handoff authentication purpose"),
        }
    }

    const fn discriminator(self) -> u8 {
        match self {
            Self::Install => 1,
            Self::Backup => 2,
            Self::Expand => 3,
            Self::Maintenance => 4,
        }
    }
}

/// A per-session secret. Its debug representation and errors never expose key bytes.
///
/// Serialization is deliberately explicit via [`SessionAuthKey::expose_secret_hex`]. Callers may
/// use it only while writing the protected LRPE journal; the returned string must never be logged
/// or copied to the writable handoff volume.
pub struct SessionAuthKey([u8; SESSION_AUTH_KEY_BYTES]);

impl SessionAuthKey {
    pub fn from_bytes(bytes: [u8; SESSION_AUTH_KEY_BYTES]) -> Result<Self> {
        if bytes.ct_eq(&[0; SESSION_AUTH_KEY_BYTES]).into() {
            bail!("handoff authentication key must not be all-zero");
        }
        Ok(Self(bytes))
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        let bytes: [u8; SESSION_AUTH_KEY_BYTES] = bytes
            .try_into()
            .context("handoff authentication key must contain exactly 32 bytes")?;
        Self::from_bytes(bytes)
    }

    /// Parse the sole canonical journal representation: exactly 64 lowercase hexadecimal bytes.
    pub fn from_secret_hex(value: &str) -> Result<Self> {
        Self::from_bytes(decode_lower_hex::<SESSION_AUTH_KEY_BYTES>(
            value,
            "authentication key",
        )?)
    }

    /// Generate a key using the Windows system-preferred CNG random number generator.
    ///
    /// `BCryptGenRandom` with `BCRYPT_USE_SYSTEM_PREFERRED_RNG` is available on Windows 7 and does
    /// not require opening an algorithm provider. Entropy failure is fail-closed.
    #[cfg(windows)]
    pub fn generate() -> Result<Self> {
        let mut bytes = [0_u8; SESSION_AUTH_KEY_BYTES];
        fill_system_random(&mut bytes, "handoff authentication key")?;
        Self::from_bytes(bytes)
    }

    /// Explicit secret exposure for the protected LRPE journal writer only.
    pub(crate) fn expose_secret_hex(&self) -> Zeroizing<String> {
        Zeroizing::new(encode_lower_hex(&self.0))
    }
}

impl fmt::Debug for SessionAuthKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SessionAuthKey([REDACTED])")
    }
}

impl Drop for SessionAuthKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// A non-secret HMAC-SHA256 tag encoded as lowercase hexadecimal in handoff markers.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct SessionAuthTag([u8; SESSION_AUTH_TAG_BYTES]);

impl SessionAuthTag {
    pub fn from_hex(value: &str) -> Result<Self> {
        Ok(Self(decode_lower_hex::<SESSION_AUTH_TAG_BYTES>(
            value,
            "authentication tag",
        )?))
    }

    pub fn to_hex(self) -> String {
        encode_lower_hex(&self.0)
    }
}

impl fmt::Debug for SessionAuthTag {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("SessionAuthTag")
            .field(&self.to_hex())
            .finish()
    }
}

/// Strict public marker fields. The marker carries no secret; authenticity comes only from the
/// tag verified with the key in the protected LRHC1 boot-payload capsule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthenticatedHandoffMarker {
    pub purpose: HandoffPurpose,
    pub session_id: String,
    pub tag: SessionAuthTag,
}

impl AuthenticatedHandoffMarker {
    pub fn new(
        key: &SessionAuthKey,
        purpose: HandoffPurpose,
        session_id: &str,
        config_bytes: &[u8],
    ) -> Result<Self> {
        Ok(Self {
            purpose,
            session_id: session_id.to_owned(),
            tag: authenticate(key, purpose, session_id, config_bytes)?,
        })
    }

    /// Canonical marker encoding. Parsers reject reordered, duplicate, unknown and legacy fields.
    pub fn to_text(&self) -> Result<String> {
        validate_session_id(&self.session_id)?;
        Ok(format!(
            "{AUTH_MARKER_MAGIC}\r\nPurpose={}\r\nSessionId={}\r\nConfigHmacSha256={}\r\n",
            self.purpose.as_str(),
            self.session_id,
            self.tag.to_hex(),
        ))
    }

    pub fn parse(marker_bytes: &[u8]) -> Result<Self> {
        if marker_bytes.len() > AUTH_MARKER_MAX_BYTES {
            bail!("handoff authentication marker exceeds its byte limit");
        }
        let marker = std::str::from_utf8(marker_bytes)
            .context("handoff authentication marker is not UTF-8")?;
        if marker.starts_with('\u{feff}') {
            bail!("handoff authentication marker must not contain a BOM");
        }
        let canonical_newlines = marker.replace("\r\n", "\n");
        if canonical_newlines.contains('\r') {
            bail!("handoff authentication marker has invalid line endings");
        }
        let mut lines = canonical_newlines.lines();
        if lines.next() != Some(AUTH_MARKER_MAGIC) {
            bail!("unsupported or legacy handoff authentication marker");
        }
        let purpose = lines
            .next()
            .and_then(|line| line.strip_prefix("Purpose="))
            .context("handoff authentication marker is missing Purpose")?;
        let session_id = lines
            .next()
            .and_then(|line| line.strip_prefix("SessionId="))
            .context("handoff authentication marker is missing SessionId")?;
        let tag = lines
            .next()
            .and_then(|line| line.strip_prefix("ConfigHmacSha256="))
            .context("handoff authentication marker is missing ConfigHmacSha256")?;
        if lines.next().is_some() {
            bail!("handoff authentication marker has trailing fields");
        }
        validate_session_id(session_id)?;
        Ok(Self {
            purpose: HandoffPurpose::parse(purpose)?,
            session_id: session_id.to_owned(),
            tag: SessionAuthTag::from_hex(tag)?,
        })
    }

    /// Require the caller-selected domain, then authenticate the exact raw config bytes.
    pub fn verify(
        &self,
        key: &SessionAuthKey,
        expected_purpose: HandoffPurpose,
        config_bytes: &[u8],
    ) -> Result<()> {
        if self.purpose != expected_purpose {
            bail!("handoff authentication operation domain does not match");
        }
        verify(
            key,
            expected_purpose,
            &self.session_id,
            config_bytes,
            &self.tag,
        )
    }
}

/// Authoritative authentication capsule embedded in the private, protected PE boot payload.
///
/// Unlike the public LRHA1 marker, this value owns the session key. It must never be written to a
/// data volume. The custom `Debug` implementation deliberately excludes the key and serialized
/// capsule text must never be logged.
pub struct HandoffAuthCapsule {
    purpose: HandoffPurpose,
    session_id: String,
    key: SessionAuthKey,
    config_length: u64,
    config_sha256: [u8; 32],
    config_tag: SessionAuthTag,
}

impl HandoffAuthCapsule {
    pub fn new(
        key: SessionAuthKey,
        purpose: HandoffPurpose,
        session_id: &str,
        config_bytes: &[u8],
    ) -> Result<Self> {
        validate_config_length(config_bytes)?;
        Ok(Self {
            purpose,
            session_id: session_id.to_owned(),
            config_length: config_bytes.len() as u64,
            config_sha256: Sha256::digest(config_bytes).into(),
            config_tag: authenticate(&key, purpose, session_id, config_bytes)?,
            key,
        })
    }

    pub const fn purpose(&self) -> HandoffPurpose {
        self.purpose
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Canonical LRHC1 bytes for the protected boot payload only.
    pub fn to_text(&self) -> Result<Zeroizing<String>> {
        validate_session_id(&self.session_id)?;
        if self.config_length == 0 || self.config_length > AUTH_CONFIG_MAX_BYTES as u64 {
            bail!("authentication capsule has invalid configuration length");
        }
        let key_hex = self.key.expose_secret_hex();
        Ok(Zeroizing::new(format!(
            "{AUTH_CAPSULE_MAGIC}\r\nPurpose={}\r\nSessionId={}\r\nAuthKey={}\r\nConfigLength={}\r\nConfigSha256={}\r\nConfigHmacSha256={}\r\n",
            self.purpose.as_str(),
            self.session_id,
            key_hex.as_str(),
            self.config_length,
            encode_lower_hex(&self.config_sha256),
            self.config_tag.to_hex(),
        )))
    }

    pub fn parse(capsule_bytes: &[u8]) -> Result<Self> {
        if capsule_bytes.len() > AUTH_CAPSULE_MAX_BYTES {
            bail!("handoff authentication capsule exceeds its byte limit");
        }
        let capsule = std::str::from_utf8(capsule_bytes)
            .context("handoff authentication capsule is not UTF-8")?;
        if capsule.starts_with('\u{feff}') {
            bail!("handoff authentication capsule must not contain a BOM");
        }
        let canonical_newlines = Zeroizing::new(capsule.replace("\r\n", "\n"));
        if canonical_newlines.contains('\r') {
            bail!("handoff authentication capsule has invalid line endings");
        }
        let mut lines = canonical_newlines.lines();
        if lines.next() != Some(AUTH_CAPSULE_MAGIC) {
            bail!("unsupported or legacy handoff authentication capsule");
        }
        let purpose = exact_field(lines.next(), "Purpose")?;
        let session_id = exact_field(lines.next(), "SessionId")?;
        let key = exact_field(lines.next(), "AuthKey")?;
        let config_length = exact_field(lines.next(), "ConfigLength")?;
        let config_sha256 = exact_field(lines.next(), "ConfigSha256")?;
        let config_tag = exact_field(lines.next(), "ConfigHmacSha256")?;
        if lines.next().is_some() {
            bail!("handoff authentication capsule has trailing fields");
        }
        validate_session_id(session_id)?;
        if config_length.is_empty()
            || (config_length.len() > 1 && config_length.starts_with('0'))
            || !config_length.bytes().all(|byte| byte.is_ascii_digit())
        {
            bail!("authentication capsule has non-canonical ConfigLength");
        }
        let config_length = config_length
            .parse::<u64>()
            .context("authentication capsule ConfigLength is invalid")?;
        if config_length == 0 || config_length > AUTH_CONFIG_MAX_BYTES as u64 {
            bail!("authentication capsule ConfigLength is outside its limit");
        }
        Ok(Self {
            purpose: HandoffPurpose::parse(purpose)?,
            session_id: session_id.to_owned(),
            key: SessionAuthKey::from_secret_hex(key)?,
            config_length,
            config_sha256: decode_lower_hex::<32>(config_sha256, "configuration SHA-256")?,
            config_tag: SessionAuthTag::from_hex(config_tag)?,
        })
    }

    /// Verify the endpoint-selected domain and exact config bytes. The public SHA-256 is checked
    /// only as corruption diagnostics; HMAC verification remains the authenticity decision.
    pub fn verify_config(
        &self,
        expected_purpose: HandoffPurpose,
        config_bytes: &[u8],
    ) -> Result<()> {
        validate_config_length(config_bytes)?;
        if self.purpose != expected_purpose {
            bail!("authentication capsule operation domain does not match");
        }
        if self.config_length != config_bytes.len() as u64 {
            bail!("authentication capsule configuration length does not match");
        }
        let actual_sha256: [u8; 32] = Sha256::digest(config_bytes).into();
        if actual_sha256.ct_eq(&self.config_sha256).unwrap_u8() != 1 {
            bail!("authentication capsule configuration SHA-256 does not match");
        }
        verify(
            &self.key,
            expected_purpose,
            &self.session_id,
            config_bytes,
            &self.config_tag,
        )
    }

    /// Verify the public LRHA1 locator against this protected capsule and the exact authoritative
    /// configuration bytes. Callers never receive or serialize the session key.
    pub fn verify_public_marker(
        &self,
        marker: &AuthenticatedHandoffMarker,
        config_bytes: &[u8],
    ) -> Result<()> {
        self.verify_config(self.purpose, config_bytes)?;
        if marker.session_id != self.session_id {
            bail!("public handoff marker session does not match authentication capsule");
        }
        marker.verify(&self.key, self.purpose, config_bytes)
    }
}

impl fmt::Debug for HandoffAuthCapsule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HandoffAuthCapsule")
            .field("purpose", &self.purpose)
            .field("session_id", &self.session_id)
            .field("key", &"[REDACTED]")
            .field("config_length", &self.config_length)
            .field("config_sha256", &encode_lower_hex(&self.config_sha256))
            .field("config_tag", &self.config_tag)
            .finish()
    }
}

fn exact_field<'a>(line: Option<&'a str>, name: &str) -> Result<&'a str> {
    line.and_then(|line| line.strip_prefix(&format!("{name}=")))
        .with_context(|| format!("handoff authentication capsule is missing {name}"))
}

fn validate_config_length(config_bytes: &[u8]) -> Result<()> {
    if config_bytes.is_empty() || config_bytes.len() > AUTH_CONFIG_MAX_BYTES {
        bail!("authenticated configuration length is outside its limit");
    }
    Ok(())
}

/// Authenticate the exact raw configuration bytes for one session and operation domain.
pub fn authenticate(
    key: &SessionAuthKey,
    purpose: HandoffPurpose,
    session_id: &str,
    config_bytes: &[u8],
) -> Result<SessionAuthTag> {
    validate_session_id(session_id)?;
    let session_len = u16::try_from(session_id.len()).context("session identifier is too long")?;
    let config_len = u64::try_from(config_bytes.len()).context("configuration is too large")?;

    let mut framed = HmacSha256::new(&key.0);
    framed.update(&(PROTOCOL_LABEL.len() as u16).to_be_bytes());
    framed.update(PROTOCOL_LABEL);
    framed.update(&[purpose.discriminator()]);
    framed.update(&(SESSION_FIELD_LABEL.len() as u16).to_be_bytes());
    framed.update(SESSION_FIELD_LABEL);
    framed.update(&session_len.to_be_bytes());
    framed.update(session_id.as_bytes());
    framed.update(&(CONFIG_FIELD_LABEL.len() as u16).to_be_bytes());
    framed.update(CONFIG_FIELD_LABEL);
    framed.update(&config_len.to_be_bytes());
    framed.update(config_bytes);
    Ok(SessionAuthTag(framed.finalize()))
}

/// Verify a typed tag in constant time after validating the public framing inputs.
pub fn verify(
    key: &SessionAuthKey,
    purpose: HandoffPurpose,
    session_id: &str,
    config_bytes: &[u8],
    supplied: &SessionAuthTag,
) -> Result<()> {
    let expected = authenticate(key, purpose, session_id, config_bytes)?;
    if expected.0.ct_eq(&supplied.0).unwrap_u8() != 1 {
        bail!("handoff authentication failed");
    }
    Ok(())
}

/// Strictly decode a marker tag, then perform the same constant-time fixed-size verification.
pub fn verify_hex_tag(
    key: &SessionAuthKey,
    purpose: HandoffPurpose,
    session_id: &str,
    config_bytes: &[u8],
    supplied_hex: &str,
) -> Result<()> {
    let supplied = SessionAuthTag::from_hex(supplied_hex)?;
    verify(key, purpose, session_id, config_bytes, &supplied)
}

#[cfg(windows)]
fn fill_system_random(bytes: &mut [u8], purpose: &str) -> Result<()> {
    use windows::Win32::Security::Cryptography::{
        BCryptGenRandom, BCRYPT_ALG_HANDLE, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
    };

    let status = unsafe {
        BCryptGenRandom(
            BCRYPT_ALG_HANDLE::default(),
            bytes,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status.is_err() {
        bytes.zeroize();
        bail!("Windows CSPRNG failed while generating {purpose}");
    }
    Ok(())
}

fn encode_lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn decode_lower_hex<const N: usize>(value: &str, field: &str) -> Result<[u8; N]> {
    if value.len() != N * 2
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!(
            "{field} must contain exactly {} lowercase hexadecimal characters",
            N * 2
        );
    }
    let mut output = [0_u8; N];
    for (index, slot) in output.iter_mut().enumerate() {
        let high = hex_nibble(value.as_bytes()[index * 2]);
        let low = hex_nibble(value.as_bytes()[index * 2 + 1]);
        *slot = (high << 4) | low;
    }
    Ok(output)
}

fn hex_nibble(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        _ => unreachable!("hex syntax was validated before decoding"),
    }
}

/// Minimal fixed-key HMAC-SHA256 state. The key size is fixed at the SHA-256 output size, so RFC
/// 2104 key normalization cannot truncate or hash caller-controlled variable-length material.
struct HmacSha256 {
    outer_pad: [u8; HMAC_BLOCK_BYTES],
    inner: Sha256,
}

impl HmacSha256 {
    fn new(key: &[u8]) -> Self {
        let mut normalized_key = [0_u8; HMAC_BLOCK_BYTES];
        if key.len() > HMAC_BLOCK_BYTES {
            let digest = Sha256::digest(key);
            normalized_key[..digest.len()].copy_from_slice(&digest);
        } else {
            normalized_key[..key.len()].copy_from_slice(key);
        }
        let mut inner_pad = [0x36_u8; HMAC_BLOCK_BYTES];
        let mut outer_pad = [0x5c_u8; HMAC_BLOCK_BYTES];
        for (index, byte) in normalized_key.iter().copied().enumerate() {
            inner_pad[index] ^= byte;
            outer_pad[index] ^= byte;
        }
        normalized_key.zeroize();
        let mut inner = Sha256::new();
        inner.update(inner_pad);
        inner_pad.fill(0);
        Self { outer_pad, inner }
    }

    fn update(&mut self, bytes: &[u8]) {
        self.inner.update(bytes);
    }

    fn finalize(mut self) -> [u8; SESSION_AUTH_TAG_BYTES] {
        let inner_digest = self.inner.finalize();
        let mut outer = Sha256::new();
        outer.update(self.outer_pad);
        outer.update(inner_digest);
        self.outer_pad.fill(0);
        outer.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION_A: &str = "00112233445566778899aabbccddeeff";
    const SESSION_B: &str = "102132435465768798a9bacbdcedfe0f";
    const SESSION_C: &str = "ffeeddccbbaa99887766554433221100";

    fn key() -> SessionAuthKey {
        SessionAuthKey::from_bytes([0x42; SESSION_AUTH_KEY_BYTES]).unwrap()
    }

    #[test]
    fn round_trip_and_strict_hex_encoding() {
        let key = key();
        let tag = authenticate(
            &key,
            HandoffPurpose::Install,
            SESSION_A,
            b"[Install]\r\nTarget=C:\r\n",
        )
        .unwrap();
        let encoded = tag.to_hex();
        assert_eq!(encoded.len(), SESSION_AUTH_HEX_CHARS);
        assert_eq!(SessionAuthTag::from_hex(&encoded).unwrap(), tag);
        assert!(SessionAuthTag::from_hex(&encoded.to_ascii_uppercase()).is_err());
        assert!(SessionAuthTag::from_hex(&encoded[..62]).is_err());
        verify_hex_tag(
            &key,
            HandoffPurpose::Install,
            SESSION_A,
            b"[Install]\r\nTarget=C:\r\n",
            &encoded,
        )
        .unwrap();
    }

    #[test]
    fn strict_marker_round_trip_rejects_legacy_unknown_duplicate_and_reordering() {
        let key = key();
        let marker =
            AuthenticatedHandoffMarker::new(&key, HandoffPurpose::Install, SESSION_A, b"config")
                .unwrap();
        let text = marker.to_text().unwrap();
        let parsed = AuthenticatedHandoffMarker::parse(text.as_bytes()).unwrap();
        assert_eq!(parsed, marker);
        parsed
            .verify(&key, HandoffPurpose::Install, b"config")
            .unwrap();

        for invalid in [
            "LetRecovery Install Marker\r\nSessionId=00112233445566778899aabbccddeeff\r\n",
            "LRHA1\r\nSessionId=00112233445566778899aabbccddeeff\r\nPurpose=install\r\nConfigHmacSha256=0000000000000000000000000000000000000000000000000000000000000000\r\n",
            "LRHA1\r\nPurpose=install\r\nSessionId=00112233445566778899aabbccddeeff\r\nSessionId=00112233445566778899aabbccddeeff\r\nConfigHmacSha256=0000000000000000000000000000000000000000000000000000000000000000\r\n",
            "LRHA1\r\nPurpose=install\r\nSessionId=00112233445566778899aabbccddeeff\r\nConfigHmacSha256=0000000000000000000000000000000000000000000000000000000000000000\r\nUnknown=true\r\n",
        ] {
            assert!(AuthenticatedHandoffMarker::parse(invalid.as_bytes()).is_err());
        }
    }

    #[test]
    fn marker_tampering_and_cross_domain_replay_are_rejected() {
        let key = key();
        let marker = AuthenticatedHandoffMarker::new(
            &key,
            HandoffPurpose::Expand,
            SESSION_B,
            b"[Expand]\r\nSize=10\r\n",
        )
        .unwrap();
        assert!(marker
            .verify(&key, HandoffPurpose::Expand, b"[Expand]\r\nSize=11\r\n")
            .is_err());
        assert!(marker
            .verify(&key, HandoffPurpose::Install, b"[Expand]\r\nSize=10\r\n")
            .is_err());
        let tampered = marker.to_text().unwrap().replace(
            &format!("SessionId={SESSION_B}"),
            &format!("SessionId={SESSION_C}"),
        );
        let tampered = AuthenticatedHandoffMarker::parse(tampered.as_bytes()).unwrap();
        assert!(tampered
            .verify(&key, HandoffPurpose::Expand, b"[Expand]\r\nSize=10\r\n")
            .is_err());
    }

    #[test]
    fn protected_capsule_round_trip_authenticates_exact_config() {
        let config = b"[Backup]\r\nSessionId=00112233445566778899aabbccddeeff\r\n";
        let capsule =
            HandoffAuthCapsule::new(key(), HandoffPurpose::Backup, SESSION_A, config).unwrap();
        let text = capsule.to_text().unwrap();
        let parsed = HandoffAuthCapsule::parse(text.as_bytes()).unwrap();
        assert_eq!(parsed.purpose(), HandoffPurpose::Backup);
        assert_eq!(parsed.session_id(), SESSION_A);
        parsed
            .verify_config(HandoffPurpose::Backup, config)
            .unwrap();
        assert!(parsed
            .verify_config(HandoffPurpose::Install, config)
            .is_err());
        assert!(parsed
            .verify_config(
                HandoffPurpose::Backup,
                b"[Backup]\r\nSessionId=ffeeddccbbaa99887766554433221100\r\n"
            )
            .is_err());
        let debug = format!("{parsed:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("42424242424242424242424242424242"));
    }

    #[test]
    fn protected_capsule_verifies_public_locator_without_exposing_key() {
        let config = b"[Install]\r\nSessionId=00112233445566778899aabbccddeeff\r\n";
        let signing_key = key();
        let marker = AuthenticatedHandoffMarker::new(
            &signing_key,
            HandoffPurpose::Install,
            SESSION_A,
            config,
        )
        .unwrap();
        let capsule =
            HandoffAuthCapsule::new(signing_key, HandoffPurpose::Install, SESSION_A, config)
                .unwrap();
        capsule.verify_public_marker(&marker, config).unwrap();
        assert!(capsule
            .verify_public_marker(&marker, b"[Install]\r\nTampered=true\r\n")
            .is_err());
        let wrong_session = AuthenticatedHandoffMarker::new(
            &SessionAuthKey::from_bytes([0x42; SESSION_AUTH_KEY_BYTES]).unwrap(),
            HandoffPurpose::Install,
            SESSION_B,
            config,
        )
        .unwrap();
        assert!(capsule
            .verify_public_marker(&wrong_session, config)
            .is_err());
    }

    #[test]
    fn protected_capsule_rejects_legacy_reordered_duplicate_and_tampered_fields() {
        let text = HandoffAuthCapsule::new(
            key(),
            HandoffPurpose::Expand,
            SESSION_C,
            b"[Expand]\r\nSize=20\r\n",
        )
        .unwrap()
        .to_text()
        .unwrap();
        let length_line = text
            .lines()
            .find(|line| line.starts_with("ConfigLength="))
            .unwrap();
        let key_line = text
            .lines()
            .find(|line| line.starts_with("AuthKey="))
            .unwrap();
        for invalid in [
            text.replacen("LRHC1", "LRHC0", 1),
            text.replace(
                &format!("Purpose=expand\r\nSessionId={SESSION_C}"),
                &format!("SessionId={SESSION_C}\r\nPurpose=expand"),
            ),
            text.replace(length_line, "ConfigLength=0001"),
            text.replace(key_line, &format!("{key_line}\r\n{key_line}")),
            format!("{}Unknown=true\r\n", text.as_str()),
            text.replace("Purpose=expand", "Purpose=backup"),
        ] {
            match HandoffAuthCapsule::parse(invalid.as_bytes()) {
                Err(_) => {}
                Ok(capsule) => assert!(capsule
                    .verify_config(HandoffPurpose::Expand, b"[Expand]\r\nSize=20\r\n")
                    .is_err()),
            }
        }
    }

    #[test]
    fn every_operation_domain_is_independent() {
        let key = key();
        let session = SESSION_A;
        let config = b"same exact bytes";
        let install = authenticate(&key, HandoffPurpose::Install, session, config).unwrap();
        let backup = authenticate(&key, HandoffPurpose::Backup, session, config).unwrap();
        let expand = authenticate(&key, HandoffPurpose::Expand, session, config).unwrap();
        let maintenance = authenticate(&key, HandoffPurpose::Maintenance, session, config).unwrap();
        assert_ne!(install, backup);
        assert_ne!(install, expand);
        assert_ne!(install, maintenance);
        assert_ne!(backup, expand);
        assert_ne!(backup, maintenance);
        assert_ne!(expand, maintenance);
        assert!(verify(&key, HandoffPurpose::Backup, session, config, &install).is_err());
        assert!(verify(&key, HandoffPurpose::Expand, session, config, &backup).is_err());
        assert!(verify(&key, HandoffPurpose::Install, session, config, &expand).is_err());
        assert!(verify(&key, HandoffPurpose::Maintenance, session, config, &install).is_err());
    }

    #[test]
    fn session_and_exact_config_bytes_are_independently_framed() {
        let key = key();
        let original = authenticate(&key, HandoffPurpose::Backup, SESSION_A, b"c").unwrap();
        assert_ne!(
            original,
            authenticate(&key, HandoffPurpose::Backup, SESSION_B, b"c").unwrap()
        );
        assert!(verify(&key, HandoffPurpose::Backup, SESSION_A, b"c\r\n", &original).is_err());
        assert!(verify(&key, HandoffPurpose::Backup, SESSION_B, b"c", &original).is_err());
    }

    #[test]
    fn different_key_and_tampered_tag_fail() {
        let key = key();
        let other = SessionAuthKey::from_bytes([0x24; SESSION_AUTH_KEY_BYTES]).unwrap();
        let tag = authenticate(&key, HandoffPurpose::Expand, SESSION_C, b"payload").unwrap();
        assert!(verify(&other, HandoffPurpose::Expand, SESSION_C, b"payload", &tag).is_err());
        let mut encoded = tag.to_hex().into_bytes();
        encoded[63] = if encoded[63] == b'0' { b'1' } else { b'0' };
        assert!(verify_hex_tag(
            &key,
            HandoffPurpose::Expand,
            SESSION_C,
            b"payload",
            std::str::from_utf8(&encoded).unwrap()
        )
        .is_err());
    }

    #[test]
    fn key_debug_and_validation_do_not_disclose_secret() {
        let secret = [0xa5; SESSION_AUTH_KEY_BYTES];
        let key = SessionAuthKey::from_bytes(secret).unwrap();
        let debug = format!("{key:?}");
        assert_eq!(debug, "SessionAuthKey([REDACTED])");
        assert!(!debug.contains(&encode_lower_hex(&secret)));
        assert!(SessionAuthKey::from_bytes([0; SESSION_AUTH_KEY_BYTES]).is_err());
        let hex = key.expose_secret_hex();
        let reparsed = SessionAuthKey::from_secret_hex(&hex).unwrap();
        assert_eq!(
            authenticate(&key, HandoffPurpose::Install, SESSION_A, b"config").unwrap(),
            authenticate(&reparsed, HandoffPurpose::Install, SESSION_A, b"config").unwrap()
        );
        assert!(SessionAuthKey::from_secret_hex(&hex.to_ascii_uppercase()).is_err());
    }

    #[test]
    fn rejects_noncanonical_session_identifiers() {
        let key = key();
        for session in [
            "",
            "00112233445566778899aabbccddeef",
            "00112233445566778899aabbccddeeff0",
            "00112233445566778899AABBCCDDEEFF",
            "00112233445566778899aabbccddee-g",
        ] {
            assert!(authenticate(&key, HandoffPurpose::Install, session, b"config").is_err());
        }
    }

    #[cfg(windows)]
    #[test]
    fn generated_session_ids_are_canonical_and_fresh() {
        let first = generate_session_id().unwrap();
        let second = generate_session_id().unwrap();
        assert_eq!(first.as_str().len(), SESSION_ID_HEX_CHARS);
        assert_eq!(validate_session_id(first.as_str()).unwrap(), first);
        assert_eq!(validate_session_id(second.as_str()).unwrap(), second);
        assert_ne!(first, second);
    }

    #[test]
    fn locator_tokens_have_one_strict_canonical_form() {
        let token = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(validate_locator_token(token).unwrap().as_str(), token);
        for invalid in [
            "",
            "0123456789abcdef0123456789abcdef",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0",
            "0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF",
        ] {
            assert!(
                validate_locator_token(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn generated_locator_tokens_are_independent_and_fresh() {
        let data = generate_locator_token().unwrap();
        let target = generate_locator_token().unwrap();
        assert_eq!(data.as_str().len(), LOCATOR_TOKEN_HEX_CHARS);
        assert_ne!(data, target);
    }

    #[test]
    fn hmac_sha256_matches_rfc4231_known_answer() {
        // RFC 4231 test case 1. This exercises the primitive independently of our framing and
        // round-trip tests, so a matching bug in sign and verify cannot make the test pass.
        let mut hmac = HmacSha256::new(&[0x0b; 20]);
        hmac.update(b"Hi There");
        assert_eq!(
            encode_lower_hex(&hmac.finalize()),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }
}
