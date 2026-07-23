//! Read-only hardware inspection used by the detailed native hardware tool.
//!
//! The collector deliberately keeps firmware parsing, CPUID decoding and device queries outside
//! the UI thread. Every parser is bounds checked because SMBIOS tables and device descriptors are
//! firmware/driver supplied input.

use std::mem::{size_of, zeroed};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::Storage::Nvme::{NVME_HEALTH_INFO_LOG, NVME_IDENTIFY_CONTROLLER_DATA};
use windows::Win32::System::Ioctl::{
    NVMeDataTypeIdentify, NVMeDataTypeLogPage, PropertyStandardQuery, ProtocolTypeNvme,
    StorageDeviceProtocolSpecificProperty, IOCTL_STORAGE_QUERY_PROPERTY, STORAGE_PROPERTY_ID,
    STORAGE_PROPERTY_QUERY, STORAGE_PROTOCOL_DATA_DESCRIPTOR,
};
use windows::Win32::System::SystemInformation::{GetSystemFirmwareTable, FIRMWARE_TABLE_PROVIDER};
use windows::Win32::System::IO::DeviceIoControl;

use super::hardware_info::{format_bytes, DiskInfo, HardwareInfo};

#[derive(Clone, Debug, Default)]
pub struct HardwareInspectorSnapshot {
    pub base: HardwareInfo,
    pub cpuid: CpuIdDetails,
    pub smbios: SmbiosSnapshot,
    pub graphics: Vec<GraphicsAdapterDetails>,
    pub disks: Vec<StorageDeviceDetails>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CpuIdDetails {
    pub vendor: String,
    pub brand: String,
    pub family: u32,
    pub model: u32,
    pub stepping: u32,
    pub features: Vec<String>,
    pub microarchitecture: String,
    pub process_node: String,
    pub l2_cache_bytes: u64,
    pub l3_cache_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SmbiosSnapshot {
    pub major_version: u8,
    pub minor_version: u8,
    pub bios_vendor: String,
    pub bios_version: String,
    pub bios_date: String,
    pub system_manufacturer: String,
    pub system_product: String,
    pub system_version: String,
    pub system_serial: String,
    pub board_manufacturer: String,
    pub board_product: String,
    pub board_version: String,
    pub board_serial: String,
    pub memory_modules: Vec<MemoryModuleDetails>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemoryModuleDetails {
    pub locator: String,
    pub bank: String,
    pub manufacturer: String,
    pub part_number: String,
    pub serial_number: String,
    pub memory_type: String,
    pub size_bytes: u64,
    pub speed_mts: u32,
    pub configured_speed_mts: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphicsAdapterDetails {
    pub name: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub subsystem_id: u32,
    pub revision: u32,
    pub dedicated_video_memory: u64,
    pub dedicated_system_memory: u64,
    pub shared_system_memory: u64,
    pub software_adapter: bool,
    pub architecture: String,
    pub process_node: String,
    pub core_configuration: String,
    adapter_luid: i64,
}

#[derive(Clone, Debug, Default)]
pub struct StorageDeviceDetails {
    pub disk: DiskInfo,
    pub trim_enabled: Option<bool>,
    pub incurs_seek_penalty: Option<bool>,
    pub nvme_health: Option<NvmeHealthDetails>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NvmeHealthDetails {
    pub health_percentage: u8,
    pub temperature_celsius: Option<i32>,
    pub data_read_bytes: u128,
    pub data_written_bytes: u128,
    pub power_cycles: u128,
    pub power_on_hours: u128,
    pub unsafe_shutdowns: u128,
    pub media_errors: u128,
    pub critical_warning: u8,
}

impl HardwareInspectorSnapshot {
    pub fn collect() -> Result<Self, String> {
        let base = HardwareInfo::collect().map_err(|error| error.to_string())?;
        let cpuid = collect_cpuid();
        let smbios = read_smbios()
            .ok()
            .and_then(|raw| parse_raw_smbios(&raw).ok())
            .unwrap_or_default();
        let graphics = collect_dxgi_adapters();
        let disks = base
            .disks
            .iter()
            .cloned()
            .map(|mut disk| {
                if let Some(identity) = query_nvme_identity(disk.disk_index) {
                    if !identity.model.is_empty() {
                        disk.model = identity.model;
                    }
                    if !identity.serial_number.is_empty() {
                        disk.serial_number = identity.serial_number;
                    }
                    if !identity.firmware_revision.is_empty() {
                        disk.firmware_revision = identity.firmware_revision;
                    }
                }
                StorageDeviceDetails {
                    trim_enabled: query_storage_boolean_property(disk.disk_index, 8),
                    incurs_seek_penalty: query_storage_boolean_property(disk.disk_index, 7),
                    nvme_health: query_nvme_health(disk.disk_index),
                    disk,
                }
            })
            .collect();
        Ok(Self {
            base,
            cpuid,
            smbios,
            graphics,
            disks,
        })
    }
}

impl StorageDeviceDetails {
    pub fn display_name(&self) -> String {
        let model = if self.disk.model.trim().is_empty() {
            "Unknown storage device"
        } else {
            self.disk.model.trim()
        };
        format!(
            "PhysicalDrive{} · {} · {}",
            self.disk.disk_index,
            model,
            format_bytes(self.disk.size)
        )
    }
}

#[cfg(target_arch = "x86_64")]
fn collect_cpuid() -> CpuIdDetails {
    use std::arch::x86_64::{__cpuid, __cpuid_count};

    let mut details = CpuIdDetails::default();
    // SAFETY: CPUID is available on every x86-64 processor.
    unsafe {
        let root = __cpuid(0);
        details.vendor = bytes_to_ascii(&[
            root.ebx.to_le_bytes(),
            root.edx.to_le_bytes(),
            root.ecx.to_le_bytes(),
        ]);
        if root.eax >= 1 {
            let leaf = __cpuid(1);
            let base_family = (leaf.eax >> 8) & 0x0f;
            let extended_family = (leaf.eax >> 20) & 0xff;
            details.family = if base_family == 0x0f {
                base_family + extended_family
            } else {
                base_family
            };
            let base_model = (leaf.eax >> 4) & 0x0f;
            let extended_model = (leaf.eax >> 16) & 0x0f;
            details.model = if matches!(base_family, 0x06 | 0x0f) {
                base_model | (extended_model << 4)
            } else {
                base_model
            };
            details.stepping = leaf.eax & 0x0f;
            push_feature(&mut details.features, leaf.edx, 23, "MMX");
            push_feature(&mut details.features, leaf.edx, 25, "SSE");
            push_feature(&mut details.features, leaf.edx, 26, "SSE2");
            push_feature(&mut details.features, leaf.ecx, 0, "SSE3");
            push_feature(&mut details.features, leaf.ecx, 9, "SSSE3");
            push_feature(&mut details.features, leaf.ecx, 19, "SSE4.1");
            push_feature(&mut details.features, leaf.ecx, 20, "SSE4.2");
            push_feature(&mut details.features, leaf.ecx, 25, "AES");
            push_feature(&mut details.features, leaf.ecx, 28, "AVX");
        }
        if root.eax >= 7 {
            let leaf = __cpuid_count(7, 0);
            push_feature(&mut details.features, leaf.ebx, 3, "BMI1");
            push_feature(&mut details.features, leaf.ebx, 5, "AVX2");
            push_feature(&mut details.features, leaf.ebx, 8, "BMI2");
            push_feature(&mut details.features, leaf.ebx, 16, "AVX-512F");
            push_feature(&mut details.features, leaf.ebx, 29, "SHA");
        }
        let extended = __cpuid(0x8000_0000).eax;
        if extended >= 0x8000_0004 {
            let mut brand = Vec::with_capacity(48);
            for leaf in 0x8000_0002..=0x8000_0004 {
                let value = __cpuid(leaf);
                brand.extend_from_slice(&value.eax.to_le_bytes());
                brand.extend_from_slice(&value.ebx.to_le_bytes());
                brand.extend_from_slice(&value.ecx.to_le_bytes());
                brand.extend_from_slice(&value.edx.to_le_bytes());
            }
            details.brand = String::from_utf8_lossy(&brand)
                .trim_matches(char::from(0))
                .trim()
                .to_owned();
        }
        if extended >= 0x8000_0001 {
            let leaf = __cpuid(0x8000_0001);
            push_feature(&mut details.features, leaf.ecx, 5, "ABM");
            push_feature(&mut details.features, leaf.edx, 20, "NX");
            push_feature(&mut details.features, leaf.edx, 29, "x86-64");
        }
        if extended >= 0x8000_0006 {
            let cache = __cpuid(0x8000_0006);
            details.l2_cache_bytes = u64::from((cache.ecx >> 16) & 0xffff) * 1024;
            details.l3_cache_bytes = u64::from((cache.edx >> 18) & 0x3fff) * 512 * 1024;
        }
    }
    let (architecture, process) = identify_cpu(
        &details.vendor,
        &details.brand,
        details.family,
        details.model,
    );
    details.microarchitecture = architecture.to_owned();
    details.process_node = process.to_owned();
    details
}

#[cfg(not(target_arch = "x86_64"))]
fn collect_cpuid() -> CpuIdDetails {
    CpuIdDetails::default()
}

fn push_feature(features: &mut Vec<String>, register: u32, bit: u32, name: &str) {
    if register & (1 << bit) != 0 {
        features.push(name.to_owned());
    }
}

fn bytes_to_ascii(words: &[[u8; 4]; 3]) -> String {
    let mut bytes = Vec::with_capacity(12);
    for word in words {
        bytes.extend_from_slice(word);
    }
    String::from_utf8_lossy(&bytes)
        .trim_matches(char::from(0))
        .trim()
        .to_owned()
}

fn read_smbios() -> Result<Vec<u8>, String> {
    let provider = FIRMWARE_TABLE_PROVIDER(u32::from_be_bytes(*b"RSMB"));
    // SAFETY: the first call requests the required byte count; the second writes into an owned
    // buffer of exactly that size.
    unsafe {
        let required = GetSystemFirmwareTable(provider, 0, None);
        if required < 8 {
            return Err("GetSystemFirmwareTable did not return an SMBIOS table".to_owned());
        }
        let mut raw = vec![0u8; required as usize];
        let written = GetSystemFirmwareTable(provider, 0, Some(&mut raw));
        if written == 0 || written as usize > raw.len() {
            return Err("GetSystemFirmwareTable failed while reading SMBIOS".to_owned());
        }
        raw.truncate(written as usize);
        Ok(raw)
    }
}

fn parse_raw_smbios(raw: &[u8]) -> Result<SmbiosSnapshot, &'static str> {
    if raw.len() < 8 {
        return Err("SMBIOS header is truncated");
    }
    let table_length = u32::from_le_bytes(raw[4..8].try_into().unwrap()) as usize;
    let table_end = 8usize
        .checked_add(table_length)
        .filter(|end| *end <= raw.len())
        .ok_or("SMBIOS table length is invalid")?;
    let mut result = SmbiosSnapshot {
        major_version: raw[1],
        minor_version: raw[2],
        ..Default::default()
    };
    let mut offset = 8usize;
    while offset + 4 <= table_end {
        let structure_type = raw[offset];
        let formatted_length = raw[offset + 1] as usize;
        if formatted_length < 4 || offset + formatted_length > table_end {
            return Err("SMBIOS structure is truncated");
        }
        let formatted = &raw[offset..offset + formatted_length];
        let strings_start = offset + formatted_length;
        let Some(strings_end) = find_double_nul(raw, strings_start, table_end) else {
            return Err("SMBIOS string table is unterminated");
        };
        let strings = &raw[strings_start..strings_end];
        match structure_type {
            0 => {
                result.bios_vendor = smbios_string(strings, byte_at(formatted, 4));
                result.bios_version = smbios_string(strings, byte_at(formatted, 5));
                result.bios_date = smbios_string(strings, byte_at(formatted, 8));
            }
            1 => {
                result.system_manufacturer = smbios_string(strings, byte_at(formatted, 4));
                result.system_product = smbios_string(strings, byte_at(formatted, 5));
                result.system_version = smbios_string(strings, byte_at(formatted, 6));
                result.system_serial = smbios_string(strings, byte_at(formatted, 7));
            }
            2 => {
                result.board_manufacturer = smbios_string(strings, byte_at(formatted, 4));
                result.board_product = smbios_string(strings, byte_at(formatted, 5));
                result.board_version = smbios_string(strings, byte_at(formatted, 6));
                result.board_serial = smbios_string(strings, byte_at(formatted, 7));
            }
            17 => {
                if let Some(module) = parse_memory_module(formatted, strings) {
                    result.memory_modules.push(module);
                }
            }
            127 => break,
            _ => {}
        }
        offset = strings_end + 2;
    }
    Ok(result)
}

fn parse_memory_module(formatted: &[u8], strings: &[u8]) -> Option<MemoryModuleDetails> {
    let size_field = read_u16(formatted, 12)?;
    if size_field == 0 {
        return None;
    }
    let size_bytes = match size_field {
        0xffff => 0,
        0x7fff => u64::from(read_u32(formatted, 28).unwrap_or(0)) * 1024 * 1024,
        size if size & 0x8000 != 0 => u64::from(size & 0x7fff) * 1024,
        size => u64::from(size) * 1024 * 1024,
    };
    Some(MemoryModuleDetails {
        locator: smbios_string(strings, byte_at(formatted, 16)),
        bank: smbios_string(strings, byte_at(formatted, 17)),
        manufacturer: smbios_string(strings, byte_at(formatted, 23)),
        serial_number: smbios_string(strings, byte_at(formatted, 24)),
        part_number: smbios_string(strings, byte_at(formatted, 26)),
        memory_type: memory_type_name(byte_at(formatted, 18)).to_owned(),
        size_bytes,
        speed_mts: u32::from(read_u16(formatted, 21).unwrap_or(0)),
        configured_speed_mts: u32::from(read_u16(formatted, 32).unwrap_or(0)),
    })
}

fn byte_at(bytes: &[u8], offset: usize) -> u8 {
    bytes.get(offset).copied().unwrap_or(0)
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn find_double_nul(bytes: &[u8], start: usize, end: usize) -> Option<usize> {
    if start >= end {
        return None;
    }
    (start..end.saturating_sub(1)).find(|index| bytes[*index] == 0 && bytes[*index + 1] == 0)
}

fn smbios_string(strings: &[u8], index: u8) -> String {
    if index == 0 {
        return String::new();
    }
    strings
        .split(|byte| *byte == 0)
        .nth(usize::from(index - 1))
        .map(|value| String::from_utf8_lossy(value).trim().to_owned())
        .unwrap_or_default()
}

fn memory_type_name(value: u8) -> &'static str {
    match value {
        0x0f => "SDRAM",
        0x12 => "DDR",
        0x13 => "DDR2",
        0x18 => "DDR3",
        0x1a => "DDR4",
        0x1b => "LPDDR",
        0x1c => "LPDDR2",
        0x1d => "LPDDR3",
        0x1e => "LPDDR4",
        0x22 => "DDR5",
        0x23 => "LPDDR5",
        _ => "Unknown",
    }
}

fn collect_dxgi_adapters() -> Vec<GraphicsAdapterDetails> {
    let mut adapters = Vec::new();
    // SAFETY: DXGI returns reference-counted interfaces and initialized descriptors.
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else {
            return adapters;
        };
        for index in 0..64 {
            let Ok(adapter) = factory.EnumAdapters1(index) else {
                break;
            };
            let Ok(desc) = adapter.GetDesc1() else {
                continue;
            };
            let name_end = desc
                .Description
                .iter()
                .position(|value| *value == 0)
                .unwrap_or(desc.Description.len());
            let name = String::from_utf16_lossy(&desc.Description[..name_end])
                .trim()
                .to_owned();
            let (architecture, process_node, core_configuration) =
                identify_gpu(desc.VendorId, desc.DeviceId, &name);
            let candidate = GraphicsAdapterDetails {
                name,
                vendor_id: desc.VendorId,
                device_id: desc.DeviceId,
                subsystem_id: desc.SubSysId,
                revision: desc.Revision,
                dedicated_video_memory: desc.DedicatedVideoMemory as u64,
                dedicated_system_memory: desc.DedicatedSystemMemory as u64,
                shared_system_memory: desc.SharedSystemMemory as u64,
                software_adapter: desc.Flags & 2 != 0,
                architecture: architecture.to_owned(),
                process_node: process_node.to_owned(),
                core_configuration: core_configuration.to_owned(),
                adapter_luid: (i64::from(desc.AdapterLuid.HighPart) << 32)
                    | i64::from(desc.AdapterLuid.LowPart),
            };
            if !candidate.software_adapter
                && !adapters.iter().any(|existing| {
                    existing.adapter_luid == candidate.adapter_luid
                        || (existing.vendor_id == candidate.vendor_id
                            && existing.device_id == candidate.device_id
                            && existing.subsystem_id == candidate.subsystem_id
                            && existing.name.eq_ignore_ascii_case(&candidate.name))
                })
            {
                adapters.push(candidate);
            }
        }
    }
    adapters
}

fn identify_cpu(
    vendor: &str,
    brand: &str,
    family: u32,
    model: u32,
) -> (&'static str, &'static str) {
    if vendor == "AuthenticAMD" && family == 25 && model == 116 {
        return ("Phoenix / Zen 4", "TSMC 4 nm");
    }
    if vendor == "AuthenticAMD" && brand.contains("Ryzen AI 9 HX 3") {
        return ("Strix Point / Zen 5", "TSMC 4 nm");
    }
    if vendor == "GenuineIntel" && brand.contains("Core(TM) Ultra 1") {
        return ("Meteor Lake", "Intel 4");
    }
    if vendor == "GenuineIntel" && brand.contains("Core(TM) Ultra 2") {
        return ("Intel Core Ultra 200 family", "多制程封装");
    }
    ("无法可靠识别", "无法可靠识别")
}

fn identify_gpu(
    vendor: u32,
    device: u32,
    name: &str,
) -> (&'static str, &'static str, &'static str) {
    if vendor == 0x10de && name.contains("RTX 4060") {
        return ("AD107 / Ada Lovelace", "TSMC 4N", "3072 CUDA cores");
    }
    if vendor == 0x10de && (0x2600..=0x28ff).contains(&device) {
        return ("Ada Lovelace", "TSMC 4N", "无法可靠识别");
    }
    if vendor == 0x1002 && name.contains("780M") {
        return ("Phoenix / RDNA 3", "TSMC 4 nm", "12 CU / 768 shaders");
    }
    if vendor == 0x1002 && name.contains("680M") {
        return ("Rembrandt / RDNA 2", "TSMC 6 nm", "12 CU / 768 shaders");
    }
    ("无法可靠识别", "无法可靠识别", "无法可靠识别")
}

fn query_storage_boolean_property(disk_index: u32, property_id: i32) -> Option<bool> {
    #[repr(C)]
    struct BooleanDescriptor {
        version: u32,
        size: u32,
        value: u8,
        reserved: [u8; 3],
    }

    let path: Vec<u16> = format!(r"\\.\PhysicalDrive{disk_index}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: the handle is read-only and every DeviceIoControl buffer is initialized and sized.
    unsafe {
        let handle = CreateFileW(
            PCWSTR(path.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            HANDLE::default(),
        )
        .ok()?;
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut query: STORAGE_PROPERTY_QUERY = zeroed();
        query.PropertyId = STORAGE_PROPERTY_ID(property_id);
        query.QueryType = PropertyStandardQuery;
        let mut descriptor = BooleanDescriptor {
            version: size_of::<BooleanDescriptor>() as u32,
            size: size_of::<BooleanDescriptor>() as u32,
            value: 0,
            reserved: [0; 3],
        };
        let mut returned = 0u32;
        let result = DeviceIoControl(
            handle,
            0x002d_1400,
            Some(&query as *const _ as *const _),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(&mut descriptor as *mut _ as *mut _),
            size_of::<BooleanDescriptor>() as u32,
            Some(&mut returned),
            None,
        );
        let _ = CloseHandle(handle);
        if result.is_ok() && returned >= 9 {
            Some(descriptor.value != 0)
        } else {
            None
        }
    }
}

fn query_nvme_health(disk_index: u32) -> Option<NvmeHealthDetails> {
    const QUERY_HEADER_SIZE: usize = 8;
    const PROTOCOL_DATA_SIZE: usize = 40;
    const HEALTH_LOG_ID: u32 = 2;
    const NVME_DATA_UNIT_BYTES: u128 = 512_000;

    let path: Vec<u16> = format!(r"\\.\PhysicalDrive{disk_index}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: the physical-drive handle is opened read-only. The protocol query and returned
    // descriptor use the exact offsets documented for STORAGE_PROPERTY_QUERY and
    // STORAGE_PROTOCOL_SPECIFIC_DATA, and every returned offset is range checked before reading.
    unsafe {
        let handle = CreateFileW(
            PCWSTR(path.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            HANDLE::default(),
        )
        .ok()?;
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }

        let mut buffer =
            vec![0u8; QUERY_HEADER_SIZE + PROTOCOL_DATA_SIZE + size_of::<NVME_HEALTH_INFO_LOG>()];
        write_u32(
            &mut buffer,
            0,
            StorageDeviceProtocolSpecificProperty.0 as u32,
        )?;
        write_u32(&mut buffer, 4, PropertyStandardQuery.0 as u32)?;
        write_u32(&mut buffer, 8, ProtocolTypeNvme.0 as u32)?;
        write_u32(&mut buffer, 12, NVMeDataTypeLogPage.0 as u32)?;
        write_u32(&mut buffer, 16, HEALTH_LOG_ID)?;
        // For NVMeDataTypeLogPage this field is the low 32 bits of the log-page byte offset.
        // The SMART/health page starts at offset zero; the storage stack issues the controller
        // request for the selected physical device.
        write_u32(&mut buffer, 20, 0)?;
        write_u32(&mut buffer, 24, PROTOCOL_DATA_SIZE as u32)?;
        write_u32(&mut buffer, 28, size_of::<NVME_HEALTH_INFO_LOG>() as u32)?;

        let input = buffer.clone();
        let mut returned = 0u32;
        let result = DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(input.as_ptr().cast()),
            input.len() as u32,
            Some(buffer.as_mut_ptr().cast()),
            buffer.len() as u32,
            Some(&mut returned),
            None,
        );
        let _ = CloseHandle(handle);
        if result.is_err() || returned < size_of::<STORAGE_PROTOCOL_DATA_DESCRIPTOR>() as u32 {
            return None;
        }

        let protocol_offset = QUERY_HEADER_SIZE;
        if read_u32_at(&buffer, protocol_offset)? != ProtocolTypeNvme.0 as u32 {
            return None;
        }
        let data_offset = read_u32_at(&buffer, protocol_offset + 16)? as usize;
        let data_length = read_u32_at(&buffer, protocol_offset + 20)? as usize;
        if data_length < size_of::<NVME_HEALTH_INFO_LOG>() {
            return None;
        }
        let log_offset = protocol_offset.checked_add(data_offset)?;
        let log_end = log_offset.checked_add(size_of::<NVME_HEALTH_INFO_LOG>())?;
        let log_bytes = buffer.get(log_offset..log_end)?;
        let log = std::ptr::read_unaligned(log_bytes.as_ptr().cast::<NVME_HEALTH_INFO_LOG>());

        let kelvin = u16::from_le_bytes(log.Temperature);
        let temperature_celsius = (kelvin != 0).then(|| i32::from(kelvin) - 273);
        Some(NvmeHealthDetails {
            health_percentage: 100u8.saturating_sub(log.PercentageUsed.min(100)),
            temperature_celsius,
            data_read_bytes: nvme_counter(log.DataUnitRead).saturating_mul(NVME_DATA_UNIT_BYTES),
            data_written_bytes: nvme_counter(log.DataUnitWritten)
                .saturating_mul(NVME_DATA_UNIT_BYTES),
            power_cycles: nvme_counter(log.PowerCycle),
            power_on_hours: nvme_counter(log.PowerOnHours),
            unsafe_shutdowns: nvme_counter(log.UnsafeShutdowns),
            media_errors: nvme_counter(log.MediaErrors),
            critical_warning: log.CriticalWarning.AsUchar,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct NvmeIdentityDetails {
    model: String,
    serial_number: String,
    firmware_revision: String,
}

fn query_nvme_identity(disk_index: u32) -> Option<NvmeIdentityDetails> {
    const QUERY_HEADER_SIZE: usize = 8;
    const PROTOCOL_DATA_SIZE: usize = 40;
    const IDENTIFY_CONTROLLER: u32 = 1;

    let path: Vec<u16> = format!(r"\\.\PhysicalDrive{disk_index}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: identical read-only protocol-query boundary to query_nvme_health. Returned offsets
    // and lengths are validated before the fixed-size identify payload is read.
    unsafe {
        let handle = CreateFileW(
            PCWSTR(path.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            HANDLE::default(),
        )
        .ok()?;
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }

        let mut buffer =
            vec![
                0u8;
                QUERY_HEADER_SIZE + PROTOCOL_DATA_SIZE + size_of::<NVME_IDENTIFY_CONTROLLER_DATA>()
            ];
        write_u32(
            &mut buffer,
            0,
            StorageDeviceProtocolSpecificProperty.0 as u32,
        )?;
        write_u32(&mut buffer, 4, PropertyStandardQuery.0 as u32)?;
        write_u32(&mut buffer, 8, ProtocolTypeNvme.0 as u32)?;
        write_u32(&mut buffer, 12, NVMeDataTypeIdentify.0 as u32)?;
        write_u32(&mut buffer, 16, IDENTIFY_CONTROLLER)?;
        write_u32(&mut buffer, 20, 0)?;
        write_u32(&mut buffer, 24, PROTOCOL_DATA_SIZE as u32)?;
        write_u32(
            &mut buffer,
            28,
            size_of::<NVME_IDENTIFY_CONTROLLER_DATA>() as u32,
        )?;

        let input = buffer.clone();
        let mut returned = 0u32;
        let result = DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(input.as_ptr().cast()),
            input.len() as u32,
            Some(buffer.as_mut_ptr().cast()),
            buffer.len() as u32,
            Some(&mut returned),
            None,
        );
        let _ = CloseHandle(handle);
        if result.is_err() || returned < size_of::<STORAGE_PROTOCOL_DATA_DESCRIPTOR>() as u32 {
            return None;
        }
        let protocol_offset = QUERY_HEADER_SIZE;
        let data_offset = read_u32_at(&buffer, protocol_offset + 16)? as usize;
        let data_length = read_u32_at(&buffer, protocol_offset + 20)? as usize;
        if data_length < size_of::<NVME_IDENTIFY_CONTROLLER_DATA>() {
            return None;
        }
        let identity_offset = protocol_offset.checked_add(data_offset)?;
        let identity_end =
            identity_offset.checked_add(size_of::<NVME_IDENTIFY_CONTROLLER_DATA>())?;
        let identity_bytes = buffer.get(identity_offset..identity_end)?;
        let identity = std::ptr::read_unaligned(
            identity_bytes
                .as_ptr()
                .cast::<NVME_IDENTIFY_CONTROLLER_DATA>(),
        );
        Some(NvmeIdentityDetails {
            model: fixed_ascii(&identity.MN),
            serial_number: fixed_ascii(&identity.SN),
            firmware_revision: fixed_ascii(&identity.FR),
        })
    }
}

fn fixed_ascii(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_matches(char::from(0))
        .trim()
        .to_owned()
}

fn write_u32(buffer: &mut [u8], offset: usize, value: u32) -> Option<()> {
    buffer
        .get_mut(offset..offset.checked_add(4)?)?
        .copy_from_slice(&value.to_le_bytes());
    Some(())
}

fn read_u32_at(buffer: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        buffer
            .get(offset..offset.checked_add(4)?)?
            .try_into()
            .ok()?,
    ))
}

fn nvme_counter(value: [u8; 16]) -> u128 {
    u128::from_le_bytes(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpuid_vendor_word_order_matches_architecture_contract() {
        assert_eq!(
            bytes_to_ascii(&[
                u32::from_le_bytes(*b"Genu").to_le_bytes(),
                u32::from_le_bytes(*b"ineI").to_le_bytes(),
                u32::from_le_bytes(*b"ntel").to_le_bytes(),
            ]),
            "GenuineIntel"
        );
    }

    #[test]
    fn raw_smbios_provider_signature_matches_windows_multichar_constant() {
        assert_eq!(u32::from_be_bytes(*b"RSMB"), 0x5253_4d42);
    }

    #[test]
    fn known_current_hardware_is_mapped_conservatively() {
        assert_eq!(
            identify_cpu("AuthenticAMD", "AMD Ryzen 9 7940H", 25, 116),
            ("Phoenix / Zen 4", "TSMC 4 nm")
        );
        assert_eq!(
            identify_gpu(0x10de, 0x28e0, "NVIDIA GeForce RTX 4060 Laptop GPU"),
            ("AD107 / Ada Lovelace", "TSMC 4N", "3072 CUDA cores")
        );
        assert_eq!(
            identify_gpu(0x1002, 0x15bf, "AMD Radeon 780M Graphics"),
            ("Phoenix / RDNA 3", "TSMC 4 nm", "12 CU / 768 shaders")
        );
    }

    #[test]
    fn nvme_counters_preserve_all_128_bits() {
        let value = 0xfeed_face_cafe_beef_0123_4567_89ab_cdefu128;
        assert_eq!(nvme_counter(value.to_le_bytes()), value);
    }

    #[test]
    #[ignore = "reads the host firmware and physical storage inventory; run only on a disposable test machine"]
    fn live_snapshot_contains_real_hardware_inventory() {
        let snapshot =
            HardwareInspectorSnapshot::collect().expect("collect live hardware snapshot");
        eprintln!("{snapshot:#?}");
        assert!(!snapshot.cpuid.brand.is_empty());
        assert!(!snapshot.graphics.is_empty());
        assert!(!snapshot.disks.is_empty());
    }

    #[test]
    fn malformed_smbios_length_is_rejected() {
        let raw = [0u8, 3, 6, 0, 32, 0, 0, 0, 127, 4, 0, 0, 0, 0];
        assert!(parse_raw_smbios(&raw).is_err());
    }

    #[test]
    fn parses_memory_device_without_reading_past_formatted_area() {
        let mut table = vec![17, 34, 0, 0];
        table.resize(34, 0);
        table[12..14].copy_from_slice(&8192u16.to_le_bytes());
        table[16] = 1;
        table[17] = 2;
        table[18] = 0x1a;
        table[21..23].copy_from_slice(&3200u16.to_le_bytes());
        table[23] = 3;
        table[24] = 4;
        table[26] = 5;
        table[32..34].copy_from_slice(&2933u16.to_le_bytes());
        table.extend_from_slice(b"DIMM_A1\0BANK 0\0Kingston\0SERIAL\0PART\0\0");
        table.extend_from_slice(&[127, 4, 0, 0, 0, 0]);
        let mut raw = vec![0, 3, 6, 0];
        raw.extend_from_slice(&(table.len() as u32).to_le_bytes());
        raw.extend_from_slice(&table);

        let parsed = parse_raw_smbios(&raw).unwrap();
        assert_eq!(parsed.memory_modules.len(), 1);
        let module = &parsed.memory_modules[0];
        assert_eq!(module.locator, "DIMM_A1");
        assert_eq!(module.memory_type, "DDR4");
        assert_eq!(module.size_bytes, 8 * 1024 * 1024 * 1024);
        assert_eq!(module.configured_speed_mts, 2933);
    }
}
