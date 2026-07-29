//! Native Win32 frontend.
//!
//! This module owns only presentation and user intent. Destructive operations remain in the
//! existing typed Rust core and are never executed from a window procedure.

mod controls;
pub mod dialog;
pub mod driver_transfer_dialog;
pub(crate) mod layout;
mod pages;
mod redraw;
mod scrollbar_compositor;
mod theme;
pub mod tool_dialogs;
pub mod tool_dialogs_mutating;
pub mod tools;
mod window;

pub(crate) use window::enable_process_dpi_awareness;
pub use window::run;
