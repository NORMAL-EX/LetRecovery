//! Reversible desktop side effects for the built-in DPRK language easter egg.
//!
//! The image is embedded in the executable. The first activation records the current wallpaper
//! path under LocalAppData before publishing the embedded image. Selecting any other language
//! restores that recorded path. A packaged MP3 is played in a stoppable loop through Windows MCI
//! while the language is active. WinPE deliberately does not call this module.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::ffi::{c_void, OsStr};
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::Media::Multimedia::{
    mciGetErrorStringW, mciSendStringW, MCIERR_INVALID_DEVICE_ID, MCIERR_INVALID_DEVICE_NAME,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE, SPI_GETDESKWALLPAPER,
    SPI_SETDESKWALLPAPER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

const WALLPAPER_BYTES: &[u8] = include_bytes!("../../assets/easter_egg/dprk_wallpaper.jpg");
const WALLPAPER_FILE_NAME: &str = "dprk-easter-egg-wallpaper.jpg";
const BACKUP_FILE_NAME: &str = "dprk-easter-egg-wallpaper-backup.json";
const AUDIO_FILE_NAME: &str = "dprk_easter_egg.mp3";
const AUDIO_ALIAS: &str = "letrecovery_dprk_easter_egg";
const MINIMUM_AUDIO_BYTES: u64 = 64 * 1024;

static AUDIO_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Serialize, Deserialize)]
struct WallpaperBackup {
    previous_wallpaper: String,
}

pub fn sync_for_language(language_code: &str) -> Result<()> {
    if crate::utils::i18n::is_dprk_easter_egg_language(language_code) {
        combine_results(enable(), start_audio())
    } else {
        combine_results(stop_audio(), restore())
    }
}

/// Stops the process-local player without changing the persisted wallpaper selection.
///
/// Window teardown calls this even when the message loop returns an error so the MCI alias never
/// keeps the packaged MP3 open after the normal-system client exits.
pub fn shutdown() {
    if let Err(error) = stop_audio() {
        log::warn!("停止朝鲜文彩蛋音频失败: {error:#}");
    }
}

fn combine_results(first: Result<()>, second: Result<()>) -> Result<()> {
    match (first, second) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(first), Err(second)) => Err(anyhow!("{first:#}; {second:#}")),
    }
}

fn audio_path() -> PathBuf {
    crate::utils::path::get_bin_dir().join(AUDIO_FILE_NAME)
}

fn audio_guard() -> MutexGuard<'static, ()> {
    AUDIO_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn start_audio() -> Result<()> {
    let _guard = audio_guard();
    let path = audio_path();
    let metadata = std::fs::symlink_metadata(&path)
        .with_context(|| format!("读取彩蛋音频失败: {}", path.display()))?;
    if !metadata.file_type().is_file() {
        return Err(anyhow!("彩蛋音频不是普通文件: {}", path.display()));
    }
    if metadata.len() < MINIMUM_AUDIO_BYTES {
        return Err(anyhow!(
            "彩蛋音频大小异常（{} 字节）: {}",
            metadata.len(),
            path.display()
        ));
    }

    let path_text = path.to_string_lossy();
    if path_text.contains('"') {
        return Err(anyhow!("彩蛋音频路径包含 MCI 不支持的引号"));
    }

    stop_audio_locked()?;
    send_mci(
        &format!("open \"{path_text}\" type mpegvideo alias {AUDIO_ALIAS}"),
        "打开彩蛋音频",
    )?;
    if let Err(error) = send_mci(&format!("play {AUDIO_ALIAS} repeat"), "循环播放彩蛋音频")
    {
        let _ = send_mci_code(&format!("close {AUDIO_ALIAS}"));
        return Err(error);
    }
    Ok(())
}

fn stop_audio() -> Result<()> {
    let _guard = audio_guard();
    stop_audio_locked()
}

fn stop_audio_locked() -> Result<()> {
    let code = send_mci_code(&format!("close {AUDIO_ALIAS}"));
    if code == 0 || is_missing_alias_error(code) {
        Ok(())
    } else {
        Err(mci_error("关闭彩蛋音频", code))
    }
}

fn is_missing_alias_error(code: u32) -> bool {
    code == MCIERR_INVALID_DEVICE_ID || code == MCIERR_INVALID_DEVICE_NAME
}

fn send_mci(command: &str, operation: &str) -> Result<()> {
    let code = send_mci_code(command);
    if code == 0 {
        Ok(())
    } else {
        Err(mci_error(operation, code))
    }
}

fn send_mci_code(command: &str) -> u32 {
    let wide = OsStr::new(command)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    unsafe { mciSendStringW(PCWSTR(wide.as_ptr()), None, HWND::default()) }
}

fn mci_error(operation: &str, code: u32) -> anyhow::Error {
    let mut buffer = [0_u16; 512];
    let message = unsafe {
        if mciGetErrorStringW(code, &mut buffer).as_bool() {
            let length = buffer
                .iter()
                .position(|character| *character == 0)
                .unwrap_or(buffer.len());
            String::from_utf16_lossy(&buffer[..length])
        } else {
            String::from("无法取得 Windows MCI 错误说明")
        }
    };
    anyhow!("{operation}失败（MCI {code}）: {message}")
}

