//! 硬件信息模块
//! 使用纯 WinAPI 获取硬件信息

use std::collections::HashMap;
use std::ffi::OsString;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStringExt;

use windows::core::{BSTR, PCWSTR, VARIANT};
use windows::Win32::Foundation::{BOOL, CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayDevicesW, EnumDisplaySettingsW, DEVMODEW, DISPLAY_DEVICEW,
    ENUM_CURRENT_SETTINGS,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoInitializeSecurity, CoSetProxyBlanket, CoUninitialize,
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL,
    RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IMPERSONATE, SAFEARRAY,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE,
    KEY_READ, REG_VALUE_TYPE,
};
use windows::Win32::System::SystemInformation::{
    GetNativeSystemInfo, GlobalMemoryStatusEx, MEMORYSTATUSEX, SYSTEM_INFO,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    FILE_FLAGS_AND_ATTRIBUTES,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    IOCTL_STORAGE_QUERY_PROPERTY, IOCTL_DISK_GET_LENGTH_INFO,
    STORAGE_PROPERTY_QUERY, StorageDeviceProperty, PropertyStandardQuery,
};
use windows::Win32::System::Wmi::{
    IEnumWbemClassObject, IWbemClassObject, IWbemLocator, IWbemServices,
    WbemLocator, WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY,
};
use windows::Win32::System::Variant::{
    VT_NULL, VT_EMPTY, VT_BSTR, VT_I4, VT_UI4, VT_I2, VT_UI2, VT_UI1, VT_I8, VT_UI8, VT_ARRAY,
    VARENUM,
};
use windows::Win32::System::Ole::SafeArrayGetElement;

/// 设备类型枚举
#[derive(Debug, Clone, Default, PartialEq)]
pub enum DeviceType {
    #[default]
    Unknown,
    Desktop,
    Laptop,
    Tablet,
    Server,
    Workstation,
}

impl std::fmt::Display for DeviceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceType::Desktop => write!(f, "台式机"),
            DeviceType::Laptop => write!(f, "笔记本"),
            DeviceType::Tablet => write!(f, "平板电脑"),
            DeviceType::Server => write!(f, "服务器"),
            DeviceType::Workstation => write!(f, "工作站"),
            DeviceType::Unknown => write!(f, "未知"),
        }
    }
}

/// 电池信息
#[derive(Debug, Clone, Default)]
pub struct BatteryInfo {
    pub charge_percent: u8,
    pub is_charging: bool,
    pub is_ac_connected: bool,
    pub model: String,
    pub manufacturer: String,
    pub design_capacity_mwh: u32,
    pub full_charge_capacity_mwh: u32,
    pub current_capacity_mwh: u32,
}

/// CPU 信息
#[derive(Debug, Clone, Default)]
pub struct CpuInfo {
    pub name: String,
    pub manufacturer: String,
    pub cores: u32,
    pub logical_processors: u32,
    pub max_clock_speed: u32,
    pub current_clock_speed: u32,
    pub l2_cache_size: u32,
    pub l3_cache_size: u32,
    pub architecture: String,
    pub supports_ai: bool,
}

/// 内存条信息
#[derive(Debug, Clone, Default)]
pub struct MemoryStickInfo {
    pub capacity: u64,
    pub speed: u32,
    pub manufacturer: String,
    pub part_number: String,
    pub bank_label: String,
    pub device_locator: String,
    pub memory_type: String,
}

/// 内存信息
#[derive(Debug, Clone, Default)]
pub struct MemoryInfo {
    pub total_physical: u64,
    pub available_physical: u64,
    pub total_virtual: u64,
    pub available_virtual: u64,
    pub memory_load: u32,
    pub sticks: Vec<MemoryStickInfo>,
    pub slot_count: u32,
}

/// 主板信息
#[derive(Debug, Clone, Default)]
pub struct MotherboardInfo {
    pub manufacturer: String,
    pub product: String,
    pub version: String,
    pub serial_number: String,
}

/// BIOS 信息
#[derive(Debug, Clone, Default)]
pub struct BiosInfo {
    pub manufacturer: String,
    pub version: String,
    pub release_date: String,
    pub smbios_version: String,
}

/// 硬盘信息
#[derive(Debug, Clone, Default)]
pub struct DiskInfo {
    pub model: String,
    pub interface_type: String,
    pub media_type: String,
    pub size: u64,
    pub serial_number: String,
    pub firmware_revision: String,
    pub partitions: u32,
    pub bitlocker_status: BitLockerStatus,
    pub partition_style: String,
    pub is_ssd: bool,
    pub disk_index: u32,
}

/// BitLocker 加密状态
#[derive(Debug, Clone, Default, PartialEq)]
pub enum BitLockerStatus {
    #[default]
    Unknown,
    Encrypted,
    NotEncrypted,
    EncryptionInProgress,
    DecryptionInProgress,
}

/// 显卡信息
#[derive(Debug, Clone, Default)]
pub struct GpuInfo {
    pub name: String,
    pub adapter_compatibility: String,
    pub driver_version: String,
    pub driver_date: String,
    pub video_memory: u64,
    pub current_resolution: String,
    pub refresh_rate: u32,
    pub video_processor: String,
}

/// 操作系统信息
#[derive(Debug, Clone, Default)]
pub struct OsInfo {
    pub name: String,
    pub version: String,
    pub build_number: String,
    pub architecture: String,
    pub product_id: String,
    pub registered_owner: String,
    pub install_date: String,
}

/// 网络适配器信息
#[derive(Debug, Clone, Default)]
pub struct NetworkAdapterInfo {
    pub name: String,
    pub description: String,
    pub mac_address: String,
    pub ip_addresses: Vec<String>,
    pub adapter_type: String,
    pub status: String,
    pub speed: u64,
}

/// 完整硬件信息
#[derive(Debug, Clone, Default)]
pub struct HardwareInfo {
    pub cpu: CpuInfo,
    pub memory: MemoryInfo,
    pub motherboard: MotherboardInfo,
    pub bios: BiosInfo,
    pub disks: Vec<DiskInfo>,
    pub gpus: Vec<GpuInfo>,
    pub os: OsInfo,
    pub computer_name: String,
    pub computer_model: String,
    pub computer_manufacturer: String,
    pub network_adapters: Vec<NetworkAdapterInfo>,
    pub system_bitlocker_status: BitLockerStatus,
    pub system_serial_number: String,
    pub device_type: DeviceType,
    pub battery: Option<BatteryInfo>,
}

#[repr(C)]
#[allow(non_snake_case, dead_code)]
struct STORAGE_DEVICE_DESCRIPTOR {
    Version: u32,
    Size: u32,
    DeviceType: u8,
    DeviceTypeModifier: u8,
    RemovableMedia: u8,
    CommandQueueing: u8,
    VendorIdOffset: u32,
    ProductIdOffset: u32,
    ProductRevisionOffset: u32,
    SerialNumberOffset: u32,
    BusType: u32,
    RawPropertiesLength: u32,
    RawDeviceProperties: [u8; 1],
}

#[repr(C)]
#[allow(non_snake_case, dead_code)]
struct GET_LENGTH_INFORMATION {
    length: i64,
}

#[repr(C)]
#[allow(non_snake_case, dead_code)]
struct SYSTEM_POWER_STATUS {
    ACLineStatus: u8,
    BatteryFlag: u8,
    BatteryLifePercent: u8,
    SystemStatusFlag: u8,
    BatteryLifeTime: u32,
    BatteryFullLifeTime: u32,
}

// ============================================================================
// WMI 辅助模块 - 纯 WinAPI COM 实现
// ============================================================================

/// WMI 连接管理器
/// 用于执行 WMI 查询，替代 wmic 命令行工具
struct WmiConnection {
    services: IWbemServices,
}

/// COM 初始化守卫，确保 COM 正确初始化和清理
struct ComInitGuard {
    initialized: bool,
}

impl ComInitGuard {
    fn new() -> Self {
        let initialized = unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).is_ok()
        };
        if initialized {
            let _ = unsafe {
                CoInitializeSecurity(
                    None,
                    -1,
                    None,
                    None,
                    RPC_C_AUTHN_LEVEL_DEFAULT,
                    RPC_C_IMP_LEVEL_IMPERSONATE,
                    None,
                    EOAC_NONE,
                    None,
                )
            };
        }
        Self { initialized }
    }
}

impl Drop for ComInitGuard {
    fn drop(&mut self) {
        if self.initialized {
            unsafe { CoUninitialize() };
        }
    }
}

// RPC 常量定义
const RPC_C_AUTHN_DEFAULT: u32 = 0xFFFFFFFF;
const RPC_C_AUTHZ_NONE: u32 = 0;

impl WmiConnection {
    /// 连接到指定的 WMI 命名空间
    fn connect(namespace: &str) -> Option<Self> {
        unsafe {
            let locator: IWbemLocator = CoCreateInstance(
                &WbemLocator,
                None,
                CLSCTX_INPROC_SERVER,
            ).ok()?;

            let namespace_bstr = BSTR::from(namespace);
            let services = locator.ConnectServer(
                &namespace_bstr,
                &BSTR::new(),
                &BSTR::new(),
                &BSTR::new(),
                0,
                &BSTR::new(),
                None,
            ).ok()?;

            // CoSetProxyBlanket 正确的 8 个参数:
            // pproxy, dwauthnsvc, dwauthzsvc, pserverprincname, dwauthnlevel, dwimplevel, pauthinfo, dwcapabilities
            CoSetProxyBlanket(
                &services,
                RPC_C_AUTHN_DEFAULT,    // dwauthnsvc - 身份验证服务
                RPC_C_AUTHZ_NONE,       // dwauthzsvc - 授权服务
                None,                   // pserverprincname - 服务器主体名称
                RPC_C_AUTHN_LEVEL_CALL, // dwauthnlevel - 身份验证级别
                RPC_C_IMP_LEVEL_IMPERSONATE, // dwimplevel - 模拟级别
                None,                   // pauthinfo - 身份验证信息
                EOAC_NONE,              // dwcapabilities - 能力标志
            ).ok()?;

            Some(Self { services })
        }
    }

    /// 连接到默认的 root\cimv2 命名空间
    fn connect_cimv2() -> Option<Self> {
        Self::connect("ROOT\\CIMV2")
    }

