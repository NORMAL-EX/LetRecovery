//! Native presentation for installation advanced options.
//!
//! This page only mirrors [`AdvancedOptionsData`] into Win32 controls. It deliberately does not
//! browse files, capture Wi-Fi credentials, touch an offline registry, or start installation.

use std::cell::Cell;
use std::ffi::c_void;
use std::mem::size_of;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateRectRgn,
    CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, GetBkColor, GetDC,
    InvalidateRect, ReleaseDC, SelectObject, SetBkMode, SetTextColor, SetWindowRgn, DT_LEFT,
    DT_NOPREFIX, DT_SINGLELINE, HBRUSH, HFONT, HRGN, PAINTSTRUCT, RGN_ERROR, SRCCOPY, TRANSPARENT,
};
use windows::Win32::UI::Controls::{SetScrollInfo, DRAWITEMSTRUCT};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetCapture, IsWindowEnabled, ReleaseCapture, SetCapture, TrackMouseEvent,
    TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT,
};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, GetCursorPos, GetParent, GetPropW, GetScrollBarInfo, GetScrollInfo,
    GetWindowRect, GetWindowTextLengthW, GetWindowTextW, PostMessageW, RemovePropW, ScrollWindowEx,
    SendMessageW, SetPropW, ShowWindow, BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX,
    BS_AUTORADIOBUTTON, BS_OWNERDRAW, ES_AUTOHSCROLL, ES_PASSWORD, HMENU, HTCLIENT, OBJID_VSCROLL,
    SB_BOTTOM, SB_ENDSCROLL, SB_LINEDOWN, SB_LINEUP, SB_PAGEDOWN, SB_PAGEUP, SB_THUMBPOSITION,
    SB_THUMBTRACK, SB_TOP, SB_VERT, SCROLLBARINFO, SCROLLINFO, SIF_PAGE, SIF_POS, SIF_RANGE,
    SIF_TRACKPOS, SW_HIDE, SW_INVALIDATE, SW_SCROLLCHILDREN, SW_SHOW, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_APP, WM_CAPTURECHANGED, WM_COMMAND, WM_CTLCOLORBTN, WM_CTLCOLOREDIT,
    WM_CTLCOLORSTATIC, WM_DRAWITEM, WM_ENABLE, WM_ERASEBKGND, WM_GETFONT, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCACTIVATE, WM_NCDESTROY, WM_NCHITTEST,
    WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCPAINT, WM_PAINT, WM_SETFONT, WM_SIZE, WM_THEMECHANGED,
    WM_VSCROLL, WS_CHILD, WS_CLIPCHILDREN, WS_EX_CLIENTEDGE, WS_GROUP, WS_TABSTOP, WS_VSCROLL,
};

use super::super::controls::{
    alpha_blend_premultiplied_bgra, center_single_line_edit_in_row, child,
    move_layout_window as MoveWindow, wide, InnoMetrics,
};
use super::super::layout::measure_text;
use super::super::scrollbar_compositor;
use super::super::theme::{apply_control_theme, NativeControlKind, Palette};
use crate::core::ui_state::{
    default_install_username, AdvancedOptionCapabilities, AdvancedOptionsData,
};

const ID_FIRST: u16 = 700;
const MIN_THREE_COLUMN_WIDTH: i32 = 320;
const MIN_TWO_COLUMN_WIDTH: i32 = 250;
const COLUMN_GAP: i32 = 16;
const VERTICAL_SCROLLBAR_WIDTH: i32 = 17;
const SCROLLBAR_CONTENT_GAP: i32 = 24;
const SS_OWNERDRAW_STYLE: i32 = 0x0000_000d;
const VIEWPORT_SUBCLASS_ID: usize = 1;
const SCROLLBAR_OVERLAY_SUBCLASS_ID: usize = 2;
const WHEEL_DELTA: i32 = 120;
const WM_NCMOUSEMOVE_MESSAGE: u32 = 0x00a0;
const WM_NCMOUSELEAVE_MESSAGE: u32 = 0x02a2;
const WM_MOUSELEAVE_MESSAGE: u32 = 0x02a3;
const ADVANCED_SCROLLBAR_STATE_PROPERTY: PCWSTR = w!("LetRecovery.AdvancedScrollbarThemeState");
const ADVANCED_SCROLLBAR_OVERLAY_PROPERTY: PCWSTR = w!("LetRecovery.AdvancedScrollbarOverlay");
const ADVANCED_SCROLLBAR_DRAG_OFFSET_PROPERTY: PCWSTR =
    w!("LetRecovery.AdvancedScrollbarDragOffset");
const ADVANCED_SCROLLBAR_PROXY_POSITION_PROPERTY: PCWSTR =
    w!("LetRecovery.AdvancedScrollbarProxyPosition");
const ADVANCED_SCROLLBAR_PENDING_POSITION_PROPERTY: PCWSTR =
    w!("LetRecovery.AdvancedScrollbarPendingPosition");
const ADVANCED_SCROLLBAR_PENDING_CODE_PROPERTY: PCWSTR =
    w!("LetRecovery.AdvancedScrollbarPendingCode");
const ADVANCED_SCROLLBAR_FRAME_PENDING_PROPERTY: PCWSTR =
    w!("LetRecovery.AdvancedScrollbarFramePending");
