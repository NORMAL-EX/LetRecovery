//! Shared DWM Mica boundary for normal-endpoint top-level windows.
//!
//! Mica is deliberately the only supported experimental material.  Main and tool windows must
//! use the same request/reset sequence so their control palettes never disagree with DWM.

use std::mem::size_of;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMSBT_MAINWINDOW, DWMSBT_NONE,
    DWMWA_SYSTEMBACKDROP_TYPE, DWM_SYSTEMBACKDROP_TYPE,
};
use windows::Win32::UI::Controls::MARGINS;

/// Applies or removes full-client Mica and reports whether the material is active.
///
/// Both DWM calls are part of one logical request.  A partial failure is reset to the ordinary
/// non-material state before the error is returned, preventing a black glass client without a
/// matching system backdrop.
pub(crate) unsafe fn apply_mica(hwnd: HWND, enabled: bool) -> windows::core::Result<bool> {
    let backdrop = if enabled {
        DWMSBT_MAINWINDOW
    } else {
        DWMSBT_NONE
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
            let none = DWMSBT_NONE;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_SYSTEMBACKDROP_TYPE,
                (&none as *const DWM_SYSTEMBACKDROP_TYPE).cast(),
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
    fn the_experimental_material_boundary_only_uses_mica_or_none() {
        assert_eq!(DWMSBT_MAINWINDOW, DWM_SYSTEMBACKDROP_TYPE(2));
        assert_eq!(DWMSBT_NONE, DWM_SYSTEMBACKDROP_TYPE(1));
    }
}