    /// 执行 WQL 查询
    fn query(&self, wql: &str) -> Option<WmiQueryResult> {
        unsafe {
            let query_lang = BSTR::from("WQL");
            let query_str = BSTR::from(wql);

            let enumerator = self.services.ExecQuery(
                &query_lang,
                &query_str,
                WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                None,
            ).ok()?;

            Some(WmiQueryResult { enumerator })
        }
    }
}

/// WMI 查询结果迭代器
struct WmiQueryResult {
    enumerator: IEnumWbemClassObject,
}

impl Iterator for WmiQueryResult {
    type Item = WmiObject;

    fn next(&mut self) -> Option<Self::Item> {
        unsafe {
            let mut objects: [Option<IWbemClassObject>; 1] = [None];
            let mut returned: u32 = 0;

            let result = self.enumerator.Next(
                5000, // 5秒超时，避免无限等待
                &mut objects,
                &mut returned,
            );

            if result.is_ok() && returned > 0 {
                objects[0].take().map(|obj| WmiObject { inner: obj })
            } else {
                None
            }
        }
    }
}

/// WMI 对象包装器
struct WmiObject {
    inner: IWbemClassObject,
}

impl WmiObject {
    /// 获取字符串属性
    fn get_string(&self, property: &str) -> Option<String> {
        unsafe {
            let prop_name = BSTR::from(property);
            let mut value = VARIANT::default();

            if self.inner.Get(&prop_name, 0, &mut value, None, None).is_ok() {
                variant_to_string(&value)
            } else {
                None
            }
        }
    }

    /// 获取 u32 属性
    fn get_u32(&self, property: &str) -> Option<u32> {
        unsafe {
            let prop_name = BSTR::from(property);
            let mut value = VARIANT::default();

            if self.inner.Get(&prop_name, 0, &mut value, None, None).is_ok() {
                variant_to_u32(&value)
            } else {
                None
            }
        }
    }

    /// 获取 u64 属性
    fn get_u64(&self, property: &str) -> Option<u64> {
        unsafe {
            let prop_name = BSTR::from(property);
            let mut value = VARIANT::default();

            if self.inner.Get(&prop_name, 0, &mut value, None, None).is_ok() {
                variant_to_u64(&value)
            } else {
                None
            }
        }
    }

    /// 获取 u16 数组属性（用于 ChassisTypes）
    fn get_u16_array(&self, property: &str) -> Option<Vec<u16>> {
        unsafe {
            let prop_name = BSTR::from(property);
            let mut value = VARIANT::default();

            if self.inner.Get(&prop_name, 0, &mut value, None, None).is_ok() {
                variant_to_u16_array(&value)
            } else {
                None
            }
        }
    }
}

/// 从 VARIANT 获取 vt 字段的辅助函数
/// 通过直接访问 VARIANT 的内存布局来获取类型标记
#[inline]
fn get_variant_vt(var: &VARIANT) -> VARENUM {
    unsafe {
        // VARIANT 结构的内存布局：前 2 字节是 vt (VARENUM)
        // 参考：https://docs.microsoft.com/en-us/windows/win32/api/oaidl/ns-oaidl-variant
        let var_ptr = var as *const VARIANT as *const u16;
        VARENUM(*var_ptr)
    }
}

/// 将 VARIANT 转换为字符串
fn variant_to_string(var: &VARIANT) -> Option<String> {
    let vt = get_variant_vt(var);
    
    if vt == VT_NULL || vt == VT_EMPTY {
        return None;
    }

    if vt == VT_BSTR {
        // 使用 TryFrom 转换
        if let Ok(bstr) = BSTR::try_from(var) {
            if bstr.is_empty() {
                return None;
            }
            return Some(bstr.to_string());
        }
        return None;
    }

    // 尝试将其他类型转换为字符串
    if vt == VT_I4 {
        if let Ok(val) = i32::try_from(var) {
            return Some(val.to_string());
        }
    }

    if vt == VT_UI4 {
        if let Ok(val) = u32::try_from(var) {
            return Some(val.to_string());
        }
    }

    if vt == VT_I2 {
        if let Ok(val) = i16::try_from(var) {
            return Some(val.to_string());
        }
    }

    if vt == VT_UI2 {
        if let Ok(val) = u16::try_from(var) {
            return Some(val.to_string());
        }
    }

    None
}

/// 将 VARIANT 转换为 u32
fn variant_to_u32(var: &VARIANT) -> Option<u32> {
    let vt = get_variant_vt(var);

    if vt == VT_NULL || vt == VT_EMPTY {
        return None;
    }

    if vt == VT_I4 {
        if let Ok(val) = i32::try_from(var) {
            return Some(val as u32);
        }
    }

    if vt == VT_UI4 {
        if let Ok(val) = u32::try_from(var) {
            return Some(val);
        }
    }

    if vt == VT_I2 {
        if let Ok(val) = i16::try_from(var) {
            return Some(val as u32);
        }
    }

    if vt == VT_UI2 {
        if let Ok(val) = u16::try_from(var) {
            return Some(val as u32);
        }
    }

    if vt == VT_UI1 {
        // 对于 VT_UI1 (u8)，windows-rs 可能没有直接的 TryFrom
        // 尝试通过其他方式获取
        unsafe {
            // 使用原始内存访问作为后备
            let ptr = var as *const VARIANT as *const u8;
            let data_offset = 8; // VARIANT 数据部分的偏移量
            return Some(*ptr.add(data_offset) as u32);
        }
    }

    if vt == VT_BSTR {
        if let Ok(bstr) = BSTR::try_from(var) {
            return bstr.to_string().parse().ok();
        }
    }

    None
}

/// 将 VARIANT 转换为 u64
fn variant_to_u64(var: &VARIANT) -> Option<u64> {
    let vt = get_variant_vt(var);

    if vt == VT_NULL || vt == VT_EMPTY {
        return None;
    }

    if vt == VT_I4 {
        if let Ok(val) = i32::try_from(var) {
            return Some(val as u64);
        }
    }

    if vt == VT_UI4 {
        if let Ok(val) = u32::try_from(var) {
            return Some(val as u64);
        }
    }

    if vt == VT_I8 {
        if let Ok(val) = i64::try_from(var) {
            return Some(val as u64);
        }
    }

    if vt == VT_UI8 {
        if let Ok(val) = u64::try_from(var) {
            return Some(val);
        }
    }

    if vt == VT_BSTR {
        if let Ok(bstr) = BSTR::try_from(var) {
            return bstr.to_string().parse().ok();
        }
    }

    None
}

/// 将 VARIANT 转换为 u16 数组
fn variant_to_u16_array(var: &VARIANT) -> Option<Vec<u16>> {
    unsafe {
        let vt = get_variant_vt(var);

        // 检查是否为数组类型 VT_ARRAY | VT_I2 或 VT_ARRAY | VT_UI2
        let is_array = (vt.0 & VT_ARRAY.0) != 0;
        
        if !is_array {
            // 不是数组，尝试作为单个值处理
            if let Some(val) = variant_to_u32(var) {
                return Some(vec![val as u16]);
            }
            return None;
        }

        // 获取 SAFEARRAY 指针
        // VARIANT 内部结构: vt(2) + wReserved1(2) + wReserved2(2) + wReserved3(2) + data(8 on x64)
        // 数据部分是一个联合体，对于数组类型，它包含一个 SAFEARRAY 指针
        let var_ptr = var as *const VARIANT as *const u8;
        let parray_ptr = var_ptr.add(8) as *const *mut SAFEARRAY;
        let parray = *parray_ptr;
        
        if parray.is_null() {
            return None;
        }

        let sa = &*parray;
        let lower_bound = sa.rgsabound[0].lLbound;
        let count = sa.rgsabound[0].cElements as i32;

        let mut result = Vec::with_capacity(count as usize);

        for i in 0..count {
            let mut element: i32 = 0;
            let index = lower_bound + i;
            if SafeArrayGetElement(parray, &index, &mut element as *mut _ as *mut _).is_ok() {
                result.push(element as u16);
            }
        }

        if result.is_empty() {
            None
        } else {
            Some(result)
        }
    }
}

// ============================================================================
// 使用 WMI 获取硬件信息的函数
// ============================================================================

/// 使用 WMI 获取内存条信息
fn get_memory_sticks_wmi() -> Vec<MemoryStickInfo> {
    let _com = ComInitGuard::new();

    let mut sticks = Vec::new();

    let Some(wmi) = WmiConnection::connect_cimv2() else {
        return sticks;
    };

    let Some(result) = wmi.query("SELECT BankLabel, Capacity, Manufacturer, PartNumber, Speed, DeviceLocator, SMBIOSMemoryType FROM Win32_PhysicalMemory") else {
        return sticks;
    };

    for obj in result {
        let capacity = obj.get_u64("Capacity").unwrap_or(0);
        if capacity == 0 {
            continue;
        }

        let bank_label = obj.get_string("BankLabel").unwrap_or_default();
        let manufacturer = obj.get_string("Manufacturer").unwrap_or_default();
        let part_number = obj.get_string("PartNumber").unwrap_or_default().trim().to_string();
        let speed = obj.get_u32("Speed").unwrap_or(0);
        let device_locator = obj.get_string("DeviceLocator").unwrap_or_default();
        let smbios_memory_type = obj.get_u32("SMBIOSMemoryType").unwrap_or(0);

        let memory_type = match smbios_memory_type {
            20 => "DDR".to_string(),
            21 => "DDR2".to_string(),
            24 => "DDR3".to_string(),
            26 => "DDR4".to_string(),
            34 => "DDR5".to_string(),
            _ => String::new(),
        };

        sticks.push(MemoryStickInfo {
            capacity,
            speed,
            manufacturer,
            part_number,
            bank_label,
            device_locator,
            memory_type,
        });
    }

    sticks
}

/// 使用 WMI 获取内存插槽数
fn get_memory_slot_count_wmi() -> u32 {
    let _com = ComInitGuard::new();

    let Some(wmi) = WmiConnection::connect_cimv2() else {
        return 0;
    };

    let Some(result) = wmi.query("SELECT MemoryDevices FROM Win32_PhysicalMemoryArray") else {
        return 0;
    };

    for obj in result {
        if let Some(slots) = obj.get_u32("MemoryDevices") {
            if slots > 0 {
                return slots;
            }
        }
    }

    0
}

