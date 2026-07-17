use std::mem::size_of;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{BOOL, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, DeleteObject, EndPaint, FillRect, GetMonitorInfoW, MonitorFromWindow, SetBkColor,
    SetBkMode, SetTextColor, HBRUSH, HDC, HFONT, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    PAINTSTRUCT, RDW_ALLCHILDREN, RDW_FRAME, RDW_INVALIDATE, RDW_UPDATENOW, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, DRAWITEMSTRUCT, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, LoadCursorW, LoadImageW, MoveWindow,
    PeekMessageW, PostQuitMessage, RegisterClassExW, SendMessageW, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, TranslateMessage, BN_CLICKED, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT,
    GWLP_USERDATA, HICON, HMENU, ICON_BIG, ICON_SMALL, IDC_ARROW, IMAGE_ICON, LR_SHARED,
    MINMAXINFO, MSG, PM_REMOVE, SM_CXICON, SM_CXSCREEN, SM_CXSMICON, SM_CYICON, SM_CYSCREEN,
    SM_CYSMICON, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE,
    WM_COMMAND, WM_CREATE, WM_CTLCOLORBTN, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DPICHANGED,
    WM_DRAWITEM, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_NCCREATE, WM_PAINT, WM_QUIT, WM_SETFONT,
    WM_SETICON, WM_SETTINGCHANGE, WM_SIZE, WM_SYSCOLORCHANGE, WM_THEMECHANGED, WNDCLASSEXW,
    WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

use super::controls::{
    apply_theme as apply_control_theme, create_control, create_ui_font, create_ui_font_for_role,
    draw_inno_button, measured_button_width, wide, NativeControlKind, UiFontRole,
};
use super::layout::{
    centered_rect_in_work_area, clamp_rect_to_work_area, shell_geometry, PixelRect,
};
use super::state::{NativeWindowState, WorkflowKind};
use super::theme::{ThemeBrushes, ThemeContext, ThemeMode};

const CLASS_NAME: PCWSTR = w!("LetRecovery.PE.Native.Shell");
const ID_PRIMARY: u16 = 1001;
const ID_CANCEL: u16 = 1002;
const MIN_WIDTH: i32 = 640;
const MIN_HEIGHT: i32 = 440;
const PREFERRED_WIDTH: i32 = 760;
const PREFERRED_HEIGHT: i32 = 520;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellExit {
    /// P2 never consumes the workflow. The caller must continue into the existing progress UI.
    ContinueLegacyProgress,
}

struct NativeShell {
    state: NativeWindowState<WorkflowKind>,
    theme: ThemeContext,
    brushes: ThemeBrushes,
    font: HFONT,
    title_font: HFONT,
    title: HWND,
    subtitle: HWND,
    body: HWND,
    primary: HWND,
    cancel: HWND,
    exit: ShellExit,
}

impl NativeShell {
    unsafe fn new(state: NativeWindowState<WorkflowKind>, dpi: u32) -> Self {
        let theme = ThemeContext::detect(dpi);
        Self {
            state,
            theme,
            brushes: ThemeBrushes::new(theme.palette),
            font: create_ui_font(dpi, 10),
            title_font: create_ui_font_for_role(dpi, 12, UiFontRole::Heading),
            title: HWND::default(),
            subtitle: HWND::default(),
            body: HWND::default(),
            primary: HWND::default(),
            cancel: HWND::default(),
            exit: ShellExit::ContinueLegacyProgress,
        }
    }

    unsafe fn create_children(&mut self, hwnd: HWND) -> windows::core::Result<()> {
        let title = page_title(self.state.workflow);
        let subtitle = page_subtitle(self.state.workflow);
        let body = crate::tr!(
            "当前任务的进度页面将在后续阶段迁移。继续后将进入现有兼容进度界面，安装、备份和扩容状态不会丢失。"
        );
        self.title = create_static(hwnd, 1101, &title)?;
        self.subtitle = create_static(hwnd, 1102, &subtitle)?;
        self.body = create_static(hwnd, 1103, &body)?;
        self.primary = create_control(
            hwnd,
            ID_PRIMARY,
            NativeControlKind::Button,
            &crate::tr!("继续"),
            self.theme,
        )?;
        self.cancel = create_control(
            hwnd,
            ID_CANCEL,
            NativeControlKind::Button,
            &crate::tr!("取消"),
            self.theme,
        )?;
        self.apply_fonts();
        self.layout(hwnd);
        Ok(())
    }

    unsafe fn apply_fonts(&self) {
        if !self.title.0.is_null() {
            let _ = SendMessageW(
                self.title,
                WM_SETFONT,
                WPARAM(self.title_font.0 as usize),
                LPARAM(1),
            );
        }
        for control in [self.subtitle, self.body, self.primary, self.cancel] {
            if !control.0.is_null() {
                let _ = SendMessageW(control, WM_SETFONT, WPARAM(self.font.0 as usize), LPARAM(1));
            }
        }
    }

    unsafe fn refresh_dpi(&mut self, hwnd: HWND, dpi: u32) {
        self.theme = ThemeContext::new(self.theme.mode, dpi.max(96));
        if !self.font.0.is_null() {
            let _ = DeleteObject(self.font);
        }
        if !self.title_font.0.is_null() {
            let _ = DeleteObject(self.title_font);
        }
        self.font = create_ui_font(dpi.max(96), 10);
        self.title_font = create_ui_font_for_role(dpi.max(96), 12, UiFontRole::Heading);
        self.apply_fonts();
        self.layout(hwnd);
    }

    unsafe fn refresh_theme(&mut self, hwnd: HWND) {
        // WinPE normally keeps a fixed mode, but a shell or deployment can still broadcast a
        // theme/settings change. Rebuild cached brushes and refresh every live child in one frame;
        // WM_THEMECHANGED must be honoured even when the explicit PE mode itself is unchanged.
        let _ = SendMessageW(hwnd, 0x000B, WPARAM(0), LPARAM(0)); // WM_SETREDRAW(FALSE)
        self.theme = ThemeContext::detect(GetDpiForWindow(hwnd).max(96));
        self.brushes = ThemeBrushes::new(self.theme.palette);
        apply_title_bar_theme(hwnd, self.theme.mode);
        for control in [self.primary, self.cancel] {
            if !control.0.is_null() {
                apply_control_theme(control, NativeControlKind::Button, self.theme.palette);
            }
        }
        let _ = SendMessageW(hwnd, 0x000B, WPARAM(1), LPARAM(0)); // WM_SETREDRAW(TRUE)
        let _ = windows::Win32::Graphics::Gdi::RedrawWindow(
            hwnd,
            None,
            None,
            RDW_INVALIDATE | RDW_FRAME | RDW_ALLCHILDREN | RDW_UPDATENOW,
        );
    }

    unsafe fn layout(&self, hwnd: HWND) {
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let width = (client.right - client.left).max(1);
        let height = (client.bottom - client.top).max(1);
        let layout = shell_geometry(width, height, self.theme.dpi);
        let button_height = self.theme.metrics.button_height;
        let content_width = (width - layout.pad * 2).max(1);
        let maximum_button = ((content_width - self.theme.metrics.item_gap).max(1) / 2).max(1);
        let cancel_width = measured_button_width(
            hwnd,
            self.font,
            &crate::tr!("取消"),
            self.theme.dpi,
            maximum_button,
        );
        let primary_width = measured_button_width(
            hwnd,
            self.font,
            &crate::tr!("继续"),
            self.theme.dpi,
            maximum_button,
        );
        let button_y = layout.command.y + (layout.command.height - button_height) / 2;

        move_pixel_control(self.title, layout.title);
        if let Some(subtitle) = layout.subtitle {
            move_pixel_control(self.subtitle, subtitle);
            let _ = ShowWindow(self.subtitle, SW_SHOW);
        } else {
            let _ = ShowWindow(
                self.subtitle,
                windows::Win32::UI::WindowsAndMessaging::SW_HIDE,
            );
        }
        move_pixel_control(self.body, layout.body);
        let _ = MoveWindow(
            self.cancel,
            width - layout.pad - cancel_width,
            button_y,
            cancel_width,
            button_height,
            true,
        );
        let _ = MoveWindow(
            self.primary,
            width - layout.pad - cancel_width - self.theme.metrics.item_gap - primary_width,
            button_y,
            primary_width,
            button_height,
            true,
        );
    }
}

impl Drop for NativeShell {
    fn drop(&mut self) {
        unsafe {
            if !self.font.0.is_null() {
                let _ = DeleteObject(self.font);
            }
            if !self.title_font.0.is_null() {
                let _ = DeleteObject(self.title_font);
            }
        }
    }
}

pub fn run_shell_preview(workflow: WorkflowKind) -> windows::core::Result<ShellExit> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let controls = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES,
        };
        let _ = InitCommonControlsEx(&controls);
        let instance = GetModuleHandleW(None)?;
        let cursor = LoadCursorW(None, IDC_ARROW)?;
        let (large_icon, small_icon) = load_application_icons(HINSTANCE(instance.0))?;
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            hInstance: HINSTANCE(instance.0),
            hCursor: cursor,
            hIcon: large_icon,
            hIconSm: small_icon,
            hbrBackground: HBRUSH::default(),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            let error = windows::core::Error::from_win32();
            // Re-entry in the same process may report an existing class; creating the window is
            // still safe, so only return when CreateWindowExW also fails below.
            log::debug!("PE 原生窗口类注册结果: {error}");
        }

        let initial_dpi = GetDpiForSystem().max(96);
        let mut shell = Box::new(NativeShell::new(
            NativeWindowState::new(workflow),
            initial_dpi,
        ));
        let screen_width = GetSystemMetrics(SM_CXSCREEN).max(1);
        let screen_height = GetSystemMetrics(SM_CYSCREEN).max(1);
        let width = scaled(PREFERRED_WIDTH, initial_dpi).min(screen_width);
        let height = scaled(PREFERRED_HEIGHT, initial_dpi).min(screen_height);
        let window_title = wide(crate::tr!("LetRecovery PE"));
        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS_NAME,
            PCWSTR(window_title.as_ptr()),
            WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0 | WS_CLIPCHILDREN.0 | WS_CLIPSIBLINGS.0),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            width,
            height,
            HWND::default(),
            HMENU::default(),
            HINSTANCE(instance.0),
            Some((&mut *shell as *mut NativeShell).cast()),
        ) {
            Ok(hwnd) => hwnd,
            Err(error) => {
                drain_pending_quit_message();
                return Err(error);
            }
        };
        let _ = SendMessageW(
            hwnd,
            WM_SETICON,
            WPARAM(ICON_BIG as usize),
            LPARAM(large_icon.0 as isize),
        );
        let _ = SendMessageW(
            hwnd,
            WM_SETICON,
            WPARAM(ICON_SMALL as usize),
            LPARAM(small_icon.0 as isize),
        );
        apply_title_bar_theme(hwnd, shell.theme.mode);
        let actual_dpi = GetDpiForWindow(hwnd).max(96);
        if actual_dpi != initial_dpi {
            shell.refresh_dpi(hwnd, actual_dpi);
        }
        fit_window_to_work_area(hwnd, PREFERRED_WIDTH, PREFERRED_HEIGHT, actual_dpi);
        let _ = ShowWindow(hwnd, SW_SHOW);

        let mut message = MSG::default();
        loop {
            let result = GetMessageW(&mut message, None, 0, 0);
            if result.0 == -1 {
                return Err(windows::core::Error::from_win32());
            }
            if result.0 == 0 {
                break;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        Ok(shell.exit)
    }
}

