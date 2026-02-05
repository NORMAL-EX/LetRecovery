//! FVEAPI.dll 动态加载模块
//!
//! 提供对Windows BitLocker驱动器加密API的底层访问。
//! fveapi.dll是Windows未公开的API，本模块基于逆向工程分析实现。
//!
//! # 结构体说明（通过逆向工程确认）
//!
//! ## FVE_STATUS_INFO / FVE_GET_STATUS_OUTPUT 结构体（版本2，0x80字节）
//!
//! 关键字段偏移（已通过反汇编验证）：
//! - +0x00 dwSize: 结构体大小 (0x80 = 128)
//! - +0x04 dwVersion: 版本号 (2)
//! - +0x0C dwConversionStatus: 转换状态 (0-5)
//! - +0x10 dblPercentComplete: 加密百分比 (0.0-100.0)
//! - +0x38 dwProtectionStatus: 保护状态 (0=off/已解锁, 1=on/已锁定)
//! - +0x70 dwEncryptionFlags: 加密标志 (掩码 0x17F)
//!
//! # 安全说明
//! - 所有FFI调用都在unsafe块中
//! - 使用RAII模式确保句柄正确释放
//! - 所有字符串转换都经过安全检查

use std::ffi::c_void;
use std::sync::OnceLock;

use libloading::Library;

// Windows类型说明（用于FFI注释）
// LPCWSTR -> *const u16
// HANDLE -> *mut c_void

/// 加密标志检测掩码（来自逆向分析 @ 0x18000d76a: test dword [rsi+0x70], 0x17f）
const FVE_FLAG_CHECK_MASK: u32 = 0x0000017F;

/// FveOpenVolumeW 访问模式标志
/// 根据逆向分析，FveConversionDecryptEx 需要 mode=1（写权限）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FveAccessMode {
    /// 只读模式 - 用于状态查询、解锁验证等不修改卷状态的操作
    ReadOnly = 0,
    /// 读写模式 - 用于解密、加密等需要修改卷状态的操作
    ReadWrite = 1,
}

/// FVE API错误码
/// 基于fveapi.dll逆向分析结果和Windows SDK定义
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FveError {
    /// 成功
    Success = 0,
    /// 无效参数 (E_INVALIDARG)
    InvalidParameter = 0x80070057,
    /// 访问被拒绝 (E_ACCESSDENIED)
    AccessDenied = 0x80070005,
    /// 卷已锁定，需要密码解锁 (FVE_E_LOCKED_VOLUME)
    VolumeLocked = 0x80310000,
    /// 卷不支持BitLocker
    NotSupported = 0x80310001,
    /// 卷未加密/不是BitLocker卷 (FVE_E_NOT_ENCRYPTED)
    NotEncrypted = 0x80310008,
    /// 需要认证密钥 (FVE_E_KEY_REQUIRED)
    KeyRequired = 0x80310044,
    /// 认证失败 (FVE_E_FAILED_AUTHENTICATION)
    AuthenticationFailed = 0x8031000D,
    /// 密码错误
    BadPassword = 0x80310027,
    /// 恢复密钥错误
    BadRecoveryPassword = 0x80310028,
    /// 卷已解锁
    VolumeUnlocked = 0x80310023,
    /// 不是BitLocker卷
    NotBitLockerVolume = 0x80310049,
    /// 卷已移除
    VolumeRemoved = 0x8031004A,
    /// 未知错误
    Unknown = 0xFFFFFFFF,
}

impl FveError {
    /// 从错误码创建FveError
    pub fn from_hresult(code: u32) -> Self {
        match code {
            0 => FveError::Success,
            0x80070057 => FveError::InvalidParameter,
            0x80070005 => FveError::AccessDenied,
            0x80310000 => FveError::VolumeLocked,
            0x80310001 => FveError::NotSupported,
            0x80310008 => FveError::NotEncrypted,
            0x80310044 => FveError::KeyRequired,
            0x8031000D => FveError::AuthenticationFailed,
            0x80310027 => FveError::BadPassword,
            0x80310028 => FveError::BadRecoveryPassword,
            0x80310023 => FveError::VolumeUnlocked,
            0x80310049 => FveError::NotBitLockerVolume,
            0x8031004A => FveError::VolumeRemoved,
            _ => FveError::Unknown,
        }
    }

    /// 获取原始错误码
    pub fn code(&self) -> u32 {
        *self as u32
    }

    /// 检查是否表示卷未加密（包括多种相关错误）
    pub fn indicates_not_encrypted(&self) -> bool {
        matches!(
            self,
            FveError::NotEncrypted | FveError::NotBitLockerVolume | FveError::NotSupported
        )
    }
    
    /// 检查是否表示卷需要解锁
    pub fn indicates_locked(&self) -> bool {
        matches!(
            self,
            FveError::VolumeLocked | FveError::KeyRequired | FveError::AuthenticationFailed
        )
    }
}

