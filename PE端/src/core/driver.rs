//! Windows 驱动管理模块（实现已移入共享库 lr-core，此处再导出以保持调用方不变）。

// 保留原模块导出边界；PE 当前没有直接引用，但后续恢复驱动 UI 时无需改调用路径。
#[allow(unused_imports)]
pub use lr_core::driver::*;
