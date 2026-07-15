//! Compact native easy-mode page. It renders controller snapshots and emits
//! commands only; catalogue loading, downloads and installation stay outside UI.

use std::cell::Cell;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{InvalidateRect, HFONT};
use windows::Win32::UI::Controls::{BST_CHECKED, BST_UNCHECKED};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    MoveWindow, SendMessageW, SetWindowTextW, ShowWindow, BM_GETCHECK, BM_SETCHECK,
    BS_AUTOCHECKBOX, BS_OWNERDRAW, CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL, CB_RESETCONTENT,
    CB_SETCURSEL, SW_HIDE, SW_SHOW, WM_SETFONT, WS_BORDER, WS_TABSTOP,
};

use super::download::PageRect;
use crate::core::native_easy_mode_controller::EasyModeView;
use crate::native_ui::controls::{child, wide};
use crate::native_ui::layout::measured_button_width;
use crate::native_ui::theme::{apply_control_theme, NativeControlKind, Palette};

pub const ID_EASY_ENABLED: u16 = 5_700;
pub const ID_EASY_DISMISS_TIP: u16 = 5_701;
pub const ID_EASY_SYSTEM: u16 = 5_702;
pub const ID_EASY_VOLUME: u16 = 5_703;
pub const ID_EASY_INSTALL: u16 = 5_704;
const SS_CENTER_STYLE: i32 = 0x0000_0001;

