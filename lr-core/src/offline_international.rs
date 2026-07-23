//! Read and validate international defaults from an applied offline Windows installation.
//!
//! This is the deterministic fallback for environments whose DISM international provider
//! cannot be loaded. It only queries temporary mounts of the target SYSTEM and DEFAULT hives.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{bail, Context, Result};

use crate::registry::OfflineRegistry;

#[cfg(windows)]
use windows::Win32::Globalization::LCIDToLocaleName;

static OFFLINE_INTL_HIVE_SEQUENCE: AtomicU32 = AtomicU32::new(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfflineInternationalSettings {
    pub ui_language: String,
    pub system_locale: String,
    pub user_locale: String,
    pub input_locale: String,
    pub time_zone: String,
}

fn valid_locale_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 35
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
}

fn valid_input_locale(value: &str) -> bool {
    let Some((language, keyboard)) = value.split_once(':') else {
        return false;
    };
    language.len() == 4
        && keyboard.len() == 8
        && language
            .chars()
            .all(|character| character.is_ascii_hexdigit())
        && keyboard
            .chars()
            .all(|character| character.is_ascii_hexdigit())
}

fn valid_time_zone(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value
            .chars()
            .any(|character| matches!(character, '<' | '>'))
}

fn locale_id_from_registry(value: &str) -> Result<u32> {
    let normalized = value
        .trim()
        .strip_prefix("0x")
        .or_else(|| value.trim().strip_prefix("0X"))
        .unwrap_or_else(|| value.trim());
    if normalized.is_empty()
        || normalized.len() > 8
        || !normalized
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        bail!("无效的十六进制区域标识: {value}");
    }
    u32::from_str_radix(normalized, 16).with_context(|| format!("无法解析区域标识: {value}"))
}

#[cfg(windows)]
fn locale_name_from_registry_id(value: &str) -> Result<String> {
    let locale_id = locale_id_from_registry(value)?;
    let mut buffer = [0u16; 85];
    let length = unsafe { LCIDToLocaleName(locale_id, Some(&mut buffer), 0) };
    if length == 0 {
        bail!("Windows 无法把区域标识 {value} 转换为区域名称");
    }
    let locale_name = String::from_utf16(&buffer[..length.saturating_sub(1) as usize])
        .context("Windows 返回了无效的 UTF-16 区域名称")?;
    if !valid_locale_name(&locale_name) {
        bail!("Windows 返回了无效的区域名称: {locale_name}");
    }
    Ok(locale_name)
}

#[cfg(not(windows))]
fn locale_name_from_registry_id(value: &str) -> Result<String> {
    let _ = locale_id_from_registry(value)?;
    bail!("离线 Windows 区域标识转换只能在 Windows 上执行")
}

fn input_locale_from_keyboard_layout(value: &str) -> Result<String> {
    let keyboard_layout = value.trim();
    if keyboard_layout.len() != 8
        || !keyboard_layout
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        bail!("无效的默认键盘布局: {value}");
    }
    let language_id = &keyboard_layout[4..];
    let input_locale = format!("{language_id}:{keyboard_layout}");
    if !valid_input_locale(&input_locale) {
        bail!("无法从默认键盘布局构造输入区域: {value}");
    }
    Ok(input_locale)
}

struct LoadedOfflineHive {
    name: Option<String>,
}

impl LoadedOfflineHive {
    fn load(name: String, hive_file: &Path) -> Result<Self> {
        let hive_file = hive_file.to_str().ok_or_else(|| {
            anyhow::anyhow!("离线注册表路径不是有效的 Unicode: {}", hive_file.display())
        })?;
        OfflineRegistry::load_hive(&name, hive_file)
            .with_context(|| format!("加载离线注册表配置单元失败: {hive_file}"))?;
        Ok(Self { name: Some(name) })
    }

    fn key(&self, relative_path: &str) -> String {
        format!(
            "HKLM\\{}\\{}",
            self.name.as_deref().expect("loaded hive name is present"),
            relative_path
        )
    }

    fn unload(mut self) -> Result<()> {
        let name = self.name.take().expect("loaded hive name is present");
        OfflineRegistry::unload_hive(&name)
            .with_context(|| format!("failed to unload offline registry hive {name}"))
    }
}

impl Drop for LoadedOfflineHive {
    fn drop(&mut self) {
        let Some(name) = self.name.take() else {
            return;
        };
        if let Err(error) = OfflineRegistry::unload_hive(&name) {
            log::error!("卸载国际化探测注册表配置单元失败 [{}]: {:#}", name, error);
        }
    }
}

