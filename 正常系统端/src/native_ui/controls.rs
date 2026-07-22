use std::ffi::c_void;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    COLORREF, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    AlphaBlend, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDIBSection, CreatePen,
    CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, FillRect, GdiFlush, GetCurrentObject,
    GetDC, GetTextMetricsW, InvalidateRect, ReleaseDC, RoundRect, ScreenToClient, SelectObject,
    SetBkColor, SetBkMode, SetStretchBltMode, SetTextColor, StretchBlt, StretchDIBits,
    AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS,
    DRAW_TEXT_FORMAT, DT_CENTER, DT_END_ELLIPSIS, DT_SINGLELINE, DT_VCENTER, HALFTONE, HDC, HFONT,
    OBJ_FONT, OPAQUE, PEN_STYLE, SRCCOPY, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    SetWindowTheme, DRAWITEMSTRUCT, ODA_FOCUS, ODS_DISABLED, ODS_FOCUS, ODS_HOTLIGHT, ODS_SELECTED,
    WM_MOUSELEAVE,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetParent, GetPropW, GetWindowLongPtrW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, IsWindow, LoadCursorW, RemovePropW, SendMessageW,
    SetCursor, SetPropW, SetWindowPos, ShowWindow, BS_OWNERDRAW, GWL_STYLE, HMENU, IDC_ARROW,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_SHOW, WINDOWPOS,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CANCELMODE, WM_ENABLE, WM_ERASEBKGND, WM_GETFONT,
    WM_MOUSEMOVE, WM_NCDESTROY, WM_SETCURSOR, WM_SETFONT, WM_SHOWWINDOW, WM_WINDOWPOSCHANGING,
    WS_BORDER, WS_CHILD, WS_CLIPSIBLINGS, WS_VISIBLE,
};

use super::theme::Palette;

const BUTTON_HOT_PROPERTY: PCWSTR = w!("LetRecovery.InnoButton.Hot");
const OWNER_DRAW_BUTTON_SUBCLASS_ID: usize = 0x4c52;
const SINGLE_LINE_EDIT_LAYOUT_SUBCLASS_ID: usize = 0x4c52_4544;
const SINGLE_LINE_EDIT_FRAME_PROPERTY: PCWSTR = w!("LetRecovery.InnoEdit.Frame");
const SINGLE_LINE_EDIT_OWNER_PROPERTY: PCWSTR = w!("LetRecovery.InnoEdit.Owner");
const SINGLE_LINE_EDIT_INTERNAL_LAYOUT_PROPERTY: PCWSTR = w!("LetRecovery.InnoEdit.Layout");
const LIST_VIEW_LAYOUT_SUBCLASS_ID: usize = 0x4c52_4c46;
const LIST_VIEW_FRAME_PROPERTY: PCWSTR = w!("LetRecovery.InnoListView.Frame");
const LIST_VIEW_OWNER_PROPERTY: PCWSTR = w!("LetRecovery.InnoListView.Owner");
const LIST_VIEW_INTERNAL_LAYOUT_PROPERTY: PCWSTR = w!("LetRecovery.InnoListView.Layout");

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
            // readable without returning to the former oversized spacing.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ButtonSurfaceVisual {
    fill: COLORREF,
    border: COLORREF,
    text: COLORREF,
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

