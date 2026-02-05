//! BitLocker核心功能模块
//!
//! 提供BitLocker加密卷的检测、状态查询、解锁和解密功能。
//!
//! # 实现策略
//! 优先使用 fveapi.dll 原生API实现高性能操作：
//! - 无需为每个驱动器启动外部进程
//! - 状态检测速度提升10倍以上
//! - 更准确的错误信息
//!
//! # 安全说明
//! - 所有密码和恢复密钥仅在内存中短暂存在
//! - 使用RAII模式确保句柄正确释放

use std::os::windows::process::CommandExt;
#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetVolumeInformationW,
};

#[cfg(windows)]
use super::fveapi::{
    format_recovery_key, FveAccessMode, FveApi, FveError, FveLockStatus, FveProtectionStatus,
    FveVolumeStatus,
};

/// 驱动器类型常量
const DRIVE_FIXED: u32 = 3;

/// BitLocker卷状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeStatus {
    /// 未加密
    NotEncrypted,
    /// 已加密已解锁
    EncryptedUnlocked,
    /// 已加密已锁定（需要解锁）
    EncryptedLocked,
    /// 正在加密
    Encrypting,
    /// 正在解密
    Decrypting,
    /// 状态未知
    Unknown,
}

impl VolumeStatus {
    /// 获取状态的中文描述
    pub fn as_str(&self) -> &'static str {
        match self {
            VolumeStatus::NotEncrypted => "未加密",
            VolumeStatus::EncryptedUnlocked => "已解锁",
            VolumeStatus::EncryptedLocked => "已锁定",
            VolumeStatus::Encrypting => "正在加密",
            VolumeStatus::Decrypting => "正在解密",
            VolumeStatus::Unknown => "未知",
        }
    }

    /// 是否需要解锁
    pub fn needs_unlock(&self) -> bool {
        matches!(self, VolumeStatus::EncryptedLocked)
    }

    /// 是否已加密（无论是否锁定）
    pub fn is_encrypted(&self) -> bool {
        matches!(
            self,
            VolumeStatus::EncryptedLocked
                | VolumeStatus::EncryptedUnlocked
                | VolumeStatus::Encrypting
                | VolumeStatus::Decrypting
        )
    }
}

#[cfg(windows)]
impl From<&super::fveapi::FveVolumeInfo> for VolumeStatus {
    fn from(info: &super::fveapi::FveVolumeInfo) -> Self {
        // 关键：检查解密百分比！
        // 当 manage-bde -off 开始解密后，状态可能显示为 FullyDecrypted，
        // 但 encryption_percentage 仍然 > 0，表示还在解密过程中
        // 只有当 encryption_percentage == 0 时，才是真正完全解密

        match info.volume_status {
            FveVolumeStatus::FullyDecrypted => {
                // 检查是否真的完全解密（百分比为0）
                if info.encryption_percentage > 0 {
                    // 虽然状态显示已解密，但百分比>0，说明还在解密中
                    VolumeStatus::Decrypting
                } else {
                    VolumeStatus::NotEncrypted
                }
            }
            FveVolumeStatus::FullyEncrypted => {
                if info.lock_status == FveLockStatus::Locked {
                    VolumeStatus::EncryptedLocked
                } else {
                    VolumeStatus::EncryptedUnlocked
                }
            }
            FveVolumeStatus::EncryptionInProgress | FveVolumeStatus::EncryptionPaused => {
                VolumeStatus::Encrypting
            }
            FveVolumeStatus::DecryptionInProgress | FveVolumeStatus::DecryptionPaused => {
                VolumeStatus::Decrypting
            }
        }
    }
}

/// BitLocker卷信息
#[derive(Debug, Clone)]
pub struct VolumeInfo {
    /// 盘符（如 "D:"）
    pub letter: String,
    /// 卷标
    pub label: String,
    /// 总大小（MB）
    pub total_size_mb: u64,
    /// BitLocker状态
    pub status: VolumeStatus,
    /// 保护方法描述
    pub protection_method: String,
    /// 加密进度百分比（0-100），正在加密/解密时有效
    pub encryption_percentage: Option<u8>,
}

impl VolumeInfo {
    /// 是否需要解锁
    pub fn needs_unlock(&self) -> bool {
        self.status.needs_unlock()
    }
}

/// 解锁操作结果
#[derive(Debug, Clone)]
pub struct UnlockResult {
    /// 盘符
    pub letter: String,
    /// 是否成功
    pub success: bool,
    /// 消息
    pub message: String,
    /// 错误代码（如果有）
    pub error_code: Option<u32>,
}

impl UnlockResult {
    /// 创建成功结果
    pub fn success(letter: &str, message: &str) -> Self {
        Self {
            letter: letter.to_string(),
            success: true,
            message: message.to_string(),
            error_code: None,
        }
    }

    /// 创建失败结果
    pub fn failure(letter: &str, message: &str, error_code: Option<u32>) -> Self {
        Self {
            letter: letter.to_string(),
            success: false,
            message: message.to_string(),
            error_code,
        }
    }
}

/// 解密操作结果（彻底解锁/关闭BitLocker）
#[derive(Debug, Clone)]
pub struct DecryptResult {
    /// 盘符
    pub letter: String,
    /// 是否成功启动解密
    pub success: bool,
    /// 消息
    pub message: String,
    /// 错误代码（如果有）
    pub error_code: Option<u32>,
}

impl DecryptResult {
    /// 创建成功结果
    pub fn success(letter: &str, message: &str) -> Self {
        Self {
            letter: letter.to_string(),
            success: true,
            message: message.to_string(),
            error_code: None,
        }
    }

    /// 创建失败结果
    pub fn failure(letter: &str, message: &str, error_code: Option<u32>) -> Self {
        Self {
            letter: letter.to_string(),
            success: false,
            message: message.to_string(),
            error_code,
        }
    }
}

/// BitLocker管理器
///
/// 提供BitLocker卷的检测、查询和操作功能。
/// 优先使用fveapi.dll实现，失败时回退到manage-bde命令行。
pub struct BitLockerManager {
    /// 是否使用fveapi（内部状态）
    use_fveapi: bool,
}

impl BitLockerManager {
    /// 创建新的BitLocker管理器
    pub fn new() -> Self {
        let use_fveapi = cfg!(windows) && Self::init_fveapi();
        Self { use_fveapi }
    }

