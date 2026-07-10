use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::OnceLock;

use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};

use super::checkpoint::{write_atomic, OperationCheckpoint};
use super::{OperationError, OperationErrorKind, OperationKind, OperationStatus, StepStatus};

const DEFAULT_ATTACHMENT_LIMIT: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportOperationStep {
    pub id: String,
    pub name: String,
    pub status: StepStatus,
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<OperationErrorKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportOperationSummary {
    pub operation_id: String,
    pub kind: OperationKind,
    pub status: OperationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_step: Option<String>,
    pub revision: u64,
    pub created_unix_ms: u64,
    pub updated_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<OperationErrorKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub steps: Vec<SupportOperationStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportAttachment {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_name: Option<String>,
    pub truncated: bool,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportBundle {
    pub schema_version: u32,
    pub generated_unix_ms: u64,
    pub application: String,
    pub application_version: String,
    pub endpoint: String,
    pub environment: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<SupportOperationSummary>,
    pub attachments: Vec<SupportAttachment>,
}

impl SupportBundle {
    pub fn write_json(&self, path: &Path) -> Result<(), OperationError> {
        let data = serde_json::to_vec_pretty(self).map_err(|error| {
            OperationError::new(
                OperationErrorKind::State,
                None::<String>,
                format!("serialize support bundle: {error}"),
                false,
            )
        })?;
        write_atomic(path, &data)
            .map_err(|error| OperationError::io("write support bundle", &error))
    }
}

pub struct SupportBundleBuilder {
    bundle: SupportBundle,
    explicit_secrets: Vec<String>,
    attachment_limit: usize,
}

impl SupportBundleBuilder {
    pub fn new(
        application: impl Into<String>,
        application_version: impl Into<String>,
        endpoint: impl Into<String>,
        generated_unix_ms: u64,
    ) -> Result<Self, OperationError> {
        let application = application.into();
        let application_version = application_version.into();
        let endpoint = endpoint.into();
        validate_label(&application, "application")?;
        validate_label(&application_version, "application version")?;
        validate_label(&endpoint, "endpoint")?;
        Ok(Self {
            bundle: SupportBundle {
                schema_version: 1,
                generated_unix_ms,
                application,
                application_version,
                endpoint,
                environment: BTreeMap::new(),
                operation: None,
                attachments: Vec::new(),
            },
            explicit_secrets: Vec::new(),
            attachment_limit: DEFAULT_ATTACHMENT_LIMIT,
        })
    }

    pub fn attachment_limit(mut self, limit: usize) -> Result<Self, OperationError> {
        if limit == 0 || limit > 4 * 1024 * 1024 {
            return Err(OperationError::validation(
                "support attachment limit must be between 1 byte and 4 MiB",
            ));
        }
        self.attachment_limit = limit;
        Ok(self)
    }

    pub fn redact_value(&mut self, value: impl Into<String>) {
        let value = value.into();
        if value.len() >= 4 && !self.explicit_secrets.contains(&value) {
            self.explicit_secrets.push(value);
            self.explicit_secrets
                .sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        }
    }

    pub fn add_environment(
        &mut self,
        key: impl Into<String>,
        value: impl AsRef<str>,
    ) -> Result<(), OperationError> {
        let key = key.into();
        validate_key(&key)?;
        let value = self.redact(value.as_ref());
        self.bundle.environment.insert(key, value);
        Ok(())
    }

    pub fn set_operation(&mut self, checkpoint: &OperationCheckpoint) {
        let target = checkpoint.target.as_ref();
        self.bundle.operation = Some(SupportOperationSummary {
            operation_id: checkpoint.operation_id.clone(),
            kind: checkpoint.kind,
            status: checkpoint.status,
            current_step: checkpoint.current_step.clone(),
            revision: checkpoint.revision,
            created_unix_ms: checkpoint.created_unix_ms,
            updated_unix_ms: checkpoint.updated_unix_ms,
            disk_number: target.and_then(|value| value.disk_number),
            partition_number: target.and_then(|value| value.partition_number),
            disk_size_bytes: target.and_then(|value| value.disk_size_bytes),
            partition_size_bytes: target.and_then(|value| value.partition_size_bytes),
            error_kind: checkpoint.last_error.as_ref().map(|error| error.kind),
            error_message: checkpoint
                .last_error
                .as_ref()
                .map(|error| self.redact(&error.message)),
            steps: checkpoint
                .steps
                .iter()
                .map(|step| SupportOperationStep {
                    id: step.id.clone(),
                    name: step.name.clone(),
                    status: step.status,
                    attempts: step.attempts,
                    error_kind: step.last_error.as_ref().map(|error| error.kind),
                    error_message: step
                        .last_error
                        .as_ref()
                        .map(|error| self.redact(&error.message)),
                })
                .collect(),
        });
    }

    pub fn add_text(
        &mut self,
        label: impl Into<String>,
        source_name: Option<String>,
        content: &str,
    ) -> Result<(), OperationError> {
        let label = label.into();
        validate_label(&label, "attachment label")?;
        let (content, truncated) = tail_text(content.as_bytes(), self.attachment_limit);
        self.bundle.attachments.push(SupportAttachment {
            label,
            source_name: source_name.map(|name| safe_source_name(Path::new(&name))),
            truncated,
            content: self.redact(&content),
        });
        Ok(())
    }

    pub fn add_text_file(
        &mut self,
        label: impl Into<String>,
        path: &Path,
    ) -> Result<(), OperationError> {
        let metadata = fs::metadata(path)
            .map_err(|error| OperationError::io("read support attachment metadata", &error))?;
        if !metadata.is_file() {
            return Err(OperationError::validation(
                "support attachment is not a regular file",
            ));
        }
        let bytes = read_tail(path, self.attachment_limit)
            .map_err(|error| OperationError::io("read support attachment", &error))?;
        let content = String::from_utf8_lossy(&bytes.data);
        self.add_attachment(
            label.into(),
            Some(safe_source_name(path)),
            self.redact(&content),
            bytes.truncated,
        )
    }

    pub fn build(self) -> SupportBundle {
        self.bundle
    }

    fn add_attachment(
        &mut self,
        label: String,
        source_name: Option<String>,
        content: String,
        truncated: bool,
    ) -> Result<(), OperationError> {
        validate_label(&label, "attachment label")?;
        self.bundle.attachments.push(SupportAttachment {
            label,
            source_name,
            truncated,
            content,
        });
        Ok(())
    }

    fn redact(&self, input: &str) -> String {
        let mut output = input.to_string();
        for secret in &self.explicit_secrets {
            output = output.replace(secret, "[REDACTED]");
        }
        output = credential_regex()
            .replace_all(&output, |captures: &Captures<'_>| {
                format!("{}[REDACTED]", &captures[1])
            })
            .into_owned();
        recovery_key_regex()
            .replace_all(&output, "[BITLOCKER-RECOVERY-KEY-REDACTED]")
            .into_owned()
    }
}

struct TailBytes {
    data: Vec<u8>,
    truncated: bool,
}

fn read_tail(path: &Path, limit: usize) -> io::Result<TailBytes> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = fs::File::open(path)?;
    let length = file.metadata()?.len();
    let truncated = length > limit as u64;
    let read_limit = limit.saturating_add(8 * 1024);
    if length > read_limit as u64 {
        file.seek(SeekFrom::End(-(read_limit as i64)))?;
    }
    let mut data = Vec::with_capacity(length.min(read_limit as u64) as usize);
    file.read_to_end(&mut data)?;
    if truncated {
        data = complete_line_tail(&data, limit);
    }
    Ok(TailBytes { data, truncated })
}

fn tail_text(bytes: &[u8], limit: usize) -> (String, bool) {
    if bytes.len() <= limit {
        return (String::from_utf8_lossy(bytes).into_owned(), false);
    }
    let tail = complete_line_tail(bytes, limit);
    (String::from_utf8_lossy(&tail).into_owned(), true)
}

fn complete_line_tail(bytes: &[u8], limit: usize) -> Vec<u8> {
    let start = bytes.len().saturating_sub(limit);
    if start == 0 {
        return bytes.to_vec();
    }
    let Some(relative_newline) = bytes[start..].iter().position(|byte| *byte == b'\n') else {
        return b"[TRUNCATED OVERLONG LINE OMITTED]".to_vec();
    };
    bytes[start + relative_newline + 1..].to_vec()
}

fn safe_source_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("attachment.txt")
        .to_string()
}