impl From<u32> for FveError {
    fn from(code: u32) -> Self {
        Self::from_hresult(code)
    }
}

impl std::fmt::Display for FveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FveError::Success => write!(f, "操作成功"),
            FveError::InvalidParameter => write!(f, "无效参数"),
            FveError::AccessDenied => write!(f, "访问被拒绝，请以管理员权限运行"),
            FveError::VolumeLocked => write!(f, "卷已锁定，需要密码解锁"),
            FveError::NotSupported => write!(f, "卷不支持BitLocker"),
            FveError::NotEncrypted => write!(f, "卷未启用BitLocker加密"),
            FveError::KeyRequired => write!(f, "需要认证密钥"),
            FveError::AuthenticationFailed => write!(f, "认证失败"),
            FveError::BadPassword => write!(f, "密码错误"),
            FveError::BadRecoveryPassword => write!(f, "恢复密钥错误"),
            FveError::VolumeUnlocked => write!(f, "卷已解锁"),
            FveError::NotBitLockerVolume => write!(f, "不是BitLocker加密卷"),
            FveError::VolumeRemoved => write!(f, "卷已移除"),
            FveError::Unknown => write!(f, "未知错误"),
        }
    }
}

impl std::error::Error for FveError {}

/// BitLocker卷转换状态（来自FveGetStatus）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FveVolumeStatus {
    /// 完全解密（未加密）
    FullyDecrypted = 0,
    /// 完全加密
    FullyEncrypted = 1,
    /// 正在加密
    EncryptionInProgress = 2,
    /// 正在解密
    DecryptionInProgress = 3,
    /// 加密暂停
    EncryptionPaused = 4,
    /// 解密暂停
    DecryptionPaused = 5,
}

impl From<u32> for FveVolumeStatus {
    fn from(value: u32) -> Self {
        match value {
            0 => FveVolumeStatus::FullyDecrypted,
            1 => FveVolumeStatus::FullyEncrypted,
            2 => FveVolumeStatus::EncryptionInProgress,
            3 => FveVolumeStatus::DecryptionInProgress,
            4 => FveVolumeStatus::EncryptionPaused,
            5 => FveVolumeStatus::DecryptionPaused,
            _ => {
                log::warn!(
                    "未知的 FveVolumeStatus 值: {}, 将视为 FullyDecrypted",
                    value
                );
                FveVolumeStatus::FullyDecrypted
            }
        }
    }
}

/// BitLocker保护状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FveProtectionStatus {
    /// 保护关闭（已解锁）
    Off = 0,
    /// 保护开启（已锁定）
    On = 1,
    /// 未知
    Unknown = 2,
}

impl From<u32> for FveProtectionStatus {
    fn from(value: u32) -> Self {
        match value {
            0 => FveProtectionStatus::Off,
            1 => FveProtectionStatus::On,
            _ => FveProtectionStatus::Unknown,
        }
    }
}

/// BitLocker锁定状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FveLockStatus {
    /// 已解锁（可访问）
    Unlocked = 0,
    /// 已锁定（需要密码）
    Locked = 1,
}

impl From<u32> for FveLockStatus {
    fn from(value: u32) -> Self {
        match value {
            0 => FveLockStatus::Unlocked,
            _ => FveLockStatus::Locked,
        }
    }
}

/// FVE_GET_STATUS_OUTPUT 结构体（版本2，0x80字节）
///
/// 根据fveapi.dll逆向工程分析确认的结构体布局。
/// 这是 FveGetStatusW 和 FveGetStatus 函数使用的输出结构。
///
/// 关键字段偏移（已验证）：
/// - +0x00 dwSize: 结构体大小，必须设置为 0x80 (128)
/// - +0x04 dwVersion: 版本号，必须设置为 2
/// - +0x0C dwConversionStatus: 转换状态 (0=解密, 1=加密, 2-5=转换中)
/// - +0x10 dblPercentComplete: 加密百分比 (0.0-100.0)
/// - +0x38 dwProtectionStatus: 保护状态 (0=关闭/已解锁, 1=开启/已锁定)
/// - +0x50 qwVolumeSize: 卷大小（字节）
/// - +0x58 qwEncryptedSize: 已加密大小（字节）
/// - +0x70 dwEncryptionFlags: 加密标志
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FveGetStatusOutput {
    /// +0x00: 结构体大小（必须为 0x80 = 128）
    pub size: u32,
    /// +0x04: 版本号（必须为 2）
    pub version: u32,
    /// +0x08: 保留字段
    reserved1: u32,
    /// +0x0C: 转换状态 (0-5)
    pub conversion_status: u32,
    /// +0x10: 加密百分比 (0.0-100.0)
    pub percent_complete: f64,
    /// +0x18: 保留字段数组 (0x20字节, 到0x37)
    reserved2: [u8; 0x20],
    /// +0x38: 保护状态 (0=关闭/已解锁, 1=开启/已锁定)
    pub protection_status: u32,
    /// +0x3C: 保留字段数组 (0x14字节, 到0x4F)
    reserved3: [u8; 0x14],
    /// +0x50: 卷大小（字节）
    pub volume_size: u64,
    /// +0x58: 已加密大小（字节）
    pub encrypted_size: u64,
    /// +0x60: 保留字段数组 (0x10字节, 到0x6F)
    reserved4: [u8; 0x10],
    /// +0x70: 加密标志
    pub encryption_flags: u32,
    /// +0x74: 保留字段数组 (0x0C字节, 到0x7F)
    reserved5: [u8; 0x0C],
}

