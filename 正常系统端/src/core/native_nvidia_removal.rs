//! Typed native boundary for the legacy NVIDIA-driver removal workflow.
//!
//! The original dialog required an inventory-provided Windows target and always removed the
//! complete NVIDIA driver scope supported by `nvidia_driver`. It never supported selecting
//! individual devices or uninstalling NVIDIA applications, so those misleading options are not
//! represented here.

use super::native_tool_backend::{
    NativeToolBackend, NativeToolBackendError, NativeToolBackendRequest,
};
use super::native_tool_executor::ConfirmedToolPlan;
use super::native_tools_controller::NativeToolAction;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NvidiaRemovalTarget {
    CurrentSystem,
    OfflineWindows(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NvidiaRemovalRequest {
    pub target: NvidiaRemovalTarget,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NvidiaHardwareRow {
    pub item: String,
    pub value: String,
    pub is_nvidia: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NvidiaHardwareReport {
    pub rows: Vec<NvidiaHardwareRow>,
    pub nvidia_device_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeNvidiaRemovalError {
    DevelopmentBuildDenied,
    InvalidTarget(String),
    InvalidPlan,
    Inventory(String),
    Backend(String),
}

impl std::fmt::Display for NativeNvidiaRemovalError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::DevelopmentBuildDenied => {
                crate::tr!("开发测试构建禁止读取宿主 NVIDIA 硬件信息")
            }
            Self::InvalidTarget(target) => crate::tr!("无效的 NVIDIA 驱动清理目标: {}", target),
            Self::InvalidPlan => crate::tr!("NVIDIA 驱动清理确认计划无效"),
            Self::Inventory(detail) => crate::tr!("读取 NVIDIA 硬件信息失败: {}", detail),
            Self::Backend(detail) => crate::tr!("NVIDIA 驱动清理请求无效: {}", detail),
        };
        formatter.write_str(&message)
    }
}

impl std::error::Error for NativeNvidiaRemovalError {}

pub fn validate_request(request: &NvidiaRemovalRequest) -> Result<(), NativeNvidiaRemovalError> {
    match &request.target {
        NvidiaRemovalTarget::CurrentSystem => Ok(()),
        NvidiaRemovalTarget::OfflineWindows(partition) if matches!(partition.trim().as_bytes(), [letter, b':'] if letter.is_ascii_alphabetic()) => {
            Ok(())
        }
        NvidiaRemovalTarget::OfflineWindows(partition) => {
            Err(NativeNvidiaRemovalError::InvalidTarget(partition.clone()))
        }
    }
}

/// Converts the confirmed typed request to the existing production backend without widening its
/// deletion scope.
pub fn build_backend_request(
    request: &NvidiaRemovalRequest,
    plan: ConfirmedToolPlan,
) -> Result<NativeToolBackendRequest, NativeNvidiaRemovalError> {
    validate_request(request)?;
    if plan.action != NativeToolAction::NvidiaDriverRemoval {
        return Err(NativeNvidiaRemovalError::InvalidPlan);
    }
    let offline_target = match &request.target {
        NvidiaRemovalTarget::CurrentSystem => None,
        NvidiaRemovalTarget::OfflineWindows(partition) => Some(partition.trim().to_owned()),
    };
    let backend_request = NativeToolBackendRequest::RemoveNvidiaDrivers {
        plan,
        offline_target,
    };
    NativeToolBackend::route(&backend_request)
        .map_err(|error| NativeNvidiaRemovalError::Backend(error.to_string()))?;
    Ok(backend_request)
}

#[cfg(feature = "non-elevated-tests")]
pub fn load_hardware_report() -> Result<NvidiaHardwareReport, NativeNvidiaRemovalError> {
    Err(NativeNvidiaRemovalError::DevelopmentBuildDenied)
}

#[cfg(not(feature = "non-elevated-tests"))]
pub fn load_hardware_report() -> Result<NvidiaHardwareReport, NativeNvidiaRemovalError> {
    super::nvidia_driver::get_system_hardware_summary()
        .map_err(|error| NativeNvidiaRemovalError::Inventory(error.to_string()))
        .map(|summary| hardware_report(&summary))
}

#[cfg(not(feature = "non-elevated-tests"))]
fn hardware_report(summary: &super::nvidia_driver::SystemHardwareSummary) -> NvidiaHardwareReport {
    let mut report = NvidiaHardwareReport::default();
    for gpu in summary.gpu_devices.iter().filter(|gpu| gpu.is_nvidia) {
        let name = if gpu.friendly_name.trim().is_empty() {
            &gpu.name
        } else {
            &gpu.friendly_name
        };
        report.rows.push(NvidiaHardwareRow {
            item: crate::tr!("NVIDIA 显卡"),
            value: super::nvidia_driver::beautify_gpu_name(name),
            is_nvidia: true,
        });
        report.nvidia_device_count += 1;
    }
    report
}

pub fn removal_scope(target: &NvidiaRemovalTarget) -> Result<String, NativeNvidiaRemovalError> {
    validate_request(&NvidiaRemovalRequest {
        target: target.clone(),
    })?;
    Ok(match target {
        NvidiaRemovalTarget::CurrentSystem => {
            crate::tr!("当前系统中检测到的全部 NVIDIA 显卡驱动设备")
        }
        NvidiaRemovalTarget::OfflineWindows(partition) => {
            crate::tr!("{} 中的全部 NVIDIA 驱动目录和驱动 INF 文件", partition)
        }
    })
}

impl From<NativeToolBackendError> for NativeNvidiaRemovalError {
    fn from(error: NativeToolBackendError) -> Self {
        Self::Backend(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::native_tool_backend::NativeToolBackendRoute;
    use crate::core::native_tool_executor::{
        plan_execution, ToolExecutionPlan, ToolExecutionRequest,
    };

    fn confirmed_plan() -> ConfirmedToolPlan {
        match plan_execution(ToolExecutionRequest::NativeAction {
            action: NativeToolAction::NvidiaDriverRemoval,
            confirmed: true,
        }) {
            ToolExecutionPlan::Mutating(plan) => plan,
            other => panic!("expected mutating plan, got {other:?}"),
        }
    }

    #[test]
    fn request_only_represents_the_legacy_complete_removal_scope() {
        let request = NvidiaRemovalRequest {
            target: NvidiaRemovalTarget::CurrentSystem,
        };
        assert_eq!(validate_request(&request), Ok(()));
        assert!(matches!(request.target, NvidiaRemovalTarget::CurrentSystem));
    }

    #[test]
    fn current_and_offline_targets_map_to_existing_backend_routes() {
        let current = build_backend_request(
            &NvidiaRemovalRequest {
                target: NvidiaRemovalTarget::CurrentSystem,
            },
            confirmed_plan(),
        )
        .unwrap();
        assert_eq!(
            NativeToolBackend::route(&current),
            Ok(NativeToolBackendRoute::NvidiaOnline)
        );

        let offline = build_backend_request(
            &NvidiaRemovalRequest {
                target: NvidiaRemovalTarget::OfflineWindows("D:".to_owned()),
            },
            confirmed_plan(),
        )
        .unwrap();
        assert_eq!(
            NativeToolBackend::route(&offline),
            Ok(NativeToolBackendRoute::NvidiaOffline)
        );
    }

    #[test]
    fn free_form_or_mismatched_targets_fail_closed() {
        for target in ["", "Windows", "D:\\Windows", "1:"] {
            assert!(matches!(
                validate_request(&NvidiaRemovalRequest {
                    target: NvidiaRemovalTarget::OfflineWindows(target.to_owned()),
                }),
                Err(NativeNvidiaRemovalError::InvalidTarget(_))
            ));
        }
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn development_build_denies_hardware_inventory_before_host_io() {
        assert_eq!(
            load_hardware_report(),
            Err(NativeNvidiaRemovalError::DevelopmentBuildDenied)
        );
    }
}
