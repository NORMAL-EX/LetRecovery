//! Reusable Inno Setup 6.7 Modern Windows 11 dialog chrome.
//!
//! The shell owns presentation, keyboard navigation and modal lifetime only. Callers place
//! business controls in [`DialogShell::content`] and receive their normal `WM_COMMAND` /
//! `WM_NOTIFY` messages through the owner window.

use std::mem::size_of;
use std::sync::OnceLock;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{BOOL, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect, GetDC,
    GetMonitorInfoW, InvalidateRect, MonitorFromWindow, RedrawWindow, ReleaseDC, SelectObject,
    SetBkColor, SetBkMode, SetTextColor, DT_CALCRECT, DT_END_ELLIPSIS, DT_NOPREFIX, DT_SINGLELINE,
    DT_VCENTER, HDC, HFONT, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, RDW_ALLCHILDREN,
    RDW_FRAME, RDW_INVALIDATE, RDW_UPDATENOW, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Controls::{SetWindowTheme, DRAWITEMSTRUCT, HDITEMW, HDI_TEXT, ODT_HEADER};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EnumChildWindows,
    EnumThreadWindows, GetClassNameW, GetClientRect, GetMessageW, GetParent, GetWindowLongPtrW,
    GetWindowRect, IsDialogMessageW, IsWindow, IsWindowVisible, LoadCursorW, MoveWindow,
    RegisterClassExW, SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, TranslateMessage, BN_CLICKED, BS_OWNERDRAW, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW,
    GWLP_USERDATA, HMENU, ICON_BIG, ICON_SMALL, IDC_ARROW, MSG, SWP_FRAMECHANGED, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_SHOW, WM_CLOSE, WM_COMMAND, WM_CREATE,
    WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DPICHANGED,
    WM_DRAWITEM, WM_ERASEBKGND, WM_GETFONT, WM_HSCROLL, WM_KEYDOWN, WM_NCCREATE, WM_NCDESTROY,
    WM_NOTIFY, WM_PAINT, WM_SETFONT, WM_SETICON, WM_SETTINGCHANGE, WM_SIZE, WM_SYSCOLORCHANGE,
    WM_THEMECHANGED, WNDCLASSEXW, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
    WS_EX_CONTROLPARENT, WS_EX_DLGMODALFRAME, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};

use super::controls::{child, draw_inno_button, wide, ButtonRole, InnoMetrics};
use super::layout::{measure_text, LayoutMetrics};
use super::theme::{
    apply_control_theme, apply_list_view_theme, apply_progress_theme, apply_trackbar_theme,
    Brushes, NativeControlKind, Palette,
};
use super::{backdrop, redraw};
use crate::core::app_config::{AppConfig, ExperimentalWindowBackdrop};

const DIALOG_CLASS: PCWSTR = w!("LetRecovery.Native.InnoDialog");
const CONTENT_CLASS: PCWSTR = w!("LetRecovery.Native.InnoDialogContent");
const ID_TITLE: u16 = 61_000;
const ID_DESCRIPTION: u16 = 61_001;
const ID_PRIMARY: u16 = 61_002;
const ID_SECONDARY: u16 = 61_003;
const ID_CANCEL: u16 = 61_004;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LogicalRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DialogWindowSize {
    width: i32,
    height: i32,
}

fn clamp_dialog_size(
    requested_width: i32,
    requested_height: i32,
    dpi: u32,
    work_width: i32,
    work_height: i32,
) -> DialogWindowSize {
    let available_width = work_width.max(1);
    let available_height = work_height.max(1);
    let minimum_width = scale(320, dpi).min(available_width);
    let minimum_height = scale(220, dpi).min(available_height);
    DialogWindowSize {
        width: scale(requested_width.max(320), dpi).clamp(minimum_width, available_width),
        height: scale(requested_height.max(220), dpi).clamp(minimum_height, available_height),
    }
}

