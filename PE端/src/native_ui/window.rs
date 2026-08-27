use std::mem::size_of;

use windows::Win32::Foundation::{BOOL, HWND, RECT};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, GetWindowRect, PeekMessageW, SetWindowPos, MSG, PM_REMOVE, SM_CXSCREEN,
    SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOZORDER, WM_QUIT,
};

use super::layout::{centered_rect_in_work_area, clamp_rect_to_work_area, PixelRect};
use super::theme::ThemeMode;

pub(crate) unsafe fn drain_pending_quit_message() {
    let mut message = MSG::default();
    while PeekMessageW(&mut message, None, WM_QUIT, WM_QUIT, PM_REMOVE).as_bool() {}
}

pub(crate) unsafe fn monitor_work_area(hwnd: HWND) -> RECT {
    let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !monitor.is_invalid() && GetMonitorInfoW(monitor, &mut info).as_bool() {
        info.rcWork
    } else {
        RECT {
            left: 0,
            top: 0,
            right: GetSystemMetrics(SM_CXSCREEN).max(1),
            bottom: GetSystemMetrics(SM_CYSCREEN).max(1),
        }
    }
}

pub(crate) unsafe fn fit_window_to_work_area(
    hwnd: HWND,
    preferred_width: i32,
    preferred_height: i32,
    dpi: u32,
) {
    let work = monitor_work_area(hwnd);
    let work = PixelRect {
        x: work.left,
        y: work.top,
        width: (work.right - work.left).max(1),
        height: (work.bottom - work.top).max(1),
    };
    let fitted = centered_rect_in_work_area(
        scaled(preferred_width, dpi),
        scaled(preferred_height, dpi),
        work,
    );
    let _ = SetWindowPos(
        hwnd,
        HWND::default(),
        fitted.x,
        fitted.y,
        fitted.width,
        fitted.height,
        SWP_NOACTIVATE | SWP_NOZORDER,
    );
}

pub(crate) unsafe fn clamp_window_to_work_area(hwnd: HWND) {
    let work = monitor_work_area(hwnd);
    let mut current = RECT::default();
    if GetWindowRect(hwnd, &mut current).is_err() {
        return;
    }
    let fitted = clamp_rect_to_work_area(
        PixelRect {
            x: current.left,
            y: current.top,
            width: current.right - current.left,
            height: current.bottom - current.top,
        },
        PixelRect {
            x: work.left,
            y: work.top,
            width: work.right - work.left,
            height: work.bottom - work.top,
        },
    );
    let _ = SetWindowPos(
        hwnd,
        HWND::default(),
        fitted.x,
        fitted.y,
        fitted.width,
        fitted.height,
        SWP_NOACTIVATE | SWP_NOZORDER,
    );
}

pub(crate) fn scaled(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32
}

pub(crate) unsafe fn apply_title_bar_theme(hwnd: HWND, mode: ThemeMode) {
    let enabled = BOOL::from(matches!(mode, ThemeMode::Dark));
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_USE_IMMERSIVE_DARK_MODE,
        (&enabled as *const BOOL).cast(),
        size_of::<BOOL>() as u32,
    );
}
