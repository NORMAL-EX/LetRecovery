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