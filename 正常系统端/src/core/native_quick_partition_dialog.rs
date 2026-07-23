//! Pure editor state for the dedicated native quick-partition dialog.
//!
//! This module deliberately performs no enumeration and no disk writes. It restores the legacy
//! editor's defaults and calculations, then emits fingerprinted requests for the existing
//! fail-closed native quick-partition boundary. Resizing an existing partition is represented as
//! a separate typed request so merely opening or editing the dialog can never resize a volume.

use super::disk::PartitionStyle;
use super::native_quick_partition::{
    validate_request, DiskFingerprint, QuickPartitionError, QuickPartitionRequest,
};
use super::quick_partition::{
    get_unallocated_space_after_partition_with_disk, DiskPartitionInfo, PartitionLayout,
    PhysicalDisk, ResizePartitionResult,
};

const ESP_SIZE_GB: f64 = 0.5;

#[derive(Clone, Debug, PartialEq)]
pub struct ExistingPartitionResizeRequest {
    pub disk: DiskFingerprint,
    pub partition_number: u32,
    pub drive_letter: char,
    pub current_size_mb: u64,
    pub new_size_mb: u64,
    pub used_size_mb: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExistingPartitionResizeOutcome {
    pub message: String,
    pub new_size_mb: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExistingPartitionResizeError {
    DevelopmentBuildDenied,
    InvalidRequest(String),
    Inventory(String),
    DiskMissing(u32),
    DiskChanged,
    PartitionMissing(u32),
    PartitionChanged,
    Execution(String),
}

impl std::fmt::Display for ExistingPartitionResizeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DevelopmentBuildDenied => formatter.write_str(
                "existing partition resize is disabled in non-elevated development builds",
            ),
            Self::InvalidRequest(detail) | Self::Inventory(detail) | Self::Execution(detail) => {
                formatter.write_str(detail)
            }
            Self::DiskMissing(number) => write!(formatter, "disk {number} is no longer present"),
            Self::DiskChanged => formatter.write_str(
                "the selected disk identity or partition layout changed; refresh and review again",
            ),
            Self::PartitionMissing(number) => {
                write!(formatter, "partition {number} is no longer present")
            }
            Self::PartitionChanged => formatter.write_str(
                "the selected partition drive letter, size, or used space changed; refresh and review again",
            ),
        }
    }
}

impl std::error::Error for ExistingPartitionResizeError {}

pub trait ExistingPartitionResizeInventory {
    fn enumerate(&mut self) -> Result<Vec<PhysicalDisk>, String>;
}

pub trait ExistingPartitionResizeRunner {
    fn run(&mut self, request: &ExistingPartitionResizeRequest) -> ResizePartitionResult;
}

/// Executes a resize only after a fresh inventory proves that the exact disk and partition are
/// unchanged and that the requested size still fits the adjacent unallocated range.
pub(crate) fn execute_existing_partition_resize_with_backends(
    request: &ExistingPartitionResizeRequest,
    system_drive: char,
    inventory: &mut dyn ExistingPartitionResizeInventory,
    runner: &mut dyn ExistingPartitionResizeRunner,
) -> Result<ExistingPartitionResizeOutcome, ExistingPartitionResizeError> {
    validate_resize_request_shape(request)?;
    let disks = inventory
        .enumerate()
        .map_err(ExistingPartitionResizeError::Inventory)?;
    let disk = disks
        .iter()
        .find(|disk| disk.disk_number == request.disk.disk_number)
        .ok_or(ExistingPartitionResizeError::DiskMissing(
            request.disk.disk_number,
        ))?;
    if DiskFingerprint::from(disk) != request.disk {
        return Err(ExistingPartitionResizeError::DiskChanged);
    }
    validate_resize_request_against_disk(request, disk, system_drive)?;
    let result = runner.run(request);
    if !result.success {
        return Err(ExistingPartitionResizeError::Execution(result.message));
    }
    if result.new_size_mb != request.new_size_mb {
        return Err(ExistingPartitionResizeError::Execution(
            "resize runner reported an unexpected final size".into(),
        ));
    }
    Ok(ExistingPartitionResizeOutcome {
        message: result.message,
        new_size_mb: result.new_size_mb,
    })
}

