//! Automatic diagnostic feedback shared by the desktop and PE clients.
//!
//! 自动错误报告只包含排查需要的内容：最后一次运行的 `[诊断环境]` 行、导致失败的 ERROR 行和
//! 失败前少量 WARN 行，总量有上限。整份日志不再上传；用户名、密码、恢复密钥、令牌和邮箱在
//! 客户端就被移除。服务端按错误签名去重，同一问题只累加次数。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

pub const PROTOCOL: u16 = 2;
pub const MAX_LINE_BYTES: usize = 1500;
pub const MAX_ERRORS: usize = 12;
pub const MAX_WARNINGS: usize = 10;
pub const MAX_ENVIRONMENT: usize = 40;
/// 失败前的上下文行数和总字节上限：足够结合前后步骤判断原因，又不会把整份日志传上去。
pub const MAX_CONTEXT: usize = 80;
pub const MAX_CONTEXT_BYTES: usize = 48 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeedbackReport {
    pub protocol: u16,
    pub client_version: String,
    pub stage: String,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub environment: Vec<String>,
    /// 失败前的最近日志（含 INFO），按时间顺序；不含环境行和反馈自身的日志。
    #[serde(default)]
    pub context: Vec<String>,
    /// 客户端本地去重用；服务端会按自己的规则重新计算签名，不信任这个值。
    pub fingerprint: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Error,
    Warn,
    Other,
}

fn prefix(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// tracing 文件格式：`2026-10-29T01:17:12.837854Z ERROR target: file.rs:12: message`。
fn level(line: &str) -> Level {
    if !line.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        return Level::Other;
    }
    let head = prefix(line, 40);
    if head.contains("Z ERROR ") {
        Level::Error
    } else if head.contains("Z  WARN ") || head.contains("Z WARN ") {
        Level::Warn
    } else {
        Level::Other
    }
}

