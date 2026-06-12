//! FVEAPI.dll 动态加载模块
//!
//! 提供对Windows BitLocker驱动器加密API的底层访问。
//! fveapi.dll是Windows未公开的API，本模块基于逆向工程分析实现。
//!
//! # 结构体说明（通过逆向工程确认）
//!
//! ## FVE_STATUS_INFO / FVE_GET_STATUS_OUTPUT 结构体（version 8，0x78=120字节）
//!
//! 关键字段偏移（已通过反汇编验证）：
//! - +0x00 dwSize: 结构体大小 (0x78 = 120) —— version 8 必须为 0x78
//! - +0x04 dwVersion: 版本号 (8) —— FveGetStatus 内部 `cmp eax,8` 选择 0x78 结构
//! - +0x0C dwConversionStatus: 转换状态 (0-5)
//! - +0x10 dblPercentComplete: 加密百分比 (0.0-100.0)
//! - +0x38 dwProtectionStatus: 保护开关 (0=off, 1=on) —— 注意这不是“锁定状态”
//! - +0x70 dwEncryptionFlags: 加密标志 (掩码 0x17F)
//!
//! # 跨版本兼容（重要）
//!
//! FveGetStatus 的结构体 version/size 是**随 Windows build 变化的**，经两份官方
//! 二进制确认: 21H1(19043)=v8/0x78，1709(16299)=v5/0x58；DLL 对高于本机支持的
//! version 返回 0x80070057。故本模块**不写死版本**，运行时通过 `FVE_STATUS_VERSIONS`
//! 从高到低协商，并用 `FveVolumeInfo::from_output` 按命中的 size 守卫字段读取。
//! 解锁相关函数(FveOpenVolumeW / FveUnlockVolume / FveAuthElementFromRecoveryPasswordW)
//! 的签名跨版本稳定。
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
    // 注意: 0x80310008 实为 FVE_E_NOT_ACTIVATED(卷未激活BitLocker)。
    // 枚举解锁时非BitLocker卷会返回此码, 属正常跳过; 真正的"未加密"是 0x80310001。
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
    /// 未知/无效(读取到非法值, 不应当作已解密)
    Unknown = 0xFFFF_FFFF,
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
                    "未知的 FveVolumeStatus 值: {} (0x{:08X}), 记为 Unknown",
                    value, value
                );
                FveVolumeStatus::Unknown
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

/// FVE_GET_STATUS_OUTPUT 结构体（version 8，0x78=120字节）
///
/// 根据fveapi.dll逆向工程分析确认的结构体布局。
/// 这是 FveGetStatusW 和 FveGetStatus 函数使用的输出结构。
///
/// 关键字段偏移（已验证）：
/// - +0x00 dwSize: 结构体大小，必须设置为 0x78 (120)
/// - +0x04 dwVersion: 版本号，必须设置为 8
/// - +0x0C dwConversionStatus: 转换状态 (0=解密, 1=加密, 2-5=转换中)
/// - +0x10 dblPercentComplete: 加密百分比 (0.0-100.0)
/// - +0x38 dwProtectionStatus: 保护状态 (0=关闭/已解锁, 1=开启/已锁定)
/// - +0x50 qwVolumeSize: 卷大小（字节）
/// - +0x58 qwEncryptedSize: 已加密大小（字节）
/// - +0x70 dwEncryptionFlags: 加密标志
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FveGetStatusOutput {
    /// +0x00: 结构体大小（version 8 必须为 0x78 = 120；逆向: cmp dword [rbx],0x78 @0x1800240bd）
    pub size: u32,
    /// +0x04: 版本号（必须为 8；逆向: mov eax,[rbx+4]; cmp eax,8 @0x1800240af）
    pub version: u32,
    /// +0x08: 保留字段
    reserved1: u32,
    /// +0x0C: 转换状态 (0-5)
    pub conversion_status: u32,
    /// +0x10: 加密百分比 (0.0-100.0)
    pub percent_complete: f64,
    /// +0x18: 保留字段数组 (0x20字节, 到0x37)
    reserved2: [u8; 0x20],
    /// +0x38: BitLocker 保护开关 ProtectionStatus (0=关, 1=开)。
    /// 逆向重要提示: 这是“保护是否启用”，**不等于卷是否被锁定**。
    /// 一个正常运行、已解锁的加密卷该值同样是 1(开)。v8 填充路径
    /// (fveapi 0x180024070) 中**没有**独立的锁定状态字段——锁定(密钥
    /// 可用性)由 API 行为通过错误码体现，见 `is_locked` / `get_lock_status` 文档。
    pub protection_status: u32,
    /// +0x3C: 保留字段数组 (0x14字节, 到0x4F)
    reserved3: [u8; 0x14],
    /// +0x50: 卷大小（字节）
    pub volume_size: u64,
    /// +0x58: 已加密大小（字节）。逆向注意: v8 填充路径**未写入** +0x58,
    /// 实测读到初始化的 0，不可依赖。可靠的容量字段是 volume_size(+0x50)。
    pub encrypted_size: u64,
    /// +0x60: 保留字段数组 (0x10字节, 到0x6F)
    reserved4: [u8; 0x10],
    /// +0x70: 加密标志
    pub encryption_flags: u32,
    /// +0x74: 保留字段数组 (0x04字节, 到0x77)
    reserved5: [u8; 0x04],
}

// 确保结构体大小正确
const _: () = assert!(std::mem::size_of::<FveGetStatusOutput>() == 0x78);

/// FveGetStatus 结构体的 (dwVersion, dwSize) 表，从最新到最旧。
///
/// 逆向自官方二进制对比:
/// - Windows 10 21H1 (build 19043): 最高 version=8, size=0x78
/// - Windows 10 1709  (build 16299): 最高 version=5, size=0x58
///
/// FveGetStatus 内部 `cmp eax,N; ja error` 对“高于本机支持的 version”返回
/// 0x80070057(InvalidParameter)。因此**不能写死版本**，运行时需从高到低
/// 协商，命中第一个非 InvalidParameter 的 (version,size) 即本机所用版本。
/// 字段为追加式: 老字段偏移跨版本不变，新版本仅在尾部追加字段。
pub const FVE_STATUS_VERSIONS: &[(u32, u32)] = &[
    (8, 0x78),
    (7, 0x70),
    (6, 0x68),
    (5, 0x58),
    (4, 0x40),
];

impl Default for FveGetStatusOutput {
    fn default() -> Self {
        Self {
            size: 0x78,
            version: 8,
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
            reserved5: [0; 0x04],
        }
    }
}

impl FveGetStatusOutput {
    /// 创建新的状态输出结构
    pub fn new() -> Self {
        Self::default()
    }

