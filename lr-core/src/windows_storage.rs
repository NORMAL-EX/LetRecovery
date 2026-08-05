//! Shared, documented Win32 storage-management boundary.
//!
//! Configuration operations use Virtual Disk Service (VDS) COM interfaces and
//! documented disk IOCTLs. Callers remain responsible for presenting a
//! destructive-operation confirmation and for comparing a fresh disk/partition
//! fingerprint immediately before calling this module. Every operation returns
//! the original HRESULT context and callers must re-enumerate afterward.

use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiskStyle {
    Mbr,
    Gpt,
}

/// Physical storage bus classification used for fail-closed install defaults.
///
/// Only an explicit `BusTypeNvme` result is classified as NVMe. RAID/VMD,
/// virtual and failed queries are deliberately not guessed to be NVMe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiskBusType {
    Nvme,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileSystem {
    Ntfs,
    Fat,
    Fat32,
    ExFat,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormatOptions {
    pub file_system: FileSystem,
    pub label: String,
    /// Zero lets Windows choose the default allocation-unit size.
    pub allocation_unit_size: u32,
    pub quick: bool,
}

impl FileSystem {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ntfs => "NTFS",
            Self::Fat => "FAT",
            Self::Fat32 => "FAT32",
            Self::ExFat => "EXFAT",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PartitionKind {
    BasicData,
    EfiSystem,
    MicrosoftReserved,
    Recovery,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GptPartitionMetadata {
    /// In-memory GUID bytes as returned by `PARTITION_INFORMATION_GPT`.
    pub partition_id: [u8; 16],
    pub attributes: u64,
    pub name: [u16; 36],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatePartitionRequest {
    pub disk_number: u32,
    /// Zero selects the first aligned free extent.
    pub offset_bytes: u64,
    /// Zero consumes the selected free extent.
    pub size_bytes: u64,
    pub kind: PartitionKind,
    pub file_system: Option<FileSystem>,
    pub label: String,
    pub drive_letter: Option<char>,
    pub active: bool,
    /// Used only when recreating an existing GPT partition after an offline block move.
    /// Ordinary partition creation must leave this as `None`.
    pub preserve_gpt_metadata: Option<GptPartitionMetadata>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CreatedPartition {
    pub offset_bytes: u64,
    pub size_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VolumeIdentity {
    pub disk_number: u32,
    pub offset_bytes: u64,
    pub extent_length_bytes: u64,
}

fn same_physical_partition(left: VolumeIdentity, right: VolumeIdentity) -> bool {
    left.disk_number == right.disk_number && left.offset_bytes == right.offset_bytes
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PartitionRecord {
    pub partition_number: u32,
    pub offset_bytes: u64,
    pub size_bytes: u64,
    pub kind: PartitionKind,
    pub active: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageError {
    operation: &'static str,
    detail: String,
}

impl StorageError {
    fn new(operation: &'static str, detail: impl Into<String>) -> Self {
        Self {
            operation,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.detail)
    }
}

impl std::error::Error for StorageError {}

pub fn validate_create_request(request: &CreatePartitionRequest) -> Result<(), StorageError> {
    if request.size_bytes != 0 && request.size_bytes < 1024 * 1024 {
        return Err(StorageError::new(
            "validate partition",
            "partition size must be zero (remaining space) or at least 1 MiB",
        ));
    }
    if request.offset_bytes != 0 && !request.offset_bytes.is_multiple_of(1024 * 1024) {
        return Err(StorageError::new(
            "validate partition",
            "explicit partition offset must be 1 MiB aligned",
        ));
    }
    if request.label.encode_utf16().count() > 32 || request.label.contains(['\0', '\r', '\n']) {
        return Err(StorageError::new(
            "validate partition",
            "volume label is empty-invalid, too long, or contains a control character",
        ));
    }
    if let Some(letter) = request.drive_letter {
        let letter = letter.to_ascii_uppercase();
        if !('C'..='Z').contains(&letter) {
            return Err(StorageError::new(
                "validate partition",
                "drive letter must be in the C-Z range",
            ));
        }
    }
    match request.kind {
        PartitionKind::EfiSystem => {
            if request.file_system != Some(FileSystem::Fat32)
                || request.drive_letter.is_some()
                || request.active
            {
                return Err(StorageError::new(
                    "validate partition",
                    "EFI system partitions require FAT32, no drive letter, and no MBR active flag",
                ));
            }
        }
        PartitionKind::MicrosoftReserved => {
            if request.file_system.is_some() || request.drive_letter.is_some() || request.active {
                return Err(StorageError::new(
                    "validate partition",
                    "Microsoft reserved partitions cannot be formatted, mounted, or active",
                ));
            }
        }
        PartitionKind::Recovery => {
            if request.active {
                return Err(StorageError::new(
                    "validate partition",
                    "recovery partitions cannot be marked active",
                ));
            }
        }
        PartitionKind::BasicData => {}
    }
    Ok(())
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::ffi::c_void;
    use std::mem::{size_of, ManuallyDrop};

    use windows::core::{IUnknown, Interface, GUID, HRESULT, PCWSTR, PWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, BOOLEAN, ERROR_NO_MORE_FILES, E_UNEXPECTED, HANDLE, RPC_E_CHANGED_MODE,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, DefineDosDeviceW, FindFirstVolumeW, FindNextVolumeW, FindVolumeClose,
        GetLogicalDrives, QueryDosDeviceW, DDD_EXACT_MATCH_ON_REMOVE, DDD_RAW_TARGET_PATH,
        DDD_REMOVE_DEFINITION, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::Storage::VirtualDiskService::{
        CLSID_VdsLoader, IEnumVdsObject, IVdsAdvancedDisk, IVdsAsync, IVdsCreatePartitionEx,
        IVdsDisk, IVdsDiskPartitionMF, IVdsPack, IVdsService, IVdsServiceLoader, IVdsSwProvider,
        IVdsVolume, IVdsVolumeMF, IVdsVolumeMF2, IVdsVolumeShrink, CHANGE_ATTRIBUTES_PARAMETERS,
        CHANGE_ATTRIBUTES_PARAMETERS_0, CHANGE_ATTRIBUTES_PARAMETERS_0_1,
        CREATE_PARTITION_PARAMETERS, CREATE_PARTITION_PARAMETERS_0,
        CREATE_PARTITION_PARAMETERS_0_0, CREATE_PARTITION_PARAMETERS_0_1, VDS_ASYNCOUT_CLEAN,
        VDS_ASYNCOUT_CREATEPARTITION, VDS_ASYNCOUT_EXTENDVOLUME, VDS_ASYNCOUT_FORMAT,
        VDS_ASYNCOUT_SHRINKVOLUME, VDS_ASYNC_OUTPUT, VDS_DET_FREE, VDS_DISK_EXTENT, VDS_DISK_PROP,
        VDS_DRIVE_LETTER_PROP, VDS_FST_EXFAT, VDS_FST_FAT, VDS_FST_FAT32, VDS_FST_NTFS,
        VDS_INPUT_DISK, VDS_OT_VOLUME, VDS_PARTITION_STYLE, VDS_PST_GPT, VDS_PST_MBR,
        VDS_QUERY_SOFTWARE_PROVIDERS,
    };
    use windows::Win32::System::Com::{
        CoCreateGuid, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
        CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED,
    };
    use windows::Win32::System::SystemInformation::GetWindowsDirectoryW;

    const ALIGNMENT: u64 = 1024 * 1024;
    const GPT_BASIC_DATA: GUID = GUID::from_u128(0xebd0a0a2_b9e5_4433_87c0_68b6b72699c7);
    const GPT_ESP: GUID = GUID::from_u128(0xc12a7328_f81f_11d2_ba4b_00a0c93ec93b);
    const GPT_MSR: GUID = GUID::from_u128(0xe3c9e316_0b5c_4db8_817d_f92df00215ae);
    const GPT_RECOVERY: GUID = GUID::from_u128(0xde94bba4_06d1_4d40_a16a_bfd50179d6ac);

    struct ComApartment {
        uninitialize: bool,
    }

    struct VolumeSearchHandle(HANDLE);

    impl Drop for VolumeSearchHandle {
        fn drop(&mut self) {
            unsafe {
                let _ = FindVolumeClose(self.0);
            }
        }
    }

    impl ComApartment {
        unsafe fn enter() -> Result<Self, StorageError> {
            let result = CoInitializeEx(None, COINIT_MULTITHREADED);
            if result.is_ok() {
                return Ok(Self { uninitialize: true });
            }
            if result == RPC_E_CHANGED_MODE {
                return Ok(Self {
                    uninitialize: false,
                });
            }
            Err(hresult_error("initialize COM", result))
        }
    }

    impl Drop for ComApartment {
        fn drop(&mut self) {
            if self.uninitialize {
                unsafe { CoUninitialize() };
            }
        }
    }

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    struct Vds {
        _apartment: ComApartment,
        service: IVdsService,
    }

    struct DiskObject {
        disk: IVdsDisk,
        id: GUID,
        style: VDS_PARTITION_STYLE,
    }

    impl Vds {
        unsafe fn connect() -> Result<Self, StorageError> {
            let apartment = ComApartment::enter()?;
            let loader: IVdsServiceLoader =
                CoCreateInstance(&CLSID_VdsLoader, None, CLSCTX_LOCAL_SERVER)
                    .map_err(|error| api_error("create VDS loader", error))?;
            let service = loader
                .LoadService(PCWSTR::null())
                .map_err(|error| api_error("load VDS service", error))?;
            service
                .WaitForServiceReady()
                .map_err(|error| api_error("wait for VDS service", error))?;
            Ok(Self {
                _apartment: apartment,
                service,
            })
        }

        unsafe fn refresh(&self) -> Result<(), StorageError> {
            self.service
                .Reenumerate()
                .map_err(|error| api_error("re-enumerate VDS", error))?;
            self.service
                .Refresh()
                .map_err(|error| api_error("refresh VDS", error))
        }

        unsafe fn providers(&self) -> Result<Vec<IVdsSwProvider>, StorageError> {
            let enumerator = self
                .service
                .QueryProviders(VDS_QUERY_SOFTWARE_PROVIDERS.0 as u32)
                .map_err(|error| api_error("enumerate VDS providers", error))?;
            enum_objects(&enumerator)?
                .into_iter()
                .map(|object| {
                    object
                        .cast::<IVdsSwProvider>()
                        .map_err(|error| api_error("open VDS software provider", error))
                })
                .collect()
        }

        unsafe fn packs(&self) -> Result<Vec<IVdsPack>, StorageError> {
            let mut result = Vec::new();
            for provider in self.providers()? {
                let enumerator = provider
                    .QueryPacks()
                    .map_err(|error| api_error("enumerate VDS packs", error))?;
                for object in enum_objects(&enumerator)? {
                    result.push(
                        object
                            .cast::<IVdsPack>()
                            .map_err(|error| api_error("open VDS pack", error))?,
                    );
                }
            }
            Ok(result)
        }

        unsafe fn find_disk(&self, disk_number: u32) -> Result<DiskObject, StorageError> {
            for pack in self.packs()? {
                let enumerator = pack
                    .QueryDisks()
                    .map_err(|error| api_error("enumerate VDS disks", error))?;
                for object in enum_objects(&enumerator)? {
                    let disk = object
                        .cast::<IVdsDisk>()
                        .map_err(|error| api_error("open VDS disk", error))?;
                    let mut properties = VDS_DISK_PROP::default();
                    disk.GetProperties(&mut properties)
                        .map_err(|error| api_error("read VDS disk properties", error))?;
                    let number = disk_number_from_properties(&properties);
                    free_disk_properties(&mut properties);
                    if number == Some(disk_number) {
                        return Ok(DiskObject {
                            disk,
                            id: properties.id,
                            style: properties.PartitionStyle,
                        });
                    }
                }
            }
            Err(StorageError::new(
                "find disk",
                format!("physical disk {disk_number} was not found by VDS"),
            ))
        }

        unsafe fn find_volume_by_letter(
            &self,
            drive_letter: char,
        ) -> Result<IVdsVolume, StorageError> {
            let letter = normalize_letter(drive_letter)?;
            let mut properties = [VDS_DRIVE_LETTER_PROP::default(); 26];
            self.service
                .QueryDriveLetters('A' as u16, &mut properties)
                .map_err(|error| api_error("query VDS drive letters", error))?;
            let property = properties
                .iter()
                .find(|property| property.bUsed.as_bool() && property.wcLetter == letter as u16)
                .ok_or_else(|| {
                    StorageError::new(
                        "find volume",
                        format!("drive letter {letter}: is not assigned to a VDS volume"),
                    )
                })?;
            let object = self
                .service
                .GetObject(property.volumeId, VDS_OT_VOLUME)
                .map_err(|error| api_error("open VDS volume", error))?;
            object
                .cast::<IVdsVolume>()
                .map_err(|error| api_error("query VDS volume interface", error))
        }

        unsafe fn find_volume_by_id(&self, volume_id: GUID) -> Result<IVdsVolume, StorageError> {
            if volume_id == GUID::zeroed() {
                return Err(StorageError::new(
                    "find volume",
                    "the provider did not associate a volume with the created partition",
                ));
            }
            self.service
                .GetObject(volume_id, VDS_OT_VOLUME)
                .map_err(|error| api_error("open created VDS volume", error))?
                .cast::<IVdsVolume>()
                .map_err(|error| api_error("query created VDS volume interface", error))
        }
    }

    unsafe fn enum_objects(enumerator: &IEnumVdsObject) -> Result<Vec<IUnknown>, StorageError> {
        let mut result = Vec::new();
        loop {
            let mut values: [Option<IUnknown>; 1] = [None];
            let mut fetched = 0;
            enumerator
                .Next(&mut values, &mut fetched)
                .map_err(|error| api_error("enumerate VDS object", error))?;
            if fetched == 0 {
                break;
            }
            let value = values[0]
                .take()
                .ok_or_else(|| StorageError::new("enumerate VDS object", "null object"))?;
            result.push(value);
        }
        Ok(result)
    }

    unsafe fn wait_async(
        operation: &'static str,
        asynchronous: &IVdsAsync,
        expected_type: Option<windows::Win32::Storage::VirtualDiskService::VDS_ASYNC_OUTPUT_TYPE>,
    ) -> Result<VDS_ASYNC_OUTPUT, StorageError> {
        let mut result = HRESULT(0);
        let mut output = VDS_ASYNC_OUTPUT::default();
        asynchronous
            .Wait(&mut result, &mut output)
            .map_err(|error| api_error(operation, error))?;
        result.ok().map_err(|_| hresult_error(operation, result))?;
        if let Some(expected) = expected_type {
            if output.r#type != expected {
                return Err(hresult_error(operation, E_UNEXPECTED));
            }
        }
        Ok(output)
    }

    unsafe fn disk_number_from_properties(properties: &VDS_DISK_PROP) -> Option<u32> {
        let candidates = [
            copy_pwstr(properties.pwszName),
            copy_pwstr(properties.pwszDevicePath),
        ];
        candidates
            .into_iter()
            .flatten()
            .find_map(|value| physical_drive_number(&value))
    }

    fn physical_drive_number(value: &str) -> Option<u32> {
        let value = value.trim_end_matches('\0');
        let index = value.to_ascii_lowercase().rfind("physicaldrive")?;
        value[index + "physicaldrive".len()..].parse().ok()
    }

    unsafe fn copy_pwstr(value: PWSTR) -> Option<String> {
        if value.is_null() {
            None
        } else {
            value.to_string().ok()
        }
    }

    unsafe fn free_pwstr(value: &mut PWSTR) {
        if !value.is_null() {
            CoTaskMemFree(Some(value.0.cast::<c_void>()));
            *value = PWSTR::null();
        }
    }

    unsafe fn free_disk_properties(properties: &mut VDS_DISK_PROP) {
        free_pwstr(&mut properties.pwszDiskAddress);
        free_pwstr(&mut properties.pwszName);
        free_pwstr(&mut properties.pwszFriendlyName);
        free_pwstr(&mut properties.pwszAdaptorName);
        free_pwstr(&mut properties.pwszDevicePath);
    }

    fn normalize_letter(letter: char) -> Result<char, StorageError> {
        let letter = letter.to_ascii_uppercase();
        if ('C'..='Z').contains(&letter) {
            Ok(letter)
        } else {
            Err(StorageError::new(
                "validate drive letter",
                "drive letter must be in the C-Z range",
            ))
        }
    }

    pub fn current_windows_drive_letter() -> Result<char, StorageError> {
        let mut buffer = [0_u16; 32_768];
        let length = unsafe { GetWindowsDirectoryW(Some(&mut buffer)) } as usize;
        if length == 0 || length >= buffer.len() {
            return Err(StorageError::new(
                "locate running Windows volume",
                "GetWindowsDirectoryW returned an invalid path length",
            ));
        }
        let path = String::from_utf16(&buffer[..length]).map_err(|error| {
            StorageError::new(
                "locate running Windows volume",
                format!("GetWindowsDirectoryW returned invalid UTF-16: {error}"),
            )
        })?;
        let mut characters = path.chars();
        let letter = characters.next().ok_or_else(|| {
            StorageError::new(
                "locate running Windows volume",
                "Windows directory path is empty",
            )
        })?;
        if characters.next() != Some(':') {
            return Err(StorageError::new(
                "locate running Windows volume",
                format!("Windows directory is not drive-letter based: {path}"),
            ));
        }
        normalize_letter(letter)
    }

    pub fn assigned_drive_letter_mask() -> Result<u32, StorageError> {
        let mask = unsafe { GetLogicalDrives() };
        if mask == 0 {
            return Err(StorageError::new(
                "enumerate assigned drive letters",
                windows::core::Error::from_win32().to_string(),
            ));
        }
        Ok(mask)
    }

    unsafe fn volume_identity_from_device_path(
        device_path: &str,
    ) -> Result<VolumeIdentity, StorageError> {
        use windows::Win32::Storage::FileSystem::IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS;
        use windows::Win32::System::Ioctl::VOLUME_DISK_EXTENTS;
        use windows::Win32::System::IO::DeviceIoControl;

        let path = wide(device_path);
        let handle = CreateFileW(
            PCWSTR(path.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            Default::default(),
            None,
        )
        .map(OwnedHandle)
        .map_err(|error| api_error("open volume identity", error))?;
        let mut storage = vec![0_u64; 128];
        let mut returned = 0_u32;
        DeviceIoControl(
            handle.0,
            IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
            None,
            0,
            Some(storage.as_mut_ptr().cast()),
            (storage.len() * size_of::<u64>()) as u32,
            Some(&mut returned),
            None,
        )
        .map_err(|error| api_error("query volume disk extents", error))?;
        if returned < size_of::<VOLUME_DISK_EXTENTS>() as u32 {
            return Err(StorageError::new(
                "query volume disk extents",
                "response is shorter than VOLUME_DISK_EXTENTS",
            ));
        }
        let extents = &*storage.as_ptr().cast::<VOLUME_DISK_EXTENTS>();
        if extents.NumberOfDiskExtents != 1 {
            return Err(StorageError::new(
                "query volume disk extents",
                format!(
                    "expected one basic-disk extent, received {}",
                    extents.NumberOfDiskExtents
                ),
            ));
        }
        let extent = extents.Extents[0];
        let offset_bytes = u64::try_from(extent.StartingOffset).map_err(|_| {
            StorageError::new("query volume disk extents", "volume offset is negative")
        })?;
        let extent_length_bytes = u64::try_from(extent.ExtentLength).map_err(|_| {
            StorageError::new(
                "query volume disk extents",
                "volume extent length is negative",
            )
        })?;
        Ok(VolumeIdentity {
            disk_number: extent.DiskNumber,
            offset_bytes,
            extent_length_bytes,
        })
    }

    pub unsafe fn volume_identity(drive_letter: char) -> Result<VolumeIdentity, StorageError> {
        let letter = normalize_letter(drive_letter)?;
        volume_identity_from_device_path(&format!(r"\\.\{letter}:"))
    }

    fn volume_name_from_buffer(buffer: &[u16]) -> Result<String, StorageError> {
        let end = buffer.iter().position(|value| *value == 0).ok_or_else(|| {
            StorageError::new(
                "enumerate volume GUID paths",
                "FindFirstVolumeW/FindNextVolumeW returned an unterminated path",
            )
        })?;
        let volume_name = String::from_utf16(&buffer[..end]).map_err(|error| {
            StorageError::new(
                "enumerate volume GUID paths",
                format!("volume GUID path is not valid UTF-16: {error}"),
            )
        })?;
        if !volume_name.starts_with(r"\\?\Volume{") || !volume_name.ends_with(r"}\") {
            return Err(StorageError::new(
                "enumerate volume GUID paths",
                format!("unexpected volume path returned by Windows: {volume_name}"),
            ));
        }
        Ok(volume_name)
    }

    /// Resolve an exact physical partition to its existing volume GUID root without assigning a
    /// drive letter. Microsoft documents volume GUID paths as directly usable absolute roots; the
    /// trailing slash is removed only while opening the volume for the extent identity IOCTL.
    pub unsafe fn volume_guid_path_for_partition(
        disk_number: u32,
        offset_bytes: u64,
    ) -> Result<String, StorageError> {
        let expected = VolumeIdentity {
            disk_number,
            offset_bytes,
            extent_length_bytes: 0,
        };
        let mut buffer = vec![0_u16; 1_024];
        let search = FindFirstVolumeW(&mut buffer)
            .map(VolumeSearchHandle)
            .map_err(|error| api_error("begin volume GUID enumeration", error))?;

        loop {
            let volume_name = volume_name_from_buffer(&buffer)?;
            let device_path = volume_name.trim_end_matches('\\');
            if let Ok(actual) = volume_identity_from_device_path(device_path) {
                if same_physical_partition(actual, expected) {
                    return Ok(volume_name);
                }
            }

            buffer.fill(0);
            match FindNextVolumeW(search.0, &mut buffer) {
                Ok(()) => {}
                Err(error) if error.code() == HRESULT::from_win32(ERROR_NO_MORE_FILES.0) => break,
                Err(error) => {
                    return Err(api_error("continue volume GUID enumeration", error));
                }
            }
        }

        Err(StorageError::new(
            "resolve partition volume GUID path",
            format!("no volume maps to disk {disk_number} offset {offset_bytes}"),
        ))
    }

    pub unsafe fn mbr_signature(disk_number: u32) -> Result<Option<u32>, StorageError> {
        let (_, storage, returned) = read_drive_layout(disk_number, false)?;
        if returned < 48 {
            return Err(StorageError::new(
                "read MBR signature",
                "drive layout response is shorter than its fixed header",
            ));
        }
        let layout = &*storage
            .as_ptr()
            .cast::<windows::Win32::System::Ioctl::DRIVE_LAYOUT_INFORMATION_EX>();
        use windows::Win32::System::Ioctl::PARTITION_STYLE_MBR;
        if layout.PartitionStyle != PARTITION_STYLE_MBR.0 as u32 {
            return Ok(None);
        }
        Ok(Some(layout.Anonymous.Mbr.Signature))
    }

    pub unsafe fn disk_style(disk_number: u32) -> Result<DiskStyle, StorageError> {
        let vds = Vds::connect()?;
        let disk = vds.find_disk(disk_number)?;
        if disk.style == VDS_PST_MBR {
            Ok(DiskStyle::Mbr)
        } else if disk.style == VDS_PST_GPT {
            Ok(DiskStyle::Gpt)
        } else {
            Err(StorageError::new(
                "query disk style",
                "disk is RAW or uses an unsupported partition style",
            ))
        }
    }

    pub unsafe fn partitions(disk_number: u32) -> Result<Vec<PartitionRecord>, StorageError> {
        use windows::Win32::Storage::VirtualDiskService::VDS_PARTITION_PROP;

        let vds = Vds::connect()?;
        let disk = vds.find_disk(disk_number)?;
        let advanced = disk
            .disk
            .cast::<IVdsAdvancedDisk>()
            .map_err(|error| api_error("open VDS advanced disk", error))?;
        let mut pointer = std::ptr::null_mut::<VDS_PARTITION_PROP>();
        let mut count = 0_i32;
        advanced
            .QueryPartitions(&mut pointer, &mut count)
            .map_err(|error| api_error("query VDS partitions", error))?;
        if count < 0 || (count > 0 && pointer.is_null()) {
            return Err(StorageError::new(
                "query VDS partitions",
                "provider returned an invalid partition array",
            ));
        }
        let properties = if count == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(pointer, count as usize).to_vec()
        };
        CoTaskMemFree((!pointer.is_null()).then_some(pointer.cast::<c_void>()));
        properties
            .into_iter()
            .map(|property| {
                let (kind, active) = if property.PartitionStyle == VDS_PST_GPT {
                    let info = property.Anonymous.Gpt;
                    let kind = if info.partitionType == GPT_ESP {
                        PartitionKind::EfiSystem
                    } else if info.partitionType == GPT_MSR {
                        PartitionKind::MicrosoftReserved
                    } else if info.partitionType == GPT_RECOVERY {
                        PartitionKind::Recovery
                    } else {
                        PartitionKind::BasicData
                    };
                    (kind, false)
                } else if property.PartitionStyle == VDS_PST_MBR {
                    let info = property.Anonymous.Mbr;
                    let kind = match info.partitionType {
                        0xEF => PartitionKind::EfiSystem,
                        0x27 => PartitionKind::Recovery,
                        _ => PartitionKind::BasicData,
                    };
                    (kind, info.bootIndicator.0 != 0)
                } else {
                    return Err(StorageError::new(
                        "query VDS partitions",
                        "provider returned a partition with unsupported style",
                    ));
                };
                Ok(PartitionRecord {
                    partition_number: property.ulPartitionNumber,
                    offset_bytes: property.ullOffset,
                    size_bytes: property.ullSize,
                    kind,
                    active,
                })
            })
            .collect()
    }

    pub unsafe fn contiguous_free_bytes_after(
        disk_number: u32,
        end_offset_bytes: u64,
    ) -> Result<u64, StorageError> {
        let vds = Vds::connect()?;
        let disk = vds.find_disk(disk_number)?;
        let extents = free_extents(&disk.disk)?;
        extents
            .into_iter()
            .filter(|extent| extent.r#type == VDS_DET_FREE)
            .filter_map(|extent| {
                let gap = extent.ullOffset.checked_sub(end_offset_bytes)?;
                (gap <= ALIGNMENT).then_some(extent.ullSize.saturating_sub(gap))
            })
            .max()
            .ok_or_else(|| {
                StorageError::new(
                    "query contiguous free space",
                    "no free extent immediately follows the partition",
                )
            })
    }

    pub unsafe fn set_mbr_signature(disk_number: u32, signature: u32) -> Result<(), StorageError> {
        use windows::Win32::System::Ioctl::{
            DRIVE_LAYOUT_INFORMATION_EX, IOCTL_DISK_SET_DRIVE_LAYOUT_EX,
            IOCTL_DISK_UPDATE_PROPERTIES, PARTITION_STYLE_MBR,
        };
        use windows::Win32::System::IO::DeviceIoControl;

        if signature == 0 {
            return Err(StorageError::new(
                "set MBR signature",
                "MBR signature must be non-zero",
            ));
        }
        let (handle, mut storage, returned) = read_drive_layout(disk_number, true)?;
        if returned < 48 {
            return Err(StorageError::new(
                "set MBR signature",
                "drive layout response is shorter than its fixed header",
            ));
        }
        let layout = &mut *storage.as_mut_ptr().cast::<DRIVE_LAYOUT_INFORMATION_EX>();
        if layout.PartitionStyle != PARTITION_STYLE_MBR.0 as u32 {
            return Err(StorageError::new(
                "set MBR signature",
                "target disk is not MBR",
            ));
        }
        layout.Anonymous.Mbr.Signature = signature;
        let mut bytes = 0_u32;
        DeviceIoControl(
            handle.0,
            IOCTL_DISK_SET_DRIVE_LAYOUT_EX,
            Some(storage.as_ptr().cast()),
            returned,
            None,
            0,
            Some(&mut bytes),
            None,
        )
        .map_err(|error| api_error("set MBR drive layout", error))?;
        DeviceIoControl(
            handle.0,
            IOCTL_DISK_UPDATE_PROPERTIES,
            None,
            0,
            None,
            0,
            Some(&mut bytes),
            None,
        )
        .map_err(|error| api_error("refresh MBR drive layout", error))?;
        if mbr_signature(disk_number)? != Some(signature) {
            return Err(StorageError::new(
                "set MBR signature",
                "post-operation signature does not match the requested value",
            ));
        }
        Ok(())
    }

    unsafe fn read_drive_layout(
        disk_number: u32,
        writable: bool,
    ) -> Result<(OwnedHandle, Vec<u64>, u32), StorageError> {
        use windows::Win32::System::Ioctl::IOCTL_DISK_GET_DRIVE_LAYOUT_EX;
        use windows::Win32::System::IO::DeviceIoControl;

        let path = wide(&format!(r"\\.\PhysicalDrive{disk_number}"));
        let access = if writable {
            0x8000_0000 | 0x4000_0000
        } else {
            0
        };
        let handle = CreateFileW(
            PCWSTR(path.as_ptr()),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            Default::default(),
            None,
        )
        .map(OwnedHandle)
        .map_err(|error| api_error("open physical disk layout", error))?;
        let mut storage = vec![0_u64; 16_384];
        let mut returned = 0_u32;
        DeviceIoControl(
            handle.0,
            IOCTL_DISK_GET_DRIVE_LAYOUT_EX,
            None,
            0,
            Some(storage.as_mut_ptr().cast()),
            (storage.len() * size_of::<u64>()) as u32,
            Some(&mut returned),
            None,
        )
        .map_err(|error| api_error("read physical disk layout", error))?;
        Ok((handle, storage, returned))
    }

    /// Reads `STORAGE_DEVICE_DESCRIPTOR.BusType` for one physical disk.
    ///
    /// Microsoft documents a header query followed by an allocation using the
    /// returned `Size`; using the two-call form avoids truncating descriptors on
    /// storage stacks that append bus-specific properties.
    pub unsafe fn disk_bus_type(disk_number: u32) -> Result<DiskBusType, StorageError> {
        use windows::Win32::Storage::FileSystem::BusTypeNvme;
        use windows::Win32::System::Ioctl::{
            PropertyStandardQuery, StorageDeviceProperty, IOCTL_STORAGE_QUERY_PROPERTY,
            STORAGE_DESCRIPTOR_HEADER, STORAGE_DEVICE_DESCRIPTOR, STORAGE_PROPERTY_QUERY,
        };
        use windows::Win32::System::IO::DeviceIoControl;

        const MAX_DESCRIPTOR_BYTES: usize = 1024 * 1024;

        let path = wide(&format!(r"\\.\PhysicalDrive{disk_number}"));
        let handle = CreateFileW(
            PCWSTR(path.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            Default::default(),
            None,
        )
        .map(OwnedHandle)
        .map_err(|error| api_error("open physical disk for bus query", error))?;
        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceProperty,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        let mut header = STORAGE_DESCRIPTOR_HEADER::default();
        let mut returned = 0_u32;
        DeviceIoControl(
            handle.0,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&query as *const _ as *const c_void),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(&mut header as *mut _ as *mut c_void),
            size_of::<STORAGE_DESCRIPTOR_HEADER>() as u32,
            Some(&mut returned),
            None,
        )
        .map_err(|error| api_error("query physical disk descriptor size", error))?;
        if returned < size_of::<STORAGE_DESCRIPTOR_HEADER>() as u32 {
            return Err(StorageError::new(
                "query physical disk bus",
                "storage descriptor header was truncated",
            ));
        }
        let descriptor_size = usize::try_from(header.Size).map_err(|_| {
            StorageError::new(
                "query physical disk bus",
                "storage descriptor size does not fit in memory",
            )
        })?;
        if descriptor_size < size_of::<STORAGE_DEVICE_DESCRIPTOR>()
            || descriptor_size > MAX_DESCRIPTOR_BYTES
        {
            return Err(StorageError::new(
                "query physical disk bus",
                format!("invalid storage descriptor size: {descriptor_size}"),
            ));
        }

        let mut buffer = vec![0_u8; descriptor_size];
        returned = 0;
        DeviceIoControl(
            handle.0,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&query as *const _ as *const c_void),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(buffer.as_mut_ptr() as *mut c_void),
            buffer.len() as u32,
            Some(&mut returned),
            None,
        )
        .map_err(|error| api_error("query physical disk descriptor", error))?;
        if returned < size_of::<STORAGE_DEVICE_DESCRIPTOR>() as u32 {
            return Err(StorageError::new(
                "query physical disk bus",
                "storage device descriptor was truncated",
            ));
        }
        let descriptor =
            std::ptr::read_unaligned(buffer.as_ptr() as *const STORAGE_DEVICE_DESCRIPTOR);
        Ok(if descriptor.BusType == BusTypeNvme {
            DiskBusType::Nvme
        } else {
            DiskBusType::Other
        })
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn api_error(operation: &'static str, error: windows::core::Error) -> StorageError {
        StorageError::new(
            operation,
            format!("{} (HRESULT 0x{:08X})", error, error.code().0 as u32),
        )
    }

    fn hresult_error(operation: &'static str, result: HRESULT) -> StorageError {
        StorageError::new(
            operation,
            format!(
                "{} (HRESULT 0x{:08X})",
                windows::core::Error::from(result),
                result.0 as u32
            ),
        )
    }

    unsafe fn free_extents(disk: &IVdsDisk) -> Result<Vec<VDS_DISK_EXTENT>, StorageError> {
        let mut pointer = std::ptr::null_mut();
        let mut count = 0;
        disk.QueryExtents(&mut pointer, &mut count)
            .map_err(|error| api_error("query VDS disk extents", error))?;
        if count < 0 || (count > 0 && pointer.is_null()) {
            return Err(StorageError::new(
                "query VDS disk extents",
                "provider returned an invalid extent array",
            ));
        }
        let values = if count == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(pointer, count as usize).to_vec()
        };
        CoTaskMemFree((!pointer.is_null()).then_some(pointer.cast::<c_void>()));
        Ok(values
            .into_iter()
            .filter(|extent| extent.r#type == VDS_DET_FREE)
            .collect())
    }

    fn align_up(value: u64, alignment: u64) -> Option<u64> {
        value
            .checked_add(alignment.checked_sub(1)?)
            .map(|value| value / alignment * alignment)
    }

    fn aligned_extent(
        extents: &[VDS_DISK_EXTENT],
        requested_offset: u64,
        requested_size: u64,
    ) -> Result<(u64, u64), StorageError> {
        for extent in extents {
            let start = if requested_offset == 0 {
                align_up(extent.ullOffset, ALIGNMENT).ok_or_else(|| {
                    StorageError::new("select free extent", "extent offset overflow")
                })?
            } else {
                requested_offset
            };
            let extent_end = extent
                .ullOffset
                .checked_add(extent.ullSize)
                .ok_or_else(|| StorageError::new("select free extent", "extent end overflow"))?;
            if start < extent.ullOffset || start >= extent_end {
                continue;
            }
            let available = extent_end - start;
            let size = if requested_size == 0 {
                available / ALIGNMENT * ALIGNMENT
            } else {
                requested_size / ALIGNMENT * ALIGNMENT
            };
            if size >= ALIGNMENT && size <= available {
                return Ok((start, size));
            }
        }
        Err(StorageError::new(
            "select free extent",
            "no aligned free extent can satisfy the requested partition",
        ))
    }

    unsafe fn create_parameters(
        style: VDS_PARTITION_STYLE,
        kind: PartitionKind,
        active: bool,
        label: &str,
        preserved_gpt: Option<&GptPartitionMetadata>,
    ) -> Result<CREATE_PARTITION_PARAMETERS, StorageError> {
        if preserved_gpt.is_some() && (style != VDS_PST_GPT || kind != PartitionKind::BasicData) {
            return Err(StorageError::new(
                "create partition",
                "preserved GPT metadata is valid only for a GPT basic-data partition",
            ));
        }
        if style == VDS_PST_GPT {
            let partition_type = match kind {
                PartitionKind::BasicData => GPT_BASIC_DATA,
                PartitionKind::EfiSystem => GPT_ESP,
                PartitionKind::MicrosoftReserved => GPT_MSR,
                PartitionKind::Recovery => GPT_RECOVERY,
            };
            let (partition_id, attributes, name) = if let Some(metadata) = preserved_gpt {
                let bytes = metadata.partition_id;
                (
                    GUID::from_values(
                        u32::from_le_bytes(bytes[0..4].try_into().expect("GUID data1")),
                        u16::from_le_bytes(bytes[4..6].try_into().expect("GUID data2")),
                        u16::from_le_bytes(bytes[6..8].try_into().expect("GUID data3")),
                        bytes[8..16].try_into().expect("GUID data4"),
                    ),
                    metadata.attributes,
                    metadata.name,
                )
            } else {
                let mut name = [0_u16; 36];
                for (target, value) in name.iter_mut().zip(label.encode_utf16()) {
                    *target = value;
                }
                (
                    CoCreateGuid()
                        .map_err(|error| api_error("create GPT partition identifier", error))?,
                    0,
                    name,
                )
            };
            return Ok(CREATE_PARTITION_PARAMETERS {
                style,
                Anonymous: CREATE_PARTITION_PARAMETERS_0 {
                    GptPartInfo: CREATE_PARTITION_PARAMETERS_0_0 {
                        partitionType: partition_type,
                        partitionId: partition_id,
                        attributes,
                        name,
                    },
                },
            });
        }
        if style == VDS_PST_MBR {
            let partition_type = match kind {
                PartitionKind::BasicData | PartitionKind::Recovery => 0x07,
                PartitionKind::EfiSystem => 0xEF,
                PartitionKind::MicrosoftReserved => {
                    return Err(StorageError::new(
                        "create MBR partition",
                        "Microsoft reserved partitions require GPT",
                    ))
                }
            };
            return Ok(CREATE_PARTITION_PARAMETERS {
                style,
                Anonymous: CREATE_PARTITION_PARAMETERS_0 {
                    MbrPartInfo: CREATE_PARTITION_PARAMETERS_0_1 {
                        partitionType: partition_type,
                        bootIndicator: BOOLEAN(u8::from(active)),
                    },
                },
            });
        }
        Err(StorageError::new(
            "create partition",
            "disk is not initialized as MBR or GPT",
        ))
    }

    unsafe fn format_volume(
        volume: &IVdsVolume,
        options: &FormatOptions,
    ) -> Result<(), StorageError> {
        let label = wide(&options.label);
        let filesystem = wide(options.file_system.name());
        if let Ok(formatter) = volume.cast::<IVdsVolumeMF2>() {
            let asynchronous = formatter
                .FormatEx(
                    PCWSTR(filesystem.as_ptr()),
                    0,
                    options.allocation_unit_size,
                    PCWSTR(label.as_ptr()),
                    false,
                    options.quick,
                    false,
                )
                .map_err(|error| api_error("start VDS volume format", error))?;
            wait_async("format volume", &asynchronous, Some(VDS_ASYNCOUT_FORMAT))?;
            return Ok(());
        }
        if options.file_system == FileSystem::ExFat {
            return Err(StorageError::new(
                "format volume",
                "the installed VDS provider does not expose IVdsVolumeMF2 required for exFAT",
            ));
        }
        let formatter = volume
            .cast::<IVdsVolumeMF>()
            .map_err(|error| api_error("open VDS volume formatter", error))?;
        let fs = match options.file_system {
            FileSystem::Ntfs => VDS_FST_NTFS,
            FileSystem::Fat => VDS_FST_FAT,
            FileSystem::Fat32 => VDS_FST_FAT32,
            FileSystem::ExFat => VDS_FST_EXFAT,
        };
        let asynchronous = formatter
            .Format(
                fs,
                PCWSTR(label.as_ptr()),
                options.allocation_unit_size,
                false,
                options.quick,
                false,
            )
            .map_err(|error| api_error("start VDS volume format", error))?;
        wait_async("format volume", &asynchronous, Some(VDS_ASYNCOUT_FORMAT))?;
        Ok(())
    }

    unsafe fn format_partition(
        disk: &IVdsDisk,
        offset_bytes: u64,
        file_system: FileSystem,
        label: &str,
    ) -> Result<(), StorageError> {
        let formatter = disk
            .cast::<IVdsDiskPartitionMF>()
            .map_err(|error| api_error("open VDS partition formatter", error))?;
        let filesystem = wide(file_system.name());
        let label = wide(label);
        let asynchronous = formatter
            .FormatPartitionEx(
                offset_bytes,
                PCWSTR(filesystem.as_ptr()),
                0,
                0,
                PCWSTR(label.as_ptr()),
                false,
                true,
                false,
            )
            .map_err(|error| api_error("start VDS partition format", error))?;
        wait_async("format partition", &asynchronous, Some(VDS_ASYNCOUT_FORMAT))?;
        Ok(())
    }

    unsafe fn add_access_path(volume: &IVdsVolume, drive_letter: char) -> Result<(), StorageError> {
        let formatter = volume
            .cast::<IVdsVolumeMF>()
            .map_err(|error| api_error("open VDS volume access-path interface", error))?;
        let path = wide(&format!("{}:\\", normalize_letter(drive_letter)?));
        formatter
            .AddAccessPath(PCWSTR(path.as_ptr()))
            .map_err(|error| api_error("assign drive letter", error))
    }

    unsafe fn initialize_disk_ioctl(
        disk_number: u32,
        style: DiskStyle,
    ) -> Result<(), StorageError> {
        use windows::Win32::System::Ioctl::{
            CREATE_DISK, CREATE_DISK_0, CREATE_DISK_GPT, CREATE_DISK_MBR, IOCTL_DISK_CREATE_DISK,
            IOCTL_DISK_UPDATE_PROPERTIES, PARTITION_STYLE_GPT, PARTITION_STYLE_MBR,
        };
        use windows::Win32::System::IO::DeviceIoControl;

        let path = wide(&format!(r"\\.\PhysicalDrive{disk_number}"));
        let handle = CreateFileW(
            PCWSTR(path.as_ptr()),
            0x8000_0000 | 0x4000_0000,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            Default::default(),
            None,
        )
        .map(OwnedHandle)
        .map_err(|error| api_error("open physical disk for initialization", error))?;

        let guid = CoCreateGuid().map_err(|error| api_error("create disk identifier", error))?;
        let signature =
            u32::from_le_bytes(guid.to_u128().to_le_bytes()[..4].try_into().unwrap()).max(1);
        let create = match style {
            DiskStyle::Mbr => CREATE_DISK {
                PartitionStyle: PARTITION_STYLE_MBR,
                Anonymous: CREATE_DISK_0 {
                    Mbr: CREATE_DISK_MBR {
                        Signature: signature,
                    },
                },
            },
            DiskStyle::Gpt => CREATE_DISK {
                PartitionStyle: PARTITION_STYLE_GPT,
                Anonymous: CREATE_DISK_0 {
                    Gpt: CREATE_DISK_GPT {
                        DiskId: guid,
                        MaxPartitionCount: 128,
                    },
                },
            },
        };
        let mut returned = 0;
        DeviceIoControl(
            handle.0,
            IOCTL_DISK_CREATE_DISK,
            Some((&create as *const CREATE_DISK).cast::<c_void>()),
            size_of::<CREATE_DISK>() as u32,
            None,
            0,
            Some(&mut returned),
            None,
        )
        .map_err(|error| api_error("initialize disk partition table", error))?;
        DeviceIoControl(
            handle.0,
            IOCTL_DISK_UPDATE_PROPERTIES,
            None,
            0,
            None,
            0,
            Some(&mut returned),
            None,
        )
        .map_err(|error| api_error("update disk properties", error))?;
        Ok(())
    }

    pub unsafe fn clean_and_initialize(
        disk_number: u32,
        style: DiskStyle,
    ) -> Result<(), StorageError> {
        let vds = Vds::connect()?;
        let disk = vds.find_disk(disk_number)?;
        let advanced = disk
            .disk
            .cast::<IVdsAdvancedDisk>()
            .map_err(|error| api_error("open VDS advanced disk", error))?;
        let asynchronous = advanced
            .Clean(true, true, false)
            .map_err(|error| api_error("start VDS disk clean", error))?;
        wait_async("clean disk", &asynchronous, Some(VDS_ASYNCOUT_CLEAN))?;
        initialize_disk_ioctl(disk_number, style)?;
        vds.refresh()
    }

    pub unsafe fn create_partition(
        request: &CreatePartitionRequest,
    ) -> Result<CreatedPartition, StorageError> {
        validate_create_request(request)?;
        let vds = Vds::connect()?;
        let disk = vds.find_disk(request.disk_number)?;
        let extents = free_extents(&disk.disk)?;
        let (offset_bytes, size_bytes) =
            aligned_extent(&extents, request.offset_bytes, request.size_bytes)?;
        let parameters = create_parameters(
            disk.style,
            request.kind,
            request.active,
            &request.label,
            request.preserve_gpt_metadata.as_ref(),
        )?;
        let creator = disk
            .disk
            .cast::<IVdsCreatePartitionEx>()
            .map_err(|error| api_error("open VDS partition creator", error))?;
        let asynchronous = creator
            .CreatePartitionEx(offset_bytes, size_bytes, ALIGNMENT as u32, &parameters)
            .map_err(|error| api_error("start VDS partition creation", error))?;
        let output = wait_async(
            "create partition",
            &asynchronous,
            Some(VDS_ASYNCOUT_CREATEPARTITION),
        )?;
        let created = output.Anonymous.cp;
        if created.ullOffset != offset_bytes {
            return Err(StorageError::new(
                "create partition",
                format!(
                    "provider created partition at unexpected offset {} instead of {}",
                    created.ullOffset, offset_bytes
                ),
            ));
        }
        vds.refresh()?;
        if created.volumeId != GUID::zeroed() {
            let volume = vds.find_volume_by_id(created.volumeId)?;
            if let Some(file_system) = request.file_system {
                format_volume(
                    &volume,
                    &FormatOptions {
                        file_system,
                        label: request.label.clone(),
                        allocation_unit_size: 0,
                        quick: true,
                    },
                )?;
            }
            if let Some(letter) = request.drive_letter {
                add_access_path(&volume, letter)?;
            }
        } else {
            let disk = vds.find_disk(request.disk_number)?;
            if let Some(file_system) = request.file_system {
                format_partition(&disk.disk, created.ullOffset, file_system, &request.label)?;
            }
            if let Some(letter) = request.drive_letter {
                disk.disk
                    .cast::<IVdsAdvancedDisk>()
                    .map_err(|error| api_error("open VDS advanced disk", error))?
                    .AssignDriveLetter(created.ullOffset, normalize_letter(letter)? as u16)
                    .map_err(|error| api_error("assign partition drive letter", error))?;
            }
        }
        vds.refresh()?;
        Ok(CreatedPartition {
            offset_bytes: created.ullOffset,
            size_bytes,
        })
    }

    pub unsafe fn delete_partition(
        disk_number: u32,
        offset_bytes: u64,
        force_protected: bool,
    ) -> Result<(), StorageError> {
        if offset_bytes == 0 {
            return Err(StorageError::new(
                "delete partition",
                "partition offset must be non-zero",
            ));
        }
        let vds = Vds::connect()?;
        let disk = vds.find_disk(disk_number)?;
        disk.disk
            .cast::<IVdsAdvancedDisk>()
            .map_err(|error| api_error("open VDS advanced disk", error))?
            .DeletePartition(offset_bytes, false, force_protected)
            .map_err(|error| api_error("delete partition", error))?;
        vds.refresh()
    }

    pub unsafe fn format_drive(
        drive_letter: char,
        file_system: FileSystem,
        label: &str,
    ) -> Result<(), StorageError> {
        format_drive_with_options(
            drive_letter,
            &FormatOptions {
                file_system,
                label: label.to_owned(),
                allocation_unit_size: 0,
                quick: true,
            },
        )
    }

    pub unsafe fn format_drive_with_options(
        drive_letter: char,
        options: &FormatOptions,
    ) -> Result<(), StorageError> {
        let label = &options.label;
        if label.encode_utf16().count() > 32 || label.contains(['\0', '\r', '\n']) {
            return Err(StorageError::new(
                "format volume",
                "volume label is too long or contains a control character",
            ));
        }
        let vds = Vds::connect()?;
        let volume = vds.find_volume_by_letter(drive_letter)?;
        format_volume(&volume, options)?;
        vds.refresh()
    }

    pub unsafe fn query_max_reclaimable_bytes(drive_letter: char) -> Result<u64, StorageError> {
        let vds = Vds::connect()?;
        let volume = vds.find_volume_by_letter(drive_letter)?;
        volume
            .cast::<IVdsVolumeShrink>()
            .map_err(|error| api_error("open VDS volume shrink interface", error))?
            .QueryMaxReclaimableBytes()
            .map_err(|error| api_error("query maximum reclaimable bytes", error))
    }

    pub unsafe fn shrink_volume(
        drive_letter: char,
        desired_bytes: u64,
        minimum_bytes: u64,
    ) -> Result<u64, StorageError> {
        if desired_bytes == 0 || minimum_bytes == 0 || minimum_bytes > desired_bytes {
            return Err(StorageError::new(
                "shrink volume",
                "desired and minimum shrink sizes must be non-zero and ordered",
            ));
        }
        let vds = Vds::connect()?;
        let volume = vds.find_volume_by_letter(drive_letter)?;
        let shrink = volume
            .cast::<IVdsVolumeShrink>()
            .map_err(|error| api_error("open VDS volume shrink interface", error))?;
        let maximum = shrink
            .QueryMaxReclaimableBytes()
            .map_err(|error| api_error("query maximum reclaimable bytes", error))?;
        if minimum_bytes > maximum {
            return Err(StorageError::new(
                "shrink volume",
                format!(
                    "requested minimum {} bytes exceeds current reclaimable limit {} bytes",
                    minimum_bytes, maximum
                ),
            ));
        }
        let asynchronous = shrink
            .Shrink(desired_bytes.min(maximum), minimum_bytes)
            .map_err(|error| api_error("start VDS volume shrink", error))?;
        let output = wait_async(
            "shrink volume",
            &asynchronous,
            Some(VDS_ASYNCOUT_SHRINKVOLUME),
        )?;
        let reclaimed = output.Anonymous.sv.ullReclaimedBytes;
        if reclaimed < minimum_bytes {
            return Err(StorageError::new(
                "shrink volume",
                format!(
                    "provider reclaimed {} bytes, below required minimum {} bytes",
                    reclaimed, minimum_bytes
                ),
            ));
        }
        vds.refresh()?;
        Ok(reclaimed)
    }

    pub unsafe fn extend_volume(
        drive_letter: char,
        disk_number: u32,
        bytes_to_add: u64,
    ) -> Result<(), StorageError> {
        if bytes_to_add == 0 {
            return Err(StorageError::new(
                "extend volume",
                "extension size must be non-zero",
            ));
        }
        let vds = Vds::connect()?;
        let disk = vds.find_disk(disk_number)?;
        let volume = vds.find_volume_by_letter(drive_letter)?;
        let input = VDS_INPUT_DISK {
            diskId: disk.id,
            ullSize: bytes_to_add,
            plexId: GUID::zeroed(),
            memberIdx: 0,
        };
        let asynchronous = volume
            .Extend(&[input])
            .map_err(|error| api_error("start VDS volume extension", error))?;
        wait_async(
            "extend volume",
            &asynchronous,
            Some(VDS_ASYNCOUT_EXTENDVOLUME),
        )?;
        vds.refresh()
    }

    pub unsafe fn set_mbr_active(
        disk_number: u32,
        offset_bytes: u64,
        active: bool,
    ) -> Result<(), StorageError> {
        if offset_bytes == 0 {
            return Err(StorageError::new(
                "change active partition",
                "partition offset must be non-zero",
            ));
        }
        let vds = Vds::connect()?;
        let disk = vds.find_disk(disk_number)?;
        if disk.style != VDS_PST_MBR {
            return Err(StorageError::new(
                "change active partition",
                "active flags are valid only on MBR disks",
            ));
        }
        let parameters = CHANGE_ATTRIBUTES_PARAMETERS {
            style: VDS_PST_MBR,
            Anonymous: CHANGE_ATTRIBUTES_PARAMETERS_0 {
                MbrPartInfo: CHANGE_ATTRIBUTES_PARAMETERS_0_1 {
                    bootIndicator: BOOLEAN(u8::from(active)),
                },
            },
        };
        disk.disk
            .cast::<IVdsAdvancedDisk>()
            .map_err(|error| api_error("open VDS advanced disk", error))?
            .ChangeAttributes(offset_bytes, &parameters)
            .map_err(|error| api_error("change active partition", error))?;
        vds.refresh()?;
        let disk = vds.find_disk(disk_number)?;
        let advanced = disk
            .disk
            .cast::<IVdsAdvancedDisk>()
            .map_err(|error| api_error("reopen VDS advanced disk", error))?;
        let mut properties =
            windows::Win32::Storage::VirtualDiskService::VDS_PARTITION_PROP::default();
        advanced
            .GetPartitionProperties(offset_bytes, &mut properties)
            .map_err(|error| api_error("verify active partition", error))?;
        if properties.PartitionStyle != VDS_PST_MBR
            || (properties.Anonymous.Mbr.bootIndicator.0 != 0) != active
        {
            return Err(StorageError::new(
                "change active partition",
                "post-operation active flag does not match the requested state",
            ));
        }
        Ok(())
    }

    fn drive_letter_bit(letter: char) -> u32 {
        1_u32 << (u32::from(letter as u8) - u32::from(b'A'))
    }

    unsafe fn wait_for_drive_letter_removal(letter: char) -> Result<bool, StorageError> {
        let bit = drive_letter_bit(letter);
        for _ in 0..20 {
            let mask = GetLogicalDrives();
            if mask == 0 {
                return Err(StorageError::new(
                    "verify removed drive letter",
                    windows::core::Error::from_win32().to_string(),
                ));
            }
            if mask & bit == 0 {
                return Ok(true);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Ok(false)
    }

    fn first_dos_device_target(buffer: &[u16], length: u32) -> Result<Vec<u16>, StorageError> {
        let length = usize::try_from(length)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let end = buffer[..length]
            .iter()
            .position(|value| *value == 0)
            .ok_or_else(|| {
                StorageError::new(
                    "query drive letter DOS target",
                    "QueryDosDeviceW returned an unterminated target",
                )
            })?;
        if end == 0 {
            return Err(StorageError::new(
                "query drive letter DOS target",
                "QueryDosDeviceW returned an empty target",
            ));
        }
        let mut target = buffer[..end].to_vec();
        target.push(0);
        Ok(target)
    }

    unsafe fn query_dos_device_target(letter: char) -> Result<Vec<u16>, StorageError> {
        let device_name = wide(&format!("{letter}:"));
        let mut buffer = vec![0_u16; 32_768];
        let length = QueryDosDeviceW(PCWSTR(device_name.as_ptr()), Some(&mut buffer));
        if length == 0 {
            return Err(StorageError::new(
                "query drive letter DOS target",
                windows::core::Error::from_win32().to_string(),
            ));
        }
        first_dos_device_target(&buffer, length)
    }

    unsafe fn remove_exact_dos_device_mapping(
        letter: char,
        target: &[u16],
    ) -> Result<(), StorageError> {
        let device_name = wide(&format!("{letter}:"));
        let flags = DDD_RAW_TARGET_PATH | DDD_REMOVE_DEFINITION | DDD_EXACT_MATCH_ON_REMOVE;
        DefineDosDeviceW(flags, PCWSTR(device_name.as_ptr()), PCWSTR(target.as_ptr()))
            .map_err(|error| api_error("remove exact drive letter DOS mapping", error))
    }

    unsafe fn remove_drive_letter_via_vds(
        drive_letter: char,
        force: bool,
    ) -> Result<(), StorageError> {
        let letter = normalize_letter(drive_letter)?;
        let vds = Vds::connect()?;
        let volume = vds.find_volume_by_letter(letter)?;
        let formatter = volume
            .cast::<IVdsVolumeMF>()
            .map_err(|error| api_error("open VDS volume access-path interface", error))?;
        let path = wide(&format!("{letter}:\\"));
        formatter
            .DeleteAccessPath(PCWSTR(path.as_ptr()), force)
            .map_err(|error| api_error("remove drive letter access path", error))?;
        vds.refresh()
    }

    pub unsafe fn remove_drive_letter(drive_letter: char) -> Result<(), StorageError> {
        let letter = normalize_letter(drive_letter)?;
        remove_drive_letter_via_vds(letter, false)?;
        if wait_for_drive_letter_removal(letter)? {
            Ok(())
        } else {
            Err(StorageError::new(
                "verify removed drive letter",
                format!("{letter}: remains assigned after the access-path removal completed"),
            ))
        }
    }

    pub unsafe fn remove_drive_letter_if_matches(
        drive_letter: char,
        expected: VolumeIdentity,
    ) -> Result<(), StorageError> {
        let actual = volume_identity(drive_letter)?;
        if !same_physical_partition(actual, expected) {
            return Err(StorageError::new(
                "verify temporary drive letter ownership",
                format!(
                    "{}: now maps to disk {} offset {}, expected disk {} offset {}",
                    drive_letter.to_ascii_uppercase(),
                    actual.disk_number,
                    actual.offset_bytes,
                    expected.disk_number,
                    expected.offset_bytes
                ),
            ));
        }
        let letter = normalize_letter(drive_letter)?;
        let dos_target = query_dos_device_target(letter)?;
        let vds_error = remove_drive_letter_via_vds(letter, true).err();
        if wait_for_drive_letter_removal(letter)? {
            return Ok(());
        }

        remove_exact_dos_device_mapping(letter, &dos_target).map_err(|dos_error| {
            let vds_context = vds_error
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| {
                    "VDS reported success but the drive letter remained".to_string()
                });
            StorageError::new(
                "remove temporary drive letter",
                format!("{vds_context}; exact DOS mapping cleanup also failed: {dos_error}"),
            )
        })?;
        if wait_for_drive_letter_removal(letter)? {
            Ok(())
        } else {
            Err(StorageError::new(
                "verify removed temporary drive letter",
                format!("{letter}: remains assigned after exact DOS mapping cleanup"),
            ))
        }
    }

    pub unsafe fn assign_partition_drive_letter(
        disk_number: u32,
        offset_bytes: u64,
        drive_letter: char,
    ) -> Result<(), StorageError> {
        let vds = Vds::connect()?;
        let disk = vds.find_disk(disk_number)?;
        disk.disk
            .cast::<IVdsAdvancedDisk>()
            .map_err(|error| api_error("open VDS advanced disk", error))?
            .AssignDriveLetter(offset_bytes, normalize_letter(drive_letter)? as u16)
            .map_err(|error| api_error("assign partition drive letter", error))?;
        vds.refresh()
    }

    // VDS_ASYNC_OUTPUT owns interface pointers for some operation types. The
    // operations used here return only scalar output or no object, so reading
    // the matching union arm above does not transfer an interface reference.
    #[allow(dead_code)]
    fn _assert_no_manually_drop_leak(_: ManuallyDrop<Option<IUnknown>>) {}

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_only_exact_physical_drive_suffixes() {
            assert_eq!(physical_drive_number(r"\\?\PhysicalDrive0"), Some(0));
            assert_eq!(physical_drive_number(r"\\.\PhysicalDrive123"), Some(123));
            assert_eq!(physical_drive_number(r"PhysicalDriveX"), None);
            assert_eq!(physical_drive_number(r"PhysicalDrive1\Partition2"), None);
        }

        #[test]
        fn dos_device_cleanup_uses_only_the_current_exact_mapping() {
            let buffer = [
                b'\\' as u16,
                b'D' as u16,
                b'e' as u16,
                b'v' as u16,
                b'i' as u16,
                b'c' as u16,
                b'e' as u16,
                b'1' as u16,
                0,
                b'\\' as u16,
                b'D' as u16,
                b'e' as u16,
                b'v' as u16,
                b'i' as u16,
                b'c' as u16,
                b'e' as u16,
                b'2' as u16,
                0,
                0,
            ];
            let target = first_dos_device_target(&buffer, buffer.len() as u32).unwrap();
            assert_eq!(String::from_utf16_lossy(&target), "\\Device1\0");
        }

        #[test]
        fn volume_enumeration_accepts_only_terminated_guid_roots() {
            let expected = r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\";
            let mut valid: Vec<u16> = expected.encode_utf16().chain(std::iter::once(0)).collect();
            valid.resize(128, 0);
            assert_eq!(volume_name_from_buffer(&valid).unwrap(), expected);

            let unterminated: Vec<u16> = expected.encode_utf16().collect();
            assert!(volume_name_from_buffer(&unterminated).is_err());

            let invalid: Vec<u16> = r"Z:\".encode_utf16().chain(std::iter::once(0)).collect();
            assert!(volume_name_from_buffer(&invalid).is_err());
        }

        #[test]
        #[ignore = "requires an explicit read-only Windows volume-enumeration integration test"]
        fn volume_guid_resolution_does_not_change_drive_letters() {
            let before = assigned_drive_letter_mask().unwrap();
            let letter = current_windows_drive_letter().unwrap();
            let identity = unsafe { volume_identity(letter).unwrap() };
            let volume_root = unsafe {
                volume_guid_path_for_partition(identity.disk_number, identity.offset_bytes).unwrap()
            };
            assert!(std::path::Path::new(&volume_root).is_dir());
            assert_eq!(assigned_drive_letter_mask().unwrap(), before);
        }

        #[test]
        fn selects_an_aligned_first_fit_without_crossing_extent_end() {
            let extents = [
                VDS_DISK_EXTENT {
                    ullOffset: 513,
                    ullSize: 2 * ALIGNMENT,
                    r#type: VDS_DET_FREE,
                    ..Default::default()
                },
                VDS_DISK_EXTENT {
                    ullOffset: 4 * ALIGNMENT,
                    ullSize: 8 * ALIGNMENT,
                    r#type: VDS_DET_FREE,
                    ..Default::default()
                },
            ];
            assert_eq!(
                aligned_extent(&extents, 0, 3 * ALIGNMENT).unwrap(),
                (4 * ALIGNMENT, 3 * ALIGNMENT)
            );
            assert!(aligned_extent(&extents, ALIGNMENT, 3 * ALIGNMENT).is_err());
        }

        #[test]
        fn recreated_gpt_basic_partition_preserves_identity_attributes_and_name() {
            let mut name = [0_u16; 36];
            name[..4].copy_from_slice(&[b'D' as u16, b'a' as u16, b't' as u16, b'a' as u16]);
            let metadata = GptPartitionMetadata {
                partition_id: [
                    0x78, 0x56, 0x34, 0x12, 0xbc, 0x9a, 0xf0, 0xde, 1, 2, 3, 4, 5, 6, 7, 8,
                ],
                attributes: 0x8000_0000_0000_0001,
                name,
            };
            let parameters = unsafe {
                create_parameters(
                    VDS_PST_GPT,
                    PartitionKind::BasicData,
                    false,
                    "",
                    Some(&metadata),
                )
            }
            .unwrap();
            let actual = unsafe { parameters.Anonymous.GptPartInfo };
            assert_eq!(actual.partitionId.data1, 0x1234_5678);
            assert_eq!(actual.partitionId.data2, 0x9abc);
            assert_eq!(actual.partitionId.data3, 0xdef0);
            assert_eq!(actual.partitionId.data4, [1, 2, 3, 4, 5, 6, 7, 8]);
            assert_eq!(actual.attributes, metadata.attributes);
            assert_eq!(actual.name, metadata.name);
        }
    }
}

#[cfg(windows)]
pub fn clean_and_initialize(disk_number: u32, style: DiskStyle) -> Result<(), StorageError> {
    unsafe { platform::clean_and_initialize(disk_number, style) }
}

#[cfg(windows)]
pub fn create_partition(
    request: &CreatePartitionRequest,
) -> Result<CreatedPartition, StorageError> {
    unsafe { platform::create_partition(request) }
}

#[cfg(windows)]
pub fn delete_partition(
    disk_number: u32,
    offset_bytes: u64,
    force_protected: bool,
) -> Result<(), StorageError> {
    unsafe { platform::delete_partition(disk_number, offset_bytes, force_protected) }
}

#[cfg(windows)]
pub fn format_drive(
    drive_letter: char,
    file_system: FileSystem,
    label: &str,
) -> Result<(), StorageError> {
    unsafe { platform::format_drive(drive_letter, file_system, label) }
}

#[cfg(windows)]
pub fn format_drive_with_options(
    drive_letter: char,
    options: &FormatOptions,
) -> Result<(), StorageError> {
    unsafe { platform::format_drive_with_options(drive_letter, options) }
}

#[cfg(windows)]
pub fn query_max_reclaimable_bytes(drive_letter: char) -> Result<u64, StorageError> {
    unsafe { platform::query_max_reclaimable_bytes(drive_letter) }
}

#[cfg(windows)]
pub fn shrink_volume(
    drive_letter: char,
    desired_bytes: u64,
    minimum_bytes: u64,
) -> Result<u64, StorageError> {
    unsafe { platform::shrink_volume(drive_letter, desired_bytes, minimum_bytes) }
}

#[cfg(windows)]
pub fn extend_volume(
    drive_letter: char,
    disk_number: u32,
    bytes_to_add: u64,
) -> Result<(), StorageError> {
    unsafe { platform::extend_volume(drive_letter, disk_number, bytes_to_add) }
}

#[cfg(windows)]
pub fn set_mbr_active(
    disk_number: u32,
    offset_bytes: u64,
    active: bool,
) -> Result<(), StorageError> {
    unsafe { platform::set_mbr_active(disk_number, offset_bytes, active) }
}

#[cfg(windows)]
pub fn remove_drive_letter(drive_letter: char) -> Result<(), StorageError> {
    unsafe { platform::remove_drive_letter(drive_letter) }
}

/// Remove a temporary drive letter only while it still identifies the volume that was mounted.
/// This prevents a delayed cleanup from deleting a different volume's newly reused drive letter.
#[cfg(windows)]
pub fn remove_drive_letter_if_matches(
    drive_letter: char,
    expected: VolumeIdentity,
) -> Result<(), StorageError> {
    unsafe { platform::remove_drive_letter_if_matches(drive_letter, expected) }
}

#[cfg(windows)]
pub fn assign_partition_drive_letter(
    disk_number: u32,
    offset_bytes: u64,
    drive_letter: char,
) -> Result<(), StorageError> {
    unsafe { platform::assign_partition_drive_letter(disk_number, offset_bytes, drive_letter) }
}

#[cfg(windows)]
pub fn current_windows_drive_letter() -> Result<char, StorageError> {
    platform::current_windows_drive_letter()
}

#[cfg(windows)]
pub fn assigned_drive_letter_mask() -> Result<u32, StorageError> {
    platform::assigned_drive_letter_mask()
}

/// Return every currently assigned drive letter that resolves to the requested physical
/// partition. Individual inaccessible roots (for example empty optical drives) are skipped after
/// `GetLogicalDrives` has provided the authoritative assignment mask.
#[cfg(windows)]
pub fn assigned_drive_letters_for_partition(
    disk_number: u32,
    offset_bytes: u64,
) -> Result<Vec<char>, StorageError> {
    let mask = assigned_drive_letter_mask()?;
    let expected = VolumeIdentity {
        disk_number,
        offset_bytes,
        extent_length_bytes: 0,
    };
    Ok((b'C'..=b'Z')
        .filter(|letter| mask & (1_u32 << u32::from(*letter - b'A')) != 0)
        .filter_map(|letter| {
            let letter = char::from(letter);
            volume_identity(letter)
                .ok()
                .filter(|actual| same_physical_partition(*actual, expected))
                .map(|_| letter)
        })
        .collect())
}

#[cfg(not(windows))]
pub fn assigned_drive_letter_mask() -> Result<u32, StorageError> {
    Err(StorageError::new(
        "enumerate assigned drive letters",
        "Windows storage APIs are unavailable",
    ))
}

#[cfg(windows)]
pub fn volume_identity(drive_letter: char) -> Result<VolumeIdentity, StorageError> {
    unsafe { platform::volume_identity(drive_letter) }
}

#[cfg(windows)]
pub fn volume_guid_path_for_partition(
    disk_number: u32,
    offset_bytes: u64,
) -> Result<String, StorageError> {
    unsafe { platform::volume_guid_path_for_partition(disk_number, offset_bytes) }
}

#[cfg(windows)]
pub fn mbr_signature(disk_number: u32) -> Result<Option<u32>, StorageError> {
    unsafe { platform::mbr_signature(disk_number) }
}

#[cfg(windows)]
pub fn set_mbr_signature(disk_number: u32, signature: u32) -> Result<(), StorageError> {
    unsafe { platform::set_mbr_signature(disk_number, signature) }
}

#[cfg(windows)]
pub fn disk_style(disk_number: u32) -> Result<DiskStyle, StorageError> {
    unsafe { platform::disk_style(disk_number) }
}

#[cfg(windows)]
pub fn disk_bus_type(disk_number: u32) -> Result<DiskBusType, StorageError> {
    unsafe { platform::disk_bus_type(disk_number) }
}

#[cfg(windows)]
pub fn contiguous_free_bytes_after(
    disk_number: u32,
    end_offset_bytes: u64,
) -> Result<u64, StorageError> {
    unsafe { platform::contiguous_free_bytes_after(disk_number, end_offset_bytes) }
}

#[cfg(windows)]
pub fn partitions(disk_number: u32) -> Result<Vec<PartitionRecord>, StorageError> {
    unsafe { platform::partitions(disk_number) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(kind: PartitionKind) -> CreatePartitionRequest {
        CreatePartitionRequest {
            disk_number: 2,
            offset_bytes: 0,
            size_bytes: 1024 * 1024,
            kind,
            file_system: Some(FileSystem::Ntfs),
            label: "Data".into(),
            drive_letter: Some('D'),
            active: false,
            preserve_gpt_metadata: None,
        }
    }

    #[test]
    fn validates_partition_role_constraints_before_windows_io() {
        let mut value = request(PartitionKind::EfiSystem);
        assert!(validate_create_request(&value).is_err());
        value.file_system = Some(FileSystem::Fat32);
        value.drive_letter = None;
        assert!(validate_create_request(&value).is_ok());

        let mut value = request(PartitionKind::MicrosoftReserved);
        assert!(validate_create_request(&value).is_err());
        value.file_system = None;
        value.drive_letter = None;
        assert!(validate_create_request(&value).is_ok());
    }

    #[test]
    fn rejects_unaligned_offsets_invalid_letters_and_control_characters() {
        let mut value = request(PartitionKind::BasicData);
        value.offset_bytes = 1;
        assert!(validate_create_request(&value).is_err());
        value.offset_bytes = 1024 * 1024;
        value.drive_letter = Some('A');
        assert!(validate_create_request(&value).is_err());
        value.drive_letter = Some('D');
        value.label = "bad\nlabel".into();
        assert!(validate_create_request(&value).is_err());
    }

    #[test]
    fn temporary_mount_cleanup_requires_the_same_disk_and_partition_offset() {
        let expected = VolumeIdentity {
            disk_number: 2,
            offset_bytes: 1_048_576,
            extent_length_bytes: 268_435_456,
        };
        assert!(same_physical_partition(expected, expected));
        assert!(!same_physical_partition(
            expected,
            VolumeIdentity {
                disk_number: 3,
                ..expected
            }
        ));
        assert!(!same_physical_partition(
            expected,
            VolumeIdentity {
                offset_bytes: 2_097_152,
                ..expected
            }
        ));
    }
}
