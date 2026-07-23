use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect,
    GetMonitorInfoW, InvalidateRect, LineTo, MonitorFromWindow, MoveToEx, RedrawWindow,
    SelectObject, SetBkColor, SetBkMode, SetTextColor, DT_END_ELLIPSIS, DT_NOPREFIX, DT_SINGLELINE,
    DT_VCENTER, HBRUSH, HDC, HFONT, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, PEN_STYLE,
    RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RDW_UPDATENOW, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, SetWindowTheme, DRAWITEMSTRUCT, HDF_OWNERDRAW, HDITEMW, HDI_TEXT,
    ICC_LISTVIEW_CLASSES, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX, LVCF_FMT, LVCF_TEXT,
    LVCF_WIDTH, LVCOLUMNW, LVCOLUMNW_FORMAT, LVIF_STATE, LVIF_TEXT, LVIS_SELECTED, LVITEMW,
    LVM_DELETEALLITEMS, LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETEXTENDEDLISTVIEWSTYLE,
    LVN_ITEMCHANGED, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_REPORT, LVS_SHOWSELALWAYS,
    NMHDR, NMLISTVIEW, ODT_HEADER,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, IsWindowEnabled};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClassNameW, GetClientRect, GetMessageW,
    GetSystemMetrics, GetWindowLongPtrW, GetWindowTextLengthW, IsWindowVisible, KillTimer,
    LoadCursorW, LoadImageW, MoveWindow, PostMessageW, PostQuitMessage, RegisterClassExW,
    SendMessageW, SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, TranslateMessage, BN_CLICKED, BS_AUTOCHECKBOX, BS_OWNERDRAW, CBN_SELCHANGE,
    CBS_DROPDOWNLIST, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, EN_CHANGE,
    EN_KILLFOCUS, ES_AUTOHSCROLL, GWLP_USERDATA, GWL_EXSTYLE, HICON, HMENU, ICON_BIG, ICON_SMALL,
    IDC_ARROW, IMAGE_ICON, LBN_SELCHANGE, LR_SHARED, LWA_ALPHA, MINMAXINFO, MSG, SM_CXICON,
    SM_CXSCREEN, SM_CXSMICON, SM_CYICON, SM_CYSCREEN, SM_CYSMICON, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_SHOW, SW_SHOWNORMAL,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CANCELMODE, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORBTN,
    WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DEVICECHANGE,
    WM_DPICHANGED, WM_DRAWITEM, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_HSCROLL, WM_MOUSEWHEEL,
    WM_NCCREATE, WM_NOTIFY, WM_PAINT, WM_SETFONT, WM_SETICON, WM_SETTINGCHANGE, WM_SIZE,
    WM_SYSCOLORCHANGE, WM_THEMECHANGED, WM_TIMER, WM_VSCROLL, WNDCLASSEXW, WS_CHILD,
    WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_CONTROLPARENT, WS_EX_LAYERED, WS_OVERLAPPEDWINDOW,
    WS_TABSTOP, WS_VISIBLE,
};

use super::controls::{center_single_line_edit_in_row, child, draw_inno_button, wide, ButtonRole};
use super::dialog::{DialogButtons, DialogResult, DialogShell, DialogSpec};
use super::driver_transfer_dialog::NativeDriverTransferDialog;
use super::layout::{centered_control_y_ceil, measure_text, measured_button_width, LayoutMetrics};
use super::pages::advanced::{
    AdvancedBrowseTarget, AdvancedPage, AdvancedPageContext, AdvancedPageIntent,
};
use super::pages::backup::{
    localized_backup_defaults, BackupPage, BackupPageState, BackupPartitionRow,
};
use super::pages::download::{
    DownloadIntent, DownloadLabels, DownloadPage, DownloadTab, PageRect, ID_RESOURCE_LIST,
};
use super::pages::easy_mode::{EasyModeCommand, EasyModeLabels, EasyModePage};
use super::pages::info::{
    hardware_info_rows, AboutLabels, AboutLink, AboutPage, HardwareInfoPage, HardwareInfoRow,
    HardwareLabels, InfoIntent,
};
use super::pages::progress::{
    DownloadCompletionAction, LongTaskProgress, ProgressCompletion, ProgressIntent, ProgressPage,
    ProgressStatus, ProgressValue, ID_CANCEL_OPERATION, ID_PROGRESS_PRIMARY, ID_PROGRESS_SECONDARY,
};
use super::pages::tools::{ToolIntent, ToolLabels, ToolsPage};
use super::redraw;
use super::theme::{self, Brushes};
use super::tool_dialogs::{NativeToolDialog, ToolDialogIntent, ToolDialogKind};
use super::tool_dialogs_mutating::{
    MutatingDialogIntent, MutatingToolKind, MutatingToolState, NativeMutatingToolDialog,
};
use super::tools::appx::NativeAppxDialog;
use super::tools::batch_format::{
    BatchFormatDialogIntent, BatchFormatVolume, NativeBatchFormatDialog,
};
use super::tools::bitlocker_manage::{BitLockerManageDialogIntent, NativeBitLockerManageDialog};
use super::tools::boot_repair::{BootRepairDialogIntent, NativeBootRepairDialog};
use super::tools::expand_c::{ExpandCDialogIntent, ExpandCRequest, NativeExpandCDialog};
use super::tools::hardware_inspector::{HardwareInspectorIntent, NativeHardwareInspectorDialog};
use super::tools::network_reset::{NativeNetworkResetDialog, NetworkResetDialogIntent};
use super::tools::nvidia_removal::{
    NativeNvidiaRemovalDialog, NvidiaRemovalDialogIntent, NvidiaRemovalTargetOption,
};
use super::tools::partition_copy::{
    NativePartitionCopyDialog, PartitionCopyDialogIntent, PartitionCopyInventoryRow,
    PartitionCopyResumeState,
};
use super::tools::password_reset::{
    NativePasswordResetDialog, PasswordResetDialogIntent, PasswordResetTargetOption,
};
use super::tools::quick_partition::{NativeQuickPartitionDialog, QuickPartitionDialogIntent};
use super::tools::storage_driver::{NativeStorageDriverDialog, StorageDriverDialogIntent};
use super::tools::time_sync::{NativeTimeSyncDialog, TimeSyncDialogIntent};
use crate::core::native_backup_controller::{plan_backup_launch, BackupLaunchIntent};
use crate::core::native_backup_executor::{execute_backup, BackupExecution, BackupWorkerMessage};
use crate::core::native_bitlocker_gate::{
    execute_unlock, plan_backup_locked_volumes, plan_install_locked_volumes, validate_credential,
    BitLockerCredential as GateCredential, BitLockerVolumeSnapshot,
};
use crate::core::native_download_controller::{
    CatalogueState, ControllerIntent, DownloadAction, NativeDownloadController, ResourceCategory,
    SoftwareArchitecture,
};
use crate::core::native_download_executor::{
    DownloadFailureStage, DownloadWorker, DownloadWorkerCommand, DownloadWorkerError,
    DownloadWorkerMessage, NativeDownloadExecutor,
};
use crate::core::native_easy_mode_controller::{EasyModeAction, NativeEasyModeController};
use crate::core::native_expand_c_executor::{
    start_expand_c_handoff, ExpandCHandoffRequest, ExpandCWorkerMessage,
};
use crate::core::native_install_backend::ProductionInstallBackend;
use crate::core::native_install_controller::{
    InstallTarget, NativeInstallState, SelectedImageMetadata,
};
use crate::core::native_install_executor::{
    BitLockerRequirement, InstallExecutionContext, InstallExecutionEvent, NativeInstallExecutor,
    StableTargetIdentity,
};
use crate::core::native_tool_backend::{
    NativeToolBackend, NativeToolBackendRequest, NativeToolBackendResult,
};
use crate::core::native_tool_executor::{
    plan_execution, NativeToolExecutor, ReadOnlyToolRequest, ReadOnlyToolResult,
    ToolExecutionEvent, ToolExecutionPlan, ToolExecutionRequest,
};
use crate::download::config::{ConfigManager, OnlinePE, PeCache};
use crate::PreloadedConfig;

const CLASS_NAME: PCWSTR = w!("LetRecovery.Native.MainWindow");
const SS_CENTER_STYLE: i32 = 0x0000_0001;

fn catalogue_status_message(state: &CatalogueState) -> String {
    match state {
        CatalogueState::NotLoaded => String::new(),
        CatalogueState::Loading => crate::tr!("正在刷新在线资源目录..."),
        CatalogueState::Ready => crate::tr!("在线资源目录已刷新。"),
        CatalogueState::Failed(message) => message.clone(),
    }
}

// Keeps the longest English navigation caption readable at 100-200% DPI without leaving an
// oversized empty rail beside the centred button captions.
const NAV_WIDTH: i32 = 168;
const HEADER_HEIGHT: i32 = 66;
const COMMAND_HEIGHT: i32 = 56;
const WM_HARDWARE_INFO_READY: u32 = 0x8001;
const WM_IMAGE_INFO_READY: u32 = 0x8002;
const WM_PCA_FIRMWARE_READY: u32 = 0x8003;
const WM_PCA_TARGET_READY: u32 = 0x8004;
const WM_TOOL_WORKER_READY: u32 = 0x8005;
const WM_PARTITIONS_READY: u32 = 0x8006;
const WM_INSTALL_PARTITION_SELECTION_CHANGED: u32 = 0x8007;
const BACKUP_TIMER_ID: usize = 1;
const DOWNLOAD_TIMER_ID: usize = 2;
const INSTALL_TIMER_ID: usize = 3;
const TOOL_DIALOG_TIMER_ID: usize = 4;
const CATALOGUE_TIMER_ID: usize = 5;
const HARDWARE_COPY_TIMER_ID: usize = 6;
const INSTALL_VOLUME_LAYOUT_TIMER_ID: usize = 7;
const PARTITION_REFRESH_TIMER_ID: usize = 8;
const INSTALL_VOLUME_LAYOUT_TICK_MS: u32 = 40;
const INSTALL_VOLUME_LAYOUT_FRAMES: u8 = 3;
const PARTITION_REFRESH_DEBOUNCE_MS: u32 = 350;

const DBT_DEVNODES_CHANGED: usize = 0x0007;
const DBT_CONFIGCHANGED: usize = 0x0018;
const DBT_DEVICEARRIVAL: usize = 0x8000;
const DBT_DEVICEREMOVECOMPLETE: usize = 0x8004;

fn device_change_requests_partition_refresh(event: usize) -> bool {
    matches!(
        event,
        DBT_DEVNODES_CHANGED | DBT_CONFIGCHANGED | DBT_DEVICEARRIVAL | DBT_DEVICEREMOVECOMPLETE
    )
}

const fn list_view_selection_state_changed(changed: u32, old_state: u32, new_state: u32) -> bool {
    changed & LVIF_STATE.0 != 0 && (old_state ^ new_state) & LVIS_SELECTED.0 != 0
}

const fn unattended_checked_for_source_preference(
    configured_preference: bool,
    source_has_unattend: bool,
) -> bool {
    configured_preference && !source_has_unattend
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PartitionSelectionKey {
    letter: String,
    disk_number: Option<u32>,
    partition_number: Option<u32>,
    total_size_mb: u64,
}

impl From<&crate::core::disk::Partition> for PartitionSelectionKey {
    fn from(partition: &crate::core::disk::Partition) -> Self {
        Self {
            letter: partition.letter.clone(),
            disk_number: partition.disk_number,
            partition_number: partition.partition_number,
            total_size_mb: partition.total_size_mb,
        }
    }
}

impl PartitionSelectionKey {
    fn matches(&self, partition: &crate::core::disk::Partition) -> bool {
        match (
            self.disk_number,
            self.partition_number,
            partition.disk_number,
            partition.partition_number,
        ) {
            (Some(expected_disk), Some(expected_partition), Some(disk), Some(partition_number)) => {
                expected_disk == disk && expected_partition == partition_number
            }
            _ => {
                self.letter.eq_ignore_ascii_case(&partition.letter)
                    && self.total_size_mb == partition.total_size_mb
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct HardwareCopyFeedback {
    active: bool,
}

impl HardwareCopyFeedback {
    fn start(&mut self) {
        self.active = true;
    }

    fn expire(&mut self) {
        self.active = false;
    }

    const fn caption_key(self) -> &'static str {
        if self.active {
            "已复制"
        } else {
            "复制信息"
        }
    }
}

enum InstallWorkerMessage {
    Event(InstallExecutionEvent),
    Cancelled,
    Failed(String),
}

struct ImageInfoMessage {
    generation: u64,
    requested_path: String,
    result: Result<
        crate::core::native_image_source::InspectedImageSource,
        crate::core::native_image_source::ImageSourceError,
    >,
}

struct PartitionRefreshMessage {
    generation: u64,
    result: Result<Vec<crate::core::disk::Partition>, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PcaTargetKey {
    partition: String,
    disk_number: Option<u32>,
    partition_number: Option<u32>,
}

struct PcaTargetMessage {
    generation: u64,
    target: PcaTargetKey,
    result: Result<(), String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InstallControlSnapshot {
    format_partition: bool,
    repair_boot: bool,
    unattended_install: bool,
    auto_reboot: bool,
    run_diskpart_scripts: bool,
    driver_index: isize,
    boot_mode_index: isize,
    pca_mode_index: isize,
}

impl InstallControlSnapshot {
    fn apply_to(self, prefs: &mut crate::core::ui_state::InstallPrefs) {
        prefs.format_partition = self.format_partition;
        prefs.repair_boot = self.repair_boot;
        prefs.unattended_install = self.unattended_install;
        prefs.auto_reboot = self.auto_reboot;
        prefs.run_diskpart_scripts = self.run_diskpart_scripts;
        prefs.driver_action = match self.driver_index {
            1 => crate::core::ui_state::DriverAction::SaveOnly,
            2 => crate::core::ui_state::DriverAction::None,
            _ => crate::core::ui_state::DriverAction::AutoImport,
        };
        prefs.boot_mode = match self.boot_mode_index {
            1 => crate::core::ui_state::BootModeSelection::UEFI,
            2 => crate::core::ui_state::BootModeSelection::Legacy,
            _ => crate::core::ui_state::BootModeSelection::Auto,
        };
        prefs.boot_pca_mode = match self.pca_mode_index {
            1 => lr_core::boot_pca::BootPcaMode::Pca2011,
            2 => lr_core::boot_pca::BootPcaMode::Pca2023,
            _ => lr_core::boot_pca::BootPcaMode::Auto,
        };
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PcaTargetContext {
    repair_boot: bool,
    boot_mode: crate::core::ui_state::BootModeSelection,
    partition_style: crate::core::disk::PartitionStyle,
    image_supports_pca: bool,
    advanced_options_enabled: bool,
    run_diskpart_scripts: bool,
}

fn pca_target_uses_uefi(
    boot_mode: crate::core::ui_state::BootModeSelection,
    partition_style: crate::core::disk::PartitionStyle,
) -> bool {
    use crate::core::ui_state::BootModeSelection;
    match boot_mode {
        BootModeSelection::Legacy => false,
        BootModeSelection::UEFI => true,
        BootModeSelection::Auto => partition_style != crate::core::disk::PartitionStyle::MBR,
    }
}

fn pca_target_probe_required(context: PcaTargetContext) -> bool {
    context.repair_boot
        && pca_target_uses_uefi(context.boot_mode, context.partition_style)
        && context.image_supports_pca
}

fn pca_target_error_blocks(
    context: PcaTargetContext,
    firmware_available: bool,
    target_error: bool,
) -> bool {
    target_error
        && !(context.advanced_options_enabled && context.run_diskpart_scripts && firmware_available)
}

fn pca_target_result_is_current(
    active_generation: u64,
    active_target: Option<&PcaTargetKey>,
    message: &PcaTargetMessage,
) -> bool {
    active_generation == message.generation && active_target == Some(&message.target)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PcaPendingStatus {
    FirmwareCompatibility,
    TargetEfiSignature,
}

const fn pca_pending_status(
    selection_is_relevant: bool,
    firmware_pending: bool,
    target_pending: bool,
) -> Option<PcaPendingStatus> {
    if !selection_is_relevant {
        None
    } else if firmware_pending {
        Some(PcaPendingStatus::FirmwareCompatibility)
    } else if target_pending {
        Some(PcaPendingStatus::TargetEfiSignature)
    } else {
        None
    }
}

const fn page_switch_requires_full_layout(page: Page) -> bool {
    matches!(page, Page::Install)
}

enum ToolWorkerMessage {
    Progress(ToolDialogKind, ReadOnlyToolRequest, ToolExecutionEvent),
    Completed(
        ToolDialogKind,
        ReadOnlyToolRequest,
        Result<ReadOnlyToolResult, String>,
    ),
    MutatingCompleted(MutatingToolKind, Result<String, String>),
    ExternalCompleted(
        crate::core::native_tools_controller::NativeToolAction,
        Result<String, String>,
    ),
    BitLockerGateCompleted {
        drive: String,
        result: Result<(), String>,
    },
    DynamicInventoryCompleted {
        kind: MutatingToolKind,
        target: String,
        generation: u64,
        result: Result<Vec<crate::core::native_tool_inventory::InventoryEntry>, String>,
    },
    FirstChoiceInventoryCompleted {
        kind: MutatingToolKind,
        result: Result<Vec<crate::core::native_tool_inventory::InventoryEntry>, String>,
    },
    BatchFormatInventoryCompleted(Result<Vec<BatchFormatVolume>, String>),
    StorageDriverTargetsCompleted(
        Result<Vec<crate::core::native_storage_driver::StorageDriverTarget>, String>,
    ),
    StorageDriverPrepared(Result<super::tool_dialogs_mutating::MutatingToolIntent, String>),
    PasswordResetTargetsCompleted {
        generation: u64,
        result: Result<Vec<PasswordResetTargetOption>, String>,
    },
    PasswordResetAccountsCompleted {
        generation: u64,
        target: crate::core::native_password_reset::PasswordResetTarget,
        result: Result<Vec<crate::core::native_password_reset::PasswordResetAccount>, String>,
    },
    PasswordResetCompleted {
        generation: u64,
        request: crate::core::native_password_reset::PasswordResetRequest,
        result: Result<crate::core::native_password_reset::PasswordResetResult, String>,
    },
    DriverTransferInventoryCompleted(
        Result<Vec<crate::core::native_tool_inventory::InventoryEntry>, String>,
    ),
    BootRepairTargetsCompleted {
        generation: u64,
        result: Result<Vec<crate::core::native_boot_repair::BootRepairTarget>, String>,
    },
    BootRepairCompleted {
        generation: u64,
        result: Result<String, String>,
    },
    AppxTargetsCompleted {
        generation: u64,
        result: Result<Vec<crate::core::native_tool_inventory::InventoryEntry>, String>,
    },
    AppxPackagesCompleted {
        generation: u64,
        target: String,
        result: Result<Vec<crate::core::native_tool_inventory::InventoryEntry>, String>,
    },
    NvidiaTargetsCompleted {
        generation: u64,
        result: Result<Vec<NvidiaRemovalTargetOption>, String>,
    },
    NvidiaHardwareCompleted {
        generation: u64,
        result: Result<crate::core::native_nvidia_removal::NvidiaHardwareReport, String>,
    },
    NvidiaRemovalCompleted {
        generation: u64,
        result: Result<String, String>,
    },
    PartitionCopyInventoryCompleted {
        generation: u64,
        result: Result<Vec<PartitionCopyInventoryRow>, String>,
    },
    PartitionCopyResumeChecked {
        generation: u64,
        result: Result<bool, String>,
    },
    PartitionCopyProgress {
        generation: u64,
        progress: crate::core::native_partition_copy::PartitionCopyProgress,
    },
    PartitionCopyCompleted {
        generation: u64,
        result: Result<crate::core::native_partition_copy::PartitionCopyExecutionResult, String>,
    },
    QuickPartitionInventoryCompleted(
        Result<Vec<crate::core::quick_partition::PhysicalDisk>, String>,
    ),
    QuickPartitionResizeCompleted(Result<String, String>),
    BitLockerManageInventoryCompleted(
        Result<Vec<crate::core::native_bitlocker_manage::BitLockerManageVolume>, String>,
    ),
    BitLockerManageOperationCompleted {
        recovery_key: bool,
        result: Result<String, String>,
    },
    HardwareInspectorCompleted {
        generation: u64,
        result: Box<Result<crate::core::hardware_inspector::HardwareInspectorSnapshot, String>>,
    },
}

#[derive(Clone)]
enum PendingBitLockerIntent {
    Install(crate::core::native_install_controller::StartInstallIntent),
    Backup(BackupLaunchIntent),
}

impl PendingBitLockerIntent {
    fn locked_volumes(
        &self,
        partitions: &[crate::core::disk::Partition],
    ) -> Result<Vec<String>, crate::core::native_bitlocker_gate::NativeBitLockerGateError> {
        let volumes: Vec<_> = partitions
            .iter()
            .map(BitLockerVolumeSnapshot::from)
            .collect();
        match self {
            Self::Install(intent) => {
                plan_install_locked_volumes(&intent.target_partition, &volumes)
            }
            Self::Backup(intent) => {
                let source = match intent {
                    BackupLaunchIntent::Direct(intent) => &intent.config.source_partition,
                    BackupLaunchIntent::ViaPe(intent) => &intent.config.source_partition,
                };
                plan_backup_locked_volumes(source, &volumes)
            }
        }
    }
}

#[derive(Clone)]
struct PendingBitLockerGate {
    intent: PendingBitLockerIntent,
    current_drive: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BitLockerGateCompletion {
    KeepDialog,
    PromptNext,
    ContinuePending,
}

const fn bitlocker_gate_completion(
    unlock_succeeded: bool,
    refresh_succeeded: bool,
    remaining_locked: usize,
) -> BitLockerGateCompletion {
    if !unlock_succeeded || !refresh_succeeded {
        BitLockerGateCompletion::KeepDialog
    } else if remaining_locked > 0 {
        BitLockerGateCompletion::PromptNext
    } else {
        BitLockerGateCompletion::ContinuePending
    }
}

fn preferred_window_size(dpi: i32, screen_width: i32, screen_height: i32) -> (i32, i32) {
    let dpi = dpi.max(96);
    (
        (860 * dpi / 96).min((screen_width - 16 * dpi / 96).max(640)),
        (600 * dpi / 96).min((screen_height - 40 * dpi / 96).max(480)),
    )
}

fn minimum_window_size(dpi: i32, work_width: i32, work_height: i32) -> (i32, i32) {
    let dpi = dpi.max(96);
    // `rcWork` already excludes the taskbar and other app bars. Subtracting another DPI-scaled
    // title-bar allowance here made the minimum client area roughly 80 px too short at 200% DPI,
    // so the last About-page action overlapped the stable command/status bar in low-resolution PE.
    // Clamp directly to the monitor work area; the non-client frame is already part of the tracked
    // window size reported through WM_GETMINMAXINFO.
    let available_width = work_width.max(1);
    let available_height = work_height.max(1);
    (
        (800 * dpi / 96).min(available_width),
        // 600 logical pixels is the compact page/command-bar baseline.  About can need a second
        // measured button row after localization, so the old 560 baseline was not sufficient even
        // on an otherwise roomy 96-DPI monitor.
        (600 * dpi / 96).min(available_height),
    )
}

fn localized_bitlocker_status(status: &crate::core::bitlocker::VolumeStatus) -> String {
    crate::tr!(status.as_str())
}

fn download_failure_message(error: &DownloadWorkerError) -> String {
    let normalized = error.message.to_ascii_lowercase();
    if error.stage == DownloadFailureStage::Transfer
        && (normalized.contains("resource not found")
            || normalized.contains("http 404")
            || normalized.contains("status 404"))
    {
        return crate::tr!("服务器中的资源文件不存在或链接已失效，请刷新后重试。");
    }
    error.message.clone()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CommandBarLayout {
    /// Advanced/Save, Refresh, Primary/Copy positions in control order.
    x: [Option<i32>; 3],
    left_edge: i32,
}

const fn effective_easy_mode_enabled(configured: bool, is_pe_environment: bool) -> bool {
    configured && !is_pe_environment
}

const fn command_bar_visibility(
    page: Page,
    easy_mode_enabled: bool,
    advanced_visible: bool,
    progress_visible: bool,
) -> [bool; 3] {
    if progress_visible {
        [false, false, false]
    } else if advanced_visible {
        [true, false, false]
    } else {
        let easy_visible = matches!(page, Page::Install) && easy_mode_enabled;
        let install_visible = matches!(page, Page::Install) && !easy_visible;
        [
            install_visible || matches!(page, Page::Hardware),
            install_visible,
            !matches!(page, Page::Download | Page::Tools) && !easy_visible,
        ]
    }
}

fn command_bar_layout(
    content_right: i32,
    button_gap: i32,
    button_width: i32,
    visible: [bool; 3],
) -> CommandBarLayout {
    let mut x = [None; 3];
    let mut next_right = content_right;
    for index in (0..x.len()).rev() {
        if visible[index] {
            let button_x = next_right - button_width;
            x[index] = Some(button_x);
            next_right = button_x - button_gap;
        }
    }
    let left_edge = x.into_iter().flatten().min().unwrap_or(content_right);
    CommandBarLayout { x, left_edge }
}

fn centered_command_button_x(content_left: i32, content_width: i32, button_width: i32) -> i32 {
    content_left + (content_width - button_width).max(0) / 2
}

fn preserved_pe_selection(
    preferred_filename: Option<&str>,
    available: &[OnlinePE],
) -> Option<usize> {
    preferred_filename
        .and_then(|filename| {
            available
                .iter()
                .position(|pe| pe.filename.eq_ignore_ascii_case(filename))
        })
        .or_else(|| (available.len() == 1).then_some(0))
}

/// Returns the top of the installation partition heading relative to the image row.
///
/// The optional image-volume row must be a true zero-height row while no WIM volume
/// inventory is available. Keeping this geometry pure makes both visibility states
/// deterministic and avoids exposing an intermediate blank slot during repaint.
fn install_partition_heading_y(image_row_y: i32, dpi: u32, row_expansion: i32) -> i32 {
    image_row_y + (32 + row_expansion.clamp(0, 34)) * dpi as i32 / 96
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InstallVolumeLayoutTransition {
    start: i32,
    target: i32,
    frame: u8,
}

impl InstallVolumeLayoutTransition {
    fn new(start: i32, visible: bool) -> Self {
        Self {
            start: start.clamp(0, 34),
            target: if visible { 34 } else { 0 },
            frame: 0,
        }
    }

    fn expansion(self) -> i32 {
        let distance = self.target - self.start;
        self.start + distance * i32::from(self.frame) / i32::from(INSTALL_VOLUME_LAYOUT_FRAMES)
    }

    fn advance(&mut self) -> bool {
        self.frame = self
            .frame
            .saturating_add(1)
            .min(INSTALL_VOLUME_LAYOUT_FRAMES);
        self.frame == INSTALL_VOLUME_LAYOUT_FRAMES
    }
}

#[cfg(test)]
mod layout_tests {
    use super::{
        bitlocker_gate_completion, catalogue_status_message, centered_command_button_x,
        command_bar_layout, command_bar_visibility, command_button_role,
        confirmed_tool_backend_request, device_change_requests_partition_refresh,
        download_failure_message, effective_easy_mode_enabled, initial_mutating_tool_state,
        install_partition_heading_y, list_view_selection_state_changed, may_publish_install_chrome,
        minimum_window_size, page_switch_requires_full_layout, pca_pending_status,
        pca_target_error_blocks, pca_target_probe_required, pca_target_result_is_current,
        pca_target_uses_uefi, preferred_window_size, preserved_pe_selection,
        primary_state_refresh_for_page, tool_backend_result_succeeded,
        unattended_checked_for_source_preference, BitLockerGateCompletion, InstallControlSnapshot,
        Page, PcaPendingStatus, PcaTargetContext, PcaTargetKey, PcaTargetMessage,
        PrimaryStateRefresh, DBT_CONFIGCHANGED, DBT_DEVICEARRIVAL, DBT_DEVICEREMOVECOMPLETE,
        DBT_DEVNODES_CHANGED, LVIF_STATE, LVIF_TEXT, LVIS_SELECTED,
    };
    use crate::core::disk::PartitionStyle;
    use crate::core::native_download_controller::CatalogueState;
    use crate::core::native_download_executor::{DownloadFailureStage, DownloadWorkerError};
    use crate::core::native_tool_backend::NativeToolBackendRequest;
    use crate::core::ui_state::{BootModeSelection, DriverAction, InstallPrefs};
    use crate::native_ui::tool_dialogs_mutating::{MutatingToolIntent, MutatingToolKind};

    #[test]
    fn catalogue_status_text_always_tracks_the_terminal_controller_state() {
        assert!(catalogue_status_message(&CatalogueState::NotLoaded).is_empty());
        assert_eq!(
            catalogue_status_message(&CatalogueState::Loading),
            crate::tr!("正在刷新在线资源目录...")
        );
        assert_eq!(
            catalogue_status_message(&CatalogueState::Ready),
            crate::tr!("在线资源目录已刷新。")
        );
        assert_eq!(
            catalogue_status_message(&CatalogueState::Failed("network failed".into())),
            "network failed"
        );
    }

    #[test]
    fn window_scales_to_monitor_dpi_when_space_allows() {
        assert_eq!(preferred_window_size(144, 1920, 1080), (1290, 900));
        assert_eq!(preferred_window_size(192, 2560, 1440), (1720, 1200));
    }

    #[test]
    fn low_resolution_window_stays_inside_the_compact_bounds() {
        assert_eq!(preferred_window_size(96, 800, 600), (784, 560));
        assert_eq!(preferred_window_size(144, 1280, 720), (1256, 660));
    }

    #[test]
    fn minimum_window_tracks_dpi_but_never_exceeds_the_work_area() {
        assert_eq!(minimum_window_size(96, 1920, 1080), (800, 600));
        assert_eq!(minimum_window_size(144, 1280, 720), (1200, 720));
        assert_eq!(minimum_window_size(192, 1280, 720), (1280, 720));
    }

    #[test]
    fn partition_refresh_only_accepts_inventory_changing_device_events() {
        for event in [
            DBT_DEVNODES_CHANGED,
            DBT_CONFIGCHANGED,
            DBT_DEVICEARRIVAL,
            DBT_DEVICEREMOVECOMPLETE,
        ] {
            assert!(device_change_requests_partition_refresh(event));
        }
        // Query/remove-pending notifications require their normal DefWindowProc contract and
        // must not start an expensive inventory scan.
        assert!(!device_change_requests_partition_refresh(0x8001));
        assert!(!device_change_requests_partition_refresh(0x8003));
    }

    #[test]
    fn install_selection_work_is_limited_to_real_selected_bit_transitions() {
        assert!(list_view_selection_state_changed(
            LVIF_STATE.0,
            0,
            LVIS_SELECTED.0
        ));
        assert!(list_view_selection_state_changed(
            LVIF_STATE.0,
            LVIS_SELECTED.0,
            0
        ));
        assert!(!list_view_selection_state_changed(LVIF_STATE.0, 1, 1));
        assert!(!list_view_selection_state_changed(
            LVIF_TEXT.0,
            0,
            LVIS_SELECTED.0
        ));
    }

    #[test]
    fn unattended_default_uses_configured_preference_and_selected_source_only() {
        assert!(unattended_checked_for_source_preference(true, false));
        assert!(!unattended_checked_for_source_preference(false, false));
        assert!(!unattended_checked_for_source_preference(true, true));
        assert!(!unattended_checked_for_source_preference(false, true));
    }

    #[test]
    fn command_bar_packs_only_visible_buttons_from_the_right() {
        let hardware = command_bar_layout(1_000, 8, 120, [true, false, true]);
        assert_eq!(hardware.x, [Some(752), None, Some(880)]);
        assert_eq!(hardware.left_edge, 752);

        let install = command_bar_layout(1_000, 8, 120, [true, true, true]);
        assert_eq!(install.x, [Some(624), Some(752), Some(880)]);
        assert_eq!(install.left_edge, 624);

        let backup = command_bar_layout(1_000, 8, 120, [false, false, true]);
        assert_eq!(backup.x, [None, None, Some(880)]);
        assert_eq!(backup.left_edge, 880);
    }

    #[test]
    fn command_visibility_matches_every_install_shell_state() {
        assert_eq!(
            command_bar_visibility(Page::Install, false, false, false),
            [true, true, true]
        );
        assert_eq!(
            command_bar_visibility(Page::Install, true, false, false),
            [false, false, false]
        );
        assert_eq!(
            command_bar_visibility(Page::Install, false, true, false),
            [true, false, false]
        );
        assert_eq!(
            command_bar_visibility(Page::Install, false, false, true),
            [false, false, false]
        );
    }

    #[test]
    fn pe_environment_disables_configured_easy_mode_without_rewriting_the_preference() {
        assert!(effective_easy_mode_enabled(true, false));
        assert!(!effective_easy_mode_enabled(true, true));
        assert!(!effective_easy_mode_enabled(false, false));
        assert!(!effective_easy_mode_enabled(false, true));
    }

    #[test]
    fn startup_pca_probe_is_silent_until_the_install_selection_is_relevant() {
        assert_eq!(pca_pending_status(false, true, false), None);
        assert_eq!(
            pca_pending_status(true, true, false),
            Some(PcaPendingStatus::FirmwareCompatibility)
        );
        assert_eq!(
            pca_pending_status(true, false, true),
            Some(PcaPendingStatus::TargetEfiSignature)
        );
        assert_eq!(pca_pending_status(true, false, false), None);
    }

    #[test]
    fn refreshed_pe_catalogue_preserves_a_matching_user_selection() {
        let pe = |name: &str, filename: &str| crate::download::config::OnlinePE {
            download_url: format!("https://example.invalid/{filename}"),
            display_name: name.to_owned(),
            filename: filename.to_owned(),
            md5: None,
            sha256: None,
        };
        let available = vec![pe("PE A renamed", "a.wim"), pe("PE B", "b.wim")];
        assert_eq!(preserved_pe_selection(Some("B.WIM"), &available), Some(1));
        assert_eq!(
            preserved_pe_selection(Some("missing.wim"), &available),
            None
        );
        assert_eq!(
            preserved_pe_selection(None, &[pe("Only PE", "only.wim")]),
            Some(0)
        );
    }

    #[test]
    fn hardware_save_and_copy_remain_adjacent_at_supported_dpi() {
        for dpi in [96, 144, 192] {
            let scale = |value: i32| value * dpi / 96;
            let layout =
                command_bar_layout(scale(1_000), scale(8), scale(136), [true, false, true]);
            let save_x = layout.x[0].expect("hardware Save must be visible");
            let copy_x = layout.x[2].expect("hardware Copy must be visible");
            assert_eq!(copy_x - (save_x + scale(136)), scale(8));
            assert_eq!(copy_x + scale(136), scale(1_000));
        }
    }

    #[test]
    fn advanced_save_and_return_is_centered_without_changing_normal_command_packing() {
        assert_eq!(centered_command_button_x(272, 960, 136), 684);
        assert_eq!(centered_command_button_x(41, 501, 100), 241);
        assert_eq!(centered_command_button_x(20, 60, 96), 20);

        let normal = command_bar_layout(1_232, 8, 136, [true, true, true]);
        assert_eq!(normal.x, [Some(808), Some(952), Some(1_096)]);
    }

    #[test]
    fn returning_to_install_requests_live_primary_state_recalculation() {
        assert_eq!(
            primary_state_refresh_for_page(Page::Install),
            PrimaryStateRefresh::Install
        );
        assert_eq!(
            primary_state_refresh_for_page(Page::Backup),
            PrimaryStateRefresh::Backup
        );
        for page in [Page::Download, Page::Tools, Page::Hardware, Page::About] {
            assert_eq!(
                primary_state_refresh_for_page(page),
                PrimaryStateRefresh::None
            );
        }
    }

    #[test]
    fn install_async_results_never_overwrite_other_page_chrome() {
        assert!(may_publish_install_chrome(Page::Install, false, false));
        assert!(!may_publish_install_chrome(Page::About, false, false));
        assert!(!may_publish_install_chrome(Page::Hardware, false, false));
        assert!(!may_publish_install_chrome(Page::Install, true, false));
        assert!(!may_publish_install_chrome(Page::Install, false, true));
    }

    #[test]
    fn every_install_page_entry_rebuilds_inventory_dependent_geometry() {
        assert!(page_switch_requires_full_layout(Page::Install));
        for page in [
            Page::Backup,
            Page::Download,
            Page::Tools,
            Page::Hardware,
            Page::About,
        ] {
            assert!(!page_switch_requires_full_layout(page));
        }
    }

    #[test]
    fn visible_install_defaults_override_stale_cached_preferences_without_notifications() {
        let mut prefs = InstallPrefs {
            format_partition: false,
            repair_boot: false,
            unattended_install: false,
            auto_reboot: false,
            run_diskpart_scripts: true,
            driver_action: DriverAction::None,
            boot_mode: BootModeSelection::Legacy,
            boot_pca_mode: lr_core::boot_pca::BootPcaMode::Pca2011,
            ..InstallPrefs::default()
        };
        InstallControlSnapshot {
            format_partition: true,
            repair_boot: true,
            unattended_install: true,
            auto_reboot: true,
            run_diskpart_scripts: false,
            driver_index: 0,
            boot_mode_index: 0,
            pca_mode_index: 2,
        }
        .apply_to(&mut prefs);

        assert!(prefs.format_partition);
        assert!(prefs.repair_boot);
        assert!(prefs.unattended_install);
        assert!(prefs.auto_reboot);
        assert!(!prefs.run_diskpart_scripts);
        assert_eq!(prefs.driver_action, DriverAction::AutoImport);
        assert_eq!(prefs.boot_mode, BootModeSelection::Auto);
        assert_eq!(prefs.boot_pca_mode, lr_core::boot_pca::BootPcaMode::Pca2023);
    }

    #[test]
    fn hidden_image_volume_row_has_zero_layout_occupancy_at_supported_dpi() {
        for dpi in [96, 120, 144, 192] {
            let image_y = 40 * dpi as i32 / 96;
            let hidden = install_partition_heading_y(image_y, dpi, 0);
            let visible = install_partition_heading_y(image_y, dpi, 34);

            assert_eq!(hidden, image_y + 32 * dpi as i32 / 96);
            assert_eq!(visible - hidden, 34 * dpi as i32 / 96);
        }
    }

    #[test]
    fn image_volume_layout_transition_is_short_linear_and_interruptible() {
        let mut showing = super::InstallVolumeLayoutTransition::new(0, true);
        assert_eq!(showing.expansion(), 0);
        assert!(!showing.advance());
        assert_eq!(showing.expansion(), 11);
        assert!(!showing.advance());
        assert_eq!(showing.expansion(), 22);

        let mut interrupted = super::InstallVolumeLayoutTransition::new(showing.expansion(), false);
        assert_eq!(interrupted.expansion(), 22);
        assert!(!interrupted.advance());
        assert_eq!(interrupted.expansion(), 15);
        assert!(!interrupted.advance());
        assert_eq!(interrupted.expansion(), 8);
        assert!(interrupted.advance());
        assert_eq!(interrupted.expansion(), 0);
    }

    #[test]
    fn dead_remote_resource_is_presented_as_a_localized_actionable_failure() {
        let error = DownloadWorkerError {
            stage: DownloadFailureStage::Transfer,
            message: "Resource not found".into(),
        };
        assert_eq!(
            download_failure_message(&error),
            crate::tr!("服务器中的资源文件不存在或链接已失效，请刷新后重试。")
        );
    }

    #[test]
    fn pca_target_probe_only_runs_for_repaired_uefi_supported_images() {
        let base = PcaTargetContext {
            repair_boot: true,
            boot_mode: BootModeSelection::Auto,
            partition_style: PartitionStyle::GPT,
            image_supports_pca: true,
            advanced_options_enabled: false,
            run_diskpart_scripts: false,
        };
        assert!(pca_target_probe_required(base));
        assert!(!pca_target_probe_required(PcaTargetContext {
            repair_boot: false,
            ..base
        }));
        assert!(!pca_target_probe_required(PcaTargetContext {
            boot_mode: BootModeSelection::Legacy,
            ..base
        }));
        assert!(!pca_target_probe_required(PcaTargetContext {
            partition_style: PartitionStyle::MBR,
            ..base
        }));
        assert!(!pca_target_probe_required(PcaTargetContext {
            image_supports_pca: false,
            ..base
        }));
        assert!(pca_target_uses_uefi(
            BootModeSelection::Auto,
            PartitionStyle::Unknown
        ));
    }

    #[test]
    fn diskpart_override_only_allows_a_missing_esp_when_firmware_is_known() {
        let context = PcaTargetContext {
            repair_boot: true,
            boot_mode: BootModeSelection::UEFI,
            partition_style: PartitionStyle::GPT,
            image_supports_pca: true,
            advanced_options_enabled: true,
            run_diskpart_scripts: true,
        };
        assert!(pca_target_error_blocks(context, false, true));
        assert!(!pca_target_error_blocks(context, true, true));
        assert!(pca_target_error_blocks(
            PcaTargetContext {
                run_diskpart_scripts: false,
                ..context
            },
            true,
            true
        ));
    }

    #[test]
    fn stale_pca_target_results_are_rejected_by_generation_and_identity() {
        let target = PcaTargetKey {
            partition: "D:".into(),
            disk_number: Some(1),
            partition_number: Some(3),
        };
        let message = PcaTargetMessage {
            generation: 7,
            target: target.clone(),
            result: Ok(()),
        };
        assert!(pca_target_result_is_current(7, Some(&target), &message));
        assert!(!pca_target_result_is_current(8, Some(&target), &message));
        let replaced = PcaTargetKey {
            partition: "D:".into(),
            disk_number: Some(2),
            partition_number: Some(1),
        };
        assert!(!pca_target_result_is_current(7, Some(&replaced), &message));
    }

    #[test]
    fn bitlocker_gate_only_continues_after_successful_unlock_and_refresh() {
        assert_eq!(
            bitlocker_gate_completion(false, true, 0),
            BitLockerGateCompletion::KeepDialog
        );
        assert_eq!(
            bitlocker_gate_completion(true, false, 0),
            BitLockerGateCompletion::KeepDialog
        );
        assert_eq!(
            bitlocker_gate_completion(true, true, 1),
            BitLockerGateCompletion::PromptNext
        );
        assert_eq!(
            bitlocker_gate_completion(true, true, 0),
            BitLockerGateCompletion::ContinuePending
        );
        assert!(!tool_backend_result_succeeded(
            &crate::core::native_tool_backend::NativeToolBackendResult::BitLocker {
                success: false,
                message: "denied".into(),
                error_code: Some(1),
            }
        ));
    }

    #[test]
    fn confirmed_batch_format_maps_to_typed_backend_request() {
        let request = confirmed_tool_backend_request(
            MutatingToolKind::BatchFormat,
            &MutatingToolIntent::BatchFormat {
                partitions: vec!["D:".into(), "E:".into()],
                file_system: "NTFS".into(),
                volume_label: "Data".into(),
            },
        )
        .unwrap();

        match request {
            NativeToolBackendRequest::BatchFormat { plan, request } => {
                assert_eq!(
                    plan.action,
                    crate::core::native_tools_controller::NativeToolAction::BatchFormat
                );
                assert_eq!(request.drives, ["D:", "E:"]);
                assert_eq!(request.file_system, "NTFS");
                assert_eq!(request.volume_label, "Data");
            }
            other => panic!("expected batch format request, got {other:?}"),
        }
    }

    #[test]
    fn confirmed_appx_intent_preserves_typed_online_and_offline_targets() {
        for (root, expected) in [
            (
                "__CURRENT__",
                crate::core::native_appx::AppxTarget::CurrentSystem,
            ),
            (
                "D:",
                crate::core::native_appx::AppxTarget::OfflineWindows("D:".into()),
            ),
        ] {
            let request = confirmed_tool_backend_request(
                MutatingToolKind::RemoveAppx,
                &MutatingToolIntent::RemoveAppx {
                    packages: vec!["Contoso.App_1.0_x64__test".into()],
                    offline_root: root.into(),
                },
            )
            .unwrap();
            match request {
                NativeToolBackendRequest::RemoveAppx { request, .. } => {
                    assert_eq!(request.target, expected);
                    assert_eq!(request.packages, ["Contoso.App_1.0_x64__test"]);
                }
                other => panic!("expected APPX backend request, got {other:?}"),
            }
        }
    }

    #[test]
    fn confirmed_partition_copy_maps_to_typed_backend_request() {
        let request = confirmed_tool_backend_request(
            MutatingToolKind::PartitionCopy,
            &MutatingToolIntent::CopyPartition {
                source: "D:".into(),
                target: "E:".into(),
            },
        )
        .unwrap();
        match request {
            NativeToolBackendRequest::PartitionCopy { plan, request } => {
                assert_eq!(
                    plan.action,
                    crate::core::native_tools_controller::NativeToolAction::PartitionCopy
                );
                assert_eq!(request.source, "D:");
                assert_eq!(request.target, "E:");
            }
            other => panic!("expected partition-copy request, got {other:?}"),
        }
    }

    #[test]
    fn backup_browse_routes_to_secondary_owner_draw_visual() {
        assert_eq!(
            command_button_role(crate::native_ui::pages::backup::ID_BROWSE),
            crate::native_ui::controls::ButtonRole::Secondary
        );
        assert_eq!(
            command_button_role(super::ID_PRIMARY),
            crate::native_ui::controls::ButtonRole::Primary
        );
    }

    #[test]
    fn mutating_dialog_partition_inventory_is_routed_to_choices_and_lists() {
        let partitions = vec![
            crate::core::disk::Partition {
                letter: "C:".into(),
                total_size_mb: 100,
                free_size_mb: 50,
                label: "Windows".into(),
                is_system_partition: true,
                has_windows: true,
                partition_style: crate::core::disk::PartitionStyle::GPT,
                disk_number: Some(0),
                partition_number: Some(1),
                bitlocker_status: crate::core::bitlocker::VolumeStatus::NotEncrypted,
            },
            crate::core::disk::Partition {
                letter: "D:".into(),
                total_size_mb: 200,
                free_size_mb: 100,
                label: "Data".into(),
                is_system_partition: false,
                has_windows: false,
                partition_style: crate::core::disk::PartitionStyle::GPT,
                disk_number: Some(1),
                partition_number: Some(1),
                bitlocker_status: crate::core::bitlocker::VolumeStatus::EncryptedUnlocked,
            },
        ];
        let copy = initial_mutating_tool_state(MutatingToolKind::PartitionCopy, &partitions, false);
        assert_eq!(copy.first_choices, ["D:"]);
        assert_eq!(copy.second_choices, ["D:"]);
        let format = initial_mutating_tool_state(MutatingToolKind::BatchFormat, &partitions, false);
        assert_eq!(format.available_items, ["D:"]);
        let repair = initial_mutating_tool_state(MutatingToolKind::RepairBoot, &partitions, false);
        assert_eq!(repair.first_choices, ["C:"]);
        let quick =
            initial_mutating_tool_state(MutatingToolKind::QuickPartition, &partitions, false);
        assert_eq!(quick.first_choices, ["0", "1"]);
    }

    #[test]
    fn english_catalogue_covers_native_pages_tools_and_runtime_messages() {
        let document: serde_json::Value =
            serde_json::from_str(include_str!("../../../assets/release/lang/en-US.json"))
                .expect("en-US.json must remain valid JSON");
        let data = document["data"]
            .as_object()
            .expect("en-US.json must contain a data object");

        for key in [
            "系统安装",
            "系统备份",
            "在线下载",
            "工具箱",
            "硬件信息",
            "关于",
            "卸载 NVIDIA 驱动",
            "分区对拷",
            "批量格式化",
            "导入存储驱动",
            "一键分区",
            "移除 APPX",
            "驱动备份与恢复",
            "修复系统引导",
            "网络信息",
            "软件列表",
            "时间同步",
            "运行 Ghost",
            "查看 GHO 密码",
            "重置网络",
            "磁盘空间分析",
            "校验系统镜像",
            "管理 BitLocker",
            "文件哈希校验",
            "重置系统密码",
            "选择要移除的 NVIDIA 设备和组件。",
            "确认源分区和目标分区；目标内容将被覆盖。",
            "选择分区、文件系统和卷标。",
            "选择包含 INF 的存储控制器驱动目录。",
            "选择物理磁盘并复核完整分区布局。",
            "选择备份或恢复模式以及驱动目录。",
            "选择 Windows 分区并确认 BIOS/UEFI 模式。",
            "同步系统时间",
            "从指定 NTP 服务器读取并设置系统时间。",
            "启动随包提供的 Ghost 工具。",
            "将重置网络组件和适配器配置。",
            "运行 SpaceSniffer",
            "启动随包提供的磁盘空间分析工具。",
            "选择卷和要执行的 BitLocker 操作。",
            "选择离线 Windows 和账户；不会显示或保存密码。",
            "源分区",
            "目标分区",
            "我已核对目标分区",
            "快速格式化",
            "离线 Windows（可选）",
            "驱动目录",
            "包含子目录",
            "恢复驱动",
            "Windows 分区",
            "自动检测启动模式",
            "NTP 服务器",
            "当前状态",
            "同步后重新读取",
            "BitLocker 卷",
            "使用恢复密钥解锁",
            "系统目标（当前系统或 Windows 目录）",
            "账户筛选",
            "同时启用所选账户",
            "检测到的设备",
            "移除范围",
            "同时移除 NVIDIA 软件",
            "应用筛选",
            "说明",
            "我了解此操作的影响",
            "系统镜像:",
            "选择安装分区:",
            "选择要备份的分区:",
            "选择要下载的资源。",
            "选择要运行的系统维护、修复或诊断工具。",
            "当前计算机的系统和硬件摘要。",
            "界面语言:",
            "下载线程:",
            "名称：{}\r\n描述：{}\r\n类型：{}\r\n状态：{}\r\n速度：{} Mbps\r\nMAC：{}\r\nIP：{}",
            "{}不能为空",
            "请至少选择一项",
            "请从列表中选择{}",
            "源分区和目标分区不能相同",
            "目标磁盘编号无效",
            "目标磁盘指纹不存在，请刷新磁盘列表",
            "恢复驱动时必须选择离线 Windows",
            "解锁 BitLocker 时必须填写密码或恢复密钥",
            "BitLocker 恢复密钥必须是 8 组、每组 6 位数字",
            "请选择当前系统或离线 Windows 目录",
            "请再次确认目标和选项。此操作尚未执行。\r\n{}",
        ] {
            let translated = data
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_else(|| panic!("missing English translation for {key:?}"));
            assert!(
                !translated
                    .chars()
                    .any(|character| ('\u{4e00}'..='\u{9fff}').contains(&character)),
                "English translation for {key:?} still contains CJK text: {translated:?}"
            );
        }
    }
}

const ID_NAV_INSTALL: u16 = 100;
const ID_NAV_BACKUP: u16 = 101;
const ID_NAV_DOWNLOAD: u16 = 102;
const ID_NAV_TOOLS: u16 = 103;
const ID_NAV_HARDWARE: u16 = 104;
const ID_NAV_ABOUT: u16 = 105;
const ID_IMAGE_EDIT: u16 = 200;
const ID_BROWSE: u16 = 201;
const ID_PARTITIONS: u16 = 202;
const ID_FORMAT: u16 = 203;
const ID_BOOT: u16 = 204;
const ID_UNATTEND: u16 = 205;
const ID_DRIVER: u16 = 206;
const ID_REBOOT: u16 = 207;
const ID_DRIVER_COMBO: u16 = 208;
const ID_BOOT_COMBO: u16 = 209;
const ID_ADVANCED: u16 = 210;
const ID_REFRESH: u16 = 211;
const ID_PRIMARY: u16 = 212;
const ID_IMAGE_VOLUME: u16 = 213;
const ID_INSTALL_PE: u16 = 214;
const ID_UNATTEND_BROWSE: u16 = 215;
const ID_UNATTEND_CLEAR: u16 = 216;
const ID_PCA_MODE: u16 = 217;
const ID_RUN_DISKPART: u16 = 218;
const ID_OPEN_DISKPART_DIR: u16 = 219;
const ID_EDIT_BOOT_COMMANDS: u16 = 220;

const fn command_button_role(id: u16) -> ButtonRole {
    if id == ID_PRIMARY {
        ButtonRole::Primary
    } else {
        ButtonRole::Secondary
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Install,
    Backup,
    Download,
    Tools,
    Hardware,
    About,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrimaryStateRefresh {
    Install,
    Backup,
    None,
}

const fn primary_state_refresh_for_page(page: Page) -> PrimaryStateRefresh {
    match page {
        Page::Install => PrimaryStateRefresh::Install,
        Page::Backup => PrimaryStateRefresh::Backup,
        Page::Download | Page::Tools | Page::Hardware | Page::About => PrimaryStateRefresh::None,
    }
}

fn may_publish_install_chrome(page: Page, advanced_visible: bool, progress_visible: bool) -> bool {
    page == Page::Install && !advanced_visible && !progress_visible
}

#[derive(Clone, Copy)]
struct Handles {
    brand: HWND,
    nav: [HWND; 6],
    title: HWND,
    description: HWND,
    image_label: HWND,
    image_edit: HWND,
    browse: HWND,
    image_volume_label: HWND,
    image_volume: HWND,
    partitions_label: HWND,
    partitions: HWND,
    format: HWND,
    boot: HWND,
    unattend: HWND,
    unattend_browse: HWND,
    unattend_clear: HWND,
    unattend_path: HWND,
    driver_label: HWND,
    driver: HWND,
    reboot: HWND,
    boot_label: HWND,
    boot_mode: HWND,
    pca_label: HWND,
    pca_mode: HWND,
    run_diskpart: HWND,
    open_diskpart_dir: HWND,
    edit_boot_commands: HWND,
    pe_label: HWND,
    pe: HWND,
    advanced: HWND,
    refresh: HWND,
    status: HWND,
    primary: HWND,
}

struct NativeWindow {
    page: Page,
    dpi: u32,
    font: HFONT,
    font_bold: HFONT,
    font_brand: HFONT,
    palette: theme::Palette,
    brushes: Brushes,
    handles: Option<Handles>,
    app_config: crate::core::app_config::AppConfig,
    is_pe_environment: bool,
    partitions: Vec<crate::core::disk::Partition>,
    partition_refresh_generation: u64,
    partition_refresh_in_flight: bool,
    partition_refresh_requested: bool,
    partition_refresh_error: Option<String>,
    partition_list_replacing: bool,
    install_selection_update_pending: bool,
    image_volumes: Vec<crate::core::dism::ImageInfo>,
    install_volume_row_presented: bool,
    install_volume_layout_transition: Option<InstallVolumeLayoutTransition>,
    effective_image_path: Option<String>,
    xp_i386_source: Option<String>,
    mounted_iso: Option<std::path::PathBuf>,
    image_request_generation: u64,
    image_edit_programmatic_change: bool,
    advanced_defaults_target: Option<String>,
    custom_unattend_path: String,
    custom_unattend_error: Option<String>,
    source_has_unattend: bool,
    pca_firmware: Option<lr_core::boot_pca::FirmwarePcaInfo>,
    pca_detection_pending: bool,
    pca_target_generation: u64,
    pca_target_key: Option<PcaTargetKey>,
    pca_target_detection_pending: bool,
    pca_target_detection_error: Option<String>,
    backup_page: Option<BackupPage>,
    download_page: Option<DownloadPage>,
    download_controller: NativeDownloadController,
    pe_catalogue: Vec<OnlinePE>,
    easy_page: Option<EasyModePage>,
    easy_controller: NativeEasyModeController,
    pending_easy_install: Option<crate::core::native_easy_mode_controller::StartEasyInstallIntent>,
    pending_install_after_pe_download:
        Option<crate::core::native_install_controller::StartInstallIntent>,
    pending_backup_after_pe_download: Option<BackupLaunchIntent>,
    pending_expand_after_pe_download: Option<ExpandCRequest>,
    pending_bitlocker_gate: Option<PendingBitLockerGate>,
    tools_page: Option<ToolsPage>,
    hardware_page: Option<HardwareInfoPage>,
    hardware_copy_feedback: HardwareCopyFeedback,
    about_page: Option<AboutPage>,
    advanced_page: Option<AdvancedPage>,
    progress_page: Option<ProgressPage>,
    progress_visible: bool,
    close_after_task: bool,
    backup_execution: Option<BackupExecution>,
    download_worker: Option<DownloadWorker>,
    download_follow_up: Option<crate::core::native_download_controller::DownloadCompletion>,
    install_messages: Option<Receiver<InstallWorkerMessage>>,
    install_cancel: Option<Arc<AtomicBool>>,
    install_auto_reboot: bool,
    catalogue_messages: Option<Receiver<crate::download::server_config::RemoteConfig>>,
    tool_dialogs: Vec<NativeToolDialog>,
    tool_background_jobs: usize,
    image_verify_cancel: Option<Arc<AtomicBool>>,
    mutating_tool_dialogs: Vec<NativeMutatingToolDialog>,
    time_sync_dialog: Option<NativeTimeSyncDialog>,
    network_reset_dialog: Option<NativeNetworkResetDialog>,
    batch_format_dialog: Option<NativeBatchFormatDialog>,
    storage_driver_dialog: Option<NativeStorageDriverDialog>,
    password_reset_dialog: Option<NativePasswordResetDialog>,
    password_reset_generation: u64,
    driver_transfer_dialog: Option<NativeDriverTransferDialog>,
    boot_repair_dialog: Option<NativeBootRepairDialog>,
    boot_repair_generation: u64,
    appx_dialog: Option<NativeAppxDialog>,
    appx_generation: u64,
    nvidia_dialog: Option<NativeNvidiaRemovalDialog>,
    nvidia_generation: u64,
    partition_copy_dialog: Option<NativePartitionCopyDialog>,
    partition_copy_generation: u64,
    quick_partition_dialog: Option<NativeQuickPartitionDialog>,
    pending_quick_partition_command: Option<QuickPartitionDialogIntent>,
    bitlocker_manage_dialog: Option<NativeBitLockerManageDialog>,
    pending_bitlocker_manage_command: Option<BitLockerManageDialogIntent>,
    expand_c_dialog: Option<NativeExpandCDialog>,
    expand_c_analysis: Option<
        Receiver<
            Result<
                crate::core::native_expand_c_controller::NativeExpandCAnalysis,
                crate::core::native_expand_c_controller::NativeExpandCAnalysisError,
            >,
        >,
    >,
    expand_c_execution: Option<Receiver<ExpandCWorkerMessage>>,
    hardware_inspector_dialog: Option<NativeHardwareInspectorDialog>,
    hardware_inspector_generation: u64,
    tool_worker_sender: std::sync::mpsc::Sender<ToolWorkerMessage>,
    tool_worker_messages: Receiver<ToolWorkerMessage>,
    advanced_visible: bool,
    config: Arc<PreloadedConfig>,
}

impl Drop for NativeWindow {
    fn drop(&mut self) {
        if let Some(path) = self.mounted_iso.take() {
            if let Err(error) =
                crate::core::iso::IsoMounter::unmount_iso_by_path(&path.to_string_lossy())
            {
                log::warn!("卸载原生安装页 ISO 失败: {error}");
            }
        }
        unsafe {
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font);
            }
            if !self.font_bold.is_invalid() {
                let _ = DeleteObject(self.font_bold);
            }
            if !self.font_brand.is_invalid() {
                let _ = DeleteObject(self.font_brand);
            }
        }
    }
}

impl NativeWindow {
    fn new(config: Arc<PreloadedConfig>) -> Self {
        let palette = theme::Palette::system();
        let app_config = config.app_config.clone();
        let is_pe_environment = config
            .system_info
            .as_ref()
            .map(|info| info.is_pe_environment)
            .unwrap_or_else(crate::core::disk::DiskManager::is_pe_environment);
        let partitions = config.partitions.clone();
        let mut download_controller = NativeDownloadController::default();
        let mut pe_catalogue = PeCache::load().unwrap_or_default();
        let mut easy_controller = NativeEasyModeController::new(
            effective_easy_mode_enabled(app_config.easy_mode_enabled, is_pe_environment),
            app_config.easy_mode_settings_tip_dismissed,
        );
        if let Some(remote) = &config.remote_config {
            let catalogue = ConfigManager {
                systems: remote
                    .dl_content
                    .as_deref()
                    .map(ConfigManager::parse_system_list)
                    .unwrap_or_default(),
                pe_list: remote
                    .pe_content
                    .as_deref()
                    .map(ConfigManager::parse_pe_list)
                    .unwrap_or_default(),
                software_list: remote
                    .soft_content
                    .as_deref()
                    .map(ConfigManager::parse_software_list)
                    .unwrap_or_default(),
                gpu_driver_list: remote
                    .gpu_content
                    .as_deref()
                    .map(ConfigManager::parse_gpu_driver_list)
                    .unwrap_or_default(),
                ..ConfigManager::default()
            };
            download_controller.replace_trusted_remote_catalogue(&catalogue);
            if !catalogue.pe_list.is_empty() {
                pe_catalogue = catalogue.pe_list.clone();
                if let Err(error) = PeCache::save(&catalogue.pe_list) {
                    log::warn!("保存 PE 目录缓存失败: {error}");
                }
            }
            let easy_config = remote
                .easy_content
                .as_deref()
                .and_then(|content| serde_json::from_str(content).ok());
            easy_controller.set_catalogue(easy_config.as_ref(), false);
        }
        let (tool_worker_sender, tool_worker_messages) = std::sync::mpsc::channel();
        Self {
            page: Page::Install,
            dpi: 96,
            font: HFONT::default(),
            font_bold: HFONT::default(),
            font_brand: HFONT::default(),
            palette,
            brushes: Brushes::new(palette),
            handles: None,
            app_config,
            is_pe_environment,
            partitions,
            partition_refresh_generation: 0,
            partition_refresh_in_flight: false,
            partition_refresh_requested: false,
            partition_refresh_error: None,
            partition_list_replacing: false,
            install_selection_update_pending: false,
            image_volumes: Vec::new(),
            install_volume_row_presented: false,
            install_volume_layout_transition: None,
            effective_image_path: None,
            xp_i386_source: None,
            mounted_iso: None,
            image_request_generation: 0,
            image_edit_programmatic_change: false,
            advanced_defaults_target: None,
            custom_unattend_path: String::new(),
            custom_unattend_error: None,
            source_has_unattend: false,
            pca_firmware: None,
            pca_detection_pending: false,
            pca_target_generation: 0,
            pca_target_key: None,
            pca_target_detection_pending: false,
            pca_target_detection_error: None,
            backup_page: None,
            download_page: None,
            download_controller,
            pe_catalogue,
            easy_page: None,
            easy_controller,
            pending_easy_install: None,
            pending_install_after_pe_download: None,
            pending_backup_after_pe_download: None,
            pending_expand_after_pe_download: None,
            pending_bitlocker_gate: None,
            tools_page: None,
            hardware_page: None,
            hardware_copy_feedback: HardwareCopyFeedback::default(),
            about_page: None,
            advanced_page: None,
            progress_page: None,
            progress_visible: false,
            close_after_task: false,
            backup_execution: None,
            download_worker: None,
            download_follow_up: None,
            install_messages: None,
            install_cancel: None,
            install_auto_reboot: false,
            catalogue_messages: None,
            tool_dialogs: Vec::new(),
            tool_background_jobs: 0,
            image_verify_cancel: None,
            mutating_tool_dialogs: Vec::new(),
            time_sync_dialog: None,
            network_reset_dialog: None,
            batch_format_dialog: None,
            storage_driver_dialog: None,
            password_reset_dialog: None,
            password_reset_generation: 0,
            driver_transfer_dialog: None,
            boot_repair_dialog: None,
            boot_repair_generation: 0,
            appx_dialog: None,
            appx_generation: 0,
            nvidia_dialog: None,
            nvidia_generation: 0,
            partition_copy_dialog: None,
            partition_copy_generation: 0,
            quick_partition_dialog: None,
            pending_quick_partition_command: None,
            bitlocker_manage_dialog: None,
            pending_bitlocker_manage_command: None,
            expand_c_dialog: None,
            expand_c_analysis: None,
            expand_c_execution: None,
            hardware_inspector_dialog: None,
            hardware_inspector_generation: 0,
            tool_worker_sender,
            tool_worker_messages,
            advanced_visible: false,
            config,
        }
    }

    fn scale(&self, value: i32) -> i32 {
        value * self.dpi as i32 / 96
    }

    fn easy_mode_enabled(&self) -> bool {
        effective_easy_mode_enabled(self.app_config.easy_mode_enabled, self.is_pe_environment)
    }

    fn has_active_long_task(&self) -> bool {
        self.backup_execution.is_some()
            || self.download_worker.is_some()
            || self.install_messages.is_some()
            || self.expand_c_execution.is_some()
            || self.image_verify_cancel.is_some()
    }

    unsafe fn request_safe_close(&mut self, hwnd: HWND) {
        if self.close_after_task {
            return;
        }
        self.close_after_task = true;
        if let Some(execution) = &self.backup_execution {
            execution.request_cancel();
        }
        if let Some(worker) = &self.download_worker {
            let _ = worker.send(DownloadWorkerCommand::Cancel);
        }
        if let Some(cancel) = &self.install_cancel {
            cancel.store(true, Ordering::SeqCst);
        }
        if let Some(cancel) = &self.image_verify_cancel {
            cancel.store(true, Ordering::SeqCst);
        }
        if let Some(page) = &mut self.progress_page {
            let mut progress = page.state().clone();
            if progress.cancellable {
                progress.status = ProgressStatus::Cancelling;
                progress.current_step = crate::tr!("正在请求取消...");
                progress.status_text =
                    crate::tr!("窗口将在当前操作到达安全停止点或正常完成后关闭。");
                progress.cancellable = false;
                page.update(progress);
            }
        }
        self.show_information(
            hwnd,
            crate::tr!("正在安全结束操作"),
            crate::tr!("不能在安装、备份、下载或磁盘操作进行中直接退出。程序已请求取消；无法立即中断的阶段完成后，窗口将自动关闭。"),
        );
    }

    unsafe fn create_fonts(&mut self) {
        if !self.font.is_invalid() {
            let _ = DeleteObject(self.font);
        }
        if !self.font_bold.is_invalid() {
            let _ = DeleteObject(self.font_bold);
        }
        if !self.font_brand.is_invalid() {
            let _ = DeleteObject(self.font_brand);
        }
        // Keep every native surface on the same CJK-capable UI family.  Mixing Segoe UI on the
        // main window with Microsoft YaHei UI in tool dialogs changes glyph metrics and makes the
        // migrated interface visibly jump between pages.
        let face = wide("Microsoft YaHei UI");
        self.font = CreateFontW(
            -self.scale(12),
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
        self.font_bold = CreateFontW(
            -self.scale(14),
            0,
            0,
            0,
            600,
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
        self.font_brand = CreateFontW(
            -self.scale(16),
            0,
            0,
            0,
            700,
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
    }

    unsafe fn create_children(&mut self, hwnd: HWND) -> windows::core::Result<()> {
        self.dpi = GetDpiForWindow(hwnd);
        self.create_fonts();

        let brand = child(hwnd, w!("STATIC"), "LetRecovery", SS_CENTER_STYLE, 299)?;
        let nav_labels = [
            crate::tr!("系统安装"),
            crate::tr!("系统备份"),
            crate::tr!("在线下载"),
            crate::tr!("工具箱"),
            crate::tr!("硬件信息"),
            crate::tr!("关于"),
        ];
        let nav_ids = [
            ID_NAV_INSTALL,
            ID_NAV_BACKUP,
            ID_NAV_DOWNLOAD,
            ID_NAV_TOOLS,
            ID_NAV_HARDWARE,
            ID_NAV_ABOUT,
        ];
        let mut nav = [HWND::default(); 6];
        for (index, (label, id)) in nav_labels.into_iter().zip(nav_ids).enumerate() {
            nav[index] = child(
                hwnd,
                w!("BUTTON"),
                &label,
                BS_OWNERDRAW | WS_TABSTOP.0 as i32,
                id,
            )?;
        }

        let title = child(hwnd, w!("STATIC"), &crate::tr!("系统安装"), 0, 300)?;
        let description = child(
            hwnd,
            w!("STATIC"),
            &crate::tr!("选择系统镜像、目标分区和安装选项。"),
            0,
            301,
        )?;
        let image_label = child(hwnd, w!("STATIC"), &crate::tr!("系统镜像:"), 0, 302)?;
        let image_edit = CreateWindowExW(
            // Keep the native Edit text/caret/IME, but never create a second square CLIENTEDGE
            // behind the deterministic Windows 11 field frame.
            WINDOW_EX_STYLE(0x0000_0004),
            w!("EDIT"),
            w!(""),
            WINDOW_STYLE((WS_CHILD | WS_VISIBLE | WS_TABSTOP).0 | ES_AUTOHSCROLL as u32),
            0,
            0,
            0,
            0,
            hwnd,
            HMENU(ID_IMAGE_EDIT as isize as *mut _),
            HINSTANCE::default(),
            None,
        )?;
        center_single_line_edit_in_row(image_edit);
        let browse = child(
            hwnd,
            w!("BUTTON"),
            &crate::tr!("浏览..."),
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            ID_BROWSE,
        )?;
        let image_volume_label = child(hwnd, w!("STATIC"), &crate::tr!("镜像卷:"), 0, 307)?;
        let image_volume = child(
            hwnd,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            ID_IMAGE_VOLUME,
        )?;
        let _ = ShowWindow(image_volume_label, SW_HIDE);
        let _ = ShowWindow(image_volume, SW_HIDE);
        let partitions_label = child(hwnd, w!("STATIC"), &crate::tr!("选择安装分区:"), 0, 303)?;
        let partitions = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("SysListView32"),
            w!(""),
            WINDOW_STYLE((WS_CHILD | WS_VISIBLE | WS_TABSTOP).0 | LVS_REPORT | LVS_SHOWSELALWAYS),
            0,
            0,
            0,
            0,
            hwnd,
            HMENU(ID_PARTITIONS as isize as *mut _),
            HINSTANCE::default(),
            None,
        )?;
        let _ = SendMessageW(
            partitions,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            WPARAM(0),
            LPARAM((LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as isize),
        );
        self.populate_partitions(partitions, true);

        let format = child(
            hwnd,
            w!("BUTTON"),
            &crate::tr!("格式化分区"),
            BS_AUTOCHECKBOX | WS_TABSTOP.0 as i32,
            ID_FORMAT,
        )?;
        let boot = child(
            hwnd,
            w!("BUTTON"),
            &crate::tr!("添加引导"),
            BS_AUTOCHECKBOX | WS_TABSTOP.0 as i32,
            ID_BOOT,
        )?;
        let unattend = child(
            hwnd,
            w!("BUTTON"),
            &crate::tr!("无人值守"),
            BS_AUTOCHECKBOX | WS_TABSTOP.0 as i32,
            ID_UNATTEND,
        )?;
        let unattend_browse = child(
            hwnd,
            w!("BUTTON"),
            &crate::tr!("选择无人值守文件..."),
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            ID_UNATTEND_BROWSE,
        )?;
        let unattend_clear = child(
            hwnd,
            w!("BUTTON"),
            &crate::tr!("清除"),
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            ID_UNATTEND_CLEAR,
        )?;
        let unattend_path = child(
            hwnd,
            w!("STATIC"),
            &crate::tr!("未选择则使用内置生成的无人值守配置"),
            0,
            309,
        )?;
        // SS_CENTERIMAGE keeps this inline field label on the same visual baseline as the
        // checkbox captions and the closed ComboBox in every DPI bucket.
        const SS_CENTERIMAGE_VALUE: i32 = 0x0000_0200;
        let driver_label = child(
            hwnd,
            w!("STATIC"),
            &crate::tr!("驱动:"),
            SS_CENTERIMAGE_VALUE,
            304,
        )?;
        let driver = child(
            hwnd,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            ID_DRIVER_COMBO,
        )?;
        for value in [
            crate::tr!("自动导入"),
            crate::tr!("仅导出"),
            crate::tr!("跳过"),
        ] {
            let value = wide(&value);
            let _ = SendMessageW(driver, 0x0143, WPARAM(0), LPARAM(value.as_ptr() as isize));
        }
        let driver_index = match self.app_config.install_prefs.driver_action {
            crate::core::ui_state::DriverAction::AutoImport => 0,
            crate::core::ui_state::DriverAction::SaveOnly => 1,
            crate::core::ui_state::DriverAction::None => 2,
        };
        let _ = SendMessageW(driver, 0x014E, WPARAM(driver_index), LPARAM(0));
        let reboot = child(
            hwnd,
            w!("BUTTON"),
            &crate::tr!("立即重启"),
            BS_AUTOCHECKBOX | WS_TABSTOP.0 as i32,
            ID_REBOOT,
        )?;
        let prefs = &self.app_config.install_prefs;
        for (checkbox, checked) in [
            (format, prefs.format_partition),
            (boot, prefs.repair_boot),
            (unattend, prefs.unattended_install),
            (reboot, prefs.auto_reboot),
        ] {
            let _ = SendMessageW(checkbox, 0x00F1, WPARAM(usize::from(checked)), LPARAM(0));
        }
        let boot_label = child(
            hwnd,
            w!("STATIC"),
            &crate::tr!("引导模式:"),
            SS_CENTERIMAGE_VALUE,
            305,
        )?;
        let boot_mode = child(
            hwnd,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            ID_BOOT_COMBO,
        )?;
        for value in [crate::tr!("自动"), "UEFI".to_owned(), "Legacy".to_owned()] {
            let value = wide(&value);
            let _ = SendMessageW(
                boot_mode,
                0x0143,
                WPARAM(0),
                LPARAM(value.as_ptr() as isize),
            );
        }
        let boot_index = match prefs.boot_mode {
            crate::core::ui_state::BootModeSelection::Auto => 0,
            crate::core::ui_state::BootModeSelection::UEFI => 1,
            crate::core::ui_state::BootModeSelection::Legacy => 2,
        };
        let _ = SendMessageW(boot_mode, 0x014E, WPARAM(boot_index), LPARAM(0));
        let pca_label = child(hwnd, w!("STATIC"), &crate::tr!("启动签名:"), 0, 310)?;
        let pca_mode = child(
            hwnd,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            ID_PCA_MODE,
        )?;
        for value in [
            crate::tr!("自动（PCA2011）"),
            "PCA2011".to_owned(),
            "PCA2023".to_owned(),
        ] {
            let value = wide(&value);
            let _ = SendMessageW(pca_mode, 0x0143, WPARAM(0), LPARAM(value.as_ptr() as isize));
        }
        let pca_index = match prefs.boot_pca_mode {
            lr_core::boot_pca::BootPcaMode::Auto => 0,
            lr_core::boot_pca::BootPcaMode::Pca2011 => 1,
            lr_core::boot_pca::BootPcaMode::Pca2023 => 2,
        };
        let _ = SendMessageW(pca_mode, 0x014E, WPARAM(pca_index), LPARAM(0));
        let _ = ShowWindow(pca_label, SW_HIDE);
        let _ = ShowWindow(pca_mode, SW_HIDE);
        let run_diskpart = child(
            hwnd,
            w!("BUTTON"),
            &crate::tr!("运行Diskpart脚本"),
            BS_AUTOCHECKBOX | WS_TABSTOP.0 as i32,
            ID_RUN_DISKPART,
        )?;
        let _ = SendMessageW(
            run_diskpart,
            0x00F1,
            WPARAM(usize::from(prefs.run_diskpart_scripts)),
            LPARAM(0),
        );
        let open_diskpart_dir = child(
            hwnd,
            w!("BUTTON"),
            &crate::tr!("打开目录"),
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            ID_OPEN_DISKPART_DIR,
        )?;
        let edit_boot_commands = child(
            hwnd,
            w!("BUTTON"),
            &crate::tr!("修改引导命令"),
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            ID_EDIT_BOOT_COMMANDS,
        )?;
        for control in [run_diskpart, open_diskpart_dir, edit_boot_commands] {
            let _ = ShowWindow(control, SW_HIDE);
        }
        let pe_label = child(hwnd, w!("STATIC"), &crate::tr!("PE 环境:"), 0, 308)?;
        let pe = child(
            hwnd,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            ID_INSTALL_PE,
        )?;
        self.populate_install_pe_combo(pe, None);
        let _ = ShowWindow(pe_label, SW_HIDE);
        let _ = ShowWindow(pe, SW_HIDE);
        let advanced = child(
            hwnd,
            w!("BUTTON"),
            &crate::tr!("高级选项..."),
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            ID_ADVANCED,
        )?;
        let refresh = child(
            hwnd,
            w!("BUTTON"),
            &crate::tr!("刷新分区"),
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            ID_REFRESH,
        )?;
        let status = child(
            hwnd,
            w!("STATIC"),
            &crate::tr!("启动模式: 检测中 | TPM: 检测中 | 安全启动: 检测中"),
            0,
            306,
        )?;
        let primary = child(
            hwnd,
            w!("BUTTON"),
            &crate::tr!("开始安装"),
            BS_OWNERDRAW | WS_TABSTOP.0 as i32,
            ID_PRIMARY,
        )?;
        let _ = EnableWindow(primary, false);

        let handles = Handles {
            brand,
            nav,
            title,
            description,
            image_label,
            image_edit,
            browse,
            image_volume_label,
            image_volume,
            partitions_label,
            partitions,
            format,
            boot,
            unattend,
            unattend_browse,
            unattend_clear,
            unattend_path,
            driver_label,
            driver,
            reboot,
            boot_label,
            boot_mode,
            pca_label,
            pca_mode,
            run_diskpart,
            open_diskpart_dir,
            edit_boot_commands,
            pe_label,
            pe,
            advanced,
            refresh,
            status,
            primary,
        };
        self.handles = Some(handles);
        #[cfg(feature = "non-elevated-tests")]
        if std::env::var_os("LETRECOVERY_UI_TEST_IMAGE_VOLUME").is_some() {
            self.image_volumes = vec![crate::core::dism::ImageInfo {
                index: 1,
                name: "Windows 11 Professional (UI fixture)".to_owned(),
                size_bytes: 8 * 1024 * 1024 * 1024,
                installation_type: "Client".to_owned(),
                major_version: Some(10),
                minor_version: Some(0),
                build: Some(26_100),
                architecture: Some(9),
                image_type: lr_core::image_meta::WimImageType::StandardInstall,
                verified_installable: true,
            }];
            self.effective_image_path = Some(r"C:\UI-Fixture\sources\install.wim".to_owned());
            self.install_volume_row_presented = true;
            set_text(image_edit, r"C:\UI-Fixture\sources\install.wim");
            let label = wide("1. Windows 11 Professional (UI fixture)");
            let _ = SendMessageW(
                image_volume,
                0x0143, // CB_ADDSTRING
                WPARAM(0),
                LPARAM(label.as_ptr() as isize),
            );
            let _ = SendMessageW(image_volume, 0x014E, WPARAM(0), LPARAM(0)); // CB_SETCURSEL
            let _ = ShowWindow(image_volume_label, SW_SHOW);
            let _ = ShowWindow(image_volume, SW_SHOW);
            for (row, values) in [
                [
                    "C: (当前系统)",
                    "299.0 GB",
                    "48.5 GB",
                    "OS",
                    "GPT",
                    "未加密",
                    "已有系统",
                ],
                ["D:", "200.0 GB", "30.3 GB", "", "GPT", "未加密", "空闲"],
                ["E:", "428.5 GB", "110.3 GB", "", "GPT", "未加密", "空闲"],
            ]
            .into_iter()
            .enumerate()
            {
                for (column, value) in values.into_iter().enumerate() {
                    let mut value = wide(value);
                    let mut item = LVITEMW {
                        mask: LVIF_TEXT,
                        iItem: row as i32,
                        iSubItem: column as i32,
                        pszText: windows::core::PWSTR(value.as_mut_ptr()),
                        ..Default::default()
                    };
                    let message = if column == 0 { LVM_INSERTITEMW } else { 0x104c };
                    let _ = SendMessageW(
                        partitions,
                        message,
                        WPARAM(0),
                        LPARAM((&mut item as *mut LVITEMW) as isize),
                    );
                }
            }
            let mut selected = LVITEMW {
                stateMask: LVIS_SELECTED,
                state: LVIS_SELECTED,
                iItem: 0,
                ..Default::default()
            };
            let _ = SendMessageW(
                partitions,
                0x102b,
                WPARAM(0),
                LPARAM((&mut selected as *mut LVITEMW) as isize),
            );
        }
        self.update_pca_combo_labels();
        self.create_secondary_pages(hwnd)?;
        // The firmware probe already started alongside process preloading. Attach its receiver
        // before the initial page transaction so a preloaded install intent can never become
        // briefly actionable while PCA compatibility is still unknown.
        self.request_pca_firmware_detection(hwnd);
        // Child HWNDs are created visible by default, while easy mode intentionally hides the
        // ordinary Install page and its shared command bar. Reconcile the initial route before
        // the top-level window is ever shown; merely laying out an invisible command at the right
        // edge leaves a still-visible HWND clipped to a narrow rectangle on small displays.
        self.select_page_impl(hwnd, Page::Install, false);
        // Keep the first visible status useful: startup PCA work remains silent until a selected
        // image and target make it relevant, while the boot/TPM/Secure Boot summary is immediate.
        self.update_system_status();
        self.apply_native_dark_theme(hwnd);
        self.apply_fonts();
        self.layout(hwnd);
        Ok(())
    }

    fn request_pca_firmware_detection(&mut self, hwnd: HWND) {
        #[cfg(feature = "non-elevated-tests")]
        {
            let _ = hwnd;
            self.pca_detection_pending = false;
        }
        #[cfg(not(feature = "non-elevated-tests"))]
        {
            self.pca_detection_pending = true;
            let startup_receiver = self
                .config
                .pca_firmware_receiver
                .lock()
                .ok()
                .and_then(|mut receiver| receiver.take());
            let window = hwnd.0 as usize;
            std::thread::spawn(move || {
                let result = startup_receiver
                    .and_then(|receiver| receiver.recv().ok())
                    .unwrap_or_else(lr_core::boot_pca::inspect_firmware_pca);
                let payload = Box::into_raw(Box::new(result));
                unsafe {
                    if PostMessageW(
                        HWND(window as *mut _),
                        WM_PCA_FIRMWARE_READY,
                        WPARAM(0),
                        LPARAM(payload as isize),
                    )
                    .is_err()
                    {
                        drop(Box::from_raw(payload));
                    }
                }
            });
        }
    }

    fn clear_pca_target_detection(&mut self) {
        if self.pca_target_key.is_some()
            || self.pca_target_detection_pending
            || self.pca_target_detection_error.is_some()
        {
            self.pca_target_generation = self.pca_target_generation.wrapping_add(1);
        }
        self.pca_target_key = None;
        self.pca_target_detection_pending = false;
        self.pca_target_detection_error = None;
    }

    unsafe fn pca_target_context(&self) -> Option<(PcaTargetKey, PcaTargetContext)> {
        let target = self.selected_install_target()?;
        Some((
            PcaTargetKey {
                partition: target.partition,
                disk_number: target.disk_number,
                partition_number: target.partition_number,
            },
            PcaTargetContext {
                repair_boot: self.app_config.install_prefs.repair_boot,
                boot_mode: self.app_config.install_prefs.boot_mode,
                partition_style: target.style,
                image_supports_pca: self.selected_image_supports_pca(),
                advanced_options_enabled: self.app_config.enable_advanced_options,
                run_diskpart_scripts: self.app_config.install_prefs.run_diskpart_scripts,
            },
        ))
    }

    unsafe fn request_pca_target_detection(&mut self, hwnd: HWND) {
        let Some((target, context)) = self.pca_target_context() else {
            self.clear_pca_target_detection();
            return;
        };
        if !pca_target_probe_required(context) {
            self.clear_pca_target_detection();
            return;
        }
        if self.pca_target_key.as_ref() == Some(&target) {
            return;
        }

        self.pca_target_generation = self.pca_target_generation.wrapping_add(1);
        self.pca_target_key = Some(target.clone());
        self.pca_target_detection_error = None;

        #[cfg(feature = "non-elevated-tests")]
        {
            let _ = hwnd;
            self.pca_target_detection_pending = false;
        }
        #[cfg(not(feature = "non-elevated-tests"))]
        {
            self.pca_target_detection_pending = true;
            if let Some(handles) = self.handles {
                set_text(
                    handles.status,
                    &crate::tr!("正在检测目标磁盘的 EFI 引导签名..."),
                );
            }
            let generation = self.pca_target_generation;
            let partition = target.partition.clone();
            let window = hwnd.0 as usize;
            std::thread::spawn(move || {
                let result = crate::core::bcdedit::BootManager::new()
                    .inspect_existing_esp_pca(&partition)
                    .map(|_| ())
                    .map_err(|error| error.to_string());
                let payload = Box::into_raw(Box::new(PcaTargetMessage {
                    generation,
                    target,
                    result,
                }));
                unsafe {
                    if PostMessageW(
                        HWND(window as *mut _),
                        WM_PCA_TARGET_READY,
                        WPARAM(0),
                        LPARAM(payload as isize),
                    )
                    .is_err()
                    {
                        drop(Box::from_raw(payload));
                    }
                }
            });
        }
    }

    unsafe fn update_pca_detection_status(&self) {
        if !may_publish_install_chrome(self.page, self.advanced_visible, self.progress_visible) {
            return;
        }
        let selection_is_relevant = self.pca_selection_is_relevant();
        if !selection_is_relevant {
            return;
        }
        let Some(handles) = self.handles else { return };
        if let Some(pending) = pca_pending_status(
            selection_is_relevant,
            self.pca_detection_pending,
            self.pca_target_detection_pending,
        ) {
            let text = match pending {
                PcaPendingStatus::FirmwareCompatibility => {
                    crate::tr!("正在检测 PCA 兼容性，请稍候。")
                }
                PcaPendingStatus::TargetEfiSignature => {
                    crate::tr!("正在检测目标磁盘的 EFI 引导签名...")
                }
            };
            set_text(handles.status, &text);
        } else if let Some(error) = self.pca_target_detection_error.as_ref() {
            let diskpart_may_create_esp = self.pca_target_context().is_some_and(|(_, context)| {
                !pca_target_error_blocks(context, self.pca_firmware.is_some(), true)
            });
            if diskpart_may_create_esp {
                set_text(
                    handles.status,
                    &crate::tr!("当前未检测到同盘 ESP；Diskpart 脚本必须在安装前创建 ESP。"),
                );
            } else {
                set_text(
                    handles.status,
                    &crate::tr!("目标系统所在磁盘没有可用的 ESP: {}", error),
                );
            }
        } else if let Some(error) = self.pca_selection_error() {
            set_text(handles.status, &error);
        } else {
            set_text(
                handles.status,
                &crate::tr!("目标磁盘 EFI 引导签名检测完成。"),
            );
        }
    }

    unsafe fn create_secondary_pages(&mut self, hwnd: HWND) -> windows::core::Result<()> {
        let backup_rows: Vec<_> = self
            .partitions
            .iter()
            .map(|partition| BackupPartitionRow {
                volume: partition.letter.clone(),
                total_size: format!("{:.1} GB", partition.total_size_mb as f64 / 1024.0),
                used_size: format!(
                    "{:.1} GB",
                    partition
                        .total_size_mb
                        .saturating_sub(partition.free_size_mb) as f64
                        / 1024.0
                ),
                label: partition.label.clone(),
                bitlocker: localized_bitlocker_status(&partition.bitlocker_status),
                status: if partition.has_windows {
                    crate::tr!("已有系统")
                } else {
                    crate::tr!("空闲")
                },
                has_windows: partition.has_windows,
                is_system_partition: partition.is_system_partition,
            })
            .collect();
        let backup_timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
        let (backup_name, backup_description) = localized_backup_defaults(&backup_timestamp);
        let backup_initial = BackupPageState {
            name: backup_name,
            description: backup_description,
            ..BackupPageState::default()
        };
        let backup_pe_labels: Vec<String> = self
            .available_pe()
            .into_iter()
            .map(|pe| pe.display_name)
            .collect();
        let backup = BackupPage::create(
            hwnd,
            &backup_rows,
            &backup_pe_labels,
            &backup_initial,
            &backup_timestamp,
        )?;
        backup.apply_font(self.font);
        backup.apply_theme(self.palette);
        self.backup_page = Some(backup);

        let advanced = AdvancedPage::create(
            hwnd,
            &self.app_config.install_prefs.advanced_options,
            AdvancedPageContext {
                unattended_enabled: self.app_config.install_prefs.unattended_install,
                ..AdvancedPageContext::default()
            },
        )?;
        advanced.apply_font(self.font, self.font_bold);
        advanced.apply_theme(self.palette);
        advanced.show(false);
        self.advanced_page = Some(advanced);

        let download = DownloadPage::create(
            hwnd,
            self.font,
            &DownloadLabels {
                system_tab: &crate::tr!("系统镜像"),
                software_tab: &crate::tr!("常用软件"),
                gpu_driver_tab: &crate::tr!("显卡驱动"),
                status_ready: &self.initial_download_status(),
                name_column: &crate::tr!("名称"),
                type_column: &crate::tr!("类型"),
                size_column: &crate::tr!("大小"),
                save_path: &crate::tr!("保存位置:"),
                browse: &crate::tr!("浏览..."),
                refresh: &crate::tr!("刷新"),
                download: &crate::tr!("下载"),
                install: &crate::tr!("安装"),
            },
        )?;
        download.apply_theme(self.palette);
        download.replace_rows(&self.download_controller.rows());
        let default_download_path = crate::utils::path::get_exe_dir().join("downloads");
        set_text(download.save_path, &default_download_path.to_string_lossy());
        self.download_page = Some(download);

        let mut easy = EasyModePage::create(
            hwnd,
            self.font,
            &EasyModeLabels {
                enabled: &crate::tr!("启用小白模式"),
                settings_tip: &crate::tr!("可在“关于”页面随时关闭小白模式。"),
                dismiss_tip: &crate::tr!("不再提示"),
                system: &crate::tr!("选择系统:"),
                volume: &crate::tr!("选择版本:"),
                loading: &crate::tr!("正在加载系统列表..."),
                install: &crate::tr!("一键安装"),
            },
        )?;
        easy.update(&self.easy_controller.view());
        // `EasyModePage::update` refreshes conditional children such as the settings tip.
        // Keep the page hidden until `select_page` has made the final page-visibility decision;
        // otherwise an asynchronous catalogue refresh can place those children over the normal
        // install controls because both pages share the main window as their parent.
        easy.show(false);
        easy.apply_theme(self.palette);
        self.easy_page = Some(easy);

        let tools = ToolsPage::create(
            hwnd,
            self.font,
            &ToolLabels {
                introduction: &crate::tr!("选择要运行的系统维护、修复或诊断工具。"),
                buttons: [
                    &crate::tr!("卸载 NVIDIA 驱动"),
                    &crate::tr!("分区对拷"),
                    &crate::tr!("批量格式化"),
                    &crate::tr!("导入存储驱动"),
                    &crate::tr!("一键分区"),
                    &crate::tr!("移除 APPX"),
                    &crate::tr!("驱动备份与恢复"),
                    &crate::tr!("修复系统引导"),
                    &crate::tr!("网络信息"),
                    &crate::tr!("软件列表"),
                    &crate::tr!("时间同步"),
                    &crate::tr!("运行 Ghost"),
                    &crate::tr!("查看 GHO 密码"),
                    &crate::tr!("重置网络"),
                    &crate::tr!("磁盘空间分析"),
                    &crate::tr!("校验系统镜像"),
                    &crate::tr!("管理 BitLocker"),
                    &crate::tr!("文件哈希校验"),
                    &crate::tr!("重置系统密码"),
                ],
            },
        )?;
        let is_pe_environment = self
            .config
            .system_info
            .as_ref()
            .is_some_and(|info| info.is_pe_environment);
        tools.apply_environment(is_pe_environment);
        self.tools_page = Some(tools);

        let hardware = HardwareInfoPage::create(
            hwnd,
            self.font,
            &HardwareLabels {
                introduction: &crate::tr!("当前计算机的系统和硬件摘要。"),
                loading: &crate::tr!("启动时未能读取硬件信息。请重新启动程序后重试。"),
                save: &crate::tr!("保存..."),
            },
        )?;
        if let Some(info) = &self.config.hardware_info {
            hardware.set_rows(hardware_info_rows(info, self.config.system_info.as_ref()));
        }
        hardware.apply_theme(self.palette);
        self.hardware_page = Some(hardware);

        let about_product_name = crate::build_info::product_name();
        let about_version = crate::build_info::display_version();
        let about_description = crate::build_info::description();
        let about = AboutPage::create(
            hwnd,
            self.font,
            self.font_bold,
            &AboutLabels {
                product_name: &about_product_name,
                version_label: &crate::tr!("版本:"),
                version: &about_version,
                description: &about_description,
                link_labels: [
                    &crate::tr!("项目主页"),
                    &crate::tr!("问题反馈"),
                    &crate::tr!("开源许可"),
                ],
                easy_mode: &crate::tr!("启用小白模式"),
                easy_mode_enabled: self.easy_mode_enabled(),
                easy_mode_available: !self.is_pe_environment,
                log_enabled: self.app_config.log_enabled,
                wim_engine: self.app_config.wim_engine,
                download_threads: self.app_config.download_threads,
                advanced_options_enabled: self.app_config.enable_advanced_options,
            },
        )?;
        about.apply_theme(self.palette);
        self.about_page = Some(about);

        let progress = ProgressPage::create(hwnd, LongTaskProgress::default())?;
        progress.apply_font(self.font, self.font_bold);
        progress.apply_theme(self.palette);
        progress.show(false);
        self.progress_page = Some(progress);
        Ok(())
    }

    fn initial_download_status(&self) -> String {
        #[cfg(feature = "non-elevated-tests")]
        if self.config.remote_config.is_none() {
            return crate::tr!("开发预览构建不会发起网络请求，在线资源目录未加载。");
        }
        match self.config.remote_config.as_ref() {
            Some(remote) if !remote.loaded => remote
                .error
                .clone()
                .unwrap_or_else(|| crate::tr!("在线资源目录加载失败。")),
            Some(_) if self.download_controller.rows().is_empty() => {
                crate::tr!("服务器未返回此分类的可用资源。")
            }
            Some(_) => crate::tr!("选择要下载的资源。"),
            None => crate::tr!("在线资源目录加载超时，请点击“刷新”重试。"),
        }
    }

    unsafe fn apply_native_dark_theme(&mut self, hwnd: HWND) {
        let enabled: i32 = i32::from(self.palette.dark);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&enabled as *const i32).cast(),
            size_of::<i32>() as u32,
        );

        let control_palette = self.control_palette();
        self.brushes = Brushes::new(control_palette);

        let Some(h) = self.handles else { return };
        let control_theme = if self.palette.dark {
            w!("DarkMode_Explorer")
        } else {
            w!("Explorer")
        };
        for control in h.nav.iter().copied().chain([
            h.browse,
            h.format,
            h.boot,
            h.unattend,
            h.unattend_browse,
            h.unattend_clear,
            h.reboot,
            h.run_diskpart,
            h.open_diskpart_dir,
            h.edit_boot_commands,
            h.advanced,
            h.refresh,
            h.primary,
        ]) {
            let _ = SetWindowTheme(control, control_theme, PCWSTR::null());
        }
        // The main installation check boxes are created directly by this window rather than by a
        // page object with its own `apply_theme` method.  Merely assigning Explorer/DarkExplorer
        // leaves Windows 10 drawing a light glyph and black caption in dark mode.  Route these
        // controls through the shared deterministic checkbox renderer just like the backup and
        // easy-mode pages do.  Reapplying this on a system-theme change also refreshes the
        // subclass palette reference without changing USER32's checkbox behaviour.
        for checkbox in [h.format, h.boot, h.unattend, h.reboot, h.run_diskpart] {
            theme::apply_control_theme(
                checkbox,
                control_palette,
                theme::NativeControlKind::General,
            );
        }
        for field in [
            h.image_edit,
            h.image_volume,
            h.driver,
            h.boot_mode,
            h.pca_mode,
            h.pe,
        ] {
            theme::apply_control_theme(field, control_palette, theme::NativeControlKind::Field);
        }

        for list in [
            Some(h.partitions),
            self.backup_page
                .as_ref()
                .map(|page| page.handles().source_list),
            self.download_page.as_ref().map(|page| page.resources),
            self.hardware_page.as_ref().map(|page| page.report),
        ]
        .into_iter()
        .flatten()
        {
            let _ = theme::apply_list_view_theme(list, control_palette);
        }

        // ListView does not consistently inherit the dark client colors before Windows 11.
        // Explicit colors keep the control readable while retaining native header/selection drawing.
        let _ = SendMessageW(
            h.partitions,
            0x1001,
            WPARAM(0),
            LPARAM(control_palette.edit.0 as isize),
        );
        let _ = SendMessageW(
            h.partitions,
            0x1026,
            WPARAM(0),
            LPARAM(control_palette.edit.0 as isize),
        );
        let _ = SendMessageW(
            h.partitions,
            0x1024,
            WPARAM(0),
            LPARAM(control_palette.text.0 as isize),
        );
        if let Some(page) = &self.backup_page {
            page.apply_theme(control_palette);
        }
        if let Some(page) = &self.advanced_page {
            page.apply_theme(control_palette);
        }
        if let Some(page) = &self.download_page {
            page.apply_theme(control_palette);
        }
        if let Some(page) = &self.progress_page {
            page.apply_theme(control_palette);
        }
        if let Some(page) = &self.easy_page {
            page.apply_theme(control_palette);
        }
        if let Some(page) = &self.hardware_page {
            page.apply_theme(control_palette);
        }
        if let Some(page) = &self.about_page {
            page.apply_theme(control_palette);
        }
    }

    fn control_palette(&self) -> theme::Palette {
        self.palette
    }

    unsafe fn refresh_system_theme(&mut self, hwnd: HWND) {
        let palette = theme::Palette::system();
        // WM_THEMECHANGED invalidates cached UxTheme handles even when the light/dark bit did not
        // change.  Reapply the complete control tree every time, but keep the visible transition
        // atomic so pages and their scrollbars cannot expose a mixture of old and new colours.
        let redraw = redraw::suspend(hwnd);
        self.palette = palette;
        self.apply_native_dark_theme(hwnd);
        redraw::resume(hwnd, redraw);
    }

    unsafe fn populate_partitions(&self, list: HWND, add_columns: bool) {
        if add_columns {
            let long_state_labels = crate::tr!("未加密").chars().count() > 6;
            for (index, (title, width)) in [
                (crate::tr!("分区卷"), 130),
                (crate::tr!("总空间"), 88),
                (crate::tr!("可用空间"), 88),
                (crate::tr!("卷标"), 90),
                (crate::tr!("分区表"), 76),
                (
                    "BitLocker".to_owned(),
                    if long_state_labels { 120 } else { 92 },
                ),
                (crate::tr!("状态"), if long_state_labels { 148 } else { 80 }),
            ]
            .into_iter()
            .enumerate()
            {
                let mut text = wide(&title);
                let mut column = LVCOLUMNW {
                    mask: LVCF_FMT | LVCF_TEXT | LVCF_WIDTH,
                    fmt: LVCOLUMNW_FORMAT(HDF_OWNERDRAW.0),
                    cx: self.scale(width),
                    pszText: windows::core::PWSTR(text.as_mut_ptr()),
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
        for (row, partition) in self.partitions.iter().enumerate() {
            let status = if partition.has_windows {
                crate::tr!("已有系统")
            } else {
                crate::tr!("空闲")
            };
            let first = if partition.is_system_partition {
                crate::tr!("{} (当前系统)", partition.letter)
            } else {
                partition.letter.clone()
            };
            let values = [
                first,
                format!("{:.1} GB", partition.total_size_mb as f64 / 1024.0),
                format!("{:.1} GB", partition.free_size_mb as f64 / 1024.0),
                partition.label.clone(),
                partition.partition_style.to_string(),
                localized_bitlocker_status(&partition.bitlocker_status),
                status,
            ];
            for (column, value) in values.into_iter().enumerate() {
                let mut value = wide(value);
                let mut item = LVITEMW {
                    mask: LVIF_TEXT,
                    iItem: row as i32,
                    iSubItem: column as i32,
                    pszText: windows::core::PWSTR(value.as_mut_ptr()),
                    ..Default::default()
                };
                let message = if column == 0 { LVM_INSERTITEMW } else { 0x104C };
                let _ = SendMessageW(
                    list,
                    message,
                    WPARAM(0),
                    LPARAM((&mut item as *mut LVITEMW) as isize),
                );
            }
            if partition.is_system_partition {
                let mut item = LVITEMW {
                    stateMask: LVIS_SELECTED,
                    state: LVIS_SELECTED,
                    iItem: row as i32,
                    ..Default::default()
                };
                let _ = SendMessageW(
                    list,
                    0x102B,
                    WPARAM(row),
                    LPARAM((&mut item as *mut LVITEMW) as isize),
                );
            }
        }
    }

    unsafe fn apply_fonts(&self) {
        if let Some(h) = &self.handles {
            for hwnd in h.nav.iter().copied().chain([
                h.description,
                h.image_label,
                h.image_edit,
                h.browse,
                h.image_volume_label,
                h.image_volume,
                h.partitions_label,
                h.partitions,
                h.format,
                h.boot,
                h.unattend,
                h.unattend_browse,
                h.unattend_clear,
                h.unattend_path,
                h.driver_label,
                h.driver,
                h.reboot,
                h.boot_label,
                h.boot_mode,
                h.pca_label,
                h.pca_mode,
                h.run_diskpart,
                h.open_diskpart_dir,
                h.edit_boot_commands,
                h.pe_label,
                h.pe,
                h.advanced,
                h.refresh,
                h.status,
                h.primary,
            ]) {
                let _ = SendMessageW(hwnd, WM_SETFONT, WPARAM(self.font.0 as usize), LPARAM(1));
            }
            let _ = SendMessageW(
                h.title,
                WM_SETFONT,
                WPARAM(self.font_bold.0 as usize),
                LPARAM(1),
            );
            let _ = SendMessageW(
                h.brand,
                WM_SETFONT,
                WPARAM(self.font_brand.0 as usize),
                LPARAM(1),
            );
        }
        if let Some(page) = &self.backup_page {
            page.apply_font(self.font);
        }
        if let Some(page) = &self.download_page {
            page.apply_font(self.font);
        }
        if let Some(page) = &self.easy_page {
            page.apply_font(self.font);
        }
        if let Some(page) = &self.tools_page {
            page.apply_font(self.font);
        }
        if let Some(page) = &self.hardware_page {
            page.apply_font(self.font);
        }
        if let Some(page) = &self.about_page {
            page.apply_font(self.font, self.font_bold);
        }
        if let Some(page) = &self.advanced_page {
            page.apply_font(self.font, self.font_bold);
        }
        if let Some(page) = &self.progress_page {
            page.apply_font(self.font, self.font_bold);
        }
    }

    unsafe fn layout(&self, hwnd: HWND) {
        let Some(h) = self.handles else { return };
        let mut rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut rect);
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        let nav = self.scale(NAV_WIDTH);
        let header = self.scale(HEADER_HEIGHT);
        let command = self.scale(COMMAND_HEIGHT);
        let margin = self.scale(24);
        let content_left = if self.progress_visible {
            margin
        } else {
            nav + margin
        };
        let content_right = width - margin;
        let content_width = (content_right - content_left).max(0);
        let footer_y = height - command;

        let _ = MoveWindow(
            h.brand,
            self.scale(10),
            self.scale(14),
            nav - self.scale(20),
            self.scale(26),
            true,
        );
        for (i, item) in h.nav.iter().enumerate() {
            let _ = MoveWindow(
                *item,
                self.scale(10),
                self.scale(58 + i as i32 * 34),
                nav - self.scale(20),
                self.scale(28),
                true,
            );
        }
        let _ = MoveWindow(
            h.title,
            content_left,
            self.scale(16),
            (content_width - self.scale(68)).max(0),
            self.scale(22),
            true,
        );
        let _ = MoveWindow(
            h.description,
            content_left + self.scale(16),
            self.scale(42),
            (content_width - self.scale(90)).max(0),
            self.scale(20),
            true,
        );
        let y = header + self.scale(14);
        let metrics = LayoutMetrics::for_dpi(self.dpi);
        let compact_chinese = self
            .app_config
            .language
            .to_ascii_lowercase()
            .starts_with("zh");
        let label_width = self.scale(if compact_chinese { 68 } else { 108 });
        let browse_width = self.scale(80);
        let image_row_height = metrics.field_height.max(self.scale(24));
        let _ = MoveWindow(
            h.image_label,
            content_left,
            centered_control_y_ceil(y, image_row_height, metrics.label_height),
            label_width,
            metrics.label_height,
            true,
        );
        let _ = MoveWindow(
            h.image_edit,
            content_left + label_width,
            centered_control_y_ceil(y, image_row_height, metrics.field_height),
            (content_width - label_width - browse_width - self.scale(10)).max(0),
            metrics.field_height,
            true,
        );
        let _ = MoveWindow(
            h.browse,
            content_right - browse_width,
            centered_control_y_ceil(y, image_row_height, self.scale(24)),
            browse_width,
            self.scale(24),
            true,
        );
        let volume_y = y + self.scale(32);
        let volume_closed_height = theme::combo_closed_height(h.image_volume, metrics.field_height);
        let volume_row_height = volume_closed_height.max(metrics.label_height);
        let _ = MoveWindow(
            h.image_volume_label,
            content_left,
            centered_control_y_ceil(volume_y, volume_row_height, metrics.label_height),
            label_width,
            metrics.label_height,
            true,
        );
        let _ = MoveWindow(
            h.image_volume,
            content_left + label_width,
            centered_control_y_ceil(volume_y, volume_row_height, volume_closed_height),
            (content_width - label_width).clamp(0, self.scale(420)),
            self.scale(180),
            true,
        );
        let image_volume_layout_active = self.page == Page::Install
            && !self.easy_mode_enabled()
            && !self.advanced_visible
            && !self.progress_visible;
        let volume_row_expansion = if image_volume_layout_active {
            self.install_volume_layout_transition
                .map(InstallVolumeLayoutTransition::expansion)
                .unwrap_or(if self.install_volume_row_presented {
                    34
                } else {
                    0
                })
        } else {
            0
        };
        let table_label_y = install_partition_heading_y(y, self.dpi, volume_row_expansion);
        let _ = MoveWindow(
            h.partitions_label,
            content_left,
            table_label_y,
            self.scale(160),
            self.scale(22),
            true,
        );
        let table_y = table_label_y + self.scale(26);
        let option_rows = 6;
        let reserved_below_table = self.scale(22 + option_rows * 34);
        let table_height = self
            .scale(140)
            .min((footer_y - table_y - reserved_below_table).max(0));
        let _ = MoveWindow(
            h.partitions,
            content_left,
            table_y,
            content_width,
            table_height,
            true,
        );
        let options_y = table_y + table_height + self.scale(12);
        let second_y = options_y + self.scale(34);
        let check_width = |control: HWND| {
            measure_text(hwnd, self.font, &get_text(control), None).width + self.scale(26)
        };
        let format_width = check_width(h.format);
        let boot_width = check_width(h.boot);
        let unattended_width = check_width(h.unattend);
        let reboot_width = check_width(h.reboot).max(self.scale(72));
        let driver_label_width =
            measure_text(hwnd, self.font, &get_text(h.driver_label), None).width + self.scale(2);
        let driver_width = self.scale(116);
        let required_option_width = format_width
            + boot_width
            + unattended_width
            + reboot_width
            + driver_label_width
            + driver_width
            + metrics.control_gap * 5
            + metrics.tight_gap;
        let very_compact_options = content_width < required_option_width;
        let driver_closed_height = theme::combo_closed_height(h.driver, metrics.field_height);
        let option_row_height = driver_closed_height.max(self.scale(24));
        let check_y = centered_control_y_ceil(options_y, option_row_height, self.scale(24));
        let _ = MoveWindow(
            h.format,
            content_left,
            check_y,
            format_width,
            self.scale(24),
            true,
        );
        let boot_x = content_left + format_width + metrics.control_gap;
        let _ = MoveWindow(h.boot, boot_x, check_y, boot_width, self.scale(24), true);
        let unattended_x = boot_x + boot_width + metrics.control_gap;
        let _ = MoveWindow(
            h.unattend,
            unattended_x,
            check_y,
            unattended_width,
            self.scale(24),
            true,
        );
        let driver_x = if very_compact_options {
            content_left
        } else {
            unattended_x + unattended_width + metrics.control_gap
        };
        let driver_y = if very_compact_options {
            second_y + self.scale(34)
        } else {
            options_y
        };
        let driver_field_x = driver_x + driver_label_width + metrics.tight_gap;
        // Checkbox controls include an 8px visual tail after their caption.  Add the same tail
        // after the driver field so the field-to-Restart glyph distance matches the preceding
        // checkbox-to-checkbox rhythm instead of appearing cramped in a wide window.
        let reboot_gap = metrics.control_gap + self.scale(8);
        let driver_width = if very_compact_options {
            (content_width - driver_label_width - metrics.tight_gap).max(0)
        } else {
            driver_width.min((content_right - driver_field_x - reboot_gap - reboot_width).max(0))
        };
        let reboot_x = if very_compact_options {
            content_right - reboot_width
        } else {
            driver_field_x + driver_width + reboot_gap
        };
        let _ = MoveWindow(
            h.driver_label,
            driver_x,
            centered_control_y_ceil(driver_y, option_row_height, metrics.label_height),
            driver_label_width,
            metrics.label_height,
            true,
        );
        let _ = MoveWindow(
            h.driver,
            driver_field_x,
            centered_control_y_ceil(driver_y, option_row_height, driver_closed_height),
            driver_width,
            self.scale(180),
            true,
        );
        let _ = MoveWindow(
            h.reboot,
            reboot_x,
            check_y,
            reboot_width,
            self.scale(24),
            true,
        );
        let boot_label_width = self.scale(if compact_chinese { 60 } else { 76 });
        let boot_mode_closed_height = theme::combo_closed_height(h.boot_mode, metrics.field_height);
        let second_row_height = boot_mode_closed_height.max(self.scale(24));
        let _ = MoveWindow(
            h.boot_label,
            content_left,
            centered_control_y_ceil(second_y, second_row_height, metrics.label_height),
            boot_label_width,
            metrics.label_height,
            true,
        );
        let boot_mode_x = content_left + boot_label_width + self.scale(4);
        let boot_mode_width = self.scale(124);
        let _ = MoveWindow(
            h.boot_mode,
            boot_mode_x,
            centered_control_y_ceil(second_y, second_row_height, boot_mode_closed_height),
            boot_mode_width,
            self.scale(180),
            true,
        );
        let unattended_enabled = SendMessageW(h.unattend, 0x00F0, WPARAM(0), LPARAM(0)).0 == 1;
        let unattend_browse_width = self.scale(if compact_chinese { 132 } else { 180 });
        let unattend_clear_width = self.scale(if compact_chinese { 58 } else { 76 });
        let unattend_x = boot_mode_x + boot_mode_width + self.scale(12);
        let _ = MoveWindow(
            h.unattend_browse,
            unattend_x,
            second_y,
            unattend_browse_width.min((content_right - unattend_x).max(0)),
            self.scale(24),
            true,
        );
        let clear_x = unattend_x + unattend_browse_width + self.scale(8);
        let has_custom_unattend = !self.custom_unattend_path.trim().is_empty();
        if has_custom_unattend {
            let _ = MoveWindow(
                h.unattend_clear,
                clear_x,
                second_y,
                unattend_clear_width.min((content_right - clear_x).max(0)),
                self.scale(24),
                true,
            );
        } else {
            // A hidden owner-drawn button must not overlap the hint. Windows can retain its last
            // composed pixels while the row is being relaid out, which looked like an unlabeled
            // button underneath the built-in unattended-config hint.
            let _ = MoveWindow(h.unattend_clear, 0, 0, 0, 0, false);
        }
        // Do not reserve room for Clear until a custom answer file actually exists.
        let inline_path_x = if !has_custom_unattend {
            clear_x
        } else {
            clear_x + unattend_clear_width + self.scale(8)
        };
        let inline_path_width = (content_right - inline_path_x).max(0);
        let path_on_own_row = inline_path_width < self.scale(260);
        let path_y = if path_on_own_row {
            if very_compact_options {
                driver_y + self.scale(34)
            } else {
                second_y + self.scale(34)
            }
        } else {
            second_y
        };
        let path_x = if path_on_own_row {
            content_left
        } else {
            inline_path_x
        };
        let _ = MoveWindow(
            h.unattend_path,
            path_x,
            path_y + self.scale(3),
            (content_right - path_x).max(0),
            self.scale(20),
            true,
        );
        let third_y = if path_on_own_row {
            path_y + self.scale(28)
        } else {
            second_y + self.scale(34)
        };
        let pe_label_width = self.scale(if compact_chinese { 60 } else { 84 });
        let pe_x = content_left;
        let pe_closed_height = theme::combo_closed_height(h.pe, metrics.field_height);
        let pe_row_height = pe_closed_height.max(metrics.label_height);
        let _ = MoveWindow(
            h.pe_label,
            pe_x,
            centered_control_y_ceil(third_y, pe_row_height, metrics.label_height),
            pe_label_width,
            metrics.label_height,
            true,
        );
        let _ = MoveWindow(
            h.pe,
            pe_x + pe_label_width + self.scale(4),
            centered_control_y_ceil(third_y, pe_row_height, pe_closed_height),
            (content_right - pe_x - pe_label_width - self.scale(4)).clamp(0, self.scale(300)),
            self.scale(180),
            true,
        );
        let pe_selector_visible = self.install_pe_selector_should_be_visible();
        let pca_y = if pe_selector_visible {
            third_y + self.scale(34)
        } else {
            third_y
        };
        let open_width = measured_button_width(
            hwnd,
            self.font,
            &crate::tr!("打开目录"),
            self.dpi,
            self.scale(75),
        );
        let edit_width = measured_button_width(
            hwnd,
            self.font,
            &crate::tr!("修改引导命令"),
            self.dpi,
            self.scale(96),
        );
        let run_width = measure_text(hwnd, self.font, &crate::tr!("运行Diskpart脚本"), None).width
            + self.scale(26);
        let advanced_inline_x = unattend_x;
        let advanced_inline_width =
            run_width + metrics.tight_gap + open_width + metrics.control_gap + edit_width;
        // The unattended file controls are conditional. Once the checkbox is cleared, repack the
        // following advanced actions into the released part of the boot-mode row instead of
        // retaining an invisible browse/hint slot. Keep a separate row when the translated labels
        // do not fit or when the PE selector already owns the next conditional layout branch.
        let advanced_follows_boot = !unattended_enabled
            && self.app_config.enable_advanced_options
            && !pe_selector_visible
            && advanced_inline_x + advanced_inline_width <= content_right;
        let advanced_row_x = if advanced_follows_boot {
            advanced_inline_x
        } else {
            content_left
        };
        let advanced_row_y = if advanced_follows_boot {
            second_y
        } else {
            pca_y
        };
        let _ = MoveWindow(
            h.run_diskpart,
            advanced_row_x,
            advanced_row_y,
            run_width,
            self.scale(24),
            true,
        );
        let _ = MoveWindow(
            h.open_diskpart_dir,
            advanced_row_x + run_width + metrics.tight_gap,
            advanced_row_y,
            open_width,
            self.scale(24),
            true,
        );
        let edit_x =
            advanced_row_x + run_width + metrics.tight_gap + open_width + metrics.control_gap;
        let _ = MoveWindow(
            h.edit_boot_commands,
            edit_x,
            advanced_row_y,
            edit_width.min((content_right - edit_x).max(0)),
            self.scale(24),
            true,
        );
        // When advanced install options are enabled, keep all advanced install actions on one
        // scan line: DiskPart, its adjacent directory action, boot-command editing, then the
        // image-dependent PCA selector at the end. When the actions are disabled, the PCA selector
        // naturally starts at the left edge instead of reserving invisible gaps.
        let pca_row_y = if advanced_follows_boot {
            third_y
        } else {
            advanced_row_y
        };
        let pca_x = if self.app_config.enable_advanced_options && !advanced_follows_boot {
            edit_x + edit_width + self.scale(16)
        } else {
            content_left
        };
        let pca_label_width = self.scale(if compact_chinese { 72 } else { 98 });
        let pca_closed_height = theme::combo_closed_height(h.pca_mode, metrics.field_height);
        let pca_row_height = pca_closed_height.max(metrics.label_height);
        let _ = MoveWindow(
            h.pca_label,
            pca_x,
            centered_control_y_ceil(pca_row_y, pca_row_height, metrics.label_height),
            pca_label_width.min((content_right - pca_x).max(0)),
            metrics.label_height,
            true,
        );
        let pca_combo_x = pca_x + pca_label_width + self.scale(4);
        let _ = MoveWindow(
            h.pca_mode,
            pca_combo_x,
            centered_control_y_ceil(pca_row_y, pca_row_height, pca_closed_height),
            self.scale(144).min((content_right - pca_combo_x).max(0)),
            self.scale(180),
            true,
        );
        let button_gap = self.scale(8);
        let preferred_button_width = self.scale(if compact_chinese { 96 } else { 136 });
        let command_button_width =
            preferred_button_width.min(((content_width - button_gap * 2) / 3).max(0));
        let command_visibility = command_bar_visibility(
            self.page,
            self.easy_mode_enabled(),
            self.advanced_visible,
            self.progress_visible,
        );
        // Pack the controls that are actually visible. In particular, Hardware has Save and
        // Copy but no Refresh; reserving the hidden middle slot left an obvious empty gap after a
        // page switch. Keeping this calculation independent of the previous page also makes a
        // relayout after localization or DPI changes deterministic.
        let command_layout = command_bar_layout(
            content_right,
            button_gap,
            command_button_width,
            command_visibility,
        );
        let advanced_x = if self.advanced_visible {
            centered_command_button_x(content_left, content_width, command_button_width)
        } else {
            command_layout.x[0].unwrap_or(content_right)
        };
        let refresh_x = command_layout.x[1].unwrap_or(content_right);
        let primary_x = command_layout.x[2].unwrap_or(content_right);
        let _ = MoveWindow(
            h.advanced,
            advanced_x,
            footer_y + self.scale(12),
            command_button_width,
            self.scale(28),
            true,
        );
        let _ = MoveWindow(
            h.refresh,
            refresh_x,
            footer_y + self.scale(12),
            command_button_width,
            self.scale(28),
            true,
        );
        let _ = MoveWindow(
            h.primary,
            primary_x,
            footer_y + self.scale(12),
            command_button_width,
            self.scale(28),
            true,
        );
        let _ = MoveWindow(
            h.status,
            self.scale(16),
            footer_y + self.scale(18),
            (command_layout.left_edge - self.scale(24)).max(0),
            self.scale(20),
            true,
        );
        let page_top = if self.progress_visible {
            margin
        } else {
            header + self.scale(14)
        };
        let page_height = if self.progress_visible {
            (height - margin * 2).max(0)
        } else {
            (footer_y - page_top - self.scale(10)).max(0)
        };
        if let Some(page) = &self.backup_page {
            page.layout(content_left, page_top, content_width, self.dpi);
        }
        let page_rect = PageRect {
            x: content_left,
            y: page_top,
            width: content_width,
            height: page_height,
        };
        if let Some(page) = &self.download_page {
            page.layout(page_rect, self.dpi);
        }
        if let Some(page) = &self.easy_page {
            page.layout(page_rect, self.dpi);
        }
        if let Some(page) = &self.tools_page {
            page.layout(page_rect, self.dpi);
        }
        if let Some(page) = &self.hardware_page {
            page.layout(page_rect, self.dpi);
        }
        if let Some(page) = &self.about_page {
            page.layout(page_rect, self.dpi);
        }
        if let Some(page) = &self.advanced_page {
            page.layout(content_left, page_top, content_width, page_height, self.dpi);
        }
        if let Some(page) = &self.progress_page {
            page.layout(content_left, page_top, content_width, page_height, self.dpi);
        }
    }

    /// Page controls are created and laid out together at startup and on every real size, DPI or
    /// language change. Navigation only changes visibility and the small command bar at the
    /// bottom; rerunning every hidden page's text measurement and dozens of MoveWindow calls here
    /// made a simple navigation click hundreds of milliseconds slower on a high-DPI display.
    unsafe fn layout_page_switch_chrome(&self, hwnd: HWND) {
        let Some(h) = self.handles else { return };
        let mut rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut rect);
        let width = (rect.right - rect.left).max(0);
        let height = (rect.bottom - rect.top).max(0);
        let nav = self.scale(NAV_WIDTH);
        let command = self.scale(COMMAND_HEIGHT);
        let margin = self.scale(24);
        let content_left = if self.progress_visible {
            margin
        } else {
            nav + margin
        };
        let content_right = width - margin;
        let content_width = (content_right - content_left).max(0);
        let footer_y = height - command;
        let compact_chinese = self
            .app_config
            .language
            .to_ascii_lowercase()
            .starts_with("zh");
        let button_gap = self.scale(8);
        let preferred_button_width = self.scale(if compact_chinese { 96 } else { 136 });
        let command_button_width =
            preferred_button_width.min(((content_width - button_gap * 2) / 3).max(0));
        let command_visibility = command_bar_visibility(
            self.page,
            self.easy_mode_enabled(),
            self.advanced_visible,
            self.progress_visible,
        );
        let command_layout = command_bar_layout(
            content_right,
            button_gap,
            command_button_width,
            command_visibility,
        );
        let advanced_x = if self.advanced_visible {
            centered_command_button_x(content_left, content_width, command_button_width)
        } else {
            command_layout.x[0].unwrap_or(content_right)
        };
        for (control, x) in [
            (h.advanced, advanced_x),
            (h.refresh, command_layout.x[1].unwrap_or(content_right)),
            (h.primary, command_layout.x[2].unwrap_or(content_right)),
        ] {
            let _ = MoveWindow(
                control,
                x,
                footer_y + self.scale(12),
                command_button_width,
                self.scale(28),
                false,
            );
        }
        let _ = MoveWindow(
            h.status,
            self.scale(16),
            footer_y + self.scale(18),
            (command_layout.left_edge - self.scale(24)).max(0),
            self.scale(20),
            false,
        );
    }

    fn install_page_content_visible(&self) -> bool {
        self.page == Page::Install
            && !self.easy_mode_enabled()
            && !self.advanced_visible
            && !self.progress_visible
    }

    unsafe fn redraw_install_volume_layout_frame(&self, hwnd: HWND, row_visibility: Option<bool>) {
        let redraw_was_suspended = IsWindowVisible(hwnd).as_bool();
        if redraw_was_suspended {
            let _ = SendMessageW(hwnd, 0x000B, WPARAM(0), LPARAM(0)); // WM_SETREDRAW(FALSE)
        }
        if let (Some(handles), Some(visible)) = (self.handles, row_visibility) {
            let command = if visible { SW_SHOW } else { SW_HIDE };
            let _ = ShowWindow(handles.image_volume_label, command);
            let _ = ShowWindow(handles.image_volume, command);
        }
        self.layout(hwnd);
        if redraw_was_suspended {
            let _ = SendMessageW(hwnd, 0x000B, WPARAM(1), LPARAM(0)); // WM_SETREDRAW(TRUE)
            let _ = RedrawWindow(
                hwnd,
                None,
                None,
                RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN | RDW_UPDATENOW,
            );
        } else {
            let _ = InvalidateRect(hwnd, None, false);
        }
    }

    /// Reveals or collapses the optional image-volume row with three deterministic linear frames.
    /// The row itself remains hidden while space is moving, so it cannot overlap the partition
    /// list; no focus, selection or business state is changed by this transition.
    unsafe fn set_install_volume_row_visible(&mut self, hwnd: HWND, visible: bool) {
        let Some(_) = self.handles else {
            return;
        };
        let _ = KillTimer(hwnd, INSTALL_VOLUME_LAYOUT_TIMER_ID);
        let current_expansion = self
            .install_volume_layout_transition
            .map(InstallVolumeLayoutTransition::expansion)
            .unwrap_or(if self.install_volume_row_presented {
                34
            } else {
                0
            });
        let target_expansion = if visible { 34 } else { 0 };
        let can_animate = self.install_page_content_visible()
            && IsWindowVisible(hwnd).as_bool()
            && current_expansion != target_expansion;

        if !can_animate {
            self.install_volume_layout_transition = None;
            self.install_volume_row_presented = visible;
            self.redraw_install_volume_layout_frame(
                hwnd,
                Some(visible && self.install_page_content_visible()),
            );
            return;
        }

        // Keep the row itself out of the z-order while the reserved space moves. It is shown only
        // in the final atomic frame, preventing transient overlap and native ComboBox focus churn.
        self.install_volume_layout_transition = Some(InstallVolumeLayoutTransition::new(
            current_expansion,
            visible,
        ));
        self.redraw_install_volume_layout_frame(hwnd, Some(false));
        let _ = SetTimer(
            hwnd,
            INSTALL_VOLUME_LAYOUT_TIMER_ID,
            INSTALL_VOLUME_LAYOUT_TICK_MS,
            None,
        );
    }

    unsafe fn advance_install_volume_layout(&mut self, hwnd: HWND) {
        let Some(mut transition) = self.install_volume_layout_transition else {
            let _ = KillTimer(hwnd, INSTALL_VOLUME_LAYOUT_TIMER_ID);
            return;
        };
        let complete = transition.advance();
        self.install_volume_layout_transition = Some(transition);
        if complete {
            let _ = KillTimer(hwnd, INSTALL_VOLUME_LAYOUT_TIMER_ID);
            self.install_volume_row_presented = transition.target != 0;
            self.install_volume_layout_transition = None;
            self.redraw_install_volume_layout_frame(
                hwnd,
                Some(self.install_volume_row_presented && self.install_page_content_visible()),
            );
        } else {
            self.redraw_install_volume_layout_frame(hwnd, None);
        }
    }

    unsafe fn select_page(&mut self, hwnd: HWND, page: Page) {
        self.select_page_impl(hwnd, page, true);
    }

    unsafe fn select_page_impl(&mut self, hwnd: HWND, page: Page, manage_redraw: bool) {
        let Some(h) = self.handles else { return };
        // Navigation and advanced-page switches settle any in-flight three-frame transition. The
        // target is derived from the already accepted image inventory, never from focus/selection.
        let _ = KillTimer(hwnd, INSTALL_VOLUME_LAYOUT_TIMER_ID);
        self.install_volume_layout_transition = None;
        self.install_volume_row_presented = !self.image_volumes.is_empty();
        if page != Page::Hardware {
            let _ = KillTimer(hwnd, HARDWARE_COPY_TIMER_ID);
            self.hardware_copy_feedback.expire();
        }
        // A page switch changes the visibility and geometry of dozens of child windows.  Letting
        // every ShowWindow call paint immediately exposes intermediate layouts as flashes. Suspend
        // the visible top level and every descendant; WM_SETREDRAW is per HWND and freezing only
        // the parent does not stop a child common control from publishing its own intermediate DC.
        let redraw = manage_redraw.then(|| redraw::suspend(hwnd)).flatten();
        if self.advanced_visible {
            if let Some(advanced) = &self.advanced_page {
                advanced.show(false);
            }
            self.advanced_visible = false;
        }
        self.page = page;
        let (title, description, primary) = match page {
            Page::Install => (
                crate::tr!("系统安装"),
                crate::tr!("选择系统镜像、目标分区和安装选项。"),
                crate::tr!("开始安装"),
            ),
            Page::Backup => (
                crate::tr!("系统备份"),
                crate::tr!("选择源分区、保存位置和备份格式。"),
                crate::tr!("开始备份"),
            ),
            Page::Download => (
                crate::tr!("在线下载"),
                crate::tr!("下载 Windows 镜像、驱动和常用软件。"),
                crate::tr!("下载"),
            ),
            Page::Tools => (
                crate::tr!("工具箱"),
                crate::tr!("运行系统维护、修复和诊断工具。"),
                crate::tr!("打开"),
            ),
            Page::Hardware => (
                crate::tr!("系统与硬件信息"),
                crate::tr!("查看当前计算机的系统与硬件摘要。"),
                crate::tr!("复制信息"),
            ),
            Page::About => (
                crate::build_info::about_title(),
                crate::build_info::display_version(),
                crate::tr!("关闭"),
            ),
        };
        set_text(h.title, &title);
        set_text(h.description, &description);
        set_text(h.primary, &primary);
        set_text(
            h.advanced,
            &if page == Page::Hardware {
                crate::tr!("保存...")
            } else {
                crate::tr!("高级选项...")
            },
        );
        let _ = EnableWindow(h.primary, page != Page::Install);
        let easy_visible = page == Page::Install && self.easy_mode_enabled();
        let install_visible = page == Page::Install && !easy_visible;
        for control in [
            h.image_label,
            h.image_edit,
            h.browse,
            h.image_volume_label,
            h.image_volume,
            h.partitions_label,
            h.partitions,
            h.format,
            h.boot,
            h.unattend,
            h.unattend_browse,
            h.unattend_clear,
            h.unattend_path,
            h.driver_label,
            h.driver,
            h.reboot,
            h.boot_label,
            h.boot_mode,
        ] {
            let _ = ShowWindow(
                control,
                if install_visible {
                    SW_SHOW
                } else {
                    windows::Win32::UI::WindowsAndMessaging::SW_HIDE
                },
            );
        }
        let _ = ShowWindow(
            h.advanced,
            if install_visible || page == Page::Hardware {
                SW_SHOW
            } else {
                windows::Win32::UI::WindowsAndMessaging::SW_HIDE
            },
        );
        let _ = ShowWindow(
            h.refresh,
            if install_visible {
                SW_SHOW
            } else {
                windows::Win32::UI::WindowsAndMessaging::SW_HIDE
            },
        );
        let show_install_advanced = install_visible && self.app_config.enable_advanced_options;
        for control in [h.run_diskpart, h.open_diskpart_dir, h.edit_boot_commands] {
            let _ = ShowWindow(
                control,
                if show_install_advanced {
                    SW_SHOW
                } else {
                    SW_HIDE
                },
            );
        }
        let show_pe = install_visible && self.install_pe_selector_should_be_visible();
        let pe_command = if show_pe { SW_SHOW } else { SW_HIDE };
        let _ = ShowWindow(h.pe_label, pe_command);
        let _ = ShowWindow(h.pe, pe_command);
        self.update_unattend_controls_visibility();
        // PCA controls have stricter, image-dependent visibility than the rest of the install
        // page. Reapply that policy after every page switch instead of showing them wholesale.
        self.update_advanced_install_context();
        if install_visible && self.image_volumes.is_empty() {
            let _ = ShowWindow(h.image_volume_label, SW_HIDE);
            let _ = ShowWindow(h.image_volume, SW_HIDE);
        }
        if let Some(backup) = &self.backup_page {
            backup.show(page == Page::Backup);
        }
        if let Some(download) = &self.download_page {
            download.show(page == Page::Download);
        }
        if let Some(easy) = &self.easy_page {
            easy.show(easy_visible);
        }
        if let Some(tools) = &self.tools_page {
            tools.show(page == Page::Tools);
        }
        if let Some(hardware) = &self.hardware_page {
            hardware.show(page == Page::Hardware);
        }
        if let Some(about) = &self.about_page {
            about.show(page == Page::About);
        }
        let show_global_primary = !matches!(page, Page::Download | Page::Tools) && !easy_visible;
        let _ = ShowWindow(
            h.primary,
            if show_global_primary {
                SW_SHOW
            } else {
                windows::Win32::UI::WindowsAndMessaging::SW_HIDE
            },
        );
        match primary_state_refresh_for_page(page) {
            PrimaryStateRefresh::Install => self.update_install_primary_state(),
            PrimaryStateRefresh::Backup => self.update_backup_primary_state(),
            PrimaryStateRefresh::None => {}
        }
        if page_switch_requires_full_layout(page) {
            // The install geometry is inventory-dependent: loading an ISO adds the image-volume
            // row even while another page is visible. Re-entering Install must rebuild its
            // complete geometry from accepted inventory, not merely restore HWND visibility and
            // assume the startup layout is still valid.
            self.layout(hwnd);
        } else {
            // Ordinary navigation keeps page geometry stable from startup/WM_SIZE. Only the
            // command bar depends on page visibility; relayout of every hidden page here caused
            // the visible controls to be presented one by one and made Tools especially slow.
            self.layout_page_switch_chrome(hwnd);
        }
        for nav in h.nav {
            let _ = InvalidateRect(nav, None, false);
        }
        if redraw.is_some() {
            redraw::resume_client(hwnd, redraw);
        } else if manage_redraw {
            let _ = InvalidateRect(hwnd, None, false);
        }
    }

    unsafe fn install_control_snapshot(&self) -> Option<InstallControlSnapshot> {
        let Some(h) = &self.handles else {
            return None;
        };
        let checked = |control: HWND| SendMessageW(control, 0x00F0, WPARAM(0), LPARAM(0)).0 == 1;
        Some(InstallControlSnapshot {
            format_partition: checked(h.format),
            repair_boot: checked(h.boot),
            unattended_install: checked(h.unattend),
            auto_reboot: checked(h.reboot),
            run_diskpart_scripts: checked(h.run_diskpart),
            driver_index: SendMessageW(h.driver, 0x0147, WPARAM(0), LPARAM(0)).0,
            boot_mode_index: SendMessageW(h.boot_mode, 0x0147, WPARAM(0), LPARAM(0)).0,
            pca_mode_index: SendMessageW(h.pca_mode, 0x0147, WPARAM(0), LPARAM(0)).0,
        })
    }

    unsafe fn sync_install_preferences_from_controls(&mut self) {
        if let Some(snapshot) = self.install_control_snapshot() {
            snapshot.apply_to(&mut self.app_config.install_prefs);
        }
    }

    unsafe fn persist_install_preferences(&mut self) {
        self.sync_install_preferences_from_controls();
        if let Err(error) = self.app_config.save() {
            log::warn!("保存原生 UI 安装偏好失败: {error}");
        }
    }

    unsafe fn update_unattend_controls_visibility(&self) {
        let Some(handles) = &self.handles else { return };
        let visible = self.page == Page::Install
            && !self.easy_mode_enabled()
            && !self.advanced_visible
            && !self.progress_visible
            && SendMessageW(handles.unattend, 0x00F0, WPARAM(0), LPARAM(0)).0 == 1;
        let command = if visible { SW_SHOW } else { SW_HIDE };
        let _ = ShowWindow(handles.unattend_browse, command);
        let _ = ShowWindow(handles.unattend_path, command);
        let _ = ShowWindow(
            handles.unattend_clear,
            if visible && !self.custom_unattend_path.trim().is_empty() {
                SW_SHOW
            } else {
                SW_HIDE
            },
        );
    }

    unsafe fn browse_for_unattend(&mut self) {
        let mut dialog = rfd::FileDialog::new();
        if self.xp_i386_source.is_some() {
            dialog = dialog
                .add_filter(crate::tr!("XP/2003 应答文件"), &["sif"])
                .add_filter(crate::tr!("所有文件"), &["*"]);
        } else {
            dialog = dialog
                .add_filter(crate::tr!("无人值守文件"), &["xml"])
                .add_filter(crate::tr!("所有文件"), &["*"]);
        }
        let Some(path) = dialog.pick_file() else {
            return;
        };
        let path_text = path.to_string_lossy().into_owned();
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                let body = content.trim_start_matches('\u{feff}');
                let is_sif = path_text.to_ascii_lowercase().ends_with(".sif")
                    || body.trim_start().starts_with('[');
                self.custom_unattend_error = if is_sif {
                    crate::core::install_config::validate_winnt_sif(&content).err()
                } else {
                    crate::core::install_config::validate_unattend_xml(&content).err()
                };
                self.custom_unattend_path = path_text;
            }
            Err(error) => {
                self.custom_unattend_path = path_text;
                self.custom_unattend_error = Some(error.to_string());
            }
        }
        if let Some(handles) = &self.handles {
            let text = match &self.custom_unattend_error {
                Some(error) => crate::tr!("无人值守文件语法错误：{}（已禁用安装）", error),
                None => crate::tr!("已选择：{}", self.custom_unattend_path),
            };
            set_text(handles.unattend_path, &text);
        }
        self.update_unattend_controls_visibility();
        self.update_advanced_install_context();
        self.update_install_primary_state();
    }

    unsafe fn clear_custom_unattend(&mut self) {
        self.custom_unattend_path.clear();
        self.custom_unattend_error = None;
        if let Some(handles) = &self.handles {
            set_text(
                handles.unattend_path,
                &crate::tr!("未选择则使用内置生成的无人值守配置"),
            );
        }
        self.update_unattend_controls_visibility();
        self.update_install_primary_state();
    }

    unsafe fn toggle_advanced_page(&mut self, hwnd: HWND) {
        if !self.advanced_visible
            && self
                .app_config
                .install_prefs
                .advanced_options
                .wifi_detected
                .is_none()
        {
            let available = crate::core::native_wifi::connected_wifi_available().unwrap_or(false);
            self.app_config.install_prefs.advanced_options.wifi_detected = Some(available);
        }
        self.update_advanced_install_context();
        let Some(h) = &self.handles else { return };
        if self.advanced_visible {
            if let Some(advanced) = &self.advanced_page {
                advanced.read_into(&mut self.app_config.install_prefs.advanced_options);
            }
            if let Err(error) = self.app_config.save() {
                log::warn!("保存高级选项失败: {error}");
            }
            // `select_page_impl` rebuilds the inventory-dependent Install geometry before it
            // publishes the page. Keep the advanced flag set until that transaction has hidden
            // the embedded page and restored the ordinary Install controls.
            self.select_page(hwnd, Page::Install);
            return;
        }

        // The same owner-draw button is reused at a different position as “Save and return”.
        // Clear the hot/pressed state left by the click before moving it; otherwise the old
        // pointer position does not generate WM_MOUSELEAVE until the user moves the mouse and
        // the stale button surface can cover the first frame at its new position.
        let _ = SendMessageW(h.advanced, WM_CANCELMODE, WPARAM(0), LPARAM(0));
        let redraw = redraw::suspend(hwnd);
        self.advanced_visible = true;
        if let Some(advanced) = &self.advanced_page {
            let ssid = self
                .app_config
                .install_prefs
                .advanced_options
                .wifi_ssid
                .as_str();
            advanced.set_wifi_caption((!ssid.is_empty()).then_some(ssid));
        }
        for control in [
            h.image_label,
            h.image_edit,
            h.browse,
            h.image_volume_label,
            h.image_volume,
            h.partitions_label,
            h.partitions,
            h.format,
            h.boot,
            h.unattend,
            h.unattend_browse,
            h.unattend_clear,
            h.unattend_path,
            h.driver_label,
            h.driver,
            h.reboot,
            h.boot_label,
            h.boot_mode,
            h.pca_label,
            h.pca_mode,
            h.run_diskpart,
            h.open_diskpart_dir,
            h.edit_boot_commands,
            h.pe_label,
            h.pe,
            h.refresh,
            h.primary,
        ] {
            let _ = ShowWindow(control, windows::Win32::UI::WindowsAndMessaging::SW_HIDE);
        }
        set_text(h.title, &crate::tr!("高级选项"));
        set_text(
            h.description,
            &crate::tr!("配置系统优化、驱动、脚本和兼容性选项。"),
        );
        set_text(h.advanced, &crate::tr!("保存并返回"));
        if let Some(advanced) = &self.advanced_page {
            advanced.show(true);
            // Dialog shells reassert descendant themes after their final ShowWindow pass. Do the
            // same for this embedded page so its checkboxes use exactly the same shared painter
            // and current light/dark palette as controls that were visible during startup.
            advanced.apply_theme(self.control_palette());
        }
        // The advanced page leaves only “Save and return” in the global command bar. Repack it
        // after hiding the normal Install commands rather than retaining the three-button layout.
        self.layout(hwnd);
        if redraw.is_some() {
            redraw::resume(hwnd, redraw);
        } else {
            let _ = RedrawWindow(
                hwnd,
                None,
                None,
                RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN | RDW_UPDATENOW,
            );
        }
    }

    unsafe fn update_install_primary_state(&mut self) {
        // Programmatic defaults (CB_SETCURSEL/BM_SETCHECK) do not emit CBN_SELCHANGE or
        // BN_CLICKED. Synchronize the visible controls first so the enabled state and the click
        // path both use the same current preferences even before the user touches a ComboBox.
        self.sync_install_preferences_from_controls();
        let validation = self.install_intent();
        let enabled = validation.is_ok();
        let pca_pending = pca_pending_status(
            self.pca_selection_is_relevant(),
            self.pca_detection_pending,
            self.pca_target_detection_pending,
        )
        .is_some();
        if !may_publish_install_chrome(self.page, self.advanced_visible, self.progress_visible) {
            return;
        }
        let Some(h) = &self.handles else { return };
        let was_enabled = IsWindowEnabled(h.primary).as_bool();
        if was_enabled != enabled {
            let _ = EnableWindow(h.primary, enabled);
            let _ = InvalidateRect(h.primary, None, false);
            if was_enabled && !enabled {
                if let Err(error) = validation {
                    log::warn!("安装按钮因校验状态变化被禁用: {error:?}");
                    if !pca_pending {
                        set_text(h.status, &error.to_string());
                    }
                }
            }
        }
        // Selection changes can move an already-enabled install intent back behind either PCA
        // probe. Keep the dedicated loading text authoritative instead of replacing it with the
        // generic validation error emitted while disabling the command.
        if pca_pending {
            self.update_pca_detection_status();
        }
    }

    unsafe fn update_backup_primary_state(&self) {
        let (Some(handles), Some(page)) = (&self.handles, &self.backup_page) else {
            return;
        };
        let rows: Vec<_> = self
            .partitions
            .iter()
            .map(|partition| BackupPartitionRow {
                volume: partition.letter.clone(),
                total_size: String::new(),
                used_size: String::new(),
                label: partition.label.clone(),
                bitlocker: localized_bitlocker_status(&partition.bitlocker_status),
                status: String::new(),
                has_windows: partition.has_windows,
                is_system_partition: partition.is_system_partition,
            })
            .collect();
        let state = page.read_state();
        let is_pe_environment = crate::core::disk::DiskManager::is_pe_environment();
        page.update_source_warning(&rows, is_pe_environment);
        let mut enabled = state.validate(&rows).is_ok();
        let mut requires_pe = false;
        if let Some(index) = state.source_partition {
            requires_pe = rows
                .get(index)
                .is_some_and(|partition| partition.is_system_partition)
                && !is_pe_environment;
            if requires_pe && (self.available_pe().is_empty() || page.selected_pe().is_none()) {
                enabled = false;
                set_text(
                    page.handles().warning,
                    &crate::tr!("备份当前系统分区前请选择可用的 PE 环境。"),
                );
            }
        }
        page.show_pe_selector(requires_pe);
        if IsWindowEnabled(handles.primary).as_bool() != enabled {
            let _ = EnableWindow(handles.primary, enabled);
            let _ = InvalidateRect(handles.primary, None, false);
        }
    }

    unsafe fn handle_install_partition_changed(&mut self, hwnd: HWND) {
        let Some(handles) = &self.handles else { return };
        let target = self.selected_install_target();
        if target
            .as_ref()
            .is_some_and(|target| target.is_current_system || target.has_windows)
        {
            let _ = SendMessageW(handles.format, 0x00F1, WPARAM(1), LPARAM(0));
            let _ = SendMessageW(handles.boot, 0x00F1, WPARAM(1), LPARAM(0));
            self.app_config.install_prefs.format_partition = true;
            self.app_config.install_prefs.repair_boot = true;
            if let Err(error) = self.app_config.save() {
                log::warn!("保存目标分区推荐安装选项失败: {error}");
            }
        }
        let show_pe = !crate::core::disk::DiskManager::is_pe_environment()
            && self.available_pe().len() > 1
            && target.is_some_and(|target| target.is_current_system);
        let command = if show_pe { SW_SHOW } else { SW_HIDE };
        let _ = ShowWindow(handles.pe_label, command);
        let _ = ShowWindow(handles.pe, command);
        self.layout(hwnd);
        self.apply_unattend_default();
        self.update_unattend_conflict();
        self.update_advanced_install_context();
        self.request_pca_target_detection(hwnd);
        self.update_pca_detection_status();
        self.update_install_primary_state();
    }

    unsafe fn update_unattend_conflict(&mut self) {
        let Some(handles) = &self.handles else { return };
        // 目标安装分区中的 Panther/Sysprep 文件属于旧系统，格式化后会被删除，不能
        // 用来推断本次所选镜像是否自带应答文件。冲突判断只看源镜像/安装介质。
        let _ = EnableWindow(handles.unattend, true);
        self.update_unattend_controls_visibility();
    }

    unsafe fn selected_target_uses_uefi(&self) -> bool {
        self.selected_install_target().is_some_and(|target| {
            pca_target_uses_uefi(self.app_config.install_prefs.boot_mode, target.style)
        })
    }

    unsafe fn selected_image_supports_pca(&self) -> bool {
        let Some(handles) = self.handles else {
            return false;
        };
        if self.xp_i386_source.is_some() {
            return false;
        }
        let selected = SendMessageW(handles.image_volume, 0x0147, WPARAM(0), LPARAM(0)).0;
        usize::try_from(selected)
            .ok()
            .and_then(|index| self.image_volumes.get(index))
            .is_some_and(|image| {
                lr_core::pca_preflight::supports_pca_selection(
                    image.major_version,
                    image.architecture,
                )
            })
    }

    unsafe fn pca_selection_is_relevant(&self) -> bool {
        self.app_config.install_prefs.repair_boot
            && self.selected_target_uses_uefi()
            && self.selected_image_supports_pca()
    }

    unsafe fn pca_selection_error(&self) -> Option<String> {
        if !self.pca_selection_is_relevant() {
            return None;
        }
        if let Some(error) = self.pca_target_detection_error.as_ref() {
            let target_error_blocks = self.pca_target_context().is_none_or(|(_, context)| {
                pca_target_error_blocks(context, self.pca_firmware.is_some(), true)
            });
            if target_error_blocks {
                return Some(crate::tr!("目标系统所在磁盘没有可用的 ESP: {}", error));
            }
        }
        let firmware = self.pca_firmware.as_ref()?;
        if firmware.secure_boot_enabled != Some(true) {
            return None;
        }
        match self.app_config.install_prefs.boot_pca_mode {
            lr_core::boot_pca::BootPcaMode::Pca2011
                if firmware.revokes_pca2011 == Some(true)
                    || firmware.trusts_pca2011 == Some(false) =>
            {
                Some(crate::tr!("当前固件无法启动 PCA2011，引导签名选择无效。"))
            }
            lr_core::boot_pca::BootPcaMode::Pca2023
                if firmware.trusts_pca2023 == Some(false) =>
            {
                Some(crate::tr!("当前固件未信任 PCA2023，引导签名选择无效。"))
            }
            lr_core::boot_pca::BootPcaMode::Auto
                if firmware.revokes_pca2011 == Some(true)
                    && firmware.trusts_pca2023 != Some(true) =>
            {
                Some(crate::tr!(
                    "固件已撤销 PCA2011，但无法确认 PCA2023 信任；请完成固件证书更新，或手动选择 PCA2023。"
                ))
            }
            _ => None,
        }
    }

    fn automatic_pca_label(&self) -> String {
        let use_pca2023 = self.pca_firmware.as_ref().is_some_and(|firmware| {
            firmware.secure_boot_enabled == Some(true)
                && (firmware.trusts_pca2023 == Some(true) || firmware.revokes_pca2011 == Some(true))
        });
        if use_pca2023 {
            crate::tr!("自动（PCA2023）")
        } else {
            crate::tr!("自动（PCA2011）")
        }
    }

    unsafe fn update_pca_combo_labels(&self) {
        let Some(handles) = self.handles else { return };
        replace_combo_labels(
            handles.pca_mode,
            &[
                self.automatic_pca_label(),
                "PCA2011".to_owned(),
                "PCA2023".to_owned(),
            ],
        );
    }

    unsafe fn refresh_source_unattend(&mut self) {
        let (path, index) = if let Some(path) = self.xp_i386_source.as_deref() {
            (path, 1)
        } else if let Some(path) = self.effective_image_path.as_deref() {
            let index = self
                .handles
                .and_then(|handles| {
                    usize::try_from(
                        SendMessageW(handles.image_volume, 0x0147, WPARAM(0), LPARAM(0)).0,
                    )
                    .ok()
                })
                .and_then(|selected| self.image_volumes.get(selected))
                .or_else(|| self.image_volumes.first())
                .map_or(1, |image| image.index);
            (path, index)
        } else {
            self.source_has_unattend = false;
            self.apply_unattend_default();
            return;
        };
        self.source_has_unattend = crate::core::native_image_source::source_has_unattend(
            std::path::Path::new(path),
            index,
        );
        self.apply_unattend_default();
    }

    unsafe fn apply_unattend_default(&mut self) {
        let Some(handles) = self.handles else { return };
        let enabled = unattended_checked_for_source_preference(
            self.app_config.install_prefs.unattended_install,
            self.source_has_unattend,
        );
        let _ = SendMessageW(
            handles.unattend,
            0x00F1,
            WPARAM(usize::from(enabled)),
            LPARAM(0),
        );
        self.update_unattend_controls_visibility();
    }

    unsafe fn update_advanced_install_context(&mut self) {
        let Some(handles) = &self.handles else { return };
        let selected_index = SendMessageW(handles.image_volume, 0x0147, WPARAM(0), LPARAM(0)).0;
        let selected = usize::try_from(selected_index)
            .ok()
            .and_then(|index| self.image_volumes.get(index));
        // The bundled Windows 7 USB/NVMe and UefiSeven resources were retired. Keep the legacy
        // config fields parse-compatible, but do not expose options that no longer have a vetted
        // release payload.
        let show_windows_7 = false;
        let show_xp = self.xp_i386_source.is_some()
            || selected.is_some_and(|image| image.major_version == Some(5));
        let show_pca = self.page == Page::Install
            && !self.easy_mode_enabled()
            && !self.advanced_visible
            && !self.progress_visible
            && self.pca_selection_is_relevant();
        let pca_command = if show_pca { SW_SHOW } else { SW_HIDE };
        let _ = ShowWindow(handles.pca_label, pca_command);
        let _ = ShowWindow(handles.pca_mode, pca_command);
        let target_uefi = self.selected_target_uses_uefi();
        let unattended_enabled =
            SendMessageW(handles.unattend, 0x00F0, WPARAM(0), LPARAM(0)).0 == 1;
        if let Some(page) = &mut self.advanced_page {
            page.set_context(AdvancedPageContext {
                unattended_enabled,
                wifi_available: self
                    .app_config
                    .install_prefs
                    .advanced_options
                    .wifi_detected
                    .unwrap_or(false),
                show_windows_7,
                show_windows_7_uefi: show_windows_7 && target_uefi,
                show_xp,
            });
        }
    }

    unsafe fn handle_wifi_migration_toggle(&mut self, hwnd: HWND) {
        let Some(page) = &self.advanced_page else {
            return;
        };
        let checkbox = page.handles().system_checks[9];
        let checked = SendMessageW(checkbox, 0x00F0, WPARAM(0), LPARAM(0)).0 == 1;
        if !checked {
            let data = &mut self.app_config.install_prefs.advanced_options;
            data.migrate_wifi = false;
            data.wifi_profile_xml.clear();
            data.wifi_ssid.clear();
            page.set_wifi_caption(None);
            return;
        }
        match crate::core::native_wifi::capture_connected_wifi() {
            Ok(profile) => {
                let data = &mut self.app_config.install_prefs.advanced_options;
                data.migrate_wifi = true;
                data.wifi_detected = Some(true);
                data.wifi_ssid = profile.ssid;
                data.wifi_profile_xml = profile.xml;
                page.set_wifi_caption(Some(&data.wifi_ssid));
            }
            Err(error) => {
                let _ = SendMessageW(checkbox, 0x00F1, WPARAM(0), LPARAM(0));
                let data = &mut self.app_config.install_prefs.advanced_options;
                data.migrate_wifi = false;
                data.wifi_profile_xml.clear();
                data.wifi_ssid.clear();
                page.set_wifi_caption(None);
                self.show_information(
                    hwnd,
                    crate::tr!("无法迁移 Wi-Fi 配置"),
                    crate::tr!("未能读取当前连接的 Wi-Fi 配置：{}", error),
                );
            }
        }
    }

    unsafe fn browse_advanced_path(&self, target: AdvancedBrowseTarget) {
        let selected = match target {
            AdvancedBrowseTarget::DeployScript | AdvancedBrowseTarget::FirstLoginScript => {
                rfd::FileDialog::new().pick_file()
            }
            AdvancedBrowseTarget::RegistryFile => rfd::FileDialog::new()
                .add_filter(crate::tr!("注册表文件"), &["reg"])
                .pick_file(),
            AdvancedBrowseTarget::CustomDriversDirectory
            | AdvancedBrowseTarget::CustomFilesDirectory
            | AdvancedBrowseTarget::Windows7Usb3Drivers
            | AdvancedBrowseTarget::Windows7NvmeDrivers => rfd::FileDialog::new().pick_folder(),
        };
        if let (Some(page), Some(path)) = (&self.advanced_page, selected) {
            page.set_path(target, &path.to_string_lossy());
        }
    }

    unsafe fn update_storage_driver_default(&mut self) {
        let Some(handles) = &self.handles else { return };
        let selected_index = SendMessageW(handles.image_volume, 0x0147, WPARAM(0), LPARAM(0)).0;
        let selected = usize::try_from(selected_index)
            .ok()
            .and_then(|index| self.image_volumes.get(index));
        let target = selected.map(|image| {
            format!(
                "{}::{}::{}",
                self.effective_image_path.as_deref().unwrap_or_default(),
                image.index,
                image.name
            )
        });
        if target == self.advanced_defaults_target {
            return;
        }
        self.advanced_defaults_target = target;
        let advanced = &mut self.app_config.install_prefs.advanced_options;
        advanced.win7_inject_usb3_driver = false;
        advanced.win7_usb3_driver_path.clear();
        advanced.win7_inject_nvme_driver = false;
        advanced.win7_nvme_driver_path.clear();
        advanced.win7_fix_acpi_bsod = false;
        advanced.win7_fix_storage_bsod = false;
        advanced.win7_uefi_patch = false;
        advanced.import_storage_controller_drivers =
            selected.is_some_and(|image| image.major_version.is_some_and(|major| major >= 10));
        if self.xp_i386_source.is_some()
            || selected.is_some_and(|image| image.major_version == Some(5))
        {
            advanced.xp_inject_usb3_driver = true;
            advanced.xp_inject_nvme_driver = true;
            advanced.xp_defaults_applied = true;
        }
        if let Some(page) = &self.advanced_page {
            page.apply(advanced);
        }
    }

    unsafe fn update_system_status(&self) {
        let Some(status) = self.handles.as_ref().map(|handles| handles.status) else {
            return;
        };
        let info = self.config.system_info.clone();
        if let Some(info) = info {
            set_text(
                status,
                &crate::tr!(
                    "启动模式: {} | TPM: {} | 安全启动: {}",
                    info.boot_mode,
                    if info.tpm_enabled {
                        crate::tr!("已启用")
                    } else {
                        crate::tr!("未启用")
                    },
                    if info.secure_boot {
                        crate::tr!("已开启")
                    } else {
                        crate::tr!("未开启")
                    }
                ),
            );
        } else {
            set_text(
                status,
                &crate::tr!("启动模式: 未知 | TPM: 未知 | 安全启动: 未知"),
            );
        }
    }

    unsafe fn browse_for_image(&mut self, hwnd: HWND) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter(
                crate::tr!("系统镜像"),
                &["wim", "esd", "swm", "gho", "ghs", "iso"],
            )
            .pick_file()
        {
            self.load_image_path(hwnd, path);
        }
    }

    unsafe fn browse_for_backup(&self) {
        let Some(page) = &self.backup_page else {
            return;
        };
        let format = page.selected_format();
        let extension = format.extension();
        let default_name = format!("backup.{extension}");
        let Some(path) = rfd::FileDialog::new()
            .add_filter(format.filter_description(), &[extension])
            .set_file_name(&default_name)
            .save_file()
        else {
            return;
        };
        let path = if path.extension().is_none() {
            path.with_extension(extension)
        } else {
            path
        };
        page.set_save_path(&path.to_string_lossy(), path.exists());
    }

    unsafe fn handle_image_edit_changed(&mut self, hwnd: HWND) {
        if self.image_edit_programmatic_change {
            return;
        }
        let Some(handles) = self.handles else { return };
        // A visible path must never remain associated with metadata from a previously inspected
        // source. Invalidate the generation immediately; a late result is discarded by the
        // existing generation/text check and can never re-enable installation for stale input.
        self.image_request_generation = self.image_request_generation.wrapping_add(1);
        if let Some(previous) = self.mounted_iso.take() {
            if let Err(error) =
                crate::core::iso::IsoMounter::unmount_iso_by_path(&previous.to_string_lossy())
            {
                log::warn!("手工修改镜像路径时卸载旧 ISO 失败: {error}");
            }
        }
        self.image_volumes.clear();
        self.effective_image_path = None;
        self.xp_i386_source = None;
        self.source_has_unattend = false;
        self.clear_pca_target_detection();
        self.update_advanced_install_context();
        let _ = SendMessageW(handles.image_volume, 0x014B, WPARAM(0), LPARAM(0));
        self.set_install_volume_row_visible(hwnd, false);
        self.apply_unattend_default();
        self.update_unattend_conflict();
        let _ = EnableWindow(handles.primary, false);
        let path = get_text(handles.image_edit);
        set_text(
            handles.status,
            &if path.trim().is_empty() {
                crate::tr!("请选择系统镜像。")
            } else {
                crate::tr!("镜像路径已更改，离开输入框后将重新读取。")
            },
        );
    }

    unsafe fn commit_image_edit(&mut self, hwnd: HWND) {
        if self.image_edit_programmatic_change {
            return;
        }
        let Some(handles) = self.handles else { return };
        let path = get_text(handles.image_edit);
        let path = path.trim();
        if !path.is_empty() {
            self.load_image_path(hwnd, std::path::PathBuf::from(path));
        }
    }

    unsafe fn load_image_path(&mut self, hwnd: HWND, path: std::path::PathBuf) {
        let Some(h) = self.handles else { return };
        if let Some(previous) = self.mounted_iso.take() {
            if let Err(error) =
                crate::core::iso::IsoMounter::unmount_iso_by_path(&previous.to_string_lossy())
            {
                log::warn!("切换镜像前卸载 ISO 失败: {error}");
            }
        }
        self.image_edit_programmatic_change = true;
        set_text(h.image_edit, &path.to_string_lossy());
        self.image_edit_programmatic_change = false;
        self.image_volumes.clear();
        self.effective_image_path = None;
        self.xp_i386_source = None;
        self.clear_pca_target_detection();
        self.update_advanced_install_context();
        let _ = SendMessageW(h.image_volume, 0x014B, WPARAM(0), LPARAM(0));
        self.set_install_volume_row_visible(hwnd, false);
        let _ = EnableWindow(h.primary, false);
        set_text(h.status, &crate::tr!("正在读取系统镜像卷..."));
        self.image_request_generation = self.image_request_generation.wrapping_add(1);
        self.request_image_info(
            hwnd,
            path.to_string_lossy().into_owned(),
            self.image_request_generation,
        );
    }

    fn request_image_info(&self, hwnd: HWND, path: String, generation: u64) {
        let window = hwnd.0 as usize;
        std::thread::spawn(move || {
            let result = crate::core::native_image_source::inspect_image_source(&path);
            let payload = Box::into_raw(Box::new(ImageInfoMessage {
                generation,
                requested_path: path,
                result,
            }));
            unsafe {
                if PostMessageW(
                    HWND(window as *mut _),
                    WM_IMAGE_INFO_READY,
                    WPARAM(0),
                    LPARAM(payload as isize),
                )
                .is_err()
                {
                    drop(Box::from_raw(payload));
                }
            }
        });
    }

    unsafe fn selected_install_partition_key(&self, list: HWND) -> Option<PartitionSelectionKey> {
        let selected = SendMessageW(list, 0x100C, WPARAM(usize::MAX), LPARAM(2)).0;
        usize::try_from(selected)
            .ok()
            .and_then(|index| self.partitions.get(index))
            .map(PartitionSelectionKey::from)
    }

    unsafe fn selected_backup_partition_key(&self) -> Option<PartitionSelectionKey> {
        self.backup_page
            .as_ref()
            .and_then(|page| page.read_state().source_partition)
            .and_then(|index| self.partitions.get(index))
            .map(PartitionSelectionKey::from)
    }

    unsafe fn apply_partition_inventory(
        &mut self,
        partitions: Vec<crate::core::disk::Partition>,
    ) -> bool {
        let Some(list) = self.handles.as_ref().map(|handles| handles.partitions) else {
            return false;
        };
        let previous_install_target = self.selected_install_partition_key(list);
        let previous_backup_source = self.selected_backup_partition_key();
        self.partitions = partitions;
        let selected_install_target = previous_install_target.as_ref().and_then(|key| {
            self.partitions
                .iter()
                .position(|partition| key.matches(partition))
        });
        let selected_backup_source = previous_backup_source.as_ref().and_then(|key| {
            self.partitions
                .iter()
                .position(|partition| key.matches(partition))
        });

        self.partition_list_replacing = true;
        let _ = SendMessageW(list, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));
        self.populate_partitions(list, false);
        let mut clear_selection = LVITEMW {
            stateMask: LVIS_SELECTED,
            state: Default::default(),
            iItem: -1,
            ..Default::default()
        };
        let _ = SendMessageW(
            list,
            0x102B,
            WPARAM(usize::MAX),
            LPARAM((&mut clear_selection as *mut LVITEMW) as isize),
        );
        if let Some(row) = selected_install_target {
            let mut select = LVITEMW {
                stateMask: LVIS_SELECTED,
                state: LVIS_SELECTED,
                iItem: row as i32,
                ..Default::default()
            };
            let _ = SendMessageW(
                list,
                0x102B,
                WPARAM(row),
                LPARAM((&mut select as *mut LVITEMW) as isize),
            );
        }
        self.partition_list_replacing = false;

        let backup_rows = self.backup_partition_rows();
        if let Some(page) = &self.backup_page {
            page.replace_partitions(&backup_rows, selected_backup_source);
        }
        self.update_backup_primary_state();
        true
    }

    unsafe fn refresh_partitions(&mut self) -> bool {
        match crate::core::disk::DiskManager::get_partitions() {
            Ok(partitions) => {
                self.partition_refresh_error = None;
                self.apply_partition_inventory(partitions)
            }
            Err(error) => {
                log::warn!("原生 UI 刷新分区失败: {error}");
                self.partition_refresh_error = Some(error.to_string());
                if self.page == Page::Install {
                    if let Some(handles) = self.handles {
                        set_text(
                            handles.status,
                            &crate::tr!("刷新分区信息失败，请手动刷新后重试。"),
                        );
                    }
                }
                false
            }
        }
    }

    unsafe fn schedule_partition_refresh(&mut self, hwnd: HWND) {
        self.partition_refresh_requested = true;
        self.partition_refresh_error = None;
        let _ = KillTimer(hwnd, PARTITION_REFRESH_TIMER_ID);
        if self.pca_target_detection_pending {
            // The read-only PCA probe temporarily assigns and removes an ESP drive letter. Those
            // operations generate device-change broadcasts while DiskPart still owns the probe
            // transaction. Defer the inventory scan until the probe has posted its terminal
            // result so a transient snapshot cannot replace a previously stable target.
            return;
        }
        if self.page == Page::Install {
            if let Some(handles) = self.handles {
                set_text(handles.status, &crate::tr!("正在刷新分区信息，请稍候。"));
            }
            self.update_install_primary_state();
        }
        let _ = SetTimer(
            hwnd,
            PARTITION_REFRESH_TIMER_ID,
            PARTITION_REFRESH_DEBOUNCE_MS,
            None,
        );
    }

    unsafe fn start_scheduled_partition_refresh(&mut self, hwnd: HWND) {
        let _ = KillTimer(hwnd, PARTITION_REFRESH_TIMER_ID);
        if self.pca_target_detection_pending
            || self.partition_refresh_in_flight
            || !self.partition_refresh_requested
        {
            return;
        }
        self.partition_refresh_requested = false;
        self.partition_refresh_in_flight = true;
        self.partition_refresh_generation = self.partition_refresh_generation.wrapping_add(1);
        let generation = self.partition_refresh_generation;
        let window = hwnd.0 as usize;
        std::thread::spawn(move || {
            let result =
                crate::core::disk::DiskManager::get_partitions().map_err(|error| error.to_string());
            let payload = Box::into_raw(Box::new(PartitionRefreshMessage { generation, result }));
            unsafe {
                if PostMessageW(
                    HWND(window as *mut _),
                    WM_PARTITIONS_READY,
                    WPARAM(0),
                    LPARAM(payload as isize),
                )
                .is_err()
                {
                    drop(Box::from_raw(payload));
                }
            }
        });
    }

    unsafe fn finish_partition_refresh(&mut self, hwnd: HWND, message: PartitionRefreshMessage) {
        if message.generation != self.partition_refresh_generation {
            return;
        }
        self.partition_refresh_in_flight = false;
        match message.result {
            Ok(partitions) => {
                self.partition_refresh_error = None;
                if self.apply_partition_inventory(partitions) {
                    self.request_pca_target_detection(hwnd);
                    self.update_pca_detection_status();
                }
            }
            Err(error) => {
                log::warn!("设备变更后的异步分区刷新失败: {error}");
                self.partition_refresh_error = Some(error);
                if self.page == Page::Install {
                    if let Some(handles) = self.handles {
                        set_text(
                            handles.status,
                            &crate::tr!("刷新分区信息失败，请手动刷新后重试。"),
                        );
                    }
                }
            }
        }
        if self.partition_refresh_requested {
            self.schedule_partition_refresh(hwnd);
        }
        if self.page == Page::Install {
            self.update_install_primary_state();
        }
    }

    fn backup_partition_rows(&self) -> Vec<BackupPartitionRow> {
        self.partitions
            .iter()
            .map(|partition| BackupPartitionRow {
                volume: partition.letter.clone(),
                total_size: format!("{:.1} GB", partition.total_size_mb as f64 / 1024.0),
                used_size: format!(
                    "{:.1} GB",
                    partition
                        .total_size_mb
                        .saturating_sub(partition.free_size_mb) as f64
                        / 1024.0
                ),
                label: partition.label.clone(),
                bitlocker: localized_bitlocker_status(&partition.bitlocker_status),
                status: if partition.has_windows {
                    crate::tr!("已有系统")
                } else {
                    crate::tr!("空闲")
                },
                has_windows: partition.has_windows,
                is_system_partition: partition.is_system_partition,
            })
            .collect()
    }

    unsafe fn handle_download_intent(&mut self, hwnd: HWND, intent: DownloadIntent) {
        match intent {
            DownloadIntent::SelectTab(tab) => {
                let category = match tab {
                    DownloadTab::SystemImage => ResourceCategory::SystemImage,
                    DownloadTab::Software => ResourceCategory::Software,
                    DownloadTab::GpuDriver => ResourceCategory::GpuDriver,
                };
                let _ = self
                    .download_controller
                    .apply_intent(ControllerIntent::SelectCategory(category));
                if let Some(page) = &mut self.download_page {
                    page.select_tab(tab);
                    page.replace_rows(&self.download_controller.rows());
                    for button in page.tabs {
                        let _ = InvalidateRect(button, None, true);
                    }
                }
            }
            DownloadIntent::BrowseSaveFolder => {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    if let Some(page) = &self.download_page {
                        set_text(page.save_path, &path.to_string_lossy());
                    }
                }
            }
            DownloadIntent::RefreshCatalogue => {
                if self.catalogue_messages.is_some() {
                    return;
                }
                let _ = self
                    .download_controller
                    .apply_intent(ControllerIntent::RefreshCatalogue);
                if let Some(page) = &self.download_page {
                    set_text(
                        page.status,
                        &catalogue_status_message(self.download_controller.state()),
                    );
                }
                #[cfg(feature = "non-elevated-tests")]
                {
                    let message = crate::tr!("开发预览构建不会发起网络请求。");
                    self.download_controller.fail_refresh(message);
                    if let Some(page) = &self.download_page {
                        set_text(
                            page.status,
                            &catalogue_status_message(self.download_controller.state()),
                        );
                    }
                }
                #[cfg(not(feature = "non-elevated-tests"))]
                {
                    let (sender, receiver) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let _ = sender
                            .send(crate::download::server_config::RemoteConfig::load_from_server());
                    });
                    self.catalogue_messages = Some(receiver);
                    let _ = SetTimer(hwnd, CATALOGUE_TIMER_ID, 100, None);
                }
            }
            DownloadIntent::DownloadSelected | DownloadIntent::InstallSelected => {
                let Some(page) = &self.download_page else {
                    return;
                };
                if let Some(index) = page.selected_resource() {
                    let _ = self
                        .download_controller
                        .apply_intent(ControllerIntent::SelectResource(index));
                }
                let action = if intent == DownloadIntent::DownloadSelected {
                    DownloadAction::Download
                } else {
                    DownloadAction::InstallAfterDownload
                };
                let architecture = if self
                    .config
                    .system_info
                    .as_ref()
                    .is_some_and(|info| !info.is_64bit)
                {
                    SoftwareArchitecture::X86
                } else {
                    SoftwareArchitecture::X64
                };
                match self.download_controller.plan_selected(
                    action,
                    get_text(page.save_path),
                    architecture,
                    self.app_config.allow_insecure_http_downloads,
                    self.app_config.download_threads,
                ) {
                    Ok(plan) => {
                        log::info!(
                            "原生下载计划已生成: file={}, destination={}",
                            plan.filename,
                            plan.save_directory.display()
                        );
                        match NativeDownloadExecutor::start(plan) {
                            Ok(worker) => self.show_download_progress(hwnd, worker),
                            Err(error) => {
                                set_text(page.status, &crate::tr!("无法启动下载：{}", error))
                            }
                        }
                    }
                    Err(error) => set_text(page.status, &crate::tr!("无法创建下载任务：{}", error)),
                }
            }
        }
    }

    unsafe fn handle_easy_mode_command(&mut self, hwnd: HWND, command: EasyModeCommand) {
        let Some(page) = &self.easy_page else { return };
        match command {
            EasyModeCommand::ToggleEnabled => {
                let enabled = page.enabled_value();
                self.easy_controller
                    .apply(EasyModeAction::SetEnabled(enabled));
                self.app_config.set_easy_mode(enabled);
                self.select_page(hwnd, Page::Install);
            }
            EasyModeCommand::DismissSettingsTip => {
                self.easy_controller
                    .apply(EasyModeAction::DismissSettingsTip);
                self.app_config.dismiss_easy_mode_settings_tip();
            }
            EasyModeCommand::SelectSystem => {
                if let Some(index) = page.selected_system() {
                    self.easy_controller
                        .apply(EasyModeAction::SelectSystem(index));
                }
            }
            EasyModeCommand::SelectVolume => {
                if let Some(index) = page.selected_volume() {
                    self.easy_controller
                        .apply(EasyModeAction::SelectVolume(index));
                }
            }
            EasyModeCommand::StartInstall => {
                let system_partition = self
                    .partitions
                    .iter()
                    .position(|partition| partition.is_system_partition);
                let download_directory = dirs::download_dir()
                    .unwrap_or_else(|| std::env::temp_dir().join("LetRecovery"));
                match self.easy_controller.start_install_intent(
                    system_partition,
                    &download_directory,
                    std::env::var("USERNAME").ok().as_deref(),
                ) {
                    Ok(intent) => {
                        let target = self
                            .partitions
                            .get(intent.system_partition_index)
                            .map(|partition| partition.letter.as_str())
                            .unwrap_or("?");
                        let spec = DialogSpec {
                            window_title: crate::tr!("确认重装系统"),
                            title: crate::tr!("确认重装系统"),
                            description: crate::tr!(
                                "系统：{}\r\n目标分区：{}\r\n镜像卷：{}\r\n\r\n继续后将先下载并校验镜像，随后进入安装流程。目标分区的数据可能被清除。",
                                intent.system_name,
                                target,
                                intent.volume_number
                            ),
                            width: 640,
                            height: 340,
                            buttons: DialogButtons {
                                primary: crate::tr!("确认安装"),
                                secondary: None,
                                cancel: Some(crate::tr!("取消")),
                            },
                        };
                        let confirmed = DialogShell::create(hwnd, spec)
                            .map(|mut dialog| dialog.show_modal() == DialogResult::Primary)
                            .unwrap_or(false);
                        if !confirmed {
                            return;
                        }
                        let url = match lr_core::download_integrity::validate_download_url(
                            &intent.download_url,
                            // Easy-mode entries are loaded only from LetRecovery's fixed HTTPS
                            // service. That service still publishes historical Microsoft HTTP
                            // payload URLs, so give those verbatim catalogue entries the same
                            // scoped compatibility exception as the normal download controller.
                            true,
                        ) {
                            Ok(url) => url.into_string(),
                            Err(error) => {
                                log::error!("简易模式下载 URL 无效: {error}");
                                return;
                            }
                        };
                        if let Err(error) = lr_core::download_integrity::validate_download_filename(
                            &intent.filename,
                        ) {
                            log::error!("简易模式下载文件名无效: {error}");
                            return;
                        }
                        let integrity =
                            match lr_core::download_integrity::select_expected_hash(None, None) {
                                Ok(value) => value,
                                Err(error) => {
                                    log::error!("简易模式完整性元数据无效: {error}");
                                    return;
                                }
                            };
                        let plan = crate::core::native_download_controller::DownloadPlan {
                            url,
                            save_directory: intent.download_directory.clone(),
                            filename: intent.filename.clone(),
                            integrity,
                            completion: crate::core::native_download_controller::DownloadCompletion::OpenSystemImage(intent.download_path.clone()),
                            download_threads: self.app_config.download_threads,
                        };
                        match NativeDownloadExecutor::start(plan) {
                            Ok(worker) => {
                                self.pending_easy_install = Some(intent);
                                self.show_download_progress(hwnd, worker);
                            }
                            Err(error) => log::error!("无法启动简易模式下载: {error}"),
                        }
                    }
                    Err(error) => log::warn!("简易模式输入不完整: {error}"),
                }
            }
        }
        let easy_mode_enabled = self.easy_mode_enabled();
        if let Some(page) = &mut self.easy_page {
            page.update(&self.easy_controller.view());
            if self.page != Page::Install
                || !easy_mode_enabled
                || self.advanced_visible
                || self.progress_visible
            {
                page.show(false);
            }
        }
    }

    unsafe fn activate_visible_tool_dialog(&self) -> bool {
        self.tool_dialogs
            .iter()
            .any(|dialog| dialog.shell.activate_if_visible())
            || self
                .mutating_tool_dialogs
                .iter()
                .any(|dialog| dialog.shell.activate_if_visible())
            || self
                .time_sync_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.shell.activate_if_visible())
            || self
                .network_reset_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.shell.activate_if_visible())
            || self
                .batch_format_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.shell.activate_if_visible())
            || self
                .storage_driver_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.shell.activate_if_visible())
            || self
                .password_reset_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.shell.activate_if_visible())
            || self
                .driver_transfer_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.activate_if_visible())
            || self
                .boot_repair_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.shell.activate_if_visible())
            || self
                .appx_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.shell.activate_if_visible())
            || self
                .nvidia_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.shell.activate_if_visible())
            || self
                .partition_copy_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.shell.activate_if_visible())
            || self
                .quick_partition_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.shell.activate_if_visible())
            || self
                .bitlocker_manage_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.shell.activate_if_visible())
            || self
                .expand_c_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.shell.activate_if_visible())
            || self
                .hardware_inspector_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.activate_if_visible())
    }

    unsafe fn handle_tool_intent(&mut self, hwnd: HWND, intent: ToolIntent) {
        let Some(action) =
            crate::core::native_tools_controller::NativeToolAction::from_native_index(
                intent as usize,
            )
        else {
            return;
        };
        let plan = crate::core::native_tools_controller::plan_tool(action);
        let environment = if crate::core::disk::DiskManager::is_pe_environment() {
            crate::core::native_tools_controller::ToolEnvironment::Pe
        } else {
            crate::core::native_tools_controller::ToolEnvironment::Desktop
        };
        if !plan.is_supported(environment) {
            log::warn!("当前环境不支持工具操作: {action:?}");
            return;
        }
        // Consume a pending Close/command result before deciding whether another tool may open.
        // This also prevents a just-hidden dialog from surviving until a later tool restarts the
        // polling timer.
        self.poll_tool_dialogs(hwnd);
        if self.activate_visible_tool_dialog() {
            return;
        }
        log::info!(
            "原生工具意图已安全路由: action={action:?}, route={:?}, safety={:?}",
            plan.route,
            plan.safety
        );
        if intent == ToolIntent::HardwareInspector {
            match NativeHardwareInspectorDialog::create(hwnd) {
                Ok(mut dialog) => {
                    dialog.show_modeless();
                    self.hardware_inspector_dialog = Some(dialog);
                    self.start_hardware_inspector(hwnd);
                }
                Err(error) => log::error!("创建详细硬件检测对话框失败: {error}"),
            }
            return;
        }
        if intent == ToolIntent::ExpandC {
            match NativeExpandCDialog::create(hwnd) {
                Ok(mut dialog) => {
                    dialog.show_modeless();
                    self.expand_c_dialog = Some(dialog);
                    self.start_expand_c_analysis(hwnd);
                }
                Err(error) => log::error!("创建无损扩大 C 盘对话框失败: {error}"),
            }
            return;
        }
        if matches!(intent, ToolIntent::RunGhost | ToolIntent::RunSpaceSniffer) {
            self.start_external_tool(hwnd, action);
            return;
        }
        if intent == ToolIntent::TimeSynchronization {
            match NativeTimeSyncDialog::create(hwnd) {
                Ok(mut dialog) => {
                    dialog.show_modeless();
                    self.time_sync_dialog = Some(dialog);
                    let _ = SetTimer(hwnd, TOOL_DIALOG_TIMER_ID, 100, None);
                }
                Err(error) => log::error!("创建系统时间校准对话框失败: {error}"),
            }
            return;
        }
        if intent == ToolIntent::ResetNetwork {
            match NativeNetworkResetDialog::create(hwnd) {
                Ok(mut dialog) => {
                    dialog.show_modeless();
                    self.network_reset_dialog = Some(dialog);
                    let _ = SetTimer(hwnd, TOOL_DIALOG_TIMER_ID, 100, None);
                }
                Err(error) => log::error!("创建网络重置对话框失败: {error}"),
            }
            return;
        }
        if intent == ToolIntent::BatchFormat {
            match NativeBatchFormatDialog::create(hwnd) {
                Ok(mut dialog) => {
                    dialog.show_modeless();
                    self.batch_format_dialog = Some(dialog);
                    self.start_batch_format_inventory();
                    let _ = SetTimer(hwnd, TOOL_DIALOG_TIMER_ID, 100, None);
                }
                Err(error) => log::error!("创建批量格式化对话框失败: {error}"),
            }
            return;
        }
        if intent == ToolIntent::ImportStorageDriver {
            match NativeStorageDriverDialog::create(hwnd) {
                Ok(mut dialog) => {
                    dialog.show_modeless();
                    self.storage_driver_dialog = Some(dialog);
                    self.start_storage_driver_inventory();
                    let _ = SetTimer(hwnd, TOOL_DIALOG_TIMER_ID, 100, None);
                }
                Err(error) => log::error!("创建存储控制器驱动导入对话框失败: {error}"),
            }
            return;
        }
        if intent == ToolIntent::ResetPassword {
            self.password_reset_generation = self.password_reset_generation.wrapping_add(1);
            let generation = self.password_reset_generation;
            let is_pe = self
                .config
                .system_info
                .as_ref()
                .is_some_and(|info| info.is_pe_environment);
            let targets = if is_pe {
                Vec::new()
            } else {
                vec![PasswordResetTargetOption {
                    target: crate::core::native_password_reset::PasswordResetTarget::CurrentSystem,
                    label: crate::tr!("当前系统（在线）"),
                }]
            };
            match NativePasswordResetDialog::create(hwnd, targets) {
                Ok(mut dialog) => {
                    dialog.show_modeless();
                    self.password_reset_dialog = Some(dialog);
                    self.start_password_reset_targets(generation);
                    let _ = SetTimer(hwnd, TOOL_DIALOG_TIMER_ID, 100, None);
                }
                Err(error) => log::error!("创建密码重置对话框失败: {error}"),
            }
            return;
        }
        if intent == ToolIntent::DriverBackupRestore {
            let state = crate::core::native_driver_transfer::DriverTransferState {
                inventory_loading: true,
                status: crate::tr!("正在检测 Windows 分区，请稍候"),
                ..Default::default()
            };
            match NativeDriverTransferDialog::create(hwnd, state) {
                Ok(mut dialog) => {
                    dialog.show_modeless();
                    self.driver_transfer_dialog = Some(dialog);
                    self.start_driver_transfer_inventory();
                    let _ = SetTimer(hwnd, TOOL_DIALOG_TIMER_ID, 100, None);
                }
                Err(error) => log::error!("创建驱动备份还原对话框失败: {error}"),
            }
            return;
        }
        if intent == ToolIntent::RepairBoot {
            self.boot_repair_generation = self.boot_repair_generation.wrapping_add(1);
            let generation = self.boot_repair_generation;
            match NativeBootRepairDialog::create(hwnd, Vec::new()) {
                Ok(mut dialog) => {
                    dialog.set_loading();
                    dialog.show_modeless();
                    self.boot_repair_dialog = Some(dialog);
                    self.start_boot_repair_inventory(generation);
                    let _ = SetTimer(hwnd, TOOL_DIALOG_TIMER_ID, 100, None);
                }
                Err(error) => log::error!("创建一键修复引导对话框失败: {error}"),
            }
            return;
        }
        if intent == ToolIntent::RemoveAppx {
            self.appx_generation = self.appx_generation.wrapping_add(1);
            let generation = self.appx_generation;
            let is_pe = self
                .config
                .system_info
                .as_ref()
                .is_some_and(|info| info.is_pe_environment);
            let state = crate::core::native_appx_selection::NativeAppxDialogState::loading(
                is_pe,
                crate::tr!("正在检测 Windows 系统..."),
            );
            match NativeAppxDialog::create(hwnd, state) {
                Ok(mut dialog) => {
                    dialog.show_modeless();
                    self.appx_dialog = Some(dialog);
                    self.start_appx_targets(generation, !is_pe);
                    let _ = SetTimer(hwnd, TOOL_DIALOG_TIMER_ID, 100, None);
                }
                Err(error) => log::error!("创建 APPX 移除对话框失败: {error}"),
            }
            return;
        }
        if intent == ToolIntent::NvidiaDriverRemoval {
            self.nvidia_generation = self.nvidia_generation.wrapping_add(1);
            let generation = self.nvidia_generation;
            let is_pe = self
                .config
                .system_info
                .as_ref()
                .is_some_and(|info| info.is_pe_environment);
            let targets = if is_pe {
                Vec::new()
            } else {
                vec![NvidiaRemovalTargetOption {
                    target: crate::core::native_nvidia_removal::NvidiaRemovalTarget::CurrentSystem,
                    label: crate::tr!("当前系统（在线）"),
                }]
            };
            match NativeNvidiaRemovalDialog::create(hwnd, targets) {
                Ok(mut dialog) => {
                    let _ = dialog.begin_initial_load();
                    dialog.show_modeless();
                    self.nvidia_dialog = Some(dialog);
                    self.start_nvidia_targets(generation, !is_pe);
                    self.start_nvidia_hardware(generation);
                    let _ = SetTimer(hwnd, TOOL_DIALOG_TIMER_ID, 100, None);
                }
                Err(error) => log::error!("创建 NVIDIA 驱动卸载对话框失败: {error}"),
            }
            return;
        }
        if intent == ToolIntent::PartitionCopy {
            self.partition_copy_generation = self.partition_copy_generation.wrapping_add(1);
            let generation = self.partition_copy_generation;
            match NativePartitionCopyDialog::create(hwnd) {
                Ok(mut dialog) => {
                    dialog.show_modeless();
                    self.partition_copy_dialog = Some(dialog);
                    self.start_partition_copy_inventory(hwnd, generation);
                    let _ = SetTimer(hwnd, TOOL_DIALOG_TIMER_ID, 100, None);
                }
                Err(error) => log::error!("创建分区对拷对话框失败: {error}"),
            }
            return;
        }
        if intent == ToolIntent::QuickPartition {
            let recommended_style =
                if self.config.system_info.as_ref().is_some_and(|info| {
                    info.boot_mode == crate::core::system_info::BootMode::Legacy
                }) {
                    crate::core::disk::PartitionStyle::MBR
                } else {
                    crate::core::disk::PartitionStyle::GPT
                };
            let used_drive_letters = self
                .partitions
                .iter()
                .filter_map(|partition| partition.letter.chars().next())
                .collect();
            let system_drive = std::env::var("SystemDrive")
                .ok()
                .and_then(|drive| drive.chars().next())
                .unwrap_or('C');
            match NativeQuickPartitionDialog::create(
                hwnd,
                recommended_style,
                used_drive_letters,
                system_drive,
            ) {
                Ok(mut dialog) => {
                    dialog.show_modeless();
                    self.quick_partition_dialog = Some(dialog);
                    self.start_quick_partition_inventory();
                    let _ = SetTimer(hwnd, TOOL_DIALOG_TIMER_ID, 100, None);
                }
                Err(error) => log::error!("创建一键分区对话框失败: {error}"),
            }
            return;
        }
        if intent == ToolIntent::ManageBitLocker {
            match NativeBitLockerManageDialog::create(hwnd) {
                Ok(mut dialog) => {
                    dialog.show_modeless();
                    self.bitlocker_manage_dialog = Some(dialog);
                    self.start_bitlocker_manage_inventory();
                    let _ = SetTimer(hwnd, TOOL_DIALOG_TIMER_ID, 100, None);
                }
                Err(error) => log::error!("创建 BitLocker 管理对话框失败: {error}"),
            }
            return;
        }
        let kind = match intent {
            ToolIntent::NetworkInformation => Some(ToolDialogKind::NetworkInformation),
            ToolIntent::SoftwareList => Some(ToolDialogKind::SoftwareList),
            ToolIntent::ReadGhoPassword => Some(ToolDialogKind::ReadGhoPassword),
            ToolIntent::VerifyImage => Some(ToolDialogKind::VerifyImage),
            ToolIntent::VerifyFileHash => Some(ToolDialogKind::VerifyFileHash),
            _ => None,
        };
        if let Some(kind) = kind {
            match NativeToolDialog::create(hwnd, kind) {
                Ok(mut dialog) => {
                    dialog.show_modeless();
                    self.tool_dialogs.push(dialog);
                    let _ = SetTimer(hwnd, TOOL_DIALOG_TIMER_ID, 100, None);
                    match kind {
                        ToolDialogKind::NetworkInformation => {
                            self.start_read_only_tool(kind, ReadOnlyToolRequest::NetworkInformation)
                        }
                        ToolDialogKind::SoftwareList => {
                            self.start_read_only_tool(kind, ReadOnlyToolRequest::InstalledSoftware)
                        }
                        _ => {}
                    }
                }
                Err(error) => log::error!("创建原生工具对话框失败: {error}"),
            }
        }
    }

    unsafe fn start_expand_c_analysis(&mut self, hwnd: HWND) {
        if let Some(dialog) = &mut self.expand_c_dialog {
            dialog.set_loading();
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(crate::core::native_expand_c_controller::analyze_expand_c());
        });
        self.expand_c_analysis = Some(receiver);
        let _ = SetTimer(hwnd, TOOL_DIALOG_TIMER_ID, 100, None);
    }

    unsafe fn start_hardware_inspector(&mut self, hwnd: HWND) {
        self.hardware_inspector_generation = self.hardware_inspector_generation.wrapping_add(1);
        let generation = self.hardware_inspector_generation;
        if let Some(dialog) = &mut self.hardware_inspector_dialog {
            dialog.set_loading();
        }
        let sender = self.tool_worker_sender.clone();
        std::thread::spawn(move || {
            let result =
                Box::new(crate::core::hardware_inspector::HardwareInspectorSnapshot::collect());
            let _ =
                sender.send(ToolWorkerMessage::HardwareInspectorCompleted { generation, result });
        });
        let _ = SetTimer(hwnd, TOOL_DIALOG_TIMER_ID, 100, None);
    }

    fn start_quick_partition_inventory(&self) {
        let sender = self.tool_worker_sender.clone();
        std::thread::spawn(move || {
            #[cfg(feature = "non-elevated-tests")]
            let result = Err(crate::tr!("开发测试构建已禁用物理磁盘读取。"));
            #[cfg(not(feature = "non-elevated-tests"))]
            let result = Ok(crate::core::quick_partition::get_physical_disks());
            let _ = sender.send(ToolWorkerMessage::QuickPartitionInventoryCompleted(result));
        });
    }

    fn start_quick_partition_resize(
        &self,
        request: crate::core::native_quick_partition_dialog::ExistingPartitionResizeRequest,
    ) {
        let sender = self.tool_worker_sender.clone();
        std::thread::spawn(move || {
            let result =
                crate::core::native_quick_partition_dialog::execute_existing_partition_resize(
                    &request,
                )
                .map(|outcome| outcome.message)
                .map_err(|error| error.to_string());
            let _ = sender.send(ToolWorkerMessage::QuickPartitionResizeCompleted(result));
        });
    }

    fn start_bitlocker_manage_inventory(&self) {
        let sender = self.tool_worker_sender.clone();
        std::thread::spawn(move || {
            let result = crate::core::native_bitlocker_manage::read_inventory()
                .map_err(|error| error.to_string());
            let _ = sender.send(ToolWorkerMessage::BitLockerManageInventoryCompleted(result));
        });
    }

    fn start_bitlocker_manage_operation(
        &self,
        intent: crate::core::native_bitlocker_manage::BitLockerManageIntent,
    ) {
        let recovery_key = matches!(
            intent,
            crate::core::native_bitlocker_manage::BitLockerManageIntent::ReadRecoveryKey { .. }
        );
        let sender = self.tool_worker_sender.clone();
        std::thread::spawn(move || {
            let result = crate::core::native_bitlocker_manage::execute_intent(intent);
            let _ = sender.send(ToolWorkerMessage::BitLockerManageOperationCompleted {
                recovery_key,
                result,
            });
        });
    }

    fn start_batch_format_inventory(&self) {
        let sender = self.tool_worker_sender.clone();
        std::thread::spawn(move || {
            let result = crate::core::native_batch_format::inventory_current()
                .map(|volumes| {
                    volumes
                        .into_iter()
                        .map(|volume| {
                            BatchFormatVolume::new(
                                volume.drive,
                                volume.label,
                                volume.file_system,
                                volume.total_size_mb,
                                volume.free_size_mb,
                            )
                        })
                        .collect()
                })
                .map_err(|error| error.to_string());
            let _ = sender.send(ToolWorkerMessage::BatchFormatInventoryCompleted(result));
        });
    }

    fn start_storage_driver_inventory(&self) {
        let sender = self.tool_worker_sender.clone();
        let partitions = self.partitions.clone();
        std::thread::spawn(move || {
            let result =
                crate::core::native_tool_inventory::load_windows_targets(&partitions, true)
                    .map(|entries| {
                        entries
                            .into_iter()
                            .skip(1)
                            .map(|entry| {
                                crate::core::native_storage_driver::StorageDriverTarget::new(
                                    entry.value,
                                    entry.label,
                                )
                            })
                            .collect()
                    })
                    .map_err(|error| error.to_string());
            let _ = sender.send(ToolWorkerMessage::StorageDriverTargetsCompleted(result));
        });
    }

    fn start_storage_driver_prepare(
        &self,
        request: crate::core::native_storage_driver::StorageDriverImportRequest,
    ) {
        let sender = self.tool_worker_sender.clone();
        std::thread::spawn(move || {
            #[cfg(feature = "non-elevated-tests")]
            let _ = &request;
            #[cfg(feature = "non-elevated-tests")]
            let result = Err(
                crate::core::native_storage_driver::StorageDriverImportError::DevelopmentBuildDenied
                    .to_string(),
            );

            #[cfg(not(feature = "non-elevated-tests"))]
            let result = (|| {
                let partitions = crate::core::disk::DiskManager::get_partitions()
                    .map_err(|error| error.to_string())?;
                let fresh_targets =
                    crate::core::native_tool_inventory::load_windows_targets(&partitions, true)
                        .map_err(|error| error.to_string())?
                        .into_iter()
                        .skip(1)
                        .map(|entry| {
                            crate::core::native_storage_driver::StorageDriverTarget::new(
                                entry.value,
                                entry.label,
                            )
                        })
                        .collect::<Vec<_>>();
                let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_owned());
                let plan = crate::core::native_storage_driver::prepare_current(
                    &request,
                    &fresh_targets,
                    &system_drive,
                )
                .map_err(|error| error.to_string())?;
                let hardware_ids = lr_core::driver::list_present_hardware_ids()
                    .map_err(|error| error.to_string())?;
                let packages =
                    lr_core::storage_driver_match::select_builtin_storage_driver_packages(
                        hardware_ids.iter().map(String::as_str),
                    );
                let [package] = packages.as_slice() else {
                    return Err(crate::tr!(
                        "未检测到唯一匹配的 Intel VMD 控制器，已拒绝导入随包存储驱动。"
                    ));
                };
                let directory = plan.driver_directory().join(package.directory_name());
                if !directory.is_dir() {
                    return Err(crate::tr!("匹配的 Intel VMD 驱动包不存在。"));
                }
                Ok(
                    super::tool_dialogs_mutating::MutatingToolIntent::ImportStorageDriver {
                        directory: directory.to_string_lossy().into_owned(),
                        offline_root: plan.target().to_owned(),
                        recursive: false,
                    },
                )
            })();

            let _ = sender.send(ToolWorkerMessage::StorageDriverPrepared(result));
        });
    }

    fn start_password_reset_targets(&self, generation: u64) {
        let sender = self.tool_worker_sender.clone();
        let partitions = self.partitions.clone();
        let include_current = !self
            .config
            .system_info
            .as_ref()
            .is_some_and(|info| info.is_pe_environment);
        std::thread::spawn(move || {
            let targets = if include_current {
                vec![PasswordResetTargetOption {
                    target: crate::core::native_password_reset::PasswordResetTarget::CurrentSystem,
                    label: crate::tr!("当前系统（在线）"),
                }]
            } else {
                Vec::new()
            };
            #[cfg(feature = "non-elevated-tests")]
            let result = {
                let _ = partitions;
                Ok(targets)
            };
            #[cfg(not(feature = "non-elevated-tests"))]
            let result = {
                let mut targets = targets;
                crate::core::native_tool_inventory::load_windows_targets(
                    &partitions,
                    include_current,
                )
                    .map(|entries| {
                        targets.extend(entries.into_iter().skip(usize::from(include_current)).map(|entry| {
                            PasswordResetTargetOption {
                                target: crate::core::native_password_reset::PasswordResetTarget::OfflineWindows(
                                    entry.value,
                                ),
                                label: entry.label,
                            }
                        }));
                        targets
                    })
                    .map_err(|error| error.to_string())
            };
            let _ = sender
                .send(ToolWorkerMessage::PasswordResetTargetsCompleted { generation, result });
        });
    }

    fn start_password_reset_accounts(
        &self,
        generation: u64,
        target: crate::core::native_password_reset::PasswordResetTarget,
    ) {
        let sender = self.tool_worker_sender.clone();
        std::thread::spawn(move || {
            let result = crate::core::native_password_reset::load_password_reset_accounts(&target)
                .map_err(|error| error.to_string());
            let _ = sender.send(ToolWorkerMessage::PasswordResetAccountsCompleted {
                generation,
                target,
                result,
            });
        });
    }

    fn start_password_reset_execution(
        &self,
        generation: u64,
        request: crate::core::native_password_reset::PasswordResetRequest,
    ) {
        let sender = self.tool_worker_sender.clone();
        std::thread::spawn(move || {
            let result = crate::core::native_password_reset::execute_password_reset(&request)
                .map_err(|error| error.to_string());
            let _ = sender.send(ToolWorkerMessage::PasswordResetCompleted {
                generation,
                request,
                result,
            });
        });
    }

    fn start_driver_transfer_inventory(&self) {
        let sender = self.tool_worker_sender.clone();
        let partitions = self.partitions.clone();
        std::thread::spawn(move || {
            let result =
                crate::core::native_tool_inventory::load_windows_targets(&partitions, false)
                    .map_err(|error| error.to_string());
            let _ = sender.send(ToolWorkerMessage::DriverTransferInventoryCompleted(result));
        });
    }

    fn start_boot_repair_inventory(&self, generation: u64) {
        let sender = self.tool_worker_sender.clone();
        let partitions = self.partitions.clone();
        std::thread::spawn(move || {
            let result = crate::core::native_tool_inventory::load_boot_repair_targets(&partitions)
                .map_err(|error| error.to_string());
            let _ =
                sender.send(ToolWorkerMessage::BootRepairTargetsCompleted { generation, result });
        });
    }

    fn start_boot_repair_execution(
        &self,
        generation: u64,
        request: crate::core::native_boot_repair::BootRepairRequest,
    ) {
        let sender = self.tool_worker_sender.clone();
        std::thread::spawn(move || {
            #[cfg(feature = "non-elevated-tests")]
            let result = {
                let _ = request;
                Err(crate::tr!("开发测试构建已禁用引导修复执行。"))
            };
            #[cfg(not(feature = "non-elevated-tests"))]
            let result = (|| {
                let partitions = crate::core::disk::DiskManager::get_partitions()
                    .map_err(|error| error.to_string())?;
                let fresh_targets =
                    crate::core::native_tool_inventory::load_boot_repair_targets(&partitions)
                        .map_err(|error| error.to_string())?;
                let plan = match plan_execution(ToolExecutionRequest::NativeAction {
                    action: crate::core::native_tools_controller::NativeToolAction::RepairBoot,
                    confirmed: true,
                }) {
                    ToolExecutionPlan::Mutating(plan) => plan,
                    _ => return Err(crate::tr!("工具执行计划与对话框不匹配。")),
                };
                let backend_request = crate::core::native_boot_repair::build_backend_request(
                    plan,
                    &request,
                    &fresh_targets,
                )
                .map_err(|error| error.to_string())?;
                let result = NativeToolBackend::execute(&backend_request)
                    .map_err(|error| error.to_string())?;
                format_tool_backend_result(result)
            })();
            let _ = sender.send(ToolWorkerMessage::BootRepairCompleted { generation, result });
        });
    }

    fn start_appx_targets(&self, generation: u64, include_current: bool) {
        let sender = self.tool_worker_sender.clone();
        let partitions = self.partitions.clone();
        std::thread::spawn(move || {
            #[cfg(feature = "non-elevated-tests")]
            let result = {
                let _ = partitions;
                Ok(if include_current {
                    vec![crate::core::native_tool_inventory::InventoryEntry {
                        value: "当前系统".to_owned(),
                        label: crate::tr!("当前系统"),
                        disk_fingerprint: None,
                    }]
                } else {
                    Vec::new()
                })
            };
            #[cfg(not(feature = "non-elevated-tests"))]
            let result = crate::core::native_tool_inventory::load_windows_targets(
                &partitions,
                include_current,
            )
            .map_err(|error| error.to_string());
            let _ = sender.send(ToolWorkerMessage::AppxTargetsCompleted { generation, result });
        });
    }

    fn start_appx_packages(&mut self, target: String) {
        self.appx_generation = self.appx_generation.wrapping_add(1);
        let generation = self.appx_generation;
        let sender = self.tool_worker_sender.clone();
        std::thread::spawn(move || {
            let result = crate::core::native_tool_inventory::load_dynamic(
                crate::core::native_tool_inventory::DynamicInventoryKind::RemoveAppxPackages,
                &target,
            )
            .map_err(|error| error.to_string());
            let _ = sender.send(ToolWorkerMessage::AppxPackagesCompleted {
                generation,
                target,
                result,
            });
        });
    }

    fn start_nvidia_targets(&self, generation: u64, include_current: bool) {
        let sender = self.tool_worker_sender.clone();
        let partitions = self.partitions.clone();
        std::thread::spawn(move || {
            let targets = if include_current {
                vec![NvidiaRemovalTargetOption {
                    target: crate::core::native_nvidia_removal::NvidiaRemovalTarget::CurrentSystem,
                    label: crate::tr!("当前系统（在线）"),
                }]
            } else {
                Vec::new()
            };
            #[cfg(feature = "non-elevated-tests")]
            let result = {
                let _ = partitions;
                Ok(targets)
            };
            #[cfg(not(feature = "non-elevated-tests"))]
            let result = {
                let mut targets = targets;
                crate::core::native_tool_inventory::load_windows_targets(
                    &partitions,
                    include_current,
                )
                    .map(|entries| {
                        targets.extend(entries.into_iter().skip(usize::from(include_current)).map(|entry| {
                            NvidiaRemovalTargetOption {
                                target: crate::core::native_nvidia_removal::NvidiaRemovalTarget::OfflineWindows(
                                    entry.value,
                                ),
                                label: entry.label,
                            }
                        }));
                        targets
                    })
                    .map_err(|error| error.to_string())
            };
            let _ = sender.send(ToolWorkerMessage::NvidiaTargetsCompleted { generation, result });
        });
    }

    fn start_nvidia_hardware(&self, generation: u64) {
        let sender = self.tool_worker_sender.clone();
        std::thread::spawn(move || {
            let result = crate::core::native_nvidia_removal::load_hardware_report()
                .map_err(|error| error.to_string());
            let _ = sender.send(ToolWorkerMessage::NvidiaHardwareCompleted { generation, result });
        });
    }

    fn start_nvidia_removal(
        &self,
        generation: u64,
        request: crate::core::native_nvidia_removal::NvidiaRemovalRequest,
    ) {
        let sender = self.tool_worker_sender.clone();
        std::thread::spawn(move || {
            let result = (|| {
                #[cfg(not(feature = "non-elevated-tests"))]
                if let crate::core::native_nvidia_removal::NvidiaRemovalTarget::OfflineWindows(
                    root,
                ) = &request.target
                {
                    let partitions = crate::core::disk::DiskManager::get_partitions()
                        .map_err(|error| error.to_string())?;
                    let targets = crate::core::native_tool_inventory::load_windows_targets(
                        &partitions,
                        false,
                    )
                    .map_err(|error| error.to_string())?;
                    if !targets
                        .iter()
                        .any(|entry| entry.value.eq_ignore_ascii_case(root))
                    {
                        return Err(crate::tr!("所选系统分区已不可用，请重新选择"));
                    }
                }
                let plan = match plan_execution(ToolExecutionRequest::NativeAction {
                    action:
                        crate::core::native_tools_controller::NativeToolAction::NvidiaDriverRemoval,
                    confirmed: true,
                }) {
                    ToolExecutionPlan::Mutating(plan) => plan,
                    _ => return Err(crate::tr!("工具执行计划与对话框不匹配。")),
                };
                let backend_request =
                    crate::core::native_nvidia_removal::build_backend_request(&request, plan)
                        .map_err(|error| error.to_string())?;
                let result = NativeToolBackend::execute(&backend_request)
                    .map_err(|error| error.to_string())?;
                format_tool_backend_result(result)
            })();
            let _ = sender.send(ToolWorkerMessage::NvidiaRemovalCompleted { generation, result });
        });
    }

    fn start_partition_copy_inventory(&self, hwnd: HWND, generation: u64) {
        let sender = self.tool_worker_sender.clone();
        let window = hwnd.0 as usize;
        std::thread::spawn(move || {
            let result = crate::core::native_partition_copy::read_inventory()
                .map(|items| {
                    items
                        .into_iter()
                        .map(PartitionCopyInventoryRow::from)
                        .collect()
                })
                .map_err(|error| error.to_string());
            if sender
                .send(ToolWorkerMessage::PartitionCopyInventoryCompleted { generation, result })
                .is_ok()
            {
                unsafe {
                    let _ = PostMessageW(
                        HWND(window as *mut _),
                        WM_TOOL_WORKER_READY,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            }
        });
    }

    fn start_partition_copy_resume_check(
        &self,
        generation: u64,
        request: crate::core::native_partition_copy::PartitionCopyRequest,
    ) {
        let sender = self.tool_worker_sender.clone();
        std::thread::spawn(move || {
            let result = crate::core::native_partition_copy::validate_current(&request)
                .map(|plan| plan.resume())
                .map_err(|error| error.to_string());
            let _ =
                sender.send(ToolWorkerMessage::PartitionCopyResumeChecked { generation, result });
        });
    }

    fn start_partition_copy_execution(
        &self,
        generation: u64,
        request: crate::core::native_partition_copy::PartitionCopyRequest,
    ) {
        let sender = self.tool_worker_sender.clone();
        std::thread::spawn(move || {
            let result = (|| {
                match plan_execution(ToolExecutionRequest::NativeAction {
                    action: crate::core::native_tools_controller::NativeToolAction::PartitionCopy,
                    confirmed: true,
                }) {
                    ToolExecutionPlan::Mutating(_) => {}
                    _ => return Err(crate::tr!("工具执行计划与对话框不匹配。")),
                }
                let plan = crate::core::native_partition_copy::validate_current(&request)
                    .map_err(|error| error.to_string())?;
                crate::core::native_partition_copy::execute_with_progress(&plan, |progress| {
                    let _ = sender.send(ToolWorkerMessage::PartitionCopyProgress {
                        generation,
                        progress: progress.clone(),
                    });
                })
                .map_err(|error| error.to_string())
            })();
            let _ = sender.send(ToolWorkerMessage::PartitionCopyCompleted { generation, result });
        });
    }

    unsafe fn start_expand_c_execution(&mut self, _hwnd: HWND, request: ExpandCRequest) {
        let Some(handles) = self.handles else { return };
        let pe = self.available_pe();
        let selected = usize::try_from(SendMessageW(handles.pe, 0x0147, WPARAM(0), LPARAM(0)).0)
            .ok()
            .or_else(|| (pe.len() == 1).then_some(0));
        let Some(pe) = selected.and_then(|index| pe.get(index)).cloned() else {
            if let Some(dialog) = &mut self.expand_c_dialog {
                dialog.set_error(crate::tr!("未选择 PE 环境，无法扩容"));
            }
            return;
        };
        #[cfg(not(feature = "non-elevated-tests"))]
        match crate::core::pe::PeManager::check_cached_pe(
            &pe.filename,
            pe.sha256.as_deref(),
            pe.md5.as_deref(),
        ) {
            Ok(lr_core::cached_artifact::CachedArtifactStatus::Missing) => {
                let integrity = match lr_core::download_integrity::select_expected_hash(
                    pe.sha256.as_deref(),
                    pe.md5.as_deref(),
                ) {
                    Ok(value) => value,
                    Err(error) => {
                        if let Some(dialog) = &mut self.expand_c_dialog {
                            dialog.set_error(crate::tr!("PE 校验配置无效：{}", error));
                        }
                        return;
                    }
                };
                let plan = crate::core::native_download_controller::DownloadPlan {
                    url: pe.download_url.clone(),
                    save_directory: crate::utils::path::get_pe_download_cache_dir(),
                    filename: pe.filename.clone(),
                    integrity,
                    completion: crate::core::native_download_controller::DownloadCompletion::None,
                    download_threads: self.app_config.download_threads,
                };
                match NativeDownloadExecutor::start(plan) {
                    Ok(worker) => {
                        self.pending_expand_after_pe_download = Some(request);
                        if let Some(dialog) = &self.expand_c_dialog {
                            let _ = ShowWindow(dialog.shell.hwnd(), SW_HIDE);
                        }
                        self.show_download_progress(_hwnd, worker);
                    }
                    Err(error) => {
                        if let Some(dialog) = &mut self.expand_c_dialog {
                            dialog.set_error(crate::tr!("无法下载所需 PE 环境：{}", error));
                        }
                    }
                }
                return;
            }
            Ok(lr_core::cached_artifact::CachedArtifactStatus::Ready { .. }) => {}
            Err(error) => {
                if let Some(dialog) = &mut self.expand_c_dialog {
                    dialog.set_error(crate::tr!("PE 文件安全校验失败：{}", error));
                }
                return;
            }
        }
        let handoff = ExpandCHandoffRequest {
            target_size_mb: request.target_size_mb,
            use_maximum: request.use_maximum,
            analyzed_current_size_mb: request.analyzed_current_size_mb,
            analyzed_max_size_mb: request.analyzed_max_size_mb,
            analyzed_no_move_max_mb: request.analyzed_no_move_max_mb,
            wim_engine: self.app_config.wim_engine,
            pe,
        };
        match start_expand_c_handoff(handoff) {
            Ok(receiver) => {
                if let Some(dialog) = &mut self.expand_c_dialog {
                    dialog.set_executing(true, crate::tr!("正在准备扩容环境..."));
                }
                self.expand_c_execution = Some(receiver);
            }
            Err(error) => {
                if let Some(dialog) = &mut self.expand_c_dialog {
                    dialog.set_error(error.to_string());
                }
            }
        }
    }

    unsafe fn start_external_tool(
        &mut self,
        hwnd: HWND,
        action: crate::core::native_tools_controller::NativeToolAction,
    ) {
        let plan = plan_execution(ToolExecutionRequest::NativeAction {
            action,
            // The legacy button click itself is the explicit request to launch the bundled tool.
            confirmed: true,
        });
        let ToolExecutionPlan::External(external) = plan else {
            if let Some(page) = &self.tools_page {
                set_text(page.introduction, &crate::tr!("无法生成外部工具启动计划。"));
            }
            return;
        };
        if let Some(page) = &self.tools_page {
            set_text(page.introduction, &crate::tr!("正在启动工具..."));
        }
        self.tool_background_jobs = self.tool_background_jobs.saturating_add(1);
        let _ = SetTimer(hwnd, TOOL_DIALOG_TIMER_ID, 100, None);
        let sender = self.tool_worker_sender.clone();
        std::thread::spawn(move || {
            let request = NativeToolBackendRequest::External(external);
            let result = NativeToolBackend::execute(&request)
                .map(format_tool_backend_result)
                .unwrap_or_else(|error| Err(error.to_string()));
            let _ = sender.send(ToolWorkerMessage::ExternalCompleted(action, result));
        });
    }

    unsafe fn start_read_only_tool(&mut self, kind: ToolDialogKind, request: ReadOnlyToolRequest) {
        if let Some(dialog) = self
            .tool_dialogs
            .iter_mut()
            .find(|dialog| dialog.kind() == kind)
        {
            match &request {
                ReadOnlyToolRequest::NetworkInformation => {
                    dialog.set_network_state(&super::tool_dialogs::NetworkInformationState {
                        loading: true,
                        ..Default::default()
                    })
                }
                ReadOnlyToolRequest::InstalledSoftware => {
                    dialog.set_software_state(&super::tool_dialogs::SoftwareListState {
                        loading: true,
                        ..Default::default()
                    })
                }
                ReadOnlyToolRequest::GhoPassword { path } => {
                    dialog.set_gho_password_state(&super::tool_dialogs::GhoPasswordState {
                        path: path.clone(),
                        reading: true,
                        ..Default::default()
                    })
                }
                ReadOnlyToolRequest::VerifyImage { path } => dialog.set_image_verification_state(
                    &super::tool_dialogs::ImageVerificationState {
                        path: path.clone(),
                        verifying: true,
                        ..Default::default()
                    },
                ),
                ReadOnlyToolRequest::Sha256 { path, expected } => {
                    dialog.set_file_hash_state(&super::tool_dialogs::FileHashState {
                        path: path.clone(),
                        expected: expected.clone(),
                        verifying: true,
                        ..Default::default()
                    })
                }
            }
            dialog.show_modeless();
        }

        let cancel = if matches!(request, ReadOnlyToolRequest::VerifyImage { .. }) {
            if let Some(previous) = self.image_verify_cancel.take() {
                previous.store(true, Ordering::SeqCst);
            }
            let flag = Arc::new(AtomicBool::new(false));
            self.image_verify_cancel = Some(Arc::clone(&flag));
            Some(flag)
        } else {
            None
        };
        let sender = self.tool_worker_sender.clone();
        std::thread::spawn(move || {
            let plan = crate::core::native_tool_executor::plan_execution(
                ToolExecutionRequest::ReadOnly(request.clone()),
            );
            let progress_sender = sender.clone();
            let progress_request = request.clone();
            let mut reporter = move |event| {
                let _ = progress_sender.send(ToolWorkerMessage::Progress(
                    kind,
                    progress_request.clone(),
                    event,
                ));
            };
            let result =
                NativeToolExecutor::execute_read_only_with_cancel(&plan, &mut reporter, cancel)
                    .map_err(|error| error.to_string());
            let _ = sender.send(ToolWorkerMessage::Completed(kind, request, result));
        });
    }

    unsafe fn poll_tool_worker_messages(&mut self, hwnd: HWND) {
        let messages: Vec<_> = self.tool_worker_messages.try_iter().collect();
        for message in messages {
            match message {
                ToolWorkerMessage::Progress(
                    kind,
                    request,
                    ToolExecutionEvent::Progress { percentage, detail },
                ) => {
                    if let Some(dialog) = self
                        .tool_dialogs
                        .iter_mut()
                        .find(|dialog| dialog.kind() == kind)
                    {
                        match kind {
                            ToolDialogKind::VerifyImage => dialog.set_image_verification_state(
                                &super::tool_dialogs::ImageVerificationState {
                                    path: read_only_request_path(&request).to_owned(),
                                    verifying: true,
                                    percentage,
                                    result: detail,
                                    ..Default::default()
                                },
                            ),
                            ToolDialogKind::VerifyFileHash => {
                                dialog.set_file_hash_state(&super::tool_dialogs::FileHashState {
                                    path: read_only_request_path(&request).to_owned(),
                                    expected: read_only_expected_hash(&request).to_owned(),
                                    verifying: true,
                                    percentage,
                                    result: detail,
                                    ..Default::default()
                                })
                            }
                            _ => {}
                        }
                    }
                }
                ToolWorkerMessage::Completed(kind, request, result) => {
                    if kind == ToolDialogKind::VerifyImage {
                        self.image_verify_cancel = None;
                    }
                    let Some(dialog) = self
                        .tool_dialogs
                        .iter_mut()
                        .find(|dialog| dialog.kind() == kind)
                    else {
                        continue;
                    };
                    apply_tool_result(dialog, &request, result);
                    dialog.show_modeless();
                }
                ToolWorkerMessage::MutatingCompleted(kind, result) => {
                    self.tool_background_jobs = self.tool_background_jobs.saturating_sub(1);
                    if let Some(dialog) = self
                        .mutating_tool_dialogs
                        .iter_mut()
                        .find(|dialog| dialog.kind() == kind)
                    {
                        let mut state = dialog.state().clone();
                        state.loading = false;
                        state.status =
                            result.unwrap_or_else(|error| crate::tr!("操作失败：{}", error));
                        dialog.set_state(state);
                        dialog.show_modeless();
                    } else if let Some(page) = &self.tools_page {
                        set_text(
                            page.introduction,
                            &result.unwrap_or_else(|error| crate::tr!("操作失败：{}", error)),
                        );
                    }
                }
                ToolWorkerMessage::ExternalCompleted(action, result) => {
                    self.tool_background_jobs = self.tool_background_jobs.saturating_sub(1);
                    if let Some(page) = &self.tools_page {
                        let message = match result {
                            Ok(message) if !message.trim().is_empty() => message,
                            Ok(_) => crate::tr!("工具已启动。"),
                            Err(error) => crate::tr!("工具启动失败：{}", error),
                        };
                        set_text(page.introduction, &message);
                    }
                    log::info!("外部工具启动结果: action={action:?}");
                }
                ToolWorkerMessage::BitLockerGateCompleted { drive, result } => {
                    self.handle_bitlocker_gate_completed(hwnd, drive, result);
                }
                ToolWorkerMessage::DynamicInventoryCompleted {
                    kind,
                    target,
                    generation,
                    result,
                } => {
                    if let Some(dialog) = self
                        .mutating_tool_dialogs
                        .iter_mut()
                        .find(|dialog| dialog.kind() == kind)
                    {
                        dialog.apply_dynamic_inventory(&target, generation, result);
                        dialog.show_modeless();
                    }
                }
                ToolWorkerMessage::FirstChoiceInventoryCompleted { kind, result } => {
                    let target = if let Some(dialog) = self
                        .mutating_tool_dialogs
                        .iter_mut()
                        .find(|dialog| dialog.kind() == kind)
                    {
                        dialog
                            .apply_first_choice_inventory(result, &crate::tr!("未找到可用目标。"));
                        let target = dialog.begin_dynamic_inventory_load();
                        dialog.show_modeless();
                        target
                    } else {
                        None
                    };
                    if let Some((target, generation)) = target {
                        self.start_dynamic_tool_inventory(kind, target, generation);
                    }
                }
                ToolWorkerMessage::BatchFormatInventoryCompleted(result) => {
                    if let Some(dialog) = &mut self.batch_format_dialog {
                        dialog.set_inventory(result);
                        dialog.show_modeless();
                    }
                }
                ToolWorkerMessage::StorageDriverTargetsCompleted(result) => {
                    if let Some(dialog) = &mut self.storage_driver_dialog {
                        dialog.set_targets(result);
                        dialog.show_modeless();
                    }
                }
                ToolWorkerMessage::StorageDriverPrepared(result) => match result {
                    Ok(execution) => {
                        self.storage_driver_dialog = None;
                        self.start_confirmed_tool(
                            MutatingToolKind::ImportStorageDriver,
                            &execution,
                        );
                    }
                    Err(error) => {
                        if let Some(dialog) = &mut self.storage_driver_dialog {
                            dialog.set_targets(Err(error));
                            dialog.show_modeless();
                        }
                    }
                },
                ToolWorkerMessage::PasswordResetTargetsCompleted { generation, result } => {
                    if generation != self.password_reset_generation {
                        continue;
                    }
                    let next = if let Some(dialog) = &mut self.password_reset_dialog {
                        match result {
                            Ok(targets) => dialog.apply_targets(targets),
                            Err(error) => {
                                dialog.set_operation_result(crate::tr!(
                                    "读取 Windows 系统列表失败：{}",
                                    error
                                ));
                                None
                            }
                        }
                    } else {
                        None
                    };
                    if let Some(PasswordResetDialogIntent::LoadAccounts(target)) = next {
                        self.start_password_reset_accounts(generation, target);
                    }
                    if let Some(dialog) = &mut self.password_reset_dialog {
                        dialog.show_modeless();
                    }
                }
                ToolWorkerMessage::PasswordResetAccountsCompleted {
                    generation,
                    target,
                    result,
                } => {
                    if generation == self.password_reset_generation {
                        if let Some(dialog) = &mut self.password_reset_dialog {
                            dialog.apply_accounts(&target, result);
                            dialog.show_modeless();
                        }
                    }
                }
                ToolWorkerMessage::PasswordResetCompleted {
                    generation,
                    request,
                    result,
                } => {
                    if generation == self.password_reset_generation {
                        if let Some(dialog) = &mut self.password_reset_dialog {
                            let message = match result {
                                Ok(_) => crate::tr!(
                                    "账户“{}”的密码已清空，账户已启用。",
                                    request.account
                                ),
                                Err(error) => crate::tr!("密码重置失败：{}", error),
                            };
                            dialog.set_operation_result(message);
                            dialog.show_modeless();
                        }
                    }
                }
                ToolWorkerMessage::DriverTransferInventoryCompleted(result) => {
                    if let Some(dialog) = &mut self.driver_transfer_dialog {
                        let mut state = dialog.state().clone();
                        state.inventory_loading = false;
                        state.selected_windows = None;
                        match result {
                            Ok(targets) => {
                                state.windows_targets = targets;
                                state.selected_windows = state
                                    .windows_targets
                                    .first()
                                    .map(|target| target.value.clone());
                                state.status = if state.windows_targets.is_empty() {
                                    crate::tr!("未检测到包含 Windows 的分区")
                                } else {
                                    String::new()
                                };
                            }
                            Err(error) => {
                                state.windows_targets.clear();
                                state.status = crate::tr!("读取 Windows 系统列表失败：{}", error);
                            }
                        }
                        dialog.set_state(state);
                        dialog.show_modeless();
                    }
                }
                ToolWorkerMessage::BootRepairTargetsCompleted { generation, result } => {
                    if generation == self.boot_repair_generation {
                        if let Some(dialog) = &mut self.boot_repair_dialog {
                            match result {
                                Ok(targets) => dialog.apply_targets(targets),
                                Err(error) => dialog
                                    .set_status(crate::tr!("读取 Windows 系统列表失败：{}", error)),
                            }
                            dialog.show_modeless();
                        }
                    }
                }
                ToolWorkerMessage::BootRepairCompleted { generation, result } => {
                    if generation == self.boot_repair_generation {
                        if let Some(dialog) = &mut self.boot_repair_dialog {
                            dialog.set_status(match result {
                                Ok(message) => message,
                                Err(error) => crate::tr!("引导修复失败：{}", error),
                            });
                            dialog.show_modeless();
                        }
                    }
                }
                ToolWorkerMessage::AppxTargetsCompleted { generation, result } => {
                    if generation == self.appx_generation {
                        let load = if let Some(dialog) = &mut self.appx_dialog {
                            let load = dialog.set_targets(result);
                            dialog.show_modeless();
                            load
                        } else {
                            None
                        };
                        if let Some(
                            crate::core::native_appx_selection::NativeAppxDialogIntent::LoadPackages {
                                inventory_target,
                            },
                        ) = load
                        {
                            self.start_appx_packages(inventory_target);
                        }
                    }
                }
                ToolWorkerMessage::AppxPackagesCompleted {
                    generation,
                    target,
                    result,
                } => {
                    if generation == self.appx_generation {
                        if let Some(dialog) = &mut self.appx_dialog {
                            let _ = dialog.set_packages(&target, result);
                            dialog.show_modeless();
                        }
                    }
                }
                ToolWorkerMessage::NvidiaTargetsCompleted { generation, result } => {
                    if generation == self.nvidia_generation {
                        if let Some(dialog) = &mut self.nvidia_dialog {
                            match result {
                                Ok(targets) => dialog.apply_targets(targets),
                                Err(error) => dialog.set_operation_result(crate::tr!(
                                    "读取 Windows 系统列表失败：{}",
                                    error
                                )),
                            }
                            dialog.show_modeless();
                        }
                    }
                }
                ToolWorkerMessage::NvidiaHardwareCompleted { generation, result } => {
                    if generation == self.nvidia_generation {
                        if let Some(dialog) = &mut self.nvidia_dialog {
                            dialog.apply_hardware_report(result);
                            dialog.show_modeless();
                        }
                    }
                }
                ToolWorkerMessage::NvidiaRemovalCompleted { generation, result } => {
                    if generation == self.nvidia_generation {
                        if let Some(dialog) = &mut self.nvidia_dialog {
                            dialog.set_operation_result(match result {
                                Ok(message) => message,
                                Err(error) => crate::tr!("NVIDIA 驱动卸载失败：{}", error),
                            });
                            dialog.show_modeless();
                        }
                    }
                }
                ToolWorkerMessage::PartitionCopyInventoryCompleted { generation, result } => {
                    if generation == self.partition_copy_generation {
                        if let Some(dialog) = &mut self.partition_copy_dialog {
                            dialog.set_inventory(result);
                            dialog.show_modeless();
                        }
                    }
                }
                ToolWorkerMessage::PartitionCopyResumeChecked { generation, result } => {
                    if generation == self.partition_copy_generation {
                        if let Some(dialog) = &mut self.partition_copy_dialog {
                            dialog.set_resume_state(match result {
                                Ok(true) => PartitionCopyResumeState::Resumable,
                                Ok(false) => PartitionCopyResumeState::NewCopy,
                                Err(error) => PartitionCopyResumeState::Unavailable(error),
                            });
                            dialog.show_modeless();
                        }
                    }
                }
                ToolWorkerMessage::PartitionCopyProgress {
                    generation,
                    progress,
                } => {
                    if generation == self.partition_copy_generation {
                        if let Some(dialog) = &mut self.partition_copy_dialog {
                            dialog.apply_progress(progress);
                        }
                    }
                }
                ToolWorkerMessage::PartitionCopyCompleted { generation, result } => {
                    if generation == self.partition_copy_generation {
                        if let Some(dialog) = &mut self.partition_copy_dialog {
                            let progress = match result {
                                Ok(result) => {
                                    crate::core::native_partition_copy::PartitionCopyProgress {
                                        current_file: result.message,
                                        copied_count: result.copied_count,
                                        total_count: result.total_count,
                                        skipped_count: result.skipped_count,
                                        failed_count: result.failed_count,
                                        failed_files: result.failed_files,
                                        completed: true,
                                        error: None,
                                    }
                                }
                                Err(error) => {
                                    crate::core::native_partition_copy::PartitionCopyProgress {
                                        current_file: String::new(),
                                        completed: true,
                                        error: Some(error),
                                        ..Default::default()
                                    }
                                }
                            };
                            dialog.apply_progress(progress);
                            dialog.set_copying(false);
                            dialog.show_modeless();
                        }
                    }
                }
                ToolWorkerMessage::QuickPartitionInventoryCompleted(result) => {
                    if let Some(dialog) = &mut self.quick_partition_dialog {
                        dialog.set_inventory(result);
                        dialog.show_modeless();
                    }
                }
                ToolWorkerMessage::QuickPartitionResizeCompleted(result) => {
                    if let Some(page) = &self.tools_page {
                        set_text(
                            page.introduction,
                            &result.unwrap_or_else(|error| crate::tr!("调整分区失败：{}", error)),
                        );
                    }
                    self.start_quick_partition_inventory();
                }
                ToolWorkerMessage::BitLockerManageInventoryCompleted(result) => {
                    if let Some(dialog) = &mut self.bitlocker_manage_dialog {
                        dialog.set_inventory(result);
                        dialog.show_modeless();
                    }
                }
                ToolWorkerMessage::BitLockerManageOperationCompleted {
                    recovery_key,
                    result,
                } => {
                    if let Some(dialog) = &mut self.bitlocker_manage_dialog {
                        if recovery_key {
                            dialog.set_recovery_key(result);
                        } else {
                            dialog.set_operation_result(
                                result.unwrap_or_else(|error| crate::tr!("操作失败：{}", error)),
                            );
                        }
                        dialog.show_modeless();
                    }
                    if !recovery_key {
                        self.start_bitlocker_manage_inventory();
                    }
                }
                ToolWorkerMessage::HardwareInspectorCompleted { generation, result } => {
                    if generation == self.hardware_inspector_generation {
                        if let Some(dialog) = &mut self.hardware_inspector_dialog {
                            dialog.apply_snapshot(*result);
                            dialog.show_modeless();
                        }
                    }
                }
            }
        }
    }

    fn start_dynamic_tool_inventory(
        &self,
        kind: MutatingToolKind,
        target: String,
        generation: u64,
    ) {
        let inventory_kind = match kind {
            MutatingToolKind::ResetPassword => {
                crate::core::native_tool_inventory::DynamicInventoryKind::ResetPasswordAccounts
            }
            MutatingToolKind::RemoveAppx => {
                crate::core::native_tool_inventory::DynamicInventoryKind::RemoveAppxPackages
            }
            MutatingToolKind::NvidiaDriverRemoval => {
                crate::core::native_tool_inventory::DynamicInventoryKind::NvidiaDevices
            }
            _ => return,
        };
        let sender = self.tool_worker_sender.clone();
        std::thread::spawn(move || {
            let result = crate::core::native_tool_inventory::load_dynamic(inventory_kind, &target)
                .map_err(|error| error.to_string());
            let _ = sender.send(ToolWorkerMessage::DynamicInventoryCompleted {
                kind,
                target,
                generation,
                result,
            });
        });
    }

    fn start_first_choice_inventory(&self, kind: MutatingToolKind, include_current: bool) {
        let sender = self.tool_worker_sender.clone();
        let partitions = self.partitions.clone();
        std::thread::spawn(move || {
            let result = if kind == MutatingToolKind::QuickPartition {
                crate::core::native_tool_inventory::load_physical_disks()
            } else {
                crate::core::native_tool_inventory::load_windows_targets(
                    &partitions,
                    include_current,
                )
            }
            .map_err(|error| error.to_string());
            let _ = sender.send(ToolWorkerMessage::FirstChoiceInventoryCompleted { kind, result });
        });
    }

    unsafe fn start_confirmed_tool(
        &mut self,
        kind: MutatingToolKind,
        execution: &super::tool_dialogs_mutating::MutatingToolIntent,
    ) {
        let request = match confirmed_tool_backend_request(kind, execution) {
            Ok(request) => request,
            Err(error) => {
                if let Some(dialog) = self
                    .mutating_tool_dialogs
                    .iter_mut()
                    .find(|dialog| dialog.kind() == kind)
                {
                    let mut state = dialog.state().clone();
                    state.status = error;
                    dialog.set_state(state);
                    dialog.show_modeless();
                }
                return;
            }
        };
        if let Some(dialog) = self
            .mutating_tool_dialogs
            .iter_mut()
            .find(|dialog| dialog.kind() == kind)
        {
            let mut state = dialog.state().clone();
            state.loading = true;
            state.status = crate::tr!("正在执行已确认的操作...");
            dialog.set_state(state);
            dialog.show_modeless();
        }
        self.tool_background_jobs = self.tool_background_jobs.saturating_add(1);
        let sender = self.tool_worker_sender.clone();
        std::thread::spawn(move || {
            let result = match NativeToolBackend::execute(&request) {
                Ok(result) => format_tool_backend_result(result),
                Err(error) => Err(error.to_string()),
            };
            let _ = sender.send(ToolWorkerMessage::MutatingCompleted(kind, result));
        });
    }

    unsafe fn begin_bitlocker_gate(
        &mut self,
        hwnd: HWND,
        intent: PendingBitLockerIntent,
        locked_volumes: Vec<String>,
    ) {
        let Some(current_drive) = locked_volumes.first().cloned() else {
            self.continue_pending_bitlocker_intent(hwnd, intent);
            return;
        };

        // A gate owns the single ManageBitLocker dialog while it is pending. This prevents a
        // toolbox dialog result from being mistaken for an install/backup unlock result.
        self.mutating_tool_dialogs
            .retain(|dialog| dialog.kind() != MutatingToolKind::ManageBitLocker);
        self.pending_bitlocker_gate = Some(PendingBitLockerGate {
            intent,
            current_drive: current_drive.clone(),
        });

        match NativeMutatingToolDialog::create(hwnd, MutatingToolKind::ManageBitLocker) {
            Ok(mut dialog) => {
                dialog.set_state(MutatingToolState {
                    target: current_drive,
                    available_items: locked_volumes,
                    bitlocker_action: super::tool_dialogs_mutating::BitLockerAction::Unlock,
                    status: crate::tr!("该卷已被 BitLocker 锁定。请输入密码或恢复密钥后解锁。"),
                    ..Default::default()
                });
                dialog.show_modeless();
                self.mutating_tool_dialogs.push(dialog);
                let _ = SetTimer(hwnd, TOOL_DIALOG_TIMER_ID, 100, None);
            }
            Err(error) => {
                self.pending_bitlocker_gate = None;
                log::error!("创建 BitLocker 安全门禁对话框失败: {error}");
                if let Some(handles) = &self.handles {
                    set_text(
                        handles.status,
                        &crate::tr!("无法打开 BitLocker 解锁对话框：{}", error),
                    );
                }
            }
        }
    }

    unsafe fn start_bitlocker_gate_unlock(
        &mut self,
        execution: &super::tool_dialogs_mutating::MutatingToolIntent,
    ) {
        let Some(pending) = &self.pending_bitlocker_gate else {
            return;
        };
        let (volume, credential) = match execution {
            super::tool_dialogs_mutating::MutatingToolIntent::ManageBitLocker {
                volume,
                action: super::tool_dialogs_mutating::BitLockerAction::Unlock,
                credential: Some(credential),
            } if volume.eq_ignore_ascii_case(&pending.current_drive) => {
                (volume.clone(), credential)
            }
            _ => {
                self.set_bitlocker_gate_error(crate::tr!(
                    "BitLocker 解锁请求与当前安装或备份目标不匹配。"
                ));
                return;
            }
        };
        let input = match credential {
            super::tool_dialogs_mutating::BitLockerCredential::Password(value) => {
                GateCredential::Password(value.clone())
            }
            super::tool_dialogs_mutating::BitLockerCredential::RecoveryKey(value) => {
                GateCredential::RecoveryKey(value.clone())
            }
        };
        let credential = match validate_credential(input) {
            Ok(credential) => credential,
            Err(error) => {
                self.set_bitlocker_gate_error(crate::tr!("BitLocker 凭据无效：{}", error));
                return;
            }
        };
        if let Some(dialog) = self
            .mutating_tool_dialogs
            .iter_mut()
            .find(|dialog| dialog.kind() == MutatingToolKind::ManageBitLocker)
        {
            let mut state = dialog.state().clone();
            state.loading = true;
            state.status = crate::tr!("正在解锁 BitLocker 卷 {}...", volume);
            dialog.set_state(state);
            dialog.show_modeless();
        }

        let sender = self.tool_worker_sender.clone();
        std::thread::spawn(move || {
            let result = execute_unlock(&volume, &credential)
                .map_err(|error| error.to_string())
                .and_then(|outcome| {
                    if outcome.success {
                        Ok(())
                    } else {
                        Err(match outcome.error_code {
                            Some(code) => format!("{} ({code:#010X})", outcome.message),
                            None => outcome.message,
                        })
                    }
                });
            let _ = sender.send(ToolWorkerMessage::BitLockerGateCompleted {
                drive: volume,
                result,
            });
        });
    }

    unsafe fn set_bitlocker_gate_error(&mut self, message: String) {
        if let Some(dialog) = self
            .mutating_tool_dialogs
            .iter_mut()
            .find(|dialog| dialog.kind() == MutatingToolKind::ManageBitLocker)
        {
            let mut state = dialog.state().clone();
            state.loading = false;
            state.status = message;
            dialog.set_state(state);
            dialog.show_modeless();
        }
    }

    unsafe fn handle_bitlocker_gate_completed(
        &mut self,
        hwnd: HWND,
        drive: String,
        result: Result<(), String>,
    ) {
        let Some(pending) = &self.pending_bitlocker_gate else {
            return;
        };
        if !drive.eq_ignore_ascii_case(&pending.current_drive) {
            self.set_bitlocker_gate_error(crate::tr!(
                "收到的 BitLocker 解锁结果与当前等待的卷不匹配。"
            ));
            return;
        }
        if let Err(error) = result {
            self.set_bitlocker_gate_error(crate::tr!("BitLocker 解锁失败：{}", error));
            return;
        }

        let refreshed = self.refresh_partitions();
        if bitlocker_gate_completion(true, refreshed, 0) == BitLockerGateCompletion::KeepDialog {
            self.set_bitlocker_gate_error(crate::tr!(
                "BitLocker 已报告解锁成功，但无法重新读取分区状态。请重试。"
            ));
            return;
        }
        let remaining = match self
            .pending_bitlocker_gate
            .as_ref()
            .expect("gate remains pending during refresh")
            .intent
            .locked_volumes(&self.partitions)
        {
            Ok(remaining) => remaining,
            Err(error) => {
                self.set_bitlocker_gate_error(crate::tr!("重新检查 BitLocker 状态失败：{}", error));
                return;
            }
        };
        match bitlocker_gate_completion(true, true, remaining.len()) {
            BitLockerGateCompletion::PromptNext => {
                let next = remaining[0].clone();
                if let Some(pending) = &mut self.pending_bitlocker_gate {
                    pending.current_drive = next.clone();
                }
                if let Some(dialog) = self
                    .mutating_tool_dialogs
                    .iter_mut()
                    .find(|dialog| dialog.kind() == MutatingToolKind::ManageBitLocker)
                {
                    dialog.set_state(MutatingToolState {
                        target: next,
                        available_items: remaining,
                        bitlocker_action: super::tool_dialogs_mutating::BitLockerAction::Unlock,
                        status: crate::tr!("请继续解锁下一被 BitLocker 锁定的卷。"),
                        ..Default::default()
                    });
                    dialog.show_modeless();
                }
            }
            BitLockerGateCompletion::ContinuePending => {
                self.mutating_tool_dialogs
                    .retain(|dialog| dialog.kind() != MutatingToolKind::ManageBitLocker);
                if let Some(pending) = self.pending_bitlocker_gate.take() {
                    self.continue_pending_bitlocker_intent(hwnd, pending.intent);
                }
            }
            BitLockerGateCompletion::KeepDialog => unreachable!(),
        }
    }

    unsafe fn continue_pending_bitlocker_intent(
        &mut self,
        hwnd: HWND,
        intent: PendingBitLockerIntent,
    ) {
        match intent {
            PendingBitLockerIntent::Install(intent) => self.start_install_execution(hwnd, intent),
            PendingBitLockerIntent::Backup(_) => self.prepare_backup_from_page(hwnd),
        }
    }

    /// Re-enters the complete backup preflight from live controls. This is used for the
    /// initial click, after BitLocker unlock, and after a PE download so no cached intent can
    /// bypass a fresh disk inventory, route decision, or lock-state check.
    unsafe fn prepare_backup_from_page(&mut self, hwnd: HWND) {
        let Some(page) = &self.backup_page else {
            return;
        };
        let warning = page.handles().warning;
        if !self.refresh_partitions() {
            set_text(
                warning,
                &crate::tr!("无法重新读取备份源和 BitLocker 状态，备份已停止。"),
            );
            return;
        }

        let Some(page) = &self.backup_page else {
            return;
        };
        let backup_state = page.read_state();
        let selected_pe = page.selected_pe();
        let rows = self.backup_partition_rows();
        let config = match backup_state.to_backup_config(&rows, self.app_config.wim_engine) {
            Ok(config) => config,
            Err(error) => {
                set_text(warning, &error.to_string());
                return;
            }
        };
        let Some(source) = self.partitions.iter().find(|partition| {
            partition
                .letter
                .eq_ignore_ascii_case(&config.source_partition)
        }) else {
            set_text(warning, &crate::tr!("所选备份分区已不可用，请重新选择"));
            return;
        };
        let pe = self.available_pe();
        let plan = match plan_backup_launch(
            &config,
            crate::core::disk::DiskManager::is_pe_environment(),
            source.is_system_partition,
            selected_pe.and_then(|index| pe.get(index)),
        ) {
            Ok(plan) => plan,
            Err(error) => {
                set_text(warning, &error.to_string());
                return;
            }
        };
        let route = match &plan.intent {
            BackupLaunchIntent::Direct(_) => crate::tr!("直接备份"),
            BackupLaunchIntent::ViaPe(_) => crate::tr!("PE 环境备份"),
        };
        set_text(warning, &crate::tr!("备份配置已通过安全验证：{}。", route));
        log::info!(
            "原生备份意图已生成: route={route}, source={}",
            config.source_partition
        );
        let pending = PendingBitLockerIntent::Backup(plan.intent);
        match pending.locked_volumes(&self.partitions) {
            Ok(locked) if !locked.is_empty() => self.begin_bitlocker_gate(hwnd, pending, locked),
            Ok(_) => {
                let PendingBitLockerIntent::Backup(intent) = pending else {
                    unreachable!()
                };
                self.start_backup_execution(hwnd, intent);
            }
            Err(error) => set_text(warning, &crate::tr!("无法检查 BitLocker 锁定卷：{}", error)),
        }
    }

    unsafe fn start_backup_execution(&mut self, hwnd: HWND, intent: BackupLaunchIntent) {
        #[cfg(not(feature = "non-elevated-tests"))]
        if self.prepare_pe_download_for_backup(hwnd, &intent) {
            return;
        }
        match execute_backup(intent) {
            Ok(execution) => self.show_backup_progress(hwnd, execution),
            Err(error) => {
                if let Some(page) = &self.backup_page {
                    set_text(
                        page.handles().warning,
                        &crate::tr!("无法启动备份：{}", error),
                    );
                }
            }
        }
    }

    #[cfg(not(feature = "non-elevated-tests"))]
    unsafe fn prepare_pe_download_for_backup(
        &mut self,
        hwnd: HWND,
        intent: &BackupLaunchIntent,
    ) -> bool {
        let BackupLaunchIntent::ViaPe(preparation) = intent else {
            return false;
        };
        let pe = &preparation.pe;
        match crate::core::pe::PeManager::check_cached_pe(
            &pe.filename,
            pe.sha256.as_deref(),
            pe.md5.as_deref(),
        ) {
            Ok(lr_core::cached_artifact::CachedArtifactStatus::Ready { .. }) => false,
            Ok(lr_core::cached_artifact::CachedArtifactStatus::Missing) => {
                let integrity = match lr_core::download_integrity::select_expected_hash(
                    pe.sha256.as_deref(),
                    pe.md5.as_deref(),
                ) {
                    Ok(value) => value,
                    Err(error) => {
                        if let Some(page) = &self.backup_page {
                            set_text(
                                page.handles().warning,
                                &crate::tr!("PE 校验配置无效：{}", error),
                            );
                        }
                        return true;
                    }
                };
                let plan = crate::core::native_download_controller::DownloadPlan {
                    url: pe.download_url.clone(),
                    save_directory: crate::utils::path::get_pe_download_cache_dir(),
                    filename: pe.filename.clone(),
                    integrity,
                    completion: crate::core::native_download_controller::DownloadCompletion::None,
                    download_threads: self.app_config.download_threads,
                };
                match NativeDownloadExecutor::start(plan) {
                    Ok(worker) => {
                        self.pending_backup_after_pe_download = Some(intent.clone());
                        self.show_download_progress(hwnd, worker);
                    }
                    Err(error) => {
                        if let Some(page) = &self.backup_page {
                            set_text(
                                page.handles().warning,
                                &crate::tr!("无法下载所需 PE 环境：{}", error),
                            );
                        }
                    }
                }
                true
            }
            Err(error) => {
                if let Some(page) = &self.backup_page {
                    set_text(
                        page.handles().warning,
                        &crate::tr!("PE 文件安全校验失败：{}", error),
                    );
                }
                true
            }
        }
    }

    unsafe fn handle_tool_content_action(&mut self, command_id: u16, control: HWND) -> bool {
        let intent = self
            .tool_dialogs
            .iter()
            .find(|dialog| dialog.owns_content_action(control))
            .and_then(|dialog| dialog.handle_content_action(command_id));
        match intent {
            Some(ToolDialogIntent::BrowseGhoImage) => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter(crate::tr!("Ghost 镜像"), &["gho", "ghs"])
                    .pick_file()
                {
                    if let Some(dialog) = self
                        .tool_dialogs
                        .iter_mut()
                        .find(|dialog| dialog.kind() == ToolDialogKind::ReadGhoPassword)
                    {
                        dialog.set_gho_password_state(&super::tool_dialogs::GhoPasswordState {
                            path: path.to_string_lossy().into_owned(),
                            ..Default::default()
                        });
                        dialog.show_modeless();
                    }
                }
                true
            }
            Some(ToolDialogIntent::BrowseImageForVerification) => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter(
                        crate::tr!("系统镜像"),
                        &["wim", "esd", "swm", "gho", "ghs", "iso"],
                    )
                    .pick_file()
                {
                    if let Some(dialog) = self
                        .tool_dialogs
                        .iter_mut()
                        .find(|dialog| dialog.kind() == ToolDialogKind::VerifyImage)
                    {
                        dialog.set_image_verification_state(
                            &super::tool_dialogs::ImageVerificationState {
                                path: path.to_string_lossy().into_owned(),
                                ..Default::default()
                            },
                        );
                        dialog.show_modeless();
                    }
                }
                true
            }
            Some(ToolDialogIntent::BrowseFileForHash) => {
                if let Some(path) = rfd::FileDialog::new().pick_file() {
                    if let Some(dialog) = self
                        .tool_dialogs
                        .iter_mut()
                        .find(|dialog| dialog.kind() == ToolDialogKind::VerifyFileHash)
                    {
                        dialog.set_file_hash_state(&super::tool_dialogs::FileHashState {
                            path: path.to_string_lossy().into_owned(),
                            ..Default::default()
                        });
                        dialog.show_modeless();
                    }
                }
                true
            }
            Some(ToolDialogIntent::CopyGhoPassword { password }) => {
                if let Err(error) = clipboard_win::set_clipboard_string(&password) {
                    log::warn!("复制 GHO 密码到剪贴板失败: {error}");
                }
                true
            }
            Some(ToolDialogIntent::CancelImageVerification) => {
                if let Some(cancel) = &self.image_verify_cancel {
                    cancel.store(true, Ordering::SeqCst);
                }
                true
            }
            _ => false,
        }
    }

    unsafe fn poll_tool_dialogs(&mut self, hwnd: HWND) {
        match self
            .time_sync_dialog
            .as_mut()
            .and_then(|dialog| dialog.take_intent())
        {
            Some(TimeSyncDialogIntent::Confirm) => {
                self.time_sync_dialog = None;
                self.start_confirmed_tool(
                    MutatingToolKind::TimeSynchronization,
                    &super::tool_dialogs_mutating::MutatingToolIntent::SynchronizeTime {
                        server: String::new(),
                    },
                );
            }
            Some(TimeSyncDialogIntent::Close) => self.time_sync_dialog = None,
            None => {}
        }
        match self
            .network_reset_dialog
            .as_mut()
            .and_then(|dialog| dialog.take_intent())
        {
            Some(NetworkResetDialogIntent::Confirm) => {
                self.network_reset_dialog = None;
                self.start_confirmed_tool(
                    MutatingToolKind::ResetNetwork,
                    &super::tool_dialogs_mutating::MutatingToolIntent::ResetNetwork,
                );
            }
            Some(NetworkResetDialogIntent::Close) => self.network_reset_dialog = None,
            None => {}
        }
        let batch_format_intent = self
            .batch_format_dialog
            .as_mut()
            .and_then(|dialog| dialog.take_intent());
        match batch_format_intent {
            Some(BatchFormatDialogIntent::Refresh) => self.start_batch_format_inventory(),
            Some(BatchFormatDialogIntent::Close) => self.batch_format_dialog = None,
            Some(BatchFormatDialogIntent::RequestConfirmation(execution)) => {
                let selected = match &execution {
                    super::tool_dialogs_mutating::MutatingToolIntent::BatchFormat {
                        partitions,
                        ..
                    } => partitions.join("、"),
                    _ => String::new(),
                };
                let spec = DialogSpec {
                    window_title: crate::tr!("确认格式化所选分区"),
                    title: crate::tr!("确认格式化所选分区"),
                    description: crate::tr!(
                        "将格式化以下分区并清除其中全部数据：{}\n\n此操作无法撤销。",
                        selected
                    ),
                    width: 620,
                    height: 300,
                    buttons: DialogButtons {
                        primary: crate::tr!("确认格式化"),
                        secondary: None,
                        cancel: Some(crate::tr!("返回检查")),
                    },
                };
                match DialogShell::create(hwnd, spec) {
                    Ok(mut confirmation) => {
                        if confirmation.show_modal() == DialogResult::Primary {
                            self.batch_format_dialog = None;
                            self.start_confirmed_tool(MutatingToolKind::BatchFormat, &execution);
                        } else if let Some(dialog) = &mut self.batch_format_dialog {
                            dialog.show_modeless();
                        }
                    }
                    Err(error) => {
                        log::error!("创建批量格式化二次确认对话框失败: {error}");
                        if let Some(dialog) = &mut self.batch_format_dialog {
                            dialog.show_modeless();
                        }
                    }
                }
            }
            None => {}
        }
        let storage_driver_intent = self
            .storage_driver_dialog
            .as_mut()
            .and_then(|dialog| dialog.take_intent());
        match storage_driver_intent {
            Some(StorageDriverDialogIntent::Close) => self.storage_driver_dialog = None,
            Some(StorageDriverDialogIntent::RequestConfirmation(request)) => {
                let spec = DialogSpec {
                    window_title: crate::tr!("确认导入存储控制器驱动"),
                    title: crate::tr!("确认导入存储控制器驱动"),
                    description: crate::tr!(
                        "将把 LetRecovery 随包提供的存储控制器驱动导入离线 Windows：{}\n\n继续前请确认目标系统分区正确。",
                        request.target
                    ),
                    width: 620,
                    height: 300,
                    buttons: DialogButtons {
                        primary: crate::tr!("确认导入"),
                        secondary: None,
                        cancel: Some(crate::tr!("返回检查")),
                    },
                };
                match DialogShell::create(hwnd, spec) {
                    Ok(mut confirmation) => {
                        if confirmation.show_modal() == DialogResult::Primary {
                            self.start_storage_driver_prepare(request);
                        } else if let Some(dialog) = &mut self.storage_driver_dialog {
                            dialog.show_modeless();
                        }
                    }
                    Err(error) => {
                        log::error!("创建存储控制器驱动二次确认对话框失败: {error}");
                        if let Some(dialog) = &mut self.storage_driver_dialog {
                            dialog.show_modeless();
                        }
                    }
                }
            }
            None => {}
        }
        let password_reset_intent = self
            .password_reset_dialog
            .as_mut()
            .and_then(|dialog| dialog.take_intent());
        match password_reset_intent {
            Some(PasswordResetDialogIntent::Close) => {
                self.password_reset_generation = self.password_reset_generation.wrapping_add(1);
                self.password_reset_dialog = None;
            }
            Some(PasswordResetDialogIntent::ReloadTargets) => {
                self.password_reset_generation = self.password_reset_generation.wrapping_add(1);
                let generation = self.password_reset_generation;
                if let Some(dialog) = &mut self.password_reset_dialog {
                    dialog.set_busy(crate::tr!("正在检测 Windows 系统..."));
                }
                self.start_password_reset_targets(generation);
            }
            Some(PasswordResetDialogIntent::LoadAccounts(target)) => {
                self.start_password_reset_accounts(self.password_reset_generation, target);
            }
            Some(PasswordResetDialogIntent::RequestConfirmation(request)) => {
                let target = match &request.target {
                    crate::core::native_password_reset::PasswordResetTarget::CurrentSystem => {
                        crate::tr!("当前系统（在线）")
                    }
                    crate::core::native_password_reset::PasswordResetTarget::OfflineWindows(
                        root,
                    ) => crate::tr!("离线 Windows（{}）", root),
                };
                let spec = DialogSpec {
                    window_title: crate::tr!("确认重置账户密码"),
                    title: crate::tr!("确认重置账户密码"),
                    description: crate::tr!(
                        "目标：{}\n账户：{}\n\n将清空该账户密码并启用账户。",
                        target,
                        request.account
                    ),
                    width: 620,
                    height: 320,
                    buttons: DialogButtons {
                        primary: crate::tr!("确认重置"),
                        secondary: None,
                        cancel: Some(crate::tr!("返回检查")),
                    },
                };
                match DialogShell::create(hwnd, spec) {
                    Ok(mut confirmation) => {
                        if confirmation.show_modal() == DialogResult::Primary {
                            if let Some(dialog) = &mut self.password_reset_dialog {
                                dialog.set_busy(crate::tr!("正在重置所选账户密码..."));
                            }
                            self.start_password_reset_execution(
                                self.password_reset_generation,
                                request,
                            );
                        } else if let Some(dialog) = &mut self.password_reset_dialog {
                            dialog.show_modeless();
                        }
                    }
                    Err(error) => {
                        log::error!("创建密码重置二次确认对话框失败: {error}");
                        if let Some(dialog) = &mut self.password_reset_dialog {
                            dialog.show_modeless();
                        }
                    }
                }
            }
            None => {}
        }
        let driver_transfer_intent = self
            .driver_transfer_dialog
            .as_mut()
            .and_then(|dialog| dialog.take_intent());
        match driver_transfer_intent {
            Some(crate::core::native_driver_transfer::DriverTransferIntent::Close) => {
                self.driver_transfer_dialog = None;
            }
            Some(crate::core::native_driver_transfer::DriverTransferIntent::BrowseDirectory(_)) => {
                if let Some(dialog) = &mut self.driver_transfer_dialog {
                    dialog.show_modeless();
                }
            }
            Some(crate::core::native_driver_transfer::DriverTransferIntent::Execute(request)) => {
                let (operation, mode) = match request.mode {
                    crate::core::native_driver_transfer::DriverTransferMode::Export => (
                        crate::tr!("导出驱动"),
                        super::tool_dialogs_mutating::DriverTransferMode::Backup,
                    ),
                    crate::core::native_driver_transfer::DriverTransferMode::Import => (
                        crate::tr!("导入驱动"),
                        super::tool_dialogs_mutating::DriverTransferMode::Restore,
                    ),
                };
                let execution = super::tool_dialogs_mutating::MutatingToolIntent::TransferDrivers {
                    mode,
                    directory: request.directory.clone(),
                    system_root: request.windows_root.clone(),
                };
                let spec = DialogSpec {
                    window_title: crate::tr!("确认驱动操作"),
                    title: crate::tr!("确认驱动操作"),
                    description: crate::tr!(
                        "操作：{}\n系统分区：{}\n目录：{}\n\n请确认目标和目录正确。",
                        operation,
                        request.windows_root,
                        request.directory
                    ),
                    width: 620,
                    height: 320,
                    buttons: DialogButtons {
                        primary: crate::tr!("确认执行"),
                        secondary: None,
                        cancel: Some(crate::tr!("返回检查")),
                    },
                };
                match DialogShell::create(hwnd, spec) {
                    Ok(mut confirmation) => {
                        if confirmation.show_modal() == DialogResult::Primary {
                            self.driver_transfer_dialog = None;
                            self.start_confirmed_tool(
                                MutatingToolKind::DriverBackupRestore,
                                &execution,
                            );
                        } else if let Some(dialog) = &mut self.driver_transfer_dialog {
                            dialog.show_modeless();
                        }
                    }
                    Err(error) => {
                        log::error!("创建驱动备份还原二次确认对话框失败: {error}");
                        if let Some(dialog) = &mut self.driver_transfer_dialog {
                            dialog.show_modeless();
                        }
                    }
                }
            }
            None => {}
        }
        let boot_repair_intent = self
            .boot_repair_dialog
            .as_mut()
            .and_then(|dialog| dialog.take_intent());
        match boot_repair_intent {
            Some(BootRepairDialogIntent::Close) => {
                self.boot_repair_generation = self.boot_repair_generation.wrapping_add(1);
                self.boot_repair_dialog = None;
            }
            Some(BootRepairDialogIntent::Refresh) => {
                self.boot_repair_generation = self.boot_repair_generation.wrapping_add(1);
                let generation = self.boot_repair_generation;
                self.start_boot_repair_inventory(generation);
            }
            Some(BootRepairDialogIntent::RequestConfirmation(request)) => {
                let spec = DialogSpec {
                    window_title: crate::tr!("确认修复 Windows 引导"),
                    title: crate::tr!("确认修复 Windows 引导"),
                    description: crate::tr!(
                        "目标系统分区：{}\n\nLetRecovery 将自动根据目标磁盘和系统环境选择正确的引导修复方式。",
                        request.target_partition
                    ),
                    width: 620,
                    height: 320,
                    buttons: DialogButtons {
                        primary: crate::tr!("确认修复"),
                        secondary: None,
                        cancel: Some(crate::tr!("返回检查")),
                    },
                };
                match DialogShell::create(hwnd, spec) {
                    Ok(mut confirmation) => {
                        if confirmation.show_modal() == DialogResult::Primary {
                            if let Some(dialog) = &mut self.boot_repair_dialog {
                                dialog.set_running();
                            }
                            self.start_boot_repair_execution(self.boot_repair_generation, request);
                        } else if let Some(dialog) = &mut self.boot_repair_dialog {
                            dialog.show_modeless();
                        }
                    }
                    Err(error) => {
                        log::error!("创建引导修复二次确认对话框失败: {error}");
                        if let Some(dialog) = &mut self.boot_repair_dialog {
                            dialog.show_modeless();
                        }
                    }
                }
            }
            None => {}
        }
        let appx_intent = self
            .appx_dialog
            .as_mut()
            .and_then(|dialog| dialog.take_intent());
        match appx_intent {
            Some(crate::core::native_appx_selection::NativeAppxDialogIntent::Close) => {
                self.appx_generation = self.appx_generation.wrapping_add(1);
                self.appx_dialog = None;
            }
            Some(crate::core::native_appx_selection::NativeAppxDialogIntent::LoadPackages {
                inventory_target,
            }) => self.start_appx_packages(inventory_target),
            Some(crate::core::native_appx_selection::NativeAppxDialogIntent::RequestRemoval(
                request,
            )) => {
                let target = match &request.target {
                    crate::core::native_appx::AppxTarget::CurrentSystem => {
                        crate::tr!("当前系统（在线）")
                    }
                    crate::core::native_appx::AppxTarget::OfflineWindows(root) => {
                        crate::tr!("离线 Windows（{}）", root)
                    }
                };
                let execution = super::tool_dialogs_mutating::MutatingToolIntent::RemoveAppx {
                    packages: request.packages.clone(),
                    offline_root: match &request.target {
                        crate::core::native_appx::AppxTarget::CurrentSystem => {
                            "__CURRENT__".to_owned()
                        }
                        crate::core::native_appx::AppxTarget::OfflineWindows(root) => root.clone(),
                    },
                };
                let spec = DialogSpec {
                    window_title: crate::tr!("确认移除所选 APPX 应用"),
                    title: crate::tr!("确认移除所选 APPX 应用"),
                    description: crate::tr!(
                        "目标：{}\n已选择 {} 个应用。\n\n受保护的系统关键包不会被移除。",
                        target,
                        request.packages.len()
                    ),
                    width: 620,
                    height: 320,
                    buttons: DialogButtons {
                        primary: crate::tr!("确认移除"),
                        secondary: None,
                        cancel: Some(crate::tr!("返回检查")),
                    },
                };
                match DialogShell::create(hwnd, spec) {
                    Ok(mut confirmation) => {
                        if confirmation.show_modal() == DialogResult::Primary {
                            self.appx_generation = self.appx_generation.wrapping_add(1);
                            self.appx_dialog = None;
                            self.start_confirmed_tool(MutatingToolKind::RemoveAppx, &execution);
                        } else if let Some(dialog) = &mut self.appx_dialog {
                            dialog.show_modeless();
                        }
                    }
                    Err(error) => {
                        log::error!("创建 APPX 移除二次确认对话框失败: {error}");
                        if let Some(dialog) = &mut self.appx_dialog {
                            dialog.show_modeless();
                        }
                    }
                }
            }
            None => {}
        }
        let nvidia_intent = self
            .nvidia_dialog
            .as_mut()
            .and_then(|dialog| dialog.take_intent());
        match nvidia_intent {
            Some(NvidiaRemovalDialogIntent::Close) => {
                self.nvidia_generation = self.nvidia_generation.wrapping_add(1);
                self.nvidia_dialog = None;
            }
            Some(NvidiaRemovalDialogIntent::LoadHardwareReport) => {
                self.start_nvidia_hardware(self.nvidia_generation);
            }
            Some(NvidiaRemovalDialogIntent::ReloadTargetsAndHardware) => {
                self.nvidia_generation = self.nvidia_generation.wrapping_add(1);
                let generation = self.nvidia_generation;
                let is_pe = self
                    .config
                    .system_info
                    .as_ref()
                    .is_some_and(|info| info.is_pe_environment);
                if let Some(dialog) = &mut self.nvidia_dialog {
                    dialog.set_busy(crate::tr!("正在刷新目标和硬件信息..."));
                }
                self.start_nvidia_targets(generation, !is_pe);
                self.start_nvidia_hardware(generation);
            }
            Some(NvidiaRemovalDialogIntent::RequestConfirmation(request)) => {
                let scope = crate::core::native_nvidia_removal::removal_scope(&request.target)
                    .unwrap_or_else(|error| error.to_string());
                let spec = DialogSpec {
                    window_title: crate::tr!("确认卸载 NVIDIA 驱动"),
                    title: crate::tr!("确认卸载 NVIDIA 驱动"),
                    description: crate::tr!("{}\n\n操作完成后可能需要重新启动 Windows。", scope),
                    width: 640,
                    height: 330,
                    buttons: DialogButtons {
                        primary: crate::tr!("确认卸载"),
                        secondary: None,
                        cancel: Some(crate::tr!("返回检查")),
                    },
                };
                match DialogShell::create(hwnd, spec) {
                    Ok(mut confirmation) => {
                        if confirmation.show_modal() == DialogResult::Primary {
                            if let Some(dialog) = &mut self.nvidia_dialog {
                                dialog.set_busy(crate::tr!("正在卸载 NVIDIA 驱动..."));
                            }
                            self.start_nvidia_removal(self.nvidia_generation, request);
                        } else if let Some(dialog) = &mut self.nvidia_dialog {
                            dialog.show_modeless();
                        }
                    }
                    Err(error) => {
                        log::error!("创建 NVIDIA 驱动卸载二次确认对话框失败: {error}");
                        if let Some(dialog) = &mut self.nvidia_dialog {
                            dialog.show_modeless();
                        }
                    }
                }
            }
            None => {}
        }
        let partition_copy_intent = self
            .partition_copy_dialog
            .as_mut()
            .and_then(|dialog| dialog.take_intent());
        match partition_copy_intent {
            Some(PartitionCopyDialogIntent::Close) => {
                if self
                    .partition_copy_dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.state().copying)
                {
                    if let Some(dialog) = &mut self.partition_copy_dialog {
                        dialog.show_modeless();
                    }
                } else {
                    self.partition_copy_generation = self.partition_copy_generation.wrapping_add(1);
                    self.partition_copy_dialog = None;
                }
            }
            Some(PartitionCopyDialogIntent::RefreshInventory) => {
                self.partition_copy_generation = self.partition_copy_generation.wrapping_add(1);
                self.start_partition_copy_inventory(hwnd, self.partition_copy_generation);
            }
            Some(PartitionCopyDialogIntent::RequestConfirmation(request)) => {
                let spec = DialogSpec {
                    window_title: crate::tr!("确认分区对拷"),
                    title: crate::tr!("确认分区对拷"),
                    description: crate::tr!(
                        "将把 {} 的全部文件复制到 {}。\n\n目标分区中的同名文件可能被覆盖；请再次确认源分区和目标分区。",
                        request.source,
                        request.target
                    ),
                    width: 640,
                    height: 330,
                    buttons: DialogButtons {
                        primary: crate::tr!("确认开始对拷"),
                        secondary: None,
                        cancel: Some(crate::tr!("返回检查")),
                    },
                };
                match DialogShell::create(hwnd, spec) {
                    Ok(mut confirmation) => {
                        if confirmation.show_modal() == DialogResult::Primary {
                            if let Some(dialog) = &mut self.partition_copy_dialog {
                                dialog.set_copying(true);
                            }
                            self.start_partition_copy_execution(
                                self.partition_copy_generation,
                                request,
                            );
                        } else if let Some(dialog) = &mut self.partition_copy_dialog {
                            dialog.show_modeless();
                        }
                    }
                    Err(error) => {
                        log::error!("创建分区对拷二次确认对话框失败: {error}");
                        if let Some(dialog) = &mut self.partition_copy_dialog {
                            dialog.show_modeless();
                        }
                    }
                }
            }
            None => {}
        }
        let quick_partition_intent = self.pending_quick_partition_command.take().or_else(|| {
            self.quick_partition_dialog
                .as_mut()
                .and_then(|dialog| dialog.take_intent())
        });
        match quick_partition_intent {
            Some(QuickPartitionDialogIntent::Close) => self.quick_partition_dialog = None,
            Some(QuickPartitionDialogIntent::RefreshInventory) => {
                self.start_quick_partition_inventory();
            }
            Some(QuickPartitionDialogIntent::RequestConfirmation(request)) => {
                let spec = DialogSpec {
                    window_title: crate::tr!("确认一键分区"),
                    title: crate::tr!("确认一键分区"),
                    description: crate::tr!(
                        "将清除物理磁盘 {} 上的全部分区和数据，并按当前规划重新分区。\n\n此操作无法撤销，请再次核对磁盘型号、容量和分区规划。",
                        request.disk.disk_number
                    ),
                    width: 650,
                    height: 340,
                    buttons: DialogButtons {
                        primary: crate::tr!("确认一键分区"),
                        secondary: None,
                        cancel: Some(crate::tr!("返回检查")),
                    },
                };
                match DialogShell::create(hwnd, spec) {
                    Ok(mut confirmation) => {
                        if confirmation.show_modal() == DialogResult::Primary {
                            self.quick_partition_dialog = None;
                            self.start_confirmed_tool(
                                MutatingToolKind::QuickPartition,
                                &super::tool_dialogs_mutating::MutatingToolIntent::QuickPartition {
                                    request,
                                },
                            );
                        } else if let Some(dialog) = &mut self.quick_partition_dialog {
                            dialog.show_modeless();
                        }
                    }
                    Err(error) => log::error!("创建一键分区二次确认对话框失败: {error}"),
                }
            }
            Some(QuickPartitionDialogIntent::RequestExistingResize(request)) => {
                let spec = DialogSpec {
                    window_title: crate::tr!("确认调整分区大小"),
                    title: crate::tr!("确认调整分区大小"),
                    description: crate::tr!(
                        "将把 {}: 从 {} MB 调整为 {} MB。\n\n执行前会重新读取并复核物理磁盘和分区状态。",
                        request.drive_letter,
                        request.current_size_mb,
                        request.new_size_mb
                    ),
                    width: 620,
                    height: 310,
                    buttons: DialogButtons {
                        primary: crate::tr!("确认调整"),
                        secondary: None,
                        cancel: Some(crate::tr!("返回检查")),
                    },
                };
                match DialogShell::create(hwnd, spec) {
                    Ok(mut confirmation) => {
                        if confirmation.show_modal() == DialogResult::Primary {
                            if let Some(dialog) = &mut self.quick_partition_dialog {
                                dialog.set_loading();
                            }
                            self.start_quick_partition_resize(request);
                        } else if let Some(dialog) = &mut self.quick_partition_dialog {
                            dialog.show_modeless();
                        }
                    }
                    Err(error) => log::error!("创建分区调整二次确认对话框失败: {error}"),
                }
            }
            None => {}
        }
        let bitlocker_intent = self.pending_bitlocker_manage_command.take().or_else(|| {
            self.bitlocker_manage_dialog
                .as_mut()
                .and_then(|dialog| dialog.take_intent())
        });
        match bitlocker_intent {
            Some(BitLockerManageDialogIntent::Close) => self.bitlocker_manage_dialog = None,
            Some(BitLockerManageDialogIntent::RefreshInventory) => {
                self.start_bitlocker_manage_inventory();
            }
            Some(BitLockerManageDialogIntent::ExportRecoveryKey(key)) => {
                if let Some(path) = rfd::FileDialog::new()
                    .set_file_name("BitLocker-Recovery-Key.txt")
                    .add_filter(crate::tr!("文本文件"), &["txt"])
                    .save_file()
                {
                    if let Err(error) = std::fs::write(path, key.expose()) {
                        log::warn!("导出 BitLocker 恢复密钥失败: {error}");
                    }
                }
                if let Some(dialog) = &mut self.bitlocker_manage_dialog {
                    dialog.show_modeless();
                }
            }
            Some(BitLockerManageDialogIntent::RequestOperation(operation)) => {
                let read_only = matches!(
                    &operation,
                    crate::core::native_bitlocker_manage::BitLockerManageIntent::ReadRecoveryKey { .. }
                );
                let confirmed = if read_only {
                    true
                } else {
                    let spec = DialogSpec {
                        window_title: crate::tr!("确认 BitLocker 操作"),
                        title: crate::tr!("确认 BitLocker 操作"),
                        description: crate::tr!(
                            "将对所选 BitLocker 分区执行当前操作。执行前会重新读取卷状态并复核操作是否仍然可用。"
                        ),
                        width: 620,
                        height: 300,
                        buttons: DialogButtons {
                            primary: crate::tr!("确认执行"),
                            secondary: None,
                            cancel: Some(crate::tr!("返回检查")),
                        },
                    };
                    match DialogShell::create(hwnd, spec) {
                        Ok(mut confirmation) => confirmation.show_modal() == DialogResult::Primary,
                        Err(error) => {
                            log::error!("创建 BitLocker 二次确认对话框失败: {error}");
                            false
                        }
                    }
                };
                if confirmed {
                    if let Some(dialog) = &mut self.bitlocker_manage_dialog {
                        dialog.set_running(if read_only {
                            crate::tr!("正在读取恢复密钥...")
                        } else {
                            crate::tr!("正在执行 BitLocker 操作...")
                        });
                    }
                    self.start_bitlocker_manage_operation(operation);
                } else if let Some(dialog) = &mut self.bitlocker_manage_dialog {
                    dialog.show_modeless();
                }
            }
            None => {}
        }
        if let Some(dialog) = &mut self.hardware_inspector_dialog {
            dialog.refresh_layout();
        }
        let hardware_inspector_intent = self
            .hardware_inspector_dialog
            .as_mut()
            .and_then(|dialog| dialog.take_intent());
        match hardware_inspector_intent {
            Some(HardwareInspectorIntent::Refresh) => self.start_hardware_inspector(hwnd),
            Some(HardwareInspectorIntent::Close) => {
                self.hardware_inspector_generation =
                    self.hardware_inspector_generation.wrapping_add(1);
                self.hardware_inspector_dialog = None;
            }
            None => {}
        }

        let mut remove = Vec::new();
        let mut read_only_jobs = Vec::new();
        for (index, dialog) in self.tool_dialogs.iter_mut().enumerate() {
            let kind = dialog.kind();
            let Some(intent) = dialog.take_intent() else {
                continue;
            };
            match intent {
                ToolDialogIntent::Close => {
                    if kind == ToolDialogKind::VerifyImage {
                        if let Some(cancel) = &self.image_verify_cancel {
                            cancel.store(true, Ordering::SeqCst);
                        }
                    }
                    remove.push(index);
                }
                ToolDialogIntent::BrowseGhoImage => {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter(crate::tr!("Ghost 镜像"), &["gho", "ghs"])
                        .pick_file()
                    {
                        dialog.set_gho_password_state(&super::tool_dialogs::GhoPasswordState {
                            path: path.to_string_lossy().into_owned(),
                            ..Default::default()
                        });
                    }
                    dialog.show_modeless();
                }
                ToolDialogIntent::ReadGhoPassword { path } => {
                    read_only_jobs.push((kind, ReadOnlyToolRequest::GhoPassword { path }));
                }
                ToolDialogIntent::CopyGhoPassword { password } => {
                    if let Err(error) = clipboard_win::set_clipboard_string(&password) {
                        log::warn!("复制 GHO 密码到剪贴板失败: {error}");
                    }
                    dialog.show_modeless();
                }
                ToolDialogIntent::BrowseImageForVerification => {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter(
                            crate::tr!("系统镜像"),
                            &["wim", "esd", "swm", "gho", "ghs", "iso"],
                        )
                        .pick_file()
                    {
                        dialog.set_image_verification_state(
                            &super::tool_dialogs::ImageVerificationState {
                                path: path.to_string_lossy().into_owned(),
                                ..Default::default()
                            },
                        );
                    }
                    dialog.show_modeless();
                }
                ToolDialogIntent::BrowseFileForHash => {
                    if let Some(path) = rfd::FileDialog::new().pick_file() {
                        dialog.set_file_hash_state(&super::tool_dialogs::FileHashState {
                            path: path.to_string_lossy().into_owned(),
                            ..Default::default()
                        });
                    }
                    dialog.show_modeless();
                }
                ToolDialogIntent::VerifyFileHash { path, expected } => {
                    read_only_jobs.push((kind, ReadOnlyToolRequest::Sha256 { path, expected }));
                }
                ToolDialogIntent::VerifyImage { path } => {
                    read_only_jobs.push((kind, ReadOnlyToolRequest::VerifyImage { path }));
                }
                ToolDialogIntent::CancelImageVerification => {
                    if let Some(cancel) = &self.image_verify_cancel {
                        cancel.store(true, Ordering::SeqCst);
                    }
                    dialog.show_modeless();
                }
                ToolDialogIntent::RefreshNetworkInformation => {
                    read_only_jobs.push((kind, ReadOnlyToolRequest::NetworkInformation));
                }
                ToolDialogIntent::RefreshSoftwareList => {
                    read_only_jobs.push((kind, ReadOnlyToolRequest::InstalledSoftware));
                }
                ToolDialogIntent::CopyNetworkReport => {
                    let report = dialog.report_text();
                    if let Err(error) = clipboard_win::set_clipboard_string(&report) {
                        log::warn!("复制网络信息到剪贴板失败: {error}");
                    }
                    dialog.show_modeless();
                }
                ToolDialogIntent::ExportSoftwareList => {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_file_name("installed_software.txt")
                        .add_filter(crate::tr!("文本文件"), &["txt"])
                        .save_file()
                    {
                        if let Err(error) = std::fs::write(path, dialog.report_text()) {
                            log::warn!("导出软件列表失败: {error}");
                        }
                    }
                    dialog.show_modeless();
                }
            }
        }
        for index in remove.into_iter().rev() {
            self.tool_dialogs.remove(index);
        }
        for (kind, request) in read_only_jobs {
            self.start_read_only_tool(kind, request);
        }
        let mut remove_mutating = Vec::new();
        let mut confirmed_jobs = Vec::new();
        let mut cancel_bitlocker_gate = false;
        for (index, dialog) in self.mutating_tool_dialogs.iter_mut().enumerate() {
            let is_bitlocker_gate = self.pending_bitlocker_gate.is_some()
                && dialog.kind() == MutatingToolKind::ManageBitLocker;
            let Some(intent) = dialog.take_intent() else {
                continue;
            };
            match intent {
                MutatingDialogIntent::Close => {
                    remove_mutating.push(index);
                    cancel_bitlocker_gate |= is_bitlocker_gate;
                }
                MutatingDialogIntent::BrowsePath => {
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        let mut state = dialog.state().clone();
                        state.path = path.to_string_lossy().into_owned();
                        dialog.set_state(state);
                    }
                    dialog.show_modeless();
                }
                MutatingDialogIntent::RequestConfirmation { summary, .. } => {
                    let spec = DialogSpec {
                        window_title: crate::tr!("确认执行此操作"),
                        title: crate::tr!("确认执行此操作"),
                        description: summary,
                        width: 620,
                        height: 300,
                        buttons: DialogButtons {
                            primary: crate::tr!("确认执行"),
                            secondary: None,
                            cancel: Some(crate::tr!("返回检查")),
                        },
                    };
                    match DialogShell::create(hwnd, spec) {
                        Ok(mut confirmation) => {
                            if confirmation.show_modal() == DialogResult::Primary {
                                if let Some(MutatingDialogIntent::Execute(execution)) =
                                    dialog.confirm(true)
                                {
                                    confirmed_jobs.push((
                                        dialog.kind(),
                                        execution,
                                        is_bitlocker_gate,
                                    ));
                                }
                            } else {
                                dialog.show_modeless();
                            }
                        }
                        Err(error) => {
                            log::error!("创建二次确认对话框失败: {error}");
                            dialog.show_modeless();
                        }
                    }
                }
                MutatingDialogIntent::Execute(execution) => {
                    confirmed_jobs.push((dialog.kind(), execution, is_bitlocker_gate));
                }
            }
        }
        for index in remove_mutating.into_iter().rev() {
            self.mutating_tool_dialogs.remove(index);
        }
        if cancel_bitlocker_gate {
            self.pending_bitlocker_gate = None;
        }
        for (kind, execution, is_bitlocker_gate) in confirmed_jobs {
            if is_bitlocker_gate {
                self.start_bitlocker_gate_unlock(&execution);
            } else {
                self.start_confirmed_tool(kind, &execution);
            }
        }
        if let Some(receiver) = &self.expand_c_analysis {
            match receiver.try_recv() {
                Ok(Ok(analysis)) => {
                    if let Some(dialog) = &mut self.expand_c_dialog {
                        dialog.apply_analysis(analysis.into());
                    }
                    self.expand_c_analysis = None;
                }
                Ok(Err(error)) => {
                    if let Some(dialog) = &mut self.expand_c_dialog {
                        dialog.set_error(error.to_string());
                    }
                    self.expand_c_analysis = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    if let Some(dialog) = &mut self.expand_c_dialog {
                        dialog.set_error(crate::tr!("C 盘扩容分析任务异常结束"));
                    }
                    self.expand_c_analysis = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        let expand_messages: Vec<_> = self
            .expand_c_execution
            .as_ref()
            .map(|receiver| receiver.try_iter().collect())
            .unwrap_or_default();
        for message in expand_messages {
            match message {
                ExpandCWorkerMessage::Progress(status) => {
                    if let Some(dialog) = &mut self.expand_c_dialog {
                        dialog.set_executing(true, status);
                    }
                }
                ExpandCWorkerMessage::ReadyToReboot => {
                    if let Some(dialog) = &mut self.expand_c_dialog {
                        dialog.set_executing(false, crate::tr!("准备完成，即将重启进入 WinPE..."));
                    }
                    self.expand_c_execution = None;
                    crate::core::pe::PeManager::reboot();
                }
                ExpandCWorkerMessage::Failed(error) => {
                    if let Some(dialog) = &mut self.expand_c_dialog {
                        dialog.set_error(error);
                    }
                    self.expand_c_execution = None;
                }
            }
        }
        let expand_intent = self
            .expand_c_dialog
            .as_mut()
            .and_then(|dialog| dialog.take_intent());
        match expand_intent {
            Some(ExpandCDialogIntent::Analyze) => self.start_expand_c_analysis(hwnd),
            Some(ExpandCDialogIntent::Close) => {
                self.expand_c_dialog = None;
                self.expand_c_analysis = None;
            }
            Some(ExpandCDialogIntent::RequestConfirmation(request)) => {
                let moving = if request.requires_partition_move {
                    crate::tr!("此目标需要移动 C 盘后的分区数据。")
                } else {
                    crate::tr!("此目标只使用相邻未分配空间。")
                };
                let spec = DialogSpec {
                    window_title: crate::tr!("确认无损扩大 C 盘"),
                    title: crate::tr!("确认无损扩大 C 盘"),
                    description: crate::tr!(
                        "目标大小：{} GB。{} 操作将在 WinPE 中执行，请确保重要数据已有备份。",
                        format_args!("{:.1}", request.target_size_mb as f64 / 1024.0),
                        moving
                    ),
                    width: 640,
                    height: 320,
                    buttons: DialogButtons {
                        primary: crate::tr!("确认扩容"),
                        secondary: None,
                        cancel: Some(crate::tr!("返回检查")),
                    },
                };
                match DialogShell::create(hwnd, spec) {
                    Ok(mut confirmation) => {
                        if confirmation.show_modal() == DialogResult::Primary {
                            self.start_expand_c_execution(hwnd, request);
                        } else if let Some(dialog) = &mut self.expand_c_dialog {
                            dialog.show_modeless();
                        }
                    }
                    Err(error) => {
                        if let Some(dialog) = &mut self.expand_c_dialog {
                            dialog.set_error(error.to_string());
                            dialog.show_modeless();
                        }
                    }
                }
            }
            None => {}
        }
        // Consume user commands before asynchronous results. Otherwise a late inventory result can
        // call show_modeless on a just-closed dialog and erase the close result before it is seen.
        self.poll_tool_worker_messages(hwnd);
        if !self.has_tool_dialog_activity() {
            let _ = KillTimer(hwnd, TOOL_DIALOG_TIMER_ID);
        }
    }

    fn has_tool_dialog_activity(&self) -> bool {
        !self.tool_dialogs.is_empty()
            || !self.mutating_tool_dialogs.is_empty()
            || self.time_sync_dialog.is_some()
            || self.network_reset_dialog.is_some()
            || self.batch_format_dialog.is_some()
            || self.storage_driver_dialog.is_some()
            || self.password_reset_dialog.is_some()
            || self.driver_transfer_dialog.is_some()
            || self.boot_repair_dialog.is_some()
            || self.appx_dialog.is_some()
            || self.nvidia_dialog.is_some()
            || self.partition_copy_dialog.is_some()
            || self.quick_partition_dialog.is_some()
            || self.bitlocker_manage_dialog.is_some()
            || self.expand_c_dialog.is_some()
            || self.expand_c_analysis.is_some()
            || self.expand_c_execution.is_some()
            || self.hardware_inspector_dialog.is_some()
            || self.tool_background_jobs != 0
    }

    fn request_hardware_refresh(&self, hwnd: HWND) {
        let window = hwnd.0 as usize;
        std::thread::spawn(move || {
            let result = crate::core::hardware_info::HardwareInfo::collect().ok();
            let payload = Box::into_raw(Box::new(result));
            unsafe {
                if PostMessageW(
                    HWND(window as *mut _),
                    WM_HARDWARE_INFO_READY,
                    WPARAM(0),
                    LPARAM(payload as isize),
                )
                .is_err()
                {
                    drop(Box::from_raw(payload));
                }
            }
        });
    }

    fn available_pe(&self) -> Vec<OnlinePE> {
        self.pe_catalogue.clone()
    }

    unsafe fn selected_install_pe_filename(&self) -> Option<String> {
        let handles = self.handles?;
        let selected = SendMessageW(handles.pe, 0x0147, WPARAM(0), LPARAM(0)).0;
        self.pe_catalogue
            .get(usize::try_from(selected).ok()?)
            .map(|pe| pe.filename.clone())
    }

    unsafe fn install_pe_selector_should_be_visible(&self) -> bool {
        !crate::core::disk::DiskManager::is_pe_environment()
            && self.available_pe().len() > 1
            && self
                .selected_install_target()
                .is_some_and(|target| target.is_current_system)
    }

    unsafe fn populate_install_pe_combo(&self, combo: HWND, preferred_filename: Option<&str>) {
        let _ = SendMessageW(combo, 0x014B, WPARAM(0), LPARAM(0));
        let available = self.available_pe();
        for pe in &available {
            let label = wide(&pe.display_name);
            let _ = SendMessageW(combo, 0x0143, WPARAM(0), LPARAM(label.as_ptr() as isize));
        }
        let selected = preserved_pe_selection(preferred_filename, &available).unwrap_or(usize::MAX);
        let _ = SendMessageW(combo, 0x014E, WPARAM(selected), LPARAM(0));
    }

    unsafe fn selected_install_target(&self) -> Option<InstallTarget> {
        let handles = self.handles.as_ref()?;
        let selected = SendMessageW(handles.partitions, 0x100C, WPARAM(usize::MAX), LPARAM(2)).0;
        let partition = self.partitions.get(usize::try_from(selected).ok()?)?;
        Some(InstallTarget {
            partition: partition.letter.clone(),
            disk_number: partition.disk_number,
            partition_number: partition.partition_number,
            style: partition.partition_style,
            is_current_system: partition.is_system_partition,
            has_windows: partition.has_windows,
        })
    }

    unsafe fn install_intent(
        &self,
    ) -> Result<
        crate::core::native_install_controller::StartInstallIntent,
        crate::core::native_install_controller::InstallValidationError,
    > {
        let handles = self.handles.as_ref().expect("native controls must exist");
        let image_path = self.effective_image_path.clone().unwrap_or_default();
        let is_gho = std::path::Path::new(&image_path)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(extension.to_ascii_lowercase().as_str(), "gho" | "ghs")
            });
        let pe = self.available_pe();
        NativeInstallState {
            image_ready: !image_path.trim().is_empty() || self.xp_i386_source.is_some(),
            selected_image: if is_gho {
                None
            } else {
                let index = SendMessageW(handles.image_volume, 0x0147, WPARAM(0), LPARAM(0)).0;
                self.image_volumes
                    .get(usize::try_from(index).unwrap_or(usize::MAX))
                    .map(|image| SelectedImageMetadata {
                        volume_index: image.index,
                        major_version: image.major_version,
                        architecture: image.architecture,
                    })
            },
            image_path,
            xp_i386_source: self.xp_i386_source.clone(),
            target: self.selected_install_target(),
            is_pe_environment: crate::core::disk::DiskManager::is_pe_environment(),
            pe_available: !pe.is_empty(),
            selected_pe: usize::try_from(SendMessageW(handles.pe, 0x0147, WPARAM(0), LPARAM(0)).0)
                .ok()
                .filter(|index| *index < pe.len()),
            custom_unattend_path: self.custom_unattend_path.clone(),
            custom_unattend_error: self.custom_unattend_error.clone(),
            partition_refresh_pending: self.partition_refresh_requested
                || self.partition_refresh_in_flight,
            partition_refresh_error: self.partition_refresh_error.clone(),
            pca_detection_pending: self.pca_detection_pending || self.pca_target_detection_pending,
            pca_selection_error: self.pca_selection_error(),
            advanced_options_enabled: self.app_config.enable_advanced_options,
            prefs: self.app_config.install_prefs.clone(),
        }
        .start_intent()
    }

    unsafe fn enter_progress(&mut self, hwnd: HWND, initial: LongTaskProgress, timer_id: usize) {
        let Some(handles) = &self.handles else { return };
        let redraw = redraw::suspend(hwnd);
        self.progress_visible = true;
        for control in handles.nav.into_iter().chain([
            handles.brand,
            handles.title,
            handles.description,
            handles.image_label,
            handles.image_edit,
            handles.browse,
            handles.image_volume_label,
            handles.image_volume,
            handles.partitions_label,
            handles.partitions,
            handles.format,
            handles.boot,
            handles.unattend,
            handles.unattend_browse,
            handles.unattend_clear,
            handles.unattend_path,
            handles.driver_label,
            handles.driver,
            handles.reboot,
            handles.boot_label,
            handles.boot_mode,
            handles.pca_label,
            handles.pca_mode,
            handles.run_diskpart,
            handles.open_diskpart_dir,
            handles.edit_boot_commands,
            handles.pe_label,
            handles.pe,
            handles.advanced,
            handles.refresh,
            handles.status,
            handles.primary,
        ]) {
            let _ = ShowWindow(control, SW_HIDE);
        }
        if let Some(page) = &self.backup_page {
            page.show(false);
        }
        if let Some(page) = &self.download_page {
            page.show(false);
        }
        if let Some(page) = &self.easy_page {
            page.show(false);
        }
        if let Some(page) = &self.tools_page {
            page.show(false);
        }
        if let Some(page) = &self.hardware_page {
            page.show(false);
        }
        if let Some(page) = &self.about_page {
            page.show(false);
        }
        if let Some(page) = &mut self.progress_page {
            page.set_completion(ProgressCompletion::Generic);
            page.update(initial);
            page.show(true);
        }
        self.layout(hwnd);
        if redraw.is_some() {
            redraw::resume(hwnd, redraw);
        } else {
            let _ = InvalidateRect(hwnd, None, false);
        }
        let _ = SetTimer(hwnd, timer_id, 100, None);
    }

    unsafe fn show_backup_progress(&mut self, hwnd: HWND, execution: BackupExecution) {
        self.backup_execution = Some(execution);
        self.enter_progress(
            hwnd,
            LongTaskProgress {
                title: crate::tr!("正在备份系统"),
                description: crate::tr!("正在创建系统镜像，请勿关闭程序。"),
                current_step: crate::tr!("正在准备备份任务..."),
                detail: String::new(),
                overall: ProgressValue::new(0, 100),
                step: ProgressValue::new(0, 100),
                status: ProgressStatus::Running,
                status_text: crate::tr!("准备中"),
                cancellable: true,
            },
            BACKUP_TIMER_ID,
        );
    }

    unsafe fn show_download_progress(&mut self, hwnd: HWND, worker: DownloadWorker) {
        self.download_worker = Some(worker);
        self.enter_progress(
            hwnd,
            LongTaskProgress {
                title: crate::tr!("正在下载"),
                description: crate::tr!("正在下载并验证所选文件，请勿关闭程序。"),
                current_step: crate::tr!("正在启动下载引擎..."),
                detail: String::new(),
                overall: ProgressValue::new(0, 100),
                step: ProgressValue::new(0, 100),
                status: ProgressStatus::Running,
                status_text: crate::tr!("准备中"),
                cancellable: true,
            },
            DOWNLOAD_TIMER_ID,
        );
    }

    unsafe fn start_install_execution(
        &mut self,
        hwnd: HWND,
        intent: crate::core::native_install_controller::StartInstallIntent,
    ) {
        if !self.refresh_partitions() {
            if let Some(handles) = &self.handles {
                set_text(
                    handles.status,
                    &crate::tr!("无法重新读取目标分区和 BitLocker 状态，安装已停止。"),
                );
            }
            return;
        }
        let partition = self.partitions.iter().find(|partition| {
            partition
                .letter
                .eq_ignore_ascii_case(&intent.target_partition)
        });
        let stable_target = partition.and_then(|partition| {
            Some(StableTargetIdentity {
                disk_number: partition.disk_number?,
                partition_number: partition.partition_number?,
            })
        });
        let expected_target = StableTargetIdentity {
            disk_number: intent.target_disk_number,
            partition_number: intent.target_partition_number,
        };
        if stable_target != Some(expected_target) {
            if let Some(handles) = &self.handles {
                set_text(
                    handles.status,
                    &crate::tr!("安装目标的磁盘或分区身份已变化，请重新选择目标后再试。"),
                );
            }
            return;
        }
        let pending = PendingBitLockerIntent::Install(intent.clone());
        match pending.locked_volumes(&self.partitions) {
            Ok(locked) if !locked.is_empty() => {
                self.begin_bitlocker_gate(hwnd, pending, locked);
                return;
            }
            Err(error) => {
                if let Some(handles) = &self.handles {
                    set_text(
                        handles.status,
                        &crate::tr!("无法检查 BitLocker 锁定卷：{}", error),
                    );
                }
                return;
            }
            _ => {}
        }
        let bitlocker = match partition.map(|partition| partition.bitlocker_status) {
            Some(crate::core::bitlocker::VolumeStatus::EncryptedLocked) => {
                BitLockerRequirement::UnlockRequired
            }
            Some(crate::core::bitlocker::VolumeStatus::Decrypting) => {
                BitLockerRequirement::AwaitDecryption
            }
            Some(crate::core::bitlocker::VolumeStatus::EncryptedUnlocked)
                if target_recovery_key_unavailable(&intent.target_partition) =>
            {
                BitLockerRequirement::AwaitDecryption
            }
            _ => BitLockerRequirement::Ready,
        };
        let context = InstallExecutionContext {
            stable_target,
            bitlocker,
        };
        if let Err(error) = NativeInstallExecutor::build_plan(&intent, &context) {
            log::error!("无法建立原生安装计划: {error}");
            if let Some(handles) = &self.handles {
                set_text(handles.status, &error.user_message());
            }
            return;
        }

        #[cfg(not(feature = "non-elevated-tests"))]
        if self.prepare_pe_download_for_install(hwnd, &intent) {
            return;
        }

        let (sender, receiver) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        self.install_auto_reboot = intent.options.auto_reboot;
        std::thread::spawn(move || {
            let mut backend = ProductionInstallBackend::new(&intent);
            let event_sender = sender.clone();
            let mut reporter = move |event| {
                let _ = event_sender.send(InstallWorkerMessage::Event(event));
            };
            let cancellation = || worker_cancel.load(Ordering::SeqCst);
            if let Err(error) = NativeInstallExecutor::execute(
                &intent,
                &context,
                &mut backend,
                &mut reporter,
                &cancellation,
            ) {
                let message = if matches!(
                    error,
                    crate::core::native_install_executor::InstallExecutionError::Cancelled
                ) {
                    InstallWorkerMessage::Cancelled
                } else {
                    log::error!("原生安装执行失败: {error}");
                    InstallWorkerMessage::Failed(error.user_message())
                };
                let _ = sender.send(message);
            }
        });
        self.install_messages = Some(receiver);
        self.install_cancel = Some(cancel);
        self.enter_progress(
            hwnd,
            LongTaskProgress {
                title: crate::tr!("正在安装系统"),
                description: crate::tr!("正在应用系统镜像和安装选项，请勿关闭程序。"),
                current_step: crate::tr!("正在执行安装前安全检查..."),
                detail: String::new(),
                overall: ProgressValue::new(0, 100),
                step: ProgressValue::new(0, 100),
                status: ProgressStatus::Running,
                status_text: crate::tr!("准备中"),
                cancellable: true,
            },
            INSTALL_TIMER_ID,
        );
    }

    #[cfg(not(feature = "non-elevated-tests"))]
    unsafe fn prepare_pe_download_for_install(
        &mut self,
        hwnd: HWND,
        intent: &crate::core::native_install_controller::StartInstallIntent,
    ) -> bool {
        if intent.mode != crate::core::native_install_controller::InstallMode::ViaPe {
            return false;
        }
        let Some(index) = intent.selected_pe else {
            return false;
        };
        let available = self.available_pe();
        let Some(pe) = available.get(index) else {
            if let Some(handles) = &self.handles {
                set_text(
                    handles.status,
                    &crate::tr!("所选 PE 环境已不可用，请刷新后重试。"),
                );
            }
            return true;
        };
        match crate::core::pe::PeManager::check_cached_pe(
            &pe.filename,
            pe.sha256.as_deref(),
            pe.md5.as_deref(),
        ) {
            Ok(lr_core::cached_artifact::CachedArtifactStatus::Ready { .. }) => false,
            Ok(lr_core::cached_artifact::CachedArtifactStatus::Missing) => {
                let integrity = match lr_core::download_integrity::select_expected_hash(
                    pe.sha256.as_deref(),
                    pe.md5.as_deref(),
                ) {
                    Ok(value) => value,
                    Err(error) => {
                        if let Some(handles) = &self.handles {
                            set_text(handles.status, &crate::tr!("PE 校验配置无效：{}", error));
                        }
                        return true;
                    }
                };
                let plan = crate::core::native_download_controller::DownloadPlan {
                    url: pe.download_url.clone(),
                    save_directory: crate::utils::path::get_pe_download_cache_dir(),
                    filename: pe.filename.clone(),
                    integrity,
                    completion: crate::core::native_download_controller::DownloadCompletion::None,
                    download_threads: self.app_config.download_threads,
                };
                match NativeDownloadExecutor::start(plan) {
                    Ok(worker) => {
                        self.pending_install_after_pe_download = Some(intent.clone());
                        self.show_download_progress(hwnd, worker);
                    }
                    Err(error) => {
                        if let Some(handles) = &self.handles {
                            set_text(
                                handles.status,
                                &crate::tr!("无法下载所需 PE 环境：{}", error),
                            );
                        }
                    }
                }
                true
            }
            Err(error) => {
                if let Some(handles) = &self.handles {
                    set_text(
                        handles.status,
                        &crate::tr!("缓存的 PE 文件未通过安全校验：{}", error),
                    );
                }
                true
            }
        }
    }

    unsafe fn poll_install_messages(&mut self, hwnd: HWND) {
        let mut messages = Vec::new();
        let mut disconnected = false;
        if let Some(receiver) = &self.install_messages {
            loop {
                match receiver.try_recv() {
                    Ok(message) => messages.push(message),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        let mut reboot_after_completion = false;
        for message in messages {
            let mut terminal = false;
            if let Some(page) = &mut self.progress_page {
                let mut state = page.state().clone();
                match message {
                    InstallWorkerMessage::Event(InstallExecutionEvent::Started {
                        total_phases,
                    }) => {
                        state.overall = ProgressValue::new(0, total_phases as u64);
                        state.current_step = crate::tr!("安装任务已启动");
                        state.status_text = crate::tr!("正在安装");
                    }
                    InstallWorkerMessage::Event(InstallExecutionEvent::PhaseStarted {
                        phase,
                        ..
                    }) => {
                        state.overall =
                            ProgressValue::new(u64::from(phase.weighted_overall_progress(0)), 100);
                        state.step = ProgressValue::new(0, 100);
                        state.current_step = install_phase_label(phase);
                    }
                    InstallWorkerMessage::Event(InstallExecutionEvent::Progress {
                        phase,
                        percentage,
                        detail,
                    }) => {
                        state.step = ProgressValue::new(u64::from(percentage), 100);
                        state.overall = ProgressValue::new(
                            u64::from(phase.weighted_overall_progress(percentage)),
                            100,
                        );
                        state.detail = detail;
                    }
                    InstallWorkerMessage::Event(InstallExecutionEvent::PhaseCompleted {
                        phase,
                        ..
                    }) => {
                        state.overall = ProgressValue::new(
                            u64::from(phase.weighted_overall_progress(100)),
                            100,
                        );
                        state.step = ProgressValue::new(100, 100);
                    }
                    InstallWorkerMessage::Event(InstallExecutionEvent::Completed(outcome)) => {
                        state.overall = ProgressValue::new(100, 100);
                        state.step = state.overall;
                        state.status = ProgressStatus::Succeeded;
                        state.cancellable = false;
                        state.current_step = crate::tr!("系统安装已完成");
                        state.status_text = match outcome {
                            crate::core::native_install_executor::InstallExecutionOutcome::DirectInstallCompleted => {
                                page.set_completion(ProgressCompletion::DirectInstall);
                                crate::tr!("系统安装已完成。")
                            }
                            crate::core::native_install_executor::InstallExecutionOutcome::ReadyToRebootIntoPe => {
                                page.set_completion(ProgressCompletion::ViaPePrepared);
                                crate::tr!("PE 环境准备完成，请选择立即重启或稍后重启。")
                            }
                        };
                        reboot_after_completion = self.install_auto_reboot;
                        terminal = true;
                    }
                    InstallWorkerMessage::Failed(error) => {
                        state.status = ProgressStatus::Failed;
                        state.current_step = crate::tr!("安装失败");
                        state.detail = error;
                        state.status_text = crate::tr!("安装已安全停止，请检查错误信息。");
                        terminal = true;
                    }
                    InstallWorkerMessage::Cancelled => {
                        state.status = ProgressStatus::Cancelled;
                        state.cancellable = false;
                        state.current_step = crate::tr!("安装已取消");
                        state.status_text =
                            crate::tr!("安装已在安全阶段停止。请检查目标分区状态后再重试。");
                        state.cancellable = false;
                        terminal = true;
                    }
                }
                page.update(state);
            }
            if terminal {
                self.install_messages = None;
                self.install_cancel = None;
                let _ = KillTimer(hwnd, INSTALL_TIMER_ID);
            }
        }
        if disconnected && self.install_messages.is_some() {
            if let Some(page) = &mut self.progress_page {
                let mut state = page.state().clone();
                state.status = ProgressStatus::Failed;
                state.current_step = crate::tr!("安装任务异常结束");
                state.status_text =
                    crate::tr!("安装工作线程未返回完成状态，请检查目标分区和日志后再重试。");
                state.cancellable = false;
                page.update(state);
            }
            self.install_messages = None;
            self.install_cancel = None;
            let _ = KillTimer(hwnd, INSTALL_TIMER_ID);
        }
        if reboot_after_completion {
            log::info!("安装完成，用户已选择立即重启");
            crate::core::pe::PeManager::reboot();
        }
    }

    unsafe fn poll_catalogue_messages(&mut self, hwnd: HWND) {
        let remote = match self
            .catalogue_messages
            .as_ref()
            .map(|receiver| receiver.try_recv())
        {
            None | Some(Err(std::sync::mpsc::TryRecvError::Empty)) => return,
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                self.catalogue_messages = None;
                let _ = KillTimer(hwnd, CATALOGUE_TIMER_ID);
                let error = crate::tr!("远程资源目录加载线程异常结束，请重试。");
                self.download_controller.fail_refresh(error.clone());
                if let Some(page) = &self.download_page {
                    set_text(
                        page.status,
                        &catalogue_status_message(self.download_controller.state()),
                    );
                }
                return;
            }
            Some(Ok(remote)) => remote,
        };

        self.catalogue_messages = None;
        let _ = KillTimer(hwnd, CATALOGUE_TIMER_ID);
        if !remote.loaded {
            let error = remote
                .error
                .unwrap_or_else(|| crate::tr!("远程资源目录加载失败"));
            self.download_controller.fail_refresh(error.clone());
            if let Some(page) = &self.download_page {
                set_text(
                    page.status,
                    &catalogue_status_message(self.download_controller.state()),
                );
            }
            return;
        }

        let catalogue = ConfigManager {
            systems: remote
                .dl_content
                .as_deref()
                .map(ConfigManager::parse_system_list)
                .unwrap_or_default(),
            pe_list: remote
                .pe_content
                .as_deref()
                .map(ConfigManager::parse_pe_list)
                .unwrap_or_default(),
            software_list: remote
                .soft_content
                .as_deref()
                .map(ConfigManager::parse_software_list)
                .unwrap_or_default(),
            gpu_driver_list: remote
                .gpu_content
                .as_deref()
                .map(ConfigManager::parse_gpu_driver_list)
                .unwrap_or_default(),
            ..ConfigManager::default()
        };
        self.download_controller
            .replace_trusted_remote_catalogue(&catalogue);
        if !catalogue.pe_list.is_empty() {
            let selected_install_pe = self.selected_install_pe_filename();
            self.pe_catalogue = catalogue.pe_list.clone();
            if let Err(error) = PeCache::save(&catalogue.pe_list) {
                log::warn!("刷新 PE 目录后保存本地缓存失败: {error}");
            }
            if let Some(page) = &mut self.backup_page {
                let labels: Vec<String> = catalogue
                    .pe_list
                    .iter()
                    .map(|pe| pe.display_name.clone())
                    .collect();
                page.replace_pe_labels(&labels);
            }
            self.update_backup_primary_state();
            if let Some(handles) = self.handles {
                self.populate_install_pe_combo(handles.pe, selected_install_pe.as_deref());
                let show_pe = self.page == Page::Install
                    && !self.easy_mode_enabled()
                    && !self.advanced_visible
                    && !self.progress_visible
                    && self.install_pe_selector_should_be_visible();
                let command = if show_pe { SW_SHOW } else { SW_HIDE };
                let _ = ShowWindow(handles.pe_label, command);
                let _ = ShowWindow(handles.pe, command);
                self.layout(hwnd);
                self.update_install_primary_state();
            }
        }
        if let Some(page) = &mut self.download_page {
            page.replace_rows(&self.download_controller.rows());
            set_text(
                page.status,
                &catalogue_status_message(self.download_controller.state()),
            );
        }

        let easy_config = remote
            .easy_content
            .as_deref()
            .and_then(crate::download::config::EasyModeConfig::parse);
        self.easy_controller
            .set_catalogue(easy_config.as_ref(), false);
        let easy_mode_enabled = self.easy_mode_enabled();
        if let Some(page) = &mut self.easy_page {
            page.update(&self.easy_controller.view());
            if self.page != Page::Install
                || !easy_mode_enabled
                || self.advanced_visible
                || self.progress_visible
            {
                page.show(false);
            }
        }
    }

    unsafe fn leave_progress(&mut self, hwnd: HWND) {
        self.leave_progress_to(hwnd, self.page);
    }

    unsafe fn leave_progress_to(&mut self, hwnd: HWND, destination: Page) {
        // Returning from a full-window task used to show and move every child one by one.  The
        // intermediate states were visible as a short flash, especially on software-rendered or
        // remote desktops.  Suspend painting until the destination page has its final layout.
        let redraw = redraw::suspend(hwnd);
        self.progress_visible = false;
        if let Some(page) = &self.progress_page {
            page.show(false);
        }
        if let Some(handles) = &self.handles {
            for control in handles.nav.into_iter().chain([
                handles.brand,
                handles.title,
                handles.description,
                handles.status,
            ]) {
                let _ = ShowWindow(control, SW_SHOW);
            }
        }
        self.select_page_impl(hwnd, destination, false);
        self.layout(hwnd);
        redraw::resume(hwnd, redraw);
    }

    unsafe fn handle_progress_command(&mut self, hwnd: HWND, intent: ProgressIntent) {
        match intent {
            ProgressIntent::CancelRequested => {
                if let Some(execution) = &self.backup_execution {
                    execution.request_cancel();
                    if let Some(page) = &mut self.progress_page {
                        let mut state = page.state().clone();
                        state.status = ProgressStatus::Cancelling;
                        state.status_text = crate::tr!("正在请求取消，请等待当前安全点...");
                        state.cancellable = false;
                        page.update(state);
                    }
                } else if let Some(cancel) = &self.install_cancel {
                    cancel.store(true, Ordering::SeqCst);
                    if let Some(page) = &mut self.progress_page {
                        let mut state = page.state().clone();
                        state.status = ProgressStatus::Cancelling;
                        state.status_text = crate::tr!("正在等待当前安装阶段安全停止...");
                        state.cancellable = false;
                        page.update(state);
                    }
                } else if let Some(worker) = &self.download_worker {
                    let _ = worker.send(DownloadWorkerCommand::Cancel);
                    if let Some(page) = &mut self.progress_page {
                        let mut state = page.state().clone();
                        state.status = ProgressStatus::Cancelling;
                        state.status_text = crate::tr!("正在取消下载...");
                        state.cancellable = false;
                        page.update(state);
                    }
                }
            }
            ProgressIntent::RestartNow => {
                #[cfg(feature = "non-elevated-tests")]
                log::warn!("开发隔离构建拒绝执行重启");
                #[cfg(not(feature = "non-elevated-tests"))]
                crate::core::pe::PeManager::reboot();
            }
            ProgressIntent::ContinueDownloadedInstallation => match self.download_follow_up.take() {
                Some(
                    crate::core::native_download_controller::DownloadCompletion::OpenSystemImage(
                        path,
                    ),
                ) => {
                    self.leave_progress_to(hwnd, Page::Install);
                    self.load_image_path(hwnd, path);
                }
                Some(
                    crate::core::native_download_controller::DownloadCompletion::RunDownloadedFile(
                        path,
                    ),
                ) => {
                    #[cfg(feature = "non-elevated-tests")]
                    log::warn!("开发隔离构建拒绝启动下载文件: {}", path.display());
                    #[cfg(not(feature = "non-elevated-tests"))]
                    {
                        let verb = wide("open");
                        let target = wide(&path);
                        let _ = ShellExecuteW(
                            hwnd,
                            PCWSTR(verb.as_ptr()),
                            PCWSTR(target.as_ptr()),
                            PCWSTR::null(),
                            PCWSTR::null(),
                            SW_SHOWNORMAL,
                        );
                    }
                    self.leave_progress(hwnd);
                }
                _ => self.leave_progress(hwnd),
            },
            ProgressIntent::Back
            | ProgressIntent::RestartLater
            | ProgressIntent::ReturnToDownloads => {
                self.download_follow_up = None;
                self.leave_progress(hwnd);
            }
        }
    }

    unsafe fn poll_download_messages(&mut self, hwnd: HWND) {
        let mut messages = Vec::new();
        let mut disconnected = false;
        if let Some(worker) = &self.download_worker {
            loop {
                match worker.try_recv() {
                    Ok(message) => messages.push(message),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        if messages.is_empty() && !disconnected {
            return;
        }

        // A worker can publish several progress snapshots before the 100 ms UI timer fires. Apply
        // them to one in-memory snapshot and repaint once, instead of erasing/repainting every
        // STATIC and owner-drawn bar for each queued message.
        let Some(page) = &self.progress_page else {
            return;
        };
        let mut state = page.state().clone();
        let mut terminal = false;
        let mut completion = None;
        let mut follow_up_result = None;
        let mut easy_install = None;
        let mut pe_install = None;
        let mut pe_backup = None;
        let mut pe_expand = None;
        for message in messages {
            if terminal {
                break;
            }
            match message {
                DownloadWorkerMessage::Starting => {
                    state.current_step = crate::tr!("正在连接下载服务器...");
                    state.status_text = crate::tr!("正在下载");
                }
                DownloadWorkerMessage::Progress {
                    completed_bytes,
                    total_bytes,
                    bytes_per_second,
                    paused,
                    ..
                } => {
                    state.overall = ProgressValue::new(completed_bytes, total_bytes);
                    state.step = state.overall;
                    state.current_step = if paused {
                        crate::tr!("下载已暂停")
                    } else {
                        crate::tr!("正在下载")
                    };
                    state.detail = crate::tr!(
                        "速度：{} MB/s",
                        format_args!("{:.1}", bytes_per_second as f64 / 1_048_576.0)
                    );
                }
                DownloadWorkerMessage::Verifying { algorithm } => {
                    state.current_step = crate::tr!("正在验证下载文件...");
                    state.detail = crate::tr!("校验算法：{}", format_args!("{algorithm:?}"));
                    state.cancellable = false;
                }
                DownloadWorkerMessage::Completed {
                    path,
                    integrity,
                    follow_up,
                } => {
                    state.overall = ProgressValue::new(100, 100);
                    state.step = state.overall;
                    state.status = ProgressStatus::Succeeded;
                    state.cancellable = false;
                    state.current_step = match integrity {
                        crate::core::native_download_executor::IntegrityOutcome::Passed(_) => {
                            crate::tr!("下载和完整性验证已完成")
                        }
                        crate::core::native_download_executor::IntegrityOutcome::NotProvided => {
                            crate::tr!("未提供文件校验值，已跳过完整性校验")
                        }
                    };
                    state.detail = path.display().to_string();
                    state.status_text = match &follow_up {
                            crate::core::native_download_controller::DownloadCompletion::None => {
                                crate::tr!("下载已完成，可返回在线下载页面。")
                            }
                            crate::core::native_download_controller::DownloadCompletion::OpenSystemImage(_) => {
                                crate::tr!("下载已完成，可继续选择安装目标。")
                            }
                            crate::core::native_download_controller::DownloadCompletion::RunDownloadedFile(_) => {
                                crate::tr!("下载已完成，可继续打开文件。")
                            }
                        };
                    completion = Some(
                        if matches!(
                            follow_up,
                            crate::core::native_download_controller::DownloadCompletion::None
                        ) {
                            DownloadCompletionAction::ReturnToDownloads
                        } else {
                            DownloadCompletionAction::ContinueInstallation
                        },
                    );
                    follow_up_result = Some(follow_up);
                    easy_install = self.pending_easy_install.take();
                    pe_install = self.pending_install_after_pe_download.take();
                    pe_backup = self.pending_backup_after_pe_download.take();
                    pe_expand = self.pending_expand_after_pe_download.take();
                    terminal = true;
                }
                DownloadWorkerMessage::Cancelled => {
                    self.pending_easy_install = None;
                    self.pending_install_after_pe_download = None;
                    self.pending_backup_after_pe_download = None;
                    self.pending_expand_after_pe_download = None;
                    self.download_follow_up = None;
                    state.status = ProgressStatus::Cancelled;
                    state.cancellable = false;
                    state.current_step = crate::tr!("下载已取消");
                    state.status_text = crate::tr!("未执行下载后的安装或打开操作。");
                    terminal = true;
                }
                DownloadWorkerMessage::Failed(error) => {
                    self.pending_easy_install = None;
                    self.pending_install_after_pe_download = None;
                    self.pending_backup_after_pe_download = None;
                    self.pending_expand_after_pe_download = None;
                    self.download_follow_up = None;
                    log::warn!(
                        "原生下载失败: stage={:?}, detail={}",
                        error.stage,
                        error.message
                    );
                    state.status = ProgressStatus::Failed;
                    state.cancellable = false;
                    state.current_step = crate::tr!("下载失败");
                    state.detail = download_failure_message(&error);
                    state.status_text = crate::tr!("未执行下载后的安装或打开操作。");
                    terminal = true;
                }
            }
        }
        if disconnected && !terminal {
            self.pending_easy_install = None;
            self.pending_install_after_pe_download = None;
            self.pending_backup_after_pe_download = None;
            self.pending_expand_after_pe_download = None;
            self.download_follow_up = None;
            state.status = ProgressStatus::Failed;
            state.current_step = crate::tr!("下载任务异常结束");
            state.detail = crate::tr!("下载工作线程未返回完成状态，请刷新资源后重试。");
            state.status_text = crate::tr!("未执行下载后的安装或打开操作。");
            state.cancellable = false;
            terminal = true;
        }
        let terminal_redraw_suspended = terminal && IsWindowVisible(hwnd).as_bool();
        if terminal_redraw_suspended {
            let _ = SendMessageW(hwnd, 0x000B, WPARAM(0), LPARAM(0)); // WM_SETREDRAW(FALSE)
        }
        if let Some(page) = &mut self.progress_page {
            if let Some(completion) = completion {
                page.set_completion(ProgressCompletion::Download(completion));
            }
            page.update(state);
        }
        if terminal {
            self.download_follow_up = follow_up_result;
            self.download_worker = None;
            let _ = KillTimer(hwnd, DOWNLOAD_TIMER_ID);
            // The terminal command set differs from the running Cancel button. Re-layout before a
            // single child redraw so the newly shown Return/Continue button never appears at 0,0.
            self.layout(hwnd);
            if terminal_redraw_suspended {
                let _ = SendMessageW(hwnd, 0x000B, WPARAM(1), LPARAM(0)); // WM_SETREDRAW(TRUE)
            }
            let _ = RedrawWindow(
                hwnd,
                None,
                None,
                RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_UPDATENOW,
            );
        }
        if let Some(intent) = easy_install {
            self.start_easy_install_after_download(hwnd, intent);
        } else if let Some(intent) = pe_install {
            self.start_install_execution(hwnd, intent);
        } else if pe_backup.is_some() {
            self.prepare_backup_from_page(hwnd);
        } else if let Some(request) = pe_expand {
            self.leave_progress(hwnd);
            if let Some(dialog) = &mut self.expand_c_dialog {
                dialog.show_modeless();
            }
            self.start_expand_c_execution(hwnd, request);
        }
    }

    unsafe fn start_easy_install_after_download(
        &mut self,
        hwnd: HWND,
        intent: crate::core::native_easy_mode_controller::StartEasyInstallIntent,
    ) {
        let inspected =
            match crate::core::native_image_source::inspect_image_source(&intent.download_path) {
                Ok(source) => source,
                Err(error) => {
                    self.fail_easy_install_after_download(error.to_string());
                    return;
                }
            };
        let (effective_image_path, selected_image, mounted_iso) = match inspected {
            crate::core::native_image_source::InspectedImageSource::WimFamily {
                effective_image_path,
                volumes,
                mounted_iso,
                ..
            } => {
                let Some(volume) = volumes.iter().find(|volume| {
                    volume.index == intent.volume_number && is_installable_image(volume)
                }) else {
                    if let Some(path) = mounted_iso {
                        let _ = crate::core::iso::IsoMounter::unmount_iso_by_path(
                            &path.to_string_lossy(),
                        );
                    }
                    self.fail_easy_install_after_download(crate::tr!(
                        "下载的系统镜像中不存在配置指定的可安装卷，请刷新在线资源后重试。"
                    ));
                    return;
                };
                (
                    effective_image_path,
                    SelectedImageMetadata {
                        volume_index: volume.index,
                        major_version: volume.major_version,
                        architecture: volume.architecture,
                    },
                    mounted_iso,
                )
            }
            other => {
                discard_stale_inspected_source(other);
                self.fail_easy_install_after_download(crate::tr!(
                    "下载完成的文件不是可用的 WIM、ESD 或 SWM 系统镜像。"
                ));
                return;
            }
        };
        if self.mounted_iso != mounted_iso {
            if let Some(previous) = self.mounted_iso.take() {
                let _ =
                    crate::core::iso::IsoMounter::unmount_iso_by_path(&previous.to_string_lossy());
            }
            self.mounted_iso = mounted_iso;
        }
        let target = self
            .partitions
            .get(intent.system_partition_index)
            .map(|partition| InstallTarget {
                partition: partition.letter.clone(),
                disk_number: partition.disk_number,
                partition_number: partition.partition_number,
                style: partition.partition_style,
                is_current_system: partition.is_system_partition,
                has_windows: partition.has_windows,
            });
        let pe = self.available_pe();
        let state = NativeInstallState {
            image_path: effective_image_path.to_string_lossy().into_owned(),
            image_ready: true,
            selected_image: Some(selected_image),
            xp_i386_source: None,
            target,
            is_pe_environment: crate::core::disk::DiskManager::is_pe_environment(),
            pe_available: !pe.is_empty(),
            selected_pe: (!pe.is_empty()).then_some(0),
            custom_unattend_path: String::new(),
            custom_unattend_error: None,
            partition_refresh_pending: false,
            partition_refresh_error: None,
            pca_detection_pending: false,
            pca_selection_error: None,
            advanced_options_enabled: false,
            prefs: intent.prefs,
        };
        match state.start_intent() {
            Ok(install) => self.start_install_execution(hwnd, install),
            Err(error) => {
                if let Some(page) = &mut self.progress_page {
                    let mut state = page.state().clone();
                    state.status = ProgressStatus::Failed;
                    state.current_step = crate::tr!("无法开始一键安装");
                    state.detail = error.to_string();
                    state.status_text = crate::tr!("下载已完成，但安装前安全检查未通过。");
                    page.update(state);
                }
            }
        }
    }

    unsafe fn fail_easy_install_after_download(&mut self, detail: String) {
        if let Some(page) = &mut self.progress_page {
            let mut state = page.state().clone();
            state.status = ProgressStatus::Failed;
            state.current_step = crate::tr!("无法开始一键安装");
            state.detail = detail;
            state.status_text = crate::tr!("下载已完成，但安装前安全检查未通过。");
            page.update(state);
        }
    }

    unsafe fn poll_backup_messages(&mut self, hwnd: HWND) {
        let mut messages = Vec::new();
        let mut disconnected = false;
        if let Some(execution) = &self.backup_execution {
            loop {
                match execution.messages.try_recv() {
                    Ok(message) => messages.push(message),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        for message in messages {
            let mut terminal = false;
            if let Some(page) = &mut self.progress_page {
                let mut state = page.state().clone();
                match message {
                    BackupWorkerMessage::Started { .. } => {
                        state.current_step = crate::tr!("备份任务已启动");
                        state.status_text = crate::tr!("正在备份");
                    }
                    BackupWorkerMessage::Progress { percentage, status } => {
                        state.overall = ProgressValue::new(u64::from(percentage), 100);
                        state.step = state.overall;
                        state.current_step = status;
                    }
                    BackupWorkerMessage::CancellationRequested {
                        operation_may_still_be_running,
                    } => {
                        state.status = ProgressStatus::Cancelling;
                        state.status_text = if operation_may_still_be_running {
                            crate::tr!("已请求取消；当前镜像引擎可能仍在完成安全收尾。")
                        } else {
                            crate::tr!("正在取消备份...")
                        };
                    }
                    BackupWorkerMessage::PeCommitStarted => {
                        state.current_step = crate::tr!("正在提交 PE 启动环境");
                        state.status_text = crate::tr!("已进入不可取消的提交阶段，请勿关闭程序。");
                        state.cancellable = false;
                    }
                    BackupWorkerMessage::Completed { mode } => {
                        state.overall = ProgressValue::new(100, 100);
                        state.step = state.overall;
                        state.status = ProgressStatus::Succeeded;
                        match mode {
                            crate::core::native_backup_controller::BackupLaunchMode::Direct => {
                                state.current_step = crate::tr!("系统备份已完成");
                                state.status_text = crate::tr!("备份文件已成功创建。");
                                page.set_completion(ProgressCompletion::Generic);
                            }
                            crate::core::native_backup_controller::BackupLaunchMode::ViaPe => {
                                state.current_step = crate::tr!("PE 备份环境准备完成");
                                state.status_text =
                                    crate::tr!("系统将在重启进入 PE 后执行实际备份。");
                                page.set_completion(ProgressCompletion::ViaPePrepared);
                            }
                        }
                        terminal = true;
                    }
                    BackupWorkerMessage::Cancelled { output_may_exist } => {
                        state.status = ProgressStatus::Cancelled;
                        state.current_step = crate::tr!("备份已取消");
                        state.status_text = if output_may_exist {
                            crate::tr!("目标位置可能保留未完成文件，请确认后再使用。")
                        } else {
                            crate::tr!("未创建备份文件。")
                        };
                        terminal = true;
                    }
                    BackupWorkerMessage::Failed { error, .. } => {
                        state.status = ProgressStatus::Failed;
                        state.current_step = crate::tr!("备份失败");
                        state.detail = error;
                        state.status_text = crate::tr!("请检查错误信息后重试。");
                        terminal = true;
                    }
                }
                page.update(state);
            }
            if terminal {
                self.backup_execution = None;
                let _ = KillTimer(hwnd, BACKUP_TIMER_ID);
            }
        }
        if disconnected && self.backup_execution.is_some() {
            if let Some(page) = &mut self.progress_page {
                let mut state = page.state().clone();
                state.status = ProgressStatus::Failed;
                state.current_step = crate::tr!("备份任务异常结束");
                state.status_text =
                    crate::tr!("备份工作线程未返回完成状态，请勿使用可能残留的输出文件。");
                state.cancellable = false;
                page.update(state);
            }
            self.backup_execution = None;
            let _ = KillTimer(hwnd, BACKUP_TIMER_ID);
        }
    }

    unsafe fn handle_primary_action(&mut self, hwnd: HWND) {
        match self.page {
            Page::Install => match self.install_intent() {
                Ok(intent) => {
                    if let Some(handles) = &self.handles {
                        set_text(
                            handles.status,
                            &crate::tr!("安装配置已通过安全校验，正在准备执行环境。"),
                        );
                    }
                    log::info!(
                        "原生安装意图已生成: mode={:?}, target={}, volume={}",
                        intent.mode,
                        intent.target_partition,
                        intent.volume_index
                    );
                    self.start_install_execution(hwnd, intent);
                }
                Err(error) => {
                    if let Some(handles) = &self.handles {
                        set_text(handles.status, &error.to_string());
                    }
                }
            },
            Page::Backup => self.prepare_backup_from_page(hwnd),
            Page::Hardware => {
                if let Some(page) = &self.hardware_page {
                    if let Err(error) = clipboard_win::set_clipboard_string(&page.report_text()) {
                        log::warn!("复制硬件信息失败: {error}");
                    } else if let Some(handles) = self.handles {
                        self.hardware_copy_feedback.start();
                        set_text(handles.primary, &crate::tr!("已复制"));
                        let _ = KillTimer(hwnd, HARDWARE_COPY_TIMER_ID);
                        let _ = SetTimer(hwnd, HARDWARE_COPY_TIMER_ID, 3_000, None);
                        let _ = InvalidateRect(handles.primary, None, false);
                    }
                }
            }
            Page::About => PostQuitMessage(0),
            Page::Download | Page::Tools => {}
        }
    }

    unsafe fn open_about_link(&self, hwnd: HWND, link: AboutLink) {
        let url = match link {
            AboutLink::ProjectHomepage => "https://letrecovery.net",
            AboutLink::Documentation => "https://github.com/NORMAL-EX/LetRecovery/issues",
            AboutLink::License => "https://github.com/NORMAL-EX/LetRecovery/blob/main/LICENSE",
        };
        let url = wide(url);
        let result = ShellExecuteW(
            hwnd,
            w!("open"),
            PCWSTR(url.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        if result.0 as isize <= 32 {
            log::warn!("打开关于页链接失败: {link:?}");
        }
    }

    unsafe fn open_diskpart_script_directory(&self, hwnd: HWND) {
        let directory = crate::utils::path::get_bin_dir().join("diskpart");
        if let Err(error) = std::fs::create_dir_all(&directory) {
            self.show_information(
                hwnd,
                crate::tr!("无法打开脚本目录"),
                crate::tr!("创建目录失败：{}", error),
            );
            return;
        }
        let target = wide(&directory);
        let result = ShellExecuteW(
            hwnd,
            w!("open"),
            PCWSTR(target.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        if result.0 as isize <= 32 {
            self.show_information(
                hwnd,
                crate::tr!("无法打开脚本目录"),
                crate::tr!("Windows 无法打开目录：{}", directory.display()),
            );
        }
    }

    unsafe fn edit_repair_boot_script(&self, hwnd: HWND) {
        let file = crate::utils::path::get_bin_dir().join("repair_boot.txt");
        if let Some(parent) = file.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                self.show_information(
                    hwnd,
                    crate::tr!("无法编辑引导命令"),
                    crate::tr!("创建目录失败：{}", error),
                );
                return;
            }
        }
        let target = wide(&file);
        let result = ShellExecuteW(
            hwnd,
            w!("open"),
            w!("notepad.exe"),
            PCWSTR(target.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        if result.0 as isize <= 32 {
            self.show_information(
                hwnd,
                crate::tr!("无法编辑引导命令"),
                crate::tr!("Windows 无法启动记事本打开：{}", file.display()),
            );
        }
    }

    unsafe fn relocalize_after_language_change(&mut self, hwnd: HWND) {
        // SetWindowText, ComboBox resets and ListView repopulation each invalidate their own
        // native HWND. Keep those intermediate mixed-language frames hidden and publish one
        // complete non-client/client/descendant transaction after layout has stabilised.
        let redraw = redraw::suspend(hwnd);
        set_text(hwnd, &crate::build_info::window_title());
        if let Some(handles) = &self.handles {
            for (control, label) in handles.nav.into_iter().zip([
                crate::tr!("系统安装"),
                crate::tr!("系统备份"),
                crate::tr!("在线下载"),
                crate::tr!("工具箱"),
                crate::tr!("硬件信息"),
                crate::tr!("关于"),
            ]) {
                set_text(control, &label);
            }

            set_text(handles.image_label, &crate::tr!("系统镜像:"));
            set_text(handles.browse, &crate::tr!("浏览..."));
            set_text(handles.image_volume_label, &crate::tr!("镜像卷:"));
            set_text(handles.partitions_label, &crate::tr!("选择安装分区:"));
            set_text(handles.format, &crate::tr!("格式化分区"));
            set_text(handles.boot, &crate::tr!("添加引导"));
            set_text(handles.unattend, &crate::tr!("无人值守"));
            set_text(handles.unattend_browse, &crate::tr!("选择无人值守文件..."));
            set_text(handles.unattend_clear, &crate::tr!("清除"));
            if self.custom_unattend_path.trim().is_empty() {
                set_text(
                    handles.unattend_path,
                    &crate::tr!("未选择则使用内置生成的无人值守配置"),
                );
            }
            set_text(handles.driver_label, &crate::tr!("驱动:"));
            replace_combo_labels(
                handles.driver,
                &[
                    crate::tr!("自动导入"),
                    crate::tr!("仅导出"),
                    crate::tr!("跳过"),
                ],
            );
            set_text(handles.reboot, &crate::tr!("立即重启"));
            set_text(handles.boot_label, &crate::tr!("引导模式:"));
            replace_combo_labels(
                handles.boot_mode,
                &[crate::tr!("自动"), crate::tr!("UEFI"), crate::tr!("Legacy")],
            );
            set_text(handles.pca_label, &crate::tr!("启动签名:"));
            self.update_pca_combo_labels();
            set_text(handles.run_diskpart, &crate::tr!("运行Diskpart脚本"));
            set_text(handles.open_diskpart_dir, &crate::tr!("打开目录"));
            set_text(handles.edit_boot_commands, &crate::tr!("修改引导命令"));
            set_text(handles.pe_label, &crate::tr!("PE 环境:"));
            set_text(handles.advanced, &crate::tr!("高级选项..."));
            set_text(handles.refresh, &crate::tr!("刷新分区"));
            update_list_column_titles(
                handles.partitions,
                &[
                    crate::tr!("分区卷"),
                    crate::tr!("总空间"),
                    crate::tr!("可用空间"),
                    crate::tr!("卷标"),
                    crate::tr!("分区表"),
                    crate::tr!("BitLocker"),
                    crate::tr!("状态"),
                ],
            );
            let long_state_labels = crate::tr!("未加密").chars().count() > 6;
            let _ = SendMessageW(
                handles.partitions,
                0x101E,
                WPARAM(5),
                LPARAM(self.scale(if long_state_labels { 120 } else { 92 }) as isize),
            );
            let _ = SendMessageW(
                handles.partitions,
                0x101E,
                WPARAM(6),
                LPARAM(self.scale(if long_state_labels { 148 } else { 80 }) as isize),
            );
            let _ = SendMessageW(handles.partitions, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));
            self.populate_partitions(handles.partitions, false);
        }

        let backup_rows = self.backup_partition_rows();
        if let Some(page) = &self.backup_page {
            let selected = page.read_state().source_partition;
            page.relocalize();
            page.replace_partitions(&backup_rows, selected);
        }
        if let Some(page) = &self.download_page {
            page.relocalize(&DownloadLabels {
                system_tab: &crate::tr!("系统镜像"),
                software_tab: &crate::tr!("常用软件"),
                gpu_driver_tab: &crate::tr!("显卡驱动"),
                status_ready: &self.initial_download_status(),
                name_column: &crate::tr!("名称"),
                type_column: &crate::tr!("类型"),
                size_column: &crate::tr!("大小"),
                save_path: &crate::tr!("保存位置:"),
                browse: &crate::tr!("浏览..."),
                refresh: &crate::tr!("刷新"),
                download: &crate::tr!("下载"),
                install: &crate::tr!("安装"),
            });
        }
        if let Some(page) = &mut self.easy_page {
            page.relocalize(&EasyModeLabels {
                enabled: &crate::tr!("启用小白模式"),
                settings_tip: &crate::tr!("可在“关于”页面随时关闭小白模式。"),
                dismiss_tip: &crate::tr!("不再提示"),
                system: &crate::tr!("选择系统:"),
                volume: &crate::tr!("选择版本:"),
                loading: &crate::tr!("正在加载系统列表..."),
                install: &crate::tr!("一键安装"),
            });
        }
        if let Some(page) = &self.tools_page {
            page.relocalize(&ToolLabels {
                introduction: &crate::tr!("选择要运行的系统维护、修复或诊断工具。"),
                buttons: [
                    &crate::tr!("卸载 NVIDIA 驱动"),
                    &crate::tr!("分区对拷"),
                    &crate::tr!("批量格式化"),
                    &crate::tr!("导入存储驱动"),
                    &crate::tr!("一键分区"),
                    &crate::tr!("移除 APPX"),
                    &crate::tr!("驱动备份与恢复"),
                    &crate::tr!("修复系统引导"),
                    &crate::tr!("网络信息"),
                    &crate::tr!("软件列表"),
                    &crate::tr!("时间同步"),
                    &crate::tr!("运行 Ghost"),
                    &crate::tr!("查看 GHO 密码"),
                    &crate::tr!("重置网络"),
                    &crate::tr!("磁盘空间分析"),
                    &crate::tr!("校验系统镜像"),
                    &crate::tr!("管理 BitLocker"),
                    &crate::tr!("文件哈希校验"),
                    &crate::tr!("重置系统密码"),
                ],
            });
        }
        if let Some(page) = &self.hardware_page {
            page.relocalize(&HardwareLabels {
                introduction: &crate::tr!("当前计算机的系统和硬件摘要。"),
                loading: &crate::tr!("启动时未能读取硬件信息。请重新启动程序后重试。"),
                save: &crate::tr!("保存..."),
            });
            if let Some(info) = &self.config.hardware_info {
                page.set_rows(hardware_info_rows(info, self.config.system_info.as_ref()));
            }
        }
        if let Some(page) = &self.advanced_page {
            page.relocalize();
        }
        let easy_mode_available = !self
            .config
            .system_info
            .as_ref()
            .is_some_and(|info| info.is_pe_environment);
        if let Some(page) = &self.about_page {
            page.relocalize(easy_mode_available);
        }
        if let Some(dialog) = &mut self.hardware_inspector_dialog {
            dialog.relocalize();
        }
        self.update_system_status();
        self.select_page(hwnd, self.page);
        self.layout(hwnd);
        redraw::resume(hwnd, redraw);
    }

    unsafe fn save_hardware_report(&self, hwnd: HWND) {
        let Some(page) = &self.hardware_page else {
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Text", &["txt"])
            .set_file_name("LetRecovery-hardware-info.txt")
            .save_file()
        else {
            return;
        };
        let path = if path.extension().is_none() {
            path.with_extension("txt")
        } else {
            path
        };
        let report = format!("\u{feff}{}", page.report_text());
        match std::fs::write(&path, report) {
            Ok(()) => self.show_information(
                hwnd,
                crate::tr!("硬件信息已保存"),
                crate::tr!("硬件信息已保存到：{}", path.display()),
            ),
            Err(error) => self.show_information(
                hwnd,
                crate::tr!("保存硬件信息失败"),
                crate::tr!("无法写入文件：{}", error),
            ),
        }
    }

    unsafe fn open_log_directory(&self, hwnd: HWND) {
        let path = crate::utils::logger::LogManager::get_log_dir();
        if !path.exists() {
            self.show_information(
                hwnd,
                crate::tr!("日志目录不可用"),
                crate::tr!("当前尚未生成日志目录。"),
            );
            return;
        }
        let target = wide(&path);
        let result = ShellExecuteW(
            hwnd,
            w!("open"),
            PCWSTR(target.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        if result.0 as isize <= 32 {
            log::warn!("打开日志目录失败: {}", path.display());
        }
    }

    unsafe fn show_information(&self, hwnd: HWND, title: String, description: String) {
        let spec = DialogSpec {
            window_title: title.clone(),
            title,
            description,
            width: 620,
            height: 220,
            buttons: DialogButtons {
                primary: crate::tr!("确定"),
                secondary: None,
                cancel: None,
            },
        };
        if let Ok(mut dialog) = DialogShell::create(hwnd, spec) {
            let _ = dialog.show_modal();
        }
    }
}

unsafe fn set_text(hwnd: HWND, value: &str) {
    let value = wide(value);
    let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(hwnd, PCWSTR(value.as_ptr()));
}

unsafe fn replace_combo_labels(combo: HWND, labels: &[String]) {
    let selected = SendMessageW(combo, 0x0147, WPARAM(0), LPARAM(0)).0;
    let _ = SendMessageW(combo, 0x014B, WPARAM(0), LPARAM(0));
    for label in labels {
        let label = wide(label);
        let _ = SendMessageW(combo, 0x0143, WPARAM(0), LPARAM(label.as_ptr() as isize));
    }
    let selected = usize::try_from(selected)
        .ok()
        .filter(|index| *index < labels.len())
        .unwrap_or(usize::MAX);
    let _ = SendMessageW(combo, 0x014E, WPARAM(selected), LPARAM(0));
}

unsafe fn update_list_column_titles(list: HWND, titles: &[String]) {
    for (index, title) in titles.iter().enumerate() {
        let mut title = wide(title);
        let mut column = LVCOLUMNW {
            mask: LVCF_TEXT,
            pszText: windows::core::PWSTR(title.as_mut_ptr()),
            ..Default::default()
        };
        let _ = SendMessageW(
            list,
            0x1060,
            WPARAM(index),
            LPARAM((&mut column as *mut LVCOLUMNW) as isize),
        );
    }
}

unsafe fn get_text(hwnd: HWND) -> String {
    let length = GetWindowTextLengthW(hwnd).max(0) as usize;
    let mut buffer = vec![0u16; length + 1];
    let copied = windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(hwnd, &mut buffer);
    String::from_utf16_lossy(&buffer[..copied.max(0) as usize])
}

fn install_phase_label(
    phase: crate::core::native_install_executor::InstallExecutionPhase,
) -> String {
    use crate::core::native_install_executor::InstallExecutionPhase as Phase;
    match phase {
        Phase::InspectBitLocker => crate::tr!("检查 BitLocker"),
        Phase::AwaitBitLockerDecryption => crate::tr!("等待 BitLocker 解密"),
        Phase::VerifyPcaBeforeDiskWrite => crate::tr!("检查启动签名兼容性"),
        Phase::ResolveStableTarget => crate::tr!("确认目标磁盘"),
        Phase::RunDiskpartScripts => crate::tr!("执行分区脚本"),
        Phase::ResolveTargetAfterDiskpart => crate::tr!("重新确认目标分区"),
        Phase::FormatTarget => crate::tr!("格式化目标分区"),
        Phase::ExportHostDrivers => crate::tr!("导出驱动"),
        Phase::ApplyXpTextModeSource => crate::tr!("准备 XP/2003 文本安装"),
        Phase::ApplyGhostImage => crate::tr!("恢复 Ghost 镜像"),
        Phase::ApplyWimImage => crate::tr!("释放系统镜像"),
        Phase::ProcessDrivers => crate::tr!("处理驱动"),
        Phase::RepairBoot => crate::tr!("修复引导"),
        Phase::ApplyAdvancedOptions => crate::tr!("应用高级选项"),
        Phase::FinishDirectInstall => crate::tr!("完成安装"),
        Phase::VerifyPeEnvironment => crate::tr!("验证 PE 环境"),
        Phase::InstallPeBootEntry => crate::tr!("安装 PE 启动项"),
        Phase::SelectDataPartition => crate::tr!("选择数据分区"),
        Phase::PersistPcaCompatibilityPackage => crate::tr!("准备启动签名兼容包"),
        Phase::ExportDriversToPeData => crate::tr!("导出驱动到 PE 数据区"),
        Phase::VerifySourceImage => crate::tr!("校验镜像"),
        Phase::CopySourceImage => crate::tr!("复制镜像文件"),
        Phase::StageUefiSeven => crate::tr!("准备 UEFI 兼容文件"),
        Phase::StageUserDrivers => crate::tr!("准备用户驱动"),
        Phase::WritePeInstallConfig => crate::tr!("写入配置文件"),
        Phase::ReadyToRebootIntoPe => crate::tr!("准备重启"),
    }
}

fn is_installable_image(volume: &crate::core::dism::ImageInfo) -> bool {
    use lr_core::image_meta::WimImageType;

    match volume.image_type {
        WimImageType::StandardInstall | WimImageType::FullBackup => return true,
        WimImageType::WindowsPE => return false,
        WimImageType::Unknown => {}
    }
    let name = volume.name.to_lowercase();
    let install_type = volume.installation_type.to_lowercase();
    if install_type == "windowspe"
        || ["windows pe", "windows setup", "setup media", "winpe"]
            .iter()
            .any(|keyword| name.contains(keyword))
    {
        return false;
    }
    if install_type.is_empty() && volume.major_version.is_none() {
        return [
            "windows 10",
            "windows 11",
            "windows server",
            "windows 8",
            "windows 7",
            "backup",
            "备份",
            "系统镜像",
            "镜像",
        ]
        .iter()
        .any(|keyword| name.contains(keyword));
    }
    true
}

unsafe fn discard_stale_inspected_source(
    source: crate::core::native_image_source::InspectedImageSource,
) {
    use crate::core::native_image_source::InspectedImageSource;
    let mounted_iso = match source {
        InspectedImageSource::WimFamily { mounted_iso, .. }
        | InspectedImageSource::XpTextMode { mounted_iso, .. } => mounted_iso,
        InspectedImageSource::Ghost { .. } => None,
    };
    if let Some(path) = mounted_iso {
        if let Err(error) =
            crate::core::iso::IsoMounter::unmount_iso_by_path(&path.to_string_lossy())
        {
            log::warn!("清理过期镜像请求的 ISO 挂载失败: {error}");
        }
    }
}

#[cfg(not(feature = "non-elevated-tests"))]
fn target_recovery_key_unavailable(volume: &str) -> bool {
    match crate::core::bitlocker::BitLockerManager::new().get_recovery_key(volume) {
        Ok(_) => false,
        Err(error) => {
            log::warn!(
                "目标卷 {volume} 无法读取 BitLocker 恢复密钥，将在继续安装前彻底解密：{error}"
            );
            true
        }
    }
}

#[cfg(feature = "non-elevated-tests")]
const fn target_recovery_key_unavailable(_volume: &str) -> bool {
    false
}

pub(super) unsafe fn load_application_icons(
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

pub fn run(config: Arc<PreloadedConfig>) -> windows::core::Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let controls = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            // The v6 manifest selects visual styles; initializing standard classes before any
            // page HWND is created lets Edit/Combo use those host styles as documented.
            dwICC: ICC_LISTVIEW_CLASSES | ICC_STANDARD_CLASSES,
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
            return Err(windows::core::Error::from_win32());
        }
        let mut state = Box::new(NativeWindow::new(config));
        let title_text = crate::build_info::window_title();
        let title = wide(&title_text);
        let initial_dpi = GetDpiForSystem().max(96) as i32;
        let screen_width = GetSystemMetrics(SM_CXSCREEN);
        let screen_height = GetSystemMetrics(SM_CYSCREEN);
        let (window_width, window_height) =
            preferred_window_size(initial_dpi, screen_width, screen_height);
        let hwnd = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            CLASS_NAME,
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            window_width,
            window_height,
            HWND::default(),
            HMENU::default(),
            HINSTANCE(instance.0),
            Some((&mut *state as *mut NativeWindow).cast()),
        )?;
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
        // Reconcile the size after the HWND is assigned to its actual monitor. On some
        // per-monitor-v2 configurations `GetDpiForSystem` still reports 96 during startup.
        let actual_dpi = GetDpiForWindow(hwnd).max(96) as i32;
        if actual_dpi != initial_dpi {
            let (corrected_width, corrected_height) =
                preferred_window_size(actual_dpi, screen_width, screen_height);
            let _ = SetWindowPos(
                hwnd,
                HWND::default(),
                0,
                0,
                corrected_width,
                corrected_height,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        // Keep the first child-control paint transaction outside the visible DWM surface. A
        // non-zero alpha still owns hit testing, unlike alpha=0, while making USER32's stock
        // white Edit/Combo/STATIC intermediate pixels effectively invisible. Reduced WinPE
        // implementations may reject layered attributes; in that case the synchronous redraw
        // below remains the supported fallback.
        let original_ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let _ = SetWindowLongPtrW(
            hwnd,
            GWL_EXSTYLE,
            original_ex_style | WS_EX_LAYERED.0 as isize,
        );
        let first_frame_layered =
            SetLayeredWindowAttributes(hwnd, COLORREF(0), 1, LWA_ALPHA).is_ok();
        let _ = ShowWindow(hwnd, SW_SHOW);
        // WinPE's reduced USER32/UxTheme implementation can defer the first paint of child
        // controls until later messages. If we enter the message loop immediately, the
        // compositor may present stock white Edit/Combo/List surfaces before their installed
        // subclasses and palette get a turn to paint. Complete one whole-window paint
        // transaction synchronously while still inside the initial ShowWindow call sequence.
        // Omitting RDW_ERASE is intentional: erasing with a stock class brush would recreate the
        // white intermediate frame this startup barrier is meant to prevent.
        let _ = RedrawWindow(
            hwnd,
            None,
            None,
            RDW_INVALIDATE | RDW_FRAME | RDW_ALLCHILDREN | RDW_UPDATENOW,
        );
        if first_frame_layered {
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
            let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, original_ex_style);
            let _ = SetWindowPos(
                hwnd,
                HWND::default(),
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let _ = RedrawWindow(
                hwnd,
                None,
                None,
                RDW_INVALIDATE | RDW_FRAME | RDW_ALLCHILDREN | RDW_UPDATENOW,
            );
        }
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        drop(state);
        Ok(())
    }
}

unsafe fn is_non_click_button_notification(source: HWND, notification: u16) -> bool {
    if source.0.is_null() || notification == BN_CLICKED as u16 {
        return false;
    }
    let mut class_name = [0u16; 32];
    let length = GetClassNameW(source, &mut class_name);
    length > 0
        && String::from_utf16_lossy(&class_name[..length as usize]).eq_ignore_ascii_case("Button")
}

unsafe fn control_has_class(control: HWND, expected: &str) -> bool {
    if control.0.is_null() {
        return false;
    }
    let mut class_name = [0u16; 32];
    let length = GetClassNameW(control, &mut class_name);
    length > 0
        && String::from_utf16_lossy(&class_name[..length as usize]).eq_ignore_ascii_case(expected)
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
    let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut NativeWindow;
    let state = state_ptr.as_mut();
    match message {
        WM_CREATE => {
            if let Some(state) = state {
                if let Err(error) = state.create_children(hwnd) {
                    log::error!("创建原生 Win32 控件失败: {error}");
                    return LRESULT(-1);
                }
                #[cfg(not(feature = "non-elevated-tests"))]
                state.handle_download_intent(hwnd, DownloadIntent::RefreshCatalogue);
            }
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            let minmax = lparam.0 as *mut MINMAXINFO;
            if !minmax.is_null() {
                let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                let mut monitor_info = MONITORINFO {
                    cbSize: size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                let (work_width, work_height) =
                    if GetMonitorInfoW(monitor, &mut monitor_info).as_bool() {
                        (
                            monitor_info.rcWork.right - monitor_info.rcWork.left,
                            monitor_info.rcWork.bottom - monitor_info.rcWork.top,
                        )
                    } else {
                        (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN))
                    };
                let (minimum_width, minimum_height) =
                    minimum_window_size(GetDpiForWindow(hwnd) as i32, work_width, work_height);
                (*minmax).ptMinTrackSize.x = minimum_width;
                (*minmax).ptMinTrackSize.y = minimum_height;
            }
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(state) = state {
                state.layout(hwnd);
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            if let Some(state) = state {
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
                state.dpi = GetDpiForWindow(hwnd);
                state.create_fonts();
                state.apply_fonts();
                state.layout(hwnd);
            }
            LRESULT(0)
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED | WM_SYSCOLORCHANGE => {
            if let Some(state) = state {
                state.refresh_system_theme(hwnd);
            }
            LRESULT(0)
        }
        WM_DEVICECHANGE => {
            if let Some(state) = state {
                if device_change_requests_partition_refresh(wparam.0) {
                    state.schedule_partition_refresh(hwnd);
                    return LRESULT(1);
                }
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_TOOL_WORKER_READY => {
            if let Some(state) = state {
                state.poll_tool_dialogs(hwnd);
            }
            LRESULT(0)
        }
        WM_HARDWARE_INFO_READY => {
            if let Some(state) = state {
                let result = Box::from_raw(
                    lparam.0 as *mut Option<crate::core::hardware_info::HardwareInfo>,
                );
                if let Some(page) = &state.hardware_page {
                    if let Some(info) = result.as_ref() {
                        page.set_rows(hardware_info_rows(info, state.config.system_info.as_ref()));
                    } else {
                        page.set_rows(vec![HardwareInfoRow {
                            category: crate::tr!("状态"),
                            item: crate::tr!("硬件信息"),
                            value: crate::tr!("读取硬件信息失败，请稍后重试。"),
                        }]);
                    }
                }
            }
            LRESULT(0)
        }
        WM_PCA_FIRMWARE_READY => {
            if let Some(state) = state {
                let firmware = *Box::from_raw(lparam.0 as *mut lr_core::boot_pca::FirmwarePcaInfo);
                state.pca_firmware = Some(firmware);
                state.pca_detection_pending = false;
                state.update_pca_combo_labels();
                state.update_pca_detection_status();
                state.update_install_primary_state();
            }
            LRESULT(0)
        }
        WM_PCA_TARGET_READY => {
            if let Some(state) = state {
                let message = *Box::from_raw(lparam.0 as *mut PcaTargetMessage);
                if pca_target_result_is_current(
                    state.pca_target_generation,
                    state.pca_target_key.as_ref(),
                    &message,
                ) {
                    state.pca_target_detection_pending = false;
                    state.pca_target_detection_error = message.result.err();
                    state.update_pca_detection_status();
                    if state.partition_refresh_requested {
                        state.schedule_partition_refresh(hwnd);
                    } else {
                        state.update_install_primary_state();
                    }
                }
            }
            LRESULT(0)
        }
        WM_PARTITIONS_READY => {
            if let Some(state) = state {
                let message = *Box::from_raw(lparam.0 as *mut PartitionRefreshMessage);
                state.finish_partition_refresh(hwnd, message);
            }
            LRESULT(0)
        }
        WM_INSTALL_PARTITION_SELECTION_CHANGED => {
            if let Some(state) = state {
                state.install_selection_update_pending = false;
                let redraw = redraw::suspend(hwnd);
                state.handle_install_partition_changed(hwnd);
                redraw::resume_client(hwnd, redraw);
            }
            LRESULT(0)
        }
        WM_IMAGE_INFO_READY => {
            if let Some(state) = state {
                let message = Box::from_raw(lparam.0 as *mut ImageInfoMessage);
                if message.generation != state.image_request_generation
                    || get_text(
                        state
                            .handles
                            .as_ref()
                            .map(|handles| handles.image_edit)
                            .unwrap_or_default(),
                    ) != message.requested_path
                {
                    if let Ok(source) = message.result {
                        discard_stale_inspected_source(source);
                    }
                    return LRESULT(0);
                }
                let Some(handles) = state.handles else {
                    return LRESULT(0);
                };
                let publish_install_chrome = may_publish_install_chrome(
                    state.page,
                    state.advanced_visible,
                    state.progress_visible,
                );
                match message.result {
                    Ok(source) => {
                        use crate::core::native_image_source::InspectedImageSource;
                        state.image_volumes.clear();
                        state.effective_image_path = None;
                        state.xp_i386_source = None;
                        state.mounted_iso = None;
                        match source {
                            InspectedImageSource::WimFamily {
                                effective_image_path,
                                volumes,
                                mounted_iso,
                                ..
                            } => {
                                state.effective_image_path =
                                    Some(effective_image_path.to_string_lossy().into_owned());
                                state.image_volumes =
                                    volumes.into_iter().filter(is_installable_image).collect();
                                state.mounted_iso = mounted_iso;
                            }
                            InspectedImageSource::Ghost { path } => {
                                state.effective_image_path =
                                    Some(path.to_string_lossy().into_owned());
                            }
                            InspectedImageSource::XpTextMode {
                                i386_directory,
                                mounted_iso,
                                ..
                            } => {
                                state.xp_i386_source =
                                    Some(i386_directory.to_string_lossy().into_owned());
                                state.mounted_iso = mounted_iso;
                            }
                        }
                        let _ = SendMessageW(handles.image_volume, 0x014B, WPARAM(0), LPARAM(0));
                        for volume in &state.image_volumes {
                            let label = wide(format!("{}. {}", volume.index, volume.name));
                            let _ = SendMessageW(
                                handles.image_volume,
                                0x0143,
                                WPARAM(0),
                                LPARAM(label.as_ptr() as isize),
                            );
                        }
                        if state.xp_i386_source.is_some() {
                            if publish_install_chrome {
                                set_text(
                                    handles.status,
                                    &crate::tr!("已识别 XP/2003 文本模式安装源。"),
                                );
                            }
                        } else if state.image_volumes.is_empty()
                            && state.effective_image_path.as_deref().is_some_and(|path| {
                                !matches!(
                                    crate::core::native_image_source::classify_image_source(
                                        std::path::Path::new(path)
                                    ),
                                    crate::core::native_image_source::ImageSourceKind::Ghost
                                )
                            })
                        {
                            if publish_install_chrome {
                                set_text(
                                    handles.status,
                                    &crate::tr!("系统镜像中没有可用的安装卷。"),
                                );
                            }
                        } else if state.image_volumes.is_empty() {
                            if publish_install_chrome {
                                set_text(handles.status, &crate::tr!("Ghost 镜像已就绪。"));
                            }
                        } else {
                            let _ =
                                SendMessageW(handles.image_volume, 0x014E, WPARAM(0), LPARAM(0));
                            state.update_storage_driver_default();
                            state.update_advanced_install_context();
                            if publish_install_chrome {
                                set_text(
                                    handles.status,
                                    &crate::tr!("系统镜像读取完成，请选择目标分区。"),
                                );
                            }
                        }
                        let has_image_volume_row = !state.image_volumes.is_empty();
                        state.set_install_volume_row_visible(hwnd, has_image_volume_row);
                        state.refresh_source_unattend();
                        state.update_unattend_conflict();
                        state.request_pca_target_detection(hwnd);
                        state.update_pca_detection_status();
                        state.update_install_primary_state();
                    }
                    Err(error) => {
                        state.image_volumes.clear();
                        state.clear_pca_target_detection();
                        state.update_advanced_install_context();
                        state.source_has_unattend = false;
                        state.apply_unattend_default();
                        state.set_install_volume_row_visible(hwnd, false);
                        if publish_install_chrome {
                            set_text(handles.status, &crate::tr!("读取系统镜像失败：{}", error));
                            let _ = EnableWindow(handles.primary, false);
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            if let Some(state) = state {
                let command_id = (wparam.0 & 0xffff) as u16;
                let notification = ((wparam.0 >> 16) & 0xffff) as u16;
                let source = HWND(lparam.0 as *mut _);
                if is_non_click_button_notification(source, notification) {
                    return LRESULT(0);
                }
                if state.handle_tool_content_action(command_id, source) {
                    return LRESULT(0);
                }
                if NativeHardwareInspectorDialog::owns_command(command_id) {
                    if notification == BN_CLICKED as u16 {
                        if let Some(dialog) = &mut state.hardware_inspector_dialog {
                            dialog.handle_command(command_id);
                        }
                    }
                    return LRESULT(0);
                }
                if NativeQuickPartitionDialog::owns_command(command_id) {
                    if let Some(dialog) = &mut state.quick_partition_dialog {
                        state.pending_quick_partition_command = dialog.handle_command(command_id);
                    }
                    return LRESULT(0);
                }
                if NativeBitLockerManageDialog::owns_command(command_id) {
                    if let Some(dialog) = &mut state.bitlocker_manage_dialog {
                        state.pending_bitlocker_manage_command = dialog.handle_command(command_id);
                    }
                    return LRESULT(0);
                }
                if NativeBatchFormatDialog::owns_command(command_id) {
                    if let Some(dialog) = &mut state.batch_format_dialog {
                        dialog.handle_command(command_id);
                    }
                    return LRESULT(0);
                }
                if NativeAppxDialog::accepts_command(command_id, notification) {
                    let outcome = state
                        .appx_dialog
                        .as_mut()
                        .map(|dialog| dialog.handle_command(command_id));
                    match outcome {
                        Some(Ok(Some(
                            crate::core::native_appx_selection::NativeAppxDialogIntent::LoadPackages {
                                inventory_target,
                            },
                        ))) => state.start_appx_packages(inventory_target),
                        Some(Err(error)) => {
                            if let Some(dialog) = &mut state.appx_dialog {
                                dialog.set_status(error.to_string());
                                dialog.show_modeless();
                            }
                        }
                        _ => {}
                    }
                    return LRESULT(0);
                }
                let driver_browse = state
                    .driver_transfer_dialog
                    .as_ref()
                    .and_then(|dialog| dialog.intent_for_command(command_id));
                if matches!(
                    driver_browse,
                    Some(
                        crate::core::native_driver_transfer::DriverTransferIntent::BrowseDirectory(
                            _
                        )
                    )
                ) {
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        if let Some(dialog) = &mut state.driver_transfer_dialog {
                            dialog.set_directory(&path.to_string_lossy());
                            dialog.show_modeless();
                        }
                    } else if let Some(dialog) = &mut state.driver_transfer_dialog {
                        dialog.show_modeless();
                    }
                    return LRESULT(0);
                }
                if let Some(dialog) = &mut state.driver_transfer_dialog {
                    if dialog.handle_command(command_id) {
                        return LRESULT(0);
                    }
                }
                let advanced_intent = state
                    .advanced_page
                    .as_ref()
                    .and_then(|page| page.intent_for_command(command_id));
                if let Some(AdvancedPageIntent::Browse(target)) = advanced_intent {
                    state.browse_advanced_path(target);
                }
                if notification == EN_CHANGE as u16 {
                    let control = HWND(lparam.0 as *mut _);
                    if let Some(dialog) = &mut state.bitlocker_manage_dialog {
                        if dialog.owns_credential(control) {
                            dialog.handle_credential_changed();
                            return LRESULT(0);
                        }
                    }
                    if let Some(dialog) = &mut state.expand_c_dialog {
                        if dialog.owns_target_edit(control) {
                            dialog.handle_target_edit_changed();
                        }
                    }
                }
                if notification == CBN_SELCHANGE as u16 {
                    let control = HWND(lparam.0 as *mut _);
                    if let Some(dialog) = &mut state.quick_partition_dialog {
                        if dialog.owns_choice(control) {
                            dialog.handle_choice_changed(control);
                            return LRESULT(0);
                        }
                    }
                    if let Some(dialog) = &mut state.bitlocker_manage_dialog {
                        if dialog.owns_choice(control) {
                            dialog.handle_choice_changed(control);
                            return LRESULT(0);
                        }
                    }
                    let copy_request = if let Some(dialog) = &mut state.partition_copy_dialog {
                        if dialog.owns_choice(control) {
                            dialog.handle_choice_changed(control)
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if let Some(request) = copy_request {
                        state.start_partition_copy_resume_check(
                            state.partition_copy_generation,
                            request,
                        );
                        return LRESULT(0);
                    }
                    if let Some(dialog) = &mut state.nvidia_dialog {
                        if dialog.owns_target_combo(control) {
                            dialog.handle_target_changed();
                            return LRESULT(0);
                        }
                    }
                    if let Some(dialog) = &mut state.boot_repair_dialog {
                        if dialog.owns_target_combo(control) {
                            dialog.handle_target_changed();
                            return LRESULT(0);
                        }
                    }
                    if let Some(dialog) = &mut state.storage_driver_dialog {
                        if dialog.owns_target(control) {
                            dialog.handle_target_changed();
                            return LRESULT(0);
                        }
                    }
                    let password_target = if let Some(dialog) = &mut state.password_reset_dialog {
                        if dialog.owns_target_combo(control) {
                            dialog.handle_target_changed()
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if let Some(PasswordResetDialogIntent::LoadAccounts(target)) = password_target {
                        state
                            .start_password_reset_accounts(state.password_reset_generation, target);
                        return LRESULT(0);
                    }
                    if let Some(dialog) = state
                        .mutating_tool_dialogs
                        .iter_mut()
                        .find(|dialog| dialog.owns_choice(control))
                    {
                        dialog.handle_choice_changed(control);
                    }
                    let dynamic = state
                        .mutating_tool_dialogs
                        .iter_mut()
                        .find(|dialog| dialog.owns_first_choice(control))
                        .and_then(|dialog| {
                            let kind = dialog.kind();
                            dialog
                                .begin_dynamic_inventory_load()
                                .map(|(target, generation)| (kind, target, generation))
                        });
                    if let Some((kind, target, generation)) = dynamic {
                        state.start_dynamic_tool_inventory(kind, target, generation);
                    }
                }
                if notification == LBN_SELCHANGE as u16 {
                    let control = HWND(lparam.0 as *mut _);
                    if let Some(dialog) = &mut state.password_reset_dialog {
                        if dialog.owns_account_list(control) {
                            dialog.handle_account_changed();
                            return LRESULT(0);
                        }
                    }
                }
                let wifi_toggled = notification == 0
                    && state.advanced_page.as_ref().is_some_and(|page| {
                        page.handles().system_checks[9] == HWND(lparam.0 as *mut _)
                    });
                if wifi_toggled {
                    state.handle_wifi_migration_toggle(hwnd);
                }
                if notification == BN_CLICKED as u16 {
                    let control = HWND(lparam.0 as *mut _);
                    if let Some(page) = &state.advanced_page {
                        if page.owns_dependency_toggle(control) {
                            page.update_dependencies();
                        }
                    }
                }
                match command_id {
                    ID_NAV_INSTALL => state.select_page(hwnd, Page::Install),
                    ID_NAV_BACKUP => state.select_page(hwnd, Page::Backup),
                    ID_NAV_DOWNLOAD => state.select_page(hwnd, Page::Download),
                    ID_NAV_TOOLS => state.select_page(hwnd, Page::Tools),
                    ID_NAV_HARDWARE => state.select_page(hwnd, Page::Hardware),
                    ID_NAV_ABOUT => state.select_page(hwnd, Page::About),
                    ID_ADVANCED if state.page == Page::Hardware => state.save_hardware_report(hwnd),
                    ID_ADVANCED => state.toggle_advanced_page(hwnd),
                    ID_FORMAT => {
                        state.persist_install_preferences();
                        state.update_unattend_conflict();
                        state.update_install_primary_state();
                    }
                    ID_BOOT => {
                        state.persist_install_preferences();
                        state.update_advanced_install_context();
                        state.request_pca_target_detection(hwnd);
                        state.update_pca_detection_status();
                        state.update_install_primary_state();
                    }
                    ID_REBOOT | ID_DRIVER_COMBO => state.persist_install_preferences(),
                    ID_RUN_DISKPART => {
                        state.persist_install_preferences();
                        state.update_pca_detection_status();
                        state.update_install_primary_state();
                    }
                    ID_OPEN_DISKPART_DIR => state.open_diskpart_script_directory(hwnd),
                    ID_EDIT_BOOT_COMMANDS => state.edit_repair_boot_script(hwnd),
                    ID_BOOT_COMBO if notification == CBN_SELCHANGE as u16 => {
                        state.persist_install_preferences();
                        state.update_advanced_install_context();
                        state.request_pca_target_detection(hwnd);
                        state.update_pca_detection_status();
                        state.update_install_primary_state();
                    }
                    ID_PCA_MODE if notification == CBN_SELCHANGE as u16 => {
                        state.persist_install_preferences();
                        if let (Some(handles), Some(error)) =
                            (state.handles, state.pca_selection_error())
                        {
                            set_text(handles.status, &error);
                        }
                        state.update_install_primary_state();
                    }
                    ID_BROWSE => state.browse_for_image(hwnd),
                    crate::native_ui::pages::backup::ID_BROWSE => {
                        state.browse_for_backup();
                        state.update_backup_primary_state();
                    }
                    crate::native_ui::pages::backup::ID_FORMAT => {
                        if let Some(page) = &state.backup_page {
                            page.update_format_controls();
                        }
                        state.update_backup_primary_state();
                    }
                    crate::native_ui::pages::backup::ID_SWM_SIZE
                    | crate::native_ui::pages::backup::ID_SAVE_PATH
                    | crate::native_ui::pages::backup::ID_NAME
                    | crate::native_ui::pages::backup::ID_DESCRIPTION
                    | crate::native_ui::pages::backup::ID_INCREMENTAL
                    | crate::native_ui::pages::backup::ID_PE => {
                        state.update_backup_primary_state();
                    }
                    ID_REFRESH => {
                        if state.refresh_partitions() {
                            state.request_pca_target_detection(hwnd);
                            state.update_pca_detection_status();
                        }
                        state.update_install_primary_state();
                    }
                    ID_IMAGE_EDIT if notification == EN_CHANGE as u16 => {
                        state.handle_image_edit_changed(hwnd)
                    }
                    ID_IMAGE_EDIT if notification == EN_KILLFOCUS as u16 => {
                        state.commit_image_edit(hwnd)
                    }
                    ID_IMAGE_EDIT => {}
                    ID_IMAGE_VOLUME if notification == CBN_SELCHANGE as u16 => {
                        state.refresh_source_unattend();
                        state.update_unattend_conflict();
                        state.update_storage_driver_default();
                        state.update_advanced_install_context();
                        state.request_pca_target_detection(hwnd);
                        state.update_pca_detection_status();
                        state.update_install_primary_state();
                    }
                    ID_INSTALL_PE => state.update_install_primary_state(),
                    ID_UNATTEND => {
                        state.persist_install_preferences();
                        state.update_advanced_install_context();
                        state.update_unattend_controls_visibility();
                        state.update_install_primary_state();
                        state.redraw_install_volume_layout_frame(hwnd, None);
                    }
                    ID_UNATTEND_BROWSE => state.browse_for_unattend(),
                    ID_UNATTEND_CLEAR => state.clear_custom_unattend(),
                    ID_PRIMARY => state.handle_primary_action(hwnd),
                    _ => {}
                }
                if matches!(
                    command_id,
                    ID_CANCEL_OPERATION | ID_PROGRESS_PRIMARY | ID_PROGRESS_SECONDARY
                ) {
                    if let Some(intent) = state
                        .progress_page
                        .as_ref()
                        .and_then(|page| page.command_intent(command_id))
                    {
                        state.handle_progress_command(hwnd, intent);
                    }
                }
                if state.page == Page::Install && state.easy_mode_enabled() {
                    if let Some(command) = EasyModePage::command(command_id) {
                        let is_combo = matches!(
                            command,
                            EasyModeCommand::SelectSystem | EasyModeCommand::SelectVolume
                        );
                        if !is_combo || notification == CBN_SELCHANGE as u16 {
                            state.handle_easy_mode_command(hwnd, command);
                        }
                    }
                } else if state.page == Page::Download {
                    if let Some(intent) = DownloadPage::command_intent(command_id) {
                        state.handle_download_intent(hwnd, intent);
                    }
                } else if state.page == Page::Tools {
                    if let Some(intent) = ToolsPage::command_intent(command_id) {
                        state.handle_tool_intent(hwnd, intent);
                    }
                } else if state.page == Page::Hardware {
                    if HardwareInfoPage::command_intent(command_id)
                        == Some(InfoIntent::SaveHardwareText)
                    {
                        state.save_hardware_report(hwnd);
                    }
                } else if state.page == Page::About {
                    match AboutPage::command_intent(command_id) {
                        Some(InfoIntent::OpenLink(link)) => state.open_about_link(hwnd, link),
                        Some(InfoIntent::ToggleEasyMode) => {
                            let is_pe = state
                                .config
                                .system_info
                                .as_ref()
                                .is_some_and(|info| info.is_pe_environment);
                            if !is_pe {
                                if let Some(page) = &state.about_page {
                                    let enabled = page.easy_mode_enabled();
                                    state.app_config.set_easy_mode(enabled);
                                    state
                                        .easy_controller
                                        .apply(EasyModeAction::SetEnabled(enabled));
                                    page.set_easy_mode_state(enabled, true);
                                    if let Some(easy) = &mut state.easy_page {
                                        easy.update(&state.easy_controller.view());
                                        // The About page remains active here. `update` may show
                                        // conditional easy-mode children, so immediately restore
                                        // the page-level visibility invariant.
                                        easy.show(false);
                                    }
                                }
                            }
                        }
                        Some(InfoIntent::SelectLanguage)
                            if notification == CBN_SELCHANGE as u16 =>
                        {
                            let language = state
                                .about_page
                                .as_ref()
                                .and_then(|page| page.selected_language_code());
                            if let Some(language) = language {
                                if language != state.app_config.language {
                                    state.app_config.set_language(&language);
                                    state.relocalize_after_language_change(hwnd);
                                }
                            }
                        }
                        Some(InfoIntent::RefreshLanguages) => {
                            if let Some(page) = &state.about_page {
                                page.refresh_language_choices();
                            }
                        }
                        Some(InfoIntent::ToggleLogging) => {
                            if let Some(page) = &state.about_page {
                                let enabled = page.logging_enabled();
                                state.app_config.set_log_enabled(enabled);
                                page.set_logging_enabled(enabled);
                            }
                        }
                        Some(InfoIntent::SelectWimEngine) => {
                            if let Some(page) = &state.about_page {
                                state.app_config.set_wim_engine(page.selected_wim_engine());
                            }
                        }
                        Some(InfoIntent::SelectDownloadThreads)
                            if notification == CBN_SELCHANGE as u16 =>
                        {
                            if let Some(page) = &state.about_page {
                                state
                                    .app_config
                                    .set_download_threads(page.selected_download_threads());
                            }
                        }
                        Some(InfoIntent::ToggleAdvancedOptions) => {
                            if let Some(page) = &state.about_page {
                                state
                                    .app_config
                                    .set_advanced_options(page.advanced_options_enabled());
                            }
                        }
                        Some(InfoIntent::OpenLogDirectory) => state.open_log_directory(hwnd),
                        _ => {}
                    }
                }
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            if let Some(state) = state {
                if state.advanced_visible {
                    if let Some(page) = &state.advanced_page {
                        let delta = ((wparam.0 >> 16) as u16) as i16;
                        if page.scroll_wheel(delta) {
                            return LRESULT(0);
                        }
                    }
                }
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_HSCROLL => {
            if let Some(state) = state {
                let control = HWND(lparam.0 as *mut _);
                if let Some(dialog) = &mut state.expand_c_dialog {
                    if dialog.owns_slider(control) {
                        dialog.handle_slider_changed();
                        return LRESULT(0);
                    }
                }
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_VSCROLL => {
            if let Some(state) = state {
                if state.advanced_visible {
                    if let Some(page) = &state.advanced_page {
                        if lparam.0 == page.viewport().0 as isize && page.handle_vscroll(wparam.0) {
                            return LRESULT(0);
                        }
                    }
                }
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_TIMER => {
            if let Some(state) = state {
                if wparam.0 == BACKUP_TIMER_ID {
                    state.poll_backup_messages(hwnd);
                } else if wparam.0 == DOWNLOAD_TIMER_ID {
                    state.poll_download_messages(hwnd);
                } else if wparam.0 == INSTALL_TIMER_ID {
                    state.poll_install_messages(hwnd);
                } else if wparam.0 == TOOL_DIALOG_TIMER_ID {
                    state.poll_tool_dialogs(hwnd);
                } else if wparam.0 == CATALOGUE_TIMER_ID {
                    state.poll_catalogue_messages(hwnd);
                } else if wparam.0 == HARDWARE_COPY_TIMER_ID {
                    let _ = KillTimer(hwnd, HARDWARE_COPY_TIMER_ID);
                    state.hardware_copy_feedback.expire();
                    if state.page == Page::Hardware {
                        if let Some(handles) = state.handles {
                            set_text(handles.primary, &crate::tr!("复制信息"));
                            let _ = InvalidateRect(handles.primary, None, false);
                        }
                    }
                } else if wparam.0 == INSTALL_VOLUME_LAYOUT_TIMER_ID {
                    state.advance_install_volume_layout(hwnd);
                } else if wparam.0 == PARTITION_REFRESH_TIMER_ID {
                    state.start_scheduled_partition_refresh(hwnd);
                }
                if state.close_after_task && !state.has_active_long_task() {
                    let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            if let Some(state) = state {
                let header = &*(lparam.0 as *const NMHDR);
                if header.code == LVN_ITEMCHANGED
                    && state
                        .quick_partition_dialog
                        .as_ref()
                        .is_some_and(|dialog| dialog.owns_list(header.hwndFrom))
                {
                    if let Some(dialog) = &mut state.quick_partition_dialog {
                        dialog.handle_list_changed();
                    }
                } else if header.code == LVN_ITEMCHANGED
                    && state
                        .bitlocker_manage_dialog
                        .as_ref()
                        .is_some_and(|dialog| dialog.owns_list(header.hwndFrom))
                {
                    if let Some(dialog) = &mut state.bitlocker_manage_dialog {
                        dialog.handle_list_changed();
                    }
                } else if header.code == LVN_ITEMCHANGED
                    && state
                        .partition_copy_dialog
                        .as_ref()
                        .is_some_and(|dialog| dialog.owns_list(header.hwndFrom))
                {
                    let request = state
                        .partition_copy_dialog
                        .as_mut()
                        .and_then(|dialog| dialog.handle_list_changed(header.hwndFrom));
                    if let Some(request) = request {
                        state.start_partition_copy_resume_check(
                            state.partition_copy_generation,
                            request,
                        );
                    }
                } else if header.code == LVN_ITEMCHANGED
                    && state
                        .appx_dialog
                        .as_ref()
                        .is_some_and(|dialog| dialog.accepts_list_change(header.hwndFrom))
                {
                    if let Some(dialog) = &mut state.appx_dialog {
                        dialog.handle_list_changed();
                    }
                } else if header.code == LVN_ITEMCHANGED
                    && state
                        .batch_format_dialog
                        .as_ref()
                        .is_some_and(|dialog| dialog.owns_list(header.hwndFrom))
                {
                    if let Some(dialog) = &mut state.batch_format_dialog {
                        dialog.handle_list_changed();
                    }
                } else if header.idFrom == ID_PARTITIONS as usize && header.code == LVN_ITEMCHANGED
                {
                    let change = &*(lparam.0 as *const NMLISTVIEW);
                    let selection_state_changed = list_view_selection_state_changed(
                        change.uChanged.0,
                        change.uOldState,
                        change.uNewState,
                    );
                    if !state.partition_list_replacing
                        && selection_state_changed
                        && !state.install_selection_update_pending
                    {
                        // A single selection move normally sends one notification for the old row
                        // and another for the new row. Defer and coalesce both so expensive target
                        // checks and layout run once against the final ListView state, outside the
                        // control's synchronous notification/paint transaction.
                        state.install_selection_update_pending = true;
                        if PostMessageW(
                            hwnd,
                            WM_INSTALL_PARTITION_SELECTION_CHANGED,
                            WPARAM(0),
                            LPARAM(0),
                        )
                        .is_err()
                        {
                            state.install_selection_update_pending = false;
                            let redraw = redraw::suspend(hwnd);
                            state.handle_install_partition_changed(hwnd);
                            redraw::resume_client(hwnd, redraw);
                        }
                    }
                } else if header.idFrom == crate::native_ui::pages::backup::ID_SOURCE_LIST as usize
                    && header.code == LVN_ITEMCHANGED
                {
                    state.update_backup_primary_state();
                } else if header.idFrom == ID_RESOURCE_LIST as usize
                    && header.code == LVN_ITEMCHANGED
                {
                    if let Some(index) = state
                        .download_page
                        .as_ref()
                        .and_then(|page| page.selected_resource())
                    {
                        let _ = state
                            .download_controller
                            .apply_intent(ControllerIntent::SelectResource(index));
                    }
                }
            }
            LRESULT(0)
        }
        WM_DRAWITEM => {
            if let Some(state) = state {
                let item = &*(lparam.0 as *const DRAWITEMSTRUCT);
                if state
                    .progress_page
                    .as_ref()
                    .is_some_and(|page| page.draw_item(item, state.control_palette()))
                {
                    return LRESULT(1);
                } else if item.CtlType.0 == ODT_HEADER {
                    state.draw_list_header(item);
                } else {
                    state.draw_button(item);
                }
                return LRESULT(1);
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_CTLCOLOREDIT => {
            if let Some(state) = state {
                let dc = HDC(wparam.0 as *mut _);
                let control = HWND(lparam.0 as *mut _);
                let palette = state.control_palette();
                let background = palette.edit_brush_color_for(control);
                let _ = SetTextColor(dc, palette.edit_text_color_for(control));
                let _ = SetBkColor(dc, background);
                let brush = if background == palette.edit {
                    state.brushes.edit_opaque
                } else {
                    state.brushes.edit
                };
                return LRESULT(brush.0 as isize);
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_CTLCOLORLISTBOX => {
            if let Some(state) = state {
                let dc = HDC(wparam.0 as *mut _);
                let palette = state.control_palette();
                let _ = SetTextColor(dc, palette.text);
                let _ = SetBkColor(dc, palette.edit);
                return LRESULT(state.brushes.list.0 as isize);
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_CTLCOLORSTATIC => {
            if let Some(state) = state {
                let dc = HDC(wparam.0 as *mut _);
                let control = HWND(lparam.0 as *mut _);
                // Disabled and read-only Edit controls send WM_CTLCOLORSTATIC instead of
                // WM_CTLCOLOREDIT. Keep their field surface identical to enabled edits while
                // using the disabled caption colour, rather than painting the window background
                // through the Edit client area as a mismatched grey block.
                if control_has_class(control, "Edit") {
                    let palette = state.control_palette();
                    let enabled = IsWindowEnabled(control).as_bool();
                    let background = palette.edit_brush_color_for(control);
                    let _ = SetTextColor(
                        dc,
                        if enabled {
                            palette.edit_text_color_for(control)
                        } else {
                            palette.text_disabled
                        },
                    );
                    let _ = SetBkColor(dc, background);
                    let brush = if background == palette.edit {
                        state.brushes.edit_opaque
                    } else {
                        state.brushes.edit
                    };
                    return LRESULT(brush.0 as isize);
                }
                let _ = SetTextColor(dc, state.control_palette().text);
                let _ = SetBkColor(dc, state.palette.window);
                let _ = SetBkMode(dc, TRANSPARENT);
                return LRESULT(state.brushes.window.0 as isize);
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_CTLCOLORBTN => {
            if let Some(state) = state {
                let dc = HDC(wparam.0 as *mut _);
                let _ = SetTextColor(dc, state.control_palette().text);
                let _ = SetBkColor(dc, state.palette.window);
                let _ = SetBkMode(dc, TRANSPARENT);
                return LRESULT(state.brushes.window.0 as isize);
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            if let Some(state) = state {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut paint);
                let mut rect = RECT::default();
                let _ = GetClientRect(hwnd, &mut rect);
                let _ = FillRect(dc, &rect, state.brushes.window);
                // Long tasks intentionally occupy the complete client area.  Painting the normal
                // navigation rail underneath their transparent STATIC controls leaked the old
                // navigation separator through as several disconnected vertical strokes.
                if !state.progress_visible {
                    let nav_rect = RECT {
                        left: 0,
                        top: 0,
                        right: state.scale(NAV_WIDTH),
                        bottom: rect.bottom - state.scale(COMMAND_HEIGHT),
                    };
                    let _ = FillRect(dc, &nav_rect, state.brushes.nav);
                    let footer_rect = RECT {
                        left: 0,
                        top: rect.bottom - state.scale(COMMAND_HEIGHT),
                        right: rect.right,
                        bottom: rect.bottom,
                    };
                    let _ = FillRect(dc, &footer_rect, state.brushes.window);
                    draw_line(
                        dc,
                        state.scale(NAV_WIDTH),
                        0,
                        state.scale(NAV_WIDTH),
                        rect.bottom - state.scale(COMMAND_HEIGHT),
                        state.palette.separator,
                    );
                }
                let _ = EndPaint(hwnd, &paint);
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_CLOSE => {
            if let Some(state) = state {
                if state.has_active_long_task() {
                    state.request_safe_close(hwnd);
                    return LRESULT(0);
                }
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_DESTROY => {
            if let Some(state) = state {
                if let Some(execution) = &state.backup_execution {
                    execution.request_cancel();
                }
                if let Some(worker) = &state.download_worker {
                    let _ = worker.send(DownloadWorkerCommand::Cancel);
                }
                if let Some(cancel) = &state.install_cancel {
                    cancel.store(true, Ordering::SeqCst);
                }
                if let Some(cancel) = &state.image_verify_cancel {
                    cancel.store(true, Ordering::SeqCst);
                }
            }
            let _ = KillTimer(hwnd, BACKUP_TIMER_ID);
            let _ = KillTimer(hwnd, DOWNLOAD_TIMER_ID);
            let _ = KillTimer(hwnd, INSTALL_TIMER_ID);
            let _ = KillTimer(hwnd, TOOL_DIALOG_TIMER_ID);
            let _ = KillTimer(hwnd, CATALOGUE_TIMER_ID);
            let _ = KillTimer(hwnd, HARDWARE_COPY_TIMER_ID);
            let _ = KillTimer(hwnd, INSTALL_VOLUME_LAYOUT_TIMER_ID);
            let _ = KillTimer(hwnd, PARTITION_REFRESH_TIMER_ID);
            crate::utils::dprk_easter_egg::shutdown();
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

impl NativeWindow {
    unsafe fn draw_list_header(&self, item: &DRAWITEMSTRUCT) {
        self.draw_header_cell(item.hDC, item.hwndItem, item.itemID as usize, item.rcItem);
    }

    unsafe fn draw_header_cell(&self, dc: HDC, header: HWND, index: usize, rect: RECT) {
        let palette = self.control_palette();
        let brush = CreateSolidBrush(palette.button);
        let _ = FillRect(dc, &rect, brush);
        let _ = DeleteObject(brush);

        let mut text = vec![0u16; 128];
        let mut header_item = HDITEMW {
            mask: HDI_TEXT,
            pszText: windows::core::PWSTR(text.as_mut_ptr()),
            cchTextMax: text.len() as i32,
            ..Default::default()
        };
        let _ = SendMessageW(
            header,
            0x120B,
            WPARAM(index),
            LPARAM((&mut header_item as *mut HDITEMW) as isize),
        );
        let length = text.iter().position(|ch| *ch == 0).unwrap_or(text.len());
        text.truncate(length);

        let _ = SetBkMode(dc, TRANSPARENT);
        let _ = SetTextColor(dc, palette.text);
        let old_font = SelectObject(dc, self.font);
        let mut text_rect = rect;
        text_rect.left += self.scale(8);
        text_rect.right -= self.scale(6);
        let _ = DrawTextW(
            dc,
            &mut text,
            &mut text_rect,
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        let _ = SelectObject(dc, old_font);
        draw_line(
            dc,
            rect.right - 1,
            rect.top + self.scale(4),
            rect.right - 1,
            rect.bottom - self.scale(4),
            palette.separator,
        );
    }

    unsafe fn draw_button(&self, item: &DRAWITEMSTRUCT) {
        let id = item.CtlID as u16;
        let is_nav = (ID_NAV_INSTALL..=ID_NAV_ABOUT).contains(&id);
        let is_current_nav = match self.page {
            Page::Install => id == ID_NAV_INSTALL,
            Page::Backup => id == ID_NAV_BACKUP,
            Page::Download => id == ID_NAV_DOWNLOAD,
            Page::Tools => id == ID_NAV_TOOLS,
            Page::Hardware => id == ID_NAV_HARDWARE,
            Page::About => id == ID_NAV_ABOUT,
        };
        let download_tab = match DownloadPage::command_intent(id) {
            Some(DownloadIntent::SelectTab(tab)) => Some(tab),
            _ => None,
        };
        let role = if let Some(tab) = download_tab {
            ButtonRole::Navigation {
                selected: self
                    .download_page
                    .as_ref()
                    .is_some_and(|page| page.selected_tab() == tab),
            }
        } else if is_nav {
            ButtonRole::Navigation {
                selected: is_current_nav,
            }
        } else {
            command_button_role(id)
        };
        draw_inno_button(item, self.control_palette(), role, self.font, self.dpi);
    }
}

fn read_only_request_path(request: &ReadOnlyToolRequest) -> &str {
    match request {
        ReadOnlyToolRequest::Sha256 { path, .. }
        | ReadOnlyToolRequest::GhoPassword { path }
        | ReadOnlyToolRequest::VerifyImage { path } => path,
        ReadOnlyToolRequest::InstalledSoftware | ReadOnlyToolRequest::NetworkInformation => "",
    }
}

fn read_only_expected_hash(request: &ReadOnlyToolRequest) -> &str {
    match request {
        ReadOnlyToolRequest::Sha256 { expected, .. } => expected,
        _ => "",
    }
}

fn initial_mutating_tool_state(
    kind: MutatingToolKind,
    partitions: &[crate::core::disk::Partition],
    is_pe: bool,
) -> MutatingToolState {
    let volumes: Vec<String> = partitions
        .iter()
        .map(|partition| partition.letter.clone())
        .collect();
    let data_volumes: Vec<String> = partitions
        .iter()
        .filter(|partition| !partition.is_system_partition)
        .map(|partition| partition.letter.clone())
        .collect();
    let windows_volumes: Vec<String> = partitions
        .iter()
        .filter(|partition| partition.has_windows)
        .map(|partition| partition.letter.clone())
        .collect();
    let mut systems = windows_volumes.clone();
    if !is_pe {
        systems.insert(0, "当前系统".to_string());
    }
    systems.dedup();
    let mut disks = Vec::new();
    for disk in partitions
        .iter()
        .filter_map(|partition| partition.disk_number)
    {
        let disk = disk.to_string();
        if !disks.contains(&disk) {
            disks.push(disk);
        }
    }

    let mut state = MutatingToolState {
        status: crate::tr!("请从列表选择目标和选项后继续。"),
        ..Default::default()
    };
    match kind {
        MutatingToolKind::PartitionCopy => {
            state.first_choices = data_volumes.clone();
            state.second_choices = data_volumes;
        }
        MutatingToolKind::BatchFormat => {
            state.first_choices = vec!["NTFS".into(), "FAT32".into(), "exFAT".into()];
            state.value = "NTFS".into();
            state.available_items = data_volumes;
        }
        MutatingToolKind::ImportStorageDriver => {
            state.first_choices = windows_volumes;
        }
        MutatingToolKind::DriverBackupRestore => {
            state.first_choices = systems;
        }
        MutatingToolKind::RepairBoot => {
            state.first_choices = windows_volumes;
            state.second_choices = vec!["Auto".into(), "UEFI".into(), "Legacy".into()];
            state.value = "Auto".into();
        }
        MutatingToolKind::ManageBitLocker => {
            state.first_choices = volumes;
            state.second_choices = vec![
                crate::tr!("解锁"),
                crate::tr!("暂停保护"),
                crate::tr!("恢复保护"),
                crate::tr!("解密"),
            ];
            state.value = state.second_choices.first().cloned().unwrap_or_default();
        }
        MutatingToolKind::ResetPassword
        | MutatingToolKind::RemoveAppx
        | MutatingToolKind::NvidiaDriverRemoval => {
            state.first_choices = systems;
            state.status = crate::tr!("选择系统后加载可选项目。");
        }
        MutatingToolKind::QuickPartition => {
            state.first_choices = disks;
            state.second_choices = vec!["GPT".into(), "MBR".into()];
            state.quick_partition_style = crate::core::disk::PartitionStyle::GPT;
            state.value = "GPT".into();
        }
        MutatingToolKind::TimeSynchronization => {
            state.value = "ntp.aliyun.com".into();
        }
        MutatingToolKind::RunGhost
        | MutatingToolKind::ResetNetwork
        | MutatingToolKind::RunSpaceSniffer => {}
    }
    state
}

fn confirmed_tool_backend_request(
    kind: MutatingToolKind,
    execution: &super::tool_dialogs_mutating::MutatingToolIntent,
) -> Result<NativeToolBackendRequest, String> {
    use crate::core::native_tools_controller::NativeToolAction as Action;
    let action = match kind {
        MutatingToolKind::NvidiaDriverRemoval => Action::NvidiaDriverRemoval,
        MutatingToolKind::PartitionCopy => Action::PartitionCopy,
        MutatingToolKind::BatchFormat => Action::BatchFormat,
        MutatingToolKind::ImportStorageDriver => Action::ImportStorageDriver,
        MutatingToolKind::QuickPartition => Action::QuickPartition,
        MutatingToolKind::RemoveAppx => Action::RemoveAppx,
        MutatingToolKind::DriverBackupRestore => Action::DriverBackupRestore,
        MutatingToolKind::RepairBoot => Action::RepairBoot,
        MutatingToolKind::TimeSynchronization => Action::TimeSynchronization,
        MutatingToolKind::RunGhost => Action::RunGhost,
        MutatingToolKind::ResetNetwork => Action::ResetNetwork,
        MutatingToolKind::RunSpaceSniffer => Action::RunSpaceSniffer,
        MutatingToolKind::ManageBitLocker => Action::ManageBitLocker,
        MutatingToolKind::ResetPassword => Action::ResetPassword,
    };
    match crate::core::native_tool_executor::plan_execution(ToolExecutionRequest::NativeAction {
        action,
        confirmed: true,
    }) {
        ToolExecutionPlan::External(plan) => match (kind, execution) {
            (
                MutatingToolKind::RunGhost,
                super::tool_dialogs_mutating::MutatingToolIntent::LaunchGhost,
            )
            | (
                MutatingToolKind::RunSpaceSniffer,
                super::tool_dialogs_mutating::MutatingToolIntent::LaunchSpaceSniffer,
            ) => Ok(NativeToolBackendRequest::External(plan)),
            _ => Err(crate::tr!("工具执行计划与对话框不匹配。")),
        },
        ToolExecutionPlan::Mutating(plan) => match (kind, execution) {
            (
                MutatingToolKind::QuickPartition,
                super::tool_dialogs_mutating::MutatingToolIntent::QuickPartition { request },
            ) => Ok(NativeToolBackendRequest::QuickPartition {
                plan,
                request: request.clone(),
            }),
            (
                MutatingToolKind::NvidiaDriverRemoval,
                super::tool_dialogs_mutating::MutatingToolIntent::RemoveNvidiaDrivers {
                    offline_root,
                    ..
                },
            ) => Ok(NativeToolBackendRequest::RemoveNvidiaDrivers {
                plan,
                offline_target: offline_root.clone(),
            }),
            (
                MutatingToolKind::PartitionCopy,
                super::tool_dialogs_mutating::MutatingToolIntent::CopyPartition { source, target },
            ) => Ok(NativeToolBackendRequest::PartitionCopy {
                plan,
                request: crate::core::native_partition_copy::PartitionCopyRequest {
                    source: source.clone(),
                    target: target.clone(),
                },
            }),
            (
                MutatingToolKind::BatchFormat,
                super::tool_dialogs_mutating::MutatingToolIntent::BatchFormat {
                    partitions,
                    file_system,
                    volume_label,
                },
            ) => Ok(NativeToolBackendRequest::BatchFormat {
                plan,
                request: crate::core::native_batch_format::BatchFormatRequest {
                    drives: partitions.clone(),
                    file_system: file_system.clone(),
                    volume_label: volume_label.clone(),
                },
            }),
            (
                MutatingToolKind::RemoveAppx,
                super::tool_dialogs_mutating::MutatingToolIntent::RemoveAppx {
                    packages,
                    offline_root,
                },
            ) => Ok(NativeToolBackendRequest::RemoveAppx {
                plan,
                request: crate::core::native_appx::RemoveAppxRequest {
                    target: if offline_root == "__CURRENT__" {
                        crate::core::native_appx::AppxTarget::CurrentSystem
                    } else {
                        crate::core::native_appx::AppxTarget::OfflineWindows(offline_root.clone())
                    },
                    packages: packages.clone(),
                },
            }),
            (
                MutatingToolKind::ImportStorageDriver,
                super::tool_dialogs_mutating::MutatingToolIntent::ImportStorageDriver {
                    directory,
                    offline_root,
                    ..
                },
            ) => Ok(NativeToolBackendRequest::ImportStorageDriver {
                plan,
                target: offline_root.clone(),
                driver_directory: directory.clone(),
            }),
            (
                MutatingToolKind::DriverBackupRestore,
                super::tool_dialogs_mutating::MutatingToolIntent::TransferDrivers {
                    mode,
                    directory,
                    system_root,
                },
            ) => Ok(NativeToolBackendRequest::TransferDrivers {
                plan,
                mode: match mode {
                    super::tool_dialogs_mutating::DriverTransferMode::Backup => {
                        crate::core::native_tool_backend::DriverTransferMode::Backup
                    }
                    super::tool_dialogs_mutating::DriverTransferMode::Restore => {
                        crate::core::native_tool_backend::DriverTransferMode::Restore
                    }
                },
                system_partition: (!system_root.trim().is_empty()).then(|| system_root.clone()),
                directory: directory.clone(),
            }),
            (
                MutatingToolKind::RepairBoot,
                super::tool_dialogs_mutating::MutatingToolIntent::RepairBoot {
                    windows_partition,
                    boot_mode,
                },
            ) => Ok(NativeToolBackendRequest::RepairBoot {
                plan,
                target: windows_partition.clone(),
                boot_mode: match boot_mode {
                    super::tool_dialogs_mutating::BootRepairMode::Auto => {
                        crate::core::native_tool_backend::BootRepairMode::Auto
                    }
                    super::tool_dialogs_mutating::BootRepairMode::Uefi => {
                        crate::core::native_tool_backend::BootRepairMode::Uefi
                    }
                    super::tool_dialogs_mutating::BootRepairMode::Legacy => {
                        crate::core::native_tool_backend::BootRepairMode::Legacy
                    }
                },
            }),
            (
                MutatingToolKind::TimeSynchronization,
                super::tool_dialogs_mutating::MutatingToolIntent::SynchronizeTime { .. },
            ) => Ok(NativeToolBackendRequest::SynchronizeTime(plan)),
            (
                MutatingToolKind::ResetNetwork,
                super::tool_dialogs_mutating::MutatingToolIntent::ResetNetwork,
            ) => Ok(NativeToolBackendRequest::ResetNetwork(plan)),
            (
                MutatingToolKind::ManageBitLocker,
                super::tool_dialogs_mutating::MutatingToolIntent::ManageBitLocker {
                    volume,
                    action,
                    credential,
                },
            ) => {
                let operation = match (action, credential) {
                    (
                        super::tool_dialogs_mutating::BitLockerAction::Unlock,
                        Some(super::tool_dialogs_mutating::BitLockerCredential::Password(value)),
                    ) => crate::core::native_tool_backend::BitLockerOperation::UnlockWithPassword(
                        value.clone(),
                    ),
                    (
                        super::tool_dialogs_mutating::BitLockerAction::Unlock,
                        Some(super::tool_dialogs_mutating::BitLockerCredential::RecoveryKey(value)),
                    ) => {
                        crate::core::native_tool_backend::BitLockerOperation::UnlockWithRecoveryKey(
                            value.clone(),
                        )
                    }
                    (super::tool_dialogs_mutating::BitLockerAction::SuspendProtection, None) => {
                        crate::core::native_tool_backend::BitLockerOperation::SuspendProtection
                    }
                    (super::tool_dialogs_mutating::BitLockerAction::ResumeProtection, None) => {
                        crate::core::native_tool_backend::BitLockerOperation::ResumeProtection
                    }
                    (super::tool_dialogs_mutating::BitLockerAction::Decrypt, None) => {
                        crate::core::native_tool_backend::BitLockerOperation::Decrypt
                    }
                    _ => return Err(crate::tr!("BitLocker 操作缺少有效凭据。")),
                };
                Ok(NativeToolBackendRequest::ManageBitLocker {
                    plan,
                    volume: volume.clone(),
                    operation,
                })
            }
            (
                MutatingToolKind::ResetPassword,
                super::tool_dialogs_mutating::MutatingToolIntent::ResetPasswords {
                    target:
                        super::tool_dialogs_mutating::PasswordResetTarget::OfflineWindows(target),
                    accounts,
                    enable_accounts,
                },
            ) => Ok(NativeToolBackendRequest::ResetOfflinePassword {
                plan,
                target: target.clone(),
                accounts: accounts.clone(),
                enable_accounts: *enable_accounts,
            }),
            (
                MutatingToolKind::ResetPassword,
                super::tool_dialogs_mutating::MutatingToolIntent::ResetPasswords {
                    target: super::tool_dialogs_mutating::PasswordResetTarget::CurrentSystem,
                    ..
                },
            ) => Err(crate::tr!(
                "当前系统密码重置后端尚未迁移，请选择离线 Windows。"
            )),
            _ => Err(crate::tr!("工具执行计划与对话框不匹配。")),
        },
        _ => Err(crate::tr!("工具执行计划未通过确认校验。")),
    }
}

fn format_tool_backend_result(result: NativeToolBackendResult) -> Result<String, String> {
    let succeeded = tool_backend_result_succeeded(&result);
    let message = match result {
        NativeToolBackendResult::ExternalStarted => crate::tr!("外部工具已启动。"),
        NativeToolBackendResult::TimeSynchronization {
            success,
            message,
            old_time,
            new_time,
        } => crate::tr!(
            "{}\r\n同步前：{}\r\n同步后：{}",
            if success {
                crate::tr!("时间同步成功")
            } else {
                message
            },
            old_time.unwrap_or_default(),
            new_time.unwrap_or_default()
        ),
        NativeToolBackendResult::NetworkReset { succeeded, failed } => {
            crate::tr!("网络重置完成：成功 {} 项，失败 {} 项。", succeeded, failed)
        }
        NativeToolBackendResult::NvidiaRemoval {
            success,
            message,
            needs_reboot,
            uninstalled_count,
            failed_count,
        } => crate::tr!(
            "{}\r\n已卸载：{}，失败：{}，需要重启：{}",
            if success {
                crate::tr!("NVIDIA 驱动清理完成")
            } else {
                message
            },
            uninstalled_count,
            failed_count,
            if needs_reboot {
                crate::tr!("是")
            } else {
                crate::tr!("否")
            }
        ),
        NativeToolBackendResult::BatchFormat(result) => {
            let mut summary = crate::tr!(
                "批量格式化完成：成功 {} 个卷，失败 {} 个卷。",
                result.success_count,
                result.fail_count
            );
            for volume in result.volumes {
                summary.push_str("\r\n");
                summary.push_str(&volume.drive);
                summary.push_str("  ");
                if volume.success {
                    summary.push_str(&crate::tr!("操作成功"));
                } else {
                    summary.push_str(&crate::tr!("操作失败：{}", volume.message));
                }
            }
            summary
        }
        NativeToolBackendResult::AppxRemoval(result) => crate::tr!(
            "APPX 移除完成：成功 {} 个，失败 {} 个。",
            result.removed,
            result.failed
        ),
        NativeToolBackendResult::PartitionCopy(result) => {
            let mut summary = crate::tr!(
                "分区对拷完成：复制 {}，跳过 {}，失败 {}，总计 {}。",
                result.copied_count,
                result.skipped_count,
                result.failed_count,
                result.total_count
            );
            if result.resumed {
                summary.push_str(&crate::tr!("\r\n本次操作从有效断点继续。"));
            }
            if result.partial_success {
                summary.push_str(&crate::tr!("\r\n部分文件复制失败，断点已保留。"));
                for failed in result.failed_files.iter().take(8) {
                    summary.push_str("\r\n");
                    summary.push_str(failed);
                }
            }
            summary
        }
        NativeToolBackendResult::Completed { message } => message,
        NativeToolBackendResult::BitLocker {
            success,
            message,
            error_code,
        } => match error_code {
            Some(code) => crate::tr!(
                "{}\r\n错误代码：{}",
                if success {
                    crate::tr!("操作成功")
                } else {
                    message
                },
                code
            ),
            None => message,
        },
    };
    if succeeded {
        Ok(message)
    } else {
        Err(message)
    }
}

fn tool_backend_result_succeeded(result: &NativeToolBackendResult) -> bool {
    match result {
        NativeToolBackendResult::ExternalStarted | NativeToolBackendResult::Completed { .. } => {
            true
        }
        NativeToolBackendResult::TimeSynchronization { success, .. }
        | NativeToolBackendResult::NvidiaRemoval { success, .. }
        | NativeToolBackendResult::BitLocker { success, .. } => *success,
        NativeToolBackendResult::NetworkReset { failed, .. } => *failed == 0,
        NativeToolBackendResult::AppxRemoval(result) => result.failed == 0,
        NativeToolBackendResult::BatchFormat(result) => result.fail_count == 0,
        NativeToolBackendResult::PartitionCopy(result) => result.success,
    }
}

unsafe fn apply_tool_result(
    dialog: &mut NativeToolDialog,
    request: &ReadOnlyToolRequest,
    result: Result<ReadOnlyToolResult, String>,
) {
    let error_text = |error: String| crate::tr!("操作失败：{}", error);
    match result {
        Ok(ReadOnlyToolResult::Sha256(result)) => {
            dialog.set_file_hash_state(&super::tool_dialogs::FileHashState {
                path: result.path.clone(),
                expected: result.expected.clone(),
                outcome: Some(result),
                percentage: 100,
                ..Default::default()
            });
        }
        Ok(ReadOnlyToolResult::GhoPassword(result)) => {
            dialog.set_gho_password_state(&super::tool_dialogs::GhoPasswordState {
                path: result.path.clone(),
                outcome: Some(result),
                ..Default::default()
            });
        }
        Ok(ReadOnlyToolResult::ImageVerification(result)) => {
            dialog.set_image_verification_state(&super::tool_dialogs::ImageVerificationState {
                path: result.path.clone(),
                percentage: 100,
                outcome: Some(result),
                ..Default::default()
            });
        }
        Ok(ReadOnlyToolResult::InstalledSoftware(records)) => {
            dialog.set_software_state(&super::tool_dialogs::SoftwareListState {
                records,
                ..Default::default()
            });
        }
        Ok(ReadOnlyToolResult::NetworkInformation(records)) => {
            let report = records
                .into_iter()
                .map(|record| {
                    crate::tr!(
                        "名称：{}\r\n描述：{}\r\n类型：{}\r\n状态：{}\r\n速度：{} Mbps\r\nMAC：{}\r\nIP：{}",
                        record.name,
                        record.description,
                        crate::tr!(record.adapter_type.as_str()),
                        crate::tr!(record.status.as_str()),
                        record.speed / 1_000_000,
                        record.mac_address,
                        record.ip_addresses.join(", ")
                    )
                })
                .collect::<Vec<_>>()
                .join("\r\n\r\n");
            dialog.set_network_state(&super::tool_dialogs::NetworkInformationState {
                report,
                ..Default::default()
            });
        }
        Err(error) => match request {
            ReadOnlyToolRequest::Sha256 { path, expected } => {
                dialog.set_file_hash_state(&super::tool_dialogs::FileHashState {
                    path: path.clone(),
                    expected: expected.clone(),
                    result: error_text(error),
                    ..Default::default()
                })
            }
            ReadOnlyToolRequest::GhoPassword { path } => {
                dialog.set_gho_password_state(&super::tool_dialogs::GhoPasswordState {
                    path: path.clone(),
                    result: error_text(error),
                    ..Default::default()
                })
            }
            ReadOnlyToolRequest::VerifyImage { path } => {
                dialog.set_image_verification_state(&super::tool_dialogs::ImageVerificationState {
                    path: path.clone(),
                    result: error_text(error),
                    ..Default::default()
                })
            }
            ReadOnlyToolRequest::InstalledSoftware => {
                dialog.set_software_state(&super::tool_dialogs::SoftwareListState {
                    rows: vec![error_text(error)],
                    ..Default::default()
                })
            }
            ReadOnlyToolRequest::NetworkInformation => {
                dialog.set_network_state(&super::tool_dialogs::NetworkInformationState {
                    report: error_text(error),
                    ..Default::default()
                })
            }
        },
    }
}

unsafe fn draw_line(dc: HDC, x1: i32, y1: i32, x2: i32, y2: i32, color: COLORREF) {
    let pen = windows::Win32::Graphics::Gdi::CreatePen(PEN_STYLE(0), 1, color);
    let old = SelectObject(dc, pen);
    let _ = MoveToEx(dc, x1, y1, None);
    let _ = LineTo(dc, x2, y2);
    let _ = SelectObject(dc, old);
    let _ = DeleteObject(pen);
}

#[cfg(test)]
mod tests {
    use super::HardwareCopyFeedback;

    #[test]
    fn hardware_copy_feedback_expires_back_to_the_normal_caption() {
        let mut feedback = HardwareCopyFeedback::default();
        assert_eq!(feedback.caption_key(), "复制信息");
        feedback.start();
        assert_eq!(feedback.caption_key(), "已复制");
        feedback.expire();
        assert_eq!(feedback.caption_key(), "复制信息");
    }
}
