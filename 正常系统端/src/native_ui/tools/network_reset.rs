//! Dedicated native confirmation dialog for the existing network-reset operation.
//!
//! It has no input controls and never executes a command. The text deliberately mirrors the four
//! commands in the current backend so the user can assess the real firewall and connectivity risk.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontW, DeleteObject, HFONT};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, MoveWindow, SendMessageW, WM_SETFONT,
};

use super::super::controls::{child, wide};
use super::super::dialog::{DialogButtons, DialogResult, DialogShell, DialogSpec};
use super::super::layout::{measure_text, LayoutMetrics};

pub const NETWORK_RESET_COMMANDS: [&str; 4] = [
    "netsh winsock reset",
    "netsh int ip reset",
    "ipconfig /flushdns",
    "netsh advfirewall reset",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkResetDialogIntent {
    Confirm,
    Close,
}

pub struct NativeNetworkResetDialog {
    pub shell: DialogShell,
    commands: HWND,
    risk: HWND,
    font: HFONT,
}

impl NativeNetworkResetDialog {
    pub unsafe fn create(owner: HWND) -> windows::core::Result<Self> {
        let shell = DialogShell::create(
            owner,
            DialogSpec {
                window_title: crate::tr!("确认重置网络设置"),
                title: crate::tr!("确认重置网络设置"),
                description: crate::tr!("此操作会修改当前 Windows 网络配置。"),
                width: 650,
                height: 440,
                buttons: DialogButtons {
                    primary: crate::tr!("确认重置"),
                    secondary: None,
                    cancel: Some(crate::tr!("取消")),
                },
            },
        )?;
        let commands = child(shell.content(), w!("STATIC"), &commands_text(), 0, 64_710)?;
        let risk = child(
            shell.content(),
            w!("STATIC"),
            &crate::tr!(
                "风险：将重置 Winsock 和 TCP/IP、清空 DNS 缓存，并把 Windows 防火墙策略恢复为默认值。自定义防火墙规则可能丢失，网络可能暂时中断；完成后建议重启。"
            ),
            0,
            64_711,
        )?;
        let dpi = GetDpiForWindow(shell.hwnd()).max(96);
        let face = wide("Microsoft YaHei UI");
        let font = CreateFontW(
            -scale(14, dpi),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            1,
            0,
            0,
            5,
            0,
            PCWSTR(face.as_ptr()),
        );
        for control in [commands, risk] {
            let _ = SendMessageW(control, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        }
        let mut dialog = Self {
            shell,
            commands,
            risk,
            font,
        };
        dialog.fit_and_layout();
        Ok(dialog)
    }

    pub unsafe fn show_modeless(&mut self) {
        self.fit_and_layout();
        self.shell.show_modeless();
    }

    pub fn take_intent(&mut self) -> Option<NetworkResetDialogIntent> {
        self.shell.take_result().map(map_result)
    }

    unsafe fn fit_and_layout(&mut self) {
        let dpi = GetDpiForWindow(self.shell.hwnd()).max(96);
        let mut rect = RECT::default();
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let width = (rect.right - rect.left).max(0);
        let metrics = LayoutMetrics::for_dpi(dpi);
        let commands_height = measure_text(
            self.shell.content(),
            self.font,
            &commands_text(),
            Some(width),
        )
        .height
        .max(metrics.label_height);
        let risk_height = measure_text(
            self.shell.content(),
            self.font,
            &crate::tr!(
                "风险：将重置 Winsock 和 TCP/IP、清空 DNS 缓存，并把 Windows 防火墙策略恢复为默认值。自定义防火墙规则可能丢失，网络可能暂时中断；完成后建议重启。"
            ),
            Some(width),
        )
        .height
        .max(metrics.label_height);
        let layout =
            NetworkResetContentLayout::calculate(commands_height, risk_height, metrics.control_gap);
        self.shell
            .fit_content_height(logical_height(layout.content_height, dpi));
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let width = (rect.right - rect.left).max(0);
        let _ = MoveWindow(self.commands, 0, 0, width, layout.commands_height, true);
        let _ = MoveWindow(self.risk, 0, layout.risk_y, width, layout.risk_height, true);
    }
}

impl Drop for NativeNetworkResetDialog {
    fn drop(&mut self) {
        unsafe {
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font);
            }
        }
    }
}

fn commands_text() -> String {
    let commands = NETWORK_RESET_COMMANDS
        .iter()
        .map(|command| format!("• {command}"))
        .collect::<Vec<_>>()
        .join("\r\n");
    format!("{}\r\n\r\n{commands}", crate::tr!("实际执行的操作："))
}

fn map_result(result: DialogResult) -> NetworkResetDialogIntent {
    match result {
        DialogResult::Primary => NetworkResetDialogIntent::Confirm,
        DialogResult::Secondary | DialogResult::Cancel => NetworkResetDialogIntent::Close,
    }
}

fn scale(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32
}

fn logical_height(value: i32, dpi: u32) -> i32 {
    ((i64::from(value.max(0)) * 96 + i64::from(dpi.max(1)) / 2) / i64::from(dpi.max(1))) as i32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NetworkResetContentLayout {
    commands_height: i32,
    risk_y: i32,
    risk_height: i32,
    content_height: i32,
}

impl NetworkResetContentLayout {
    const fn calculate(commands_height: i32, risk_height: i32, gap: i32) -> Self {
        let risk_y = commands_height + gap;
        Self {
            commands_height,
            risk_y,
            risk_height,
            content_height: risk_y + risk_height,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_disclosure_matches_the_existing_backend() {
        assert_eq!(
            NETWORK_RESET_COMMANDS,
            [
                "netsh winsock reset",
                "netsh int ip reset",
                "ipconfig /flushdns",
                "netsh advfirewall reset",
            ]
        );
        let text = commands_text();
        assert!(NETWORK_RESET_COMMANDS
            .iter()
            .all(|command| text.contains(command)));
    }

    #[test]
    fn only_primary_result_confirms_network_reset() {
        assert_eq!(
            map_result(DialogResult::Primary),
            NetworkResetDialogIntent::Confirm
        );
        assert_eq!(
            map_result(DialogResult::Cancel),
            NetworkResetDialogIntent::Close
        );
        assert_eq!(
            map_result(DialogResult::Secondary),
            NetworkResetDialogIntent::Close
        );
    }

    #[test]
    fn compact_layout_keeps_disclosure_and_risk_together() {
        let layout = NetworkResetContentLayout::calculate(110, 100, 10);
        assert_eq!(layout.risk_y - layout.commands_height, 10);
        assert_eq!(layout.content_height, 220);
    }
}
