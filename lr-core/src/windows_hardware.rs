//! Shared, read-only machine identity detection for compatibility policy.
//!
//! Arbitrary CPUID leaves are exposed by the compiler intrinsic, not by a Win32 API. Firmware
//! identity and present-device hardware IDs are independently collected through the documented
//! `GetSystemFirmwareTable` and SetupAPI boundaries. Callers must keep an unknown result fail-safe:
//! it is never equivalent to confirmed physical hardware.

use anyhow::{bail, Context, Result};

const MAX_SMBIOS_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuIdRegisters {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcessorIdentity {
    pub vendor: String,
    pub family: u32,
    pub model: u32,
    pub stepping: u32,
    pub hypervisor_present: bool,
    pub hypervisor_vendor: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FirmwareIdentity {
    pub system_manufacturer: String,
    pub system_product: String,
    pub system_version: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MachineEnvironment {
    Physical,
    Vmware,
    HyperV,
    VirtualBox,
    QemuKvm,
    Xen,
    Parallels,
    OtherHypervisor,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineIdentity {
    pub processor: Option<ProcessorIdentity>,
    pub firmware: Option<FirmwareIdentity>,
    pub present_hardware_ids: Option<Vec<String>>,
    pub environment: MachineEnvironment,
    pub diagnostics: Vec<String>,
}

/// Executes one CPUID leaf through the architecture intrinsic.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub fn cpuid(leaf: u32, subleaf: u32) -> Option<CpuIdRegisters> {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::__cpuid_count;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::__cpuid_count;

    // SAFETY: CPUID is available on supported Windows x86/x64 processors. The intrinsic does not
    // dereference pointers or mutate memory; unsupported leaves return architectural defaults.
    let value = unsafe { __cpuid_count(leaf, subleaf) };
    Some(CpuIdRegisters {
        eax: value.eax,
        ebx: value.ebx,
        ecx: value.ecx,
        edx: value.edx,
    })
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
pub fn cpuid(_leaf: u32, _subleaf: u32) -> Option<CpuIdRegisters> {
    None
}

pub fn read_processor_identity() -> Option<ProcessorIdentity> {
    let root = cpuid(0, 0)?;
    if root.eax < 1 {
        return None;
    }
    let leaf1 = cpuid(1, 0)?;
    let mut identity = decode_processor_identity(root, leaf1);
    if identity.hypervisor_present {
        if let Some(hypervisor) = cpuid(0x4000_0000, 0) {
            identity.hypervisor_vendor =
                ascii_words([hypervisor.ebx, hypervisor.ecx, hypervisor.edx]);
        }
    }
    Some(identity)
}

fn decode_processor_identity(root: CpuIdRegisters, leaf1: CpuIdRegisters) -> ProcessorIdentity {
    let base_family = (leaf1.eax >> 8) & 0x0f;
    let extended_family = (leaf1.eax >> 20) & 0xff;
    let family = if base_family == 0x0f {
        base_family + extended_family
    } else {
        base_family
    };
    let base_model = (leaf1.eax >> 4) & 0x0f;
    let extended_model = (leaf1.eax >> 16) & 0x0f;
    let model = if matches!(base_family, 0x06 | 0x0f) {
        base_model | (extended_model << 4)
    } else {
        base_model
    };
    ProcessorIdentity {
        vendor: ascii_words([root.ebx, root.edx, root.ecx]),
        family,
        model,
        stepping: leaf1.eax & 0x0f,
        hypervisor_present: leaf1.ecx & (1 << 31) != 0,
        hypervisor_vendor: String::new(),
    }
}

fn ascii_words(words: [u32; 3]) -> String {
    let mut bytes = Vec::with_capacity(12);
    for word in words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    String::from_utf8_lossy(&bytes)
        .trim_matches(char::from(0))
        .trim()
        .to_owned()
}

#[cfg(windows)]
pub fn read_firmware_identity() -> Result<FirmwareIdentity> {
    use windows::Win32::System::SystemInformation::{
        GetSystemFirmwareTable, FIRMWARE_TABLE_PROVIDER,
    };

    let provider = FIRMWARE_TABLE_PROVIDER(u32::from_be_bytes(*b"RSMB"));
    // SAFETY: the first call obtains the required size. The second receives an owned mutable
    // buffer of that exact size, and both the returned length and SMBIOS structure are bounded.
    let raw = unsafe {
        let required = GetSystemFirmwareTable(provider, 0, None) as usize;
        if !(8..=MAX_SMBIOS_BYTES).contains(&required) {
            bail!("GetSystemFirmwareTable returned invalid SMBIOS size {required}");
        }
        let mut raw = vec![0u8; required];
        let written = GetSystemFirmwareTable(provider, 0, Some(&mut raw)) as usize;
        if written < 8 || written > raw.len() {
            bail!("GetSystemFirmwareTable failed while reading SMBIOS ({written}/{required})");
        }
        raw.truncate(written);
        raw
    };
    parse_raw_smbios_identity(&raw).context("parse SMBIOS system identity")
}

#[cfg(not(windows))]
pub fn read_firmware_identity() -> Result<FirmwareIdentity> {
    bail!("GetSystemFirmwareTable is unavailable on this platform")
}

fn parse_raw_smbios_identity(raw: &[u8]) -> Result<FirmwareIdentity> {
    if raw.len() < 8 {
        bail!("SMBIOS header is truncated");
    }
    let table_length = u32::from_le_bytes(raw[4..8].try_into().unwrap()) as usize;
    let table_end = 8usize
        .checked_add(table_length)
        .filter(|end| *end <= raw.len())
        .context("SMBIOS table length is invalid")?;
    let mut offset = 8usize;
    while offset + 4 <= table_end {
        let structure_type = raw[offset];
        let structure_length = raw[offset + 1] as usize;
        if structure_length < 4 || offset + structure_length > table_end {
            bail!("SMBIOS structure is truncated");
        }
        let strings_start = offset + structure_length;
        let strings_end = find_double_nul(raw, strings_start, table_end)
            .context("SMBIOS string table is unterminated")?;
        if structure_type == 1 {
            let formatted = &raw[offset..offset + structure_length];
            return Ok(FirmwareIdentity {
                system_manufacturer: smbios_string(
                    raw,
                    strings_start,
                    strings_end,
                    formatted.get(4).copied().unwrap_or(0),
                ),
                system_product: smbios_string(
                    raw,
                    strings_start,
                    strings_end,
                    formatted.get(5).copied().unwrap_or(0),
                ),
                system_version: smbios_string(
                    raw,
                    strings_start,
                    strings_end,
                    formatted.get(6).copied().unwrap_or(0),
                ),
            });
        }
        offset = strings_end + 2;
        if structure_type == 127 {
            break;
        }
    }
    bail!("SMBIOS System Information (type 1) is missing")
}

fn find_double_nul(raw: &[u8], start: usize, end: usize) -> Option<usize> {
    if start >= end {
        return None;
    }
    (start..end.saturating_sub(1)).find(|index| raw[*index] == 0 && raw[*index + 1] == 0)
}

fn smbios_string(raw: &[u8], start: usize, end: usize, index: u8) -> String {
    if index == 0 || start >= end {
        return String::new();
    }
    let mut current = 1u8;
    let mut cursor = start;
    while cursor < end {
        let next = raw[cursor..end]
            .iter()
            .position(|byte| *byte == 0)
            .map(|relative| cursor + relative)
            .unwrap_or(end);
        if current == index {
            return String::from_utf8_lossy(&raw[cursor..next])
                .trim()
                .to_owned();
        }
        current = current.saturating_add(1);
        cursor = next.saturating_add(1);
    }
    String::new()
}

pub fn classify_machine_environment(
    processor: Option<&ProcessorIdentity>,
    firmware: Option<&FirmwareIdentity>,
    present_hardware_ids: Option<&[String]>,
) -> MachineEnvironment {
    let cpu_vendor = processor
        .map(|cpu| cpu.hypervisor_vendor.to_ascii_lowercase())
        .unwrap_or_default();
    let firmware_text = firmware
        .map(|value| {
            format!(
                "{} {} {}",
                value.system_manufacturer, value.system_product, value.system_version
            )
            .to_ascii_lowercase()
        })
        .unwrap_or_default();
    let ids = present_hardware_ids
        .map(|values| values.join(" ").to_ascii_lowercase())
        .unwrap_or_default();

    if cpu_vendor.contains("vmware") || firmware_text.contains("vmware") || ids.contains("ven_15ad")
    {
        return MachineEnvironment::Vmware;
    }
    if cpu_vendor.contains("microsoft hv")
        || (firmware_text.contains("microsoft corporation")
            && firmware_text.contains("virtual machine"))
    {
        return MachineEnvironment::HyperV;
    }
    if cpu_vendor.contains("vbox")
        || firmware_text.contains("virtualbox")
        || firmware_text.contains("innotek")
        || ids.contains("ven_80ee")
    {
        return MachineEnvironment::VirtualBox;
    }
    if cpu_vendor.contains("kvm")
        || firmware_text.contains("qemu")
        || firmware_text.contains("kvm")
        || ids.contains("ven_1af4")
    {
        return MachineEnvironment::QemuKvm;
    }
    if cpu_vendor.contains("xen") || firmware_text.contains("xen") {
        return MachineEnvironment::Xen;
    }
    if firmware_text.contains("parallels") {
        return MachineEnvironment::Parallels;
    }
    if processor.is_some_and(|cpu| cpu.hypervisor_present) {
        return MachineEnvironment::OtherHypervisor;
    }

    // Confirming physical hardware is compatibility-sensitive. Require every probe to succeed so
    // a hidden hypervisor identity or failed SetupAPI scan never becomes affirmative evidence.
    if processor.is_some()
        && firmware.is_some()
        && present_hardware_ids.is_some()
        && processor.is_some_and(|cpu| !cpu.hypervisor_present)
    {
        MachineEnvironment::Physical
    } else {
        MachineEnvironment::Unknown
    }
}

pub fn collect_machine_identity() -> MachineIdentity {
    let processor = read_processor_identity();
    let mut diagnostics = Vec::new();
    if processor.is_none() {
        diagnostics.push("CPUID is unavailable on this architecture".to_owned());
    }
    let firmware = match read_firmware_identity() {
        Ok(value) => Some(value),
        Err(error) => {
            diagnostics.push(format!("SMBIOS probe failed: {error:#}"));
            None
        }
    };
    let present_hardware_ids = match crate::driver::list_present_hardware_ids() {
        Ok(values) => Some(values),
        Err(error) => {
            diagnostics.push(format!("SetupAPI device enumeration failed: {error:#}"));
            None
        }
    };
    let environment = classify_machine_environment(
        processor.as_ref(),
        firmware.as_ref(),
        present_hardware_ids.as_deref(),
    );
    MachineIdentity {
        processor,
        firmware,
        present_hardware_ids,
        environment,
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intel_tenth_generation() -> ProcessorIdentity {
        ProcessorIdentity {
            vendor: "GenuineIntel".to_owned(),
            family: 6,
            model: 0xa5,
            stepping: 2,
            hypervisor_present: false,
            hypervisor_vendor: String::new(),
        }
    }

    #[test]
    fn decodes_extended_model_and_hypervisor_bit() {
        let root = CpuIdRegisters {
            ebx: u32::from_le_bytes(*b"Genu"),
            edx: u32::from_le_bytes(*b"ineI"),
            ecx: u32::from_le_bytes(*b"ntel"),
            ..Default::default()
        };
        let identity = decode_processor_identity(
            root,
            CpuIdRegisters {
                eax: 0x000a_0652,
                ecx: 1 << 31,
                ..Default::default()
            },
        );
        assert_eq!(identity.vendor, "GenuineIntel");
        assert_eq!(identity.family, 6);
        assert_eq!(identity.model, 0xa5);
        assert_eq!(identity.stepping, 2);
        assert!(identity.hypervisor_present);
    }

    #[test]
    fn vmware_wins_over_guest_intel_model() {
        let mut cpu = intel_tenth_generation();
        cpu.hypervisor_present = true;
        cpu.hypervisor_vendor = "VMwareVMware".to_owned();
        let firmware = FirmwareIdentity {
            system_manufacturer: "VMware, Inc.".to_owned(),
            system_product: "VMware Virtual Platform".to_owned(),
            ..Default::default()
        };
        assert_eq!(
            classify_machine_environment(Some(&cpu), Some(&firmware), Some(&[])),
            MachineEnvironment::Vmware
        );
    }

    #[test]
    fn vmware_pci_id_still_wins_when_cpuid_is_masked() {
        let cpu = intel_tenth_generation();
        let firmware = FirmwareIdentity {
            system_manufacturer: "Unknown".to_owned(),
            system_product: "Unknown".to_owned(),
            ..Default::default()
        };
        let ids = vec!["PCI\\VEN_15AD&DEV_07C0".to_owned()];
        assert_eq!(
            classify_machine_environment(Some(&cpu), Some(&firmware), Some(&ids)),
            MachineEnvironment::Vmware
        );
    }

    #[test]
    fn failed_probe_is_not_called_physical() {
        let cpu = intel_tenth_generation();
        let firmware = FirmwareIdentity {
            system_manufacturer: "ASUSTeK COMPUTER INC.".to_owned(),
            system_product: "TUF Gaming".to_owned(),
            ..Default::default()
        };
        assert_eq!(
            classify_machine_environment(Some(&cpu), Some(&firmware), None),
            MachineEnvironment::Unknown
        );
    }

    #[test]
    fn complete_negative_evidence_can_confirm_physical_machine() {
        let cpu = intel_tenth_generation();
        let firmware = FirmwareIdentity {
            system_manufacturer: "ASUSTeK COMPUTER INC.".to_owned(),
            system_product: "TUF Gaming".to_owned(),
            ..Default::default()
        };
        assert_eq!(
            classify_machine_environment(Some(&cpu), Some(&firmware), Some(&[])),
            MachineEnvironment::Physical
        );
    }

    #[test]
    fn parses_smbios_type_one_strings_with_bounds() {
        let strings = b"VMware, Inc.\0VMware Virtual Platform\0None\0\0";
        let mut table = vec![1, 8, 0, 0, 1, 2, 3, 0];
        table.extend_from_slice(strings);
        table.extend_from_slice(&[127, 4, 0, 0, 0, 0]);
        let mut raw = vec![0, 3, 2, 0];
        raw.extend_from_slice(&(table.len() as u32).to_le_bytes());
        raw.extend_from_slice(&table);
        let parsed = parse_raw_smbios_identity(&raw).unwrap();
        assert_eq!(parsed.system_manufacturer, "VMware, Inc.");
        assert_eq!(parsed.system_product, "VMware Virtual Platform");
        assert_eq!(parsed.system_version, "None");
    }

    #[test]
    #[ignore = "reads live CPUID, SMBIOS and SetupAPI inventory; run explicitly on a test host"]
    fn live_probe_returns_consistent_environment() {
        let identity = collect_machine_identity();
        assert!(identity.processor.is_some());
        assert!(identity.firmware.is_some());
        assert!(identity.present_hardware_ids.is_some());
        assert_ne!(identity.environment, MachineEnvironment::Unknown);
        assert!(identity.diagnostics.is_empty());
    }
}