/// Production wrapper. Development builds return before constructing an inventory or runner, so
/// tests and UI visual regression runs cannot reach host disk enumeration or DiskPart.
pub fn execute_existing_partition_resize(
    request: &ExistingPartitionResizeRequest,
) -> Result<ExistingPartitionResizeOutcome, ExistingPartitionResizeError> {
    #[cfg(feature = "non-elevated-tests")]
    {
        let _ = request;
        Err(ExistingPartitionResizeError::DevelopmentBuildDenied)
    }
    #[cfg(not(feature = "non-elevated-tests"))]
    {
        struct Inventory;
        impl ExistingPartitionResizeInventory for Inventory {
            fn enumerate(&mut self) -> Result<Vec<PhysicalDisk>, String> {
                Ok(super::quick_partition::get_physical_disks())
            }
        }
        struct Runner;
        impl ExistingPartitionResizeRunner for Runner {
            fn run(&mut self, request: &ExistingPartitionResizeRequest) -> ResizePartitionResult {
                super::quick_partition::resize_existing_partition(
                    request.disk.disk_number,
                    request.partition_number,
                    Some(request.drive_letter),
                    request.current_size_mb,
                    request.new_size_mb,
                    request.used_size_mb,
                )
            }
        }
        let system_drive = std::env::var("SystemDrive")
            .ok()
            .and_then(|drive| drive.chars().next())
            .unwrap_or('C');
        execute_existing_partition_resize_with_backends(
            request,
            system_drive,
            &mut Inventory,
            &mut Runner,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorRow {
    Existing(usize),
    Planned(usize),
}

#[derive(Clone, Debug)]
pub struct QuickPartitionDialogState {
    pub loading: bool,
    pub disks: Vec<PhysicalDisk>,
    pub selected_disk_number: Option<u32>,
    pub partition_style: PartitionStyle,
    pub recommended_style: PartitionStyle,
    pub planned: Vec<PartitionLayout>,
    pub selected_row: Option<EditorRow>,
    pub resize_size_text: String,
    pub message: String,
    used_drive_letters: Vec<char>,
    system_drive: char,
}

impl QuickPartitionDialogState {
    pub fn new(
        recommended_style: PartitionStyle,
        used_drive_letters: Vec<char>,
        system_drive: char,
    ) -> Self {
        let recommended_style = normalize_style(recommended_style);
        Self {
            loading: true,
            disks: Vec::new(),
            selected_disk_number: None,
            partition_style: recommended_style,
            recommended_style,
            planned: Vec::new(),
            selected_row: None,
            resize_size_text: String::new(),
            message: crate::tr!("正在加载磁盘列表..."),
            used_drive_letters: used_drive_letters
                .into_iter()
                .map(|letter| letter.to_ascii_uppercase())
                .collect(),
            system_drive: system_drive.to_ascii_uppercase(),
        }
    }

    pub fn begin_refresh(&mut self) {
        self.loading = true;
        self.disks.clear();
        self.selected_disk_number = None;
        self.planned.clear();
        self.clear_row_selection();
        self.message = crate::tr!("正在加载磁盘列表...");
    }

    pub fn apply_inventory(&mut self, result: Result<Vec<PhysicalDisk>, String>) {
        self.loading = false;
        match result {
            Ok(mut disks) => {
                disks.sort_by_key(|disk| disk.disk_number);
                disks.dedup_by_key(|disk| disk.disk_number);
                self.disks = disks;
                self.selected_disk_number = None;
                self.planned.clear();
                self.clear_row_selection();
                if self.disks.len() == 1 {
                    let number = self.disks[0].disk_number;
                    self.select_disk(Some(number));
                }
                self.message = if self.disks.is_empty() {
                    crate::tr!("未检测到物理磁盘")
                } else {
                    String::new()
                };
            }
            Err(error) => {
                self.disks.clear();
                self.selected_disk_number = None;
                self.planned.clear();
                self.clear_row_selection();
                self.message = crate::tr!("加载失败：{}", error);
            }
        }
    }

    pub fn selected_disk(&self) -> Option<&PhysicalDisk> {
        let number = self.selected_disk_number?;
        self.disks.iter().find(|disk| disk.disk_number == number)
    }

    pub fn select_disk(&mut self, disk_number: Option<u32>) {
        self.selected_disk_number =
            disk_number.filter(|number| self.disks.iter().any(|disk| disk.disk_number == *number));
        self.planned.clear();
        self.clear_row_selection();
        self.partition_style = self
            .selected_disk()
            .filter(|disk| disk.is_initialized)
            .map_or(self.recommended_style, |disk| {
                normalize_style(disk.partition_style)
            });
        self.message.clear();
    }

    pub fn set_partition_style(&mut self, style: PartitionStyle) {
        self.partition_style = normalize_style(style);
        if self.partition_style == PartitionStyle::MBR {
            let removed_esp = self.planned.iter().any(|layout| layout.is_esp);
            self.planned.retain(|layout| !layout.is_esp);
            if removed_esp {
                self.clear_row_selection();
            }
        }
        self.message.clear();
    }

    pub fn rows(&self) -> impl Iterator<Item = EditorRow> + '_ {
        let existing = self
            .selected_disk()
            .into_iter()
            .flat_map(|disk| (0..disk.partitions.len()).map(EditorRow::Existing));
        existing.chain((0..self.planned.len()).map(EditorRow::Planned))
    }

    pub fn select_row(&mut self, row: Option<EditorRow>) {
        self.selected_row = row.filter(|row| self.row_exists(*row));
        self.resize_size_text = self
            .selected_row
            .and_then(|row| self.row_size_gb(row))
            .map(|size| format!("{size:.1}"))
            .unwrap_or_default();
        self.message.clear();
    }

    pub fn add_data_partition(&mut self) -> bool {
        let Some(disk) = self.selected_disk().cloned() else {
            self.message = crate::tr!("请先选择要分区的磁盘");
            return false;
        };
        let used = self.used_letters_for_selected_disk();
        let drive_letter = ('C'..='Z').find(|letter| !used.contains(letter));
        let available = self.unallocated_gb(&disk);
        if available >= 1.0 {
            self.planned.push(PartitionLayout {
                size_gb: round_tenth(available),
                drive_letter,
                label: String::new(),
                is_esp: false,
                file_system: "NTFS".into(),
            });
            self.select_row(Some(EditorRow::Planned(self.planned.len() - 1)));
            return true;
        }
        if let Some(index) = self
            .planned
            .iter()
            .rposition(|layout| !layout.is_esp && layout.size_gb >= 2.0)
        {
            let original = self.planned[index].size_gb;
            let split = ((original / 5.0) * 10.0).floor() / 10.0;
            if split >= 1.0 {
                self.planned[index].size_gb = round_tenth(original - split);
                self.planned.push(PartitionLayout {
                    size_gb: split,
                    drive_letter,
                    label: String::new(),
                    is_esp: false,
                    file_system: "NTFS".into(),
                });
                self.select_row(Some(EditorRow::Planned(self.planned.len() - 1)));
                return true;
            }
        }
        self.message = crate::tr!("无法创建新分区：没有足够的可用空间");
        false
    }

    pub fn add_esp_partition(&mut self) -> bool {
        if self.partition_style != PartitionStyle::GPT {
            self.message = crate::tr!("ESP 分区仅适用于 GPT 分区表");
            return false;
        }
        let Some(disk) = self.selected_disk().cloned() else {
            self.message = crate::tr!("请先选择要分区的磁盘");
            return false;
        };
        if disk.partitions.iter().any(|partition| partition.is_esp)
            || self.planned.iter().any(|layout| layout.is_esp)
        {
            self.message = crate::tr!("已存在 ESP 分区");
            return false;
        }
        if self.unallocated_gb(&disk) < ESP_SIZE_GB {
            let Some(index) = self
                .planned
                .iter()
                .position(|layout| !layout.is_esp && layout.size_gb > ESP_SIZE_GB + 1.0)
            else {
                self.message = crate::tr!("无法创建 ESP 分区：没有足够的可用空间");
                return false;
            };
            self.planned[index].size_gb -= ESP_SIZE_GB;
        }
        self.planned.insert(
            0,
            PartitionLayout {
                size_gb: ESP_SIZE_GB,
                drive_letter: None,
                label: "EFI".into(),
                is_esp: true,
                file_system: "FAT32".into(),
            },
        );
        self.select_row(Some(EditorRow::Planned(0)));
        true
    }

    pub fn delete_selected(&mut self) -> bool {
        match self.selected_row {
            Some(EditorRow::Planned(index)) if index < self.planned.len() => {
                self.planned.remove(index);
                self.clear_row_selection();
                true
            }
            Some(EditorRow::Existing(_)) => {
                self.message = crate::tr!("无法删除已有分区，一键分区会清除整个磁盘");
                false
            }
            _ => false,
        }
    }

    pub fn apply_resize_text(&mut self) -> Result<Option<ExistingPartitionResizeRequest>, String> {
        let size = self
            .resize_size_text
            .trim()
            .parse::<f64>()
            .map_err(|_| crate::tr!("请输入有效的数字"))?;
        let row = self.selected_row.ok_or_else(|| crate::tr!("未选择分区"))?;
        match row {
            EditorRow::Planned(index) => {
                let max = self.max_planned_size(index)?;
                if !(0.5..=max).contains(&size) {
                    return Err(crate::tr!(
                        "大小必须在 0.5 GB 到 {} GB 之间",
                        format!("{max:.1}")
                    ));
                }
                self.planned[index].size_gb = size;
                self.message.clear();
                Ok(None)
            }
            EditorRow::Existing(index) => self.existing_resize_request(index, size).map(Some),
        }
    }

    pub fn quick_partition_request(&self) -> Result<QuickPartitionRequest, String> {
        let disk = self
            .selected_disk()
            .ok_or_else(|| crate::tr!("请先选择要分区的磁盘"))?;
        if self.planned.is_empty() {
            return Err(crate::tr!("请至少添加一个新分区"));
        }
        let request = QuickPartitionRequest {
            disk: DiskFingerprint::from(disk),
            partition_style: self.partition_style,
            layouts: self.planned.clone(),
        };
        validate_request(&request).map_err(localize_plan_error)?;
        Ok(request)
    }

    fn existing_resize_request(
        &self,
        index: usize,
        new_size_gb: f64,
    ) -> Result<ExistingPartitionResizeRequest, String> {
        let disk = self
            .selected_disk()
            .ok_or_else(|| crate::tr!("请先选择要分区的磁盘"))?;
        let partition = disk
            .partitions
            .get(index)
            .ok_or_else(|| crate::tr!("分区信息不可用"))?;
        validate_existing_resize_target(partition, self.system_drive)?;
        let min = (partition.used_gb() + 0.1).max(0.5);
        let max = partition.size_gb()
            + get_unallocated_space_after_partition_with_disk(disk, partition.partition_number)
                as f64
                / 1024.0;
        if new_size_gb < min || new_size_gb > max {
            return Err(crate::tr!(
                "大小必须在 {} GB 到 {} GB 之间",
                format!("{min:.1}"),
                format!("{max:.1}")
            ));
        }
        Ok(ExistingPartitionResizeRequest {
            disk: DiskFingerprint::from(disk),
            partition_number: partition.partition_number,
            drive_letter: partition.drive_letter.expect("validated drive letter"),
            current_size_mb: bytes_to_mib(partition.size_bytes),
            new_size_mb: gib_to_mib(new_size_gb),
            used_size_mb: bytes_to_mib(partition.used_bytes),
        })
    }

    fn max_planned_size(&self, index: usize) -> Result<f64, String> {
        if index >= self.planned.len() {
            return Err(crate::tr!("分区信息不可用"));
        }
        let disk = self
            .selected_disk()
            .ok_or_else(|| crate::tr!("请先选择要分区的磁盘"))?;
        let existing: f64 = disk.partitions.iter().map(DiskPartitionInfo::size_gb).sum();
        let other: f64 = self
            .planned
            .iter()
            .enumerate()
            .filter(|(current, _)| *current != index)
            .map(|(_, layout)| layout.size_gb)
            .sum();
        Ok((disk.size_gb() - existing - other).max(0.0))
    }

    fn unallocated_gb(&self, disk: &PhysicalDisk) -> f64 {
        let existing: f64 = disk.partitions.iter().map(DiskPartitionInfo::size_gb).sum();
        let planned: f64 = self.planned.iter().map(|layout| layout.size_gb).sum();
        (disk.size_gb() - existing - planned).max(0.0)
    }

    fn used_letters_for_selected_disk(&self) -> Vec<char> {
        let mut used = self.used_drive_letters.clone();
        if let Some(disk) = self.selected_disk() {
            used.extend(
                disk.partitions
                    .iter()
                    .filter_map(|partition| partition.drive_letter),
            );
        }
        used.extend(self.planned.iter().filter_map(|layout| layout.drive_letter));
        used.into_iter()
            .map(|letter| letter.to_ascii_uppercase())
            .collect()
    }

    fn row_exists(&self, row: EditorRow) -> bool {
        match row {
            EditorRow::Existing(index) => self
                .selected_disk()
                .is_some_and(|disk| index < disk.partitions.len()),
            EditorRow::Planned(index) => index < self.planned.len(),
        }
    }

    fn row_size_gb(&self, row: EditorRow) -> Option<f64> {
        match row {
            EditorRow::Existing(index) => self
                .selected_disk()?
                .partitions
                .get(index)
                .map(DiskPartitionInfo::size_gb),
            EditorRow::Planned(index) => self.planned.get(index).map(|layout| layout.size_gb),
        }
    }

    fn clear_row_selection(&mut self) {
        self.selected_row = None;
        self.resize_size_text.clear();
    }
}

fn validate_resize_request_shape(
    request: &ExistingPartitionResizeRequest,
) -> Result<(), ExistingPartitionResizeError> {
    if request.disk.size_bytes == 0 {
        return Err(invalid_resize("selected disk reports zero capacity"));
    }
    if request.partition_number == 0 {
        return Err(invalid_resize("partition number must be non-zero"));
    }
    if !request.drive_letter.is_ascii_alphabetic() {
        return Err(invalid_resize("drive letter must be an ASCII letter"));
    }
    if request.current_size_mb == 0 || request.new_size_mb == 0 {
        return Err(invalid_resize("partition sizes must be non-zero"));
    }
    if request.used_size_mb > request.current_size_mb {
        return Err(invalid_resize(
            "used space cannot exceed the current partition size",
        ));
    }
    let minimum = request.used_size_mb.saturating_add(100);
    if request.new_size_mb < minimum {
        return Err(invalid_resize(format!(
            "target size must be at least {minimum} MiB"
        )));
    }
    Ok(())
}

fn validate_resize_request_against_disk(
    request: &ExistingPartitionResizeRequest,
    disk: &PhysicalDisk,
    system_drive: char,
) -> Result<(), ExistingPartitionResizeError> {
    let partition = disk
        .partitions
        .iter()
        .find(|partition| partition.partition_number == request.partition_number)
        .ok_or(ExistingPartitionResizeError::PartitionMissing(
            request.partition_number,
        ))?;
    let current_size_mb = bytes_to_mib(partition.size_bytes);
    let used_size_mb = bytes_to_mib(partition.used_bytes);
    if partition
        .drive_letter
        .map(|letter| letter.to_ascii_uppercase())
        != Some(request.drive_letter.to_ascii_uppercase())
        || current_size_mb != request.current_size_mb
        || used_size_mb != request.used_size_mb
    {
        return Err(ExistingPartitionResizeError::PartitionChanged);
    }
    validate_existing_resize_target(partition, system_drive)
        .map_err(ExistingPartitionResizeError::InvalidRequest)?;
    let adjacent_mb =
        get_unallocated_space_after_partition_with_disk(disk, request.partition_number);
    let maximum = current_size_mb.saturating_add(adjacent_mb);
    if request.new_size_mb > maximum {
        return Err(invalid_resize(format!(
            "target size exceeds the current adjacent limit of {maximum} MiB"
        )));
    }
    Ok(())
}

fn validate_existing_resize_target(
    partition: &DiskPartitionInfo,
    system_drive: char,
) -> Result<(), String> {
    if partition.is_esp {
        return Err(crate::tr!("ESP分区不支持调整大小"));
    }
    if partition.is_msr {
        return Err(crate::tr!("MSR分区不支持调整大小"));
    }
    if partition.is_recovery {
        return Err(crate::tr!("恢复分区不支持调整大小"));
    }
    let letter = partition
        .drive_letter
        .ok_or_else(|| crate::tr!("分区没有盘符，无法调整大小"))?;
    if letter.eq_ignore_ascii_case(&system_drive) {
        return Err(crate::tr!("无法调整当前系统分区大小"));
    }
    Ok(())
}

fn localize_plan_error(error: QuickPartitionError) -> String {
    match error {
        QuickPartitionError::InvalidPlan(detail) => crate::tr!("分区规划无效：{}", detail),
        other => other.to_string(),
    }
}

fn normalize_style(style: PartitionStyle) -> PartitionStyle {
    match style {
        PartitionStyle::MBR => PartitionStyle::MBR,
        _ => PartitionStyle::GPT,
    }
}

fn round_tenth(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn gib_to_mib(value: f64) -> u64 {
    (value * 1024.0).round().max(0.0) as u64
}

fn bytes_to_mib(value: u64) -> u64 {
    value / 1024 / 1024
}

fn invalid_resize(detail: impl Into<String>) -> ExistingPartitionResizeError {
    ExistingPartitionResizeError::InvalidRequest(detail.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

    fn disk(number: u32, initialized: bool, style: PartitionStyle) -> PhysicalDisk {
        PhysicalDisk {
            disk_number: number,
            size_bytes: 100 * GIB as u64,
            model: format!("Test Disk {number}"),
            partition_style: style,
            is_initialized: initialized,
            partitions: Vec::new(),
            unallocated_bytes: 100 * GIB as u64,
        }
    }

    fn resizable_disk() -> PhysicalDisk {
        let mut value = disk(5, true, PartitionStyle::GPT);
        value.partitions.push(DiskPartitionInfo {
            partition_number: 7,
            size_bytes: 50 * GIB as u64,
            offset_bytes: 1024 * 1024,
            drive_letter: Some('D'),
            label: "Data".into(),
            file_system: "NTFS".into(),
            is_esp: false,
            is_msr: false,
            is_recovery: false,
            partition_type: "basic".into(),
            used_bytes: 10 * GIB as u64,
            free_bytes: 40 * GIB as u64,
            is_active: false,
        });
        value.unallocated_bytes = 50 * GIB as u64;
        value
    }

    fn resize_request() -> ExistingPartitionResizeRequest {
        let disk = resizable_disk();
        ExistingPartitionResizeRequest {
            disk: DiskFingerprint::from(&disk),
            partition_number: 7,
            drive_letter: 'D',
            current_size_mb: 50 * 1024,
            new_size_mb: 40 * 1024,
            used_size_mb: 10 * 1024,
        }
    }

    struct Inventory(Result<Vec<PhysicalDisk>, String>);

    impl ExistingPartitionResizeInventory for Inventory {
        fn enumerate(&mut self) -> Result<Vec<PhysicalDisk>, String> {
            self.0.clone()
        }
    }

    struct Runner {
        calls: usize,
        result: ResizePartitionResult,
    }

    impl ExistingPartitionResizeRunner for Runner {
        fn run(&mut self, _request: &ExistingPartitionResizeRequest) -> ResizePartitionResult {
            self.calls += 1;
            self.result.clone()
        }
    }

    fn runner() -> Runner {
        Runner {
            calls: 0,
            result: ResizePartitionResult {
                success: true,
                message: "resized".into(),
                new_size_mb: 40 * 1024,
            },
        }
    }

    #[test]
    fn one_disk_is_auto_selected_without_inventing_a_layout() {
        let mut state = QuickPartitionDialogState::new(PartitionStyle::GPT, vec!['C'], 'C');
        state.apply_inventory(Ok(vec![disk(2, false, PartitionStyle::Unknown)]));
        assert_eq!(state.selected_disk_number, Some(2));
        assert_eq!(state.partition_style, PartitionStyle::GPT);
        assert!(state.planned.is_empty());
    }

    #[test]
    fn initialized_disk_keeps_its_style_and_mbr_removes_only_planned_esp() {
        let mut state = QuickPartitionDialogState::new(PartitionStyle::GPT, vec![], 'C');
        state.apply_inventory(Ok(vec![disk(1, true, PartitionStyle::MBR)]));
        assert_eq!(state.partition_style, PartitionStyle::MBR);
        state.set_partition_style(PartitionStyle::GPT);
        assert!(state.add_esp_partition());
        assert!(state.add_data_partition());
        state.set_partition_style(PartitionStyle::MBR);
        assert_eq!(state.planned.len(), 1);
        assert!(!state.planned[0].is_esp);
    }

    #[test]
    fn data_partition_restores_legacy_default_letter_size_label_and_file_system() {
        let mut state = QuickPartitionDialogState::new(PartitionStyle::GPT, vec!['C', 'D'], 'C');
        state.apply_inventory(Ok(vec![disk(3, false, PartitionStyle::Unknown)]));
        assert!(state.add_data_partition());
        let layout = &state.planned[0];
        assert_eq!(layout.size_gb, 100.0);
        assert_eq!(layout.drive_letter, Some('E'));
        assert_eq!(layout.label, "");
        assert_eq!(layout.file_system, "NTFS");
    }

    #[test]
    fn adding_second_partition_splits_the_last_new_data_partition_like_legacy_ui() {
        let mut state = QuickPartitionDialogState::new(PartitionStyle::GPT, vec![], 'C');
        state.apply_inventory(Ok(vec![disk(3, false, PartitionStyle::Unknown)]));
        assert!(state.add_data_partition());
        assert!(state.add_data_partition());
        assert_eq!(state.planned[0].size_gb, 80.0);
        assert_eq!(state.planned[1].size_gb, 20.0);
    }

    #[test]
    fn request_contains_only_new_rows_and_the_complete_disk_fingerprint() {
        let mut state = QuickPartitionDialogState::new(PartitionStyle::GPT, vec![], 'C');
        let mut selected = disk(4, true, PartitionStyle::GPT);
        selected.partitions.push(DiskPartitionInfo {
            partition_number: 1,
            size_bytes: 20 * GIB as u64,
            offset_bytes: 1024 * 1024,
            drive_letter: Some('D'),
            label: "Old".into(),
            file_system: "NTFS".into(),
            is_esp: false,
            is_msr: false,
            is_recovery: false,
            partition_type: "basic".into(),
            used_bytes: 5 * GIB as u64,
            free_bytes: 15 * GIB as u64,
            is_active: false,
        });
        state.apply_inventory(Ok(vec![selected]));
        assert!(state.add_data_partition());
        let request = state.quick_partition_request().unwrap();
        assert_eq!(request.disk.partitions.len(), 1);
        assert_eq!(request.layouts.len(), 1);
        assert_eq!(request.layouts[0].size_gb, 80.0);
    }

    #[test]
    fn existing_resize_is_typed_and_never_changes_editor_inventory() {
        let mut state = QuickPartitionDialogState::new(PartitionStyle::GPT, vec![], 'C');
        state.apply_inventory(Ok(vec![resizable_disk()]));
        state.select_row(Some(EditorRow::Existing(0)));
        state.resize_size_text = "40".into();
        let request = state.apply_resize_text().unwrap().unwrap();
        assert_eq!(request.disk.disk_number, 5);
        assert_eq!(request.partition_number, 7);
        assert_eq!(request.new_size_mb, 40 * 1024);
        assert_eq!(state.selected_disk().unwrap().partitions[0].size_gb(), 50.0);
    }

    #[test]
    fn fresh_matching_inventory_reaches_injected_runner_and_returns_typed_outcome() {
        let request = resize_request();
        let mut inventory = Inventory(Ok(vec![resizable_disk()]));
        let mut runner = runner();
        let outcome = execute_existing_partition_resize_with_backends(
            &request,
            'C',
            &mut inventory,
            &mut runner,
        )
        .unwrap();
        assert_eq!(runner.calls, 1);
        assert_eq!(outcome.new_size_mb, 40 * 1024);
        assert_eq!(outcome.message, "resized");
    }

    #[test]
    fn changed_disk_fingerprint_fails_before_injected_runner() {
        let request = resize_request();
        let mut changed = resizable_disk();
        changed.model = "different disk".into();
        let mut inventory = Inventory(Ok(vec![changed]));
        let mut runner = runner();
        assert_eq!(
            execute_existing_partition_resize_with_backends(
                &request,
                'C',
                &mut inventory,
                &mut runner,
            ),
            Err(ExistingPartitionResizeError::DiskChanged)
        );
        assert_eq!(runner.calls, 0);
    }

    #[test]
    fn changed_used_space_fails_partition_recheck_before_injected_runner() {
        let request = resize_request();
        let mut changed = resizable_disk();
        changed.partitions[0].used_bytes += 1024 * 1024;
        let mut inventory = Inventory(Ok(vec![changed]));
        let mut runner = runner();
        assert_eq!(
            execute_existing_partition_resize_with_backends(
                &request,
                'C',
                &mut inventory,
                &mut runner,
            ),
            Err(ExistingPartitionResizeError::PartitionChanged)
        );
        assert_eq!(runner.calls, 0);
    }

    #[test]
    fn stale_or_out_of_range_target_never_reaches_injected_runner() {
        let mut request = resize_request();
        request.new_size_mb = 101 * 1024;
        let mut inventory = Inventory(Ok(vec![resizable_disk()]));
        let mut runner = runner();
        assert!(matches!(
            execute_existing_partition_resize_with_backends(
                &request,
                'C',
                &mut inventory,
                &mut runner,
            ),
            Err(ExistingPartitionResizeError::InvalidRequest(_))
        ));
        assert_eq!(runner.calls, 0);
    }

    #[test]
    fn non_elevated_wrapper_denies_before_production_inventory_or_runner() {
        #[cfg(feature = "non-elevated-tests")]
        assert_eq!(
            execute_existing_partition_resize(&resize_request()),
            Err(ExistingPartitionResizeError::DevelopmentBuildDenied)
        );
    }

    #[test]
    fn system_esp_msr_recovery_and_letterless_existing_rows_cannot_resize() {
        let templates = [
            (true, false, false, Some('D')),
            (false, true, false, Some('D')),
            (false, false, true, Some('D')),
            (false, false, false, None),
            (false, false, false, Some('C')),
        ];
        for (is_esp, is_msr, is_recovery, drive_letter) in templates {
            let partition = DiskPartitionInfo {
                partition_number: 1,
                size_bytes: 10 * GIB as u64,
                offset_bytes: 0,
                drive_letter,
                label: String::new(),
                file_system: "NTFS".into(),
                is_esp,
                is_msr,
                is_recovery,
                partition_type: String::new(),
                used_bytes: GIB as u64,
                free_bytes: 9 * GIB as u64,
                is_active: false,
            };
            assert!(validate_existing_resize_target(&partition, 'C').is_err());
        }
    }
}
