//! Native password-reset dialog preserving the legacy single-account workflow.
//!
//! It selects a typed current/offline target and one inventory-provided account. The operation is
//! fixed to clearing that password and enabling the account; no batch or enable-only option exists.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, DeleteObject, RedrawWindow, HFONT, RDW_INVALIDATE, RDW_NOERASE, RDW_UPDATENOW,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, MoveWindow, SendMessageW, SetWindowTextW, ShowWindow, CBS_DROPDOWNLIST,
    CB_ADDSTRING, CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL, LBS_NOINTEGRALHEIGHT, LBS_NOTIFY,
    LB_ADDSTRING, LB_GETCURSEL, LB_RESETCONTENT, LB_SETCURSEL, SW_HIDE, SW_SHOW, WM_SETFONT,
    WS_BORDER, WS_TABSTOP, WS_VSCROLL,
};

use super::super::controls::{child, combo_inventory_index, wide, NO_COMBO_SELECTION};
use super::super::dialog::{DialogButtons, DialogResult, DialogShell, DialogSpec};
use super::super::layout::{
    arrange_field, measure_text, preferred_list_height, FieldArrangement, LayoutMetrics,
};
use super::super::theme::{apply_control_theme, NativeControlKind, Palette};
use crate::core::native_password_reset::{
    validate_request, PasswordResetAccount, PasswordResetRequest, PasswordResetTarget,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PasswordResetTargetOption {
    pub target: PasswordResetTarget,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PasswordResetDialogIntent {
    LoadAccounts(PasswordResetTarget),
    ReloadTargets,
    RequestConfirmation(PasswordResetRequest),
    Close,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct State {
    targets: Vec<PasswordResetTargetOption>,
    selected_target: Option<PasswordResetTarget>,
    accounts: Vec<PasswordResetAccount>,
    selected_account: Option<String>,
    loading: bool,
    message: String,
}

#[derive(Clone, Copy)]
struct Controls {
    target_label: HWND,
    target_combo: HWND,
    account_label: HWND,
    account_list: HWND,
    fixed_action: HWND,
    warning: HWND,
    status: HWND,
}

pub struct NativePasswordResetDialog {
    pub shell: DialogShell,
    controls: Controls,
    state: State,
    font: HFONT,
}

impl NativePasswordResetDialog {
    pub unsafe fn create(
        owner: HWND,
        targets: Vec<PasswordResetTargetOption>,
    ) -> windows::core::Result<Self> {
        let shell = DialogShell::create(
            owner,
            DialogSpec {
                window_title: crate::tr!("密码重置"),
                title: crate::tr!("密码重置"),
                description: crate::tr!(
                    "清除 Windows 本地账户的密码（等效空密码），并启用被禁用的账户。"
                ),
                width: 660,
                height: 560,
                buttons: DialogButtons {
                    primary: crate::tr!("重置所选账户密码"),
                    secondary: Some(crate::tr!("刷新")),
                    cancel: Some(crate::tr!("关闭")),
                },
            },
        )?;
        let controls = create_controls(shell.content())?;
        let dpi = GetDpiForWindow(shell.hwnd()).max(96);
        let face = wide("Microsoft YaHei UI");
        let font = CreateFontW(
            -scale(14, dpi),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            1,
            0,
            0,
            5,
            0,
            PCWSTR(face.as_ptr()),
        );
        let mut dialog = Self {
            shell,
            controls,
            state: State::default(),
            font,
        };
        dialog.apply_font_and_theme();
        let _ = dialog.apply_targets(targets);
        Ok(dialog)
    }

    pub fn owns_target_combo(&self, control: HWND) -> bool {
        control == self.controls.target_combo
    }

    pub fn owns_account_list(&self, control: HWND) -> bool {
        control == self.controls.account_list
    }

    pub unsafe fn apply_targets(
        &mut self,
        targets: Vec<PasswordResetTargetOption>,
    ) -> Option<PasswordResetDialogIntent> {
        let previous = self.state.selected_target.clone();
        self.state.targets = targets;
        self.state.selected_target = previous
            .filter(|selected| {
                self.state
                    .targets
                    .iter()
                    .any(|option| &option.target == selected)
            })
            .or_else(|| {
                self.state
                    .targets
                    .first()
                    .map(|option| option.target.clone())
            });
        self.clear_accounts();
        refill_targets(self.controls.target_combo, &self.state);
        let reload = self.state.selected_target.clone();
        if reload.is_some() {
            self.state.loading = true;
            self.state.message = crate::tr!("正在读取账户列表...");
        }
        self.render_state();
        reload.map(PasswordResetDialogIntent::LoadAccounts)
    }

    /// Host hook for `CBN_SELCHANGE`; only requests read-only inventory.
    pub unsafe fn handle_target_changed(&mut self) -> Option<PasswordResetDialogIntent> {
        let index = SendMessageW(
            self.controls.target_combo,
            CB_GETCURSEL,
            WPARAM(0),
            LPARAM(0),
        )
        .0;
        self.state.selected_target = combo_inventory_index(index, self.state.targets.len())
            .and_then(|index| self.state.targets.get(index))
            .map(|option| option.target.clone());
        self.clear_accounts();
        let target = self.state.selected_target.clone()?;
        self.state.loading = true;
        self.state.message = crate::tr!("正在读取账户列表...");
        self.render_state();
        Some(PasswordResetDialogIntent::LoadAccounts(target))
    }

    /// Ignores stale async inventory belonging to a target the user has already left.
    pub unsafe fn apply_accounts(
        &mut self,
        target: &PasswordResetTarget,
        result: Result<Vec<PasswordResetAccount>, String>,
    ) {
        if self.state.selected_target.as_ref() != Some(target) {
            return;
        }
        self.state.loading = false;
        self.state.selected_account = None;
        match result {
            Ok(accounts) => {
                self.state.accounts = accounts;
                self.state.message = if self.state.accounts.is_empty() {
                    crate::tr!("该系统中未找到本地账户。")
                } else {
                    String::new()
                };
            }
            Err(error) => {
                self.state.accounts.clear();
                self.state.message = crate::tr!("读取账户列表失败：{}", error);
            }
        }
        refill_accounts(self.controls.account_list, &self.state);
        self.render_state();
    }

    pub unsafe fn set_busy(&mut self, message: String) {
        self.state.loading = true;
        self.state.message = message;
        self.render_state();
    }

    pub unsafe fn set_operation_result(&mut self, message: String) {
        self.state.loading = false;
        self.state.message = message;
        self.render_state();
    }

    /// Host hook for `LBN_SELCHANGE`; the list box is deliberately single-selection.
    pub unsafe fn handle_account_changed(&mut self) {
        let index = SendMessageW(
            self.controls.account_list,
            LB_GETCURSEL,
            WPARAM(0),
            LPARAM(0),
        )
        .0;
        self.state.selected_account = if index >= 0 {
            self.state
                .accounts
                .get(index as usize)
                .map(|account| account.username.clone())
        } else {
            None
        };
        self.state.message.clear();
        // Selection is already committed by the native ListBox.  Do not refit or move the dialog
        // here: doing so during LBN_SELCHANGE visibly shifted the first clicked row and exposed
        // the system-blue intermediate selection before our Inno row palette repainted it.
        self.shell
            .set_primary_enabled(self.request().is_ok() && !self.state.loading);
        set_text(self.controls.status, "");
        let _ = ShowWindow(self.controls.status, SW_HIDE);
        let _ = RedrawWindow(
            self.controls.account_list,
            None,
            None,
            RDW_INVALIDATE | RDW_NOERASE | RDW_UPDATENOW,
        );
    }

    pub unsafe fn show_modeless(&mut self) {
        self.fit_and_layout();
        self.shell.show_modeless();
    }

    pub unsafe fn take_intent(&mut self) -> Option<PasswordResetDialogIntent> {
        match self.shell.take_result()? {
            DialogResult::Secondary => Some(PasswordResetDialogIntent::ReloadTargets),
            DialogResult::Cancel => Some(PasswordResetDialogIntent::Close),
            DialogResult::Primary => match self.request() {
                Ok(request) => Some(PasswordResetDialogIntent::RequestConfirmation(request)),
                Err(error) => {
                    self.state.message = error;
                    self.render_state();
                    None
                }
            },
        }
    }

    pub unsafe fn layout(&self) {
        let dpi = GetDpiForWindow(self.shell.hwnd()).max(96);
        let mut rect = RECT::default();
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let width = (rect.right - rect.left).max(0);
        let height = (rect.bottom - rect.top).max(0);
        let metrics = LayoutMetrics::for_dpi(dpi);
        let target_label = crate::tr!("目标系统:");
        let target_label_width =
            measure_text(self.shell.hwnd(), self.font, &target_label, None).width;
        let combo_drop_height = metrics.field_height + metrics.list_row_height * 8;
        let mut y = match arrange_field(width, target_label_width, scale(240, dpi), dpi) {
            FieldArrangement::Inline {
                label_width,
                control_x,
                control_width,
            } => {
                let _ = MoveWindow(
                    self.controls.target_label,
                    0,
                    (metrics.field_height - metrics.label_height) / 2,
                    label_width,
                    metrics.label_height,
                    true,
                );
                let _ = MoveWindow(
                    self.controls.target_combo,
                    control_x,
                    0,
                    control_width,
                    combo_drop_height,
                    true,
                );
                metrics.field_height
            }
            FieldArrangement::Stacked => {
                let _ = MoveWindow(
                    self.controls.target_label,
                    0,
                    0,
                    width,
                    metrics.label_height,
                    true,
                );
                let combo_y = metrics.label_height + metrics.tight_gap;
                let _ = MoveWindow(
                    self.controls.target_combo,
                    0,
                    combo_y,
                    width,
                    combo_drop_height,
                    true,
                );
                combo_y + metrics.field_height
            }
        };
        y += metrics.section_gap;
        let account_label_height = measure_text(
            self.shell.hwnd(),
            self.font,
            &crate::tr!("选择要重置密码的账户:"),
            Some(width),
        )
        .height
        .max(metrics.label_height);
        let _ = MoveWindow(
            self.controls.account_label,
            0,
            y,
            width,
            account_label_height,
            true,
        );
        y += account_label_height + metrics.tight_gap;
        let fixed_action_height = measure_text(
            self.shell.hwnd(),
            self.font,
            &crate::tr!("固定操作：清空所选账户密码，并启用该账户。"),
            Some(width),
        )
        .height
        .max(metrics.label_height);
        let warning_height = measure_text(
            self.shell.hwnd(),
            self.font,
            &crate::tr!("仅用于自己的系统或已获授权的场景。离线系统会修改 SAM，并在修改前创建备份；当前系统使用 net user。"),
            Some(width),
        )
        .height
        .max(metrics.label_height);
        let status_height = if self.state.message.is_empty() {
            0
        } else {
            measure_text(
                self.shell.hwnd(),
                self.font,
                &self.state.message,
                Some(width),
            )
            .height
            .max(metrics.label_height)
        };
        let fixed_trailing = metrics.control_gap
            + fixed_action_height
            + metrics.control_gap
            + warning_height
            + if status_height > 0 {
                metrics.control_gap + status_height
            } else {
                0
            };
        let minimum_list = preferred_list_height(self.state.accounts.len(), dpi, 3, 8);
        let list_height = (height - y - fixed_trailing).max(minimum_list);
        let _ = MoveWindow(self.controls.account_list, 0, y, width, list_height, true);
        y += list_height + metrics.control_gap;
        let _ = MoveWindow(
            self.controls.fixed_action,
            0,
            y,
            width,
            fixed_action_height,
            true,
        );
        y += fixed_action_height + metrics.control_gap;
        let _ = MoveWindow(self.controls.warning, 0, y, width, warning_height, true);
        y += warning_height;
        let status_y = y + if status_height > 0 {
            metrics.control_gap
        } else {
            0
        };
        let _ = MoveWindow(
            self.controls.status,
            0,
            status_y,
            width,
            status_height,
            true,
        );
    }

    unsafe fn fit_and_layout(&mut self) {
        let dpi = GetDpiForWindow(self.shell.hwnd()).max(96);
        let mut rect = RECT::default();
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let width = (rect.right - rect.left).max(scale(320, dpi));
        let metrics = LayoutMetrics::for_dpi(dpi);
        let target_label_width =
            measure_text(self.shell.hwnd(), self.font, &crate::tr!("目标系统:"), None).width;
        let target_height = match arrange_field(width, target_label_width, scale(240, dpi), dpi) {
            FieldArrangement::Inline { .. } => metrics.field_height,
            FieldArrangement::Stacked => {
                metrics.label_height + metrics.tight_gap + metrics.field_height
            }
        };
        let account_label_height = measure_text(
            self.shell.hwnd(),
            self.font,
            &crate::tr!("选择要重置密码的账户:"),
            Some(width),
        )
        .height
        .max(metrics.label_height);
        let list_height = preferred_list_height(self.state.accounts.len(), dpi, 3, 8);
        let fixed_action_height = measure_text(
            self.shell.hwnd(),
            self.font,
            &crate::tr!("固定操作：清空所选账户密码，并启用该账户。"),
            Some(width),
        )
        .height
        .max(metrics.label_height);
        let warning_height = measure_text(
            self.shell.hwnd(),
            self.font,
            &crate::tr!("仅用于自己的系统或已获授权的场景。离线系统会修改 SAM，并在修改前创建备份；当前系统使用 net user。"),
            Some(width),
        )
        .height
        .max(metrics.label_height);
        let status_height = if self.state.message.is_empty() {
            0
        } else {
            metrics.control_gap
                + measure_text(
                    self.shell.hwnd(),
                    self.font,
                    &self.state.message,
                    Some(width),
                )
                .height
                .max(metrics.label_height)
        };
        let content_height = target_height
            + metrics.section_gap
            + account_label_height
            + metrics.tight_gap
            + list_height
            + metrics.control_gap
            + fixed_action_height
            + metrics.control_gap
            + warning_height
            + status_height;
        self.shell
            .fit_content_height(pixels_to_logical(content_height, dpi));
        self.layout();
    }

    fn request(&self) -> Result<PasswordResetRequest, String> {
        let target = self
            .state
            .selected_target
            .clone()
            .ok_or_else(|| crate::tr!("请先选择目标系统"))?;
        let account = self
            .state
            .selected_account
            .clone()
            .ok_or_else(|| crate::tr!("请先在列表中选择一个账户"))?;
        let request = PasswordResetRequest { target, account };
        validate_request(&request).map_err(|error| error.to_string())?;
        Ok(request)
    }

    unsafe fn clear_accounts(&mut self) {
        self.state.accounts.clear();
        self.state.selected_account = None;
        self.state.loading = false;
        self.state.message.clear();
        refill_accounts(self.controls.account_list, &self.state);
    }

    unsafe fn render_state(&mut self) {
        let _ = EnableWindow(
            self.controls.account_list,
            self.state.selected_target.is_some() && !self.state.loading,
        );
        self.shell
            .set_primary_enabled(self.request().is_ok() && !self.state.loading);
        set_text(self.controls.status, &self.state.message);
        let _ = ShowWindow(
            self.controls.status,
            if self.state.message.is_empty() {
                SW_HIDE
            } else {
                SW_SHOW
            },
        );
        self.fit_and_layout();
    }

    unsafe fn apply_font_and_theme(&self) {
        for control in self.controls() {
            let _ = SendMessageW(control, WM_SETFONT, WPARAM(self.font.0 as usize), LPARAM(1));
        }
        let palette = Palette::system();
        apply_control_theme(
            self.controls.target_combo,
            palette,
            NativeControlKind::Field,
        );
        apply_control_theme(self.controls.account_list, palette, NativeControlKind::List);
    }

    fn controls(&self) -> [HWND; 7] {
        let c = self.controls;
        [
            c.target_label,
            c.target_combo,
            c.account_label,
            c.account_list,
            c.fixed_action,
            c.warning,
            c.status,
        ]
    }
}

impl Drop for NativePasswordResetDialog {
    fn drop(&mut self) {
        unsafe {
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font);
            }
        }
    }
}

unsafe fn create_controls(parent: HWND) -> windows::core::Result<Controls> {
    Ok(Controls {
        target_label: child(parent, w!("STATIC"), &crate::tr!("目标系统:"), 0, 64_720)?,
        target_combo: child(
            parent,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            64_721,
        )?,
        account_label: child(
            parent,
            w!("STATIC"),
            &crate::tr!("选择要重置密码的账户:"),
            0,
            64_722,
        )?,
        account_list: child(
            parent,
            w!("LISTBOX"),
            "",
            WS_BORDER.0 as i32
                | WS_TABSTOP.0 as i32
                | WS_VSCROLL.0 as i32
                | LBS_NOTIFY
                | LBS_NOINTEGRALHEIGHT,
            64_723,
        )?,
        fixed_action: child(
            parent,
            w!("STATIC"),
            &crate::tr!("固定操作：清空所选账户密码，并启用该账户。"),
            0,
            64_724,
        )?,
        warning: child(
            parent,
            w!("STATIC"),
            &crate::tr!(
                "仅用于自己的系统或已获授权的场景。离线系统会修改 SAM，并在修改前创建备份；当前系统使用 net user。"
            ),
            0,
            64_725,
        )?,
        status: child(parent, w!("STATIC"), "", 0, 64_726)?,
    })
}

unsafe fn refill_targets(control: HWND, state: &State) {
    let _ = SendMessageW(control, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
    let mut selected = NO_COMBO_SELECTION;
    for (index, option) in state.targets.iter().enumerate() {
        add_string(control, CB_ADDSTRING, &option.label);
        if state.selected_target.as_ref() == Some(&option.target) {
            selected = index;
        }
    }
    let _ = SendMessageW(control, CB_SETCURSEL, WPARAM(selected), LPARAM(0));
}

unsafe fn refill_accounts(control: HWND, state: &State) {
    let _ = SendMessageW(control, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
    let mut selected = -1_isize;
    for (index, account) in state.accounts.iter().enumerate() {
        let label = if account.disabled {
            crate::tr!("{}（已禁用）", account.username)
        } else {
            account.username.clone()
        };
        add_string(control, LB_ADDSTRING, &label);
        if state.selected_account.as_deref() == Some(account.username.as_str()) {
            selected = index as isize;
        }
    }
    let _ = SendMessageW(control, LB_SETCURSEL, WPARAM(selected as usize), LPARAM(0));
}

unsafe fn add_string(control: HWND, message: u32, value: &str) {
    let value = wide(value);
    let _ = SendMessageW(control, message, WPARAM(0), LPARAM(value.as_ptr() as isize));
}

unsafe fn set_text(control: HWND, value: &str) {
    let value = wide(value);
    let _ = SetWindowTextW(control, PCWSTR(value.as_ptr()));
}

fn scale(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32
}

fn pixels_to_logical(value: i32, dpi: u32) -> i32 {
    ((i64::from(value.max(0)) * 96 + i64::from(dpi.max(1)) - 1) / i64::from(dpi.max(1))) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_can_only_build_one_account_request() {
        let state = State {
            selected_target: Some(PasswordResetTarget::CurrentSystem),
            selected_account: Some("Administrator".to_owned()),
            ..Default::default()
        };
        let request = PasswordResetRequest {
            target: state.selected_target.unwrap(),
            account: state.selected_account.unwrap(),
        };
        assert_eq!(validate_request(&request), Ok(()));
        assert_eq!(request.account, "Administrator");
    }

    #[test]
    fn changing_target_requires_a_fresh_single_account_selection() {
        let mut state = State {
            selected_target: Some(PasswordResetTarget::CurrentSystem),
            accounts: vec![PasswordResetAccount {
                username: "Administrator".to_owned(),
                disabled: true,
            }],
            selected_account: Some("Administrator".to_owned()),
            ..Default::default()
        };
        state.selected_target = Some(PasswordResetTarget::OfflineWindows("D:".to_owned()));
        state.accounts.clear();
        state.selected_account = None;
        assert!(state.accounts.is_empty());
        assert!(state.selected_account.is_none());
    }

    #[test]
    fn account_list_height_tracks_real_rows_with_bounded_density() {
        let empty = preferred_list_height(0, 96, 3, 8);
        let four = preferred_list_height(4, 96, 3, 8);
        let many = preferred_list_height(99, 96, 3, 8);
        assert!(empty < four && four < many);
        assert_eq!(empty, preferred_list_height(3, 96, 3, 8));
        assert_eq!(many, preferred_list_height(8, 96, 3, 8));
    }
}
