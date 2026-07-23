//! Native toolbox page controls and side-effect-free command intents.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::HFONT;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    MoveWindow, SendMessageW, SetWindowTextW, ShowWindow, BS_OWNERDRAW, SW_HIDE, SW_SHOW,
    WM_SETFONT, WS_TABSTOP,
};

use super::download::PageRect;
use crate::native_ui::controls::{child, wide};

const FIRST_TOOL_ID: u16 = 5_100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ToolGridLayout {
    columns: i32,
    button_width: i32,
    button_height: i32,
    gap: i32,
    grid_y: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolIntent {
    NvidiaDriverRemoval,
    PartitionCopy,
    BatchFormat,
    ImportStorageDriver,
    QuickPartition,
    RemoveAppx,
    DriverBackupRestore,
    RepairBoot,
    NetworkInformation,
    SoftwareList,
    TimeSynchronization,
    RunGhost,
    ReadGhoPassword,
    ResetNetwork,
    RunSpaceSniffer,
    VerifyImage,
    ManageBitLocker,
    VerifyFileHash,
    ResetPassword,
    ExpandC,
    HardwareInspector,
}

impl ToolIntent {
    /// Existing command IDs remain stable; new tools are appended.
    pub const ALL: [Self; 21] = [
        Self::NvidiaDriverRemoval,
        Self::PartitionCopy,
        Self::BatchFormat,
        Self::ImportStorageDriver,
        Self::QuickPartition,
        Self::RemoveAppx,
        Self::DriverBackupRestore,
        Self::RepairBoot,
        Self::NetworkInformation,
        Self::SoftwareList,
        Self::TimeSynchronization,
        Self::RunGhost,
        Self::ReadGhoPassword,
        Self::ResetNetwork,
        Self::RunSpaceSniffer,
        Self::VerifyImage,
        Self::ManageBitLocker,
        Self::VerifyFileHash,
        Self::ResetPassword,
        Self::ExpandC,
        Self::HardwareInspector,
    ];

    pub const fn command_id(self) -> u16 {
        FIRST_TOOL_ID + self as u16
    }
}

pub struct ToolLabels<'a> {
    /// Labels for the first nineteen legacy entries, in `ToolIntent::ALL` order.
    /// The restored Expand C entry owns its caption in this module until the host is wired.
    pub buttons: [&'a str; 19],
    pub introduction: &'a str,
}

fn tool_grid_layout(rect: PageRect, dpi: u32) -> ToolGridLayout {
    let s = |value: i32| value * dpi.max(1) as i32 / 96;
    let width = rect.width.max(0);
    let height = rect.height.max(0);
    let gap = s(8);
    let grid_y = rect.y + s(34).min(height);
    let available_height = (rect.y + height - grid_y).max(0);
    let preferred = ((width + gap) / (s(156) + gap)).clamp(1, 4);
    let minimum_height = s(26);
    let mut columns = preferred;
    while columns < 4 {
        let rows = (ToolIntent::ALL.len() as i32 + columns - 1) / columns;
        let candidate = (available_height - gap * (rows - 1)) / rows.max(1);
        if candidate >= minimum_height {
            break;
        }
        columns += 1;
    }
    let rows = (ToolIntent::ALL.len() as i32 + columns - 1) / columns;
    let button_width = ((width - gap * (columns - 1)) / columns).max(0);
    let button_height = s(38).min(((available_height - gap * (rows - 1)) / rows.max(1)).max(0));
    ToolGridLayout {
        columns,
        button_width,
        button_height,
        gap,
        grid_y,
    }
}

pub struct ToolsPage {
    pub introduction: HWND,
    pub buttons: [HWND; 21],
}

impl ToolsPage {
    /// Creates hidden buttons. No tool process or privileged operation is launched here.
    pub unsafe fn create(
        parent: HWND,
        font: HFONT,
        labels: &ToolLabels<'_>,
    ) -> windows::core::Result<Self> {
        let introduction = child(parent, w!("STATIC"), labels.introduction, 0, 5_099)?;
        let mut buttons = [HWND::default(); 21];
        for (index, intent) in ToolIntent::ALL.into_iter().enumerate() {
            // Keep the existing `ToolLabels` contract untouched for the first nineteen tools.
            // The restored legacy entry owns its caption here until the host adopts a dedicated
            // label field; this prevents an unrelated window.rs edit from being required.
            let label = match intent {
                ToolIntent::ExpandC => crate::tr!("无损扩大C盘"),
                ToolIntent::HardwareInspector => crate::tr!("详细硬件检测"),
                _ => labels.buttons[index].to_owned(),
            };
            buttons[index] = child(
                parent,
                w!("BUTTON"),
                &label,
                BS_OWNERDRAW | WS_TABSTOP.0 as i32,
                intent.command_id(),
            )?;
        }
        let page = Self {
            introduction,
            buttons,
        };
        page.apply_font(font);
        page.show(false);
        Ok(page)
    }

    /// Translates a command ID into a request. The host must confirm and dispatch it.
    pub fn command_intent(command_id: u16) -> Option<ToolIntent> {
        let index = command_id.checked_sub(FIRST_TOOL_ID)? as usize;
        ToolIntent::ALL.get(index).copied()
    }

    /// Some tools are deliberately unavailable outside their supported environment.
    pub unsafe fn apply_environment(&self, is_pe: bool) {
        for intent in [
            ToolIntent::RepairBoot,
            ToolIntent::SoftwareList,
            ToolIntent::ResetNetwork,
            ToolIntent::ExpandC,
            ToolIntent::HardwareInspector,
        ] {
            let supported = match intent {
                ToolIntent::RepairBoot => is_pe,
                ToolIntent::SoftwareList
                | ToolIntent::ResetNetwork
                | ToolIntent::ExpandC
                | ToolIntent::HardwareInspector => !is_pe,
                _ => true,
            };
            let _ = EnableWindow(self.buttons[intent as usize], supported);
        }
    }

    pub unsafe fn relocalize(&self, labels: &ToolLabels<'_>) {
        set_text(self.introduction, labels.introduction);
        for (index, intent) in ToolIntent::ALL.into_iter().enumerate() {
            let label = match intent {
                ToolIntent::ExpandC => crate::tr!("无损扩大C盘"),
                ToolIntent::HardwareInspector => crate::tr!("详细硬件检测"),
                _ => labels.buttons[index].to_owned(),
            };
            set_text(self.buttons[index], &label);
        }
    }

    pub unsafe fn layout(&self, rect: PageRect, dpi: u32) {
        let s = |value: i32| value * dpi as i32 / 96;
        let layout = tool_grid_layout(rect, dpi);
        let _ = MoveWindow(
            self.introduction,
            rect.x,
            rect.y,
            rect.width.max(0),
            s(24).min(rect.height.max(0)),
            true,
        );
        for (index, button) in self.buttons.iter().copied().enumerate() {
            let column = index as i32 % layout.columns;
            let row = index as i32 / layout.columns;
            let _ = MoveWindow(
                button,
                rect.x + column * (layout.button_width + layout.gap),
                layout.grid_y + row * (layout.button_height + layout.gap),
                layout.button_width,
                layout.button_height,
                true,
            );
        }
    }

    pub unsafe fn show(&self, visible: bool) {
        let command = if visible { SW_SHOW } else { SW_HIDE };
        let _ = ShowWindow(self.introduction, command);
        for button in self.buttons {
            let _ = ShowWindow(button, command);
        }
    }

    pub unsafe fn apply_font(&self, font: HFONT) {
        let _ = SendMessageW(
            self.introduction,
            WM_SETFONT,
            WPARAM(font.0 as usize),
            LPARAM(1),
        );
        for button in self.buttons {
            let _ = SendMessageW(button, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        }
    }
}

unsafe fn set_text(hwnd: HWND, value: &str) {
    let value = wide(value);
    let _ = SetWindowTextW(hwnd, PCWSTR(value.as_ptr()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_a_stable_unique_command() {
        for (index, intent) in ToolIntent::ALL.into_iter().enumerate() {
            assert_eq!(ToolsPage::command_intent(intent.command_id()), Some(intent));
            assert_eq!(intent.command_id(), FIRST_TOOL_ID + index as u16);
        }
        assert_eq!(ToolsPage::command_intent(FIRST_TOOL_ID - 1), None);
        assert_eq!(
            ToolsPage::command_intent(FIRST_TOOL_ID + 19),
            Some(ToolIntent::ExpandC)
        );
        assert_eq!(
            ToolsPage::command_intent(FIRST_TOOL_ID + 20),
            Some(ToolIntent::HardwareInspector)
        );
        assert_eq!(ToolsPage::command_intent(FIRST_TOOL_ID + 21), None);
    }

    #[test]
    fn tool_grid_never_crosses_the_page_bottom_at_low_resolution() {
        for (rect, dpi) in [
            (
                PageRect {
                    x: 0,
                    y: 0,
                    width: 556,
                    height: 340,
                },
                96,
            ),
            (
                PageRect {
                    x: 20,
                    y: 30,
                    width: 1_112,
                    height: 680,
                },
                192,
            ),
        ] {
            let layout = tool_grid_layout(rect, dpi);
            let last_index = ToolIntent::ALL.len() as i32 - 1;
            let last_row = last_index / layout.columns;
            let bottom = layout.grid_y
                + last_row * (layout.button_height + layout.gap)
                + layout.button_height;
            assert!(bottom <= rect.y + rect.height);
            assert!(layout.button_width >= 0);
            assert!(layout.button_height >= 0);
        }
    }

    #[test]
    fn tool_grid_drops_the_forced_two_column_minimum() {
        let layout = tool_grid_layout(
            PageRect {
                x: 0,
                y: 0,
                width: 140,
                height: 800,
            },
            96,
        );
        assert_eq!(layout.columns, 1);
        assert_eq!(layout.button_width, 140);
    }
}