fn description_height(width: i32, dpi: u32) -> i32 {
    let logical_width = i64::from(width.max(0)) * 96 / i64::from(dpi.max(1));
    if logical_width < 620 {
        scale(62, dpi)
    } else if logical_width < 760 {
        scale(48, dpi)
    } else {
        scale(38, dpi)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DialogLayout {
    pub title: LogicalRect,
    pub description: LogicalRect,
    pub content: LogicalRect,
    pub command_bar: LogicalRect,
    /// Secondary, primary, cancel; absent buttons keep no empty slot.
    pub buttons: [Option<LogicalRect>; 3],
}

impl DialogLayout {
    pub fn calculate(width: i32, height: i32, dpi: u32, buttons: [bool; 3]) -> Self {
        let minimum = InnoMetrics::for_dpi(dpi).button_min_width;
        Self::calculate_with_button_widths(width, height, dpi, buttons, [minimum; 3])
    }

    fn calculate_with_button_widths(
        width: i32,
        height: i32,
        dpi: u32,
        buttons: [bool; 3],
        button_widths: [i32; 3],
    ) -> Self {
        Self::calculate_measured(
            width,
            height,
            dpi,
            buttons,
            button_widths,
            description_height(width, dpi),
        )
    }

    fn calculate_measured(
        width: i32,
        height: i32,
        dpi: u32,
        buttons: [bool; 3],
        button_widths: [i32; 3],
        measured_description_height: i32,
    ) -> Self {
        let s = |value: i32| ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32;
        let layout_metrics = LayoutMetrics::for_dpi(dpi);
        let margin = layout_metrics.outer_margin;
        let command_margin = layout_metrics.command_margin;
        let command_height = layout_metrics.command_height;
        let metrics = InnoMetrics::for_dpi(dpi);
        let title = LogicalRect {
            x: margin,
            y: s(17),
            width: (width - margin * 2).max(0),
            height: s(27),
        };
        let description = LogicalRect {
            x: margin,
            y: s(48),
            width: (width - margin * 2).max(0),
            height: measured_description_height.max(0),
        };
        let command_bar = LogicalRect {
            x: 0,
            y: (height - command_height).max(0),
            width,
            height: command_height.min(height.max(0)),
        };
        let content_top = description.y + description.height + s(8);
        let content = LogicalRect {
            x: margin,
            y: content_top,
            width: (width - margin * 2).max(0),
            height: (command_bar.y - content_top - s(12)).max(0),
        };

        let mut result = [None; 3];
        let mut right = width - command_margin;
        let mut right_hand_button = None;
        for index in (0..3).rev() {
            if buttons[index] {
                if right_hand_button.is_some() {
                    // Tool-dialog commands are peer actions (for example Refresh + Apply), not
                    // the Back/Next pair from a sequential wizard.  Keep Inno's normal command
                    // spacing between every pair so translated labels never look fused together.
                    right -= s(10);
                }
                let button_width = button_widths[index].max(metrics.button_min_width);
                right -= button_width;
                result[index] = Some(LogicalRect {
                    x: right,
                    y: command_bar.y + (command_bar.height - metrics.button_height) / 2,
                    width: button_width,
                    height: metrics.button_height,
                });
                right_hand_button = Some(index);
            }
        }
        Self {
            title,
            description,
            content,
            command_bar,
            buttons: result,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DialogButtons {
    pub primary: String,
    pub secondary: Option<String>,
    pub cancel: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DialogSpec {
    pub window_title: String,
    pub title: String,
    pub description: String,
    pub width: i32,
    pub height: i32,
    pub buttons: DialogButtons,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogResult {
    Primary,
    Secondary,
    Cancel,
}

struct Handles {
    title: HWND,
    description: HWND,
    content: HWND,
    primary: HWND,
    secondary: Option<HWND>,
    cancel: Option<HWND>,
}

struct DialogState {
    owner: HWND,
    hwnd: HWND,
    dpi: u32,
    palette: Palette,
    brushes: Brushes,
    font: HFONT,
    heading_font: HFONT,
    labels: [String; 5],
    handles: Option<Handles>,
    result: Option<DialogResult>,
    first_presentation_pending: bool,
}

impl DialogState {
    fn new(owner: HWND, spec: &DialogSpec) -> Self {
        let palette = Palette::system();
        Self {
            owner,
            hwnd: HWND::default(),
            dpi: 96,
            palette,
            brushes: Brushes::new(palette),
            font: HFONT::default(),
            heading_font: HFONT::default(),
            labels: [
                spec.title.to_owned(),
                spec.description.to_owned(),
                spec.buttons.primary.to_owned(),
                spec.buttons.secondary.clone().unwrap_or_default(),
                spec.buttons.cancel.clone().unwrap_or_default(),
            ],
            handles: None,
            result: None,
            first_presentation_pending: true,
        }
    }

    unsafe fn create_children(&mut self, hwnd: HWND) -> windows::core::Result<()> {
        self.dpi = GetDpiForWindow(hwnd).max(96);
        self.refresh_palette_and_backdrop();
        self.create_fonts();
        let title = child(hwnd, w!("STATIC"), &self.labels[0], 0, ID_TITLE)?;
        let description = child(hwnd, w!("STATIC"), &self.labels[1], 0, ID_DESCRIPTION)?;
        let content = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            CONTENT_CLASS,
            PCWSTR::null(),
            WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
            0,
            0,
            0,
            0,
            hwnd,
            HMENU::default(),
            HINSTANCE::default(),
            None,
        )?;
        let primary = button(hwnd, &self.labels[2], ID_PRIMARY)?;
        let secondary = (!self.labels[3].is_empty())
            .then(|| button(hwnd, &self.labels[3], ID_SECONDARY))
            .transpose()?;
        let cancel = should_create_cancel_button(&self.labels[4], &crate::tr!("关闭"))
            .then(|| button(hwnd, &self.labels[4], ID_CANCEL))
            .transpose()?;
        self.handles = Some(Handles {
            title,
            description,
            content,
            primary,
            secondary,
            cancel,
        });
        self.apply_fonts();
        self.apply_theme();
        self.layout();
        let _ = SetFocus(primary);
        Ok(())
    }

    unsafe fn refresh_palette_and_backdrop(&mut self) {
        let base = Palette::system();
        let requested = AppConfig::load().experimental_window_backdrop;
        let backdrop_active =
            match backdrop::apply_mica(self.hwnd, requested == ExperimentalWindowBackdrop::Mica) {
                Ok(active) => active,
                Err(error) => {
                    if requested == ExperimentalWindowBackdrop::Mica {
                        log::warn!("工具窗口 Mica 不可用，已回退为普通背景: {error}");
                    }
                    false
                }
            };
        self.palette = if backdrop_active {
            base.with_system_backdrop_surface()
        } else {
            base
        };
        self.brushes = Brushes::new(self.palette);
    }

    unsafe fn create_fonts(&mut self) {
        for font in [self.font, self.heading_font] {
            if !font.is_invalid() {
                let _ = DeleteObject(font);
            }
        }
        let face = wide("Microsoft YaHei UI");
        self.font = create_font(scale(12, self.dpi), 400, &face);
        self.heading_font = create_font(scale(15, self.dpi), 600, &face);
    }

    unsafe fn apply_fonts(&self) {
        let Some(handles) = &self.handles else { return };
        for control in [handles.description, handles.primary, handles.content]
            .into_iter()
            .chain(handles.secondary)
            .chain(handles.cancel)
        {
            let _ = SendMessageW(control, WM_SETFONT, WPARAM(self.font.0 as usize), LPARAM(1));
        }
        let _ = SendMessageW(
            handles.title,
            WM_SETFONT,
            WPARAM(self.heading_font.0 as usize),
            LPARAM(1),
        );
    }

    unsafe fn apply_theme(&self) {
        let Some(handles) = &self.handles else { return };
        let dark = i32::from(self.palette.dark);
        let _ = DwmSetWindowAttribute(
            self.hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const i32 as *const _,
            size_of::<i32>() as u32,
        );
        let theme = if self.palette.dark {
            w!("DarkMode_Explorer")
        } else {
            w!("Explorer")
        };
        for button in [Some(handles.primary), handles.secondary, handles.cancel]
            .into_iter()
            .flatten()
        {
            let _ = SetWindowTheme(button, theme, PCWSTR::null());
        }
        // WM_ERASEBKGND is suppressed and WM_PAINT owns the background. Asking USER32 for an
        // erase here adds a redundant paint phase that is visible around owner-drawn buttons.
        let _ = InvalidateRect(self.hwnd, None, false);
        let _ = InvalidateRect(handles.content, None, false);
    }

    unsafe fn layout(&self) {
        let Some(handles) = &self.handles else { return };
        let mut client = RECT::default();
        let _ = GetClientRect(self.hwnd, &mut client);
        let metrics = InnoMetrics::for_dpi(self.dpi);
        let button_widths = [3, 2, 4].map(|label_index| {
            measured_button_width(
                self.hwnd,
                self.font,
                &self.labels[label_index],
                self.dpi,
                metrics.button_min_width,
            )
        });
        let description_width =
            (client.right - LayoutMetrics::for_dpi(self.dpi).outer_margin * 2).max(0);
        let measured_description = measure_text(
            self.hwnd,
            self.font,
            &self.labels[1],
            Some(description_width),
        )
        .height;
        let layout = DialogLayout::calculate_measured(
            client.right,
            client.bottom,
            self.dpi,
            [handles.secondary.is_some(), true, handles.cancel.is_some()],
            button_widths,
            measured_description,
        );
        move_to(handles.title, layout.title);
        move_to(handles.description, layout.description);
        move_to(handles.content, layout.content);
        for (control, rect) in [handles.secondary, Some(handles.primary), handles.cancel]
            .into_iter()
            .zip(layout.buttons)
        {
            if let (Some(control), Some(rect)) = (control, rect) {
                move_to(control, rect);
            }
        }
    }

    unsafe fn fit_content_height(&mut self, logical_content_height: i32) {
        let preferred = scale(logical_content_height.max(0), self.dpi);
        let mut client = RECT::default();
        let mut window = RECT::default();
        let _ = GetClientRect(self.hwnd, &mut client);
        let _ = GetWindowRect(self.hwnd, &mut window);
        let metrics = LayoutMetrics::for_dpi(self.dpi);
        let description_width = (client.right - metrics.outer_margin * 2).max(0);
        let description_height = measure_text(
            self.hwnd,
            self.font,
            &self.labels[1],
            Some(description_width),
        )
        .height;
        let content_top = scale(48, self.dpi) + description_height + metrics.control_gap;
        let target_client_height =
            content_top + preferred + metrics.command_margin + metrics.command_height;
        let non_client_height = (window.bottom - window.top - client.bottom).max(0);
        let work = monitor_work_area(self.hwnd);
        let target_height = (target_client_height + non_client_height)
            .max(scale(220, self.dpi))
            .min((work.bottom - work.top).max(1));
        let width = (window.right - window.left).max(1);
        let x = window
            .left
            .clamp(work.left, (work.right - width).max(work.left));
        let y = window
            .top
            .clamp(work.top, (work.bottom - target_height).max(work.top));
        let _ = SetWindowPos(
            self.hwnd,
            HWND::default(),
            x,
            y,
            width,
            target_height,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
        self.layout();
    }

    unsafe fn choose(&mut self, result: DialogResult) {
        self.result = Some(result);
        let _ = ShowWindow(self.hwnd, SW_HIDE);
    }

    unsafe fn draw_item(&self, item: &DRAWITEMSTRUCT) {
        if item.CtlType.0 == ODT_HEADER {
            self.draw_header(item);
            return;
        }
        let role = if item.CtlID == u32::from(ID_PRIMARY) {
            ButtonRole::Primary
        } else {
            ButtonRole::Secondary
        };
        draw_inno_button(item, self.palette, role, self.font, self.dpi);
    }

    unsafe fn draw_header(&self, item: &DRAWITEMSTRUCT) {
        let background = CreateSolidBrush(self.palette.button);
        let _ = FillRect(item.hDC, &item.rcItem, background);
        let _ = DeleteObject(background);

        let mut text = vec![0u16; 256];
        let mut header_item = HDITEMW {
            mask: HDI_TEXT,
            pszText: windows::core::PWSTR(text.as_mut_ptr()),
            cchTextMax: text.len() as i32,
            ..Default::default()
        };
        let _ = SendMessageW(
            item.hwndItem,
            0x120B, // HDM_GETITEMW
            WPARAM(item.itemID as usize),
            LPARAM((&mut header_item as *mut HDITEMW) as isize),
        );
        let length = text
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(text.len());
        text.truncate(length);

        let _ = SetBkMode(item.hDC, TRANSPARENT);
        let _ = SetTextColor(item.hDC, self.palette.text);
        let old_font = SelectObject(item.hDC, self.font);
        let mut text_rect = item.rcItem;
        text_rect.left += scale(8, self.dpi);
        text_rect.right -= scale(6, self.dpi);
        let _ = DrawTextW(
            item.hDC,
            &mut text,
            &mut text_rect,
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        let _ = SelectObject(item.hDC, old_font);

        let mut separator = item.rcItem;
        separator.left = separator.right - 1;
        separator.top += scale(4, self.dpi);
        separator.bottom -= scale(4, self.dpi);
        let brush = CreateSolidBrush(self.palette.separator);
        let _ = FillRect(item.hDC, &separator, brush);
        let _ = DeleteObject(brush);
    }
}

fn should_create_cancel_button(label: &str, localized_close: &str) -> bool {
    !label.is_empty() && label != localized_close
}

impl Drop for DialogState {
    fn drop(&mut self) {
        unsafe {
            for font in [self.font, self.heading_font] {
                if !font.is_invalid() {
                    let _ = DeleteObject(font);
                }
            }
        }
    }
}

/// Stable owner of a top-level dialog. It can be shown repeatedly as modal or modeless.
pub struct DialogShell {
    state: Box<DialogState>,
}

impl DialogShell {
    pub unsafe fn create(owner: HWND, spec: DialogSpec) -> windows::core::Result<Self> {
        register_classes()?;
        let mut state = Box::new(DialogState::new(owner, &spec));
        let title = wide(&spec.window_title);
        let initial_dpi = if owner.is_invalid() {
            96
        } else {
            GetDpiForWindow(owner).max(96)
        };
        let work = monitor_work_area(owner);
        let size = clamp_dialog_size(
            spec.width,
            spec.height,
            initial_dpi,
            work.right - work.left,
            work.bottom - work.top,
        );
        let x = work.left + ((work.right - work.left - size.width) / 2).max(0);
        let y = work.top + ((work.bottom - work.top - size.height) / 2).max(0);
        let hwnd = CreateWindowExW(
            WS_EX_DLGMODALFRAME | WS_EX_CONTROLPARENT,
            DIALOG_CLASS,
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN,
            x,
            y,
            size.width,
            size.height,
            owner,
            HMENU::default(),
            HINSTANCE::default(),
            Some(state.as_mut() as *mut DialogState as *const _),
        )?;
        state.hwnd = hwnd;
        // A NULL WM_SETICON lParam removes the requested per-window icon. Clear both sizes and
        // refresh the hidden dialog's non-client frame before it can be shown. This keeps the
        // normal-height dialog caption while leaving the main-window class and icons untouched.
        let _ = SendMessageW(hwnd, WM_SETICON, WPARAM(ICON_SMALL as usize), LPARAM(0));
        let _ = SendMessageW(hwnd, WM_SETICON, WPARAM(ICON_BIG as usize), LPARAM(0));
        let _ = SetWindowPos(
            hwnd,
            HWND::default(),
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER,
        );
        state.apply_theme();
        Ok(Self { state })
    }

    pub fn hwnd(&self) -> HWND {
        self.state.hwnd
    }

    pub fn content(&self) -> HWND {
        self.state
            .handles
            .as_ref()
            .map_or(HWND::default(), |h| h.content)
    }

    /// Returns the palette already resolved against the shell's actual DWM material state.
    /// Tool-specific controls created after the shell must use this instead of recomputing the
    /// ordinary system palette and accidentally replacing Mica-safe text or field colors.
    pub fn palette(&self) -> Palette {
        self.state.palette
    }

    /// Fits the shell around the tool's natural visible content. The translated description is
    /// measured with the actual dialog font before the final window height is chosen.
    pub unsafe fn fit_content_height(&mut self, logical_content_height: i32) {
        self.state.fit_content_height(logical_content_height);
    }

    pub unsafe fn set_primary_enabled(&self, enabled: bool) {
        if let Some(handles) = &self.state.handles {
            let _ = EnableWindow(handles.primary, enabled);
        }
    }

    pub unsafe fn show_modeless(&mut self) {
        // A completed command hides the window until its owner consumes the result.  Async
        // inventory completions can arrive in that small interval; they must never clear the
        // pending Close/Primary result and resurrect a window the user already dismissed.
        if self.state.result.is_some() {
            return;
        }
        // Tool-specific controls are created after the shell. Prepare every descendant while the
        // top-level window is still hidden, so USER32 never exposes the common-control defaults
        // (white empty ListView bodies, black header/check text, or the default GUI font).
        prepare_dialog_descendants(&self.state);
        let first_visible_frame = self.state.first_presentation_pending;
        let _ = ShowWindow(self.state.hwnd, SW_SHOW);
        if first_visible_frame {
            // Paint the complete dialog once before the owner starts its asynchronous inventory.
            // Otherwise USER32 can expose unpainted white ListView/button child surfaces until
            // the next queued paint message, which is especially visible on tool dialogs.
            let _ = RedrawWindow(
                self.state.hwnd,
                None,
                None,
                RDW_INVALIDATE | RDW_FRAME | RDW_ALLCHILDREN | RDW_UPDATENOW,
            );
            self.state.first_presentation_pending = false;
        }
        if let Some(handles) = &self.state.handles {
            let _ = SetFocus(handles.primary);
        }
    }

    /// Brings an already-open modeless dialog to the foreground. Tool routing uses this to make
    /// repeated button notifications idempotent instead of creating a second window.
    pub unsafe fn activate_if_visible(&self) -> bool {
        if !IsWindowVisible(self.state.hwnd).as_bool() {
            return false;
        }
        let _ = ShowWindow(self.state.hwnd, SW_SHOW);
        let _ = SetForegroundWindow(self.state.hwnd);
        true
    }

    pub fn take_result(&mut self) -> Option<DialogResult> {
        self.state.result.take()
    }

    /// Runs the standard nested dialog loop. No business callback is executed here.
    pub unsafe fn show_modal(&mut self) -> DialogResult {
        self.show_modeless();
        if !self.state.owner.is_invalid() {
            let _ = EnableWindow(self.state.owner, false);
        }
        let mut message = MSG::default();
        while self.state.result.is_none() && GetMessageW(&mut message, HWND::default(), 0, 0).into()
        {
            if !IsDialogMessageW(self.state.hwnd, &message).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        if !self.state.owner.is_invalid() && IsWindow(self.state.owner).as_bool() {
            let _ = EnableWindow(self.state.owner, true);
            let _ = SetFocus(self.state.owner);
        }
        self.state.result.take().unwrap_or(DialogResult::Cancel)
    }
}

struct DialogDescendantTheme {
    palette: Palette,
    font: HFONT,
}

unsafe fn prepare_dialog_descendants(state: &DialogState) {
    let context = DialogDescendantTheme {
        palette: state.palette,
        font: state.font,
    };
    let _ = EnumChildWindows(
        state.hwnd,
        Some(prepare_dialog_descendant),
        LPARAM((&context as *const DialogDescendantTheme) as isize),
    );
    super::theme::apply_backdrop_composition_to_descendants(state.hwnd, state.palette);
}

/// Reapplies the persisted Mica setting to every open reusable tool shell on the UI thread.
pub(crate) unsafe fn refresh_open_dialog_backdrops() {
    let _ = EnumThreadWindows(
        GetCurrentThreadId(),
        Some(refresh_dialog_backdrop),
        LPARAM(0),
    );
}

unsafe extern "system" fn refresh_dialog_backdrop(hwnd: HWND, _lparam: LPARAM) -> BOOL {
    let mut class_name = [0u16; 64];
    let length = GetClassNameW(hwnd, &mut class_name);
    if length > 0
        && String::from_utf16_lossy(&class_name[..length as usize])
            .eq_ignore_ascii_case("LetRecovery.Native.InnoDialog")
    {
        let _ = SendMessageW(hwnd, WM_SETTINGCHANGE, WPARAM(0), LPARAM(0));
    }
    BOOL(1)
}

unsafe extern "system" fn prepare_dialog_descendant(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let context = &*(lparam.0 as *const DialogDescendantTheme);
    if SendMessageW(hwnd, WM_GETFONT, WPARAM(0), LPARAM(0)).0 == 0 {
        let _ = SendMessageW(hwnd, WM_SETFONT, WPARAM(context.font.0 as usize), LPARAM(0));
    }

    let mut class_name = [0u16; 32];
    let length = GetClassNameW(hwnd, &mut class_name);
    let class_name = if length > 0 {
        String::from_utf16_lossy(&class_name[..length as usize])
    } else {
        String::new()
    };
    if class_name.eq_ignore_ascii_case("SysListView32") {
        let _ = apply_list_view_theme(hwnd, context.palette);
    } else if class_name.eq_ignore_ascii_case("Edit") || class_name.eq_ignore_ascii_case("ComboBox")
    {
        apply_control_theme(hwnd, context.palette, NativeControlKind::Field);
    } else if class_name.eq_ignore_ascii_case("ListBox") {
        apply_control_theme(hwnd, context.palette, NativeControlKind::List);
    } else if class_name.eq_ignore_ascii_case("Button") {
        apply_control_theme(hwnd, context.palette, NativeControlKind::General);
    } else if class_name.eq_ignore_ascii_case("msctls_progress32") {
        apply_progress_theme(hwnd, context.palette);
    } else if class_name.eq_ignore_ascii_case("msctls_trackbar32") {
        apply_trackbar_theme(hwnd, context.palette);
    }
    let _ = InvalidateRect(hwnd, None, false);
    BOOL(1)
}

unsafe fn monitor_work_area(hwnd: HWND) -> RECT {
    let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !monitor.is_invalid() && GetMonitorInfoW(monitor, &mut info).as_bool() {
        info.rcWork
    } else {
        // A nearest monitor is normally always available; keep a conservative fallback for
        // unusually early process startup and test hosts without an interactive desktop.
        RECT {
            left: 0,
            top: 0,
            right: 1024,
            bottom: 768,
        }
    }
}

impl Drop for DialogShell {
    fn drop(&mut self) {
        unsafe {
            if !self.state.hwnd.is_invalid() && IsWindow(self.state.hwnd).as_bool() {
                let _ = DestroyWindow(self.state.hwnd);
            }
        }
    }
}

unsafe fn register_classes() -> windows::core::Result<()> {
    static REGISTERED: OnceLock<bool> = OnceLock::new();
    if *REGISTERED.get_or_init(|| {
        let Ok(module) = GetModuleHandleW(None) else {
            return false;
        };
        let Ok(cursor) = LoadCursorW(None, IDC_ARROW) else {
            return false;
        };
        let dialog = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(dialog_proc),
            hInstance: HINSTANCE(module.0),
            hCursor: cursor,
            // Tool dialogs use a dedicated icon-free class. USER32 may substitute a process
            // default for empty class fields, so create() also clears both instance icons and
            // refreshes the non-client frame before first show. The main window uses its own
            // class and keeps the published large/small icons.
            hIcon: Default::default(),
            hIconSm: Default::default(),
            lpszClassName: DIALOG_CLASS,
            ..Default::default()
        };
        let content = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(content_proc),
            hInstance: HINSTANCE(module.0),
            hCursor: cursor,
            lpszClassName: CONTENT_CLASS,
            ..Default::default()
        };
        RegisterClassExW(&dialog) != 0 && RegisterClassExW(&content) != 0
    }) {
        Ok(())
    } else {
        Err(windows::core::Error::from_win32())
    }
}

unsafe extern "system" fn dialog_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = &*(lparam.0 as *const CREATESTRUCTW);
        let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        let state = &mut *(create.lpCreateParams as *mut DialogState);
        state.hwnd = hwnd;
    }
    let state = (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut DialogState).as_mut();
    match message {
        WM_CREATE => {
            if state.is_some_and(|state| state.create_children(hwnd).is_err()) {
                LRESULT(-1)
            } else {
                LRESULT(0)
            }
        }
        WM_SIZE => {
            if let Some(state) = state {
                state.layout();
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            if let Some(state) = state {
                state.dpi = GetDpiForWindow(hwnd).max(96);
                let suggested = &*(lparam.0 as *const RECT);
                let work = monitor_work_area(hwnd);
                let width = (suggested.right - suggested.left)
                    .max(1)
                    .min((work.right - work.left).max(1));
                let height = (suggested.bottom - suggested.top)
                    .max(1)
                    .min((work.bottom - work.top).max(1));
                let x = suggested
                    .left
                    .clamp(work.left, (work.right - width).max(work.left));
                let y = suggested
                    .top
                    .clamp(work.top, (work.bottom - height).max(work.top));
                let _ = SetWindowPos(
                    hwnd,
                    HWND::default(),
                    x,
                    y,
                    width,
                    height,
                    SWP_NOACTIVATE | SWP_NOZORDER,
                );
                state.create_fonts();
                state.apply_fonts();
                state.layout();
            }
            LRESULT(0)
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED | WM_SYSCOLORCHANGE => {
            if let Some(state) = state {
                // Existing UxTheme handles become stale on WM_THEMECHANGED.  Freeze the complete
                // dialog while replacing brushes and descendant theme classes so a modeless tool
                // never shows half of each palette during an online system-theme switch.
                let redraw = redraw::suspend(hwnd);
                state.refresh_palette_and_backdrop();
                state.apply_theme();
                // Reapplying an existing subclass updates its palette reference data. Walk every
                // live descendant in the same frozen frame so fields, ComboLBox popups, reports,
                // choices, progress bars and sliders cannot retain their creation-time theme.
                prepare_dialog_descendants(state);
                redraw::resume(hwnd, redraw);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            if let Some(state) = state {
                let command_id = (wparam.0 & 0xffff) as u16;
                let notification = ((wparam.0 >> 16) & 0xffff) as u16;
                match command_id {
                    ID_PRIMARY if notification == BN_CLICKED as u16 => {
                        state.choose(DialogResult::Primary)
                    }
                    ID_SECONDARY if notification == BN_CLICKED as u16 => {
                        state.choose(DialogResult::Secondary)
                    }
                    ID_CANCEL if notification == BN_CLICKED as u16 => {
                        state.choose(DialogResult::Cancel)
                    }
                    ID_PRIMARY | ID_SECONDARY | ID_CANCEL => {}
                    _ if !state.owner.is_invalid() => {
                        let _ = SendMessageW(state.owner, message, wparam, lparam);
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_NOTIFY | WM_HSCROLL => {
            if let Some(state) = state {
                if !state.owner.is_invalid() {
                    return SendMessageW(state.owner, message, wparam, lparam);
                }
            }
            LRESULT(0)
        }
        WM_DRAWITEM => {
            if let Some(state) = state {
                state.draw_item(&*(lparam.0 as *const DRAWITEMSTRUCT));
                return LRESULT(1);
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            if let Some(state) = state {
                let dc = HDC(wparam.0 as *mut _);
                let _ = SetTextColor(
                    dc,
                    if state.palette.dark {
                        state.palette.text
                    } else {
                        state.palette.foreground_black()
                    },
                );
                let _ = SetBkColor(dc, state.palette.window);
                let _ = SetBkMode(dc, TRANSPARENT);
                return LRESULT(state.brushes.window.0 as isize);
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_CTLCOLOREDIT => {
            if let Some(state) = state {
                let dc = HDC(wparam.0 as *mut _);
                let control = HWND(lparam.0 as *mut _);
                let background = state.palette.edit_brush_color_for(control);
                let _ = SetTextColor(dc, state.palette.edit_text_color_for(control));
                let _ = SetBkColor(dc, background);
                let brush = if background == state.palette.edit {
                    state.brushes.edit_opaque
                } else {
                    state.brushes.edit
                };
                return LRESULT(brush.0 as isize);
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_CTLCOLORLISTBOX => {
            if let Some(state) = state {
                let dc = HDC(wparam.0 as *mut _);
                let _ = SetTextColor(dc, state.palette.text);
                let _ = SetBkColor(dc, state.palette.edit);
                return LRESULT(state.brushes.list.0 as isize);
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_CLOSE => {
            if let Some(state) = state {
                state.choose(DialogResult::Cancel);
            }
            LRESULT(0)
        }
        WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize => {
            if let Some(state) = state {
                state.choose(DialogResult::Cancel);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            if let Some(state) = state {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut paint);
                // Honour the update region instead of repainting the complete dialog behind all
                // child controls. This keeps local button invalidation local.
                let _ = FillRect(dc, &paint.rcPaint, state.brushes.window);
                let _ = EndPaint(hwnd, &paint);
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_NCDESTROY => {
            let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

unsafe extern "system" fn content_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_COMMAND | WM_NOTIFY | WM_DRAWITEM | WM_HSCROLL | WM_CTLCOLORBTN | WM_CTLCOLORSTATIC
        | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            SendMessageW(GetParent(hwnd).unwrap_or_default(), message, wparam, lparam)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let parent = GetParent(hwnd).unwrap_or_default();
            let state = (GetWindowLongPtrW(parent, GWLP_USERDATA) as *mut DialogState).as_ref();
            if let Some(state) = state {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut paint);
                let _ = FillRect(dc, &paint.rcPaint, state.brushes.window);
                let _ = EndPaint(hwnd, &paint);
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

unsafe fn button(parent: HWND, text: &str, id: u16) -> windows::core::Result<HWND> {
    child(
        parent,
        w!("BUTTON"),
        text,
        BS_OWNERDRAW | WS_TABSTOP.0 as i32,
        id,
    )
}

unsafe fn create_font(height: i32, weight: i32, face: &[u16]) -> HFONT {
    CreateFontW(
        -height,
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        1,
        0,
        0,
        5,
        0,
        PCWSTR(face.as_ptr()),
    )
}

/// Inno uses 75 logical pixels as a minimum, but grows command buttons for translated text.
/// Measuring with the actual dialog font avoids clipping Chinese and longer English captions at
/// high DPI while retaining the compact baseline for short labels.
unsafe fn measured_button_width(
    hwnd: HWND,
    font: HFONT,
    label: &str,
    dpi: u32,
    minimum: i32,
) -> i32 {
    if label.is_empty() {
        return minimum;
    }
    let dc = GetDC(hwnd);
    if dc.is_invalid() {
        return minimum;
    }
    let old_font = SelectObject(dc, font);
    let mut text = wide(label);
    let mut bounds = RECT::default();
    let _ = DrawTextW(
        dc,
        &mut text,
        &mut bounds,
        DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX,
    );
    let _ = SelectObject(dc, old_font);
    let _ = ReleaseDC(hwnd, dc);
    let horizontal_padding = scale(24, dpi);
    (bounds.right - bounds.left + horizontal_padding).max(minimum)
}

const fn scale(value: i32, dpi: u32) -> i32 {
    ((value as i64 * dpi as i64 + 48) / 96) as i32
}

unsafe fn move_to(hwnd: HWND, rect: LogicalRect) {
    let _ = MoveWindow(hwnd, rect.x, rect.y, rect.width, rect.height, true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_compact_and_keeps_content_above_commands() {
        let layout = DialogLayout::calculate(640, 440, 96, [true, true, true]);
        assert_eq!(layout.content.x, 28);
        assert_eq!(layout.command_bar.height, 46);
        assert!(layout.content.y + layout.content.height < layout.command_bar.y);
        assert_eq!(layout.buttons[1].unwrap().height, 23);
    }

    #[test]
    fn buttons_pack_from_right_without_empty_slots() {
        let layout = DialogLayout::calculate(520, 320, 96, [false, true, true]);
        assert!(layout.buttons[0].is_none());
        let primary = layout.buttons[1].unwrap();
        let cancel = layout.buttons[2].unwrap();
        assert_eq!(cancel.x - (primary.x + primary.width), 10);
        assert_eq!(cancel.x + cancel.width, 520 - 12);
    }

    #[test]
    fn translated_command_button_can_grow_without_changing_command_gaps() {
        let layout = DialogLayout::calculate_with_button_widths(
            640,
            360,
            96,
            [false, true, true],
            [75, 142, 75],
        );
        let primary = layout.buttons[1].unwrap();
        let cancel = layout.buttons[2].unwrap();
        assert_eq!(primary.width, 142);
        assert_eq!(cancel.x - (primary.x + primary.width), 10);
        assert_eq!(cancel.x + cancel.width, 640 - 12);
    }

    #[test]
    fn titlebar_only_close_omits_redundant_command_button() {
        assert!(!should_create_cancel_button("", "关闭"));
        assert!(!should_create_cancel_button("关闭", "关闭"));
        assert!(!should_create_cancel_button("Close", "Close"));
        assert!(should_create_cancel_button("取消", "关闭"));
        assert!(should_create_cancel_button("Cancel", "Close"));
    }

    #[test]
    fn dpi_scaling_preserves_button_order_and_doubles_metrics() {
        let layout = DialogLayout::calculate(1280, 880, 192, [true, true, true]);
        assert_eq!(layout.title.x, 56);
        assert_eq!(layout.command_bar.height, 92);
        assert_eq!(layout.buttons[0].unwrap().height, 46);
        assert!(layout.buttons[0].unwrap().x < layout.buttons[1].unwrap().x);
        assert_eq!(
            layout.buttons[1].unwrap().x
                - (layout.buttons[0].unwrap().x + layout.buttons[0].unwrap().width),
            20
        );
    }

    #[test]
    fn constrained_height_never_produces_negative_content() {
        let layout = DialogLayout::calculate(360, 120, 144, [false, true, false]);
        assert_eq!(layout.content.height, 0);
        assert!(layout.command_bar.y >= 0);
    }

    #[test]
    fn dialog_size_is_clamped_to_low_resolution_work_area_at_200_percent() {
        let size = clamp_dialog_size(760, 560, 192, 1280, 680);
        assert_eq!(size.width, 1280);
        assert_eq!(size.height, 680);
    }

    #[test]
    fn narrow_dialog_reserves_more_lines_for_long_descriptions() {
        assert!(description_height(560, 96) > description_height(820, 96));
        let layout = DialogLayout::calculate(560, 440, 96, [false, true, true]);
        assert_eq!(
            layout.content.y,
            layout.description.y + layout.description.height + 8
        );
    }
}
