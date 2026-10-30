//! 首次启动欢迎页：问候用户、选择使用模式（小白 / 正常 / 高级），同意用户协议后才进入主界面。
//!
//! 与程序里其它对话框一样使用 `DialogShell`：同一套标题、按钮、字体和深浅色主题，内容区的
//! 下拉框、复选框也由对话框按当前主题统一着色。用户协议不再塞进编辑框，底部的“查看《用户协议》”
//! 打开一个单独的窗口显示全文。协议版本记录在 `config.json` 的 `accepted_terms_version`，协议
//! 更新时提高 [`crate::core::app_config::CURRENT_TERMS_VERSION`] 即可让所有用户重新确认。

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, ICC_LISTVIEW_CLASSES, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX,
};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, MoveWindow, SendMessageW, WM_NCDESTROY, WS_TABSTOP, WS_VSCROLL,
};

use super::controls::{child, wide};
use super::dialog::{DialogButtons, DialogResult, DialogShell, DialogSpec};
use super::layout::LayoutMetrics;
use super::GetDpiForWindow;
use crate::core::app_config::UiMode;

const ID_MODE_LABEL: u16 = 64_100;
const ID_MODE: u16 = 64_101;
const ID_MODE_HINT: u16 = 64_102;
const ID_AGREE: u16 = 64_103;
const ID_TERMS_TEXT: u16 = 64_110;
const AGREE_SUBCLASS_ID: usize = 0x4C52_5741;

const BS_AUTOCHECKBOX: i32 = 0x0003;
const CBS_DROPDOWNLIST: i32 = 0x0003;
const ES_MULTILINE: i32 = 0x0004;
const ES_AUTOVSCROLL: i32 = 0x0040;
const ES_READONLY: i32 = 0x0800;
const BM_GETCHECK: u32 = 0x00F0;
const BM_SETCHECK: u32 = 0x00F1;
const BM_CLICK: u32 = 0x00F5;
const CB_ADDSTRING: u32 = 0x0143;
const CB_GETCURSEL: u32 = 0x0147;
const CB_SETCURSEL: u32 = 0x014E;
const WM_KEYUP: u32 = 0x0101;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_LBUTTONDBLCLK: u32 = 0x0203;

/// 这些是电脑厂商、学校机房和系统镜像常见的默认账户名，不是用户自己的名字。
const GENERIC_USER_NAMES: &[&str] = &[
    "admin",
    "administrator",
    "administration",
    "adminstrator",
    "user",
    "users",
    "asus",
    "seewo",
    "owner",
    "guest",
    "default",
    "defaultuser",
    "test",
    "pc",
    "lenovo",
    "dell",
    "hp",
    "acer",
    "msi",
    "huawei",
    "honor",
    "xiaomi",
    "mechrevo",
    "thinkpad",
    "windows",
    "win",
    "system",
    "root",
    "student",
    "teacher",
    "sysprep",
    "学生",
    "老师",
    "教师",
    "用户",
    "管理员",
    "电脑",
];

/// 账户名是不是用户自己的名字。默认账户名、机器名式的名字和纯数字都按“没有用户名”处理。
pub fn is_personal_name(name: &str) -> bool {
    let name = name.trim();
    let lower = name.to_lowercase();
    let base = lower.trim_end_matches(|c: char| c.is_ascii_digit() || c == '-' || c == '_');
    !name.is_empty()
        && name.chars().count() <= 24
        && name.chars().any(char::is_alphabetic)
        && !GENERIC_USER_NAMES.contains(&base)
        && !["desktop-", "laptop-", "win-", "pc-", "user-"]
            .iter()
            .any(|prefix| lower.starts_with(prefix))
}

pub fn greeting(user_name: Option<&str>) -> String {
    match user_name
        .map(str::trim)
        .filter(|name| is_personal_name(name))
    {
        Some(name) => crate::tr!("{}，欢迎使用 LetRecovery", name),
        None => crate::tr!("欢迎使用 LetRecovery"),
    }
}

