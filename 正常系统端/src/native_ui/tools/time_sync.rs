//! Dedicated native confirmation dialog for synchronizing the system clock.
//!
//! The legacy operation has no user-selectable server or other input. This module only displays
//! the fixed fallback order used by the existing backend and returns a confirmation intent.

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

pub const TRUSTED_NTP_FALLBACKS: [&str; 5] = [
    "ntp.aliyun.com",
    "ntp.tencent.com",
    "cn.ntp.org.cn",
    "time.windows.com",
    "pool.ntp.org",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeSyncDialogIntent {
    Confirm,
    Close,
}

pub struct NativeTimeSyncDialog {
    pub shell: DialogShell,
    fallback_list: HWND,
    notice: HWND,
    font: HFONT,
}

impl NativeTimeSyncDialog {
    pub unsafe fn create(owner: HWND) -> windows::core::Result<Self> {
        let shell = DialogShell::create(
            owner,
            DialogSpec {
                window_title: crate::tr!("系统时间校准"),
                title: crate::tr!("系统时间校准"),
                description: crate::tr!("是否立即网络同步本机的时间到北京时间？"),
                width: 560,
                height: 390,
                buttons: DialogButtons {
                    primary: crate::tr!("确定"),
                    secondary: None,
                    cancel: Some(crate::tr!("取消")),
                },
            },
        )?;
        let fallback_list = child(shell.content(), w!("STATIC"), &fallback_text(), 0, 64_700)?;
        let notice = child(
            shell.content(),
            w!("STATIC"),
            &crate::tr!("将按顺序尝试，前一个服务器不可用时才使用下一个。"),
            0,
            64_701,
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
        for control in [fallback_list, notice] {
            let _ = SendMessageW(control, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        }
        let mut dialog = Self {
            shell,
            fallback_list,
            notice,
            font,
        };
        dialog.fit_and_layout();
        Ok(dialog)
    }

    pub unsafe fn show_modeless(&mut self) {
        self.fit_and_layout();
        self.shell.show_modeless();
    }

    pub fn take_intent(&mut self) -> Option<TimeSyncDialogIntent> {
        self.shell.take_result().map(map_result)
    }

    unsafe fn fit_and_layout(&mut self) {
        let dpi = GetDpiForWindow(self.shell.hwnd()).max(96);
        let mut rect = RECT::default();
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let width = (rect.right - rect.left).max(0);
        let metrics = LayoutMetrics::for_dpi(dpi);
        let fallback_height = measure_text(
            self.shell.content(),
            self.font,
            &fallback_text(),
            Some(width),
        )
        .height
        .max(metrics.label_height);
        let notice_height = measure_text(
            self.shell.content(),
            self.font,
            &crate::tr!("将按顺序尝试，前一个服务器不可用时才使用下一个。"),
            Some(width),
        )
        .height
        .max(metrics.label_height);
        let layout =
            TimeSyncContentLayout::calculate(fallback_height, notice_height, metrics.control_gap);
        self.shell
            .fit_content_height(logical_height(layout.content_height, dpi));
        let _ = GetClientRect(self.shell.content(), &mut rect);
        let width = (rect.right - rect.left).max(0);
        let _ = MoveWindow(
            self.fallback_list,
            0,
            0,
            width,
            layout.fallback_height,
            true,
        );
        let _ = MoveWindow(
            self.notice,
            0,
            layout.notice_y,
            width,
            layout.notice_height,
            true,
        );
    }
}

impl Drop for NativeTimeSyncDialog {
    fn drop(&mut self) {
        unsafe {
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font);
            }
        }
    }
}

fn fallback_text() -> String {
    let servers = TRUSTED_NTP_FALLBACKS
        .iter()
        .map(|server| format!("• {server}"))
        .collect::<Vec<_>>()
        .join("\r\n");
    format!("{}\r\n\r\n{servers}", crate::tr!("固定 NTP 回退顺序："))
}

fn map_result(result: DialogResult) -> TimeSyncDialogIntent {
    match result {
        DialogResult::Primary => TimeSyncDialogIntent::Confirm,
        DialogResult::Secondary | DialogResult::Cancel => TimeSyncDialogIntent::Close,
    }
}

fn scale(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32
}

fn logical_height(value: i32, dpi: u32) -> i32 {
    ((i64::from(value.max(0)) * 96 + i64::from(dpi.max(1)) / 2) / i64::from(dpi.max(1))) as i32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TimeSyncContentLayout {
    fallback_height: i32,
    notice_y: i32,
    notice_height: i32,
    content_height: i32,
}

impl TimeSyncContentLayout {
    const fn calculate(fallback_height: i32, notice_height: i32, gap: i32) -> Self {
        let notice_y = fallback_height + gap;
        Self {
            fallback_height,
            notice_y,
            notice_height,
            content_height: notice_y + notice_height,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_order_matches_the_existing_backend() {
        assert_eq!(
            TRUSTED_NTP_FALLBACKS,
            [
                "ntp.aliyun.com",
                "ntp.tencent.com",
                "cn.ntp.org.cn",
                "time.windows.com",
                "pool.ntp.org",
            ]
        );
        let text = fallback_text();
        let positions: Vec<_> = TRUSTED_NTP_FALLBACKS
            .iter()
            .map(|server| text.find(server).unwrap())
            .collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn only_primary_result_confirms_time_sync() {
        assert_eq!(
            map_result(DialogResult::Primary),
            TimeSyncDialogIntent::Confirm
        );
        assert_eq!(
            map_result(DialogResult::Cancel),
            TimeSyncDialogIntent::Close
        );
        assert_eq!(
            map_result(DialogResult::Secondary),
            TimeSyncDialogIntent::Close
        );
    }

    #[test]
    fn compact_layout_has_one_standard_gap_and_no_trailing_reservation() {
        let layout = TimeSyncContentLayout::calculate(154, 40, 8);
        assert_eq!(layout.notice_y - layout.fallback_height, 8);
        assert_eq!(layout.content_height, 202);
    }
}
