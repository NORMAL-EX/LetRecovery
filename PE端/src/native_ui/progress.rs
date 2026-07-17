use std::fmt;
use std::mem::size_of;
use std::thread;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, DeleteObject, EndPaint, FillRect, GetDC, InvalidateRect, ReleaseDC, SetBkColor,
    SetBkMode, SetTextColor, HBRUSH, HDC, HFONT, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, DRAWITEMSTRUCT, ICC_LISTVIEW_CLASSES, ICC_STANDARD_CLASSES,
    INITCOMMONCONTROLSEX,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    GetSystemMetrics, GetWindowLongPtrW, KillTimer, LoadCursorW, MoveWindow, PeekMessageW,
    PostQuitMessage, RegisterClassExW, SendMessageW, SetTimer, SetWindowLongPtrW, SetWindowPos,
    SetWindowTextW, ShowWindow, TranslateMessage, BN_CLICKED, CREATESTRUCTW, CS_HREDRAW,
    CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA, HMENU, ICON_BIG, ICON_SMALL, IDC_ARROW, MINMAXINFO,
    MSG, PM_REMOVE, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_SHOW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORBTN,
    WM_CTLCOLORSTATIC, WM_DESTROY, WM_DPICHANGED, WM_DRAWITEM, WM_ERASEBKGND, WM_GETMINMAXINFO,
    WM_NCCREATE, WM_PAINT, WM_QUIT, WM_SETFONT, WM_SETICON, WM_SIZE, WM_TIMER, WNDCLASSEXW,
    WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

use crate::app::{WorkflowRecoverySnapshot, WorkflowSession};
use crate::core::config::{ConfigFileManager, OperationType};
use crate::ui::progress::{BackupStep, InstallStep, ProgressState, StepStatus};

use super::controls::{
    create_control, create_ui_font, create_ui_font_for_role, draw_indeterminate_ring,
    draw_inno_button, draw_progress, draw_step_status_icon, measured_button_width, wide,
    NativeControlKind, StepStatusIcon, UiFontRole,
};
use super::details::{page_content, AdvancedOptionsSummary, DetailsPane};
use super::layout::{command_bar_geometry, progress_geometry, PixelRect};
use super::state::{NativePage, NativeWindowState, WorkflowKind};
use super::theme::{ThemeBrushes, ThemeContext};
use super::window::{
    apply_title_bar_theme, clamp_window_to_work_area, drain_pending_quit_message,
    fit_window_to_work_area, load_application_icons, monitor_work_area, scaled,
};

const CLASS_NAME: PCWSTR = w!("LetRecovery.PE.Native.Progress");
const WORKER_TIMER_ID: usize = 1;
const ANIMATION_TIMER_ID: usize = 2;
const WORKER_POLL_INTERVAL_MS: u32 = 50;
const ANIMATION_FRAME_INTERVAL_MS: u32 = 16;
const ID_CLOSE: u16 = 2001;
const ID_BACK: u16 = 2002;
const ID_DETAILS: u16 = 2003;
const MIN_WIDTH: i32 = 680;
const MIN_HEIGHT: i32 = 500;
const PREFERRED_WIDTH: i32 = 800;
const PREFERRED_HEIGHT: i32 = 600;
const SS_CENTER_STYLE: u32 = 0x0000_0001;

#[derive(Debug)]
pub struct ProgressRunError {
    source: windows::core::Error,
}

impl ProgressRunError {
    fn before_worker(source: windows::core::Error) -> Self {
        Self { source }
    }

    fn after_worker(source: windows::core::Error) -> Self {
        Self { source }
    }
}

impl fmt::Display for ProgressRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(formatter)
    }
}