fn button_surface_visual(
    palette: Palette,
    role: ButtonRole,
    state: ControlState,
) -> ButtonSurfaceVisual {
    let visual = button_visual(palette, role, state);
    ButtonSurfaceVisual {
        fill: visual.fill,
        border: visual.border,
        text: visual.text,
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
    let visual = button_surface_visual(palette, role, state);
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

    // Compose geometry and text into one 1x buffer, then publish it with one transfer. Previously
    // the rounded body was stretched directly to the screen before the text was drawn; repeated
    // focus/selection notifications could therefore expose an incomplete button for one frame.
    let memory_dc = CreateCompatibleDC(item.hDC);
    if !memory_dc.is_invalid() {
        let mut bits = std::ptr::null_mut::<c_void>();
        let bitmap_info = top_down_bgra_bitmap_info(width, height);
        let alpha_bitmap = if background.0 == 0 {
            CreateDIBSection(
                memory_dc,
                &bitmap_info,
                DIB_RGB_COLORS,
                &mut bits,
                HANDLE::default(),
                0,
            )
            .ok()
        } else {
            None
        };
        let compatible_bitmap = if alpha_bitmap.is_none() {
            let bitmap = CreateCompatibleBitmap(item.hDC, width, height);
            (!bitmap.is_invalid()).then_some(bitmap)
        } else {
            None
        };
        if let Some(bitmap) = alpha_bitmap.or(compatible_bitmap) {
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
            if !bits.is_null() {
                let _ = StretchDIBits(
                    item.hDC,
                    item.rcItem.left,
                    item.rcItem.top,
                    width,
                    height,
                    0,
                    0,
                    width,
                    height,
                    Some(bits.cast_const()),
                    &bitmap_info,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
            } else {
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
            }
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
    visual: ButtonSurfaceVisual,
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
    draw_native_text(
        dc,
        &text,
        &mut text_rect,
        DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
        visual.text,
    );
    let _ = SelectObject(dc, old_font);
}

/// Draws text with the selected native font, ellipsis and mnemonic layout.
pub(crate) unsafe fn draw_native_text(
    dc: HDC,
    text: &[u16],
    rect: &mut RECT,
    flags: DRAW_TEXT_FORMAT,
    color: COLORREF,
) {
    draw_text_fallback(dc, text, rect, flags, color);
}

/// Publishes an already premultiplied top-down BGRA surface over a classic child-window DC.
/// Pixels with zero alpha preserve the destination around antialiased glyphs.
pub(crate) unsafe fn alpha_blend_premultiplied_bgra(
    dc: HDC,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    pixels: &[u8],
) -> bool {
    if width <= 0 || height <= 0 || pixels.len() != width as usize * height as usize * 4 {
        return false;
    }
    let buffer_dc = CreateCompatibleDC(dc);
    if buffer_dc.is_invalid() {
        return false;
    }
    let bitmap_info = top_down_bgra_bitmap_info(width, height);
    let mut bits = std::ptr::null_mut::<c_void>();
    let Ok(bitmap) = CreateDIBSection(
        buffer_dc,
        &bitmap_info,
        DIB_RGB_COLORS,
        &mut bits,
        HANDLE::default(),
        0,
    ) else {
        let _ = DeleteDC(buffer_dc);
        return false;
    };
    let old_bitmap = SelectObject(buffer_dc, bitmap);
    std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u8>(), pixels.len());
    let blended = AlphaBlend(
        dc,
        x,
        y,
        width,
        height,
        buffer_dc,
        0,
        0,
        width,
        height,
        BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        },
    )
    .as_bool();
    let _ = SelectObject(buffer_dc, old_bitmap);
    let _ = DeleteObject(bitmap);
    let _ = DeleteDC(buffer_dc);
    blended
}

/// Draws text over a known opaque row/cell surface while preserving GDI ClearType RGB coverage.
/// ListView custom draw must use the same opaque background contract as comctl32; transparent
/// `DrawTextW` silently falls back to grayscale antialiasing and makes selected rows look like a
/// different font. The alpha byte is repaired only after GDI has been flushed.
pub(crate) unsafe fn draw_opaque_surface_text(
    dc: HDC,
    text: &[u16],
    rect: &mut RECT,
    flags: DRAW_TEXT_FORMAT,
    color: COLORREF,
    background: COLORREF,
) {
    if text.is_empty() {
        return;
    }
    let width = (rect.right - rect.left).max(0);
    let height = (rect.bottom - rect.top).max(0);
    if width == 0 || height == 0 {
        return;
    }
    let buffer_dc = CreateCompatibleDC(dc);
    if buffer_dc.is_invalid() {
        draw_opaque_text_fallback(dc, text, rect, flags, color, background);
        return;
    }
    let bitmap_info = top_down_bgra_bitmap_info(width, height);
    let mut bits = std::ptr::null_mut::<c_void>();
    let Ok(bitmap) = CreateDIBSection(
        buffer_dc,
        &bitmap_info,
        DIB_RGB_COLORS,
        &mut bits,
        HANDLE::default(),
        0,
    ) else {
        let _ = DeleteDC(buffer_dc);
        draw_opaque_text_fallback(dc, text, rect, flags, color, background);
        return;
    };
    let old_bitmap = SelectObject(buffer_dc, bitmap);
    let background_red = (background.0 & 0xff) as u8;
    let background_green = ((background.0 >> 8) & 0xff) as u8;
    let background_blue = ((background.0 >> 16) & 0xff) as u8;
    let byte_len = width as usize * height as usize * 4;
    for pixel in std::slice::from_raw_parts_mut(bits.cast::<u8>(), byte_len).chunks_exact_mut(4) {
        pixel[0] = background_blue;
        pixel[1] = background_green;
        pixel[2] = background_red;
        pixel[3] = 255;
    }
    let font = GetCurrentObject(dc, OBJ_FONT);
    let old_font = (!font.is_invalid()).then(|| SelectObject(buffer_dc, font));
    let _ = SetBkMode(buffer_dc, OPAQUE);
    let _ = SetBkColor(buffer_dc, background);
    let _ = SetTextColor(buffer_dc, color);
    let mut local_rect = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };
    let mut native_text = text.to_vec();
    let _ = DrawTextW(buffer_dc, &mut native_text, &mut local_rect, flags);
    let _ = GdiFlush();
    for pixel in std::slice::from_raw_parts_mut(bits.cast::<u8>(), byte_len).chunks_exact_mut(4) {
        pixel[3] = 255;
    }
    let _ = StretchDIBits(
        dc,
        rect.left,
        rect.top,
        width,
        height,
        0,
        0,
        width,
        height,
        Some(bits.cast_const()),
        &bitmap_info,
        DIB_RGB_COLORS,
        SRCCOPY,
    );
    if let Some(old_font) = old_font {
        let _ = SelectObject(buffer_dc, old_font);
    }
    let _ = SelectObject(buffer_dc, old_bitmap);
    let _ = DeleteObject(bitmap);
    let _ = DeleteDC(buffer_dc);
}

unsafe fn draw_opaque_text_fallback(
    dc: HDC,
    text: &[u16],
    rect: &mut RECT,
    flags: DRAW_TEXT_FORMAT,
    color: COLORREF,
    background: COLORREF,
) {
    let _ = SetBkMode(dc, OPAQUE);
    let _ = SetBkColor(dc, background);
    let _ = SetTextColor(dc, color);
    let mut native_text = text.to_vec();
    let _ = DrawTextW(dc, &mut native_text, rect, flags);
}

unsafe fn draw_text_fallback(
    dc: HDC,
    text: &[u16],
    rect: &mut RECT,
    flags: DRAW_TEXT_FORMAT,
    color: COLORREF,
) {
    if text.is_empty() {
        return;
    }
    let mut fallback = text.to_vec();
    let _ = SetBkMode(dc, TRANSPARENT);
    let _ = SetTextColor(dc, color);
    let _ = DrawTextW(dc, &mut fallback, rect, flags);
}

fn top_down_bgra_bitmap_info(width: i32, height: i32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (width * height * 4) as u32,
            ..Default::default()
        },
        ..Default::default()
    }
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
    let radius = ((height * 5 + 8) / 16).clamp(2, (height / 2).max(2));
    let inner_width = (width - 2).max(0);
    let filled = if total == 0 {
        0
    } else {
        ((inner_width as u64).saturating_mul(completed.min(total)) / total) as i32
    };
    let color = match role {
        ProgressRole::Normal | ProgressRole::Success => palette.progress,
        ProgressRole::Error => rgb(196, 43, 28),
        ProgressRole::Paused => rgb(247, 153, 52),
    };
    let pixels = render_progress_pixels(width, height, radius, filled, color, palette);
    let info = top_down_bgra_bitmap_info(width, height);
    let _ = StretchDIBits(
        dc,
        rect.left,
        rect.top,
        width,
        height,
        0,
        0,
        width,
        height,
        Some(pixels.as_ptr().cast()),
        &info,
        DIB_RGB_COLORS,
        SRCCOPY,
    );
}

