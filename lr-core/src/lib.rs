//! LetRecovery 两端（PE端 / 正常系统端）共享核心库。
//!
//! 目标：逐步收纳两端重复的核心模块，消除复制粘贴。
//! 当前已收纳：
//! - wimlib DLL 兜底（内置 libwim-15.dll，运行时自动释放到 exe 目录）
//!
//! 后续计划收纳：镜像元数据类型 + XML 解析、wimlib FFI 封装等
//! （见仓库 TESTING.md）。

pub mod bl_passthrough;
pub mod command;
pub mod encoding;
pub mod fveapi;
pub mod hash;
pub mod image_meta;
pub mod reboot;
pub mod registry;
pub mod sam;
pub mod wimlib;
pub mod wimlib_dll;

pub use wimlib_dll::ensure_dll_available;
