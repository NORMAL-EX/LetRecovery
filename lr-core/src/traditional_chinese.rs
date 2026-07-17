//! Shared Simplified-to-Traditional Chinese conversion for both UI frontends.
//!
//! Windows already ships the maintained character mapping used by the rest of the
//! operating system.  Keeping that conversion here prevents the desktop and PE
//! frontends from drifting apart when new Simplified-Chinese source strings are added.

#[cfg(windows)]
use windows::core::w;
#[cfg(windows)]
use windows::Win32::Foundation::LPARAM;
#[cfg(windows)]
use windows::Win32::Globalization::{LCMapStringEx, LCMAP_TRADITIONAL_CHINESE};

/// Converts a Simplified-Chinese UI string to Traditional Chinese, then applies a
/// small Traditional-Chinese UI terminology glossary. ASCII placeholders and punctuation are left
/// untouched by the Windows mapping API.
pub fn to_traditional_chinese(text: &str) -> String {
    let mapped = map_characters(text).unwrap_or_else(|| text.to_string());
    apply_traditional_ui_terminology(mapped)
}

#[cfg(windows)]
fn map_characters(text: &str) -> Option<String> {
    if text.is_empty() {
        return Some(String::new());
    }

    let source: Vec<u16> = text.encode_utf16().collect();
    let required = unsafe {
        LCMapStringEx(
            w!("zh-TW"),
            LCMAP_TRADITIONAL_CHINESE,
            &source,
            None,
            None,
            None,
            LPARAM(0),
        )
    };
    if required <= 0 {
        return None;
    }

    let mut destination = vec![0_u16; required as usize];
    let written = unsafe {
        LCMapStringEx(
            w!("zh-TW"),
            LCMAP_TRADITIONAL_CHINESE,
            &source,
            Some(&mut destination),
            None,
            None,
            LPARAM(0),
        )
    };
    if written <= 0 {
        return None;
    }
    String::from_utf16(&destination[..written as usize]).ok()
}

#[cfg(not(windows))]
fn map_characters(_text: &str) -> Option<String> {
    None
}

fn apply_traditional_ui_terminology(mut text: String) -> String {
    // Longer phrases come first so the shorter replacements cannot split them.
    for (mainland, taiwan) in [
        ("文件夾", "資料夾"),
        ("驅動程序", "驅動程式"),
        ("盤符", "磁碟機代號"),
        ("軟件", "軟體"),
        ("硬盤", "硬碟"),
        ("磁盤", "磁碟"),
        ("網絡", "網路"),
        ("信息", "資訊"),
        ("文件", "檔案"),
        ("用戶", "使用者"),
        ("默認", "預設"),
        ("設置", "設定"),
        ("保存", "儲存"),
        ("加載", "載入"),
        ("程序", "程式"),
        ("內置", "內建"),
        ("分區", "分割區"),
    ] {
        text = text.replace(mainland, taiwan);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn maps_characters_and_traditional_ui_terms_without_touching_placeholders() {
        assert_eq!(
            to_traditional_chinese("系统安装：选择软件、网络、用户和分区文件 {}"),
            "系統安裝：選擇軟體、網路、使用者和分割區檔案 {}"
        );
    }

    #[test]
    fn empty_text_stays_empty() {
        assert_eq!(to_traditional_chinese(""), "");
    }
}