fn right_aligned_control_x(left: i32, available_width: i32, control_width: i32) -> i32 {
    left + (available_width.max(0) - control_width.max(0)).max(0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EasyModeCommand {
    ToggleEnabled,
    DismissSettingsTip,
    SelectSystem,
    SelectVolume,
    StartInstall,
}

pub struct EasyModeLabels<'a> {
    pub enabled: &'a str,
    pub settings_tip: &'a str,
    pub dismiss_tip: &'a str,
    pub system: &'a str,
    pub volume: &'a str,
    pub loading: &'a str,
    pub install: &'a str,
}

pub struct EasyModePage {
    pub enabled: HWND,
    pub settings_tip: HWND,
    pub dismiss_tip: HWND,
    pub system_label: HWND,
    pub system: HWND,
    pub volume_label: HWND,
    pub volume: HWND,
    pub logo: HWND,
    pub description: HWND,
    pub install: HWND,
    loading_text: String,
    dismiss_tip_text: String,
    install_text: String,
    font: HFONT,
    systems: Vec<String>,
    volumes: Vec<String>,
    logo_visible: bool,
    page_visible: Cell<bool>,
    settings_tip_visible: Cell<bool>,
}

impl EasyModePage {
    pub unsafe fn create(
        parent: HWND,
        font: HFONT,
        labels: &EasyModeLabels<'_>,
    ) -> windows::core::Result<Self> {
        let enabled = child(
            parent,
            w!("BUTTON"),
            labels.enabled,
            BS_AUTOCHECKBOX | WS_TABSTOP.0 as i32,
            ID_EASY_ENABLED,
        )?;
        let settings_tip = child(parent, w!("STATIC"), labels.settings_tip, 0, 5_710)?;
        let dismiss_tip = child(
            parent,
            w!("BUTTON"),
            labels.dismiss_tip,
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            ID_EASY_DISMISS_TIP,
        )?;
        let system_label = child(parent, w!("STATIC"), labels.system, 0, 5_711)?;
        let system = child(
            parent,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            ID_EASY_SYSTEM,
        )?;
        let volume_label = child(parent, w!("STATIC"), labels.volume, 0, 5_712)?;
        let volume = child(
            parent,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            ID_EASY_VOLUME,
        )?;
        let logo = child(
            parent,
            w!("STATIC"),
            "",
            SS_CENTER_STYLE | WS_BORDER.0 as i32,
            5_713,
        )?;
        let description = child(parent, w!("STATIC"), labels.loading, 0, 5_714)?;
        let install = child(
            parent,
            w!("BUTTON"),
            labels.install,
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            ID_EASY_INSTALL,
        )?;
        let page = Self {
            enabled,
            settings_tip,
            dismiss_tip,
            system_label,
            system,
            volume_label,
            volume,
            logo,
            description,
            install,
            loading_text: labels.loading.to_string(),
            dismiss_tip_text: labels.dismiss_tip.to_string(),
            install_text: labels.install.to_string(),
            font,
            systems: Vec::new(),
            volumes: Vec::new(),
            logo_visible: false,
            page_visible: Cell::new(false),
            settings_tip_visible: Cell::new(false),
        };
        page.apply_font(font);
        page.apply_theme(Palette::system());
        page.show(false);
        Ok(page)
    }

    pub const fn command(command_id: u16) -> Option<EasyModeCommand> {
        match command_id {
            ID_EASY_ENABLED => Some(EasyModeCommand::ToggleEnabled),
            ID_EASY_DISMISS_TIP => Some(EasyModeCommand::DismissSettingsTip),
            ID_EASY_SYSTEM => Some(EasyModeCommand::SelectSystem),
            ID_EASY_VOLUME => Some(EasyModeCommand::SelectVolume),
            ID_EASY_INSTALL => Some(EasyModeCommand::StartInstall),
            _ => None,
        }
    }

    pub unsafe fn enabled_value(&self) -> bool {
        SendMessageW(self.enabled, BM_GETCHECK, WPARAM(0), LPARAM(0)).0 as u32 == BST_CHECKED.0
    }

    pub unsafe fn selected_system(&self) -> Option<usize> {
        selected_combo(self.system)
    }

    pub unsafe fn selected_volume(&self) -> Option<usize> {
        selected_combo(self.volume)
    }

    pub unsafe fn update(&mut self, view: &EasyModeView) {
        let check = if view.enabled {
            BST_CHECKED
        } else {
            BST_UNCHECKED
        };
        let _ = SendMessageW(
            self.enabled,
            BM_SETCHECK,
            WPARAM(check.0 as usize),
            LPARAM(0),
        );
        replace_combo(
            self.system,
            &mut self.systems,
            &view.systems,
            view.selected_system,
        );
        replace_combo(
            self.volume,
            &mut self.volumes,
            &view.volumes,
            view.selected_volume,
        );

        // The native page does not yet decode the remote logo into a bitmap. Showing its
        // `display_hint` inside a bordered STATIC produced a duplicate system name beside the
        // ComboBox and looked like a broken second selector. Keep the optional logo region hidden
        // until an actual bitmap is available; the system and version selectors retain full width.
        self.logo_visible = false;
        set_text(self.logo, "");
        let _ = ShowWindow(self.logo, SW_HIDE);
        let text = if view.loading {
            &self.loading_text
        } else if let Some(error) = view.error.as_deref() {
            error
        } else {
            &view.description
        };
        set_text(self.description, text);
        self.settings_tip_visible.set(view.settings_tip_visible);
        let tip_command = if self.page_visible.get() && view.settings_tip_visible {
            SW_SHOW
        } else {
            SW_HIDE
        };
        let _ = ShowWindow(self.settings_tip, tip_command);
        let _ = ShowWindow(self.dismiss_tip, tip_command);
        let _ = EnableWindow(
            self.system,
            view.enabled && !view.loading && view.error.is_none(),
        );
        let _ = EnableWindow(
            self.volume,
            view.enabled && view.selected_system.is_some() && !view.volumes.is_empty(),
        );
        let _ = EnableWindow(self.install, view.can_install && view.error.is_none());
    }

    /// Refreshes every static caption after the application language changes.
    pub unsafe fn relocalize(&mut self, labels: &EasyModeLabels<'_>) {
        set_text(self.enabled, labels.enabled);
        set_text(self.settings_tip, labels.settings_tip);
        set_text(self.dismiss_tip, labels.dismiss_tip);
        set_text(self.system_label, labels.system);
        set_text(self.volume_label, labels.volume);
        set_text(self.install, labels.install);
        self.loading_text = labels.loading.to_owned();
        self.dismiss_tip_text = labels.dismiss_tip.to_owned();
        self.install_text = labels.install.to_owned();
    }

    pub unsafe fn layout(&self, rect: PageRect, dpi: u32) {
        let s = |value: i32| ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32;
        let width = rect.width.max(0);
        let height = rect.height.max(0);
        let compact = height < s(280);
        let gap = s(if compact { 4 } else { 8 });
        let label_height = s(if compact { 18 } else { 22 });
        let control_height = s(if compact { 26 } else { 30 });
        let button_width =
            measured_button_width(self.install, self.font, &self.install_text, dpi, s(110))
                .min(width);
        let tip_button_width = measured_button_width(
            self.dismiss_tip,
            self.font,
            &self.dismiss_tip_text,
            dpi,
            s(75),
        )
        .min((width / 2).max(0));
        let tip_height = s(if compact { 30 } else { 42 });
        let logo_size = if height < s(390) { s(52) } else { s(68) }.min(width);
        // Easy mode is disabled from the About page. Once active, the install page must
        // concentrate on choosing a system and must not render a second "disable" checkbox.
        let mut y = rect.y;

        let _ = MoveWindow(
            self.settings_tip,
            rect.x,
            y,
            (width - tip_button_width - gap).max(0),
            tip_height,
            true,
        );
        let _ = MoveWindow(
            self.dismiss_tip,
            rect.x + width - tip_button_width.min(width),
            y,
            tip_button_width,
            control_height,
            true,
        );
        y += tip_height + gap;

        let field_width = if self.logo_visible {
            (width - logo_size - gap).max(0)
        } else {
            width
        };
        let _ = MoveWindow(
            self.system_label,
            rect.x,
            y,
            field_width,
            label_height,
            true,
        );
        y += label_height;
        let _ = MoveWindow(self.system, rect.x, y, field_width, s(240), true);
        y += control_height + gap;
        let _ = MoveWindow(
            self.volume_label,
            rect.x,
            y,
            field_width,
            label_height,
            true,
        );
        y += label_height;
        let _ = MoveWindow(self.volume, rect.x, y, field_width, s(240), true);
        let logo_y = y - label_height - control_height - gap;
        let _ = MoveWindow(
            self.logo,
            rect.x + width - logo_size.min(width),
            logo_y,
            logo_size,
            logo_size,
            true,
        );
        y += control_height + gap;
        let install_y = (rect.y + height - control_height).max(rect.y);
        let description_y = y.min(install_y);
        let description_height = (install_y - gap - description_y).clamp(0, s(90));
        let _ = MoveWindow(
            self.description,
            rect.x,
            description_y,
            width,
            description_height,
            true,
        );
        let _ = MoveWindow(
            self.install,
            right_aligned_control_x(rect.x, width, button_width),
            install_y,
            button_width,
            control_height,
            true,
        );
    }

    pub unsafe fn show(&self, visible: bool) {
        self.page_visible.set(visible);
        let command = if visible { SW_SHOW } else { SW_HIDE };
        for control in self.controls() {
            let _ = ShowWindow(control, command);
        }
        if visible {
            let _ = ShowWindow(self.enabled, SW_HIDE);
        }
        if visible && !self.logo_visible {
            let _ = ShowWindow(self.logo, SW_HIDE);
        }
        if visible && !self.settings_tip_visible.get() {
            let _ = ShowWindow(self.settings_tip, SW_HIDE);
            let _ = ShowWindow(self.dismiss_tip, SW_HIDE);
        }
    }

    pub unsafe fn apply_font(&self, font: HFONT) {
        for control in self.controls() {
            let _ = SendMessageW(control, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        }
    }

    pub unsafe fn apply_theme(&self, palette: Palette) {
        apply_control_theme(self.enabled, palette, NativeControlKind::General);
        for control in [self.system, self.volume] {
            apply_control_theme(control, palette, NativeControlKind::Field);
        }
    }

    fn controls(&self) -> [HWND; 10] {
        [
            self.enabled,
            self.settings_tip,
            self.dismiss_tip,
            self.system_label,
            self.system,
            self.volume_label,
            self.volume,
            self.logo,
            self.description,
            self.install,
        ]
    }
}

unsafe fn replace_combo(
    combo: HWND,
    cached_values: &mut Vec<String>,
    values: &[String],
    selected: Option<usize>,
) {
    // CBN_SELCHANGE is sent while the native popup list is still open. Resetting that same
    // ComboBox from inside the notification leaves stale popup pixels (often the first Windows
    // version) behind until another repaint. Rebuild only when the actual catalogue changed.
    if cached_values.as_slice() != values {
        let _ = SendMessageW(combo, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
        for value in values {
            let value = wide(value);
            let _ = SendMessageW(
                combo,
                CB_ADDSTRING,
                WPARAM(0),
                LPARAM(value.as_ptr() as isize),
            );
        }
        cached_values.clear();
        cached_values.extend_from_slice(values);
    }
    let index = selected
        .filter(|index| *index < values.len())
        .map_or(usize::MAX, |index| index);
    let current = SendMessageW(combo, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
    if usize::try_from(current).unwrap_or(usize::MAX) != index {
        let _ = SendMessageW(combo, CB_SETCURSEL, WPARAM(index), LPARAM(0));
        // Dark themed ComboBoxes on Windows 11 do not always invalidate their arrow sub-rect
        // after a programmatic selection made from CBN_SELCHANGE.  Repaint the complete closed
        // field so the previous selected string cannot survive inside the arrow area.
        let _ = InvalidateRect(combo, None, true);
    }
}

unsafe fn selected_combo(combo: HWND) -> Option<usize> {
    usize::try_from(SendMessageW(combo, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0).ok()
}

unsafe fn set_text(control: HWND, value: &str) {
    let value = wide(value);
    let _ = SetWindowTextW(control, PCWSTR(value.as_ptr()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_button_stays_on_the_right_at_supported_widths() {
        assert_eq!(right_aligned_control_x(20, 900, 150), 770);
        assert_eq!(right_aligned_control_x(20, 320, 150), 190);
        assert_eq!(right_aligned_control_x(20, 100, 100), 20);
    }

    #[test]
    fn command_ids_are_stable_and_isolated() {
        assert_eq!(
            EasyModePage::command(ID_EASY_ENABLED),
            Some(EasyModeCommand::ToggleEnabled)
        );
        assert_eq!(
            EasyModePage::command(ID_EASY_SYSTEM),
            Some(EasyModeCommand::SelectSystem)
        );
        assert_eq!(
            EasyModePage::command(ID_EASY_VOLUME),
            Some(EasyModeCommand::SelectVolume)
        );
        assert_eq!(
            EasyModePage::command(ID_EASY_INSTALL),
            Some(EasyModeCommand::StartInstall)
        );
        assert_eq!(EasyModePage::command(5_699), None);
    }
}
