//! Atomic native control-tree redraw transactions.
//!
//! The top-level window uses `WS_EX_COMPOSITED`, so suspending that root removes the complete tree
//! from visible painting while child visibility/layout changes are applied. One final RedrawWindow
//! publishes the fully composed descendants without toggling every child control's redraw state.

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{
    RedrawWindow, RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RDW_UPDATENOW,
};
use windows::Win32::UI::WindowsAndMessaging::{IsWindowVisible, SendMessageW, WM_SETREDRAW};

pub(crate) struct SuspendedRedrawTree;

/// Suspends a currently visible composited top-level window. Hidden startup windows intentionally
/// return `None`, because enabling redraw through DefWindowProc would show the root prematurely.
pub(crate) unsafe fn suspend(root: HWND) -> Option<SuspendedRedrawTree> {
    if !IsWindowVisible(root).as_bool() {
        return None;
    }
    let _ = SendMessageW(root, WM_SETREDRAW, WPARAM(0), LPARAM(0));
    Some(SuspendedRedrawTree)
}

/// Restores the root and synchronously publishes one composited non-client/client/child frame.
pub(crate) unsafe fn resume(root: HWND, transaction: Option<SuspendedRedrawTree>) {
    resume_with_flags(
        root,
        transaction,
        RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN | RDW_UPDATENOW,
    );
}

/// Navigation does not change the top-level frame or rely on class-background erasure: the main
/// WM_PAINT fills the complete client area. Avoiding redundant non-client/erase passes keeps a
/// page switch within one composited client paint.
pub(crate) unsafe fn resume_client(root: HWND, transaction: Option<SuspendedRedrawTree>) {
    resume_with_flags(
        root,
        transaction,
        RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_UPDATENOW,
    );
}

unsafe fn resume_with_flags(
    root: HWND,
    transaction: Option<SuspendedRedrawTree>,
    flags: windows::Win32::Graphics::Gdi::REDRAW_WINDOW_FLAGS,
) {
    let Some(_transaction) = transaction else {
        return;
    };
    let _ = SendMessageW(root, WM_SETREDRAW, WPARAM(1), LPARAM(0));
    let _ = RedrawWindow(root, None, None, flags);
}