/// Reads the applied image's installation language, locales, default keyboard and time zone.
///
/// `image_path` must be a drive designator such as `C:`. Every required value is validated;
/// incomplete or malformed hives fail closed instead of silently substituting the host locale.
pub fn read_offline_international_settings(
    image_path: &str,
) -> Result<OfflineInternationalSettings> {
    let image_root = image_path.trim_end_matches(['\\', '/']);
    if image_root.len() != 2
        || !image_root.as_bytes()[0].is_ascii_alphabetic()
        || image_root.as_bytes()[1] != b':'
    {
        bail!("离线系统根目录必须是盘符: {image_path}");
    }

    let config_dir = PathBuf::from(format!(r"{}\Windows\System32\config", image_root));
    let system_hive_path = config_dir.join("SYSTEM");
    let default_hive_path = config_dir.join("DEFAULT");
    if !system_hive_path.is_file() || !default_hive_path.is_file() {
        bail!(
            "目标系统缺少国际化探测所需的 SYSTEM 或 DEFAULT 注册表配置单元: {}",
            config_dir.display()
        );
    }

    let sequence = OFFLINE_INTL_HIVE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let prefix = format!("lr-intl-{}-{sequence}", std::process::id());
    let system_hive = LoadedOfflineHive::load(format!("{prefix}-system"), &system_hive_path)?;
    let default_hive = LoadedOfflineHive::load(format!("{prefix}-default"), &default_hive_path)?;

    let select_key = system_hive.key("Select");
    let control_set =
        OfflineRegistry::query_dword(&select_key, "Current").or_else(|current_error| {
            OfflineRegistry::query_dword(&select_key, "Default").with_context(|| {
                format!("读取活动控制集 Current 失败 ({current_error:#})，且 Default 回退也失败")
            })
        })?;
    if !(1..=999).contains(&control_set) {
        bail!("离线 SYSTEM 注册表返回了无效的控制集编号: {control_set}");
    }
    let control_root = format!("ControlSet{control_set:03}\\Control");

    let language_key = system_hive.key(&format!(r"{control_root}\Nls\Language"));
    let install_language = OfflineRegistry::query_string(&language_key, "InstallLanguage")
        .context("读取目标系统安装语言失败")?;
    let system_language = OfflineRegistry::query_string(&language_key, "Default")
        .context("读取目标系统区域设置失败")?;
    let ui_language =
        locale_name_from_registry_id(&install_language).context("转换目标系统安装语言失败")?;
    let system_locale =
        locale_name_from_registry_id(&system_language).context("转换目标系统区域设置失败")?;

    let international_key = default_hive.key(r"Control Panel\International");
    let user_locale = OfflineRegistry::query_string(&international_key, "LocaleName")
        .context("读取目标系统默认用户区域设置失败")?;
    if !valid_locale_name(&user_locale) {
        bail!("离线 DEFAULT 注册表返回了无效的用户区域设置: {user_locale}");
    }

    let keyboard_key = default_hive.key(r"Keyboard Layout\Preload");
    let keyboard_layout = OfflineRegistry::query_string(&keyboard_key, "1")
        .context("读取目标系统默认键盘布局失败")?;
    let input_locale = input_locale_from_keyboard_layout(&keyboard_layout)?;

    let time_zone_key = system_hive.key(&format!(r"{control_root}\TimeZoneInformation"));
    let time_zone = OfflineRegistry::query_string(&time_zone_key, "TimeZoneKeyName")
        .context("读取目标系统默认时区失败")?;
    if !valid_time_zone(&time_zone) {
        bail!("离线 SYSTEM 注册表返回了无效的默认时区: {time_zone}");
    }

    let settings = OfflineInternationalSettings {
        ui_language,
        system_locale,
        user_locale,
        input_locale,
        time_zone,
    };
    default_hive.unload()?;
    system_hive.unload()?;
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_registry_lcids_and_keyboard_layouts() {
        assert_eq!(locale_id_from_registry("0804").unwrap(), 0x0804);
        assert_eq!(locale_id_from_registry("0x0409").unwrap(), 0x0409);
        assert_eq!(
            input_locale_from_keyboard_layout("00000804").unwrap(),
            "0804:00000804"
        );
        assert_eq!(
            input_locale_from_keyboard_layout("d0010409").unwrap(),
            "0409:d0010409"
        );
    }

    #[cfg(windows)]
    #[test]
    fn converts_standard_windows_lcids_to_locale_names() {
        assert_eq!(locale_name_from_registry_id("0804").unwrap(), "zh-CN");
        assert_eq!(locale_name_from_registry_id("0409").unwrap(), "en-US");
    }

    #[test]
    fn rejects_invalid_registry_international_values() {
        assert!(locale_id_from_registry("not-a-lcid").is_err());
        assert!(input_locale_from_keyboard_layout("804").is_err());
        assert!(input_locale_from_keyboard_layout("0000080Z").is_err());
    }
}