impl std::error::Error for ProgressRunError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ProgressTerminal {
    #[default]
    Running,
    Completed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProgressStepRow {
    name: &'static str,
    status: StepStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProgressPresentation {
    workflow: WorkflowKind,
    current_step: Option<&'static str>,
    step_progress: u8,
    overall_progress: u8,
    status: String,
    terminal: ProgressTerminal,
    rows: Vec<ProgressStepRow>,
}

impl ProgressPresentation {
    fn from_state(state: &ProgressState) -> Self {
        let workflow = if state.is_expand_mode {
            WorkflowKind::Expand
        } else if state.is_install_mode {
            WorkflowKind::Install
        } else {
            WorkflowKind::Backup
        };
        let terminal = if state.is_failed {
            ProgressTerminal::Failed
        } else if state.is_completed {
            ProgressTerminal::Completed
        } else {
            ProgressTerminal::Running
        };
        let current_step = match workflow {
            WorkflowKind::Install if state.has_current_step => {
                Some(state.current_install_step.name())
            }
            WorkflowKind::Backup if state.has_current_step => {
                Some(state.current_backup_step.name())
            }
            WorkflowKind::Expand | WorkflowKind::Missing => None,
            WorkflowKind::Install | WorkflowKind::Backup => None,
        };
        let rows = match workflow {
            WorkflowKind::Install => install_rows(state),
            WorkflowKind::Backup => backup_rows(state),
            WorkflowKind::Expand | WorkflowKind::Missing => Vec::new(),
        };
        let status = if state.is_failed {
            crate::tr!("操作失败，请查看错误信息。")
        } else {
            state.status_message.clone()
        };
        Self {
            workflow,
            current_step,
            step_progress: state.step_progress,
            overall_progress: state.overall_progress,
            status,
            terminal,
            rows,
        }
    }
}

fn install_rows(state: &ProgressState) -> Vec<ProgressStepRow> {
    let current = state.current_install_step.index();
    InstallStep::all()
        .into_iter()
        .map(|step| {
            let index = step.index();
            let status = if state.has_current_step {
                step_status(index, current, state.step_progress, state.is_failed)
            } else {
                StepStatus::Pending
            };
            ProgressStepRow {
                name: step.name(),
                status,
            }
        })
        .collect()
}

fn backup_rows(state: &ProgressState) -> Vec<ProgressStepRow> {
    let current = state.current_backup_step.index();
    BackupStep::all()
        .into_iter()
        .map(|step| {
            let index = step.index();
            let status = if state.has_current_step {
                step_status(index, current, state.step_progress, state.is_failed)
            } else {
                StepStatus::Pending
            };
            ProgressStepRow {
                name: step.name(),
                status,
            }
        })
        .collect()
}

fn step_status(index: usize, current: usize, progress: u8, failed: bool) -> StepStatus {
    if failed && index == current {
        StepStatus::Failed
    } else if index < current || (index == current && progress == 100) {
        StepStatus::Completed
    } else if index == current {
        StepStatus::InProgress
    } else {
        StepStatus::Pending
    }
}

struct NativeProgressWindow {
    operation_type: OperationType,
    start_worker: bool,
    state: NativeWindowState<Option<WorkflowSession>>,
    presentation: ProgressPresentation,
    worker_finished: bool,
    theme: ThemeContext,
    brushes: ThemeBrushes,
    body_font: HFONT,
    title_font: HFONT,
    title: HWND,
    subtitle: HWND,
    step_caption: HWND,
    step_percent: HWND,
    overall_caption: HWND,
    overall_percent: HWND,
    status: HWND,
    footer_status: HWND,
    back: HWND,
    details_button: HWND,
    close: HWND,
    details_pane: Option<DetailsPane>,
    advanced_options: Option<AdvancedOptionsSummary>,
    row_labels: Vec<HWND>,
    row_icons: Vec<RECT>,
    spinner_started: Instant,
    spinner_rect: RECT,
    step_bar: RECT,
    overall_bar: RECT,
}

impl NativeProgressWindow {
    unsafe fn new(
        operation_type: OperationType,
        dpi: u32,
        advanced_options: Option<AdvancedOptionsSummary>,
        start_worker: bool,
    ) -> Self {
        let mut progress = initial_progress(operation_type);
        if !start_worker {
            match operation_type {
                OperationType::Install => progress.set_install_step(InstallStep::ApplyImage),
                OperationType::Backup => progress.set_backup_step(BackupStep::CaptureImage),
                OperationType::Expand => {}
            }
            progress.set_step_progress(5);
        }
        let presentation = ProgressPresentation::from_state(&progress);
        let mut state = NativeWindowState::new(None);
        state.navigate(NativePage::Progress);
        let theme = ThemeContext::detect(dpi);
        Self {
            operation_type,
            start_worker,
            state,
            presentation,
            worker_finished: false,
            theme,
            brushes: ThemeBrushes::new(theme.palette),
            body_font: create_ui_font(dpi, 10),
            title_font: create_ui_font_for_role(dpi, 12, UiFontRole::Heading),
            title: HWND::default(),
            subtitle: HWND::default(),
            step_caption: HWND::default(),
            step_percent: HWND::default(),
            overall_caption: HWND::default(),
            overall_percent: HWND::default(),
            status: HWND::default(),
            footer_status: HWND::default(),
            back: HWND::default(),
            details_button: HWND::default(),
            close: HWND::default(),
            details_pane: None,
            advanced_options,
            row_labels: Vec::new(),
            row_icons: Vec::new(),
            spinner_started: Instant::now(),
            spinner_rect: RECT::default(),
            step_bar: RECT::default(),
            overall_bar: RECT::default(),
        }
    }

    unsafe fn create_children(&mut self, hwnd: HWND) -> windows::core::Result<()> {
        self.title =
            create_centered_static(hwnd, 2101, &progress_title(self.presentation.workflow))?;
        self.subtitle = HWND::default();
        self.step_caption = create_centered_static(hwnd, 2103, &crate::tr!("当前步骤"))?;
        self.step_percent = create_static(hwnd, 2104, "0%")?;
        self.overall_caption = create_static(hwnd, 2105, &crate::tr!("总体进度"))?;
        self.overall_percent = create_static(hwnd, 2106, "0%")?;
        // The progress page is deliberately read-only and compact. Detailed diagnostics remain in
        // the log instead of a disabled multiline Edit that looks interactive in WinPE.
        self.status = HWND::default();
        self.footer_status = create_static(hwnd, 2108, &crate::tr!("操作进行中"))?;
        self.back = create_control(
            hwnd,
            ID_BACK,
            NativeControlKind::Button,
            &crate::tr!("返回"),
            self.theme,
        )?;
        self.details_button = create_control(
            hwnd,
            ID_DETAILS,
            NativeControlKind::Button,
            &crate::tr!("高级选项"),
            self.theme,
        )?;
        self.close = create_control(
            hwnd,
            ID_CLOSE,
            NativeControlKind::Button,
            &crate::tr!("关闭"),
            self.theme,
        )?;
        let _ = EnableWindow(self.close, false);
        for control in [self.back, self.details_button, self.close] {
            let _ = ShowWindow(control, SW_HIDE);
        }
        for (index, _) in self.presentation.rows.iter().enumerate() {
            self.row_labels
                .push(create_static(hwnd, 2200 + index as u16, "")?);
            self.row_icons.push(RECT::default());
        }
        self.details_pane = Some(DetailsPane::create(hwnd, self.theme)?);
        self.apply_fonts();
        self.layout(hwnd);
        self.render_full_presentation(hwnd);

        if SetTimer(hwnd, ANIMATION_TIMER_ID, ANIMATION_FRAME_INTERVAL_MS, None) == 0 {
            return Err(windows::core::Error::from_win32());
        }
        if self.start_worker {
            if SetTimer(hwnd, WORKER_TIMER_ID, WORKER_POLL_INTERVAL_MS, None) == 0 {
                let _ = KillTimer(hwnd, ANIMATION_TIMER_ID);
                return Err(windows::core::Error::from_win32());
            }
            let mut session = WorkflowSession::new_for_operation(Some(self.operation_type));
            session.start_worker();
            self.state.workflow = Some(session);
        }
        Ok(())
    }

    unsafe fn render_full_presentation(&self, hwnd: HWND) {
        let step = current_step_text(self.presentation.current_step);
        set_text(self.step_caption, &step);
        set_text(
            self.step_percent,
            &format!("{}%", self.presentation.step_progress),
        );
        set_text(
            self.overall_percent,
            &format!("{}%", self.presentation.overall_progress),
        );
        let status = if self.presentation.status.is_empty() {
            terminal_status_text(self.presentation.terminal)
        } else {
            self.presentation.status.clone()
        };
        set_text(self.status, &status);
        set_text(self.footer_status, &self.footer_text());
        for (label, row) in self.row_labels.iter().zip(&self.presentation.rows) {
            set_text(*label, &crate::tr!(row.name));
        }
        let _ = EnableWindow(self.close, self.can_close());
        self.update_command_bar();
        let _ = InvalidateRect(hwnd, None, false);
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
        for control in [
            self.subtitle,
            self.step_caption,
            self.step_percent,
            self.overall_caption,
            self.overall_percent,
            self.footer_status,
            self.back,
            self.details_button,
            self.close,
        ]
        .into_iter()
        .chain(self.row_labels.iter().copied())
        {
            if !control.0.is_null() {
                let _ = SendMessageW(
                    control,
                    WM_SETFONT,
                    WPARAM(self.body_font.0 as usize),
                    LPARAM(1),
                );
            }
        }
        if let Some(details) = &self.details_pane {
            details.apply_fonts(self.body_font, self.title_font);
        }
    }

    unsafe fn refresh_dpi(&mut self, hwnd: HWND, dpi: u32) {
        self.theme = ThemeContext::new(self.theme.mode, dpi.max(96));
        let _ = DeleteObject(self.body_font);
        let _ = DeleteObject(self.title_font);
        self.body_font = create_ui_font(dpi.max(96), 10);
        self.title_font = create_ui_font_for_role(dpi.max(96), 12, UiFontRole::Heading);
        self.apply_fonts();
        self.layout(hwnd);
        let _ = InvalidateRect(hwnd, None, false);
    }

    unsafe fn poll_worker(&mut self, hwnd: HWND) {
        let (snapshot, worker_finished) = {
            let Some(session) = self.state.workflow.as_mut() else {
                return;
            };
            session.process_messages();
            let snapshot = session.snapshot();
            let worker_finished = session.reap_worker_if_finished();
            (snapshot, worker_finished)
        };
        let worker_state_changed = self.worker_finished != worker_finished;
        self.worker_finished = worker_finished;
        let next = ProgressPresentation::from_state(&snapshot);
        let presentation_changed = self.presentation != next;
        self.apply_presentation(hwnd, next);
        if worker_state_changed {
            set_text(self.footer_status, &self.footer_text());
            let _ = EnableWindow(self.close, self.can_close());
            let _ = InvalidateRect(hwnd, None, false);
        }
        if self.state.page == NativePage::Recovery && (worker_state_changed || presentation_changed)
        {
            self.render_detail_page();
        }
    }

    unsafe fn apply_presentation(&mut self, hwnd: HWND, next: ProgressPresentation) {
        if self.presentation.current_step != next.current_step {
            let text = current_step_text(next.current_step);
            set_text(self.step_caption, &text);
            self.layout(hwnd);
        }
        if self.presentation.step_progress != next.step_progress {
            set_text(self.step_percent, &format!("{}%", next.step_progress));
            let _ = InvalidateRect(hwnd, Some(&self.step_bar), false);
        }
        if self.presentation.overall_progress != next.overall_progress {
            set_text(self.overall_percent, &format!("{}%", next.overall_progress));
            let _ = InvalidateRect(hwnd, Some(&self.overall_bar), false);
        }
        if self.presentation.status != next.status || self.presentation.terminal != next.terminal {
            let status = if next.status.is_empty() {
                terminal_status_text(next.terminal)
            } else {
                next.status.clone()
            };
            set_text(self.status, &status);
            set_text(self.footer_status, &self.footer_text_for(next.terminal));
        }
        if self.presentation.rows != next.rows {
            for (label, row) in self.row_labels.iter().zip(&next.rows) {
                set_text(*label, &crate::tr!(row.name));
            }
            for icon in &self.row_icons {
                let _ = InvalidateRect(hwnd, Some(icon), false);
            }
        }
        let terminal_changed = self.presentation.terminal != next.terminal;
        if terminal_changed {
            let _ = EnableWindow(
                self.close,
                next.terminal != ProgressTerminal::Running && self.worker_finished,
            );
            let _ = InvalidateRect(hwnd, None, false);
        }
        self.presentation = next;
        if terminal_changed {
            self.update_command_bar();
        }
    }

    fn can_close(&self) -> bool {
        self.presentation.terminal != ProgressTerminal::Running && self.worker_finished
    }

    fn footer_text(&self) -> String {
        self.footer_text_for(self.presentation.terminal)
    }

    fn footer_text_for(&self, terminal: ProgressTerminal) -> String {
        if terminal != ProgressTerminal::Running && !self.worker_finished {
            crate::tr!("正在完成清理和收尾操作...")
        } else {
            terminal_footer_text(terminal)
        }
    }

    fn recovery_snapshot(&self) -> WorkflowRecoverySnapshot {
        self.state
            .workflow
            .as_ref()
            .map(WorkflowSession::recovery_snapshot)
            .unwrap_or(WorkflowRecoverySnapshot {
                checkpoint: None,
                worker_started: false,
                worker_finished: self.worker_finished,
            })
    }

    fn details_target(&self) -> Option<NativePage> {
        match self.state.page {
            NativePage::Progress => {
                if self.presentation.terminal == ProgressTerminal::Failed {
                    Some(NativePage::Error)
                } else if self.presentation.workflow == WorkflowKind::Install
                    && self.advanced_options.is_some()
                {
                    Some(NativePage::AdvancedOptions)
                } else {
                    Some(NativePage::Recovery)
                }
            }
            NativePage::AdvancedOptions | NativePage::Error => Some(NativePage::Recovery),
            NativePage::Recovery | NativePage::Overview => None,
        }
    }

    unsafe fn navigate(&mut self, hwnd: HWND, page: NativePage) {
        let page = if page == NativePage::Overview {
            NativePage::Progress
        } else {
            page
        };
        self.state.navigate(page);
        let progress_visible = page == NativePage::Progress;
        let visibility = if progress_visible { SW_SHOW } else { SW_HIDE };
        for control in [
            self.title,
            self.subtitle,
            self.step_caption,
            self.step_percent,
            self.overall_caption,
            self.overall_percent,
        ]
        .into_iter()
        .chain(self.row_labels.iter().copied())
        {
            let _ = ShowWindow(control, visibility);
        }
        if let Some(details) = &self.details_pane {
            details.set_visible(!progress_visible);
        }
        if !progress_visible {
            self.render_detail_page();
        }
        self.update_command_bar();
        self.layout(hwnd);
        let _ = InvalidateRect(hwnd, None, false);
    }

    unsafe fn render_detail_page(&self) {
        let Some(details) = &self.details_pane else {
            return;
        };
        let progress = self
            .state
            .workflow
            .as_ref()
            .map(WorkflowSession::snapshot)
            .unwrap_or_else(|| initial_progress(self.operation_type));
        let recovery = self.recovery_snapshot();
        let content = page_content(
            self.state.page,
            self.presentation.workflow,
            &progress,
            &recovery,
            self.advanced_options.as_ref(),
        );
        details.render(&content);
    }

    unsafe fn update_command_bar(&self) {
        if self.state.page == NativePage::Progress {
            for control in [self.back, self.details_button, self.close] {
                let _ = ShowWindow(control, SW_HIDE);
            }
            return;
        }
        let detail_target = self.details_target();
        let _ = ShowWindow(
            self.back,
            if self.state.page == NativePage::Progress {
                SW_HIDE
            } else {
                SW_SHOW
            },
        );
        let _ = ShowWindow(
            self.details_button,
            if detail_target.is_some() {
                SW_SHOW
            } else {
                SW_HIDE
            },
        );
        if let Some(target) = detail_target {
            let label = match target {
                NativePage::AdvancedOptions => crate::tr!("高级选项"),
                NativePage::Error => crate::tr!("错误详情"),
                NativePage::Recovery => crate::tr!("恢复信息"),
                NativePage::Overview | NativePage::Progress => String::new(),
            };
            set_text(self.details_button, &label);
        }
    }

    unsafe fn layout(&mut self, hwnd: HWND) {
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let width = (client.right - client.left).max(1);
        let height = (client.bottom - client.top).max(1);
        let has_step = self.presentation.workflow != WorkflowKind::Expand;
        let layout = progress_geometry(width, height, self.theme.dpi, has_step);
        move_pixel_control(self.title, layout.title);
        if let Some(subtitle) = layout.subtitle {
            move_pixel_control(self.subtitle, subtitle);
            let _ = ShowWindow(self.subtitle, SW_SHOW);
        } else {
            let _ = ShowWindow(self.subtitle, SW_HIDE);
        }
        if let Some(caption) = layout.step_caption {
            move_pixel_control(self.step_caption, caption);
            let _ = ShowWindow(self.step_caption, SW_SHOW);
        } else {
            move_control(self.step_caption, 0, 0, 0, 0);
            let _ = ShowWindow(self.step_caption, SW_HIDE);
        }
        if let (Some(percent), Some(bar)) = (layout.step_percent, layout.step_bar) {
            move_pixel_control(self.step_percent, percent);
            let _ = ShowWindow(self.step_percent, SW_SHOW);
            self.step_bar = pixel_rect(bar);
        } else {
            move_control(self.step_percent, 0, 0, 0, 0);
            let _ = ShowWindow(self.step_percent, SW_HIDE);
            self.step_bar = RECT::default();
        }
        move_pixel_control(self.overall_caption, layout.overall_caption);
        move_pixel_control(self.overall_percent, layout.overall_percent);
        self.overall_bar = pixel_rect(layout.overall_bar);
        let line_height = scaled(22, self.theme.dpi);
        let per_column = (layout.rows.height / line_height).max(0) as usize;
        let visible_count = self.row_labels.len().min(per_column.saturating_mul(2));
        let columns = if visible_count > per_column { 2 } else { 1 };
        let column_width = layout.rows.width / columns;
        let icon_size = scaled(16, self.theme.dpi);
        let icon_gap = scaled(10, self.theme.dpi);
        let row_indent = scaled(32, self.theme.dpi);
        self.spinner_rect = RECT::default();
        for (index, label) in self.row_labels.iter().enumerate() {
            if index < visible_count && per_column > 0 {
                let column = index / per_column;
                let row = index % per_column;
                let column_x = layout.rows.x + column as i32 * column_width;
                let icon_top =
                    layout.rows.y + row as i32 * line_height + (line_height - icon_size).max(0) / 2;
                self.row_icons[index] = RECT {
                    left: column_x + row_indent,
                    top: icon_top,
                    right: column_x + row_indent + icon_size,
                    bottom: icon_top + icon_size,
                };
                if self.presentation.rows[index].status == StepStatus::InProgress {
                    self.spinner_rect = self.row_icons[index];
                }
                move_control(
                    *label,
                    column_x + row_indent + icon_size + icon_gap,
                    layout.rows.y + row as i32 * line_height,
                    (column_width
                        - row_indent
                        - icon_size
                        - icon_gap
                        - self.theme.metrics.item_gap)
                        .max(1),
                    line_height,
                );
                let _ = ShowWindow(*label, SW_SHOW);
            } else {
                self.row_icons[index] = RECT::default();
                let _ = ShowWindow(*label, SW_HIDE);
            }
        }

        if let Some(details) = &self.details_pane {
            details.layout(
                RECT {
                    left: layout.pad,
                    top: layout.title.y,
                    right: width - layout.pad,
                    bottom: layout.command.y - scaled(6, self.theme.dpi).min(12),
                },
                |value| scaled(value, self.theme.dpi),
            );
        }

        let button_height = self.theme.metrics.button_height;
        let gap = self.theme.metrics.item_gap.min(scaled(10, self.theme.dpi));
        let content_width = (width - layout.pad * 2).max(1);
        let maximum_button = (content_width / 3).max(1);
        let close_width = measured_button_width(
            hwnd,
            self.body_font,
            &crate::tr!("关闭"),
            self.theme.dpi,
            maximum_button,
        );
        let back_width = measured_button_width(
            hwnd,
            self.body_font,
            &crate::tr!("返回"),
            self.theme.dpi,
            maximum_button,
        );
        let detail_label = self
            .details_target()
            .map(detail_page_label)
            .unwrap_or_default();
        let details_width = measured_button_width(
            hwnd,
            self.body_font,
            &detail_label,
            self.theme.dpi,
            maximum_button,
        );
        let details_visible = self.details_target().is_some();
        let back_visible = self.state.page != NativePage::Progress;
        if self.state.page == NativePage::Progress {
            for control in [self.back, self.details_button, self.close] {
                let _ = ShowWindow(control, SW_HIDE);
            }
            move_pixel_control(
                self.footer_status,
                PixelRect {
                    x: layout.pad,
                    y: layout.command.y + (layout.command.height - button_height).max(0) / 2,
                    width: (layout.command.width - layout.pad * 2).max(1),
                    height: button_height.min(layout.command.height).max(1),
                },
            );
            let _ = ShowWindow(self.footer_status, SW_SHOW);
            return;
        }
        let command = command_bar_geometry(
            layout.command,
            layout.pad,
            gap,
            button_height,
            back_width,
            details_width,
            close_width,
            back_visible,
            details_visible,
        );
        if let Some(back) = command.back {
            move_pixel_control(self.back, back);
        }
        if let Some(details) = command.details {
            move_pixel_control(self.details_button, details);
        }
        move_pixel_control(self.close, command.close);
        if let Some(footer) = command.footer {
            move_pixel_control(self.footer_status, footer);
        }
        let footer_width = command.footer.map_or(0, |rect| rect.width);
        let _ = ShowWindow(
            self.footer_status,
            if footer_width >= scaled(72, self.theme.dpi) {
                SW_SHOW
            } else {
                SW_HIDE
            },
        );
    }
}

impl Drop for NativeProgressWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.body_font);
            let _ = DeleteObject(self.title_font);
        }
    }
}