// 确保结构体大小正确
const _: () = assert!(std::mem::size_of::<FveGetStatusOutput>() == 0x80);

impl Default for FveGetStatusOutput {
    fn default() -> Self {
        Self {
            size: 0x80,
            version: 2,
            reserved1: 0,
            conversion_status: 0,
            percent_complete: 0.0,
            reserved2: [0; 0x20],
            protection_status: 0,
            reserved3: [0; 0x14],
            volume_size: 0,
            encrypted_size: 0,
            reserved4: [0; 0x10],
            encryption_flags: 0,
            reserved5: [0; 0x0C],
        }
    }
}

impl FveGetStatusOutput {
    /// 创建新的状态输出结构
    pub fn new() -> Self {
        Self::default()
    }

    /// 检查卷是否已加密
    pub fn is_encrypted(&self) -> bool {
        // 使用加密标志掩码检查
        (self.encryption_flags & FVE_FLAG_CHECK_MASK) != 0
            || self.conversion_status == FveVolumeStatus::FullyEncrypted as u32
            || self.conversion_status == FveVolumeStatus::EncryptionInProgress as u32
            || self.conversion_status == FveVolumeStatus::EncryptionPaused as u32
            || self.conversion_status == FveVolumeStatus::DecryptionInProgress as u32
            || self.conversion_status == FveVolumeStatus::DecryptionPaused as u32
    }

    /// 检查卷是否已锁定
    pub fn is_locked(&self) -> bool {
        self.protection_status == FveProtectionStatus::On as u32
    }

    /// 获取转换状态枚举
    pub fn get_volume_status(&self) -> FveVolumeStatus {
        FveVolumeStatus::from(self.conversion_status)
    }

    /// 获取保护状态枚举
    pub fn get_protection_status(&self) -> FveProtectionStatus {
        FveProtectionStatus::from(self.protection_status)
    }

    /// 获取锁定状态枚举
    pub fn get_lock_status(&self) -> FveLockStatus {
        FveLockStatus::from(self.protection_status)
    }
}

/// BitLocker卷信息（解析后的状态信息）
#[derive(Debug, Clone)]
pub struct FveVolumeInfo {
    /// 卷状态
    pub volume_status: FveVolumeStatus,
    /// 保护状态
    pub protection_status: FveProtectionStatus,
    /// 锁定状态
    pub lock_status: FveLockStatus,
    /// 加密百分比
    pub encryption_percentage: u8,
    /// 加密标志
    pub encryption_flags: u32,
    /// 卷大小（字节）
    pub volume_size: u64,
    /// 已加密大小（字节）
    pub encrypted_size: u64,
}

impl From<&FveGetStatusOutput> for FveVolumeInfo {
    fn from(output: &FveGetStatusOutput) -> Self {
        Self {
            volume_status: output.get_volume_status(),
            protection_status: output.get_protection_status(),
            lock_status: output.get_lock_status(),
            encryption_percentage: output.percent_complete.round().clamp(0.0, 100.0) as u8,
            encryption_flags: output.encryption_flags,
            volume_size: output.volume_size,
            encrypted_size: output.encrypted_size,
        }
    }
}

// ==================== FFI 函数类型定义 ====================

#[cfg(windows)]
type FnFveOpenVolumeW = unsafe extern "system" fn(
    volume_path: *const u16,  // LPCWSTR
    flags: u32,               // DWORD
    ph_volume: *mut *mut c_void, // HANDLE*
) -> u32; // HRESULT

#[cfg(windows)]
type FnFveCloseVolume = unsafe extern "system" fn(
    h_volume: *mut c_void, // HANDLE
) -> u32; // HRESULT

#[cfg(windows)]
type FnFveGetStatusW = unsafe extern "system" fn(
    volume_path: *const u16,        // LPCWSTR
    status_info: *mut FveGetStatusOutput, // PFVE_STATUS_INFO
) -> u32; // HRESULT

#[cfg(windows)]
type FnFveGetStatus = unsafe extern "system" fn(
    h_volume: *mut c_void,          // HANDLE
    status_info: *mut FveGetStatusOutput, // PFVE_STATUS_INFO
) -> u32; // HRESULT

#[cfg(windows)]
type FnFveUnlockVolume = unsafe extern "system" fn(
    h_volume: *mut c_void,      // HANDLE
    auth_element: *mut c_void,  // PFVE_AUTH_ELEMENT
) -> u32; // HRESULT