fn pattern(cell: &'static OnceLock<regex::Regex>, source: &str) -> &'static regex::Regex {
    cell.get_or_init(|| regex::Regex::new(source).expect("static feedback pattern"))
}

/// 去掉日志行里的个人信息与凭据，并限制长度；整行都可能是密钥材料时返回 `None`。
pub fn sanitize_line(line: &str) -> Option<String> {
    static PROFILE: OnceLock<regex::Regex> = OnceLock::new();
    static RECOVERY_KEY: OnceLock<regex::Regex> = OnceLock::new();
    static ASSIGNMENT: OnceLock<regex::Regex> = OnceLock::new();
    static BEARER: OnceLock<regex::Regex> = OnceLock::new();
    static EMAIL: OnceLock<regex::Regex> = OnceLock::new();
    if line.to_ascii_uppercase().contains("PRIVATE KEY") {
        return None;
    }
    // 旧实现只把 C:\Users\ 换成 %USERPROFILE%\，用户名仍留在后面；这里连同用户名一起替换。
    let text = pattern(
        &PROFILE,
        r#"(?i)(?:\\\\\?\\)?[a-z]:\\(?:users|documents and settings)\\[^\\\s"'<>|]+"#,
    )
    .replace_all(line, "%USERPROFILE%");
    let text = pattern(&RECOVERY_KEY, r"\b\d{6}(?:-\d{6}){7}\b").replace_all(&text, "[恢复密钥]");
    let text = pattern(
        &ASSIGNMENT,
        r"(?i)(password|passwd|pwd|token|secret|api_?key|密码)(\s*[:=：]\s*)\S+",
    )
    .replace_all(&text, "$1$2[已隐去]");
    let text = pattern(&BEARER, r"(?i)bearer\s+\S+").replace_all(&text, "Bearer [已隐去]");
    let text = pattern(&EMAIL, r"(?i)[a-z0-9._%+\-]+@[a-z0-9.\-]+\.[a-z]{2,}")
        .replace_all(&text, "[邮箱]");
    let mut out = prefix(&text, MAX_LINE_BYTES).to_string();
    if out.len() < text.len() {
        out.push('…');
    }
    Some(out)
}

/// 跨版本、跨机器稳定的错误签名：去掉时间戳、源码行号、GUID、长数字和盘符后取哈希。
pub fn fingerprint(line: &str) -> String {
    static LOCATION: OnceLock<regex::Regex> = OnceLock::new();
    static GUID: OnceLock<regex::Regex> = OnceLock::new();
    static HEX: OnceLock<regex::Regex> = OnceLock::new();
    static NUMBER: OnceLock<regex::Regex> = OnceLock::new();
    static DRIVE: OnceLock<regex::Regex> = OnceLock::new();
    let text = pattern(
        &LOCATION,
        r"^\S+Z\s+(?:ERROR|WARN)\s+\S+:\s+\S+\.rs:\d+:\s*",
    )
    .replace(line, "");
    let text = pattern(
        &GUID,
        r"\{?[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}\}?",
    )
    .replace_all(&text, "{GUID}");
    let text = pattern(&HEX, r"\b[0-9A-Fa-f]{16,}\b").replace_all(&text, "<hex>");
    let text = pattern(&NUMBER, r"\d{5,}").replace_all(&text, "<n>");
    let text = pattern(&DRIVE, r"\b[A-Za-z]:\\").replace_all(&text, "?:\\");
    let digest = Sha256::digest(text.trim().as_bytes());
    digest
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn keep_last(lines: &mut Vec<String>, limit: usize) {
    if lines.len() > limit {
        lines.drain(..lines.len() - limit);
    }
}

/// 从（按天滚动、可能包含多次运行的）日志里提取最后一次运行的失败摘要。没有 ERROR 行时返回 `None`。
pub fn build_report(log: &str, client_version: &str, stage: &str) -> Option<FeedbackReport> {
    let lines: Vec<&str> = log.lines().collect();
    let start = lines
        .iter()
        .rposition(|line| {
            line.contains("LetRecovery 启动中") || line.contains("[诊断环境] 软件版本")
        })
        .unwrap_or(0);
    let session = &lines[start..];
    let terminal = session
        .iter()
        .rposition(|line| level(line) == Level::Error)?;
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut environment = Vec::new();
    let mut context = Vec::new();
    for (index, line) in session.iter().enumerate() {
        if line.contains("[诊断环境]") {
            if environment.len() < MAX_ENVIRONMENT {
                environment.extend(sanitize_line(line));
            }
            continue;
        }
        if index > terminal {
            continue;
        }
        if index < terminal && !line.contains("[FEEDBACK]") && !line.trim().is_empty() {
            context.extend(sanitize_line(line));
        }
        match level(line) {
            Level::Error => errors.extend(sanitize_line(line)),
            Level::Warn if !line.contains("[FEEDBACK]") => warnings.extend(sanitize_line(line)),
            _ => {}
        }
    }
    keep_last(&mut errors, MAX_ERRORS);
    keep_last(&mut warnings, MAX_WARNINGS);
    keep_last(&mut context, MAX_CONTEXT);
    while context.iter().map(String::len).sum::<usize>() > MAX_CONTEXT_BYTES && !context.is_empty()
    {
        context.remove(0);
    }
    let fingerprint = fingerprint(errors.last()?);
    Some(FeedbackReport {
        protocol: PROTOCOL,
        client_version: client_version.into(),
        stage: stage.into(),
        errors,
        warnings,
        environment,
        context,
        fingerprint,
    })
}

/// 本次运行最后一条错误的正文（去掉时间、模块和源码位置），用于手动反馈弹窗的“出错原因”。
pub fn failure_summary(log: &str) -> Option<String> {
    static LOCATION: OnceLock<regex::Regex> = OnceLock::new();
    let report = build_report(log, "", "")?;
    let line = report.errors.last()?;
    let text = pattern(&LOCATION, r"^\S+Z\s+ERROR\s+\S+:\s+(?:\S+\.rs:\d+:\s*)?").replace(line, "");
    let text = text.trim();
    let mut out = prefix(text, 600).to_string();
    if out.len() < text.len() {
        out.push('…');
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "2026-10-28T01:00:00.000000Z  INFO LetRecovery: main.rs:1: LetRecovery 启动中...\n\
2026-10-28T01:00:01.000000Z ERROR LetRecovery::x: x.rs:9: an older run failed\n\
2026-10-29T01:17:10.000000Z  INFO LetRecovery: main.rs:1: LetRecovery 启动中...\n\
2026-10-29T01:17:10.100000Z  INFO LetRecovery: main.rs:2: [诊断环境] 镜像: file=C:\\Users\\alice\\Downloads\\win.iso\n\
2026-10-29T01:17:11.000000Z  INFO LetRecovery: main.rs:3: unlock with password=hunter2\n\
2026-10-29T01:17:12.000000Z  WARN LetRecovery::p: p.rs:21: [BOOT PCA] 读取 SecureBoot 失败\n\
2026-10-29T01:17:12.500000Z ERROR LetRecovery::w: window.rs:12131: phase X failed: key 111111-222222-333333-444444-555555-666666-777777-888888\n\
2026-10-29T01:17:13.000000Z  WARN LetRecovery::w: window.rs:13759: [FEEDBACK] previous upload failed\n";

    #[test]
    fn report_keeps_only_the_last_run_and_strips_personal_data() {
        let report = build_report(LOG, "v2026.10.29", "normal").unwrap();
        assert_eq!(report.protocol, PROTOCOL);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("[恢复密钥]"));
        assert!(!report.errors[0].contains("111111"));
        assert_eq!(report.warnings.len(), 1);
        assert!(!report.warnings[0].contains("[FEEDBACK]"));
        assert_eq!(report.environment.len(), 1);
        assert!(report.environment[0].contains("%USERPROFILE%\\Downloads"));
        assert!(!report.environment[0].contains("alice"));
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("hunter2"));
        assert!(!json.contains("older run"));
    }

    #[test]
    fn fingerprint_ignores_line_numbers_and_drive_letters() {
        assert_eq!(
            fingerprint("2026-10-29T01:00:00Z ERROR LetRecovery::w: window.rs:12131: canonicalize D:\\a.wim"),
            fingerprint("2026-10-30T02:00:00Z ERROR LetRecovery::w: window.rs:12125: canonicalize E:\\a.wim")
        );
    }

    #[test]
    fn logs_without_errors_produce_no_report() {
        assert!(
            build_report("2026-10-29T01:00:00Z  INFO a: b.rs:1: ok\n", "v1", "normal").is_none()
        );
    }
}
