//! Typed, fail-closed planning boundary for the native quick-partition tool.
//!
//! The UI captures an immutable disk fingerprint together with a typed partition
//! table and layouts. Production execution enumerates the disk again and refuses
//! to call the legacy DiskPart boundary if any identity or layout field changed.

use std::collections::HashSet;

use super::disk::PartitionStyle;
use super::quick_partition::{
    DiskPartitionInfo, PartitionLayout, PhysicalDisk, QuickPartitionResult,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskPartitionFingerprint {
    pub partition_number: u32,
    pub offset_bytes: u64,
    pub size_bytes: u64,
    pub drive_letter: Option<char>,
    pub partition_type: String,
    pub is_esp: bool,
    pub is_msr: bool,
    pub is_recovery: bool,
}

impl From<&DiskPartitionInfo> for DiskPartitionFingerprint {
    fn from(partition: &DiskPartitionInfo) -> Self {
        Self {
            partition_number: partition.partition_number,
            offset_bytes: partition.offset_bytes,
            size_bytes: partition.size_bytes,
            drive_letter: partition
                .drive_letter
                .map(|letter| letter.to_ascii_uppercase()),
            partition_type: partition.partition_type.trim().to_ascii_uppercase(),
            is_esp: partition.is_esp,
            is_msr: partition.is_msr,
            is_recovery: partition.is_recovery,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskFingerprint {
    pub disk_number: u32,
    pub model: String,
    pub size_bytes: u64,
    pub partition_style: PartitionStyle,
    pub partitions: Vec<DiskPartitionFingerprint>,
}

impl From<&PhysicalDisk> for DiskFingerprint {
    fn from(disk: &PhysicalDisk) -> Self {
        let mut partitions = disk
            .partitions
            .iter()
            .map(DiskPartitionFingerprint::from)
            .collect::<Vec<_>>();
        partitions.sort_by_key(|partition| (partition.offset_bytes, partition.partition_number));
        Self {
            disk_number: disk.disk_number,
            model: normalize_model(&disk.model),
            size_bytes: disk.size_bytes,
            partition_style: disk.partition_style,
            partitions,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct QuickPartitionRequest {
    pub disk: DiskFingerprint,
    pub partition_style: PartitionStyle,
    pub layouts: Vec<PartitionLayout>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuickPartitionError {
    DevelopmentBuildDenied,
    InvalidPlan(String),
    Inventory(String),
    DiskMissing(u32),
    DiskChanged,
    UnsafeDisk(String),
    Execution(String),
}

impl std::fmt::Display for QuickPartitionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DevelopmentBuildDenied => formatter
                .write_str("quick partition is disabled in non-elevated development builds"),
            Self::InvalidPlan(detail)
            | Self::Inventory(detail)
            | Self::UnsafeDisk(detail)
            | Self::Execution(detail) => formatter.write_str(detail),
            Self::DiskMissing(number) => {
                write!(formatter, "disk {number} is no longer present")
            }
            Self::DiskChanged => formatter.write_str(
                "the selected disk identity or partition layout changed; refresh and review again",
            ),
        }
    }
}

impl std::error::Error for QuickPartitionError {}

pub fn validate_request(request: &QuickPartitionRequest) -> Result<(), QuickPartitionError> {
    if request.disk.size_bytes == 0 {
        return Err(invalid("selected disk reports zero capacity"));
    }
    if !matches!(
        request.partition_style,
        PartitionStyle::GPT | PartitionStyle::MBR
    ) {
        return Err(invalid("partition table must be GPT or MBR"));
    }
    if request.layouts.is_empty() || request.layouts.len() > 128 {
        return Err(invalid(
            "at least one and at most 128 partitions are required",
        ));
    }

    let mut letters = HashSet::new();
    let mut planned_bytes = 0_u64;
    for (index, layout) in request.layouts.iter().enumerate() {
        let last = index + 1 == request.layouts.len();
        if !layout.size_gb.is_finite() || layout.size_gb < 0.0 || (!last && layout.size_gb == 0.0) {
            return Err(invalid("partition sizes must be finite and positive; only the final partition may use remaining space"));
        }
        if layout.size_gb > 0.0 {
            let bytes = layout.size_gb * 1024.0 * 1024.0 * 1024.0;
            if bytes > u64::MAX as f64 {
                return Err(invalid("partition size is too large"));
            }
            planned_bytes = planned_bytes.saturating_add(bytes.round() as u64);
        }
        if planned_bytes > request.disk.size_bytes.saturating_add(1024 * 1024) {
            return Err(invalid(
                "planned partitions exceed the selected disk capacity",
            ));
        }
        if layout.label.contains(['"', '\r', '\n']) {
            return Err(invalid(
                "volume labels may not contain quotes or line breaks",
            ));
        }
        let fs = layout.file_system.trim();
        if !["NTFS", "FAT32", "EXFAT"]
            .iter()
            .any(|allowed| fs.eq_ignore_ascii_case(allowed))
        {
            return Err(invalid("file system must be NTFS, FAT32, or exFAT"));
        }
        if let Some(letter) = layout.drive_letter {
            let letter = letter.to_ascii_uppercase();
            if !letter.is_ascii_alphabetic() || !letters.insert(letter) {
                return Err(invalid("drive letters must be unique ASCII letters"));
            }
        }
        if layout.is_esp {
            if request.partition_style != PartitionStyle::GPT {
                return Err(invalid("ESP partitions require GPT"));
            }
            if !fs.eq_ignore_ascii_case("FAT32") || layout.drive_letter.is_some() {
                return Err(invalid(
                    "ESP must use FAT32 and must not assign a drive letter",
                ));
            }
        }
    }
    if request
        .layouts
        .iter()
        .filter(|layout| layout.is_esp)
        .count()
        > 1
    {
        return Err(invalid("only one ESP partition may be planned"));
    }
    Ok(())
}

pub fn verify_current_disk<'a>(
    request: &QuickPartitionRequest,
    current: &'a [PhysicalDisk],
) -> Result<&'a PhysicalDisk, QuickPartitionError> {
    validate_request(request)?;
    let disk = current
        .iter()
        .find(|disk| disk.disk_number == request.disk.disk_number)
        .ok_or(QuickPartitionError::DiskMissing(request.disk.disk_number))?;
    if DiskFingerprint::from(disk) != request.disk {
        return Err(QuickPartitionError::DiskChanged);
    }
    Ok(disk)
}

pub trait QuickPartitionInventory {
    fn enumerate(&mut self) -> Result<Vec<PhysicalDisk>, String>;
}

pub trait QuickPartitionRunner {
    fn run(
        &mut self,
        disk_number: u32,
        style: PartitionStyle,
        layouts: &[PartitionLayout],
    ) -> Result<QuickPartitionResult, String>;
}

/// The injectable boundary used by tests and by the production wrapper after its
/// development-build guard. The inventory is always consulted before the runner.
pub(crate) fn execute_with_backends(
    request: &QuickPartitionRequest,
    inventory: &mut dyn QuickPartitionInventory,
    runner: &mut dyn QuickPartitionRunner,
) -> Result<QuickPartitionResult, QuickPartitionError> {
    validate_request(request)?;
    let disks = inventory
        .enumerate()
        .map_err(QuickPartitionError::Inventory)?;
    let disk = verify_current_disk(request, &disks)?;
    let (safe, reason) = super::quick_partition::can_safely_partition(disk);
    if !safe {
        return Err(QuickPartitionError::UnsafeDisk(reason));
    }
    runner
        .run(
            request.disk.disk_number,
            request.partition_style,
            &request.layouts,
        )
        .map_err(QuickPartitionError::Execution)
}

pub fn execute(
    request: &QuickPartitionRequest,
) -> Result<QuickPartitionResult, QuickPartitionError> {
    #[cfg(feature = "non-elevated-tests")]
    {
        let _ = request;
        Err(QuickPartitionError::DevelopmentBuildDenied)
    }
    #[cfg(not(feature = "non-elevated-tests"))]
    {
        struct Inventory;
        impl QuickPartitionInventory for Inventory {
            fn enumerate(&mut self) -> Result<Vec<PhysicalDisk>, String> {
                Ok(super::quick_partition::get_physical_disks())
            }
        }
        struct Runner;
        impl QuickPartitionRunner for Runner {
            fn run(
                &mut self,
                disk_number: u32,
                style: PartitionStyle,
                layouts: &[PartitionLayout],
            ) -> Result<QuickPartitionResult, String> {
                Ok(super::quick_partition::execute_quick_partition_validated(
                    disk_number,
                    style,
                    layouts,
                ))
            }
        }
        execute_with_backends(request, &mut Inventory, &mut Runner)
    }
}

pub fn default_layouts(style: PartitionStyle, disk_size_bytes: u64) -> Vec<PartitionLayout> {
    let mut layouts = Vec::new();
    if style == PartitionStyle::GPT {
        layouts.push(PartitionLayout {
            size_gb: 0.5,
            drive_letter: None,
            label: "EFI".into(),
            is_esp: true,
            file_system: "FAT32".into(),
        });
    }
    let reserved = if style == PartitionStyle::GPT {
        0.5
    } else {
        0.0
    };
    let size_gb = disk_size_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
    layouts.push(PartitionLayout {
        size_gb: (size_gb - reserved).max(0.0),
        drive_letter: None,
        label: "Data".into(),
        is_esp: false,
        file_system: "NTFS".into(),
    });
    layouts
}

/// One editable row per line: `size-GB-or-* | drive-letter-or-* | label | FS | ESP-or-DATA`.
pub fn format_layouts(layouts: &[PartitionLayout]) -> String {
    layouts
        .iter()
        .enumerate()
        .map(|(index, layout)| {
            let size = if index + 1 == layouts.len() && layout.size_gb == 0.0 {
                "*".into()
            } else {
                format!("{:.1}", layout.size_gb)
            };
            format!(
                "{} | {} | {} | {} | {}",
                size,
                layout
                    .drive_letter
                    .map(|letter| letter.to_string())
                    .unwrap_or_else(|| "*".into()),
                layout.label,
                layout.file_system,
                if layout.is_esp { "ESP" } else { "DATA" }
            )
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}

pub fn parse_layouts(value: &str) -> Result<Vec<PartitionLayout>, QuickPartitionError> {
    value
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            let fields = line.split('|').map(str::trim).collect::<Vec<_>>();
            if fields.len() != 5 {
                return Err(invalid(format!(
                    "layout row {} must contain five fields",
                    index + 1
                )));
            }
            let size_gb = if fields[0] == "*"
                || fields[0].eq_ignore_ascii_case("remaining")
                || fields[0] == "剩余"
            {
                0.0
            } else {
                fields[0]
                    .parse::<f64>()
                    .map_err(|_| invalid(format!("layout row {} has an invalid size", index + 1)))?
            };
            let drive_letter = if fields[1].is_empty() || fields[1] == "*" || fields[1] == "-" {
                None
            } else {
                let mut chars = fields[1].chars();
                let letter = chars
                    .next()
                    .ok_or_else(|| invalid("missing drive letter"))?;
                if chars.next().is_some() {
                    return Err(invalid(format!(
                        "layout row {} has an invalid drive letter",
                        index + 1
                    )));
                }
                Some(letter.to_ascii_uppercase())
            };
            Ok(PartitionLayout {
                size_gb,
                drive_letter,
                label: fields[2].to_string(),
                file_system: fields[3].to_string(),
                is_esp: fields[4].eq_ignore_ascii_case("ESP"),
            })
        })
        .collect()
}

fn normalize_model(model: &str) -> String {
    model
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn invalid(detail: impl Into<String>) -> QuickPartitionError {
    QuickPartitionError::InvalidPlan(detail.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk() -> PhysicalDisk {
        PhysicalDisk {
            disk_number: 3,
            size_bytes: 128 * 1024 * 1024 * 1024,
            model: " Example  SSD ".into(),
            partition_style: PartitionStyle::GPT,
            is_initialized: true,
            partitions: vec![DiskPartitionInfo {
                partition_number: 1,
                size_bytes: 1024 * 1024 * 1024,
                offset_bytes: 1024 * 1024,
                drive_letter: Some('D'),
                label: "data".into(),
                file_system: "NTFS".into(),
                is_esp: false,
                is_msr: false,
                is_recovery: false,
                partition_type: "basic".into(),
                used_bytes: 0,
                free_bytes: 0,
                is_active: false,
            }],
            unallocated_bytes: 0,
        }
    }

    fn request() -> QuickPartitionRequest {
        let disk = disk();
        QuickPartitionRequest {
            disk: DiskFingerprint::from(&disk),
            partition_style: PartitionStyle::GPT,
            layouts: default_layouts(PartitionStyle::GPT, disk.size_bytes),
        }
    }

    #[test]
    fn disk_number_model_capacity_and_layout_are_all_part_of_the_fingerprint() {
        let request = request();
        for mutate in 0..4 {
            let mut current = disk();
            match mutate {
                0 => current.disk_number += 1,
                1 => current.model = "different".into(),
                2 => current.size_bytes += 4096,
                _ => current.partitions[0].offset_bytes += 4096,
            }
            assert!(verify_current_disk(&request, &[current]).is_err());
        }
    }

    #[test]
    fn layout_editor_round_trips_size_letter_label_file_system_and_esp() {
        let layouts = vec![
            PartitionLayout {
                size_gb: 0.5,
                drive_letter: None,
                label: "EFI".into(),
                is_esp: true,
                file_system: "FAT32".into(),
            },
            PartitionLayout {
                size_gb: 0.0,
                drive_letter: Some('D'),
                label: "Work".into(),
                is_esp: false,
                file_system: "NTFS".into(),
            },
        ];
        assert_eq!(parse_layouts(&format_layouts(&layouts)).unwrap(), layouts);
    }

    #[test]
    fn unsafe_labels_filesystems_duplicate_letters_and_oversized_plans_are_rejected() {
        let mut candidate = request();
        candidate.layouts[1].label = "bad\"label".into();
        assert!(validate_request(&candidate).is_err());
        candidate = request();
        candidate.layouts[1].file_system = "NTFS & clean".into();
        assert!(validate_request(&candidate).is_err());
        candidate = request();
        candidate.layouts.push(PartitionLayout {
            size_gb: 1.0,
            drive_letter: Some('D'),
            label: "x".into(),
            is_esp: false,
            file_system: "NTFS".into(),
        });
        candidate.layouts[1].drive_letter = Some('D');
        assert!(validate_request(&candidate).is_err());
        candidate = request();
        candidate.layouts[1].size_gb = 500.0;
        assert!(validate_request(&candidate).is_err());
    }

    #[test]
    fn non_elevated_build_refuses_before_inventory_or_runner_io() {
        #[cfg(feature = "non-elevated-tests")]
        assert!(matches!(
            execute(&request()),
            Err(QuickPartitionError::DevelopmentBuildDenied)
        ));
    }

    #[test]
    fn changed_inventory_fails_closed_before_the_runner() {
        struct Inventory(Vec<PhysicalDisk>);
        impl QuickPartitionInventory for Inventory {
            fn enumerate(&mut self) -> Result<Vec<PhysicalDisk>, String> {
                Ok(self.0.clone())
            }
        }
        struct Runner(usize);
        impl QuickPartitionRunner for Runner {
            fn run(
                &mut self,
                _disk_number: u32,
                _style: PartitionStyle,
                _layouts: &[PartitionLayout],
            ) -> Result<QuickPartitionResult, String> {
                self.0 += 1;
                panic!("runner must not be called for a changed disk")
            }
        }

        let request = request();
        let mut changed = disk();
        changed.partitions[0].size_bytes += 4096;
        let mut inventory = Inventory(vec![changed]);
        let mut runner = Runner(0);
        assert!(matches!(
            execute_with_backends(&request, &mut inventory, &mut runner),
            Err(QuickPartitionError::DiskChanged)
        ));
        assert_eq!(runner.0, 0);
    }
}
