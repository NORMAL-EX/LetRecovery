//! 软件列表模块
//!
//! 提供获取已安装软件列表的功能

use std::collections::HashSet;
use super::types::InstalledSoftware;

/// 获取已安装软件列表
pub fn get_installed_software() -> Vec<InstalledSoftware> {
    let mut software_list = Vec::new();

    #[cfg(windows)]
    {
        use winreg::enums::*;
        use winreg::RegKey;

        let registry_paths = [
            (HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"),
            (HKEY_LOCAL_MACHINE, r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall"),
            (HKEY_CURRENT_USER, r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"),
        ];

        let mut seen_names: HashSet<String> = HashSet::new();

        for (hkey, path) in &registry_paths {
            if let Ok(key) = RegKey::predef(*hkey).open_subkey(path) {
                for subkey_name in key.enum_keys().filter_map(|k| k.ok()) {
                    if let Ok(subkey) = key.open_subkey(&subkey_name) {
                        let name: String = subkey.get_value("DisplayName").unwrap_or_default();
                        if name.is_empty() {
                            continue;
                        }

                        // 跳过Windows更新
                        if name.starts_with("KB") || subkey_name.starts_with("KB") {
                            continue;
                        }

                        // 跳过已添加的软件（去重）
                        if seen_names.contains(&name) {
                            continue;
                        }
                        seen_names.insert(name.clone());

                        let version: String = subkey.get_value("DisplayVersion").unwrap_or_default();
                        let publisher: String = subkey.get_value("Publisher").unwrap_or_default();
                        let install_location: String =
                            subkey.get_value("InstallLocation").unwrap_or_default();

                        software_list.push(InstalledSoftware {
                            name,
                            version,
                            publisher,
                            install_location,
                        });
                    }
                }
            }
        }
    }

    // 按名称排序
    software_list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    software_list
}

/// 截断字符串到指定长度
pub fn truncate_string(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(3)).collect();
        format!("{}...", truncated)
    }
}

/// 保存软件列表到文件
pub fn save_software_list_to_file(path: &std::path::Path, software_list: &[InstalledSoftware]) {
    let mut content = String::new();
    content.push_str(&format!(
        "已安装软件列表 - 导出时间: {}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    ));
    content.push_str(&"=".repeat(100));
    content.push('\n');
    content.push_str(&format!(
        "{:<50} {:<20} {:<30}\n",
        "软件名称", "版本", "发布者"
    ));
    content.push_str(&"-".repeat(100));
    content.push('\n');

    for software in software_list {
        content.push_str(&format!(
            "{:<50} {:<20} {:<30}\n",
            truncate_string(&software.name, 48),
            truncate_string(&software.version, 18),
            truncate_string(&software.publisher, 28)
        ));
    }

    content.push_str(&"=".repeat(100));
    content.push('\n');
    content.push_str(&format!("共 {} 个软件\n", software_list.len()));

    if let Err(e) = std::fs::write(path, &content) {
        log::error!("保存软件列表失败: {}", e);
    }
}