#[cfg(windows)]
type FnFveLockVolume = unsafe extern "system" fn(
    h_volume: *mut c_void, // HANDLE
    dismount_first: u32,   // BOOL
) -> u32; // HRESULT

#[cfg(windows)]
type FnFveConversionDecrypt = unsafe extern "system" fn(
    h_volume: *mut c_void, // HANDLE
) -> u32; // HRESULT

#[cfg(windows)]
type FnFveConversionDecryptEx = unsafe extern "system" fn(
    h_volume: *mut c_void, // HANDLE
    flags: u32,            // DWORD
) -> u32; // HRESULT

#[cfg(windows)]
type FnFveAuthElementFromPassPhraseW = unsafe extern "system" fn(
    passphrase: *const u16,         // LPCWSTR
    pp_auth_element: *mut *mut c_void, // PFVE_AUTH_ELEMENT*
) -> u32; // HRESULT

#[cfg(windows)]
type FnFveAuthElementFromRecoveryPasswordW = unsafe extern "system" fn(
    recovery_password: *const u16,  // LPCWSTR
    pp_auth_element: *mut *mut c_void, // PFVE_AUTH_ELEMENT*
) -> u32; // HRESULT

// ==================== FveApi 实现 ====================

/// FveApi 全局单例
#[cfg(windows)]
static FVE_API_INSTANCE: OnceLock<Result<FveApi, String>> = OnceLock::new();

/// FVE API 封装
#[cfg(windows)]
pub struct FveApi {
    _library: Library,
    fn_open_volume: FnFveOpenVolumeW,
    fn_close_volume: FnFveCloseVolume,
    fn_get_status_w: FnFveGetStatusW,
    fn_get_status: FnFveGetStatus,
    fn_unlock_volume: FnFveUnlockVolume,
    fn_lock_volume: FnFveLockVolume,
    fn_conversion_decrypt: FnFveConversionDecrypt,
    fn_conversion_decrypt_ex: FnFveConversionDecryptEx,
    fn_auth_from_passphrase: FnFveAuthElementFromPassPhraseW,
    fn_auth_from_recovery: FnFveAuthElementFromRecoveryPasswordW,
}

#[cfg(windows)]
unsafe impl Send for FveApi {}
#[cfg(windows)]
unsafe impl Sync for FveApi {}

