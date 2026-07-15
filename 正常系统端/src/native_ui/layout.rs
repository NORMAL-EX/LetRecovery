//! Shared responsive layout primitives for the native Win32 frontend.
//!
//! This module deliberately owns measurements and geometry only.  It never creates controls or
//! performs business actions, so tool dialogs can share one spacing and text-measurement contract
//! without becoming coupled to each other's state.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    DrawTextW, GetDC, ReleaseDC, SelectObject, DT_CALCRECT, DT_NOPREFIX, DT_SINGLELINE,
    DT_WORDBREAK, HFONT,
};
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

use super::controls::InnoMetrics;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextSize {
    pub width: i32,
    pub height: i32,
}

/// One DPI-scaled spacing contract for pages and tool dialogs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutMetrics {
    pub outer_margin: i32,
    pub tight_gap: i32,
    pub control_gap: i32,
    pub section_gap: i32,
    pub label_height: i32,
    pub field_height: i32,
    pub button_height: i32,
    pub list_row_height: i32,
    pub command_margin: i32,
    pub command_height: i32,
}

impl LayoutMetrics {
    pub fn for_dpi(dpi: u32) -> Self {
        let inno = InnoMetrics::for_dpi(dpi);
        Self {
            outer_margin: scale(28, dpi),
            tight_gap: scale(4, dpi),
            control_gap: inno.control_gap,
            section_gap: scale(16, dpi),
            label_height: scale(20, dpi),
            field_height: inno.field_height,
            button_height: inno.button_height,
            list_row_height: inno.list_item_height,
            command_margin: scale(12, dpi),
            command_height: scale(46, dpi),
        }
    }
}

/// Measures with the exact font installed on the target dialog.  Wrapped text receives a real
/// maximum width; no language-specific character-count estimate is used.
pub unsafe fn measure_text(
    hwnd: HWND,
    font: HFONT,
    text: &str,
    maximum_width: Option<i32>,
) -> TextSize {
    if text.is_empty() {
        return TextSize::default();
    }
    let dc = GetDC(hwnd);
    if dc.is_invalid() {
        return TextSize::default();
    }
    let old_font = SelectObject(dc, font);
    let mut wide = text.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let mut bounds = RECT {
        right: maximum_width.unwrap_or_default().max(0),
        ..Default::default()
    };
    let flags = if maximum_width.is_some() {
        DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX
    } else {
        DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX
    };
    let _ = DrawTextW(dc, &mut wide, &mut bounds, flags);
    let _ = SelectObject(dc, old_font);
    let _ = ReleaseDC(hwnd, dc);
    TextSize {
        width: (bounds.right - bounds.left).max(0),
        height: (bounds.bottom - bounds.top).max(0),
    }
}

pub unsafe fn measured_button_width(
    hwnd: HWND,
    font: HFONT,
    text: &str,
    dpi: u32,
    minimum: i32,
) -> i32 {
    let text_width = measure_text(hwnd, font, text, None).width;
    (text_width + scale(24, dpi)).max(minimum)
}

/// A list follows its inventory rather than consuming every unused pixel.  Empty inventories keep
/// a small usable body, ordinary inventories expose their rows, and larger inventories scroll.
pub fn preferred_list_height(
    item_count: usize,
    dpi: u32,
    minimum_rows: usize,
    maximum_rows: usize,
) -> i32 {
    let metrics = LayoutMetrics::for_dpi(dpi);
    let rows = item_count.clamp(minimum_rows.max(1), maximum_rows.max(minimum_rows.max(1)));
    // One header row plus a one-pixel logical frame at both edges.
    metrics.list_row_height * (rows as i32 + 1) + scale(2, dpi)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FieldArrangement {
    Inline {
        label_width: i32,
        control_x: i32,
        control_width: i32,
    },
    #[default]
    Stacked,
}

/// Keeps a label and field inline only while the measured label and useful field width both fit.
/// This naturally responds to long English text without a hard-coded language breakpoint.
pub fn arrange_field(
    available_width: i32,
    measured_label_width: i32,
    minimum_control_width: i32,
    dpi: u32,
) -> FieldArrangement {
    let gap = LayoutMetrics::for_dpi(dpi).control_gap;
    let label_width = measured_label_width.max(0);
    let control_x = label_width + gap;
    let control_width = available_width - control_x;
    if control_width >= minimum_control_width {
        FieldArrangement::Inline {
            label_width,
            control_x,
            control_width,
        }
    } else {
        FieldArrangement::Stacked
    }
}

pub fn scale(value: i32, dpi: u32) -> i32 {
    ((value as i64 * dpi.max(1) as i64 + 48) / 96) as i32
}

/// Returns the control's actual on-screen height. This matters for stock ComboBox controls:
/// `MoveWindow` receives the complete drop-down height, while USER32 independently chooses the
/// closed field height from the installed font and visual style.
pub unsafe fn control_height(control: HWND) -> Option<i32> {
    let mut rect = RECT::default();
    GetWindowRect(control, &mut rect)
        .ok()
        .map(|_| rect.bottom - rect.top)
        .filter(|height| *height > 0)
}

/// Centers an item of `item_height` inside a logical row without language- or DPI-specific
/// offsets. All values are already physical pixels.
pub fn centered_control_y(row_top: i32, row_height: i32, item_height: i32) -> i32 {
    row_top + (row_height.saturating_sub(item_height).max(0) / 2)
}

/// Centers an item while assigning an odd spare pixel to the top edge.
///
/// USER32 single-line fields and GDI text both otherwise leave that pixel below the item, which
/// makes a 23px field look one pixel higher than the neighbouring 24px control on 96-DPI layouts.
/// Keep this opt-in for mixed-height field rows instead of changing the geometry of every control.
pub fn centered_control_y_ceil(row_top: i32, row_height: i32, item_height: i32) -> i32 {
    let spare = row_height.saturating_sub(item_height).max(0);
    row_top + (spare + 1) / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_scale_once_and_keep_one_spacing_contract() {
        let normal = LayoutMetrics::for_dpi(96);
        let high = LayoutMetrics::for_dpi(192);
        assert_eq!(normal.control_gap, 8);
        assert_eq!(normal.section_gap, 16);
        assert_eq!(high.control_gap, 16);
        assert_eq!(high.section_gap, 32);
        assert_eq!(high.field_height, normal.field_height * 2);
    }

    #[test]
    fn long_labels_stack_instead_of_squeezing_the_field() {
        assert!(matches!(
            arrange_field(500, 100, 240, 96),
            FieldArrangement::Inline { .. }
        ));
        assert_eq!(arrange_field(360, 180, 240, 96), FieldArrangement::Stacked);
    }

    #[test]
    fn list_height_tracks_inventory_with_bounded_density() {
        assert_eq!(preferred_list_height(0, 96, 3, 8), 90);
        assert_eq!(preferred_list_height(5, 96, 3, 8), 134);
        assert_eq!(preferred_list_height(80, 96, 3, 8), 200);
    }

    #[test]
    fn mixed_height_field_rows_put_the_odd_pixel_above_the_field() {
        assert_eq!(centered_control_y(100, 24, 23), 100);
        assert_eq!(centered_control_y_ceil(100, 24, 23), 101);
        assert_eq!(centered_control_y_ceil(100, 24, 24), 100);
    }
}
