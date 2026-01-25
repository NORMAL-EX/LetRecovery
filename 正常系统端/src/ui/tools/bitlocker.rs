//! BitLocker解锁模块
//!
//! 提供BitLocker加密分区的检测和解锁功能
//! 使用Windows API和WMI实现
//! 不依赖外部的manage-bde.exe命令

use std::path::Path;

#[cfg(windows)]
use windows::{
    core::{BSTR, PCWSTR},
    Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDriveTypeW, GetVolumeInformationW,
    },
    Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoInitializeSecurity, CoSetProxyBlanket,
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, EOAC_NONE, RPC_C_AUTHN_LEVEL_DEFAULT,
        RPC_C_IMP_LEVEL_IMPERSONATE,
    },
    Win32::System::Wmi::{
        IWbemClassObject, IWbemLocator, IWbemServices, WbemLocator, WBEM_FLAG_FORWARD_ONLY,
        WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_GENERIC_FLAG_TYPE,
    },
};

/// 驱动器类型常量
const DRIVE_FIXED: u32 = 3;

/// BitLocker状态枚举
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BitLockerStatus {
    /// 未加密
    NotEncrypted,
    /// 已加密已解锁
    EncryptedUnlocked,
    /// 已加密已锁定
    EncryptedLocked,
    /// 正在加密
    Encrypting,
    /// 正在解密
    Decrypting,
    /// 状态未知
    Unknown,
}

impl BitLockerStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            BitLockerStatus::NotEncrypted => "未加密",
            BitLockerStatus::EncryptedUnlocked => "已解锁",
            BitLockerStatus::EncryptedLocked => "已锁定",
            BitLockerStatus::Encrypting => "正在加密",
            BitLockerStatus::Decrypting => "正在解密",
            BitLockerStatus::Unknown => "未知",
        }
    }
}

/// BitLocker分区信息
#[derive(Debug, Clone)]
pub struct BitLockerPartition {
    /// 盘符（如 "D:"）
    pub letter: String,
    /// 卷标
    pub label: String,
    /// 总大小（MB）
    pub total_size_mb: u64,
    /// BitLocker状态
    pub status: BitLockerStatus,
    /// 保护方法描述
    pub protection_method: String,
}

/// BitLocker解锁结果
#[derive(Debug, Clone)]
pub struct UnlockResult {
    /// 盘符
    pub letter: String,
    /// 是否成功
    pub success: bool,
    /// 消息
    pub message: String,
}

/// 获取当前系统盘符
fn get_system_drive() -> String {
    std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string())
}

/// WMI连接助手
#[cfg(windows)]
struct WmiHelper {
    services: IWbemServices,
}