#[cfg(windows)]
impl FveApi {
    /// 获取全局 FveApi 实例
    pub fn instance() -> Result<&'static FveApi, String> {
        FVE_API_INSTANCE
            .get_or_init(|| Self::load())
            .as_ref()
            .map_err(|e| e.clone())
    }

    /// 加载 fveapi.dll
    fn load() -> Result<Self, String> {
        log::info!("正在加载 fveapi.dll...");

        let library = unsafe { Library::new("fveapi.dll") }
            .map_err(|e| format!("无法加载 fveapi.dll: {}", e))?;

        // 在unsafe块中获取所有函数指针，然后立即解引用
        // 这样可以避免Symbol生命周期与library move的冲突
        let (
            fn_open_volume,
            fn_close_volume,
            fn_get_status_w,
            fn_get_status,
            fn_unlock_volume,
            fn_lock_volume,
            fn_conversion_decrypt,
            fn_conversion_decrypt_ex,
            fn_auth_from_passphrase,
            fn_auth_from_recovery,
        ) = unsafe {
            let fn_open_volume: FnFveOpenVolumeW = *library
                .get::<FnFveOpenVolumeW>(b"FveOpenVolumeW")
                .map_err(|e| format!("找不到 FveOpenVolumeW: {}", e))?;
            let fn_close_volume: FnFveCloseVolume = *library
                .get::<FnFveCloseVolume>(b"FveCloseVolume")
                .map_err(|e| format!("找不到 FveCloseVolume: {}", e))?;
            let fn_get_status_w: FnFveGetStatusW = *library
                .get::<FnFveGetStatusW>(b"FveGetStatusW")
                .map_err(|e| format!("找不到 FveGetStatusW: {}", e))?;
            let fn_get_status: FnFveGetStatus = *library
                .get::<FnFveGetStatus>(b"FveGetStatus")
                .map_err(|e| format!("找不到 FveGetStatus: {}", e))?;
            let fn_unlock_volume: FnFveUnlockVolume = *library
                .get::<FnFveUnlockVolume>(b"FveUnlockVolume")
                .map_err(|e| format!("找不到 FveUnlockVolume: {}", e))?;
            let fn_lock_volume: FnFveLockVolume = *library
                .get::<FnFveLockVolume>(b"FveLockVolume")
                .map_err(|e| format!("找不到 FveLockVolume: {}", e))?;
            let fn_conversion_decrypt: FnFveConversionDecrypt = *library
                .get::<FnFveConversionDecrypt>(b"FveConversionDecrypt")
                .map_err(|e| format!("找不到 FveConversionDecrypt: {}", e))?;
            let fn_conversion_decrypt_ex: FnFveConversionDecryptEx = *library
                .get::<FnFveConversionDecryptEx>(b"FveConversionDecryptEx")
                .map_err(|e| format!("找不到 FveConversionDecryptEx: {}", e))?;
            let fn_auth_from_passphrase: FnFveAuthElementFromPassPhraseW = *library
                .get::<FnFveAuthElementFromPassPhraseW>(b"FveAuthElementFromPassPhraseW")
                .map_err(|e| format!("找不到 FveAuthElementFromPassPhraseW: {}", e))?;
            let fn_auth_from_recovery: FnFveAuthElementFromRecoveryPasswordW = *library
                .get::<FnFveAuthElementFromRecoveryPasswordW>(b"FveAuthElementFromRecoveryPasswordW")
                .map_err(|e| format!("找不到 FveAuthElementFromRecoveryPasswordW: {}", e))?;

            (
                fn_open_volume,
                fn_close_volume,
                fn_get_status_w,
                fn_get_status,
                fn_unlock_volume,
                fn_lock_volume,
                fn_conversion_decrypt,
                fn_conversion_decrypt_ex,
                fn_auth_from_passphrase,
                fn_auth_from_recovery,
            )
        };

        log::info!("fveapi.dll 加载成功，所有函数已获取");

        Ok(Self {
            _library: library,
            fn_open_volume,
            fn_close_volume,
            fn_get_status_w,
            fn_get_status,
            fn_unlock_volume,
            fn_lock_volume,
            fn_conversion_decrypt,
            fn_conversion_decrypt_ex,
            fn_auth_from_passphrase,
            fn_auth_from_recovery,
        })
    }

    /// 通过路径直接获取卷状态（推荐方法，无需打开句柄）
    ///
    /// # 参数
    /// - `volume_path`: 卷路径，支持多种格式：
    ///   - 简单盘符: `C:` 或 `D:`
    ///   - 带反斜杠: `C:\` 或 `D:\`
    ///   - 设备路径: `\\.\C:` 或 `\\?\Volume{GUID}`
    ///
    /// # 返回
    /// 成功返回 FveVolumeInfo，失败返回 FveError
    pub fn get_status_by_path(&self, volume_path: &str) -> Result<FveVolumeInfo, FveError> {
        // 标准化路径格式：提取盘符并使用简单格式
        let normalized_path = normalize_volume_path(volume_path);
        let wide_path = to_wide_string(&normalized_path);
        let mut status_output = FveGetStatusOutput::new();

        log::debug!(
            "FveGetStatusW 调用: 原始路径='{}', 标准化路径='{}'",
            volume_path,
            normalized_path
        );

        let hr = unsafe { (self.fn_get_status_w)(wide_path.as_ptr(), &mut status_output) };

        if hr == 0 {
            log::debug!(
                "FveGetStatusW 成功: path={}, conversion={}, protection={}, flags=0x{:04X}, percent={}",
                normalized_path,
                status_output.conversion_status,
                status_output.protection_status,
                status_output.encryption_flags,
                status_output.percent_complete
            );
            Ok(FveVolumeInfo::from(&status_output))
        } else {
            let error = FveError::from_hresult(hr);
            log::debug!(
                "FveGetStatusW 返回: path={}, hr=0x{:08X}, error={:?}",
                normalized_path,
                hr,
                error
            );
            Err(error)
        }
    }

    /// 打开卷并返回句柄包装器（只读模式）
    ///
    /// # 参数
    /// - `volume_path`: 卷路径，支持多种格式
    ///
    /// # 返回
    /// 成功返回 FveVolumeHandle，失败返回 FveError
    ///
    /// # 注意
    /// 此方法以只读模式打开卷，适用于状态查询和解锁操作。
    /// 如需进行解密等写操作，请使用 `open_volume_ex` 并指定 `FveAccessMode::ReadWrite`。
    pub fn open_volume(&self, volume_path: &str) -> Result<FveVolumeHandle, FveError> {
        self.open_volume_ex(volume_path, FveAccessMode::ReadOnly)
    }

    /// 打开卷并返回句柄包装器（指定访问模式）
    ///
    /// # 参数
    /// - `volume_path`: 卷路径，支持多种格式
    /// - `access_mode`: 访问模式
    ///   - `FveAccessMode::ReadOnly`: 只读模式，用于状态查询、解锁验证
    ///   - `FveAccessMode::ReadWrite`: 读写模式，用于解密、加密等修改操作
    ///
    /// # 返回
    /// 成功返回 FveVolumeHandle，失败返回 FveError
    ///
    /// # 重要
    /// 根据逆向分析，以下操作需要读写模式：
    /// - `FveConversionDecrypt` / `FveConversionDecryptEx` - 开始解密
    /// - `FveConversionEncrypt` / `FveConversionEncryptEx` - 开始加密
    pub fn open_volume_ex(&self, volume_path: &str, access_mode: FveAccessMode) -> Result<FveVolumeHandle, FveError> {
        let normalized_path = normalize_volume_path(volume_path);
        let wide_path = to_wide_string(&normalized_path);
        let mut handle: *mut c_void = std::ptr::null_mut();
        let flags = access_mode as u32;

        log::debug!(
            "FveOpenVolumeW 调用: 原始路径='{}', 标准化路径='{}', flags={}",
            volume_path,
            normalized_path,
            flags
        );

        let hr = unsafe { (self.fn_open_volume)(wide_path.as_ptr(), flags, &mut handle) };

        if hr == 0 && !handle.is_null() {
            log::debug!("FveOpenVolumeW 成功: path={}, handle={:p}, flags={}", normalized_path, handle, flags);
            Ok(FveVolumeHandle {
                handle,
                api: self,
            })
        } else {
            let error = FveError::from_hresult(hr);
            log::debug!(
                "FveOpenVolumeW 返回: path={}, hr=0x{:08X}, error={:?}, flags={}",
                normalized_path,
                hr,
                error,
                flags
            );
            Err(error)
        }
    }

    /// 创建密码认证元素
    fn create_passphrase_auth(&self, passphrase: &str) -> Result<*mut c_void, FveError> {
        let wide_passphrase = to_wide_string(passphrase);
        let mut auth_element: *mut c_void = std::ptr::null_mut();

        let hr = unsafe {
            (self.fn_auth_from_passphrase)(wide_passphrase.as_ptr(), &mut auth_element)
        };

        if hr == 0 && !auth_element.is_null() {
            Ok(auth_element)
        } else {
            Err(FveError::from_hresult(hr))
        }
    }

    /// 创建恢复密钥认证元素
    fn create_recovery_auth(&self, recovery_key: &str) -> Result<*mut c_void, FveError> {
        let wide_recovery = to_wide_string(recovery_key);
        let mut auth_element: *mut c_void = std::ptr::null_mut();

        let hr = unsafe {
            (self.fn_auth_from_recovery)(wide_recovery.as_ptr(), &mut auth_element)
        };

        if hr == 0 && !auth_element.is_null() {
            Ok(auth_element)
        } else {
            Err(FveError::from_hresult(hr))
        }
    }
}

