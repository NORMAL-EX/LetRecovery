//! Pure state and intent mapping for the dedicated native APPX-removal dialog.
//!
//! Inventory loading and removal remain outside this module. The state accepts only typed target
//! and package inventory entries, filters protected packages defensively, and emits requests for
//! the existing [`super::native_appx`] safety boundary.

use std::collections::BTreeSet;

use super::native_appx::{AppxTarget, RemoveAppxError, RemoveAppxRequest};
use super::native_tool_inventory::InventoryEntry;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppxSelectionAction {
    SelectAll,
    SelectNone,
    Invert,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeAppxDialogIntent {
    LoadPackages { inventory_target: String },
    RequestRemoval(RemoveAppxRequest),
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeAppxSelectionError {
    TargetInventoryLoading,
    PackageInventoryLoading,
    MissingTarget,
    StaleTarget,
    EmptySelection,
    StalePackage(String),
    Safety(RemoveAppxError),
}

impl std::fmt::Display for NativeAppxSelectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::TargetInventoryLoading => crate::tr!("正在检测 Windows 分区，请稍候"),
            Self::PackageInventoryLoading => crate::tr!("正在加载应用列表，请稍候"),
            Self::MissingTarget => crate::tr!("请先选择目标系统"),
            Self::StaleTarget => crate::tr!("所选目标系统已不可用，请重新选择"),
            Self::EmptySelection => crate::tr!("请先选择要移除的应用"),
            Self::StalePackage(package) => {
                crate::tr!("应用列表已变化，请刷新后重新选择：{}", package)
            }
            Self::Safety(RemoveAppxError::InvalidOfflineTarget(_)) => {
                crate::tr!("离线 Windows 目标无效，请重新选择")
            }
            Self::Safety(RemoveAppxError::CriticalPackage(_)) => {
                crate::tr!("所选列表包含受保护的系统关键应用")
            }
            Self::Safety(RemoveAppxError::EmptySelection) => {
                crate::tr!("请先选择要移除的应用")
            }
            Self::Safety(error) => crate::tr!("APPX 请求未通过安全校验：{}", error),
        };
        formatter.write_str(&message)
    }
}

impl std::error::Error for NativeAppxSelectionError {}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NativeAppxDialogState {
    pub is_pe_environment: bool,
    pub targets: Vec<InventoryEntry>,
    pub selected_target: Option<String>,
    pub packages: Vec<InventoryEntry>,
    pub(crate) selected_packages: BTreeSet<String>,
    pub pending_inventory_target: Option<String>,
    pub targets_loading: bool,
    pub packages_loading: bool,
    pub status: String,
}

impl NativeAppxDialogState {
    pub fn loading(is_pe_environment: bool, status: String) -> Self {
        Self {
            targets_loading: true,
            status,
            is_pe_environment,
            ..Self::default()
        }
    }

    pub fn selected_count(&self) -> usize {
        self.selected_packages.len()
    }

    pub fn is_selected(&self, package: &str) -> bool {
        self.selected_packages.contains(package)
    }

    pub fn set_targets(&mut self, targets: Vec<InventoryEntry>) {
        self.targets = sanitize_entries(targets, false);
        self.targets_loading = false;
        if self.selected_target_entry().is_none() {
            self.selected_target = self.targets.first().map(|entry| entry.value.clone());
            self.clear_packages();
        }
    }

    pub fn select_target(
        &mut self,
        target: Option<String>,
    ) -> Result<Option<NativeAppxDialogIntent>, NativeAppxSelectionError> {
        if self.targets_loading {
            return Err(NativeAppxSelectionError::TargetInventoryLoading);
        }
        self.selected_target = target;
        self.clear_packages();
        let Some(target) = self
            .selected_target_entry()
            .map(|entry| entry.value.clone())
        else {
            if self.selected_target.is_some() {
                self.selected_target = None;
                return Err(NativeAppxSelectionError::StaleTarget);
            }
            return Ok(None);
        };
        self.packages_loading = true;
        self.status = crate::tr!("正在加载应用列表...");
        let inventory_target = inventory_target(&target);
        self.pending_inventory_target = Some(inventory_target.clone());
        Ok(Some(NativeAppxDialogIntent::LoadPackages {
            inventory_target,
        }))
    }

    pub fn refresh_intent(&mut self) -> Result<NativeAppxDialogIntent, NativeAppxSelectionError> {
        let target = self
            .selected_target_entry()
            .map(|entry| entry.value.clone())
            .ok_or(NativeAppxSelectionError::MissingTarget)?;
        let inventory_target = inventory_target(&target);
        self.clear_packages();
        self.packages_loading = true;
        self.status = crate::tr!("正在加载应用列表...");
        self.pending_inventory_target = Some(inventory_target.clone());
        Ok(NativeAppxDialogIntent::LoadPackages { inventory_target })
    }

