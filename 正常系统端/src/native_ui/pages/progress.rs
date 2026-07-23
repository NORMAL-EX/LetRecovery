//! Native Inno-style presentation for long-running operations.
//!
//! Workers, cancellation, reboot and follow-up actions stay in controllers.
//! This page displays snapshots and emits command intents only.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::HFONT;
use windows::Win32::UI::Controls::{SetWindowTheme, DRAWITEMSTRUCT};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    MoveWindow, SendMessageW, ShowWindow, BS_OWNERDRAW, SW_HIDE, SW_SHOW, WM_GETFONT, WM_SETFONT,
    WS_TABSTOP,
};

use super::super::controls::{
    child, draw_inno_button, draw_progress, wide, ButtonRole, ProgressRole,
};
use super::super::theme::Palette;

pub const ID_OVERALL_PROGRESS: u16 = 800;
pub const ID_STEP_PROGRESS: u16 = 801;
pub const ID_CANCEL_OPERATION: u16 = 802;
pub const ID_PROGRESS_PRIMARY: u16 = 812;
pub const ID_PROGRESS_SECONDARY: u16 = 813;
const SS_OWNERDRAW_STYLE: i32 = 0x0000_000D;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProgressValue {
    pub completed: u64,
    pub total: u64,
}

impl ProgressValue {
    pub const fn new(completed: u64, total: u64) -> Self {
        Self {
            completed: if completed > total { total } else { completed },
            total,
        }
    }

