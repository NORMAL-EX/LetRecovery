//! Shared DWM Mica boundary for normal-endpoint top-level windows.
//!
//! Mica is deliberately the only supported experimental material.  Main and tool windows must
//! use the same request/reset sequence so their control palettes never disagree with DWM.

use std::mem::size_of;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMSBT_AUTO, DWMSBT_MAINWINDOW,
    DWMWA_SYSTEMBACKDROP_TYPE, DWM_SYSTEMBACKDROP_TYPE,
};
use windows::Win32::UI::Controls::MARGINS;

/// Applies or removes full-client Mica and reports whether the material is active.
///
/// Both DWM calls are part of one logical request.  A partial failure is reset to the ordinary
/// non-material state before the error is returned, preventing a black glass client without a
/// matching system backdrop.
pub(crate) unsafe fn apply_mica(hwnd: HWND, enabled: bool) -> windows::core::Result<bool> {
    // AUTO keeps the default Win32 title bar under DWM policy while leaving the client opaque.
    // MAINWINDOW is reserved for the explicit experimental full-window option.
    let backdrop = if enabled {
        DWMSBT_MAINWINDOW
    } else {
        DWMSBT_AUTO
    };
    DwmSetWindowAttribute(
        hwnd,
        DWMWA_SYSTEMBACKDROP_TYPE,
        (&backdrop as *const DWM_SYSTEMBACKDROP_TYPE).cast(),
        size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
    )?;

    let margins = if enabled {
        MARGINS {
            cxLeftWidth: -1,
            cxRightWidth: -1,
            cyTopHeight: -1,
            cyBottomHeight: -1,
        }
    } else {
        MARGINS::default()
    };
    if let Err(error) = DwmExtendFrameIntoClientArea(hwnd, &margins) {
        if enabled {
            let automatic_title_bar = DWMSBT_AUTO;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_SYSTEMBACKDROP_TYPE,
                (&automatic_title_bar as *const DWM_SYSTEMBACKDROP_TYPE).cast(),
                size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
            );
            let _ = DwmExtendFrameIntoClientArea(hwnd, &MARGINS::default());
        }
        return Err(error);
    }
    Ok(enabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_experimental_material_boundary_uses_mica_or_system_title_bar_auto() {
        assert_eq!(DWMSBT_AUTO, DWM_SYSTEMBACKDROP_TYPE(0));
        assert_eq!(DWMSBT_MAINWINDOW, DWM_SYSTEMBACKDROP_TYPE(2));
    }
}
