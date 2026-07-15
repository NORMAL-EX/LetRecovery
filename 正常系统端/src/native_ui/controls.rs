use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    COLORREF, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreatePen, CreateSolidBrush, DeleteDC,
    DeleteObject, DrawTextW, FillRect, InvalidateRect, RoundRect, SelectObject, SetBkMode,
    SetStretchBltMode, SetTextColor, StretchBlt, DT_CENTER, DT_END_ELLIPSIS, DT_SINGLELINE,
    DT_VCENTER, HALFTONE, HDC, HFONT, PEN_STYLE, SRCCOPY, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    SetWindowTheme, DRAWITEMSTRUCT, ODA_FOCUS, ODS_DISABLED, ODS_FOCUS, ODS_HOTLIGHT, ODS_SELECTED,
    WM_MOUSELEAVE,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, GetPropW, GetWindowTextLengthW, GetWindowTextW, LoadCursorW, RemovePropW,
    SetCursor, SetPropW, BS_OWNERDRAW, HMENU, IDC_ARROW, SWP_NOMOVE, SWP_NOSIZE, WINDOWPOS,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CANCELMODE, WM_ENABLE, WM_ERASEBKGND, WM_MOUSEMOVE,
    WM_NCDESTROY, WM_SETCURSOR, WM_SHOWWINDOW, WM_WINDOWPOSCHANGING, WS_BORDER, WS_CHILD,
    WS_VISIBLE,
};

use super::theme::Palette;

const BUTTON_HOT_PROPERTY: PCWSTR = w!("LetRecovery.InnoButton.Hot");
const OWNER_DRAW_BUTTON_SUBCLASS_ID: usize = 0x4c52;
const SINGLE_LINE_EDIT_LAYOUT_SUBCLASS_ID: usize = 0x4c52_4544;

const fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    COLORREF((red as u32) | ((green as u32) << 8) | ((blue as u32) << 16))
}

/// Pixel metrics used by the Inno Setup 6.7 Modern Windows 11 control family.
///
/// Values are specified at 96 DPI. `for_dpi` rounds instead of truncating so repeated
/// layout calculations remain stable at 125%, 150%, 175%, and 200% scaling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InnoMetrics {
    pub button_height: i32,
    pub field_height: i32,
    pub list_item_height: i32,
    pub button_min_width: i32,
    pub button_padding_x: i32,
    pub control_gap: i32,
    pub corner_radius: i32,
    pub focus_inset: i32,
    pub separator_thickness: i32,
    pub progress_height: i32,
}

