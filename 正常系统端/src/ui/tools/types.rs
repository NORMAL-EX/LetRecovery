//! 工具箱类型定义

/// 驱动备份/还原模式
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum DriverBackupMode {
    #[default]
    Export,
    Import,
}

/// APPX包信息
#[derive(Debug, Clone)]
pub struct AppxPackageInfo {
    pub package_name: String,
    pub display_name: String,
}

/// 已安装软件信息
#[derive(Debug, Clone)]
pub struct InstalledSoftware {
    pub name: String,
    pub version: String,
    pub publisher: String,
    pub install_location: String,
}

/// Windows分区信息（用于下拉框显示）
#[derive(Debug, Clone)]
pub struct WindowsPartitionInfo {
    pub letter: String,
    pub windows_version: String,
    pub architecture: String,
}

/// GHO密码查看结果
#[derive(Debug, Clone, Default)]
pub struct GhoPasswordResult {
    /// 文件路径
    pub file_path: String,
    /// 是否有效的GHO文件
    pub is_valid: bool,
    /// 是否有密码保护
    pub has_password: bool,
    /// 密码（如果能解密）
    pub password: Option<String>,
    /// 密码长度
    pub password_length: usize,
    /// 错误/状态消息
    pub message: String,
}

/// 英伟达驱动卸载结果
#[derive(Debug, Clone, Default)]
pub struct NvidiaUninstallResult {
    /// 是否成功
    pub success: bool,
    /// 消息
    pub message: String,
    /// 是否需要重启
    pub needs_reboot: bool,
    /// 成功卸载数量
    pub uninstalled_count: usize,
    /// 失败数量
    pub failed_count: usize,
}

/// 镜像校验结果（用于UI显示）
#[derive(Debug, Clone, Default)]
pub struct ImageVerifyResult {
    /// 文件路径
    pub file_path: String,
    /// 镜像类型名称
    pub image_type: String,
    /// 是否通过校验
    pub is_valid: bool,
    /// 状态文本
    pub status_text: String,
    /// 文件大小（字节）
    pub file_size: u64,
    /// 镜像数量
    pub image_count: u32,
    /// 分卷数量
    pub part_count: u16,
    /// 详细消息
    pub message: String,
    /// 详细信息列表
    pub details: Vec<String>,
}
