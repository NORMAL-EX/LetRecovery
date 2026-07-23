//! Dedicated native UI for the complete legacy BitLocker-management workflow.
//!
//! The dialog consumes caller-supplied read-only inventory and returns typed intents. It never
//! creates a BitLocker manager, reads a recovery protector, writes a file, or changes protection.

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontW, DeleteObject, HFONT};
use windows::Win32::UI::Controls::{
    LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_TEXT, LVITEMW, LVM_DELETEALLITEMS, LVM_GETNEXTITEM,
    LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETBKCOLOR, LVM_SETCOLUMNWIDTH,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMSTATE, LVM_SETITEMTEXTW, LVM_SETTEXTBKCOLOR,
    LVM_SETTEXTCOLOR, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_EX_INFOTIP, LVS_REPORT,
    LVS_SHOWSELALWAYS,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetWindowTextLengthW, GetWindowTextW, MoveWindow, SendMessageW, SetWindowTextW,
    ShowWindow, BS_OWNERDRAW, CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL, CB_RESETCONTENT,
    CB_SETCURSEL, ES_AUTOHSCROLL, ES_PASSWORD, ES_READONLY, SW_HIDE, SW_SHOW, WM_SETFONT,
    WS_BORDER, WS_TABSTOP,
};

use super::super::controls::{child, wide};
use super::super::dialog::{DialogButtons, DialogResult, DialogShell, DialogSpec};
use super::super::layout::{
    measure_text, measured_button_width, preferred_list_height, LayoutMetrics,
};
use super::super::theme::{apply_control_theme, apply_list_view_theme, NativeControlKind, Palette};
use crate::core::bitlocker::VolumeStatus;
use crate::core::native_bitlocker_manage::{
    build_intent, BitLockerManageAction, BitLockerManageIntent, BitLockerManageVolume,
    BitLockerUnlockMethod,
};

pub const ID_EXPORT_RECOVERY: u16 = 65_200;
pub const ID_HIDE_RECOVERY: u16 = 65_201;
const ID_VOLUME_LIST: u16 = 65_202;
const ID_ACTION_COMBO: u16 = 65_203;
const ID_METHOD_COMBO: u16 = 65_204;
const ID_CREDENTIAL: u16 = 65_205;
const ID_RECOVERY_VALUE: u16 = 65_206;
const LVNI_SELECTED: isize = 0x0002;
const LVIS_SELECTED: u32 = 0x0002;
const LVIS_FOCUSED: u32 = 0x0001;
const EM_SETPASSWORDCHAR: u32 = 0x00CC;

#[derive(Clone, PartialEq, Eq)]
pub struct DisplayedRecoveryKey(String);

impl DisplayedRecoveryKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for DisplayedRecoveryKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DisplayedRecoveryKey(<redacted>)")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BitLockerManageDialogIntent {
    RefreshInventory,
    RequestOperation(BitLockerManageIntent),
    ExportRecoveryKey(DisplayedRecoveryKey),
    Close,
}

#[derive(Clone, PartialEq, Eq)]
pub struct BitLockerManageDialogState {
    pub loading: bool,
    pub running: bool,
    pub volumes: Vec<BitLockerManageVolume>,
    pub selected_volume: Option<String>,
    pub action: BitLockerManageAction,
    pub unlock_method: BitLockerUnlockMethod,
    credential: String,
    pub recovery_key: Option<DisplayedRecoveryKey>,
    pub message: String,
}

impl std::fmt::Debug for BitLockerManageDialogState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BitLockerManageDialogState")
            .field("loading", &self.loading)
            .field("running", &self.running)
            .field("volumes", &self.volumes)
            .field("selected_volume", &self.selected_volume)
            .field("action", &self.action)
            .field("unlock_method", &self.unlock_method)
            .field("credential", &"<redacted>")
            .field("recovery_key", &self.recovery_key)
            .field("message", &self.message)
            .finish()
    }
}

impl Default for BitLockerManageDialogState {
    fn default() -> Self {
        Self {
            loading: true,
            running: false,
            volumes: Vec::new(),
            selected_volume: None,
            action: BitLockerManageAction::Unlock,
            unlock_method: BitLockerUnlockMethod::Password,
            credential: String::new(),
            recovery_key: None,
            message: crate::tr!("正在检测 BitLocker 分区..."),
        }
    }
}

impl BitLockerManageDialogState {
    pub fn apply_inventory(&mut self, result: Result<Vec<BitLockerManageVolume>, String>) {
        self.loading = false;
        self.running = false;
        match result {
            Ok(volumes) => {
                let previous = self.selected_volume.clone();
                self.volumes = sanitize_inventory(volumes);
                self.selected_volume = previous
                    .and_then(|drive| inventory_drive(&self.volumes, &drive))
                    .or_else(|| self.volumes.first().map(|volume| volume.drive.clone()));
                self.reset_conditions();
                self.message = if self.volumes.is_empty() {
                    crate::tr!("未检测到 BitLocker 加密分区")
                } else {
                    String::new()
                };
            }
            Err(error) => {
                self.volumes.clear();
                self.selected_volume = None;
                self.reset_conditions();
                self.message = crate::tr!("加载失败：{}", error);
            }
        }
    }

