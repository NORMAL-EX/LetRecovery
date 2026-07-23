//! Native Inno-style dialogs for toolbox operations that may change state.
//!
//! These dialogs never execute an operation. The first primary action creates
//! a confirmation request; only an explicit confirmation can produce a typed
//! execution intent for the existing controller/action layer.

use std::fmt;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontW, DeleteObject, HFONT};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetWindowTextLengthW, GetWindowTextW, MoveWindow, SendMessageW, SetWindowTextW,
    BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL,
    CB_RESETCONTENT, CB_SETCURSEL, ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE, ES_PASSWORD,
    ES_READONLY, LBS_EXTENDEDSEL, LBS_NOINTEGRALHEIGHT, LB_ADDSTRING, LB_GETSELCOUNT,
    LB_GETSELITEMS, LB_RESETCONTENT, LB_SETSEL, SW_HIDE, WM_SETFONT, WS_BORDER, WS_TABSTOP,
    WS_VSCROLL,
};

use super::controls::{child, combo_inventory_index, wide, NO_COMBO_SELECTION};
use super::dialog::{DialogButtons, DialogResult, DialogShell, DialogSpec, LogicalRect};
use super::layout::{measure_text, preferred_list_height, LayoutMetrics};
use super::theme::{apply_control_theme, NativeControlKind, Palette};
use crate::core::disk::PartitionStyle;
use crate::core::native_quick_partition::{
    default_layouts, format_layouts, parse_layouts, DiskFingerprint, QuickPartitionRequest,
};
use crate::core::quick_partition::PartitionLayout;

const ID_MUTATING_CONTROL_BASE: u16 = 63_300;
const BUTTON_CHECKED: usize = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutatingToolKind {
    NvidiaDriverRemoval,
    PartitionCopy,
    BatchFormat,
    ImportStorageDriver,
    QuickPartition,
    RemoveAppx,
    DriverBackupRestore,
    RepairBoot,
    TimeSynchronization,
    RunGhost,
    ResetNetwork,
    RunSpaceSniffer,
    ManageBitLocker,
    ResetPassword,
}

