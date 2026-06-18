//! FVEAPI.dll 动态加载模块（BitLocker / 全卷加密）
//!
//! 本模块是 a1ive/fvetool 的 `fvelib/fvelib.c`（GPL-3.0）的忠实 Rust 移植，
//! 该参考实现经生产验证、行为正确。关键事实（均来自参考实现，勿臆改）：
//!
//! - **FVE_GET_STATUS_OUTPUT**：`dwSize = 0x80`、`dwVersion = 2`，对 `E_INVALIDARG`
//!   回退 `dwVersion = 1`；只读取 `ConversionStatus(@0x0C)` 与 `ProtectionStatus(@0x38)`。
//! - **开卷**：统一走 `FveOpenVolumeW`，优先把盘符映射成 `\\?\Volume{GUID}\`
//!   （`GetVolumeNameForVolumeMountPointW`，保留尾反斜杠）再开。**锁定卷**用该路径开卷
//!   会返回 `0x80310000(VolumeLocked)` 但**仍写出可用句柄**；故除该码外的失败才算失败
//!   （见 `fve_failed`）。无需 `CreateFileW` / `FveOpenVolumeByHandle` / SYSTEM 冒充。
//! - **解锁**：认证元素是 **584 字节**结构 `{ MagicValue, MustBeOne=1, .. }`，
//!   口令 Magic=578、恢复密钥 Magic=32；外层 `FVE_UNLOCK_SETTINGS`（x64=0x38 字节）
//!   含 `SecretType`（口令 0x00800000 / 恢复 0x00080000）。优先用
//!   `FveUnlockVolumeWithAccessMode(handle, &settings, 0)`，无该导出时回退
//!   `FveUnlockVolume(handle, &authElement)`。
//! - **解密（关闭 BitLocker）**：读写开卷 + `FveConversionDecrypt`（或 Ex）。
//!
//! 仅以普通管理员身份运行即可（程序清单已请求 requireAdministrator）。

// =====================================================================================
// 平台无关：枚举与公共类型（两端/调用方共用）
// =====================================================================================

/// FveOpenVolumeW 访问模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FveAccessMode {
    /// 只读：状态查询 / 解锁
    ReadOnly = 0,
    /// 读写：解密 / 加密 / 锁定
    ReadWrite = 1,
}

/// FVE API 错误码（HRESULT 子集）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FveError {
    Success = 0,
    InvalidParameter = 0x80070057,
    AccessDenied = 0x80070005,
    /// 卷已锁定（FVE_E_LOCKED_VOLUME）
    VolumeLocked = 0x80310000,
    /// 卷不支持/未激活 BitLocker（FVE_E_NOT_ACTIVATED）
    NotSupported = 0x80310001,
    /// 卷未加密（FVE_E_NOT_ENCRYPTED / NOT_ACTIVATED 取值）
    NotEncrypted = 0x80310008,
    KeyRequired = 0x80310044,
    AuthenticationFailed = 0x8031000D,
    BadPassword = 0x80310027,
    BadRecoveryPassword = 0x80310028,
    /// 卷本就未锁定（FVE_E_VOLUME_NOT_LOCKED）
    VolumeUnlocked = 0x80310023,
    NotBitLockerVolume = 0x80310049,
    VolumeRemoved = 0x8031004A,
    Unknown = 0xFFFFFFFF,
}

impl FveError {
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

    pub fn code(&self) -> u32 {
        *self as u32
    }
}

impl std::fmt::Display for FveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            FveError::Success => "操作成功",
            FveError::InvalidParameter => "无效参数",
            FveError::AccessDenied => "访问被拒绝，请以管理员权限运行",
            FveError::VolumeLocked => "卷已锁定，需要密码解锁",
            FveError::NotSupported => "卷不支持 BitLocker",
            FveError::NotEncrypted => "卷未启用 BitLocker 加密",
            FveError::KeyRequired => "需要认证密钥",
            FveError::AuthenticationFailed => "认证失败",
            FveError::BadPassword => "密码错误",
            FveError::BadRecoveryPassword => "恢复密钥错误",
            FveError::VolumeUnlocked => "卷已解锁",
            FveError::NotBitLockerVolume => "不是 BitLocker 加密卷",
            FveError::VolumeRemoved => "卷已移除",
            FveError::Unknown => "未知错误",
        };
        write!(f, "{}", s)
    }
}

impl std::error::Error for FveError {}

/// 卷转换状态（来自 FveGetStatus 的 ConversionStatus）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FveVolumeStatus {
    FullyDecrypted = 0,
    FullyEncrypted = 1,
    EncryptionInProgress = 2,
    DecryptionInProgress = 3,
    EncryptionPaused = 4,
    DecryptionPaused = 5,
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
            other => {
                log::warn!(
                    "未知 FveVolumeStatus 值: {} (0x{:08X})，记为 Unknown",
                    other,
                    other
                );
                FveVolumeStatus::Unknown
            }
        }
    }
}