    pub fn apply_package_inventory(
        &mut self,
        inventory_target: &str,
        result: Result<Vec<InventoryEntry>, String>,
    ) -> bool {
        if !self
            .pending_inventory_target
            .as_deref()
            .is_some_and(|pending| pending.eq_ignore_ascii_case(inventory_target))
        {
            return false;
        }
        self.pending_inventory_target = None;
        self.packages_loading = false;
        self.selected_packages.clear();
        match result {
            Ok(packages) => {
                self.packages = sanitize_entries(packages, true);
                self.status = if self.packages.is_empty() {
                    crate::tr!("未找到可移除的应用")
                } else {
                    String::new()
                };
            }
            Err(error) => {
                self.packages.clear();
                self.status = crate::tr!("加载应用列表失败：{}", error);
            }
        }
        true
    }

    pub fn set_package_selected(&mut self, package: &str, selected: bool) {
        if !self.packages.iter().any(|entry| entry.value == package) {
            return;
        }
        if selected {
            self.selected_packages.insert(package.to_owned());
        } else {
            self.selected_packages.remove(package);
        }
    }

    pub fn apply_selection(&mut self, action: AppxSelectionAction) {
        match action {
            AppxSelectionAction::SelectAll => {
                self.selected_packages = self
                    .packages
                    .iter()
                    .map(|entry| entry.value.clone())
                    .collect();
            }
            AppxSelectionAction::SelectNone => self.selected_packages.clear(),
            AppxSelectionAction::Invert => {
                self.selected_packages = self
                    .packages
                    .iter()
                    .filter(|entry| !self.selected_packages.contains(&entry.value))
                    .map(|entry| entry.value.clone())
                    .collect();
            }
        }
    }

    pub fn removal_intent(&self) -> Result<NativeAppxDialogIntent, NativeAppxSelectionError> {
        if self.targets_loading {
            return Err(NativeAppxSelectionError::TargetInventoryLoading);
        }
        if self.packages_loading {
            return Err(NativeAppxSelectionError::PackageInventoryLoading);
        }
        let target = self.selected_target_entry().ok_or_else(|| {
            if self.selected_target.is_some() {
                NativeAppxSelectionError::StaleTarget
            } else {
                NativeAppxSelectionError::MissingTarget
            }
        })?;
        if self.selected_packages.is_empty() {
            return Err(NativeAppxSelectionError::EmptySelection);
        }
        let mut packages = Vec::with_capacity(self.selected_packages.len());
        for entry in &self.packages {
            if self.selected_packages.contains(&entry.value) {
                packages.push(entry.value.clone());
            }
        }
        if packages.len() != self.selected_packages.len() {
            let stale = self
                .selected_packages
                .iter()
                .find(|package| !packages.contains(package))
                .cloned()
                .unwrap_or_default();
            return Err(NativeAppxSelectionError::StalePackage(stale));
        }
        let request = RemoveAppxRequest {
            target: appx_target(&target.value).ok_or(NativeAppxSelectionError::StaleTarget)?,
            packages,
        };
        super::native_appx::validate_request(&request).map_err(NativeAppxSelectionError::Safety)?;
        Ok(NativeAppxDialogIntent::RequestRemoval(request))
    }

    fn selected_target_entry(&self) -> Option<&InventoryEntry> {
        let selected = self.selected_target.as_deref()?;
        self.targets
            .iter()
            .find(|entry| entry.value.eq_ignore_ascii_case(selected))
    }

    fn clear_packages(&mut self) {
        self.packages.clear();
        self.selected_packages.clear();
        self.pending_inventory_target = None;
        self.packages_loading = false;
        self.status.clear();
    }
}

fn sanitize_entries(entries: Vec<InventoryEntry>, filter_critical: bool) -> Vec<InventoryEntry> {
    let mut seen = BTreeSet::new();
    entries
        .into_iter()
        .filter(|entry| {
            let value = entry.value.trim();
            !value.is_empty()
                && (!filter_critical || !super::native_appx_legacy::is_critical(value))
                && seen.insert(value.to_ascii_lowercase())
        })
        .collect()
}

fn appx_target(value: &str) -> Option<AppxTarget> {
    if matches!(value, "当前系统" | "__CURRENT__" | "__ONLINE__") {
        return Some(AppxTarget::CurrentSystem);
    }
    matches!(value.trim().as_bytes(), [letter, b':'] if letter.is_ascii_alphabetic())
        .then(|| AppxTarget::OfflineWindows(value.trim().to_ascii_uppercase()))
}

