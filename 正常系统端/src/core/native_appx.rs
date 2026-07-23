//! Safe native APPX removal boundary.
//!
//! Online removal is revalidated against a fresh PackageManager inventory immediately before the
//! mutation. Offline removal uses DISM's supported provisioned-package interface; the legacy
//! WindowsApps-directory deletion path is never exposed to native code.

use std::collections::HashSet;

#[cfg(not(feature = "non-elevated-tests"))]
use lr_core::command::SystemCommandExecutor;
use lr_core::command::{CommandExecutor, CommandRequest};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppxTarget {
    CurrentSystem,
    OfflineWindows(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoveAppxRequest {
    pub target: AppxTarget,
    pub packages: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoveAppxResult {
    pub removed: usize,
    pub failed: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfflineAppxPackage {
    pub package_name: String,
    pub display_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoveAppxError {
    DevelopmentBuildDenied,
    InvalidOfflineTarget(String),
    EmptySelection,
    DuplicatePackage(String),
    CriticalPackage(String),
    StaleSelection(String),
    Inventory(String),
    Execution(String),
}

impl std::fmt::Display for RemoveAppxError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DevelopmentBuildDenied => {
                formatter.write_str("APPX removal is disabled in non-elevated development builds")
            }
            Self::InvalidOfflineTarget(target) => {
                write!(formatter, "invalid offline Windows target: {target:?}")
            }
            Self::EmptySelection => formatter.write_str("no APPX package was selected"),
            Self::DuplicatePackage(package) => {
                write!(formatter, "duplicate APPX package selection: {package:?}")
            }
            Self::CriticalPackage(package) => {
                write!(
                    formatter,
                    "protected APPX package cannot be removed: {package:?}"
                )
            }
            Self::StaleSelection(package) => write!(
                formatter,
                "APPX inventory changed; package was not removed: {package:?}"
            ),
            Self::Inventory(detail) | Self::Execution(detail) => formatter.write_str(detail),
        }
    }
}

impl std::error::Error for RemoveAppxError {}

pub fn validate_request(request: &RemoveAppxRequest) -> Result<(), RemoveAppxError> {
    if let AppxTarget::OfflineWindows(target) = &request.target {
        normalize_offline_root(target)?;
    }
    if request.packages.is_empty() {
        return Err(RemoveAppxError::EmptySelection);
    }
    let mut unique = HashSet::new();
    for package in &request.packages {
        let package = package.trim();
        if package.is_empty() {
            return Err(RemoveAppxError::EmptySelection);
        }
        if super::native_appx_legacy::is_critical(package) {
            return Err(RemoveAppxError::CriticalPackage(package.into()));
        }
        if !unique.insert(package.to_ascii_lowercase()) {
            return Err(RemoveAppxError::DuplicatePackage(package.into()));
        }
    }
    Ok(())
}

trait AppxDeployment {
    fn current_inventory(&mut self) -> Result<Vec<String>, RemoveAppxError>;
    fn remove_current(&mut self, packages: &[String]) -> Result<RemoveAppxResult, RemoveAppxError>;
    fn offline_inventory(&mut self, target: &str) -> Result<Vec<String>, RemoveAppxError>;
    fn remove_offline(
        &mut self,
        target: &str,
        packages: &[String],
    ) -> Result<RemoveAppxResult, RemoveAppxError>;
}

fn execute_with(
    request: &RemoveAppxRequest,
    deployment: &mut dyn AppxDeployment,
) -> Result<RemoveAppxResult, RemoveAppxError> {
    validate_request(request)?;
    let fresh = match &request.target {
        AppxTarget::CurrentSystem => deployment.current_inventory()?,
        AppxTarget::OfflineWindows(target) => deployment.offline_inventory(target)?,
    };
    let fresh: HashSet<String> = fresh
        .into_iter()
        .filter(|package| !super::native_appx_legacy::is_critical(package))
        .map(|package| package.to_ascii_lowercase())
        .collect();
    for package in &request.packages {
        if !fresh.contains(&package.trim().to_ascii_lowercase()) {
            return Err(RemoveAppxError::StaleSelection(package.clone()));
        }
    }
    match &request.target {
        AppxTarget::CurrentSystem => deployment.remove_current(&request.packages),
        AppxTarget::OfflineWindows(target) => deployment.remove_offline(target, &request.packages),
    }
}

/// Loads offline provisioned packages through DISM. Development builds reject this before
/// constructing the production command executor, so UI tests cannot inspect the host image.
pub fn offline_inventory(target: &str) -> Result<Vec<OfflineAppxPackage>, RemoveAppxError> {
    #[cfg(feature = "non-elevated-tests")]
    {
        let _ = target;
        Err(RemoveAppxError::DevelopmentBuildDenied)
    }
    #[cfg(not(feature = "non-elevated-tests"))]
    {
        offline_inventory_with(&SystemCommandExecutor, target)
    }
}

pub fn execute(request: &RemoveAppxRequest) -> Result<RemoveAppxResult, RemoveAppxError> {
    #[cfg(feature = "non-elevated-tests")]
    {
        let _ = request;
        Err(RemoveAppxError::DevelopmentBuildDenied)
    }
    #[cfg(not(feature = "non-elevated-tests"))]
    {
        execute_with(request, &mut WindowsAppxDeployment)
    }
}

#[cfg(not(feature = "non-elevated-tests"))]
struct WindowsAppxDeployment;

#[cfg(not(feature = "non-elevated-tests"))]
impl AppxDeployment for WindowsAppxDeployment {
    fn current_inventory(&mut self) -> Result<Vec<String>, RemoveAppxError> {
        Ok(super::native_appx_legacy::current_inventory()
            .into_iter()
            .map(|package| package.package_name)
            .collect())
    }

    fn remove_current(&mut self, packages: &[String]) -> Result<RemoveAppxResult, RemoveAppxError> {
        // The sentinel is fixed here, so the legacy dispatch can only enter its PackageManager
        // branch and can never reach its unsupported offline directory-deletion branch.
        let (removed, failed) = super::native_appx_legacy::remove_current(packages);
        Ok(RemoveAppxResult { removed, failed })
    }

    fn offline_inventory(&mut self, target: &str) -> Result<Vec<String>, RemoveAppxError> {
        offline_inventory_with(&SystemCommandExecutor, target).map(|packages| {
            packages
                .into_iter()
                .map(|package| package.package_name)
                .collect()
        })
    }

    fn remove_offline(
        &mut self,
        target: &str,
        packages: &[String],
    ) -> Result<RemoveAppxResult, RemoveAppxError> {
        remove_offline_with(&SystemCommandExecutor, target, packages)
    }
}

fn offline_inventory_with(
    executor: &dyn CommandExecutor,
    target: &str,
) -> Result<Vec<OfflineAppxPackage>, RemoveAppxError> {
    let image = normalize_offline_root(target)?;
    let request = CommandRequest::new("dism.exe").args([
        "/English".to_owned(),
        format!("/Image:{image}"),
        "/Get-ProvisionedAppxPackages".to_owned(),
    ]);
    let outcome = executor
        .execute(&request)
        .map_err(|error| RemoveAppxError::Inventory(error.to_string()))?;
    let output = command_text(&outcome);
    if !dism_succeeded(&outcome, &output) {
        return Err(RemoveAppxError::Inventory(dism_error(&outcome, &output)));
    }
    Ok(parse_provisioned_packages(&output))
}

fn remove_offline_with(
    executor: &dyn CommandExecutor,
    target: &str,
    packages: &[String],
) -> Result<RemoveAppxResult, RemoveAppxError> {
    let image = normalize_offline_root(target)?;
    let mut removed = 0;
    let mut failed = 0;
    for package in packages {
        let package = package.trim();
        if !valid_package_name(package) {
            return Err(RemoveAppxError::Execution(format!(
                "invalid provisioned APPX package name: {package:?}"
            )));
        }
        let request = CommandRequest::new("dism.exe").args([
            "/English".to_owned(),
            format!("/Image:{image}"),
            "/Remove-ProvisionedAppxPackage".to_owned(),
            format!("/PackageName:{package}"),
        ]);
        let outcome = executor
            .execute(&request)
            .map_err(|error| RemoveAppxError::Execution(error.to_string()))?;
        let output = command_text(&outcome);
        if dism_succeeded(&outcome, &output) {
            removed += 1;
        } else {
            failed += 1;
        }
    }
    Ok(RemoveAppxResult { removed, failed })
}

fn normalize_offline_root(target: &str) -> Result<String, RemoveAppxError> {
    let target = target.trim();
    match target.as_bytes() {
        [letter, b':'] if letter.is_ascii_alphabetic() => {
            Ok(format!("{}:\\", (*letter as char).to_ascii_uppercase()))
        }
        [letter, b':', b'\\'] if letter.is_ascii_alphabetic() => {
            Ok(format!("{}:\\", (*letter as char).to_ascii_uppercase()))
        }
        _ => Err(RemoveAppxError::InvalidOfflineTarget(target.to_owned())),
    }
}

fn valid_package_name(package: &str) -> bool {
    !package.is_empty()
        && package.len() <= 512
        && package
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'~'))
}