/// 保护状态（ProtectionStatus）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FveProtectionStatus {
    Off = 0,
    On = 1,
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

/// 锁定状态。参考实现把它由 ProtectionStatus 推导：`ProtectionStatus==1 → Locked`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FveLockStatus {
    Unlocked = 0,
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

/// 解析后的卷信息
#[derive(Debug, Clone)]
pub struct FveVolumeInfo {
    pub volume_status: FveVolumeStatus,
    pub protection_status: FveProtectionStatus,
    pub lock_status: FveLockStatus,
    /// 加密百分比（0/100/或转换中的近似值；精确进度由 manage-bde 复核）
    pub encryption_percentage: u8,
    /// 加密标志（保留，参考实现不提供，恒为 0）
    pub encryption_flags: u32,
}

/// 把 48 位恢复密钥格式化为 `XXXXXX-XXXXXX-...-XXXXXX`（8 组 6 位）。
///
/// 对齐参考实现 `FveLibFormatRecoveryPassword`：剔除非数字，必须恰为 48 位。
pub fn format_recovery_key(input: &str) -> Result<String, String> {
    let digits: String = input.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() != 48 {
        return Err(format!(
            "恢复密钥格式错误：应为 48 位数字，实际 {} 位",
            digits.len()
        ));
    }
    let parts: Vec<&str> = (0..8).map(|i| &digits[i * 6..i * 6 + 6]).collect();
    Ok(parts.join("-"))
}

// =====================================================================================
// Windows 实现
// =====================================================================================

#[cfg(windows)]
mod imp {
    use super::*;
    use std::ffi::c_void;
    use std::iter::once;
    use std::sync::OnceLock;

    use libloading::Library;

    // ---- 常量（对齐 fvelib.c）----
    const STATUS_OUTPUT_SIZE: u32 = 0x80;
    const STATUS_OUTPUT_VERSION: u32 = 2;
    const STATUS_OUTPUT_LEGACY_VERSION: u32 = 1;

    const AUTH_ELEMENT_SIZE: usize = 584;
    const AUTH_MAGIC_PASSPHRASE: i32 = 578;
    const AUTH_MAGIC_RECOVERY_PASSWORD: i32 = 32;
    const SECRET_TYPE_PASSPHRASE: u32 = 0x0080_0000;
    const SECRET_TYPE_RECOVERY_PASSWORD: u32 = 0x0008_0000;
    const UNLOCK_SETTINGS_VERSION: u32 = 1;

    #[cfg(target_pointer_width = "64")]
    const UNLOCK_SETTINGS_SIZE: usize = 0x38;
    #[cfg(target_pointer_width = "32")]
    const UNLOCK_SETTINGS_SIZE: usize = 0x30;

    // ---- HRESULT ----
    const HR_OK: u32 = 0;
    const E_INVALIDARG: u32 = 0x8007_0057;
    const HR_VOLUME_LOCKED: u32 = 0x8031_0000;
    const HR_VOLUME_UNLOCKED: u32 = 0x8031_0023;
    const HR_NOT_SUPPORTED: u32 = 0x8031_0001;
    const HR_NOT_ENCRYPTED: u32 = 0x8031_0008;
    const HR_NOT_BITLOCKER_VOLUME: u32 = 0x8031_0049;

    #[inline]
    fn hr_failed(hr: u32) -> bool {
        (hr & 0x8000_0000) != 0
    }
    /// FVE_LIB_FAILED：失败但排除“卷已锁定”（锁定卷开卷仍写出可用句柄）。
    #[inline]
    fn fve_failed(hr: u32) -> bool {
        hr_failed(hr) && hr != HR_VOLUME_LOCKED
    }
    #[inline]
    fn is_not_encrypted_hr(hr: u32) -> bool {
        matches!(
            hr,
            HR_NOT_SUPPORTED | HR_NOT_ENCRYPTED | HR_NOT_BITLOCKER_VOLUME
        )
    }

    // ---- FFI 结构（严格对照 fvelib.c）----

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct FveGetStatusOutput {
        size: u32,              // 0x00
        version: u32,           // 0x04
        reserved1: u32,         // 0x08
        conversion_status: u32, // 0x0C
        percent_complete: u64,  // 0x10
        reserved2: [u8; 0x20],  // 0x18
        protection_status: u32, // 0x38
        reserved3: [u8; 0x44],  // 0x3C
    }
    const _: () = assert!(std::mem::size_of::<FveGetStatusOutput>() == STATUS_OUTPUT_SIZE as usize);

    impl FveGetStatusOutput {
        fn new(version: u32) -> Self {
            // 全零，仅设置 size/version（缓冲区始终 0x80 字节）
            let mut o: FveGetStatusOutput = unsafe { std::mem::zeroed() };
            o.size = STATUS_OUTPUT_SIZE;
            o.version = version;
            o
        }
    }