/// 使用 WMI 获取主板序列号
fn get_baseboard_serial_wmi() -> Option<String> {
    let _com = ComInitGuard::new();

    let wmi = WmiConnection::connect_cimv2()?;
    let result = wmi.query("SELECT SerialNumber FROM Win32_BaseBoard")?;

    for obj in result {
        if let Some(serial) = obj.get_string("SerialNumber") {
            let serial = serial.trim().to_string();
            if !serial.is_empty() && !is_placeholder(&serial) {
                return Some(serial);
            }
        }
    }

    None
}

/// 使用 WMI 获取 BIOS 序列号
fn get_bios_serial_wmi() -> Option<String> {
    let _com = ComInitGuard::new();

    let wmi = WmiConnection::connect_cimv2()?;
    let result = wmi.query("SELECT SerialNumber FROM Win32_BIOS")?;

    for obj in result {
        if let Some(serial) = obj.get_string("SerialNumber") {
            let serial = serial.trim().to_string();
            if !serial.is_empty() && !is_placeholder(&serial) {
                return Some(serial);
            }
        }
    }

    None
}

/// 使用 WMI 获取机箱类型
fn get_chassis_types_wmi() -> Option<Vec<u16>> {
    let _com = ComInitGuard::new();

    let wmi = WmiConnection::connect_cimv2()?;
    let result = wmi.query("SELECT ChassisTypes FROM Win32_SystemEnclosure")?;

    for obj in result {
        if let Some(types) = obj.get_u16_array("ChassisTypes") {
            if !types.is_empty() {
                return Some(types);
            }
        }
    }

    None
}

/// 使用 WMI 获取电池信息
fn get_battery_wmi_info() -> (Option<u32>, Option<u32>, Option<String>) {
    let _com = ComInitGuard::new();

    let Some(wmi) = WmiConnection::connect_cimv2() else {
        return (None, None, None);
    };

    let Some(result) = wmi.query("SELECT DesignCapacity, FullChargeCapacity, Name FROM Win32_Battery") else {
        return (None, None, None);
    };

    for obj in result {
        let design_capacity = obj.get_u32("DesignCapacity");
        let full_charge_capacity = obj.get_u32("FullChargeCapacity");
        let name = obj.get_string("Name");
        return (design_capacity, full_charge_capacity, name);
    }

    (None, None, None)
}

/// 使用 WMI 获取便携电池制造商
fn get_portable_battery_manufacturer_wmi() -> Option<String> {
    let _com = ComInitGuard::new();

    let wmi = WmiConnection::connect_cimv2()?;
    let result = wmi.query("SELECT Manufacturer FROM Win32_PortableBattery")?;

    for obj in result {
        if let Some(mfr) = obj.get_string("Manufacturer") {
            let mfr = mfr.trim().to_string();
            if !mfr.is_empty() {
                return Some(mfr);
            }
        }
    }

    None
}

/// 使用 WMI 获取磁盘大小信息
fn get_disk_sizes_wmi() -> HashMap<u32, u64> {
    let _com = ComInitGuard::new();

    let mut sizes = HashMap::new();

    let Some(wmi) = WmiConnection::connect_cimv2() else {
        return sizes;
    };

    let Some(result) = wmi.query("SELECT Index, Size FROM Win32_DiskDrive") else {
        return sizes;
    };

    for obj in result {
        if let (Some(index), Some(size)) = (obj.get_u32("Index"), obj.get_u64("Size")) {
            sizes.insert(index, size);
        }
    }

    sizes
}

/// 使用 WMI 获取 BitLocker 加密状态
/// 通过 Win32_EncryptableVolume 类查询（需要管理员权限）
fn get_bitlocker_status_wmi(drive_letter: &str) -> BitLockerStatus {
    let _com = ComInitGuard::new();

    // BitLocker 信息在 root\cimv2\Security\MicrosoftVolumeEncryption 命名空间
    let Some(wmi) = WmiConnection::connect("ROOT\\CIMV2\\Security\\MicrosoftVolumeEncryption") else {
        // 尝试备用方法：通过注册表检查
        return get_bitlocker_status_registry(drive_letter);
    };

    // 格式化驱动器路径为 WMI 查询格式
    let drive = if drive_letter.ends_with(':') {
        drive_letter.to_uppercase()
    } else {
        format!("{}:", drive_letter.to_uppercase())
    };

    let query = format!(
        "SELECT ProtectionStatus, ConversionStatus FROM Win32_EncryptableVolume WHERE DriveLetter = '{}'",
        drive
    );

    let Some(result) = wmi.query(&query) else {
        return get_bitlocker_status_registry(drive_letter);
    };

    for obj in result {
        // ProtectionStatus:
        // 0 = 保护关闭
        // 1 = 保护开启
        // 2 = 未知
        let protection_status = obj.get_u32("ProtectionStatus").unwrap_or(2);

        // ConversionStatus:
        // 0 = 完全解密
        // 1 = 完全加密
        // 2 = 正在加密
        // 3 = 正在解密
        // 4 = 已暂停加密
        // 5 = 已暂停解密
        let conversion_status = obj.get_u32("ConversionStatus").unwrap_or(0);

        return match conversion_status {
            1 => BitLockerStatus::Encrypted,
            2 | 4 => BitLockerStatus::EncryptionInProgress,
            3 | 5 => BitLockerStatus::DecryptionInProgress,
            0 => {
                // 完全解密，但检查保护状态
                if protection_status == 1 {
                    BitLockerStatus::Encrypted
                } else {
                    BitLockerStatus::NotEncrypted
                }
            }
            _ => BitLockerStatus::Unknown,
        };
    }

    get_bitlocker_status_registry(drive_letter)
}

/// 通过注册表备用检查 BitLocker 状态
fn get_bitlocker_status_registry(drive_letter: &str) -> BitLockerStatus {
    // 尝试通过注册表检查 BitLocker 状态
    // 这是一个备用方法，当 WMI 查询失败时使用

    let drive = if drive_letter.ends_with(':') {
        &drive_letter[..1]
    } else {
        drive_letter
    };

    let subkey = format!("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\BitLocker\\Volumes\\{}", drive);

    if let Some(protection_type) = read_registry_dword(HKEY_LOCAL_MACHINE, &subkey, "ProtectionType") {
        return match protection_type {
            0 => BitLockerStatus::NotEncrypted,
            1 | 2 => BitLockerStatus::Encrypted,
            _ => BitLockerStatus::Unknown,
        };
    }

    // 检查 FVE (Full Volume Encryption) 注册表项
    let fve_subkey = format!(
        "SYSTEM\\CurrentControlSet\\Control\\FVE\\OSBitLocker\\{}",
        drive.to_uppercase()
    );

    if read_registry_dword(HKEY_LOCAL_MACHINE, &fve_subkey, "Enabled").is_some() {
        return BitLockerStatus::Encrypted;
    }

    BitLockerStatus::Unknown
}

// ============================================================================
// HardwareInfo 实现
// ============================================================================

impl HardwareInfo {
    pub fn collect() -> Result<Self, Box<dyn std::error::Error>> {
        let mut info = HardwareInfo::default();
        Self::get_computer_info(&mut info);
        info.os = Self::get_os_info();
        info.cpu = Self::get_cpu_info();
        info.memory = Self::get_memory_info();
        info.motherboard = Self::get_motherboard_info();
        info.bios = Self::get_bios_info();
        info.disks = Self::get_disk_info();
        info.gpus = Self::get_gpu_info();
        info.network_adapters = Self::get_network_adapters();
        info.system_bitlocker_status = Self::get_system_bitlocker_status();
        info.system_serial_number = Self::get_system_serial_number();
        info.device_type = Self::get_device_type();
        info.battery = Self::get_battery_info();
        Ok(info)
    }