fn inventory_target(value: &str) -> String {
    match appx_target(value) {
        Some(AppxTarget::CurrentSystem) => "__CURRENT__".to_owned(),
        Some(AppxTarget::OfflineWindows(partition)) => partition,
        None => value.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(value: &str, label: &str) -> InventoryEntry {
        InventoryEntry {
            value: value.to_owned(),
            label: label.to_owned(),
            disk_fingerprint: None,
        }
    }

    fn online_state() -> NativeAppxDialogState {
        let mut state = NativeAppxDialogState::default();
        state.set_targets(vec![
            entry("当前系统", "当前系统"),
            entry("D:", "D: [Windows]"),
        ]);
        state.select_target(Some("当前系统".to_owned())).unwrap();
        assert!(state.apply_package_inventory(
            "__CURRENT__",
            Ok(vec![
                entry("Contoso.One_1.0_x64__test", "Contoso One"),
                entry("Contoso.Two_1.0_x64__test", "Contoso Two"),
            ])
        ));
        state
    }

    #[test]
    fn first_safe_target_is_selected_by_default_and_requests_dynamic_inventory() {
        let mut state = NativeAppxDialogState::default();
        state.set_targets(vec![entry("当前系统", "当前系统")]);
        assert_eq!(state.selected_target.as_deref(), Some("当前系统"));
        assert_eq!(
            state.select_target(Some("当前系统".to_owned())).unwrap(),
            Some(NativeAppxDialogIntent::LoadPackages {
                inventory_target: "__CURRENT__".to_owned()
            })
        );
        assert!(state.packages_loading);
    }

    #[test]
    fn all_none_and_invert_preserve_inventory_values_and_count() {
        let mut state = online_state();
        state.apply_selection(AppxSelectionAction::SelectAll);
        assert_eq!(state.selected_count(), 2);
        state.apply_selection(AppxSelectionAction::Invert);
        assert_eq!(state.selected_count(), 0);
        state.set_package_selected("Contoso.One_1.0_x64__test", true);
        state.apply_selection(AppxSelectionAction::Invert);
        assert_eq!(state.selected_count(), 1);
        assert!(state.is_selected("Contoso.Two_1.0_x64__test"));
        state.apply_selection(AppxSelectionAction::SelectNone);
        assert_eq!(state.selected_count(), 0);
    }

    #[test]
    fn critical_and_duplicate_inventory_entries_never_reach_selection() {
        let mut state = online_state();
        state.refresh_intent().unwrap();
        assert!(state.apply_package_inventory(
            "__CURRENT__",
            Ok(vec![
                entry("Microsoft.WindowsStore_1.0_x64__test", "Store"),
                entry("Contoso.One_1.0_x64__test", "One"),
                entry("contoso.one_1.0_x64__test", "Duplicate"),
            ])
        ));
        assert_eq!(state.packages.len(), 1);
        assert_eq!(state.packages[0].label, "One");
    }

    #[test]
    fn online_and_offline_selection_build_safe_inventory_backed_requests() {
        let mut state = online_state();
        state.set_package_selected("Contoso.One_1.0_x64__test", true);
        assert!(matches!(
            state.removal_intent(),
            Ok(NativeAppxDialogIntent::RequestRemoval(RemoveAppxRequest {
                target: AppxTarget::CurrentSystem,
                ..
            }))
        ));

        state.select_target(Some("D:".to_owned())).unwrap();
        assert!(state.apply_package_inventory(
            "D:",
            Ok(vec![entry("Contoso.One_1.0_x64__test", "Contoso One")])
        ));
        state.set_package_selected("Contoso.One_1.0_x64__test", true);
        assert!(matches!(
            state.removal_intent(),
            Ok(NativeAppxDialogIntent::RequestRemoval(RemoveAppxRequest {
                target: AppxTarget::OfflineWindows(ref root),
                ..
            })) if root == "D:"
        ));
    }

    #[test]
    fn refresh_clears_stale_package_selection() {
        let mut state = online_state();
        state.apply_selection(AppxSelectionAction::SelectAll);
        let intent = state.refresh_intent().unwrap();
        assert_eq!(state.selected_count(), 0);
        assert!(state.packages.is_empty());
        assert_eq!(
            intent,
            NativeAppxDialogIntent::LoadPackages {
                inventory_target: "__CURRENT__".to_owned()
            }
        );
    }

    #[test]
    fn stale_inventory_result_from_previous_target_is_ignored() {
        let mut state = NativeAppxDialogState::default();
        state.set_targets(vec![entry("当前系统", "当前系统"), entry("D:", "D:")]);
        state.select_target(Some("当前系统".to_owned())).unwrap();
        state.select_target(Some("D:".to_owned())).unwrap();
        assert!(!state.apply_package_inventory(
            "__CURRENT__",
            Ok(vec![entry("Contoso.Stale_1.0_x64__test", "Stale")])
        ));
        assert!(state.packages.is_empty());
        assert_eq!(state.pending_inventory_target.as_deref(), Some("D:"));
    }
}