#[cfg(windows)]
impl WmiHelper {
    /// 创建到BitLocker命名空间的WMI连接
    fn connect() -> Result<Self, String> {
        unsafe {
            // 初始化COM
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

            // 设置安全级别
            let _ = CoInitializeSecurity(
                None,
                -1,
                None,
                None,
                RPC_C_AUTHN_LEVEL_DEFAULT,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
                None,
            );

            // 创建WbemLocator
            let locator: IWbemLocator =
                CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)
                    .map_err(|e| format!("创建WbemLocator失败: {}", e))?;

            // 连接到BitLocker命名空间
            let namespace = BSTR::from("root\\CIMV2\\Security\\MicrosoftVolumeEncryption");
            let services = locator
                .ConnectServer(&namespace, None, None, None, 0, None, None)
                .map_err(|e| format!("连接WMI命名空间失败: {}", e))?;

            // 设置代理
            CoSetProxyBlanket(
                &services,
                10,
                0,
                None,
                RPC_C_AUTHN_LEVEL_DEFAULT,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
            )
            .map_err(|e| format!("设置代理失败: {}", e))?;

            Ok(WmiHelper { services })
        }
    }

    /// 查询BitLocker卷是否存在
    fn query_volume(&self, drive_letter: char) -> Option<IWbemClassObject> {
        unsafe {
            let query = BSTR::from(format!(
                "SELECT * FROM Win32_EncryptableVolume WHERE DriveLetter = '{}:'",
                drive_letter.to_ascii_uppercase()
            ));
            let wql = BSTR::from("WQL");
            let flags = WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY;

            let enumerator = self.services.ExecQuery(&wql, &query, flags, None).ok()?;

            let mut objects = [None; 1];
            let mut returned = 0u32;

            let hr = enumerator.Next(-1, &mut objects, &mut returned);
            if hr.is_ok() && returned > 0 {
                objects[0].take()
            } else {
                None
            }
        }
    }

    /// 执行WMI方法
    fn exec_method(
        &self,
        volume: &IWbemClassObject,
        method_name: &str,
        param_name: &str,
        param_value: &str,
    ) -> Result<u32, String> {
        unsafe {
            // 获取对象路径
            let path_prop = BSTR::from("__PATH");
            let mut path_var = windows::core::VARIANT::default();
            volume
                .Get(&path_prop, 0, &mut path_var, None, None)
                .map_err(|e| format!("获取对象路径失败: {}", e))?;

            // 提取路径字符串 - 使用BSTR的VT类型
            let path_str = extract_bstr_from_variant(&path_var)
                .ok_or_else(|| "解析对象路径失败".to_string())?;

            // 获取类定义
            let class_name = BSTR::from("Win32_EncryptableVolume");
            let wbem_flags = WBEM_GENERIC_FLAG_TYPE(0);

            let mut class_obj = None;
            self.services
                .GetObject(&class_name, wbem_flags, None, Some(&mut class_obj), None)
                .map_err(|e| format!("获取类定义失败: {}", e))?;

            let class_obj = class_obj.ok_or_else(|| "类对象为空".to_string())?;

            // 获取方法定义
            let method_bstr = BSTR::from(method_name);
            let mut in_params_def = None;
            let mut out_params_def = None;
            class_obj
                .GetMethod(&method_bstr, 0, &mut in_params_def, &mut out_params_def)
                .map_err(|e| format!("获取方法定义失败: {}", e))?;

            let in_params_def =
                in_params_def.ok_or_else(|| "输入参数定义为空".to_string())?;

            // 创建输入参数实例
            let in_params = in_params_def
                .SpawnInstance(0)
                .map_err(|e| format!("创建参数实例失败: {}", e))?;

            // 设置参数值
            let param_bstr = BSTR::from(param_name);
            let value_var = create_bstr_variant(param_value);
            in_params
                .Put(&param_bstr, 0, &value_var, 0)
                .map_err(|e| format!("设置参数失败: {}", e))?;

            // 执行方法
            let path_bstr = BSTR::from(path_str);
            let mut out_params = None;

            self.services
                .ExecMethod(
                    &path_bstr,
                    &method_bstr,
                    wbem_flags,
                    None,
                    &in_params,
                    Some(&mut out_params),
                    None,
                )
                .map_err(|e| format!("执行方法失败: {}", e))?;

            // 获取返回值
            if let Some(out) = out_params {
                let return_name = BSTR::from("ReturnValue");
                let mut return_var = windows::core::VARIANT::default();
                out.Get(&return_name, 0, &mut return_var, None, None)
                    .map_err(|e| format!("获取返回值失败: {}", e))?;

                let ret_code = extract_i32_from_variant(&return_var).unwrap_or(-1) as u32;
                return Ok(ret_code);
            }

            Err("无法获取返回值".to_string())
        }
    }
}

