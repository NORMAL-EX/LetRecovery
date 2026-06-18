//! 命令行无人值守安装（正常系统端）。
//!
//! 从桌面一键驱动一次无人值守重装，复用 GUI 的「PE 安装准备」链路：
//! 读取安装配置 JSON（`--config`）+ 高级选项 JSON（`--advanced`）→ 把镜像放进数据分区
//! → 写安装配置（INI）+ 目标盘标记 → 设置下次重启进 PE → 重启；进 PE 后由 PE 端读取
//! 配置完成实际部署（格式化、释放镜像、导入驱动、应用高级选项、修引导）。
//!
//! 用法：
//! ```text
//! LetRecovery.exe --install --config <install.json> [--advanced <advanced.json>]
//! ```
//!
//! 高级选项映射与 GUI 的 PE 安装路径完全一致（取 AdvancedOptions 的同一子集写入
//! InstallConfig）；脚本/自定义文件/WiFi 等更丰富的选项不属于 PE 安装流程，故此处亦不涉及。
//!
//! 注意：整条流程依赖真实重装环境（PE 启动 + 重启 + 部署），需真机回归。

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::core::disk::DiskManager;
use crate::core::install_config::{ConfigFileManager, InstallConfig};
use crate::core::pe::PeManager;
use crate::ui::advanced_options::AdvancedOptions;

fn default_volume_index() -> u32 {
    1
}
fn default_true() -> bool {
    true
}

/// 命令行安装配置（`--config` 指向的 JSON）。
#[derive(Debug, Deserialize)]
struct CliInstallSpec {
    /// 目标分区盘符（要重装的系统盘，如 "C:"）——进 PE 后会被格式化。
    target_partition: String,
    /// 镜像绝对路径（.wim/.esd/.swm 或 .gho/.ghs）。
    image_path: String,
    /// PE 启动文件绝对路径（.wim 或 .iso）。
    pe_path: String,

    /// 镜像内分卷索引（WIM/ESD 多版本时选择；默认 1）。
    #[serde(default = "default_volume_index")]
    volume_index: u32,
    /// 是否 GHO；缺省按扩展名自动判断。
    #[serde(default)]
    is_gho: Option<bool>,
    /// 驱动处理：0=不处理 1=仅备份 2=自动导入（默认 0）。
    #[serde(default)]
    driver_action_mode: u8,
    /// 是否生成无人值守配置。
    #[serde(default)]
    unattended: bool,
    /// 安装准备完成后是否自动重启进 PE（默认 true）。
    #[serde(default = "default_true")]
    auto_reboot: bool,
    /// 自定义无人值守 XML 绝对路径（可选）。
    #[serde(default)]
    custom_unattend_path: String,
    /// 暂存配置/镜像的数据分区盘符（可选；缺省自动选一个空间足够、非目标盘的分区）。
    #[serde(default)]
    data_partition: Option<String>,
    /// PE 启动项显示名（可选）。
    #[serde(default)]
    pe_display_name: Option<String>,
}

