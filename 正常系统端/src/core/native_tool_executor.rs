//! Typed safety boundary for native toolbox intents.
//!
//! Read-only operations may execute through this module. Mutating and bundled
//! external-tool actions only become plans; they are never executed here.

#[cfg(not(feature = "non-elevated-tests"))]
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use super::native_tools_controller::{plan_tool, NativeToolAction, ToolRoute, ToolSafetyClass};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadOnlyToolRequest {
    Sha256 { path: String, expected: String },
    GhoPassword { path: String },
    VerifyImage { path: String },
    InstalledSoftware,
    NetworkInformation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolExecutionRequest {
    ReadOnly(ReadOnlyToolRequest),
    NativeAction {
        action: NativeToolAction,
        /// Set only after the confirmation dialog has returned affirmative.
        confirmed: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolExecutionClass {
    ReadOnly,
    Mutating,
    External,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmedToolPlan {
    pub action: NativeToolAction,
    pub safety: ToolSafetyClass,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalToolPlan {
    pub action: NativeToolAction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolExecutionPlan {
    ReadOnly(ReadOnlyToolRequest),
    ReadOnlyInputRequired {
        action: NativeToolAction,
    },
    ConfirmationRequired {
        action: NativeToolAction,
        safety: ToolSafetyClass,
    },
    Mutating(ConfirmedToolPlan),
    External(ExternalToolPlan),
}

impl ToolExecutionPlan {
    pub const fn class(&self) -> ToolExecutionClass {
        match self {
            Self::ReadOnly(_) | Self::ReadOnlyInputRequired { .. } => ToolExecutionClass::ReadOnly,
            Self::ConfirmationRequired { .. } | Self::Mutating(_) => ToolExecutionClass::Mutating,
            Self::External(_) => ToolExecutionClass::External,
        }
    }
}

pub fn plan_execution(request: ToolExecutionRequest) -> ToolExecutionPlan {
    match request {
        ToolExecutionRequest::ReadOnly(request) => ToolExecutionPlan::ReadOnly(request),
        ToolExecutionRequest::NativeAction { action, confirmed } => {
            let plan = plan_tool(action);
            if matches!(
                plan.safety,
                ToolSafetyClass::ReadOnly | ToolSafetyClass::SensitiveRead
            ) {
                ToolExecutionPlan::ReadOnlyInputRequired { action }
            } else if plan.safety.requires_explicit_execution() && !confirmed {
                ToolExecutionPlan::ConfirmationRequired {
                    action,
                    safety: plan.safety,
                }
            } else if matches!(plan.route, ToolRoute::LaunchBundledTool(_)) {
                ToolExecutionPlan::External(ExternalToolPlan { action })
            } else {
                ToolExecutionPlan::Mutating(ConfirmedToolPlan {
                    action,
                    safety: plan.safety,
                })
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sha256Result {
    pub path: String,
    pub file_size: u64,
    pub sha256: String,
    pub expected: String,
    pub matched: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GhoPasswordResult {
    pub path: String,
    pub valid: bool,
    pub has_password: bool,
    pub password: Option<String>,
    pub password_length: usize,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageVerificationResult {
    pub path: String,
    pub image_type: String,
    pub status: String,
    pub valid: bool,
    pub file_size: u64,
    pub image_count: u32,
    pub part_count: u16,
    pub message: String,
    pub details: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledSoftwareRecord {
    pub name: String,
    pub version: String,
    pub publisher: String,
    pub install_location: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkAdapterRecord {
    pub name: String,
    pub description: String,
    pub mac_address: String,
    pub ip_addresses: Vec<String>,
    pub adapter_type: String,
    pub status: String,
    pub speed: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadOnlyToolResult {
    Sha256(Sha256Result),
    GhoPassword(GhoPasswordResult),
    ImageVerification(ImageVerificationResult),
    InstalledSoftware(Vec<InstalledSoftwareRecord>),
    NetworkInformation(Vec<NetworkAdapterRecord>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolExecutionEvent {
    Progress { percentage: u8, detail: String },
}

pub trait ToolExecutionReporter {
    fn report(&mut self, event: ToolExecutionEvent);
}

impl<F> ToolExecutionReporter for F
where
    F: FnMut(ToolExecutionEvent),
{
    fn report(&mut self, event: ToolExecutionEvent) {
        self(event);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolExecutionError {
    DevelopmentBuildDenied,
    NotReadOnlyPlan,
    Io(String),
    Collection(String),
}

impl std::fmt::Display for ToolExecutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DevelopmentBuildDenied => {
                formatter.write_str("tool execution is disabled in non-elevated development builds")
            }
            Self::NotReadOnlyPlan => formatter.write_str("only read-only plans may execute here"),
            Self::Io(detail) | Self::Collection(detail) => formatter.write_str(detail),
        }
    }
}

impl std::error::Error for ToolExecutionError {}

pub struct NativeToolExecutor;

impl NativeToolExecutor {
    pub fn execute_read_only(
        plan: &ToolExecutionPlan,
        reporter: &mut dyn ToolExecutionReporter,
    ) -> Result<ReadOnlyToolResult, ToolExecutionError> {
        Self::execute_read_only_with_cancel(plan, reporter, None)
    }

    pub fn execute_read_only_with_cancel(
        plan: &ToolExecutionPlan,
        reporter: &mut dyn ToolExecutionReporter,
        cancel: Option<Arc<AtomicBool>>,
    ) -> Result<ReadOnlyToolResult, ToolExecutionError> {
        #[cfg(feature = "non-elevated-tests")]
        {
            let _ = (plan, reporter, cancel);
            Err(ToolExecutionError::DevelopmentBuildDenied)
        }

        #[cfg(not(feature = "non-elevated-tests"))]
        {
            let ToolExecutionPlan::ReadOnly(request) = plan else {
                return Err(ToolExecutionError::NotReadOnlyPlan);
            };
            match request {
                ReadOnlyToolRequest::Sha256 { path, expected } => {
                    Self::sha256(path, expected, reporter)
                }
                ReadOnlyToolRequest::GhoPassword { path } => Ok(Self::gho_password(path)),
                ReadOnlyToolRequest::VerifyImage { path } => {
                    Ok(Self::verify_image(path, reporter, cancel.as_ref()))
                }
                ReadOnlyToolRequest::InstalledSoftware => {
                    Ok(ReadOnlyToolResult::InstalledSoftware(installed_software()))
                }
                ReadOnlyToolRequest::NetworkInformation => Self::network_information(),
            }
        }
    }

    #[cfg(not(feature = "non-elevated-tests"))]
    fn sha256(
        path: &str,
        expected: &str,
        reporter: &mut dyn ToolExecutionReporter,
    ) -> Result<ReadOnlyToolResult, ToolExecutionError> {
        let file_size = std::fs::metadata(path)
            .map_err(|error| ToolExecutionError::Io(error.to_string()))?
            .len();
        let sha256 = lr_core::hash::sha256_file(path, |read| {
            let percentage = if file_size == 0 {
                100
            } else {
                ((read.min(file_size) * 100 / file_size).min(100)) as u8
            };
            reporter.report(ToolExecutionEvent::Progress {
                percentage,
                detail: path.to_string(),
            });
        })
        .map_err(|error| ToolExecutionError::Io(error.to_string()))?;
        let matched =
            (!expected.trim().is_empty()).then(|| lr_core::hash::hash_matches(&sha256, expected));
        Ok(ReadOnlyToolResult::Sha256(Sha256Result {
            path: path.to_string(),
            file_size,
            sha256,
            expected: expected.to_string(),
            matched,
        }))
    }

    #[cfg(not(feature = "non-elevated-tests"))]
    fn gho_password(path: &str) -> ReadOnlyToolResult {
        let info = super::gho_password::read_gho_password(path);
        ReadOnlyToolResult::GhoPassword(GhoPasswordResult {
            path: path.to_string(),
            valid: info.is_valid_gho,
            has_password: info.has_password,
            password: info.password,
            password_length: info.password_length,
            error: info.error,
        })
    }

    #[cfg(not(feature = "non-elevated-tests"))]
    fn verify_image(
        path: &str,
        reporter: &mut dyn ToolExecutionReporter,
        cancel: Option<&Arc<AtomicBool>>,
    ) -> ReadOnlyToolResult {
        let (tx, rx) = std::sync::mpsc::channel();
        let verifier = super::image_verify::ImageVerifier::new();
        let verifier_cancel = verifier.get_cancel_flag();
        let verification = std::thread::scope(|scope| {
            let worker = scope.spawn(move || verifier.verify(path, Some(tx)));
            while !worker.is_finished() {
                if cancel.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst)) {
                    verifier_cancel.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                match rx.recv_timeout(std::time::Duration::from_millis(50)) {
                    Ok(progress) => report_image_progress(reporter, progress),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        std::thread::yield_now();
                    }
                }
            }
            for progress in rx.try_iter() {
                report_image_progress(reporter, progress);
            }
            worker.join()
        });
        let Ok(result) = verification else {
            return ReadOnlyToolResult::ImageVerification(ImageVerificationResult {
                path: path.to_string(),
                image_type: String::new(),
                status: crate::tr!("校验出错"),
                valid: false,
                file_size: 0,
                image_count: 0,
                part_count: 0,
                message: crate::tr!("镜像校验工作线程异常结束。"),
                details: Vec::new(),
            });
        };
        ReadOnlyToolResult::ImageVerification(ImageVerificationResult {
            path: result.file_path,
            image_type: result.image_type.to_string(),
            status: result.status.to_string(),
            valid: result.status == super::image_verify::VerifyStatus::Valid,
            file_size: result.file_size,
            image_count: result.image_count,
            part_count: result.part_count,
            message: result.message,
            details: result.details,
        })
    }

    #[cfg(not(feature = "non-elevated-tests"))]
    fn network_information() -> Result<ReadOnlyToolResult, ToolExecutionError> {
        let hardware = super::hardware_info::HardwareInfo::collect()
            .map_err(|error| ToolExecutionError::Collection(error.to_string()))?;
        Ok(ReadOnlyToolResult::NetworkInformation(
            hardware
                .network_adapters
                .into_iter()
                .map(|adapter| NetworkAdapterRecord {
                    name: adapter.name,
                    description: adapter.description,
                    mac_address: adapter.mac_address,
                    ip_addresses: adapter.ip_addresses,
                    adapter_type: adapter.adapter_type,
                    status: adapter.status,
                    speed: adapter.speed,
                })
                .collect(),
        ))
    }
}

#[cfg(not(feature = "non-elevated-tests"))]
fn report_image_progress(
    reporter: &mut dyn ToolExecutionReporter,
    progress: super::image_verify::VerifyProgress,
) {
    reporter.report(ToolExecutionEvent::Progress {
        percentage: progress.percentage,
        detail: if progress.current_item.is_empty() {
            progress.status
        } else {
            progress.current_item
        },
    });
}

#[cfg(not(feature = "non-elevated-tests"))]
fn installed_software() -> Vec<InstalledSoftwareRecord> {
    #[cfg(windows)]
    {
        use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
        use winreg::RegKey;

        let mut records = Vec::new();
        let mut seen = HashSet::new();
        for (root, path) in [
            (
                HKEY_LOCAL_MACHINE,
                r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
            ),
            (
                HKEY_LOCAL_MACHINE,
                r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
            ),
            (
                HKEY_CURRENT_USER,
                r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
            ),
        ] {
            let Ok(key) = RegKey::predef(root).open_subkey(path) else {
                continue;
            };
            for subkey_name in key.enum_keys().flatten() {
                let Ok(subkey) = key.open_subkey(&subkey_name) else {
                    continue;
                };
                let name: String = subkey.get_value("DisplayName").unwrap_or_default();
                if name.is_empty()
                    || name.starts_with("KB")
                    || subkey_name.starts_with("KB")
                    || !seen.insert(name.clone())
                {
                    continue;
                }
                records.push(InstalledSoftwareRecord {
                    name,
                    version: subkey.get_value("DisplayVersion").unwrap_or_default(),
                    publisher: subkey.get_value("Publisher").unwrap_or_default(),
                    install_location: subkey.get_value("InstallLocation").unwrap_or_default(),
                });
            }
        }
        records.sort_by(|left, right| {
            left.name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
        });
        records
    }

    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_requests_are_classified_without_confirmation() {
        let plan = plan_execution(ToolExecutionRequest::ReadOnly(
            ReadOnlyToolRequest::Sha256 {
                path: "image.wim".into(),
                expected: String::new(),
            },
        ));
        assert_eq!(plan.class(), ToolExecutionClass::ReadOnly);
    }

    #[test]
    fn every_mutating_native_action_requires_confirmation() {
        for action in NativeToolAction::ALL {
            let route = plan_tool(action);
            if route.safety.requires_explicit_execution()
                && !matches!(route.route, ToolRoute::LaunchBundledTool(_))
            {
                let pending = plan_execution(ToolExecutionRequest::NativeAction {
                    action,
                    confirmed: false,
                });
                assert!(matches!(
                    pending,
                    ToolExecutionPlan::ConfirmationRequired { .. }
                ));
                let confirmed = plan_execution(ToolExecutionRequest::NativeAction {
                    action,
                    confirmed: true,
                });
                assert!(matches!(confirmed, ToolExecutionPlan::Mutating(_)));
            }
        }
    }

    #[test]
    fn bundled_programs_only_produce_external_plans() {
        for action in [
            NativeToolAction::RunGhost,
            NativeToolAction::RunSpaceSniffer,
        ] {
            let pending = plan_execution(ToolExecutionRequest::NativeAction {
                action,
                confirmed: false,
            });
            assert!(matches!(
                pending,
                ToolExecutionPlan::ConfirmationRequired { .. }
            ));
            let plan = plan_execution(ToolExecutionRequest::NativeAction {
                action,
                confirmed: true,
            });
            assert!(matches!(plan, ToolExecutionPlan::External(_)));
        }
    }

    #[test]
    fn native_read_only_buttons_are_never_misclassified_as_mutations() {
        for action in [
            NativeToolAction::NetworkInformation,
            NativeToolAction::SoftwareList,
            NativeToolAction::ReadGhoPassword,
            NativeToolAction::VerifyImage,
            NativeToolAction::VerifyFileHash,
        ] {
            let plan = plan_execution(ToolExecutionRequest::NativeAction {
                action,
                confirmed: false,
            });
            assert_eq!(plan.class(), ToolExecutionClass::ReadOnly);
            assert!(matches!(
                plan,
                ToolExecutionPlan::ReadOnlyInputRequired { .. }
            ));
        }
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn development_build_refuses_even_read_only_host_io() {
        let plan = ToolExecutionPlan::ReadOnly(ReadOnlyToolRequest::InstalledSoftware);
        let mut reporter = |_: ToolExecutionEvent| {};
        assert_eq!(
            NativeToolExecutor::execute_read_only(&plan, &mut reporter),
            Err(ToolExecutionError::DevelopmentBuildDenied)
        );
    }
}
