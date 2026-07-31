//! Dedicated native Inno-style UI for the legacy quick-partition editor.
//!
//! The dialog owns only presentation and the pure editor state. Disk inventory is supplied by the
//! host and every destructive action is returned as a fingerprinted intent. No DiskPart command,
//! resize operation, refresh enumeration, or other host I/O is performed here.

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{BOOL, COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUNDSMALL,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, ClientToScreen, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW,
    CreateRoundRectRgn, CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect,
    IntersectClipRect, InvalidateRect, MapWindowPoints, RestoreDC, SaveDC, SelectClipRgn,
    SelectObject, SetBkMode, SetTextColor, SetWindowRgn, DT_CENTER, DT_END_ELLIPSIS, DT_NOPREFIX,
    DT_SINGLELINE, DT_VCENTER, HFONT, PAINTSTRUCT, SRCCOPY, TRANSPARENT,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_TEXT, LVITEMW, LVM_DELETEALLITEMS,
    LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETBKCOLOR, LVM_SETCOLUMNWIDTH,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETTEXTBKCOLOR, LVM_SETTEXTCOLOR, LVS_EX_DOUBLEBUFFER,
    LVS_EX_FULLROWSELECT, LVS_EX_INFOTIP, LVS_REPORT, LVS_SHOWSELALWAYS, MEASUREITEMSTRUCT,
    ODS_DISABLED, ODS_GRAYED, ODS_SELECTED, ODT_MENU,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetCapture, ReleaseCapture, SetCapture,
};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, EnumThreadWindows, GetClassNameW, GetClientRect,
    GetParent, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, MoveWindow, PostMessageW,
    SendMessageW, SetMenuInfo, SetWindowTextW, ShowWindow, TrackPopupMenu, BM_SETCHECK,
    BS_AUTORADIOBUTTON, BS_OWNERDRAW, CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL,
    CB_RESETCONTENT, CB_SETCURSEL, ES_AUTOHSCROLL, MENUINFO, MF_GRAYED, MF_OWNERDRAW, MF_POPUP,
    MIM_BACKGROUND, SW_HIDE, SW_SHOW, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_CAPTURECHANGED,
    WM_COMMAND, WM_DRAWITEM, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MEASUREITEM,
    WM_MOUSEMOVE, WM_NCDESTROY, WM_PAINT, WM_RBUTTONUP, WM_SETFONT, WS_BORDER, WS_TABSTOP,
};

use super::super::controls::fill_round_rect_antialiased;
use super::super::controls::{child, combo_inventory_index, wide, NO_COMBO_SELECTION};
use super::super::dialog::{DialogButtons, DialogResult, DialogShell, DialogSpec};
use super::super::layout::{
    arrange_field, measure_text, measured_button_width, preferred_list_height, FieldArrangement,
    LayoutMetrics,
};
use super::super::theme::{apply_control_theme, apply_list_view_theme, NativeControlKind, Palette};
use crate::core::disk::PartitionStyle;
use crate::core::native_quick_partition::{
    DiskFingerprint, DiskPartitionFingerprint, QuickPartitionRequest,
};
use crate::core::native_quick_partition_dialog::{
    AdjacentPartitionTransferRequest, EditorRow, ExistingPartitionResizeRequest,
    PartitionManagementAction, PartitionManagementRequest, PendingPartitionOperation,
    QuickPartitionDialogState,
};
use crate::core::quick_partition::{DiskPartitionInfo, PartitionLayout, PhysicalDisk};

pub const ID_DISK: u16 = 65_300;
pub const ID_STYLE_MBR: u16 = 65_301;
pub const ID_STYLE_GPT: u16 = 65_302;
pub const ID_ADD_PARTITION: u16 = 65_303;
pub const ID_ADD_ESP: u16 = 65_304;
pub const ID_DELETE: u16 = 65_305;
pub const ID_APPLY_SIZE: u16 = 65_306;
const ID_PARTITIONS: u16 = 65_307;
const ID_SIZE: u16 = 65_308;
const ID_PARTITION_MAP: u16 = 65_309;
const ID_MAP_SELECT: u16 = 65_310;
const ID_MAP_RESIZE: u16 = 65_311;
const ID_MAP_DELETE: u16 = 65_312;
const ID_MAP_FORMAT_NTFS: u16 = 65_313;
const ID_MAP_REMOVE_LETTER: u16 = 65_314;
const ID_MAP_SET_ACTIVE: u16 = 65_315;
const ID_MAP_CLEAR_ACTIVE: u16 = 65_316;
const ID_APPLY_PENDING: u16 = 65_318;
const ID_MAP_DRAG_COMMIT: u16 = 65_319;
const ID_MAP_ASSIGN_LETTER_FIRST: u16 = 65_340;
const ID_MAP_CREATE_LETTER_FIRST: u16 = 65_366;
const PARTITION_MAP_SUBCLASS_ID: usize = 0x4c52_504d;
const RADIO_CONTROL_KIND: NativeControlKind = NativeControlKind::General;

const LVM_SETITEMTEXTW_LOCAL: u32 = 0x104C;
const LVM_SETITEMSTATE: u32 = 0x102B;
const LVNI_SELECTED: isize = 0x0002;
const LVIS_SELECTED: u32 = 0x0002;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PartitionMapTarget {
    Existing(usize),
    Unallocated { offset_bytes: u64, size_bytes: u64 },
}

#[derive(Clone, Debug)]
struct PartitionMapSegment {
    target: PartitionMapTarget,
    label: String,
    size: String,
    weight: u64,
    special: bool,
    protected: bool,
    drive_letter: Option<char>,
    active: bool,
    minimum_bytes: u64,
}

struct PartitionMapModel {
    font: HFONT,
    segments: Vec<PartitionMapSegment>,
    selected: Option<PartitionMapTarget>,
    context_target: Option<PartitionMapTarget>,
    available_letters: Vec<char>,
    style: PartitionStyle,
    initialized: bool,
    enabled: bool,
    drag: Option<PartitionMapDrag>,
    committed_resize: Option<(usize, u64)>,
    committed_transfer: Option<(usize, usize, u64)>,
    growth_blocked_by_neighbor: bool,
}

struct PartitionMenuItem {
    text: String,
    palette: Palette,
    font: HFONT,
    dpi: u32,
    width: i32,
    separator: bool,
    submenu: bool,
}

struct PartitionMenuBuilder {
    owner: HWND,
    font: HFONT,
    palette: Palette,
    dpi: u32,
    background: windows::Win32::Graphics::Gdi::HBRUSH,
    // Windows stores each owner-draw item pointer until TrackPopupMenu returns.
    // Individual boxes keep those addresses stable while this vector grows.
    #[allow(clippy::vec_box)]
    visuals: Vec<Box<PartitionMenuItem>>,
}

impl PartitionMenuBuilder {
    fn new(
        owner: HWND,
        font: HFONT,
        palette: Palette,
        dpi: u32,
        background: windows::Win32::Graphics::Gdi::HBRUSH,
    ) -> Self {
        Self {
            owner,
            font,
            palette,
            dpi,
            background,
            visuals: Vec::new(),
        }
    }

    unsafe fn item(
        &mut self,
        menu: windows::Win32::UI::WindowsAndMessaging::HMENU,
        command: u16,
        text: &str,
        disabled: bool,
    ) {
        let visual = Box::new(PartitionMenuItem {
            text: text.to_owned(),
            palette: self.palette,
            font: self.font,
            dpi: self.dpi,
            width: partition_menu_item_width(self.owner, self.font, text, false, self.dpi),
            separator: false,
            submenu: false,
        });
        let data = (&*visual as *const PartitionMenuItem) as *const u16;
        self.visuals.push(visual);
        let flags = if disabled {
            MF_OWNERDRAW | MF_GRAYED
        } else {
            MF_OWNERDRAW
        };
        let _ = AppendMenuW(menu, flags, command as usize, PCWSTR(data));
    }

    unsafe fn separator(&mut self, menu: windows::Win32::UI::WindowsAndMessaging::HMENU) {
        let visual = Box::new(PartitionMenuItem {
            text: String::new(),
            palette: self.palette,
            font: self.font,
            dpi: self.dpi,
            width: 0,
            separator: true,
            submenu: false,
        });
        let data = (&*visual as *const PartitionMenuItem) as *const u16;
        self.visuals.push(visual);
        let _ = AppendMenuW(menu, MF_OWNERDRAW | MF_GRAYED, 0, PCWSTR(data));
    }

    unsafe fn letter_submenu(
        &mut self,
        menu: windows::Win32::UI::WindowsAndMessaging::HMENU,
        title: &str,
        first_command: u16,
        letters: &[char],
    ) {
        let Ok(submenu) = CreatePopupMenu() else {
            return;
        };
        apply_partition_menu_background(submenu, self.background);
        if letters.is_empty() {
            self.item(submenu, 0, &crate::tr!("没有可用盘符"), true);
        } else {
            for letter in letters {
                self.item(
                    submenu,
                    first_command + (*letter as u16 - 'A' as u16),
                    &format!("{letter}:"),
                    false,
                );
            }
        }
        let visual = Box::new(PartitionMenuItem {
            text: title.to_owned(),
            palette: self.palette,
            font: self.font,
            dpi: self.dpi,
            width: partition_menu_item_width(self.owner, self.font, title, true, self.dpi),
            separator: false,
            submenu: true,
        });
        let data = (&*visual as *const PartitionMenuItem) as *const u16;
        self.visuals.push(visual);
        let _ = AppendMenuW(
            menu,
            MF_POPUP | MF_OWNERDRAW,
            submenu.0 as usize,
            PCWSTR(data),
        );
    }
}