fn validate_key(value: &str) -> Result<(), OperationError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(OperationError::validation(
            "support environment key is empty or contains unsupported characters",
        ));
    }
    Ok(())
}

fn validate_label(value: &str, field: &str) -> Result<(), OperationError> {
    if value.trim().is_empty() || value.len() > 256 || value.contains(['\r', '\n', '\0']) {
        return Err(OperationError::validation(format!(
            "{field} is empty, too long, or contains a control character"
        )));
    }
    Ok(())
}

fn credential_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r#"(?i)((?:authorization|password|passwd|token|secret|recovery[_-]?key)\s*[:=]\s*(?:bearer\s+)?)(?:"[^"]*"|'[^']*'|[^\s,;]+)"#,
        )
        .expect("credential redaction regex is valid")
    })
}

fn recovery_key_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"\b(?:\d{6}-){7}\d{6}\b").expect("recovery-key redaction regex is valid")
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::operation::{OperationKind, StepDefinition};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    fn temp_directory() -> PathBuf {
        let id = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("lr-support-bundle-{}-{id}", std::process::id()))
    }

    #[test]
    fn redacts_credentials_recovery_keys_and_explicit_values() {
        let mut builder = SupportBundleBuilder::new("LetRecovery", "1", "PE", 10).unwrap();
        builder.redact_value("private-user-value");
        builder
            .add_text(
                "log",
                None,
                "password=hunter2 token: abc Authorization=Bearer xyz\n\
                 111111-222222-333333-444444-555555-666666-777777-888888\n\
                 private-user-value",
            )
            .unwrap();
        let content = &builder.build().attachments[0].content;
        assert!(!content.contains("hunter2"));
        assert!(!content.contains("abc"));
        assert!(!content.contains("xyz"));
        assert!(!content.contains("111111-222222"));
        assert!(!content.contains("private-user-value"));
        assert!(content.contains("[REDACTED]"));
    }

    #[test]
    fn file_attachment_keeps_only_name_and_truncated_tail() {
        let directory = temp_directory();
        fs::create_dir(&directory).unwrap();
        let path = directory.join("LetRecovery PE.log");
        fs::write(&path, "prefix-should-be-cut-secret=gone-TAIL").unwrap();

        let mut builder = SupportBundleBuilder::new("LetRecovery", "1", "PE", 10)
            .unwrap()
            .attachment_limit(12)
            .unwrap();
        builder.add_text_file("runtime log", &path).unwrap();
        let attachment = &builder.build().attachments[0];
        assert_eq!(
            attachment.source_name.as_deref(),
            Some("LetRecovery PE.log")
        );
        assert!(attachment.truncated);
        assert_eq!(attachment.content, "[TRUNCATED OVERLONG LINE OMITTED]");
        assert!(!attachment.content.contains("gone"));
        assert!(!attachment
            .content
            .contains(&directory.to_string_lossy().to_string()));

        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn writes_self_contained_json_with_sanitized_operation_summary() {
        let directory = temp_directory();
        let destination = directory.join("support.json");
        let mut checkpoint = OperationCheckpoint::new(
            "pe-install",
            OperationKind::Install,
            [StepDefinition::new("verify", "Verify", true)],
            1,
        )
        .unwrap();
        checkpoint.begin_step("verify", 2).unwrap();
        checkpoint
            .fail_step(
                "verify",
                OperationError::verification("password=do-not-export"),
                3,
            )
            .unwrap();

        let mut builder = SupportBundleBuilder::new("LetRecovery", "1", "PE", 4).unwrap();
        builder.set_operation(&checkpoint);
        let bundle = builder.build();
        bundle.write_json(&destination).unwrap();

        let parsed: SupportBundle =
            serde_json::from_slice(&fs::read(&destination).unwrap()).unwrap();
        assert_eq!(parsed.operation.unwrap().status, OperationStatus::Failed);
        assert!(!fs::read_to_string(&destination)
            .unwrap()
            .contains("do-not-export"));

        fs::remove_file(destination).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
