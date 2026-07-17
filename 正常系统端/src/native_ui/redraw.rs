//! Atomic native control-tree redraw transactions.
//!
//! A page switch freezes the top-level window and its currently visible descendants while child
//! visibility/layout changes are applied. One final `RedrawWindow` publishes the completed tree.
//! The top level deliberately does not use `WS_EX_COMPOSITED`: that legacy whole-tree compositor
//! turns an Edit caret repaint into an observable bottom-to-top repaint of unrelated controls.

use windows::Win32::Foundation::{BOOL, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{
    RedrawWindow, RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RDW_UPDATENOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetWindowLongPtrW, IsWindowVisible, SendMessageW, SetWindowLongPtrW,
    ShowWindow, GWL_STYLE, SW_HIDE, WM_SETREDRAW, WS_VISIBLE,
};

pub(crate) struct SuspendedRedrawTree {
    visible_descendants: Vec<HWND>,
}

unsafe extern "system" fn collect_visible_descendant(hwnd: HWND, lparam: LPARAM) -> BOOL {
    if IsWindowVisible(hwnd).as_bool() {
        let descendants = &mut *(lparam.0 as *mut Vec<HWND>);
        descendants.push(hwnd);
    }
    BOOL(1)
}

/// Suspends a currently visible composited top-level window. Hidden startup windows intentionally
/// return `None`, because enabling redraw through DefWindowProc would show the root prematurely.
pub(crate) unsafe fn suspend(root: HWND) -> Option<SuspendedRedrawTree> {
    if !IsWindowVisible(root).as_bool() {
        return None;
    }
    // WM_SETREDRAW is per HWND. Record visibility before freezing the root, because
    // IsWindowVisible includes ancestor visibility and becomes false for every descendant after
    // DefWindowProc temporarily removes the root's WS_VISIBLE bit.
    let mut visible_descendants = Vec::new();
    let _ = EnumChildWindows(
        root,
        Some(collect_visible_descendant),
        LPARAM((&mut visible_descendants as *mut Vec<HWND>) as isize),
    );
    let _ = SendMessageW(root, WM_SETREDRAW, WPARAM(0), LPARAM(0));
    // Freeze the currently published child surfaces while the top-level compositor still retains
    // their last complete pixels. Hidden destination-page controls need no freeze: ShowWindow only
    // invalidates them during this message, and the final synchronous root redraw paints them.
    for descendant in &visible_descendants {
        let _ = SendMessageW(*descendant, WM_SETREDRAW, WPARAM(0), LPARAM(0));
        // Keep the logical visibility bit as the page transaction's source of truth while the
        // independent SysSetRedraw flag blocks painting. A later ShowWindow(SW_HIDE) can then be
        // distinguished from a control that is meant to remain visible.
        let style = GetWindowLongPtrW(*descendant, GWL_STYLE);
        let _ = SetWindowLongPtrW(*descendant, GWL_STYLE, style | WS_VISIBLE.0 as isize);
    }
    Some(SuspendedRedrawTree {
        visible_descendants,
    })
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
    let Some(transaction) = transaction else {
        return;
    };
    // Restore the child HWNDs while the root is still frozen, then expose exactly one composed
    // root frame. Only controls that were visible before the transaction are re-enabled, so a
    // hidden page cannot be accidentally shown by DefWindowProc's WM_SETREDRAW(TRUE) behaviour.
    for descendant in &transaction.visible_descendants {
        let should_remain_visible =
            GetWindowLongPtrW(*descendant, GWL_STYLE) & WS_VISIBLE.0 as isize != 0;
        let _ = SendMessageW(*descendant, WM_SETREDRAW, WPARAM(1), LPARAM(0));
        if !should_remain_visible {
            let _ = ShowWindow(*descendant, SW_HIDE);
        }
    }
    let _ = SendMessageW(root, WM_SETREDRAW, WPARAM(1), LPARAM(0));
    let _ = RedrawWindow(root, None, None, flags);
}