fn command_text(outcome: &lr_core::command::CommandOutcome) -> String {
    let mut output = lr_core::encoding::gbk_to_utf8(outcome.stdout());
    let stderr = lr_core::encoding::gbk_to_utf8(outcome.stderr());
    if !stderr.trim().is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&stderr);
    }
    output
}

fn dism_succeeded(outcome: &lr_core::command::CommandOutcome, output: &str) -> bool {
    outcome.succeeded() && !contains_dism_error(output)
}

fn contains_dism_error(output: &str) -> bool {
    output.lines().any(|line| {
        let line = line.trim();
        line.starts_with("Error:")
            || line.starts_with("错误:")
            || line.eq_ignore_ascii_case("The operation failed.")
    })
}

fn dism_error(outcome: &lr_core::command::CommandOutcome, output: &str) -> String {
    let detail = output.trim();
    if detail.is_empty() {
        format!("DISM exited with code {:?}", outcome.exit_code())
    } else {
        format!("DISM failed (exit {:?}): {detail}", outcome.exit_code())
    }
}

fn parse_provisioned_packages(output: &str) -> Vec<OfflineAppxPackage> {
    let mut packages = Vec::new();
    let mut package_name: Option<String> = None;
    let mut display_name: Option<String> = None;
    let flush = |packages: &mut Vec<OfflineAppxPackage>,
                 package_name: &mut Option<String>,
                 display_name: &mut Option<String>| {
        let Some(package_name) = package_name.take() else {
            display_name.take();
            return;
        };
        if !valid_package_name(&package_name)
            || super::native_appx_legacy::is_critical(&package_name)
            || packages.iter().any(|existing: &OfflineAppxPackage| {
                existing.package_name.eq_ignore_ascii_case(&package_name)
            })
        {
            display_name.take();
            return;
        }
        let display_name = display_name
            .take()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| friendly_package_name(&package_name));
        packages.push(OfflineAppxPackage {
            package_name,
            display_name,
        });
    };
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            flush(&mut packages, &mut package_name, &mut display_name);
            continue;
        }
        if let Some(value) = field_value(line, "PackageName") {
            if package_name.is_some() {
                flush(&mut packages, &mut package_name, &mut display_name);
            }
            package_name = Some(value.to_owned());
        } else if let Some(value) = field_value(line, "DisplayName") {
            if display_name.is_some() && package_name.is_some() {
                flush(&mut packages, &mut package_name, &mut display_name);
            }
            display_name = Some(value.to_owned());
        }
    }
    flush(&mut packages, &mut package_name, &mut display_name);
    packages.sort_by(|left, right| left.display_name.cmp(&right.display_name));
    packages
}