const WM_ADVANCED_SCROLLBAR_FRAME: u32 = WM_APP + 0x2d;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AdvancedViewportGeometry {
    content_width: i32,
    scrollbar_left: i32,
    scrollbar_width: i32,
    corner_diameter: i32,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AdvancedScrollbarThumb {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[cfg(test)]
impl AdvancedScrollbarThumb {
    fn calculate(
        track_width: i32,
        track_height: i32,
        minimum: i32,
        maximum: i32,
        page: u32,
        position: i32,
        dpi: u32,
    ) -> Option<Self> {
        let track_width = track_width.max(0);
        let track_height = track_height.max(0);
        let content_length = maximum.saturating_sub(minimum).saturating_add(1).max(1);
        let page = i32::try_from(page)
            .unwrap_or(i32::MAX)
            .clamp(0, content_length);
        let maximum_position = content_length.saturating_sub(page);
        if track_width == 0 || track_height == 0 || maximum_position == 0 {
            return None;
        }

        let scale = |value: i32| ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32;
        let padding = scale(5).min(track_height / 2);
        let available_height = (track_height - padding * 2).max(1);
        let thumb_height = ((i64::from(available_height) * i64::from(page)
            / i64::from(content_length)) as i32)
            .max(scale(28))
            .min(available_height);
        let travel = available_height - thumb_height;
        let clamped_position = position.clamp(minimum, minimum.saturating_add(maximum_position));
        let top = padding
            + ((i64::from(travel) * i64::from(clamped_position.saturating_sub(minimum))
                / i64::from(maximum_position)) as i32);
        Some(Self {
            left: 0,
            top,
            right: track_width,
            bottom: top + thumb_height,
        })
    }
}

#[derive(Clone, Copy)]
struct EmbeddedScrollbarGlyph {
    width: i32,
    height: i32,
    sizing_left: i32,
    sizing_top: i32,
    sizing_right: i32,
    sizing_bottom: i32,
    bgra: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/win11_scrollbar_theme.rs"));

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum AdvancedScrollbarState {
    #[default]
    Normal = 0,
    Hot = 1,
    Pressed = 2,
    Disabled = 3,
    Hover = 4,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AdvancedScrollbarPart {
    TopArrow = 0,
    UpperTrack = 1,
    Thumb = 2,
    LowerTrack = 3,
    BottomArrow = 4,
}

const ADVANCED_SCROLLBAR_PARTS: [AdvancedScrollbarPart; 5] = [
    AdvancedScrollbarPart::TopArrow,
    AdvancedScrollbarPart::UpperTrack,
    AdvancedScrollbarPart::Thumb,
    AdvancedScrollbarPart::LowerTrack,
    AdvancedScrollbarPart::BottomArrow,
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum AdvancedScrollbarInteraction {
    #[default]
    Normal,
    Hovered(AdvancedScrollbarPart),
    Pressed(AdvancedScrollbarPart),
    Disabled,
}

impl AdvancedScrollbarInteraction {
    const fn shows_expanded_parts(self) -> bool {
        matches!(self, Self::Hovered(_) | Self::Pressed(_))
    }

    const fn state_for(self, part: AdvancedScrollbarPart) -> AdvancedScrollbarState {
        match self {
            Self::Normal => AdvancedScrollbarState::Normal,
            Self::Hovered(active) => {
                if active as u8 == part as u8 {
                    AdvancedScrollbarState::Hot
                } else {
                    AdvancedScrollbarState::Hover
                }
            }
            Self::Pressed(active) => {
                if active as u8 == part as u8 {
                    AdvancedScrollbarState::Pressed
                } else {
                    AdvancedScrollbarState::Hover
                }
            }
            Self::Disabled => AdvancedScrollbarState::Disabled,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AdvancedScrollbarGeometry {
    bar: RECT,
    top_arrow: RECT,
    upper_track: RECT,
    thumb: RECT,
    lower_track: RECT,
    bottom_arrow: RECT,
}

impl AdvancedScrollbarGeometry {
    fn from_scrollbar_info(info: &SCROLLBARINFO) -> Option<Self> {
        const STATE_SYSTEM_INVISIBLE: u32 = 0x0000_8000;
        const STATE_SYSTEM_OFFSCREEN: u32 = 0x0001_0000;

        let bar = info.rcScrollBar;
        let width = bar.right.saturating_sub(bar.left);
        let height = bar.bottom.saturating_sub(bar.top);
        if width <= 0
            || height <= 0
            || info.rgstate[0] & (STATE_SYSTEM_INVISIBLE | STATE_SYSTEM_OFFSCREEN) != 0
        {
            return None;
        }

        let line_button = info.dxyLineButton.clamp(0, height / 2);
        let track_top = bar.top.saturating_add(line_button);
        let track_bottom = bar.bottom.saturating_sub(line_button).max(track_top);
        // SCROLLBARINFO reports the thumb coordinates as offsets from rcScrollBar's leading
        // edge for a non-client scrollbar. Keep them inside the region between both line buttons
        // so corrupt or reduced WinPE implementations cannot produce overlapping rectangles.
        let thumb_top = bar
            .top
            .saturating_add(info.xyThumbTop)
            .clamp(track_top, track_bottom);
        let thumb_bottom = bar
            .top
            .saturating_add(info.xyThumbBottom)
            .clamp(thumb_top, track_bottom);

        Some(Self {
            bar,
            top_arrow: RECT {
                left: bar.left,
                top: bar.top,
                right: bar.right,
                bottom: track_top,
            },
            upper_track: RECT {
                left: bar.left,
                top: track_top,
                right: bar.right,
                bottom: thumb_top,
            },
            thumb: RECT {
                left: bar.left,
                top: thumb_top,
                right: bar.right,
                bottom: thumb_bottom,
            },
            lower_track: RECT {
                left: bar.left,
                top: thumb_bottom,
                right: bar.right,
                bottom: track_bottom,
            },
            bottom_arrow: RECT {
                left: bar.left,
                top: track_bottom,
                right: bar.right,
                bottom: bar.bottom,
            },
        })
    }

    const fn rect_for(self, part: AdvancedScrollbarPart) -> RECT {
        match part {
            AdvancedScrollbarPart::TopArrow => self.top_arrow,
            AdvancedScrollbarPart::UpperTrack => self.upper_track,
            AdvancedScrollbarPart::Thumb => self.thumb,
            AdvancedScrollbarPart::LowerTrack => self.lower_track,
            AdvancedScrollbarPart::BottomArrow => self.bottom_arrow,
        }
    }

    fn hit_test(self, point: POINT) -> Option<AdvancedScrollbarPart> {
        ADVANCED_SCROLLBAR_PARTS
            .into_iter()
            .find(|part| point_in_rect(self.rect_for(*part), point))
    }

    fn translated(self, dx: i32, dy: i32) -> Self {
        let translate = |rect: RECT| RECT {
            left: rect.left.saturating_add(dx),
            top: rect.top.saturating_add(dy),
            right: rect.right.saturating_add(dx),
            bottom: rect.bottom.saturating_add(dy),
        };
        Self {
            bar: translate(self.bar),
            top_arrow: translate(self.top_arrow),
            upper_track: translate(self.upper_track),
            thumb: translate(self.thumb),
            lower_track: translate(self.lower_track),
            bottom_arrow: translate(self.bottom_arrow),
        }
    }
}

fn point_in_rect(rect: RECT, point: POINT) -> bool {
    rect.right > rect.left
        && rect.bottom > rect.top
        && point.x >= rect.left
        && point.x < rect.right
        && point.y >= rect.top
        && point.y < rect.bottom
}

const fn embedded_scrollbar_dpi_index(dpi: u32) -> usize {
    if dpi < 108 {
        0
    } else if dpi < 132 {
        1
    } else if dpi < 168 {
        2
    } else if dpi < 216 {
        3
    } else {
        4
    }
}

fn embedded_scrollbar_thumb_glyph(
    dark: bool,
    dpi: u32,
    state: AdvancedScrollbarState,
) -> &'static EmbeddedScrollbarGlyph {
    let mode = usize::from(dark);
    let dpi = embedded_scrollbar_dpi_index(dpi);
    &WIN11_SCROLLBAR_THUMB_GLYPHS[((mode * 5 + dpi) * 5) + state as usize]
}

fn embedded_scrollbar_track_glyph(
    dark: bool,
    dpi: u32,
    state: AdvancedScrollbarState,
) -> &'static EmbeddedScrollbarGlyph {
    let mode = usize::from(dark);
    let dpi = embedded_scrollbar_dpi_index(dpi);
    &WIN11_SCROLLBAR_TRACK_GLYPHS[((mode * 5 + dpi) * 5) + state as usize]
}

fn embedded_scrollbar_arrow_glyph(
    dark: bool,
    dpi: u32,
    bottom: bool,
    state: AdvancedScrollbarState,
) -> &'static EmbeddedScrollbarGlyph {
    let mode = usize::from(dark);
    let dpi = embedded_scrollbar_dpi_index(dpi);
    &WIN11_SCROLLBAR_ARROW_GLYPHS
        [((mode * 5 + dpi) * 10) + usize::from(bottom) * 5 + state as usize]
}

fn nine_slice_coordinate(
    destination: i32,
    destination_length: i32,
    source_length: i32,
    leading: i32,
    trailing: i32,
) -> i32 {
    if destination_length <= 0 || source_length <= 0 {
        return 0;
    }
    let leading = leading.clamp(0, source_length);
    let trailing = trailing.clamp(0, source_length - leading);
    if destination_length < leading + trailing {
        return ((i64::from(destination) * i64::from(source_length)) / i64::from(destination_length))
            .clamp(0, i64::from(source_length - 1)) as i32;
    }
    if destination < leading {
        return destination.min(source_length - 1);
    }
    if destination >= destination_length - trailing {
        return (source_length - (destination_length - destination)).clamp(0, source_length - 1);
    }
    let source_middle = (source_length - leading - trailing).max(1);
    let destination_middle = (destination_length - leading - trailing).max(1);
    leading
        + ((i64::from(destination - leading) * i64::from(source_middle))
            / i64::from(destination_middle))
        .min(i64::from(source_middle - 1)) as i32
}

fn stretch_scrollbar_glyph(glyph: &EmbeddedScrollbarGlyph, width: i32, height: i32) -> Vec<u8> {
    if width <= 0 || height <= 0 || glyph.width <= 0 || glyph.height <= 0 {
        return Vec::new();
    }
    let mut pixels = vec![0_u8; width as usize * height as usize * 4];
    for destination_y in 0..height {
        let source_y = nine_slice_coordinate(
            destination_y,
            height,
            glyph.height,
            glyph.sizing_top,
            glyph.sizing_bottom,
        );
        for destination_x in 0..width {
            let source_x = nine_slice_coordinate(
                destination_x,
                width,
                glyph.width,
                glyph.sizing_left,
                glyph.sizing_right,
            );
            let source = (source_y as usize * glyph.width as usize + source_x as usize) * 4;
            let destination =
                (destination_y as usize * width as usize + destination_x as usize) * 4;
            pixels[destination..destination + 4].copy_from_slice(&glyph.bgra[source..source + 4]);
        }
    }
    pixels
}

fn colorref_is_dark(color: u32) -> bool {
    let red = color & 0xff;
    let green = (color >> 8) & 0xff;
    let blue = (color >> 16) & 0xff;
    red * 299 + green * 587 + blue * 114 < 128_000
}

impl AdvancedViewportGeometry {
    fn calculate(width: i32, height: i32, dpi: u32) -> Option<Self> {
        let width = width.max(0);
        let height = height.max(0);
        if width == 0 || height == 0 {
            return None;
        }
        let scale = |value: i32| ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32;
        let scrollbar_width = scale(VERTICAL_SCROLLBAR_WIDTH).clamp(1, width);
        let content_gap = scale(SCROLLBAR_CONTENT_GAP);
        Some(Self {
            content_width: (width - scrollbar_width - content_gap).max(0),
            scrollbar_left: width - scrollbar_width,
            scrollbar_width,
            corner_diameter: scrollbar_width.min(height),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AdvancedGrid {
    columns: usize,
    column_width: i32,
    gap: i32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CenteredButtonLayout {
    x: i32,
    width: i32,
}

fn centered_button_layout(
    column_x: i32,
    column_width: i32,
    measured_text_width: i32,
    horizontal_padding: i32,
    minimum_width: i32,
) -> CenteredButtonLayout {
    let column_width = column_width.max(0);
    let width = measured_text_width
        .max(0)
        .saturating_add(horizontal_padding.max(0))
        .max(minimum_width.max(0))
        .min(column_width);
    CenteredButtonLayout {
        x: column_x + column_width.saturating_sub(width) / 2,
        width,
    }
}

impl AdvancedGrid {
    fn calculate(width: i32, dpi: u32) -> Self {
        let scale = |value: i32| ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32;
        let width = width.max(0);
        let gap = scale(COLUMN_GAP);
        // `width` is the page viewport already returned in the window's current coordinate
        // space. Scaling the breakpoints again made a 1270 px window collapse to one column.
        let three_column_minimum = MIN_THREE_COLUMN_WIDTH;
        let two_column_minimum = MIN_TWO_COLUMN_WIDTH;
        let columns = if width >= three_column_minimum * 3 + gap * 2 {
            3
        } else if width >= two_column_minimum * 2 + gap {
            2
        } else {
            1
        };
        let column_width = ((width - gap * (columns as i32 - 1)) / columns as i32).max(0);
        Self {
            columns,
            column_width,
            gap,
        }
    }

    fn x(self, left: i32, column: usize) -> i32 {
        left + column as i32 * (self.column_width + self.gap)
    }
}

fn shortest_column(columns: &[i32]) -> usize {
    columns
        .iter()
        .enumerate()
        .min_by_key(|(_, height)| *height)
        .map(|(index, _)| index)
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ScrollModel {
    offset: i32,
    content_height: i32,
    viewport_height: i32,
}

impl ScrollModel {
    fn maximum(self) -> i32 {
        (self.content_height - self.viewport_height).max(0)
    }

    fn clamped_offset(self, requested: i32) -> i32 {
        requested.clamp(0, self.maximum())
    }
}

fn smooth_scroll_step(current: i32, target: i32) -> i32 {
    let remaining = target.saturating_sub(current);
    if remaining == 0 {
        return current;
    }
    let distance = remaining.unsigned_abs() as i32;
    if distance <= 2 {
        return target;
    }
    let step = ((distance * 3 + 9) / 10).max(2);
    current.saturating_add(step.min(distance) * remaining.signum())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdvancedPageContext {
    pub unattended_enabled: bool,
    pub builtin_administrator_available: bool,
    pub wifi_available: bool,
    pub preinstall_catalogue_available: bool,
    pub vmware_tools_available: bool,
    pub target_capabilities: AdvancedOptionCapabilities,
}

impl Default for AdvancedPageContext {
    fn default() -> Self {
        Self {
            unattended_enabled: true,
            builtin_administrator_available: true,
            wifi_available: false,
            preinstall_catalogue_available: false,
            vmware_tools_available: false,
            target_capabilities: AdvancedOptionCapabilities::unknown(),
        }
    }
}

#[derive(Clone, Copy)]
struct CheckEdit {
    check: HWND,
    edit: HWND,
    browse: Option<BrowseControl>,
}

#[derive(Clone, Copy)]
struct BrowseControl {
    button: HWND,
    id: u16,
    target: AdvancedBrowseTarget,
}

/// Identifies which advanced-option path the window controller should browse for.
///
/// The page deliberately returns an intent instead of opening a dialog itself so the owning
/// window remains responsible for modality, filters and filesystem access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdvancedBrowseTarget {
    DeployScript,
    FirstLoginScript,
    CustomDriversDirectory,
    RegistryFile,
    CustomFilesDirectory,
    Windows7Usb3Drivers,
    Windows7NvmeDrivers,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdvancedPageIntent {
    Browse(AdvancedBrowseTarget),
    SelectPreinstalledSoftware,
}

fn browse_intent_for_id(
    control_id: u16,
    controls: impl IntoIterator<Item = (u16, AdvancedBrowseTarget)>,
) -> Option<AdvancedPageIntent> {
    controls
        .into_iter()
        .find(|(id, _)| *id == control_id)
        .map(|(_, target)| AdvancedPageIntent::Browse(target))
}

#[derive(Clone)]
pub struct AdvancedPageHandles {
    pub system_header: HWND,
    pub preserve_personal_files: HWND,
    pub system_checks: [HWND; 10],
    pub preinstalled_software_button: HWND,
    preinstalled_software_button_id: u16,
    pub vmware_tools: HWND,
    pub scripts_header: HWND,
    deploy_script: CheckEdit,
    first_login_script: CheckEdit,
    pub content_header: HWND,
    custom_drivers: CheckEdit,
    pub storage_drivers: HWND,
    registry_file: CheckEdit,
    custom_files: CheckEdit,
    pub identity_header: HWND,
    username: CheckEdit,
    builtin_administrator: HWND,
    builtin_administrator_name_label: HWND,
    builtin_administrator_name: HWND,
    builtin_administrator_password_label: HWND,
    builtin_administrator_password: HWND,
    builtin_administrator_auto_logon: HWND,
    volume_label: CheckEdit,
    pub windows_7_header: HWND,
    windows_7_usb3: CheckEdit,
    windows_7_nvme: CheckEdit,
    pub windows_7_acpi: HWND,
    pub windows_7_storage: HWND,
    pub windows_7_uefi: HWND,
    pub xp_header: HWND,
    pub xp_usb3: HWND,
    pub xp_nvme: HWND,
}

pub struct AdvancedPage {
    handles: AdvancedPageHandles,
    context: AdvancedPageContext,
    viewport: HWND,
    scrollbar_overlay: HWND,
    width: Cell<i32>,
    height: Cell<i32>,
    dpi: Cell<u32>,
    scroll_offset: Cell<i32>,
    target_scroll_offset: Cell<i32>,
    wheel_distance_remainder: Cell<i32>,
    content_height: Cell<i32>,
    selected_preinstall_count: Cell<usize>,
    preinstall_selection_automatic: Cell<bool>,
}

impl AdvancedPage {
    pub unsafe fn create(
        parent: HWND,
        initial: &AdvancedOptionsData,
        context: AdvancedPageContext,
    ) -> windows::core::Result<Self> {
        let owner = parent;
        let viewport = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            PCWSTR::null(),
            WINDOW_STYLE((WS_CHILD | WS_CLIPCHILDREN | WS_VSCROLL).0),
            0,
            0,
            0,
            0,
            parent,
            HMENU(799_isize as *mut _),
            HINSTANCE::default(),
            None,
        )?;
        let scrollbar_overlay = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            PCWSTR::null(),
            WS_CHILD,
            0,
            0,
            0,
            0,
            owner,
            HMENU::default(),
            HINSTANCE::default(),
            None,
        )?;
        let _ = SetWindowSubclass(
            viewport,
            Some(advanced_viewport_proc),
            VIEWPORT_SUBCLASS_ID,
            owner.0 as usize,
        );
        let _ = SetWindowSubclass(
            scrollbar_overlay,
            Some(advanced_scrollbar_overlay_proc),
            SCROLLBAR_OVERLAY_SUBCLASS_ID,
            viewport.0 as usize,
        );
        let _ = SetPropW(
            viewport,
            ADVANCED_SCROLLBAR_OVERLAY_PROPERTY,
            HANDLE(scrollbar_overlay.0),
        );
        let parent = viewport;
        let mut id = ID_FIRST;
        let mut next_id = || {
            let result = id;
            id += 1;
            result
        };

        let system_header = label(parent, &crate::tr!("系统设置"), next_id())?;
        let preserve_personal_files = checkbox(
            parent,
            &crate::tr!("保留个人文件重装（桌面、文档、下载、图片、音乐、视频）"),
            next_id(),
        )?;
        let system_checks = [
            checkbox(parent, &crate::tr!("移除快捷方式箭头"), next_id())?,
            checkbox(parent, &crate::tr!("恢复经典右键菜单"), next_id())?,
            checkbox(parent, &crate::tr!("跳过 Windows 11 联网要求"), next_id())?,
            checkbox(parent, &crate::tr!("移除 Windows Update"), next_id())?,
            checkbox(
                parent,
                &crate::tr!("移除 Defender 与 Windows 安全中心"),
                next_id(),
            )?,
            checkbox(parent, &crate::tr!("禁用保留存储"), next_id())?,
            checkbox(parent, &crate::tr!("禁用用户账户控制 (UAC)"), next_id())?,
            checkbox(parent, &crate::tr!("禁用设备自动加密"), next_id())?,
            checkbox(parent, &crate::tr!("移除指定预装应用"), next_id())?,
            checkbox(parent, &crate::tr!("迁移当前 Wi-Fi 配置"), next_id())?,
        ];
        let preinstalled_software_button_id = next_id();
        let preinstalled_software_button = action_button(
            parent,
            &crate::tr!("选择预装应用..."),
            preinstalled_software_button_id,
        )?;
        let vmware_tools = checkbox(parent, &crate::tr!("安装 VMware Tools"), next_id())?;

        let scripts_header = label(parent, &crate::tr!("部署脚本"), next_id())?;
        let deploy_script = check_edit(
            parent,
            &crate::tr!("部署过程中运行脚本"),
            &initial.deploy_script_path,
            next_id(),
            next_id(),
            Some((next_id(), AdvancedBrowseTarget::DeployScript)),
        )?;
        let first_login_script = check_edit(
            parent,
            &crate::tr!("首次登录时运行脚本"),
            &initial.first_login_script_path,
            next_id(),
            next_id(),
            Some((next_id(), AdvancedBrowseTarget::FirstLoginScript)),
        )?;

        let content_header = label(parent, &crate::tr!("驱动与自定义内容"), next_id())?;
        let custom_drivers = check_edit(
            parent,
            &crate::tr!("导入自定义驱动目录"),
            &initial.custom_drivers_path,
            next_id(),
            next_id(),
            Some((next_id(), AdvancedBrowseTarget::CustomDriversDirectory)),
        )?;
        let storage_drivers = checkbox(parent, &crate::tr!("导入存储控制器驱动"), next_id())?;
        let registry_file = check_edit(
            parent,
            &crate::tr!("导入注册表文件"),
            &initial.registry_file_path,
            next_id(),
            next_id(),
            Some((next_id(), AdvancedBrowseTarget::RegistryFile)),
        )?;
        let custom_files = check_edit(
            parent,
            &crate::tr!("复制自定义文件目录"),
            &initial.custom_files_path,
            next_id(),
            next_id(),
            Some((next_id(), AdvancedBrowseTarget::CustomFilesDirectory)),
        )?;

        let identity_header = label(parent, &crate::tr!("用户与系统盘"), next_id())?;
        let username = radio_edit(
            parent,
            &crate::tr!("自定义用户名"),
            &initial.username,
            next_id(),
            next_id(),
            None,
        )?;
        let builtin_administrator = radio_button(
            parent,
            &crate::tr!("启用内置 Administrator 账户"),
            next_id(),
            false,
        )?;
        let builtin_administrator_name_label =
            owner_draw_label(parent, &crate::tr!("Administrator 账户名"), next_id())?;
        let builtin_administrator_name = edit(
            parent,
            &initial.builtin_administrator.account_name,
            next_id(),
        )?;
        let builtin_administrator_password_label =
            owner_draw_label(parent, &crate::tr!("Administrator 密码"), next_id())?;
        let builtin_administrator_password = password_edit(
            parent,
            initial.builtin_administrator.password.expose_secret(),
            next_id(),
        )?;
        let builtin_administrator_auto_logon =
            checkbox(parent, &crate::tr!("自动登录内置 Administrator"), next_id())?;
        let volume_label = check_edit(
            parent,
            &crate::tr!("自定义系统盘卷标"),
            &initial.volume_label,
            next_id(),
            next_id(),
            None,
        )?;

        let windows_7_header = label(parent, &crate::tr!("Windows 7 兼容选项"), next_id())?;
        let windows_7_usb3 = check_edit(
            parent,
            &crate::tr!("注入 USB 3.x 驱动"),
            &initial.win7_usb3_driver_path,
            next_id(),
            next_id(),
            Some((next_id(), AdvancedBrowseTarget::Windows7Usb3Drivers)),
        )?;
        let windows_7_nvme = check_edit(
            parent,
            &crate::tr!("注入 NVMe 驱动"),
            &initial.win7_nvme_driver_path,
            next_id(),
            next_id(),
            Some((next_id(), AdvancedBrowseTarget::Windows7NvmeDrivers)),
        )?;
        let windows_7_acpi = checkbox(
            parent,
            &crate::tr!("尝试修复 0xA5（禁用处理器电源驱动）"),
            next_id(),
        )?;
        let windows_7_storage =
            checkbox(parent, &crate::tr!("修复 0x7B 存储控制器蓝屏"), next_id())?;
        let windows_7_uefi = checkbox(parent, &crate::tr!("启用 Windows 7 UEFI 补丁"), next_id())?;

        let xp_header = label(parent, &crate::tr!("Windows XP / 2003 选项"), next_id())?;
        let xp_usb3 = checkbox(parent, &crate::tr!("注入 USB 3.x 驱动"), next_id())?;
        let xp_nvme = checkbox(parent, &crate::tr!("注入 NVMe 驱动"), next_id())?;

        let page = Self {
            handles: AdvancedPageHandles {
                system_header,
                preserve_personal_files,
                system_checks,
                preinstalled_software_button,
                preinstalled_software_button_id,
                vmware_tools,
                scripts_header,
                deploy_script,
                first_login_script,
                content_header,
                custom_drivers,
                storage_drivers,
                registry_file,
                custom_files,
                identity_header,
                username,
                builtin_administrator,
                builtin_administrator_name_label,
                builtin_administrator_name,
                builtin_administrator_password_label,
                builtin_administrator_password,
                builtin_administrator_auto_logon,
                volume_label,
                windows_7_header,
                windows_7_usb3,
                windows_7_nvme,
                windows_7_acpi,
                windows_7_storage,
                windows_7_uefi,
                xp_header,
                xp_usb3,
                xp_nvme,
            },
            context,
            viewport,
            scrollbar_overlay,
            width: Cell::new(0),
            height: Cell::new(0),
            dpi: Cell::new(96),
            scroll_offset: Cell::new(0),
            target_scroll_offset: Cell::new(0),
            wheel_distance_remainder: Cell::new(0),
            content_height: Cell::new(0),
            selected_preinstall_count: Cell::new(initial.preinstalled_software.len()),
            preinstall_selection_automatic: Cell::new(false),
        };
        page.apply(initial);
        page.apply_context();
        page.show(false);
        Ok(page)
    }

    pub fn handles(&self) -> &AdvancedPageHandles {
        &self.handles
    }

    /// Replaces all captions in place while preserving every option value.
    pub unsafe fn relocalize(&self) {
        let h = &self.handles;
        set_text(h.system_header, &crate::tr!("系统设置"));
        set_text(
            h.preserve_personal_files,
            &crate::tr!("保留个人文件重装（桌面、文档、下载、图片、音乐、视频）"),
        );
        for (control, label) in h.system_checks.into_iter().zip([
            crate::tr!("移除快捷方式箭头"),
            crate::tr!("恢复经典右键菜单"),
            crate::tr!("跳过 Windows 11 联网要求"),
            crate::tr!("移除 Windows Update"),
            crate::tr!("移除 Defender 与 Windows 安全中心"),
            crate::tr!("禁用保留存储"),
            crate::tr!("禁用用户账户控制 (UAC)"),
            crate::tr!("禁用设备自动加密"),
            crate::tr!("移除指定预装应用"),
            crate::tr!("迁移当前 Wi-Fi 配置"),
        ]) {
            set_text(control, &label);
        }
        self.set_preinstalled_software_selection(
            self.selected_preinstall_count.get(),
            self.preinstall_selection_automatic.get(),
        );
        set_text(h.vmware_tools, &crate::tr!("安装 VMware Tools"));

        set_text(h.scripts_header, &crate::tr!("部署脚本"));
        relocalize_check_edit(h.deploy_script, &crate::tr!("部署过程中运行脚本"));
        relocalize_check_edit(h.first_login_script, &crate::tr!("首次登录时运行脚本"));
        set_text(h.content_header, &crate::tr!("驱动与自定义内容"));
        relocalize_check_edit(h.custom_drivers, &crate::tr!("导入自定义驱动目录"));
        set_text(h.storage_drivers, &crate::tr!("导入存储控制器驱动"));
        relocalize_check_edit(h.registry_file, &crate::tr!("导入注册表文件"));
        relocalize_check_edit(h.custom_files, &crate::tr!("复制自定义文件目录"));
        set_text(h.identity_header, &crate::tr!("用户与系统盘"));
        relocalize_check_edit(h.username, &crate::tr!("自定义用户名"));
        set_text(
            h.builtin_administrator,
            &crate::tr!("启用内置 Administrator 账户"),
        );
        set_text(
            h.builtin_administrator_name_label,
            &crate::tr!("Administrator 账户名"),
        );
        set_text(
            h.builtin_administrator_password_label,
            &crate::tr!("Administrator 密码"),
        );
        set_text(
            h.builtin_administrator_auto_logon,
            &crate::tr!("自动登录内置 Administrator"),
        );
        relocalize_check_edit(h.volume_label, &crate::tr!("自定义系统盘卷标"));

        set_text(h.windows_7_header, &crate::tr!("Windows 7 兼容选项"));
        relocalize_check_edit(h.windows_7_usb3, &crate::tr!("注入 USB 3.x 驱动"));
        relocalize_check_edit(h.windows_7_nvme, &crate::tr!("注入 NVMe 驱动"));
        set_text(
            h.windows_7_acpi,
            &crate::tr!("尝试修复 0xA5（禁用处理器电源驱动）"),
        );
        set_text(h.windows_7_storage, &crate::tr!("修复 0x7B 存储控制器蓝屏"));
        set_text(h.windows_7_uefi, &crate::tr!("启用 Windows 7 UEFI 补丁"));
        set_text(h.xp_header, &crate::tr!("Windows XP / 2003 选项"));
        set_text(h.xp_usb3, &crate::tr!("注入 USB 3.x 驱动"));
        set_text(h.xp_nvme, &crate::tr!("注入 NVMe 驱动"));
        self.apply_context();
    }

    /// Converts a forwarded `WM_COMMAND` control id into a side-effect-free browse intent.
    pub fn intent_for_command(&self, control_id: u16) -> Option<AdvancedPageIntent> {
        if control_id == self.handles.preinstalled_software_button_id {
            return Some(AdvancedPageIntent::SelectPreinstalledSoftware);
        }
        browse_intent_for_id(
            control_id,
            self.check_edits()
                .into_iter()
                .filter_map(|pair| pair.browse.map(|browse| (browse.id, browse.target))),
        )
    }

    /// Applies a path selected by the owning window and enables the corresponding option.
    /// Passing an empty path clears and disables it, preserving the required-value invariant.
    pub unsafe fn set_path(&self, target: AdvancedBrowseTarget, path: &str) {
        let Some(pair) = self.pair_for_browse_target(target) else {
            return;
        };
        let path = path.trim();
        set_text(pair.edit, path);
        set_checked(pair.check, !path.is_empty());
        self.update_dependencies();
    }

    pub unsafe fn set_wifi_caption(&self, ssid: Option<&str>) {
        let caption = ssid.map_or_else(
            || crate::tr!("迁移当前 Wi-Fi 配置"),
            |ssid| crate::tr!("迁移当前 Wi-Fi 配置（SSID：{}）", ssid),
        );
        set_text(self.handles.system_checks[9], &caption);
    }

    pub unsafe fn set_preinstalled_software_count(&self, selected: usize) {
        self.set_preinstalled_software_selection(selected, false);
    }

    pub unsafe fn set_preinstalled_software_selection(&self, selected: usize, automatic: bool) {
        self.selected_preinstall_count.set(selected);
        self.preinstall_selection_automatic
            .set(automatic && selected > 0);
        let caption = if selected == 0 {
            crate::tr!("选择预装应用...")
        } else if automatic {
            crate::tr!("选择预装应用（已自动选 {} 项）", selected)
        } else {
            crate::tr!("选择预装应用（已选 {} 项）", selected)
        };
        set_text(self.handles.preinstalled_software_button, &caption);
        if self.width.get() > 0 {
            let content_height =
                self.layout_content(self.width.get(), self.dpi.get(), -self.scroll_offset.get());
            self.content_height.set(content_height);
            self.update_scrollbar();
            let _ = InvalidateRect(self.viewport, None, true);
        }
    }

    pub unsafe fn apply(&self, data: &AdvancedOptionsData) {
        let h = &self.handles;
        set_checked(h.preserve_personal_files, data.preserve_personal_files);
        for (control, checked) in h.system_checks.into_iter().zip([
            data.remove_shortcut_arrow,
            data.restore_classic_context_menu,
            data.bypass_nro,
            data.disable_windows_update,
            data.disable_windows_defender,
            data.disable_reserved_storage,
            data.disable_uac,
            data.disable_device_encryption,
            data.remove_uwp_apps,
            data.migrate_wifi,
        ]) {
            set_checked(control, checked);
        }
        set_checked(h.vmware_tools, data.install_vmware_tools);
        self.set_preinstalled_software_count(data.preinstalled_software.len());
        apply_check_edit(
            h.deploy_script,
            data.run_script_during_deploy,
            &data.deploy_script_path,
        );
        apply_check_edit(
            h.first_login_script,
            data.run_script_first_login,
            &data.first_login_script_path,
        );
        apply_check_edit(
            h.custom_drivers,
            data.import_custom_drivers,
            &data.custom_drivers_path,
        );
        set_checked(h.storage_drivers, data.import_storage_controller_drivers);
        apply_check_edit(
            h.registry_file,
            data.import_registry_file,
            &data.registry_file_path,
        );
        apply_check_edit(
            h.custom_files,
            data.import_custom_files,
            &data.custom_files_path,
        );
        let builtin_selected = data.builtin_administrator.enabled;
        apply_check_edit(h.username, !builtin_selected, &data.username);
        set_checked(h.builtin_administrator, builtin_selected);
        set_text(
            h.builtin_administrator_name,
            &data.builtin_administrator.account_name,
        );
        set_text(
            h.builtin_administrator_password,
            data.builtin_administrator.password.expose_secret(),
        );
        set_checked(
            h.builtin_administrator_auto_logon,
            data.builtin_administrator.enabled || data.builtin_administrator.auto_logon,
        );
        apply_check_edit(h.volume_label, data.custom_volume_label, &data.volume_label);
        apply_check_edit(
            h.windows_7_usb3,
            data.win7_inject_usb3_driver,
            &data.win7_usb3_driver_path,
        );
        apply_check_edit(
            h.windows_7_nvme,
            data.win7_inject_nvme_driver,
            &data.win7_nvme_driver_path,
        );
        set_checked(h.windows_7_acpi, data.win7_fix_acpi_bsod);
        set_checked(h.windows_7_storage, data.win7_fix_storage_bsod);
        set_checked(h.windows_7_uefi, data.win7_uefi_patch);
        set_checked(h.xp_usb3, data.xp_inject_usb3_driver);
        set_checked(h.xp_nvme, data.xp_inject_nvme_driver);
        self.update_dependencies();
    }

    pub unsafe fn read(&self) -> AdvancedOptionsData {
        let mut data = AdvancedOptionsData::default();
        self.read_into(&mut data);
        data
    }

    /// Updates the current install-session fields while preserving runtime-only Wi-Fi material
    /// and the XP one-shot marker already held by the controller. The personal-file option is
    /// intentionally session-only even though the remaining compatible fields may be persisted.
    pub unsafe fn read_into(&self, data: &mut AdvancedOptionsData) {
        let h = &self.handles;
        data.preserve_personal_files = is_checked(h.preserve_personal_files);
        data.update_supported_system_options(
            self.context.target_capabilities,
            h.system_checks.map(|control| is_checked(control)),
        );
        data.install_vmware_tools = self.context.vmware_tools_available
            && self.context.unattended_enabled
            && is_checked(h.vmware_tools);
        (data.run_script_during_deploy, data.deploy_script_path) =
            read_required_pair(h.deploy_script);
        (data.run_script_first_login, data.first_login_script_path) =
            read_required_pair(h.first_login_script);
        (data.import_custom_drivers, data.custom_drivers_path) =
            read_required_pair(h.custom_drivers);
        if self.context.target_capabilities.storage_controller_drivers {
            data.import_storage_controller_drivers = is_checked(h.storage_drivers);
        }
        (data.import_registry_file, data.registry_file_path) = read_required_pair(h.registry_file);
        (data.import_custom_files, data.custom_files_path) = read_required_pair(h.custom_files);
        data.builtin_administrator.enabled = is_checked(h.builtin_administrator);
        data.custom_username = !data.builtin_administrator.enabled;
        data.username = read_text(h.username.edit).trim().to_string();
        if data.custom_username && data.username.is_empty() {
            data.username = default_install_username();
            set_text(h.username.edit, &data.username);
        }
        data.builtin_administrator.account_name =
            read_text(h.builtin_administrator_name).trim().to_string();
        if data.builtin_administrator.account_name.is_empty() {
            data.builtin_administrator.account_name = "Administrator".to_string();
            set_text(
                h.builtin_administrator_name,
                &data.builtin_administrator.account_name,
            );
        }
        data.builtin_administrator.password = read_text(h.builtin_administrator_password).into();
        data.builtin_administrator.auto_logon =
            data.builtin_administrator.enabled || is_checked(h.builtin_administrator_auto_logon);
        (data.custom_volume_label, data.volume_label) = read_required_pair(h.volume_label);
        // USB3/NVMe selection is controller-owned. Preserve only the explicit historical
        // processor-power workaround; the broad storage mutation and UefiSeven remain retired.
        data.win7_fix_acpi_bsod =
            self.context.target_capabilities.windows_7 && is_checked(h.windows_7_acpi);
        data.win7_fix_storage_bsod = false;
        data.win7_uefi_patch = false;
        if self.context.target_capabilities.xp {
            data.xp_inject_usb3_driver = is_checked(h.xp_usb3);
            data.xp_inject_nvme_driver = is_checked(h.xp_nvme);
        }
    }

    pub unsafe fn set_context(&mut self, context: AdvancedPageContext) {
        self.context = context;
        self.apply_context();
        // Image selection can change while this page already exists. Reflow immediately so
        // newly hidden target-specific rows do not leave holes until the next WM_SIZE.
        let width = self.width.get();
        let height = self.height.get();
        if width > 0 && height > 0 {
            let dpi = self.dpi.get().max(1);
            let content_height = self.layout_content(width, dpi, 0);
            self.content_height.set(content_height);
            let model = ScrollModel {
                offset: self.scroll_offset.get(),
                content_height,
                viewport_height: height,
            };
            self.scroll_offset
                .set(model.clamped_offset(self.scroll_offset.get()));
            self.target_scroll_offset.set(self.scroll_offset.get());
            self.layout_content(width, dpi, -self.scroll_offset.get());
            self.update_scrollbar();
            let _ = InvalidateRect(self.viewport, None, true);
        }
    }

    pub unsafe fn update_dependencies(&self) {
        let h = &self.handles;
        for pair in [
            h.deploy_script,
            h.first_login_script,
            h.custom_drivers,
            h.registry_file,
            h.custom_files,
            h.volume_label,
            h.windows_7_usb3,
            h.windows_7_nvme,
        ] {
            let enabled = is_checked(pair.check);
            let _ = EnableWindow(pair.edit, enabled);
            if let Some(browse) = pair.browse {
                let _ = EnableWindow(browse.button, enabled);
            }
        }

        let builtin_enabled = self.context.unattended_enabled
            && self.context.builtin_administrator_available
            && is_checked(h.builtin_administrator);
        if builtin_enabled {
            set_checked(h.username.check, false);
            // RID-500 already exists, so Windows 10/11 only skips OOBE account creation through
            // a one-time AutoLogon. Keep the compatibility checkbox visible but deterministic.
            set_checked(h.builtin_administrator_auto_logon, true);
        } else {
            set_checked(h.username.check, true);
        }
        let _ = EnableWindow(h.username.check, true);
        let _ = EnableWindow(
            h.username.edit,
            !builtin_enabled && is_checked(h.username.check),
        );
        // A disabled stock STATIC uses USER32's etched two-pass caption renderer even when the
        // parent supplies an opaque brush. Keep the two captions enabled and let WM_CTLCOLORSTATIC
        // select the disabled text colour; only the interactive fields are actually disabled.
        for label in [
            h.builtin_administrator_name_label,
            h.builtin_administrator_password_label,
        ] {
            let _ = EnableWindow(label, true);
            let _ = InvalidateRect(label, None, true);
        }
        for control in [
            h.builtin_administrator_name,
            h.builtin_administrator_password,
        ] {
            let _ = EnableWindow(control, builtin_enabled);
        }
        // A built-in RID-500 account must perform exactly one first logon to prevent Windows
        // 10/11 from reopening OOBE account creation. Keep this visible but non-optional.
        let _ = EnableWindow(h.builtin_administrator_auto_logon, false);
    }

    pub unsafe fn handle_dependency_toggle(&self, control: HWND) {
        let h = &self.handles;
        if control == h.builtin_administrator && is_checked(control) {
            set_checked(h.username.check, false);
        } else if control == h.username.check && is_checked(control) {
            set_checked(h.builtin_administrator, false);
        }
        self.update_dependencies();
    }

    pub unsafe fn show(&self, visible: bool) {
        let command = if visible { SW_SHOW } else { SW_HIDE };
        for control in self.all_controls() {
            let _ = ShowWindow(control, command);
        }
        if visible {
            self.apply_context();
        }
        let _ = ShowWindow(self.viewport, command);
        let _ = ShowWindow(self.scrollbar_overlay, command);
    }

    pub unsafe fn apply_theme(&self, palette: Palette) {
        // The viewport owns a non-client WS_VSCROLL scrollbar.  Unlike its child controls it is a
        // plain STATIC, so it never passed through the shared theme path and Windows kept painting
        // a bright Explorer scrollbar on the dark page.  Theme the owning HWND itself; this keeps
        // sizing, hit testing and keyboard scrolling native while selecting the matching
        // DarkMode_Explorer/Explorer scrollbar family.
        apply_control_theme(self.viewport, palette, NativeControlKind::General);
        if let Ok(owner) = GetParent(self.viewport) {
            paint_advanced_scrollbar(self.viewport, owner);
        }
        for control in self.checkbox_controls() {
            // Reuse the shared checkbox/radio subclass instead of relying on USER32 to recolour
            // captions after a live light/dark switch.  The host theme commonly updates the
            // glyph while leaving the BUTTON caption cached in the previous (black) colour.
            apply_control_theme(control, palette, NativeControlKind::General);
        }
        for pair in self.check_edits() {
            apply_control_theme(pair.edit, palette, NativeControlKind::Field);
        }
        for control in [
            self.handles.builtin_administrator_name,
            self.handles.builtin_administrator_password,
        ] {
            apply_control_theme(control, palette, NativeControlKind::Field);
        }
    }

    /// Returns whether `control` toggles one of the conditional Edit/Browse rows.
    ///
    /// The checkbox is parented to the page viewport, so its `BN_CLICKED` notification is
    /// forwarded to the top-level window.  Keeping ownership testing here avoids coupling the
    /// controller to the page's generated control IDs.
    pub fn owns_dependency_toggle(&self, control: HWND) -> bool {
        control == self.handles.builtin_administrator
            || self
                .check_edits()
                .into_iter()
                .any(|pair| pair.check == control)
    }

    pub unsafe fn apply_font(&self, font: HFONT, heading_font: HFONT) {
        for control in self.all_controls() {
            let _ = SendMessageW(control, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        }
        for heading in self.headings() {
            let _ = SendMessageW(
                heading,
                WM_SETFONT,
                WPARAM(heading_font.0 as usize),
                LPARAM(1),
            );
        }
    }

    /// Compact responsive layout. Columns keep enough logical width for the longest English
    /// captions instead of forcing three narrow columns and clipping standard Win32 checkboxes.
    pub unsafe fn layout(&self, left: i32, top: i32, width: i32, height: i32, dpi: u32) {
        let width = width.max(0);
        let height = height.max(0);
        let _ = MoveWindow(self.viewport, left, top, width, height, false);
        update_viewport_region(self.viewport, width, height, dpi);
        if let Some(geometry) = AdvancedViewportGeometry::calculate(width, height, dpi) {
            let _ = MoveWindow(
                self.scrollbar_overlay,
                left + geometry.scrollbar_left,
                top,
                (width - geometry.scrollbar_left).max(0),
                height,
                false,
            );
        }
        self.width.set(width);
        self.height.set(height);
        self.dpi.set(dpi.max(1));

        let content_height = self.layout_content(width, dpi, 0);
        self.content_height.set(content_height);
        let model = ScrollModel {
            offset: self.scroll_offset.get(),
            content_height,
            viewport_height: height,
        };
        self.scroll_offset
            .set(model.clamped_offset(self.scroll_offset.get()));
        self.target_scroll_offset.set(self.scroll_offset.get());
        self.wheel_distance_remainder.set(0);
        self.layout_content(width, dpi, -self.scroll_offset.get());
        self.update_scrollbar();
    }

    unsafe fn layout_content(&self, width: i32, dpi: u32, origin_y: i32) -> i32 {
        let s = |value: i32| ((value as i64 * dpi.max(1) as i64 + 48) / 96) as i32;
        // WS_VSCROLL owns pixels inside the viewport width. Keep a separate field gap before it
        // so the final Browse button never uses the scrollbar trough as its right-hand border.
        let content_width = AdvancedViewportGeometry::calculate(width, self.height.get(), dpi)
            .map_or(width.max(0), |geometry| geometry.content_width);
        let grid = AdvancedGrid::calculate(content_width, dpi);
        let mut bottoms = vec![origin_y; grid.columns];
        let section_gap = s(5);
        let h = &self.handles;

        let column = shortest_column(&bottoms);
        let x = grid.x(0, column);
        layout_heading(
            h.system_header,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        layout_check(
            h.preserve_personal_files,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        for (index, check) in h.system_checks.into_iter().enumerate() {
            if self
                .context
                .target_capabilities
                .supports_system_option(index)
                && (index != 9 || self.context.wifi_available)
            {
                layout_check(check, x, &mut bottoms[column], grid.column_width, dpi);
            }
        }
        if self.context.preinstall_catalogue_available && self.context.unattended_enabled {
            self.layout_preinstalled_software_button(
                h.preinstalled_software_button,
                x,
                &mut bottoms[column],
                grid.column_width,
                dpi,
            );
        }
        if self.context.vmware_tools_available && self.context.unattended_enabled {
            layout_check(
                h.vmware_tools,
                x,
                &mut bottoms[column],
                grid.column_width,
                dpi,
            );
        }
        bottoms[column] += section_gap;

        let column = shortest_column(&bottoms);
        let x = grid.x(0, column);
        layout_heading(
            h.scripts_header,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        layout_pair(
            h.deploy_script,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        layout_pair(
            h.first_login_script,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        bottoms[column] += section_gap;

        let column = shortest_column(&bottoms);
        let x = grid.x(0, column);
        layout_heading(
            h.content_header,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        layout_pair(
            h.custom_drivers,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        if self.context.target_capabilities.storage_controller_drivers {
            layout_check(
                h.storage_drivers,
                x,
                &mut bottoms[column],
                grid.column_width,
                dpi,
            );
        }
        layout_pair(
            h.registry_file,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        layout_pair(
            h.custom_files,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        bottoms[column] += section_gap;

        let column = shortest_column(&bottoms);
        let x = grid.x(0, column);
        layout_heading(
            h.identity_header,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        layout_pair(h.username, x, &mut bottoms[column], grid.column_width, dpi);
        layout_check(
            h.builtin_administrator,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        layout_labeled_edit(
            h.builtin_administrator_name_label,
            h.builtin_administrator_name,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        layout_labeled_edit(
            h.builtin_administrator_password_label,
            h.builtin_administrator_password,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        layout_check(
            h.builtin_administrator_auto_logon,
            x + s(20),
            &mut bottoms[column],
            (grid.column_width - s(20)).max(0),
            dpi,
        );
        layout_pair(
            h.volume_label,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        bottoms[column] += section_gap;

        // Windows 7 USB3/NVMe support is selected automatically from the locked built-in payload,
        // image architecture and target disk bus. Preserve the old processor-power workaround as
        // an explicit compatibility attempt; the broad 0x7B mutation and UefiSeven stay hidden.
        if self.context.target_capabilities.windows_7 {
            let column = shortest_column(&bottoms);
            let x = grid.x(0, column);
            layout_heading(
                h.windows_7_header,
                x,
                &mut bottoms[column],
                grid.column_width,
                dpi,
            );
            layout_check(
                h.windows_7_acpi,
                x,
                &mut bottoms[column],
                grid.column_width,
                dpi,
            );
            bottoms[column] += section_gap;
        }

        if self.context.target_capabilities.xp {
            let column = shortest_column(&bottoms);
            let x = grid.x(0, column);
            layout_heading(h.xp_header, x, &mut bottoms[column], grid.column_width, dpi);
            layout_check(h.xp_usb3, x, &mut bottoms[column], grid.column_width, dpi);
            layout_check(h.xp_nvme, x, &mut bottoms[column], grid.column_width, dpi);
        }
        bottoms.into_iter().max().unwrap_or(origin_y) - origin_y + s(8)
    }

    unsafe fn layout_preinstalled_software_button(
        &self,
        control: HWND,
        column_x: i32,
        y: &mut i32,
        column_width: i32,
        dpi: u32,
    ) {
        let s = |value: i32| ((value as i64 * dpi.max(1) as i64 + 48) / 96) as i32;
        let font = SendMessageW(control, WM_GETFONT, WPARAM(0), LPARAM(0));
        let font = HFONT(font.0 as *mut c_void);
        let measured_width = measure_text(self.viewport, font, &read_text(control), None).width;
        let layout = centered_button_layout(column_x, column_width, measured_width, s(28), s(96));
        let _ = MoveWindow(control, layout.x, *y, layout.width, s(24), false);
        *y += s(24);
    }

    pub fn viewport(&self) -> HWND {
        self.viewport
    }

    pub unsafe fn draw_item(&self, item: &DRAWITEMSTRUCT, palette: Palette) -> bool {
        if item.hwndItem != self.handles.builtin_administrator_name_label
            && item.hwndItem != self.handles.builtin_administrator_password_label
        {
            return false;
        }

        // Stock disabled STATIC controls use an etched two-pass caption that leaves a visible
        // duplicate after ScrollWindowEx. These two dependent captions remain ordinary child
        // HWNDs, but their complete opaque frame and single text pass are deterministic here.
        let brush = CreateSolidBrush(palette.window);
        let _ = FillRect(item.hDC, &item.rcItem, brush);
        let _ = DeleteObject(brush);
        let _ = SetBkMode(item.hDC, TRANSPARENT);
        let _ = SetTextColor(
            item.hDC,
            if IsWindowEnabled(self.handles.builtin_administrator_name).as_bool() {
                palette.text
            } else {
                palette.text_disabled
            },
        );

        let text_length = GetWindowTextLengthW(item.hwndItem).max(0) as usize;
        if text_length > 0 {
            let mut text = vec![0u16; text_length + 1];
            let copied = GetWindowTextW(item.hwndItem, &mut text).max(0) as usize;
            text.truncate(copied.min(text.len()));
            let font = SendMessageW(item.hwndItem, WM_GETFONT, WPARAM(0), LPARAM(0));
            let old_font = (font.0 != 0).then(|| {
                SelectObject(
                    item.hDC,
                    windows::Win32::Graphics::Gdi::HGDIOBJ(font.0 as *mut _),
                )
            });
            let mut rect = item.rcItem;
            let _ = DrawTextW(
                item.hDC,
                &mut text,
                &mut rect,
                DT_LEFT | DT_SINGLELINE | DT_NOPREFIX,
            );
            if let Some(old_font) = old_font {
                let _ = SelectObject(item.hDC, old_font);
            }
        }
        true
    }

    pub unsafe fn scroll_wheel(&self, wheel_delta: i16) -> bool {
        if self.height.get() <= 0 || self.content_height.get() <= self.height.get() {
            return false;
        }
        let line = ((32_i64 * i64::from(self.dpi.get()) + 48) / 96) as i32;
        let distance_numerator = self
            .wheel_distance_remainder
            .get()
            .saturating_add(i32::from(wheel_delta).saturating_mul(line.saturating_mul(3)));
        let distance = distance_numerator / WHEEL_DELTA;
        self.wheel_distance_remainder
            .set(distance_numerator % WHEEL_DELTA);
        if distance == 0 {
            return true;
        }

        let model = ScrollModel {
            offset: self.scroll_offset.get(),
            content_height: self.content_height.get(),
            viewport_height: self.height.get(),
        };
        let target = model.clamped_offset(self.target_scroll_offset.get().saturating_sub(distance));
        let changed = target != self.target_scroll_offset.get();
        self.target_scroll_offset.set(target);
        changed || target != self.scroll_offset.get()
    }

    /// Advances one coalesced wheel-animation frame. Returns `true` while another frame is needed.
    pub unsafe fn advance_smooth_scroll(&self) -> bool {
        let current = self.scroll_offset.get();
        let target = ScrollModel {
            offset: current,
            content_height: self.content_height.get(),
            viewport_height: self.height.get(),
        }
        .clamped_offset(self.target_scroll_offset.get());
        self.target_scroll_offset.set(target);
        let next = smooth_scroll_step(current, target);
        if next != current {
            self.set_scroll_offset(next);
        }
        next != target
    }

    pub unsafe fn handle_vscroll(&self, request: usize) -> bool {
        let code = (request & 0xffff) as u32;
        let model = ScrollModel {
            offset: self.scroll_offset.get(),
            content_height: self.content_height.get(),
            viewport_height: self.height.get(),
        };
        let line = ((32_i64 * i64::from(self.dpi.get()) + 48) / 96) as i32;
        let requested = match code {
            value if value == SB_TOP.0 as u32 => 0,
            value if value == SB_BOTTOM.0 as u32 => model.maximum(),
            value if value == SB_LINEUP.0 as u32 => model.offset - line,
            value if value == SB_LINEDOWN.0 as u32 => model.offset + line,
            value if value == SB_PAGEUP.0 as u32 => model.offset - model.viewport_height,
            value if value == SB_PAGEDOWN.0 as u32 => model.offset + model.viewport_height,
            value if value == SB_THUMBPOSITION.0 as u32 || value == SB_THUMBTRACK.0 as u32 => {
                let proxy =
                    GetPropW(self.viewport, ADVANCED_SCROLLBAR_PROXY_POSITION_PROPERTY).0 as usize;
                if proxy != 0 {
                    let _ = RemovePropW(self.viewport, ADVANCED_SCROLLBAR_PROXY_POSITION_PROPERTY);
                    proxy.saturating_sub(1).min(i32::MAX as usize) as i32
                } else {
                    let mut info = SCROLLINFO {
                        cbSize: size_of::<SCROLLINFO>() as u32,
                        fMask: SIF_TRACKPOS,
                        ..Default::default()
                    };
                    let _ = GetScrollInfo(self.viewport, SB_VERT, &mut info);
                    info.nTrackPos
                }
            }
            value if value == SB_ENDSCROLL.0 as u32 => return false,
            _ => return false,
        };
        self.target_scroll_offset
            .set(model.clamped_offset(requested));
        self.wheel_distance_remainder.set(0);
        self.set_scroll_offset(requested)
    }

    unsafe fn set_scroll_offset(&self, requested: i32) -> bool {
        let model = ScrollModel {
            offset: self.scroll_offset.get(),
            content_height: self.content_height.get(),
            viewport_height: self.height.get(),
        };
        let offset = model.clamped_offset(requested);
        if offset == model.offset {
            return false;
        }
        self.scroll_offset.set(offset);
        let delta_y = model.offset.saturating_sub(offset);
        // All advanced-page fields share the viewport as their direct parent. Moving the complete
        // child tree as one scroll transaction avoids dozens of visible MoveWindow/layout passes
        // for every wheel-animation frame and every native thumb-track notification.
        let scroll_result = ScrollWindowEx(
            self.viewport,
            0,
            delta_y,
            None,
            None,
            HRGN::default(),
            None,
            SW_SCROLLCHILDREN | SW_INVALIDATE,
        );
        if scroll_result == RGN_ERROR.0 {
            // Fail visibly and deterministically if a reduced WinPE USER32 rejects
            // ScrollWindowEx; the slower full layout remains a safe compatibility fallback.
            self.layout_content(self.width.get(), self.dpi.get(), -offset);
            let _ = InvalidateRect(self.viewport, None, true);
        } else {
            // These disabled-capable STATIC labels can repaint independently after USER32 has
            // copied their previous pixels. Repaint them with the advanced page's opaque
            // WM_CTLCOLORSTATIC background so a second text pass cannot leave a shadow.
            for label in [
                self.handles.builtin_administrator_name_label,
                self.handles.builtin_administrator_password_label,
            ] {
                let _ = InvalidateRect(label, None, false);
            }
        }
        self.update_scrollbar();
        true
    }

    unsafe fn update_scrollbar(&self) {
        let model = ScrollModel {
            offset: self.scroll_offset.get(),
            content_height: self.content_height.get(),
            viewport_height: self.height.get(),
        };
        let info = SCROLLINFO {
            cbSize: size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
            nMin: 0,
            nMax: (model.content_height - 1).max(0),
            nPage: model.viewport_height.max(0) as u32,
            nPos: model.clamped_offset(model.offset),
            ..Default::default()
        };
        // The sibling overlay is the sole visible scrollbar. Asking USER32 to redraw the hidden
        // stock non-client bar on every position change races that overlay through a separate DWM
        // redirection surface and is the primary source of drag flicker.
        let _ = SetScrollInfo(self.viewport, SB_VERT, &info, false);
        if let Ok(owner) = GetParent(self.viewport) {
            paint_advanced_scrollbar(self.viewport, owner);
        }
    }

    unsafe fn apply_context(&self) {
        let h = &self.handles;
        let unattended = self.context.unattended_enabled;
        for (index, control) in h.system_checks.into_iter().enumerate() {
            let supported = self
                .context
                .target_capabilities
                .supports_system_option(index);
            let visible = supported && (index != 9 || self.context.wifi_available);
            let _ = ShowWindow(control, if visible { SW_SHOW } else { SW_HIDE });
            let requires_unattended = matches!(index, 2 | 8 | 9);
            let _ = EnableWindow(control, supported && (!requires_unattended || unattended));
        }
        if !unattended {
            for control in [h.system_checks[2], h.system_checks[8], h.system_checks[9]] {
                set_checked(control, false);
            }
        }
        let preinstall_visible = unattended && self.context.preinstall_catalogue_available;
        let _ = ShowWindow(
            h.preinstalled_software_button,
            if preinstall_visible { SW_SHOW } else { SW_HIDE },
        );
        let _ = EnableWindow(h.preinstalled_software_button, preinstall_visible);
        let vmware_visible = unattended && self.context.vmware_tools_available;
        let _ = ShowWindow(
            h.vmware_tools,
            if vmware_visible { SW_SHOW } else { SW_HIDE },
        );
        let _ = EnableWindow(h.vmware_tools, vmware_visible);
        if !vmware_visible {
            set_checked(h.vmware_tools, false);
        }
        let builtin_available = unattended && self.context.builtin_administrator_available;
        let _ = EnableWindow(h.builtin_administrator, builtin_available);
        if !builtin_available {
            set_checked(h.builtin_administrator, false);
            set_checked(h.username.check, true);
        }

        let storage_supported = self.context.target_capabilities.storage_controller_drivers;
        let _ = ShowWindow(
            h.storage_drivers,
            if storage_supported { SW_SHOW } else { SW_HIDE },
        );
        let _ = EnableWindow(h.storage_drivers, storage_supported);

        for control in self.windows_7_controls() {
            let _ = ShowWindow(control, SW_HIDE);
        }
        let win7_compat_visible = self.context.target_capabilities.windows_7;
        for control in [h.windows_7_header, h.windows_7_acpi] {
            let _ = ShowWindow(
                control,
                if win7_compat_visible {
                    SW_SHOW
                } else {
                    SW_HIDE
                },
            );
        }
        let _ = EnableWindow(h.windows_7_acpi, win7_compat_visible);
        for control in self.xp_controls() {
            let _ = ShowWindow(
                control,
                if self.context.target_capabilities.xp {
                    SW_SHOW
                } else {
                    SW_HIDE
                },
            );
        }
        self.update_dependencies();
    }

    fn headings(&self) -> [HWND; 6] {
        let h = &self.handles;
        [
            h.system_header,
            h.scripts_header,
            h.content_header,
            h.identity_header,
            h.windows_7_header,
            h.xp_header,
        ]
    }

    fn check_edits(&self) -> [CheckEdit; 9] {
        let h = &self.handles;
        [
            h.deploy_script,
            h.first_login_script,
            h.custom_drivers,
            h.registry_file,
            h.custom_files,
            h.username,
            h.volume_label,
            h.windows_7_usb3,
            h.windows_7_nvme,
        ]
    }

    fn pair_for_browse_target(&self, target: AdvancedBrowseTarget) -> Option<CheckEdit> {
        self.check_edits()
            .into_iter()
            .find(|pair| pair.browse.is_some_and(|browse| browse.target == target))
    }

    fn checkbox_controls(&self) -> Vec<HWND> {
        let h = &self.handles;
        let mut controls = h.system_checks.to_vec();
        controls.push(h.preserve_personal_files);
        controls.extend(self.check_edits().into_iter().map(|pair| pair.check));
        controls.extend([
            h.storage_drivers,
            h.builtin_administrator,
            h.builtin_administrator_auto_logon,
            h.windows_7_acpi,
            h.windows_7_storage,
            h.windows_7_uefi,
            h.xp_usb3,
            h.xp_nvme,
            h.vmware_tools,
        ]);
        controls
    }

    fn windows_7_controls(&self) -> [HWND; 8] {
        let h = &self.handles;
        [
            h.windows_7_header,
            h.windows_7_usb3.check,
            h.windows_7_usb3.edit,
            h.windows_7_nvme.check,
            h.windows_7_nvme.edit,
            h.windows_7_acpi,
            h.windows_7_storage,
            h.windows_7_uefi,
        ]
    }

    fn xp_controls(&self) -> [HWND; 3] {
        [
            self.handles.xp_header,
            self.handles.xp_usb3,
            self.handles.xp_nvme,
        ]
    }

    fn all_controls(&self) -> Vec<HWND> {
        let mut controls = self.headings().to_vec();
        controls.extend(self.checkbox_controls());
        controls.extend(self.check_edits().into_iter().map(|pair| pair.edit));
        controls.extend(
            self.check_edits()
                .into_iter()
                .filter_map(|pair| pair.browse.map(|browse| browse.button)),
        );
        controls.extend([
            self.handles.preinstalled_software_button,
            self.handles.builtin_administrator_name_label,
            self.handles.builtin_administrator_name,
            self.handles.builtin_administrator_password_label,
            self.handles.builtin_administrator_password,
        ]);
        controls
    }
}

unsafe fn update_viewport_region(viewport: HWND, width: i32, height: i32, dpi: u32) {
    let Some(geometry) = AdvancedViewportGeometry::calculate(width, height, dpi) else {
        let _ = SetWindowRgn(viewport, None, true);
        return;
    };

    // The stock WS_VSCROLL bar remains attached for range, keyboard and accessibility semantics,
    // but it must never become a second visible renderer behind the themed sibling overlay.
    // Clipping the viewport to its content rectangle removes the native trough and its dark outer
    // ring without changing the scrollbar model returned by GetScrollInfo/GetScrollBarInfo.
    let content = CreateRectRgn(0, 0, geometry.scrollbar_left, height);
    if content.is_invalid() {
        let _ = SetWindowRgn(viewport, None, true);
        return;
    }
    if SetWindowRgn(viewport, content, true) == 0 {
        let _ = DeleteObject(content);
        let _ = SetWindowRgn(viewport, None, true);
    }
}

const fn advanced_scrollbar_part_from_index(index: usize) -> Option<AdvancedScrollbarPart> {
    match index {
        0 => Some(AdvancedScrollbarPart::TopArrow),
        1 => Some(AdvancedScrollbarPart::UpperTrack),
        2 => Some(AdvancedScrollbarPart::Thumb),
        3 => Some(AdvancedScrollbarPart::LowerTrack),
        4 => Some(AdvancedScrollbarPart::BottomArrow),
        _ => None,
    }
}

unsafe fn advanced_scrollbar_interaction(viewport: HWND) -> AdvancedScrollbarInteraction {
    let value = GetPropW(viewport, ADVANCED_SCROLLBAR_STATE_PROPERTY).0 as usize;
    match value {
        1..=5 => advanced_scrollbar_part_from_index(value - 1).map_or(
            AdvancedScrollbarInteraction::Normal,
            AdvancedScrollbarInteraction::Hovered,
        ),
        6..=10 => advanced_scrollbar_part_from_index(value - 6).map_or(
            AdvancedScrollbarInteraction::Normal,
            AdvancedScrollbarInteraction::Pressed,
        ),
        11 => AdvancedScrollbarInteraction::Disabled,
        _ => AdvancedScrollbarInteraction::Normal,
    }
}

unsafe fn set_advanced_scrollbar_interaction(
    viewport: HWND,
    interaction: AdvancedScrollbarInteraction,
) -> bool {
    if advanced_scrollbar_interaction(viewport) == interaction {
        return false;
    }
    if interaction == AdvancedScrollbarInteraction::Normal {
        let _ = RemovePropW(viewport, ADVANCED_SCROLLBAR_STATE_PROPERTY);
    } else {
        let value = match interaction {
            AdvancedScrollbarInteraction::Normal => 0,
            AdvancedScrollbarInteraction::Hovered(part) => part as usize + 1,
            AdvancedScrollbarInteraction::Pressed(part) => part as usize + 6,
            AdvancedScrollbarInteraction::Disabled => 11,
        };
        let _ = SetPropW(
            viewport,
            ADVANCED_SCROLLBAR_STATE_PROPERTY,
            HANDLE(value as *mut core::ffi::c_void),
        );
    }
    true
}

unsafe fn advanced_scrollbar_geometry(viewport: HWND) -> Option<(AdvancedScrollbarGeometry, bool)> {
    let mut info = SCROLLBARINFO {
        cbSize: size_of::<SCROLLBARINFO>() as u32,
        ..Default::default()
    };
    GetScrollBarInfo(viewport, OBJID_VSCROLL, &mut info).ok()?;
    const STATE_SYSTEM_UNAVAILABLE: u32 = 0x0000_0001;
    let disabled =
        !IsWindowEnabled(viewport).as_bool() || info.rgstate[0] & STATE_SYSTEM_UNAVAILABLE != 0;
    AdvancedScrollbarGeometry::from_scrollbar_info(&info).map(|geometry| (geometry, disabled))
}

unsafe fn advanced_scrollbar_pointer_interaction(
    viewport: HWND,
    point: POINT,
    pressed: bool,
) -> AdvancedScrollbarInteraction {
    let Some((geometry, disabled)) = advanced_scrollbar_geometry(viewport) else {
        return AdvancedScrollbarInteraction::Normal;
    };
    if disabled {
        AdvancedScrollbarInteraction::Disabled
    } else if let Some(part) = geometry.hit_test(point) {
        if pressed {
            AdvancedScrollbarInteraction::Pressed(part)
        } else {
            AdvancedScrollbarInteraction::Hovered(part)
        }
    } else {
        AdvancedScrollbarInteraction::Normal
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AdvancedScrollbarProxyRange {
    track_top: i32,
    track_bottom: i32,
    thumb_height: i32,
    minimum: i32,
    maximum: i32,
    page: u32,
}

fn advanced_scrollbar_position_from_pointer(
    range: AdvancedScrollbarProxyRange,
    pointer_y: i32,
    drag_offset: i32,
) -> i32 {
    let thumb_height = range.thumb_height.max(1);
    let travel = (range.track_bottom - range.track_top - thumb_height).max(0);
    let maximum_position = range
        .maximum
        .saturating_sub(range.minimum)
        .saturating_sub(range.page.saturating_sub(1) as i32)
        .max(0);
    if travel == 0 || maximum_position == 0 {
        return range.minimum;
    }
    let thumb_top = pointer_y
        .saturating_sub(drag_offset)
        .clamp(range.track_top, range.track_top.saturating_add(travel));
    range.minimum.saturating_add(
        (i64::from(thumb_top - range.track_top) * i64::from(maximum_position) / i64::from(travel))
            as i32,
    )
}

unsafe fn advanced_scrollbar_proxy_position(
    viewport: HWND,
    point: POINT,
    drag_offset: i32,
) -> Option<i32> {
    let (geometry, disabled) = advanced_scrollbar_geometry(viewport)?;
    if disabled {
        return None;
    }
    let mut info = SCROLLINFO {
        cbSize: size_of::<SCROLLINFO>() as u32,
        fMask: SIF_RANGE | SIF_PAGE,
        ..Default::default()
    };
    let _ = GetScrollInfo(viewport, SB_VERT, &mut info);
    let track_top = geometry.top_arrow.bottom;
    let track_bottom = geometry.bottom_arrow.top;
    let thumb_height = (geometry.thumb.bottom - geometry.thumb.top).max(1);
    Some(advanced_scrollbar_position_from_pointer(
        AdvancedScrollbarProxyRange {
            track_top,
            track_bottom,
            thumb_height,
            minimum: info.nMin,
            maximum: info.nMax,
            page: info.nPage,
        },
        point.y,
        drag_offset,
    ))
}

unsafe fn send_advanced_scrollbar_proxy_position(viewport: HWND, code: u32, position: i32) {
    let stored = position.max(0) as usize + 1;
    let _ = SetPropW(
        viewport,
        ADVANCED_SCROLLBAR_PROXY_POSITION_PROPERTY,
        HANDLE(stored as *mut core::ffi::c_void),
    );
    let _ = SendMessageW(
        viewport,
        WM_VSCROLL,
        WPARAM(code as usize),
        LPARAM(viewport.0 as isize),
    );
}

unsafe fn advanced_scrollbar_current_position(viewport: HWND) -> Option<i32> {
    let mut info = SCROLLINFO {
        cbSize: size_of::<SCROLLINFO>() as u32,
        fMask: SIF_POS,
        ..Default::default()
    };
    GetScrollInfo(viewport, SB_VERT, &mut info)
        .is_ok()
        .then_some(info.nPos)
}

fn screen_point_from_lparam(lparam: LPARAM) -> POINT {
    let packed = lparam.0 as u32;
    POINT {
        x: (packed as u16 as i16) as i32,
        y: ((packed >> 16) as u16 as i16) as i32,
    }
}

unsafe fn track_advanced_scrollbar_leave(viewport: HWND) {
    let mut tracking = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE | TME_NONCLIENT,
        hwndTrack: viewport,
        dwHoverTime: 0,
    };
    let _ = TrackMouseEvent(&mut tracking);
}

unsafe fn track_advanced_scrollbar_overlay_leave(overlay: HWND) {
    let mut tracking = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: overlay,
        dwHoverTime: 0,
    };
    let _ = TrackMouseEvent(&mut tracking);
}

unsafe fn paint_embedded_scrollbar_glyph(
    dc: windows::Win32::Graphics::Gdi::HDC,
    rect: RECT,
    glyph: &EmbeddedScrollbarGlyph,
) {
    let width = (rect.right - rect.left).max(0);
    let height = (rect.bottom - rect.top).max(0);
    if width == 0 || height == 0 {
        return;
    }
    let pixels = stretch_scrollbar_glyph(glyph, width, height);
    let _ =
        alpha_blend_premultiplied_bgra(dc, rect.left, rect.top, width, height, pixels.as_slice());
}

unsafe fn paint_advanced_scrollbar_to_dc(
    viewport: HWND,
    owner: HWND,
    surface: HWND,
    dc: windows::Win32::Graphics::Gdi::HDC,
) {
    if viewport.0.is_null() || owner.0.is_null() || surface.0.is_null() || dc.is_invalid() {
        return;
    }

    let mut surface_rect = RECT::default();
    let _ = GetWindowRect(surface, &mut surface_rect);
    let width = (surface_rect.right - surface_rect.left).max(0);
    let height = (surface_rect.bottom - surface_rect.top).max(0);

    // Ask the owning window for the exact current page brush and text colour. This keeps this
    // non-client painter synchronized with live light/dark changes without duplicating palette
    // state inside the viewport subclass.
    let background = SendMessageW(
        owner,
        WM_CTLCOLORSTATIC,
        WPARAM(dc.0 as usize),
        LPARAM(viewport.0 as isize),
    );
    let background_brush = HBRUSH(background.0 as *mut _);
    let background_color = GetBkColor(dc);
    if !background_brush.is_invalid() {
        let _ = FillRect(
            dc,
            &RECT {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            },
            background_brush,
        );
    }
    let Some((screen_geometry, disabled)) = advanced_scrollbar_geometry(viewport) else {
        return;
    };
    let geometry = screen_geometry.translated(-surface_rect.left, -surface_rect.top);

    let interaction = if disabled {
        AdvancedScrollbarInteraction::Disabled
    } else {
        advanced_scrollbar_interaction(viewport)
    };
    let dark = colorref_is_dark(background_color.0);
    let dpi = crate::native_ui::GetDpiForWindow(viewport).max(96);

    // The msstyles resources describe how each part looks once the Windows 11 overlay scrollbar
    // expands, but USER32 separately controls whether the track and arrow parts are present.
    // Normal/Disabled must expose only the compact thumb; drawing the track's Normal bitmap here
    // would leave the permanent dark capsule visible in the user's screenshot.
    if interaction.shows_expanded_parts() {
        // The build step removes only the source theme host surface around the opaque track
        // pixels. Drawing both track halves first preserves the continuous expanded scrollbar
        // without reintroducing the full-height #202020/white frame around it.
        paint_embedded_scrollbar_glyph(
            dc,
            geometry.upper_track,
            embedded_scrollbar_track_glyph(
                dark,
                dpi,
                interaction.state_for(AdvancedScrollbarPart::UpperTrack),
            ),
        );
        paint_embedded_scrollbar_glyph(
            dc,
            geometry.lower_track,
            embedded_scrollbar_track_glyph(
                dark,
                dpi,
                interaction.state_for(AdvancedScrollbarPart::LowerTrack),
            ),
        );
        paint_embedded_scrollbar_glyph(
            dc,
            geometry.top_arrow,
            embedded_scrollbar_arrow_glyph(
                dark,
                dpi,
                false,
                interaction.state_for(AdvancedScrollbarPart::TopArrow),
            ),
        );
        paint_embedded_scrollbar_glyph(
            dc,
            geometry.bottom_arrow,
            embedded_scrollbar_arrow_glyph(
                dark,
                dpi,
                true,
                interaction.state_for(AdvancedScrollbarPart::BottomArrow),
            ),
        );
    }
    paint_embedded_scrollbar_glyph(
        dc,
        geometry.thumb,
        embedded_scrollbar_thumb_glyph(
            dark,
            dpi,
            interaction.state_for(AdvancedScrollbarPart::Thumb),
        ),
    );
}

unsafe fn paint_advanced_scrollbar_buffered_to_dc(
    viewport: HWND,
    owner: HWND,
    surface: HWND,
    target_dc: windows::Win32::Graphics::Gdi::HDC,
) {
    if target_dc.is_invalid() {
        return;
    }
    let mut surface_rect = RECT::default();
    if GetWindowRect(surface, &mut surface_rect).is_err() {
        return;
    }
    let width = (surface_rect.right - surface_rect.left).max(0);
    let height = (surface_rect.bottom - surface_rect.top).max(0);
    if width == 0 || height == 0 {
        return;
    }

    // Compose the complete scrollbar frame offscreen. Publishing background, tracks, arrows and
    // thumb in separate screen writes exposes those intermediate states during a thumb drag.
    let memory_dc = CreateCompatibleDC(target_dc);
    if memory_dc.is_invalid() {
        paint_advanced_scrollbar_to_dc(viewport, owner, surface, target_dc);
        return;
    }
    let bitmap = CreateCompatibleBitmap(target_dc, width, height);
    if bitmap.is_invalid() {
        let _ = DeleteDC(memory_dc);
        paint_advanced_scrollbar_to_dc(viewport, owner, surface, target_dc);
        return;
    }
    let old_bitmap = SelectObject(memory_dc, bitmap);
    paint_advanced_scrollbar_to_dc(viewport, owner, surface, memory_dc);
    let _ = BitBlt(target_dc, 0, 0, width, height, memory_dc, 0, 0, SRCCOPY);
    let _ = SelectObject(memory_dc, old_bitmap);
    let _ = DeleteObject(bitmap);
    let _ = DeleteDC(memory_dc);
}

fn blend_advanced_scrollbar_glyph(
    destination: &mut [u8],
    destination_width: i32,
    destination_height: i32,
    rect: RECT,
    glyph: &EmbeddedScrollbarGlyph,
) {
    let width = (rect.right - rect.left).max(0);
    let height = (rect.bottom - rect.top).max(0);
    if width == 0 || height == 0 || destination_width <= 0 || destination_height <= 0 {
        return;
    }
    let source = stretch_scrollbar_glyph(glyph, width, height);
    for source_y in 0..height {
        let destination_y = rect.top + source_y;
        if !(0..destination_height).contains(&destination_y) {
            continue;
        }
        for source_x in 0..width {
            let destination_x = rect.left + source_x;
            if !(0..destination_width).contains(&destination_x) {
                continue;
            }
            let source_index = (source_y as usize * width as usize + source_x as usize) * 4;
            let destination_index =
                (destination_y as usize * destination_width as usize + destination_x as usize) * 4;
            let alpha = u32::from(source[source_index + 3]);
            let inverse_alpha = 255 - alpha;
            for channel in 0..3 {
                destination[destination_index + channel] =
                    (u32::from(source[source_index + channel])
                        + (u32::from(destination[destination_index + channel]) * inverse_alpha
                            + 127)
                            / 255)
                        .min(255) as u8;
            }
            destination[destination_index + 3] = 255;
        }
    }
}

unsafe fn compose_advanced_scrollbar_frame(
    viewport: HWND,
    owner: HWND,
    surface: HWND,
) -> Option<(i32, i32, u32, Vec<u8>)> {
    let mut surface_rect = RECT::default();
    GetWindowRect(surface, &mut surface_rect).ok()?;
    let width = (surface_rect.right - surface_rect.left).max(0);
    let height = (surface_rect.bottom - surface_rect.top).max(0);
    if width == 0 || height == 0 {
        return None;
    }

    let dc = GetDC(surface);
    if dc.is_invalid() {
        return None;
    }
    let _ = SendMessageW(
        owner,
        WM_CTLCOLORSTATIC,
        WPARAM(dc.0 as usize),
        LPARAM(viewport.0 as isize),
    );
    let background = GetBkColor(dc).0;
    let _ = ReleaseDC(surface, dc);

    let red = (background & 0xff) as u8;
    let green = ((background >> 8) & 0xff) as u8;
    let blue = ((background >> 16) & 0xff) as u8;
    let mut pixels = vec![0_u8; width as usize * height as usize * 4];
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.copy_from_slice(&[blue, green, red, 255]);
    }

    let dpi = crate::native_ui::GetDpiForWindow(viewport).max(96);
    let Some((screen_geometry, disabled)) = advanced_scrollbar_geometry(viewport) else {
        return Some((width, height, dpi, pixels));
    };
    let geometry = screen_geometry.translated(-surface_rect.left, -surface_rect.top);
    let interaction = if disabled {
        AdvancedScrollbarInteraction::Disabled
    } else {
        advanced_scrollbar_interaction(viewport)
    };
    let dark = colorref_is_dark(background);
    if interaction.shows_expanded_parts() {
        for (rect, glyph) in [
            (
                geometry.upper_track,
                embedded_scrollbar_track_glyph(
                    dark,
                    dpi,
                    interaction.state_for(AdvancedScrollbarPart::UpperTrack),
                ),
            ),
            (
                geometry.lower_track,
                embedded_scrollbar_track_glyph(
                    dark,
                    dpi,
                    interaction.state_for(AdvancedScrollbarPart::LowerTrack),
                ),
            ),
            (
                geometry.top_arrow,
                embedded_scrollbar_arrow_glyph(
                    dark,
                    dpi,
                    false,
                    interaction.state_for(AdvancedScrollbarPart::TopArrow),
                ),
            ),
            (
                geometry.bottom_arrow,
                embedded_scrollbar_arrow_glyph(
                    dark,
                    dpi,
                    true,
                    interaction.state_for(AdvancedScrollbarPart::BottomArrow),
                ),
            ),
        ] {
            blend_advanced_scrollbar_glyph(&mut pixels, width, height, rect, glyph);
        }
    }
    blend_advanced_scrollbar_glyph(
        &mut pixels,
        width,
        height,
        geometry.thumb,
        embedded_scrollbar_thumb_glyph(
            dark,
            dpi,
            interaction.state_for(AdvancedScrollbarPart::Thumb),
        ),
    );
    Some((width, height, dpi, pixels))
}

unsafe fn request_advanced_scrollbar_frame(viewport: HWND) {
    let overlay = HWND(GetPropW(viewport, ADVANCED_SCROLLBAR_OVERLAY_PROPERTY).0);
    if overlay.0.is_null()
        || !GetPropW(overlay, ADVANCED_SCROLLBAR_FRAME_PENDING_PROPERTY)
            .0
            .is_null()
    {
        return;
    }
    let _ = SetPropW(
        overlay,
        ADVANCED_SCROLLBAR_FRAME_PENDING_PROPERTY,
        HANDLE(std::ptr::dangling_mut::<c_void>()),
    );
    if PostMessageW(overlay, WM_ADVANCED_SCROLLBAR_FRAME, WPARAM(0), LPARAM(0)).is_err() {
        let _ = RemovePropW(overlay, ADVANCED_SCROLLBAR_FRAME_PENDING_PROPERTY);
        let _ = InvalidateRect(overlay, None, false);
    }
}

unsafe fn queue_advanced_scrollbar_position(viewport: HWND, code: u32, position: i32) {
    let stored = position.max(0) as usize + 1;
    let _ = SetPropW(
        viewport,
        ADVANCED_SCROLLBAR_PENDING_POSITION_PROPERTY,
        HANDLE(stored as *mut c_void),
    );
    let _ = SetPropW(
        viewport,
        ADVANCED_SCROLLBAR_PENDING_CODE_PROPERTY,
        HANDLE(code as usize as *mut c_void),
    );
    request_advanced_scrollbar_frame(viewport);
}

unsafe fn publish_advanced_scrollbar_frame(viewport: HWND, overlay: HWND) {
    let pending_position =
        GetPropW(viewport, ADVANCED_SCROLLBAR_PENDING_POSITION_PROPERTY).0 as usize;
    if pending_position != 0 {
        let code = GetPropW(viewport, ADVANCED_SCROLLBAR_PENDING_CODE_PROPERTY).0 as usize as u32;
        let _ = RemovePropW(viewport, ADVANCED_SCROLLBAR_PENDING_POSITION_PROPERTY);
        let _ = RemovePropW(viewport, ADVANCED_SCROLLBAR_PENDING_CODE_PROPERTY);
        send_advanced_scrollbar_proxy_position(
            viewport,
            if code == 0 {
                SB_THUMBTRACK.0 as u32
            } else {
                code
            },
            pending_position.saturating_sub(1).min(i32::MAX as usize) as i32,
        );
    }

    let composed = GetParent(overlay)
        .ok()
        .and_then(|owner| compose_advanced_scrollbar_frame(viewport, owner, overlay));
    let published = composed
        .as_ref()
        .is_some_and(|(width, height, dpi, pixels)| {
            scrollbar_compositor::publish(overlay, *width, *height, *dpi, pixels)
        });
    if !published {
        let _ = InvalidateRect(overlay, None, false);
    }
}

unsafe fn paint_advanced_scrollbar(viewport: HWND, _owner: HWND) {
    if viewport.0.is_null() {
        return;
    }
    request_advanced_scrollbar_frame(viewport);
}

unsafe extern "system" fn advanced_scrollbar_overlay_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    let viewport = HWND(reference_data as *mut _);
    match message {
        WM_NCHITTEST => LRESULT(HTCLIENT as isize),
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE => {
            let mut point = POINT::default();
            if GetCursorPos(&mut point).is_ok() {
                let mut repaint = false;
                let drag = GetPropW(viewport, ADVANCED_SCROLLBAR_DRAG_OFFSET_PROPERTY).0 as usize;
                if GetCapture() == hwnd && drag != 0 {
                    if let Some(position) =
                        advanced_scrollbar_proxy_position(viewport, point, drag as i32 - 1)
                    {
                        let queued =
                            GetPropW(viewport, ADVANCED_SCROLLBAR_PENDING_POSITION_PROPERTY).0
                                as usize;
                        let queued = (queued != 0)
                            .then_some(queued.saturating_sub(1).min(i32::MAX as usize) as i32);
                        if advanced_scrollbar_current_position(viewport) != Some(position)
                            && queued != Some(position)
                        {
                            // Pointer messages may arrive faster than DWM can present. Keep only
                            // the newest target and publish at most one queued scrollbar frame.
                            queue_advanced_scrollbar_position(
                                viewport,
                                SB_THUMBTRACK.0 as u32,
                                position,
                            );
                        }
                    }
                    repaint = set_advanced_scrollbar_interaction(
                        viewport,
                        AdvancedScrollbarInteraction::Pressed(AdvancedScrollbarPart::Thumb),
                    );
                } else if GetCapture() != hwnd {
                    let next = advanced_scrollbar_pointer_interaction(viewport, point, false);
                    repaint = set_advanced_scrollbar_interaction(viewport, next);
                    if repaint && matches!(next, AdvancedScrollbarInteraction::Hovered(_)) {
                        track_advanced_scrollbar_overlay_leave(hwnd);
                    }
                }
                if repaint {
                    if let Ok(owner) = GetParent(hwnd) {
                        paint_advanced_scrollbar(viewport, owner);
                    }
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE_MESSAGE => {
            if GetCapture() != hwnd
                && set_advanced_scrollbar_interaction(
                    viewport,
                    AdvancedScrollbarInteraction::Normal,
                )
            {
                if let Ok(owner) = GetParent(hwnd) {
                    paint_advanced_scrollbar(viewport, owner);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let mut point = POINT::default();
            let Some((geometry, disabled)) = advanced_scrollbar_geometry(viewport) else {
                return LRESULT(0);
            };
            if disabled || GetCursorPos(&mut point).is_err() {
                return LRESULT(0);
            }
            let Some(part) = geometry.hit_test(point) else {
                return LRESULT(0);
            };
            let _ = SetCapture(hwnd);
            if part == AdvancedScrollbarPart::Thumb {
                let offset = point.y.saturating_sub(geometry.thumb.top).max(0) as usize + 1;
                let _ = SetPropW(
                    viewport,
                    ADVANCED_SCROLLBAR_DRAG_OFFSET_PROPERTY,
                    HANDLE(offset as *mut core::ffi::c_void),
                );
            } else {
                let code = match part {
                    AdvancedScrollbarPart::TopArrow => SB_LINEUP.0 as u32,
                    AdvancedScrollbarPart::UpperTrack => SB_PAGEUP.0 as u32,
                    AdvancedScrollbarPart::LowerTrack => SB_PAGEDOWN.0 as u32,
                    AdvancedScrollbarPart::BottomArrow => SB_LINEDOWN.0 as u32,
                    AdvancedScrollbarPart::Thumb => unreachable!(),
                };
                let _ = SendMessageW(
                    viewport,
                    WM_VSCROLL,
                    WPARAM(code as usize),
                    LPARAM(viewport.0 as isize),
                );
            }
            let _ = set_advanced_scrollbar_interaction(
                viewport,
                AdvancedScrollbarInteraction::Pressed(part),
            );
            if let Ok(owner) = GetParent(hwnd) {
                paint_advanced_scrollbar(viewport, owner);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let mut point = POINT::default();
            let drag = GetPropW(viewport, ADVANCED_SCROLLBAR_DRAG_OFFSET_PROPERTY).0 as usize;
            if drag != 0 && GetCursorPos(&mut point).is_ok() {
                if let Some(position) =
                    advanced_scrollbar_proxy_position(viewport, point, drag as i32 - 1)
                {
                    queue_advanced_scrollbar_position(
                        viewport,
                        SB_THUMBPOSITION.0 as u32,
                        position,
                    );
                }
            }
            let _ = RemovePropW(viewport, ADVANCED_SCROLLBAR_DRAG_OFFSET_PROPERTY);
            if GetCapture() == hwnd {
                let _ = ReleaseCapture();
            }
            let next = if GetCursorPos(&mut point).is_ok() {
                advanced_scrollbar_pointer_interaction(viewport, point, false)
            } else {
                AdvancedScrollbarInteraction::Normal
            };
            let _ = set_advanced_scrollbar_interaction(viewport, next);
            if matches!(next, AdvancedScrollbarInteraction::Hovered(_)) {
                track_advanced_scrollbar_overlay_leave(hwnd);
            }
            if let Ok(owner) = GetParent(hwnd) {
                paint_advanced_scrollbar(viewport, owner);
            }
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            let _ = RemovePropW(viewport, ADVANCED_SCROLLBAR_DRAG_OFFSET_PROPERTY);
            if set_advanced_scrollbar_interaction(viewport, AdvancedScrollbarInteraction::Normal) {
                if let Ok(owner) = GetParent(hwnd) {
                    paint_advanced_scrollbar(viewport, owner);
                }
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => SendMessageW(viewport, message, wparam, lparam),
        WM_ADVANCED_SCROLLBAR_FRAME => {
            publish_advanced_scrollbar_frame(viewport, hwnd);
            let _ = RemovePropW(hwnd, ADVANCED_SCROLLBAR_FRAME_PENDING_PROPERTY);
            if !GetPropW(viewport, ADVANCED_SCROLLBAR_PENDING_POSITION_PROPERTY)
                .0
                .is_null()
            {
                request_advanced_scrollbar_frame(viewport);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            if !scrollbar_compositor::is_active(hwnd) {
                if let Ok(owner) = GetParent(hwnd) {
                    paint_advanced_scrollbar_buffered_to_dc(viewport, owner, hwnd, dc);
                }
            }
            let _ = EndPaint(hwnd, &paint);
            LRESULT(0)
        }
        WM_NCDESTROY => {
            scrollbar_compositor::remove(hwnd);
            let _ = RemovePropW(hwnd, ADVANCED_SCROLLBAR_FRAME_PENDING_PROPERTY);
            let _ = RemoveWindowSubclass(
                hwnd,
                Some(advanced_scrollbar_overlay_proc),
                SCROLLBAR_OVERLAY_SUBCLASS_ID,
            );
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

unsafe extern "system" fn advanced_viewport_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    let owner = HWND(reference_data as *mut _);
    match message {
        WM_COMMAND | WM_DRAWITEM | WM_CTLCOLORBTN | WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC => {
            SendMessageW(owner, message, wparam, lparam)
        }
        WM_MOUSEWHEEL => SendMessageW(owner, message, wparam, lparam),
        WM_VSCROLL => SendMessageW(owner, message, wparam, LPARAM(hwnd.0 as isize)),
        WM_NCMOUSEMOVE_MESSAGE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            let point = screen_point_from_lparam(lparam);
            let previous = advanced_scrollbar_interaction(hwnd);
            let next = if matches!(previous, AdvancedScrollbarInteraction::Pressed(_)) {
                previous
            } else {
                advanced_scrollbar_pointer_interaction(hwnd, point, false)
            };
            if set_advanced_scrollbar_interaction(hwnd, next) {
                if previous == AdvancedScrollbarInteraction::Normal
                    && matches!(next, AdvancedScrollbarInteraction::Hovered(_))
                {
                    track_advanced_scrollbar_leave(hwnd);
                }
                paint_advanced_scrollbar(hwnd, owner);
            }
            result
        }
        WM_NCMOUSELEAVE_MESSAGE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if !matches!(
                advanced_scrollbar_interaction(hwnd),
                AdvancedScrollbarInteraction::Pressed(_)
            ) && set_advanced_scrollbar_interaction(hwnd, AdvancedScrollbarInteraction::Normal)
            {
                paint_advanced_scrollbar(hwnd, owner);
            }
            result
        }
        WM_NCLBUTTONDOWN => {
            let point = screen_point_from_lparam(lparam);
            let pressed = advanced_scrollbar_pointer_interaction(hwnd, point, true);
            let did_press = matches!(pressed, AdvancedScrollbarInteraction::Pressed(_));
            if did_press && set_advanced_scrollbar_interaction(hwnd, pressed) {
                paint_advanced_scrollbar(hwnd, owner);
            }
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            let mut cursor = POINT::default();
            let next = if GetCursorPos(&mut cursor).is_ok() {
                advanced_scrollbar_pointer_interaction(hwnd, cursor, false)
            } else {
                AdvancedScrollbarInteraction::Normal
            };
            if set_advanced_scrollbar_interaction(hwnd, next) || did_press {
                if matches!(next, AdvancedScrollbarInteraction::Hovered(_)) {
                    track_advanced_scrollbar_leave(hwnd);
                }
                paint_advanced_scrollbar(hwnd, owner);
            }
            result
        }
        WM_NCLBUTTONUP => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            let point = screen_point_from_lparam(lparam);
            let next = advanced_scrollbar_pointer_interaction(hwnd, point, false);
            if set_advanced_scrollbar_interaction(hwnd, next) {
                if matches!(next, AdvancedScrollbarInteraction::Hovered(_)) {
                    track_advanced_scrollbar_leave(hwnd);
                }
                paint_advanced_scrollbar(hwnd, owner);
            }
            result
        }
        WM_ENABLE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            let next = if IsWindowEnabled(hwnd).as_bool() {
                AdvancedScrollbarInteraction::Normal
            } else {
                AdvancedScrollbarInteraction::Disabled
            };
            if set_advanced_scrollbar_interaction(hwnd, next) {
                paint_advanced_scrollbar(hwnd, owner);
            }
            result
        }
        WM_NCPAINT | WM_NCACTIVATE | WM_SIZE | WM_THEMECHANGED => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            paint_advanced_scrollbar(hwnd, owner);
            result
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_NCDESTROY => {
            let _ = RemovePropW(hwnd, ADVANCED_SCROLLBAR_STATE_PROPERTY);
            let _ = RemovePropW(hwnd, ADVANCED_SCROLLBAR_DRAG_OFFSET_PROPERTY);
            let _ = RemovePropW(hwnd, ADVANCED_SCROLLBAR_PROXY_POSITION_PROPERTY);
            let _ = RemovePropW(hwnd, ADVANCED_SCROLLBAR_PENDING_POSITION_PROPERTY);
            let _ = RemovePropW(hwnd, ADVANCED_SCROLLBAR_PENDING_CODE_PROPERTY);
            let _ = RemovePropW(hwnd, ADVANCED_SCROLLBAR_OVERLAY_PROPERTY);
            let _ = RemoveWindowSubclass(hwnd, Some(advanced_viewport_proc), VIEWPORT_SUBCLASS_ID);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

unsafe fn label(parent: HWND, text: &str, id: u16) -> windows::core::Result<HWND> {
    child(parent, w!("STATIC"), text, 0, id)
}

unsafe fn owner_draw_label(parent: HWND, text: &str, id: u16) -> windows::core::Result<HWND> {
    child(parent, w!("STATIC"), text, SS_OWNERDRAW_STYLE, id)
}

unsafe fn checkbox(parent: HWND, text: &str, id: u16) -> windows::core::Result<HWND> {
    child(
        parent,
        w!("BUTTON"),
        text,
        BS_AUTOCHECKBOX | WS_TABSTOP.0 as i32,
        id,
    )
}

unsafe fn action_button(parent: HWND, text: &str, id: u16) -> windows::core::Result<HWND> {
    child(
        parent,
        w!("BUTTON"),
        text,
        BS_OWNERDRAW | WS_TABSTOP.0 as i32,
        id,
    )
}

unsafe fn radio_button(
    parent: HWND,
    text: &str,
    id: u16,
    starts_group: bool,
) -> windows::core::Result<HWND> {
    let group_style = if starts_group { WS_GROUP.0 as i32 } else { 0 };
    child(
        parent,
        w!("BUTTON"),
        text,
        BS_AUTORADIOBUTTON | WS_TABSTOP.0 as i32 | group_style,
        id,
    )
}

unsafe fn edit(parent: HWND, text: &str, id: u16) -> windows::core::Result<HWND> {
    edit_with_style(parent, text, id, 0)
}

unsafe fn password_edit(parent: HWND, text: &str, id: u16) -> windows::core::Result<HWND> {
    edit_with_style(parent, text, id, ES_PASSWORD as u32)
}

unsafe fn edit_with_style(
    parent: HWND,
    text: &str,
    id: u16,
    extra_style: u32,
) -> windows::core::Result<HWND> {
    let text = wide(text);
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE(WS_EX_CLIENTEDGE.0 | 0x0000_0004),
        w!("EDIT"),
        PCWSTR(text.as_ptr()),
        WINDOW_STYLE((WS_CHILD | WS_TABSTOP).0 | ES_AUTOHSCROLL as u32 | extra_style),
        0,
        0,
        0,
        0,
        parent,
        HMENU(id as isize as *mut _),
        HINSTANCE::default(),
        None,
    )?;
    center_single_line_edit_in_row(hwnd);
    Ok(hwnd)
}

unsafe fn check_edit(
    parent: HWND,
    label: &str,
    text: &str,
    check_id: u16,
    edit_id: u16,
    browse: Option<(u16, AdvancedBrowseTarget)>,
) -> windows::core::Result<CheckEdit> {
    Ok(CheckEdit {
        check: checkbox(parent, label, check_id)?,
        edit: edit(parent, text, edit_id)?,
        browse: browse
            .map(|(id, target)| {
                Ok::<BrowseControl, windows::core::Error>(BrowseControl {
                    button: child(
                        parent,
                        w!("BUTTON"),
                        &crate::tr!("浏览..."),
                        BS_OWNERDRAW | WS_TABSTOP.0 as i32,
                        id,
                    )?,
                    id,
                    target,
                })
            })
            .transpose()?,
    })
}

unsafe fn radio_edit(
    parent: HWND,
    label: &str,
    text: &str,
    check_id: u16,
    edit_id: u16,
    browse: Option<(u16, AdvancedBrowseTarget)>,
) -> windows::core::Result<CheckEdit> {
    Ok(CheckEdit {
        check: radio_button(parent, label, check_id, true)?,
        edit: edit(parent, text, edit_id)?,
        browse: browse
            .map(|(id, target)| {
                Ok::<BrowseControl, windows::core::Error>(BrowseControl {
                    button: child(
                        parent,
                        w!("BUTTON"),
                        &crate::tr!("浏览..."),
                        BS_OWNERDRAW | WS_TABSTOP.0 as i32,
                        id,
                    )?,
                    id,
                    target,
                })
            })
            .transpose()?,
    })
}

unsafe fn set_checked(control: HWND, checked: bool) {
    let _ = SendMessageW(
        control,
        BM_SETCHECK,
        WPARAM(usize::from(checked)),
        LPARAM(0),
    );
}

unsafe fn is_checked(control: HWND) -> bool {
    SendMessageW(control, BM_GETCHECK, WPARAM(0), LPARAM(0)).0 == 1
}

unsafe fn apply_check_edit(pair: CheckEdit, checked: bool, text: &str) {
    set_checked(pair.check, checked);
    set_text(pair.edit, text);
}

unsafe fn read_text(control: HWND) -> String {
    let length = GetWindowTextLengthW(control).max(0) as usize;
    let mut buffer = vec![0_u16; length + 1];
    let copied = GetWindowTextW(control, &mut buffer).max(0) as usize;
    String::from_utf16_lossy(&buffer[..copied])
}

/// Checked options backed by a required value must never persist as active with an empty value.
/// Returning from the page normalizes both the visible checkbox and the stored model.
unsafe fn read_required_pair(pair: CheckEdit) -> (bool, String) {
    let (enabled, value) = normalize_required_value(is_checked(pair.check), &read_text(pair.edit));
    if !enabled {
        set_checked(pair.check, false);
    }
    (enabled, value)
}

fn normalize_required_value(checked: bool, value: &str) -> (bool, String) {
    let value = value.trim().to_owned();
    (checked && !value.is_empty(), value)
}

unsafe fn set_text(control: HWND, text: &str) {
    let text = wide(text);
    let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(control, PCWSTR(text.as_ptr()));
}

unsafe fn relocalize_check_edit(pair: CheckEdit, label: &str) {
    set_text(pair.check, label);
    if let Some(browse) = pair.browse {
        set_text(browse.button, &crate::tr!("浏览..."));
    }
}

unsafe fn layout_heading(control: HWND, x: i32, y: &mut i32, width: i32, dpi: u32) {
    let s = |value: i32| ((value as i64 * dpi.max(1) as i64 + 48) / 96) as i32;
    let _ = MoveWindow(control, x, *y, width, s(22), false);
    *y += s(27);
}

unsafe fn layout_check(control: HWND, x: i32, y: &mut i32, width: i32, dpi: u32) {
    let s = |value: i32| ((value as i64 * dpi.max(1) as i64 + 48) / 96) as i32;
    // Match the 24 px checkbox HWND used by the main install page. The shared 13 px glyph is then
    // centred against the same client height instead of looking vertically tighter on this page.
    let _ = MoveWindow(control, x, *y, width, s(24), false);
    *y += s(24);
}

unsafe fn layout_pair(pair: CheckEdit, x: i32, y: &mut i32, width: i32, dpi: u32) {
    let s = |value: i32| ((value as i64 * dpi.max(1) as i64 + 48) / 96) as i32;
    let field_height = InnoMetrics::for_dpi(dpi).field_height;
    let _ = MoveWindow(pair.check, x, *y, width, s(24), false);
    *y += s(24);
    let browse_width = pair.browse.map_or(0, |_| s(76));
    let browse_gap = pair.browse.map_or(0, |_| s(6));
    let edit_width = (width - s(20) - browse_width - browse_gap).max(0);
    let _ = MoveWindow(pair.edit, x + s(20), *y, edit_width, field_height, false);
    if let Some(browse) = pair.browse {
        let _ = MoveWindow(
            browse.button,
            x + s(20) + edit_width + browse_gap,
            *y,
            browse_width,
            field_height,
            false,
        );
    }
    *y += s(30);
}

unsafe fn layout_labeled_edit(label: HWND, edit: HWND, x: i32, y: &mut i32, width: i32, dpi: u32) {
    let s = |value: i32| ((value as i64 * dpi.max(1) as i64 + 48) / 96) as i32;
    let field_height = InnoMetrics::for_dpi(dpi).field_height;
    let _ = MoveWindow(label, x + s(20), *y, (width - s(20)).max(0), s(24), false);
    *y += s(24);
    let _ = MoveWindow(
        edit,
        x + s(20),
        *y,
        (width - s(20)).max(0),
        field_height,
        false,
    );
    *y += s(30);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_defaults_do_not_expose_version_specific_options() {
        let context = AdvancedPageContext::default();
        assert!(context.unattended_enabled);
        assert!(context.builtin_administrator_available);
        assert!(!context.wifi_available);
        assert_eq!(
            context.target_capabilities,
            AdvancedOptionCapabilities::unknown()
        );
    }

    #[test]
    fn preinstall_button_is_centred_and_expands_only_for_its_caption() {
        assert_eq!(
            centered_button_layout(100, 400, 62, 28, 96),
            CenteredButtonLayout { x: 252, width: 96 }
        );
        assert_eq!(
            centered_button_layout(100, 400, 142, 28, 96),
            CenteredButtonLayout { x: 215, width: 170 }
        );
        assert_eq!(
            centered_button_layout(100, 120, 240, 28, 96),
            CenteredButtonLayout { x: 100, width: 120 }
        );
    }

    #[test]
    fn every_user_selectable_advanced_flag_has_a_native_control_mapping() {
        let data = AdvancedOptionsData {
            remove_shortcut_arrow: true,
            restore_classic_context_menu: true,
            bypass_nro: true,
            disable_windows_update: true,
            disable_windows_defender: true,
            disable_reserved_storage: true,
            disable_uac: true,
            disable_device_encryption: true,
            remove_uwp_apps: true,
            migrate_wifi: true,
            run_script_during_deploy: true,
            run_script_first_login: true,
            import_custom_drivers: true,
            import_storage_controller_drivers: true,
            import_registry_file: true,
            import_custom_files: true,
            custom_username: true,
            custom_volume_label: true,
            win7_inject_usb3_driver: true,
            win7_inject_nvme_driver: true,
            win7_fix_acpi_bsod: true,
            win7_fix_storage_bsod: true,
            win7_uefi_patch: true,
            xp_inject_usb3_driver: true,
            xp_inject_nvme_driver: true,
            ..AdvancedOptionsData::default()
        };
        let mapped_flags = [
            data.remove_shortcut_arrow,
            data.restore_classic_context_menu,
            data.bypass_nro,
            data.disable_windows_update,
            data.disable_windows_defender,
            data.disable_reserved_storage,
            data.disable_uac,
            data.disable_device_encryption,
            data.remove_uwp_apps,
            data.migrate_wifi,
            data.run_script_during_deploy,
            data.run_script_first_login,
            data.import_custom_drivers,
            data.import_storage_controller_drivers,
            data.import_registry_file,
            data.import_custom_files,
            data.custom_username,
            data.custom_volume_label,
            data.xp_inject_usb3_driver,
            data.xp_inject_nvme_driver,
        ];
        assert!(mapped_flags.into_iter().all(|value| value));
    }

    #[test]
    fn responsive_grid_uses_three_two_and_one_columns() {
        assert_eq!(AdvancedGrid::calculate(1_200, 96).columns, 3);
        assert_eq!(AdvancedGrid::calculate(820, 96).columns, 2);
        assert_eq!(AdvancedGrid::calculate(480, 96).columns, 1);
    }

    #[test]
    fn viewport_width_is_not_scaled_twice_at_high_dpi() {
        for (width, expected_columns) in [(1_200, 3), (820, 2), (480, 1)] {
            for dpi in [96, 120, 144, 192] {
                assert_eq!(
                    AdvancedGrid::calculate(width, dpi).columns,
                    expected_columns,
                    "viewport width {width} at {dpi} DPI"
                );
            }
        }
    }

    #[test]
    fn every_column_stays_inside_the_available_width() {
        for (width, dpi) in [(1_200, 96), (820, 96), (600, 96), (1_640, 192)] {
            let grid = AdvancedGrid::calculate(width, dpi);
            for column in 0..grid.columns {
                let x = grid.x(0, column);
                assert!(x >= 0);
                assert!(x + grid.column_width <= width);
            }
            assert!(grid.column_width >= 0);
        }
    }

    #[test]
    fn final_column_reserves_scrollbar_width_and_a_separate_field_gap() {
        for (width, height, dpi) in [(1_017, 640, 96), (1_200, 900, 144), (1_640, 960, 192)] {
            let geometry = AdvancedViewportGeometry::calculate(width, height, dpi).unwrap();
            let grid = AdvancedGrid::calculate(geometry.content_width, dpi);
            let final_column = grid.columns - 1;
            let final_field_right = grid.x(0, final_column) + grid.column_width;
            assert!(final_field_right <= geometry.content_width);
            assert!(geometry.content_width < geometry.scrollbar_left);
            assert_eq!(
                geometry.scrollbar_left - geometry.content_width,
                ((i64::from(SCROLLBAR_CONTENT_GAP) * i64::from(dpi) + 48) / 96) as i32
            );
            assert_eq!(geometry.corner_diameter, geometry.scrollbar_width);
        }
    }

    #[test]
    fn custom_scrollbar_thumb_uses_the_full_theme_frame_and_tracks_both_endpoints() {
        let first = AdvancedScrollbarThumb::calculate(26, 625, 0, 999, 400, 0, 144).unwrap();
        let last = AdvancedScrollbarThumb::calculate(26, 625, 0, 999, 400, 600, 144).unwrap();
        assert_eq!(first.right - first.left, 26);
        assert_eq!(first.top, 8);
        assert_eq!(last.bottom, 617);
        assert_eq!(first.bottom - first.top, last.bottom - last.top);
        assert!(first.bottom < last.top);
        assert_eq!(
            AdvancedScrollbarThumb::calculate(26, 625, 0, 399, 400, 0, 144),
            None
        );
    }

    #[test]
    fn scrollbar_pointer_proxy_maps_drag_to_native_range_endpoints() {
        let range = AdvancedScrollbarProxyRange {
            track_top: 10,
            track_bottom: 110,
            thumb_height: 20,
            minimum: 100,
            maximum: 1_099,
            page: 400,
        };
        let position = |pointer_y| advanced_scrollbar_position_from_pointer(range, pointer_y, 5);
        assert_eq!(position(-500), 100);
        assert_eq!(position(15), 100);
        assert_eq!(position(55), 400);
        assert_eq!(position(95), 700);
        assert_eq!(position(500), 700);
        assert_eq!(
            advanced_scrollbar_position_from_pointer(
                AdvancedScrollbarProxyRange {
                    track_top: 10,
                    track_bottom: 30,
                    thumb_height: 20,
                    minimum: 42,
                    maximum: 99,
                    page: 10,
                },
                20,
                5,
            ),
            42
        );
    }

    #[test]
    fn extracted_scrollbar_theme_maps_five_dpi_buckets_and_preserves_round_caps() {
        assert_eq!(embedded_scrollbar_dpi_index(96), 0);
        assert_eq!(embedded_scrollbar_dpi_index(120), 1);
        assert_eq!(embedded_scrollbar_dpi_index(144), 2);
        assert_eq!(embedded_scrollbar_dpi_index(192), 3);
        assert_eq!(embedded_scrollbar_dpi_index(240), 4);

        let glyph = embedded_scrollbar_thumb_glyph(true, 144, AdvancedScrollbarState::Normal);
        assert_eq!((glyph.width, glyph.height), (26, 16));
        let stretched = stretch_scrollbar_glyph(glyph, 26, 96);
        assert_eq!(stretched.len(), 26 * 96 * 4);
        // Both end caps come straight from different source rows instead of repeating the centre.
        assert_ne!(&stretched[..26 * 4], &stretched[7 * 26 * 4..8 * 26 * 4]);
        assert_ne!(
            &stretched[95 * 26 * 4..96 * 26 * 4],
            &stretched[88 * 26 * 4..89 * 26 * 4]
        );
    }

    #[test]
    fn extracted_scrollbar_transparency_removes_source_frame_backgrounds() {
        fn opaque_bounds(glyph: &EmbeddedScrollbarGlyph) -> Option<(i32, i32, i32, i32)> {
            let mut left = glyph.width;
            let mut top = glyph.height;
            let mut right = 0;
            let mut bottom = 0;
            let mut found = false;
            for y in 0..glyph.height {
                for x in 0..glyph.width {
                    let alpha = glyph.bgra[((y * glyph.width + x) * 4 + 3) as usize];
                    if alpha == 0 {
                        continue;
                    }
                    found = true;
                    left = left.min(x);
                    top = top.min(y);
                    right = right.max(x + 1);
                    bottom = bottom.max(y + 1);
                }
            }
            found.then_some((left, top, right, bottom))
        }

        for dark in [false, true] {
            let normal = embedded_scrollbar_thumb_glyph(dark, 144, AdvancedScrollbarState::Normal);
            let hot = embedded_scrollbar_thumb_glyph(dark, 144, AdvancedScrollbarState::Hot);
            let arrow =
                embedded_scrollbar_arrow_glyph(dark, 144, false, AdvancedScrollbarState::Normal);
            let track = embedded_scrollbar_track_glyph(dark, 144, AdvancedScrollbarState::Normal);
            let track_hot = embedded_scrollbar_track_glyph(dark, 144, AdvancedScrollbarState::Hot);

            assert_eq!(
                normal.bgra[3], 0,
                "thumb source background must be transparent"
            );
            assert_eq!(
                arrow.bgra[3], 0,
                "arrow source background must be transparent"
            );
            let hot_arrow =
                embedded_scrollbar_arrow_glyph(dark, 144, false, AdvancedScrollbarState::Hot);
            let host_fringe = if dark { [33, 33, 33] } else { [254, 254, 254] };
            assert!(
                hot_arrow
                    .bgra
                    .chunks_exact(4)
                    .all(|pixel| pixel[3] == 0 || pixel[..3] != host_fringe),
                "the hot arrow must not retain the scaled host-surface fringe"
            );
            assert!(
                track.bgra.chunks_exact(4).all(|pixel| pixel[3] == 0),
                "the hidden normal track must not leave its source host surface behind"
            );
            assert_eq!(
                track_hot.bgra[3], 0,
                "expanded track source host surface must be transparent"
            );
            assert!(
                track_hot.bgra.chunks_exact(4).any(|pixel| pixel[3] != 0),
                "expanded track must retain its visible theme pixels"
            );

            let transparent_pixels = normal
                .bgra
                .chunks_exact(4)
                .filter(|pixel| pixel[3] == 0)
                .count();
            assert!(
                transparent_pixels * 2 > (normal.width * normal.height) as usize,
                "the thin normal thumb must not retain an opaque frame-sized rectangle"
            );

            let normal_bounds = opaque_bounds(normal).expect("normal thumb has visible pixels");
            let hot_bounds = opaque_bounds(hot).expect("hot thumb has visible pixels");
            assert!(
                hot_bounds.2 - hot_bounds.0 > normal_bounds.2 - normal_bounds.0,
                "hovered thumb must be wider than the resting indicator"
            );
        }
    }

    #[test]
    fn composition_frame_blends_theme_pixels_onto_one_opaque_background() {
        let glyph = embedded_scrollbar_thumb_glyph(true, 144, AdvancedScrollbarState::Hot);
        let mut frame = vec![0_u8; (glyph.width * glyph.height * 4) as usize];
        for pixel in frame.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[32, 32, 32, 255]);
        }
        blend_advanced_scrollbar_glyph(
            &mut frame,
            glyph.width,
            glyph.height,
            RECT {
                left: 0,
                top: 0,
                right: glyph.width,
                bottom: glyph.height,
            },
            glyph,
        );

        assert!(frame.chunks_exact(4).all(|pixel| pixel[3] == 255));
        let transparent = glyph
            .bgra
            .chunks_exact(4)
            .position(|pixel| pixel[3] == 0)
            .expect("theme source includes transparent host pixels");
        assert_eq!(
            &frame[transparent * 4..transparent * 4 + 4],
            &[32, 32, 32, 255]
        );
        let visible = glyph
            .bgra
            .chunks_exact(4)
            .position(|pixel| pixel[3] != 0)
            .expect("theme source includes visible thumb pixels");
        assert_ne!(&frame[visible * 4..visible * 4 + 3], &[32, 32, 32]);
    }

    #[test]
    fn extracted_scrollbar_theme_contains_every_component_and_state() {
        for dark in [false, true] {
            for state in [
                AdvancedScrollbarState::Normal,
                AdvancedScrollbarState::Hot,
                AdvancedScrollbarState::Pressed,
                AdvancedScrollbarState::Disabled,
                AdvancedScrollbarState::Hover,
            ] {
                let thumb = embedded_scrollbar_thumb_glyph(dark, 144, state);
                let track = embedded_scrollbar_track_glyph(dark, 144, state);
                let top = embedded_scrollbar_arrow_glyph(dark, 144, false, state);
                let bottom = embedded_scrollbar_arrow_glyph(dark, 144, true, state);
                assert_eq!((thumb.width, thumb.height), (26, 16));
                assert_eq!((track.width, track.height), (26, 1));
                assert_eq!((top.width, top.height), (26, 26));
                assert_eq!((bottom.width, bottom.height), (26, 26));
                for glyph in [thumb, track, top, bottom] {
                    assert_eq!(glyph.bgra.len(), (glyph.width * glyph.height * 4) as usize);
                }
            }
        }
    }

    #[test]
    fn scrollbar_interaction_expands_siblings_and_marks_only_the_active_part_hot() {
        let hovered = AdvancedScrollbarInteraction::Hovered(AdvancedScrollbarPart::Thumb);
        assert_eq!(
            hovered.state_for(AdvancedScrollbarPart::Thumb),
            AdvancedScrollbarState::Hot
        );
        for part in [
            AdvancedScrollbarPart::TopArrow,
            AdvancedScrollbarPart::UpperTrack,
            AdvancedScrollbarPart::LowerTrack,
            AdvancedScrollbarPart::BottomArrow,
        ] {
            assert_eq!(hovered.state_for(part), AdvancedScrollbarState::Hover);
        }

        let pressed = AdvancedScrollbarInteraction::Pressed(AdvancedScrollbarPart::TopArrow);
        assert_eq!(
            pressed.state_for(AdvancedScrollbarPart::TopArrow),
            AdvancedScrollbarState::Pressed
        );
        assert_eq!(
            pressed.state_for(AdvancedScrollbarPart::Thumb),
            AdvancedScrollbarState::Hover
        );
        for part in ADVANCED_SCROLLBAR_PARTS {
            assert_eq!(
                AdvancedScrollbarInteraction::Disabled.state_for(part),
                AdvancedScrollbarState::Disabled
            );
            assert_eq!(
                AdvancedScrollbarInteraction::Normal.state_for(part),
                AdvancedScrollbarState::Normal
            );
        }
        assert!(!AdvancedScrollbarInteraction::Normal.shows_expanded_parts());
        assert!(!AdvancedScrollbarInteraction::Disabled.shows_expanded_parts());
        assert!(hovered.shows_expanded_parts());
        assert!(pressed.shows_expanded_parts());
    }

    #[test]
    fn system_scrollbar_geometry_partitions_the_exact_reported_rectangle() {
        let info = SCROLLBARINFO {
            cbSize: size_of::<SCROLLBARINFO>() as u32,
            rcScrollBar: RECT {
                left: 100,
                top: 200,
                right: 126,
                bottom: 825,
            },
            dxyLineButton: 26,
            xyThumbTop: 120,
            xyThumbBottom: 320,
            ..Default::default()
        };
        let geometry = AdvancedScrollbarGeometry::from_scrollbar_info(&info).unwrap();
        assert_eq!(
            (geometry.top_arrow.top, geometry.top_arrow.bottom),
            (200, 226)
        );
        assert_eq!(
            (geometry.upper_track.top, geometry.upper_track.bottom),
            (226, 320)
        );
        assert_eq!((geometry.thumb.top, geometry.thumb.bottom), (320, 520));
        assert_eq!(
            (geometry.lower_track.top, geometry.lower_track.bottom),
            (520, 799)
        );
        assert_eq!(
            (geometry.bottom_arrow.top, geometry.bottom_arrow.bottom),
            (799, 825)
        );
        assert_eq!(
            geometry.hit_test(POINT { x: 112, y: 400 }),
            Some(AdvancedScrollbarPart::Thumb)
        );
        assert_eq!(geometry.hit_test(POINT { x: 99, y: 400 }), None);
    }

    #[test]
    fn scrollbar_theme_mode_comes_from_the_actual_page_background() {
        assert!(colorref_is_dark(0x0020_2020));
        assert!(!colorref_is_dark(0x00f5_f5f5));
    }

    #[test]
    fn shortest_column_balances_sections_without_reordering_inside_them() {
        assert_eq!(shortest_column(&[280, 130]), 1);
        assert_eq!(shortest_column(&[280, 350]), 0);
        assert_eq!(shortest_column(&[210, 210, 300]), 0);
    }

    #[test]
    fn browse_command_ids_map_to_explicit_targets_and_ignore_other_controls() {
        let controls = [
            (801, AdvancedBrowseTarget::DeployScript),
            (802, AdvancedBrowseTarget::FirstLoginScript),
            (803, AdvancedBrowseTarget::CustomDriversDirectory),
            (804, AdvancedBrowseTarget::RegistryFile),
            (805, AdvancedBrowseTarget::CustomFilesDirectory),
            (806, AdvancedBrowseTarget::Windows7Usb3Drivers),
            (807, AdvancedBrowseTarget::Windows7NvmeDrivers),
        ];
        for (id, target) in controls {
            assert_eq!(
                browse_intent_for_id(id, controls),
                Some(AdvancedPageIntent::Browse(target))
            );
        }
        assert_eq!(browse_intent_for_id(999, controls), None);
    }

    #[test]
    fn required_path_options_disable_when_the_path_is_empty() {
        assert_eq!(
            normalize_required_value(true, "   "),
            (false, String::new())
        );
        assert_eq!(
            normalize_required_value(true, " C:\\drivers "),
            (true, String::from("C:\\drivers"))
        );
        assert_eq!(
            normalize_required_value(false, "C:\\drivers"),
            (false, String::from("C:\\drivers"))
        );
    }

    #[test]
    fn scroll_model_clamps_to_the_visible_content_range() {
        let model = ScrollModel {
            offset: 0,
            content_height: 900,
            viewport_height: 420,
        };
        assert_eq!(model.maximum(), 480);
        assert_eq!(model.clamped_offset(-20), 0);
        assert_eq!(model.clamped_offset(240), 240);
        assert_eq!(model.clamped_offset(900), 480);
    }

    #[test]
    fn scroll_model_disables_scrolling_when_content_fits() {
        let model = ScrollModel {
            offset: 100,
            content_height: 360,
            viewport_height: 420,
        };
        assert_eq!(model.maximum(), 0);
        assert_eq!(model.clamped_offset(100), 0);
    }

    #[test]
    fn smooth_scroll_step_is_monotonic_and_reaches_both_targets() {
        for target in [480_i32, -480_i32] {
            let mut current = 0;
            let direction = target.signum();
            for _ in 0..64 {
                let next = smooth_scroll_step(current, target);
                assert_eq!((next - current).signum(), direction);
                assert!((target - next).unsigned_abs() < (target - current).unsigned_abs());
                current = next;
                if current == target {
                    break;
                }
            }
            assert_eq!(current, target);
        }
        assert_eq!(smooth_scroll_step(240, 240), 240);
    }
}
