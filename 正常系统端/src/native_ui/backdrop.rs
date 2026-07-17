//! Shared DWM Mica boundary for normal-endpoint top-level windows.
//!
//! Mica is deliberately the only supported experimental material.  Main and tool windows must
//! use the same request/reset sequence so their control palettes never disagree with DWM.

use std::mem::size_of;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmFlush, DwmGetWindowAttribute, DwmIsCompositionEnabled,
    DwmSetWindowAttribute, DWMSBT_AUTO, DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE,
    DWM_SYSTEMBACKDROP_TYPE,
};
use windows::Win32::UI::Controls::MARGINS;

/// Applies or removes full-client Mica and reports whether the material is active.
///
/// Both DWM calls are part of one logical request.  A partial failure is reset to the ordinary
/// non-material state before the error is returned, preventing a black glass client without a
/// matching system backdrop.
pub(crate) unsafe fn apply_mica(
    hwnd: HWND,
    enabled: bool,
    extend_into_client: bool,
) -> windows::core::Result<bool> {
    let composition_enabled = DwmIsCompositionEnabled()?;
    if !composition_enabled.as_bool() {
        return Ok(false);
    }

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

    if enabled {
        // A config value is only a request. Read back the public DWM attribute before allowing
        // child controls to use material contribution colours; unsupported/reduced shells can
        // accept part of the sequence without providing a full-client system backdrop.
        let mut actual = DWMSBT_AUTO;
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            (&mut actual as *mut DWM_SYSTEMBACKDROP_TYPE).cast(),
            size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
        )?;
        if actual != DWMSBT_MAINWINDOW {
            let automatic_title_bar = DWMSBT_AUTO;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_SYSTEMBACKDROP_TYPE,
                (&automatic_title_bar as *const DWM_SYSTEMBACKDROP_TYPE).cast(),
                size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
            );
            return Ok(false);
        }
    }

    let margins = if enabled && extend_into_client {
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
    Ok(enabled && extend_into_client)
}

/// Waits until DWM has presented this thread's queued backdrop/surface changes. Activation changes
/// otherwise return while old child surfaces and the new backdrop can still share one compositor
/// frame.
pub(crate) unsafe fn flush_composition() {
    let _ = DwmFlush();
}

/// Child HWNDs may use material contribution colours only while their own top-level window has a
/// confirmed DWM backdrop and is active. DWM itself keeps the Mica session alive and changes an
/// inactive window to its neutral fallback; only the child palette follows `window_active`.
pub(crate) const fn controls_use_mica(backdrop_available: bool, window_active: bool) -> bool {
    backdrop_available && window_active
}

/// Keep the supported backdrop session installed while only the client extension follows focus.
/// This avoids restarting DWMSBT_MAINWINDOW during every WM_NCACTIVATE transaction.
pub(crate) const fn mica_session_enabled(requested: bool, endpoint_supports_mica: bool) -> bool {
    requested && endpoint_supports_mica
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_experimental_material_boundary_uses_mica_or_system_title_bar_auto() {
        assert_eq!(DWMSBT_AUTO, DWM_SYSTEMBACKDROP_TYPE(0));
        assert_eq!(DWMSBT_MAINWINDOW, DWM_SYSTEMBACKDROP_TYPE(2));
    }

    #[test]
    fn controls_require_both_confirmed_dwm_material_and_an_active_window() {
        assert!(controls_use_mica(true, true));
        assert!(!controls_use_mica(true, false));
        assert!(!controls_use_mica(false, true));
        assert!(!controls_use_mica(false, false));
    }

    #[test]
    fn a_mica_session_requires_request_and_endpoint_support_not_activation() {
        assert!(mica_session_enabled(true, true));
        assert!(!mica_session_enabled(false, true));
        assert!(!mica_session_enabled(true, false));
    }
}
