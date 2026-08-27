//! Generates a versioned CLI configuration and a small launcher beside the desktop executable.
//!
//! The exporter is deliberately non-executing: it snapshots current UI intent, validates the
//! public CLI schema, and publishes files under `cli\`. Running the command file remains an
//! explicit later administrator action.

use anyhow::{anyhow, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::cli_config::{
    AdvancedSpec, BuiltInAdministratorSpec, CliConfig, CLI_CONFIG_SCHEMA_VERSION,
};
use super::ui_state::AdvancedOptionsData;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedAutomation {
    pub directory: PathBuf,
    pub config_path: PathBuf,
    pub script_path: PathBuf,
}

pub fn advanced_spec_from_options(value: &AdvancedOptionsData) -> AdvancedSpec {
    AdvancedSpec {
        preserve_personal_files: value.preserve_personal_files,
        remove_shortcut_arrow: value.remove_shortcut_arrow,
        restore_classic_context_menu: value.restore_classic_context_menu,
        bypass_nro: value.bypass_nro,
        disable_windows_update: value.disable_windows_update,
        disable_windows_defender: value.disable_windows_defender,
        disable_reserved_storage: value.disable_reserved_storage,
        disable_uac: value.disable_uac,
        disable_device_encryption: value.disable_device_encryption,
        remove_uwp_apps: value.remove_uwp_apps,
        migrate_wifi: value.migrate_wifi,
        wifi_ssid: value.wifi_ssid.clone(),
        wifi_profile_xml: value.wifi_profile_xml.clone(),
        install_vmware_tools: value.install_vmware_tools,
        deploy_script_path: if value.run_script_during_deploy {
            value.deploy_script_path.clone()
        } else {
            String::new()
        },
        first_login_script_path: if value.run_script_first_login {
            value.first_login_script_path.clone()
        } else {
            String::new()
        },
        custom_drivers_path: if value.import_custom_drivers {
            value.custom_drivers_path.clone()
        } else {
            String::new()
        },
        import_storage_controller_drivers: value.import_storage_controller_drivers,
        registry_file_path: if value.import_registry_file {
            value.registry_file_path.clone()
        } else {
            String::new()
        },
        custom_files_path: if value.import_custom_files {
            value.custom_files_path.clone()
        } else {
            String::new()
        },
        username: if value.custom_username {
            value.username.clone()
        } else {
            String::new()
        },
        builtin_administrator: BuiltInAdministratorSpec {
            enabled: value.builtin_administrator.enabled,
            account_name: value.builtin_administrator.account_name.clone(),
            password: value
                .builtin_administrator
                .password
                .expose_secret()
                .to_owned(),
            auto_logon: value.builtin_administrator.auto_logon,
        },
        volume_label: if value.custom_volume_label {
            value.volume_label.clone()
        } else {
            String::new()
        },
        win7_fix_acpi_bsod: value.win7_fix_acpi_bsod,
        win7_inject_usb3_driver: value.win7_inject_usb3_driver,
        win7_usb3_driver_path: value.win7_usb3_driver_path.clone(),
        win7_inject_nvme_driver: value.win7_inject_nvme_driver,
        win7_nvme_driver_path: value.win7_nvme_driver_path.clone(),
        win7_fix_storage_bsod: value.win7_fix_storage_bsod,
        win7_uefi_patch: value.win7_uefi_patch,
        xp_inject_usb3_driver: value.xp_inject_usb3_driver,
        xp_inject_nvme_driver: value.xp_inject_nvme_driver,
    }
}

pub fn export(config: CliConfig, stem: &str, subcommand: &str) -> Result<ExportedAutomation> {
    validate_token(stem, "automation filename")?;
    validate_token(subcommand, "CLI subcommand")?;
    let mut config = config;
    if config.schema_version != CLI_CONFIG_SCHEMA_VERSION {
        return Err(anyhow!(
            "automation config uses an unsupported schema version"
        ));
    }
    config
        .normalize()
        .context("validate generated CLI configuration")?;

    let exe = std::env::current_exe().context("locate the running LetRecovery executable")?;
    let exe_name = exe
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("the LetRecovery executable filename is not Unicode"))?;
    validate_executable_filename(exe_name)?;
    let directory = crate::utils::path::get_exe_dir().join("cli");
    let config_path = directory.join(format!("{stem}.json"));
    let script_path = directory.join(format!("run-{stem}.cmd"));
    let script = launcher_script(exe_name, subcommand, &format!("{stem}.json"));

    // Publish the protected JSON first. If the second publication fails, the validated JSON is
    // still useful and the returned error explicitly reports that only the launcher is missing.
    config.write_atomic(&config_path, true)?;
    write_script_atomic(&script_path, script.as_bytes()).with_context(|| {
        format!(
            "configuration was generated at {}, but its launcher could not be published",
            config_path.display()
        )
    })?;
    if std::fs::read(&script_path).context("read generated launcher")? != script.as_bytes() {
        return Err(anyhow!("generated launcher readback mismatch"));
    }
    Ok(ExportedAutomation {
        directory,
        config_path,
        script_path,
    })
}