    /// 按指定 (version, size) 构造，用于运行时版本协商。
    ///
    /// 注意: Rust 端缓冲区**始终是 0x78 字节**(足以容纳所有已知版本)，这里
    /// 只设置告知 DLL 的 version/size 字段。DLL 仅填充前 `size` 字节并校验
    /// `dwSize` 与该版本严格相等，故只能从表里取已知组合。
    pub fn with_version(version: u32, size: u32) -> Self {
        let mut o = Self::default();
        o.version = version;
        o.size = size;
        o
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

    /// 检查 BitLocker 保护是否开启。
    ///
    /// # ⚠ 这不是锁定状态
    /// 逆向确认 FVE_GET_STATUS_OUTPUT(v8) 没有独立锁定字段，本方法返回的是
    /// ProtectionStatus(+0x38)，仅表示“保护开关是否开启”。已解锁的加密卷
    /// 同样返回 true。要判断**卷是否被锁定**，请用以下权威途径之一：
    /// - 调用解锁后看错误码：`FVE_E_LOCKED_VOLUME(0x80310000)` / `KeyRequired` ⇒ 锁定；
    ///   `VolumeUnlocked(0x80310023)` ⇒ 本来就没锁；`Success` ⇒ 已解锁。
    /// - 使用 `FveError::indicates_locked()`。
    #[deprecated(note = "ProtectionStatus != 锁定状态；锁定请用解锁返回的错误码判定")]
    pub fn is_protection_on(&self) -> bool {
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

    /// 获取锁定状态枚举。
    ///
    /// # ⚠ 不可靠
    /// 逆向确认本结构体不含锁定字段。此处沿用旧实现(由 ProtectionStatus 推断)，
    /// 仅作兼容；正常运行的已解锁加密卷会被误判为 Locked。真实锁定状态请通过
    /// 解锁返回的错误码判定(见 `FveError::indicates_locked`)。
    #[deprecated(note = "不可靠；锁定请用解锁返回的错误码判定")]
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
    #[allow(deprecated)] // lock_status 沿用旧映射仅作兼容(见 get_lock_status 文档)
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

impl FveVolumeInfo {
    /// 按“协商出的结构体 size”安全解析: 只读取该 size 实际覆盖到的字段，
    /// 未被 DLL 填充的尾部字段一律取 0，避免在旧版本(如 1709 的 0x58)上
    /// 读到未初始化/越界含义的数据。
    ///
    /// 字段需要的最小 size(字段结束偏移):
    /// conversion@+0x0C、percent@+0x10 → 所有版本;
    /// protection@+0x38 → size≥0x3C(v4+); volume_size@+0x50 → size≥0x58(v5+);
    /// encrypted_size@+0x58 → size≥0x60; encryption_flags@+0x70 → size≥0x74(v8+)。
    pub fn from_output(output: &FveGetStatusOutput, negotiated_size: u32) -> Self {
        let protection = if negotiated_size >= 0x3C { output.protection_status } else { 0 };
        let vol_size = if negotiated_size >= 0x58 { output.volume_size } else { 0 };
        let enc_size = if negotiated_size >= 0x60 { output.encrypted_size } else { 0 };
        let flags = if negotiated_size >= 0x74 { output.encryption_flags } else { 0 };
        Self {
            volume_status: FveVolumeStatus::from(output.conversion_status),
            protection_status: FveProtectionStatus::from(protection),
            // lock_status 由 ProtectionStatus 推断，仅兼容(不可靠，见 get_lock_status 文档)
            lock_status: FveLockStatus::from(protection),
            encryption_percentage: output.percent_complete.round().clamp(0.0, 100.0) as u8,
            encryption_flags: flags,
            volume_size: vol_size,
            encrypted_size: enc_size,
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

// 真正能解锁已激活卷的入口: 逆向证实它用 mode=2 构造上下文, 并设置核心所需的第 5 个参数
// (FveUnlockVolume 缺这个槽, 对已激活卷返回 0x80070057)。第 3 参数(访问模式 in/out)可传 NULL。
#[cfg(windows)]
type FnFveUnlockVolumeWithAccessMode = unsafe extern "system" fn(
    h_volume: *mut c_void,
    auth_element: *mut c_void,
    p_access_mode: *mut u32, // 可为 NULL
) -> u32;

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
type FnFveRevertVolume = unsafe extern "system" fn(
    h_volume: *mut c_void, // HANDLE
) -> u32; // HRESULT  —— bdesvc 关闭 BitLocker(解密/revert)实际用的入口

#[cfg(windows)]
type FnFveConversionDecryptEx = unsafe extern "system" fn(
    h_volume: *mut c_void, // HANDLE
    flags: u32,            // DWORD
) -> u32; // HRESULT

#[cfg(windows)]
type FnFveAuthElementFromPassPhraseW = unsafe extern "system" fn(
    passphrase: *const u16, // LPCWSTR
    p_auth_element: *mut c_void, // in/out: 同上 32 字节结构
) -> u32; // HRESULT

#[cfg(windows)]
type FnFveAuthElementFromRecoveryPasswordW = unsafe extern "system" fn(
    recovery_password: *const u16, // LPCWSTR
    // in/out: 指向 32 字节 FVE_AUTH_ELEMENT, 调用方须预填 {dwSize=0x20, dwVersion=1}
    p_auth_element: *mut c_void,
) -> u32; // HRESULT

// ==================== FveApi 实现 ====================

/// FveApi 全局单例
#[cfg(windows)]
static FVE_API_INSTANCE: OnceLock<Result<FveApi, String>> = OnceLock::new();

/// FveOpenVolumeByHandle: 用已有内核卷句柄打开 FVE 卷(锁定卷也能打开)。
/// 逆向自 bdesvc 真实调用: FveOpenVolumeByHandle(hKernel, 0, 0, 0xFFFFFFFF, 1, &out)。
#[cfg(windows)]
type FnFveOpenVolumeByHandle = unsafe extern "system" fn(
    kernel_handle: *mut c_void,
    arg2: usize, // bdesvc: 0
    arg3: u32,   // bdesvc: 0
    arg4: u32,   // bdesvc: 0xFFFFFFFF (-1)
    arg5: u32,   // bdesvc: 1
    out_handle: *mut *mut c_void,
) -> u32;

/// FveFindFirstVolume(&卷句柄, &查找句柄): 开始枚举 FVE 卷。
/// 逆向自 bdesvc: rcx=&volHandle(用于 OpenVolumeByHandle), rdx=&findHandle(用于 FindNext)。
// 逆向自 bdesvc: arg2 是指向 dword 的指针, 输入值固定为 1(枚举标志/卷类型)。
// 起初误当作"输出查找句柄"(空指针→0), 导致枚举条件为 0 → ERROR_NO_MORE_FILES。
#[cfg(windows)]
type FnFveFindFirstVolume = unsafe extern "system" fn(
    p_volume_handle: *mut *mut c_void, // out: 卷句柄(兼作迭代状态)
    p_flags: *mut u32,                 // in : 固定 = 1
) -> u32;

/// FveFindNextVolume(卷句柄, &flags=1): 就地推进枚举; 结束返回 ERROR_NO_MORE_FILES(0x80070012)。
#[cfg(windows)]
type FnFveFindNextVolume = unsafe extern "system" fn(
    volume_handle: *mut c_void, // in: 来自 FindFirst 的卷句柄(迭代状态)
    p_flags: *mut u32,          // in: 固定 = 1
) -> u32;

/// FveGetVolumeNameW(卷句柄, &len/inout=容量, buf): 取卷设备名(诊断用)。逆向自 bdesvc。
#[cfg(windows)]
type FnFveGetVolumeNameW = unsafe extern "system" fn(
    volume_handle: *mut c_void,
    p_len: *mut u32,
    buffer: *mut u16,
) -> u32;

// kernel32: 打开原始卷设备 \\.\X: 取内核句柄(锁定卷的设备本身仍可打开)。
#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn CreateFileW(
        lp_file_name: *const u16,
        dw_desired_access: u32,
        dw_share_mode: u32,
        lp_security_attributes: *mut c_void,
        dw_creation_disposition: u32,
        dw_flags_and_attributes: u32,
        h_template_file: *mut c_void,
    ) -> *mut c_void;
    fn CloseHandle(h: *mut c_void) -> i32;
}

/// FVE API 封装
#[cfg(windows)]
pub struct FveApi {
    _library: Library,
    fn_open_volume: FnFveOpenVolumeW,
    fn_close_volume: FnFveCloseVolume,
    fn_get_status_w: FnFveGetStatusW,
    fn_get_status: FnFveGetStatus,
    fn_unlock_volume: FnFveUnlockVolume,
    fn_unlock_volume_access: Option<FnFveUnlockVolumeWithAccessMode>,
    fn_lock_volume: FnFveLockVolume,
    fn_conversion_decrypt: FnFveConversionDecrypt,
    fn_conversion_decrypt_ex: FnFveConversionDecryptEx,
    fn_revert_volume: Option<FnFveRevertVolume>,
    fn_auth_from_passphrase: FnFveAuthElementFromPassPhraseW,
    fn_auth_from_recovery: FnFveAuthElementFromRecoveryPasswordW,
    fn_open_volume_by_handle: FnFveOpenVolumeByHandle,
    fn_find_first_volume: FnFveFindFirstVolume,
    fn_find_next_volume: FnFveFindNextVolume,
    fn_get_volume_name: Option<FnFveGetVolumeNameW>,
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
            fn_unlock_volume_access,
            fn_lock_volume,
            fn_conversion_decrypt,
            fn_conversion_decrypt_ex,
            fn_auth_from_passphrase,
            fn_auth_from_recovery,
            fn_open_volume_by_handle,
            fn_find_first_volume,
            fn_find_next_volume,
            fn_get_volume_name,
            fn_revert_volume,
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
            let fn_unlock_volume_access: Option<FnFveUnlockVolumeWithAccessMode> = library
                .get::<FnFveUnlockVolumeWithAccessMode>(b"FveUnlockVolumeWithAccessMode")
                .ok().map(|s| *s);
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
            let fn_open_volume_by_handle: FnFveOpenVolumeByHandle = *library
                .get::<FnFveOpenVolumeByHandle>(b"FveOpenVolumeByHandle")
                .map_err(|e| format!("找不到 FveOpenVolumeByHandle: {}", e))?;
            let fn_find_first_volume: FnFveFindFirstVolume = *library
                .get::<FnFveFindFirstVolume>(b"FveFindFirstVolume")
                .map_err(|e| format!("找不到 FveFindFirstVolume: {}", e))?;
            let fn_find_next_volume: FnFveFindNextVolume = *library
                .get::<FnFveFindNextVolume>(b"FveFindNextVolume")
                .map_err(|e| format!("找不到 FveFindNextVolume: {}", e))?;
            let fn_get_volume_name: Option<FnFveGetVolumeNameW> =
                library.get::<FnFveGetVolumeNameW>(b"FveGetVolumeNameW").ok().map(|s| *s);
            let fn_revert_volume: Option<FnFveRevertVolume> =
                library.get::<FnFveRevertVolume>(b"FveRevertVolume").ok().map(|s| *s);

            (
                fn_open_volume,
                fn_close_volume,
                fn_get_status_w,
                fn_get_status,
                fn_unlock_volume,
                fn_unlock_volume_access,
                fn_lock_volume,
                fn_conversion_decrypt,
                fn_conversion_decrypt_ex,
                fn_auth_from_passphrase,
                fn_auth_from_recovery,
                fn_open_volume_by_handle,
                fn_find_first_volume,
                fn_find_next_volume,
                fn_get_volume_name,
                fn_revert_volume,
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
            fn_unlock_volume_access,
            fn_lock_volume,
            fn_conversion_decrypt,
            fn_conversion_decrypt_ex,
            fn_auth_from_passphrase,
            fn_auth_from_recovery,
            fn_open_volume_by_handle,
            fn_find_first_volume,
            fn_find_next_volume,
            fn_get_volume_name,
            fn_revert_volume,
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

        log::debug!(
            "FveGetStatusW 调用: 原始路径='{}', 标准化路径='{}'",
            volume_path,
            normalized_path
        );

        // 版本协商: 从最新结构体版本往下试，命中第一个非 InvalidParameter 即为本机版本
        let mut last_err = FveError::NotSupported;
        for &(ver, size) in FVE_STATUS_VERSIONS {
            let mut status_output = FveGetStatusOutput::with_version(ver, size);
            let hr = unsafe { (self.fn_get_status_w)(wide_path.as_ptr(), &mut status_output) };
            if hr == 0 {
                log::debug!(
                    "FveGetStatusW 成功: path={}, version={}, size=0x{:X}, conversion={}, flags=0x{:04X}, percent={}",
                    normalized_path, ver, size,
                    status_output.conversion_status,
                    status_output.encryption_flags,
                    status_output.percent_complete
                );
                return Ok(FveVolumeInfo::from_output(&status_output, size));
            }
            // 0x80070057 = 本 build 不支持该 version，降级重试；其它错误是真失败
            if hr != FveError::InvalidParameter as u32 {
                let error = FveError::from_hresult(hr);
                log::debug!(
                    "FveGetStatusW 返回(非协商错误): path={}, hr=0x{:08X}, error={:?}",
                    normalized_path, hr, error
                );
                return Err(error);
            }
            log::debug!("FveGetStatusW version={} 不支持(0x80070057)，降级", ver);
            last_err = FveError::InvalidParameter;
        }
        Err(last_err)
    }

    /// 诊断: 探测本机 fveapi 实际接受的 GetStatus 结构体版本。
    ///
    /// 逐个尝试 `FVE_STATUS_VERSIONS`，返回第一个**未**被 0x80070057 拒绝的
    /// (version, size) —— 即该 build 支持的最高版本。全部被拒返回 None。
    /// 用于 CI / 跨版本验证（如在 GitHub Actions 上确认 Server 2022 的版本）。
    pub fn probe_status_version(&self, volume_path: &str) -> Option<(u32, u32)> {
        let normalized = normalize_volume_path(volume_path);
        let wide = to_wide_string(&normalized);
        for &(ver, size) in FVE_STATUS_VERSIONS {
            let mut out = FveGetStatusOutput::with_version(ver, size);
            let hr = unsafe { (self.fn_get_status_w)(wide.as_ptr(), &mut out) };
            // 只有 0x80070057 表示该 version 高于本机支持；其它结果（成功，或卷未加密
            // 等业务错误）都说明该 version/size 已通过结构校验 → 即本机版本。
            if hr != FveError::InvalidParameter as u32 {
                return Some((ver, size));
            }
        }
        None
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
    pub fn open_volume(&self, volume_path: &str) -> Result<FveVolumeHandle<'_>, FveError> {
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
    pub fn open_volume_ex(&self, volume_path: &str, access_mode: FveAccessMode) -> Result<FveVolumeHandle<'_>, FveError> {
        let normalized_path = normalize_volume_path(volume_path);
        self.open_volume_raw(&normalized_path, access_mode)
    }

    /// 用**精确**路径串开卷, 不经 normalize_volume_path(后者会把 \\.\X: / \\?\X: 砍成 X:)。
    ///
    /// 逆向+Procmon 证实: 传 "X:" 会让 fveapi 把它当成 X:\ 根目录、用 FILE_NON_DIRECTORY_FILE
    /// 打开 → STATUS_FILE_IS_A_DIRECTORY → 经 RtlNtStatusToDosError 映射成 ERROR_ACCESS_DENIED
    /// (0x80070005)。要拿到**卷设备**需传设备形式: "\\.\X:" 或不带尾反斜杠的 "\\?\Volume{GUID}"。
    pub fn open_volume_raw(&self, raw_path: &str, access_mode: FveAccessMode) -> Result<FveVolumeHandle<'_>, FveError> {
        let wide_path = to_wide_string(raw_path);
        let mut handle: *mut c_void = std::ptr::null_mut();
        let flags = access_mode as u32;
        if flags != 0 {
            enable_volume_privileges();
        }
        log::debug!("FveOpenVolumeW(raw) path='{}', flags={}", raw_path, flags);
        let hr = unsafe { (self.fn_open_volume)(wide_path.as_ptr(), flags, &mut handle) };
        if hr == 0 && !handle.is_null() {
            log::info!("FveOpenVolumeW 成功: path='{}', flags={}", raw_path, flags);
            Ok(FveVolumeHandle {
                handle,
                api: self,
                kernel_handle: std::ptr::null_mut(),
            })
        } else {
            let error = FveError::from_hresult(hr);
            log::warn!("FveOpenVolumeW 失败: path='{}', hr=0x{:08X}, error={:?}, flags={}",
                raw_path, hr, error, flags);
            Err(error)
        }
    }

    /// 打开（可能已锁定的）卷以便解锁。
    ///
    /// 为什么不能直接用 `open_volume`: `FveOpenVolumeW` 对已加密/锁定卷返回
    /// 0x80310000 且**不写句柄**(逆向确认: 内部 0x180026b60 加载 FVE 元数据时
    /// 取不到 FVEK 即失败,发生在写句柄之前,且不受 flags 控制)。因此锁定卷必须
    /// 像 bdesvc 那样: 先 CreateFileW 打开原始卷设备取内核句柄,再
    /// FveOpenVolumeByHandle 包成 FVE 句柄(不加载 FVEK)。之后即可在返回句柄上
    /// 调用 `unlock_with_recovery_key` / `unlock_with_password`。
    pub fn open_volume_for_unlock(&self, volume_path: &str) -> Result<FveVolumeHandle<'_>, FveError> {
        let drive = normalize_volume_path(volume_path); // "X:"
        let device = format!("\\\\.\\{}", drive);  // \\.\X:
        let wide = to_wide_string(&device);

        const GENERIC_READ: u32 = 0x8000_0000;
        const GENERIC_WRITE: u32 = 0x4000_0000;
        const FILE_SHARE_READ_WRITE: u32 = 0x0000_0003;
        const OPEN_EXISTING: u32 = 3;

        let kernel_handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        // INVALID_HANDLE_VALUE == -1
        if kernel_handle.is_null() || kernel_handle as isize == -1isize {
            log::warn!("CreateFileW({}) 失败", device);
            return Err(FveError::AccessDenied);
        }

        let mut fve_handle: *mut c_void = std::ptr::null_mut();
        // 与 bdesvc 一致: (hKernel, 0, 0, -1, 1, &out)
        let hr = unsafe {
            (self.fn_open_volume_by_handle)(kernel_handle, 0, 0, 0xFFFF_FFFF, 1, &mut fve_handle)
        };
        if hr != 0 || fve_handle.is_null() {
            unsafe { CloseHandle(kernel_handle); }
            let error = FveError::from_hresult(hr);
            log::warn!("FveOpenVolumeByHandle 失败: {} hr=0x{:08X} error={:?}", device, hr, error);
            return Err(error);
        }
        log::debug!("FveOpenVolumeByHandle 成功: {} fve_handle={:p}", device, fve_handle);
        Ok(FveVolumeHandle {
            handle: fve_handle,
            api: self,
            kernel_handle,
        })
    }

    /// 枚举所有 FVE 卷，对每个尝试用给定恢复密钥解锁，命中即成功。
    ///
    /// 这是恢复工具的正路(逆向自 bdesvc): 锁定卷无法用 FveOpenVolumeW 打开,
    /// 必须经 FveFindFirstVolume 取得 FVE 卷句柄,再 FveOpenVolumeByHandle 打开,
    /// 然后 FveUnlockVolume。逐个试密钥可免去“盘符→设备名”匹配——错误的卷会
    /// 返回认证失败,正确的卷被解开。
    ///
    /// 返回成功解锁的卷数(0 表示没有任何卷被这把密钥解开)。
    pub fn find_and_unlock_with_recovery_key(&self, recovery_key: &str) -> Result<u32, FveError> {
        let mut vol_handle: *mut c_void = std::ptr::null_mut();
        // arg2 是搜索/枚举状态结构(bdesvc 实证): 字段@0=1, 字段@4=0xFFFFFFFF, 其余 0。
        // 字段@4 疑似 in/out 游标, 故 First→Next 全程复用同一份, 不重置。
        let mut spec: [u32; 4] = [1, 0xFFFF_FFFF, 0, 0];
        let hr = unsafe { (self.fn_find_first_volume)(&mut vol_handle, spec.as_mut_ptr()) };
        if hr != 0 {
            log::warn!("FveFindFirstVolume 失败: hr=0x{:08X}", hr);
            return Err(FveError::from_hresult(hr));
        }

        let mut unlocked = 0u32;
        let mut scanned = 0u32;
        // 循环上限: 防止 FindNext 签名万一不对导致死循环
        for _ in 0..64u32 {
            if !vol_handle.is_null() && vol_handle as isize != -1 {
                scanned += 1;
                let idx = scanned;
                if let Some(getname) = self.fn_get_volume_name {
                    let mut buf = [0u16; 260];
                    let mut len: u32 = 260;
                    let nhr = unsafe { getname(vol_handle, &mut len, buf.as_mut_ptr()) };
                    if nhr == 0 {
                        let name = String::from_utf16_lossy(
                            &buf[..buf.iter().position(|&c| c == 0).unwrap_or(0)]);
                        log::info!("卷#{}: 名称='{}'", idx, name);
                    } else {
                        log::info!("卷#{}: FveGetVolumeNameW hr=0x{:08X}", idx, nhr);
                    }
                }
                // 用枚举得到的卷句柄打开成 FVE 卷
                let mut fve_handle: *mut c_void = std::ptr::null_mut();
                let ohr = unsafe {
                    (self.fn_open_volume_by_handle)(vol_handle, 0, 0, 0xFFFF_FFFF, 1, &mut fve_handle)
                };
                log::info!("卷#{}: FveOpenVolumeByHandle hr=0x{:08X}", idx, ohr);
                if ohr == 0 && !fve_handle.is_null() {
                    match self.create_recovery_auth(recovery_key) {
                        Ok(auth) => {
                            let uhr = if let Some(unlock_am) = self.fn_unlock_volume_access {
                                unsafe {
                                    unlock_am(fve_handle, auth.as_ptr() as *mut c_void, std::ptr::null_mut())
                                }
                            } else {
                                unsafe { (self.fn_unlock_volume)(fve_handle, auth.as_ptr() as *mut c_void) }
                            };
                            log::info!("卷#{}: FveUnlockVolumeWithAccessMode hr=0x{:08X}", idx, uhr);
                            if uhr == 0 || uhr == FveError::VolumeUnlocked as u32 {
                                log::info!("卷#{}: 被恢复密钥解开!", idx);
                                unlocked += 1;
                            }
                        }
                        Err(e) => log::warn!("卷#{}: 构造认证元素失败: {:?}", idx, e),
                    }
                    unsafe { (self.fn_close_volume)(fve_handle); }
                }
            }
            // 就地推进枚举(卷句柄 + 同一份 spec 作迭代状态, 不重置 spec)
            let nhr = unsafe { (self.fn_find_next_volume)(vol_handle, spec.as_mut_ptr()) };
            if nhr != 0 {
                break; // ERROR_NO_MORE_FILES → 枚举结束
            }
        }
        log::info!("枚举共扫描 {} 个卷, 解开 {} 个", scanned, unlocked);

        if unlocked == 0 {
            log::warn!("没有任何卷被该恢复密钥解开");
        }
        Ok(unlocked)
    }

    /// 关闭(解密)一个**已解锁**的 BitLocker 卷 —— 正常系统场景。
    ///
    /// 前提: 卷处于已解锁状态(系统已持有 VMK/FVEK), 因此**无需任何密码/恢复密钥**。
    /// 逆向(FveConversionDecrypt 0x180043970 = FveConversionDecryptEx(h, 0); 开卷
    /// FveOpenVolumeW flags=1 → 写权限路径)证实流程为:
    ///   1) 以读写模式打开卷(已解锁卷 FveOpenVolumeW 可成功);
    ///   2) FveConversionDecryptEx(handle, 0) 开始解密。
    /// 此调用**立即返回**; 解密在驱动层后台进行, 关闭句柄不影响。
    /// 之后用 `get_status_by_path` 轮询 `conversion_status` 直到 0(FullyDecrypted)即彻底关闭。
    ///
    /// 注意: 若卷处于**锁定**状态, 应先用 `find_and_unlock_with_recovery_key` 解锁。
    pub fn start_decrypt_unlocked_volume(&self, volume_path: &str) -> Result<(), FveError> {
        let handle = self.open_volume_ex(volume_path, FveAccessMode::ReadWrite)?;
        handle.start_decryption()
        // handle 在此处 drop → FveCloseVolume; 后台解密继续
    }

    /// 关闭(解密)一个**已解锁**的 BitLocker 卷 —— 对齐 a1ive/fvetool(fvelib.c) 的极简实现。
    ///
    /// 参考实现(已被生产验证)的关键事实:
    /// - 以**普通管理员**身份即可, **不冒充 SYSTEM、不做任何令牌操作**(fvelib.c 全程无提权);
    /// - 开卷优先用 `GetVolumeNameForVolumeMountPointW` 得到的 `\\?\Volume{GUID}\`(原样带尾反斜杠);
    /// - 关闭/解密用 **FveConversionDecrypt**(读写开卷 + 该调用), 而非 FveRevertVolume;
    /// - 过去十几次失败的真因是**开卷路径**(裸盘符被当目录 → STATUS_FILE_IS_A_DIRECTORY →
    ///   假 0x80070005), 与权限无关(与 Procmon 抓到的"全程零 ACCESS DENIED"吻合)。
    ///
    /// `poll_interval_ms` / `timeout_secs` 仅为兼容签名保留(解密已发起即返回, 不再轮询)。
    pub fn decrypt_unlocked_volume_blocking(
        &self,
        volume_path: &str,
        poll_interval_ms: u64,
        timeout_secs: u64,
    ) -> Result<FveVolumeInfo, FveError> {
        // 对齐 a1ive: 纯普通管理员身份, **不冒充 SYSTEM、不做任何令牌操作**。
        // RW 开卷时由 open_volume_raw 启用管理员默认就持有的 SeManageVolumePrivilege 即可。

        // 1) 读写模式打开 —— 开卷路径优先级(对齐 a1ive):
        //    ① \\?\Volume{GUID}\  (GetVolumeNameForVolumeMountPointW 原样, **带尾反斜杠不剥**);
        //    ② \\.\X:  设备形式; ③ X:  原始盘符。第一个成功即用。
        // 对 Volume GUID 路径, 带不带尾反斜杠都会被 fveapi 正确解析为卷设备, 不会当成根目录,
        // 从而避开裸盘符的 IS_DIRECTORY → 假 0x80070005 坑。
        let mut candidates: Vec<String> = Vec::new();
        // ① \\?\Volume{GUID}\  (原样, 带尾反斜杠)
        if let Some(guid) = Self::volume_guid_for_drive(volume_path) {
            candidates.push(guid);
        }
        // ② \\.\X:  设备形式
        let dl = volume_path.trim().trim_end_matches('\\');
        if dl.len() >= 2 && dl.as_bytes()[1] == b':' {
            let letter = (dl.as_bytes()[0] as char).to_ascii_uppercase();
            candidates.push(format!("\\\\.\\{}:", letter));
        }
        // ③ 兜底: 原始盘符串
        candidates.push(volume_path.to_string());

        let mut handle_opt: Option<FveVolumeHandle> = None;
        for cand in &candidates {
            let r = if cand.starts_with("\\\\") {
                self.open_volume_raw(cand, FveAccessMode::ReadWrite)
            } else {
                self.open_volume_ex(cand, FveAccessMode::ReadWrite)
            };
            match r {
                Ok(h) => { log::info!("用路径形式 '{}' 开卷成功(READ_WRITE)", cand); handle_opt = Some(h); break; }
                Err(e) => log::warn!("路径形式 '{}' 开卷失败: {:?}", cand, e),
            }
        }
        let handle = match handle_opt {
            Some(h) => h,
            None => {
                log::warn!("所有路径形式开卷均失败");
                return Err(FveError::AccessDenied);
            }
        };

        // 2) 关闭 BitLocker(解密): 对齐 a1ive —— **首选 FveConversionDecrypt**(start_decryption);
        //    仅当其未导出/返回 NotSupported 时才回退 FveRevertVolume。
        //    成功判据 = 解密 API 返回 hr=0(解密已发起即视为成功)。
        //    不依赖句柄式 FveGetStatus(此类卷对象上读出垃圾值 0x0104xxxx, 会误判)。
        match handle.start_decryption() {
            Ok(()) => {
                log::info!("卷 {} 的 FveConversionDecrypt 返回 hr=0, 解密已发起 → 视为成功", volume_path);
            }
            Err(FveError::NotSupported) => {
                log::info!("FveConversionDecrypt 不可用(NotSupported), 回退 FveRevertVolume");
                handle.revert_volume()?; // 成功(hr=0)或返回 Err
                log::info!("卷 {} 的 FveRevertVolume 返回 hr=0, 解密已发起 → 视为成功", volume_path);
            }
            Err(e) => {
                // 真实失败: 解密未发起。绝不吞错, 返回 Err(进程退出码非0)。
                log::warn!("FveConversionDecrypt 失败: {:?}", e);
                return Err(e);
            }
        }

        // 3) 解密已发起即返回成功。真实完成进度由调用方用路径式 get_status_by_path 或 manage-bde
        //    复核, 不用返回垃圾的句柄式 status。poll/timeout 参数保留以兼容签名。
        let _ = (poll_interval_ms, timeout_secs);
        Ok(FveVolumeInfo {
            volume_status: FveVolumeStatus::DecryptionInProgress,
            protection_status: FveProtectionStatus::Unknown,
            lock_status: FveLockStatus::Unlocked,
            encryption_percentage: 0,
            encryption_flags: 0,
            volume_size: 0,
            encrypted_size: 0,
        })
    }

    /// 通过枚举关闭(解密)已解锁的 BitLocker 卷 —— 正常系统场景的**可靠**路径。
    ///
    /// 背景: 在某些环境(如无交互的服务/CI)按盘符路径调 `FveOpenVolumeW`/`FveGetStatusW`
    /// 会返回 AccessDenied(0x80070005)。可靠做法与解锁一致 —— 走 `\\.\BitLocker` 控制
    /// 设备的枚举: `FveFindFirstVolume` → `FveOpenVolumeByHandle` 取得 FVE 卷句柄。
    ///
    /// 流程: 枚举每个卷 → ByHandle 打开 → 协商读 `FveGetStatus`; 仅对**已加密**
    /// (conversion_status ≠ FullyDecrypted)的卷调 `FveConversionDecryptEx(handle, 0)`
    /// 启动解密。解密在驱动后台进行, 关句柄不影响; 无需任何密码(卷已解锁)。
    ///
    /// 返回成功启动解密的卷数。
    ///
    /// 注意: 此方法会对**所有**已加密且可读状态的卷启动解密。若需只针对某个盘符,
    /// 调用方应在拿到状态后按卷名/盘符过滤(见 `FveGetVolumeNameW`)。
    pub fn find_and_decrypt_unlocked_volumes(&self) -> Result<u32, FveError> {
        let mut vol_handle: *mut c_void = std::ptr::null_mut();
        let mut spec: [u32; 4] = [1, 0xFFFF_FFFF, 0, 0];
        let hr = unsafe { (self.fn_find_first_volume)(&mut vol_handle, spec.as_mut_ptr()) };
        if hr != 0 {
            log::warn!("FveFindFirstVolume 失败: hr=0x{:08X}", hr);
            return Err(FveError::from_hresult(hr));
        }
        let mut started = 0u32;
        let mut scanned = 0u32;
        for _ in 0..64u32 {
            if !vol_handle.is_null() && vol_handle as isize != -1 {
                scanned += 1;
                let idx = scanned;
                let mut fve_handle: *mut c_void = std::ptr::null_mut();
                let ohr = unsafe {
                    (self.fn_open_volume_by_handle)(vol_handle, 0, 0, 0xFFFF_FFFF, 1, &mut fve_handle)
                };
                if ohr == 0 && !fve_handle.is_null() {
                    // 协商读状态, 判断是否已加密
                    let mut info: Option<FveVolumeInfo> = None;
                    for &(ver, size) in FVE_STATUS_VERSIONS {
                        let mut out = FveGetStatusOutput::with_version(ver, size);
                        let shr = unsafe { (self.fn_get_status)(fve_handle, &mut out) };
                        if shr != FveError::InvalidParameter as u32 {
                            if shr == 0 {
                                info = Some(FveVolumeInfo::from_output(&out, size));
                            }
                            break;
                        }
                    }
                    match info {
                        Some(i) if i.volume_status != FveVolumeStatus::FullyDecrypted => {
                            let dhr = unsafe { (self.fn_conversion_decrypt_ex)(fve_handle, 0) };
                            log::info!(
                                "卷#{}: 已加密({:?},{}%), FveConversionDecryptEx hr=0x{:08X}",
                                idx, i.volume_status, i.encryption_percentage, dhr
                            );
                            if dhr == 0 || dhr == FveError::VolumeUnlocked as u32 {
                                started += 1;
                            }
                        }
                        Some(i) => {
                            log::debug!("卷#{}: 未加密({:?}), 跳过", idx, i.volume_status);
                        }
                        None => {
                            log::debug!("卷#{}: 状态不可读, 跳过", idx);
                        }
                    }
                    unsafe { (self.fn_close_volume)(fve_handle); }
                }
            }
            let nhr = unsafe { (self.fn_find_next_volume)(vol_handle, spec.as_mut_ptr()) };
            if nhr != 0 {
                break;
            }
        }
        log::info!("解密: 共扫描 {} 个卷, 启动解密 {} 个", scanned, started);
        Ok(started)
    }

    /// 通过盘符把卷映射到卷 GUID 路径(kernel32 GetVolumeNameForVolumeMountPointW, 动态加载)。
    /// 返回形如 `\\?\Volume{GUID}\` 的字符串。
    fn volume_guid_for_drive(drive: &str) -> Option<String> {
        let mut mount = drive.trim_end_matches('\u{5c}').to_string();
        if mount.ends_with(':') {
            mount.push('\u{5c}');
        }
        let wide = to_wide_string(&mount);
        let mut buf = [0u16; 128];
        unsafe {
            let lib = libloading::Library::new("kernel32.dll").ok()?;
            let f: libloading::Symbol<
                unsafe extern "system" fn(*const u16, *mut u16, u32) -> i32,
            > = lib.get(b"GetVolumeNameForVolumeMountPointW").ok()?;
            if f(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) == 0 {
                return None;
            }
        }
        let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
        Some(String::from_utf16_lossy(&buf[..n]))
    }

    /// 取枚举句柄当前卷的设备名(FveGetVolumeNameW)。
    fn volume_name_of(&self, vol_handle: *mut c_void) -> Option<String> {
        let getname = self.fn_get_volume_name?;
        let mut buf = [0u16; 260];
        let mut len: u32 = 260;
        let hr = unsafe { getname(vol_handle, &mut len, buf.as_mut_ptr()) };
        if hr != 0 {
            return None;
        }
        let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
        Some(String::from_utf16_lossy(&buf[..n]))
    }

    /// 关闭(解密)指定盘符的 BitLocker 卷 —— 正常系统、已解锁、无需密码。
    ///
    /// 路径式 FveOpenVolumeW/FveGetStatusW 在独立进程里会 AccessDenied, 故全程走
    /// 枚举(FveFindFirstVolume/Next)+ FveOpenVolumeByHandle 拿句柄(与解锁同一可靠路径),
    /// 再在该句柄上 FveConversionDecrypt + 句柄式 FveGetStatus 轮询。
    /// 用 GetVolumeNameForVolumeMountPointW 把盘符映射为卷 GUID 来匹配目标卷。
    pub fn find_and_decrypt_drive_blocking(
        &self,
        drive: &str,
        poll_interval_ms: u64,
        timeout_secs: u64,
    ) -> Result<FveVolumeInfo, FveError> {
        // 对齐 a1ive: 纯普通管理员身份即可, 仅启用 SeManageVolumePrivilege,
        // **不冒充 SYSTEM、不做任何令牌操作**。走 枚举+ByHandle(\\.\BitLocker 控制设备)。
        enable_volume_privileges();

        let target = Self::volume_guid_for_drive(drive);
        match &target {
            Some(g) => log::info!("目标盘 {} → 卷GUID {}", drive, g),
            None => log::warn!("无法解析 {} 的卷GUID(将按‘唯一加密未锁卷’回退匹配)", drive),
        }
        let norm = |s: &str| s.trim_end_matches('\u{5c}').to_ascii_lowercase();
        let target_n = target.as_deref().map(norm);

        let mut vol_handle: *mut c_void = std::ptr::null_mut();
        let mut spec: [u32; 4] = [1, 0xFFFF_FFFF, 0, 0];
        let hr = unsafe { (self.fn_find_first_volume)(&mut vol_handle, spec.as_mut_ptr()) };
        if hr != 0 {
            log::warn!("FveFindFirstVolume 失败: hr=0x{:08X}", hr);
            return Err(FveError::from_hresult(hr));
        }

        // 回退候选: 当无法解析盘符 GUID 时, 记录唯一的“加密+未锁”卷
        let mut fallback: Option<*mut c_void> = None;
        let mut fallback_count = 0u32;
        let mut matched: Option<*mut c_void> = None;

        for _ in 0..64u32 {
            if !vol_handle.is_null() && vol_handle as isize != -1 {
                let name = self.volume_name_of(vol_handle);
                if let (Some(t), Some(n)) = (&target_n, &name) {
                    if &norm(n) == t {
                        log::info!("匹配到目标卷: {}", n);
                        matched = Some(vol_handle);
                        break;
                    }
                }
                // 回退: 开卷查状态, 找加密且未锁的卷
                if target_n.is_none() {
                    let mut fh: *mut c_void = std::ptr::null_mut();
                    let ohr = unsafe {
                        (self.fn_open_volume_by_handle)(vol_handle, 0, 0, 0xFFFF_FFFF, 1, &mut fh)
                    };
                    if ohr == 0 && !fh.is_null() {
                        let tmp = FveVolumeHandle { handle: fh, api: self, kernel_handle: std::ptr::null_mut() };
                        if let Ok(info) = tmp.get_status() {
                            if info.volume_status != FveVolumeStatus::FullyDecrypted {
                                fallback = Some(vol_handle);
                                fallback_count += 1;
                            }
                        }
                        // tmp drop 关闭 fh
                    }
                }
            }
            let nhr = unsafe { (self.fn_find_next_volume)(vol_handle, spec.as_mut_ptr()) };
            if nhr != 0 {
                break;
            }
        }

        let chosen = matched.or_else(|| {
            if fallback_count == 1 {
                log::info!("回退: 采用唯一的加密未锁卷");
                fallback
            } else {
                if fallback_count > 1 {
                    log::warn!("回退失败: 有 {} 个加密未锁卷, 无法确定目标", fallback_count);
                }
                None
            }
        });

        let vh = match chosen {
            Some(v) => v,
            None => {
                log::warn!("未找到目标卷 {}", drive);
                return Err(FveError::NotBitLockerVolume);
            }
        };

        // 用 ByHandle 打开目标, 包成 RAII 句柄
        let mut fve_handle: *mut c_void = std::ptr::null_mut();
        let ohr = unsafe {
            (self.fn_open_volume_by_handle)(vh, 0, 0, 0xFFFF_FFFF, 1, &mut fve_handle)
        };
        if ohr != 0 || fve_handle.is_null() {
            log::warn!("FveOpenVolumeByHandle(目标) 失败: hr=0x{:08X}", ohr);
            return Err(FveError::from_hresult(ohr));
        }
        let handle = FveVolumeHandle { handle: fve_handle, api: self, kernel_handle: std::ptr::null_mut() };

        match handle.get_status() {
            Ok(info) => log::info!(
                "解密前(句柄): status={:?} protection={:?} {}%",
                info.volume_status, info.protection_status, info.encryption_percentage
            ),
            Err(e) => log::warn!("句柄式 get_status 失败: {:?}", e),
        }

        // 对齐 a1ive: 首选 FveConversionDecrypt, 仅 NotSupported 时回退 FveRevertVolume。
        match handle.start_decryption() {
            Ok(()) => {}
            Err(FveError::NotSupported) => {
                log::info!("FveConversionDecrypt 不可用, 回退 FveRevertVolume");
                handle.revert_volume()?;
            }
            Err(e) => {
                log::warn!("FveConversionDecrypt 失败: {:?}", e);
                return Err(e);
            }
        }
        log::info!("BitLocker 关闭已发起, 开始轮询...");

        let start = std::time::Instant::now();
        let mut last_seen = FveVolumeStatus::Unknown;
        loop {
            match handle.get_status() {
                Ok(info) => {
                    last_seen = info.volume_status;
                    if info.volume_status == FveVolumeStatus::FullyDecrypted {
                        log::info!("卷 {} 已彻底解密(BitLocker 关闭)", drive);
                        return Ok(info);
                    }
                }
                Err(e) => log::debug!("轮询 get_status: {:?}", e),
            }
            if start.elapsed().as_secs() >= timeout_secs {
                // 句柄式状态对枚举句柄可能读不准; 转换已成功发起即视为成功,
                // 真实进度由 CI 的 manage-bde 复核确认。
                log::warn!(
                    "轮询超时(句柄状态={:?}); 转换已发起, 完成情况以 manage-bde 复核为准",
                    last_seen
                );
                return Ok(FveVolumeInfo {
                    volume_status: FveVolumeStatus::DecryptionInProgress,
                    protection_status: FveProtectionStatus::Unknown,
                    lock_status: FveLockStatus::Unlocked,
                    encryption_percentage: 0,
                    encryption_flags: 0,
                    volume_size: 0,
                    encrypted_size: 0,
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(poll_interval_ms));
        }
    }

    /// 创建密码认证元素
    fn create_passphrase_auth(&self, passphrase: &str) -> Result<AuthBundle, FveError> {
        let wide = to_wide_string(passphrase);
        let mut leaf: Box<[u32; 8]> = Box::new([0u32; 8]);
        leaf[0] = 0x20; // dwSize
        leaf[1] = 1;    // dwVersion
        let hr = unsafe {
            (self.fn_auth_from_passphrase)(wide.as_ptr(), leaf.as_mut_ptr() as *mut c_void)
        };
        if hr != 0 {
            return Err(FveError::from_hresult(hr));
        }
        Ok(AuthBundle::wrap(leaf))
    }

    /// 创建恢复密钥认证元素(并包进解锁核心要求的 0x38 容器)
    fn create_recovery_auth(&self, recovery_key: &str) -> Result<AuthBundle, FveError> {
        let wide = to_wide_string(recovery_key);
        // 叶子: 0x20 字节 FVE_AUTH_ELEMENT, 头部 {dwSize=0x20, dwVersion=1}, 函数填密钥
        let mut leaf: Box<[u32; 8]> = Box::new([0u32; 8]);
        leaf[0] = 0x20;
        leaf[1] = 1;
        let hr = unsafe {
            (self.fn_auth_from_recovery)(wide.as_ptr(), leaf.as_mut_ptr() as *mut c_void)
        };
        if hr != 0 {
            return Err(FveError::from_hresult(hr));
        }
        Ok(AuthBundle::wrap(leaf))
    }
}

/// 解锁核心(0x1800199c8)要求顶层 authElement 是 0x38 字节“容器”:
///   [0x00]=0x38(size)  [0x04]=1(version)  [0x0C]=count  [0x10]=指向元素指针数组
/// 数组里放叶子元素(FveAuthElementFromRecoveryPasswordW 产出的 0x20 结构)的指针。
/// 三层(容器→数组→叶子)必须同时存活到 FveUnlockVolume 调用结束, 故打包在一起。
#[cfg(windows)]
pub struct AuthBundle {
    _leaf: Box<[u32; 8]>,     // 0x20 叶子(密钥)
    _array: Box<[usize; 1]>,  // [&leaf]
    container: Box<[u8; 0x38]>,
}

#[cfg(windows)]
impl AuthBundle {
    fn wrap(leaf: Box<[u32; 8]>) -> AuthBundle {
        let array: Box<[usize; 1]> = Box::new([leaf.as_ptr() as usize]);
        let mut container: Box<[u8; 0x38]> = Box::new([0u8; 0x38]);
        container[0x00..0x04].copy_from_slice(&0x38u32.to_le_bytes()); // size
        container[0x04..0x08].copy_from_slice(&1u32.to_le_bytes());    // version (= rax+1, rax=0)
        container[0x0C..0x10].copy_from_slice(&1u32.to_le_bytes());    // count = 1
        let arr_addr = array.as_ptr() as usize as u64;
        container[0x10..0x18].copy_from_slice(&arr_addr.to_le_bytes()); // 指向数组
        AuthBundle { _leaf: leaf, _array: array, container }
    }
    fn as_ptr(&self) -> *mut c_void {
        self.container.as_ptr() as *mut c_void
    }
}

// ==================== FveVolumeHandle 实现 ====================

/// FVE卷句柄包装器（RAII模式）
#[cfg(windows)]
pub struct FveVolumeHandle<'a> {
    handle: *mut c_void,
    api: &'a FveApi,
    /// 若经 open_volume_for_unlock(CreateFileW)取得则保存内核句柄,Drop 时关闭;
    /// 普通 FveOpenVolumeW 路径为 null。
    kernel_handle: *mut c_void,
}

#[cfg(windows)]
impl<'a> FveVolumeHandle<'a> {
    /// 获取卷状态
    pub fn get_status(&self) -> Result<FveVolumeInfo, FveError> {
        // 版本协商，同 get_status_by_path
        let mut last_err = FveError::NotSupported;
        for &(ver, size) in FVE_STATUS_VERSIONS {
            let mut status_output = FveGetStatusOutput::with_version(ver, size);
            let hr = unsafe { (self.api.fn_get_status)(self.handle, &mut status_output) };
            if hr == 0 {
                return Ok(FveVolumeInfo::from_output(&status_output, size));
            }
            if hr != FveError::InvalidParameter as u32 {
                return Err(FveError::from_hresult(hr));
            }
            last_err = FveError::InvalidParameter;
        }
        Err(last_err)
    }

    /// 使用密码解锁卷
    pub fn unlock_with_password(&self, password: &str) -> Result<(), FveError> {
        let auth_element = self.api.create_passphrase_auth(password)?;
        let hr = if let Some(unlock_am) = self.api.fn_unlock_volume_access {
            unsafe { unlock_am(self.handle, auth_element.as_ptr() as *mut c_void, std::ptr::null_mut()) }
        } else {
            unsafe { (self.api.fn_unlock_volume)(self.handle, auth_element.as_ptr() as *mut c_void) }
        };

        // 0x80310023 = FVE_E_VOLUME_NOT_LOCKED: 卷本来就没锁，视为成功
        if hr == 0 || hr == FveError::VolumeUnlocked as u32 {
            log::info!("卷使用密码解锁成功(或本就未锁定)");
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
        let hr = if let Some(unlock_am) = self.api.fn_unlock_volume_access {
            unsafe { unlock_am(self.handle, auth_element.as_ptr() as *mut c_void, std::ptr::null_mut()) }
        } else {
            unsafe { (self.api.fn_unlock_volume)(self.handle, auth_element.as_ptr() as *mut c_void) }
        };

        // 0x80310023 = FVE_E_VOLUME_NOT_LOCKED: 卷本来就没锁，视为成功
        if hr == 0 || hr == FveError::VolumeUnlocked as u32 {
            log::info!("卷使用恢复密钥解锁成功(或本就未锁定)");
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

    /// 关闭 BitLocker / 解密卷 —— 调用 `FveRevertVolume`(bdesvc 关闭 BitLocker 实际入口)。
    ///
    /// 逆向: bdesvc 关闭 BitLocker 导入的是 FveRevertVolume(单参数=卷句柄), 而非
    /// 被驱动门禁的底层 FveConversionDecrypt。FveRevertVolume 内部以 mode=1 构造上下文后
    /// 走 0x180056c68 这条独立的 revert 实现。这才是"无密码关闭已解锁卷"的正确调用。
    pub fn revert_volume(&self) -> Result<(), FveError> {
        let f = self
            .api
            .fn_revert_volume
            .ok_or(FveError::NotSupported)?;
        let hr = unsafe { f(self.handle) };
        if hr == 0 {
            log::info!("FveRevertVolume 成功(hr=0): BitLocker 关闭已发起");
            Ok(())
        } else {
            let error = FveError::from_hresult(hr);
            log::warn!("FveRevertVolume 失败: hr=0x{:08X}, error={:?}", hr, error);
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
        if !self.kernel_handle.is_null() {
            unsafe { CloseHandle(self.kernel_handle); }
        }
    }
}

// ==================== 辅助函数 ====================

/// 将 Rust 字符串转换为以 null 结尾的宽字符串
fn to_wide_string(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[repr(C)]
struct Luid {
    low: u32,
    high: i32,
}
#[repr(C)]
struct LuidAndAttributes {
    luid: Luid,
    attributes: u32,
}
#[repr(C)]
struct TokenPrivileges {
    count: u32,
    privilege: LuidAndAttributes,
}

/// 在当前进程令牌里启用卷管理权限(仅 `SeManageVolumePrivilege`)。
///
/// 对齐 a1ive/fvetool: 关闭已解锁卷的 BitLocker 只需**普通管理员**, 管理员令牌默认
/// **持有但未启用** `SeManageVolumePrivilege`, 这里把它启用即可。**不再启用 SeDebug 等**
/// (那是旧的 SYSTEM 令牌冒充方案所需, 现已彻底不用)。失败不致命(仅记录)。
pub fn enable_volume_privileges() {
    const TOKEN_ADJUST_PRIVILEGES: u32 = 0x0020;
    const TOKEN_QUERY: u32 = 0x0008;
    const SE_PRIVILEGE_ENABLED: u32 = 0x0002;

    // 仅启用管理员默认就持有的 SeManageVolumePrivilege(对齐 a1ive 的极简实现)。
    let names = [
        "SeManageVolumePrivilege",
    ];

    unsafe {
        let advapi = match libloading::Library::new("advapi32.dll") {
            Ok(l) => l,
            Err(e) => {
                log::warn!("加载 advapi32.dll 失败: {}", e);
                return;
            }
        };
        let kernel = match libloading::Library::new("kernel32.dll") {
            Ok(l) => l,
            Err(e) => {
                log::warn!("加载 kernel32.dll 失败: {}", e);
                return;
            }
        };

        type FnOpenProcessToken =
            unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32;
        type FnLookupPriv =
            unsafe extern "system" fn(*const u16, *const u16, *mut Luid) -> i32;
        type FnAdjust = unsafe extern "system" fn(
            *mut c_void,
            i32,
            *const TokenPrivileges,
            u32,
            *mut c_void,
            *mut u32,
        ) -> i32;
        type FnCloseHandle = unsafe extern "system" fn(*mut c_void) -> i32;
        type FnGetLastError = unsafe extern "system" fn() -> u32;

        let open_token: libloading::Symbol<FnOpenProcessToken> =
            match advapi.get(b"OpenProcessToken") {
                Ok(f) => f,
                Err(_) => return,
            };
        let lookup: libloading::Symbol<FnLookupPriv> =
            match advapi.get(b"LookupPrivilegeValueW") {
                Ok(f) => f,
                Err(_) => return,
            };
        let adjust: libloading::Symbol<FnAdjust> =
            match advapi.get(b"AdjustTokenPrivileges") {
                Ok(f) => f,
                Err(_) => return,
            };
        let close: libloading::Symbol<FnCloseHandle> = match kernel.get(b"CloseHandle") {
            Ok(f) => f,
            Err(_) => return,
        };
        let get_last_error: Option<libloading::Symbol<FnGetLastError>> =
            kernel.get(b"GetLastError").ok();

        // GetCurrentProcess() 伪句柄 = -1
        let cur_proc = (-1isize) as *mut c_void;
        let mut token: *mut c_void = std::ptr::null_mut();
        if open_token(cur_proc, TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token) == 0
            || token.is_null()
        {
            log::warn!("OpenProcessToken 失败");
            return;
        }

        for name in names {
            let wname = to_wide_string(name);
            let mut luid = Luid { low: 0, high: 0 };
            if lookup(std::ptr::null(), wname.as_ptr(), &mut luid) == 0 {
                log::debug!("LookupPrivilegeValueW({}) 失败(系统可能不支持)", name);
                continue;
            }
            let tp = TokenPrivileges {
                count: 1,
                privilege: LuidAndAttributes {
                    luid,
                    attributes: SE_PRIVILEGE_ENABLED,
                },
            };
            let ok = adjust(
                token,
                0,
                &tp,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            // AdjustTokenPrivileges 即使权限未被授予也可能返回非0; 必须查 GetLastError:
            // ERROR_NOT_ALL_ASSIGNED(1300) 表示账户根本不持有该权限, 启用是无效的。
            const ERROR_NOT_ALL_ASSIGNED: u32 = 1300;
            let last = get_last_error.as_ref().map(|f| f()).unwrap_or(0);
            if ok == 0 {
                log::warn!("AdjustTokenPrivileges({}) 调用失败", name);
            } else if last == ERROR_NOT_ALL_ASSIGNED {
                log::warn!("权限 {} 未实际授予(账户不持有, NOT_ALL_ASSIGNED)", name);
            } else {
                log::info!("权限 {} 已实际启用", name);
            }
        }
        close(token);
        log::info!("卷管理权限启用流程结束(逐项结果见上)");
    }
}

#[repr(C)]
struct ProcessEntry32W {
    dw_size: u32,
    cnt_usage: u32,
    th32_process_id: u32,
    th32_default_heap_id: usize,
    th32_module_id: u32,
    cnt_threads: u32,
    th32_parent_process_id: u32,
    pc_pri_class_base: i32,
    dw_flags: u32,
    sz_exe_file: [u16; 260],
}

/// 在当前线程冒充 SYSTEM 身份(复制 winlogon.exe 的令牌)。
///
/// 逆向确认: 解密走 `FveOpenVolumeW(path, RW)` → 内层虚表槽3 用 `CreateFileW` 以
/// `0xC0000000`(读写)打开 BitLocker 卷元数据(`SEI`/`HEI`), 这些对象是 SYSTEM-only
/// DACL, 管理员也被拒(0x80070005)。且 fveapi 的 CreateFileW 不带 BACKUP_SEMANTICS,
/// 故 SeBackup/SeRestore 无法绕过 DACL。`manage-bde -off` 之所以能成, 是委托给 bdesvc
/// (SYSTEM)。因此必须以 SYSTEM 身份开卷。这里复制 winlogon 令牌并 SetThreadToken 冒充。
/// 成功返回 true; 用完应调用 `revert_to_self()`。
pub fn impersonate_system() -> bool {
    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const TOKEN_DUPLICATE: u32 = 0x0002;
    const TOKEN_QUERY: u32 = 0x0008;
    const MAXIMUM_ALLOWED: u32 = 0x0200_0000;
    const SECURITY_IMPERSONATION: i32 = 2; // SecurityImpersonation
    const TOKEN_IMPERSONATION: i32 = 2; // TokenImpersonation
    const INVALID_HANDLE: isize = -1;

    unsafe {
        let kernel = match libloading::Library::new("kernel32.dll") {
            Ok(l) => l,
            Err(_) => return false,
        };
        let advapi = match libloading::Library::new("advapi32.dll") {
            Ok(l) => l,
            Err(_) => return false,
        };

        type FnSnap = unsafe extern "system" fn(u32, u32) -> *mut c_void;
        type FnP32 = unsafe extern "system" fn(*mut c_void, *mut ProcessEntry32W) -> i32;
        type FnOpenProc = unsafe extern "system" fn(u32, i32, u32) -> *mut c_void;
        type FnCloseHandle = unsafe extern "system" fn(*mut c_void) -> i32;
        type FnOpenProcTok =
            unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32;
        type FnDupTokEx = unsafe extern "system" fn(
            *mut c_void,
            u32,
            *mut c_void,
            i32,
            i32,
            *mut *mut c_void,
        ) -> i32;
        type FnSetThreadToken =
            unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32;

        macro_rules! sym {
            ($lib:expr, $name:expr, $t:ty) => {
                match $lib.get::<$t>($name) {
                    Ok(f) => f,
                    Err(_) => {
                        log::warn!("找不到符号 {}", String::from_utf8_lossy($name));
                        return false;
                    }
                }
            };
        }

        let create_snap = sym!(kernel, b"CreateToolhelp32Snapshot", FnSnap);
        let p32first = sym!(kernel, b"Process32FirstW", FnP32);
        let p32next = sym!(kernel, b"Process32NextW", FnP32);
        let open_proc = sym!(kernel, b"OpenProcess", FnOpenProc);
        let close: libloading::Symbol<FnCloseHandle> = sym!(kernel, b"CloseHandle", FnCloseHandle);
        let open_proc_tok = sym!(advapi, b"OpenProcessToken", FnOpenProcTok);
        let dup_tok = sym!(advapi, b"DuplicateTokenEx", FnDupTokEx);
        let set_thread_tok = sym!(advapi, b"SetThreadToken", FnSetThreadToken);

        // 1) 找 winlogon.exe(必为 SYSTEM)
        let snap = create_snap(TH32CS_SNAPPROCESS, 0);
        if snap.is_null() || snap as isize == INVALID_HANDLE {
            log::warn!("CreateToolhelp32Snapshot 失败");
            return false;
        }
        let mut pe: ProcessEntry32W = std::mem::zeroed();
        pe.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;
        let mut pid: u32 = 0;
        let targets = ["winlogon.exe", "lsass.exe", "services.exe"];
        if p32first(snap, &mut pe) != 0 {
            'outer: loop {
                let end = pe.sz_exe_file.iter().position(|&c| c == 0).unwrap_or(0);
                let name = String::from_utf16_lossy(&pe.sz_exe_file[..end]).to_ascii_lowercase();
                if targets.contains(&name.as_str()) {
                    pid = pe.th32_process_id;
                    log::info!("找到 SYSTEM 进程 {} (pid={})", name, pid);
                    if name == "winlogon.exe" {
                        break 'outer; // 首选
                    }
                }
                if p32next(snap, &mut pe) == 0 {
                    break;
                }
            }
        }
        close(snap);
        if pid == 0 {
            log::warn!("未找到 SYSTEM 进程(winlogon/lsass/services)");
            return false;
        }

        // 2) 打开进程 → 取令牌 → 复制为冒充令牌
        let hproc = open_proc(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if hproc.is_null() {
            log::warn!("OpenProcess(pid={}) 失败", pid);
            return false;
        }
        let mut htok: *mut c_void = std::ptr::null_mut();
        if open_proc_tok(hproc, TOKEN_DUPLICATE | TOKEN_QUERY, &mut htok) == 0 || htok.is_null() {
            log::warn!("OpenProcessToken 失败");
            close(hproc);
            return false;
        }
        let mut hdup: *mut c_void = std::ptr::null_mut();
        let dok = dup_tok(
            htok,
            MAXIMUM_ALLOWED,
            std::ptr::null_mut(),
            SECURITY_IMPERSONATION,
            TOKEN_IMPERSONATION,
            &mut hdup,
        );
        close(htok);
        close(hproc);
        if dok == 0 || hdup.is_null() {
            log::warn!("DuplicateTokenEx 失败");
            return false;
        }

        // 3) 在当前线程冒充
        let sok = set_thread_tok(std::ptr::null_mut(), hdup);
        close(hdup);
        if sok == 0 {
            log::warn!("SetThreadToken 失败");
            return false;
        }
        log::info!("已冒充 SYSTEM 身份(线程级)");
        true
    }
}

/// 撤销线程冒充, 恢复进程自身身份。
pub fn revert_to_self() {
    unsafe {
        if let Ok(advapi) = libloading::Library::new("advapi32.dll") {
            type FnRevert = unsafe extern "system" fn() -> i32;
            if let Ok(f) = advapi.get::<FnRevert>(b"RevertToSelf") {
                f();
                log::debug!("RevertToSelf 完成");
            }
        }
    }
}

/// 返回当前进程令牌的用户 SID 字符串(如 SYSTEM = "S-1-5-18")。用于确认运行身份。
pub fn whoami_sid() -> String {
    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_USER: i32 = 1; // TokenUser
    unsafe {
        let kernel = match libloading::Library::new("kernel32.dll") {
            Ok(l) => l,
            Err(_) => return "?".into(),
        };
        let advapi = match libloading::Library::new("advapi32.dll") {
            Ok(l) => l,
            Err(_) => return "?".into(),
        };
        type FnOpenProcTok =
            unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32;
        type FnGetTokenInfo = unsafe extern "system" fn(
            *mut c_void,
            i32,
            *mut c_void,
            u32,
            *mut u32,
        ) -> i32;
        type FnConvSid = unsafe extern "system" fn(*mut c_void, *mut *mut u16) -> i32;
        type FnLocalFree = unsafe extern "system" fn(*mut c_void) -> *mut c_void;
        type FnCloseHandle = unsafe extern "system" fn(*mut c_void) -> i32;

        let open_tok: libloading::Symbol<FnOpenProcTok> =
            match advapi.get(b"OpenProcessToken") { Ok(f) => f, Err(_) => return "?".into() };
        let get_info: libloading::Symbol<FnGetTokenInfo> =
            match advapi.get(b"GetTokenInformation") { Ok(f) => f, Err(_) => return "?".into() };
        let conv: libloading::Symbol<FnConvSid> =
            match advapi.get(b"ConvertSidToStringSidW") { Ok(f) => f, Err(_) => return "?".into() };
        let local_free: libloading::Symbol<FnLocalFree> =
            match kernel.get(b"LocalFree") { Ok(f) => f, Err(_) => return "?".into() };
        let close: libloading::Symbol<FnCloseHandle> =
            match kernel.get(b"CloseHandle") { Ok(f) => f, Err(_) => return "?".into() };

        let cur = (-1isize) as *mut c_void;
        let mut tok: *mut c_void = std::ptr::null_mut();
        if open_tok(cur, TOKEN_QUERY, &mut tok) == 0 {
            return "?(OpenProcessToken失败)".into();
        }
        let mut len: u32 = 0;
        get_info(tok, TOKEN_USER, std::ptr::null_mut(), 0, &mut len);
        if len == 0 {
            close(tok);
            return "?(GetTokenInformation长度0)".into();
        }
        let mut buf = vec![0u8; len as usize];
        if get_info(tok, TOKEN_USER, buf.as_mut_ptr() as *mut c_void, len, &mut len) == 0 {
            close(tok);
            return "?(GetTokenInformation失败)".into();
        }
        // TOKEN_USER { SID_AND_ATTRIBUTES { PSID Sid; DWORD Attributes } }; Sid 在偏移0
        let psid = *(buf.as_ptr() as *const *mut c_void);
        let mut sid_str: *mut u16 = std::ptr::null_mut();
        let result = if conv(psid, &mut sid_str) != 0 && !sid_str.is_null() {
            let end = (0..).take_while(|&i| *sid_str.add(i) != 0).count();
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(sid_str, end));
            local_free(sid_str as *mut c_void);
            s
        } else {
            "?(ConvertSid失败)".into()
        };
        close(tok);
        result
    }
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
    pub fn probe_status_version(&self, _volume_path: &str) -> Option<(u32, u32)> {
        None
    }

    pub fn open_volume(&self, _volume_path: &str) -> Result<FveVolumeHandle<'_>, FveError> {
        Err(FveError::NotSupported)
    }
    pub fn open_volume_for_unlock(&self, _volume_path: &str) -> Result<FveVolumeHandle<'_>, FveError> {
        Err(FveError::NotSupported)
    }
    pub fn find_and_unlock_with_recovery_key(&self, _recovery_key: &str) -> Result<u32, FveError> {
        Err(FveError::NotSupported)
    }
    pub fn start_decrypt_unlocked_volume(&self, _volume_path: &str) -> Result<(), FveError> {
        Err(FveError::NotSupported)
    }
    pub fn find_and_decrypt_unlocked_volumes(&self) -> Result<u32, FveError> {
        Err(FveError::NotSupported)
    }
    pub fn decrypt_unlocked_volume_blocking(
        &self,
        _volume_path: &str,
        _poll_interval_ms: u64,
        _timeout_secs: u64,
    ) -> Result<FveVolumeInfo, FveError> {
        Err(FveError::NotSupported)
    }
    pub fn find_and_decrypt_drive_blocking(
        &self,
        _drive: &str,
        _poll_interval_ms: u64,
        _timeout_secs: u64,
    ) -> Result<FveVolumeInfo, FveError> {
        Err(FveError::NotSupported)
    }

    pub fn open_volume_ex(&self, _volume_path: &str, _access_mode: FveAccessMode) -> Result<FveVolumeHandle<'_>, FveError> {
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
        assert_eq!(std::mem::size_of::<FveGetStatusOutput>(), 0x78);
    }

    #[test]
    fn test_fve_get_status_output_default() {
        let output = FveGetStatusOutput::default();
        assert_eq!(output.size, 0x78);
        assert_eq!(output.version, 8);
        assert_eq!(output.conversion_status, 0);
        assert_eq!(output.protection_status, 0);
        assert!(!output.is_encrypted());
        #[allow(deprecated)]
        let _prot = output.is_protection_on();
        assert!(!_prot);
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
        assert_eq!(FveVolumeStatus::from(3), FveVolumeStatus::DecryptionInProgress);
        assert_eq!(FveVolumeStatus::from(99), FveVolumeStatus::Unknown); // 非法值记为 Unknown, 不当作已解密
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

    #[test]
    fn test_version_table() {
        // 表非空、从最新版本(8/0x78)开始、version 严格递减
        assert!(!FVE_STATUS_VERSIONS.is_empty());
        assert_eq!(FVE_STATUS_VERSIONS[0], (8, 0x78));
        for w in FVE_STATUS_VERSIONS.windows(2) {
            assert!(w[0].0 > w[1].0, "version 必须递减");
        }
        // 已逆向确认的两个真实组合都在表里
        assert!(FVE_STATUS_VERSIONS.contains(&(8, 0x78))); // 19043
        assert!(FVE_STATUS_VERSIONS.contains(&(5, 0x58))); // 1709
    }

    #[test]
    fn test_with_version() {
        let o = FveGetStatusOutput::with_version(5, 0x58);
        assert_eq!(o.version, 5);
        assert_eq!(o.size, 0x58);
        // 缓冲区本身仍是 0x78 字节
        assert_eq!(std::mem::size_of::<FveGetStatusOutput>(), 0x78);
    }

    #[test]
    fn test_from_output_size_guard() {
        // 构造一个所有字段都非零的输出
        let mut o = FveGetStatusOutput::default();
        o.conversion_status = 1;
        o.protection_status = 1;
        o.volume_size = 0x1000;
        o.encryption_flags = 0x10;

        // 按 v8(0x78) 解析: encryption_flags / volume_size 都应被读取
        let full = FveVolumeInfo::from_output(&o, 0x78);
        assert_eq!(full.encryption_flags, 0x10);
        assert_eq!(full.volume_size, 0x1000);

        // 按 v5(0x58) 解析: encryption_flags(@0x70) 未被 DLL 填充 → 守卫为 0；
        // volume_size(@0x50, 需 size≥0x58) 仍有效
        let v5 = FveVolumeInfo::from_output(&o, 0x58);
        assert_eq!(v5.encryption_flags, 0, "size=0x58 时 flags 必须守卫为 0");
        assert_eq!(v5.volume_size, 0x1000);

        // 按 v4(0x40) 解析: volume_size(@0x50) 也未填充 → 守卫为 0
        let v4 = FveVolumeInfo::from_output(&o, 0x40);
        assert_eq!(v4.volume_size, 0, "size=0x40 时 volume_size 必须守卫为 0");
        assert_eq!(v4.encryption_flags, 0);
    }
}