    #[repr(C)]
    struct FveAuthElement {
        magic_value: i32,                  // 0x00
        must_be_one: i32,                  // 0x04
        data: [u8; AUTH_ELEMENT_SIZE - 8], // 0x08..
    }
    const _: () = assert!(std::mem::size_of::<FveAuthElement>() == AUTH_ELEMENT_SIZE);

    #[repr(C)]
    struct FveUnlockSettings {
        size: u32,                               // 0x00
        version: u32,                            // 0x04
        secret_type: u32,                        // 0x08
        auth_element_count: u32,                 // 0x0C
        auth_elements: *mut *mut FveAuthElement, // 0x10
        reserved: u64,                           // 0x18（x86 上由对齐自动补 0x14 的填充）
        reserved_tail: [u8; UNLOCK_SETTINGS_SIZE - 0x20],
    }
    const _: () = assert!(std::mem::size_of::<FveUnlockSettings>() == UNLOCK_SETTINGS_SIZE);

    // ---- FFI 函数类型 ----
    type FnOpenVolumeW = unsafe extern "system" fn(*const u16, u32, *mut *mut c_void) -> u32;
    type FnCloseVolume = unsafe extern "system" fn(*mut c_void) -> u32;
    type FnGetStatusW = unsafe extern "system" fn(*const u16, *mut FveGetStatusOutput) -> u32;
    type FnGetStatus = unsafe extern "system" fn(*mut c_void, *mut FveGetStatusOutput) -> u32;
    type FnUnlockVolume = unsafe extern "system" fn(*mut c_void, *mut c_void) -> u32;
    type FnUnlockVolumeWithAccessMode =
        unsafe extern "system" fn(*mut c_void, *mut FveUnlockSettings, u32) -> u32;
    type FnConversionDecrypt = unsafe extern "system" fn(*mut c_void) -> u32;
    type FnAuthFromPassphrase = unsafe extern "system" fn(*const u16, *mut FveAuthElement) -> u32;
    type FnAuthFromRecovery = unsafe extern "system" fn(*const u16, *mut FveAuthElement) -> u32;
    type FnIsVolumeEncrypted = unsafe extern "system" fn(*mut c_void) -> u32;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetVolumeNameForVolumeMountPointW(
            lpsz_volume_mount_point: *const u16,
            lpsz_volume_name: *mut u16,
            cch_buffer_length: u32,
        ) -> i32;
    }

    // ---- FveApi ----
    static FVE_API_INSTANCE: OnceLock<Result<FveApi, String>> = OnceLock::new();

    pub struct FveApi {
        _library: Library,
        open_volume: FnOpenVolumeW,
        close_volume: FnCloseVolume,
        get_status_w: Option<FnGetStatusW>,
        get_status: Option<FnGetStatus>,
        unlock_volume: FnUnlockVolume,
        unlock_volume_access: Option<FnUnlockVolumeWithAccessMode>,
        conversion_decrypt: Option<FnConversionDecrypt>,
        auth_from_passphrase: Option<FnAuthFromPassphrase>,
        auth_from_recovery: FnAuthFromRecovery,
        is_volume_encrypted: Option<FnIsVolumeEncrypted>,
    }

    unsafe impl Send for FveApi {}
    unsafe impl Sync for FveApi {}

    macro_rules! req {
        ($lib:expr, $name:literal, $ty:ty) => {{
            let sym: libloading::Symbol<$ty> = unsafe {
                $lib.get($name)
                    .map_err(|e| format!("找不到 {}: {}", String::from_utf8_lossy($name), e))?
            };
            *sym
        }};
    }
    macro_rules! opt {
        ($lib:expr, $name:literal, $ty:ty) => {{
            let s: Option<libloading::Symbol<$ty>> = unsafe { $lib.get($name).ok() };
            s.map(|f| *f)
        }};
    }

    impl FveApi {
        pub fn instance() -> Result<&'static FveApi, String> {
            FVE_API_INSTANCE
                .get_or_init(Self::load)
                .as_ref()
                .map_err(|e| e.clone())
        }

        fn load() -> Result<Self, String> {
            log::info!("正在加载 fveapi.dll...");
            let library = unsafe { Library::new("fveapi.dll") }
                .map_err(|e| format!("无法加载 fveapi.dll: {}", e))?;

            // 恢复密钥解锁真正必需的导出：开卷/关卷/解锁/恢复密钥认证元素。
            let open_volume = req!(library, b"FveOpenVolumeW", FnOpenVolumeW);
            let close_volume = req!(library, b"FveCloseVolume", FnCloseVolume);
            let unlock_volume = req!(library, b"FveUnlockVolume", FnUnlockVolume);
            let auth_from_recovery = req!(
                library,
                b"FveAuthElementFromRecoveryPasswordW",
                FnAuthFromRecovery
            );

            // 其余非解锁必需的导出设为可选：精简版 WinPE 的 fveapi.dll 可能并不导出
            // 这些函数，缺了也不应让整个 fveapi 不可用（否则连恢复密钥解锁都做不了）。
            // 调用处各自对 None 兜底（状态查询/解密/口令解锁缺失时返回错误，由上层回退）。
            let get_status_w = opt!(library, b"FveGetStatusW", FnGetStatusW);
            let get_status = opt!(library, b"FveGetStatus", FnGetStatus);
            let conversion_decrypt = opt!(library, b"FveConversionDecrypt", FnConversionDecrypt);
            let auth_from_passphrase = opt!(
                library,
                b"FveAuthElementFromPassPhraseW",
                FnAuthFromPassphrase
            );

            let unlock_volume_access = opt!(
                library,
                b"FveUnlockVolumeWithAccessMode",
                FnUnlockVolumeWithAccessMode
            );
            let is_volume_encrypted = opt!(
                library,
                b"InternalFveIsVolumeEncrypted",
                FnIsVolumeEncrypted
            );

            log::info!("fveapi.dll 加载成功");
            log::info!(
                "fveapi: 可选导出 GetStatusW={} GetStatus={} ConversionDecrypt={} AuthPassphrase={} UnlockWithAccessMode={} IsVolumeEncrypted={}",
                get_status_w.is_some(),
                get_status.is_some(),
                conversion_decrypt.is_some(),
                auth_from_passphrase.is_some(),
                unlock_volume_access.is_some(),
                is_volume_encrypted.is_some()
            );
            if unlock_volume_access.is_none() {
                log::warn!("fveapi: 缺 FveUnlockVolumeWithAccessMode 导出，将回退 FveUnlockVolume");
            }
            Ok(FveApi {
                _library: library,
                open_volume,
                close_volume,
                get_status_w,
                get_status,
                unlock_volume,
                unlock_volume_access,
                conversion_decrypt,
                auth_from_passphrase,
                auth_from_recovery,
                is_volume_encrypted,
            })
        }

        // ---- 状态查询（对齐 FveLibGetStatusByPath 的三级回退）----

        /// 通过路径获取卷状态。先按盘符查询，失败则按卷 GUID 路径查询，再失败则开卷查询。
        pub fn get_status_by_path(&self, volume_path: &str) -> Result<FveVolumeInfo, FveError> {
            let drive = normalize_volume_path(volume_path);

            let first = match self.query_status_by_path(&drive) {
                Ok(info) => return Ok(info),
                Err(e) => e,
            };

            if let Some(guid) = volume_guid_path_for_drive(&drive) {
                if let Ok(info) = self.query_status_by_path(&guid) {
                    return Ok(info);
                }
            }

            if let Ok(info) = self.query_status_by_open(&drive) {
                return Ok(info);
            }

            Err(first)
        }

        fn query_status_by_path(&self, path: &str) -> Result<FveVolumeInfo, FveError> {
            let get_status_w = self.get_status_w.ok_or(FveError::NotSupported)?;
            let wide = to_wide(path);
            let mut out = FveGetStatusOutput::new(STATUS_OUTPUT_VERSION);
            let mut hr = unsafe { get_status_w(wide.as_ptr(), &mut out) };
            if hr == E_INVALIDARG {
                out = FveGetStatusOutput::new(STATUS_OUTPUT_LEGACY_VERSION);
                hr = unsafe { get_status_w(wide.as_ptr(), &mut out) };
            }
            self.interpret_status(hr, &out)
        }

        fn query_status_by_handle(&self, handle: *mut c_void) -> Result<FveVolumeInfo, FveError> {
            if handle.is_null() {
                return Err(FveError::InvalidParameter);
            }
            let get_status = self.get_status.ok_or(FveError::NotSupported)?;
            let mut out = FveGetStatusOutput::new(STATUS_OUTPUT_VERSION);
            let mut hr = unsafe { get_status(handle, &mut out) };
            if hr == E_INVALIDARG {
                out = FveGetStatusOutput::new(STATUS_OUTPUT_LEGACY_VERSION);
                hr = unsafe { get_status(handle, &mut out) };
            }
            self.interpret_status(hr, &out)
        }

        fn interpret_status(
            &self,
            hr: u32,
            out: &FveGetStatusOutput,
        ) -> Result<FveVolumeInfo, FveError> {
            if hr == HR_VOLUME_LOCKED {
                return Ok(locked_volume_info());
            }
            if is_not_encrypted_hr(hr) {
                return Ok(not_encrypted_volume_info());
            }
            if hr_failed(hr) {
                return Err(FveError::from_hresult(hr));
            }
            Ok(volume_info_from_output(out))
        }

        /// 开卷（只读）查状态：路径式失败时的兜底（对齐 QueryStatusByOpenVolume）。
        fn query_status_by_open(&self, drive: &str) -> Result<FveVolumeInfo, FveError> {
            let (hr, handle) = self.raw_open(drive, FveAccessMode::ReadOnly);
            if is_not_encrypted_hr(hr) {
                return Ok(not_encrypted_volume_info());
            }
            if fve_failed(hr) {
                return Err(FveError::from_hresult(hr));
            }
            if handle.is_null() {
                return Err(FveError::Unknown);
            }

            let mut result = self.query_status_by_handle(handle);
            if result.is_err() {
                if let Some(is_enc) = self.is_volume_encrypted {
                    let enc_hr = unsafe { is_enc(handle) };
                    if enc_hr == 0 {
                        result = Ok(encrypted_unlocked_volume_info());
                    } else if enc_hr == 1 /* S_FALSE */ || is_not_encrypted_hr(enc_hr) {
                        result = Ok(not_encrypted_volume_info());
                    }
                }
            }
            unsafe { (self.close_volume)(handle) };
            result
        }

        // ---- 开卷 ----

        /// 打开卷（只读），用于解锁。锁定卷返回的句柄同样可用。
        pub fn open_volume(&self, volume_path: &str) -> Result<FveVolumeHandle<'_>, FveError> {
            self.open_volume_ex(volume_path, FveAccessMode::ReadOnly)
        }

        /// 打开卷（指定访问模式）。优先用 `\\?\Volume{GUID}\` 路径开卷。
        pub fn open_volume_ex(
            &self,
            volume_path: &str,
            mode: FveAccessMode,
        ) -> Result<FveVolumeHandle<'_>, FveError> {
            let drive = normalize_volume_path(volume_path);
            if mode == FveAccessMode::ReadWrite {
                super::enable_volume_privileges();
            }
            let (hr, handle) = self.raw_open(&drive, mode);
            // 仅 FVE_LIB_FAILED 才算失败：0x80310000(锁定)会带回可用句柄，用于解锁。
            if fve_failed(hr) {
                log::warn!("FveOpenVolumeW 失败: drive={} hr=0x{:08X}", drive, hr);
                return Err(FveError::from_hresult(hr));
            }
            if handle.is_null() {
                return Err(FveError::Unknown);
            }
            if hr == HR_VOLUME_LOCKED {
                log::info!("fveapi: 开卷 {} 卷已锁定(0x80310000)，已取得句柄用于解锁", drive);
            } else {
                log::info!("fveapi: 开卷 {} 成功 hr=0x{:08X}", drive, hr);
            }
            Ok(FveVolumeHandle { handle, api: self })
        }

        /// 调用 FveOpenVolumeW：优先卷 GUID 路径，回退盘符。返回 (hr, handle)。
        fn raw_open(&self, drive: &str, mode: FveAccessMode) -> (u32, *mut c_void) {
            let mut handle: *mut c_void = std::ptr::null_mut();
            let path = match volume_guid_path_for_drive(drive) {
                Some(g) => g,
                None => drive.to_string(),
            };
            let wide = to_wide(&path);
            let hr = unsafe { (self.open_volume)(wide.as_ptr(), mode as u32, &mut handle) };
            (hr, handle)
        }

        /// 关闭（解密）一个已解锁的 BitLocker 卷：读写开卷 + FveConversionDecrypt。
        ///
        /// 解密在驱动层后台进行，关句柄不影响。`_poll`/`_timeout` 仅为兼容旧签名保留。
        pub fn decrypt_unlocked_volume_blocking(
            &self,
            volume_path: &str,
            _poll_interval_ms: u64,
            _timeout_secs: u64,
        ) -> Result<FveVolumeInfo, FveError> {
            let handle = self.open_volume_ex(volume_path, FveAccessMode::ReadWrite)?;
            handle.start_decryption()?;
            log::info!("卷 {} 的 FveConversionDecrypt 已发起", volume_path);
            Ok(FveVolumeInfo {
                volume_status: FveVolumeStatus::DecryptionInProgress,
                protection_status: FveProtectionStatus::Unknown,
                lock_status: FveLockStatus::Unlocked,
                encryption_percentage: 0,
                encryption_flags: 0,
            })
        }
    }

    // ---- FveVolumeHandle ----

    pub struct FveVolumeHandle<'a> {
        handle: *mut c_void,
        api: &'a FveApi,
    }

    impl<'a> FveVolumeHandle<'a> {
        pub fn unlock_with_password(&self, password: &str) -> Result<(), FveError> {
            self.unlock_with_secret(password, SECRET_TYPE_PASSPHRASE)
        }

        pub fn unlock_with_recovery_key(&self, recovery_key: &str) -> Result<(), FveError> {
            self.unlock_with_secret(recovery_key, SECRET_TYPE_RECOVERY_PASSWORD)
        }

        /// 核心解锁流程，对齐 fvelib.c 的 UnlockWithSecret。
        fn unlock_with_secret(&self, secret: &str, secret_type: u32) -> Result<(), FveError> {
            if secret.is_empty() {
                return Err(FveError::InvalidParameter);
            }
            let is_recovery = secret_type == SECRET_TYPE_RECOVERY_PASSWORD;

            // 1) 构造 584 字节认证元素（先填 Magic/MustBeOne，再由 fveapi 填密钥数据）
            let mut auth: Box<FveAuthElement> = Box::new(unsafe { std::mem::zeroed() });
            auth.magic_value = if is_recovery {
                AUTH_MAGIC_RECOVERY_PASSWORD
            } else {
                AUTH_MAGIC_PASSPHRASE
            };
            auth.must_be_one = 1;

            // 恢复密钥先规范化为带连字符的标准格式
            let secret_w = if is_recovery {
                match format_recovery_key(secret) {
                    Ok(f) => to_wide(&f),
                    Err(_) => to_wide(secret),
                }
            } else {
                to_wide(secret)
            };

            let hr = unsafe {
                if is_recovery {
                    (self.api.auth_from_recovery)(secret_w.as_ptr(), &mut *auth)
                } else {
                    match self.api.auth_from_passphrase {
                        Some(f) => f(secret_w.as_ptr(), &mut *auth),
                        None => {
                            log::warn!("fveapi: 缺 FveAuthElementFromPassPhraseW 导出，无法用口令解锁");
                            return Err(FveError::NotSupported);
                        }
                    }
                }
            };
            if hr_failed(hr) {
                log::warn!(
                    "fveapi: 生成认证元素失败(is_recovery={}) hr=0x{:08X} ({})",
                    is_recovery,
                    hr,
                    FveError::from_hresult(hr)
                );
                return Err(FveError::from_hresult(hr));
            }
            log::info!(
                "fveapi: 认证元素已生成(is_recovery={})，解锁入口={}",
                is_recovery,
                if self.api.unlock_volume_access.is_some() {
                    "FveUnlockVolumeWithAccessMode"
                } else {
                    "FveUnlockVolume"
                }
            );

            // 2) 优先用带 AccessMode 的解锁入口（需要 0x38 字节 settings + SecretType）
            let hr = if let Some(unlock_am) = self.api.unlock_volume_access {
                let mut auth_ptr: *mut FveAuthElement = &mut *auth;
                let mut settings: FveUnlockSettings = unsafe { std::mem::zeroed() };
                settings.size = UNLOCK_SETTINGS_SIZE as u32;
                settings.version = UNLOCK_SETTINGS_VERSION;
                settings.secret_type = secret_type;
                settings.auth_element_count = 1;
                settings.auth_elements = &mut auth_ptr;
                settings.reserved = 0;
                unsafe { unlock_am(self.handle, &mut settings, 0) }
            } else {
                // 旧系统无该导出：直接传 584 字节认证元素
                unsafe {
                    (self.api.unlock_volume)(
                        self.handle,
                        (&mut *auth) as *mut FveAuthElement as *mut c_void,
                    )
                }
            };

            if hr == HR_OK || hr == HR_VOLUME_UNLOCKED {
                log::info!("fveapi: 解锁成功 hr=0x{:08X}", hr);
                Ok(())
            } else {
                log::warn!(
                    "fveapi: 解锁失败 hr=0x{:08X} ({})",
                    hr,
                    FveError::from_hresult(hr)
                );
                Err(FveError::from_hresult(hr))
            }
        }

        /// 开始解密（关闭 BitLocker）。立即返回，解密在后台进行。
        pub fn start_decryption(&self) -> Result<(), FveError> {
            let conversion_decrypt = self.api.conversion_decrypt.ok_or(FveError::NotSupported)?;
            let hr = unsafe { conversion_decrypt(self.handle) };
            if hr == HR_OK {
                Ok(())
            } else {
                Err(FveError::from_hresult(hr))
            }
        }
    }

    impl<'a> Drop for FveVolumeHandle<'a> {
        fn drop(&mut self) {
            if !self.handle.is_null() {
                let hr = unsafe { (self.api.close_volume)(self.handle) };
                if hr != 0 {
                    log::debug!("FveCloseVolume hr=0x{:08X}", hr);
                }
            }
        }
    }

    // ---- 状态信息构造（对齐 fvelib.c 的 Set*VolumeInfo / VolumeInfoFromOutput）----

    fn percent_from_status(status: FveVolumeStatus) -> u8 {
        match status {
            FveVolumeStatus::FullyEncrypted => 100,
            FveVolumeStatus::FullyDecrypted => 0,
            _ => 50,
        }
    }

    fn volume_info_from_output(out: &FveGetStatusOutput) -> FveVolumeInfo {
        let vs = FveVolumeStatus::from(out.conversion_status);
        FveVolumeInfo {
            volume_status: vs,
            protection_status: FveProtectionStatus::from(out.protection_status),
            lock_status: FveLockStatus::from(out.protection_status),
            encryption_percentage: percent_from_status(vs),
            encryption_flags: 0,
        }
    }

    fn locked_volume_info() -> FveVolumeInfo {
        FveVolumeInfo {
            volume_status: FveVolumeStatus::FullyEncrypted,
            protection_status: FveProtectionStatus::On,
            lock_status: FveLockStatus::Locked,
            encryption_percentage: 100,
            encryption_flags: 0,
        }
    }

    fn not_encrypted_volume_info() -> FveVolumeInfo {
        FveVolumeInfo {
            volume_status: FveVolumeStatus::FullyDecrypted,
            protection_status: FveProtectionStatus::Off,
            lock_status: FveLockStatus::Unlocked,
            encryption_percentage: 0,
            encryption_flags: 0,
        }
    }

    fn encrypted_unlocked_volume_info() -> FveVolumeInfo {
        FveVolumeInfo {
            volume_status: FveVolumeStatus::FullyEncrypted,
            protection_status: FveProtectionStatus::On,
            lock_status: FveLockStatus::Unlocked,
            encryption_percentage: 100,
            encryption_flags: 0,
        }
    }

    // ---- 路径辅助 ----

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(once(0)).collect()
    }

    /// 把各种卷路径规范化为 `X:` 形式（保留 `Volume{GUID}` 串不变）。
    pub(super) fn normalize_volume_path(path: &str) -> String {
        let trimmed = path.trim();
        if trimmed.contains("Volume{") {
            return trimmed.to_string();
        }
        let bytes = trimmed.as_bytes();
        // 设备路径 \\.\X: 或 \\?\X:
        if trimmed.len() >= 6 && (trimmed.starts_with("\\\\.\\") || trimmed.starts_with("\\\\?\\"))
        {
            let rest = &trimmed[4..];
            let rb = rest.as_bytes();
            if rb.len() >= 2 && rb[1] == b':' && (rb[0] as char).is_ascii_alphabetic() {
                return format!("{}:", (rb[0] as char).to_ascii_uppercase());
            }
        }
        // 简单盘符 X: 或 X:\
        if bytes.len() >= 2 && (bytes[0] as char).is_ascii_alphabetic() && bytes[1] == b':' {
            return format!("{}:", (bytes[0] as char).to_ascii_uppercase());
        }
        trimmed.to_string()
    }

    /// 盘符 → `\\?\Volume{GUID}\`（保留尾反斜杠，与 fvelib.c 一致）。
    fn volume_guid_path_for_drive(drive: &str) -> Option<String> {
        let letter = drive.chars().next()?;
        if !letter.is_ascii_alphabetic() {
            return None;
        }
        let mount: Vec<u16> = format!("{}:\\", letter.to_ascii_uppercase())
            .encode_utf16()
            .chain(once(0))
            .collect();
        let mut buf = [0u16; 64];
        let ok = unsafe {
            GetVolumeNameForVolumeMountPointW(mount.as_ptr(), buf.as_mut_ptr(), buf.len() as u32)
        };
        if ok == 0 {
            return None;
        }
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        if n == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..n]))
    }
}