pub(crate) unsafe fn drain_pending_quit_message() {
    let mut message = MSG::default();
    while PeekMessageW(&mut message, None, WM_QUIT, WM_QUIT, PM_REMOVE).as_bool() {}
}

unsafe fn create_static(parent: HWND, id: u16, text: &str) -> windows::core::Result<HWND> {
    let text = wide(text);
    CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("STATIC"),
        PCWSTR(text.as_ptr()),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0),
        0,
        0,
        0,
        0,
        parent,
        HMENU(id as isize as *mut _),
        HINSTANCE::default(),
        None,
    )
}

fn page_title(workflow: WorkflowKind) -> String {
    match workflow {
        WorkflowKind::Install => crate::tr!("系统安装"),
        WorkflowKind::Backup => crate::tr!("系统备份"),
        WorkflowKind::Expand => crate::tr!("无损扩大系统盘"),
        WorkflowKind::Missing => crate::tr!("LetRecovery PE"),
    }
}

fn page_subtitle(workflow: WorkflowKind) -> String {
    match workflow {
        WorkflowKind::Install => crate::tr!("准备进入系统安装进度。"),
        WorkflowKind::Backup => crate::tr!("准备进入系统备份进度。"),
        WorkflowKind::Expand => crate::tr!("准备进入系统盘扩容进度。"),
        WorkflowKind::Missing => crate::tr!("未检测到可执行的 PE 任务。"),
    }
}