    pub fn to_formatted_text(&self, sys_info: Option<&crate::core::system_info::SystemInfo>) -> String {
        let mut lines = Vec::new();
        let arch_str = match self.os.architecture.as_str() {
            "64 位" => "X64", "32 位" => "X86", "ARM64" => "ARM64", _ => &self.os.architecture,
        };
        lines.push(format!("系统名称: {} {} [10.0.{} ({})]", self.os.name, arch_str, self.os.build_number, self.os.version));
        lines.push(format!("计算机名: {}", self.computer_name));
        if !self.os.install_date.is_empty() { lines.push(format!("安装日期: {}", self.os.install_date)); }
        let boot_mode = sys_info.map(|s| format!("{}", s.boot_mode)).unwrap_or_else(|| "未知".to_string());
        lines.push(format!("启动模式: {}  设备类型: {}", boot_mode, self.device_type));
        let tpm_str = if let Some(s) = sys_info { if s.tpm_enabled { format!("已开启 v{}", s.tpm_version) } else { "未开启".to_string() } } else { "未知".to_string() };
        let secure_boot_str = if let Some(s) = sys_info { if s.secure_boot { "已启用" } else { "未启用" } } else { "未知" };
        let bitlocker_str = match self.system_bitlocker_status { BitLockerStatus::Encrypted => "是", BitLockerStatus::NotEncrypted => "否", BitLockerStatus::EncryptionInProgress => "加密中", BitLockerStatus::DecryptionInProgress => "解密中", BitLockerStatus::Unknown => "未知", };
        lines.push(format!(" TPM模块: {} 安全启动: {} BitLocker加密启动: {}", tpm_str, secure_boot_str, bitlocker_str));
        let mfr_beautified = beautify_manufacturer_name(&self.computer_manufacturer);
        lines.push(format!("电脑型号: {} {}", mfr_beautified, self.computer_model));
        lines.push(format!("  制造商: {}", mfr_beautified));
        if !self.system_serial_number.is_empty() { lines.push(format!("设备编号: {}", self.system_serial_number)); }
        let mb_product = if !self.motherboard.product.is_empty() && !is_placeholder(&self.motherboard.product) { &self.motherboard.product } else { "未知" };
        lines.push(format!("主板型号: {}", mb_product));
        let mb_serial = if !self.motherboard.serial_number.is_empty() && !is_placeholder(&self.motherboard.serial_number) { &self.motherboard.serial_number } else { "未知" };
        lines.push(format!("主板编号: {}", mb_serial));
        let mb_version = if !self.motherboard.version.is_empty() && !is_placeholder(&self.motherboard.version) { &self.motherboard.version } else { "N/A" };
        let bios_version = if !self.bios.version.is_empty() { &self.bios.version } else { "未知" };
        let bios_date = if !self.bios.release_date.is_empty() { &self.bios.release_date } else { "未知" };
        lines.push(format!("主板版本: {}  BIOS版本: {}  更新日期: {}", mb_version, bios_version, bios_date));
        lines.push(format!(" CPU型号: {}", self.cpu.name));
        let ai_str = if self.cpu.supports_ai { " [支持AI人工智能]" } else { "" };
        lines.push(format!("  核心数: {} 线程数: {}{}", self.cpu.cores, self.cpu.logical_processors, ai_str));
        let total_gb = self.memory.total_physical as f64 / (1024.0 * 1024.0 * 1024.0);
        let available_gb = self.memory.available_physical as f64 / (1024.0 * 1024.0 * 1024.0);
        lines.push(format!("内存信息: 总大小 {:.0} GB ({:.1} GB可用) 插槽数: {}", total_gb.round(), available_gb, self.memory.slot_count));
        for (i, stick) in self.memory.sticks.iter().enumerate() {
            let mfr = beautify_memory_manufacturer(&stick.manufacturer);
            let capacity_gb = stick.capacity / (1024 * 1024 * 1024);
            let mem_type = if !stick.memory_type.is_empty() { &stick.memory_type } else { "DDR" };
            let part = if !stick.part_number.is_empty() && !is_placeholder(&stick.part_number) { &stick.part_number } else { "Unknown" };
            lines.push(format!("          {}: {} {}/{}GB/{} {}", i + 1, mfr, part, capacity_gb, mem_type, stick.speed));
        }
        if !self.gpus.is_empty() {
            lines.push(format!("显卡信息: 1: {}", beautify_gpu_name(&self.gpus[0].name)));
            for (i, gpu) in self.gpus.iter().skip(1).enumerate() { lines.push(format!("          {}: {}", i + 2, beautify_gpu_name(&gpu.name))); }
        }
        if !self.network_adapters.is_empty() {
            lines.push(format!("网卡信息: 1: {}", self.network_adapters[0].description));
            for (i, adapter) in self.network_adapters.iter().skip(1).enumerate() { lines.push(format!("          {}: {}", i + 2, adapter.description)); }
        }
        if let Some(battery) = &self.battery {
            let charging_str = if battery.is_charging { "充电中" } else if battery.is_ac_connected { "未充电" } else { "放电中" };
            lines.push(format!("电池信息: 当前电量: {}% 充电状态: {}", battery.charge_percent, charging_str));
            if !battery.model.is_empty() && !is_placeholder(&battery.model) { lines.push(format!("    型号: {}", battery.model)); }
            if !battery.manufacturer.is_empty() && !is_placeholder(&battery.manufacturer) { lines.push(format!("  制造商: {} ", beautify_manufacturer_name(&battery.manufacturer))); }
            if battery.design_capacity_mwh > 0 { lines.push(format!("设计容量: {} mWh", battery.design_capacity_mwh)); }
            if battery.full_charge_capacity_mwh > 0 { lines.push(format!("最大容量: {} mWh", battery.full_charge_capacity_mwh)); }
            if battery.current_capacity_mwh > 0 { lines.push(format!("当前容量: {} mWh", battery.current_capacity_mwh)); }
        }
        if !self.disks.is_empty() {
            lines.push(format!("硬盘信息: 1: {}", Self::format_disk_info(&self.disks[0])));
            for (i, disk) in self.disks.iter().skip(1).enumerate() { lines.push(format!("          {}: {}", i + 2, Self::format_disk_info(disk))); }
        }
        lines.join("\n")
    }

    fn format_disk_info(disk: &DiskInfo) -> String {
        let size_gb = disk.size as f64 / (1024.0 * 1024.0 * 1024.0);
        let ssd_str = if disk.is_ssd { "固态" } else { "机械" };
        let partition_style = if !disk.partition_style.is_empty() { &disk.partition_style } else { "未知" };
        format!("{} [{:.1}GB-{}-{}-{}]", disk.model, size_gb, disk.interface_type, partition_style, ssd_str)
    }