// FveVolumeHandle 是 open_volume 等公开方法的返回类型，必须随 FveApi 一起再导出
// （否则触发 private_interfaces 告警）；二进制 crate 内部不会按名引用它，故单独放行。
#[cfg(windows)]
#[allow(unused_imports)]
pub use imp::{FveApi, FveVolumeHandle};

// =====================================================================================
// 令牌权限：启用 SeManageVolumePrivilege（读写操作前调用；失败不致命）
// =====================================================================================

#[cfg(windows)]
pub fn enable_volume_privileges() {
    use std::ffi::c_void;

    const TOKEN_ADJUST_PRIVILEGES: u32 = 0x0020;
    const TOKEN_QUERY: u32 = 0x0008;
    const SE_PRIVILEGE_ENABLED: u32 = 0x0002;
    const ERROR_NOT_ALL_ASSIGNED: u32 = 1300;

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
        type FnLookupPriv = unsafe extern "system" fn(*const u16, *const u16, *mut Luid) -> i32;
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
        type FnGetCurrentProcess = unsafe extern "system" fn() -> *mut c_void;

        let open_token: libloading::Symbol<FnOpenProcessToken> =
            match advapi.get(b"OpenProcessToken") {
                Ok(f) => f,
                Err(_) => return,
            };
        let lookup: libloading::Symbol<FnLookupPriv> = match advapi.get(b"LookupPrivilegeValueW") {
            Ok(f) => f,
            Err(_) => return,
        };
        let adjust: libloading::Symbol<FnAdjust> = match advapi.get(b"AdjustTokenPrivileges") {
            Ok(f) => f,
            Err(_) => return,
        };
        let close: libloading::Symbol<FnCloseHandle> = match kernel.get(b"CloseHandle") {
            Ok(f) => f,
            Err(_) => return,
        };
        let get_cur_proc: libloading::Symbol<FnGetCurrentProcess> =
            match kernel.get(b"GetCurrentProcess") {
                Ok(f) => f,
                Err(_) => return,
            };
        let get_last_error: Option<libloading::Symbol<FnGetLastError>> =
            kernel.get(b"GetLastError").ok();

        let cur_proc = get_cur_proc();
        let mut token: *mut c_void = std::ptr::null_mut();
        if open_token(cur_proc, TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token) == 0
            || token.is_null()
        {
            log::warn!("OpenProcessToken 失败");
            return;
        }

        let wname: Vec<u16> = "SeManageVolumePrivilege"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut luid = Luid { low: 0, high: 0 };
        if lookup(std::ptr::null(), wname.as_ptr(), &mut luid) != 0 {
            let tp = TokenPrivileges {
                count: 1,
                privilege: LuidAndAttributes {
                    luid,
                    attributes: SE_PRIVILEGE_ENABLED,
                },
            };
            let ok = adjust(token, 0, &tp, 0, std::ptr::null_mut(), std::ptr::null_mut());
            let last = get_last_error.as_ref().map(|f| f()).unwrap_or(0);
            if ok == 0 {
                log::warn!("AdjustTokenPrivileges 调用失败");
            } else if last == ERROR_NOT_ALL_ASSIGNED {
                log::warn!("SeManageVolumePrivilege 未实际授予(NOT_ALL_ASSIGNED)");
            } else {
                log::debug!("SeManageVolumePrivilege 已启用");
            }
        } else {
            log::debug!("LookupPrivilegeValueW(SeManageVolumePrivilege) 失败");
        }
        close(token);
    }
}