// ==================== FveVolumeHandle 实现 ====================

/// FVE卷句柄包装器（RAII模式）
#[cfg(windows)]
pub struct FveVolumeHandle<'a> {
    handle: *mut c_void,
    api: &'a FveApi,
}

#[cfg(windows)]
impl<'a> FveVolumeHandle<'a> {
    /// 获取卷状态
    pub fn get_status(&self) -> Result<FveVolumeInfo, FveError> {
        let mut status_output = FveGetStatusOutput::new();

        let hr = unsafe { (self.api.fn_get_status)(self.handle, &mut status_output) };

        if hr == 0 {
            Ok(FveVolumeInfo::from(&status_output))
        } else {
            Err(FveError::from_hresult(hr))
        }
    }

    /// 使用密码解锁卷
    pub fn unlock_with_password(&self, password: &str) -> Result<(), FveError> {
        let auth_element = self.api.create_passphrase_auth(password)?;
        let hr = unsafe { (self.api.fn_unlock_volume)(self.handle, auth_element) };

        if hr == 0 {
            log::info!("卷使用密码解锁成功");
            Ok(())
        } else {
            let error = FveError::from_hresult(hr);
            log::warn!("卷使用密码解锁失败: hr=0x{:08X}, error={:?}", hr, error);
            Err(error)
        }
    }

    /// 使用恢复密钥解锁卷
    pub fn unlock_with_recovery_key(&self, recovery_key: &str) -> Result<(), FveError> {
        let auth_element = self.api.create_recovery_auth(recovery_key)?;
        let hr = unsafe { (self.api.fn_unlock_volume)(self.handle, auth_element) };

        if hr == 0 {
            log::info!("卷使用恢复密钥解锁成功");
            Ok(())
        } else {
            let error = FveError::from_hresult(hr);
            log::warn!("卷使用恢复密钥解锁失败: hr=0x{:08X}, error={:?}", hr, error);
            Err(error)
        }
    }

    /// 锁定卷
    pub fn lock(&self, dismount_first: bool) -> Result<(), FveError> {
        let hr = unsafe { (self.api.fn_lock_volume)(self.handle, if dismount_first { 1 } else { 0 }) };

        if hr == 0 {
            log::info!("卷锁定成功");
            Ok(())
        } else {
            let error = FveError::from_hresult(hr);
            log::warn!("卷锁定失败: hr=0x{:08X}, error={:?}", hr, error);
            Err(error)
        }
    }

