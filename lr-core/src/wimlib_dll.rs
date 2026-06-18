//! wimlib DLL 兜底：内置 libwim-15.dll，运行时确保其在 exe 同目录可用。
//!
//! 背景：迁移到 wimlib 后，镜像操作依赖 `libwim-15.dll`。PE 环境默认**不含**该 DLL
//! （旧版用的 wimgapi.dll 才是 PE 自带的）。若 PE 打包未带上该 DLL，会导致备份/安装
//! 等所有镜像操作在加载阶段失败。这里把 DLL 编译进二进制，加载前自动释放到 exe 目录，
//! 从根本上消除"PE 缺 DLL"的故障。

/// 编译期嵌入的 libwim-15.dll
static EMBEDDED_WIMLIB_DLL: &[u8] = include_bytes!("../vendor/libwim-15.dll");

/// 确保 libwim-15.dll 在可执行文件同目录存在；不存在则从嵌入数据释放。幂等。
pub fn ensure_dll_available() {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let dst = dir.join("libwim-15.dll");
            if !dst.exists() {
                match std::fs::write(&dst, EMBEDDED_WIMLIB_DLL) {
                    Ok(_) => log::info!("已释放内置 libwim-15.dll 到 {}", dst.display()),
                    Err(e) => log::warn!("释放内置 libwim-15.dll 失败 {}: {}", dst.display(), e),
                }
            }
        }
    }
}
