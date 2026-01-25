pub mod about;
pub mod advanced_options;
pub mod download_progress;
pub mod easy_mode;
pub mod embedded_assets;
pub mod hardware_info;
pub mod install_progress;
pub mod online_download;
pub mod system_backup;
pub mod system_install;
pub mod tools;

// 导出MD5校验状态
pub use download_progress::Md5VerifyState;

// 导出内嵌资源
pub use embedded_assets::{EmbeddedAssets, EmbeddedLogoType};
