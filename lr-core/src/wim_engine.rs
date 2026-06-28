//! WIM 引擎选择与统一封装（libwim / wimgapi 运行时切换 + 失败回退）。
//!
//! - [`WimEngine`]：引擎枚举（默认 libwim）。
//! - 进程级全局选择：[`set_active_engine`] / [`active_engine`]。两端在读取配置后设置，
//!   之后所有镜像操作都按该选择执行；正常系统端从 `config.json` 设置，PE 端从随重启
//!   传递过来的安装/备份配置设置，从而做到“切到 wimgapi 后 PE 端也用 wimgapi”。
//! - [`WimEngineManager`]：对外暴露与 [`crate::wimlib::WimlibManager`] 同形的
//!   `apply_image` / `capture_image`。当选择 wimgapi 时优先用 wimgapi；若 wimgapi
//!   **加载/初始化失败**或**操作失败**，自动回退到 libwim，保证功能始终可用。

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::Sender;

use crate::image_meta::WimProgress;
use crate::wimgapi::WimgapiManager;
use crate::wimlib::WimlibManager;

/// 镜像引擎
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WimEngine {
    /// 内置 libwim（libwim-15.dll）——默认，跨环境一致。
    Libwim,
    /// 系统 wimgapi（wimgapi.dll）——可选，Windows 原生 API。
    Wimgapi,
}

impl Default for WimEngine {
    fn default() -> Self {
        WimEngine::Libwim
    }
}

impl WimEngine {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => WimEngine::Wimgapi,
            _ => WimEngine::Libwim,
        }
    }
    pub fn as_u8(self) -> u8 {
        match self {
            WimEngine::Libwim => 0,
            WimEngine::Wimgapi => 1,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            WimEngine::Libwim => "libwim",
            WimEngine::Wimgapi => "wimgapi",
        }
    }
}

/// 进程级当前引擎（0=libwim，1=wimgapi）。
static ACTIVE_ENGINE: AtomicU8 = AtomicU8::new(0);

/// 设置当前进程的镜像引擎选择。
pub fn set_active_engine(engine: WimEngine) {
    ACTIVE_ENGINE.store(engine.as_u8(), Ordering::SeqCst);
    log::info!("WIM 引擎已设置为：{}", engine.name());
}

/// 读取当前进程的镜像引擎选择。
pub fn active_engine() -> WimEngine {
    WimEngine::from_u8(ACTIVE_ENGINE.load(Ordering::SeqCst))
}

/// 统一镜像管理器：按所选引擎执行，必要时回退 libwim。
pub struct WimEngineManager {
    /// 实际生效的引擎（可能因 wimgapi 不可用而回退为 libwim）。
    active: WimEngine,
    /// libwim 始终初始化，作为主引擎或回退引擎。
    libwim: WimlibManager,
    /// 仅在选择并成功初始化 wimgapi 时存在。
    wimgapi: Option<WimgapiManager>,
}

impl WimEngineManager {
    /// 按指定引擎构造。libwim 始终初始化；若指定 wimgapi 但其初始化失败，则回退 libwim。
    pub fn new(engine: WimEngine) -> Result<Self, String> {
        let libwim = WimlibManager::new()
            .map_err(|e| format!("libwim 初始化失败: {}", e))?;

        let mut active = WimEngine::Libwim;
        let mut wimgapi = None;

        if engine == WimEngine::Wimgapi {
            match WimgapiManager::new() {
                Ok(w) => {
                    wimgapi = Some(w);
                    active = WimEngine::Wimgapi;
                    log::info!("WIM 引擎：wimgapi 已就绪");
                }
                Err(e) => {
                    log::warn!("wimgapi 初始化失败，自动回退 libwim：{}", e);
                }
            }
        }

        Ok(Self {
            active,
            libwim,
            wimgapi,
        })
    }

    /// 按当前进程全局选择的引擎构造。
    pub fn new_current() -> Result<Self, String> {
        Self::new(active_engine())
    }

    /// 实际生效的引擎。
    pub fn active_engine(&self) -> WimEngine {
        self.active
    }

    /// 只读：判断镜像某卷是否包含任意给定路径（始终走 libwim，仅读元数据、不挂载）。
    /// 用于廉价探测内置应答文件等。与 apply/capture 的引擎选择无关。
    pub fn image_contains_any_path(
        &self,
        image_file: &str,
        index: u32,
        paths: &[&str],
    ) -> Result<bool, String> {
        self.libwim.image_contains_any_path(image_file, index, paths)
    }

    /// 应用/释放镜像；wimgapi 失败时回退 libwim。
    pub fn apply_image(
        &self,
        image_file: &str,
        target_dir: &str,
        index: u32,
        progress_tx: Option<Sender<WimProgress>>,
    ) -> Result<(), String> {
        if self.active == WimEngine::Wimgapi {
            if let Some(w) = &self.wimgapi {
                match w.apply_image(image_file, target_dir, index, progress_tx.clone()) {
                    Ok(()) => return Ok(()),
                    Err(e) => {
                        log::warn!("wimgapi 应用镜像失败，回退 libwim：{}", e);
                    }
                }
            }
        }
        self.libwim
            .apply_image(image_file, target_dir, index, progress_tx)
    }

    /// 捕获/备份镜像；wimgapi 失败时回退 libwim（回退前清理 wimgapi 产生的半成品文件）。
    pub fn capture_image(
        &self,
        source_dir: &str,
        image_file: &str,
        name: &str,
        description: &str,
        compression: u32,
        progress_tx: Option<Sender<WimProgress>>,
    ) -> Result<(), String> {
        let existed_before = std::path::Path::new(image_file).exists();

        if self.active == WimEngine::Wimgapi {
            if let Some(w) = &self.wimgapi {
                match w.capture_image(
                    source_dir,
                    image_file,
                    name,
                    description,
                    compression,
                    progress_tx.clone(),
                ) {
                    Ok(()) => return Ok(()),
                    Err(e) => {
                        log::warn!("wimgapi 捕获镜像失败，回退 libwim：{}", e);
                        // 清理 wimgapi 失败时可能留下的半成品，避免 libwim 误当作追加。
                        if !existed_before && std::path::Path::new(image_file).exists() {
                            let _ = std::fs::remove_file(image_file);
                        }
                    }
                }
            }
        }
        self.libwim.capture_image(
            source_dir,
            image_file,
            name,
            description,
            compression,
            progress_tx,
        )
    }
}
