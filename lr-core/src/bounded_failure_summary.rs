//! Bounded, single-line summaries for collections of diagnostic failures.
//!
//! This module deliberately does not decide whether an operation succeeds or fails. It only
//! prevents a large package set or an attacker-controlled path/error from turning one failure
//! collection into unbounded log output.

use std::fmt::{Display, Formatter};

/// No caller may place more than this many individual failure samples in one summary.
pub const HARD_MAX_FAILURE_SAMPLES: usize = 8;

/// Each sanitized sample is bounded independently in UTF-8 bytes.
pub const MAX_FAILURE_SAMPLE_BYTES: usize = 256;

const TRUNCATION_MARKER: &str = "…";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundedFailureSummary {
    total: usize,
    samples: Vec<String>,
    remaining: usize,
}

/// Incrementally counts an unbounded failure stream while retaining only a bounded diagnostic
/// prefix. Use this when formatting every failure would itself create excessive allocations.
pub struct BoundedFailureCollector {
    sample_limit: usize,
    total: usize,
    samples: Vec<String>,
}

impl BoundedFailureCollector {
    pub fn new(requested_samples: usize) -> Self {
        let sample_limit = requested_samples.min(HARD_MAX_FAILURE_SAMPLES);
        Self {
            sample_limit,
            total: 0,
            samples: Vec::with_capacity(sample_limit),
        }
    }

    pub fn push(&mut self, failure: impl Display) {
        self.total = self.total.saturating_add(1);
        if self.samples.len() < self.sample_limit {
            self.samples.push(sanitize_sample(&failure.to_string()));
        }
    }

    pub fn finish(self) -> BoundedFailureSummary {
        BoundedFailureSummary {
            total: self.total,
            remaining: self.total.saturating_sub(self.samples.len()),
            samples: self.samples,
        }
    }
}

impl BoundedFailureSummary {
    pub fn total(&self) -> usize {
        self.total
    }

    pub fn samples(&self) -> &[String] {
        &self.samples
    }

    pub fn remaining(&self) -> usize {
        self.remaining
    }

    pub fn is_empty(&self) -> bool {
        self.total == 0
    }
}

impl Display for BoundedFailureSummary {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "total={} sampled={} remaining={}",
            self.total,
            self.samples.len(),
            self.remaining
        )?;
        for (index, sample) in self.samples.iter().enumerate() {
            // Debug string formatting quotes delimiters and backslashes. All line/control
            // characters were already replaced, so this remains one physical log event.
            write!(formatter, " sample[{index}]={sample:?}")?;
        }
        Ok(())
    }
}

/// Counts every failure but formats only a small, hard-bounded prefix as diagnostic samples.
///
/// `requested_samples` is clamped to [`HARD_MAX_FAILURE_SAMPLES`]. The iterator is still fully
/// consumed so `total` and `remaining` are truthful, while entries beyond the sample limit are
/// never formatted and therefore cannot inflate allocations or leak into logs.
pub fn summarize_failures<I, T>(failures: I, requested_samples: usize) -> BoundedFailureSummary
where
    I: IntoIterator<Item = T>,
    T: Display,
{
    summarize_failures_by(failures, requested_samples, |failure| failure.to_string())
}

/// Variant of [`summarize_failures`] for structured failures.
///
/// The rendering callback is invoked only for retained samples, not for every counted entry.
pub fn summarize_failures_by<I, T, F>(
    failures: I,
    requested_samples: usize,
    mut render_sample: F,
) -> BoundedFailureSummary
where
    I: IntoIterator<Item = T>,
    F: FnMut(&T) -> String,
{
    let sample_limit = requested_samples.min(HARD_MAX_FAILURE_SAMPLES);
    let mut total = 0usize;
    let mut samples = Vec::with_capacity(sample_limit);

    for failure in failures {
        total = total.saturating_add(1);
        if samples.len() < sample_limit {
            samples.push(sanitize_sample(&render_sample(&failure)));
        }
    }

    BoundedFailureSummary {
        total,
        remaining: total.saturating_sub(samples.len()),
        samples,
    }
}