/// 从VARIANT提取BSTR字符串
#[cfg(windows)]
fn extract_bstr_from_variant(var: &windows::core::VARIANT) -> Option<String> {
    // windows-core 0.58中VARIANT使用了不同的API
    // 使用try_into来安全转换
    unsafe {
        // 获取原始数据指针
        let raw_ptr = var as *const windows::core::VARIANT as *const u8;
        // VARIANT结构: vt(2) + reserved(6) + data(8/16)
        let vt = *(raw_ptr as *const u16);

        // VT_BSTR = 8
        if vt == 8 {
            #[cfg(target_pointer_width = "64")]
            let bstr_ptr = *(raw_ptr.add(8) as *const *const u16);
            #[cfg(target_pointer_width = "32")]
            let bstr_ptr = *(raw_ptr.add(8) as *const *const u16);

            if !bstr_ptr.is_null() {
                // BSTR的长度前缀在指针前4字节
                let len_ptr = (bstr_ptr as *const u8).sub(4) as *const u32;
                let byte_len = *len_ptr;
                let char_len = byte_len as usize / 2;

                if char_len > 0 && char_len < 10000 {
                    let slice = std::slice::from_raw_parts(bstr_ptr, char_len);
                    return Some(String::from_utf16_lossy(slice));
                }
            }
        }
        None
    }
}

/// 从VARIANT提取i32
#[cfg(windows)]
fn extract_i32_from_variant(var: &windows::core::VARIANT) -> Option<i32> {
    unsafe {
        let raw_ptr = var as *const windows::core::VARIANT as *const u8;
        let vt = *(raw_ptr as *const u16);

        match vt {
            2 => Some(*(raw_ptr.add(8) as *const i16) as i32), // VT_I2
            3 => Some(*(raw_ptr.add(8) as *const i32)),        // VT_I4
            18 => Some(*(raw_ptr.add(8) as *const u16) as i32), // VT_UI2
            19 => Some(*(raw_ptr.add(8) as *const u32) as i32), // VT_UI4
            _ => None,
        }
    }
}

/// 创建包含BSTR的VARIANT
#[cfg(windows)]
fn create_bstr_variant(s: &str) -> windows::core::VARIANT {
    unsafe {
        let bstr = BSTR::from(s);
        let mut var: windows::core::VARIANT = std::mem::zeroed();

        // 获取原始数据指针
        let raw_ptr = &mut var as *mut windows::core::VARIANT as *mut u8;

        // 设置VT_BSTR = 8
        *(raw_ptr as *mut u16) = 8;

        // 设置BSTR指针
        let bstr_raw = bstr.as_ptr();
        #[cfg(target_pointer_width = "64")]
        {
            *(raw_ptr.add(8) as *mut *const u16) = bstr_raw;
        }
        #[cfg(target_pointer_width = "32")]
        {
            *(raw_ptr.add(8) as *mut *const u16) = bstr_raw;
        }

        // 防止BSTR被drop
        std::mem::forget(bstr);

        var
    }
}

/// 使用文件系统访问检测BitLocker状态
#[cfg(windows)]
fn check_bitlocker_status_via_fs(drive: &str) -> BitLockerStatus {
    let drive_path = format!("{}\\", drive);

    if !Path::new(&drive_path).exists() {
        return BitLockerStatus::Unknown;
    }

    // 尝试读取目录
    match std::fs::read_dir(&drive_path) {
        Ok(_) => {
            // 可以读取，检查是否是BitLocker卷
            if is_bitlocker_volume(drive) {
                BitLockerStatus::EncryptedUnlocked
            } else {
                BitLockerStatus::NotEncrypted
            }
        }
        Err(e) => {
            if let Some(code) = e.raw_os_error() {
                match code {
                    5 | 21 | 1392 => BitLockerStatus::EncryptedLocked,
                    _ => BitLockerStatus::Unknown,
                }
            } else {
                BitLockerStatus::Unknown
            }
        }
    }
}

/// 检查是否是BitLocker卷
#[cfg(windows)]
fn is_bitlocker_volume(drive: &str) -> bool {
    match WmiHelper::connect() {
        Ok(wmi) => {
            let drive_letter = drive.chars().next().unwrap_or('C');
            wmi.query_volume(drive_letter).is_some()
        }
        Err(_) => false,
    }
}

#[cfg(not(windows))]
fn check_bitlocker_status_via_fs(_drive: &str) -> BitLockerStatus {
    BitLockerStatus::Unknown
}

