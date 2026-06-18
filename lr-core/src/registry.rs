//! 离线注册表操作（两端共享）：通过 reg.exe load/unload/add/delete 操作离线配置单元。

use anyhow::Result;

use crate::command::new_command;
use crate::encoding::gbk_to_utf8;

pub struct OfflineRegistry;

impl OfflineRegistry {
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
            log::warn!("加载离线注册表配置单元失败 [{}] <- {}: {}", hive_name, hive_file, stderr.trim());
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
            let output = new_command("reg.exe").args(["unload", &key_path]).output()?;

            if output.status.success() {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }

        let output = new_command("reg.exe").args(["unload", &key_path]).output()?;

        if !output.status.success() {
            let stderr = gbk_to_utf8(&output.stderr);
            // 卸载失败可能导致 hive 文件被占用、配置未落盘。
            log::warn!("卸载离线注册表配置单元失败 [{}]: {}", hive_name, stderr.trim());
            anyhow::bail!("Failed to unload registry hive: {}", stderr);
        }
        Ok(())
    }

    /// 写入 DWORD 值
    pub fn set_dword(key_path: &str, value_name: &str, data: u32) -> Result<()> {
        let output = new_command("reg.exe")
            .args([
                "add", key_path, "/v", value_name, "/t", "REG_DWORD", "/d",
                &data.to_string(), "/f",
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
                "add", key_path, "/v", value_name, "/t", "REG_EXPAND_SZ", "/d", data, "/f",
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
        let _ = new_command("reg.exe").args(["delete", key_path, "/f"]).output();
        Ok(())
    }

    /// 创建注册表键（如果不存在）
    pub fn create_key(key_path: &str) -> Result<()> {
        let output = new_command("reg.exe").args(["add", key_path, "/f"]).output()?;

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
