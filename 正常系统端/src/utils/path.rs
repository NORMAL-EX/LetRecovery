use std::path::PathBuf;

/// 获取程序所在目录
pub fn get_exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 获取 bin 目录路径
pub fn get_bin_dir() -> PathBuf {
    get_exe_dir().join("bin")
}

/// 获取 PE 目录路径（统一放在 bin/pe，注意小写）
pub fn get_pe_dir() -> PathBuf {
    get_bin_dir().join("pe")
}

/// 获取 tools 目录路径
///
/// 工具类原来各自一个文件夹（如 tools\SpaceSniffer.exe），现已直接平铺到 bin 根目录。
pub fn get_tools_dir() -> PathBuf {
    get_bin_dir()
}

/// 获取 drivers 目录路径（bin/drivers）
pub fn get_drivers_dir() -> PathBuf {
    get_bin_dir().join("drivers")
}

/// 获取 uefiseven 目录路径（bin/uefiseven）
pub fn get_uefiseven_dir() -> PathBuf {
    get_bin_dir().join("uefiseven")
}

/// 获取临时目录
pub fn get_temp_dir() -> PathBuf {
    get_exe_dir().join("temp")
}