/// 入口：`--install --config <json> [--advanced <json>]`。
pub fn run_cli_install(config_path: &str, advanced_path: Option<&str>) -> Result<()> {
    println!("[CLI INSTALL] ========== 命令行无人值守安装 ==========");

    // 1) 安装配置
    let spec: CliInstallSpec = {
        let text = std::fs::read_to_string(config_path)
            .with_context(|| format!("读取安装配置失败: {}", config_path))?;
        serde_json::from_str(&text)
            .with_context(|| format!("解析安装配置 JSON 失败: {}", config_path))?
    };

    // 2) 高级选项（可选）
    let advanced: AdvancedOptions = match advanced_path {
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .with_context(|| format!("读取高级选项失败: {}", p))?;
            serde_json::from_str(&text)
                .with_context(|| format!("解析高级选项 JSON 失败: {}", p))?
        }
        None => AdvancedOptions::default(),
    };

    // 3) 校验
    if spec.target_partition.trim().is_empty() {
        return Err(anyhow!("target_partition 不能为空"));
    }
    if !std::path::Path::new(&spec.image_path).exists() {
        return Err(anyhow!("镜像文件不存在: {}", spec.image_path));
    }
    if !std::path::Path::new(&spec.pe_path).exists() {
        return Err(anyhow!("PE 启动文件不存在: {}", spec.pe_path));
    }

    let is_gho = spec.is_gho.unwrap_or_else(|| {
        let l = spec.image_path.to_lowercase();
        l.ends_with(".gho") || l.ends_with(".ghs")
    });

    println!("[CLI INSTALL] 目标分区: {}", spec.target_partition);
    println!("[CLI INSTALL] 镜像: {}", spec.image_path);
    println!("[CLI INSTALL] PE: {}", spec.pe_path);

    // 4) 数据分区（暂存配置 + 镜像）
    let image_size = std::fs::metadata(&spec.image_path).map(|m| m.len()).unwrap_or(0);
    let data_partition = match &spec.data_partition {
        Some(p) => p.clone(),
        None => match DiskManager::find_suitable_data_partition(&spec.target_partition, image_size) {
            Ok(Some((p, _auto))) => p,
            Ok(None) => {
                return Err(anyhow!(
                    "未找到空间足够的数据分区来暂存镜像（需约 {:.2} GB）",
                    image_size as f64 / 1024.0 / 1024.0 / 1024.0
                ))
            }
            Err(e) => return Err(anyhow!("查找数据分区失败: {}", e)),
        },
    };
    println!("[CLI INSTALL] 数据分区: {}", data_partition);

    // 5) 把镜像放进数据目录（InstallConfig.image_path 存相对文件名）
    let data_dir = ConfigFileManager::get_data_dir(&data_partition);
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("创建数据目录失败: {}", data_dir))?;
    let image_filename = std::path::Path::new(&spec.image_path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .ok_or_else(|| anyhow!("无法取得镜像文件名: {}", spec.image_path))?;
    let staged_image = format!("{}\\{}", data_dir, image_filename);
    if same_file(&staged_image, &spec.image_path) {
        println!("[CLI INSTALL] 镜像已在数据目录，跳过拷贝");
    } else {
        println!(
            "[CLI INSTALL] 拷贝镜像到数据目录: {} -> {}",
            spec.image_path, staged_image
        );
        std::fs::copy(&spec.image_path, &staged_image)
            .with_context(|| format!("拷贝镜像失败: {} -> {}", spec.image_path, staged_image))?;
    }

    // 6) 构造 InstallConfig：基础项 + 高级选项子集（映射与 install_progress.rs 的 PE 路径一致）
    let install_config = InstallConfig {
        unattended: spec.unattended,
        restore_drivers: false, // 由 driver_action_mode 主导
        driver_action_mode: spec.driver_action_mode,
        auto_reboot: spec.auto_reboot,
        original_guid: String::new(),
        volume_index: spec.volume_index,
        target_partition: spec.target_partition.clone(),
        image_path: image_filename,
        is_gho,
        remove_shortcut_arrow: advanced.remove_shortcut_arrow,
        restore_classic_context_menu: advanced.restore_classic_context_menu,
        bypass_nro: advanced.bypass_nro,
        disable_windows_update: advanced.disable_windows_update,
        disable_windows_defender: advanced.disable_windows_defender,
        disable_reserved_storage: advanced.disable_reserved_storage,
        disable_uac: advanced.disable_uac,
        disable_device_encryption: advanced.disable_device_encryption,
        remove_uwp_apps: advanced.remove_uwp_apps,
        import_storage_controller_drivers: advanced.import_storage_controller_drivers,
        custom_username: if advanced.custom_username {
            advanced.username.clone()
        } else {
            String::new()
        },
        volume_label: if advanced.custom_volume_label {
            advanced.volume_label.clone()
        } else {
            String::new()
        },
        custom_unattend_path: spec.custom_unattend_path.clone(),
        win7_uefi_patch: advanced.win7_uefi_patch,
        win7_inject_usb3_driver: advanced.win7_inject_usb3_driver,
        win7_inject_nvme_driver: advanced.win7_inject_nvme_driver,
        win7_fix_acpi_bsod: advanced.win7_fix_acpi_bsod,
        win7_fix_storage_bsod: advanced.win7_fix_storage_bsod,
    };

    // 7) 写安装配置（含目标盘标记；自定义无人值守 XML 会被复制进数据目录）
    ConfigFileManager::write_install_config(&spec.target_partition, &data_partition, &install_config)
        .map_err(|e| anyhow!("写入安装配置失败: {}", e))?;

    // 8) 设置下次重启进 PE
    let display_name = spec
        .pe_display_name
        .clone()
        .unwrap_or_else(|| "LetRecovery PE".to_string());
    PeManager::new()
        .boot_to_pe(&spec.pe_path, &display_name)
        .map_err(|e| anyhow!("设置 PE 引导失败: {}", e))?;

    println!("[CLI INSTALL] 准备完成。");

    // 9) 重启（如启用）
    if spec.auto_reboot {
        println!("[CLI INSTALL] 即将重启进入 PE 完成安装...");
        let _ = crate::utils::cmd::create_command("shutdown")
            .args(["/r", "/t", "5", "/c", "LetRecovery 即将重启进入 PE 完成系统安装..."])
            .spawn();
    } else {
        println!("[CLI INSTALL] 未启用自动重启，请手动重启进入 PE 完成安装。");
    }

    Ok(())
}

/// 粗略判断两个路径是否指向同一文件（避免把镜像拷到自身）。
fn same_file(a: &str, b: &str) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}
