//! Atomic native control-tree redraw transactions.
//!
//! A page switch freezes only the visible top-level window while existing child visibility and
//! layout code establishes the final state. Freezing every child separately makes USER32 remove
//! and restore each child's `WS_VISIBLE` bit, which exposes empty redirected STATIC surfaces to
//! DWM. One erased root `RedrawWindow` publishes the completed tree. The top level deliberately
//! does not use `WS_EX_COMPOSITED`: that legacy compositor turns an Edit caret repaint into an
//! observable bottom-to-top repaint of unrelated controls.

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{
    RedrawWindow, RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RDW_UPDATENOW,
};
use windows::Win32::UI::WindowsAndMessaging::{IsWindowVisible, SendMessageW, WM_SETREDRAW};

pub(crate) struct SuspendedRedraw;

/// Suspends a currently visible composited top-level window. Hidden startup windows intentionally
/// return `None`, because enabling redraw through DefWindowProc would show the root prematurely.
pub(crate) unsafe fn suspend(root: HWND) -> Option<SuspendedRedraw> {
    if !IsWindowVisible(root).as_bool() {
        return None;
    }
    let _ = SendMessageW(root, WM_SETREDRAW, WPARAM(0), LPARAM(0));
    Some(SuspendedRedraw)
}

/// Restores the root and synchronously publishes one composited non-client/client/child frame.
pub(crate) unsafe fn resume(root: HWND, transaction: Option<SuspendedRedraw>) {
    resume_with_flags(
        root,
        transaction,
        RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN | RDW_UPDATENOW,
    );
}

/// Navigation publishes one erased client frame. `RDW_ERASE` is required because page controls
/// move or disappear while redraw is suspended; without it, the newly exposed parent pixels can
/// retain stale field backdrops and rounded frames. The client publish stays asynchronous so a
/// navigation click does not block on a complete tree paint; USER32 coalesces repeated switches
/// and DWM keeps the previous complete frame until the new one is ready.
pub(crate) unsafe fn resume_client(root: HWND, transaction: Option<SuspendedRedraw>) {
    resume_with_flags(
        root,
        transaction,
        RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
    );
}

unsafe fn resume_with_flags(
    root: HWND,
    transaction: Option<SuspendedRedraw>,
    flags: windows::Win32::Graphics::Gdi::REDRAW_WINDOW_FLAGS,
) {
    if transaction.is_none() {
        return;
    }
    let _ = SendMessageW(root, WM_SETREDRAW, WPARAM(1), LPARAM(0));
    let _ = RedrawWindow(root, None, None, flags);
}
