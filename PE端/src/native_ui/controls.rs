use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    COLORREF, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreatePen, CreateSolidBrush,
    DeleteDC, DeleteObject, DrawTextW, FillRect, GetBrushOrgEx, GetTextExtentPoint32W, GetWindowDC,
    InvalidateRect, LineTo, MoveToEx, ReleaseDC, RoundRect, SelectObject, SetBkMode, SetBrushOrgEx,
    SetStretchBltMode, SetTextColor, StretchBlt, DT_CENTER, DT_END_ELLIPSIS, DT_NOPREFIX,
    DT_SINGLELINE, DT_VCENTER, FW_NORMAL, HALFTONE, HDC, HFONT, HGDIOBJ, PEN_STYLE, SRCCOPY,
    STRETCH_BLT_MODE, TRANSPARENT,
};
use windows::Win32::System::SystemServices::SS_ETCHEDHORZ;
use windows::Win32::UI::Controls::{
    GetComboBoxInfo, SetWindowTheme, CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDRF_DODEFAULT,
    CDRF_NEWFONT, CDRF_NOTIFYITEMDRAW, COMBOBOXINFO, DRAWITEMSTRUCT, LVHITTESTINFO, LVS_REPORT,
    LVS_SHOWSELALWAYS, LVS_SINGLESEL, NMLVCUSTOMDRAW, NM_CUSTOMDRAW, ODA_FOCUS, ODS_COMBOBOXEDIT,
    ODS_DISABLED, ODS_FOCUS, ODS_HOTLIGHT, ODS_SELECTED, PBS_SMOOTH, WM_MOUSELEAVE,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    IsWindowEnabled, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, GetParent, GetPropW, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW,
    GetWindowTextW, RemovePropW, SendMessageW, SetPropW, SetWindowLongPtrW, SetWindowPos,
    BS_AUTOCHECKBOX, BS_AUTORADIOBUTTON, BS_OWNERDRAW, CBS_DROPDOWNLIST, CBS_HASSTRINGS,
    CBS_OWNERDRAWFIXED, ES_AUTOHSCROLL, GWL_EXSTYLE, GWL_STYLE, HMENU, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_CANCELMODE, WM_DRAWITEM, WM_ENABLE, WM_ERASEBKGND, WM_GETFONT, WM_KILLFOCUS, WM_MOUSEMOVE,
    WM_NCDESTROY, WM_NCPAINT, WM_NOTIFY, WM_PAINT, WM_SETFOCUS, WM_SHOWWINDOW, WM_SIZE,
    WM_THEMECHANGED, WS_BORDER, WS_CHILD, WS_EX_CLIENTEDGE, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use super::theme::{Palette, ThemeContext};

pub const MICROSOFT_YAHEI_UI: &str = "Microsoft YaHei UI";

const BUTTON_HOT_PROPERTY: PCWSTR = w!("LetRecovery.PE.InnoButton.Hot");
const OWNER_DRAW_BUTTON_SUBCLASS_ID: usize = 0x5045_4254;
const ROUNDED_CONTROL_SUBCLASS_ID: usize = 0x5045_5243;
const COMBO_PARENT_SUBCLASS_BASE: usize = 0x5045_4300;
const LIST_PARENT_SUBCLASS_BASE: usize = 0x5045_4c00;
const COMBO_HOT_ITEM_PROPERTY: PCWSTR = w!("LetRecovery.PE.Combo.HotItem");
const COMBO_TRACKING_PROPERTY: PCWSTR = w!("LetRecovery.PE.Combo.Tracking");
const LIST_HOT_ITEM_PROPERTY: PCWSTR = w!("LetRecovery.PE.List.HotItem");
const LIST_TRACKING_PROPERTY: PCWSTR = w!("LetRecovery.PE.List.Tracking");
const LIST_HOVER_SUBCLASS_ID: usize = 0x5045_4c48;

const fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    COLORREF((red as u32) | ((green as u32) << 8) | ((blue as u32) << 16))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UiFontRole {
    #[default]
    Body,
    Heading,
}

/// Roles that use the restrained 4-6 logical pixel rounding from Inno's modern style.
///
/// The actual painter is introduced with the PE window shell. Keeping the role and
/// anti-aliasing contract here prevents individual pages from inventing inconsistent
/// radii or falling back to jagged GDI `RoundRect` output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoundedControlRole {
    Button,
    ChoiceField,
    Popup,
    Progress,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoundedControlSpec {
    pub radius: i32,
    pub supersample: u32,
}

pub fn rounded_control_spec(role: RoundedControlRole, dpi: u32) -> RoundedControlSpec {
    let logical_radius = match role {
        RoundedControlRole::ChoiceField | RoundedControlRole::Popup => 5,
        RoundedControlRole::Button | RoundedControlRole::Progress => 4,
    };
    RoundedControlSpec {
        radius: scale_for_dpi(logical_radius, dpi).max(1),
        supersample: 4,
    }
}

/// Paints the complete Inno-style PE progress control in one off-screen composition.
///
/// Completion and failure keep the same green fill; terminal state is communicated by text, not
/// a surprise colour change. The complete bitmap is blitted once, preventing track/fill flashes
/// when worker updates arrive several times per second.
///
/// # Safety
///
/// `dc` must be a valid writable device context for the entire call, and `rect` must describe a
/// drawable region in that context. The caller must keep the target window and its DC alive while
/// the temporary GDI objects are selected and the final bitmap is copied.
pub unsafe fn draw_progress(dc: HDC, rect: RECT, percent: u8, palette: Palette) {
    let width = (rect.right - rect.left).max(0);
    let height = (rect.bottom - rect.top).max(0);
    if width == 0 || height == 0 {
        return;
    }
    let memory_dc = CreateCompatibleDC(dc);
    let bitmap = CreateCompatibleBitmap(dc, width, height);
    if memory_dc.is_invalid() || bitmap.is_invalid() {
        if !memory_dc.is_invalid() {
            let _ = DeleteDC(memory_dc);
        }
        if !bitmap.is_invalid() {
            let _ = DeleteObject(bitmap);
        }
        return;
    }
    let old_bitmap = SelectObject(memory_dc, bitmap);
    let local = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };
    let radius = ((height * 5 + 8) / 16).clamp(2, (height / 2).max(2));
    fill_solid_rect(memory_dc, &local, palette.window);
    fill_round_rect_antialiased(
        memory_dc,
        local,
        radius,
        palette.edit,
        palette.border,
        palette.window,
    );

    let inner_width = (width - 2).max(0);
    let filled = inner_width.saturating_mul(i32::from(percent.min(100))) / 100;
    if filled > 0 {
        let fill = RECT {
            left: 1,
            top: 1,
            right: 1 + filled,
            bottom: height - 1,
        };
        fill_round_rect_antialiased(
            memory_dc,
            fill,
            radius.saturating_sub(1),
            palette.progress,
            palette.progress,
            palette.edit,
        );
    }
    let _ = BitBlt(
        dc, rect.left, rect.top, width, height, memory_dc, 0, 0, SRCCOPY,
    );
    let _ = SelectObject(memory_dc, old_bitmap);
    let _ = DeleteObject(bitmap);
    let _ = DeleteDC(memory_dc);
}