fn render_progress_pixels(
    width: i32,
    height: i32,
    radius: i32,
    filled: i32,
    fill_color: COLORREF,
    palette: Palette,
) -> Vec<u8> {
    const SAMPLE_GRID: usize = 4;
    let colors = [
        colorref_rgb(palette.window),
        colorref_rgb(palette.border),
        colorref_rgb(palette.edit),
        colorref_rgb(fill_color),
    ];
    let mut pixels = vec![0_u8; width as usize * height as usize * 4];
    let sample_count = (SAMPLE_GRID * SAMPLE_GRID) as u32;
    for y in 0..height as usize {
        for x in 0..width as usize {
            let mut red = 0_u32;
            let mut green = 0_u32;
            let mut blue = 0_u32;
            for sample_y in 0..SAMPLE_GRID {
                for sample_x in 0..SAMPLE_GRID {
                    let px = x as f64 + (sample_x as f64 + 0.5) / SAMPLE_GRID as f64;
                    let py = y as f64 + (sample_y as f64 + 0.5) / SAMPLE_GRID as f64;
                    let color =
                        colors[progress_sample_layer(px, py, width, height, radius, filled)];
                    red += u32::from(color.0);
                    green += u32::from(color.1);
                    blue += u32::from(color.2);
                }
            }
            let offset = (y * width as usize + x) * 4;
            pixels[offset] = ((blue + sample_count / 2) / sample_count) as u8;
            pixels[offset + 1] = ((green + sample_count / 2) / sample_count) as u8;
            pixels[offset + 2] = ((red + sample_count / 2) / sample_count) as u8;
            pixels[offset + 3] = 255;
        }
    }
    pixels
}

fn progress_sample_layer(
    x: f64,
    y: f64,
    width: i32,
    height: i32,
    radius: i32,
    filled: i32,
) -> usize {
    if !point_in_rounded_rect(x, y, 0.0, 0.0, width as f64, height as f64, radius as f64) {
        return 0;
    }
    let inner_right = (width - 1).max(1) as f64;
    let inner_bottom = (height - 1).max(1) as f64;
    if !point_in_rounded_rect(
        x,
        y,
        1.0,
        1.0,
        inner_right,
        inner_bottom,
        radius.saturating_sub(1) as f64,
    ) {
        return 1;
    }
    if filled > 0 {
        let fill_right = (1 + filled).min(width - 1).max(1) as f64;
        let fill_radius = radius
            .saturating_sub(1)
            .min(filled / 2)
            .min((height - 2).max(0) / 2) as f64;
        if point_in_rounded_rect(x, y, 1.0, 1.0, fill_right, inner_bottom, fill_radius) {
            return 3;
        }
    }
    2
}