    pub const fn percent(self) -> u32 {
        if self.total == 0 {
            0
        } else {
            let percent = (self.completed as u128 * 100) / self.total as u128;
            if percent > 100 {
                100
            } else {
                percent as u32
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProgressStatus {
    #[default]
    Running,
    Cancelling,
    Cancelled,
    Succeeded,
    Failed,
}

impl ProgressStatus {
    const fn progress_role(self) -> ProgressRole {
        match self {
            Self::Running => ProgressRole::Normal,
            Self::Cancelling => ProgressRole::Paused,
            Self::Cancelled => ProgressRole::Paused,
            Self::Succeeded => ProgressRole::Success,
            Self::Failed => ProgressRole::Error,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DownloadCompletionAction {
    ContinueInstallation,
    #[default]
    ReturnToDownloads,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProgressCompletion {
    #[default]
    Generic,
    DirectInstall,
    ViaPePrepared,
    Download(DownloadCompletionAction),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressIntent {
    CancelRequested,
    Back,
    RestartNow,
    RestartLater,
    ContinueDownloadedInstallation,
    ReturnToDownloads,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CommandButton {
    label: String,
    intent: ProgressIntent,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CommandBarState {
    primary: Option<CommandButton>,
    secondary: Option<CommandButton>,
    cancel: Option<CommandButton>,
    cancel_enabled: bool,
}

fn command_bar_state(
    status: ProgressStatus,
    cancellable: bool,
    completion: ProgressCompletion,
) -> CommandBarState {
    let button = |label: String, intent| CommandButton { label, intent };
    match status {
        ProgressStatus::Running => CommandBarState {
            cancel: Some(button(crate::tr!("取消"), ProgressIntent::CancelRequested)),
            cancel_enabled: cancellable,
            ..Default::default()
        },
        ProgressStatus::Cancelling => CommandBarState {
            cancel: Some(button(crate::tr!("取消"), ProgressIntent::CancelRequested)),
            cancel_enabled: false,
            ..Default::default()
        },
        ProgressStatus::Cancelled => CommandBarState {
            cancel: Some(button(crate::tr!("返回"), ProgressIntent::Back)),
            cancel_enabled: true,
            ..Default::default()
        },
        ProgressStatus::Failed => CommandBarState {
            cancel: Some(button(crate::tr!("返回"), ProgressIntent::Back)),
            cancel_enabled: true,
            ..Default::default()
        },
        ProgressStatus::Succeeded => match completion {
            ProgressCompletion::Generic => CommandBarState {
                cancel: Some(button(crate::tr!("返回"), ProgressIntent::Back)),
                cancel_enabled: true,
                ..Default::default()
            },
            ProgressCompletion::DirectInstall => CommandBarState {
                primary: Some(button(crate::tr!("立即重启"), ProgressIntent::RestartNow)),
                secondary: Some(button(crate::tr!("返回主页"), ProgressIntent::Back)),
                ..Default::default()
            },
            ProgressCompletion::ViaPePrepared => CommandBarState {
                primary: Some(button(crate::tr!("立即重启"), ProgressIntent::RestartNow)),
                secondary: Some(button(crate::tr!("稍后重启"), ProgressIntent::RestartLater)),
                ..Default::default()
            },
            ProgressCompletion::Download(DownloadCompletionAction::ContinueInstallation) => {
                CommandBarState {
                    primary: Some(button(
                        crate::tr!("继续安装"),
                        ProgressIntent::ContinueDownloadedInstallation,
                    )),
                    secondary: Some(button(
                        crate::tr!("返回"),
                        ProgressIntent::ReturnToDownloads,
                    )),
                    ..Default::default()
                }
            }
            ProgressCompletion::Download(DownloadCompletionAction::ReturnToDownloads) => {
                CommandBarState {
                    primary: Some(button(
                        crate::tr!("返回"),
                        ProgressIntent::ReturnToDownloads,
                    )),
                    ..Default::default()
                }
            }
        },
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LongTaskProgress {
    pub title: String,
    pub description: String,
    pub current_step: String,
    pub detail: String,
    pub overall: ProgressValue,
    pub step: ProgressValue,
    pub status: ProgressStatus,
    pub status_text: String,
    pub cancellable: bool,
}

impl Default for LongTaskProgress {
    fn default() -> Self {
        Self {
            title: crate::tr!("正在处理"),
            description: crate::tr!("请稍候，操作完成前请勿关闭程序。"),
            current_step: crate::tr!("正在准备..."),
            detail: String::new(),
            overall: ProgressValue::default(),
            step: ProgressValue::default(),
            status: ProgressStatus::Running,
            status_text: String::new(),
            cancellable: true,
        }
    }
}

#[derive(Clone, Copy)]
pub struct ProgressPageHandles {
    pub title: HWND,
    pub description: HWND,
    pub current_step: HWND,
    pub detail: HWND,
    pub overall_caption: HWND,
    pub overall_progress: HWND,
    pub overall_percent: HWND,
    pub step_caption: HWND,
    pub step_progress: HWND,
    pub step_percent: HWND,
    pub status: HWND,
    pub primary: HWND,
    pub secondary: HWND,
    pub cancel: HWND,
}

pub struct ProgressPage {
    handles: ProgressPageHandles,
    state: LongTaskProgress,
    completion: ProgressCompletion,
}

impl ProgressPage {
    pub unsafe fn create(parent: HWND, initial: LongTaskProgress) -> windows::core::Result<Self> {
        let title = child(parent, w!("STATIC"), "", 0, 803)?;
        let description = child(parent, w!("STATIC"), "", 0, 804)?;
        let current_step = child(parent, w!("STATIC"), "", 0, 805)?;
        let detail = child(parent, w!("STATIC"), "", 0, 806)?;
        let overall_caption = child(parent, w!("STATIC"), &crate::tr!("总体进度"), 0, 807)?;
        let overall_progress = child(
            parent,
            w!("STATIC"),
            "",
            SS_OWNERDRAW_STYLE,
            ID_OVERALL_PROGRESS,
        )?;
        let overall_percent = child(parent, w!("STATIC"), "0%", 0, 808)?;
        let step_caption = child(parent, w!("STATIC"), &crate::tr!("当前步骤"), 0, 809)?;
        let step_progress = child(
            parent,
            w!("STATIC"),
            "",
            SS_OWNERDRAW_STYLE,
            ID_STEP_PROGRESS,
        )?;
        let step_percent = child(parent, w!("STATIC"), "0%", 0, 810)?;
        let status = child(parent, w!("STATIC"), "", 0, 811)?;
        let primary = command_button(parent, ID_PROGRESS_PRIMARY)?;
        let secondary = command_button(parent, ID_PROGRESS_SECONDARY)?;
        let cancel = command_button(parent, ID_CANCEL_OPERATION)?;
        let mut page = Self {
            handles: ProgressPageHandles {
                title,
                description,
                current_step,
                detail,
                overall_caption,
                overall_progress,
                overall_percent,
                step_caption,
                step_progress,
                step_percent,
                status,
                primary,
                secondary,
                cancel,
            },
            state: LongTaskProgress::default(),
            completion: ProgressCompletion::Generic,
        };
        page.update(initial);
        page.show(false);
        Ok(page)
    }

    pub const fn handles(&self) -> ProgressPageHandles {
        self.handles
    }
    pub fn state(&self) -> &LongTaskProgress {
        &self.state
    }

    pub unsafe fn set_completion(&mut self, completion: ProgressCompletion) {
        if self.completion == completion {
            return;
        }
        self.completion = completion;
        self.update_commands();
    }

    pub fn command_intent(&self, command_id: u16) -> Option<ProgressIntent> {
        let commands =
            command_bar_state(self.state.status, self.state.cancellable, self.completion);
        match command_id {
            ID_PROGRESS_PRIMARY => commands.primary.map(|value| value.intent),
            ID_PROGRESS_SECONDARY => commands.secondary.map(|value| value.intent),
            ID_CANCEL_OPERATION => commands.cancel.map(|value| value.intent),
            _ => None,
        }
    }

    pub unsafe fn update(&mut self, mut state: LongTaskProgress) {
        state.overall = ProgressValue::new(state.overall.completed, state.overall.total);
        state.step = ProgressValue::new(state.step.completed, state.step.total);
        let previous_commands =
            command_bar_state(self.state.status, self.state.cancellable, self.completion);
        let title_changed = self.state.title != state.title;
        let description_changed = self.state.description != state.description;
        let current_step_changed = self.state.current_step != state.current_step;
        let detail_changed = self.state.detail != state.detail;
        let overall_changed = self.state.overall != state.overall
            || self.state.status.progress_role() != state.status.progress_role();
        let step_changed = self.state.step != state.step
            || self.state.status.progress_role() != state.status.progress_role();
        let overall_percent_changed = self.state.overall.percent() != state.overall.percent();
        let step_percent_changed = self.state.step.percent() != state.step.percent();
        let status_changed = self.state.status_text != state.status_text;
        self.state = state;
        let h = self.handles;
        if title_changed {
            set_text(h.title, &self.state.title);
        }
        if description_changed {
            set_text(h.description, &self.state.description);
        }
        if current_step_changed {
            set_text(h.current_step, &self.state.current_step);
        }
        if detail_changed {
            set_text(h.detail, &self.state.detail);
        }
        if overall_percent_changed {
            set_text(
                h.overall_percent,
                &format!("{}%", self.state.overall.percent()),
            );
        }
        if step_percent_changed {
            set_text(h.step_percent, &format!("{}%", self.state.step.percent()));
        }
        if status_changed {
            set_text(h.status, &self.state.status_text);
        }
        let current_commands =
            command_bar_state(self.state.status, self.state.cancellable, self.completion);
        if current_commands != previous_commands {
            self.update_commands();
        }
        if overall_changed {
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(h.overall_progress, None, false);
        }
        if step_changed {
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(h.step_progress, None, false);
        }
    }

    unsafe fn update_commands(&self) {
        let commands =
            command_bar_state(self.state.status, self.state.cancellable, self.completion);
        update_command(self.handles.primary, commands.primary.as_ref(), true);
        update_command(self.handles.secondary, commands.secondary.as_ref(), true);
        update_command(
            self.handles.cancel,
            commands.cancel.as_ref(),
            commands.cancel_enabled,
        );
    }

    pub unsafe fn draw_item(&self, item: &DRAWITEMSTRUCT, palette: Palette) -> bool {
        if matches!(
            item.CtlID as u16,
            ID_PROGRESS_PRIMARY | ID_PROGRESS_SECONDARY | ID_CANCEL_OPERATION
        ) {
            let font =
                HFONT(SendMessageW(item.hwndItem, WM_GETFONT, WPARAM(0), LPARAM(0)).0 as *mut _);
            let role = if item.CtlID as u16 == ID_PROGRESS_PRIMARY {
                ButtonRole::Primary
            } else {
                ButtonRole::Secondary
            };
            draw_inno_button(item, palette, role, font, GetDpiForWindow(item.hwndItem));
            return true;
        }
        let value = match item.CtlID as u16 {
            ID_OVERALL_PROGRESS => self.state.overall,
            ID_STEP_PROGRESS => self.state.step,
            _ => return false,
        };
        draw_progress(
            item.hDC,
            item.rcItem,
            value.completed,
            value.total,
            self.state.status.progress_role(),
            palette,
        );
        true
    }

    pub unsafe fn layout(&self, left: i32, top: i32, width: i32, height: i32, dpi: u32) {
        let s = |value: i32| scale_for_dpi(value, dpi);
        let h = self.handles;
        let content_width = width.max(0);
        let percent_width = s(48);
        let bar_width = (content_width - percent_width - s(8)).max(0);
        move_control(h.title, left, top, content_width, s(30));
        move_control(h.description, left, top + s(34), content_width, s(42));
        move_control(h.current_step, left, top + s(92), content_width, s(24));
        move_control(h.detail, left, top + s(120), content_width, s(46));
        let overall_top = top + s(184);
        move_control(h.overall_caption, left, overall_top, content_width, s(20));
        move_control(
            h.overall_progress,
            left,
            overall_top + s(24),
            bar_width,
            s(16),
        );
        move_control(
            h.overall_percent,
            left + bar_width + s(8),
            overall_top + s(22),
            percent_width,
            s(20),
        );
        let step_top = overall_top + s(62);
        move_control(h.step_caption, left, step_top, content_width, s(20));
        move_control(h.step_progress, left, step_top + s(24), bar_width, s(16));
        move_control(
            h.step_percent,
            left + bar_width + s(8),
            step_top + s(22),
            percent_width,
            s(20),
        );

        let bottom = top + height.max(0);
        let commands =
            command_bar_state(self.state.status, self.state.cancellable, self.completion);
        let button_width = s(104);
        let button_gap = s(8);
        let mut right = left + content_width;
        for (control, present) in [
            (h.cancel, commands.cancel.is_some()),
            (h.primary, commands.primary.is_some()),
            (h.secondary, commands.secondary.is_some()),
        ] {
            if present {
                right -= button_width;
                move_control(control, right, bottom - s(42), button_width, s(30));
                right -= button_gap;
            }
        }
        move_control(h.status, left, bottom - s(36), (right - left).max(0), s(28));
    }

    pub unsafe fn show(&self, visible: bool) {
        let command = if visible { SW_SHOW } else { SW_HIDE };
        for control in self.all_controls() {
            let _ = ShowWindow(control, command);
        }
        if visible {
            self.update_commands();
        }
    }

    pub unsafe fn apply_theme(&self, palette: Palette) {
        let theme = if palette.dark {
            w!("DarkMode_Explorer")
        } else {
            w!("Explorer")
        };
        for button in [
            self.handles.primary,
            self.handles.secondary,
            self.handles.cancel,
        ] {
            let _ = SetWindowTheme(button, theme, PCWSTR::null());
        }
    }

    pub unsafe fn apply_font(&self, font: HFONT, heading_font: HFONT) {
        for control in self.all_controls() {
            let _ = SendMessageW(control, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        }
        for control in [self.handles.title, self.handles.current_step] {
            let _ = SendMessageW(
                control,
                WM_SETFONT,
                WPARAM(heading_font.0 as usize),
                LPARAM(1),
            );
        }
    }

    pub fn status_color(&self, palette: Palette) -> COLORREF {
        status_color(self.state.status, palette)
    }

    fn all_controls(&self) -> [HWND; 14] {
        let h = self.handles;
        [
            h.title,
            h.description,
            h.current_step,
            h.detail,
            h.overall_caption,
            h.overall_progress,
            h.overall_percent,
            h.step_caption,
            h.step_progress,
            h.step_percent,
            h.status,
            h.primary,
            h.secondary,
            h.cancel,
        ]
    }
}

pub fn status_color(status: ProgressStatus, palette: Palette) -> COLORREF {
    match status {
        ProgressStatus::Running => palette.text_secondary,
        ProgressStatus::Cancelling => COLORREF(0x003499F7),
        ProgressStatus::Cancelled => palette.text_secondary,
        ProgressStatus::Succeeded => COLORREF(0x007EC26C),
        ProgressStatus::Failed => COLORREF(0x001C2BC4),
    }
}

fn scale_for_dpi(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32
}

unsafe fn command_button(parent: HWND, id: u16) -> windows::core::Result<HWND> {
    child(
        parent,
        w!("BUTTON"),
        "",
        BS_OWNERDRAW | WS_TABSTOP.0 as i32,
        id,
    )
}

unsafe fn update_command(control: HWND, command: Option<&CommandButton>, enabled: bool) {
    if let Some(command) = command {
        set_text(control, &command.label);
        let _ = EnableWindow(control, enabled);
        let _ = ShowWindow(control, SW_SHOW);
    } else {
        let _ = ShowWindow(control, SW_HIDE);
    }
}

unsafe fn set_text(control: HWND, text: &str) {
    let text = wide(text);
    let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(control, PCWSTR(text.as_ptr()));
}

unsafe fn move_control(control: HWND, x: i32, y: i32, width: i32, height: i32) {
    let _ = MoveWindow(control, x, y, width.max(0), height.max(0), true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_value_clamps_and_large_values_do_not_overflow() {
        assert_eq!(ProgressValue::new(125, 100).percent(), 100);
        assert_eq!(ProgressValue::new(99, 0).percent(), 0);
        assert_eq!(ProgressValue::new(u64::MAX - 1, u64::MAX).percent(), 99);
    }

    #[test]
    fn install_completion_exposes_restart_choices_as_intents_only() {
        let commands = command_bar_state(
            ProgressStatus::Succeeded,
            false,
            ProgressCompletion::ViaPePrepared,
        );
        assert_eq!(commands.primary.unwrap().intent, ProgressIntent::RestartNow);
        assert_eq!(
            commands.secondary.unwrap().intent,
            ProgressIntent::RestartLater
        );
        assert!(commands.cancel.is_none());
    }

    #[test]
    fn download_completion_has_explicit_follow_up_and_back() {
        let commands = command_bar_state(
            ProgressStatus::Succeeded,
            false,
            ProgressCompletion::Download(DownloadCompletionAction::ContinueInstallation),
        );
        assert_eq!(
            commands.primary.unwrap().intent,
            ProgressIntent::ContinueDownloadedInstallation
        );
        assert_eq!(
            commands.secondary.unwrap().intent,
            ProgressIntent::ReturnToDownloads
        );
    }

    #[test]
    fn download_without_follow_up_still_exposes_a_localized_return_command() {
        let commands = command_bar_state(
            ProgressStatus::Succeeded,
            false,
            ProgressCompletion::Download(DownloadCompletionAction::ReturnToDownloads),
        );
        let primary = commands.primary.expect("download completion return button");
        assert_eq!(primary.intent, ProgressIntent::ReturnToDownloads);
        assert!(!primary.label.is_empty());
        assert!(commands.secondary.is_none());
        assert!(commands.cancel.is_none());
    }

    #[test]
    fn cancel_and_failure_back_semantics_are_preserved() {
        let running = command_bar_state(ProgressStatus::Running, true, ProgressCompletion::Generic);
        assert!(running.cancel_enabled);
        assert_eq!(
            running.cancel.unwrap().intent,
            ProgressIntent::CancelRequested
        );
        let failed = command_bar_state(
            ProgressStatus::Failed,
            false,
            ProgressCompletion::ViaPePrepared,
        );
        assert_eq!(failed.cancel.unwrap().intent, ProgressIntent::Back);
    }

    #[test]
    fn long_translated_labels_remain_owned_and_theme_colors_keep_contrast() {
        let commands = command_bar_state(
            ProgressStatus::Succeeded,
            false,
            ProgressCompletion::ViaPePrepared,
        );
        assert!(!commands.primary.unwrap().label.is_empty());
        assert_ne!(
            status_color(ProgressStatus::Running, Palette::LIGHT),
            status_color(ProgressStatus::Running, Palette::DARK)
        );
        assert_eq!(
            status_color(ProgressStatus::Succeeded, Palette::LIGHT),
            status_color(ProgressStatus::Succeeded, Palette::DARK)
        );
    }

    #[test]
    fn command_metrics_scale_from_100_to_200_percent_dpi() {
        assert_eq!(scale_for_dpi(104, 96), 104);
        assert_eq!(scale_for_dpi(104, 144), 156);
        assert_eq!(scale_for_dpi(104, 192), 208);
        assert_eq!(scale_for_dpi(8, 120), 10);
    }
}
