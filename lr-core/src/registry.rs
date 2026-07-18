//! 离线注册表操作（两端共享）：通过 reg.exe load/unload/add/delete 操作离线配置单元。

use anyhow::Result;

use crate::command::new_command;
use crate::encoding::gbk_to_utf8;

pub struct OfflineRegistry;

fn parse_string_query_output(output: &str, value_name: &str) -> Option<String> {
    for line in output.lines() {
        let trimmed = line.trim();
        for value_type in ["REG_SZ", "REG_EXPAND_SZ"] {
            let Some(type_offset) = trimmed.find(value_type) else {
                continue;
            };
            if !trimmed[..type_offset]
                .trim()
                .eq_ignore_ascii_case(value_name)
            {
                continue;
            }
            let value = trimmed[type_offset + value_type.len()..].trim();
            return Some(value.to_string());
        }
    }
    None
}

impl OfflineRegistry {
    /// 读取字符串值。查询失败和值不存在均返回带上下文的错误，调用方不能把未知状态当成功。
    pub fn query_string(key_path: &str, value_name: &str) -> Result<String> {
        let output = new_command("reg.exe")
            .args(["query", key_path, "/v", value_name])
            .output()?;
        let stdout = gbk_to_utf8(&output.stdout);
        let stderr = gbk_to_utf8(&output.stderr);
        if !output.status.success() {
            anyhow::bail!(
                "Failed to query registry value {}\\{}: {}",
                key_path,
                value_name,
                stderr.trim()
            );
        }
        parse_string_query_output(&stdout, value_name).ok_or_else(|| {
            anyhow::anyhow!(
                "Registry query did not return {}\\{} as a string value",
                key_path,
                value_name
            )
        })
    }

    /// 加载离线注册表配置单元
    pub fn load_hive(hive_name: &str, hive_file: &str) -> Result<()> {
        let key_path = format!("HKLM\\{}", hive_name);
        let output = new_command("reg.exe")
            .args(["load", &key_path, hive_file])
            .output()?;

        if !output.status.success() {
            let stderr = gbk_to_utf8(&output.stderr);
            // 加载失败是高危错误：后续所有离线注册表修改都会静默无效。
            // 即使调用方用 `let _ =` 丢弃错误，这里也确保日志里有记录。
            log::warn!(
                "加载离线注册表配置单元失败 [{}] <- {}: {}",
                hive_name,
                hive_file,
                stderr.trim()
            );
            anyhow::bail!("Failed to load registry hive: {}", stderr);
        }
        log::info!("已加载离线注册表配置单元 [{}] <- {}", hive_name, hive_file);
        Ok(())
    }

    /// 卸载离线注册表配置单元
    pub fn unload_hive(hive_name: &str) -> Result<()> {
        let key_path = format!("HKLM\\{}", hive_name);

        // 尝试多次卸载，因为有时需要等待
        for _ in 0..3 {
            let output = new_command("reg.exe")
                .args(["unload", &key_path])
                .output()?;

            if output.status.success() {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }

        let output = new_command("reg.exe")
            .args(["unload", &key_path])
            .output()?;

        if !output.status.success() {
            let stderr = gbk_to_utf8(&output.stderr);
            // 卸载失败可能导致 hive 文件被占用、配置未落盘。
            log::warn!(
                "卸载离线注册表配置单元失败 [{}]: {}",
                hive_name,
                stderr.trim()
            );
            anyhow::bail!("Failed to unload registry hive: {}", stderr);
        }
        Ok(())
    }

    /// 写入 DWORD 值
    pub fn set_dword(key_path: &str, value_name: &str, data: u32) -> Result<()> {
        let output = new_command("reg.exe")
            .args([
                "add",
                key_path,
                "/v",
                value_name,
                "/t",
                "REG_DWORD",
                "/d",
                &data.to_string(),
                "/f",
            ])
            .output()?;

        if !output.status.success() {
            let stderr = gbk_to_utf8(&output.stderr);
            anyhow::bail!("Failed to set registry value: {}", stderr);
        }
        Ok(())
    }

    /// 写入字符串值
    pub fn set_string(key_path: &str, value_name: &str, data: &str) -> Result<()> {
        let output = new_command("reg.exe")
            .args([
                "add", key_path, "/v", value_name, "/t", "REG_SZ", "/d", data, "/f",
            ])
            .output()?;

        if !output.status.success() {
            let stderr = gbk_to_utf8(&output.stderr);
            anyhow::bail!("Failed to set registry value: {}", stderr);
        }
        Ok(())
    }

    /// 写入可扩展字符串值 (REG_EXPAND_SZ)
    pub fn set_expand_string(key_path: &str, value_name: &str, data: &str) -> Result<()> {
        let output = new_command("reg.exe")
            .args([
                "add",
                key_path,
                "/v",
                value_name,
                "/t",
                "REG_EXPAND_SZ",
                "/d",
                data,
                "/f",
            ])
            .output()?;

        if !output.status.success() {
            let stderr = gbk_to_utf8(&output.stderr);
            anyhow::bail!("Failed to set registry expand string value: {}", stderr);
        }
        Ok(())
    }

    /// 删除注册表键（忽略不存在）
    pub fn delete_key(key_path: &str) -> Result<()> {
        let _ = new_command("reg.exe")
            .args(["delete", key_path, "/f"])
            .output();
        Ok(())
    }

    /// 创建注册表键（如果不存在）
    pub fn create_key(key_path: &str) -> Result<()> {
        let output = new_command("reg.exe")
            .args(["add", key_path, "/f"])
            .output()?;

        if !output.status.success() {
            let stderr = gbk_to_utf8(&output.stderr);
            anyhow::bail!("Failed to create registry key: {}", stderr);
        }
        Ok(())
    }

    /// 删除注册表值（忽略不存在）
    pub fn delete_value(key_path: &str, value_name: &str) -> Result<()> {
        let _ = new_command("reg.exe")
            .args(["delete", key_path, "/v", value_name, "/f"])
            .output();
        Ok(())
    }

    /// 导入 .reg 文件
    pub fn import_reg_file(reg_file: &str) -> Result<()> {
        let output = new_command("reg.exe").args(["import", reg_file]).output()?;

        if !output.status.success() {
            let stderr = gbk_to_utf8(&output.stderr);
            anyhow::bail!("Failed to import reg file: {}", stderr);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::parse_string_query_output;

    #[test]
    fn parses_reg_query_string_without_losing_spaces() {
        let output = "HKEY_LOCAL_MACHINE\\LR_TEST\\Setup\\State\r\n    ImageState    REG_SZ    IMAGE_STATE_GENERALIZE_RESEAL_TO_OOBE\r\n    ProductName    REG_SZ    Windows 11 专业版\r\n";
        assert_eq!(
            parse_string_query_output(output, "ImageState").as_deref(),
            Some("IMAGE_STATE_GENERALIZE_RESEAL_TO_OOBE")
        );
        assert_eq!(
            parse_string_query_output(output, "ProductName").as_deref(),
            Some("Windows 11 专业版")
        );
        assert_eq!(parse_string_query_output(output, "Missing"), None);
    }
}