    fn get_computer_info(info: &mut HardwareInfo) {
        if let Some(name) = read_registry_string(HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\ComputerName\ComputerName", "ComputerName") { info.computer_name = name; }
        if let Some(manufacturer) = read_registry_string(HKEY_LOCAL_MACHINE, r"HARDWARE\DESCRIPTION\System\BIOS", "SystemManufacturer") { info.computer_manufacturer = manufacturer; }
        if let Some(model) = read_registry_string(HKEY_LOCAL_MACHINE, r"HARDWARE\DESCRIPTION\System\BIOS", "SystemProductName") { info.computer_model = model; }
    }

    fn get_os_info() -> OsInfo {
        let mut os_info = OsInfo::default();
        let nt_path = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
        let build_number: u32 = read_registry_string(HKEY_LOCAL_MACHINE, nt_path, "CurrentBuild").and_then(|s| s.parse().ok()).unwrap_or(0);
        if let Some(name) = read_registry_string(HKEY_LOCAL_MACHINE, nt_path, "ProductName") {
            if build_number >= 22000 && name.contains("Windows 10") { os_info.name = name.replace("Windows 10", "Windows 11"); } else { os_info.name = name; }
        } else {
            os_info.name = if build_number >= 22000 { "Windows 11".to_string() } else if build_number >= 10240 { "Windows 10".to_string() } else { "Windows".to_string() };
        }
        if let Some(display_version) = read_registry_string(HKEY_LOCAL_MACHINE, nt_path, "DisplayVersion") { os_info.version = display_version; }
        else if let Some(release_id) = read_registry_string(HKEY_LOCAL_MACHINE, nt_path, "ReleaseId") { os_info.version = release_id; }
        if build_number > 0 { let ubr = read_registry_dword(HKEY_LOCAL_MACHINE, nt_path, "UBR").map(|u| format!(".{}", u)).unwrap_or_default(); os_info.build_number = format!("{}{}", build_number, ubr); }
        unsafe { let mut sys_info: SYSTEM_INFO = zeroed(); GetNativeSystemInfo(&mut sys_info); os_info.architecture = match sys_info.Anonymous.Anonymous.wProcessorArchitecture.0 { 0 => "32 位".to_string(), 9 => "64 位".to_string(), 12 => "ARM64".to_string(), _ => "未知".to_string(), }; }
        if let Some(product_id) = read_registry_string(HKEY_LOCAL_MACHINE, nt_path, "ProductId") { os_info.product_id = product_id; }
        if let Some(owner) = read_registry_string(HKEY_LOCAL_MACHINE, nt_path, "RegisteredOwner") { os_info.registered_owner = owner; }
        if let Some(install_date) = read_registry_dword(HKEY_LOCAL_MACHINE, nt_path, "InstallDate") { if let Some(dt) = chrono::DateTime::from_timestamp(install_date as i64, 0) { os_info.install_date = dt.format("%Y-%m-%d %H:%M:%S").to_string(); } }
        os_info
    }

    fn get_cpu_info() -> CpuInfo {
        let mut cpu_info = CpuInfo::default();
        unsafe { let mut sys_info: SYSTEM_INFO = zeroed(); GetNativeSystemInfo(&mut sys_info); cpu_info.logical_processors = sys_info.dwNumberOfProcessors; cpu_info.architecture = match sys_info.Anonymous.Anonymous.wProcessorArchitecture.0 { 0 => "x86".to_string(), 9 => "x64".to_string(), 12 => "ARM64".to_string(), _ => "未知".to_string(), }; }
        let cpu_path = r"HARDWARE\DESCRIPTION\System\CentralProcessor\0";
        if let Some(name) = read_registry_string(HKEY_LOCAL_MACHINE, cpu_path, "ProcessorNameString") { cpu_info.name = name.trim().to_string(); cpu_info.supports_ai = check_cpu_ai_support(&cpu_info.name); }
        if let Some(vendor) = read_registry_string(HKEY_LOCAL_MACHINE, cpu_path, "VendorIdentifier") { cpu_info.manufacturer = vendor; }
        if let Some(mhz) = read_registry_dword(HKEY_LOCAL_MACHINE, cpu_path, "~MHz") { cpu_info.max_clock_speed = mhz; cpu_info.current_clock_speed = mhz; }
        cpu_info.cores = get_physical_core_count().unwrap_or(cpu_info.logical_processors);
        cpu_info
    }

    fn get_memory_info() -> MemoryInfo {
        let mut mem_info = MemoryInfo::default();

        // 使用 GlobalMemoryStatusEx 获取内存总量
        unsafe {
            let mut mem_status: MEMORYSTATUSEX = zeroed();
            mem_status.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
            if GlobalMemoryStatusEx(&mut mem_status).is_ok() {
                mem_info.total_physical = mem_status.ullTotalPhys;
                mem_info.available_physical = mem_status.ullAvailPhys;
                mem_info.total_virtual = mem_status.ullTotalVirtual;
                mem_info.available_virtual = mem_status.ullAvailVirtual;
                mem_info.memory_load = mem_status.dwMemoryLoad;
            }
        }

        // 使用 WMI 获取内存条详细信息
        mem_info.sticks = get_memory_sticks_wmi();

        // 使用 WMI 获取内存插槽数
        mem_info.slot_count = get_memory_slot_count_wmi();
        if mem_info.slot_count == 0 && !mem_info.sticks.is_empty() {
            mem_info.slot_count = mem_info.sticks.len() as u32;
        }

        mem_info
    }

    fn get_motherboard_info() -> MotherboardInfo {
        let mut mb_info = MotherboardInfo::default();
        let bios_path = r"HARDWARE\DESCRIPTION\System\BIOS";
        if let Some(manufacturer) = read_registry_string(HKEY_LOCAL_MACHINE, bios_path, "BaseBoardManufacturer") { mb_info.manufacturer = manufacturer; }
        if let Some(product) = read_registry_string(HKEY_LOCAL_MACHINE, bios_path, "BaseBoardProduct") { mb_info.product = product; }
        if let Some(version) = read_registry_string(HKEY_LOCAL_MACHINE, bios_path, "BaseBoardVersion") { mb_info.version = version; }

        // 使用 WMI 获取主板序列号
        if let Some(serial) = get_baseboard_serial_wmi() {
            mb_info.serial_number = serial;
        }

        mb_info
    }

    fn get_bios_info() -> BiosInfo {
        let mut bios_info = BiosInfo::default();
        let bios_path = r"HARDWARE\DESCRIPTION\System\BIOS";
        if let Some(vendor) = read_registry_string(HKEY_LOCAL_MACHINE, bios_path, "BIOSVendor") { bios_info.manufacturer = vendor; }
        if let Some(version) = read_registry_string(HKEY_LOCAL_MACHINE, bios_path, "BIOSVersion") { bios_info.version = version; }
        if let Some(date) = read_registry_string(HKEY_LOCAL_MACHINE, bios_path, "BIOSReleaseDate") { bios_info.release_date = date; }
        if let Some(smbios) = read_registry_string(HKEY_LOCAL_MACHINE, bios_path, "SystemBiosVersion") { bios_info.smbios_version = smbios; }
        bios_info
    }

    fn get_disk_info() -> Vec<DiskInfo> {
        let mut disks = Vec::new();
        let partition_styles = get_disk_partition_styles();

        // 使用 WMI 获取磁盘大小
        let disk_sizes = get_disk_sizes_wmi();

        for i in 0..16 {
            let path = format!(r"\\.\PhysicalDrive{}", i);
            if let Some(mut disk) = query_disk_info(&path) {
                disk.disk_index = i;
                // 使用综合检测方法判断是否为SSD
                disk.is_ssd = detect_disk_is_ssd(i, &disk.model, &disk.interface_type);
                if let Some(style) = partition_styles.get(&i) { disk.partition_style = style.clone(); }
                // 如果DeviceIoControl没有获取到大小，使用WMI的结果
                if disk.size == 0 {
                    if let Some(&size) = disk_sizes.get(&i) { disk.size = size; }
                }
                disks.push(disk);
            }
        }
        disks
    }

    fn get_gpu_info() -> Vec<GpuInfo> {
        let mut gpus = Vec::new();
        unsafe {
            let mut device: DISPLAY_DEVICEW = zeroed();
            device.cb = size_of::<DISPLAY_DEVICEW>() as u32;
            let mut index = 0u32;
            while EnumDisplayDevicesW(PCWSTR::null(), index, &mut device, 0) != BOOL(0) {
                const DISPLAY_DEVICE_ACTIVE_FLAG: u32 = 1;
                if (device.StateFlags & DISPLAY_DEVICE_ACTIVE_FLAG) != 0 {
                    let device_string = wchar_to_string(&device.DeviceString);
                    if !device_string.contains("Remote") && !device_string.is_empty() {
                        let mut gpu = GpuInfo::default();
                        gpu.name = device_string.trim().to_string();
                        if let Some((resolution, refresh)) = get_display_mode(&device.DeviceName) { gpu.current_resolution = resolution; gpu.refresh_rate = refresh; }
                        gpus.push(gpu);
                    }
                }
                index += 1;
                device = zeroed();
                device.cb = size_of::<DISPLAY_DEVICEW>() as u32;
            }
        }
        gpus
    }

    fn get_network_adapters() -> Vec<NetworkAdapterInfo> {
        let mut adapters = Vec::new();
        #[repr(C)] #[allow(non_snake_case)] struct IP_ADDR_STRING { Next: *mut IP_ADDR_STRING, IpAddress: [i8; 16], IpMask: [i8; 16], Context: u32, }
        #[repr(C)] #[allow(non_snake_case)] struct IP_ADAPTER_INFO { Next: *mut IP_ADAPTER_INFO, ComboIndex: u32, AdapterName: [i8; 260], Description: [i8; 132], AddressLength: u32, Address: [u8; 8], Index: u32, Type: u32, DhcpEnabled: u32, CurrentIpAddress: *mut IP_ADDR_STRING, IpAddressList: IP_ADDR_STRING, GatewayList: IP_ADDR_STRING, DhcpServer: IP_ADDR_STRING, HaveWins: i32, PrimaryWinsServer: IP_ADDR_STRING, SecondaryWinsServer: IP_ADDR_STRING, LeaseObtained: i64, LeaseExpires: i64, }
        #[link(name = "iphlpapi")] extern "system" { fn GetAdaptersInfo(AdapterInfo: *mut IP_ADAPTER_INFO, SizePointer: *mut u32) -> u32; }
        unsafe {
            let mut buf_len: u32 = 0;
            let result = GetAdaptersInfo(std::ptr::null_mut(), &mut buf_len);
            if result != 111 && result != 0 { return adapters; }
            if buf_len == 0 { return adapters; }
            let mut buffer: Vec<u8> = vec![0u8; buf_len as usize];
            let adapter_info = buffer.as_mut_ptr() as *mut IP_ADAPTER_INFO;
            if GetAdaptersInfo(adapter_info, &mut buf_len) != 0 { return adapters; }
            let mut current = adapter_info;
            while !current.is_null() {
                let adapter = &*current;
                let description_bytes: Vec<u8> = adapter.Description.iter().take_while(|&&b| b != 0).map(|&b| b as u8).collect();
                let description = String::from_utf8_lossy(&description_bytes).to_string();
                let mac = if adapter.AddressLength > 0 { adapter.Address[..adapter.AddressLength as usize].iter().map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(":") } else { String::new() };
                let mut ip_addresses = Vec::new();
                let ip_bytes: Vec<u8> = adapter.IpAddressList.IpAddress.iter().take_while(|&&b| b != 0).map(|&b| b as u8).collect();
                let ip = String::from_utf8_lossy(&ip_bytes).to_string();
                if !ip.is_empty() && ip != "0.0.0.0" { ip_addresses.push(ip); }
                let adapter_type = match adapter.Type { 6 => "以太网".to_string(), 71 => "无线网络".to_string(), _ => format!("类型 {}", adapter.Type) };
                if !description.is_empty() { adapters.push(NetworkAdapterInfo { name: description.clone(), description, mac_address: mac, ip_addresses, adapter_type, status: "已连接".to_string(), speed: 0 }); }
                current = adapter.Next;
            }
        }
        adapters
    }

    fn get_system_bitlocker_status() -> BitLockerStatus {
        let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
        get_bitlocker_status_wmi(&system_drive)
    }

    fn get_system_serial_number() -> String {
        // 首先尝试从注册表获取
        if let Some(serial) = read_registry_string(HKEY_LOCAL_MACHINE, r"HARDWARE\DESCRIPTION\System\BIOS", "SystemSerialNumber") {
            if !serial.is_empty() && !is_placeholder(&serial) {
                return serial;
            }
        }

        // 使用 WMI 获取 BIOS 序列号
        if let Some(serial) = get_bios_serial_wmi() {
            return serial;
        }

        String::new()
    }

    fn get_device_type() -> DeviceType {
        // 使用 WMI 获取机箱类型
        if let Some(chassis_types) = get_chassis_types_wmi() {
            for chassis_type in chassis_types {
                let device_type = match chassis_type {
                    3 | 4 | 5 | 6 | 7 | 15 | 16 | 35 | 36 => DeviceType::Desktop,
                    8 | 9 | 10 | 11 | 12 | 14 | 18 | 21 | 31 | 32 => DeviceType::Laptop,
                    30 => DeviceType::Tablet,
                    17 | 23 | 28 => DeviceType::Server,
                    _ => DeviceType::Unknown,
                };

                if device_type != DeviceType::Unknown {
                    return device_type;
                }
            }
        }

        // 如果无法通过机箱类型判断，检查是否有电池
        if Self::get_battery_info().is_some() {
            return DeviceType::Laptop;
        }

        DeviceType::Unknown
    }

    fn get_battery_info() -> Option<BatteryInfo> {
        #[link(name = "kernel32")] extern "system" { fn GetSystemPowerStatus(lpSystemPowerStatus: *mut SYSTEM_POWER_STATUS) -> i32; }
        unsafe {
            let mut power_status: SYSTEM_POWER_STATUS = zeroed();
            if GetSystemPowerStatus(&mut power_status) == 0 { return None; }
            if power_status.BatteryFlag == 128 || power_status.BatteryFlag == 255 { return None; }

            let mut battery = BatteryInfo::default();
            battery.charge_percent = if power_status.BatteryLifePercent <= 100 { power_status.BatteryLifePercent } else { 0 };
            battery.is_ac_connected = power_status.ACLineStatus == 1;
            battery.is_charging = (power_status.BatteryFlag & 8) != 0;

            // 使用 WMI 获取电池详细信息
            let (design_capacity, full_charge_capacity, name) = get_battery_wmi_info();
            if let Some(dc) = design_capacity {
                battery.design_capacity_mwh = dc;
            }
            if let Some(fcc) = full_charge_capacity {
                battery.full_charge_capacity_mwh = fcc;
            }
            if let Some(n) = name {
                battery.model = n;
            }

            // 使用 WMI 获取电池制造商
            if let Some(mfr) = get_portable_battery_manufacturer_wmi() {
                battery.manufacturer = mfr;
            }

            if battery.full_charge_capacity_mwh > 0 && battery.charge_percent > 0 {
                battery.current_capacity_mwh = (battery.full_charge_capacity_mwh as f64 * battery.charge_percent as f64 / 100.0) as u32;
            }

            Some(battery)
        }
    }
}

fn check_cpu_ai_support(cpu_name: &str) -> bool {
    let name_lower = cpu_name.to_lowercase();
    if name_lower.contains("core ultra") { return true; }
    if name_lower.contains("ryzen") && (name_lower.contains("7940") || name_lower.contains("7945") || name_lower.contains("7840") || name_lower.contains("7640") || name_lower.contains("ai")) { return true; }
    if name_lower.contains("snapdragon") && name_lower.contains("x") { return true; }
    false
}

/// 使用纯 WinAPI 获取所有物理磁盘的分区样式（GPT/MBR/RAW）
///
/// 通过 IOCTL_DISK_GET_DRIVE_LAYOUT_EX 获取磁盘分区布局信息，
/// 从中提取分区样式字段。
///
/// # Returns
/// HashMap<磁盘编号, 分区样式字符串>，分区样式为 "GPT"、"MBR" 或 "RAW"
fn get_disk_partition_styles() -> HashMap<u32, String> {
    use windows::Win32::System::Ioctl::{IOCTL_DISK_GET_DRIVE_LAYOUT_EX, PARTITION_STYLE_GPT, PARTITION_STYLE_MBR};

    // PARTITION_STYLE_RAW = 2 (Windows SDK winioctl.h)
    const PARTITION_STYLE_RAW_VALUE: u32 = 2;

    /// DRIVE_LAYOUT_INFORMATION_EX 结构体头部
    /// 我们只需要读取 partition_style 字段，不需要完整的分区信息
    #[repr(C)]
    #[allow(non_snake_case, dead_code)]
    struct DriveLayoutInformationExHeader {
        partition_style: u32,
        partition_count: u32,
    }

    let mut styles = HashMap::new();

    // 遍历物理磁盘 0-15（与 get_disk_info 保持一致）
    for disk_index in 0u32..16 {
        let partition_style = unsafe {
            // 构造物理磁盘路径
            let disk_path = format!("\\\\.\\PhysicalDrive{}", disk_index);
            let wide_path: Vec<u16> = disk_path.encode_utf16().chain(std::iter::once(0)).collect();

            // 打开物理磁盘设备
            // 注意：不需要读写权限（传入0），只需要发送 IOCTL 查询
            let handle = match CreateFileW(
                PCWSTR(wide_path.as_ptr()),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                HANDLE::default(),
            ) {
                Ok(h) if h != INVALID_HANDLE_VALUE => h,
                _ => continue, // 磁盘不存在或无法访问，跳过
            };

            // 分配缓冲区用于接收 DRIVE_LAYOUT_INFORMATION_EX
            // 该结构体大小可变，取决于分区数量，但我们只需要头部8字节
            // 使用 4096 字节缓冲区以确保足够容纳最多 128 个分区的信息
            let mut buffer = vec![0u8; 4096];
            let mut bytes_returned: u32 = 0;

            let result = DeviceIoControl(
                handle,
                IOCTL_DISK_GET_DRIVE_LAYOUT_EX,
                None,
                0,
                Some(buffer.as_mut_ptr() as *mut std::ffi::c_void),
                buffer.len() as u32,
                Some(&mut bytes_returned),
                None,
            );

            let _ = CloseHandle(handle);

            // 检查 IOCTL 是否成功，且返回了足够的数据（至少8字节用于头部）
            if result.is_ok() && bytes_returned >= size_of::<DriveLayoutInformationExHeader>() as u32 {
                let layout_header = &*(buffer.as_ptr() as *const DriveLayoutInformationExHeader);

                // 将分区样式常量转换为字符串
                if layout_header.partition_style == PARTITION_STYLE_GPT.0 as u32 {
                    Some("GPT".to_string())
                } else if layout_header.partition_style == PARTITION_STYLE_MBR.0 as u32 {
                    Some("MBR".to_string())
                } else if layout_header.partition_style == PARTITION_STYLE_RAW_VALUE {
                    Some("RAW".to_string())
                } else {
                    // 未知的分区样式值
                    Some(format!("UNKNOWN({})", layout_header.partition_style))
                }
            } else {
                None
            }
        };

        if let Some(style) = partition_style {
            styles.insert(disk_index, style);
        }
    }

    styles
}

// ============================================================================
// 硬盘类型检测模块
// 使用多种方法综合判断硬盘是 SSD 还是 HDD
// ============================================================================

/// 磁盘类型检测结果
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DiskMediaType {
    /// 固态硬盘
    SSD,
    /// 机械硬盘
    HDD,
    /// USB闪存盘（U盘）
    USBFlash,
    /// 无法确定
    Unknown,
}

/// 从 MSFT_PhysicalDisk WMI 类获取的磁盘信息
#[derive(Debug, Clone, Default)]
struct WmiDiskMediaInfo {
    /// DeviceId (对应 PhysicalDrive 编号)
    device_id: String,
    /// 友好名称
    friendly_name: String,
    /// MediaType: 0=Unspecified, 3=HDD, 4=SSD, 5=SCM
    media_type: u16,
    /// BusType: 7=USB, 11=SATA, 17=NVMe 等
    bus_type: u16,
    /// 转速: 0=SSD/闪存, 0xFFFFFFFF=未知HDD, 其他=HDD转速
    spindle_speed: u32,
}

/// 使用 WMI MSFT_PhysicalDisk 类获取所有磁盘的媒体类型信息
/// 这是 Windows 8+ 上最可靠的方法
fn get_wmi_disk_media_info() -> HashMap<u32, WmiDiskMediaInfo> {
    let _com = ComInitGuard::new();
    let mut disk_info_map = HashMap::new();
    
    // 连接到 Storage 命名空间
    let Some(wmi) = WmiConnection::connect("ROOT\\Microsoft\\Windows\\Storage") else {
        return disk_info_map;
    };
    
    // 查询 MSFT_PhysicalDisk
    let Some(result) = wmi.query("SELECT DeviceId, FriendlyName, MediaType, BusType, SpindleSpeed FROM MSFT_PhysicalDisk") else {
        return disk_info_map;
    };
    
    for obj in result {
        let device_id = obj.get_string("DeviceId").unwrap_or_default();
        let friendly_name = obj.get_string("FriendlyName").unwrap_or_default();
        let media_type = obj.get_u32("MediaType").unwrap_or(0) as u16;
        let bus_type = obj.get_u32("BusType").unwrap_or(0) as u16;
        let spindle_speed = obj.get_u32("SpindleSpeed").unwrap_or(0);
        
        // DeviceId 通常是数字字符串，如 "0", "1" 等
        if let Ok(disk_index) = device_id.parse::<u32>() {
            disk_info_map.insert(disk_index, WmiDiskMediaInfo {
                device_id,
                friendly_name,
                media_type,
                bus_type,
                spindle_speed,
            });
        }
    }
    
    disk_info_map
}

/// BusType 常量定义 (来自 MSFT_PhysicalDisk)
mod bus_type {
    pub const UNKNOWN: u16 = 0;
    pub const SCSI: u16 = 1;
    pub const ATAPI: u16 = 2;
    pub const ATA: u16 = 3;
    pub const IEEE1394: u16 = 4;  // FireWire
    pub const SSA: u16 = 5;
    pub const FIBRE_CHANNEL: u16 = 6;
    pub const USB: u16 = 7;
    pub const RAID: u16 = 8;
    pub const ISCSI: u16 = 9;
    pub const SAS: u16 = 10;
    pub const SATA: u16 = 11;
    pub const SD: u16 = 12;       // Secure Digital
    pub const MMC: u16 = 13;      // MultiMedia Card
    pub const VIRTUAL: u16 = 14;
    pub const FILE_BACKED_VIRTUAL: u16 = 15;
    pub const STORAGE_SPACES: u16 = 16;
    pub const NVME: u16 = 17;
    pub const SCM: u16 = 18;      // Storage Class Memory
}

/// MediaType 常量定义 (来自 MSFT_PhysicalDisk)
mod media_type {
    pub const UNSPECIFIED: u16 = 0;
    pub const HDD: u16 = 3;
    pub const SSD: u16 = 4;
    pub const SCM: u16 = 5;  // Storage Class Memory
}

/// 使用 IOCTL_STORAGE_QUERY_PROPERTY 检测硬盘是否有寻道延迟
/// IncursSeekPenalty = false → SSD
/// IncursSeekPenalty = true → HDD
fn detect_seek_penalty(disk_index: u32) -> Option<bool> {
    // StorageDeviceSeekPenaltyProperty = 7
    const STORAGE_DEVICE_SEEK_PENALTY_PROPERTY: u32 = 7;
    const IOCTL_STORAGE_QUERY_PROPERTY_CODE: u32 = 0x002D1400;
    
    #[repr(C)]
    #[allow(non_snake_case, dead_code)]
    struct DEVICE_SEEK_PENALTY_DESCRIPTOR {
        Version: u32,
        Size: u32,
        IncursSeekPenalty: u8,
    }
    
    unsafe {
        let disk_path = format!("\\\\.\\PhysicalDrive{}", disk_index);
        let wide_path: Vec<u16> = disk_path.encode_utf16().chain(std::iter::once(0)).collect();
        
        let handle = match CreateFileW(
            PCWSTR(wide_path.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            HANDLE::default(),
        ) {
            Ok(h) if h != INVALID_HANDLE_VALUE => h,
            _ => return None,
        };
        
        let mut query: STORAGE_PROPERTY_QUERY = zeroed();
        query.PropertyId = windows::Win32::System::Ioctl::STORAGE_PROPERTY_ID(STORAGE_DEVICE_SEEK_PENALTY_PROPERTY as i32);
        query.QueryType = PropertyStandardQuery;
        
        let mut descriptor: DEVICE_SEEK_PENALTY_DESCRIPTOR = zeroed();
        let mut bytes_returned: u32 = 0;
        
        let result = DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY_CODE,
            Some(&query as *const _ as *const std::ffi::c_void),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(&mut descriptor as *mut _ as *mut std::ffi::c_void),
            size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
            Some(&mut bytes_returned),
            None,
        );
        
        let _ = CloseHandle(handle);
        
        if result.is_ok() && bytes_returned >= size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32 {
            return Some(descriptor.IncursSeekPenalty != 0);
        }
        
        None
    }
}

/// 使用 IOCTL 检测硬盘的 Trim 支持
fn detect_trim_support(disk_index: u32) -> Option<bool> {
    const STORAGE_DEVICE_TRIM_PROPERTY: u32 = 8;
    const IOCTL_STORAGE_QUERY_PROPERTY_CODE: u32 = 0x002D1400;
    
    #[repr(C)]
    #[allow(non_snake_case, dead_code)]
    struct DEVICE_TRIM_DESCRIPTOR {
        Version: u32,
        Size: u32,
        TrimEnabled: u8,
    }
    
    unsafe {
        let disk_path = format!("\\\\.\\PhysicalDrive{}", disk_index);
        let wide_path: Vec<u16> = disk_path.encode_utf16().chain(std::iter::once(0)).collect();
        
        let handle = match CreateFileW(
            PCWSTR(wide_path.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            HANDLE::default(),
        ) {
            Ok(h) if h != INVALID_HANDLE_VALUE => h,
            _ => return None,
        };
        
        let mut query: STORAGE_PROPERTY_QUERY = zeroed();
        query.PropertyId = windows::Win32::System::Ioctl::STORAGE_PROPERTY_ID(STORAGE_DEVICE_TRIM_PROPERTY as i32);
        query.QueryType = PropertyStandardQuery;
        
        let mut descriptor: DEVICE_TRIM_DESCRIPTOR = zeroed();
        let mut bytes_returned: u32 = 0;
        
        let result = DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY_CODE,
            Some(&query as *const _ as *const std::ffi::c_void),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(&mut descriptor as *mut _ as *mut std::ffi::c_void),
            size_of::<DEVICE_TRIM_DESCRIPTOR>() as u32,
            Some(&mut bytes_returned),
            None,
        );
        
        let _ = CloseHandle(handle);
        
        if result.is_ok() && bytes_returned >= size_of::<DEVICE_TRIM_DESCRIPTOR>() as u32 {
            return Some(descriptor.TrimEnabled != 0);
        }
        
        None
    }
}

/// 通过型号名称判断是否为已知的SSD
fn is_known_ssd_by_model(model: &str) -> Option<bool> {
    let model_lower = model.to_lowercase();
    
    // 1. 明确的SSD关键词 → 一定是SSD
    let ssd_keywords = ["ssd", "nvme", "solid state", "m.2 pcie"];
    for keyword in &ssd_keywords {
        if model_lower.contains(keyword) { return Some(true); }
    }
    
    // 2. 明确的HDD关键词/系列 → 一定是HDD
    let hdd_series = [
        // Seagate HDD系列
        "barracuda", "ironwolf", "skyhawk", "exos", "firecuda", "backup plus", "expansion",
        // WD HDD系列
        "wd blue", "wd black", "wd red", "wd purple", "wd gold", "wd elements", "my book", "easystore", "my passport ultra",
        // Toshiba HDD系列
        "toshiba dt", "toshiba hdw", "toshiba md", "toshiba p300", "toshiba x300", "toshiba n300", "toshiba s300", "canvio",
        // Hitachi/HGST
        "hitachi", "hgst", "ultrastar", "deskstar", "travelstar",
        // 其他HDD特征
        "hard disk", "hdd"
    ];
    for series in &hdd_series {
        if model_lower.contains(series) {
            // 特殊处理：某些系列同时有HDD和SSD版本，需要检查是否有"ssd"后缀
            if !model_lower.contains("ssd") { return Some(false); }
        }
    }
    
    // 3. 已知SSD品牌和型号前缀
    let ssd_patterns = [
        // 三星 NVMe/SATA SSD
        "samsung 9", "samsung 8", "samsung 7", "mzvl", "mzvp", "mzql", "pm9", "pm981", "pm991",
        // 西数 SSD (WD SN系列是NVMe SSD)
        "wd sn", "wd_black sn", "wd blue sn", "wd green sn",
        // Intel SSD
        "intel ssd", "intel optane", "ssdpe", "ssdsc",
        // 海力士/美光
        "hynix", "micron", "crucial", "p1", "p2", "p3", "p5", "mx500", "bx500",
        // 金士顿 SSD
        "kingston a", "kingston nv", "kingston kc", "kingston snv", "kingston sa", "sa400", "nv1", "nv2",
        // 闪迪
        "sandisk", "extreme pro", "extreme portable", "ultra 3d",
        // 其他品牌
        "adata", "xpg", "plextor", "corsair", "patriot", "pny", "transcend", "lexar",
        // NVMe型号前缀
        "hfm", "pc711", "pc801", "kxg", "thns", "kbg",
        // 国产品牌
        "fanxiang", "梵想", "zhitai", "致态", "changjiang", "长江", "gloway", "光威",
        "netac", "朗科", "colorful", "七彩虹", "aigo", "爱国者", "hikvision", "海康威视",
        "orico ssd", "kioxia", "铠侠",
        // 移动SSD
        "t5", "t7", "x5", "my passport ssd"
    ];
    for pattern in &ssd_patterns {
        if model_lower.contains(pattern) { return Some(true); }
    }
    
    // 4. USB闪存盘品牌/关键词
    let usb_flash_patterns = [
        "datatraveler", "cruzer", "sandisk ultra", "usb flash", "flash drive",
        "transcend jetflash", "pny attache", "lexar jumpdrive", "kingston dtig", "kingston dtse"
    ];
    for pattern in &usb_flash_patterns {
        if model_lower.contains(pattern) { return Some(false); } // 不是SSD，也不是HDD
    }
    
    // 无法通过型号判断
    None
}

/// 通过型号名称判断USB设备是否为外置机械硬盘
fn is_known_usb_hdd(model: &str) -> bool {
    let model_lower = model.to_lowercase();
    
    // 已知的USB外置机械硬盘品牌/系列
    let usb_hdd_patterns = [
        "wd elements", "wd easystore", "wd my book", "wd my passport ultra",
        "seagate expansion", "seagate backup plus", "seagate portable", "seagate game drive",
        "toshiba canvio", "toshiba store",
        "lacie", "g-drive", "g-technology",
        "transcend storejet",
        "silicon power armor"
    ];
    
    for pattern in &usb_hdd_patterns {
        if model_lower.contains(pattern) { return true; }
    }
    
    false
}

/// 通过型号名称判断USB设备是否为外置SSD
fn is_known_usb_ssd(model: &str) -> bool {
    let model_lower = model.to_lowercase();
    
    // 已知的USB外置SSD品牌/系列
    let usb_ssd_patterns = [
        "samsung t5", "samsung t7", "samsung x5",
        "sandisk extreme portable", "sandisk extreme pro portable",
        "wd my passport ssd", "wd_black p50", "wd_black p40",
        "crucial x6", "crucial x8", "crucial x9", "crucial x10",
        "seagate fast ssd", "seagate one touch ssd",
        "lacie rugged ssd", "lacie portable ssd",
        "nvme portable", "usb ssd", "portable ssd"
    ];
    
    for pattern in &usb_ssd_patterns {
        if model_lower.contains(pattern) { return true; }
    }
    
    false
}

/// 综合检测磁盘类型
/// 返回 true 表示是 SSD，false 表示是 HDD
fn detect_disk_is_ssd(disk_index: u32, model: &str, interface: &str) -> bool {
    // 获取 WMI 信息（如果可用）
    static WMI_DISK_INFO: std::sync::OnceLock<HashMap<u32, WmiDiskMediaInfo>> = std::sync::OnceLock::new();
    let wmi_info = WMI_DISK_INFO.get_or_init(get_wmi_disk_media_info);
    
    let wmi_disk = wmi_info.get(&disk_index);
    
    // =====================================================
    // 第1层：接口类型快速判断
    // =====================================================
    
    // NVMe 接口 → 一定是 SSD
    if interface.to_uppercase() == "NVME" { return true; }
    if let Some(info) = wmi_disk {
        if info.bus_type == bus_type::NVME { return true; }
        // SCM (Storage Class Memory) → 类似 SSD
        if info.bus_type == bus_type::SCM { return true; }
    }
    
    // =====================================================
    // 第2层：WMI MSFT_PhysicalDisk MediaType（最可靠）
    // =====================================================
    
    if let Some(info) = wmi_disk {
        match info.media_type {
            media_type::SSD => return true,
            media_type::HDD => return false,
            media_type::SCM => return true,
            media_type::UNSPECIFIED => {
                // MediaType = 0 时需要进一步判断
                // 检查 SpindleSpeed
                if info.spindle_speed > 0 && info.spindle_speed != 0xFFFFFFFF {
                    // 有明确转速 → HDD
                    return false;
                }
                // SpindleSpeed = 0 可能是 SSD 或 USB 闪存盘
                // SpindleSpeed = 0xFFFFFFFF 是未知转速的 HDD
                if info.spindle_speed == 0xFFFFFFFF {
                    // 未知转速，很可能是老式 HDD
                    return false;
                }
                
                // USB 设备特殊处理
                if info.bus_type == bus_type::USB {
                    // USB 设备需要更精细的判断
                    if is_known_usb_ssd(model) { return true; }
                    if is_known_usb_hdd(model) { return false; }
                    // USB 闪存盘和小型 USB SSD 通常 SpindleSpeed = 0
                    // 无法确定时，默认为非 SSD（避免误判）
                }
            }
            _ => {}
        }
    }
    
    // =====================================================
    // 第3层：IOCTL 检测
    // =====================================================
    
    // SeekPenalty 检测（非常可靠）
    if let Some(has_seek_penalty) = detect_seek_penalty(disk_index) {
        // 无寻道延迟 = SSD
        if !has_seek_penalty {
            // 但要排除 USB 闪存盘的误判
            if let Some(info) = wmi_disk {
                if info.bus_type == bus_type::USB && !is_known_usb_ssd(model) {
                    // USB 设备且不是已知的 USB SSD，可能是 U 盘
                    // 继续其他检测
                } else {
                    return true;
                }
            } else {
                return true;
            }
        } else {
            // 有寻道延迟 = HDD
            return false;
        }
    }
    
    // Trim 支持检测（辅助参考）
    if let Some(has_trim) = detect_trim_support(disk_index) {
        if has_trim {
            // 支持 Trim 通常是 SSD
            // 但某些 HDD 也可能支持，需要结合型号判断
            if let Some(is_ssd) = is_known_ssd_by_model(model) {
                return is_ssd;
            }
            // 如果支持 Trim 且不是已知 HDD，认为是 SSD
            return true;
        }
    }
    
    // =====================================================
    // 第4层：型号名称匹配（最后手段）
    // =====================================================
    
    if let Some(is_ssd) = is_known_ssd_by_model(model) {
        return is_ssd;
    }
    
    // =====================================================
    // 默认判断
    // =====================================================
    
    // 根据接口类型做最后推断
    match interface.to_uppercase().as_str() {
        "NVME" => true,   // 前面应该已经处理了
        "SCSI" | "SAS" => false,  // 服务器磁盘通常是 HDD
        "USB" => {
            // USB 设备默认为非 SSD（保守判断）
            // 因为 USB 闪存盘很常见，误判会困扰用户
            false
        }
        _ => {
            // SATA/ATA 等接口无法确定时，保守判断为 HDD
            false
        }
    }
}

/// 检查字符串是否为占位符值（如 "To Be Filled", "Default string" 等）
pub fn is_placeholder_str(s: &str) -> bool {
    let lower = s.to_lowercase();
    lower.contains("to be filled") || lower.contains("default string") || lower == "none" || lower == "n/a" || lower == "unknown" || lower.is_empty()
}

fn is_placeholder(s: &str) -> bool {
    is_placeholder_str(s)
}

pub fn beautify_manufacturer_name(name: &str) -> String {
    let name_lower = name.to_lowercase();
    if name_lower.contains("asus") || name_lower.contains("asustek") { return "华硕电脑".to_string(); }
    if name_lower.contains("lenovo") { return "联想".to_string(); }
    if name_lower.contains("dell") { return "戴尔".to_string(); }
    if name_lower.contains("hp") || name_lower.contains("hewlett") { return "惠普".to_string(); }
    if name_lower.contains("acer") { return "宏碁".to_string(); }
    if name_lower.contains("msi") || name_lower.contains("micro-star") { return "微星".to_string(); }
    if name_lower.contains("gigabyte") { return "技嘉".to_string(); }
    if name_lower.contains("huawei") { return "华为".to_string(); }
    if name_lower.contains("xiaomi") { return "小米".to_string(); }
    if name_lower.contains("honor") { return "荣耀".to_string(); }
    if name_lower.contains("samsung") { return "三星".to_string(); }
    if name_lower.contains("apple") { return "苹果".to_string(); }
    if name_lower.contains("microsoft") { return "微软".to_string(); }
    if name_lower.contains("razer") { return "雷蛇".to_string(); }
    if name_lower.contains("alienware") { return "外星人".to_string(); }
    name.to_string()
}

pub fn beautify_memory_manufacturer(name: &str) -> String {
    let name_lower = name.to_lowercase();
    if name_lower.contains("micron") { return "镁光".to_string(); }
    if name_lower.contains("samsung") { return "三星".to_string(); }
    if name_lower.contains("hynix") { return "海力士".to_string(); }
    if name_lower.contains("kingston") { return "金士顿".to_string(); }
    if name_lower.contains("corsair") { return "海盗船".to_string(); }
    if name_lower.contains("g.skill") || name_lower.contains("gskill") { return "芝奇".to_string(); }
    if name_lower.contains("crucial") { return "英睿达".to_string(); }
    if name_lower.contains("adata") { return "威刚".to_string(); }
    if name.is_empty() || is_placeholder(name) { return "未知".to_string(); }
    name.to_string()
}

pub fn beautify_gpu_name(name: &str) -> String {
    let mut result = name.to_string();
    if result.to_lowercase().contains("nvidia") { result = result.replace("NVIDIA", "英伟达").replace("nvidia", "英伟达"); }
    if result.to_lowercase().contains("intel") && !result.contains("英特尔") { result = result.replace("Intel", "英特尔").replace("intel", "英特尔"); }
    result
}

fn read_registry_string(hkey: HKEY, subkey: &str, value_name: &str) -> Option<String> {
    unsafe {
        let subkey_wide: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
        let value_name_wide: Vec<u16> = value_name.encode_utf16().chain(std::iter::once(0)).collect();
        let mut key_handle: HKEY = HKEY::default();
        if RegOpenKeyExW(hkey, PCWSTR(subkey_wide.as_ptr()), 0, KEY_READ, &mut key_handle).is_err() { return None; }
        let mut buffer: Vec<u8> = vec![0u8; 1024];
        let mut buffer_size = buffer.len() as u32;
        let mut value_type: REG_VALUE_TYPE = REG_VALUE_TYPE(0);
        let result = RegQueryValueExW(key_handle, PCWSTR(value_name_wide.as_ptr()), None, Some(&mut value_type), Some(buffer.as_mut_ptr()), Some(&mut buffer_size));
        let _ = RegCloseKey(key_handle);
        if result.is_err() || value_type.0 != 1 { return None; }
        let len = (buffer_size as usize) / 2;
        if len > 0 { let wide: Vec<u16> = buffer[..len * 2].chunks(2).map(|c| u16::from_le_bytes([c[0], c.get(1).copied().unwrap_or(0)])).collect(); let s = OsString::from_wide(&wide[..wide.len().saturating_sub(1)]); return Some(s.to_string_lossy().to_string()); }
        None
    }
}

fn read_registry_dword(hkey: HKEY, subkey: &str, value_name: &str) -> Option<u32> {
    unsafe {
        let subkey_wide: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
        let value_name_wide: Vec<u16> = value_name.encode_utf16().chain(std::iter::once(0)).collect();
        let mut key_handle: HKEY = HKEY::default();
        if RegOpenKeyExW(hkey, PCWSTR(subkey_wide.as_ptr()), 0, KEY_READ, &mut key_handle).is_err() { return None; }
        let mut value: u32 = 0;
        let mut buffer_size = size_of::<u32>() as u32;
        let mut value_type: REG_VALUE_TYPE = REG_VALUE_TYPE(0);
        let result = RegQueryValueExW(key_handle, PCWSTR(value_name_wide.as_ptr()), None, Some(&mut value_type), Some(&mut value as *mut u32 as *mut u8), Some(&mut buffer_size));
        let _ = RegCloseKey(key_handle);
        if result.is_err() || value_type.0 != 4 { return None; }
        Some(value)
    }
}

fn wchar_to_string(wchars: &[u16]) -> String { let len = wchars.iter().position(|&c| c == 0).unwrap_or(wchars.len()); OsString::from_wide(&wchars[..len]).to_string_lossy().to_string() }

fn query_disk_info(path: &str) -> Option<DiskInfo> {
    unsafe {
        let path_wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let handle = match CreateFileW(PCWSTR(path_wide.as_ptr()), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), HANDLE::default()) { Ok(h) if h != INVALID_HANDLE_VALUE => h, _ => return None };
        let mut query: STORAGE_PROPERTY_QUERY = zeroed();
        query.PropertyId = StorageDeviceProperty;
        query.QueryType = PropertyStandardQuery;
        let mut buffer = vec![0u8; 4096];
        let mut bytes_returned: u32 = 0;
        if DeviceIoControl(handle, IOCTL_STORAGE_QUERY_PROPERTY, Some(&query as *const _ as *const std::ffi::c_void), size_of::<STORAGE_PROPERTY_QUERY>() as u32, Some(buffer.as_mut_ptr() as *mut std::ffi::c_void), buffer.len() as u32, Some(&mut bytes_returned), None).is_err() || bytes_returned == 0 { let _ = CloseHandle(handle); return None; }
        let descriptor = &*(buffer.as_ptr() as *const STORAGE_DEVICE_DESCRIPTOR);
        let mut disk = DiskInfo::default();
        if descriptor.ProductIdOffset > 0 && (descriptor.ProductIdOffset as usize) < buffer.len() { let offset = descriptor.ProductIdOffset as usize; if let Some(end) = buffer[offset..].iter().position(|&b| b == 0) { disk.model = String::from_utf8_lossy(&buffer[offset..offset + end]).trim().to_string(); } }
        if descriptor.SerialNumberOffset > 0 && (descriptor.SerialNumberOffset as usize) < buffer.len() { let offset = descriptor.SerialNumberOffset as usize; if let Some(end) = buffer[offset..].iter().position(|&b| b == 0) { disk.serial_number = String::from_utf8_lossy(&buffer[offset..offset + end]).trim().to_string(); } }
        if descriptor.ProductRevisionOffset > 0 && (descriptor.ProductRevisionOffset as usize) < buffer.len() { let offset = descriptor.ProductRevisionOffset as usize; if let Some(end) = buffer[offset..].iter().position(|&b| b == 0) { disk.firmware_revision = String::from_utf8_lossy(&buffer[offset..offset + end]).trim().to_string(); } }
        disk.interface_type = match descriptor.BusType { 1 => "SCSI".to_string(), 3 => "ATA".to_string(), 7 => "USB".to_string(), 11 => "SATA".to_string(), 17 => "NVMe".to_string(), _ => format!("Unknown({})", descriptor.BusType) };
        disk.media_type = if descriptor.RemovableMedia != 0 { "可移动".to_string() } else { "固定".to_string() };
        let mut length_info: GET_LENGTH_INFORMATION = zeroed();
        let mut bytes_ret: u32 = 0;
        if DeviceIoControl(handle, IOCTL_DISK_GET_LENGTH_INFO, None, 0, Some(&mut length_info as *mut _ as *mut std::ffi::c_void), size_of::<GET_LENGTH_INFORMATION>() as u32, Some(&mut bytes_ret), None).is_ok() { disk.size = length_info.length as u64; }
        let _ = CloseHandle(handle);
        if !disk.model.is_empty() || disk.size > 0 { Some(disk) } else { None }
    }
}