    pub fn begin_refresh(&mut self) {
        self.loading = true;
        self.message = crate::tr!("正在检测 BitLocker 分区...");
    }

    pub fn select_volume(&mut self, drive: Option<&str>) {
        self.selected_volume = drive.and_then(|drive| inventory_drive(&self.volumes, drive));
        self.reset_conditions();
        self.message.clear();
    }

    pub fn set_action(&mut self, action: BitLockerManageAction) {
        if self.available_actions().contains(&action) {
            self.action = action;
            if action != BitLockerManageAction::ReadRecoveryKey {
                self.recovery_key = None;
            }
        }
    }

    pub fn set_unlock_method(&mut self, method: BitLockerUnlockMethod) {
        self.unlock_method = method;
        self.credential.clear();
    }

    pub fn set_credential(&mut self, value: String) {
        self.credential = value;
    }

    pub fn available_actions(&self) -> Vec<BitLockerManageAction> {
        match self.selected_status() {
            Some(VolumeStatus::EncryptedLocked) => vec![BitLockerManageAction::Unlock],
            Some(VolumeStatus::EncryptedUnlocked) => vec![
                BitLockerManageAction::Decrypt,
                BitLockerManageAction::ReadRecoveryKey,
                BitLockerManageAction::SuspendProtection,
                BitLockerManageAction::ResumeProtection,
            ],
            _ => Vec::new(),
        }
    }

    pub fn selected_status(&self) -> Option<VolumeStatus> {
        let selected = self.selected_volume.as_deref()?;
        self.volumes
            .iter()
            .find(|volume| volume.drive.eq_ignore_ascii_case(selected))
            .map(|volume| volume.status)
    }

    pub fn operation(&self) -> Result<BitLockerManageIntent, String> {
        if self.loading || self.running {
            return Err(crate::tr!("正在执行操作..."));
        }
        let volume = self
            .selected_volume
            .as_deref()
            .ok_or_else(|| crate::tr!("请选择一个分区进行操作。"))?;
        build_intent(
            &self.volumes,
            volume,
            self.action,
            self.unlock_method,
            self.credential.clone(),
        )
        .map_err(|error| error.to_string())
    }

    pub fn set_recovery_key(&mut self, result: Result<String, String>) {
        self.running = false;
        match result {
            Ok(key) => {
                self.recovery_key = Some(DisplayedRecoveryKey::new(key));
                self.message = crate::tr!("已读取恢复密钥");
            }
            Err(error) => {
                self.recovery_key = None;
                self.message = crate::tr!("读取恢复密钥失败: {}", error);
            }
        }
    }

    fn reset_conditions(&mut self) {
        self.action = match self.selected_status() {
            Some(VolumeStatus::EncryptedUnlocked) => BitLockerManageAction::Decrypt,
            _ => BitLockerManageAction::Unlock,
        };
        self.unlock_method = BitLockerUnlockMethod::Password;
        self.credential.clear();
        self.recovery_key = None;
    }
}

fn sanitize_inventory(volumes: Vec<BitLockerManageVolume>) -> Vec<BitLockerManageVolume> {
    let mut seen = std::collections::BTreeSet::new();
    volumes
        .into_iter()
        .filter_map(|mut volume| {
            volume.drive = canonical_drive(&volume.drive)?;
            (volume.drive != "X:"
                && volume.status != VolumeStatus::NotEncrypted
                && seen.insert(volume.drive.clone()))
            .then_some(volume)
        })
        .collect()
}

fn canonical_drive(value: &str) -> Option<String> {
    match value.as_bytes() {
        [letter, b':'] if letter.is_ascii_alphabetic() => {
            Some(format!("{}:", (*letter as char).to_ascii_uppercase()))
        }
        _ => None,
    }
}

fn inventory_drive(volumes: &[BitLockerManageVolume], drive: &str) -> Option<String> {
    let drive = canonical_drive(drive)?;
    volumes
        .iter()
        .find(|volume| volume.drive.eq_ignore_ascii_case(&drive))
        .map(|volume| volume.drive.clone())
}

#[derive(Clone, Copy)]
struct Controls {
    volumes: HWND,
    state_text: HWND,
    action_label: HWND,
    action: HWND,
    method_label: HWND,
    method: HWND,
    credential_label: HWND,
    credential: HWND,
    warning: HWND,
    recovery_label: HWND,
    recovery_value: HWND,
    export_recovery: HWND,
    hide_recovery: HWND,
    message: HWND,
}

pub struct NativeBitLockerManageDialog {
    pub shell: DialogShell,
    controls: Controls,
    state: BitLockerManageDialogState,
    font: HFONT,
}