#[cfg(not(windows))]
fn is_bitlocker_volume(_drive: &str) -> bool {
    false
}

/// 获取所有BitLocker加密的分区
pub fn get_bitlocker_partitions() -> Vec<BitLockerPartition> {
    let mut partitions = Vec::new();

    for letter in b'A'..=b'Z' {
        let drive_letter = (letter as char).to_string();
        let drive = format!("{}:", drive_letter);
        let drive_path = format!("{}\\", drive);

        // 跳过X:盘（PE系统盘）
        if drive_letter.to_uppercase() == "X" {
            continue;
        }

        // 检查驱动器类型
        #[cfg(windows)]
        {
            let wide_path: Vec<u16> =
                drive_path.encode_utf16().chain(std::iter::once(0)).collect();
            let drive_type = unsafe { GetDriveTypeW(PCWSTR(wide_path.as_ptr())) };

            if drive_type != DRIVE_FIXED && drive_type != 0 {
                continue;
            }
        }

        // 检查BitLocker状态
        let status = check_bitlocker_status_via_fs(&drive);

        // 只添加BitLocker加密的分区
        if status == BitLockerStatus::EncryptedLocked
            || status == BitLockerStatus::EncryptedUnlocked
            || status == BitLockerStatus::Encrypting
            || status == BitLockerStatus::Decrypting
        {
            let (label, total_size_mb) = get_volume_info(&drive);

            partitions.push(BitLockerPartition {
                letter: drive,
                label,
                total_size_mb,
                status,
                protection_method: "密码/恢复密钥".to_string(),
            });
        }
    }

    partitions
}

/// 获取所有已锁定的BitLocker分区
pub fn get_locked_bitlocker_partitions() -> Vec<BitLockerPartition> {
    get_bitlocker_partitions()
        .into_iter()
        .filter(|p| p.status == BitLockerStatus::EncryptedLocked)
        .collect()
}

/// 获取卷信息
#[cfg(windows)]
fn get_volume_info(drive: &str) -> (String, u64) {
    let path = format!("{}\\", drive);
    let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

    let mut volume_name = [0u16; 261];
    let mut total_bytes: u64 = 0;

    unsafe {
        let _ = GetVolumeInformationW(
            PCWSTR(wide_path.as_ptr()),
            Some(&mut volume_name),
            None,
            None,
            None,
            None,
        );

        let _ = GetDiskFreeSpaceExW(
            PCWSTR(wide_path.as_ptr()),
            None,
            Some(&mut total_bytes as *mut u64),
            None,
        );
    }

    let label = String::from_utf16_lossy(&volume_name)
        .trim_end_matches('\0')
        .to_string();
    let total_size_mb = total_bytes / 1024 / 1024;

    (label, total_size_mb)
}

#[cfg(not(windows))]
fn get_volume_info(_drive: &str) -> (String, u64) {
    (String::new(), 0)
}

/// 使用密码解锁BitLocker分区
#[cfg(windows)]
pub fn unlock_with_password(drive: &str, password: &str) -> UnlockResult {
    log::info!("尝试使用密码解锁 BitLocker 分区: {}", drive);

    let drive_letter = drive.chars().next().unwrap_or('C');

    match WmiHelper::connect() {
        Ok(wmi) => {
            match wmi.query_volume(drive_letter) {
                Some(volume) => {
                    match wmi.exec_method(&volume, "UnlockWithPassphrase", "Passphrase", password) {
                        Ok(ret_code) => {
                            if ret_code == 0 {
                                log::info!("BitLocker 分区 {} 解锁成功", drive);
                                UnlockResult {
                                    letter: drive.to_string(),
                                    success: true,
                                    message: "解锁成功".to_string(),
                                }
                            } else {
                                let msg = format!("解锁失败，错误码: {} ({})", ret_code, get_fve_error_message(ret_code));
                                log::error!("BitLocker 分区 {} {}", drive, msg);
                                UnlockResult {
                                    letter: drive.to_string(),
                                    success: false,
                                    message: msg,
                                }
                            }
                        }
                        Err(e) => UnlockResult {
                            letter: drive.to_string(),
                            success: false,
                            message: e,
                        },
                    }
                }
                None => UnlockResult {
                    letter: drive.to_string(),
                    success: false,
                    message: "未找到指定的加密卷".to_string(),
                },
            }
        }
        Err(e) => UnlockResult {
            letter: drive.to_string(),
            success: false,
            message: format!("WMI连接失败: {}", e),
        },
    }
}