fn sanitize_sample(input: &str) -> String {
    let mut output = String::with_capacity(input.len().min(MAX_FAILURE_SAMPLE_BYTES));
    let mut truncated = false;

    for character in input.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if output.len() + character.len_utf8() > MAX_FAILURE_SAMPLE_BYTES {
            truncated = true;
            break;
        }
        output.push(character);
    }

    if truncated {
        let maximum_prefix = MAX_FAILURE_SAMPLE_BYTES - TRUNCATION_MARKER.len();
        let mut boundary = output.len().min(maximum_prefix);
        while !output.is_char_boundary(boundary) {
            boundary -= 1;
        }
        output.truncate(boundary);
        output.push_str(TRUNCATION_MARKER);
    }

    if output.is_empty() {
        output.push_str("<empty>");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt;

    #[test]
    fn large_failure_set_reports_only_bounded_samples_and_truthful_remaining_count() {
        let summary = summarize_failures((0..100).map(|index| format!("failure-{index}")), 3);

        assert_eq!(summary.total(), 100);
        assert_eq!(summary.samples(), ["failure-0", "failure-1", "failure-2"]);
        assert_eq!(summary.remaining(), 97);
        let rendered = summary.to_string();
        assert!(rendered.contains("total=100 sampled=3 remaining=97"));
        assert!(!rendered.contains("failure-3"));
        assert!(!rendered.contains('\n'));
    }

    #[test]
    fn caller_cannot_raise_sample_count_above_the_hard_log_limit() {
        let summary = summarize_failures(0..20, usize::MAX);

        assert_eq!(summary.samples().len(), HARD_MAX_FAILURE_SAMPLES);
        assert_eq!(summary.remaining(), 20 - HARD_MAX_FAILURE_SAMPLES);
    }

    #[test]
    fn overlong_path_and_error_tail_never_reach_the_summary() {
        let overlong = format!(
            "C:\\\\drivers\\{}SECRET_ERROR_TAIL\r\nforged-log-line\0",
            "nested\\".repeat(80)
        );
        let summary = summarize_failures(std::iter::once(overlong), 1);
        let sample = &summary.samples()[0];
        let rendered = summary.to_string();

        assert!(sample.len() <= MAX_FAILURE_SAMPLE_BYTES);
        assert!(sample.ends_with(TRUNCATION_MARKER));
        assert!(!sample.contains("SECRET_ERROR_TAIL"));
        assert!(!rendered.contains("forged-log-line"));
        assert!(!rendered.contains('\r'));
        assert!(!rendered.contains('\n'));
        assert!(!rendered.contains('\0'));
    }

    #[test]
    fn utf8_truncation_keeps_a_valid_character_boundary() {
        let summary = summarize_failures(std::iter::once("路".repeat(200)), 1);
        let sample = &summary.samples()[0];

        assert!(sample.len() <= MAX_FAILURE_SAMPLE_BYTES);
        assert!(sample.ends_with(TRUNCATION_MARKER));
        assert!(sample[..sample.len() - TRUNCATION_MARKER.len()]
            .chars()
            .all(|character| character == '路'));
    }

    #[test]
    fn zero_sample_policy_counts_without_formatting_failure_values() {
        struct MustNotFormat;

        impl Display for MustNotFormat {
            fn fmt(&self, _formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                panic!("entries beyond the sample limit must not be formatted")
            }
        }

        let summary = summarize_failures([MustNotFormat, MustNotFormat], 0);
        assert_eq!(summary.total(), 2);
        assert!(summary.samples().is_empty());
        assert_eq!(summary.remaining(), 2);
        assert_eq!(summary.to_string(), "total=2 sampled=0 remaining=2");
    }

    #[test]
    fn structured_renderer_runs_only_for_retained_samples() {
        let mut rendered = 0;
        let summary = summarize_failures_by(0..50, 2, |failure| {
            rendered += 1;
            format!("structured-{failure}")
        });

        assert_eq!(rendered, 2);
        assert_eq!(summary.total(), 50);
        assert_eq!(summary.samples(), ["structured-0", "structured-1"]);
        assert_eq!(summary.remaining(), 48);
    }

    #[test]
    fn incremental_collector_does_not_retain_failures_beyond_its_sample_limit() {
        let mut collector = BoundedFailureCollector::new(2);
        for index in 0..100 {
            collector.push(format_args!("failure-{index}"));
        }
        let summary = collector.finish();

        assert_eq!(summary.total(), 100);
        assert_eq!(summary.samples(), ["failure-0", "failure-1"]);
        assert_eq!(summary.remaining(), 98);
    }
}