    /// 初始化fveapi
    #[cfg(windows)]
    fn init_fveapi() -> bool {
        match FveApi::instance() {
            Ok(_) => {
                log::info!("BitLocker管理器: 使用 fveapi.dll 原生API");
                true
            }
            Err(e) => {
                log::warn!(
                    "BitLocker管理器: fveapi.dll 不可用 ({}), 将回退到 manage-bde",
                    e
                );
                false
            }
        }
    }

    #[cfg(not(windows))]
    fn init_fveapi() -> bool {
        false
    }

    /// 检查管理器是否可用
    pub fn is_available(&self) -> bool {
        #[cfg(windows)]
        {
            self.use_fveapi || Self::is_manage_bde_available()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    /// 检查manage-bde是否可用
    #[cfg(windows)]
    fn is_manage_bde_available() -> bool {
        std::process::Command::new("manage-bde")
            .arg("-?")
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .output()
            .is_ok()
    }

    /// 获取指定驱动器的BitLocker状态
    #[cfg(windows)]
    pub fn get_status(&self, drive_letter: char) -> VolumeStatus {
        if self.use_fveapi {
            self.get_status_fveapi(drive_letter)
        } else {
            self.get_status_manage_bde(drive_letter)
        }
    }

    #[cfg(not(windows))]
    pub fn get_status(&self, _drive_letter: char) -> VolumeStatus {
        VolumeStatus::Unknown
    }

    /// 获取指定驱动器的BitLocker状态和加密百分比
    #[cfg(windows)]
    pub fn get_status_with_percentage(&self, drive_letter: char) -> (VolumeStatus, f32) {
        if self.use_fveapi {
            self.get_status_with_percentage_fveapi(drive_letter)
        } else {
            self.get_status_with_percentage_manage_bde(drive_letter)
        }
    }

    #[cfg(not(windows))]
    pub fn get_status_with_percentage(&self, _drive_letter: char) -> (VolumeStatus, f32) {
        (VolumeStatus::Unknown, 0.0)
    }

    /// 使用fveapi获取状态
    ///
    /// 优先使用 FveGetStatusW 直接通过路径获取状态，这是最可靠的方法。
    /// 不需要打开句柄，避免了权限问题和句柄占用问题。
    ///
    /// 重要：当FveGetStatusW返回错误码0x80310000时，表示卷已锁定，
    /// 这意味着卷是BitLocker加密的但处于锁定状态。
    #[cfg(windows)]
    fn get_status_fveapi(&self, drive_letter: char) -> VolumeStatus {
        let api = match FveApi::instance() {
            Ok(api) => api,
            Err(e) => {
                log::warn!(
                    "驱动器 {}: FveApi 实例获取失败: {}, 回退到 manage-bde",
                    drive_letter,
                    e
                );
                return self.get_status_manage_bde(drive_letter);
            }
        };

        // 使用简单的盘符格式，FveGetStatusW会自动处理
        let volume_path = format!("{}:", drive_letter);

        // 使用 FveGetStatusW 直接获取状态（推荐方法，无需打开句柄）
        match api.get_status_by_path(&volume_path) {
            Ok(info) => {
                let status = VolumeStatus::from(&info);
                log::info!(
                    "驱动器 {}: BitLocker状态={:?}, 转换={:?}, 锁定={:?}, 百分比={}%, flags=0x{:04X} (fveapi)",
                    drive_letter,
                    status,
                    info.volume_status,
                    info.lock_status,
                    info.encryption_percentage,
                    info.encryption_flags
                );
                status
            }
            // 卷已锁定 - 这是BitLocker加密的已锁定卷
            Err(FveError::VolumeLocked) | Err(FveError::KeyRequired) => {
                log::info!(
                    "驱动器 {}: BitLocker卷已锁定，需要密码解锁 (fveapi)",
                    drive_letter
                );
                VolumeStatus::EncryptedLocked
            }
            // 卷未加密
            Err(FveError::NotEncrypted)
            | Err(FveError::NotBitLockerVolume)
            | Err(FveError::NotSupported) => {
                log::debug!("驱动器 {}: 未加密或不支持BitLocker (fveapi)", drive_letter);
                VolumeStatus::NotEncrypted
            }
            Err(FveError::AccessDenied) => {
                log::warn!(
                    "驱动器 {}: 访问被拒绝，可能需要管理员权限 (fveapi), 回退到 manage-bde",
                    drive_letter
                );
                self.get_status_manage_bde(drive_letter)
            }
            Err(e) => {
                log::warn!(
                    "驱动器 {} 获取状态失败: {} (错误码: 0x{:08X}), 回退到 manage-bde",
                    drive_letter,
                    e,
                    e.code()
                );
                // 回退到 manage-bde
                self.get_status_manage_bde(drive_letter)
            }
        }
    }

    /// 使用manage-bde获取状态（回退方案）
    #[cfg(windows)]
    fn get_status_manage_bde(&self, drive_letter: char) -> VolumeStatus {
        use std::process::Command;

        let drive = format!("{}:", drive_letter);
        let output = match Command::new("manage-bde")
            .args(["-status", &drive])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .output()
        {
            Ok(o) => o,
            Err(_) => return VolumeStatus::Unknown,
        };

        let stdout = decode_windows_output(&output.stdout);
        determine_volume_status(&stdout)
    }

    /// 使用fveapi获取状态和百分比
    #[cfg(windows)]
    fn get_status_with_percentage_fveapi(&self, drive_letter: char) -> (VolumeStatus, f32) {
        let api = match FveApi::instance() {
            Ok(api) => api,
            Err(_) => {
                return self.get_status_with_percentage_manage_bde(drive_letter);
            }
        };

        let volume_path = format!("{}:", drive_letter);

        match api.get_status_by_path(&volume_path) {
            Ok(info) => {
                let status = VolumeStatus::from(&info);
                let percentage = info.encryption_percentage as f32;
                (status, percentage)
            }
            Err(FveError::VolumeLocked) | Err(FveError::KeyRequired) => {
                (VolumeStatus::EncryptedLocked, 100.0)
            }
            Err(FveError::NotEncrypted)
            | Err(FveError::NotBitLockerVolume)
            | Err(FveError::NotSupported) => (VolumeStatus::NotEncrypted, 0.0),
            Err(_) => self.get_status_with_percentage_manage_bde(drive_letter),
        }
    }

    /// 使用manage-bde获取状态和百分比
    #[cfg(windows)]
    fn get_status_with_percentage_manage_bde(&self, drive_letter: char) -> (VolumeStatus, f32) {
        use std::process::Command;

        let drive = format!("{}:", drive_letter);
        let output = match Command::new("manage-bde")
            .args(["-status", &drive])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .output()
        {
            Ok(o) => o,
            Err(_) => return (VolumeStatus::Unknown, 0.0),
        };

        let stdout = decode_windows_output(&output.stdout);
        let status = determine_volume_status(&stdout);
        let percentage = extract_encryption_percentage(&stdout).unwrap_or(0.0);
        (status, percentage)
    }

    /// 获取指定驱动器的恢复密钥（数字密码）
    #[cfg(windows)]
    pub fn get_recovery_key(&self, drive: &str) -> Result<String, String> {
        let drive_letter = drive.chars().next().unwrap_or('C');
        let drive = format!("{}:", drive_letter);

        // 无论是否使用 fveapi，获取恢复密钥目前主要依赖 manage-bde
        // 因为 fveapi 获取密钥需要复杂的结构体解析，且未公开文档
        self.get_recovery_key_manage_bde(&drive)
    }

    #[cfg(not(windows))]
    pub fn get_recovery_key(&self, _drive: &str) -> Result<String, String> {
        Err("仅支持Windows系统".to_string())
    }

    /// 使用 manage-bde 获取恢复密钥
    #[cfg(windows)]
    fn get_recovery_key_manage_bde(&self, drive: &str) -> Result<String, String> {
        use std::process::Command;

        // manage-bde -protectors -get C: -Type RecoveryPassword
        let output = match Command::new("manage-bde")
            .args(["-protectors", "-get", drive, "-Type", "RecoveryPassword"])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .output()
        {
            Ok(o) => o,
            Err(e) => return Err(format!("执行命令失败: {}", e)),
        };

        let stdout = decode_windows_output(&output.stdout);

        // 解析输出寻找 48 位数字密码
        // 格式通常为：111111-222222-333333-444444-555555-666666-777777-888888
        extract_recovery_key(&stdout).ok_or_else(|| "未找到恢复密钥".to_string())
    }

    /// 检查指定驱动器是否需要解锁
    pub fn needs_unlock(&self, drive_letter: char) -> bool {
        self.get_status(drive_letter).needs_unlock()
    }

    /// 使用密码解锁
    #[cfg(windows)]
    pub fn unlock_with_password(&self, drive: &str, password: &str) -> UnlockResult {
        let drive_letter = drive.chars().next().unwrap_or('C');
        let letter = format!("{}:", drive_letter);

        if password.is_empty() {
            return UnlockResult::failure(&letter, "密码不能为空", None);
        }

        let status = self.get_status(drive_letter);
        if status == VolumeStatus::NotEncrypted {
            return UnlockResult::failure(&letter, "该驱动器未启用 BitLocker 加密", None);
        }
        if status == VolumeStatus::EncryptedUnlocked {
            return UnlockResult::success(&letter, "驱动器已经是解锁状态");
        }

        let result = if self.use_fveapi {
            self.unlock_with_password_fveapi(drive_letter, password)
        } else {
            self.unlock_with_password_manage_bde(drive_letter, password)
        };

        // 如果解锁成功，等待分区完全可访问
        if result.success {
            self.wait_for_unlock_complete(drive_letter, &letter)
        } else {
            result
        }
    }

    #[cfg(not(windows))]
    pub fn unlock_with_password(&self, drive: &str, _password: &str) -> UnlockResult {
        UnlockResult::failure(drive, "仅支持Windows系统", None)
    }

    /// 使用fveapi密码解锁
    #[cfg(windows)]
    fn unlock_with_password_fveapi(&self, drive_letter: char, password: &str) -> UnlockResult {
        let letter = format!("{}:", drive_letter);
        let volume_path = format!("{}:", drive_letter);

        let api = match FveApi::instance() {
            Ok(api) => api,
            Err(e) => return UnlockResult::failure(&letter, &e, None),
        };

        match api.open_volume(&volume_path) {
            Ok(handle) => match handle.unlock_with_password(password) {
                Ok(()) => {
                    log::info!("BitLocker 分区 {} 使用密码解锁成功 (fveapi)", letter);
                    UnlockResult::success(&letter, "解锁成功")
                }
                Err(FveError::BadPassword) => {
                    log::warn!("BitLocker 分区 {} 密码错误 (fveapi)", letter);
                    UnlockResult::failure(&letter, "密码错误", Some(0x80310027))
                }
                Err(FveError::VolumeUnlocked) => {
                    UnlockResult::success(&letter, "驱动器已经是解锁状态")
                }
                Err(e) => {
                    log::error!("BitLocker 分区 {} 解锁失败: {} (fveapi)", letter, e);
                    UnlockResult::failure(&letter, &e.to_string(), None)
                }
            },
            Err(e) => {
                log::error!("BitLocker 分区 {} 打开失败: {} (fveapi)", letter, e);
                UnlockResult::failure(&letter, &e.to_string(), None)
            }
        }
    }

    /// 使用manage-bde密码解锁
    #[cfg(windows)]
    fn unlock_with_password_manage_bde(&self, drive_letter: char, password: &str) -> UnlockResult {
        use std::process::Command;

        let letter = format!("{}:", drive_letter);
        let drive = format!("{}:", drive_letter);

        let output = match Command::new("manage-bde")
            .args(["-unlock", &drive, "-password", password])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .output()
        {
            Ok(o) => o,
            Err(e) => return UnlockResult::failure(&letter, &format!("执行命令失败: {}", e), None),
        };

        let stdout = decode_windows_output(&output.stdout);
        let stdout_lower = stdout.to_lowercase();

        if stdout_lower.contains("successfully unlocked")
            || stdout_lower.contains("已成功解锁")
            || stdout_lower.contains("unlock was successful")
            || stdout_lower.contains("解锁成功")
        {
            log::info!("BitLocker 分区 {} 使用密码解锁成功 (manage-bde)", letter);
            UnlockResult::success(&letter, "解锁成功")
        } else if stdout_lower.contains("password failed")
            || stdout_lower.contains("密码失败")
            || stdout_lower.contains("incorrect password")
            || stdout_lower.contains("密码不正确")
            || stdout_lower.contains("the password is incorrect")
        {
            log::warn!("BitLocker 分区 {} 密码错误 (manage-bde)", letter);
            UnlockResult::failure(&letter, "密码错误", Some(0x80310027))
        } else if stdout_lower.contains("already unlocked") || stdout_lower.contains("已解锁") {
            UnlockResult::success(&letter, "驱动器已经是解锁状态")
        } else {
            log::error!(
                "BitLocker 分区 {} 解锁失败: {} (manage-bde)",
                letter,
                stdout
            );
            let error_msg = extract_error_message(&stdout)
                .unwrap_or_else(|| "解锁失败，请检查密码是否正确".to_string());
            UnlockResult::failure(&letter, &error_msg, None)
        }
    }

    /// 使用恢复密钥解锁
    #[cfg(windows)]
    pub fn unlock_with_recovery_key(&self, drive: &str, recovery_key: &str) -> UnlockResult {
        let drive_letter = drive.chars().next().unwrap_or('C');
        let letter = format!("{}:", drive_letter);

        if recovery_key.is_empty() {
            return UnlockResult::failure(&letter, "恢复密钥不能为空", None);
        }

        // 格式化恢复密钥
        let formatted_key = match format_recovery_key(recovery_key) {
            Ok(key) => key,
            Err(e) => return UnlockResult::failure(&letter, &e, None),
        };

        let status = self.get_status(drive_letter);
        if status == VolumeStatus::NotEncrypted {
            return UnlockResult::failure(&letter, "该驱动器未启用 BitLocker 加密", None);
        }
        if status == VolumeStatus::EncryptedUnlocked {
            return UnlockResult::success(&letter, "驱动器已经是解锁状态");
        }

        let result = if self.use_fveapi {
            self.unlock_with_recovery_key_fveapi(drive_letter, &formatted_key)
        } else {
            self.unlock_with_recovery_key_manage_bde(drive_letter, &formatted_key)
        };

        // 如果解锁成功，等待分区完全可访问
        if result.success {
            self.wait_for_unlock_complete(drive_letter, &letter)
        } else {
            result
        }
    }

    #[cfg(not(windows))]
    pub fn unlock_with_recovery_key(&self, drive: &str, _recovery_key: &str) -> UnlockResult {
        UnlockResult::failure(drive, "仅支持Windows系统", None)
    }

    /// 使用fveapi恢复密钥解锁
    #[cfg(windows)]
    fn unlock_with_recovery_key_fveapi(
        &self,
        drive_letter: char,
        recovery_key: &str,
    ) -> UnlockResult {
        let letter = format!("{}:", drive_letter);
        let volume_path = format!("{}:", drive_letter);

        let api = match FveApi::instance() {
            Ok(api) => api,
            Err(e) => return UnlockResult::failure(&letter, &e, None),
        };

        match api.open_volume(&volume_path) {
            Ok(handle) => match handle.unlock_with_recovery_key(recovery_key) {
                Ok(()) => {
                    log::info!("BitLocker 分区 {} 使用恢复密钥解锁成功 (fveapi)", letter);
                    UnlockResult::success(&letter, "解锁成功")
                }
                Err(FveError::BadRecoveryPassword) => {
                    log::warn!("BitLocker 分区 {} 恢复密钥错误 (fveapi)", letter);
                    UnlockResult::failure(&letter, "恢复密钥错误", Some(0x80310028))
                }
                Err(FveError::VolumeUnlocked) => {
                    UnlockResult::success(&letter, "驱动器已经是解锁状态")
                }
                Err(e) => {
                    log::error!("BitLocker 分区 {} 解锁失败: {} (fveapi)", letter, e);
                    UnlockResult::failure(&letter, &e.to_string(), None)
                }
            },
            Err(e) => {
                log::error!("BitLocker 分区 {} 打开失败: {} (fveapi)", letter, e);
                UnlockResult::failure(&letter, &e.to_string(), None)
            }
        }
    }

    /// 使用manage-bde恢复密钥解锁
    #[cfg(windows)]
    fn unlock_with_recovery_key_manage_bde(
        &self,
        drive_letter: char,
        recovery_key: &str,
    ) -> UnlockResult {
        use std::process::Command;

        let letter = format!("{}:", drive_letter);
        let drive = format!("{}:", drive_letter);

        let output = match Command::new("manage-bde")
            .args(["-unlock", &drive, "-recoverypassword", recovery_key])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .output()
        {
            Ok(o) => o,
            Err(e) => return UnlockResult::failure(&letter, &format!("执行命令失败: {}", e), None),
        };

        let stdout = decode_windows_output(&output.stdout);
        let stdout_lower = stdout.to_lowercase();

        if stdout_lower.contains("successfully unlocked")
            || stdout_lower.contains("已成功解锁")
            || stdout_lower.contains("unlock was successful")
            || stdout_lower.contains("解锁成功")
        {
            log::info!(
                "BitLocker 分区 {} 使用恢复密钥解锁成功 (manage-bde)",
                letter
            );
            UnlockResult::success(&letter, "解锁成功")
        } else if stdout_lower.contains("recovery password failed")
            || stdout_lower.contains("恢复密码失败")
            || stdout_lower.contains("incorrect recovery password")
            || stdout_lower.contains("恢复密码不正确")
            || stdout_lower.contains("恢复密钥不正确")
            || stdout_lower.contains("the recovery password is incorrect")
        {
            log::warn!("BitLocker 分区 {} 恢复密钥错误 (manage-bde)", letter);
            UnlockResult::failure(&letter, "恢复密钥错误", Some(0x80310028))
        } else if stdout_lower.contains("already unlocked") || stdout_lower.contains("已解锁") {
            UnlockResult::success(&letter, "驱动器已经是解锁状态")
        } else {
            log::error!(
                "BitLocker 分区 {} 解锁失败: {} (manage-bde)",
                letter,
                stdout
            );
            let error_msg = extract_error_message(&stdout)
                .unwrap_or_else(|| "解锁失败，请检查恢复密钥是否正确".to_string());
            UnlockResult::failure(&letter, &error_msg, None)
        }
    }

    /// 等待BitLocker解锁完全完成
    ///
    /// BitLocker解锁命令会立即返回，但实际解锁过程可能在后台继续进行。
    /// 此函数会等待分区完全可访问，确保后续操作不会因为解锁未完成而失败。
    #[cfg(windows)]
    fn wait_for_unlock_complete(&self, drive_letter: char, letter: &str) -> UnlockResult {
        use std::time::{Duration, Instant};

        log::info!("BitLocker 分区 {} 解锁命令已执行，等待完全解锁...", letter);

        let start_time = Instant::now();
        let timeout = Duration::from_secs(300); // 5分钟超时
        let check_interval = Duration::from_millis(500); // 每500ms检查一次

        loop {
            // 检查是否超时
            if start_time.elapsed() > timeout {
                log::error!("BitLocker 分区 {} 解锁超时（5分钟）", letter);
                return UnlockResult::failure(letter, "解锁超时，分区可能仍在后台处理中", None);
            }

            // 检查分区状态
            let status = self.get_status(drive_letter);

            match status {
                VolumeStatus::EncryptedUnlocked => {
                    // 状态显示已解锁，但还需要验证文件系统是否可访问
                    if self.verify_partition_accessible(drive_letter) {
                        let elapsed = start_time.elapsed();
                        log::info!(
                            "BitLocker 分区 {} 完全解锁成功，耗时 {:.1} 秒",
                            letter,
                            elapsed.as_secs_f64()
                        );
                        return UnlockResult::success(letter, "解锁成功");
                    } else {
                        log::debug!(
                            "BitLocker 分区 {} 状态为已解锁，但文件系统尚未就绪，继续等待...",
                            letter
                        );
                    }
                }
                VolumeStatus::NotEncrypted => {
                    // 已完全解密
                    log::info!("BitLocker 分区 {} 已完全解密", letter);
                    return UnlockResult::success(letter, "解锁成功");
                }
                VolumeStatus::EncryptedLocked => {
                    // 仍然锁定，可能解锁失败
                    log::warn!("BitLocker 分区 {} 仍处于锁定状态", letter);
                    return UnlockResult::failure(letter, "解锁失败，分区仍处于锁定状态", None);
                }
                _ => {
                    log::debug!(
                        "BitLocker 分区 {} 当前状态: {:?}，继续等待...",
                        letter,
                        status
                    );
                }
            }

            std::thread::sleep(check_interval);
        }
    }

    /// 验证分区是否可访问
    ///
    /// 通过尝试访问分区根目录来验证文件系统是否已就绪
    #[cfg(windows)]
    fn verify_partition_accessible(&self, drive_letter: char) -> bool {
        use std::path::Path;

        let drive_path = format!("{}:\\", drive_letter);
        let path = Path::new(&drive_path);

        // 尝试读取目录，如果成功说明文件系统已就绪
        match std::fs::read_dir(path) {
            Ok(_) => {
                log::debug!("分区 {} 文件系统可访问", drive_path);
                true
            }
            Err(e) => {
                log::debug!("分区 {} 文件系统尚未就绪: {}", drive_path, e);
                false
            }
        }
    }

    /// 彻底解密分区（关闭BitLocker加密）
    #[cfg(windows)]
    pub fn decrypt(&self, drive: &str) -> DecryptResult {
        let drive_letter = drive.chars().next().unwrap_or('C');
        let letter = format!("{}:", drive_letter);

        let status = self.get_status(drive_letter);

        match status {
            VolumeStatus::NotEncrypted => {
                return DecryptResult::success(&letter, "分区已经是未加密状态");
            }
            VolumeStatus::EncryptedLocked => {
                return DecryptResult::failure(
                    &letter,
                    "分区处于锁定状态，请先解锁后再进行彻底解密",
                    Some(0x80310001),
                );
            }
            VolumeStatus::Decrypting => {
                return DecryptResult::success(&letter, "分区正在解密中，请等待完成");
            }
            _ => {}
        }

        // 尝试使用 fveapi
        if self.use_fveapi {
            let result = self.decrypt_fveapi(drive_letter);
            if result.success {
                return result;
            }
            log::warn!("fveapi 解密失败，尝试回退到 manage-bde: {}", result.message);
        }

        // 回退到 manage-bde
        self.decrypt_manage_bde(drive_letter)
    }

    #[cfg(not(windows))]
    pub fn decrypt(&self, drive: &str) -> DecryptResult {
        DecryptResult::failure(drive, "仅支持Windows系统", None)
    }

    /// 使用fveapi解密
    #[cfg(windows)]
    fn decrypt_fveapi(&self, drive_letter: char) -> DecryptResult {
        let letter = format!("{}:", drive_letter);
        let volume_path = format!("{}:", drive_letter);

        let api = match FveApi::instance() {
            Ok(api) => api,
            Err(e) => return DecryptResult::failure(&letter, &e, None),
        };

        // 重要：解密操作需要写权限，必须使用 FveAccessMode::ReadWrite
        // 根据逆向分析，FveConversionDecryptEx 内部会验证句柄的访问模式 (mode=1)
        match api.open_volume_ex(&volume_path, FveAccessMode::ReadWrite) {
            Ok(handle) => match handle.start_decryption() {
                Ok(()) => {
                    log::info!("BitLocker 分区 {} 开始解密 (fveapi)", letter);
                    DecryptResult::success(&letter, "已开始解密，此过程可能需要较长时间，请勿中断")
                }
                Err(FveError::NotEncrypted) => {
                    DecryptResult::success(&letter, "分区已经是未加密状态")
                }
                Err(e) => {
                    log::error!("BitLocker 分区 {} 解密失败: {} (fveapi)", letter, e);
                    DecryptResult::failure(&letter, &e.to_string(), Some(e.code()))
                }
            },
            Err(e) => {
                log::error!(
                    "BitLocker 分区 {} 打开失败（需要写权限）: {} (fveapi)",
                    letter,
                    e
                );
                DecryptResult::failure(&letter, &e.to_string(), Some(e.code()))
            }
        }
    }

    /// 使用manage-bde解密
    #[cfg(windows)]
    fn decrypt_manage_bde(&self, drive_letter: char) -> DecryptResult {
        use std::process::Command;

        let letter = format!("{}:", drive_letter);
        let drive = format!("{}:", drive_letter);

        let output = match {
            let mut cmd = Command::new("manage-bde");
            cmd.args(["-off", &drive]);

            #[cfg(windows)]
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW

            cmd.output()
        } {
            Ok(o) => o,
            Err(e) => {
                return DecryptResult::failure(&letter, &format!("执行命令失败: {}", e), None)
            }
        };

        let stdout = decode_windows_output(&output.stdout);
        let stdout_lower = stdout.to_lowercase();

        if stdout_lower.contains("decryption is now in progress")
            || stdout_lower.contains("正在进行解密")
            || stdout_lower.contains("started decryption")
            || stdout_lower.contains("已开始解密")
            || stdout_lower.contains("解密正在进行")
        {
            log::info!("BitLocker 分区 {} 开始解密 (manage-bde)", letter);
            DecryptResult::success(&letter, "已开始解密，此过程可能需要较长时间，请勿中断")
        } else if stdout_lower.contains("already decrypted")
            || stdout_lower.contains("已解密")
            || stdout_lower.contains("not enabled")
            || stdout_lower.contains("未启用")
        {
            DecryptResult::success(&letter, "分区已经是未加密状态")
        } else {
            log::error!(
                "BitLocker 分区 {} 解密失败: {} (manage-bde)",
                letter,
                stdout
            );
            let error_msg =
                extract_error_message(&stdout).unwrap_or_else(|| "解密操作失败".to_string());
            DecryptResult::failure(&letter, &error_msg, None)
        }
    }

    /// 检查指定驱动器是否可以进行彻底解密
    pub fn can_decrypt(&self, drive_letter: char) -> bool {
        matches!(
            self.get_status(drive_letter),
            VolumeStatus::EncryptedUnlocked
        )
    }

    /// 获取所有BitLocker加密的卷
    #[cfg(windows)]
    pub fn get_encrypted_volumes(&self) -> Vec<VolumeInfo> {
        let mut volumes = Vec::new();

        log::info!("开始检测 BitLocker 分区...");
        let start_time = std::time::Instant::now();

        // 收集需要检测的驱动器
        let drives: Vec<char> = (b'A'..=b'Z')
            .map(|b| b as char)
            .filter(|&c| c != 'X') // 跳过PE系统盘
            .collect();

        // 检测所有驱动器
        for drive_letter in drives {
            if let Some(info) = self.probe_drive(drive_letter) {
                volumes.push(info);
            }
        }

        let elapsed = start_time.elapsed();
        log::info!(
            "BitLocker 检测完成，共发现 {} 个加密分区，耗时 {:?}",
            volumes.len(),
            elapsed
        );

        volumes
    }

    #[cfg(not(windows))]
    pub fn get_encrypted_volumes(&self) -> Vec<VolumeInfo> {
        Vec::new()
    }

    /// 探测单个驱动器
    #[cfg(windows)]
    fn probe_drive(&self, drive_letter: char) -> Option<VolumeInfo> {
        let drive = format!("{}:", drive_letter);
        let drive_path = format!("{}\\", drive);

        // 检查驱动器类型
        let wide_path: Vec<u16> = drive_path
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let drive_type = unsafe { GetDriveTypeW(PCWSTR(wide_path.as_ptr())) };

        // 只检查固定磁盘
        if drive_type != DRIVE_FIXED {
            log::trace!(
                "跳过驱动器 {}: 不是固定磁盘 (type={})",
                drive_letter,
                drive_type
            );
            return None;
        }

        log::debug!("探测驱动器 {} (固定磁盘)...", drive_letter);

        let status = self.get_status(drive_letter);

        log::debug!(
            "驱动器 {}: get_status 返回 {:?}, is_encrypted={}",
            drive_letter,
            status,
            status.is_encrypted()
        );

        if !status.is_encrypted() {
            log::debug!("驱动器 {}: 未加密，跳过", drive_letter);
            return None;
        }

        let (label, total_size_mb) = get_volume_info(&drive);

        let (protection_method, encryption_percentage) = if self.use_fveapi {
            self.get_volume_details_fveapi(drive_letter)
        } else {
            self.get_volume_details_manage_bde(drive_letter)
        };

        log::info!(
            "发现 BitLocker 分区: {} [{}] 状态={:?} 加密进度={:?}",
            drive,
            label,
            status,
            encryption_percentage
        );

        Some(VolumeInfo {
            letter: drive,
            label,
            total_size_mb,
            status,
            protection_method,
            encryption_percentage,
        })
    }

    /// 使用fveapi获取卷详情
    ///
    /// 使用 FveGetStatusW 直接获取状态，无需打开句柄
    #[cfg(windows)]
    fn get_volume_details_fveapi(&self, drive_letter: char) -> (String, Option<u8>) {
        let volume_path = format!("{}:", drive_letter);

        let api = match FveApi::instance() {
            Ok(api) => api,
            Err(_) => return self.get_volume_details_manage_bde(drive_letter),
        };

        match api.get_status_by_path(&volume_path) {
            Ok(info) => {
                let percentage = match info.volume_status {
                    FveVolumeStatus::FullyEncrypted => Some(100),
                    FveVolumeStatus::FullyDecrypted => Some(0),
                    _ => {
                        if info.encryption_percentage > 0 && info.encryption_percentage <= 100 {
                            Some(info.encryption_percentage as u8)
                        } else {
                            None
                        }
                    }
                };

                let method = match info.protection_status {
                    FveProtectionStatus::On => "密码/恢复密钥",
                    FveProtectionStatus::Off => "保护已暂停",
                    FveProtectionStatus::Unknown => "未知",
                };

                (method.to_string(), percentage)
            }
            Err(_) => self.get_volume_details_manage_bde(drive_letter),
        }
    }

    /// 使用manage-bde获取卷详情
    #[cfg(windows)]
    fn get_volume_details_manage_bde(&self, drive_letter: char) -> (String, Option<u8>) {
        use std::process::Command;

        let drive = format!("{}:", drive_letter);
        let output = match {
            let mut cmd = Command::new("manage-bde");
            cmd.args(["-status", &drive]);

            #[cfg(windows)]
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW

            cmd.output()
        } {
            Ok(o) => o,
            Err(_) => return ("密码/恢复密钥".to_string(), None),
        };

        let stdout = decode_windows_output(&output.stdout);
        (
            get_protection_method(&stdout),
            get_encryption_percentage(&stdout),
        )
    }

    /// 获取所有需要解锁的卷
    pub fn get_locked_volumes(&self) -> Vec<VolumeInfo> {
        self.get_encrypted_volumes()
            .into_iter()
            .filter(|v| v.needs_unlock())
            .collect()
    }

    /// 检查是否有任何锁定的卷
    pub fn has_locked_volumes(&self) -> bool {
        !self.get_locked_volumes().is_empty()
    }

    /// 检查指定的分区列表中是否有锁定的卷
    pub fn check_partitions_locked(&self, partitions: &[&str]) -> Vec<String> {
        partitions
            .iter()
            .filter(|p| {
                let letter = p.chars().next().unwrap_or('C');
                self.needs_unlock(letter)
            })
            .map(|p| p.to_string())
            .collect()
    }
}

impl Default for BitLockerManager {
    fn default() -> Self {
        Self::new()
    }
}

// ==================== 辅助函数 ====================

/// 解码 Windows 命令行输出
fn decode_windows_output(bytes: &[u8]) -> String {
    // 首先尝试 UTF-8
    if let Ok(s) = String::from_utf8(bytes.to_vec()) {
        let replacement_count = s.chars().filter(|&c| c == '\u{FFFD}').count();
        if replacement_count < 3 {
            return s;
        }
    }

    // 尝试 UTF-16 LE（检查 BOM）
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let u16_bytes: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();
        return String::from_utf16_lossy(&u16_bytes);
    }

    // 使用 GBK 编码解码
    let (decoded, _, _) = encoding_rs::GBK.decode(bytes);
    decoded.into_owned()
}

/// 从 manage-bde 输出中提取加密百分比
/// 例如："已加密百分比:      31.6%" 或 "Percentage Encrypted:    31.6%"
fn extract_encryption_percentage(output: &str) -> Option<f32> {
    for line in output.lines() {
        let line_lower = line.to_lowercase();
        if line_lower.contains("已加密百分比") || line_lower.contains("percentage encrypted")
        {
            // 提取百分比数字
            if let Some(colon_pos) = line.find(':').or_else(|| line.find('：')) {
                let value_part = &line[colon_pos + 1..];
                // 移除 % 符号并解析数字
                let number_str: String = value_part
                    .chars()
                    .filter(|c| c.is_numeric() || *c == '.')
                    .collect();
                if let Ok(percentage) = number_str.parse::<f32>() {
                    return Some(percentage);
                }
            }
        }
    }
    None
}

/// 从 manage-bde 状态输出判断 BitLocker 状态
fn determine_volume_status(output: &str) -> VolumeStatus {
    let output_lower = output.to_lowercase();

    // 优先检查转换状态（最准确的判断）
    // 正在解密：转换状态: 解密进行中
    if output_lower.contains("解密进行中")
        || output_lower.contains("decryption in progress")
        || output_lower.contains("解密正在进行")
    {
        return VolumeStatus::Decrypting;
    }

    // 正在加密：转换状态: 加密进行中
    if output_lower.contains("加密进行中")
        || output_lower.contains("encryption in progress")
        || output_lower.contains("加密正在进行")
    {
        return VolumeStatus::Encrypting;
    }

    // 检查是否完全解密（转换状态: 完全解密 + 已加密百分比: 0.0%）
    if output_lower.contains("完全解密") || output_lower.contains("fully decrypted") {
        // 额外检查百分比，确保真的是0%
        if output_lower.contains("已加密百分比") || output_lower.contains("percentage encrypted")
        {
            // 提取百分比
            if let Some(percentage) = extract_encryption_percentage(output) {
                if percentage > 0.0 {
                    return VolumeStatus::Decrypting;
                }
            }
        }
        return VolumeStatus::NotEncrypted;
    }

    // 检查是否未启用 BitLocker
    if output_lower.contains("protection off")
        || output_lower.contains("保护关闭")
        || output_lower.contains("未启用")
        || output_lower.contains("bitlocker drive encryption is not enabled")
        || output_lower.contains("未对此驱动器启用 bitlocker")
        || output_lower.contains("此驱动器未启用 bitlocker")
    {
        // 如果保护关闭，但还有加密百分比，说明正在解密
        if let Some(percentage) = extract_encryption_percentage(output) {
            if percentage > 0.0 {
                return VolumeStatus::Decrypting;
            }
        }
        return VolumeStatus::NotEncrypted;
    }

    // 检查是否锁定
    let is_locked = (output_lower.contains("lock status") && output_lower.contains("locked"))
        || (output_lower.contains("锁定状态") && output_lower.contains("已锁定"));

    // 检查是否已加密
    let is_encrypted = output_lower.contains("fully encrypted")
        || output_lower.contains("已完全加密")
        || output_lower.contains("protection on")
        || output_lower.contains("保护开启")
        || (output_lower.contains("encryption method") && !output_lower.contains("none"))
        || (output_lower.contains("加密方法") && !output_lower.contains("无"));

    if is_encrypted {
        if is_locked {
            return VolumeStatus::EncryptedLocked;
        } else {
            return VolumeStatus::EncryptedUnlocked;
        }
    }

    // 检查部分加密状态
    if output_lower.contains("conversion status") || output_lower.contains("转换状态") {
        if output_lower.contains("used space only encrypted")
            || output_lower.contains("仅已使用的空间已加密")
            || output_lower.contains("percentage encrypted")
            || output_lower.contains("加密百分比")
        {
            if is_locked {
                return VolumeStatus::EncryptedLocked;
            } else {
                return VolumeStatus::EncryptedUnlocked;
            }
        }
    }

    VolumeStatus::Unknown
}

/// 从 manage-bde 状态输出获取保护方法
fn get_protection_method(output: &str) -> String {
    for line in output.lines() {
        let line = line.trim();
        let line_lower = line.to_lowercase();

        if line_lower.contains("key protector")
            || line_lower.contains("密钥保护程序")
            || line_lower.contains("加密方法")
            || line_lower.contains("encryption method")
        {
            if let Some(colon_pos) = line.find(':').or_else(|| line.find('：')) {
                let value = line[colon_pos + 1..].trim();
                if !value.is_empty() && value != "None" && value != "无" {
                    return value.to_string();
                }
            }
        }
    }

    "密码/恢复密钥".to_string()
}

/// 从 manage-bde 状态输出中解析加密百分比
fn get_encryption_percentage(output: &str) -> Option<u8> {
    for line in output.lines() {
        let line = line.trim();
        let line_lower = line.to_lowercase();

        if line_lower.contains("percentage encrypted")
            || line_lower.contains("加密百分比")
            || line_lower.contains("percentage decrypted")
            || line_lower.contains("解密百分比")
        {
            if let Some(colon_pos) = line.find(':').or_else(|| line.find('：')) {
                let value_part = line[colon_pos + 1..].trim();
                let num_str: String = value_part
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.')
                    .collect();

                if let Ok(percentage) = num_str.parse::<f64>() {
                    return Some(percentage.round() as u8);
                }
            }
        }
    }

    let output_lower = output.to_lowercase();
    if output_lower.contains("fully encrypted") || output_lower.contains("已完全加密") {
        return Some(100);
    }

    if output_lower.contains("fully decrypted") || output_lower.contains("已完全解密") {
        return Some(0);
    }

    None
}

/// 从 manage-bde 输出中提取错误信息
fn extract_error_message(output: &str) -> Option<String> {
    for line in output.lines() {
        let line = line.trim();
        let line_lower = line.to_lowercase();
        if line_lower.contains("error") || line.contains("错误") {
            if line.len() > 10 {
                return Some(line.to_string());
            }
        }
    }
    None
}

/// 从 manage-bde 输出中提取恢复密钥
fn extract_recovery_key(output: &str) -> Option<String> {
    for line in output.lines() {
        let trimmed = line.trim();
        // 恢复密钥格式: 111111-222222-333333-444444-555555-666666-777777-888888
        // 长度 = 6*8 + 7个连字符 = 48 + 7 = 55
        if trimmed.len() == 55 {
            let parts: Vec<&str> = trimmed.split('-').collect();
            if parts.len() == 8 {
                let all_numeric = parts
                    .iter()
                    .all(|part| part.len() == 6 && part.chars().all(|c| c.is_ascii_digit()));
                if all_numeric {
                    return Some(trimmed.to_string());
                }
            }
        }
    }
    None
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

// ==================== 便捷函数 ====================

/// 检查是否有任何BitLocker锁定的分区
pub fn has_locked_partitions() -> bool {
    BitLockerManager::new().has_locked_volumes()
}

/// 获取所有锁定的分区
pub fn get_locked_partitions() -> Vec<VolumeInfo> {
    BitLockerManager::new().get_locked_volumes()
}

/// 获取所有加密的分区（包括已解锁的）
pub fn get_encrypted_partitions() -> Vec<VolumeInfo> {
    BitLockerManager::new().get_encrypted_volumes()
}

/// 检查指定分区是否需要解锁
pub fn partition_needs_unlock(drive: &str) -> bool {
    let letter = drive.chars().next().unwrap_or('C');
    BitLockerManager::new().needs_unlock(letter)
}

/// 使用密码解锁分区
pub fn unlock_partition_with_password(drive: &str, password: &str) -> UnlockResult {
    BitLockerManager::new().unlock_with_password(drive, password)
}

/// 使用恢复密钥解锁分区
pub fn unlock_partition_with_recovery_key(drive: &str, recovery_key: &str) -> UnlockResult {
    BitLockerManager::new().unlock_with_recovery_key(drive, recovery_key)
}

/// 彻底解密分区（关闭BitLocker加密）
pub fn decrypt_partition(drive: &str) -> DecryptResult {
    BitLockerManager::new().decrypt(drive)
}

/// 检查指定分区是否可以进行彻底解密
pub fn partition_can_decrypt(drive: &str) -> bool {
    let letter = drive.chars().next().unwrap_or('C');
    BitLockerManager::new().can_decrypt(letter)
}

/// 获取指定分区的恢复密钥
pub fn get_recovery_key_partition(drive: &str) -> Result<String, String> {
    BitLockerManager::new().get_recovery_key(drive)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_volume_status() {
        assert!(!VolumeStatus::NotEncrypted.needs_unlock());
        assert!(!VolumeStatus::EncryptedUnlocked.needs_unlock());
        assert!(VolumeStatus::EncryptedLocked.needs_unlock());

        assert!(!VolumeStatus::NotEncrypted.is_encrypted());
        assert!(VolumeStatus::EncryptedLocked.is_encrypted());
        assert!(VolumeStatus::EncryptedUnlocked.is_encrypted());
    }

    #[test]
    fn test_unlock_result() {
        let success = UnlockResult::success("C:", "OK");
        assert!(success.success);
        assert_eq!(success.letter, "C:");

        let failure = UnlockResult::failure("D:", "Error", Some(0x80310027));
        assert!(!failure.success);
        assert_eq!(failure.error_code, Some(0x80310027));
    }

    #[test]
    fn test_status_parsing() {
        let english_locked = r#"
BitLocker Drive Encryption: Configuration Tool version 10.0.19041
Volume D: []
    Conversion Status:    Fully Encrypted
    Lock Status:          Locked
"#;
        assert_eq!(
            determine_volume_status(english_locked),
            VolumeStatus::EncryptedLocked
        );

        let english_unlocked = r#"
Volume D: []
    Conversion Status:    Fully Encrypted
    Lock Status:          Unlocked
"#;
        assert_eq!(
            determine_volume_status(english_unlocked),
            VolumeStatus::EncryptedUnlocked
        );

        let not_encrypted = r#"
Volume C: []
    Protection Status:    Protection Off
    Conversion Status:    Fully Decrypted
"#;
        assert_eq!(
            determine_volume_status(not_encrypted),
            VolumeStatus::NotEncrypted
        );
    }

    #[test]
    fn test_encryption_percentage_parsing() {
        let english_with_percentage = r#"
BitLocker Drive Encryption: Configuration Tool version 10.0.19041
Volume D: []
    Percentage Encrypted: 75.5%
    Conversion Status:    Encryption In Progress
"#;
        assert_eq!(get_encryption_percentage(english_with_percentage), Some(76));

        let fully_encrypted = r#"
Volume D: []
    Conversion Status:    Fully Encrypted
"#;
        assert_eq!(get_encryption_percentage(fully_encrypted), Some(100));

        let fully_decrypted = r#"
Volume C: []
    Conversion Status:    Fully Decrypted
"#;
        assert_eq!(get_encryption_percentage(fully_decrypted), Some(0));
    }
}