pub fn run(operation_type: OperationType) -> Result<(), ProgressRunError> {
    run_internal(operation_type, true)
}

#[cfg(feature = "non-elevated-tests")]
pub fn run_preview(operation_type: OperationType) -> Result<(), ProgressRunError> {
    run_internal(operation_type, false)
}

fn run_internal(operation_type: OperationType, start_worker: bool) -> Result<(), ProgressRunError> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let controls = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES | ICC_LISTVIEW_CLASSES,
        };
        let _ = InitCommonControlsEx(&controls);
        let instance = GetModuleHandleW(None).map_err(ProgressRunError::before_worker)?;
        let (large_icon, small_icon) = load_application_icons(HINSTANCE(instance.0))
            .map_err(ProgressRunError::before_worker)?;
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            hInstance: HINSTANCE(instance.0),
            hCursor: LoadCursorW(None, IDC_ARROW).map_err(ProgressRunError::before_worker)?,
            hIcon: large_icon,
            hIconSm: small_icon,
            hbrBackground: HBRUSH::default(),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            log::debug!("PE 原生进度窗口类已经注册或注册返回错误");
        }
        let dpi = GetDpiForSystem().max(96);
        let advanced_options = if operation_type == OperationType::Install {
            match ConfigFileManager::find_install_task() {
                Ok((_, _, config)) => Some(AdvancedOptionsSummary::from_install_config(&config)),
                Err(error) => {
                    log::warn!("无法预读 PE 安装高级选项摘要，工作线程仍按原流程读取配置: {error}");
                    None
                }
            }
        } else {
            None
        };
        let mut window = Box::new(NativeProgressWindow::new(
            operation_type,
            dpi,
            advanced_options,
            start_worker,
        ));
        let screen_width = GetSystemMetrics(SM_CXSCREEN).max(1);
        let screen_height = GetSystemMetrics(SM_CYSCREEN).max(1);
        let width = scaled(PREFERRED_WIDTH, dpi).min(screen_width);
        let height = scaled(PREFERRED_HEIGHT, dpi).min(screen_height);
        let title = wide(crate::tr!("LetRecovery PE"));
        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS_NAME,
            PCWSTR(title.as_ptr()),
            WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0 | WS_CLIPCHILDREN.0 | WS_CLIPSIBLINGS.0),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            width,
            height,
            HWND::default(),
            HMENU::default(),
            HINSTANCE(instance.0),
            Some((&mut *window as *mut NativeProgressWindow).cast()),
        ) {
            Ok(hwnd) => hwnd,
            Err(error) => {
                drain_pending_quit_message();
                return Err(ProgressRunError::before_worker(error));
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
        apply_title_bar_theme(hwnd, window.theme.mode);
        let actual_dpi = GetDpiForWindow(hwnd).max(96);
        if actual_dpi != dpi {
            window.refresh_dpi(hwnd, actual_dpi);
        }
        fit_window_to_work_area(hwnd, PREFERRED_WIDTH, PREFERRED_HEIGHT, actual_dpi);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let mut message = MSG::default();
        loop {
            let result = GetMessageW(&mut message, None, 0, 0);
            if result.0 == -1 {
                finish_worker_after_message_loop_error(&mut window, hwnd);
                return Err(ProgressRunError::after_worker(
                    windows::core::Error::from_win32(),
                ));
            }
            if result.0 == 0 {
                break;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        Ok(())
    }
}

unsafe fn finish_worker_after_message_loop_error(window: &mut NativeProgressWindow, hwnd: HWND) {
    log::error!("PE 原生进度消息循环异常，保留同一会话并等待工作线程安全结束");
    while !window.worker_finished {
        window.poll_worker(hwnd);
        if window.worker_finished {
            break;
        }
        let mut pending = MSG::default();
        while PeekMessageW(&mut pending, None, 0, 0, PM_REMOVE).as_bool() {
            if pending.message != WM_QUIT {
                let _ = TranslateMessage(&pending);
                DispatchMessageW(&pending);
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn initial_progress(operation_type: OperationType) -> ProgressState {
    match operation_type {
        OperationType::Install => ProgressState::new_install(),
        OperationType::Backup => ProgressState::new_backup(),
        OperationType::Expand => ProgressState::new_expand(),
    }
}

fn progress_title(workflow: WorkflowKind) -> String {
    match workflow {
        WorkflowKind::Install => crate::tr!("LetRecovery PE 安装助手"),
        WorkflowKind::Backup => crate::tr!("LetRecovery PE 备份助手"),
        WorkflowKind::Expand => crate::tr!("LetRecovery PE 扩容助手"),
        WorkflowKind::Missing => crate::tr!("LetRecovery PE"),
    }
}

fn current_step_text(step: Option<&str>) -> String {
    let name = step
        .map(|step| crate::tr!(step))
        .unwrap_or_else(|| crate::tr!("正在准备操作..."));
    format!("{}：[{}]", crate::tr!("当前步骤"), name)
}

fn terminal_status_text(terminal: ProgressTerminal) -> String {
    match terminal {
        ProgressTerminal::Running => crate::tr!("正在准备操作..."),
        ProgressTerminal::Completed => crate::tr!("操作已完成，即将重启。"),
        ProgressTerminal::Failed => crate::tr!("操作失败，请查看错误信息。"),
    }
}

fn terminal_footer_text(terminal: ProgressTerminal) -> String {
    match terminal {
        ProgressTerminal::Running => crate::tr!("操作进行中，当前流程不支持安全取消。"),
        ProgressTerminal::Completed => crate::tr!("操作已完成"),
        ProgressTerminal::Failed => crate::tr!("操作失败"),
    }
}

fn step_status_icon(status: StepStatus) -> StepStatusIcon {
    match status {
        StepStatus::Pending => StepStatusIcon::Pending,
        StepStatus::InProgress => StepStatusIcon::Current,
        StepStatus::Completed => StepStatusIcon::Success,
        StepStatus::Failed => StepStatusIcon::Error,
    }
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

unsafe fn create_centered_static(parent: HWND, id: u16, text: &str) -> windows::core::Result<HWND> {
    let text = wide(text);
    CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("STATIC"),
        PCWSTR(text.as_ptr()),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | SS_CENTER_STYLE),
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

unsafe fn set_text(control: HWND, text: &str) {
    if !control.0.is_null() {
        let text = wide(text);
        let _ = SetWindowTextW(control, PCWSTR(text.as_ptr()));
    }
}

unsafe fn move_control(control: HWND, x: i32, y: i32, width: i32, height: i32) {
    if !control.0.is_null() {
        let _ = MoveWindow(control, x, y, width.max(0), height.max(0), true);
    }
}

unsafe fn move_pixel_control(control: HWND, rect: PixelRect) {
    move_control(control, rect.x, rect.y, rect.width, rect.height);
}

fn pixel_rect(rect: PixelRect) -> RECT {
    RECT {
        left: rect.x,
        top: rect.y,
        right: rect.right(),
        bottom: rect.bottom(),
    }
}

fn detail_page_label(page: NativePage) -> String {
    match page {
        NativePage::AdvancedOptions => crate::tr!("高级选项"),
        NativePage::Error => crate::tr!("错误详情"),
        NativePage::Recovery => crate::tr!("恢复信息"),
        NativePage::Overview | NativePage::Progress => String::new(),
    }
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
    let window = (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut NativeProgressWindow).as_mut();
    match message {
        WM_CREATE => {
            if let Some(error) = window.and_then(|window| window.create_children(hwnd).err()) {
                log::error!("创建 PE 原生进度页失败: {error}");
                return LRESULT(-1);
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == WORKER_TIMER_ID => {
            if let Some(window) = window {
                window.poll_worker(hwnd);
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == ANIMATION_TIMER_ID => {
            if let Some(window) = window {
                if window.state.page == NativePage::Progress
                    && window.presentation.terminal == ProgressTerminal::Running
                    && window.spinner_rect.right > window.spinner_rect.left
                {
                    let dc = GetDC(hwnd);
                    if !dc.is_invalid() {
                        draw_indeterminate_ring(
                            dc,
                            window.spinner_rect,
                            window.spinner_started.elapsed().as_secs_f64(),
                            window.theme.palette,
                        );
                        let _ = ReleaseDC(hwnd, dc);
                    }
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
            if let Some(window) = window {
                window.layout(hwnd);
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            if let Some(window) = window {
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
                window.refresh_dpi(hwnd, GetDpiForWindow(hwnd));
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xffff) as u16;
            let notification = ((wparam.0 >> 16) & 0xffff) as u16;
            if notification == BN_CLICKED as u16 {
                if id == ID_CLOSE && window.as_ref().is_some_and(|window| window.can_close()) {
                    let _ = DestroyWindow(hwnd);
                } else if id == ID_BACK {
                    if let Some(window) = window {
                        window.navigate(hwnd, NativePage::Progress);
                    }
                } else if id == ID_DETAILS {
                    if let Some(window) = window {
                        if let Some(target) = window.details_target() {
                            window.navigate(hwnd, target);
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_DRAWITEM if lparam.0 != 0 => {
            if let Some(window) = window {
                let item = &*(lparam.0 as *const DRAWITEMSTRUCT);
                if [ID_CLOSE, ID_BACK, ID_DETAILS]
                    .into_iter()
                    .any(|id| item.CtlID == u32::from(id))
                {
                    draw_inno_button(
                        item,
                        window.theme.palette,
                        false,
                        window.body_font,
                        window.theme.dpi,
                    );
                    return LRESULT(1);
                }
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            if let Some(window) = window {
                let source = HWND(lparam.0 as *mut _);
                let dc = HDC(wparam.0 as *mut _);
                let color = if source == window.status
                    && window.presentation.terminal == ProgressTerminal::Failed
                {
                    window.theme.palette.error
                } else if let Some(index) =
                    window.row_labels.iter().position(|label| *label == source)
                {
                    match window.presentation.rows[index].status {
                        StepStatus::Completed => window.theme.palette.progress,
                        StepStatus::InProgress => window.theme.palette.accent_border,
                        StepStatus::Failed => window.theme.palette.error,
                        StepStatus::Pending => window.theme.palette.text_secondary,
                    }
                } else {
                    window.theme.palette.text
                };
                let _ = SetTextColor(dc, color);
                let _ = SetBkColor(dc, window.theme.palette.window);
                let _ = SetBkMode(dc, TRANSPARENT);
                return LRESULT(window.brushes.window.0 as isize);
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            if let Some(window) = window {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut paint);
                let _ = FillRect(dc, &paint.rcPaint, window.brushes.window);
                if window.state.page == NativePage::Progress
                    && window.step_bar.right > window.step_bar.left
                    && window.step_bar.bottom > window.step_bar.top
                {
                    draw_progress(
                        dc,
                        window.step_bar,
                        window.presentation.step_progress,
                        window.theme.palette,
                    );
                }
                if window.state.page == NativePage::Progress {
                    draw_progress(
                        dc,
                        window.overall_bar,
                        window.presentation.overall_progress,
                        window.theme.palette,
                    );
                    for (icon, row) in window.row_icons.iter().zip(&window.presentation.rows) {
                        if icon.right <= icon.left || icon.bottom <= icon.top {
                            continue;
                        }
                        if row.status == StepStatus::InProgress
                            && window.presentation.terminal == ProgressTerminal::Running
                        {
                            draw_indeterminate_ring(
                                dc,
                                *icon,
                                window.spinner_started.elapsed().as_secs_f64(),
                                window.theme.palette,
                            );
                        } else {
                            let status = step_status_icon(row.status);
                            draw_step_status_icon(dc, *icon, status, window.theme.palette);
                        }
                    }
                }
                let mut client = RECT::default();
                let _ = GetClientRect(hwnd, &mut client);
                let layout = progress_geometry(
                    client.right,
                    client.bottom,
                    window.theme.dpi,
                    window.presentation.workflow != WorkflowKind::Expand,
                );
                let command_top = layout.command.y;
                let separator_brush =
                    windows::Win32::Graphics::Gdi::CreateSolidBrush(window.theme.palette.separator);
                if window.state.page == NativePage::Progress && layout.rows.height > 0 {
                    let list_separator_y = layout.rows.y - scaled(9, window.theme.dpi);
                    let list_separator = RECT {
                        left: layout.pad,
                        top: list_separator_y,
                        right: client.right - layout.pad,
                        bottom: list_separator_y + window.theme.metrics.separator_thickness,
                    };
                    let _ = FillRect(dc, &list_separator, separator_brush);
                }
                let separator = RECT {
                    left: 0,
                    top: command_top,
                    right: client.right,
                    bottom: command_top + window.theme.metrics.separator_thickness,
                };
                let _ = FillRect(dc, &separator, separator_brush);
                let _ = DeleteObject(separator_brush);
                let _ = EndPaint(hwnd, &paint);
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_CLOSE => {
            if window.is_some_and(|window| !window.can_close()) {
                return LRESULT(0);
            }
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = KillTimer(hwnd, WORKER_TIMER_ID);
            let _ = KillTimer(hwnd, ANIMATION_TIMER_ID);
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
    fn install_failure_marks_only_the_current_step_failed() {
        let mut state = ProgressState::new_install();
        state.set_install_step(InstallStep::RepairBoot);
        state.mark_failed("boot failed");
        let view = ProgressPresentation::from_state(&state);
        assert_eq!(view.terminal, ProgressTerminal::Failed);
        assert_eq!(view.rows[5].status, StepStatus::Failed);
        assert_eq!(view.rows[4].status, StepStatus::Completed);
        assert_eq!(view.rows[6].status, StepStatus::Pending);
    }

    #[test]
    fn backup_completion_keeps_the_final_progress_at_one_hundred() {
        let mut state = ProgressState::new_backup();
        state.mark_completed();
        let view = ProgressPresentation::from_state(&state);
        assert_eq!(view.workflow, WorkflowKind::Backup);
        assert_eq!(view.terminal, ProgressTerminal::Completed);
        assert_eq!(view.step_progress, 100);
        assert_eq!(view.overall_progress, 100);
    }

    #[test]
    fn expand_progress_has_no_fake_install_or_backup_step_rows() {
        let mut state = ProgressState::new_expand();
        state.set_step_progress(37);
        let view = ProgressPresentation::from_state(&state);
        assert_eq!(view.workflow, WorkflowKind::Expand);
        assert!(view.current_step.is_none());
        assert!(view.rows.is_empty());
        assert_eq!(view.overall_progress, 37);
    }

    #[test]
    fn install_first_frame_has_no_fake_current_step() {
        let state = ProgressState::new_install();
        let view = ProgressPresentation::from_state(&state);
        assert!(view.current_step.is_none());
        assert!(view
            .rows
            .iter()
            .all(|row| row.status == StepStatus::Pending));
    }

    #[test]
    fn progress_status_uses_vector_semantic_icons_without_font_markers() {
        assert_eq!(
            step_status_icon(StepStatus::Completed),
            StepStatusIcon::Success
        );
        assert_eq!(step_status_icon(StepStatus::Failed), StepStatusIcon::Error);
        assert_eq!(
            crate::native_ui::theme::Palette::DARK.progress,
            crate::native_ui::theme::Palette::LIGHT.progress
        );
    }
}