impl NativeBitLockerManageDialog {
    pub unsafe fn create(owner: HWND) -> windows::core::Result<Self> {
        let shell = DialogShell::create(
            owner,
            DialogSpec {
                window_title: crate::tr!("BitLocker管理"),
                title: crate::tr!("BitLocker管理"),
                description: crate::tr!("管理本机 BitLocker 加密分区：解锁已锁定的分区，或彻底关闭（解密）已解锁的分区。"),
                width: 760,
                height: 620,
                buttons: DialogButtons {
                    primary: crate::tr!("执行所选操作"),
                    secondary: Some(crate::tr!("刷新")),
                    cancel: Some(crate::tr!("关闭")),
                },
            },
        )?;
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
        let controls = create_controls(shell.content())?;
        let mut dialog = Self {
            shell,
            controls,
            state: BitLockerManageDialogState::default(),
            font,
        };
        dialog.apply_font_and_theme();
        dialog.layout();
        dialog.render_state();
        Ok(dialog)
    }

    pub fn state(&self) -> &BitLockerManageDialogState {
        &self.state
    }

    pub fn owns_list(&self, control: HWND) -> bool {
        control == self.controls.volumes
    }

    pub fn owns_choice(&self, control: HWND) -> bool {
        control == self.controls.action || control == self.controls.method
    }

    pub fn owns_credential(&self, control: HWND) -> bool {
        control == self.controls.credential
    }

    pub fn owns_command(command_id: u16) -> bool {
        matches!(command_id, ID_EXPORT_RECOVERY | ID_HIDE_RECOVERY)
    }

    pub unsafe fn handle_list_changed(&mut self) {
        let selected = selected_drive(self.controls.volumes, &self.state.volumes);
        self.state.select_volume(selected.as_deref());
        self.render_conditions();
    }

    pub unsafe fn handle_choice_changed(&mut self, control: HWND) {
        if control == self.controls.action {
            let index = combo_index(control);
            if let Some(action) = self.state.available_actions().get(index).copied() {
                self.state.set_action(action);
            }
        } else if control == self.controls.method {
            self.state.set_unlock_method(if combo_index(control) == 1 {
                BitLockerUnlockMethod::RecoveryKey
            } else {
                BitLockerUnlockMethod::Password
            });
        }
        self.render_conditions();
    }

    pub unsafe fn handle_credential_changed(&mut self) {
        self.state
            .set_credential(window_text(self.controls.credential));
        self.render_enablement();
    }

    pub unsafe fn handle_command(
        &mut self,
        command_id: u16,
    ) -> Option<BitLockerManageDialogIntent> {
        match command_id {
            ID_EXPORT_RECOVERY => self
                .state
                .recovery_key
                .clone()
                .map(BitLockerManageDialogIntent::ExportRecoveryKey),
            ID_HIDE_RECOVERY => {
                self.state.recovery_key = None;
                self.render_conditions();
                None
            }
            _ => None,
        }
    }

    pub unsafe fn set_inventory(&mut self, result: Result<Vec<BitLockerManageVolume>, String>) {
        self.state.apply_inventory(result);
        self.render_state();
    }

    pub unsafe fn set_running(&mut self, message: impl Into<String>) {
        self.state.running = true;
        if self.state.action == BitLockerManageAction::ReadRecoveryKey {
            self.state.recovery_key = None;
        }
        self.state.message = message.into();
        self.render_conditions();
        set_text(self.controls.message, &self.state.message);
    }

    pub unsafe fn set_operation_result(&mut self, message: impl Into<String>) {
        self.state.running = false;
        self.state.message = message.into();
        self.render_enablement();
        set_text(self.controls.message, &self.state.message);
    }

    pub unsafe fn set_recovery_key(&mut self, result: Result<String, String>) {
        self.state.set_recovery_key(result);
        self.render_conditions();
    }

    pub unsafe fn show_modeless(&mut self) {
        self.layout();
        self.shell.show_modeless();
    }

    pub unsafe fn take_intent(&mut self) -> Option<BitLockerManageDialogIntent> {
        match self.shell.take_result()? {
            DialogResult::Cancel => Some(BitLockerManageDialogIntent::Close),
            DialogResult::Secondary if !self.state.running => {
                self.state.begin_refresh();
                self.render_enablement();
                set_text(self.controls.message, &self.state.message);
                Some(BitLockerManageDialogIntent::RefreshInventory)
            }
            DialogResult::Secondary => None,
            DialogResult::Primary => {
                self.handle_credential_changed();
                match self.state.operation() {
                    Ok(intent) => Some(BitLockerManageDialogIntent::RequestOperation(intent)),
                    Err(error) => {
                        self.state.message = error;
                        set_text(self.controls.message, &self.state.message);
                        None
                    }
                }
            }
        }
    }