#[cfg(not(windows))]
pub fn enable_volume_privileges() {}

// =====================================================================================
// 非 Windows 平台的空实现（保持 API 一致，便于跨平台 cargo check）
// =====================================================================================

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
    pub fn open_volume(&self, _volume_path: &str) -> Result<FveVolumeHandle<'_>, FveError> {
        Err(FveError::NotSupported)
    }
    pub fn open_volume_ex(
        &self,
        _volume_path: &str,
        _mode: FveAccessMode,
    ) -> Result<FveVolumeHandle<'_>, FveError> {
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
}

#[cfg(not(windows))]
pub struct FveVolumeHandle<'a> {
    _phantom: std::marker::PhantomData<&'a ()>,
}

#[cfg(not(windows))]
impl<'a> FveVolumeHandle<'a> {
    pub fn unlock_with_password(&self, _password: &str) -> Result<(), FveError> {
        Err(FveError::NotSupported)
    }
    pub fn unlock_with_recovery_key(&self, _recovery_key: &str) -> Result<(), FveError> {
        Err(FveError::NotSupported)
    }
    pub fn start_decryption(&self) -> Result<(), FveError> {
        Err(FveError::NotSupported)
    }
}

// =====================================================================================
// 测试
// =====================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_recovery_key() {
        let r = format_recovery_key("123456789012345678901234567890123456789012345678");
        assert_eq!(
            r.unwrap(),
            "123456-789012-345678-901234-567890-123456-789012-345678"
        );

        let r = format_recovery_key("123456-789012-345678-901234-567890-123456-789012-345678");
        assert!(r.is_ok());

        assert!(format_recovery_key("12345").is_err());
    }

    #[test]
    fn test_error_mapping() {
        assert_eq!(FveError::from_hresult(0), FveError::Success);
        assert_eq!(FveError::from_hresult(0x80310000), FveError::VolumeLocked);
        assert_eq!(
            FveError::from_hresult(0x80310028),
            FveError::BadRecoveryPassword
        );
        assert_eq!(FveError::BadPassword.code(), 0x80310027);
    }

    #[test]
    fn test_enum_from() {
        assert_eq!(FveVolumeStatus::from(1), FveVolumeStatus::FullyEncrypted);
        assert_eq!(FveVolumeStatus::from(99), FveVolumeStatus::Unknown);
        assert_eq!(FveProtectionStatus::from(1), FveProtectionStatus::On);
        assert_eq!(FveLockStatus::from(0), FveLockStatus::Unlocked);
        assert_eq!(FveLockStatus::from(1), FveLockStatus::Locked);
    }

    #[cfg(windows)]
    #[test]
    fn test_normalize_volume_path() {
        use super::imp::normalize_volume_path;
        assert_eq!(normalize_volume_path("C:"), "C:");
        assert_eq!(normalize_volume_path("c:\\"), "C:");
        assert_eq!(normalize_volume_path("D:\\Windows"), "D:");
        assert_eq!(normalize_volume_path("\\\\.\\C:"), "C:");
        assert_eq!(normalize_volume_path("\\\\?\\E:"), "E:");
        assert_eq!(normalize_volume_path("  F:  "), "F:");
    }
}