    /// 开始解密（彻底关闭BitLocker）
    pub fn start_decryption(&self) -> Result<(), FveError> {
        let hr = unsafe { (self.api.fn_conversion_decrypt)(self.handle) };

        if hr == 0 {
            log::info!("开始解密操作成功");
            Ok(())
        } else {
            let error = FveError::from_hresult(hr);
            log::warn!("开始解密操作失败: hr=0x{:08X}, error={:?}", hr, error);
            Err(error)
        }
    }

    /// 开始解密（带标志）
    pub fn start_decryption_ex(&self, flags: u32) -> Result<(), FveError> {
        let hr = unsafe { (self.api.fn_conversion_decrypt_ex)(self.handle, flags) };

        if hr == 0 {
            log::info!("开始解密操作成功 (flags=0x{:08X})", flags);
            Ok(())
        } else {
            let error = FveError::from_hresult(hr);
            log::warn!("开始解密操作失败: hr=0x{:08X}, error={:?}", hr, error);
            Err(error)
        }
    }

    /// 获取原始句柄
    pub fn as_raw(&self) -> *mut c_void {
        self.handle
    }
}

#[cfg(windows)]
impl<'a> Drop for FveVolumeHandle<'a> {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            let hr = unsafe { (self.api.fn_close_volume)(self.handle) };
            if hr != 0 {
                log::warn!("FveCloseVolume 失败: hr=0x{:08X}", hr);
            } else {
                log::debug!("FveCloseVolume 成功: handle={:p}", self.handle);
            }
        }
    }
}

// ==================== 辅助函数 ====================

/// 将 Rust 字符串转换为以 null 结尾的宽字符串
fn to_wide_string(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 标准化卷路径格式
///
/// 将各种格式的卷路径转换为 FveGetStatusW 能识别的格式。
/// 根据逆向分析，FveGetStatusW 接受简单的盘符格式如 "C:"
///
/// 支持的输入格式：
/// - `C:` -> `C:`
/// - `C:\` -> `C:`
/// - `\\.\\C:` -> `C:`
/// - `\\?\Volume{GUID}` -> 保持不变
fn normalize_volume_path(path: &str) -> String {
    let trimmed = path.trim();
    
    // 如果是Volume GUID格式，保持不变
    if trimmed.contains("Volume{") {
        return trimmed.to_string();
    }
    
    // 提取盘符
    let chars: Vec<char> = trimmed.chars().collect();
    
    // 检查是否是设备路径格式 \\.\\X: 或 \\?\X:
    if chars.len() >= 6 {
        let prefix = &trimmed[..4];
        if prefix == "\\\\.\\" || prefix == "\\\\?\\" {
            // 提取盘符部分
            let rest = &trimmed[4..];
            if rest.len() >= 2 && rest.chars().nth(1) == Some(':') {
                let letter = rest.chars().next().unwrap();
                if letter.is_ascii_alphabetic() {
                    return format!("{}:", letter.to_ascii_uppercase());
                }
            }
        }
    }
    
    // 检查是否是简单的盘符格式 X: 或 X:\
    if chars.len() >= 2 && chars[0].is_ascii_alphabetic() && chars[1] == ':' {
        return format!("{}:", chars[0].to_ascii_uppercase());
    }
    
    // 如果无法解析，返回原始路径
    trimmed.to_string()
}

/// 格式化恢复密钥
///
/// 将用户输入的恢复密钥格式化为标准格式：
/// XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX
///
/// # 参数
/// - `input`: 用户输入的恢复密钥（可以包含或不包含分隔符）
///
/// # 返回
/// 格式化后的恢复密钥，或错误信息
pub fn format_recovery_key(input: &str) -> Result<String, String> {
    // 移除所有非数字字符
    let digits: String = input.chars().filter(|c| c.is_ascii_digit()).collect();

    // 恢复密钥应该有48位数字
    if digits.len() != 48 {
        return Err(format!(
            "恢复密钥格式错误：应为48位数字，实际为{}位",
            digits.len()
        ));
    }

    // 格式化为 XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX
    let parts: Vec<&str> = vec![
        &digits[0..6],
        &digits[6..12],
        &digits[12..18],
        &digits[18..24],
        &digits[24..30],
        &digits[30..36],
        &digits[36..42],
        &digits[42..48],
    ];

    Ok(parts.join("-"))
}

// ==================== 非 Windows 平台的空实现 ====================

#[cfg(not(windows))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FveAccessMode {
    ReadOnly = 0,
    ReadWrite = 1,
}

#[cfg(not(windows))]
pub struct FveApi;

#[cfg(not(windows))]
impl FveApi {
    pub fn instance() -> Result<&'static FveApi, String> {
        Err("FveApi 仅在 Windows 平台可用".to_string())
    }

    pub fn get_status_by_path(&self, _volume_path: &str) -> Result<FveVolumeInfo, FveError> {
        Err(FveError::NotSupported)
    }

    pub fn open_volume(&self, _volume_path: &str) -> Result<FveVolumeHandle, FveError> {
        Err(FveError::NotSupported)
    }

    pub fn open_volume_ex(&self, _volume_path: &str, _access_mode: FveAccessMode) -> Result<FveVolumeHandle, FveError> {
        Err(FveError::NotSupported)
    }
}

