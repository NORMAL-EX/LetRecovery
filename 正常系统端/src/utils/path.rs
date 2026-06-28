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

/// 用户分区脚本目录（新布局：bin/diskpart）。
///
/// 优先返回 bin/diskpart；若不存在则回退到 exe 同级的旧位置 diskpart——
/// 这样既兼容把目录挪进 bin 之前的旧包，也兼容重启进 PE 后从数据目录读取暂存脚本
/// （PE 中 exe 同级即数据目录、其下为暂存的 diskpart\）。
pub fn get_diskpart_scripts_dir() -> PathBuf {
    let in_bin = get_bin_dir().join("diskpart");
    if in_bin.exists() {
        in_bin
    } else {
        get_exe_dir().join("diskpart")
    }
}

/// 获取 uefiseven 目录路径（bin/uefiseven）
pub fn get_uefiseven_dir() -> PathBuf {
    get_bin_dir().join("uefiseven")
}

/// 获取临时目录
pub fn get_temp_dir() -> PathBuf {
    get_exe_dir().join("temp")
}
