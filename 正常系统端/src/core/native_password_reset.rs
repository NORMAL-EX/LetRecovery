//! Strongly typed native password-reset boundary.
//!
//! The legacy workflow selects exactly one local account from either the running Windows system
//! or one offline Windows partition. Its operation is fixed: clear that account's password and
//! enable the account. No batch or password-only mode is represented by these types.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PasswordResetTarget {
    CurrentSystem,
    OfflineWindows(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PasswordResetAccount {
    pub username: String,
    pub disabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PasswordResetRequest {
    pub target: PasswordResetTarget,
    /// Exactly one inventory-selected local account. Batch semantics are intentionally absent.
    pub account: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PasswordResetResult {
    pub target: PasswordResetTarget,
    pub account: String,
    pub password_cleared: bool,
    pub account_enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativePasswordResetError {
    DevelopmentBuildDenied,
    InvalidTarget(String),
    InvalidAccount,
    AccountNotFound(String),
    Inventory(String),
    Execution(String),
}

impl std::fmt::Display for NativePasswordResetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::DevelopmentBuildDenied => crate::tr!("开发测试构建禁止读取或修改宿主账户"),
            Self::InvalidTarget(target) => crate::tr!("无效的密码重置目标: {}", target),
            Self::InvalidAccount => crate::tr!("无效的本地账户名"),
            Self::AccountNotFound(account) => crate::tr!("未找到所选本地账户: {}", account),
            Self::Inventory(detail) => crate::tr!("读取本地账户失败: {}", detail),
            Self::Execution(detail) => crate::tr!("密码重置失败: {}", detail),
        };
        formatter.write_str(&message)
    }
}

impl std::error::Error for NativePasswordResetError {}

pub fn validate_request(request: &PasswordResetRequest) -> Result<(), NativePasswordResetError> {
    validate_target(&request.target)?;
    validate_account_name(&request.account)
}

#[cfg(feature = "non-elevated-tests")]
pub fn load_password_reset_accounts(
    _target: &PasswordResetTarget,
) -> Result<Vec<PasswordResetAccount>, NativePasswordResetError> {
    Err(NativePasswordResetError::DevelopmentBuildDenied)
}

#[cfg(not(feature = "non-elevated-tests"))]
pub fn load_password_reset_accounts(
    target: &PasswordResetTarget,
) -> Result<Vec<PasswordResetAccount>, NativePasswordResetError> {
    validate_target(target)?;
    match target {
        PasswordResetTarget::CurrentSystem => load_current_accounts_with(),
        PasswordResetTarget::OfflineWindows(partition) => lr_core::sam::list_accounts(partition)
            .map_err(|error| NativePasswordResetError::Inventory(error.to_string()))
            .map(|accounts| {
                accounts
                    .into_iter()
                    .map(|account| PasswordResetAccount {
                        username: account.username,
                        disabled: account.disabled,
                    })
                    .collect()
            }),
    }
}

#[cfg(feature = "non-elevated-tests")]
pub fn execute_password_reset(
    _request: &PasswordResetRequest,
) -> Result<PasswordResetResult, NativePasswordResetError> {
    Err(NativePasswordResetError::DevelopmentBuildDenied)
}

#[cfg(not(feature = "non-elevated-tests"))]
pub fn execute_password_reset(
    request: &PasswordResetRequest,
) -> Result<PasswordResetResult, NativePasswordResetError> {
    validate_request(request)?;
    let account = request.account.trim();
    let available = load_password_reset_accounts(&request.target)?;
    if !available
        .iter()
        .any(|candidate| candidate.username.eq_ignore_ascii_case(account))
    {
        return Err(NativePasswordResetError::AccountNotFound(
            account.to_owned(),
        ));
    }

    match &request.target {
        PasswordResetTarget::CurrentSystem => {
            clear_and_enable_current_account(account)?;
        }
        PasswordResetTarget::OfflineWindows(partition) => {
            let changed = lr_core::sam::clear_account_password(partition, account)
                .map_err(|error| NativePasswordResetError::Execution(error.to_string()))?;
            if !changed {
                return Err(NativePasswordResetError::AccountNotFound(
                    account.to_owned(),
                ));
            }
        }
    }

    Ok(PasswordResetResult {
        target: request.target.clone(),
        account: account.to_owned(),
        password_cleared: true,
        account_enabled: true,
    })
}

fn validate_target(target: &PasswordResetTarget) -> Result<(), NativePasswordResetError> {
    match target {
        PasswordResetTarget::CurrentSystem => Ok(()),
        PasswordResetTarget::OfflineWindows(partition) if matches!(partition.trim().as_bytes(), [letter, b':'] if letter.is_ascii_alphabetic()) => {
            Ok(())
        }
        PasswordResetTarget::OfflineWindows(partition) => {
            Err(NativePasswordResetError::InvalidTarget(partition.clone()))
        }
    }
}

fn validate_account_name(account: &str) -> Result<(), NativePasswordResetError> {
    let account = account.trim();
    if account.is_empty() || account.chars().any(|character| character.is_control()) {
        Err(NativePasswordResetError::InvalidAccount)
    } else {
        Ok(())
    }
}

#[cfg(not(feature = "non-elevated-tests"))]
fn load_current_accounts_with() -> Result<Vec<PasswordResetAccount>, NativePasswordResetError> {
    lr_core::windows_accounts::list_local_accounts()
        .map_err(|error| NativePasswordResetError::Inventory(error.to_string()))
        .map(|accounts| {
            accounts
                .into_iter()
                .map(|account| PasswordResetAccount {
                    username: account.name,
                    disabled: account.disabled,
                })
                .collect()
        })
}

#[cfg(not(feature = "non-elevated-tests"))]
fn clear_and_enable_current_account(account: &str) -> Result<(), NativePasswordResetError> {
    lr_core::windows_accounts::clear_password_and_enable(account)
        .map_err(|error| NativePasswordResetError::Execution(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_has_exactly_one_account_and_fixed_semantics() {
        let request = PasswordResetRequest {
            target: PasswordResetTarget::CurrentSystem,
            account: "Administrator".to_owned(),
        };
        assert_eq!(validate_request(&request), Ok(()));
        assert_eq!(request.account, "Administrator");
    }

    #[test]
    fn invalid_offline_target_and_account_fail_closed() {
        let invalid_target = PasswordResetRequest {
            target: PasswordResetTarget::OfflineWindows("Windows".to_owned()),
            account: "Administrator".to_owned(),
        };
        assert!(matches!(
            validate_request(&invalid_target),
            Err(NativePasswordResetError::InvalidTarget(_))
        ));
        let invalid_account = PasswordResetRequest {
            target: PasswordResetTarget::CurrentSystem,
            account: "\r\n".to_owned(),
        };
        assert_eq!(
            validate_request(&invalid_account),
            Err(NativePasswordResetError::InvalidAccount)
        );
    }

    #[cfg(feature = "non-elevated-tests")]
    #[test]
    fn development_build_denies_inventory_and_execution_before_host_io() {
        let request = PasswordResetRequest {
            target: PasswordResetTarget::CurrentSystem,
            account: "Administrator".to_owned(),
        };
        assert_eq!(
            load_password_reset_accounts(&request.target),
            Err(NativePasswordResetError::DevelopmentBuildDenied)
        );
        assert_eq!(
            execute_password_reset(&request),
            Err(NativePasswordResetError::DevelopmentBuildDenied)
        );
    }
}