fn point_in_rounded_rect(
    x: f64,
    y: f64,
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
    radius: f64,
) -> bool {
    if x < left || x >= right || y < top || y >= bottom {
        return false;
    }
    let radius = radius.max(0.0).min((right - left).min(bottom - top) / 2.0);
    if radius == 0.0 {
        return true;
    }
    let nearest_x = x.clamp(left + radius, right - radius);
    let nearest_y = y.clamp(top + radius, bottom - radius);
    (x - nearest_x).powi(2) + (y - nearest_y).powi(2) <= radius * radius
}

fn colorref_rgb(color: COLORREF) -> (u8, u8, u8) {
    (
        (color.0 & 0xff) as u8,
        ((color.0 >> 8) & 0xff) as u8,
        ((color.0 >> 16) & 0xff) as u8,
    )
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
    try_fill_round_rect_opaque_gdi(dc, rect, radius, fill, border, background)
}

unsafe fn try_fill_round_rect_opaque_gdi(
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
    let high_width = width.saturating_mul(SCALE);
    let high_height = height.saturating_mul(SCALE);
    let memory_dc = CreateCompatibleDC(dc);
    if memory_dc.is_invalid() {
        return false;
    }
    let bitmap = CreateCompatibleBitmap(dc, high_width, high_height);
    if bitmap.is_invalid() {
        let _ = DeleteDC(memory_dc);
        return false;
    }
    let old_bitmap = SelectObject(memory_dc, bitmap);
    let high_rect = RECT {
        left: 0,
        top: 0,
        right: high_width,
        bottom: high_height,
    };
    let background_brush = CreateSolidBrush(background);
    let _ = FillRect(memory_dc, &high_rect, background_brush);
    let _ = DeleteObject(background_brush);
    draw_high_resolution_round_rect(memory_dc, high_rect, radius, fill, border, SCALE);
    let _ = SetStretchBltMode(dc, HALFTONE);
    let copied = StretchBlt(
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
    )
    .as_bool();
    let _ = SelectObject(memory_dc, old_bitmap);
    let _ = DeleteObject(bitmap);
    let _ = DeleteDC(memory_dc);
    copied
}