    pub unsafe fn layout(&mut self) {
        let dpi = GetDpiForWindow(self.shell.hwnd()).max(96);
        let mut rect = RECT::default();
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let width = (rect.right - rect.left).max(0);
        let metrics = LayoutMetrics::for_dpi(dpi);
        let list_height = preferred_list_height(self.state.volumes.len(), dpi, 3, 8);
        move_control(self.controls.volumes, 0, 0, width, list_height);
        let mut y = list_height + metrics.control_gap;
        let state_height = measure_text(
            self.shell.hwnd(),
            self.font,
            &window_text(self.controls.state_text),
            Some(width),
        )
        .height
        .max(metrics.label_height);
        move_control(self.controls.state_text, 0, y, width, state_height);
        y += state_height + metrics.section_gap;
        let label_width = [
            self.controls.action_label,
            self.controls.method_label,
            self.controls.credential_label,
        ]
        .iter()
        .map(|control| {
            measure_text(self.shell.hwnd(), self.font, &window_text(*control), None).width
        })
        .max()
        .unwrap_or(0)
        .min(width / 3);
        let value_x = label_width + metrics.control_gap;
        let value_width = (width - value_x).max(0);
        move_control(
            self.controls.action_label,
            0,
            y + ((metrics.field_height - metrics.label_height) / 2).max(0),
            label_width,
            metrics.label_height,
        );
        move_control(
            self.controls.action,
            value_x,
            y,
            value_width,
            scale(220, dpi),
        );
        y += metrics.field_height + metrics.control_gap;
        let locked = self.state.selected_status() == Some(VolumeStatus::EncryptedLocked);
        let unlocked = self.state.selected_status() == Some(VolumeStatus::EncryptedUnlocked);
        if locked {
            for (label, control) in [
                (self.controls.method_label, self.controls.method),
                (self.controls.credential_label, self.controls.credential),
            ] {
                move_control(
                    label,
                    0,
                    y + ((metrics.field_height - metrics.label_height) / 2).max(0),
                    label_width,
                    metrics.label_height,
                );
                let control_height = if control == self.controls.method {
                    scale(220, dpi)
                } else {
                    metrics.field_height
                };
                move_control(control, value_x, y, value_width, control_height);
                y += metrics.field_height + metrics.control_gap;
            }
        }
        let recovery_visible = self.state.recovery_key.is_some();
        if unlocked && !recovery_visible {
            let warning_height = measure_text(
                self.shell.hwnd(),
                self.font,
                &window_text(self.controls.warning),
                Some(width),
            )
            .height
            .max(metrics.label_height);
            move_control(self.controls.warning, 0, y, width, warning_height);
            y += warning_height + metrics.control_gap;
        }
        if recovery_visible {
            let recovery_label_height = measure_text(
                self.shell.hwnd(),
                self.font,
                &window_text(self.controls.recovery_label),
                Some(width),
            )
            .height
            .max(metrics.label_height);
            move_control(
                self.controls.recovery_label,
                0,
                y,
                width,
                recovery_label_height,
            );
            y += recovery_label_height + metrics.tight_gap;
            move_control(
                self.controls.recovery_value,
                0,
                y,
                width,
                metrics.field_height,
            );
            y += metrics.field_height + metrics.control_gap;
            let export_width = measured_button_width(
                self.shell.hwnd(),
                self.font,
                &window_text(self.controls.export_recovery),
                dpi,
                scale(75, dpi),
            );
            let hide_width = measured_button_width(
                self.shell.hwnd(),
                self.font,
                &window_text(self.controls.hide_recovery),
                dpi,
                scale(75, dpi),
            );
            move_control(
                self.controls.export_recovery,
                0,
                y,
                export_width,
                metrics.button_height,
            );
            move_control(
                self.controls.hide_recovery,
                export_width + metrics.control_gap,
                y,
                hide_width,
                metrics.button_height,
            );
            y += metrics.button_height + metrics.control_gap;
        }
        let message = window_text(self.controls.message);
        if !message.is_empty() {
            let message_height = measure_text(self.shell.hwnd(), self.font, &message, Some(width))
                .height
                .max(metrics.label_height);
            move_control(self.controls.message, 0, y, width, message_height);
            y += message_height;
        }
        self.shell.fit_content_height(logical_height(y, dpi));
        for (index, column_width) in volume_columns(width, dpi).into_iter().enumerate() {
            let _ = SendMessageW(
                self.controls.volumes,
                LVM_SETCOLUMNWIDTH,
                WPARAM(index),
                LPARAM(column_width as isize),
            );
        }
    }

    unsafe fn render_state(&mut self) {
        refill_volumes(self.controls.volumes, &self.state);
        self.render_conditions();
        set_text(self.controls.message, &self.state.message);
    }

