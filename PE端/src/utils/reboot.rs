//! PE 结束 pecmd.exe（实现已移入共享库 lr-core，此处再导出以保持调用方不变）。

pub use lr_core::reboot::reboot_pe;