#[cfg(not(windows))]
pub fn unlock_with_password(_drive: &str, _password: &str) -> UnlockResult {
    UnlockResult {
        letter: String::new(),
        success: false,
        message: "仅支持Windows系统".to_string(),
    }
}

/// 使用恢复密钥解锁BitLocker分区
#[cfg(windows)]
pub fn unlock_with_recovery_key(drive: &str, recovery_key: &str) -> UnlockResult {
    log::info!("尝试使用恢复密钥解锁 BitLocker 分区: {}", drive);

    let drive_letter = drive.chars().next().unwrap_or('C');
    let formatted_key = recovery_key.trim().replace(" ", "-");

    match WmiHelper::connect() {
        Ok(wmi) => {
            match wmi.query_volume(drive_letter) {
                Some(volume) => {
                    match wmi.exec_method(
                        &volume,
                        "UnlockWithNumericalPassword",
                        "NumericalPassword",
                        &formatted_key,
                    ) {
                        Ok(ret_code) => {
                            if ret_code == 0 {
                                log::info!("BitLocker 分区 {} 使用恢复密钥解锁成功", drive);
                                UnlockResult {
                                    letter: drive.to_string(),
                                    success: true,
                                    message: "解锁成功".to_string(),
                                }
                            } else {
                                let msg = format!("解锁失败，错误码: {} ({})", ret_code, get_fve_error_message(ret_code));
                                log::error!("BitLocker 分区 {} {}", drive, msg);
                                UnlockResult {
                                    letter: drive.to_string(),
                                    success: false,
                                    message: msg,
                                }
                            }
                        }
                        Err(e) => UnlockResult {
                            letter: drive.to_string(),
                            success: false,
                            message: e,
                        },
                    }
                }
                None => UnlockResult {
                    letter: drive.to_string(),
                    success: false,
                    message: "未找到指定的加密卷".to_string(),
                },
            }
        }
        Err(e) => UnlockResult {
            letter: drive.to_string(),
            success: false,
            message: format!("WMI连接失败: {}", e),
        },
    }
}

#[cfg(not(windows))]
pub fn unlock_with_recovery_key(_drive: &str, _recovery_key: &str) -> UnlockResult {
    UnlockResult {
        letter: String::new(),
        success: false,
        message: "仅支持Windows系统".to_string(),
    }
}

/// 获取FVE错误消息
fn get_fve_error_message(code: u32) -> &'static str {
    match code {
        0 => "成功",
        0x80310000 => "BitLocker未启用/卷未加密",  // FVE_E_NOT_ENCRYPTED = 2150694912
        0x80310001 => "已解锁",
        0x80310002 => "密钥不匹配",
        0x80310003 => "密钥保护器不存在",
        0x80310019 => "密码错误",
        0x8031001A => "恢复密钥错误",
        0x8031006E => "TPM未就绪",
        0x80310008 => "密码错误", // FVE_E_FAILED_AUTHENTICATION (2150694920)
        _ => "未知错误",
    }
}

/// 检测是否有任何BitLocker加密的分区
pub fn has_bitlocker_partitions() -> bool {
    !get_bitlocker_partitions().is_empty()
}

/// 检测是否有已锁定的BitLocker分区
pub fn has_locked_bitlocker_partitions() -> bool {
    !get_locked_bitlocker_partitions().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_system_drive() {
        let drive = get_system_drive();
        assert!(drive.len() >= 2);
        assert!(drive.ends_with(':'));
    }
}