    unsafe fn render_conditions(&mut self) {
        let actions = self.state.available_actions();
        refill_action_combo(self.controls.action, &actions, self.state.action);
        refill_method_combo(self.controls.method, self.state.unlock_method);
        let locked = self.state.selected_status() == Some(VolumeStatus::EncryptedLocked);
        let unlocked = self.state.selected_status() == Some(VolumeStatus::EncryptedUnlocked);
        let recovery_visible = self.state.recovery_key.is_some();
        let credential_label = if self.state.unlock_method == BitLockerUnlockMethod::RecoveryKey {
            crate::tr!("恢复密钥:")
        } else {
            crate::tr!("密码:")
        };
        set_text(self.controls.credential_label, &credential_label);
        let password_character = if self.state.unlock_method == BitLockerUnlockMethod::Password {
            0x25CF
        } else {
            0
        };
        let _ = SendMessageW(
            self.controls.credential,
            EM_SETPASSWORDCHAR,
            WPARAM(password_character),
            LPARAM(0),
        );
        set_text(self.controls.credential, &self.state.credential);
        let status = selected_status_text(&self.state);
        set_text(self.controls.state_text, &status);
        let warning = if unlocked {
            crate::tr!("解密在后台进行，可能耗时较长，期间请勿断电或重启。")
        } else {
            String::new()
        };
        set_text(self.controls.warning, &warning);
        set_text(
            self.controls.recovery_value,
            self.state
                .recovery_key
                .as_ref()
                .map(DisplayedRecoveryKey::expose)
                .unwrap_or(""),
        );
        for control in [
            self.controls.method_label,
            self.controls.method,
            self.controls.credential_label,
            self.controls.credential,
        ] {
            let _ = ShowWindow(control, if locked { SW_SHOW } else { SW_HIDE });
        }
        let _ = ShowWindow(
            self.controls.warning,
            if unlocked && !recovery_visible {
                SW_SHOW
            } else {
                SW_HIDE
            },
        );
        for control in [
            self.controls.recovery_label,
            self.controls.recovery_value,
            self.controls.export_recovery,
            self.controls.hide_recovery,
        ] {
            let _ = ShowWindow(control, if recovery_visible { SW_SHOW } else { SW_HIDE });
        }
        self.layout();
        self.render_enablement();
    }

    unsafe fn render_enablement(&self) {
        let enabled = !self.state.loading && !self.state.running;
        let has_actions = !self.state.available_actions().is_empty();
        for control in [
            self.controls.volumes,
            self.controls.action,
            self.controls.method,
            self.controls.credential,
            self.controls.export_recovery,
            self.controls.hide_recovery,
        ] {
            let _ = EnableWindow(control, enabled);
        }
        self.shell
            .set_primary_enabled(enabled && has_actions && self.state.operation().is_ok());
    }

    unsafe fn apply_font_and_theme(&self) {
        let palette = Palette::system();
        for control in self.controls() {
            let _ = SendMessageW(control, WM_SETFONT, WPARAM(self.font.0 as usize), LPARAM(1));
        }
        let _ = apply_list_view_theme(self.controls.volumes, palette);
        for (message, color) in [
            (LVM_SETBKCOLOR, palette.edit),
            (LVM_SETTEXTBKCOLOR, palette.edit),
            (LVM_SETTEXTCOLOR, palette.text),
        ] {
            let _ = SendMessageW(
                self.controls.volumes,
                message,
                WPARAM(0),
                LPARAM(color.0 as isize),
            );
        }
        for control in [
            self.controls.action,
            self.controls.method,
            self.controls.credential,
        ] {
            apply_control_theme(control, palette, NativeControlKind::Field);
        }
        apply_control_theme(
            self.controls.recovery_value,
            palette,
            NativeControlKind::Field,
        );
        for control in [self.controls.export_recovery, self.controls.hide_recovery] {
            apply_control_theme(control, palette, NativeControlKind::General);
        }
    }