unsafe fn move_pixel_control(control: HWND, rect: PixelRect) {
    let _ = MoveWindow(
        control,
        rect.x,
        rect.y,
        rect.width.max(0),
        rect.height.max(0),
        true,
    );
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

pub(crate) unsafe fn load_application_icons(
    instance: HINSTANCE,
) -> windows::core::Result<(HICON, HICON)> {
    const APPLICATION_ICON_ID: usize = 1;
    let resource = PCWSTR(APPLICATION_ICON_ID as *const u16);
    let large = LoadImageW(
        instance,
        resource,
        IMAGE_ICON,
        GetSystemMetrics(SM_CXICON),
        GetSystemMetrics(SM_CYICON),
        LR_SHARED,
    )?;
    let small = LoadImageW(
        instance,
        resource,
        IMAGE_ICON,
        GetSystemMetrics(SM_CXSMICON),
        GetSystemMetrics(SM_CYSMICON),
        LR_SHARED,
    )?;
    Ok((HICON(large.0), HICON(small.0)))
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

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
    }
    let shell = (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut NativeShell).as_mut();
    match message {
        WM_CREATE => {
            if let Some(shell) = shell {
                if let Err(error) = shell.create_children(hwnd) {
                    log::error!("创建 PE 原生窗口控件失败: {error}");
                    return LRESULT(-1);
                }
            }
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            let minmax = lparam.0 as *mut MINMAXINFO;
            if !minmax.is_null() {
                let dpi = GetDpiForWindow(hwnd).max(96);
                let work = monitor_work_area(hwnd);
                let work_width = (work.right - work.left).max(1);
                let work_height = (work.bottom - work.top).max(1);
                (*minmax).ptMinTrackSize.x = scaled(MIN_WIDTH, dpi).min(work_width);
                (*minmax).ptMinTrackSize.y = scaled(MIN_HEIGHT, dpi).min(work_height);
            }
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(shell) = shell {
                shell.layout(hwnd);
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            if let Some(shell) = shell {
                let suggested = &*(lparam.0 as *const RECT);
                let _ = SetWindowPos(
                    hwnd,
                    HWND::default(),
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    SWP_NOACTIVATE | SWP_NOZORDER,
                );
                clamp_window_to_work_area(hwnd);
                shell.refresh_dpi(hwnd, GetDpiForWindow(hwnd));
            }
            LRESULT(0)
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED | WM_SYSCOLORCHANGE => {
            if let Some(shell) = shell {
                shell.refresh_theme(hwnd);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xffff) as u16;
            let notification = ((wparam.0 >> 16) & 0xffff) as u16;
            if notification == BN_CLICKED as u16 && (id == ID_PRIMARY || id == ID_CANCEL) {
                if let Some(shell) = shell {
                    shell.exit = ShellExit::ContinueLegacyProgress;
                }
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DRAWITEM if lparam.0 != 0 => {
            if let Some(shell) = shell {
                let item = &*(lparam.0 as *const DRAWITEMSTRUCT);
                if item.CtlID == u32::from(ID_PRIMARY) || item.CtlID == u32::from(ID_CANCEL) {
                    draw_inno_button(
                        item,
                        shell.theme.palette,
                        item.CtlID == u32::from(ID_PRIMARY),
                        shell.font,
                        shell.theme.dpi,
                    );
                    return LRESULT(1);
                }
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            if let Some(shell) = shell {
                let dc = HDC(wparam.0 as *mut _);
                let _ = SetTextColor(dc, shell.theme.palette.text);
                let _ = SetBkColor(dc, shell.theme.palette.window);
                let _ = SetBkMode(dc, TRANSPARENT);
                return LRESULT(shell.brushes.window.0 as isize);
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            if let Some(shell) = shell {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut paint);
                let mut rect = RECT::default();
                let _ = GetClientRect(hwnd, &mut rect);
                let _ = FillRect(dc, &rect, shell.brushes.window);
                let command_top = shell_geometry(rect.right, rect.bottom, shell.theme.dpi)
                    .command
                    .y;
                let separator = RECT {
                    left: 0,
                    top: command_top,
                    right: rect.right,
                    bottom: command_top + shell.theme.metrics.separator_thickness,
                };
                let separator_brush =
                    windows::Win32::Graphics::Gdi::CreateSolidBrush(shell.theme.palette.separator);
                let _ = FillRect(dc, &separator, separator_brush);
                let _ = DeleteObject(separator_brush);
                let _ = EndPaint(hwnd, &paint);
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_CLOSE => {
            if let Some(shell) = shell {
                shell.exit = ShellExit::ContinueLegacyProgress;
            }
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimum_shell_size_remains_compact_at_100_percent() {
        assert_eq!(scaled(MIN_WIDTH, 96), 640);
        assert_eq!(scaled(MIN_HEIGHT, 96), 440);
    }

    #[test]
    fn titles_cover_every_workflow_route() {
        for workflow in [
            WorkflowKind::Install,
            WorkflowKind::Backup,
            WorkflowKind::Expand,
            WorkflowKind::Missing,
        ] {
            assert!(!page_title(workflow).is_empty());
            assert!(!page_subtitle(workflow).is_empty());
        }
    }
}