/// 用户协议原文来自仓库根目录的 USER_TERMS.md，编译进程序，避免发布包缺文件时无法显示。
fn terms_text() -> String {
    let mut text = String::new();
    for line in include_str!("../../../USER_TERMS.md").lines() {
        let heading = line.trim_start().starts_with('#');
        let line = line.trim_start_matches('#').trim_start_matches('>').trim();
        if heading && !text.is_empty() {
            text.push_str("\r\n");
        }
        text.push_str(&line.replace("**", ""));
        text.push_str("\r\n");
    }
    text.trim_end().to_string()
}

unsafe fn init_common_controls() {
    let controls = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_LISTVIEW_CLASSES | ICC_STANDARD_CLASSES,
    };
    let _ = InitCommonControlsEx(&controls);
}

/// 显示欢迎页。`Ok(Some(mode))`：同意协议并选择了模式；`Ok(None)`：用户选择退出。
pub fn run() -> windows::core::Result<Option<UiMode>> {
    unsafe { run_dialog() }
}

unsafe fn run_dialog() -> windows::core::Result<Option<UiMode>> {
    init_common_controls();
    let user_name = std::env::var("USERNAME").ok();
    let mut shell = DialogShell::create(
        HWND::default(),
        DialogSpec {
            window_title: crate::tr!("欢迎使用 LetRecovery"),
            title: greeting(user_name.as_deref()),
            description: crate::tr!(
                "开始之前，先选一个适合你的使用模式。以后可以在“关于”页面随时更改。"
            ),
            width: 600,
            height: 400,
            buttons: DialogButtons {
                primary: crate::tr!("开始使用"),
                secondary: Some(crate::tr!("查看《用户协议》")),
                cancel: Some(crate::tr!("退出")),
            },
        },
    )?;
    // “查看《用户协议》”只打开协议窗口，不关闭欢迎页。
    shell.set_secondary_closes(false);
    let content = shell.content();
    let mode_label = child(
        content,
        w!("STATIC"),
        &crate::tr!("使用模式:"),
        0,
        ID_MODE_LABEL,
    )?;
    let mode = child(
        content,
        w!("COMBOBOX"),
        "",
        CBS_DROPDOWNLIST | WS_TABSTOP.0 as i32 | WS_VSCROLL.0 as i32,
        ID_MODE,
    )?;
    let easy_mode = lr_core::branding::feature::EASY_MODE;
    for label in [
        crate::tr!("小白模式（只保留一键重装）"),
        crate::tr!("正常模式（推荐）"),
        crate::tr!("高级模式（显示全部选项）"),
    ]
    .into_iter()
    .skip(usize::from(!easy_mode))
    {
        let text = wide(&label);
        let _ = SendMessageW(
            mode,
            CB_ADDSTRING,
            WPARAM(0),
            LPARAM(text.as_ptr() as isize),
        );
    }
    let _ = SendMessageW(
        mode,
        CB_SETCURSEL,
        WPARAM(usize::from(easy_mode)),
        LPARAM(0),
    );
    let hint = child(
        content,
        w!("STATIC"),
        &crate::tr!("小白模式只保留一键重装，步骤最少；正常模式包含常用功能，隐藏少用的高级设置；高级模式显示全部选项。"),
        0,
        ID_MODE_HINT,
    )?;
    let agree = child(
        content,
        w!("BUTTON"),
        &crate::tr!("我已阅读并同意《用户协议》"),
        BS_AUTOCHECKBOX | WS_TABSTOP.0 as i32,
        ID_AGREE,
    )?;
    // 勾选同意之前“开始使用”不可用；复选框自身的子类在状态变化后同步按钮。
    shell.set_primary_enabled(false);
    if let Some(primary) = shell.command_button(DialogResult::Primary) {
        let _ = SetWindowSubclass(
            agree,
            Some(agree_proc),
            AGREE_SUBCLASS_ID,
            primary.0 as usize,
        );
    }
    shell.fit_content_height(150);
    layout(content, mode_label, mode, hint, agree);
    loop {
        match shell.show_modal() {
            DialogResult::Primary => {
                let index = SendMessageW(mode, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
                return Ok(Some(match index {
                    index if index + isize::from(!lr_core::branding::feature::EASY_MODE) == 0 => {
                        UiMode::Beginner
                    }
                    index if index + isize::from(!lr_core::branding::feature::EASY_MODE) == 2 => {
                        UiMode::Advanced
                    }
                    _ => UiMode::Normal,
                }));
            }
            DialogResult::Secondary => {
                if let Err(error) = show_terms(shell.hwnd()) {
                    log::warn!("无法显示用户协议窗口: {error}");
                }
            }
            DialogResult::Cancel => return Ok(None),
        }
    }
}

unsafe fn layout(content: HWND, mode_label: HWND, mode: HWND, hint: HWND, agree: HWND) {
    let dpi = GetDpiForWindow(content).max(96);
    let s = |value: i32| value * dpi as i32 / 96;
    let field = LayoutMetrics::for_dpi(dpi).field_height;
    let mut rect = RECT::default();
    let _ = GetClientRect(content, &mut rect);
    let width = rect.right.max(s(320));
    let label_width = s(84);
    let _ = MoveWindow(
        mode_label,
        0,
        (field - s(20)).max(0) / 2,
        label_width,
        s(20),
        false,
    );
    let _ = MoveWindow(
        mode,
        label_width,
        0,
        (width - label_width).min(s(360)),
        s(220),
        false,
    );
    let _ = MoveWindow(hint, 0, field + s(12), width, s(42), false);
    let _ = MoveWindow(agree, 0, field + s(64), width, s(26), false);
}

unsafe extern "system" fn agree_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    primary: usize,
) -> LRESULT {
    if message == WM_NCDESTROY {
        let _ = RemoveWindowSubclass(hwnd, Some(agree_proc), AGREE_SUBCLASS_ID);
        return DefSubclassProc(hwnd, message, wparam, lparam);
    }
    let result = DefSubclassProc(hwnd, message, wparam, lparam);
    if matches!(
        message,
        WM_LBUTTONUP | WM_LBUTTONDBLCLK | WM_KEYUP | BM_CLICK | BM_SETCHECK
    ) {
        let checked = SendMessageW(hwnd, BM_GETCHECK, WPARAM(0), LPARAM(0)).0 == 1;
        let _ = EnableWindow(HWND(primary as *mut _), checked);
    }
    result
}