impl InnoMetrics {
    pub fn for_dpi(dpi: u32) -> Self {
        let scale = |value: i32| ((value as i64 * dpi.max(1) as i64 + 48) / 96) as i32;
        Self {
            button_height: scale(23),
            // Keep fields at the same 23 logical-pixel baseline as Inno's command controls. The
            // former 21px value made a DPI-scaled stock ComboBox visibly flatter than its Win11
            // counterpart, especially after the fixed-palette selection field was applied.
            field_height: scale(23),
            // Wizard check/list rows use a 22px minimum; using the same baseline keeps a popup
            // readable without returning to the oversized legacy egui spacing.
            list_item_height: scale(22),
            button_min_width: scale(75),
            button_padding_x: scale(14),
            control_gap: scale(8),
            corner_radius: scale(4).max(2),
            focus_inset: scale(2).max(1),
            separator_thickness: scale(1).max(1),
            progress_height: scale(16),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonRole {
    /// Inno's highlighted Next/Install action.
    Primary,
    /// Inno's Back/Browse/Cancel action.
    Secondary,
    /// Left navigation entry; selected entries use the highlighted action treatment.
    Navigation { selected: bool },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ControlState {
    pub hot: bool,
    pub pressed: bool,
    pub disabled: bool,
    pub focused: bool,
}

impl ControlState {
    pub fn from_draw_item(item: &DRAWITEMSTRUCT) -> Self {
        Self {
            hot: item.itemState.0 & ODS_HOTLIGHT.0 != 0,
            pressed: item.itemState.0 & ODS_SELECTED.0 != 0,
            disabled: item.itemState.0 & ODS_DISABLED.0 != 0,
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

/// Resolves every button state explicitly. This avoids relying on the host Windows theme,
/// which otherwise makes dark ComboBox/ListBox popups and owner-drawn buttons disagree.
pub fn button_visual(palette: Palette, role: ButtonRole, state: ControlState) -> ButtonVisual {
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

    let highlighted = matches!(role, ButtonRole::Primary)
        || matches!(role, ButtonRole::Navigation { selected: true });
    if highlighted {
        let fill = if state.pressed {
            if palette.dark {
                rgb(57, 171, 230)
            } else {
                rgb(0, 83, 160)
            }
        } else if state.hot {
            if palette.dark {
                rgb(96, 201, 255)
            } else {
                rgb(0, 103, 192)
            }
        } else {
            palette.highlight_fill
        };
        return ButtonVisual {
            fill,
            border: palette.highlight_border,
            text: if palette.dark {
                rgb(0, 0, 0)
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
        // Focus is deliberately not represented by a second or heavier outline. Mouse-down
        // assigns focus too, and changing the outline here makes an otherwise identical click
        // look as though the four antialiased corners suddenly became thicker.
        border: palette.border,
        text: palette.text,
    }
}

/// Draws an Inno Modern Windows 11 owner-drawn button, including mnemonic underlines.
/// The caller remains responsible for choosing `ButtonRole` from the control ID/page state.
pub unsafe fn draw_inno_button(
    item: &DRAWITEMSTRUCT,
    palette: Palette,
    role: ButtonRole,
    font: HFONT,
    dpi: u32,
) {
    // USER32 sends ODA_FOCUS when focus merely enters or leaves an owner-drawn button. Our
    // visuals intentionally do not paint a focus rectangle, so repainting the whole surface for
    // that notification only makes the default command button flash when another control is
    // clicked. Combined actions (for example ODA_FOCUS | ODA_SELECT) still need a real redraw.
    if item.itemAction.0 == ODA_FOCUS.0 {
        return;
    }

    let mut state = ControlState::from_draw_item(item);
    state.hot |= !GetPropW(item.hwndItem, BUTTON_HOT_PROPERTY).is_invalid();
    let visual = button_visual(palette, role, state);
    let metrics = InnoMetrics::for_dpi(dpi);
    let background = if matches!(role, ButtonRole::Navigation { .. }) {
        palette.nav
    } else {
        palette.window
    };

    let width = (item.rcItem.right - item.rcItem.left).max(0);
    let height = (item.rcItem.bottom - item.rcItem.top).max(0);
    if width == 0 || height == 0 {
        return;
    }

    // Compose geometry and text into one 1x buffer, then publish it with one BitBlt. Previously
    // the rounded body was stretched directly to the screen before the text was drawn; repeated
    // focus/selection notifications could therefore expose an incomplete button for one frame.
    let memory_dc = CreateCompatibleDC(item.hDC);
    if !memory_dc.is_invalid() {
        let bitmap = CreateCompatibleBitmap(item.hDC, width, height);
        if !bitmap.is_invalid() {
            let old_bitmap = SelectObject(memory_dc, bitmap);
            let local_rect = RECT {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            };
            draw_button_surface(
                memory_dc,
                local_rect,
                item.hwndItem,
                visual,
                metrics,
                background,
                font,
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

    // Low-resource fallback: keep the button usable even if allocating the temporary bitmap
    // fails. It uses the same geometry and colours, but draws directly to USER32's DC.
    draw_button_surface(
        item.hDC,
        item.rcItem,
        item.hwndItem,
        visual,
        metrics,
        background,
        font,
    );
}

unsafe fn draw_button_surface(
    dc: HDC,
    rect: RECT,
    hwnd: HWND,
    visual: ButtonVisual,
    metrics: InnoMetrics,
    background: COLORREF,
    font: HFONT,
) {
    fill_round_rect_antialiased(
        dc,
        rect,
        metrics.corner_radius,
        visual.fill,
        visual.border,
        background,
    );

    // Keep a single outline. Win32 assigns keyboard focus on mouse-down as well, so an
    // additional inset focus rectangle would make every clicked button look double framed.

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

pub unsafe fn draw_separator(dc: HDC, rect: RECT, palette: Palette, dpi: u32) {
    let height = InnoMetrics::for_dpi(dpi).separator_thickness;
    let line = RECT {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: (rect.top + height).min(rect.bottom),
    };
    fill_solid_rect(dc, &line, palette.separator);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressRole {
    Normal,
    Success,
    Error,
    Paused,
}

/// Draws the flat, non-gradient progress bar used by Inno's modern wizard.
/// `completed` and `total` are integers so long-running byte progress does not lose precision.
pub unsafe fn draw_progress(
    dc: HDC,
    rect: RECT,
    completed: u64,
    total: u64,
    role: ProgressRole,
    palette: Palette,
) {
    let width = (rect.right - rect.left).max(0);
    let height = (rect.bottom - rect.top).max(0);
    if width == 0 || height == 0 {
        return;
    }

    // Draw the complete control off-screen. Progress updates arrive several times per second and
    // painting the track and fill directly exposes the intermediate empty track as a bright flash.
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
    let filled = if total == 0 {
        0
    } else {
        ((inner_width as u64).saturating_mul(completed.min(total)) / total) as i32
    };
    if filled == 0 {
        let _ = BitBlt(
            dc, rect.left, rect.top, width, height, memory_dc, 0, 0, SRCCOPY,
        );
        let _ = SelectObject(memory_dc, old_bitmap);
        let _ = DeleteObject(bitmap);
        let _ = DeleteDC(memory_dc);
        return;
    }
    let color = match role {
        ProgressRole::Normal | ProgressRole::Success => palette.progress,
        ProgressRole::Error => rgb(196, 43, 28),
        ProgressRole::Paused => rgb(247, 153, 52),
    };
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
        color,
        color,
        palette.edit,
    );
    let _ = BitBlt(
        dc, rect.left, rect.top, width, height, memory_dc, 0, 0, SRCCOPY,
    );
    let _ = SelectObject(memory_dc, old_bitmap);
    let _ = DeleteObject(bitmap);
    let _ = DeleteDC(memory_dc);
}

unsafe fn fill_round_rect(dc: HDC, rect: RECT, radius: i32, fill: COLORREF, border: COLORREF) {
    let brush = CreateSolidBrush(fill);
    let pen = CreatePen(PEN_STYLE(0), 1, border);
    let old_brush = SelectObject(dc, brush);
    let old_pen = SelectObject(dc, pen);
    let diameter = radius.saturating_mul(2);
    let _ = RoundRect(
        dc,
        rect.left,
        rect.top,
        rect.right,
        rect.bottom,
        diameter,
        diameter,
    );
    let _ = SelectObject(dc, old_pen);
    let _ = SelectObject(dc, old_brush);
    let _ = DeleteObject(pen);
    let _ = DeleteObject(brush);
}

/// GDI's direct `RoundRect` is visibly stair-stepped at 100-200% DPI. Render the small
/// geometry at 4x into a temporary bitmap and downsample it with HALFTONE; text remains drawn
/// by the destination DC so ClearType is not blurred. Every temporary GDI object is released
/// before returning.
pub(crate) unsafe fn fill_round_rect_antialiased(
    dc: HDC,
    rect: RECT,
    radius: i32,
    fill: COLORREF,
    border: COLORREF,
    background: COLORREF,
) {
    if !try_fill_round_rect_antialiased(dc, rect, radius, fill, border, background) {
        fill_round_rect(dc, rect, radius, fill, border);
    }
}

unsafe fn try_fill_round_rect_antialiased(
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
    let memory_dc = CreateCompatibleDC(dc);
    if memory_dc.is_invalid() {
        return false;
    }
    let bitmap = CreateCompatibleBitmap(dc, width * SCALE, height * SCALE);
    if bitmap.is_invalid() {
        let _ = DeleteDC(memory_dc);
        return false;
    }
    let old_bitmap = SelectObject(memory_dc, bitmap);
    let background_brush = CreateSolidBrush(background);
    let high_rect = RECT {
        left: 0,
        top: 0,
        right: width * SCALE,
        bottom: height * SCALE,
    };
    let _ = FillRect(memory_dc, &high_rect, background_brush);
    let _ = DeleteObject(background_brush);

    let brush = CreateSolidBrush(fill);
    let pen = CreatePen(PEN_STYLE(0), SCALE, border);
    let old_brush = SelectObject(memory_dc, brush);
    let old_pen = SelectObject(memory_dc, pen);
    let diameter = radius.saturating_mul(2).saturating_mul(SCALE);
    // A GDI pen is centred on its path. Drawing on the bitmap boundary clipped half of each
    // straight edge while retaining more coverage around the arcs, which made the four corners
    // look heavier. Centre the 1px logical stroke half a pixel inside the high-resolution canvas
    // so straight edges and corners are downsampled from the same full-width outline.
    let pen_inset = SCALE / 2;
    let _ = RoundRect(
        memory_dc,
        pen_inset,
        pen_inset,
        width * SCALE - pen_inset,
        height * SCALE - pen_inset,
        diameter,
        diameter,
    );
    let _ = SelectObject(memory_dc, old_pen);
    let _ = SelectObject(memory_dc, old_brush);
    let _ = DeleteObject(pen);
    let _ = DeleteObject(brush);

    let _ = SetStretchBltMode(dc, HALFTONE);
    let _ = StretchBlt(
        dc,
        rect.left,
        rect.top,
        width,
        height,
        memory_dc,
        0,
        0,
        width * SCALE,
        height * SCALE,
        SRCCOPY,
    );
    let _ = SelectObject(memory_dc, old_bitmap);
    let _ = DeleteObject(bitmap);
    let _ = DeleteDC(memory_dc);
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RoundedControlFrameGeometry {
    pub radius: i32,
    pub arc_band: i32,
    pub side_band: i32,
}

/// Geometry for the rounded overlay used by editable/list controls.
///
/// It deliberately describes paint bands rather than a window region. The underlying content,
/// scrollbars and hit-test rectangle remain rectangular and fully usable; only the outer visual
/// frame is replaced.
pub(crate) fn rounded_control_frame_geometry(
    width: i32,
    height: i32,
    dpi: u32,
) -> Option<RoundedControlFrameGeometry> {
    if width <= 0 || height <= 0 {
        return None;
    }
    let scale = |value: i32| ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32;
    let radius = scale(5)
        .max(2)
        .min((width / 2).max(1))
        .min((height / 2).max(1));
    Some(RoundedControlFrameGeometry {
        radius,
        arc_band: (radius + scale(1).max(1)).min((height / 2).max(1)),
        side_band: scale(1).max(1).min(width),
    })
}

/// Paints a four-sample-per-axis rounded frame over only the boundary of an already painted native
/// control. Fully interior pixels are left untouched, while boundary pixels are generated from
/// absolute palette colours instead of the previous framebuffer value; repeated paint messages
/// therefore cannot darken the edge or grow a rectangular corner block.
pub(crate) unsafe fn draw_antialiased_control_frame(
    dc: HDC,
    rect: RECT,
    geometry: RoundedControlFrameGeometry,
    interior: COLORREF,
    border: COLORREF,
    exterior: COLORREF,
) {
    draw_antialiased_control_frame_impl(
        dc,
        rect,
        geometry,
        interior,
        border,
        CornerExterior::Color(exterior),
    );
}

/// Draws a rounded outline without fabricating pixels outside the rounded popup surface.  A
/// ComboLBox is a separate top-level window and its four corners can overlap arbitrary content;
/// using the owner's nominal window colour there produces visible dark/light blocks.  Fully
/// exterior pixels therefore remain under USER32's native paint, while the outline itself is
/// still generated deterministically.
pub(crate) unsafe fn draw_antialiased_control_frame_preserving_exterior(
    dc: HDC,
    rect: RECT,
    geometry: RoundedControlFrameGeometry,
    interior: COLORREF,
    border: COLORREF,
) {
    draw_antialiased_control_frame_impl(
        dc,
        rect,
        geometry,
        interior,
        border,
        CornerExterior::PreserveNative,
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CornerExterior {
    Color(COLORREF),
    PreserveNative,
}

unsafe fn draw_antialiased_control_frame_impl(
    dc: HDC,
    rect: RECT,
    geometry: RoundedControlFrameGeometry,
    interior: COLORREF,
    border: COLORREF,
    exterior: CornerExterior,
) {
    let width = (rect.right - rect.left).max(0);
    let height = (rect.bottom - rect.top).max(0);
    if width == 0 || height == 0 {
        return;
    }
    let radius = geometry.radius.min(width / 2).min(height / 2).max(1);
    let side = geometry.side_band.max(1);
    fill_solid_rect(
        dc,
        &RECT {
            left: rect.left + radius,
            top: rect.top,
            right: rect.right - radius,
            bottom: rect.top + side,
        },
        border,
    );
    fill_solid_rect(
        dc,
        &RECT {
            left: rect.left + radius,
            top: rect.bottom - side,
            right: rect.right - radius,
            bottom: rect.bottom,
        },
        border,
    );
    fill_solid_rect(
        dc,
        &RECT {
            left: rect.left,
            top: rect.top + radius,
            right: rect.left + side,
            bottom: rect.bottom - radius,
        },
        border,
    );
    fill_solid_rect(
        dc,
        &RECT {
            left: rect.right - side,
            top: rect.top + radius,
            right: rect.right,
            bottom: rect.bottom - radius,
        },
        border,
    );

    for (origin_x, origin_y, flip_x, flip_y) in [
        (rect.left, rect.top, false, false),
        (rect.right - radius, rect.top, true, false),
        (rect.left, rect.bottom - radius, false, true),
        (rect.right - radius, rect.bottom - radius, true, true),
    ] {
        paint_antialiased_frame_corner(
            dc,
            (origin_x, origin_y),
            (radius, side),
            (flip_x, flip_y),
            interior,
            border,
            exterior,
        );
    }
}

unsafe fn paint_antialiased_frame_corner(
    dc: HDC,
    origin: (i32, i32),
    geometry: (i32, i32),
    flip: (bool, bool),
    interior: COLORREF,
    border: COLORREF,
    exterior: CornerExterior,
) {
    const SAMPLES: i32 = 4;
    let (radius, border_width) = geometry;
    let outer_radius = radius as f64;
    // Keep the arc thickness identical to the straight frame at every DPI.  Using a fixed
    // one-pixel inset made the 200% straight edges two pixels wide while the corners remained one
    // pixel, which produced the visible grainy seam the user reported.
    let inner_radius = (radius - border_width.max(1)).max(0) as f64;
    for y in 0..radius {
        for x in 0..radius {
            let mut outer = 0u32;
            let mut inner = 0u32;
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let px = x as f64 + (sx as f64 + 0.5) / SAMPLES as f64;
                    let py = y as f64 + (sy as f64 + 0.5) / SAMPLES as f64;
                    let dx = outer_radius - px;
                    let dy = outer_radius - py;
                    let distance = dx * dx + dy * dy;
                    outer += u32::from(distance <= outer_radius * outer_radius);
                    inner += u32::from(distance <= inner_radius * inner_radius);
                }
            }
            let screen_x = origin.0 + if flip.0 { radius - 1 - x } else { x };
            let screen_y = origin.1 + if flip.1 { radius - 1 - y } else { y };
            if let Some(color) = deterministic_corner_color(
                interior,
                border,
                exterior,
                inner,
                outer,
                (SAMPLES * SAMPLES) as u32,
            ) {
                let _ = windows::Win32::Graphics::Gdi::SetPixelV(dc, screen_x, screen_y, color);
            }
        }
    }
}

/// Computes an absolute corner colour rather than blending with the pixel left by the previous
/// paint.  Consequently WM_PAINT followed by WM_NCPAINT writes exactly the same values and cannot
/// progressively darken the antialiased edge. Fully interior pixels are never overwritten.
fn deterministic_corner_color(
    interior: COLORREF,
    border: COLORREF,
    exterior: CornerExterior,
    inner_samples: u32,
    outer_samples: u32,
    sample_count: u32,
) -> Option<COLORREF> {
    let inner_samples = inner_samples.min(sample_count);
    let outer_samples = outer_samples.clamp(inner_samples, sample_count);
    if inner_samples == sample_count {
        return None;
    }
    let border_samples = outer_samples - inner_samples;
    let exterior_samples = sample_count - outer_samples;
    match exterior {
        CornerExterior::Color(exterior) => Some(weighted_color(
            interior,
            inner_samples,
            border,
            border_samples,
            exterior,
            exterior_samples,
        )),
        CornerExterior::PreserveNative if outer_samples == 0 => None,
        // Treat sub-pixel samples outside the rounded popup as its already-painted native client
        // colour. This smooths only the border; it never guesses the unrelated screen content
        // physically underneath the top-level ComboLBox.
        CornerExterior::PreserveNative => Some(weighted_color(
            interior,
            inner_samples + exterior_samples,
            border,
            border_samples,
            interior,
            0,
        )),
    }
}

fn weighted_color(
    first: COLORREF,
    first_weight: u32,
    second: COLORREF,
    second_weight: u32,
    third: COLORREF,
    third_weight: u32,
) -> COLORREF {
    let total = first_weight + second_weight + third_weight;
    if total == 0 {
        return first;
    }
    let channel = |shift: u32| {
        ((((first.0 >> shift) & 0xff) * first_weight
            + ((second.0 >> shift) & 0xff) * second_weight
            + ((third.0 >> shift) & 0xff) * third_weight
            + total / 2)
            / total)
            << shift
    };
    COLORREF(channel(0) | channel(8) | channel(16))
}

unsafe fn stroke_round_rect(dc: HDC, rect: RECT, radius: i32, color: COLORREF) {
    let pen = CreatePen(PEN_STYLE(0), 1, color);
    let hollow =
        windows::Win32::Graphics::Gdi::GetStockObject(windows::Win32::Graphics::Gdi::NULL_BRUSH);
    let old_brush = SelectObject(dc, hollow);
    let old_pen = SelectObject(dc, pen);
    let diameter = radius.saturating_mul(2);
    let _ = RoundRect(
        dc,
        rect.left,
        rect.top,
        rect.right,
        rect.bottom,
        diameter,
        diameter,
    );
    let _ = SelectObject(dc, old_pen);
    let _ = SelectObject(dc, old_brush);
    let _ = DeleteObject(pen);
}

unsafe fn fill_solid_rect(dc: HDC, rect: &RECT, color: COLORREF) {
    let brush = CreateSolidBrush(color);
    let _ = FillRect(dc, rect, brush);
    let _ = DeleteObject(brush);
}

unsafe fn stroke_rect(dc: HDC, rect: RECT, color: COLORREF) {
    stroke_round_rect(dc, rect, 0, color);
}

pub fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

/// Sentinel passed to `CB_SETCURSEL` when an inventory-backed combo box has no selection.
///
/// The combo must contain only real inventory entries. Keeping the blank state in USER32 rather
/// than inserting a fabricated "请选择" row makes the control index identical to the inventory
/// index and prevents an empty choice from being mistaken for the first dangerous target.
pub(crate) const NO_COMBO_SELECTION: usize = usize::MAX;

pub(crate) fn combo_inventory_index(raw_index: isize, item_count: usize) -> Option<usize> {
    usize::try_from(raw_index)
        .ok()
        .filter(|index| *index < item_count)
}

pub unsafe fn child(
    parent: HWND,
    class_name: PCWSTR,
    text: &str,
    style: i32,
    id: u16,
) -> windows::core::Result<HWND> {
    let text = wide(text);
    let is_edit = is_edit_class(class_name);
    let is_combo = is_combo_class(class_name);
    let (extended_style, control_style) = child_styles(is_edit, is_combo, style);
    let hwnd = CreateWindowExW(
        extended_style,
        class_name,
        PCWSTR(text.as_ptr()),
        control_style,
        0,
        0,
        0,
        0,
        parent,
        HMENU(id as isize as *mut _),
        HINSTANCE::default(),
        None,
    )?;
    if is_edit {
        const ES_MULTILINE: u32 = 0x0004;
        if style as u32 & ES_MULTILINE == 0 {
            // Keep USER32 text/caret/selection/IME behaviour, but disable the host's square
            // CLIENTEDGE before first display. The shared Win11 field subclass supplies the same
            // closed surface as ComboBox without a Win10/Win11 theme transition flash.
            let _ = SetWindowTheme(hwnd, w!(""), w!(""));
            // A single-line Win32 Edit does not support EM_SETRECT/EM_SETRECTNP. Keep the real
            // control centred inside whatever row height the responsive page requests.
            center_single_line_edit_in_row(hwnd);
        } else {
            let _ = SetWindowTheme(hwnd, w!("Explorer"), PCWSTR::null());
        }
    }
    // The fixed Inno reference declares TNewComboBox as a plain TComboBox. Keep USER32's normal
    // Windows 11 string/popup renderer instead of forcing CBS_OWNERDRAWFIXED globally: owner draw
    // replaces the native popup and repeatedly makes the owner fetch and paint every visible row.
    if is_button_class(class_name) && style & 0x0f == BS_OWNERDRAW {
        // Some page-state refreshes deliberately invalidate the command button with erase=true.
        // The owner draw covers every pixel (including the transparent-looking corner colour),
        // so the standard BUTTON background erase is both redundant and the visible source of
        // the one-frame flash before WM_DRAWITEM arrives.
        let _ = SetWindowSubclass(
            hwnd,
            Some(owner_draw_button_proc),
            OWNER_DRAW_BUTTON_SUBCLASS_ID,
            0,
        );
    }
    Ok(hwnd)
}

/// Keeps a stock single-line v6 Edit at the native property-page height while centring the real
/// HWND inside a taller responsive layout row. Directly-created fields must call this too; the
/// control continues to own all text, selection, IME and non-client painting.
pub(crate) unsafe fn center_single_line_edit_in_row(hwnd: HWND) {
    let _ = SetWindowSubclass(
        hwnd,
        Some(single_line_edit_layout_proc),
        SINGLE_LINE_EDIT_LAYOUT_SUBCLASS_ID,
        0,
    );
}

unsafe extern "system" fn single_line_edit_layout_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    _reference_data: usize,
) -> LRESULT {
    match message {
        WM_WINDOWPOSCHANGING if lparam.0 != 0 => {
            let position = &mut *(lparam.0 as *mut WINDOWPOS);
            if !position.flags.contains(SWP_NOSIZE) {
                let target_height =
                    InnoMetrics::for_dpi(GetDpiForWindow(hwnd).max(96)).field_height;
                (position.y, position.cy) = centered_single_line_edit_bounds(
                    position.y,
                    position.cy,
                    target_height,
                    !position.flags.contains(SWP_NOMOVE),
                );
            }
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(
                hwnd,
                Some(single_line_edit_layout_proc),
                SINGLE_LINE_EDIT_LAYOUT_SUBCLASS_ID,
            );
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

const fn centered_single_line_edit_bounds(
    y: i32,
    height: i32,
    target_height: i32,
    may_move: bool,
) -> (i32, i32) {
    if height <= 0 || target_height <= 0 || height < target_height {
        (y, height)
    } else {
        (
            if may_move {
                y + (height - target_height) / 2
            } else {
                y
            },
            target_height,
        )
    }
}

unsafe fn is_edit_class(class_name: PCWSTR) -> bool {
    class_name
        .as_wide()
        .iter()
        .copied()
        .eq("EDIT".encode_utf16())
}

unsafe fn is_button_class(class_name: PCWSTR) -> bool {
    class_name
        .as_wide()
        .iter()
        .copied()
        .eq("BUTTON".encode_utf16())
}

unsafe fn is_combo_class(class_name: PCWSTR) -> bool {
    class_name
        .as_wide()
        .iter()
        .copied()
        .eq("COMBOBOX".encode_utf16())
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
        WM_SETCURSOR => {
            // Navigation and command buttons use the same stable native arrow as Inno.  Owning
            // this message prevents theme/class cursor hand-offs from flashing hand/arrow while
            // the pointer crosses the antialiased edge.
            if let Ok(cursor) = LoadCursorW(None, IDC_ARROW) {
                let _ = SetCursor(cursor);
                LRESULT(1)
            } else {
                DefSubclassProc(hwnd, message, wparam, lparam)
            }
        }
        WM_MOUSEMOVE => {
            if GetPropW(hwnd, BUTTON_HOT_PROPERTY).is_invalid() {
                let mut tracking = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                if TrackMouseEvent(&mut tracking).is_ok()
                    && SetPropW(hwnd, BUTTON_HOT_PROPERTY, HANDLE(std::ptr::dangling_mut())).is_ok()
                {
                    // Invalidate only this button. Repainting the parent here produces the visible
                    // command-bar/page flash that hover feedback is meant to avoid.
                    let _ = InvalidateRect(hwnd, None, false);
                } else {
                    // A failed leave subscription must never leave a permanent hot marker behind.
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
                // Dialog shells reuse hidden child HWNDs. Clear hot state before a later show so
                // the next tool/page cannot inherit the last pointer position.
                clear_button_hot(hwnd);
            }
            result
        }
        WM_ENABLE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if wparam.0 == 0 {
                // A disabled control may stop receiving pointer messages before the queued leave
                // notification. Clear the cached hot state so re-enabling it cannot resurrect a
                // stale hover colour while the pointer is elsewhere.
                clear_button_hot(hwnd);
            }
            if wparam.0 != 0 {
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
    if RemovePropW(hwnd, BUTTON_HOT_PROPERTY).is_ok_and(|handle| !handle.is_invalid()) {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

fn child_styles(is_edit: bool, _is_combo: bool, style: i32) -> (WINDOW_EX_STYLE, WINDOW_STYLE) {
    let mut control_style = (WS_CHILD | WS_VISIBLE).0 | style as u32;
    let mut extended_style = WINDOW_EX_STYLE::default();
    if is_edit {
        // Single-line fields share the deterministic Win11 frame used by ComboBox. A second
        // WS_BORDER/CLIENTEDGE would expose a square host-theme frame around it.
        const ES_MULTILINE: u32 = 0x0004;
        if style as u32 & ES_MULTILINE == 0 {
            const WS_EX_NOPARENTNOTIFY_VALUE: u32 = 0x0000_0004;
            control_style &= !WS_BORDER.0;
            extended_style |= WINDOW_EX_STYLE(WS_EX_NOPARENTNOTIFY_VALUE);
        } else {
            control_style |= WS_BORDER.0;
        }
    }
    (extended_style, WINDOW_STYLE(control_style))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpi_metrics_round_consistently() {
        assert_eq!(InnoMetrics::for_dpi(96).button_height, 23);
        assert_eq!(InnoMetrics::for_dpi(96).field_height, 23);
        assert_eq!(InnoMetrics::for_dpi(96).list_item_height, 22);
        assert_eq!(InnoMetrics::for_dpi(120).button_height, 29);
        assert_eq!(InnoMetrics::for_dpi(192).field_height, 46);
        assert_eq!(InnoMetrics::for_dpi(144).button_min_width, 113);
        assert_eq!(InnoMetrics::for_dpi(192).corner_radius, 8);
    }

    #[test]
    fn combo_inventory_index_keeps_blank_and_inventory_indices_distinct() {
        assert_eq!(combo_inventory_index(-1, 3), None);
        assert_eq!(combo_inventory_index(0, 3), Some(0));
        assert_eq!(combo_inventory_index(2, 3), Some(2));
        assert_eq!(combo_inventory_index(3, 3), None);
        assert_eq!(combo_inventory_index(0, 0), None);
        assert_eq!(NO_COMBO_SELECTION as isize, -1);
    }

    #[test]
    fn combo_keeps_native_renderer_and_does_not_change_edit_styles() {
        let (_, combo) = child_styles(false, true, 0);
        const CBS_OWNERDRAWFIXED_VALUE: u32 = 0x0010;
        const CBS_OWNERDRAWVARIABLE_VALUE: u32 = 0x0020;
        assert_eq!(combo.0 & CBS_OWNERDRAWFIXED_VALUE, 0);
        assert_eq!(combo.0 & CBS_OWNERDRAWVARIABLE_VALUE, 0);

        let (edit_ex, edit) = child_styles(true, false, 0);
        assert_eq!(edit.0 & WS_BORDER.0, 0);
        assert_eq!(edit_ex.0 & 0x0000_0200, 0);
        assert_ne!(edit_ex.0 & 0x0000_0004, 0);
        assert_eq!(edit.0 & CBS_OWNERDRAWFIXED_VALUE, 0);
    }

    #[test]
    fn rounded_control_frame_scales_without_consuming_the_content_rectangle() {
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
    fn rounded_corner_color_is_idempotent_and_preserves_true_interior() {
        let interior = rgb(31, 31, 31);
        let border = rgb(67, 67, 67);
        let exterior = CornerExterior::Color(rgb(43, 43, 43));
        let first = deterministic_corner_color(interior, border, exterior, 5, 12, 16);
        let repeated = deterministic_corner_color(interior, border, exterior, 5, 12, 16);
        assert_eq!(first, repeated);
        assert_eq!(
            deterministic_corner_color(interior, border, exterior, 16, 16, 16),
            None
        );

        let popup = CornerExterior::PreserveNative;
        assert_eq!(
            deterministic_corner_color(interior, border, popup, 0, 0, 16),
            None
        );
        assert_eq!(
            deterministic_corner_color(interior, border, popup, 4, 11, 16),
            deterministic_corner_color(interior, border, popup, 4, 11, 16)
        );
    }

    #[test]
    fn edit_uses_shared_single_line_frame_but_keeps_multiline_report_border() {
        const WS_EX_CLIENTEDGE_VALUE: u32 = 0x0000_0200;
        let (single_ex, single) = child_styles(true, false, 0);
        assert_eq!(single_ex.0 & WS_EX_CLIENTEDGE_VALUE, 0);
        assert_eq!(single.0 & WS_BORDER.0, 0);

        const PASSWORD_READONLY_MULTILINE: u32 = 0x0020 | 0x0800 | 0x0004;
        let incoming = PASSWORD_READONLY_MULTILINE as i32;
        let (extended, style) = child_styles(true, false, incoming);

        assert_eq!(extended.0 & WS_EX_CLIENTEDGE_VALUE, 0);
        assert_ne!(style.0 & WS_BORDER.0, 0);
        assert_eq!(
            style.0 & PASSWORD_READONLY_MULTILINE,
            PASSWORD_READONLY_MULTILINE
        );
        assert_ne!(style.0 & WS_CHILD.0, 0);
        assert_ne!(style.0 & WS_VISIBLE.0, 0);
    }

    #[test]
    fn single_line_edit_is_vertically_centered_by_sizing_the_native_control() {
        assert_eq!(
            centered_single_line_edit_bounds(100, 30, 21, true),
            (104, 21)
        );
        assert_eq!(
            centered_single_line_edit_bounds(100, 21, 21, true),
            (100, 21)
        );
        assert_eq!(
            centered_single_line_edit_bounds(100, 18, 21, true),
            (100, 18)
        );
        assert_eq!(centered_single_line_edit_bounds(100, 0, 21, true), (100, 0));
        assert_eq!(
            centered_single_line_edit_bounds(100, 30, 21, false),
            (100, 21)
        );
        assert_eq!(
            centered_single_line_edit_bounds(200, 60, 42, true),
            (209, 42)
        );
    }

    #[test]
    fn non_edit_child_styles_are_unchanged() {
        let incoming = (WS_BORDER.0 | 0x0100) as i32;
        let (extended, style) = child_styles(false, false, incoming);
        assert_eq!(extended, WINDOW_EX_STYLE::default());
        assert_eq!(style.0, (WS_CHILD | WS_VISIBLE).0 | incoming as u32);
    }

    #[test]
    fn dark_highlighted_button_uses_the_audited_windows_accent() {
        let primary = button_visual(Palette::DARK, ButtonRole::Primary, ControlState::default());
        assert_eq!(primary.fill, rgb(76, 194, 255));
        assert_eq!(primary.border, rgb(76, 194, 255));
        assert_eq!(primary.text, rgb(0, 0, 0));

        let secondary = button_visual(
            Palette::DARK,
            ButtonRole::Secondary,
            ControlState::default(),
        );
        assert_eq!(secondary.fill, rgb(55, 55, 55));
        assert_eq!(secondary.border, rgb(67, 67, 67));
    }

    #[test]
    fn selected_navigation_uses_primary_treatment() {
        let primary = button_visual(Palette::LIGHT, ButtonRole::Primary, ControlState::default());
        let selected = button_visual(
            Palette::LIGHT,
            ButtonRole::Navigation { selected: true },
            ControlState::default(),
        );
        assert_eq!(selected, primary);
    }

    #[test]
    fn button_hot_and_pressed_states_change_fill_without_focus_border() {
        let normal = button_visual(
            Palette::DARK,
            ButtonRole::Secondary,
            ControlState::default(),
        );
        let hot = button_visual(
            Palette::DARK,
            ButtonRole::Secondary,
            ControlState {
                hot: true,
                ..ControlState::default()
            },
        );
        let pressed = button_visual(
            Palette::DARK,
            ButtonRole::Secondary,
            ControlState {
                pressed: true,
                ..ControlState::default()
            },
        );
        let focused = button_visual(
            Palette::DARK,
            ButtonRole::Secondary,
            ControlState {
                focused: true,
                ..ControlState::default()
            },
        );
        assert_ne!(normal.fill, hot.fill);
        assert_ne!(normal.fill, pressed.fill);
        assert_eq!(normal.border, hot.border);
        assert_eq!(normal, focused);
    }
}