    fn controls(&self) -> [HWND; 14] {
        let c = self.controls;
        [
            c.volumes,
            c.state_text,
            c.action_label,
            c.action,
            c.method_label,
            c.method,
            c.credential_label,
            c.credential,
            c.warning,
            c.recovery_label,
            c.recovery_value,
            c.export_recovery,
            c.hide_recovery,
            c.message,
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BitLockerDetailLayout {
    method_y: i32,
    credential_y: i32,
    warning_y: i32,
    recovery_label_y: i32,
    recovery_value_y: i32,
    recovery_buttons_y: i32,
    message_y: i32,
}

impl BitLockerDetailLayout {
    fn calculate(
        row_y: i32,
        dpi: u32,
        locked: bool,
        unlocked: bool,
        recovery_visible: bool,
    ) -> Self {
        let s = |value| scale(value, dpi);
        let mut cursor = row_y + s(64);
        let method_y = cursor + s(4);
        let credential_y = cursor + s(40);
        if locked {
            cursor += s(72);
        }
        let warning_y = cursor;
        let recovery_label_y = cursor;
        let recovery_value_y = cursor + s(28);
        let recovery_buttons_y = cursor + s(65);
        if recovery_visible {
            cursor += s(101);
        } else if unlocked {
            cursor += s(50);
        }
        Self {
            method_y,
            credential_y,
            warning_y,
            recovery_label_y,
            recovery_value_y,
            recovery_buttons_y,
            message_y: cursor + s(6),
        }
    }
}

impl Drop for NativeBitLockerManageDialog {
    fn drop(&mut self) {
        unsafe {
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font);
            }
        }
    }
}

unsafe fn create_controls(parent: HWND) -> windows::core::Result<Controls> {
    let volumes = child(
        parent,
        w!("SysListView32"),
        "",
        (LVS_REPORT | LVS_SHOWSELALWAYS | WS_BORDER.0 | WS_TABSTOP.0) as i32,
        ID_VOLUME_LIST,
    )?;
    let _ = SendMessageW(
        volumes,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        WPARAM(0),
        LPARAM((LVS_EX_DOUBLEBUFFER | LVS_EX_FULLROWSELECT | LVS_EX_INFOTIP) as isize),
    );
    insert_columns(volumes);
    let choice = |id| {
        child(
            parent,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            id,
        )
    };
    let button = |text: &str, id| {
        child(
            parent,
            w!("BUTTON"),
            text,
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            id,
        )
    };
    Ok(Controls {
        volumes,
        state_text: child(parent, w!("STATIC"), "", 0, 0)?,
        action_label: child(parent, w!("STATIC"), &crate::tr!("操作:"), 0, 0)?,
        action: choice(ID_ACTION_COMBO)?,
        method_label: child(parent, w!("STATIC"), &crate::tr!("解锁方式:"), 0, 0)?,
        method: choice(ID_METHOD_COMBO)?,
        credential_label: child(parent, w!("STATIC"), &crate::tr!("密码:"), 0, 0)?,
        credential: child(
            parent,
            w!("EDIT"),
            "",
            ES_AUTOHSCROLL | ES_PASSWORD | WS_BORDER.0 as i32 | WS_TABSTOP.0 as i32,
            ID_CREDENTIAL,
        )?,
        warning: child(parent, w!("STATIC"), "", 0, 0)?,
        recovery_label: child(
            parent,
            w!("STATIC"),
            &crate::tr!("恢复密钥（48 位数字），请妥善保管、勿泄露："),
            0,
            0,
        )?,
        recovery_value: child(
            parent,
            w!("EDIT"),
            "",
            ES_AUTOHSCROLL | ES_READONLY | WS_BORDER.0 as i32,
            ID_RECOVERY_VALUE,
        )?,
        export_recovery: button(&crate::tr!("导出到文件"), ID_EXPORT_RECOVERY)?,
        hide_recovery: button(&crate::tr!("隐藏"), ID_HIDE_RECOVERY)?,
        message: child(parent, w!("STATIC"), "", 0, 0)?,
    })
}

unsafe fn insert_columns(list: HWND) {
    for (index, title) in ["分区", "大小", "卷标", "状态", "保护方式"]
        .into_iter()
        .enumerate()
    {
        let mut text = wide(crate::tr!(title));
        let mut column = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH,
            cx: 110,
            pszText: PWSTR(text.as_mut_ptr()),
            ..Default::default()
        };
        let _ = SendMessageW(
            list,
            LVM_INSERTCOLUMNW,
            WPARAM(index),
            LPARAM((&mut column as *mut LVCOLUMNW) as isize),
        );
    }
}

unsafe fn refill_volumes(list: HWND, state: &BitLockerManageDialogState) {
    let _ = SendMessageW(list, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));
    for (row, volume) in state.volumes.iter().enumerate() {
        let status = volume_status_text(volume);
        for (column, value) in [
            volume.drive.clone(),
            format_size(volume.total_size_mb),
            display_label(&volume.label),
            status,
            display_label(&volume.protection_method),
        ]
        .into_iter()
        .enumerate()
        {
            insert_item(list, row, column, &value);
        }
        if state.selected_volume.as_deref() == Some(&volume.drive) {
            select_item(list, row);
        }
    }
}

unsafe fn insert_item(list: HWND, row: usize, column: usize, value: &str) {
    let mut value = wide(value);
    let mut item = LVITEMW {
        mask: LVIF_TEXT,
        iItem: row as i32,
        iSubItem: column as i32,
        pszText: PWSTR(value.as_mut_ptr()),
        ..Default::default()
    };
    let message = if column == 0 {
        LVM_INSERTITEMW
    } else {
        LVM_SETITEMTEXTW
    };
    let _ = SendMessageW(
        list,
        message,
        WPARAM(0),
        LPARAM((&mut item as *mut LVITEMW) as isize),
    );
}

unsafe fn select_item(list: HWND, row: usize) {
    let mut item = LVITEMW {
        stateMask: windows::Win32::UI::Controls::LIST_VIEW_ITEM_STATE_FLAGS(
            LVIS_SELECTED | LVIS_FOCUSED,
        ),
        state: windows::Win32::UI::Controls::LIST_VIEW_ITEM_STATE_FLAGS(
            LVIS_SELECTED | LVIS_FOCUSED,
        ),
        ..Default::default()
    };
    let _ = SendMessageW(
        list,
        LVM_SETITEMSTATE,
        WPARAM(row),
        LPARAM((&mut item as *mut LVITEMW) as isize),
    );
}

