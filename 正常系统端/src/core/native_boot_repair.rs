//! Side-effect-free adapter for the legacy one-click boot-repair workflow.
//!
//! The UI selects one detected Windows partition. This adapter verifies that selection against a
//! fresh detected-target list and always constructs the existing backend's automatic-mode request;
//! UEFI/Legacy resolution remains exclusively in the backend.

use super::native_tool_backend::{BootRepairMode, NativeToolBackendRequest};
use super::native_tool_executor::ConfirmedToolPlan;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootRepairTarget {
    pub partition: String,
    pub windows_version: String,
    pub architecture: String,
}

impl BootRepairTarget {
    pub fn display_label(&self) -> String {
        format!(
            "{} [{}] [{}]",
            self.partition, self.windows_version, self.architecture
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootRepairRequest {
    pub target_partition: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeBootRepairError {
    InvalidPartition,
    TargetNotDetected,
}

impl std::fmt::Display for NativeBootRepairError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&match self {
            Self::InvalidPartition => crate::tr!("无效的 Windows 目标分区"),
            Self::TargetNotDetected => {
                crate::tr!("所选 Windows 分区已不在检测结果中，请刷新后重试。")
            }
        })
    }
}

impl std::error::Error for NativeBootRepairError {}

pub fn build_backend_request(
    plan: ConfirmedToolPlan,
    request: &BootRepairRequest,
    fresh_targets: &[BootRepairTarget],
) -> Result<NativeToolBackendRequest, NativeBootRepairError> {
    let target = normalize_partition(&request.target_partition)?;
    if !fresh_targets
        .iter()
        .filter_map(|candidate| normalize_partition(&candidate.partition).ok())
        .any(|candidate| candidate.eq_ignore_ascii_case(&target))
    {
        return Err(NativeBootRepairError::TargetNotDetected);
    }
    Ok(NativeToolBackendRequest::RepairBoot {
        plan,
        target,
        boot_mode: BootRepairMode::Auto,
    })
}

fn normalize_partition(partition: &str) -> Result<String, NativeBootRepairError> {
    let value = partition.trim();
    if matches!(value.as_bytes(), [letter, b':'] if letter.is_ascii_alphabetic()) {
        Ok(format!(
            "{}:",
            (value.as_bytes()[0] as char).to_ascii_uppercase()
        ))
    } else {
        Err(NativeBootRepairError::InvalidPartition)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::native_tool_executor::{
        plan_execution, ToolExecutionPlan, ToolExecutionRequest,
    };
    use crate::core::native_tools_controller::NativeToolAction;

    fn confirmed_plan() -> ConfirmedToolPlan {
        match plan_execution(ToolExecutionRequest::NativeAction {
            action: NativeToolAction::RepairBoot,
            confirmed: true,
        }) {
            ToolExecutionPlan::Mutating(plan) => plan,
            other => panic!("expected mutating plan, got {other:?}"),
        }
    }

    fn detected() -> Vec<BootRepairTarget> {
        vec![BootRepairTarget {
            partition: "D:".to_owned(),
            windows_version: "Windows 11".to_owned(),
            architecture: "x64".to_owned(),
        }]
    }

    #[test]
    fn adapter_always_uses_backend_auto_mode() {
        let backend = build_backend_request(
            confirmed_plan(),
            &BootRepairRequest {
                target_partition: "d:".to_owned(),
            },
            &detected(),
        )
        .unwrap();
        assert!(matches!(
            backend,
            NativeToolBackendRequest::RepairBoot {
                target,
                boot_mode: BootRepairMode::Auto,
                ..
            } if target == "D:"
        ));
    }

    #[test]
    fn target_must_still_exist_in_fresh_detected_inventory() {
        assert_eq!(
            build_backend_request(
                confirmed_plan(),
                &BootRepairRequest {
                    target_partition: "E:".to_owned(),
                },
                &detected(),
            ),
            Err(NativeBootRepairError::TargetNotDetected)
        );
    }

    #[test]
    fn display_keeps_version_and_architecture() {
        assert_eq!(detected()[0].display_label(), "D: [Windows 11] [x64]");
    }
}
