use anyhow::Result;

use crate::utils::command::new_command;
use crate::utils::encoding::gbk_to_utf8;

pub struct OfflineRegistry;

impl OfflineRegistry {
    /// 加载离线注册表配置单元
    pub fn load_hive(hive_name: &str, hive_file: &str) -> Result<()> {
        let key_path = format!("HKLM\\{}", hive_name);
        log::info!("加载注册表配置单元: {} -> {}", hive_file, key_path);

        let output = new_command("reg.exe")
            .args(["load", &key_path, hive_file])
            .output()?;

        if !output.status.success() {
            let stderr = gbk_to_utf8(&output.stderr);
            anyhow::bail!("加载注册表配置单元失败: {}", stderr);
        }
        Ok(())
    }

    /// 卸载离线注册表配置单元
    pub fn unload_hive(hive_name: &str) -> Result<()> {
        let key_path = format!("HKLM\\{}", hive_name);
        log::info!("卸载注册表配置单元: {}", key_path);

        // 尝试多次卸载，因为有时需要等待
        for attempt in 0..5 {
            let output = new_command("reg.exe")
                .args(["unload", &key_path])
                .output()?;

            if output.status.success() {
                log::info!("注册表配置单元卸载成功");
                return Ok(());
            }

            log::debug!("卸载尝试 {} 失败，等待重试...", attempt + 1);
            std::thread::sleep(std::time::Duration::from_millis(500));
        }

        // 最后一次尝试
        let output = new_command("reg.exe")
            .args(["unload", &key_path])
            .output()?;

        if !output.status.success() {
            let stderr = gbk_to_utf8(&output.stderr);
            anyhow::bail!("卸载注册表配置单元失败: {}", stderr);
        }
        Ok(())
    }

    /// 写入 DWORD 值
    pub fn set_dword(key_path: &str, value_name: &str, data: u32) -> Result<()> {
        log::debug!("设置注册表DWORD: {}\\{} = {}", key_path, value_name, data);

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
            anyhow::bail!("设置注册表值失败: {}", stderr);
        }
        Ok(())
    }

    /// 写入字符串值
    pub fn set_string(key_path: &str, value_name: &str, data: &str) -> Result<()> {
        log::debug!(
            "设置注册表字符串: {}\\{} = {}",
            key_path,
            value_name,
            data
        );

        let output = new_command("reg.exe")
            .args([
                "add", key_path, "/v", value_name, "/t", "REG_SZ", "/d", data, "/f",
            ])
            .output()?;

        if !output.status.success() {
            let stderr = gbk_to_utf8(&output.stderr);
            anyhow::bail!("设置注册表值失败: {}", stderr);
        }
        Ok(())
    }

    /// 创建注册表键（如果不存在）
    pub fn create_key(key_path: &str) -> Result<()> {
        log::debug!("创建注册表键: {}", key_path);

        let output = new_command("reg.exe")
            .args(["add", key_path, "/f"])
            .output()?;

        if !output.status.success() {
            let stderr = gbk_to_utf8(&output.stderr);
            anyhow::bail!("创建注册表键失败: {}", stderr);
        }
        Ok(())
    }
}