/// 单独的用户协议窗口：只读、可滚动，深浅色跟随系统。
unsafe fn show_terms(owner: HWND) -> windows::core::Result<()> {
    let mut viewer = DialogShell::create(
        owner,
        DialogSpec {
            window_title: crate::tr!("用户协议"),
            title: crate::tr!("LetRecovery 用户协议"),
            description: crate::tr!("请仔细阅读以下条款。"),
            width: 680,
            height: 600,
            buttons: DialogButtons {
                primary: crate::tr!("我知道了"),
                secondary: None,
                cancel: None,
            },
        },
    )?;
    let content = viewer.content();
    let text = child(
        content,
        w!("EDIT"),
        &terms_text(),
        ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL | WS_VSCROLL.0 as i32 | WS_TABSTOP.0 as i32,
        ID_TERMS_TEXT,
    )?;
    viewer.fit_content_height(400);
    let mut rect = RECT::default();
    let _ = GetClientRect(content, &mut rect);
    let _ = MoveWindow(text, 0, 0, rect.right.max(0), rect.bottom.max(0), false);
    let _ = viewer.show_modal();
    Ok(())
}

thread_local! {
    static PASSWORD_INPUT: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}
const ID_PASSWORD_INPUT: u16 = 0x6A10;
const PASSWORD_SUBCLASS_ID: usize = 0x4C52_5057;