fn launcher_script(exe_name: &str, subcommand: &str, config_name: &str) -> String {
    format!(
        "@echo off\r\nsetlocal\r\n\"%~dp0..\\{exe_name}\" {subcommand} run --config \"%~dp0{config_name}\" --yes\r\nset \"lr_exit=%errorlevel%\"\r\necho.\r\npause\r\nexit /b %lr_exit%\r\n"
    )
}

fn validate_token(value: &str, field: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(anyhow!("{field} contains unsupported characters"));
    }
    Ok(())
}

fn validate_executable_filename(value: &str) -> Result<()> {
    if value.is_empty()
        || value
            .chars()
            .any(|character| character <= '\u{1f}' || "\"%&|<>^!()".contains(character))
        || value.contains(['\\', '/'])
    {
        return Err(anyhow!("unsafe LetRecovery executable filename"));
    }
    Ok(())
}

fn write_script_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("launcher output path has no parent"))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create launcher directory {}", parent.display()))?;
    if path.exists() {
        super::cli_config::require_plain_regular_file(path)?;
    }
    let pinned = lr_core::scoped_temp_file::pin_existing_directory_ancestors(parent)
        .context("pin launcher output directory")?;
    let (temporary, mut file) =
        lr_core::scoped_temp_file::ScopedTempFile::create_protected_writer_in(
            parent,
            "letrecovery-cli-launcher",
            "cmd",
        )?;
    file.write_all(bytes).context("write temporary launcher")?;
    file.sync_all().context("flush temporary launcher")?;
    drop(file);
    if std::fs::read(temporary.path()).context("read temporary launcher")? != bytes {
        return Err(anyhow!("temporary launcher readback mismatch"));
    }
    pinned.verify_unchanged()?;
    super::cli_config::publish_temporary(temporary.path(), path, true)?;
    pinned.verify_unchanged()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_keeps_only_enabled_path_options_and_runtime_wifi() {
        let value = AdvancedOptionsData {
            migrate_wifi: true,
            wifi_ssid: "Test WiFi".to_owned(),
            wifi_profile_xml: "<WLANProfile/>".to_owned(),
            deploy_script_path: r"D:\ignored.cmd".to_owned(),
            import_custom_files: true,
            custom_files_path: r"D:\payload".to_owned(),
            ..Default::default()
        };
        let spec = advanced_spec_from_options(&value);
        assert!(spec.deploy_script_path.is_empty());
        assert_eq!(spec.custom_files_path, r"D:\payload");
        assert_eq!(spec.wifi_ssid, "Test WiFi");
        assert_eq!(spec.wifi_profile_xml, "<WLANProfile/>");
    }

    #[test]
    fn launcher_uses_script_relative_paths_and_preserves_exit_code() {
        let script = launcher_script("LetRecovery.exe", "install", "install.json");
        assert!(script.contains(r#""%~dp0..\LetRecovery.exe" install run"#));
        assert!(script.contains(r#"--config "%~dp0install.json" --yes"#));
        assert!(script.contains("exit /b %lr_exit%"));
    }

    #[test]
    fn command_metacharacters_are_rejected() {
        assert!(validate_executable_filename("LetRecovery&calc.exe").is_err());
        assert!(validate_token("install & calc", "test").is_err());
    }
}