struct PartitionMenuWindowRounder {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl PartitionMenuWindowRounder {
    fn start(ui_thread_id: u32) -> Self {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = std::thread::spawn(move || unsafe {
            let mut applied = Vec::<(isize, std::time::Instant, bool)>::new();
            while !worker_stop.load(std::sync::atomic::Ordering::Acquire) {
                let mut windows = Vec::<isize>::new();
                let _ = EnumThreadWindows(
                    ui_thread_id,
                    Some(collect_popup_menu_window),
                    LPARAM((&mut windows as *mut Vec<isize>) as isize),
                );
                for raw in windows {
                    if applied.iter().any(|(existing, _, _)| *existing == raw) {
                        continue;
                    }
                    let hwnd = HWND(raw as *mut core::ffi::c_void);
                    if let Some((width, height)) = popup_menu_window_size(hwnd) {
                        round_partition_menu_window(hwnd, width, height);
                        applied.push((raw, std::time::Instant::now(), false));
                    }
                }
                // USER32 may recreate the non-client frame just after showing the popup. A single
                // delayed reapplication leaves the final region deterministic without polling or
                // redrawing an already stable menu for the rest of its lifetime.
                for (raw, first_apply, finalized) in &mut applied {
                    if !*finalized && first_apply.elapsed() >= std::time::Duration::from_millis(40)
                    {
                        let hwnd = HWND(*raw as *mut core::ffi::c_void);
                        if let Some((width, height)) = popup_menu_window_size(hwnd) {
                            round_partition_menu_window(hwnd, width, height);
                        }
                        *finalized = true;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(8));
            }
        });
        Self {
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for PartitionMenuWindowRounder {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

unsafe extern "system" fn collect_popup_menu_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    if is_popup_menu_window(hwnd) {
        let windows = &mut *(lparam.0 as *mut Vec<isize>);
        windows.push(hwnd.0 as isize);
    }
    true.into()
}

unsafe fn is_popup_menu_window(hwnd: HWND) -> bool {
    let mut class_name = [0_u16; 16];
    let length = GetClassNameW(hwnd, &mut class_name);
    length > 0 && String::from_utf16_lossy(&class_name[..length as usize]) == "#32768"
}

unsafe fn popup_menu_window_size(hwnd: HWND) -> Option<(i32, i32)> {
    let mut rect = RECT::default();
    GetWindowRect(hwnd, &mut rect).ok()?;
    let width = (rect.right - rect.left).max(0);
    let height = (rect.bottom - rect.top).max(0);
    (width > 0 && height > 0).then_some((width, height))
}

unsafe fn round_partition_menu_window(hwnd: HWND, width: i32, height: i32) {
    let preference = DWMWCP_ROUNDSMALL;
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_WINDOW_CORNER_PREFERENCE,
        (&preference as *const DWM_WINDOW_CORNER_PREFERENCE).cast(),
        std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
    );
    // DWM provides antialiased corners where supported, while the region is the deterministic
    // compatibility boundary for Win10 and WinPE and also clips USER32's square menu frame.
    let dpi = GetDpiForWindow(hwnd).max(96);
    let radius = scale(6, dpi).max(2);
    let region = CreateRoundRectRgn(0, 0, width + 1, height + 1, radius * 2, radius * 2);
    if !region.is_invalid() && SetWindowRgn(hwnd, region, true) == 0 {
        let _ = DeleteObject(region);
    }
}

fn partition_menu_item_width(owner: HWND, font: HFONT, text: &str, submenu: bool, dpi: u32) -> i32 {
    let measured = unsafe { measure_text(owner, font, text, None).width };
    measured
        .saturating_add(scale(if submenu { 48 } else { 36 }, dpi))
        .max(scale(144, dpi))
}

#[derive(Clone, Copy, Debug)]
struct PartitionMapDrag {
    partition_index: usize,
    left_segment: usize,
    start_x: i32,
    current_x: i32,
    scale_width: i32,
    scale_bytes: u64,
    original_bytes: u64,
    minimum_bytes: u64,
    maximum_bytes: u64,
    right_is_unallocated: bool,
    right_is_borrowed_partition: bool,
    right_partition_index: Option<usize>,
}

impl PartitionMapModel {
    fn new(font: HFONT) -> Self {
        Self {
            font,
            segments: Vec::new(),
            selected: None,
            context_target: None,
            available_letters: Vec::new(),
            style: PartitionStyle::Unknown,
            initialized: false,
            enabled: false,
            drag: None,
            committed_resize: None,
            committed_transfer: None,
            growth_blocked_by_neighbor: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartitionFormatTarget {
    pub disk: DiskFingerprint,
    pub partition: DiskPartitionFingerprint,
    pub current_label: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum QuickPartitionDialogIntent {
    RefreshInventory,
    RequestConfirmation(QuickPartitionRequest),
    RequestFormatOptions(PartitionFormatTarget),
    ApplyPending(Vec<PendingPartitionOperation>),
    CloseWithPending { apply_allowed: bool },
    Close,
}

#[derive(Clone, Copy)]
struct Controls {
    disk_label: HWND,
    disk: HWND,
    style_label: HWND,
    style_mbr: HWND,
    style_gpt: HWND,
    recommendation: HWND,
    partition_map: HWND,
    add_partition: HWND,
    add_esp: HWND,
    delete: HWND,
    partitions: HWND,
    size_label: HWND,
    size: HWND,
    apply_size: HWND,
    apply_pending: HWND,
    warning: HWND,
    status: HWND,
}

pub struct NativeQuickPartitionDialog {
    pub shell: DialogShell,
    controls: Controls,
    state: QuickPartitionDialogState,
    font: HFONT,
    partition_map: Box<PartitionMapModel>,
    pending: Vec<PendingPartitionOperation>,
    inventory_stale: bool,
}

impl NativeQuickPartitionDialog {
    pub unsafe fn create(
        owner: HWND,
        recommended_style: PartitionStyle,
        used_drive_letters: Vec<char>,
        system_drive: char,
    ) -> windows::core::Result<Self> {
        let shell = DialogShell::create(
            owner,
            DialogSpec {
                window_title: crate::tr!("一键分区"),
                title: crate::tr!("一键分区"),
                description: crate::tr!("选择物理磁盘并规划要创建的分区。"),
                width: 780,
                height: 650,
                buttons: DialogButtons {
                    primary: crate::tr!("清空整盘并分区"),
                    secondary: Some(crate::tr!("刷新")),
                    cancel: Some(crate::tr!("关闭")),
                },
            },
        )?;
        let dpi = GetDpiForWindow(shell.hwnd()).max(96);
        let face = wide("Microsoft YaHei UI");
        let font = CreateFontW(
            -scale(14, dpi),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            1,
            0,
            0,
            5,
            0,
            PCWSTR(face.as_ptr()),
        );
        let controls = create_controls(shell.content(), shell.hwnd())?;
        let mut partition_map = Box::new(PartitionMapModel::new(font));
        let _ = SetWindowSubclass(
            controls.partition_map,
            Some(partition_map_proc),
            PARTITION_MAP_SUBCLASS_ID,
            (&mut *partition_map as *mut PartitionMapModel) as usize,
        );
        let mut dialog = Self {
            shell,
            controls,
            state: QuickPartitionDialogState::new(
                recommended_style,
                used_drive_letters,
                system_drive,
            ),
            font,
            partition_map,
            pending: Vec::new(),
            inventory_stale: false,
        };
        dialog.apply_font_and_theme();
        dialog.layout();
        dialog.render_state();
        Ok(dialog)
    }

    pub fn state(&self) -> &QuickPartitionDialogState {
        &self.state
    }

    pub fn owns_choice(&self, control: HWND) -> bool {
        control == self.controls.disk
    }

    pub fn owns_list(&self, control: HWND) -> bool {
        control == self.controls.partitions
    }

    pub fn owns_command(command_id: u16) -> bool {
        matches!(
            command_id,
            ID_STYLE_MBR
                | ID_STYLE_GPT
                | ID_ADD_PARTITION
                | ID_ADD_ESP
                | ID_DELETE
                | ID_APPLY_SIZE
                | ID_APPLY_PENDING
                | ID_MAP_DRAG_COMMIT
                | ID_MAP_SELECT
                | ID_MAP_RESIZE
                | ID_MAP_DELETE
                | ID_MAP_FORMAT_NTFS
                | ID_MAP_REMOVE_LETTER
                | ID_MAP_SET_ACTIVE
                | ID_MAP_CLEAR_ACTIVE
        ) || (ID_MAP_ASSIGN_LETTER_FIRST..ID_MAP_ASSIGN_LETTER_FIRST + 26).contains(&command_id)
            || (ID_MAP_CREATE_LETTER_FIRST..ID_MAP_CREATE_LETTER_FIRST + 26).contains(&command_id)
    }

    pub unsafe fn set_loading(&mut self) {
        self.state.begin_refresh();
        self.render_state();
    }

    pub unsafe fn set_inventory(&mut self, result: Result<Vec<PhysicalDisk>, String>) {
        self.state.apply_inventory(result);
        self.inventory_stale = false;
        self.render_state();
    }

    pub fn has_pending_changes(&self) -> bool {
        !self.pending.is_empty()
    }

    pub unsafe fn mark_inventory_changed(&mut self) -> bool {
        if self.state.loading {
            return false;
        }
        if self.pending.is_empty() {
            return true;
        }
        self.inventory_stale = true;
        self.state.message = crate::tr!("磁盘布局已在外部发生变化；请刷新并重新检查暂存修改。");
        self.render_state();
        false
    }

    pub unsafe fn discard_pending(&mut self) {
        self.pending.clear();
        self.inventory_stale = false;
        self.state.message.clear();
        self.render_state();
    }

    pub unsafe fn finish_pending_apply(&mut self) {
        self.pending.clear();
        self.inventory_stale = false;
        self.set_loading();
    }

    pub unsafe fn set_operation_error(&mut self, message: impl Into<String>) {
        self.state.loading = false;
        self.state.message = message.into();
        self.render_state();
    }

    pub unsafe fn set_operation_status(&mut self, message: impl Into<String>) {
        self.state.loading = true;
        self.state.message = message.into();
        self.render_state();
    }

    pub fn pending_operations(&self) -> Vec<PendingPartitionOperation> {
        self.pending.clone()
    }

    pub unsafe fn handle_choice_changed(&mut self, control: HWND) -> bool {
        if control != self.controls.disk {
            return false;
        }
        let index = SendMessageW(control, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
        let number = combo_inventory_index(index, self.state.disks.len())
            .and_then(|index| self.state.disks.get(index))
            .map(|disk| disk.disk_number);
        self.state.select_disk(number);
        self.render_state();
        true
    }

    pub unsafe fn handle_list_changed(&mut self) {
        let rows = self.state.rows().collect::<Vec<_>>();
        let index = SendMessageW(
            self.controls.partitions,
            LVM_GETNEXTITEM,
            WPARAM(usize::MAX),
            LPARAM(LVNI_SELECTED),
        )
        .0;
        self.state.select_row(
            (index >= 0)
                .then(|| rows.get(index as usize).copied())
                .flatten(),
        );
        self.render_selection();
    }

    pub unsafe fn handle_command(&mut self, command_id: u16) -> Option<QuickPartitionDialogIntent> {
        match command_id {
            ID_STYLE_MBR => self.state.set_partition_style(PartitionStyle::MBR),
            ID_STYLE_GPT => self.state.set_partition_style(PartitionStyle::GPT),
            ID_ADD_PARTITION => {
                self.state.add_data_partition();
            }
            ID_ADD_ESP => {
                self.state.add_esp_partition();
            }
            ID_DELETE => {
                self.state.delete_selected();
            }
            ID_APPLY_SIZE => {
                self.state.resize_size_text = window_text(self.controls.size);
                if !matches!(self.state.selected_row, Some(EditorRow::Planned(_))) {
                    return None;
                }
                match self.state.apply_resize_text() {
                    Ok(Some(_)) => unreachable!("planned resize never returns an existing request"),
                    Ok(None) => {}
                    Err(error) => self.state.message = error,
                }
            }
            ID_APPLY_PENDING => {
                if !self.pending.is_empty() && !self.inventory_stale {
                    return Some(QuickPartitionDialogIntent::ApplyPending(
                        self.pending.clone(),
                    ));
                }
            }
            ID_MAP_DRAG_COMMIT => {
                if let Some((left_index, right_index, left_new_size_mb)) =
                    self.partition_map.committed_transfer.take()
                {
                    let staged_sizes = self.state.selected_disk().and_then(|disk| {
                        Some((
                            staged_direct_partition_size_mb(disk, &self.pending, left_index)?,
                            staged_direct_partition_size_mb(disk, &self.pending, right_index)?,
                        ))
                    });
                    if staged_sizes.is_some_and(|(left_current_size_mb, _)| {
                        left_new_size_mb == left_current_size_mb
                    }) {
                        if let Some(disk) = self.state.selected_disk() {
                            if let (Some(left), Some(right)) = (
                                disk.partitions.get(left_index),
                                disk.partitions.get(right_index),
                            ) {
                                self.pending.retain(|operation| {
                                    !matches!(
                                        operation,
                                        PendingPartitionOperation::Transfer(existing)
                                            if transfer_targets_pair(
                                                existing,
                                                left.partition_number,
                                                right.partition_number
                                            )
                                    )
                                });
                            }
                        }
                        self.state.message =
                            crate::tr!("分区间的空间转移已撤销，其他暂存修改保持不变。");
                        self.render_after_map_drag();
                        return None;
                    }
                    let request = staged_sizes
                        .ok_or_else(|| crate::tr!("分区信息不可用"))
                        .and_then(|(left_current_size_mb, right_current_size_mb)| {
                            self.state.adjacent_transfer_request_from_current_sizes_mb(
                                left_index,
                                right_index,
                                left_current_size_mb,
                                right_current_size_mb,
                                left_new_size_mb,
                            )
                        });
                    match request {
                        Ok(request) => self.stage_transfer(request),
                        Err(error) => self.state.message = error,
                    }
                } else if let Some((index, new_size_mb)) =
                    self.partition_map.committed_resize.take()
                {
                    match self.state.existing_resize_request_mb(index, new_size_mb) {
                        Ok(request) => self.stage_resize(request),
                        Err(error) => self.state.message = error,
                    }
                } else if std::mem::take(&mut self.partition_map.growth_blocked_by_neighbor) {
                    self.state.message = crate::tr!("ℹ 分区后方无未分配空间，只能缩小");
                }
                // A drag changes only the staged geometry and status. Rebuilding the unchanged
                // ListView here deletes and reinserts every row, which is both unnecessary and
                // visibly flashes on repeated drags even with LVS_EX_DOUBLEBUFFER.
                self.render_after_map_drag();
                return None;
            }
            ID_MAP_SELECT => {
                self.select_map_target(self.partition_map.selected);
            }
            ID_MAP_RESIZE => {
                self.state.message = crate::tr!("拖动分区图上的调整手柄来扩大或缩小分区。");
            }
            ID_MAP_DELETE => {
                self.stage_partition_action(|partition| PartitionManagementAction::Delete {
                    partition,
                });
            }
            ID_MAP_FORMAT_NTFS => {
                return self.partition_format_intent();
            }
            ID_MAP_REMOVE_LETTER => {
                self.stage_partition_action(|partition| {
                    PartitionManagementAction::RemoveDriveLetter { partition }
                });
            }
            ID_MAP_SET_ACTIVE | ID_MAP_CLEAR_ACTIVE => {
                let active = command_id == ID_MAP_SET_ACTIVE;
                self.stage_partition_action(|partition| PartitionManagementAction::SetMbrActive {
                    partition,
                    active,
                });
            }
            _ => {}
        }
        if (ID_MAP_ASSIGN_LETTER_FIRST..ID_MAP_ASSIGN_LETTER_FIRST + 26).contains(&command_id) {
            let drive_letter = char::from(b'A' + (command_id - ID_MAP_ASSIGN_LETTER_FIRST) as u8);
            self.stage_partition_action(|partition| PartitionManagementAction::AssignDriveLetter {
                partition,
                drive_letter,
            });
        }
        if (ID_MAP_CREATE_LETTER_FIRST..ID_MAP_CREATE_LETTER_FIRST + 26).contains(&command_id) {
            let drive_letter = char::from(b'A' + (command_id - ID_MAP_CREATE_LETTER_FIRST) as u8);
            let Some(PartitionMapTarget::Unallocated {
                offset_bytes,
                size_bytes,
            }) = self.partition_map.context_target
            else {
                self.state.message = crate::tr!("未选择未分配空间");
                self.render_state();
                return None;
            };
            let Some(disk) = self.state.selected_disk() else {
                self.state.message = crate::tr!("请先选择要分区的磁盘");
                self.render_state();
                return None;
            };
            let request = PartitionManagementRequest {
                disk: DiskFingerprint::from(disk),
                action: PartitionManagementAction::CreateNtfs {
                    offset_bytes,
                    size_bytes,
                    drive_letter,
                    initialize_style: (!disk.is_initialized).then_some(self.state.partition_style),
                },
            };
            self.stage_management(request);
        }
        self.render_state();
        None
    }

    unsafe fn render_after_map_drag(&mut self) {
        self.refresh_partition_map();
        set_text(self.controls.status, &self.state.message);
        let _ = EnableWindow(
            self.controls.apply_pending,
            !self.pending.is_empty() && !self.inventory_stale && !self.state.loading,
        );
        self.layout();
    }

    unsafe fn select_map_target(&mut self, target: Option<PartitionMapTarget>) {
        match target {
            Some(PartitionMapTarget::Existing(index)) => {
                self.state.select_row(Some(EditorRow::Existing(index)));
            }
            _ => self.state.select_row(None),
        }
        self.render_state();
    }

    unsafe fn stage_partition_action(
        &mut self,
        build: impl FnOnce(DiskPartitionFingerprint) -> PartitionManagementAction,
    ) {
        let Some(PartitionMapTarget::Existing(index)) = self.partition_map.context_target else {
            self.state.message = crate::tr!("未选择已有分区");
            self.render_state();
            return;
        };
        let Some(disk) = self.state.selected_disk() else {
            self.state.message = crate::tr!("请先选择要分区的磁盘");
            self.render_state();
            return;
        };
        let Some(partition) = disk.partitions.get(index) else {
            self.state.message = crate::tr!("分区信息不可用");
            self.render_state();
            return;
        };
        self.stage_management(PartitionManagementRequest {
            disk: DiskFingerprint::from(disk),
            action: build(DiskPartitionFingerprint::from(partition)),
        });
    }

    unsafe fn partition_format_intent(&mut self) -> Option<QuickPartitionDialogIntent> {
        let PartitionMapTarget::Existing(index) = self.partition_map.context_target? else {
            return None;
        };
        let disk = self.state.selected_disk()?;
        let partition = disk.partitions.get(index)?;
        if self.partition_map.segments.iter().any(|segment| {
            segment.target == PartitionMapTarget::Existing(index)
                && (segment.protected || segment.special || segment.drive_letter.is_none())
        }) {
            return None;
        }
        Some(QuickPartitionDialogIntent::RequestFormatOptions(
            PartitionFormatTarget {
                disk: DiskFingerprint::from(disk),
                partition: DiskPartitionFingerprint::from(partition),
                current_label: partition.label.clone(),
            },
        ))
    }

    fn stage_resize(&mut self, request: ExistingPartitionResizeRequest) {
        self.pending.retain(|operation| match operation {
            PendingPartitionOperation::Resize(existing) => {
                existing.partition_number != request.partition_number
            }
            PendingPartitionOperation::Transfer(existing) => {
                existing.left_partition.partition_number != request.partition_number
                    && existing.right_partition.partition_number != request.partition_number
            }
            PendingPartitionOperation::Manage(existing) => !matches!(
                &existing.action,
                PartitionManagementAction::Delete { partition }
                    if partition.partition_number == request.partition_number
            ),
        });
        if request.new_size_mb != request.current_size_mb {
            self.pending
                .push(PendingPartitionOperation::Resize(request));
        }
        if !self.inventory_stale {
            self.state.message = crate::tr!("修改已暂存，点击“应用”后才会写入磁盘。");
        }
    }

    fn stage_transfer(&mut self, request: AdjacentPartitionTransferRequest) {
        let left = request.left_partition.partition_number;
        let right = request.right_partition.partition_number;
        self.pending
            .retain(|operation| retain_operation_before_transfer(operation, &request, left, right));
        self.pending
            .push(PendingPartitionOperation::Transfer(request));
        if !self.inventory_stale {
            self.state.message =
                crate::tr!("相邻分区空间转移已暂存，点击“应用”后进入 WinPE 执行。");
        }
    }

    pub unsafe fn stage_format(
        &mut self,
        target: PartitionFormatTarget,
        options: lr_core::windows_storage::FormatOptions,
    ) {
        self.stage_management(PartitionManagementRequest {
            disk: target.disk,
            action: PartitionManagementAction::Format {
                partition: target.partition,
                options,
            },
        });
        self.render_state();
    }

    fn stage_management(&mut self, request: PartitionManagementRequest) {
        let key = management_partition_offset(&request.action);
        let deleting_partition = match &request.action {
            PartitionManagementAction::Delete { partition } => Some(partition.partition_number),
            _ => None,
        };
        self.pending.retain(|operation| match operation {
            PendingPartitionOperation::Manage(existing) => {
                management_partition_offset(&existing.action) != key || key.is_none()
            }
            PendingPartitionOperation::Resize(existing) => {
                deleting_partition != Some(existing.partition_number)
            }
            PendingPartitionOperation::Transfer(existing) => {
                deleting_partition != Some(existing.left_partition.partition_number)
                    && deleting_partition != Some(existing.right_partition.partition_number)
            }
        });
        self.pending
            .push(PendingPartitionOperation::Manage(request));
        if !self.inventory_stale {
            self.state.message = crate::tr!("修改已暂存，点击“应用”后才会写入磁盘。");
        }
    }

    pub unsafe fn show_modeless(&mut self) {
        self.layout();
        self.shell.show_modeless();
        // Reassert the shared Inno radio painter after the shell's final descendant theme pass;
        // USER32 still owns grouping, keyboard input and accessibility for both partition styles.
        self.apply_font_and_theme();
    }

    pub unsafe fn take_intent(&mut self) -> Option<QuickPartitionDialogIntent> {
        match self.shell.take_result()? {
            DialogResult::Secondary => {
                if !self.pending.is_empty() {
                    self.state.message = crate::tr!("请先应用或放弃暂存修改，再刷新磁盘布局。");
                    self.render_state();
                    self.shell.show_modeless();
                    return None;
                }
                self.set_loading();
                Some(QuickPartitionDialogIntent::RefreshInventory)
            }
            DialogResult::Primary => match self.state.quick_partition_request() {
                Ok(request) => Some(QuickPartitionDialogIntent::RequestConfirmation(request)),
                Err(error) => {
                    self.state.message = error;
                    self.render_state();
                    self.shell.show_modeless();
                    self.apply_font_and_theme();
                    None
                }
            },
            DialogResult::Cancel if !self.pending.is_empty() => {
                Some(QuickPartitionDialogIntent::CloseWithPending {
                    apply_allowed: !self.inventory_stale,
                })
            }
            DialogResult::Cancel => Some(QuickPartitionDialogIntent::Close),
        }
    }

    pub unsafe fn layout(&mut self) {
        let dpi = GetDpiForWindow(self.shell.hwnd()).max(96);
        let mut rect = RECT::default();
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let width = (rect.right - rect.left).max(0);
        let metrics = LayoutMetrics::for_dpi(dpi);
        let label_height = metrics.label_height;
        let label_offset = ((metrics.field_height - label_height) / 2).max(0);
        let disk_label_width = measure_text(
            self.shell.hwnd(),
            self.font,
            &window_text(self.controls.disk_label),
            None,
        )
        .width;
        let disk_field = arrange_field(width, disk_label_width, scale(260, dpi), dpi);
        let mut y = 0;
        match disk_field {
            FieldArrangement::Inline {
                label_width,
                control_x,
                control_width,
            } => {
                move_control(
                    self.controls.disk_label,
                    0,
                    y + label_offset,
                    label_width,
                    label_height,
                );
                move_control(
                    self.controls.disk,
                    control_x,
                    y,
                    control_width,
                    scale(180, dpi),
                );
                y += metrics.field_height;
            }
            FieldArrangement::Stacked => {
                move_control(self.controls.disk_label, 0, y, width, label_height);
                y += label_height + metrics.tight_gap;
                move_control(self.controls.disk, 0, y, width, scale(180, dpi));
                y += metrics.field_height;
            }
        }
        y += metrics.control_gap;

        let style_label_width = measure_text(
            self.shell.hwnd(),
            self.font,
            &window_text(self.controls.style_label),
            None,
        )
        .width;
        let mbr_width = measured_button_width(
            self.shell.hwnd(),
            self.font,
            &crate::tr!("BIOS (MBR)"),
            dpi,
            scale(64, dpi),
        );
        let gpt_width = measured_button_width(
            self.shell.hwnd(),
            self.font,
            &crate::tr!("UEFI (GPT)"),
            dpi,
            scale(64, dpi),
        );
        let mut x = 0;
        move_control(
            self.controls.style_label,
            x,
            y + label_offset,
            style_label_width,
            label_height,
        );
        x += style_label_width + metrics.control_gap;
        move_control(
            self.controls.style_mbr,
            x,
            y,
            mbr_width,
            metrics.button_height,
        );
        x += mbr_width + metrics.control_gap;
        move_control(
            self.controls.style_gpt,
            x,
            y,
            gpt_width,
            metrics.button_height,
        );
        x += gpt_width + metrics.control_gap;
        let recommendation = window_text(self.controls.recommendation);
        let recommendation_width =
            measure_text(self.shell.hwnd(), self.font, &recommendation, None).width;
        if x + recommendation_width <= width {
            move_control(
                self.controls.recommendation,
                x,
                y + label_offset,
                width - x,
                label_height,
            );
            y += metrics.button_height;
        } else {
            y += metrics.button_height + metrics.tight_gap;
            move_control(self.controls.recommendation, 0, y, width, label_height);
            y += label_height;
        }
        y += metrics.control_gap;

        move_control(self.controls.partition_map, 0, y, width, scale(68, dpi));
        y += scale(68, dpi) + metrics.control_gap;

        let has_free_space = self.state.selected_disk_unallocated_gb() >= 0.5;
        let can_create_esp = has_free_space
            && self.state.partition_style == PartitionStyle::GPT
            && !self.state.selected_disk_has_esp();
        x = 0;
        for (control, visible) in [
            (self.controls.add_partition, has_free_space),
            (self.controls.add_esp, can_create_esp),
            (self.controls.delete, true),
        ] {
            if !visible {
                continue;
            }
            let button_width = measured_button_width(
                self.shell.hwnd(),
                self.font,
                &window_text(control),
                dpi,
                scale(75, dpi),
            );
            if x > 0 && x + button_width > width {
                x = 0;
                y += metrics.button_height + metrics.control_gap;
            }
            move_control(control, x, y, button_width, metrics.button_height);
            x += button_width + metrics.control_gap;
        }
        y += metrics.button_height + metrics.control_gap;

        let list_height = preferred_list_height(self.state.rows().count(), dpi, 3, 8);
        move_control(self.controls.partitions, 0, y, width, list_height);
        y += list_height + metrics.control_gap;

        let planned_selected = matches!(self.state.selected_row, Some(EditorRow::Planned(_)));
        let size_label_width = measure_text(
            self.shell.hwnd(),
            self.font,
            &window_text(self.controls.size_label),
            None,
        )
        .width;
        let size_width = scale(105, dpi);
        let apply_width = measured_button_width(
            self.shell.hwnd(),
            self.font,
            &window_text(self.controls.apply_size),
            dpi,
            scale(75, dpi),
        );
        x = 0;
        if planned_selected {
            move_control(
                self.controls.size_label,
                0,
                y + label_offset,
                size_label_width,
                label_height,
            );
            x = size_label_width + metrics.control_gap;
            move_control(self.controls.size, x, y, size_width, metrics.field_height);
            x += size_width + metrics.control_gap;
            move_control(
                self.controls.apply_size,
                x,
                y,
                apply_width,
                metrics.button_height,
            );
            x += apply_width + metrics.control_gap;
        }
        let warning_text = window_text(self.controls.warning);
        let warning_width = measure_text(self.shell.hwnd(), self.font, &warning_text, None).width;
        if x + warning_width <= width {
            move_control(
                self.controls.warning,
                x,
                y + label_offset,
                width - x,
                label_height,
            );
            y += metrics.field_height;
        } else {
            y += metrics.field_height + metrics.tight_gap;
            let warning_height =
                measure_text(self.shell.hwnd(), self.font, &warning_text, Some(width))
                    .height
                    .max(label_height);
            move_control(self.controls.warning, 0, y, width, warning_height);
            y += warning_height;
        }
        let status_text = window_text(self.controls.status);
        if !status_text.is_empty() {
            y += metrics.control_gap;
            let status_height =
                measure_text(self.shell.hwnd(), self.font, &status_text, Some(width))
                    .height
                    .max(label_height);
            move_control(self.controls.status, 0, y, width, status_height);
            y += status_height;
        }
        self.shell.fit_content_height(logical_height(y, dpi));
        self.layout_apply_button(dpi);
        let mut list_rect = RECT::default();
        let _ = GetClientRect(self.controls.partitions, &mut list_rect);
        let list_width = (list_rect.right - list_rect.left).max(0);
        for (column, value) in partition_columns(list_width, dpi).into_iter().enumerate() {
            let _ = SendMessageW(
                self.controls.partitions,
                LVM_SETCOLUMNWIDTH,
                WPARAM(column),
                LPARAM(value as isize),
            );
        }
    }

    unsafe fn layout_apply_button(&self, dpi: u32) {
        let Some(refresh) = self.shell.command_button(DialogResult::Secondary) else {
            return;
        };
        let Some(primary) = self.shell.command_button(DialogResult::Primary) else {
            return;
        };
        let mut refresh_rect = RECT::default();
        let mut primary_rect = RECT::default();
        let _ = GetWindowRect(refresh, &mut refresh_rect);
        let _ = GetWindowRect(primary, &mut primary_rect);
        let mut points = [
            POINT {
                x: refresh_rect.left,
                y: refresh_rect.top,
            },
            POINT {
                x: refresh_rect.right,
                y: refresh_rect.bottom,
            },
        ];
        let _ = MapWindowPoints(HWND::default(), self.shell.hwnd(), &mut points);
        let mut primary_points = [
            POINT {
                x: primary_rect.left,
                y: primary_rect.top,
            },
            POINT {
                x: primary_rect.right,
                y: primary_rect.bottom,
            },
        ];
        let _ = MapWindowPoints(HWND::default(), self.shell.hwnd(), &mut primary_points);
        let gap = LayoutMetrics::for_dpi(dpi).control_gap;
        let width = measured_button_width(
            self.shell.hwnd(),
            self.font,
            &window_text(self.controls.apply_pending),
            dpi,
            scale(68, dpi),
        );
        let refresh_width = (points[1].x - points[0].x).max(1);
        let height = (points[1].y - points[0].y).max(1);
        let apply_x = primary_points[0].x - gap - width;
        let refresh_x = apply_x - gap - refresh_width;
        move_control(refresh, refresh_x, points[0].y, refresh_width, height);
        move_control(
            self.controls.apply_pending,
            apply_x,
            points[0].y,
            width,
            height,
        );
    }

    unsafe fn render_state(&mut self) {
        refill_disks(
            self.controls.disk,
            &self.state.disks,
            self.state.selected_disk_number,
        );
        set_radio(
            self.controls.style_mbr,
            self.state.partition_style == PartitionStyle::MBR,
        );
        set_radio(
            self.controls.style_gpt,
            self.state.partition_style == PartitionStyle::GPT,
        );
        set_text(
            self.controls.recommendation,
            &crate::tr!("推荐：{}", self.state.recommended_style.to_string()),
        );
        refill_partitions(self.controls.partitions, &self.state);
        self.refresh_partition_map();
        self.render_selection();
        let has_disk = !self.state.loading && self.state.selected_disk().is_some();
        let _ = EnableWindow(self.controls.disk, !self.state.loading);
        for control in [
            self.controls.style_mbr,
            self.controls.style_gpt,
            self.controls.add_partition,
            self.controls.partition_map,
            self.controls.partitions,
        ] {
            let _ = EnableWindow(control, has_disk);
        }
        let has_free_space = self.state.selected_disk_unallocated_gb() >= 0.5;
        let can_create_esp = has_disk
            && has_free_space
            && self.state.partition_style == PartitionStyle::GPT
            && !self.state.selected_disk_has_esp();
        let _ = EnableWindow(self.controls.add_esp, can_create_esp);
        let _ = ShowWindow(
            self.controls.add_partition,
            if has_disk && has_free_space {
                SW_SHOW
            } else {
                SW_HIDE
            },
        );
        let _ = ShowWindow(
            self.controls.add_esp,
            if can_create_esp { SW_SHOW } else { SW_HIDE },
        );
        let _ = EnableWindow(
            self.controls.apply_pending,
            !self.pending.is_empty() && !self.inventory_stale && !self.state.loading,
        );
        self.shell
            .set_primary_enabled(self.state.quick_partition_request().is_ok());
        // Inventory arrives asynchronously. Recompute the row-count-based ListView and dialog
        // heights after every full state replacement instead of leaving the three-row loading
        // geometry in place for the lifetime of the dialog.
        self.layout();
    }

    unsafe fn refresh_partition_map(&mut self) {
        let selected = self.state.selected_row.and_then(|row| match row {
            EditorRow::Existing(index) => Some(PartitionMapTarget::Existing(index)),
            EditorRow::Planned(_) => None,
        });
        self.partition_map.segments.clear();
        self.partition_map.selected = selected;
        self.partition_map.context_target = self
            .partition_map
            .context_target
            .filter(|target| selected_disk_contains_target(self.state.selected_disk(), *target));
        self.partition_map.available_letters = self.state.available_drive_letters();
        self.partition_map.style = self.state.partition_style;
        self.partition_map.enabled = !self.state.loading && self.state.selected_disk().is_some();
        let Some(disk) = self.state.selected_disk() else {
            self.partition_map.initialized = false;
            let _ = InvalidateRect(self.controls.partition_map, None, false);
            return;
        };
        self.partition_map.initialized = disk.is_initialized;
        let mut ordered = disk.partitions.iter().enumerate().collect::<Vec<_>>();
        ordered.sort_by_key(|(_, partition)| partition.offset_bytes);
        let mut cursor = 0_u64;
        for (index, partition) in ordered {
            if partition.offset_bytes > cursor {
                push_unallocated_segment(
                    &mut self.partition_map.segments,
                    cursor,
                    partition.offset_bytes - cursor,
                );
            }
            let name = partition_name(
                partition.drive_letter,
                partition.is_esp,
                partition.is_msr,
                partition.is_recovery,
            );
            let label = if partition.label.trim().is_empty() {
                name
            } else {
                format!("{name}  {}", partition.label.trim())
            };
            self.partition_map.segments.push(PartitionMapSegment {
                target: PartitionMapTarget::Existing(index),
                label,
                size: format_capacity(partition.size_bytes),
                weight: partition.size_bytes.max(1),
                special: partition.is_esp || partition.is_msr || partition.is_recovery,
                protected: partition.drive_letter.is_some_and(|letter| {
                    letter.eq_ignore_ascii_case(&self.state.running_windows_drive())
                }) || !partition.file_system.trim().eq_ignore_ascii_case("NTFS"),
                drive_letter: partition.drive_letter,
                active: partition.is_active,
                minimum_bytes: partition
                    .used_bytes
                    .saturating_add(100 * 1024 * 1024)
                    .max(512 * 1024 * 1024),
            });
            cursor = cursor.max(partition.offset_bytes.saturating_add(partition.size_bytes));
        }
        if cursor < disk.size_bytes {
            push_unallocated_segment(
                &mut self.partition_map.segments,
                cursor,
                disk.size_bytes - cursor,
            );
        }
        for operation in &self.pending {
            if let PendingPartitionOperation::Transfer(request) = operation {
                let left_partition_index = disk.partitions.iter().position(|partition| {
                    partition.partition_number == request.left_partition.partition_number
                });
                let right_partition_index = disk.partitions.iter().position(|partition| {
                    partition.partition_number == request.right_partition.partition_number
                });
                let (Some(left_partition_index), Some(right_partition_index)) =
                    (left_partition_index, right_partition_index)
                else {
                    continue;
                };
                for (partition_index, size_mb) in [
                    (left_partition_index, request.left_new_size_mb),
                    (right_partition_index, request.right_new_size_mb),
                ] {
                    if let Some(segment) = self.partition_map.segments.iter_mut().find(|segment| {
                        segment.target == PartitionMapTarget::Existing(partition_index)
                    }) {
                        segment.weight = size_mb.saturating_mul(1024 * 1024);
                        segment.size = format_capacity(segment.weight);
                    }
                }
                continue;
            }
            let PendingPartitionOperation::Resize(request) = operation else {
                continue;
            };
            let Some(partition_index) = disk
                .partitions
                .iter()
                .position(|partition| partition.partition_number == request.partition_number)
            else {
                continue;
            };
            let Some(segment_index) = self.partition_map.segments.iter().position(|segment| {
                segment.target == PartitionMapTarget::Existing(partition_index)
            }) else {
                continue;
            };
            let new_bytes = request.new_size_mb.saturating_mul(1024 * 1024);
            let current_bytes = disk.partitions[partition_index].size_bytes;
            if new_bytes == current_bytes {
                continue;
            }
            let offset = disk.partitions[partition_index].offset_bytes;
            self.partition_map.segments[segment_index].weight = new_bytes;
            self.partition_map.segments[segment_index].size = format_capacity(new_bytes);
            let next_is_unallocated = self
                .partition_map
                .segments
                .get(segment_index + 1)
                .is_some_and(|segment| {
                    matches!(segment.target, PartitionMapTarget::Unallocated { .. })
                });
            if new_bytes < current_bytes {
                let released = current_bytes - new_bytes;
                if next_is_unallocated {
                    let free = self.partition_map.segments[segment_index + 1]
                        .weight
                        .saturating_add(released);
                    self.partition_map.segments[segment_index + 1].weight = free;
                    self.partition_map.segments[segment_index + 1].size = format_capacity(free);
                    self.partition_map.segments[segment_index + 1].target =
                        PartitionMapTarget::Unallocated {
                            offset_bytes: offset.saturating_add(new_bytes),
                            size_bytes: free,
                        };
                } else {
                    self.partition_map.segments.insert(
                        segment_index + 1,
                        unallocated_segment(offset.saturating_add(new_bytes), released),
                    );
                }
            } else if next_is_unallocated {
                let consumed = new_bytes - current_bytes;
                let available = self.partition_map.segments[segment_index + 1].weight;
                if consumed < available {
                    let free = available - consumed;
                    self.partition_map.segments[segment_index + 1].weight = free;
                    self.partition_map.segments[segment_index + 1].size = format_capacity(free);
                    self.partition_map.segments[segment_index + 1].target =
                        PartitionMapTarget::Unallocated {
                            offset_bytes: offset.saturating_add(new_bytes),
                            size_bytes: free,
                        };
                } else if consumed == available {
                    self.partition_map.segments.remove(segment_index + 1);
                }
            } else if request.new_size_mb > request.no_move_max_size_mb {
                let borrowed = new_bytes.saturating_sub(current_bytes);
                if let Some(next) = self.partition_map.segments.get_mut(segment_index + 1) {
                    next.weight = next.weight.saturating_sub(borrowed).max(1);
                    next.size = format_capacity(next.weight);
                }
            }
        }
        let _ = InvalidateRect(self.controls.partition_map, None, false);
    }

    unsafe fn render_selection(&self) {
        set_text(self.controls.size, &self.state.resize_size_text);
        let selected = self.state.selected_row.is_some();
        let planned = matches!(self.state.selected_row, Some(EditorRow::Planned(_)));
        let _ = ShowWindow(
            self.controls.size_label,
            if planned { SW_SHOW } else { SW_HIDE },
        );
        let _ = ShowWindow(self.controls.size, if planned { SW_SHOW } else { SW_HIDE });
        let _ = ShowWindow(
            self.controls.apply_size,
            if planned { SW_SHOW } else { SW_HIDE },
        );
        let _ = EnableWindow(self.controls.size, selected && planned);
        let _ = EnableWindow(
            self.controls.apply_size,
            matches!(self.state.selected_row, Some(EditorRow::Planned(_))),
        );
        // Disabled/enabled transitions can leave USER32's previous one-pixel bottom edge cached
        // until another input message.  Repaint only this owner-drawn button, without erasing the
        // row behind it, so all four Inno edges are present in the same frame.
        let _ = InvalidateRect(self.controls.apply_size, None, false);
        let _ = EnableWindow(
            self.controls.delete,
            matches!(self.state.selected_row, Some(EditorRow::Planned(_))),
        );
        set_text(self.controls.status, &self.state.message);
    }

    unsafe fn apply_font_and_theme(&self) {
        let palette = Palette::system();
        for control in self.controls() {
            let _ = SendMessageW(control, WM_SETFONT, WPARAM(self.font.0 as usize), LPARAM(1));
        }
        apply_control_theme(self.controls.disk, palette, NativeControlKind::Field);
        apply_control_theme(self.controls.size, palette, NativeControlKind::Field);
        apply_list_view_theme(self.controls.partitions, palette);
        for radio in [self.controls.style_mbr, self.controls.style_gpt] {
            apply_control_theme(radio, palette, RADIO_CONTROL_KIND);
        }
        for button in [
            self.controls.add_partition,
            self.controls.add_esp,
            self.controls.delete,
            self.controls.apply_size,
            self.controls.apply_pending,
        ] {
            apply_control_theme(button, palette, NativeControlKind::General);
        }
        for (message, color) in [
            (LVM_SETBKCOLOR, palette.edit),
            (LVM_SETTEXTBKCOLOR, palette.edit),
            (LVM_SETTEXTCOLOR, palette.text),
        ] {
            let _ = SendMessageW(
                self.controls.partitions,
                message,
                WPARAM(0),
                LPARAM(color.0 as isize),
            );
        }
    }

    fn controls(&self) -> [HWND; 17] {
        let c = self.controls;
        [
            c.disk_label,
            c.disk,
            c.style_label,
            c.style_mbr,
            c.style_gpt,
            c.recommendation,
            c.partition_map,
            c.add_partition,
            c.add_esp,
            c.delete,
            c.partitions,
            c.size_label,
            c.size,
            c.apply_size,
            c.apply_pending,
            c.warning,
            c.status,
        ]
    }
}

impl Drop for NativeQuickPartitionDialog {
    fn drop(&mut self) {
        unsafe {
            let _ = RemoveWindowSubclass(
                self.controls.partition_map,
                Some(partition_map_proc),
                PARTITION_MAP_SUBCLASS_ID,
            );
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font);
            }
        }
    }
}

fn management_partition_offset(action: &PartitionManagementAction) -> Option<u64> {
    match action {
        PartitionManagementAction::Delete { partition }
        | PartitionManagementAction::Format { partition, .. }
        | PartitionManagementAction::AssignDriveLetter { partition, .. }
        | PartitionManagementAction::RemoveDriveLetter { partition }
        | PartitionManagementAction::SetMbrActive { partition, .. } => Some(partition.offset_bytes),
        PartitionManagementAction::CreateNtfs { offset_bytes, .. } => Some(*offset_bytes),
    }
}

fn management_targets_partition(action: &PartitionManagementAction, partition_number: u32) -> bool {
    match action {
        PartitionManagementAction::Delete { partition }
        | PartitionManagementAction::Format { partition, .. }
        | PartitionManagementAction::AssignDriveLetter { partition, .. }
        | PartitionManagementAction::RemoveDriveLetter { partition }
        | PartitionManagementAction::SetMbrActive { partition, .. } => {
            partition.partition_number == partition_number
        }
        PartitionManagementAction::CreateNtfs { .. } => false,
    }
}

fn staged_direct_partition_size_mb(
    disk: &PhysicalDisk,
    pending: &[PendingPartitionOperation],
    partition_index: usize,
) -> Option<u64> {
    let partition = disk.partitions.get(partition_index)?;
    let original_size_mb = partition.size_bytes / 1024 / 1024;
    Some(
        pending
            .iter()
            .filter_map(|operation| match operation {
                PendingPartitionOperation::Resize(request)
                    if request.partition_number == partition.partition_number
                        && request.new_size_mb <= request.no_move_max_size_mb =>
                {
                    Some(request.new_size_mb)
                }
                _ => None,
            })
            .next_back()
            .unwrap_or(original_size_mb),
    )
}

fn retain_operation_before_transfer(
    operation: &PendingPartitionOperation,
    request: &AdjacentPartitionTransferRequest,
    left: u32,
    right: u32,
) -> bool {
    match operation {
        PendingPartitionOperation::Resize(existing) => {
            let prerequisite_for_right = existing.partition_number == right
                && existing.new_size_mb > existing.current_size_mb
                && existing.new_size_mb <= existing.no_move_max_size_mb
                && existing.new_size_mb == request.right_current_size_mb;
            prerequisite_for_right
                || (existing.partition_number != left && existing.partition_number != right)
        }
        PendingPartitionOperation::Transfer(existing) => {
            !transfer_targets_pair(existing, left, right)
        }
        PendingPartitionOperation::Manage(existing) => {
            !management_targets_partition(&existing.action, left)
                && !management_targets_partition(&existing.action, right)
        }
    }
}

fn transfer_targets_pair(
    request: &AdjacentPartitionTransferRequest,
    left: u32,
    right: u32,
) -> bool {
    [
        request.left_partition.partition_number,
        request.right_partition.partition_number,
    ]
    .iter()
    .any(|partition| matches!(*partition, value if value == left || value == right))
}

fn selected_disk_contains_target(disk: Option<&PhysicalDisk>, target: PartitionMapTarget) -> bool {
    let Some(disk) = disk else {
        return false;
    };
    match target {
        PartitionMapTarget::Existing(index) => index < disk.partitions.len(),
        PartitionMapTarget::Unallocated {
            offset_bytes,
            size_bytes,
        } => {
            let end = offset_bytes.saturating_add(size_bytes);
            end <= disk.size_bytes
                && disk.partitions.iter().all(|partition| {
                    let partition_end = partition.offset_bytes.saturating_add(partition.size_bytes);
                    offset_bytes >= partition_end || end <= partition.offset_bytes
                })
        }
    }
}

fn push_unallocated_segment(
    segments: &mut Vec<PartitionMapSegment>,
    offset_bytes: u64,
    size_bytes: u64,
) {
    if size_bytes < 1024 * 1024 {
        return;
    }
    segments.push(unallocated_segment(offset_bytes, size_bytes));
}

fn unallocated_segment(offset_bytes: u64, size_bytes: u64) -> PartitionMapSegment {
    PartitionMapSegment {
        target: PartitionMapTarget::Unallocated {
            offset_bytes,
            size_bytes,
        },
        label: crate::tr!("未分配"),
        size: format_capacity(size_bytes),
        weight: size_bytes,
        special: false,
        protected: false,
        drive_letter: None,
        active: false,
        minimum_bytes: 0,
    }
}

fn format_capacity(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", bytes as f64 / GIB)
    } else {
        format!("{:.0} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn partition_map_rects(width: i32, count: usize, weights: &[u64], dpi: u32) -> Vec<(i32, i32)> {
    if count == 0 || width <= 0 {
        return Vec::new();
    }
    let usable = width.max(count as i32);
    let preferred_min = scale(38, dpi);
    let minimum = preferred_min.min((usable / count as i32).max(1));
    let fixed = minimum.saturating_mul(count as i32);
    let flexible = (usable - fixed).max(0);
    let total_weight = weights
        .iter()
        .copied()
        .fold(0_u128, |sum, value| sum + u128::from(value.max(1)));
    let mut result = Vec::with_capacity(count);
    let mut x = 0_i32;
    let mut assigned_flexible = 0_i32;
    let mut cumulative_weight = 0_u128;
    for (index, weight) in weights.iter().copied().enumerate() {
        cumulative_weight += u128::from(weight.max(1));
        let target_flexible = if index + 1 == count {
            flexible
        } else {
            ((u128::from(flexible as u32) * cumulative_weight) / total_weight) as i32
        };
        let part_flexible = target_flexible - assigned_flexible;
        assigned_flexible = target_flexible;
        let segment_width = minimum + part_flexible;
        result.push((x, x + segment_width));
        x += segment_width;
    }
    result
}

unsafe extern "system" fn partition_map_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    let model = &mut *(reference_data as *mut PartitionMapModel);
    match message {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint_partition_map(hwnd, model);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let point = point_from_lparam(lparam);
            if let Some(drag) = begin_partition_map_drag(hwnd, model, point) {
                model.committed_resize = None;
                model.committed_transfer = None;
                model.drag = Some(drag);
                let _ = SetCapture(hwnd);
                let _ = InvalidateRect(hwnd, None, false);
                return LRESULT(0);
            }
            let target = hit_test_partition_map(hwnd, model, point);
            model.selected = target;
            model.context_target = target;
            let _ = InvalidateRect(hwnd, None, false);
            if let Ok(parent) = GetParent(hwnd) {
                let _ = SendMessageW(
                    parent,
                    WM_COMMAND,
                    WPARAM(ID_MAP_SELECT as usize),
                    LPARAM(hwnd.0 as isize),
                );
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if let Some(drag) = &mut model.drag {
                let span = drag.scale_width.max(1);
                let current_x = point_from_lparam(lparam)
                    .x
                    .clamp(drag.start_x - span, drag.start_x + span);
                if current_x != drag.current_x {
                    drag.current_x = current_x;
                    let _ = InvalidateRect(hwnd, None, false);
                }
                return LRESULT(0);
            }
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_LBUTTONUP => {
            if let Some(drag) = model.drag.take() {
                if GetCapture() == hwnd {
                    let _ = ReleaseCapture();
                }
                let target = drag_target_bytes(drag);
                const MIB: u64 = 1024 * 1024;
                let minimum_mib = drag.minimum_bytes.saturating_add(MIB - 1) / MIB;
                let maximum_mib = drag.maximum_bytes / MIB;
                let aligned_mib = (target / MIB).clamp(minimum_mib, maximum_mib);
                model.growth_blocked_by_neighbor = !drag.right_is_unallocated
                    && !drag.right_is_borrowed_partition
                    && drag.current_x > drag.start_x
                    && target == drag.original_bytes;
                let original_mib = drag.original_bytes / MIB;
                model.committed_resize = None;
                model.committed_transfer = None;
                if !model.growth_blocked_by_neighbor && aligned_mib != original_mib {
                    if drag.right_is_borrowed_partition {
                        model.committed_transfer = drag
                            .right_partition_index
                            .map(|right_index| (drag.partition_index, right_index, aligned_mib));
                    } else {
                        model.committed_resize = Some((drag.partition_index, aligned_mib));
                    }
                }
                let _ = InvalidateRect(hwnd, None, false);
                if let Ok(parent) = GetParent(hwnd) {
                    // Defer the commit until this subclass callback has returned. The parent
                    // rerenders the map and mutates this model; doing that through SendMessageW
                    // would re-enter while `model` is still exclusively borrowed here.
                    let _ = PostMessageW(
                        parent,
                        WM_COMMAND,
                        WPARAM(ID_MAP_DRAG_COMMIT as usize),
                        LPARAM(hwnd.0 as isize),
                    );
                }
                return LRESULT(0);
            }
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_CAPTURECHANGED => {
            if model.drag.take().is_some() {
                let _ = InvalidateRect(hwnd, None, false);
            }
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_RBUTTONUP => {
            let point = point_from_lparam(lparam);
            model.context_target = hit_test_partition_map(hwnd, model, point);
            model.selected = model.context_target;
            let _ = InvalidateRect(hwnd, None, false);
            if model.context_target.is_some() {
                show_partition_context_menu(hwnd, model, point);
            }
            LRESULT(0)
        }
        WM_MEASUREITEM => {
            let item = &mut *(lparam.0 as *mut MEASUREITEMSTRUCT);
            if item.CtlType == ODT_MENU && item.itemData != 0 {
                let visual = &*(item.itemData as *const PartitionMenuItem);
                item.itemWidth = visual.width.max(1) as u32;
                item.itemHeight =
                    scale(if visual.separator { 6 } else { 26 }, visual.dpi).max(1) as u32;
                return LRESULT(1);
            }
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_DRAWITEM => {
            let item = &*(lparam.0 as *const DRAWITEMSTRUCT);
            if item.CtlType == ODT_MENU && item.itemData != 0 {
                draw_partition_menu_item(item);
                return LRESULT(1);
            }
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(partition_map_proc), PARTITION_MAP_SUBCLASS_ID);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

unsafe fn draw_partition_menu_item(item: &DRAWITEMSTRUCT) {
    let visual = &*(item.itemData as *const PartitionMenuItem);
    let selected = item.itemState.0 & ODS_SELECTED.0 != 0;
    let disabled = item.itemState.0 & (ODS_DISABLED.0 | ODS_GRAYED.0) != 0;
    let background = if selected && !disabled {
        visual.palette.button_pressed
    } else {
        visual.palette.window
    };
    let brush = CreateSolidBrush(background);
    let _ = FillRect(item.hDC, &item.rcItem, brush);
    let _ = DeleteObject(brush);
    if visual.separator {
        let mut separator = item.rcItem;
        separator.left += scale(10, visual.dpi);
        separator.right -= scale(10, visual.dpi);
        separator.top = (separator.top + separator.bottom) / 2;
        separator.bottom = separator.top + scale(1, visual.dpi).max(1);
        let brush = CreateSolidBrush(visual.palette.separator);
        let _ = FillRect(item.hDC, &separator, brush);
        let _ = DeleteObject(brush);
        return;
    }
    let _ = SetBkMode(item.hDC, TRANSPARENT);
    let _ = SetTextColor(
        item.hDC,
        if disabled {
            visual.palette.text_disabled
        } else {
            visual.palette.text
        },
    );
    let old_font = SelectObject(item.hDC, visual.font);
    let mut text = wide(&visual.text);
    let mut rect = item.rcItem;
    rect.left += scale(14, visual.dpi);
    rect.right -= scale(if visual.submenu { 26 } else { 12 }, visual.dpi);
    let _ = DrawTextW(
        item.hDC,
        &mut text,
        &mut rect,
        DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
    );
    if visual.submenu {
        let mut arrow = wide("›");
        let mut arrow_rect = item.rcItem;
        arrow_rect.left = arrow_rect.right - scale(24, visual.dpi);
        arrow_rect.right -= scale(8, visual.dpi);
        let _ = DrawTextW(
            item.hDC,
            &mut arrow,
            &mut arrow_rect,
            DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
        );
    }
    let _ = SelectObject(item.hDC, old_font);
}

fn drag_target_bytes(drag: PartitionMapDrag) -> u64 {
    let delta = i64::from(drag.current_x - drag.start_x);
    let change =
        (i128::from(delta) * i128::from(drag.scale_bytes)) / i128::from(drag.scale_width.max(1));
    (i128::from(drag.original_bytes) + change).clamp(
        i128::from(drag.minimum_bytes.min(drag.maximum_bytes)),
        i128::from(drag.maximum_bytes),
    ) as u64
}

fn partition_map_display_segments(model: &PartitionMapModel) -> Vec<PartitionMapSegment> {
    let mut segments = model.segments.clone();
    let Some(drag) = model.drag else {
        return segments;
    };
    if drag.left_segment >= segments.len() {
        return segments;
    }
    let target = drag_target_bytes(drag);
    segments[drag.left_segment].weight = target.max(1);
    segments[drag.left_segment].size = format_capacity(target);
    if drag.right_is_unallocated {
        let remaining = drag.scale_bytes.saturating_sub(target);
        if remaining == 0 {
            if drag.left_segment + 1 < segments.len() {
                segments.remove(drag.left_segment + 1);
            }
        } else if let Some(right) = segments.get_mut(drag.left_segment + 1) {
            right.weight = remaining;
            right.size = format_capacity(remaining);
            if let PartitionMapTarget::Unallocated {
                offset_bytes,
                size_bytes,
            } = &mut right.target
            {
                *offset_bytes = offset_bytes.saturating_add(size_bytes.saturating_sub(remaining));
                *size_bytes = remaining;
            }
        }
    } else if drag.right_is_borrowed_partition {
        if let Some(right) = segments.get_mut(drag.left_segment + 1) {
            let combined = drag.scale_bytes;
            right.weight = combined.saturating_sub(target).max(1);
            right.size = format_capacity(right.weight);
        }
    } else if target < drag.original_bytes {
        segments.insert(
            drag.left_segment + 1,
            unallocated_segment(0, drag.original_bytes - target),
        );
    }
    segments
}

unsafe fn paint_partition_map(hwnd: HWND, model: &PartitionMapModel) {
    let mut paint = PAINTSTRUCT::default();
    let target_dc = BeginPaint(hwnd, &mut paint);
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    let width = (client.right - client.left).max(0);
    let height = (client.bottom - client.top).max(0);
    let buffer = if width > 0 && height > 0 {
        let memory_dc = CreateCompatibleDC(target_dc);
        if memory_dc.is_invalid() {
            None
        } else {
            let bitmap = CreateCompatibleBitmap(target_dc, width, height);
            if bitmap.is_invalid() {
                let _ = DeleteDC(memory_dc);
                None
            } else {
                let old_bitmap = SelectObject(memory_dc, bitmap);
                Some((memory_dc, bitmap, old_bitmap))
            }
        }
    } else {
        None
    };
    let dc = buffer
        .as_ref()
        .map_or(target_dc, |(memory_dc, _, _)| *memory_dc);
    let palette = Palette::system();
    let background = CreateSolidBrush(palette.window);
    let _ = FillRect(dc, &client, background);
    let _ = DeleteObject(background);
    let dpi = GetDpiForWindow(hwnd).max(96);
    let margin = scale(2, dpi);
    let inner_width = (client.right - client.left - margin * 2).max(0);
    let display_segments = partition_map_display_segments(model);
    let weights = display_segments
        .iter()
        .map(|segment| segment.weight)
        .collect::<Vec<_>>();
    let rects = partition_map_rects(inner_width, display_segments.len(), &weights, dpi);
    if rects.is_empty() {
        let mut text = wide(if model.enabled {
            crate::tr!("磁盘没有可显示的空间")
        } else {
            crate::tr!("请选择磁盘")
        });
        let mut text_rect = client;
        let _ = SetBkMode(dc, TRANSPARENT);
        let _ = SetTextColor(dc, palette.text_secondary);
        let old_font = SelectObject(dc, model.font);
        let _ = DrawTextW(
            dc,
            &mut text,
            &mut text_rect,
            DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
        );
        let _ = SelectObject(dc, old_font);
        if let Some((memory_dc, bitmap, old_bitmap)) = buffer {
            let _ = BitBlt(target_dc, 0, 0, width, height, memory_dc, 0, 0, SRCCOPY);
            let _ = SelectObject(memory_dc, old_bitmap);
            let _ = DeleteObject(bitmap);
            let _ = DeleteDC(memory_dc);
        }
        let _ = EndPaint(hwnd, &paint);
        return;
    }
    let outer = RECT {
        left: margin,
        top: margin,
        right: client.right - margin,
        bottom: client.bottom - margin,
    };
    let segment_fill = |segment: &PartitionMapSegment| {
        let selected = model.selected == Some(segment.target);
        let unallocated = matches!(segment.target, PartitionMapTarget::Unallocated { .. });
        if selected {
            palette.highlight_fill
        } else if unallocated {
            palette.edit
        } else if segment.special {
            palette.button_pressed
        } else {
            palette.button
        }
    };
    let base_fill = segment_fill(&display_segments[0]);
    let radius = scale(4, dpi);
    fill_round_rect_antialiased(dc, outer, radius, base_fill, palette.border, palette.window);
    let frame = scale(1, dpi).max(1);
    let clip = CreateRoundRectRgn(
        outer.left + frame,
        outer.top + frame,
        outer.right - frame + 1,
        outer.bottom - frame + 1,
        (radius - frame).max(1) * 2,
        (radius - frame).max(1) * 2,
    );
    if !clip.is_invalid() {
        let _ = SelectClipRgn(dc, clip);
        for (segment, (left, right)) in display_segments.iter().zip(rects.iter().copied()) {
            let rect = RECT {
                left: margin + left,
                top: outer.top + frame,
                right: margin + right,
                bottom: outer.bottom - frame,
            };
            let brush = CreateSolidBrush(segment_fill(segment));
            let _ = FillRect(dc, &rect, brush);
            let _ = DeleteObject(brush);
        }
        for (_, right) in rects.iter().take(rects.len().saturating_sub(1)) {
            let divider = RECT {
                left: margin + right - frame / 2,
                top: outer.top + frame,
                right: margin + right - frame / 2 + frame,
                bottom: outer.bottom - frame,
            };
            let brush = CreateSolidBrush(palette.border);
            let _ = FillRect(dc, &divider, brush);
            let _ = DeleteObject(brush);
        }
        let _ = SelectClipRgn(dc, None);
        let _ = DeleteObject(clip);
    }
    for (left_index, divider) in draggable_partition_map_dividers(&display_segments, &rects, margin)
    {
        let (handle_width, line_width) = partition_handle_widths(dpi);
        let handle_height = scale(24, dpi).min((outer.bottom - outer.top - scale(8, dpi)).max(8));
        let handle_center = divider.clamp(
            outer.left + handle_width / 2 + frame,
            outer.right - (handle_width + 1) / 2 - frame,
        );
        let handle = RECT {
            left: handle_center - handle_width / 2,
            top: outer.top + (outer.bottom - outer.top - handle_height) / 2,
            right: handle_center + (handle_width + 1) / 2,
            bottom: outer.top + (outer.bottom - outer.top + handle_height) / 2,
        };
        let right_index = left_index + 1;
        for (clip_left, clip_right, background) in [
            (
                handle.left,
                divider.min(handle.right),
                segment_fill(&display_segments[left_index]),
            ),
            (
                divider.max(handle.left),
                handle.right,
                segment_fill(&display_segments[right_index]),
            ),
        ] {
            if clip_right <= clip_left {
                continue;
            }
            let saved = SaveDC(dc);
            if saved == 0 {
                continue;
            }
            let _ = IntersectClipRect(dc, clip_left, handle.top, clip_right, handle.bottom);
            fill_round_rect_antialiased(
                dc,
                handle,
                scale(3, dpi),
                palette.button_hot,
                palette.border,
                background,
            );
            let _ = RestoreDC(dc, saved);
        }
        let line_left = handle.left + (handle.right - handle.left - line_width) / 2;
        let line = RECT {
            left: line_left,
            top: handle.top + scale(5, dpi),
            right: line_left + line_width,
            bottom: handle.bottom - scale(5, dpi),
        };
        let brush = CreateSolidBrush(palette.text_secondary);
        let _ = FillRect(dc, &line, brush);
        let _ = DeleteObject(brush);
    }
    for (segment, (left, right)) in display_segments.iter().zip(rects.iter().copied()) {
        let selected = model.selected == Some(segment.target);
        let rect = RECT {
            left: margin + left,
            top: margin,
            right: margin + right,
            bottom: client.bottom - margin,
        };
        let text_color = if selected {
            if palette.dark {
                COLORREF(0)
            } else {
                COLORREF(0x00ff_ffff)
            }
        } else if segment.protected {
            palette.text_disabled
        } else {
            palette.text
        };
        let _ = SetBkMode(dc, TRANSPARENT);
        let _ = SetTextColor(dc, text_color);
        let old_font = SelectObject(dc, model.font);
        let mut label = wide(&segment.label);
        let mut label_rect = rect;
        label_rect.left += scale(5, dpi);
        label_rect.right -= scale(5, dpi);
        label_rect.bottom = rect.top + (rect.bottom - rect.top) / 2 + scale(7, dpi);
        let _ = DrawTextW(
            dc,
            &mut label,
            &mut label_rect,
            DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        let mut size = wide(&segment.size);
        let mut size_rect = rect;
        size_rect.left += scale(5, dpi);
        size_rect.right -= scale(5, dpi);
        size_rect.top = rect.top + (rect.bottom - rect.top) / 2 - scale(5, dpi);
        let _ = DrawTextW(
            dc,
            &mut size,
            &mut size_rect,
            DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        let _ = SelectObject(dc, old_font);
    }
    if let Some((memory_dc, bitmap, old_bitmap)) = buffer {
        let _ = BitBlt(target_dc, 0, 0, width, height, memory_dc, 0, 0, SRCCOPY);
        let _ = SelectObject(memory_dc, old_bitmap);
        let _ = DeleteObject(bitmap);
        let _ = DeleteDC(memory_dc);
    }
    let _ = EndPaint(hwnd, &paint);
}

fn partition_handle_widths(dpi: u32) -> (i32, i32) {
    let line_width = scale(1, dpi).max(1);
    let mut handle_width = scale(8, dpi).max(6);
    if (handle_width - line_width) % 2 != 0 {
        handle_width += 1;
    }
    (handle_width, line_width)
}

fn draggable_partition_map_dividers(
    segments: &[PartitionMapSegment],
    rects: &[(i32, i32)],
    margin: i32,
) -> Vec<(usize, i32)> {
    segments
        .iter()
        .enumerate()
        .filter_map(|(index, left)| {
            let right = segments.get(index + 1);
            if !partition_segment_can_resize(left, right) {
                return None;
            }
            rects.get(index).map(|(_, right)| (index, margin + *right))
        })
        .collect()
}

fn partition_segment_can_resize(
    segment: &PartitionMapSegment,
    right: Option<&PartitionMapSegment>,
) -> bool {
    if !matches!(segment.target, PartitionMapTarget::Existing(_))
        || segment.protected
        || segment.special
        || segment.drive_letter.is_none()
    {
        return false;
    }
    let can_shrink = segment.minimum_bytes < segment.weight;
    let can_expand = right.is_some_and(|right| {
        (matches!(right.target, PartitionMapTarget::Unallocated { .. }) && right.weight > 0)
            || partition_segment_can_be_moved(right)
    });
    can_shrink || can_expand
}

fn partition_segment_can_be_moved(segment: &PartitionMapSegment) -> bool {
    matches!(segment.target, PartitionMapTarget::Existing(_))
        && !segment.protected
        && !segment.special
        && segment.drive_letter.is_some()
}

unsafe fn begin_partition_map_drag(
    hwnd: HWND,
    model: &PartitionMapModel,
    point: POINT,
) -> Option<PartitionMapDrag> {
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    let dpi = GetDpiForWindow(hwnd).max(96);
    let margin = scale(2, dpi);
    let weights = model
        .segments
        .iter()
        .map(|segment| segment.weight)
        .collect::<Vec<_>>();
    let rects = partition_map_rects(
        (client.right - client.left - margin * 2).max(0),
        model.segments.len(),
        &weights,
        dpi,
    );
    let hit_radius = scale(7, dpi);
    for (left_segment, divider) in draggable_partition_map_dividers(&model.segments, &rects, margin)
    {
        if (point.x - divider).abs() > hit_radius {
            continue;
        }
        let left = &model.segments[left_segment];
        let PartitionMapTarget::Existing(partition_index) = left.target else {
            continue;
        };
        let right = model.segments.get(left_segment + 1);
        let right_is_unallocated = right
            .is_some_and(|right| matches!(right.target, PartitionMapTarget::Unallocated { .. }));
        let right_is_borrowed_partition = right.is_some_and(partition_segment_can_be_moved);
        let right_partition_index = right.and_then(|right| match right.target {
            PartitionMapTarget::Existing(index) if right_is_borrowed_partition => Some(index),
            _ => None,
        });
        let (scale_width, scale_bytes, maximum_bytes) = if right_is_unallocated {
            let right = right?;
            (
                rects[left_segment + 1].1 - rects[left_segment].0,
                left.weight.saturating_add(right.weight),
                left.weight.saturating_add(right.weight),
            )
        } else if right_is_borrowed_partition {
            let right = right?;
            let reclaimable = right.weight.saturating_sub(right.minimum_bytes);
            (
                (rects[left_segment + 1].1 - rects[left_segment].0).max(1),
                left.weight.saturating_add(right.weight),
                left.weight.saturating_add(reclaimable),
            )
        } else {
            (
                rects[left_segment].1 - rects[left_segment].0,
                left.weight,
                left.weight,
            )
        };
        return Some(PartitionMapDrag {
            partition_index,
            left_segment,
            start_x: divider,
            current_x: divider,
            scale_width,
            scale_bytes,
            original_bytes: left.weight,
            minimum_bytes: left.minimum_bytes,
            maximum_bytes,
            right_is_unallocated,
            right_is_borrowed_partition,
            right_partition_index,
        });
    }
    None
}

unsafe fn hit_test_partition_map(
    hwnd: HWND,
    model: &PartitionMapModel,
    point: POINT,
) -> Option<PartitionMapTarget> {
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    let dpi = GetDpiForWindow(hwnd).max(96);
    let margin = scale(2, dpi);
    let weights = model
        .segments
        .iter()
        .map(|segment| segment.weight)
        .collect::<Vec<_>>();
    let rects = partition_map_rects(
        (client.right - client.left - margin * 2).max(0),
        model.segments.len(),
        &weights,
        dpi,
    );
    model
        .segments
        .iter()
        .zip(rects)
        .find(|(_, (left, right))| {
            point.x >= margin + *left
                && point.x < margin + *right
                && point.y >= margin
                && point.y < client.bottom - margin
        })
        .map(|(segment, _)| segment.target)
}

unsafe fn show_partition_context_menu(hwnd: HWND, model: &PartitionMapModel, mut point: POINT) {
    let Some(target) = model.context_target else {
        return;
    };
    let menu = match CreatePopupMenu() {
        Ok(menu) => menu,
        Err(_) => return,
    };
    let palette = Palette::system();
    let dpi = GetDpiForWindow(hwnd).max(96);
    let menu_background = CreateSolidBrush(palette.window);
    apply_partition_menu_background(menu, menu_background);
    let mut builder = PartitionMenuBuilder::new(hwnd, model.font, palette, dpi, menu_background);
    match target {
        PartitionMapTarget::Existing(index) => {
            let Some(segment) = model
                .segments
                .iter()
                .find(|segment| segment.target == PartitionMapTarget::Existing(index))
            else {
                let _ = DestroyMenu(menu);
                let _ = DeleteObject(menu_background);
                return;
            };
            let can_resize = model
                .segments
                .iter()
                .position(|candidate| candidate.target == segment.target)
                .is_some_and(|position| {
                    partition_segment_can_resize(segment, model.segments.get(position + 1))
                });
            builder.item(
                menu,
                ID_MAP_RESIZE,
                &crate::tr!("扩大/缩小分区"),
                !can_resize
                    || segment.protected
                    || segment.special
                    || segment.drive_letter.is_none(),
            );
            builder.item(
                menu,
                ID_MAP_FORMAT_NTFS,
                &crate::tr!("格式化..."),
                segment.protected || segment.special || segment.drive_letter.is_none(),
            );
            builder.separator(menu);
            if segment.drive_letter.is_some() {
                builder.item(
                    menu,
                    ID_MAP_REMOVE_LETTER,
                    &crate::tr!("移除盘符"),
                    segment.protected,
                );
            } else {
                builder.letter_submenu(
                    menu,
                    &crate::tr!("分配盘符"),
                    ID_MAP_ASSIGN_LETTER_FIRST,
                    &model.available_letters,
                );
            }
            if model.style == PartitionStyle::MBR && !segment.special {
                let active_label = if segment.active {
                    crate::tr!("取消活动分区")
                } else {
                    crate::tr!("设为活动分区")
                };
                builder.item(
                    menu,
                    if segment.active {
                        ID_MAP_CLEAR_ACTIVE
                    } else {
                        ID_MAP_SET_ACTIVE
                    },
                    &active_label,
                    segment.protected,
                );
            }
            builder.separator(menu);
            builder.item(
                menu,
                ID_MAP_DELETE,
                &crate::tr!("删除分区"),
                segment.protected,
            );
        }
        PartitionMapTarget::Unallocated { size_bytes, .. } => {
            let label = if model.initialized {
                crate::tr!("创建 NTFS 分区并分配盘符")
            } else if model.style == PartitionStyle::GPT {
                crate::tr!("初始化为 UEFI (GPT) 并创建 NTFS")
            } else {
                crate::tr!("初始化为 BIOS (MBR) 并创建 NTFS")
            };
            if size_bytes >= 1024 * 1024 {
                builder.letter_submenu(
                    menu,
                    &label,
                    ID_MAP_CREATE_LETTER_FIRST,
                    &model.available_letters,
                );
            }
        }
    }
    let _ = ClientToScreen(hwnd, &mut point);
    let _menu_window_rounder = PartitionMenuWindowRounder::start(GetCurrentThreadId());
    let command = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON,
        point.x,
        point.y,
        0,
        hwnd,
        None,
    );
    if command.0 != 0 {
        if let Ok(parent) = GetParent(hwnd) {
            let _ = SendMessageW(
                parent,
                WM_COMMAND,
                WPARAM(command.0 as usize),
                LPARAM(hwnd.0 as isize),
            );
        }
    }
    let _ = DestroyMenu(menu);
    let _ = DeleteObject(menu_background);
}

unsafe fn apply_partition_menu_background(
    menu: windows::Win32::UI::WindowsAndMessaging::HMENU,
    brush: windows::Win32::Graphics::Gdi::HBRUSH,
) {
    let info = MENUINFO {
        cbSize: std::mem::size_of::<MENUINFO>() as u32,
        fMask: MIM_BACKGROUND,
        hbrBack: brush,
        ..Default::default()
    };
    let _ = SetMenuInfo(menu, &info);
}

fn point_from_lparam(lparam: LPARAM) -> POINT {
    POINT {
        x: (lparam.0 as u16 as i16) as i32,
        y: ((lparam.0 >> 16) as u16 as i16) as i32,
    }
}

unsafe fn create_controls(parent: HWND, command_parent: HWND) -> windows::core::Result<Controls> {
    let label = |text: &str| child(parent, w!("STATIC"), text, 0, 0);
    let button = |text: &str, id| {
        child(
            parent,
            w!("BUTTON"),
            text,
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            id,
        )
    };
    let partitions = child(
        parent,
        w!("SysListView32"),
        "",
        (LVS_REPORT | LVS_SHOWSELALWAYS | WS_BORDER.0 | WS_TABSTOP.0) as i32,
        ID_PARTITIONS,
    )?;
    let _ = SendMessageW(
        partitions,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        WPARAM(0),
        LPARAM((LVS_EX_DOUBLEBUFFER | LVS_EX_FULLROWSELECT | LVS_EX_INFOTIP) as isize),
    );
    insert_columns(partitions);
    Ok(Controls {
        disk_label: label(&crate::tr!("选择磁盘:"))?,
        disk: child(
            parent,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            ID_DISK,
        )?,
        style_label: label(&crate::tr!("分区表类型:"))?,
        style_mbr: child(
            parent,
            w!("BUTTON"),
            &crate::tr!("BIOS (MBR)"),
            BS_AUTORADIOBUTTON | WS_TABSTOP.0 as i32,
            ID_STYLE_MBR,
        )?,
        style_gpt: child(
            parent,
            w!("BUTTON"),
            &crate::tr!("UEFI (GPT)"),
            BS_AUTORADIOBUTTON | WS_TABSTOP.0 as i32,
            ID_STYLE_GPT,
        )?,
        recommendation: label("")?,
        partition_map: child(
            parent,
            w!("STATIC"),
            "",
            0x0100, // SS_NOTIFY
            ID_PARTITION_MAP,
        )?,
        add_partition: button(&crate::tr!("添加分区"), ID_ADD_PARTITION)?,
        add_esp: button(&crate::tr!("创建 ESP 分区 (500 MB)"), ID_ADD_ESP)?,
        delete: button(&crate::tr!("删除"), ID_DELETE)?,
        partitions,
        size_label: label(&crate::tr!("新大小 (GB):"))?,
        size: child(
            parent,
            w!("EDIT"),
            "",
            ES_AUTOHSCROLL | WS_TABSTOP.0 as i32,
            ID_SIZE,
        )?,
        apply_size: button(&crate::tr!("调整大小"), ID_APPLY_SIZE)?,
        apply_pending: child(
            command_parent,
            w!("BUTTON"),
            &crate::tr!("应用修改"),
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            ID_APPLY_PENDING,
        )?,
        warning: label(&crate::tr!("提示: 一键分区会清除整个磁盘"))?,
        status: label("")?,
    })
}

unsafe fn insert_columns(list: HWND) {
    for (index, title) in ["状态", "分区卷", "大小", "已用/可用", "卷标", "文件系统"]
        .into_iter()
        .enumerate()
    {
        let mut text = wide(crate::tr!(title));
        let mut column = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH,
            cx: 100,
            pszText: PWSTR(text.as_mut_ptr()),
            ..Default::default()
        };
        let _ = SendMessageW(
            list,
            LVM_INSERTCOLUMNW,
            WPARAM(index),
            LPARAM((&mut column as *mut LVCOLUMNW) as isize),
        );
    }
}

unsafe fn refill_disks(combo: HWND, disks: &[PhysicalDisk], selected: Option<u32>) {
    let _ = SendMessageW(combo, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
    for disk in disks {
        add_combo_item(combo, &disk.display_name());
    }
    let index = selected
        .and_then(|number| disks.iter().position(|disk| disk.disk_number == number))
        .map_or(NO_COMBO_SELECTION, |index| index);
    let _ = SendMessageW(combo, CB_SETCURSEL, WPARAM(index), LPARAM(0));
}

unsafe fn refill_partitions(list: HWND, state: &QuickPartitionDialogState) {
    let _ = SendMessageW(list, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));
    let mut row = 0;
    if let Some(disk) = state.selected_disk() {
        for partition in &disk.partitions {
            insert_row(list, row, existing_columns(partition));
            row += 1;
        }
    }
    for layout in &state.planned {
        insert_row(list, row, planned_columns(layout));
        row += 1;
    }
    if let Some(selected) = state.selected_row {
        let existing_count = state
            .selected_disk()
            .map_or(0, |disk| disk.partitions.len());
        let index = match selected {
            EditorRow::Existing(index) => index,
            EditorRow::Planned(index) => existing_count + index,
        };
        let mut item = LVITEMW {
            stateMask: windows::Win32::UI::Controls::LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED),
            state: windows::Win32::UI::Controls::LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED),
            ..Default::default()
        };
        let _ = SendMessageW(
            list,
            LVM_SETITEMSTATE,
            WPARAM(index),
            LPARAM((&mut item as *mut LVITEMW) as isize),
        );
    }
}

unsafe fn insert_row(list: HWND, row: i32, columns: [String; 6]) {
    for (column, value) in columns.into_iter().enumerate() {
        let mut value = wide(value);
        let mut item = LVITEMW {
            mask: LVIF_TEXT,
            iItem: row,
            iSubItem: column as i32,
            pszText: PWSTR(value.as_mut_ptr()),
            ..Default::default()
        };
        let message = if column == 0 {
            LVM_INSERTITEMW
        } else {
            LVM_SETITEMTEXTW_LOCAL
        };
        let _ = SendMessageW(
            list,
            message,
            WPARAM(0),
            LPARAM((&mut item as *mut LVITEMW) as isize),
        );
    }
}

fn existing_columns(partition: &DiskPartitionInfo) -> [String; 6] {
    [
        crate::tr!("已有"),
        partition_name(
            partition.drive_letter,
            partition.is_esp,
            partition.is_msr,
            partition.is_recovery,
        ),
        format!("{:.1} GB", partition.size_gb()),
        format!("{:.1} / {:.1} GB", partition.used_gb(), partition.free_gb()),
        display_value(&partition.label),
        display_value(&partition.file_system),
    ]
}

fn planned_columns(layout: &PartitionLayout) -> [String; 6] {
    [
        crate::tr!("新建"),
        partition_name(layout.drive_letter, layout.is_esp, false, false),
        format!("{:.1} GB", layout.size_gb),
        format!("0.0 / {:.1} GB", layout.size_gb),
        display_value(&layout.label),
        display_value(&layout.file_system),
    ]
}

fn partition_name(letter: Option<char>, is_esp: bool, is_msr: bool, is_recovery: bool) -> String {
    if is_esp {
        "ESP".into()
    } else if is_msr {
        "MSR".into()
    } else if is_recovery {
        crate::tr!("恢复分区")
    } else {
        letter
            .map(|letter| format!("{letter}:"))
            .unwrap_or_else(|| crate::tr!("未分配盘符"))
    }
}

fn display_value(value: &str) -> String {
    if value.trim().is_empty() {
        "—".into()
    } else {
        value.into()
    }
}

fn partition_columns(width: i32, dpi: u32) -> [i32; 6] {
    let usable = (width - scale(4, dpi)).max(0);
    let status = usable * 11 / 100;
    let drive = usable * 15 / 100;
    let size = usable * 14 / 100;
    let usage = usable * 21 / 100;
    let fs = usable * 14 / 100;
    let label = usable - status - drive - size - usage - fs;
    [status, drive, size, usage, label, fs]
}

unsafe fn set_radio(control: HWND, checked: bool) {
    let _ = SendMessageW(
        control,
        BM_SETCHECK,
        WPARAM(usize::from(checked)),
        LPARAM(0),
    );
}

unsafe fn add_combo_item(combo: HWND, value: &str) {
    let value = wide(value);
    let _ = SendMessageW(
        combo,
        CB_ADDSTRING,
        WPARAM(0),
        LPARAM(value.as_ptr() as isize),
    );
}

unsafe fn window_text(control: HWND) -> String {
    let length = GetWindowTextLengthW(control).max(0) as usize;
    let mut buffer = vec![0_u16; length + 1];
    let read = GetWindowTextW(control, &mut buffer).max(0) as usize;
    String::from_utf16_lossy(&buffer[..read])
}

unsafe fn set_text(control: HWND, text: &str) {
    let text = wide(text);
    let _ = SetWindowTextW(control, PCWSTR(text.as_ptr()));
}

unsafe fn move_control(control: HWND, x: i32, y: i32, width: i32, height: i32) {
    let _ = MoveWindow(control, x, y, width.max(0), height.max(0), true);
}

fn scale(value: i32, dpi: u32) -> i32 {
    ((value as i64 * dpi.max(1) as i64 + 48) / 96) as i32
}

fn logical_height(value: i32, dpi: u32) -> i32 {
    ((i64::from(value.max(0)) * 96 + i64::from(dpi.max(1)) / 2) / i64::from(dpi.max(1))) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partition_style_radios_keep_native_general_theme_role() {
        assert_eq!(RADIO_CONTROL_KIND, NativeControlKind::General);
    }

    #[test]
    fn columns_remain_positive_and_use_the_full_width_at_supported_dpi() {
        for dpi in [96, 144, 192] {
            for logical_width in [420, 720] {
                let width = scale(logical_width, dpi);
                let columns = partition_columns(width, dpi);
                assert!(columns.into_iter().all(|column| column > 0));
                assert_eq!(columns.into_iter().sum::<i32>(), width - scale(4, dpi));
            }
        }
    }

    #[test]
    fn rows_distinguish_existing_and_planned_without_editing_existing_metadata() {
        let existing = DiskPartitionInfo {
            partition_number: 1,
            size_bytes: 10 * 1024 * 1024 * 1024,
            offset_bytes: 0,
            drive_letter: Some('D'),
            label: "Archive".into(),
            file_system: "NTFS".into(),
            is_esp: false,
            is_msr: false,
            is_recovery: false,
            partition_type: String::new(),
            used_bytes: 2 * 1024 * 1024 * 1024,
            free_bytes: 8 * 1024 * 1024 * 1024,
            is_active: false,
        };
        assert_eq!(existing_columns(&existing)[0], crate::tr!("已有"));
        assert_eq!(existing_columns(&existing)[1], "D:");
        let planned = PartitionLayout {
            size_gb: 0.5,
            drive_letter: None,
            label: "EFI".into(),
            is_esp: true,
            file_system: "FAT32".into(),
        };
        assert_eq!(planned_columns(&planned)[0], crate::tr!("新建"));
        assert_eq!(planned_columns(&planned)[1], "ESP");
    }

    #[test]
    fn partition_map_rectangles_are_ordered_non_overlapping_and_fill_the_width() {
        for dpi in [96, 144, 192] {
            for weights in [
                vec![1],
                vec![1, 1],
                vec![1, 10, 100],
                vec![1, 1, 1, 1, 1, 1, 1, 1],
            ] {
                let width = scale(720, dpi);
                let rects = partition_map_rects(width, weights.len(), &weights, dpi);
                assert_eq!(rects.len(), weights.len());
                assert!(rects.iter().all(|(left, right)| right > left));
                assert!(rects.windows(2).all(|pair| pair[0].1 == pair[1].0));
                assert_eq!(rects.last().unwrap().1, width);
            }
        }
    }

    #[test]
    fn map_hit_target_validation_rejects_stale_or_overlapping_free_space() {
        let disk = PhysicalDisk {
            disk_number: 1,
            model: "test".into(),
            size_bytes: 100 * 1024 * 1024,
            partition_style: PartitionStyle::GPT,
            is_initialized: true,
            unallocated_bytes: 70 * 1024 * 1024,
            partitions: vec![DiskPartitionInfo {
                partition_number: 1,
                size_bytes: 20 * 1024 * 1024,
                offset_bytes: 10 * 1024 * 1024,
                drive_letter: Some('D'),
                label: String::new(),
                file_system: "NTFS".into(),
                is_esp: false,
                is_msr: false,
                is_recovery: false,
                partition_type: String::new(),
                used_bytes: 0,
                free_bytes: 20 * 1024 * 1024,
                is_active: false,
            }],
        };
        assert!(selected_disk_contains_target(
            Some(&disk),
            PartitionMapTarget::Existing(0)
        ));
        assert!(selected_disk_contains_target(
            Some(&disk),
            PartitionMapTarget::Unallocated {
                offset_bytes: 30 * 1024 * 1024,
                size_bytes: 70 * 1024 * 1024,
            }
        ));
        assert!(!selected_disk_contains_target(
            Some(&disk),
            PartitionMapTarget::Unallocated {
                offset_bytes: 20 * 1024 * 1024,
                size_bytes: 20 * 1024 * 1024,
            }
        ));
    }

    #[test]
    fn resize_handles_allow_shrink_without_preexisting_unallocated_space() {
        let segment = |target, protected| PartitionMapSegment {
            target,
            label: String::new(),
            size: String::new(),
            weight: 100,
            special: false,
            protected,
            drive_letter: Some('D'),
            active: false,
            minimum_bytes: 50,
        };
        let unallocated = PartitionMapSegment {
            target: PartitionMapTarget::Unallocated {
                offset_bytes: 100,
                size_bytes: 100,
            },
            label: String::new(),
            size: String::new(),
            weight: 100,
            special: false,
            protected: false,
            drive_letter: None,
            active: false,
            minimum_bytes: 0,
        };
        let mut model = PartitionMapModel::new(HFONT::default());
        model.segments = vec![
            segment(PartitionMapTarget::Existing(0), true),
            unallocated.clone(),
        ];
        let rects = vec![(0, 100), (100, 200)];
        assert!(draggable_partition_map_dividers(&model.segments, &rects, 2).is_empty());
        model.segments[0].protected = false;
        assert_eq!(
            draggable_partition_map_dividers(&model.segments, &rects, 2),
            vec![(0, 102)]
        );

        model.segments[1] = segment(PartitionMapTarget::Existing(1), false);
        assert_eq!(
            draggable_partition_map_dividers(&model.segments, &rects, 2),
            vec![(0, 102), (1, 202)]
        );

        model.segments[0].minimum_bytes = model.segments[0].weight;
        assert_eq!(
            draggable_partition_map_dividers(&model.segments, &rects, 2),
            vec![(0, 102), (1, 202)]
        );

        model.segments[0].minimum_bytes = 50;
        model.segments[1].minimum_bytes = model.segments[1].weight;
        assert_eq!(
            draggable_partition_map_dividers(&model.segments, &rects, 2),
            vec![(0, 102)]
        );
    }

    #[test]
    fn handle_inner_line_is_pixel_centered_at_supported_dpi() {
        for dpi in [96, 120, 144, 168, 192, 240, 288] {
            let (handle_width, line_width) = partition_handle_widths(dpi);
            assert!(handle_width > line_width);
            assert_eq!((handle_width - line_width) % 2, 0, "dpi={dpi}");
            let left_inset = (handle_width - line_width) / 2;
            let right_inset = handle_width - line_width - left_inset;
            assert_eq!(left_inset, right_inset, "dpi={dpi}");
        }
    }

    #[test]
    fn dragging_left_between_existing_partitions_transfers_space_to_right_partition() {
        let segment = |index, weight, minimum_bytes| PartitionMapSegment {
            target: PartitionMapTarget::Existing(index),
            label: format!("P{index}"),
            size: format_capacity(weight),
            weight,
            special: false,
            protected: false,
            drive_letter: Some((b'D' + index as u8) as char),
            active: false,
            minimum_bytes,
        };
        let mut model = PartitionMapModel::new(HFONT::default());
        model.segments = vec![segment(0, 100, 50), segment(1, 200, 100)];
        model.drag = Some(PartitionMapDrag {
            partition_index: 0,
            left_segment: 0,
            start_x: 100,
            current_x: 75,
            scale_width: 100,
            scale_bytes: 300,
            original_bytes: 100,
            minimum_bytes: 50,
            maximum_bytes: 200,
            right_is_unallocated: false,
            right_is_borrowed_partition: true,
            right_partition_index: Some(1),
        });

        let display = partition_map_display_segments(&model);
        assert_eq!(display.len(), 2);
        assert_eq!(display[0].weight, 50);
        assert_eq!(display[1].target, PartitionMapTarget::Existing(1));
        assert_eq!(display[1].weight, 250);
    }

    #[test]
    fn nonmovable_neighbor_prevents_growth_but_not_shrink() {
        let drag = PartitionMapDrag {
            partition_index: 0,
            left_segment: 0,
            start_x: 100,
            current_x: 160,
            scale_width: 100,
            scale_bytes: 100,
            original_bytes: 100,
            minimum_bytes: 50,
            maximum_bytes: 100,
            right_is_unallocated: false,
            right_is_borrowed_partition: false,
            right_partition_index: None,
        };
        assert_eq!(drag_target_bytes(drag), 100);
        assert_eq!(
            drag_target_bytes(PartitionMapDrag {
                current_x: 50,
                ..drag
            }),
            50
        );
    }

    #[test]
    fn movable_neighbor_contributes_reclaimable_space_to_right_drag() {
        let mut drag = PartitionMapDrag {
            partition_index: 0,
            left_segment: 0,
            start_x: 100,
            current_x: 150,
            scale_width: 300,
            scale_bytes: 300,
            original_bytes: 100,
            minimum_bytes: 50,
            maximum_bytes: 220,
            right_is_unallocated: false,
            right_is_borrowed_partition: true,
            right_partition_index: Some(1),
        };
        assert_eq!(drag_target_bytes(drag), 150);

        let segment = |index, weight, minimum_bytes| PartitionMapSegment {
            target: PartitionMapTarget::Existing(index),
            label: format!("P{index}"),
            size: format_capacity(weight),
            weight,
            special: false,
            protected: false,
            drive_letter: Some((b'D' + index as u8) as char),
            active: false,
            minimum_bytes,
        };
        let mut model = PartitionMapModel::new(HFONT::default());
        model.segments = vec![segment(0, 100, 50), segment(1, 200, 80)];
        drag.current_x = 220;
        model.drag = Some(drag);
        let display = partition_map_display_segments(&model);
        assert_eq!(display[0].weight, 220);
        assert_eq!(display[1].weight, 80);
    }

    #[test]
    fn expanding_right_then_transferring_to_left_keeps_the_resize_prerequisite() {
        const MIB: u64 = 1024 * 1024;
        let partition = |number, offset_mb, letter| DiskPartitionInfo {
            partition_number: number,
            size_bytes: 100 * MIB,
            offset_bytes: offset_mb * MIB,
            drive_letter: Some(letter),
            label: letter.to_string(),
            file_system: "NTFS".into(),
            is_esp: false,
            is_msr: false,
            is_recovery: false,
            partition_type: "basic".into(),
            used_bytes: 20 * MIB,
            free_bytes: 80 * MIB,
            is_active: false,
        };
        let disk = PhysicalDisk {
            disk_number: 3,
            model: "compound-plan".into(),
            size_bytes: 301 * MIB,
            partition_style: PartitionStyle::GPT,
            is_initialized: true,
            partitions: vec![partition(1, 1, 'D'), partition(2, 101, 'E')],
            unallocated_bytes: 100 * MIB,
        };
        let resize = ExistingPartitionResizeRequest {
            disk: DiskFingerprint::from(&disk),
            partition_number: 2,
            drive_letter: 'E',
            current_size_mb: 100,
            new_size_mb: 200,
            used_size_mb: 20,
            no_move_max_size_mb: 200,
            move_max_size_mb: 200,
        };
        let mut pending = vec![PendingPartitionOperation::Resize(resize.clone())];
        assert_eq!(
            staged_direct_partition_size_mb(&disk, &pending, 0),
            Some(100)
        );
        assert_eq!(
            staged_direct_partition_size_mb(&disk, &pending, 1),
            Some(200)
        );

        let mut state = QuickPartitionDialogState::new(PartitionStyle::GPT, vec![], 'C');
        state.apply_inventory(Ok(vec![disk]));
        let transfer = state
            .adjacent_transfer_request_from_current_sizes_mb(0, 1, 100, 200, 150)
            .unwrap();
        pending.retain(|operation| retain_operation_before_transfer(operation, &transfer, 1, 2));
        pending.push(PendingPartitionOperation::Transfer(transfer));

        assert_eq!(pending.len(), 2);
        assert!(matches!(
            &pending[0],
            PendingPartitionOperation::Resize(request) if request == &resize
        ));
        let PendingPartitionOperation::Transfer(transfer) = &pending[1] else {
            unreachable!()
        };
        assert_eq!(transfer.left_new_size_mb, 150);
        assert_eq!(transfer.right_new_size_mb, 150);
    }
}