unsafe fn selected_drive(list: HWND, volumes: &[BitLockerManageVolume]) -> Option<String> {
    let index = SendMessageW(
        list,
        LVM_GETNEXTITEM,
        WPARAM(usize::MAX),
        LPARAM(LVNI_SELECTED),
    )
    .0;
    (index >= 0)
        .then(|| {
            volumes
                .get(index as usize)
                .map(|volume| volume.drive.clone())
        })
        .flatten()
}

unsafe fn refill_action_combo(
    combo: HWND,
    actions: &[BitLockerManageAction],
    selected: BitLockerManageAction,
) {
    reset_combo(combo);
    for action in actions {
        add_combo(combo, &crate::tr!(action_label(*action)));
    }
    let index = actions
        .iter()
        .position(|action| *action == selected)
        .unwrap_or(0);
    let _ = SendMessageW(combo, CB_SETCURSEL, WPARAM(index), LPARAM(0));
}

unsafe fn refill_method_combo(combo: HWND, selected: BitLockerUnlockMethod) {
    reset_combo(combo);
    add_combo(combo, &crate::tr!("密码"));
    add_combo(combo, &crate::tr!("恢复密钥"));
    let index = usize::from(selected == BitLockerUnlockMethod::RecoveryKey);
    let _ = SendMessageW(combo, CB_SETCURSEL, WPARAM(index), LPARAM(0));
}

unsafe fn reset_combo(combo: HWND) {
    let _ = SendMessageW(combo, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
}

unsafe fn add_combo(combo: HWND, value: &str) {
    let value = wide(value);
    let _ = SendMessageW(
        combo,
        CB_ADDSTRING,
        WPARAM(0),
        LPARAM(value.as_ptr() as isize),
    );
}

unsafe fn combo_index(combo: HWND) -> usize {
    SendMessageW(combo, CB_GETCURSEL, WPARAM(0), LPARAM(0))
        .0
        .max(0) as usize
}

fn action_label(action: BitLockerManageAction) -> &'static str {
    match action {
        BitLockerManageAction::Unlock => "解锁",
        BitLockerManageAction::Decrypt => "关闭 BitLocker（解密）",
        BitLockerManageAction::ReadRecoveryKey => "查看恢复密钥",
        BitLockerManageAction::SuspendProtection => "挂起保护",
        BitLockerManageAction::ResumeProtection => "恢复保护",
    }
}

fn volume_status_text(volume: &BitLockerManageVolume) -> String {
    match volume.encryption_percentage {
        Some(percentage)
            if matches!(
                volume.status,
                VolumeStatus::Decrypting | VolumeStatus::Encrypting
            ) =>
        {
            format!("{} ({}%)", crate::tr!(volume.status.as_str()), percentage)
        }
        _ => crate::tr!(volume.status.as_str()),
    }
}

fn selected_status_text(state: &BitLockerManageDialogState) -> String {
    match state.selected_status() {
        Some(VolumeStatus::EncryptedLocked) => {
            crate::tr!("该分区已锁定，请选择解锁方式并输入凭据。")
        }
        Some(VolumeStatus::EncryptedUnlocked) => {
            crate::tr!("该分区已解锁，可管理保护或彻底关闭 BitLocker。")
        }
        Some(VolumeStatus::Decrypting) => crate::tr!("该分区正在解密中，请等待完成。"),
        Some(VolumeStatus::Encrypting) => crate::tr!("该分区正在加密中。"),
        Some(_) => crate::tr!("当前状态不支持管理操作。"),
        None => crate::tr!("请选择一个分区进行操作。"),
    }
}

fn volume_columns(width: i32, dpi: u32) -> [i32; 5] {
    let usable = (width - scale(4, dpi)).max(0);
    let drive = usable * 12 / 100;
    let size = usable * 17 / 100;
    let label = usable * 22 / 100;
    let status = usable * 21 / 100;
    let method = usable - drive - size - label - status;
    [drive, size, label, status, method]
}

fn format_size(value_mb: u64) -> String {
    format!("{:.1} GB", value_mb as f64 / 1024.0)
}

fn display_label(value: &str) -> String {
    if value.trim().is_empty() {
        "—".into()
    } else {
        value.into()
    }
}

unsafe fn window_text(control: HWND) -> String {
    let length = GetWindowTextLengthW(control).max(0) as usize;
    let mut buffer = vec![0_u16; length + 1];
    let copied = GetWindowTextW(control, &mut buffer).max(0) as usize;
    String::from_utf16_lossy(&buffer[..copied])
}

unsafe fn set_text(control: HWND, text: &str) {
    let text = wide(text);
    let _ = SetWindowTextW(control, PCWSTR(text.as_ptr()));
}