unsafe extern "system" fn password_input_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass: usize,
    _data: usize,
) -> LRESULT {
    if message == windows::Win32::UI::WindowsAndMessaging::WM_DESTROY {
        // The dialog destroys its children when it closes: keep the typed text first.
        let length = windows::Win32::UI::WindowsAndMessaging::GetWindowTextLengthW(hwnd).max(0);
        let mut buffer = vec![0u16; length as usize + 1];
        let copied =
            windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(hwnd, &mut buffer).max(0);
        PASSWORD_INPUT.with(|cell| {
            *cell.borrow_mut() = String::from_utf16_lossy(&buffer[..copied as usize]);
        });
        let _ = RemoveWindowSubclass(hwnd, Some(password_input_proc), PASSWORD_SUBCLASS_ID);
    }
    DefSubclassProc(hwnd, message, wparam, lparam)
}

/// Usage-password prompt of commercial builds, in the same dialog UI as the main window.
/// Returns `None` when the user quits.
pub fn show_password_prompt(title: &str, heading: &str, message: &str) -> Option<String> {
    unsafe {
        init_common_controls();
        PASSWORD_INPUT.with(|cell| cell.borrow_mut().clear());
        let mut dialog = DialogShell::create(
            HWND::default(),
            DialogSpec {
                window_title: title.to_owned(),
                title: heading.to_owned(),
                description: message.to_owned(),
                width: 520,
                height: 260,
                buttons: DialogButtons {
                    primary: crate::tr!("确定"),
                    secondary: None,
                    cancel: Some(crate::tr!("退出")),
                },
            },
        )
        .ok()?;
        let content = dialog.content();
        let input = child(
            content,
            w!("EDIT"),
            "",
            windows::Win32::UI::WindowsAndMessaging::ES_PASSWORD
                | windows::Win32::UI::WindowsAndMessaging::ES_AUTOHSCROLL
                | windows::Win32::UI::WindowsAndMessaging::WS_TABSTOP.0 as i32,
            ID_PASSWORD_INPUT,
        )
        .ok()?;
        let _ = SetWindowSubclass(input, Some(password_input_proc), PASSWORD_SUBCLASS_ID, 0);
        dialog.fit_content_height(36);
        let mut rect = RECT::default();
        let _ = GetClientRect(content, &mut rect);
        let dpi = GetDpiForWindow(content).max(96) as i32;
        let height = LayoutMetrics::for_dpi(dpi as u32)
            .field_height
            .min(rect.bottom.max(0));
        let _ = MoveWindow(input, 0, 0, rect.right.max(0), height, false);
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(input);
        let accepted = matches!(dialog.show_modal(), DialogResult::Primary);
        let text = PASSWORD_INPUT.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
        accepted.then_some(text)
    }
}

/// 主窗口创建之前的错误提示（例如程序文件缺失），使用与程序一致的对话框和深浅色主题。
/// 返回 false 时由调用方退回系统消息框。
pub fn show_startup_message(title: &str, heading: &str, message: &str) -> bool {
    unsafe {
        init_common_controls();
        match DialogShell::create(
            HWND::default(),
            DialogSpec {
                window_title: title.to_owned(),
                title: heading.to_owned(),
                description: message.to_owned(),
                width: 620,
                height: 260,
                buttons: DialogButtons {
                    primary: crate::tr!("确定"),
                    secondary: None,
                    cancel: None,
                },
            },
        ) {
            Ok(mut dialog) => {
                dialog.fit_content_height(0);
                let _ = dialog.show_modal();
                true
            }
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_account_names_are_not_used_in_the_greeting() {
        for name in [
            "admin",
            "Administrator",
            "administration",
            "USER",
            "user01",
            "asus",
            "seewo",
            "DESKTOP-8F2K1Q",
            "123",
            "",
        ] {
            assert!(!is_personal_name(name), "{name}");
        }
        for name in ["小明", "Alice", "zhang_san"] {
            assert!(is_personal_name(name), "{name}");
        }
    }
}