#[cfg(not(windows))]
pub struct FveVolumeHandle<'a> {
    _phantom: std::marker::PhantomData<&'a ()>,
}

#[cfg(not(windows))]
impl<'a> FveVolumeHandle<'a> {
    pub fn get_status(&self) -> Result<FveVolumeInfo, FveError> {
        Err(FveError::NotSupported)
    }

    pub fn unlock_with_password(&self, _password: &str) -> Result<(), FveError> {
        Err(FveError::NotSupported)
    }

    pub fn unlock_with_recovery_key(&self, _recovery_key: &str) -> Result<(), FveError> {
        Err(FveError::NotSupported)
    }

    pub fn lock(&self, _dismount_first: bool) -> Result<(), FveError> {
        Err(FveError::NotSupported)
    }

    pub fn start_decryption(&self) -> Result<(), FveError> {
        Err(FveError::NotSupported)
    }

    pub fn start_decryption_ex(&self, _flags: u32) -> Result<(), FveError> {
        Err(FveError::NotSupported)
    }
}

// ==================== 测试 ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fve_get_status_output_size() {
        assert_eq!(std::mem::size_of::<FveGetStatusOutput>(), 0x80);
    }

    #[test]
    fn test_fve_get_status_output_default() {
        let output = FveGetStatusOutput::default();
        assert_eq!(output.size, 0x80);
        assert_eq!(output.version, 2);
        assert_eq!(output.conversion_status, 0);
        assert_eq!(output.protection_status, 0);
        assert!(!output.is_encrypted());
        assert!(!output.is_locked());
    }

    #[test]
    fn test_format_recovery_key() {
        // 测试纯数字输入
        let result = format_recovery_key("123456789012345678901234567890123456789012345678");
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            "123456-789012-345678-901234-567890-123456-789012-345678"
        );

        // 测试带分隔符的输入
        let result = format_recovery_key("123456-789012-345678-901234-567890-123456-789012-345678");
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            "123456-789012-345678-901234-567890-123456-789012-345678"
        );

        // 测试带空格的输入
        let result = format_recovery_key("123456 789012 345678 901234 567890 123456 789012 345678");
        assert!(result.is_ok());

        // 测试错误长度
        let result = format_recovery_key("12345");
        assert!(result.is_err());
    }

    #[test]
    fn test_fve_error_display() {
        assert_eq!(FveError::Success.to_string(), "操作成功");
        assert_eq!(FveError::BadPassword.to_string(), "密码错误");
        assert_eq!(FveError::NotEncrypted.to_string(), "卷未启用BitLocker加密");
    }

    #[test]
    fn test_fve_volume_status() {
        assert_eq!(FveVolumeStatus::from(0), FveVolumeStatus::FullyDecrypted);
        assert_eq!(FveVolumeStatus::from(1), FveVolumeStatus::FullyEncrypted);
        assert_eq!(FveVolumeStatus::from(2), FveVolumeStatus::EncryptionInProgress);
        assert_eq!(FveVolumeStatus::from(99), FveVolumeStatus::FullyDecrypted); // 未知值回退
    }

    #[test]
    fn test_fve_protection_status() {
        assert_eq!(FveProtectionStatus::from(0), FveProtectionStatus::Off);
        assert_eq!(FveProtectionStatus::from(1), FveProtectionStatus::On);
        assert_eq!(FveProtectionStatus::from(99), FveProtectionStatus::Unknown);
    }

    #[test]
    fn test_fve_lock_status() {
        assert_eq!(FveLockStatus::from(0), FveLockStatus::Unlocked);
        assert_eq!(FveLockStatus::from(1), FveLockStatus::Locked);
        assert_eq!(FveLockStatus::from(99), FveLockStatus::Locked); // 非零值视为锁定
    }

    #[test]
    fn test_normalize_volume_path() {
        use super::normalize_volume_path;
        
        // 测试简单盘符
        assert_eq!(normalize_volume_path("C:"), "C:");
        assert_eq!(normalize_volume_path("c:"), "C:");
        assert_eq!(normalize_volume_path("D:"), "D:");
        
        // 测试带反斜杠的盘符
        assert_eq!(normalize_volume_path("C:\\"), "C:");
        assert_eq!(normalize_volume_path("D:\\Windows"), "D:");
        
        // 测试设备路径格式
        assert_eq!(normalize_volume_path("\\\\.\\C:"), "C:");
        assert_eq!(normalize_volume_path("\\\\.\\D:"), "D:");
        assert_eq!(normalize_volume_path("\\\\?\\C:"), "C:");
        
        // 测试空格
        assert_eq!(normalize_volume_path("  C:  "), "C:");
    }
}