unsafe fn draw_high_resolution_round_rect(
    dc: HDC,
    rect: RECT,
    radius: i32,
    fill: COLORREF,
    border: COLORREF,
    scale: i32,
) {
    let brush = CreateSolidBrush(fill);
    let pen = CreatePen(PEN_STYLE(0), scale, border);
    let old_brush = SelectObject(dc, brush);
    let old_pen = SelectObject(dc, pen);
    let pen_inset = scale / 2;
    let diameter = radius.max(0).saturating_mul(2).saturating_mul(scale);
    let _ = RoundRect(
        dc,
        rect.left + pen_inset,
        rect.top + pen_inset,
        rect.right - pen_inset,
        rect.bottom - pen_inset,
        diameter,
        diameter,
    );
    let _ = SelectObject(dc, old_pen);
    let _ = SelectObject(dc, old_brush);
    let _ = DeleteObject(pen);
    let _ = DeleteObject(brush);
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

/// Paints an eight-sample-per-axis rounded frame over only the boundary of an already painted native
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
    const SAMPLES: i32 = 8;
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
            let sample_count = (SAMPLES * SAMPLES) as u32;
            if let Some(color) =
                deterministic_corner_color(interior, border, exterior, inner, outer, sample_count)
            {
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

/// Creates a non-interactive sibling frame and keeps the real single-line Edit centred inside it.
///
/// The Edit remains a direct child of the page with its original control id, so USER32 continues to
/// own text, notifications, focus, selection, caret, IME and accessibility. The sibling owns only
/// the full-height field surface; it never proxies application messages and therefore cannot turn
/// a page layout width into the zero-width nested Edit regression that a parent wrapper caused.
pub(crate) unsafe fn center_single_line_edit_in_row(hwnd: HWND) {
    if single_line_edit_frame(hwnd).is_none() {
        create_single_line_edit_frame(hwnd);
    }
    let _ = SetWindowSubclass(
        hwnd,
        Some(single_line_edit_layout_proc),
        SINGLE_LINE_EDIT_LAYOUT_SUBCLASS_ID,
        0,
    );
}

pub(crate) unsafe fn single_line_edit_frame(edit: HWND) -> Option<HWND> {
    let handle = GetPropW(edit, SINGLE_LINE_EDIT_FRAME_PROPERTY);
    if handle.is_invalid() {
        return None;
    }
    let frame = HWND(handle.0);
    let owner = GetPropW(frame, SINGLE_LINE_EDIT_OWNER_PROPERTY);
    if !IsWindow(frame).as_bool() || owner.is_invalid() || owner.0 != edit.0 {
        let _ = RemovePropW(edit, SINGLE_LINE_EDIT_FRAME_PROPERTY);
        return None;
    }
    Some(frame)
}

pub(crate) unsafe fn single_line_edit_frame_owner(frame: HWND) -> Option<HWND> {
    let handle = GetPropW(frame, SINGLE_LINE_EDIT_OWNER_PROPERTY);
    if handle.is_invalid() {
        return None;
    }
    let edit = HWND(handle.0);
    let linked_frame = GetPropW(edit, SINGLE_LINE_EDIT_FRAME_PROPERTY);
    (IsWindow(edit).as_bool() && !linked_frame.is_invalid() && linked_frame.0 == frame.0)
        .then_some(edit)
}

/// Creates a fixed sibling frame for a native report ListView.
///
/// Comctl32 scrolls a report by copying pixels inside the ListView client surface. A frame painted
/// into that surface is therefore copied into the rows while a scrollbar thumb is moving. The
/// sibling owns only the non-scrolling field surface; the real ListView keeps its original parent,
/// control id, notifications, selection, keyboard handling and accessibility implementation.
pub(crate) unsafe fn ensure_list_view_frame(list: HWND) -> Option<HWND> {
    if let Some(frame) = list_view_frame(list) {
        return Some(frame);
    }
    let parent = GetParent(list).ok()?;
    let frame = CreateWindowExW(
        WINDOW_EX_STYLE(0x0000_0004), // WS_EX_NOPARENTNOTIFY
        w!("STATIC"),
        w!(""),
        // The page router has already hidden non-current ListViews before theming runs. Creating
        // every sibling visible here exposes an otherwise hidden page as a large blank STATIC.
        // Publish visibility only after the owner link and geometry are complete.
        WS_CHILD | WS_CLIPSIBLINGS,
        0,
        0,
        0,
        0,
        parent,
        HMENU::default(),
        HINSTANCE::default(),
        None,
    )
    .ok()?;
    let _ = SetWindowTheme(frame, w!(""), w!(""));
    if SetPropW(list, LIST_VIEW_FRAME_PROPERTY, HANDLE(frame.0)).is_err()
        || SetPropW(frame, LIST_VIEW_OWNER_PROPERTY, HANDLE(list.0)).is_err()
    {
        let _ = RemovePropW(list, LIST_VIEW_FRAME_PROPERTY);
        let _ = RemovePropW(frame, LIST_VIEW_OWNER_PROPERTY);
        let _ = DestroyWindow(frame);
        return None;
    }
    let _ = SetWindowSubclass(
        list,
        Some(list_view_layout_proc),
        LIST_VIEW_LAYOUT_SUBCLASS_ID,
        0,
    );
    let _ = SetWindowPos(
        frame,
        list,
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
    );
    if let Some(outer) = control_bounds_in_parent(list) {
        layout_list_view_in_frame(list, outer);
    }
    let visible = window_style_is_visible(GetWindowLongPtrW(list, GWL_STYLE));
    let _ = ShowWindow(frame, if visible { SW_SHOW } else { SW_HIDE });
    Some(frame)
}

const fn window_style_is_visible(style: isize) -> bool {
    style as u32 & WS_VISIBLE.0 != 0
}

pub(crate) unsafe fn list_view_frame(list: HWND) -> Option<HWND> {
    let handle = GetPropW(list, LIST_VIEW_FRAME_PROPERTY);
    if handle.is_invalid() {
        return None;
    }
    let frame = HWND(handle.0);
    let owner = GetPropW(frame, LIST_VIEW_OWNER_PROPERTY);
    if !IsWindow(frame).as_bool() || owner.is_invalid() || owner.0 != list.0 {
        let _ = RemovePropW(list, LIST_VIEW_FRAME_PROPERTY);
        return None;
    }
    Some(frame)
}

unsafe fn create_single_line_edit_frame(edit: HWND) {
    let Ok(parent) = GetParent(edit) else {
        return;
    };
    let Ok(frame) = CreateWindowExW(
        WINDOW_EX_STYLE(0x0000_0004), // WS_EX_NOPARENTNOTIFY
        w!("STATIC"),
        w!(""),
        WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
        0,
        0,
        0,
        0,
        parent,
        HMENU::default(),
        HINSTANCE::default(),
        None,
    ) else {
        return;
    };
    let _ = SetWindowTheme(frame, w!(""), w!(""));
    if SetPropW(edit, SINGLE_LINE_EDIT_FRAME_PROPERTY, HANDLE(frame.0)).is_err()
        || SetPropW(frame, SINGLE_LINE_EDIT_OWNER_PROPERTY, HANDLE(edit.0)).is_err()
    {
        let _ = RemovePropW(edit, SINGLE_LINE_EDIT_FRAME_PROPERTY);
        let _ = RemovePropW(frame, SINGLE_LINE_EDIT_OWNER_PROPERTY);
        let _ = DestroyWindow(frame);
        return;
    }
    let _ = SetWindowPos(
        frame,
        edit,
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SingleLineEditInnerBounds {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

fn single_line_edit_inner_bounds(
    outer_width: i32,
    outer_height: i32,
    font_height: i32,
    inset: i32,
) -> SingleLineEditInnerBounds {
    let outer_width = outer_width.max(0);
    let outer_height = outer_height.max(0);
    let inset = inset.max(0).min(outer_width / 2).min(outer_height / 2);
    let available_width = (outer_width - inset * 2).max(0);
    let available_height = (outer_height - inset * 2).max(0);
    let height = font_height.max(1).min(available_height);
    let spare = available_height.saturating_sub(height);
    SingleLineEditInnerBounds {
        x: inset,
        // Bias an odd spare pixel downward. Microsoft YaHei UI has more visible descent than
        // ascent whitespace; ordinary floor division recreates the reported top-heavy result.
        y: inset + (spare + 1) / 2,
        width: available_width,
        height,
    }
}

unsafe fn single_line_edit_font_height(edit: HWND, dpi: u32) -> i32 {
    let fallback = ((15i64 * i64::from(dpi.max(1)) + 48) / 96) as i32;
    let dc = GetDC(edit);
    if dc.is_invalid() {
        return fallback.max(1);
    }
    let font = SendMessageW(edit, WM_GETFONT, WPARAM(0), LPARAM(0));
    let old_font = (font.0 != 0)
        .then(|| SelectObject(dc, windows::Win32::Graphics::Gdi::HGDIOBJ(font.0 as *mut _)));
    let mut metrics = windows::Win32::Graphics::Gdi::TEXTMETRICW::default();
    let measured = GetTextMetricsW(dc, &mut metrics).as_bool();
    if let Some(old_font) = old_font {
        let _ = SelectObject(dc, old_font);
    }
    let _ = ReleaseDC(edit, dc);
    if measured {
        metrics.tmHeight.max(1)
    } else {
        fallback.max(1)
    }
}

unsafe fn set_single_line_edit_margins(edit: HWND, dpi: u32) {
    const EM_SETMARGINS: u32 = 0x00d3;
    const EC_LEFTMARGIN: usize = 0x0001;
    const EC_RIGHTMARGIN: usize = 0x0002;
    let margin = ((4i64 * i64::from(dpi.max(1)) + 48) / 96).clamp(1, i64::from(u16::MAX)) as u16;
    let packed = u32::from(margin) | (u32::from(margin) << 16);
    let _ = SendMessageW(
        edit,
        EM_SETMARGINS,
        WPARAM(EC_LEFTMARGIN | EC_RIGHTMARGIN),
        LPARAM(packed as isize),
    );
}

unsafe fn frame_bounds_in_parent(frame: HWND) -> Option<RECT> {
    let Ok(parent) = GetParent(frame) else {
        return None;
    };
    let mut window = RECT::default();
    GetWindowRect(frame, &mut window).ok()?;
    let mut top_left = POINT {
        x: window.left,
        y: window.top,
    };
    let mut bottom_right = POINT {
        x: window.right,
        y: window.bottom,
    };
    if !ScreenToClient(parent, &mut top_left).as_bool()
        || !ScreenToClient(parent, &mut bottom_right).as_bool()
    {
        return None;
    }
    Some(RECT {
        left: top_left.x,
        top: top_left.y,
        right: bottom_right.x,
        bottom: bottom_right.y,
    })
}

unsafe fn control_bounds_in_parent(control: HWND) -> Option<RECT> {
    let parent = GetParent(control).ok()?;
    let mut window = RECT::default();
    GetWindowRect(control, &mut window).ok()?;
    let mut top_left = POINT {
        x: window.left,
        y: window.top,
    };
    let mut bottom_right = POINT {
        x: window.right,
        y: window.bottom,
    };
    if !ScreenToClient(parent, &mut top_left).as_bool()
        || !ScreenToClient(parent, &mut bottom_right).as_bool()
    {
        return None;
    }
    Some(RECT {
        left: top_left.x,
        top: top_left.y,
        right: bottom_right.x,
        bottom: bottom_right.y,
    })
}

fn list_view_inner_bounds(width: i32, height: i32, dpi: u32) -> SingleLineEditInnerBounds {
    let width = width.max(0);
    let height = height.max(0);
    let inset = ((i64::from(dpi.max(1)) + 48) / 96) as i32;
    let inset = inset.max(1).min(width / 2).min(height / 2);
    SingleLineEditInnerBounds {
        x: inset,
        y: inset,
        width: (width - inset * 2).max(0),
        height: (height - inset * 2).max(0),
    }
}

unsafe fn layout_list_view_in_frame(list: HWND, outer: RECT) {
    let Some(frame) = list_view_frame(list) else {
        return;
    };
    let width = (outer.right - outer.left).max(0);
    let height = (outer.bottom - outer.top).max(0);
    let inner = list_view_inner_bounds(width, height, GetDpiForWindow(list).max(96));
    let _ = SetWindowPos(
        frame,
        list,
        outer.left,
        outer.top,
        width,
        height,
        SWP_NOACTIVATE,
    );
    if SetPropW(
        list,
        LIST_VIEW_INTERNAL_LAYOUT_PROPERTY,
        HANDLE(std::ptr::dangling_mut()),
    )
    .is_ok()
    {
        let _ = SetWindowPos(
            list,
            None,
            outer.left + inner.x,
            outer.top + inner.y,
            inner.width,
            inner.height,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
        let _ = RemovePropW(list, LIST_VIEW_INTERNAL_LAYOUT_PROPERTY);
    }
}

unsafe extern "system" fn list_view_layout_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    _reference_data: usize,
) -> LRESULT {
    match message {
        WM_WINDOWPOSCHANGING
            if lparam.0 != 0 && GetPropW(hwnd, LIST_VIEW_INTERNAL_LAYOUT_PROPERTY).is_invalid() =>
        {
            let position = &mut *(lparam.0 as *mut WINDOWPOS);
            if let Some(frame) = list_view_frame(hwnd) {
                let existing = frame_bounds_in_parent(frame).unwrap_or_default();
                let x = if position.flags.contains(SWP_NOMOVE) {
                    existing.left
                } else {
                    position.x
                };
                let y = if position.flags.contains(SWP_NOMOVE) {
                    existing.top
                } else {
                    position.y
                };
                let width = if position.flags.contains(SWP_NOSIZE) {
                    existing.right - existing.left
                } else {
                    position.cx
                };
                let height = if position.flags.contains(SWP_NOSIZE) {
                    existing.bottom - existing.top
                } else {
                    position.cy
                };
                let inner = list_view_inner_bounds(width, height, GetDpiForWindow(hwnd).max(96));
                let _ = SetWindowPos(frame, hwnd, x, y, width, height, SWP_NOACTIVATE);
                if !position.flags.contains(SWP_NOMOVE) {
                    position.x = x + inner.x;
                    position.y = y + inner.y;
                }
                if !position.flags.contains(SWP_NOSIZE) {
                    position.cx = inner.width;
                    position.cy = inner.height;
                }
            }
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_SHOWWINDOW => {
            if let Some(frame) = list_view_frame(hwnd) {
                let _ = ShowWindow(frame, if wparam.0 != 0 { SW_SHOW } else { SW_HIDE });
            }
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_ENABLE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if let Some(frame) = list_view_frame(hwnd) {
                let _ = InvalidateRect(frame, None, false);
            }
            result
        }
        0x02e3 => {
            // WM_DPICHANGED_AFTERPARENT: preserve the caller-owned outer rectangle while updating
            // the DPI-scaled non-scrolling inset.
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if let Some(frame) = list_view_frame(hwnd) {
                if let Some(outer) = frame_bounds_in_parent(frame) {
                    layout_list_view_in_frame(hwnd, outer);
                }
            }
            result
        }
        WM_NCDESTROY => {
            if let Some(frame) = list_view_frame(hwnd) {
                let _ = RemovePropW(hwnd, LIST_VIEW_FRAME_PROPERTY);
                let _ = RemovePropW(frame, LIST_VIEW_OWNER_PROPERTY);
                let _ = DestroyWindow(frame);
            }
            let _ = RemoveWindowSubclass(
                hwnd,
                Some(list_view_layout_proc),
                LIST_VIEW_LAYOUT_SUBCLASS_ID,
            );
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

unsafe fn layout_single_line_edit(edit: HWND, outer: RECT) {
    let Some(frame) = single_line_edit_frame(edit) else {
        return;
    };
    let width = (outer.right - outer.left).max(0);
    let height = (outer.bottom - outer.top).max(0);
    let dpi = GetDpiForWindow(edit).max(96);
    let inset = ((i64::from(dpi) + 48) / 96) as i32;
    let inner = single_line_edit_inner_bounds(
        width,
        height,
        single_line_edit_font_height(edit, dpi),
        inset.max(1),
    );
    set_single_line_edit_margins(edit, dpi);
    let _ = SetWindowPos(
        frame,
        edit,
        outer.left,
        outer.top,
        width,
        height,
        SWP_NOACTIVATE,
    );
    if SetPropW(
        edit,
        SINGLE_LINE_EDIT_INTERNAL_LAYOUT_PROPERTY,
        HANDLE(std::ptr::dangling_mut()),
    )
    .is_ok()
    {
        let _ = SetWindowPos(
            edit,
            None,
            outer.left + inner.x,
            outer.top + inner.y,
            inner.width,
            inner.height,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
        let _ = RemovePropW(edit, SINGLE_LINE_EDIT_INTERNAL_LAYOUT_PROPERTY);
    }
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
        WM_WINDOWPOSCHANGING
            if lparam.0 != 0
                && GetPropW(hwnd, SINGLE_LINE_EDIT_INTERNAL_LAYOUT_PROPERTY).is_invalid() =>
        {
            let position = &mut *(lparam.0 as *mut WINDOWPOS);
            if let Some(frame) = single_line_edit_frame(hwnd) {
                let existing = frame_bounds_in_parent(frame).unwrap_or_default();
                let outer = RECT {
                    left: if position.flags.contains(SWP_NOMOVE) {
                        existing.left
                    } else {
                        position.x
                    },
                    top: if position.flags.contains(SWP_NOMOVE) {
                        existing.top
                    } else {
                        position.y
                    },
                    right: 0,
                    bottom: 0,
                };
                let width = if position.flags.contains(SWP_NOSIZE) {
                    existing.right - existing.left
                } else {
                    position.cx
                };
                let height = if position.flags.contains(SWP_NOSIZE) {
                    existing.bottom - existing.top
                } else {
                    position.cy
                };
                if width > 0 && height > 0 {
                    let dpi = GetDpiForWindow(hwnd).max(96);
                    let inset = ((i64::from(dpi) + 48) / 96) as i32;
                    let inner = single_line_edit_inner_bounds(
                        width,
                        height,
                        single_line_edit_font_height(hwnd, dpi),
                        inset.max(1),
                    );
                    set_single_line_edit_margins(hwnd, dpi);
                    let _ = SetWindowPos(
                        frame,
                        hwnd,
                        outer.left,
                        outer.top,
                        width,
                        height,
                        SWP_NOACTIVATE,
                    );
                    if !position.flags.contains(SWP_NOMOVE) {
                        position.x = outer.left + inner.x;
                        position.y = outer.top + inner.y;
                    }
                    if !position.flags.contains(SWP_NOSIZE) {
                        position.cx = inner.width;
                        position.cy = inner.height;
                    }
                }
            }
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_SETFONT => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if let Some(frame) = single_line_edit_frame(hwnd) {
                if let Some(outer) = frame_bounds_in_parent(frame) {
                    layout_single_line_edit(hwnd, outer);
                }
            }
            result
        }
        WM_SHOWWINDOW => {
            if let Some(frame) = single_line_edit_frame(hwnd) {
                let _ = ShowWindow(frame, if wparam.0 != 0 { SW_SHOW } else { SW_HIDE });
            }
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_ENABLE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if let Some(frame) = single_line_edit_frame(hwnd) {
                let _ = InvalidateRect(frame, None, false);
            }
            result
        }
        0x02e3 => {
            // WM_DPICHANGED_AFTERPARENT: the outer layout remains authoritative, but font height
            // and the one-pixel visual inset must be recalculated for the child's new DPI.
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if let Some(frame) = single_line_edit_frame(hwnd) {
                if let Some(outer) = frame_bounds_in_parent(frame) {
                    layout_single_line_edit(hwnd, outer);
                }
            }
            result
        }
        WM_NCDESTROY => {
            if let Some(frame) = single_line_edit_frame(hwnd) {
                let _ = RemovePropW(hwnd, SINGLE_LINE_EDIT_FRAME_PROPERTY);
                let _ = RemovePropW(frame, SINGLE_LINE_EDIT_OWNER_PROPERTY);
                let _ = DestroyWindow(frame);
            }
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
    fn single_line_edit_centres_the_font_cell_inside_the_full_height_frame() {
        assert_eq!(
            single_line_edit_inner_bounds(200, 30, 21, 1),
            SingleLineEditInnerBounds {
                x: 1,
                y: 5,
                width: 198,
                height: 21,
            }
        );
        assert_eq!(
            single_line_edit_inner_bounds(400, 60, 42, 2),
            SingleLineEditInnerBounds {
                x: 2,
                y: 9,
                width: 396,
                height: 42,
            }
        );
        assert_eq!(
            single_line_edit_inner_bounds(100, 18, 21, 1),
            SingleLineEditInnerBounds {
                x: 1,
                y: 1,
                width: 98,
                height: 16,
            }
        );
        assert_eq!(
            single_line_edit_inner_bounds(0, 0, 21, 1),
            SingleLineEditInnerBounds {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            }
        );
    }

    #[test]
    fn list_view_frame_inset_scales_and_keeps_the_native_report_nonempty() {
        assert_eq!(
            list_view_inner_bounds(200, 100, 96),
            SingleLineEditInnerBounds {
                x: 1,
                y: 1,
                width: 198,
                height: 98,
            }
        );
        assert_eq!(
            list_view_inner_bounds(400, 200, 192),
            SingleLineEditInnerBounds {
                x: 2,
                y: 2,
                width: 396,
                height: 196,
            }
        );
        assert_eq!(
            list_view_inner_bounds(1, 1, 192),
            SingleLineEditInnerBounds {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            }
        );
    }

    #[test]
    fn list_view_sibling_frame_inherits_the_owner_style_visibility() {
        assert!(window_style_is_visible(WS_VISIBLE.0 as isize));
        assert!(window_style_is_visible((WS_CHILD | WS_VISIBLE).0 as isize));
        assert!(!window_style_is_visible(WS_CHILD.0 as isize));
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
        assert_eq!(secondary.fill, rgb(48, 48, 48));
        assert_eq!(secondary.border, rgb(61, 61, 61));
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

    #[test]
    fn ordinary_opaque_theme_buttons_keep_the_existing_palette_and_full_alpha() {
        let expected = button_visual(
            Palette::DARK,
            ButtonRole::Secondary,
            ControlState::default(),
        );
        let surface = button_surface_visual(
            Palette::DARK,
            ButtonRole::Secondary,
            ControlState::default(),
        );
        assert_eq!(surface.fill, expected.fill);
        assert_eq!(surface.border, expected.border);
        assert_eq!(surface.text, expected.text);
    }

    #[test]
    fn progress_raster_preserves_window_color_outside_rounded_track() {
        let pixels = render_progress_pixels(80, 16, 5, 20, Palette::DARK.progress, Palette::DARK);
        let (red, green, blue) = colorref_rgb(Palette::DARK.window);
        assert_eq!(&pixels[..4], &[blue, green, red, 255]);
        let fill_offset = (8 * 80 + 4) * 4;
        assert_ne!(
            &pixels[fill_offset..fill_offset + 4],
            &[blue, green, red, 255]
        );
    }
}
