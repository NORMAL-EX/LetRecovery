//! LetRecovery 两端（PE端 / 正常系统端）共享核心库。
//!
//! 目标：逐步收纳两端重复的核心模块，消除复制粘贴。
//! 当前已收纳：
//! - wimlib DLL 兜底（内置 libwim-15.dll，运行时自动释放到 exe 目录）
//!
//! 后续计划收纳：镜像元数据类型 + XML 解析、wimlib FFI 封装等
//! （见仓库 TESTING.md）。

pub mod backup_atomic_publish;
pub mod backup_handoff;
pub mod backup_image_catalog;
pub mod bl_passthrough;
pub mod boot;
pub mod boot_pca;
pub mod bounded_failure_summary;
pub mod cached_artifact;
pub mod command;
pub mod custom_install;
pub mod data_staging;
pub mod defender_removal;
pub mod diskpart;
pub mod dism_driver_inventory;
pub mod download_integrity;
pub mod driver;
pub mod driver_trust;
pub mod encoding;
pub mod first_logon;
pub mod format_command;
pub mod fveapi;
pub mod handoff_auth;
pub mod handoff_manifest;
pub mod hash;
pub mod image_meta;
pub mod install_handoff;
pub mod install_log_handoff;
pub mod install_source_lock;
pub mod offline_appx;
pub mod offline_international;
pub mod offline_update_control;
pub mod offline_windows_update_removal;
pub mod onedrive_removal;
pub mod operation;
pub mod pca_compat;
pub mod pca_preflight;
pub mod personal_files;
pub mod progress_raster;
pub mod reboot;
pub mod registry;
pub mod reserved_storage;
pub mod sam;
pub mod scoped_temp_file;
pub mod sec_health_ui;
pub mod service_diagnostic;
pub mod software_install;
pub mod storage_driver_match;
pub mod traditional_chinese;
pub mod unattend_account;
pub mod unattend_command;
pub mod wim_engine;
pub mod wimgapi;
pub mod wimlib;
pub mod wimlib_dll;
pub mod win7_driver_package;
pub mod windows11_shell;
pub mod windows_accounts;
pub mod windows_cabinet;
pub mod windows_compat;
pub mod windows_diagnostics;
pub mod windows_file_copy;
pub mod windows_file_version;
pub mod windows_firmware;
pub mod windows_hardware;
pub mod windows_shutdown;
pub mod windows_storage;
pub mod xp;
pub mod xp_i386;
pub mod xp_textmode_drv;

pub use wim_engine::{active_engine, set_active_engine, WimEngine, WimEngineManager};
pub use wimlib_dll::ensure_dll_available;
