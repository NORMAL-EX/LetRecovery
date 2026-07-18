//! 离线注册表操作（两端共享）：通过 reg.exe load/unload/add/delete 操作离线配置单元。

use anyhow::Result;

use crate::command::new_command;
use crate::encoding::gbk_to_utf8;

pub struct OfflineRegistry;

fn parse_string_query_output(output: &str, value_name: &str) -> Option<String> {
    parse_all_string_query_values(output, value_name)
        .into_iter()
        .next()
}

fn parse_all_string_query_values(output: &str, value_name: &str) -> Vec<String> {
    let mut values = Vec::new();
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
            values.push(value.to_string());
            break;
        }
    }
    values
}

fn parse_dword_query_output(output: &str, value_name: &str) -> Option<u32> {
    for line in output.lines() {
        let trimmed = line.trim();
        let Some(type_offset) = trimmed.find("REG_DWORD") else {
            continue;
        };
        if !trimmed[..type_offset]
            .trim()
            .eq_ignore_ascii_case(value_name)
        {
            continue;
        }
        let value = trimmed[type_offset + "REG_DWORD".len()..].trim();
        if let Some(hex) = value
            .strip_prefix("0x")
            .or_else(|| value.strip_prefix("0X"))
        {
            return u32::from_str_radix(hex, 16).ok();
        }
        return value.parse().ok();
    }
    None
}

fn registry_query_reports_missing(stdout: &str, stderr: &str) -> bool {
    let combined = format!("{stdout}\n{stderr}").to_ascii_lowercase();
    [
        "unable to find",
        "cannot find",
        "not found",
        "找不到",
        "未找到",
    ]
    .iter()
    .any(|needle| combined.contains(needle))
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

    /// Recursively read every string value with the requested name below a key.
    /// Missing roots return an empty list; all other command/query failures fail closed.
    pub fn query_string_values_recursive(key_path: &str, value_name: &str) -> Result<Vec<String>> {
        if !Self::key_exists(key_path)? {
            return Ok(Vec::new());
        }
        let output = new_command("reg.exe")
            .args(["query", key_path, "/s", "/v", value_name])
            .output()?;
        let stdout = gbk_to_utf8(&output.stdout);
        let stderr = gbk_to_utf8(&output.stderr);
        if !output.status.success() {
            if registry_query_reports_missing(&stdout, &stderr) {
                return Ok(Vec::new());
            }
            anyhow::bail!(
                "Failed to recursively query registry value {}\\{}: {} {}",
                key_path,
                value_name,
                stdout.trim(),
                stderr.trim()
            );
        }
        Ok(parse_all_string_query_values(&stdout, value_name))
    }

    /// 读取 DWORD 值。查询失败和值不存在均返回带上下文的错误。
    pub fn query_dword(key_path: &str, value_name: &str) -> Result<u32> {
        let output = new_command("reg.exe")
            .args(["query", key_path, "/v", value_name])
            .output()?;
        let stdout = gbk_to_utf8(&output.stdout);
        let stderr = gbk_to_utf8(&output.stderr);
        if !output.status.success() {
            anyhow::bail!(
                "Failed to query registry DWORD {}\\{}: {}",
                key_path,
                value_name,
                stderr.trim()
            );
        }
        parse_dword_query_output(&stdout, value_name).ok_or_else(|| {
            anyhow::anyhow!(
                "Registry query did not return {}\\{} as a DWORD value",
                key_path,
                value_name
            )
        })
    }

    /// 判断键是否存在。只有 reg.exe 明确报告“找不到”时才返回 false，其他查询错误失败关闭。
    pub fn key_exists(key_path: &str) -> Result<bool> {
        let output = new_command("reg.exe").args(["query", key_path]).output()?;
        if output.status.success() {
            return Ok(true);
        }
        let stdout = gbk_to_utf8(&output.stdout);
        let stderr = gbk_to_utf8(&output.stderr);
        if registry_query_reports_missing(&stdout, &stderr) {
            return Ok(false);
        }
        anyhow::bail!(
            "Failed to determine whether registry key exists [{}]: {} {}",
            key_path,
            stdout.trim(),
            stderr.trim()
        )
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

    /// 删除注册表键（历史兼容入口；忽略不存在和删除错误）。
    pub fn delete_key(key_path: &str) -> Result<()> {
        let _ = new_command("reg.exe")
            .args(["delete", key_path, "/f"])
            .output();
        Ok(())
    }

    /// 删除注册表键并复核结果。键不存在视为成功，其他查询或删除错误失败关闭。
    pub fn delete_key_verified(key_path: &str) -> Result<bool> {
        if !Self::key_exists(key_path)? {
            return Ok(false);
        }
        let output = new_command("reg.exe")
            .args(["delete", key_path, "/f"])
            .output()?;
        if !output.status.success() {
            let stdout = gbk_to_utf8(&output.stdout);
            let stderr = gbk_to_utf8(&output.stderr);
            anyhow::bail!(
                "Failed to delete registry key [{}]: {} {}",
                key_path,
                stdout.trim(),
                stderr.trim()
            );
        }
        if Self::key_exists(key_path)? {
            anyhow::bail!("Registry key still exists after deletion: {}", key_path);
        }
        Ok(true)
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
    use super::{
        parse_all_string_query_values, parse_dword_query_output, parse_string_query_output,
        registry_query_reports_missing,
    };

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

    #[test]
    fn parses_repeated_string_values_from_recursive_queries() {
        let output = "HKEY_LOCAL_MACHINE\\LR_TEST\\TaskA\r\n    Id    REG_SZ    {0ACC9108-2000-46C0-8407-5FD9F89521E8}\r\n\r\nHKEY_LOCAL_MACHINE\\LR_TEST\\TaskB\r\n    Id    REG_SZ    {B05F34EE-83F2-413D-BC1D-7D5BD6E98300}\r\n";
        assert_eq!(
            parse_all_string_query_values(output, "Id"),
            vec![
                "{0ACC9108-2000-46C0-8407-5FD9F89521E8}",
                "{B05F34EE-83F2-413D-BC1D-7D5BD6E98300}",
            ]
        );
    }

    #[test]
    fn parses_hex_and_decimal_dword_queries() {
        let output = "HKEY_LOCAL_MACHINE\\LR_TEST\\Select\r\n    Current    REG_DWORD    0x2\r\n    Default    REG_DWORD    3\r\n";
        assert_eq!(parse_dword_query_output(output, "Current"), Some(2));
        assert_eq!(parse_dword_query_output(output, "Default"), Some(3));
        assert_eq!(parse_dword_query_output(output, "Missing"), None);
    }

    #[test]
    fn missing_registry_queries_are_distinct_from_access_failures() {
        assert!(registry_query_reports_missing(
            "",
            "ERROR: The system was unable to find the specified registry key or value."
        ));
        assert!(registry_query_reports_missing(
            "",
            "错误: 系统找不到指定的注册表项或值。"
        ));
        assert!(!registry_query_reports_missing(
            "",
            "ERROR: Access is denied."
        ));
    }
}
