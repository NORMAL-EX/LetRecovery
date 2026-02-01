use anyhow::Result;
use crate::utils::cmd::create_command;

use crate::utils::encoding::gbk_to_utf8;

pub struct OfflineRegistry;

impl OfflineRegistry {
    /// 加载离线注册表配置单元
    pub fn load_hive(hive_name: &str, hive_file: &str) -> Result<()> {
        let key_path = format!("HKLM\\{}", hive_name);
        let output = create_command("reg.exe")
            .args(["load", &key_path, hive_file])
            .output()?;

        if !output.status.success() {
            let stderr = gbk_to_utf8(&output.stderr);
            anyhow::bail!("Failed to load registry hive: {}", stderr);
        }
        Ok(())
    }

    /// 卸载离线注册表配置单元
    pub fn unload_hive(hive_name: &str) -> Result<()> {
        let key_path = format!("HKLM\\{}", hive_name);

        // 尝试多次卸载，因为有时需要等待
        for _ in 0..3 {
            let output = create_command("reg.exe")
                .args(["unload", &key_path])
                .output()?;

            if output.status.success() {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }

        // 最后一次尝试
        let output = create_command("reg.exe")
            .args(["unload", &key_path])
            .output()?;

        if !output.status.success() {
            let stderr = gbk_to_utf8(&output.stderr);
            anyhow::bail!("Failed to unload registry hive: {}", stderr);
        }
        Ok(())
    }

    /// 写入 DWORD 值
    pub fn set_dword(key_path: &str, value_name: &str, data: u32) -> Result<()> {
        let output = create_command("reg.exe")
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
        let output = create_command("reg.exe")
            .args([
                "add",
                key_path,
                "/v",
                value_name,
                "/t",
                "REG_SZ",
                "/d",
                data,
                "/f",
            ])
            .output()?;

        if !output.status.success() {
            let stderr = gbk_to_utf8(&output.stderr);
            anyhow::bail!("Failed to set registry value: {}", stderr);
        }
        Ok(())
    }

    /// 写入可扩展字符串值 (REG_EXPAND_SZ)
    /// 用于包含环境变量引用的路径，如 %SystemRoot%\System32\drivers\xxx.sys
    pub fn set_expand_string(key_path: &str, value_name: &str, data: &str) -> Result<()> {
        let output = create_command("reg.exe")
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

    /// 删除注册表键
    pub fn delete_key(key_path: &str) -> Result<()> {
        let _ = create_command("reg.exe")
            .args(["delete", key_path, "/f"])
            .output();

        // 忽略不存在的情况
        Ok(())
    }

    /// 创建注册表键（如果不存在）
    pub fn create_key(key_path: &str) -> Result<()> {
        let output = create_command("reg.exe")
            .args(["add", key_path, "/f"])
            .output()?;

        if !output.status.success() {
            let stderr = gbk_to_utf8(&output.stderr);
            anyhow::bail!("Failed to create registry key: {}", stderr);
        }
        Ok(())
    }

    /// 删除注册表值
    pub fn delete_value(key_path: &str, value_name: &str) -> Result<()> {
        let _ = create_command("reg.exe")
            .args(["delete", key_path, "/v", value_name, "/f"])
            .output();

        Ok(())
    }

    /// 导入 .reg 文件
    pub fn import_reg_file(reg_file: &str) -> Result<()> {
        let output = create_command("reg.exe")
            .args(["import", reg_file])
            .output()?;

        if !output.status.success() {
            let stderr = gbk_to_utf8(&output.stderr);
            anyhow::bail!("Failed to import reg file: {}", stderr);
        }
        Ok(())
    }
}