impl MutatingToolKind {
    pub const ALL: [Self; 14] = [
        Self::NvidiaDriverRemoval,
        Self::PartitionCopy,
        Self::BatchFormat,
        Self::ImportStorageDriver,
        Self::QuickPartition,
        Self::RemoveAppx,
        Self::DriverBackupRestore,
        Self::RepairBoot,
        Self::TimeSynchronization,
        Self::RunGhost,
        Self::ResetNetwork,
        Self::RunSpaceSniffer,
        Self::ManageBitLocker,
        Self::ResetPassword,
    ];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DriverTransferMode {
    #[default]
    Backup,
    Restore,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BootRepairMode {
    #[default]
    Auto,
    Uefi,
    Legacy,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BitLockerUnlockMethod {
    #[default]
    Password,
    RecoveryKey,
}

#[derive(Clone, PartialEq, Eq)]
pub enum BitLockerCredential {
    Password(String),
    RecoveryKey(String),
}

impl fmt::Debug for BitLockerCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Password(_) => formatter
                .debug_tuple("Password")
                .field(&"<redacted>")
                .finish(),
            Self::RecoveryKey(_) => formatter
                .debug_tuple("RecoveryKey")
                .field(&"<redacted>")
                .finish(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PasswordResetTarget {
    CurrentSystem,
    OfflineWindows(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BitLockerAction {
    #[default]
    Unlock,
    SuspendProtection,
    ResumeProtection,
    Decrypt,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MutatingToolIntent {
    RemoveNvidiaDrivers {
        devices: Vec<String>,
        /// `None` selects the current online system.
        offline_root: Option<String>,
    },
    CopyPartition {
        source: String,
        target: String,
    },
    BatchFormat {
        partitions: Vec<String>,
        file_system: String,
        volume_label: String,
    },
    ImportStorageDriver {
        directory: String,
        offline_root: String,
        recursive: bool,
    },
    QuickPartition {
        request: QuickPartitionRequest,
    },
    RemoveAppx {
        packages: Vec<String>,
        offline_root: String,
    },
    TransferDrivers {
        mode: DriverTransferMode,
        directory: String,
        system_root: String,
    },
    RepairBoot {
        windows_partition: String,
        boot_mode: BootRepairMode,
    },
    SynchronizeTime {
        server: String,
    },
    LaunchGhost,
    ResetNetwork,
    LaunchSpaceSniffer,
    ManageBitLocker {
        volume: String,
        action: BitLockerAction,
        credential: Option<BitLockerCredential>,
    },
    ResetPasswords {
        target: PasswordResetTarget,
        accounts: Vec<String>,
        enable_accounts: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum MutatingDialogIntent {
    BrowsePath,
    RequestConfirmation {
        kind: MutatingToolKind,
        summary: String,
    },
    Execute(MutatingToolIntent),
    Close,
}

/// Presentation snapshot populated by existing read-only preload controllers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MutatingToolState {
    pub source: String,
    pub target: String,
    pub path: String,
    pub value: String,
    /// Values presented by the first drop-down. A choice control never accepts
    /// text outside this inventory.
    pub first_choices: Vec<String>,
    pub first_choice_labels: Vec<String>,
    /// Values presented by the second drop-down when that field is a choice.
    pub second_choices: Vec<String>,
    pub second_choice_labels: Vec<String>,
    pub available_items: Vec<String>,
    pub available_item_labels: Vec<String>,
    pub selected_items: Vec<String>,
    pub option_enabled: bool,
    pub driver_mode: DriverTransferMode,
    pub bitlocker_action: BitLockerAction,
    pub bitlocker_unlock_method: BitLockerUnlockMethod,
    pub boot_repair_mode: BootRepairMode,
    pub credential: String,
    pub loading: bool,
    pub status: String,
    pub inventory_generation: u64,
    pub quick_partition_disks: Vec<DiskFingerprint>,
    pub quick_partition_style: PartitionStyle,
    pub quick_partition_layouts: Vec<PartitionLayout>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutatingInputControlKind {
    Text,
    Choice,
    MultiChoice,
    ReadOnlyText,
    StructuredLayout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MutatingControlModel {
    pub first: MutatingInputControlKind,
    pub second: MutatingInputControlKind,
    pub items: MutatingInputControlKind,
}

pub const fn control_model(kind: MutatingToolKind) -> MutatingControlModel {
    use MutatingInputControlKind::{Choice, MultiChoice, ReadOnlyText, StructuredLayout, Text};
    match kind {
        MutatingToolKind::PartitionCopy => MutatingControlModel {
            first: Choice,
            second: Choice,
            items: ReadOnlyText,
        },
        MutatingToolKind::BatchFormat => MutatingControlModel {
            first: Choice,
            second: Text,
            items: MultiChoice,
        },
        MutatingToolKind::ImportStorageDriver | MutatingToolKind::DriverBackupRestore => {
            MutatingControlModel {
                first: Choice,
                second: Text,
                items: ReadOnlyText,
            }
        }
        MutatingToolKind::QuickPartition => MutatingControlModel {
            first: Choice,
            second: Choice,
            items: StructuredLayout,
        },
        MutatingToolKind::RemoveAppx
        | MutatingToolKind::NvidiaDriverRemoval
        | MutatingToolKind::ResetPassword => MutatingControlModel {
            first: Choice,
            second: Text,
            items: MultiChoice,
        },
        MutatingToolKind::RepairBoot | MutatingToolKind::ManageBitLocker => MutatingControlModel {
            first: Choice,
            second: Choice,
            items: ReadOnlyText,
        },
        MutatingToolKind::TimeSynchronization
        | MutatingToolKind::RunGhost
        | MutatingToolKind::ResetNetwork
        | MutatingToolKind::RunSpaceSniffer => MutatingControlModel {
            first: Text,
            second: ReadOnlyText,
            items: ReadOnlyText,
        },
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MutatingContentLayout {
    pub first_label: LogicalRect,
    pub first_input: LogicalRect,
    pub second_label: LogicalRect,
    pub second_input: LogicalRect,
    pub extra_label: LogicalRect,
    pub extra_input: LogicalRect,
    pub option: LogicalRect,
    pub list: LogicalRect,
    pub status: LogicalRect,
}

impl MutatingContentLayout {
    pub fn calculate(width: i32, height: i32, dpi: u32) -> Self {
        Self::calculate_with_extra(width, height, dpi, false)
    }

    fn calculate_with_extra(width: i32, height: i32, dpi: u32, extra_visible: bool) -> Self {
        let scale = |value: i32| ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32;
        let width = width.max(0);
        let height = height.max(0);
        let gap = scale(7);
        let label_height = scale(20);
        let input_height = scale(29);
        let option_height = scale(25);
        let status_height = scale(56).min((height / 4).max(scale(32)));
        let first_label = LogicalRect {
            x: 0,
            y: 0,
            width,
            height: label_height,
        };
        let first_input = LogicalRect {
            x: 0,
            y: label_height,
            width,
            height: input_height,
        };
        let second_y = first_input.y + first_input.height + gap;
        let second_label = LogicalRect {
            x: 0,
            y: second_y,
            width,
            height: label_height,
        };
        let second_input = LogicalRect {
            x: 0,
            y: second_y + label_height,
            width,
            height: input_height,
        };
        let extra_y = second_input.y + second_input.height + gap;
        let extra_label = LogicalRect {
            x: 0,
            y: extra_y,
            width,
            height: if extra_visible { label_height } else { 0 },
        };
        let extra_input = LogicalRect {
            x: 0,
            y: extra_y + extra_label.height,
            width,
            height: if extra_visible { input_height } else { 0 },
        };
        let option = LogicalRect {
            x: 0,
            y: if extra_visible {
                extra_input.y + extra_input.height + gap
            } else {
                extra_y
            },
            width,
            height: option_height,
        };
        let list_y = option.y + option.height + gap;
        let status_y = (height - status_height).max(list_y);
        let list = LogicalRect {
            x: 0,
            y: list_y,
            width,
            height: (status_y - list_y - gap).max(0),
        };
        let status = LogicalRect {
            x: 0,
            y: status_y,
            width,
            height: (height - status_y).max(0),
        };
        Self {
            first_label,
            first_input,
            second_label,
            second_input,
            extra_label,
            extra_input,
            option,
            list,
            status,
        }
    }
}

#[derive(Default)]
struct MutatingControls {
    first_label: HWND,
    first_input: HWND,
    second_label: HWND,
    second_input: HWND,
    extra_label: HWND,
    extra_input: HWND,
    option: HWND,
    list: HWND,
    status: HWND,
}

pub struct NativeMutatingToolDialog {
    kind: MutatingToolKind,
    pub shell: DialogShell,
    controls: MutatingControls,
    state: MutatingToolState,
    confirmed: bool,
    font: HFONT,
}

impl NativeMutatingToolDialog {
    pub unsafe fn create(owner: HWND, kind: MutatingToolKind) -> windows::core::Result<Self> {
        let shell = DialogShell::create(owner, dialog_spec(kind))?;
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
        let controls = create_controls(shell.content(), kind)?;
        let mut dialog = Self {
            kind,
            shell,
            controls,
            state: MutatingToolState::default(),
            confirmed: false,
            font,
        };
        dialog.apply_font_and_theme();
        dialog.layout();
        dialog.shell.set_primary_enabled(false);
        Ok(dialog)
    }

    pub const fn kind(&self) -> MutatingToolKind {
        self.kind
    }

    pub fn state(&self) -> &MutatingToolState {
        &self.state
    }

    pub fn owns_first_choice(&self, control: HWND) -> bool {
        control == self.controls.first_input
            && control_model(self.kind).first == MutatingInputControlKind::Choice
    }

    pub unsafe fn begin_dynamic_inventory_load(&mut self) -> Option<(String, u64)> {
        if !matches!(
            self.kind,
            MutatingToolKind::ResetPassword
                | MutatingToolKind::RemoveAppx
                | MutatingToolKind::NvidiaDriverRemoval
        ) {
            return None;
        }
        let target = read_input(
            self.controls.first_input,
            MutatingInputControlKind::Choice,
            &self.state.first_choices,
        );
        if target.is_empty() {
            return None;
        }
        if self.kind == MutatingToolKind::ResetPassword {
            self.state.target = target.clone();
        } else {
            self.state.source = target.clone();
        }
        self.state.available_items.clear();
        self.state.available_item_labels.clear();
        self.state.selected_items.clear();
        self.state.loading = true;
        self.state.inventory_generation = self.state.inventory_generation.wrapping_add(1);
        self.state.status = crate::tr!("正在加载可选项目…");
        self.render_state();
        self.shell.set_primary_enabled(false);
        Some((target, self.state.inventory_generation))
    }

    pub unsafe fn apply_dynamic_inventory(
        &mut self,
        target: &str,
        generation: u64,
        result: Result<Vec<crate::core::native_tool_inventory::InventoryEntry>, String>,
    ) {
        let current = if self.kind == MutatingToolKind::ResetPassword {
            self.state.target.as_str()
        } else {
            self.state.source.as_str()
        };
        if !inventory_response_is_current(
            current,
            self.state.inventory_generation,
            target,
            generation,
        ) {
            return;
        }
        self.state.loading = false;
        match result {
            Ok(entries) => {
                self.state.available_items =
                    entries.iter().map(|entry| entry.value.clone()).collect();
                self.state.available_item_labels =
                    entries.into_iter().map(|entry| entry.label).collect();
                self.state.status = if self.state.available_items.is_empty() {
                    crate::tr!("未找到可选项目。")
                } else {
                    crate::tr!(
                        "已加载 {} 项，请从列表选择。",
                        self.state.available_items.len()
                    )
                };
            }
            Err(error) => {
                self.state.available_items.clear();
                self.state.available_item_labels.clear();
                self.state.status = crate::tr!("加载失败：{}", error);
            }
        }
        self.render_state();
        self.shell
            .set_primary_enabled(self.build_execution_intent().is_ok());
    }

    pub unsafe fn apply_first_choice_inventory(
        &mut self,
        result: Result<Vec<crate::core::native_tool_inventory::InventoryEntry>, String>,
        empty_message: &str,
    ) {
        let entries = match result {
            Ok(entries) => entries,
            Err(error) => {
                self.state.loading = false;
                self.state.status = crate::tr!("加载目标失败：{}", error);
                self.render_state();
                self.shell.set_primary_enabled(false);
                return;
            }
        };
        let previous = state_input_values(self.kind, &self.state).0.to_string();
        if self.kind == MutatingToolKind::QuickPartition {
            self.state.quick_partition_disks = entries
                .iter()
                .filter_map(|entry| entry.disk_fingerprint.clone())
                .collect();
        }
        self.state.first_choices = entries.iter().map(|entry| entry.value.clone()).collect();
        self.state.first_choice_labels = entries.into_iter().map(|entry| entry.label).collect();
        let selected = retained_first_choice(self.kind, &previous, &self.state.first_choices);
        match self.kind {
            MutatingToolKind::ImportStorageDriver
            | MutatingToolKind::DriverBackupRestore
            | MutatingToolKind::RepairBoot
            | MutatingToolKind::ManageBitLocker
            | MutatingToolKind::ResetPassword => self.state.target = selected,
            _ => self.state.source = selected,
        }
        if self.kind == MutatingToolKind::QuickPartition {
            self.reset_quick_partition_layout();
        }
        self.state.loading = false;
        self.state.status = if self.state.first_choices.is_empty() {
            empty_message.to_string()
        } else if self.kind == MutatingToolKind::QuickPartition {
            crate::tr!("每行：大小GB或* | 盘符或* | 卷标 | NTFS/FAT32/exFAT | ESP/DATA。* 仅用于最后一个分区的剩余空间。")
        } else {
            crate::tr!("请选择目标。")
        };
        self.render_state();
        self.shell
            .set_primary_enabled(self.build_execution_intent().is_ok());
    }

    pub fn owns_choice(&self, control: HWND) -> bool {
        (control == self.controls.first_input
            && control_model(self.kind).first == MutatingInputControlKind::Choice)
            || (control == self.controls.second_input
                && control_model(self.kind).second == MutatingInputControlKind::Choice)
    }

    pub unsafe fn handle_choice_changed(&mut self, control: HWND) {
        if self.kind != MutatingToolKind::QuickPartition || !self.owns_choice(control) {
            return;
        }
        if control == self.controls.first_input {
            self.state.source = read_input(
                self.controls.first_input,
                MutatingInputControlKind::Choice,
                &self.state.first_choices,
            );
        } else {
            let value = read_input(
                self.controls.second_input,
                MutatingInputControlKind::Choice,
                &self.state.second_choices,
            );
            self.state.quick_partition_style =
                parse_partition_style(&value).unwrap_or(self.state.quick_partition_style);
        }
        self.reset_quick_partition_layout();
        self.render_state();
        self.shell
            .set_primary_enabled(self.build_execution_intent().is_ok());
    }

    fn reset_quick_partition_layout(&mut self) {
        let Some(disk) = self
            .state
            .quick_partition_disks
            .iter()
            .find(|disk| disk.disk_number.to_string() == self.state.source)
        else {
            self.state.quick_partition_layouts.clear();
            return;
        };
        self.state.quick_partition_layouts =
            default_layouts(self.state.quick_partition_style, disk.size_bytes);
        self.state.option_enabled = self.state.quick_partition_style == PartitionStyle::GPT;
    }

    pub unsafe fn set_state(&mut self, state: MutatingToolState) {
        self.state = state;
        self.confirmed = false;
        self.render_state();
        self.shell
            .set_primary_enabled(!self.state.loading && self.build_execution_intent().is_ok());
    }

    pub fn confirm(&mut self, accepted: bool) -> Option<MutatingDialogIntent> {
        self.confirmed = accepted;
        accepted
            .then(|| self.build_execution_intent().ok())
            .flatten()
            .map(MutatingDialogIntent::Execute)
    }

    pub unsafe fn show_modeless(&mut self) {
        self.layout();
        self.shell.show_modeless();
    }

    pub unsafe fn take_intent(&mut self) -> Option<MutatingDialogIntent> {
        let result = self.shell.take_result()?;
        if result == DialogResult::Primary {
            self.sync_state_from_controls();
        }
        Some(match result {
            DialogResult::Cancel => MutatingDialogIntent::Close,
            DialogResult::Secondary => MutatingDialogIntent::BrowsePath,
            DialogResult::Primary if self.confirmed => self
                .build_execution_intent()
                .map(MutatingDialogIntent::Execute)
                .unwrap_or_else(|error| MutatingDialogIntent::RequestConfirmation {
                    kind: self.kind,
                    summary: error,
                }),
            DialogResult::Primary => MutatingDialogIntent::RequestConfirmation {
                kind: self.kind,
                summary: self.confirmation_summary(),
            },
        })
    }

    fn build_execution_intent(&self) -> Result<MutatingToolIntent, String> {
        build_execution_intent(self.kind, &self.state)
    }

    fn confirmation_summary(&self) -> String {
        match self.build_execution_intent() {
            Ok(intent) => crate::tr!(
                "请再次确认目标和选项。此操作尚未执行。\r\n{}",
                format_args!("{intent:?}")
            ),
            Err(error) => error,
        }
    }

    unsafe fn render_state(&mut self) {
        let (first, second) = state_input_values(self.kind, &self.state);
        let model = control_model(self.kind);
        render_input(
            self.controls.first_input,
            model.first,
            &self.state.first_choices,
            &self.state.first_choice_labels,
            first,
        );
        render_input(
            self.controls.second_input,
            model.second,
            &self.state.second_choices,
            &self.state.second_choice_labels,
            second,
        );
        set_text(self.controls.extra_input, &self.state.credential);
        if self.kind == MutatingToolKind::QuickPartition {
            set_text(
                self.controls.list,
                &format_layouts(&self.state.quick_partition_layouts),
            );
        } else {
            render_items(
                self.controls.list,
                model.items,
                &self.state.available_items,
                &self.state.available_item_labels,
                &self.state.selected_items,
            );
        }
        let status = if self.state.loading {
            crate::tr!("正在读取，请稍候…")
        } else {
            self.state.status.clone()
        };
        set_text(self.controls.status, &status);
        let checked = if self.kind == MutatingToolKind::DriverBackupRestore {
            self.state.driver_mode == DriverTransferMode::Restore
        } else if self.kind == MutatingToolKind::ManageBitLocker {
            self.state.bitlocker_unlock_method == BitLockerUnlockMethod::RecoveryKey
        } else {
            self.state.option_enabled
        };
        let _ = SendMessageW(
            self.controls.option,
            BM_SETCHECK,
            WPARAM(if checked { BUTTON_CHECKED } else { 0 }),
            LPARAM(0),
        );
        self.layout();
    }

    unsafe fn sync_state_from_controls(&mut self) {
        let model = control_model(self.kind);
        let first = read_input(
            self.controls.first_input,
            model.first,
            &self.state.first_choices,
        );
        let second = read_input(
            self.controls.second_input,
            model.second,
            &self.state.second_choices,
        );
        let extra = get_text(self.controls.extra_input);
        self.state.selected_items =
            read_selected_items(self.controls.list, model.items, &self.state.available_items);
        match self.kind {
            MutatingToolKind::PartitionCopy => {
                self.state.source = first;
                self.state.target = second;
            }
            MutatingToolKind::BatchFormat => {
                self.state.value = first;
                self.state.target = second;
            }
            MutatingToolKind::ImportStorageDriver | MutatingToolKind::DriverBackupRestore => {
                self.state.target = first;
                self.state.path = second;
            }
            MutatingToolKind::QuickPartition => {
                self.state.source = first;
                self.state.quick_partition_style =
                    parse_partition_style(&second).unwrap_or(self.state.quick_partition_style);
                self.state.quick_partition_layouts =
                    parse_layouts(&get_text(self.controls.list)).unwrap_or_default();
            }
            MutatingToolKind::RepairBoot => {
                self.state.target = first;
                self.state.value = second;
                self.state.boot_repair_mode = parse_boot_repair_mode(&self.state.value)
                    .unwrap_or(self.state.boot_repair_mode);
            }
            MutatingToolKind::ResetPassword => {
                self.state.target = first;
                self.state.value = second;
            }
            MutatingToolKind::ManageBitLocker => {
                self.state.target = first;
                self.state.bitlocker_action =
                    parse_bitlocker_action(&second).unwrap_or(self.state.bitlocker_action);
                self.state.credential = extra;
            }
            MutatingToolKind::TimeSynchronization => self.state.value = first,
            MutatingToolKind::NvidiaDriverRemoval
            | MutatingToolKind::RemoveAppx
            | MutatingToolKind::RunGhost
            | MutatingToolKind::ResetNetwork
            | MutatingToolKind::RunSpaceSniffer => {
                self.state.source = first;
                self.state.value = second;
            }
        }
        let checked = SendMessageW(self.controls.option, BM_GETCHECK, WPARAM(0), LPARAM(0)).0
            as usize
            == BUTTON_CHECKED;
        if self.kind == MutatingToolKind::DriverBackupRestore {
            self.state.driver_mode = if checked {
                DriverTransferMode::Restore
            } else {
                DriverTransferMode::Backup
            };
        } else if self.kind == MutatingToolKind::ManageBitLocker {
            self.state.bitlocker_unlock_method = if checked {
                BitLockerUnlockMethod::RecoveryKey
            } else {
                BitLockerUnlockMethod::Password
            };
        } else if self.kind != MutatingToolKind::QuickPartition {
            self.state.option_enabled = checked;
        }
        if self.kind == MutatingToolKind::QuickPartition {
            self.state.option_enabled = checked;
            self.state.quick_partition_layouts.retain(|layout| {
                self.state.quick_partition_style == PartitionStyle::GPT || !layout.is_esp
            });
            if checked
                && self.state.quick_partition_style == PartitionStyle::GPT
                && !self
                    .state
                    .quick_partition_layouts
                    .iter()
                    .any(|layout| layout.is_esp)
            {
                self.state.quick_partition_layouts.insert(
                    0,
                    PartitionLayout {
                        size_gb: 0.5,
                        drive_letter: None,
                        label: "EFI".into(),
                        is_esp: true,
                        file_system: "FAT32".into(),
                    },
                );
            }
        }
    }

    unsafe fn apply_font_and_theme(&self) {
        for control in self.all_controls() {
            if !control.is_invalid() {
                let _ = SendMessageW(control, WM_SETFONT, WPARAM(self.font.0 as usize), LPARAM(1));
            }
        }
        let palette = Palette::system();
        for control in [
            self.controls.first_label,
            self.controls.second_label,
            self.controls.extra_label,
            self.controls.option,
        ] {
            apply_valid_theme(control, palette, NativeControlKind::General);
        }
        let model = control_model(self.kind);
        apply_valid_theme(
            self.controls.first_input,
            palette,
            input_theme_kind(model.first),
        );
        apply_valid_theme(
            self.controls.second_input,
            palette,
            input_theme_kind(model.second),
        );
        apply_valid_theme(self.controls.extra_input, palette, NativeControlKind::Field);
        apply_valid_theme(self.controls.list, palette, items_theme_kind(model.items));
        apply_valid_theme(
            self.controls.status,
            palette,
            NativeControlKind::ScrollableField,
        );
    }

    unsafe fn layout(&mut self) {
        let mut client = RECT::default();
        let _ = GetClientRect(self.shell.content(), &mut client);
        let dpi = GetDpiForWindow(self.shell.hwnd()).max(96);
        let metrics = LayoutMetrics::for_dpi(dpi);
        let status_text = get_text(self.controls.status);
        let status_height = if status_text.is_empty() {
            0
        } else {
            measure_text(
                self.shell.hwnd(),
                self.font,
                &status_text,
                Some((client.right - client.left).max(0)),
            )
            .height
            .max(metrics.label_height)
        };
        let extra_height = if self.kind == MutatingToolKind::ManageBitLocker {
            metrics.label_height + metrics.field_height + metrics.control_gap
        } else {
            0
        };
        let desired_height = (metrics.label_height + metrics.field_height + metrics.control_gap)
            * 2
            + extra_height
            + metrics.label_height
            + metrics.control_gap
            + preferred_list_height(self.state.available_items.len(), dpi, 3, 8)
            + if status_height > 0 {
                metrics.control_gap + status_height
            } else {
                0
            };
        self.shell
            .fit_content_height(logical_height(desired_height, dpi));
        let _ = GetClientRect(self.shell.content(), &mut client);
        let layout = MutatingContentLayout::calculate_with_extra(
            client.right - client.left,
            client.bottom - client.top,
            dpi,
            self.kind == MutatingToolKind::ManageBitLocker,
        );
        for (control, rect) in self.all_controls().into_iter().zip([
            layout.first_label,
            layout.first_input,
            layout.second_label,
            layout.second_input,
            layout.extra_label,
            layout.extra_input,
            layout.option,
            layout.list,
            layout.status,
        ]) {
            let input_kind = if control == self.controls.first_input {
                Some(control_model(self.kind).first)
            } else if control == self.controls.second_input {
                Some(control_model(self.kind).second)
            } else {
                None
            };
            if input_kind == Some(MutatingInputControlKind::Choice) {
                let mut combo_rect = rect;
                combo_rect.height = scale(190, GetDpiForWindow(self.shell.hwnd()).max(96));
                move_control(control, combo_rect);
            } else {
                move_control(control, rect);
            }
        }
    }

    fn all_controls(&self) -> [HWND; 9] {
        [
            self.controls.first_label,
            self.controls.first_input,
            self.controls.second_label,
            self.controls.second_input,
            self.controls.extra_label,
            self.controls.extra_input,
            self.controls.option,
            self.controls.list,
            self.controls.status,
        ]
    }
}

fn retained_first_choice(kind: MutatingToolKind, previous: &str, choices: &[String]) -> String {
    if choices.iter().any(|choice| choice == previous) {
        previous.to_owned()
    } else if kind == MutatingToolKind::QuickPartition && choices.len() == 1 {
        choices[0].clone()
    } else {
        String::new()
    }
}

fn inventory_response_is_current(
    current_target: &str,
    current_generation: u64,
    response_target: &str,
    response_generation: u64,
) -> bool {
    current_target == response_target && current_generation == response_generation
}

const fn input_theme_kind(kind: MutatingInputControlKind) -> NativeControlKind {
    match kind {
        MutatingInputControlKind::MultiChoice => NativeControlKind::List,
        MutatingInputControlKind::Text
        | MutatingInputControlKind::Choice
        | MutatingInputControlKind::ReadOnlyText
        | MutatingInputControlKind::StructuredLayout => NativeControlKind::Field,
    }
}

const fn items_theme_kind(kind: MutatingInputControlKind) -> NativeControlKind {
    match kind {
        MutatingInputControlKind::MultiChoice => NativeControlKind::List,
        MutatingInputControlKind::Text
        | MutatingInputControlKind::Choice
        | MutatingInputControlKind::ReadOnlyText
        | MutatingInputControlKind::StructuredLayout => NativeControlKind::ScrollableField,
    }
}

unsafe fn apply_valid_theme(control: HWND, palette: Palette, kind: NativeControlKind) {
    if !control.is_invalid() {
        apply_control_theme(control, palette, kind);
    }
}

fn build_execution_intent(
    kind: MutatingToolKind,
    state: &MutatingToolState,
) -> Result<MutatingToolIntent, String> {
    validate_choice_fields(kind, state)?;
    let require = |value: &str, field: &str| {
        (!value.trim().is_empty())
            .then(|| value.trim().to_string())
            .ok_or_else(|| crate::tr!("{}不能为空", crate::tr!(field)))
    };
    let selected = || {
        let mut items = Vec::new();
        for item in &state.selected_items {
            let item = item.trim();
            if !item.is_empty()
                && state
                    .available_items
                    .iter()
                    .any(|available| available == item)
                && !items.iter().any(|selected| selected == item)
            {
                items.push(item.to_string());
            }
        }
        (!items.is_empty())
            .then_some(items)
            .ok_or_else(|| crate::tr!("请至少选择一项"))
    };
    Ok(match kind {
        MutatingToolKind::NvidiaDriverRemoval => MutatingToolIntent::RemoveNvidiaDrivers {
            devices: selected()?,
            offline_root: selected_system_target(&state.source),
        },
        MutatingToolKind::PartitionCopy => MutatingToolIntent::CopyPartition {
            source: if state.option_enabled {
                require(&state.source, "源分区")?
            } else {
                return Err(crate::tr!("必须确认已核对目标分区"));
            },
            target: {
                let target = require(&state.target, "目标分区")?;
                if target.eq_ignore_ascii_case(state.source.trim()) {
                    return Err(crate::tr!("源分区和目标分区不能相同"));
                }
                target
            },
        },
        MutatingToolKind::BatchFormat => MutatingToolIntent::BatchFormat {
            partitions: selected()?,
            file_system: require(&state.value, "文件系统")?,
            volume_label: state.target.trim().to_string(),
        },
        MutatingToolKind::ImportStorageDriver => MutatingToolIntent::ImportStorageDriver {
            directory: require(&state.path, "驱动目录")?,
            offline_root: require(&state.target, "Windows 目录")?,
            recursive: state.option_enabled,
        },
        MutatingToolKind::QuickPartition => {
            let disk_number = require(&state.source, "目标磁盘")?
                .parse::<u32>()
                .map_err(|_| crate::tr!("目标磁盘编号无效"))?;
            let disk = state
                .quick_partition_disks
                .iter()
                .find(|disk| disk.disk_number == disk_number)
                .cloned()
                .ok_or_else(|| crate::tr!("目标磁盘指纹不存在，请刷新磁盘列表"))?;
            let request = QuickPartitionRequest {
                disk,
                partition_style: state.quick_partition_style,
                layouts: state.quick_partition_layouts.clone(),
            };
            crate::core::native_quick_partition::validate_request(&request)
                .map_err(|error| error.to_string())?;
            MutatingToolIntent::QuickPartition { request }
        }
        MutatingToolKind::RemoveAppx => MutatingToolIntent::RemoveAppx {
            packages: selected()?,
            offline_root: selected_system_target(&state.source)
                .unwrap_or_else(|| "__CURRENT__".to_string()),
        },
        MutatingToolKind::DriverBackupRestore => MutatingToolIntent::TransferDrivers {
            mode: state.driver_mode,
            directory: require(&state.path, "驱动目录")?,
            system_root: match (state.driver_mode, selected_system_target(&state.target)) {
                (DriverTransferMode::Backup, None) => String::new(),
                (_, Some(target)) => target,
                (DriverTransferMode::Restore, None) => {
                    return Err(crate::tr!("恢复驱动时必须选择离线 Windows"))
                }
            },
        },
        MutatingToolKind::RepairBoot => MutatingToolIntent::RepairBoot {
            windows_partition: require(&state.target, "Windows 分区")?,
            boot_mode: state.boot_repair_mode,
        },
        MutatingToolKind::TimeSynchronization => MutatingToolIntent::SynchronizeTime {
            server: require(&state.value, "时间服务器")?,
        },
        MutatingToolKind::RunGhost => MutatingToolIntent::LaunchGhost,
        MutatingToolKind::ResetNetwork => MutatingToolIntent::ResetNetwork,
        MutatingToolKind::RunSpaceSniffer => MutatingToolIntent::LaunchSpaceSniffer,
        MutatingToolKind::ManageBitLocker => MutatingToolIntent::ManageBitLocker {
            volume: require(&state.target, "BitLocker 卷")?,
            action: state.bitlocker_action,
            credential: bitlocker_credential(state)?,
        },
        MutatingToolKind::ResetPassword => MutatingToolIntent::ResetPasswords {
            target: password_reset_target(&state.target)?,
            accounts: selected()?,
            enable_accounts: state.option_enabled,
        },
    })
}

fn selected_system_target(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()
        && value != "当前系统"
        && !value.eq_ignore_ascii_case("__CURRENT__")
        && !value.eq_ignore_ascii_case("__ONLINE__"))
    .then(|| value.to_string())
}

fn validate_choice_fields(kind: MutatingToolKind, state: &MutatingToolState) -> Result<(), String> {
    let model = control_model(kind);
    let (first, second) = state_input_values(kind, state);
    for (control_kind, value, choices, field) in [
        (model.first, first, &state.first_choices, "第一项"),
        (model.second, second, &state.second_choices, "第二项"),
    ] {
        if control_kind == MutatingInputControlKind::Choice
            && (value.trim().is_empty() || !choices.iter().any(|choice| choice == value.trim()))
        {
            return Err(crate::tr!("请从列表中选择{}", crate::tr!(field)));
        }
    }
    Ok(())
}

fn state_input_values(kind: MutatingToolKind, state: &MutatingToolState) -> (&str, &str) {
    match kind {
        MutatingToolKind::PartitionCopy => (&state.source, &state.target),
        MutatingToolKind::BatchFormat => (&state.value, &state.target),
        MutatingToolKind::ImportStorageDriver | MutatingToolKind::DriverBackupRestore => {
            (&state.target, &state.path)
        }
        MutatingToolKind::QuickPartition => (
            &state.source,
            partition_style_label(state.quick_partition_style),
        ),
        MutatingToolKind::RepairBoot => (
            &state.target,
            boot_repair_mode_label(state.boot_repair_mode),
        ),
        MutatingToolKind::ResetPassword => (&state.target, &state.value),
        MutatingToolKind::ManageBitLocker => (
            &state.target,
            bitlocker_action_label(state.bitlocker_action),
        ),
        MutatingToolKind::TimeSynchronization => (&state.value, &state.status),
        MutatingToolKind::NvidiaDriverRemoval
        | MutatingToolKind::RemoveAppx
        | MutatingToolKind::RunGhost
        | MutatingToolKind::ResetNetwork
        | MutatingToolKind::RunSpaceSniffer => (&state.source, &state.value),
    }
}

const fn bitlocker_action_label(action: BitLockerAction) -> &'static str {
    match action {
        BitLockerAction::Unlock => "解锁",
        BitLockerAction::SuspendProtection => "暂停保护",
        BitLockerAction::ResumeProtection => "恢复保护",
        BitLockerAction::Decrypt => "解密",
    }
}

fn parse_bitlocker_action(value: &str) -> Option<BitLockerAction> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("unlock") || value == "解锁" {
        Some(BitLockerAction::Unlock)
    } else if value.eq_ignore_ascii_case("suspend") || value == "暂停保护" {
        Some(BitLockerAction::SuspendProtection)
    } else if value.eq_ignore_ascii_case("resume") || value == "恢复保护" {
        Some(BitLockerAction::ResumeProtection)
    } else if value.eq_ignore_ascii_case("decrypt") || value == "解密" {
        Some(BitLockerAction::Decrypt)
    } else {
        None
    }
}

const fn boot_repair_mode_label(mode: BootRepairMode) -> &'static str {
    match mode {
        BootRepairMode::Auto => "Auto",
        BootRepairMode::Uefi => "UEFI",
        BootRepairMode::Legacy => "Legacy",
    }
}

const fn partition_style_label(style: PartitionStyle) -> &'static str {
    match style {
        PartitionStyle::GPT => "GPT",
        PartitionStyle::MBR => "MBR",
        PartitionStyle::Unknown => "",
    }
}

fn parse_partition_style(value: &str) -> Option<PartitionStyle> {
    if value.trim().eq_ignore_ascii_case("gpt") {
        Some(PartitionStyle::GPT)
    } else if value.trim().eq_ignore_ascii_case("mbr") {
        Some(PartitionStyle::MBR)
    } else {
        None
    }
}

fn parse_boot_repair_mode(value: &str) -> Option<BootRepairMode> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("auto") || value == "自动" {
        Some(BootRepairMode::Auto)
    } else if value.eq_ignore_ascii_case("uefi") {
        Some(BootRepairMode::Uefi)
    } else if value.eq_ignore_ascii_case("legacy") || value.eq_ignore_ascii_case("bios") {
        Some(BootRepairMode::Legacy)
    } else {
        None
    }
}

fn bitlocker_credential(state: &MutatingToolState) -> Result<Option<BitLockerCredential>, String> {
    if state.bitlocker_action != BitLockerAction::Unlock {
        return Ok(None);
    }
    let value = state.credential.trim();
    if value.is_empty() {
        return Err(crate::tr!("解锁 BitLocker 时必须填写密码或恢复密钥"));
    }
    match state.bitlocker_unlock_method {
        BitLockerUnlockMethod::Password => {
            Ok(Some(BitLockerCredential::Password(value.to_string())))
        }
        BitLockerUnlockMethod::RecoveryKey => {
            let formatted = lr_core::fveapi::format_recovery_key(value)
                .map_err(|_| crate::tr!("BitLocker 恢复密钥必须是 8 组、每组 6 位数字"))?;
            Ok(Some(BitLockerCredential::RecoveryKey(formatted)))
        }
    }
}

fn password_reset_target(value: &str) -> Result<PasswordResetTarget, String> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("__ONLINE__")
        || value.eq_ignore_ascii_case("current")
        || value.eq_ignore_ascii_case("current system")
        || value == "当前系统"
    {
        Ok(PasswordResetTarget::CurrentSystem)
    } else if value.is_empty() {
        Err(crate::tr!("请选择当前系统或离线 Windows 目录"))
    } else {
        Ok(PasswordResetTarget::OfflineWindows(value.to_string()))
    }
}

impl Drop for NativeMutatingToolDialog {
    fn drop(&mut self) {
        unsafe {
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font);
            }
        }
    }
}

fn dialog_spec(kind: MutatingToolKind) -> DialogSpec {
    let (title, description, primary, secondary) = match kind {
        MutatingToolKind::NvidiaDriverRemoval => (
            "卸载 NVIDIA 驱动",
            "选择要移除的 NVIDIA 设备和组件。",
            "继续",
            None,
        ),
        MutatingToolKind::PartitionCopy => (
            "分区对拷",
            "确认源分区和目标分区；目标内容将被覆盖。",
            "继续",
            None,
        ),
        MutatingToolKind::BatchFormat => ("批量格式化", "选择分区、文件系统和卷标。", "继续", None),
        MutatingToolKind::ImportStorageDriver => (
            "导入存储驱动",
            "选择包含 INF 的存储控制器驱动目录。",
            "继续",
            Some("浏览…"),
        ),
        MutatingToolKind::QuickPartition => {
            ("一键分区", "选择物理磁盘并复核完整分区布局。", "继续", None)
        }
        MutatingToolKind::RemoveAppx => (
            "移除 APPX",
            "当前仅支持通过 Windows 部署 API 移除当前系统应用；离线系统尚无安全部署边界，将拒绝执行且不会删除 WindowsApps 文件。",
            "继续",
            None,
        ),
        MutatingToolKind::DriverBackupRestore => (
            "驱动备份与恢复",
            "选择备份或恢复模式以及驱动目录。",
            "继续",
            Some("浏览…"),
        ),
        MutatingToolKind::RepairBoot => (
            "修复系统引导",
            "选择 Windows 分区并确认 BIOS/UEFI 模式。",
            "继续",
            None,
        ),
        MutatingToolKind::TimeSynchronization => (
            "同步系统时间",
            "从指定 NTP 服务器读取并设置系统时间。",
            "继续",
            None,
        ),
        MutatingToolKind::RunGhost => ("运行 Ghost", "启动随包提供的 Ghost 工具。", "继续", None),
        MutatingToolKind::ResetNetwork => {
            ("重置网络", "将重置网络组件和适配器配置。", "继续", None)
        }
        MutatingToolKind::RunSpaceSniffer => (
            "运行 SpaceSniffer",
            "启动随包提供的磁盘空间分析工具。",
            "继续",
            None,
        ),
        MutatingToolKind::ManageBitLocker => (
            "管理 BitLocker",
            "选择卷和要执行的 BitLocker 操作。",
            "继续",
            None,
        ),
        MutatingToolKind::ResetPassword => (
            "重置系统密码",
            "选择离线 Windows 和账户；不会显示或保存密码。",
            "继续",
            Some("浏览…"),
        ),
    };
    DialogSpec {
        window_title: crate::tr!(title),
        title: crate::tr!(title),
        description: crate::tr!(description),
        width: 720,
        height: 560,
        buttons: DialogButtons {
            primary: crate::tr!(primary),
            secondary: secondary.map(|label| crate::tr!(label)),
            cancel: Some(crate::tr!("关闭")),
        },
    }
}

unsafe fn create_controls(
    parent: HWND,
    kind: MutatingToolKind,
) -> windows::core::Result<MutatingControls> {
    let (first, second, option) = control_labels(kind);
    let model = control_model(kind);
    let edit = ES_AUTOHSCROLL | WS_BORDER.0 as i32 | WS_TABSTOP.0 as i32;
    let report_style = ES_MULTILINE
        | ES_AUTOVSCROLL
        | ES_READONLY
        | WS_BORDER.0 as i32
        | WS_VSCROLL.0 as i32
        | WS_TABSTOP.0 as i32;
    let controls = MutatingControls {
        first_label: child(parent, w!("STATIC"), &crate::tr!(first), 0, control_id(0))?,
        first_input: create_input(parent, model.first, edit, control_id(1))?,
        second_label: child(parent, w!("STATIC"), &crate::tr!(second), 0, control_id(2))?,
        second_input: create_input(parent, model.second, edit, control_id(3))?,
        extra_label: child(
            parent,
            w!("STATIC"),
            &crate::tr!("密码 / 恢复密钥"),
            0,
            control_id(7),
        )?,
        extra_input: child(parent, w!("EDIT"), "", edit | ES_PASSWORD, control_id(8))?,
        option: child(
            parent,
            w!("BUTTON"),
            &crate::tr!(option),
            BS_AUTOCHECKBOX | WS_TABSTOP.0 as i32,
            control_id(4),
        )?,
        list: create_items_control(parent, model.items, report_style, control_id(5))?,
        status: child(parent, w!("EDIT"), "", report_style, control_id(6))?,
    };
    if kind != MutatingToolKind::ManageBitLocker {
        let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(controls.extra_label, SW_HIDE);
        let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(controls.extra_input, SW_HIDE);
    }
    Ok(controls)
}

unsafe fn create_input(
    parent: HWND,
    kind: MutatingInputControlKind,
    edit_style: i32,
    id: u16,
) -> windows::core::Result<HWND> {
    match kind {
        MutatingInputControlKind::Choice => child(
            parent,
            w!("COMBOBOX"),
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32,
            id,
        ),
        MutatingInputControlKind::ReadOnlyText => {
            child(parent, w!("EDIT"), "", edit_style | ES_READONLY, id)
        }
        MutatingInputControlKind::Text
        | MutatingInputControlKind::MultiChoice
        | MutatingInputControlKind::StructuredLayout => {
            child(parent, w!("EDIT"), "", edit_style, id)
        }
    }
}

unsafe fn create_items_control(
    parent: HWND,
    kind: MutatingInputControlKind,
    report_style: i32,
    id: u16,
) -> windows::core::Result<HWND> {
    if kind == MutatingInputControlKind::MultiChoice {
        child(
            parent,
            w!("LISTBOX"),
            "",
            LBS_EXTENDEDSEL
                | LBS_NOINTEGRALHEIGHT
                | WS_BORDER.0 as i32
                | WS_VSCROLL.0 as i32
                | WS_TABSTOP.0 as i32,
            id,
        )
    } else if kind == MutatingInputControlKind::StructuredLayout {
        child(
            parent,
            w!("EDIT"),
            "",
            ES_MULTILINE
                | ES_AUTOVSCROLL
                | WS_BORDER.0 as i32
                | WS_VSCROLL.0 as i32
                | WS_TABSTOP.0 as i32,
            id,
        )
    } else {
        child(parent, w!("EDIT"), "", report_style, id)
    }
}

const fn control_labels(kind: MutatingToolKind) -> (&'static str, &'static str, &'static str) {
    match kind {
        MutatingToolKind::PartitionCopy => ("源分区", "目标分区", "我已核对目标分区"),
        MutatingToolKind::BatchFormat => ("文件系统", "卷标", "快速格式化"),
        MutatingToolKind::ImportStorageDriver => ("离线 Windows（可选）", "驱动目录", "包含子目录"),
        MutatingToolKind::QuickPartition => ("目标磁盘", "分区表类型", "自动包含 GPT ESP 分区"),
        MutatingToolKind::DriverBackupRestore => ("离线 Windows（可选）", "驱动目录", "恢复驱动"),
        MutatingToolKind::RepairBoot => ("Windows 分区", "启动模式", "自动检测启动模式"),
        MutatingToolKind::TimeSynchronization => ("NTP 服务器", "当前状态", "同步后重新读取"),
        MutatingToolKind::ManageBitLocker => ("BitLocker 卷", "操作", "使用恢复密钥解锁"),
        MutatingToolKind::ResetPassword => (
            "系统目标（当前系统或 Windows 目录）",
            "账户筛选",
            "同时启用所选账户",
        ),
        MutatingToolKind::NvidiaDriverRemoval => {
            ("检测到的设备", "移除范围", "同时移除 NVIDIA 软件")
        }
        MutatingToolKind::RemoveAppx => (
            "目标 Windows（离线暂不支持）",
            "应用筛选",
            "使用 Windows 部署 API 移除",
        ),
        MutatingToolKind::RunGhost
        | MutatingToolKind::ResetNetwork
        | MutatingToolKind::RunSpaceSniffer => ("操作", "说明", "我了解此操作的影响"),
    }
}

const fn control_id(offset: u16) -> u16 {
    ID_MUTATING_CONTROL_BASE + offset
}

fn scale(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32
}

fn logical_height(value: i32, dpi: u32) -> i32 {
    ((i64::from(value.max(0)) * 96 + i64::from(dpi.max(1)) / 2) / i64::from(dpi.max(1))) as i32
}

unsafe fn move_control(control: HWND, rect: LogicalRect) {
    if !control.is_invalid() {
        let _ = MoveWindow(
            control,
            rect.x,
            rect.y,
            rect.width.max(0),
            rect.height.max(0),
            true,
        );
    }
}

unsafe fn set_text(control: HWND, value: &str) {
    if !control.is_invalid() {
        let value = wide(value);
        let _ = SetWindowTextW(control, PCWSTR(value.as_ptr()));
    }
}

unsafe fn render_input(
    control: HWND,
    kind: MutatingInputControlKind,
    choices: &[String],
    labels: &[String],
    selected: &str,
) {
    if kind != MutatingInputControlKind::Choice {
        set_text(control, selected);
        return;
    }
    let _ = SendMessageW(control, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
    let mut selected_index = None;
    for (index, choice) in choices.iter().enumerate() {
        let value = wide(labels.get(index).unwrap_or(choice));
        let _ = SendMessageW(
            control,
            CB_ADDSTRING,
            WPARAM(0),
            LPARAM(value.as_ptr() as isize),
        );
        if choice == selected {
            selected_index = Some(index);
        }
    }
    let index = selected_index.unwrap_or(NO_COMBO_SELECTION);
    let _ = SendMessageW(control, CB_SETCURSEL, WPARAM(index), LPARAM(0));
}

unsafe fn render_items(
    control: HWND,
    kind: MutatingInputControlKind,
    available: &[String],
    labels: &[String],
    selected: &[String],
) {
    if kind != MutatingInputControlKind::MultiChoice {
        set_text(control, &available.join("\r\n"));
        return;
    }
    let _ = SendMessageW(control, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
    for (index, item) in available.iter().enumerate() {
        let value = wide(labels.get(index).unwrap_or(item));
        let _ = SendMessageW(
            control,
            LB_ADDSTRING,
            WPARAM(0),
            LPARAM(value.as_ptr() as isize),
        );
        if selected.iter().any(|selected| selected == item) {
            let _ = SendMessageW(control, LB_SETSEL, WPARAM(1), LPARAM(index as isize));
        }
    }
}

unsafe fn read_input(control: HWND, kind: MutatingInputControlKind, choices: &[String]) -> String {
    if kind != MutatingInputControlKind::Choice {
        return get_text(control);
    }
    let index = SendMessageW(control, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
    combo_inventory_index(index, choices.len())
        .and_then(|index| choices.get(index))
        .cloned()
        .unwrap_or_default()
}

unsafe fn read_selected_items(
    control: HWND,
    kind: MutatingInputControlKind,
    available: &[String],
) -> Vec<String> {
    if kind != MutatingInputControlKind::MultiChoice {
        return Vec::new();
    }
    let count = SendMessageW(control, LB_GETSELCOUNT, WPARAM(0), LPARAM(0)).0;
    if count <= 0 {
        return Vec::new();
    }
    let mut indices = vec![0_i32; count as usize];
    let copied = SendMessageW(
        control,
        LB_GETSELITEMS,
        WPARAM(indices.len()),
        LPARAM(indices.as_mut_ptr() as isize),
    )
    .0;
    indices.truncate(copied.max(0) as usize);
    indices
        .into_iter()
        .filter_map(|index| available.get(index as usize).cloned())
        .collect()
}

unsafe fn get_text(control: HWND) -> String {
    if control.is_invalid() {
        return String::new();
    }
    let length = GetWindowTextLengthW(control);
    let mut buffer = vec![0_u16; length as usize + 1];
    let copied = GetWindowTextW(control, &mut buffer);
    String::from_utf16_lossy(&buffer[..copied as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_state(kind: MutatingToolKind) -> MutatingToolState {
        let mut state = MutatingToolState {
            source: "D:".into(),
            target: "E:".into(),
            path: "D:\\Drivers".into(),
            value: "NTFS".into(),
            available_items: vec!["item".into()],
            selected_items: vec!["item".into()],
            option_enabled: true,
            credential: "password".into(),
            status: "ready".into(),
            ..Default::default()
        };
        if kind == MutatingToolKind::TimeSynchronization {
            state.value = "time.windows.com".into();
        } else if kind == MutatingToolKind::QuickPartition {
            state.source = "7".into();
            state.quick_partition_style = PartitionStyle::GPT;
            state.quick_partition_disks = vec![DiskFingerprint {
                disk_number: 7,
                model: "test disk".into(),
                size_bytes: 64 * 1024 * 1024 * 1024,
                partition_style: PartitionStyle::GPT,
                partitions: Vec::new(),
            }];
            state.quick_partition_layouts = default_layouts(
                PartitionStyle::GPT,
                state.quick_partition_disks[0].size_bytes,
            );
        }
        let model = control_model(kind);
        let (first, second) = state_input_values(kind, &state);
        let first = first.to_string();
        let second = second.to_string();
        if model.first == MutatingInputControlKind::Choice {
            state.first_choices = vec![first];
        }
        if model.second == MutatingInputControlKind::Choice {
            state.second_choices = vec![second];
        }
        match kind {
            MutatingToolKind::RepairBoot => {
                state.second_choices = vec!["Auto".into(), "UEFI".into(), "Legacy".into()]
            }
            MutatingToolKind::ManageBitLocker => {
                state.second_choices = vec![
                    "解锁".into(),
                    "暂停保护".into(),
                    "恢复保护".into(),
                    "解密".into(),
                ]
            }
            MutatingToolKind::ResetPassword => {
                state.first_choices = vec!["E:".into(), "当前系统".into(), "D:\\Windows".into()]
            }
            _ => {}
        }
        state
    }

    #[test]
    fn all_fourteen_routes_have_compact_dialog_specs_and_valid_intents() {
        for kind in MutatingToolKind::ALL {
            let spec = dialog_spec(kind);
            assert!(spec.width <= 760 && spec.height <= 600);
            assert!(spec.buttons.cancel.is_some());
            let state = ready_state(kind);
            assert!(build_execution_intent(kind, &state).is_ok(), "{kind:?}");
        }
    }

    #[test]
    fn dangerous_intent_requires_confirmation_before_execute() {
        let state = ready_state(MutatingToolKind::PartitionCopy);
        let intent = MutatingToolIntent::CopyPartition {
            source: state.source,
            target: state.target,
        };
        assert_ne!(
            MutatingDialogIntent::RequestConfirmation {
                kind: MutatingToolKind::PartitionCopy,
                summary: "confirm".into(),
            },
            MutatingDialogIntent::Execute(intent)
        );
        let mut unchecked = ready_state(MutatingToolKind::PartitionCopy);
        unchecked.option_enabled = false;
        assert!(build_execution_intent(MutatingToolKind::PartitionCopy, &unchecked).is_err());
    }

    #[test]
    fn content_layout_survives_low_resolution_and_200_percent_dpi() {
        for (width, height, dpi) in [(584, 330, 96), (1168, 660, 192), (420, 240, 144)] {
            let layout = MutatingContentLayout::calculate(width, height, dpi);
            for rect in [
                layout.first_label,
                layout.first_input,
                layout.second_label,
                layout.second_input,
                layout.option,
                layout.list,
                layout.status,
            ] {
                assert!(rect.x >= 0 && rect.y >= 0);
                assert!(rect.width >= 0 && rect.height >= 0);
                assert!(rect.x + rect.width <= width.max(0));
                assert!(rect.y + rect.height <= height.max(0));
            }
        }

        let layout = MutatingContentLayout::calculate_with_extra(584, 330, 96, true);
        assert!(layout.extra_label.height > 0);
        assert!(layout.extra_input.height > 0);
        assert!(layout.extra_input.y + layout.extra_input.height <= layout.option.y);
    }

    #[test]
    fn control_ids_are_reserved_and_non_overlapping() {
        assert_eq!(control_id(0), 63_300);
        assert_eq!(control_id(6), 63_306);
        assert_eq!(control_id(8), 63_308);
    }

    #[test]
    fn destructive_targets_are_choice_controls_and_collections_are_real_lists() {
        for kind in [
            MutatingToolKind::PartitionCopy,
            MutatingToolKind::ImportStorageDriver,
            MutatingToolKind::DriverBackupRestore,
            MutatingToolKind::RepairBoot,
            MutatingToolKind::ManageBitLocker,
            MutatingToolKind::ResetPassword,
            MutatingToolKind::RemoveAppx,
            MutatingToolKind::NvidiaDriverRemoval,
            MutatingToolKind::QuickPartition,
        ] {
            assert_eq!(control_model(kind).first, MutatingInputControlKind::Choice);
        }
        assert_eq!(
            control_model(MutatingToolKind::PartitionCopy).second,
            MutatingInputControlKind::Choice
        );
        for kind in [
            MutatingToolKind::BatchFormat,
            MutatingToolKind::ResetPassword,
            MutatingToolKind::RemoveAppx,
            MutatingToolKind::NvidiaDriverRemoval,
        ] {
            assert_eq!(
                control_model(kind).items,
                MutatingInputControlKind::MultiChoice
            );
        }
    }

    #[test]
    fn typed_choices_reject_values_and_items_outside_inventory() {
        let mut state = ready_state(MutatingToolKind::PartitionCopy);
        state.source = "Z:".into();
        assert!(build_execution_intent(MutatingToolKind::PartitionCopy, &state).is_err());

        let mut state = ready_state(MutatingToolKind::BatchFormat);
        state.selected_items = vec!["not enumerated".into()];
        assert!(build_execution_intent(MutatingToolKind::BatchFormat, &state).is_err());
    }

    #[test]
    fn dynamic_inventory_does_not_silently_select_a_target() {
        let choices = vec!["当前系统".to_owned(), "D:".to_owned()];
        for kind in [
            MutatingToolKind::NvidiaDriverRemoval,
            MutatingToolKind::ImportStorageDriver,
            MutatingToolKind::RemoveAppx,
            MutatingToolKind::DriverBackupRestore,
            MutatingToolKind::RepairBoot,
            MutatingToolKind::ManageBitLocker,
            MutatingToolKind::ResetPassword,
        ] {
            assert_eq!(retained_first_choice(kind, "", &choices), "");
        }
        assert_eq!(
            retained_first_choice(MutatingToolKind::RemoveAppx, "D:", &choices),
            "D:"
        );
    }

    #[test]
    fn quick_partition_only_auto_selects_a_single_disk() {
        assert_eq!(
            retained_first_choice(MutatingToolKind::QuickPartition, "", &["0".into()]),
            "0"
        );
        assert_eq!(
            retained_first_choice(
                MutatingToolKind::QuickPartition,
                "",
                &["0".into(), "1".into()]
            ),
            ""
        );
    }

    #[test]
    fn display_labels_never_replace_typed_values_and_stale_results_are_rejected() {
        let mut state = ready_state(MutatingToolKind::BatchFormat);
        state.available_items = vec!["D:".into()];
        state.available_item_labels = vec!["D: [Windows 11 24H2] [x64]".into()];
        state.selected_items = vec!["D:".into()];
        assert!(matches!(
            build_execution_intent(MutatingToolKind::BatchFormat, &state),
            Ok(MutatingToolIntent::BatchFormat { partitions, .. }) if partitions == ["D:"]
        ));
        state.selected_items = state.available_item_labels.clone();
        assert!(build_execution_intent(MutatingToolKind::BatchFormat, &state).is_err());

        assert!(inventory_response_is_current("D:", 3, "D:", 3));
        assert!(!inventory_response_is_current("D:", 4, "D:", 3));
        assert!(!inventory_response_is_current("E:", 3, "D:", 3));
    }

    #[test]
    fn bitlocker_unlock_requires_a_valid_redacted_credential() {
        let mut state = ready_state(MutatingToolKind::ManageBitLocker);
        state.credential.clear();
        assert!(build_execution_intent(MutatingToolKind::ManageBitLocker, &state).is_err());

        state.bitlocker_unlock_method = BitLockerUnlockMethod::RecoveryKey;
        state.credential = "111111-222222-333333-444444-555555-666666-777777-888888".into();
        let intent = build_execution_intent(MutatingToolKind::ManageBitLocker, &state).unwrap();
        assert_eq!(
            intent,
            MutatingToolIntent::ManageBitLocker {
                volume: "E:".into(),
                action: BitLockerAction::Unlock,
                credential: Some(BitLockerCredential::RecoveryKey(
                    "111111-222222-333333-444444-555555-666666-777777-888888".into()
                )),
            }
        );
        assert!(!format!("{intent:?}").contains("111111"));

        state.credential = "111111-222222".into();
        assert!(build_execution_intent(MutatingToolKind::ManageBitLocker, &state).is_err());
    }

    #[test]
    fn bitlocker_non_unlock_actions_never_forward_credentials() {
        for action in [
            BitLockerAction::SuspendProtection,
            BitLockerAction::ResumeProtection,
            BitLockerAction::Decrypt,
        ] {
            let mut state = ready_state(MutatingToolKind::ManageBitLocker);
            state.bitlocker_action = action;
            state.credential = "must-not-leak".into();
            assert_eq!(
                build_execution_intent(MutatingToolKind::ManageBitLocker, &state).unwrap(),
                MutatingToolIntent::ManageBitLocker {
                    volume: "E:".into(),
                    action,
                    credential: None,
                }
            );
        }
    }

    #[test]
    fn password_reset_distinguishes_current_and_offline_targets_and_keeps_accounts() {
        let mut state = ready_state(MutatingToolKind::ResetPassword);
        state.target = "当前系统".into();
        state.available_items = vec!["Administrator".into(), "Guest".into()];
        state.selected_items = vec![" Administrator ".into(), "Guest".into(), "Guest".into()];
        assert_eq!(
            build_execution_intent(MutatingToolKind::ResetPassword, &state).unwrap(),
            MutatingToolIntent::ResetPasswords {
                target: PasswordResetTarget::CurrentSystem,
                accounts: vec!["Administrator".into(), "Guest".into()],
                enable_accounts: true,
            }
        );

        state.target = "D:\\Windows".into();
        assert_eq!(
            build_execution_intent(MutatingToolKind::ResetPassword, &state).unwrap(),
            MutatingToolIntent::ResetPasswords {
                target: PasswordResetTarget::OfflineWindows("D:\\Windows".into()),
                accounts: vec!["Administrator".into(), "Guest".into()],
                enable_accounts: true,
            }
        );
    }

    #[test]
    fn repair_boot_preserves_auto_uefi_and_legacy_modes() {
        for (label, mode) in [
            ("自动", BootRepairMode::Auto),
            ("UEFI", BootRepairMode::Uefi),
            ("BIOS", BootRepairMode::Legacy),
        ] {
            assert_eq!(parse_boot_repair_mode(label), Some(mode));
            let mut state = ready_state(MutatingToolKind::RepairBoot);
            state.boot_repair_mode = mode;
            assert_eq!(
                build_execution_intent(MutatingToolKind::RepairBoot, &state).unwrap(),
                MutatingToolIntent::RepairBoot {
                    windows_partition: "E:".into(),
                    boot_mode: mode,
                }
            );
        }
    }

    #[test]
    fn driver_import_and_transfer_options_are_preserved() {
        let mut state = ready_state(MutatingToolKind::ImportStorageDriver);
        state.option_enabled = true;
        assert_eq!(
            build_execution_intent(MutatingToolKind::ImportStorageDriver, &state).unwrap(),
            MutatingToolIntent::ImportStorageDriver {
                directory: "D:\\Drivers".into(),
                offline_root: "E:".into(),
                recursive: true,
            }
        );

        state.driver_mode = DriverTransferMode::Restore;
        assert_eq!(
            build_execution_intent(MutatingToolKind::DriverBackupRestore, &state).unwrap(),
            MutatingToolIntent::TransferDrivers {
                mode: DriverTransferMode::Restore,
                directory: "D:\\Drivers".into(),
                system_root: "E:".into(),
            }
        );
    }

    #[test]
    fn native_theme_roles_distinguish_combo_lists_and_scrollable_reports() {
        assert_eq!(
            input_theme_kind(MutatingInputControlKind::Choice),
            NativeControlKind::Field
        );
        assert_eq!(
            items_theme_kind(MutatingInputControlKind::MultiChoice),
            NativeControlKind::List
        );
        assert_eq!(
            items_theme_kind(MutatingInputControlKind::ReadOnlyText),
            NativeControlKind::ScrollableField
        );
    }
}
