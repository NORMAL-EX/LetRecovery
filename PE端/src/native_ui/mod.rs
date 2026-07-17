//! Native Win32 presentation boundary for the PE client.
//!
//! The production PE workflow uses the same Inno Setup 6.7 Modern Windows 11 colour, metric and
//! native-control direction as the desktop client. Rendering remains separate from disk and imaging
//! behaviour so the shared workflow session has exactly one worker owner.

pub mod controls;
pub mod details;
pub mod layout;
pub mod progress;
pub mod state;
pub mod theme;
pub mod window;