unsafe fn fill_solid_rect(dc: HDC, rect: &RECT, color: COLORREF) {
    let brush = CreateSolidBrush(color);
    let _ = FillRect(dc, rect, brush);
    let _ = DeleteObject(brush);
}

unsafe fn fill_round_rect_antialiased(
    dc: HDC,
    rect: RECT,
    radius: i32,
    fill: COLORREF,
    border: COLORREF,
    background: COLORREF,
) -> bool {
    const SCALE: i32 = 4;
    let width = (rect.right - rect.left).max(0);
    let height = (rect.bottom - rect.top).max(0);
    if width == 0 || height == 0 {
        return false;
    }
    let Some(high_width) = width.checked_mul(SCALE) else {
        return false;
    };
    let Some(high_height) = height.checked_mul(SCALE) else {
        return false;
    };
    let radius = radius.max(0).min(width.min(height) / 2);
    let Some(diameter) = radius
        .checked_mul(2)
        .and_then(|value| value.checked_mul(SCALE))
    else {
        return false;
    };
    let memory_dc = CreateCompatibleDC(dc);
    let bitmap = CreateCompatibleBitmap(dc, high_width, high_height);
    if memory_dc.is_invalid() || bitmap.is_invalid() {
        if !memory_dc.is_invalid() {
            let _ = DeleteDC(memory_dc);
        }
        if !bitmap.is_invalid() {
            let _ = DeleteObject(bitmap);
        }
        return false;
    }
    let old_bitmap = SelectObject(memory_dc, bitmap);
    let high_rect = RECT {
        left: 0,
        top: 0,
        right: high_width,
        bottom: high_height,
    };
    fill_solid_rect(memory_dc, &high_rect, background);
    let brush = CreateSolidBrush(fill);
    let pen = CreatePen(PEN_STYLE(0), SCALE, border);
    let old_brush = SelectObject(memory_dc, brush);
    let old_pen = SelectObject(memory_dc, pen);
    let inset = SCALE / 2;
    let _ = RoundRect(
        memory_dc,
        inset,
        inset,
        high_width - inset,
        high_height - inset,
        diameter,
        diameter,
    );
    let _ = SelectObject(memory_dc, old_pen);
    let _ = SelectObject(memory_dc, old_brush);
    let _ = DeleteObject(pen);
    let _ = DeleteObject(brush);
    let mut old_brush_origin = POINT::default();
    let has_old_brush_origin = GetBrushOrgEx(dc, &mut old_brush_origin).as_bool();
    let old_stretch_mode = SetStretchBltMode(dc, HALFTONE);
    let _ = SetBrushOrgEx(dc, 0, 0, None);
    let _ = StretchBlt(
        dc,
        rect.left,
        rect.top,
        width,
        height,
        memory_dc,
        0,
        0,
        high_width,
        high_height,
        SRCCOPY,
    );
    if old_stretch_mode != 0 {
        let _ = SetStretchBltMode(dc, STRETCH_BLT_MODE(old_stretch_mode));
    }
    if has_old_brush_origin {
        let _ = SetBrushOrgEx(dc, old_brush_origin.x, old_brush_origin.y, None);
    }
    let _ = SelectObject(memory_dc, old_bitmap);
    let _ = DeleteObject(bitmap);
    let _ = DeleteDC(memory_dc);
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RoundedControlFrameGeometry {
    radius: i32,
    arc_band: i32,
    side_band: i32,
}

fn rounded_control_frame_geometry(
    width: i32,
    height: i32,
    dpi: u32,
) -> Option<RoundedControlFrameGeometry> {
    if width <= 0 || height <= 0 {
        return None;
    }
    let radius = scale_for_dpi(5, dpi)
        .max(2)
        .min((width / 2).max(1))
        .min((height / 2).max(1));
    Some(RoundedControlFrameGeometry {
        radius,
        arc_band: (radius + scale_for_dpi(1, dpi).max(1)).min((height / 2).max(1)),
        side_band: scale_for_dpi(1, dpi).max(1).min(width),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RoundedControlFrameVisual {
    interior: COLORREF,
    border: COLORREF,
    exterior: COLORREF,
}

fn rounded_control_frame_visual(
    palette: Palette,
    enabled: bool,
    _focused: bool,
) -> RoundedControlFrameVisual {
    RoundedControlFrameVisual {
        interior: palette.edit,
        border: if enabled {
            palette.border
        } else {
            palette.separator
        },
        exterior: palette.window,
    }
}

/// Copies only the antialiased corner and one-pixel outline bands. The underlying HWND remains a
/// rectangle: ListView/ListBox scrollbars, ComboBox arrows and all native hit testing stay intact.
unsafe fn draw_antialiased_control_frame(
    dc: HDC,
    rect: RECT,
    geometry: RoundedControlFrameGeometry,
    visual: RoundedControlFrameVisual,
) {
    let width = (rect.right - rect.left).max(0);
    let height = (rect.bottom - rect.top).max(0);
    if width == 0 || height == 0 {
        return;
    }
    let surface_dc = CreateCompatibleDC(dc);
    if surface_dc.is_invalid() {
        return;
    }
    let surface_bitmap = CreateCompatibleBitmap(dc, width, height);
    if surface_bitmap.is_invalid() {
        let _ = DeleteDC(surface_dc);
        return;
    }
    let old_bitmap = SelectObject(surface_dc, surface_bitmap);
    let local = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };
    if fill_round_rect_antialiased(
        surface_dc,
        local,
        geometry.radius,
        visual.interior,
        visual.border,
        visual.exterior,
    ) {
        let arc = geometry.arc_band.min(height).min(width);
        let side = geometry.side_band.min(width);
        let edge = geometry.side_band.min(height);
        let _ = BitBlt(
            dc, rect.left, rect.top, width, edge, surface_dc, 0, 0, SRCCOPY,
        );
        let _ = BitBlt(
            dc,
            rect.left,
            rect.bottom - edge,
            width,
            edge,
            surface_dc,
            0,
            height - edge,
            SRCCOPY,
        );
        for (destination_x, destination_y, source_x, source_y) in [
            (rect.left, rect.top, 0, 0),
            (rect.right - arc, rect.top, width - arc, 0),
            (rect.left, rect.bottom - arc, 0, height - arc),
            (
                rect.right - arc,
                rect.bottom - arc,
                width - arc,
                height - arc,
            ),
        ] {
            let _ = BitBlt(
                dc,
                destination_x,
                destination_y,
                arc,
                arc,
                surface_dc,
                source_x,
                source_y,
                SRCCOPY,
            );
        }
        let middle_height = (height - arc.saturating_mul(2)).max(0);
        if middle_height > 0 {
            let _ = BitBlt(
                dc,
                rect.left,
                rect.top + arc,
                side,
                middle_height,
                surface_dc,
                0,
                arc,
                SRCCOPY,
            );
            if side < width {
                let _ = BitBlt(
                    dc,
                    rect.right - side,
                    rect.top + arc,
                    side,
                    middle_height,
                    surface_dc,
                    width - side,
                    arc,
                    SRCCOPY,
                );
            }
        }
    }
    let _ = SelectObject(surface_dc, old_bitmap);
    let _ = DeleteObject(surface_bitmap);
    let _ = DeleteDC(surface_dc);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ButtonState {
    pub hot: bool,
    pub pressed: bool,
    pub disabled: bool,
    pub primary: bool,
    pub focused: bool,
}

impl ButtonState {
    fn from_draw_item(item: &DRAWITEMSTRUCT, primary: bool) -> Self {
        Self {
            hot: item.itemState.0 & ODS_HOTLIGHT.0 != 0,
            pressed: item.itemState.0 & ODS_SELECTED.0 != 0,
            disabled: item.itemState.0 & ODS_DISABLED.0 != 0,
            primary,
            focused: item.itemState.0 & ODS_FOCUS.0 != 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ButtonVisual {
    pub fill: COLORREF,
    pub border: COLORREF,
    pub text: COLORREF,
}

pub const fn button_visual(palette: Palette, state: ButtonState) -> ButtonVisual {
    if state.disabled {
        return ButtonVisual {
            fill: if palette.dark {
                rgb(47, 47, 47)
            } else {
                rgb(249, 249, 249)
            },
            border: palette.border,
            text: palette.text_disabled,
        };
    }
    if state.primary {
        return ButtonVisual {
            fill: if state.pressed {
                if palette.dark {
                    rgb(39, 61, 71)
                } else {
                    rgb(0, 83, 160)
                }
            } else if state.hot {
                if palette.dark {
                    rgb(54, 79, 91)
                } else {
                    rgb(0, 103, 192)
                }
            } else {
                palette.accent_fill
            },
            border: palette.accent_border,
            text: if palette.dark {
                palette.text
            } else {
                rgb(255, 255, 255)
            },
        };
    }
    ButtonVisual {
        fill: if state.pressed {
            palette.button_pressed
        } else if state.hot {
            palette.button_hot
        } else {
            palette.button
        },
        // Focus never changes the outline. Mouse-down also assigns focus; a separate focus
        // rectangle would make a click appear as a transient double border.
        border: palette.border,
        text: palette.text,
    }
}

/// Draws a complete owner-drawn PE command button into one off-screen surface. The only font used
/// by callers is Microsoft YaHei UI from `create_ui_font`.
///
/// # Safety
///
/// `item` must originate from a live `WM_DRAWITEM` notification whose `hDC` and `hwndItem` remain
/// valid for the duration of this call. `font` must be a valid `HFONT` that is not deleted until
/// drawing completes.
pub unsafe fn draw_inno_button(
    item: &DRAWITEMSTRUCT,
    palette: Palette,
    primary: bool,
    font: HFONT,
    dpi: u32,
) {
    if item.itemAction.0 == ODA_FOCUS.0 {
        return;
    }
    let mut state = ButtonState::from_draw_item(item, primary);
    state.hot |= !GetPropW(item.hwndItem, BUTTON_HOT_PROPERTY).is_invalid();
    let visual = button_visual(palette, state);
    let width = (item.rcItem.right - item.rcItem.left).max(0);
    let height = (item.rcItem.bottom - item.rcItem.top).max(0);
    if width == 0 || height == 0 {
        return;
    }

    let memory_dc = CreateCompatibleDC(item.hDC);
    if !memory_dc.is_invalid() {
        let bitmap = CreateCompatibleBitmap(item.hDC, width, height);
        if !bitmap.is_invalid() {
            let old_bitmap = SelectObject(memory_dc, bitmap);
            draw_button_surface(
                memory_dc,
                RECT {
                    left: 0,
                    top: 0,
                    right: width,
                    bottom: height,
                },
                item.hwndItem,
                visual,
                font,
                dpi,
                palette.window,
            );
            let _ = BitBlt(
                item.hDC,
                item.rcItem.left,
                item.rcItem.top,
                width,
                height,
                memory_dc,
                0,
                0,
                SRCCOPY,
            );
            let _ = SelectObject(memory_dc, old_bitmap);
            let _ = DeleteObject(bitmap);
            let _ = DeleteDC(memory_dc);
            return;
        }
        let _ = DeleteDC(memory_dc);
    }

    draw_button_surface(
        item.hDC,
        item.rcItem,
        item.hwndItem,
        visual,
        font,
        dpi,
        palette.window,
    );
}

unsafe fn draw_button_surface(
    dc: HDC,
    rect: RECT,
    hwnd: HWND,
    visual: ButtonVisual,
    font: HFONT,
    dpi: u32,
    background: COLORREF,
) {
    fill_round_rect_antialiased(
        dc,
        rect,
        rounded_control_spec(RoundedControlRole::Button, dpi).radius,
        visual.fill,
        visual.border,
        background,
    );
    let length = GetWindowTextLengthW(hwnd).max(0) as usize;
    let mut text = vec![0u16; length + 1];
    let copied = GetWindowTextW(hwnd, &mut text).max(0) as usize;
    text.truncate(copied);
    let _ = SetBkMode(dc, TRANSPARENT);
    let _ = SetTextColor(dc, visual.text);
    let old_font = SelectObject(dc, font);
    let mut text_rect = rect;
    let _ = DrawTextW(
        dc,
        &mut text,
        &mut text_rect,
        DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
    );
    let _ = SelectObject(dc, old_font);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeControlKind {
    Button,
    Edit,
    ComboBox,
    List,
    CheckBox,
    RadioButton,
    Progress,
    Separator,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlFrameShape {
    Native,
    Straight,
    Rounded,
}

const fn control_frame_shape(kind: NativeControlKind) -> ControlFrameShape {
    match kind {
        NativeControlKind::Edit => ControlFrameShape::Straight,
        NativeControlKind::ComboBox | NativeControlKind::List => ControlFrameShape::Rounded,
        _ => ControlFrameShape::Native,
    }
}

fn recessed_edit_style_bits(style: isize, ex_style: isize) -> (isize, isize) {
    (
        style & !(WS_BORDER.0 as isize),
        ex_style | WS_EX_CLIENTEDGE.0 as isize,
    )
}

unsafe fn apply_recessed_edit_style(control: HWND) {
    let style = GetWindowLongPtrW(control, GWL_STYLE);
    let ex_style = GetWindowLongPtrW(control, GWL_EXSTYLE);
    let (style, ex_style) = recessed_edit_style_bits(style, ex_style);
    let _ = SetWindowLongPtrW(control, GWL_STYLE, style);
    let _ = SetWindowLongPtrW(control, GWL_EXSTYLE, ex_style);
    let _ = SetWindowPos(
        control,
        None,
        0,
        0,
        0,
        0,
        SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
    );
    let _ = InvalidateRect(control, None, true);
}

const fn list_view_base_colors(palette: Palette) -> [(u32, COLORREF); 3] {
    [
        (0x1001, palette.edit), // LVM_SETBKCOLOR
        (0x1026, palette.edit), // LVM_SETTEXTBKCOLOR
        (0x1024, palette.text), // LVM_SETTEXTCOLOR
    ]
}

/// Creates and themes a child control owned by `parent`.
///
/// # Safety
///
/// `parent` must be a live window owned by the calling UI thread, and `id` must obey that parent's
/// child-control identifier contract. The caller must destroy the returned child before destroying
/// resources, including fonts and theme state, that the control still uses.
pub unsafe fn create_control(
    parent: HWND,
    id: u16,
    kind: NativeControlKind,
    text: &str,
    theme: ThemeContext,
) -> windows::core::Result<HWND> {
    let (class_name, control_style, extended_style) = match kind {
        NativeControlKind::Button => (
            w!("BUTTON"),
            BS_OWNERDRAW as u32 | WS_TABSTOP.0,
            WINDOW_EX_STYLE(0),
        ),
        NativeControlKind::Edit => (
            w!("EDIT"),
            ES_AUTOHSCROLL as u32 | WS_TABSTOP.0,
            WS_EX_CLIENTEDGE,
        ),
        NativeControlKind::ComboBox => (
            w!("COMBOBOX"),
            CBS_DROPDOWNLIST as u32
                | CBS_HASSTRINGS as u32
                | CBS_OWNERDRAWFIXED as u32
                | WS_VSCROLL.0
                | WS_TABSTOP.0,
            WINDOW_EX_STYLE(0),
        ),
        NativeControlKind::List => (
            w!("SysListView32"),
            LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS | WS_BORDER.0,
            WINDOW_EX_STYLE(0),
        ),
        NativeControlKind::CheckBox => (
            w!("BUTTON"),
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP.0,
            WINDOW_EX_STYLE(0),
        ),
        NativeControlKind::RadioButton => (
            w!("BUTTON"),
            BS_AUTORADIOBUTTON as u32 | WS_TABSTOP.0,
            WINDOW_EX_STYLE(0),
        ),
        NativeControlKind::Progress => (w!("msctls_progress32"), PBS_SMOOTH, WINDOW_EX_STYLE(0)),
        NativeControlKind::Separator => (w!("STATIC"), SS_ETCHEDHORZ.0, WINDOW_EX_STYLE(0)),
    };
    let text = wide(text);
    let control = CreateWindowExW(
        extended_style,
        class_name,
        PCWSTR(text.as_ptr()),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | control_style),
        0,
        0,
        0,
        0,
        parent,
        HMENU(id as isize as *mut _),
        HINSTANCE::default(),
        None,
    )?;
    apply_theme(control, kind, theme.palette);
    Ok(control)
}

/// Applies the PE theme and installs the subclasses required by `kind`.
///
/// # Safety
///
/// `control` must be a live HWND of the class represented by `kind` and must belong to the calling
/// UI thread. Its parent must remain valid while owner/custom-draw messages are routed, and the
/// control must be destroyed normally so installed subclasses can release their window properties.
pub unsafe fn apply_theme(control: HWND, kind: NativeControlKind, palette: Palette) {
    let class = match (kind, palette.dark) {
        (NativeControlKind::Edit, true) => w!("DarkMode_CFD"),
        (_, true) => w!("DarkMode_Explorer"),
        _ => w!("Explorer"),
    };
    let _ = SetWindowTheme(control, class, PCWSTR::null());
    let _ = SendMessageW(control, WM_THEMECHANGED, WPARAM(0), LPARAM(0));

    if kind == NativeControlKind::Button {
        let _ = SetWindowSubclass(
            control,
            Some(owner_draw_button_proc),
            OWNER_DRAW_BUTTON_SUBCLASS_ID,
            0,
        );
    }
    if control_frame_shape(kind) == ControlFrameShape::Straight {
        apply_recessed_edit_style(control);
    }
    if control_frame_shape(kind) == ControlFrameShape::Rounded {
        let _ = SetWindowSubclass(
            control,
            Some(rounded_control_subclass),
            ROUNDED_CONTROL_SUBCLASS_ID,
            usize::from(palette.dark),
        );
        let _ = InvalidateRect(control, None, false);
    }

    if kind == NativeControlKind::ComboBox {
        let mut info = COMBOBOXINFO {
            cbSize: std::mem::size_of::<COMBOBOXINFO>() as u32,
            ..Default::default()
        };
        if GetComboBoxInfo(control, &mut info).is_ok() && !info.hwndList.0.is_null() {
            // The closed field and the owned ComboLBox are separate HWNDs. Explorer keeps the
            // popup dark without corrupting the CFD arrow/selected-text painting.
            let popup_class = if palette.dark {
                w!("DarkMode_Explorer")
            } else {
                w!("Explorer")
            };
            let _ = SetWindowTheme(info.hwndList, popup_class, PCWSTR::null());
            let _ = SendMessageW(info.hwndList, WM_THEMECHANGED, WPARAM(0), LPARAM(0));
            let _ = SetWindowSubclass(
                info.hwndList,
                Some(rounded_control_subclass),
                ROUNDED_CONTROL_SUBCLASS_ID,
                usize::from(palette.dark),
            );
            let _ = SetWindowSubclass(
                info.hwndList,
                Some(combo_popup_subclass),
                ROUNDED_CONTROL_SUBCLASS_ID ^ 0x10,
                usize::from(palette.dark),
            );
            let _ = InvalidateRect(info.hwndList, None, false);
        }
        if let Ok(parent) = GetParent(control) {
            let subclass_id = COMBO_PARENT_SUBCLASS_BASE ^ control.0 as usize;
            let dark_flag = usize::from(palette.dark) << (usize::BITS - 1);
            let _ = SetWindowSubclass(
                parent,
                Some(combo_parent_subclass),
                subclass_id,
                control.0 as usize | dark_flag,
            );
        }
    }
    if kind == NativeControlKind::List {
        for (message, color) in list_view_base_colors(palette) {
            let _ = SendMessageW(control, message, WPARAM(0), LPARAM(color.0 as isize));
        }
        let _ = InvalidateRect(control, None, false);
        let _ = SetWindowSubclass(
            control,
            Some(list_hover_subclass),
            LIST_HOVER_SUBCLASS_ID,
            usize::from(palette.dark),
        );
        if let Ok(parent) = GetParent(control) {
            let subclass_id = LIST_PARENT_SUBCLASS_BASE ^ control.0 as usize;
            let dark_flag = usize::from(palette.dark) << (usize::BITS - 1);
            let _ = SetWindowSubclass(
                parent,
                Some(list_parent_subclass),
                subclass_id,
                control.0 as usize | dark_flag,
            );
        }
    }
}

const fn palette_from_reference(reference_data: usize) -> Palette {
    if reference_data != 0 {
        Palette::DARK
    } else {
        Palette::LIGHT
    }
}

unsafe extern "system" fn rounded_control_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    match message {
        WM_PAINT | WM_NCPAINT => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            paint_rounded_control_frame(hwnd, palette_from_reference(reference_data));
            result
        }
        WM_ENABLE | WM_SETFOCUS | WM_KILLFOCUS | WM_SIZE | WM_THEMECHANGED => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            let _ = InvalidateRect(hwnd, None, false);
            result
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(
                hwnd,
                Some(rounded_control_subclass),
                ROUNDED_CONTROL_SUBCLASS_ID,
            );
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

unsafe fn paint_rounded_control_frame(hwnd: HWND, palette: Palette) {
    let mut window = RECT::default();
    if GetWindowRect(hwnd, &mut window).is_err() {
        return;
    }
    let width = (window.right - window.left).max(0);
    let height = (window.bottom - window.top).max(0);
    let Some(geometry) = rounded_control_frame_geometry(width, height, GetDpiForWindow(hwnd))
    else {
        return;
    };
    let dc = GetWindowDC(hwnd);
    if dc.is_invalid() {
        return;
    }
    let visual = rounded_control_frame_visual(palette, IsWindowEnabled(hwnd).as_bool(), false);
    draw_antialiased_control_frame(
        dc,
        RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        },
        geometry,
        visual,
    );
    let _ = ReleaseDC(hwnd, dc);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SelectionVisual {
    fill: COLORREF,
    text: COLORREF,
}

const fn primary_hot_fill(palette: Palette) -> COLORREF {
    if palette.dark {
        rgb(54, 79, 91)
    } else {
        rgb(0, 103, 192)
    }
}

const fn selection_visual(palette: Palette, selected: bool, hot: bool) -> SelectionVisual {
    if hot {
        SelectionVisual {
            fill: primary_hot_fill(palette),
            text: if palette.dark {
                palette.text
            } else {
                rgb(255, 255, 255)
            },
        }
    } else if selected {
        SelectionVisual {
            fill: palette.accent_fill,
            text: if palette.dark {
                palette.text
            } else {
                rgb(255, 255, 255)
            },
        }
    } else {
        SelectionVisual {
            fill: palette.edit,
            text: palette.text,
        }
    }
}

unsafe extern "system" fn combo_parent_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    if message == WM_DRAWITEM && lparam.0 != 0 {
        let item = &*(lparam.0 as *const DRAWITEMSTRUCT);
        let dark_flag = 1usize << (usize::BITS - 1);
        let combo = HWND((reference_data & !dark_flag) as *mut _);
        if item.hwndItem == combo {
            let palette = palette_from_reference(usize::from(reference_data & dark_flag != 0));
            draw_combo_item(combo, item, palette);
            return LRESULT(1);
        }
    }
    if message == WM_NCDESTROY {
        let _ = RemoveWindowSubclass(hwnd, Some(combo_parent_subclass), subclass_id);
    }
    DefSubclassProc(hwnd, message, wparam, lparam)
}

unsafe fn draw_combo_item(combo: HWND, item: &DRAWITEMSTRUCT, palette: Palette) {
    const CB_GETCURSEL: u32 = 0x0147;
    const CB_GETLBTEXT: u32 = 0x0148;
    const CB_GETLBTEXTLEN: u32 = 0x0149;
    let closed_field = item.itemState.0 & ODS_COMBOBOXEDIT.0 != 0;
    let mut hot_index = None;
    if !closed_field {
        let mut info = COMBOBOXINFO {
            cbSize: std::mem::size_of::<COMBOBOXINFO>() as u32,
            ..Default::default()
        };
        if GetComboBoxInfo(combo, &mut info).is_ok() && !info.hwndList.0.is_null() {
            hot_index = property_item_index(info.hwndList, COMBO_HOT_ITEM_PROPERTY);
        }
    }
    let selected = !closed_field && item.itemState.0 & ODS_SELECTED.0 != 0;
    let hot = !closed_field && hot_index == Some(item.itemID as usize);
    let visual = if item.itemState.0 & ODS_DISABLED.0 != 0 {
        SelectionVisual {
            fill: palette.edit,
            text: palette.text_disabled,
        }
    } else if closed_field {
        SelectionVisual {
            fill: palette.edit,
            text: palette.text,
        }
    } else {
        selection_visual(palette, selected, hot)
    };
    fill_solid_rect(item.hDC, &item.rcItem, visual.fill);
    if closed_field {
        // PE images carry different USER32/UxTheme generations. Keep the stock ComboBox popup,
        // selection, keyboard and accessibility semantics, but replace only the closed chevron
        // with mirrored high-resolution strokes so one arm cannot be truncated by integer joins.
        draw_combo_closed_chevron(combo, palette);
    }

    let mut index = if item.itemID == u32::MAX {
        SendMessageW(combo, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0
    } else {
        item.itemID as isize
    };
    if index < 0 {
        return;
    }
    let length = SendMessageW(combo, CB_GETLBTEXTLEN, WPARAM(index as usize), LPARAM(0)).0;
    if length < 0 {
        return;
    }
    let mut text = vec![0u16; length as usize + 1];
    index = SendMessageW(
        combo,
        CB_GETLBTEXT,
        WPARAM(index as usize),
        LPARAM(text.as_mut_ptr() as isize),
    )
    .0;
    if index < 0 {
        return;
    }
    text.truncate(index as usize);
    let font = SendMessageW(combo, WM_GETFONT, WPARAM(0), LPARAM(0)).0;
    let old_font = (font != 0).then(|| SelectObject(item.hDC, HGDIOBJ(font as *mut _)));
    let _ = SetBkMode(item.hDC, TRANSPARENT);
    let _ = SetTextColor(item.hDC, visual.text);
    let inset = scale_for_dpi(7, GetDpiForWindow(combo)).max(4);
    let mut text_rect = item.rcItem;
    text_rect.left += inset;
    text_rect.right -= inset.min((text_rect.right - text_rect.left).max(0));
    let _ = DrawTextW(
        item.hDC,
        &mut text,
        &mut text_rect,
        DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
    );
    if let Some(old_font) = old_font {
        let _ = SelectObject(item.hDC, old_font);
    }
}

unsafe fn draw_combo_closed_chevron(combo: HWND, palette: Palette) {
    let mut window = RECT::default();
    let mut info = COMBOBOXINFO {
        cbSize: std::mem::size_of::<COMBOBOXINFO>() as u32,
        ..Default::default()
    };
    if GetWindowRect(combo, &mut window).is_err() || GetComboBoxInfo(combo, &mut info).is_err() {
        return;
    }
    let width = (window.right - window.left).max(0);
    let height = (window.bottom - window.top).max(0);
    if width <= 0 || height <= 0 {
        return;
    }
    let dpi = GetDpiForWindow(combo).max(96);
    let arrow_width = (info.rcButton.right - info.rcButton.left)
        .abs()
        .max(scale_for_dpi(17, dpi))
        .min((width / 2).max(1));
    let button = RECT {
        left: width - arrow_width,
        top: 1,
        right: width - 1,
        bottom: height - 1,
    };
    let button_width = (button.right - button.left).max(0);
    let button_height = (button.bottom - button.top).max(0);
    if button_width <= 0 || button_height <= 0 {
        return;
    }

    let dc = GetWindowDC(combo);
    if dc.is_invalid() {
        return;
    }
    const SUPERSAMPLE: i32 = 4;
    let memory_dc = CreateCompatibleDC(dc);
    let bitmap = CreateCompatibleBitmap(
        dc,
        button_width.saturating_mul(SUPERSAMPLE),
        button_height.saturating_mul(SUPERSAMPLE),
    );
    if memory_dc.is_invalid() || bitmap.is_invalid() {
        if !memory_dc.is_invalid() {
            let _ = DeleteDC(memory_dc);
        }
        if !bitmap.is_invalid() {
            let _ = DeleteObject(bitmap);
        }
        let _ = ReleaseDC(combo, dc);
        return;
    }
    let old_bitmap = SelectObject(memory_dc, bitmap);
    let background = CreateSolidBrush(palette.edit);
    if !background.is_invalid() {
        let high_rect = RECT {
            left: 0,
            top: 0,
            right: button_width.saturating_mul(SUPERSAMPLE),
            bottom: button_height.saturating_mul(SUPERSAMPLE),
        };
        let _ = FillRect(memory_dc, &high_rect, background);
        let _ = DeleteObject(background);
    }

    let colour = if IsWindowEnabled(combo).as_bool() {
        palette.text_secondary
    } else {
        palette.text_disabled
    };
    let pen = CreatePen(PEN_STYLE(0), SUPERSAMPLE, colour);
    if !pen.is_invalid() {
        let old_pen = SelectObject(memory_dc, pen);
        let center_x = button_width.saturating_mul(SUPERSAMPLE) / 2;
        let center_y = button_height.saturating_mul(SUPERSAMPLE) / 2;
        let half = scale_for_dpi(3, dpi).max(2).saturating_mul(SUPERSAMPLE);
        let rise = scale_for_dpi(1, dpi).max(1).saturating_mul(SUPERSAMPLE);
        let drop = scale_for_dpi(2, dpi).max(1).saturating_mul(SUPERSAMPLE);
        let _ = MoveToEx(memory_dc, center_x - half, center_y - rise, None);
        let _ = LineTo(memory_dc, center_x, center_y + drop);
        // Start the second arm independently at the mirrored endpoint. This avoids a GDI join
        // pixel making the slash shorter than the backslash at 125%-200% DPI.
        let _ = MoveToEx(memory_dc, center_x + half, center_y - rise, None);
        let _ = LineTo(memory_dc, center_x, center_y + drop);
        let _ = SelectObject(memory_dc, old_pen);
        let _ = DeleteObject(pen);
    }
    let _ = SetStretchBltMode(dc, HALFTONE);
    let _ = StretchBlt(
        dc,
        button.left,
        button.top,
        button_width,
        button_height,
        memory_dc,
        0,
        0,
        button_width.saturating_mul(SUPERSAMPLE),
        button_height.saturating_mul(SUPERSAMPLE),
        SRCCOPY,
    );
    let _ = SelectObject(memory_dc, old_bitmap);
    let _ = DeleteObject(bitmap);
    let _ = DeleteDC(memory_dc);
    let _ = ReleaseDC(combo, dc);
}

unsafe extern "system" fn combo_popup_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass_id: usize,
    _reference_data: usize,
) -> LRESULT {
    match message {
        WM_MOUSEMOVE => {
            update_combo_hot_item(hwnd, lparam);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_MOUSELEAVE | WM_CANCELMODE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            clear_combo_hot_item(hwnd);
            result
        }
        WM_SHOWWINDOW => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if wparam.0 == 0 {
                clear_combo_hot_item(hwnd);
            }
            result
        }
        WM_NCDESTROY => {
            clear_combo_hot_item(hwnd);
            let _ = RemoveWindowSubclass(hwnd, Some(combo_popup_subclass), subclass_id);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

unsafe fn update_combo_hot_item(hwnd: HWND, lparam: LPARAM) {
    const LB_ITEMFROMPOINT: u32 = 0x01a9;
    let packed = SendMessageW(hwnd, LB_ITEMFROMPOINT, WPARAM(0), lparam).0 as u32;
    let next = (packed >> 16 == 0).then_some((packed & 0xffff) as usize);
    let previous = property_item_index(hwnd, COMBO_HOT_ITEM_PROPERTY);
    ensure_mouse_leave_tracking(hwnd, COMBO_TRACKING_PROPERTY);
    if next == previous {
        return;
    }
    set_property_item_index(hwnd, COMBO_HOT_ITEM_PROPERTY, next);
    let _ = InvalidateRect(hwnd, None, false);
}

unsafe fn clear_combo_hot_item(hwnd: HWND) {
    let changed = RemovePropW(hwnd, COMBO_HOT_ITEM_PROPERTY).is_ok_and(|value| !value.is_invalid());
    let _ = RemovePropW(hwnd, COMBO_TRACKING_PROPERTY);
    if changed {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

unsafe extern "system" fn list_parent_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    if message == WM_NOTIFY && lparam.0 != 0 {
        let draw = &mut *(lparam.0 as *mut NMLVCUSTOMDRAW);
        let dark_flag = 1usize << (usize::BITS - 1);
        let list = HWND((reference_data & !dark_flag) as *mut _);
        if draw.nmcd.hdr.hwndFrom == list && draw.nmcd.hdr.code == NM_CUSTOMDRAW {
            if draw.nmcd.dwDrawStage == CDDS_PREPAINT {
                return LRESULT(CDRF_NOTIFYITEMDRAW as isize);
            }
            if draw.nmcd.dwDrawStage == CDDS_ITEMPREPAINT {
                const CDIS_SELECTED: u32 = 0x0001;
                const CDIS_HOT: u32 = 0x0040;
                const LVM_GETITEMSTATE: u32 = 0x102c;
                const LVIS_SELECTED: isize = 0x0002;
                draw.nmcd.uItemState.0 &= !(CDIS_SELECTED | CDIS_HOT);
                let item_state = SendMessageW(
                    list,
                    LVM_GETITEMSTATE,
                    WPARAM(draw.nmcd.dwItemSpec),
                    LPARAM(LVIS_SELECTED),
                )
                .0;
                let selected = item_state & LVIS_SELECTED != 0;
                let hot =
                    property_item_index(list, LIST_HOT_ITEM_PROPERTY) == Some(draw.nmcd.dwItemSpec);
                let palette = palette_from_reference(usize::from(reference_data & dark_flag != 0));
                let visual = selection_visual(palette, selected, hot);
                draw.clrText = visual.text;
                draw.clrTextBk = visual.fill;
                return LRESULT(CDRF_NEWFONT as isize);
            }
            return LRESULT(CDRF_DODEFAULT as isize);
        }
    }
    if message == WM_NCDESTROY {
        let _ = RemoveWindowSubclass(hwnd, Some(list_parent_subclass), subclass_id);
    }
    DefSubclassProc(hwnd, message, wparam, lparam)
}

unsafe extern "system" fn list_hover_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass_id: usize,
    _reference_data: usize,
) -> LRESULT {
    match message {
        WM_MOUSEMOVE => {
            update_list_hot_item(hwnd, lparam);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_MOUSELEAVE | WM_CANCELMODE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            clear_list_hot_item(hwnd);
            result
        }
        WM_SHOWWINDOW => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if wparam.0 == 0 {
                clear_list_hot_item(hwnd);
            }
            result
        }
        WM_NCDESTROY => {
            clear_list_hot_item(hwnd);
            let _ = RemoveWindowSubclass(hwnd, Some(list_hover_subclass), subclass_id);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

unsafe fn update_list_hot_item(hwnd: HWND, lparam: LPARAM) {
    const LVM_HITTEST: u32 = 0x1012;
    let packed = lparam.0 as u32;
    let mut hit = LVHITTESTINFO {
        pt: POINT {
            x: (packed as u16 as i16) as i32,
            y: ((packed >> 16) as u16 as i16) as i32,
        },
        ..Default::default()
    };
    let index = SendMessageW(
        hwnd,
        LVM_HITTEST,
        WPARAM(0),
        LPARAM((&mut hit as *mut LVHITTESTINFO) as isize),
    )
    .0;
    let next = (index >= 0).then_some(index as usize);
    let previous = property_item_index(hwnd, LIST_HOT_ITEM_PROPERTY);
    ensure_mouse_leave_tracking(hwnd, LIST_TRACKING_PROPERTY);
    if next == previous {
        return;
    }
    set_property_item_index(hwnd, LIST_HOT_ITEM_PROPERTY, next);
    let _ = InvalidateRect(hwnd, None, false);
}

unsafe fn clear_list_hot_item(hwnd: HWND) {
    let changed = RemovePropW(hwnd, LIST_HOT_ITEM_PROPERTY).is_ok_and(|value| !value.is_invalid());
    let _ = RemovePropW(hwnd, LIST_TRACKING_PROPERTY);
    if changed {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

unsafe fn property_item_index(hwnd: HWND, property: PCWSTR) -> Option<usize> {
    let value = GetPropW(hwnd, property);
    (!value.is_invalid()).then_some(value.0 as usize - 1)
}

unsafe fn set_property_item_index(hwnd: HWND, property: PCWSTR, index: Option<usize>) {
    let _ = RemovePropW(hwnd, property);
    if let Some(index) = index {
        let _ = SetPropW(
            hwnd,
            property,
            HANDLE((index + 1) as *mut core::ffi::c_void),
        );
    }
}

unsafe fn ensure_mouse_leave_tracking(hwnd: HWND, property: PCWSTR) {
    if !GetPropW(hwnd, property).is_invalid() {
        return;
    }
    let mut tracking = TRACKMOUSEEVENT {
        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    if TrackMouseEvent(&mut tracking).is_ok() {
        let _ = SetPropW(hwnd, property, HANDLE(std::ptr::dangling_mut()));
    }
}

unsafe extern "system" fn owner_draw_button_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    _reference_data: usize,
) -> LRESULT {
    match message {
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE => {
            let already_hot = !GetPropW(hwnd, BUTTON_HOT_PROPERTY).is_invalid();
            if !already_hot {
                let mut tracking = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                if TrackMouseEvent(&mut tracking).is_ok()
                    && SetPropW(hwnd, BUTTON_HOT_PROPERTY, HANDLE(std::ptr::dangling_mut())).is_ok()
                {
                    let _ = InvalidateRect(hwnd, None, false);
                } else {
                    clear_button_hot(hwnd);
                }
            }
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_MOUSELEAVE | WM_CANCELMODE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            clear_button_hot(hwnd);
            result
        }
        WM_SHOWWINDOW => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if wparam.0 == 0 {
                clear_button_hot(hwnd);
            }
            result
        }
        WM_ENABLE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if wparam.0 == 0 {
                clear_button_hot(hwnd);
            } else {
                let _ = InvalidateRect(hwnd, None, false);
            }
            result
        }
        WM_NCDESTROY => {
            clear_button_hot(hwnd);
            let _ = RemoveWindowSubclass(
                hwnd,
                Some(owner_draw_button_proc),
                OWNER_DRAW_BUTTON_SUBCLASS_ID,
            );
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

unsafe fn clear_button_hot(hwnd: HWND) {
    if RemovePropW(hwnd, BUTTON_HOT_PROPERTY).is_ok_and(|value| !value.is_invalid()) {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

/// Creates the single UI font family used by the PE client. Callers own the returned font.
///
/// # Safety
///
/// The caller must delete the returned non-null `HFONT` only after it is no longer selected into
/// any device context and no live control refers to it.
pub unsafe fn create_ui_font(dpi: u32, point_size: i32) -> HFONT {
    create_ui_font_for_role(dpi, point_size, UiFontRole::Body)
}

/// Creates the shared PE UI font for the requested semantic role.
///
/// # Safety
///
/// The caller must delete the returned non-null `HFONT` only after it is no longer selected into
/// any device context and no live control refers to it.
pub unsafe fn create_ui_font_for_role(dpi: u32, point_size: i32, role: UiFontRole) -> HFONT {
    let face = wide(MICROSOFT_YAHEI_UI);
    let pixel_height = -((point_size.max(1) * dpi.max(1) as i32 + 36) / 72);
    let weight = match role {
        UiFontRole::Body => FW_NORMAL.0 as i32,
        UiFontRole::Heading => 600,
    };
    CreateFontW(
        pixel_height,
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

/// Measures a command label using the actual Microsoft YaHei UI font selected by the caller.
/// The result includes the restrained Inno horizontal padding and is capped by the current
/// viewport so long translations cannot force neighbouring buttons to overlap.
///
/// # Safety
///
/// `owner` must be a valid window handle for the duration of the call, and `font` must be a
/// valid GDI font handle. The caller remains responsible for both handles and must not destroy
/// them while the temporary device context is in use.
pub unsafe fn measured_button_width(
    owner: HWND,
    font: HFONT,
    text: &str,
    dpi: u32,
    maximum: i32,
) -> i32 {
    let minimum = scale_for_dpi(75, dpi);
    let maximum = maximum.max(1);
    let dc = GetWindowDC(owner);
    if dc.0.is_null() {
        return minimum.min(maximum);
    }
    let previous = SelectObject(dc, font);
    let text = wide(text);
    let mut size = SIZE::default();
    let measured =
        GetTextExtentPoint32W(dc, &text[..text.len().saturating_sub(1)], &mut size).as_bool();
    let _ = SelectObject(dc, previous);
    let _ = ReleaseDC(owner, dc);
    let width = if measured {
        size.cx.saturating_add(scale_for_dpi(24, dpi))
    } else {
        minimum
    };
    width.max(minimum.min(maximum)).min(maximum)
}

pub fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

fn scale_for_dpi(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_ui::theme::Palette;

    #[test]
    fn disabled_button_never_uses_primary_border_or_text() {
        let visual = button_visual(
            Palette::DARK,
            ButtonState {
                disabled: true,
                primary: true,
                ..Default::default()
            },
        );
        assert_eq!(visual.border, Palette::DARK.border);
        assert_eq!(visual.text, Palette::DARK.text_disabled);
    }

    #[test]
    fn primary_button_uses_the_inno_accent_border() {
        let visual = button_visual(
            Palette::DARK,
            ButtonState {
                primary: true,
                ..Default::default()
            },
        );
        assert_eq!(visual.fill, Palette::DARK.accent_fill);
        assert_eq!(visual.border, Palette::DARK.accent_border);
    }

    #[test]
    fn focus_does_not_add_a_second_button_or_choice_outline() {
        let normal = button_visual(Palette::DARK, ButtonState::default());
        let focused = button_visual(
            Palette::DARK,
            ButtonState {
                focused: true,
                ..Default::default()
            },
        );
        assert_eq!(focused, normal);

        let normal_field = rounded_control_frame_visual(Palette::DARK, true, false);
        let focused_field = rounded_control_frame_visual(Palette::DARK, true, true);
        assert_eq!(focused_field, normal_field);
    }

    #[test]
    fn rounded_roles_keep_four_to_six_logical_pixels_and_four_x_antialiasing() {
        assert_eq!(
            rounded_control_spec(RoundedControlRole::Button, 96),
            RoundedControlSpec {
                radius: 4,
                supersample: 4
            }
        );
        assert_eq!(
            rounded_control_spec(RoundedControlRole::Popup, 192),
            RoundedControlSpec {
                radius: 10,
                supersample: 4
            }
        );
    }

    #[test]
    fn combo_and_list_frame_use_five_logical_pixels_without_clipping_content() {
        assert_eq!(
            rounded_control_frame_geometry(200, 32, 96),
            Some(RoundedControlFrameGeometry {
                radius: 5,
                arc_band: 6,
                side_band: 1,
            })
        );
        assert_eq!(
            rounded_control_frame_geometry(400, 64, 192),
            Some(RoundedControlFrameGeometry {
                radius: 10,
                arc_band: 12,
                side_band: 2,
            })
        );
        assert_eq!(rounded_control_frame_geometry(0, 32, 96), None);
    }

    #[test]
    fn edit_is_straight_while_combo_and_list_are_rounded() {
        assert_eq!(
            control_frame_shape(NativeControlKind::Edit),
            ControlFrameShape::Straight
        );
        assert_eq!(
            control_frame_shape(NativeControlKind::ComboBox),
            ControlFrameShape::Rounded
        );
        assert_eq!(
            control_frame_shape(NativeControlKind::List),
            ControlFrameShape::Rounded
        );
    }

    #[test]
    fn recessed_edit_uses_client_edge_without_ws_border() {
        let (style, ex_style) = recessed_edit_style_bits(WS_BORDER.0 as isize | 0x1000, 0x2000);
        assert_eq!(style & WS_BORDER.0 as isize, 0);
        assert_ne!(ex_style & WS_EX_CLIENTEDGE.0 as isize, 0);
    }

    #[test]
    fn dark_list_view_base_colors_cover_empty_body_and_text() {
        assert_eq!(
            list_view_base_colors(Palette::DARK),
            [
                (0x1001, Palette::DARK.edit),
                (0x1026, Palette::DARK.edit),
                (0x1024, Palette::DARK.text),
            ]
        );
    }

    #[test]
    fn choice_selection_uses_primary_normal_and_hot_palette() {
        let selected = selection_visual(Palette::DARK, true, false);
        let hot = selection_visual(Palette::DARK, false, true);
        let plain = selection_visual(Palette::DARK, false, false);
        assert_eq!(selected.fill, Palette::DARK.accent_fill);
        assert_eq!(hot.fill, rgb(54, 79, 91));
        assert_eq!(plain.fill, Palette::DARK.edit);

        let light_hot = selection_visual(Palette::LIGHT, false, true);
        assert_eq!(light_hot.fill, rgb(0, 103, 192));
        assert_eq!(light_hot.text, rgb(255, 255, 255));
    }

    #[test]
    fn heading_font_role_is_distinct_without_changing_the_font_family_contract() {
        assert_ne!(UiFontRole::Body, UiFontRole::Heading);
        assert_eq!(MICROSOFT_YAHEI_UI, "Microsoft YaHei UI");
    }
}