fn state_directory() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("LetRecovery")
        .join("easter-eggs")
}

fn enable() -> Result<()> {
    let directory = state_directory();
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("创建彩蛋资源目录失败: {}", directory.display()))?;

    let wallpaper_path = directory.join(WALLPAPER_FILE_NAME);
    if std::fs::read(&wallpaper_path).ok().as_deref() != Some(WALLPAPER_BYTES) {
        write_atomic(&wallpaper_path, "dprk-wallpaper", "jpg", WALLPAPER_BYTES)?;
    }

    let backup_path = directory.join(BACKUP_FILE_NAME);
    if backup_path.exists() {
        read_backup(&backup_path)
            .with_context(|| format!("现有壁纸备份无效，拒绝覆盖: {}", backup_path.display()))?;
    } else {
        let backup = WallpaperBackup {
            previous_wallpaper: current_wallpaper().context("读取当前桌面壁纸失败")?,
        };
        let content = serde_json::to_vec_pretty(&backup).context("序列化壁纸备份失败")?;
        write_atomic(&backup_path, "dprk-wallpaper-backup", "json", &content)?;
    }

    set_wallpaper(&wallpaper_path).context("设置朝鲜文彩蛋桌面壁纸失败")
}

fn restore() -> Result<()> {
    let backup_path = state_directory().join(BACKUP_FILE_NAME);
    if !backup_path.exists() {
        return Ok(());
    }

    let backup = read_backup(&backup_path)?;
    set_wallpaper(Path::new(&backup.previous_wallpaper)).context("恢复彩蛋前桌面壁纸失败")?;
    std::fs::remove_file(&backup_path)
        .with_context(|| format!("删除已恢复的壁纸备份失败: {}", backup_path.display()))?;
    Ok(())
}

fn read_backup(path: &Path) -> Result<WallpaperBackup> {
    let content =
        std::fs::read(path).with_context(|| format!("读取壁纸备份失败: {}", path.display()))?;
    serde_json::from_slice(&content)
        .with_context(|| format!("解析壁纸备份失败: {}", path.display()))
}

fn write_atomic(path: &Path, prefix: &str, extension: &str, content: &[u8]) -> Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("目标路径没有父目录: {}", path.display()))?;
    let (temporary, mut file) =
        lr_core::scoped_temp_file::ScopedTempFile::create_writer_in(directory, prefix, extension)
            .with_context(|| format!("创建临时文件失败: {}", directory.display()))?;
    file.write_all(content)
        .with_context(|| format!("写入临时文件失败: {}", temporary.path().display()))?;
    file.flush()
        .with_context(|| format!("刷新临时文件失败: {}", temporary.path().display()))?;
    file.sync_all()
        .with_context(|| format!("同步临时文件失败: {}", temporary.path().display()))?;
    drop(file);
    temporary
        .persist_replace(path)
        .with_context(|| format!("原子发布文件失败: {}", path.display()))
}

fn current_wallpaper() -> Result<String> {
    const BUFFER_LENGTH: usize = 32_768;
    let mut buffer = vec![0_u16; BUFFER_LENGTH];
    unsafe {
        SystemParametersInfoW(
            SPI_GETDESKWALLPAPER,
            BUFFER_LENGTH as u32,
            Some(buffer.as_mut_ptr().cast::<c_void>()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .context("SystemParametersInfoW(SPI_GETDESKWALLPAPER) 失败")?;
    }
    let length = buffer
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(buffer.len());
    Ok(String::from_utf16_lossy(&buffer[..length]))
}

fn set_wallpaper(path: &Path) -> Result<()> {
    let mut wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    unsafe {
        SystemParametersInfoW(
            SPI_SETDESKWALLPAPER,
            0,
            Some(wide.as_mut_ptr().cast::<c_void>()),
            SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
        )
        .context("SystemParametersInfoW(SPI_SETDESKWALLPAPER) 失败")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_wallpaper_is_a_nonempty_jpeg() {
        assert!(WALLPAPER_BYTES.len() > 100_000);
        assert_eq!(&WALLPAPER_BYTES[..2], &[0xff, 0xd8]);
        assert_eq!(&WALLPAPER_BYTES[WALLPAPER_BYTES.len() - 2..], &[0xff, 0xd9]);
    }

    #[test]
    fn normal_languages_do_not_enable_the_easter_egg() {
        assert!(!crate::utils::i18n::is_dprk_easter_egg_language("ko-KR"));
        assert!(crate::utils::i18n::is_dprk_easter_egg_language("KO-kp"));
    }

    #[test]
    fn audio_is_loaded_from_the_normal_client_bin_directory() {
        assert_eq!(
            audio_path().file_name().and_then(|name| name.to_str()),
            Some(AUDIO_FILE_NAME)
        );
    }

    #[test]
    fn ignored_close_errors_are_limited_to_missing_mci_aliases() {
        assert!(is_missing_alias_error(MCIERR_INVALID_DEVICE_ID));
        assert!(is_missing_alias_error(MCIERR_INVALID_DEVICE_NAME));
        assert!(!is_missing_alias_error(0));
    }
}