unsafe fn move_control(control: HWND, x: i32, y: i32, width: i32, height: i32) {
    let _ = MoveWindow(control, x, y, width.max(0), height.max(0), true);
}

fn scale(value: i32, dpi: u32) -> i32 {
    ((value as i64 * dpi.max(1) as i64 + 48) / 96) as i32
}

fn logical_height(value: i32, dpi: u32) -> i32 {
    ((i64::from(value.max(0)) * 96 + i64::from(dpi.max(1)) / 2) / i64::from(dpi.max(1))) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(drive: &str, status: VolumeStatus) -> BitLockerManageVolume {
        BitLockerManageVolume {
            drive: drive.into(),
            label: "Data".into(),
            total_size_mb: 100 * 1024,
            status,
            protection_method: "TPM + Recovery Password".into(),
            encryption_percentage: None,
        }
    }

    #[test]
    fn inventory_preserves_legacy_first_selection_and_drops_stale_values() {
        let mut state = BitLockerManageDialogState::default();
        state.apply_inventory(Ok(vec![
            volume("d:", VolumeStatus::EncryptedLocked),
            volume("E:", VolumeStatus::EncryptedUnlocked),
        ]));
        assert_eq!(state.selected_volume.as_deref(), Some("D:"));
        state.select_volume(Some("E:"));
        assert_eq!(state.action, BitLockerManageAction::Decrypt);
        state.apply_inventory(Ok(vec![volume("D:", VolumeStatus::EncryptedLocked)]));
        assert_eq!(state.selected_volume.as_deref(), Some("D:"));
        assert_eq!(state.action, BitLockerManageAction::Unlock);
    }

    #[test]
    fn conditional_actions_exactly_match_volume_state() {
        let mut state = BitLockerManageDialogState::default();
        state.apply_inventory(Ok(vec![
            volume("D:", VolumeStatus::EncryptedLocked),
            volume("E:", VolumeStatus::EncryptedUnlocked),
            volume("F:", VolumeStatus::Decrypting),
        ]));
        assert_eq!(state.available_actions(), [BitLockerManageAction::Unlock]);
        state.select_volume(Some("E:"));
        assert_eq!(
            state.available_actions(),
            [
                BitLockerManageAction::Decrypt,
                BitLockerManageAction::ReadRecoveryKey,
                BitLockerManageAction::SuspendProtection,
                BitLockerManageAction::ResumeProtection,
            ]
        );
        state.select_volume(Some("F:"));
        assert!(state.available_actions().is_empty());
    }

    #[test]
    fn changing_volume_or_unlock_method_clears_sensitive_state() {
        let mut state = BitLockerManageDialogState::default();
        state.apply_inventory(Ok(vec![volume("D:", VolumeStatus::EncryptedLocked)]));
        state.set_credential("secret".into());
        state.recovery_key = Some(DisplayedRecoveryKey::new("111111-222222"));
        state.set_unlock_method(BitLockerUnlockMethod::RecoveryKey);
        assert!(state.credential.is_empty());
        assert!(state.recovery_key.is_some());
        state.select_volume(Some("D:"));
        assert!(state.credential.is_empty());
        assert!(state.recovery_key.is_none());
    }

    #[test]
    fn displayed_key_and_unlock_intent_debug_are_redacted() {
        let key = DisplayedRecoveryKey::new("111111-222222-333333");
        assert!(!format!("{key:?}").contains("111111"));
        let mut state = BitLockerManageDialogState::default();
        state.apply_inventory(Ok(vec![volume("D:", VolumeStatus::EncryptedLocked)]));
        state.set_credential("top-secret".into());
        assert!(!format!("{state:?}").contains("top-secret"));
        let intent = state.operation().unwrap();
        assert!(!format!("{intent:?}").contains("top-secret"));
    }

    #[test]
    fn columns_remain_within_width_at_low_resolution_and_high_dpi() {
        for dpi in [96, 144, 192] {
            for logical_width in [320, 680] {
                let width = scale(logical_width, dpi);
                let columns = volume_columns(width, dpi);
                assert!(columns.into_iter().all(|column| column > 0));
                assert!(columns.into_iter().sum::<i32>() <= width);
            }
        }
    }

    #[test]
    fn hidden_bitlocker_fields_do_not_reserve_vertical_rows() {
        let simple = BitLockerDetailLayout::calculate(180, 96, false, false, false);
        let unlock = BitLockerDetailLayout::calculate(180, 96, true, false, false);
        let recovery = BitLockerDetailLayout::calculate(180, 96, false, true, true);
        assert_eq!(simple.message_y, 250);
        assert_eq!(unlock.message_y - simple.message_y, 72);
        assert_eq!(recovery.recovery_label_y, simple.warning_y);
        assert_eq!(recovery.message_y - simple.message_y, 101);

        let high_dpi = BitLockerDetailLayout::calculate(360, 192, true, false, true);
        assert_eq!(high_dpi.method_y, 496);
        assert_eq!(high_dpi.message_y, 846);
    }
}