fn field_value<'a>(line: &'a str, field: &str) -> Option<&'a str> {
    let (name, value) = line.split_once(':')?;
    name.trim()
        .eq_ignore_ascii_case(field)
        .then(|| value.trim())
}

fn friendly_package_name(package: &str) -> String {
    package
        .split('_')
        .next()
        .unwrap_or(package)
        .trim_start_matches("Microsoft.")
        .replace('.', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MockDeployment {
        inventory: Vec<String>,
        removals: Vec<Vec<String>>,
        offline_inventory: Vec<String>,
        offline_removals: Vec<(String, Vec<String>)>,
    }

    impl AppxDeployment for MockDeployment {
        fn current_inventory(&mut self) -> Result<Vec<String>, RemoveAppxError> {
            Ok(self.inventory.clone())
        }

        fn remove_current(
            &mut self,
            packages: &[String],
        ) -> Result<RemoveAppxResult, RemoveAppxError> {
            self.removals.push(packages.to_vec());
            Ok(RemoveAppxResult {
                removed: packages.len(),
                failed: 0,
            })
        }

        fn offline_inventory(&mut self, _target: &str) -> Result<Vec<String>, RemoveAppxError> {
            Ok(self.offline_inventory.clone())
        }

        fn remove_offline(
            &mut self,
            target: &str,
            packages: &[String],
        ) -> Result<RemoveAppxResult, RemoveAppxError> {
            self.offline_removals
                .push((target.to_owned(), packages.to_vec()));
            Ok(RemoveAppxResult {
                removed: packages.len(),
                failed: 0,
            })
        }
    }

    fn online(packages: &[&str]) -> RemoveAppxRequest {
        RemoveAppxRequest {
            target: AppxTarget::CurrentSystem,
            packages: packages.iter().map(|value| (*value).into()).collect(),
        }
    }

    #[test]
    fn offline_request_requires_fresh_inventory_before_mutation() {
        let package = "Contoso.App_1.0_x64__test";
        let request = RemoveAppxRequest {
            target: AppxTarget::OfflineWindows("D:".into()),
            packages: vec![package.into()],
        };
        let mut deployment = MockDeployment {
            offline_inventory: vec![package.into()],
            ..Default::default()
        };
        assert_eq!(
            execute_with(&request, &mut deployment).unwrap(),
            RemoveAppxResult {
                removed: 1,
                failed: 0
            }
        );
        assert_eq!(
            deployment.offline_removals,
            [(String::from("D:"), vec![String::from(package)])]
        );

        let mut stale = MockDeployment::default();
        assert!(matches!(
            execute_with(&request, &mut stale),
            Err(RemoveAppxError::StaleSelection(_))
        ));
        assert!(stale.offline_removals.is_empty());
    }

    #[test]
    fn online_removal_requires_a_fresh_inventory_subset() {
        let request = online(&["Contoso.App_1.0_x64__test"]);
        let mut deployment = MockDeployment {
            inventory: vec!["Different.App_1.0_x64__test".into()],
            ..Default::default()
        };
        assert!(matches!(
            execute_with(&request, &mut deployment),
            Err(RemoveAppxError::StaleSelection(_))
        ));
        assert!(deployment.removals.is_empty());
    }

    #[test]
    fn critical_and_duplicate_packages_are_rejected_without_mutation() {
        assert!(matches!(
            validate_request(&online(&["Microsoft.WindowsStore_1.0_x64__test"])),
            Err(RemoveAppxError::CriticalPackage(_))
        ));
        assert!(matches!(
            validate_request(&online(&[
                "Contoso.App_1.0_x64__test",
                "contoso.app_1.0_x64__test"
            ])),
            Err(RemoveAppxError::DuplicatePackage(_))
        ));
    }

    #[test]
    fn validated_current_subset_reaches_supported_backend_once() {
        let package = "Contoso.App_1.0_x64__test";
        let request = online(&[package]);
        let mut deployment = MockDeployment {
            inventory: vec![package.into()],
            ..Default::default()
        };
        assert_eq!(
            execute_with(&request, &mut deployment).unwrap(),
            RemoveAppxResult {
                removed: 1,
                failed: 0
            }
        );
        assert_eq!(deployment.removals, [vec![String::from(package)]]);
    }

    #[test]
    fn dism_inventory_is_parsed_and_critical_packages_are_filtered() {
        let output = r#"
DisplayName : Contoso.App
Version : 1.2.3.0
PackageName : Contoso.App_1.2.3.0_neutral_~_abc123

DisplayName : Microsoft.WindowsStore
PackageName : Microsoft.WindowsStore_1.0.0.0_neutral_~_8wekyb3d8bbwe

PackageName : Fabrikam.Tools_2.0.0.0_x64__publisher
"#;
        assert_eq!(
            parse_provisioned_packages(output),
            vec![
                OfflineAppxPackage {
                    package_name: "Contoso.App_1.2.3.0_neutral_~_abc123".into(),
                    display_name: "Contoso.App".into(),
                },
                OfflineAppxPackage {
                    package_name: "Fabrikam.Tools_2.0.0.0_x64__publisher".into(),
                    display_name: "Fabrikam Tools".into(),
                },
            ]
        );
    }

    #[test]
    fn dism_requests_keep_image_and_package_as_separate_arguments() {
        use lr_core::command::{CommandOutcome, DryRunCommandExecutor};

        let inventory_output = b"PackageName : Contoso.App_1.0.0.0_x64__publisher\r\n\
DisplayName : Contoso App\r\n";
        let inventory_executor = DryRunCommandExecutor::new(CommandOutcome::new(
            Some(0),
            inventory_output.to_vec(),
            Vec::new(),
        ));
        let inventory = offline_inventory_with(&inventory_executor, "d:").unwrap();
        assert_eq!(inventory.len(), 1);
        let requests = inventory_executor.requests().unwrap();
        assert_eq!(requests[0].program(), std::ffi::OsStr::new("dism.exe"));
        assert_eq!(
            requests[0].arguments(),
            ["/English", "/Image:D:\\", "/Get-ProvisionedAppxPackages"]
                .map(std::ffi::OsString::from)
                .as_slice()
        );

        let remove_executor = DryRunCommandExecutor::default();
        let result = remove_offline_with(
            &remove_executor,
            "D:",
            &["Contoso.App_1.0.0.0_x64__publisher".into()],
        )
        .unwrap();
        assert_eq!(result.removed, 1);
        let requests = remove_executor.requests().unwrap();
        assert_eq!(
            requests[0].arguments(),
            [
                "/English",
                "/Image:D:\\",
                "/Remove-ProvisionedAppxPackage",
                "/PackageName:Contoso.App_1.0.0.0_x64__publisher",
            ]
            .map(std::ffi::OsString::from)
            .as_slice()
        );
    }

    #[test]
    fn invalid_target_package_and_textual_dism_error_fail_closed() {
        assert!(matches!(
            normalize_offline_root("D:\\Windows"),
            Err(RemoveAppxError::InvalidOfflineTarget(_))
        ));
        assert!(!valid_package_name("Contoso.App & whoami"));
        let executor =
            lr_core::command::DryRunCommandExecutor::new(lr_core::command::CommandOutcome::new(
                Some(0),
                b"Error: 87\r\nThe parameter is incorrect.".to_vec(),
                Vec::new(),
            ));
        assert!(matches!(
            offline_inventory_with(&executor, "D:"),
            Err(RemoveAppxError::Inventory(_))
        ));
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn development_entry_rejects_before_constructing_the_windows_backend() {
        assert_eq!(
            execute(&online(&["Contoso.App_1.0_x64__test"])),
            Err(RemoveAppxError::DevelopmentBuildDenied)
        );
        assert_eq!(
            offline_inventory("D:"),
            Err(RemoveAppxError::DevelopmentBuildDenied)
        );
    }
}