fn get_display_mode(device_name: &[u16]) -> Option<(String, u32)> {
    unsafe {
        let mut devmode: DEVMODEW = zeroed();
        devmode.dmSize = size_of::<DEVMODEW>() as u16;
        if EnumDisplaySettingsW(PCWSTR(device_name.as_ptr()), ENUM_CURRENT_SETTINGS, &mut devmode) != BOOL(0) { return Some((format!("{}x{}", devmode.dmPelsWidth, devmode.dmPelsHeight), devmode.dmDisplayFrequency)); }
        None
    }
}

fn get_physical_core_count() -> Option<u32> {
    #[repr(C)] #[allow(non_snake_case)] struct SYSTEM_LOGICAL_PROCESSOR_INFORMATION { ProcessorMask: usize, Relationship: u32, Reserved: [u64; 2], }
    const RELATION_PROCESSOR_CORE: u32 = 0;
    #[link(name = "kernel32")] extern "system" { fn GetLogicalProcessorInformation(buffer: *mut SYSTEM_LOGICAL_PROCESSOR_INFORMATION, return_length: *mut u32) -> i32; }
    unsafe {
        let mut length: u32 = 0;
        let _ = GetLogicalProcessorInformation(std::ptr::null_mut(), &mut length);
        if length == 0 { return None; }
        let count = length as usize / size_of::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION>();
        let mut buffer: Vec<SYSTEM_LOGICAL_PROCESSOR_INFORMATION> = Vec::with_capacity(count);
        buffer.set_len(count);
        if GetLogicalProcessorInformation(buffer.as_mut_ptr(), &mut length) == 0 { return None; }
        let actual_count = length as usize / size_of::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION>();
        let physical_cores = buffer[..actual_count].iter().filter(|info| info.Relationship == RELATION_PROCESSOR_CORE).count() as u32;
        if physical_cores > 0 { Some(physical_cores) } else { None }
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024; const MB: u64 = KB * 1024; const GB: u64 = MB * 1024; const TB: u64 = GB * 1024;
    if bytes >= TB { format!("{:.2} TB", bytes as f64 / TB as f64) } else if bytes >= GB { format!("{:.2} GB", bytes as f64 / GB as f64) } else if bytes >= MB { format!("{:.2} MB", bytes as f64 / MB as f64) } else if bytes >= KB { format!("{:.2} KB", bytes as f64 / KB as f64) } else { format!("{} Bytes", bytes) }
}