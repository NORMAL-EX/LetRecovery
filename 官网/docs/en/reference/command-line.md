---
title: Command-Line Reference
description: Command-line parameters for LetRecovery and the PE client, plus the install/advanced-options JSON configuration.
---

# Command-Line Parameters and Unattended Installation

LetRecovery consists of two executables, each supporting its own set of command-line parameters:

- **`LetRecovery.exe`** (desktop client, runs inside an installed Windows)
- **`LetRecoveryPE.exe`** (PE client, runs inside WinPE)

Parameter names are **case-insensitive**, and both the `/FLAG` and `--flag` forms are accepted.

::: tip About the PE client's filename
The PE client's executable is named **`LetRecoveryPE.exe`** (the Cargo package name `letrecovery-pe` is only an internal name).
:::

## 1. LetRecovery.exe (desktop client)

| Parameter | Alias | Description |
| --- | --- | --- |
| *(no parameters)* | | Launches the GUI. When run as a non-administrator, it automatically requests UAC elevation (forwarding the original command-line parameters). |
| `--install` | `/INSTALL` | **Command-line unattended installation** (drives a one-click reinstall from the desktop; see below). Must be combined with `--config`. |
| `--config <path>` | `/CONFIG`, `--config=<path>` | Specifies the **install configuration JSON** (see [install.json](#install-configuration-install-json)). |
| `--advanced <path>` | `/ADVANCED`, `--advanced=<path>` | Specifies the **advanced-options JSON** (optional; see [advanced.json](#advanced-options-advanced-json)). |
| `/PEINSTALL` | `--pe-install` | Reads the install configuration already written to the data partition and performs the installation (usually invoked by the program itself after preparation). |
| `/PEBACKUP` | `--pe-backup` | Reads the backup configuration already written to the data partition and performs the backup. |

### Command-line unattended installation

```bat
LetRecovery.exe --install --config <install.json> [--advanced <advanced.json>]
```

**Behavior**: runs on the **normal Windows desktop**; reads `--config` (and the optional `--advanced`) → places the image into the
automatically selected data partition → writes the install configuration and the target-disk marker → sets the next reboot to enter PE → (by default) reboots. After the reboot,
PE is entered and the PE client reads the configuration to complete the actual deployment (format the target disk, apply the image, import drivers, apply advanced options,
repair the boot).

::: warning Must run in an elevated environment
This must run in an "elevated (administrator)" environment (e.g., a command line opened as administrator, or a scheduled task). If launched from a non-elevated environment,
a UAC prompt appears; after you accept it, the parameters are forwarded and execution continues.
:::

## 2. LetRecoveryPE.exe (PE client)

| Parameter | Alias | Description |
| --- | --- | --- |
| *(no parameters)* | | Launches the PE GUI. |
| `/PEINSTALL` | `--pe-install` | Command-line automatic installation (reads the data-partition configuration, no GUI). |
| `/PEBACKUP` | `--pe-backup` | Command-line automatic backup. |
| `/AUTO` | `--auto` | Automatically detects the configuration type (install/backup) in the data partition and launches the corresponding interface; if no configuration is found, it prompts. |

## Install configuration install.json

The JSON pointed to by `--config`. **The first three fields are required**; the rest may be omitted (the defaults from the table below are used).

| Field | Type | Default | Description |
| --- | --- | --- | --- |
| `target_partition` | string | **Required** | The drive letter of the system disk to reinstall, e.g., `"C:"`. It will be formatted after entering PE. |
| `image_path` | string | **Required** | **Absolute path** of the image (`.wim`/`.esd`/`.swm` or `.gho`/`.ghs`). |
| `pe_path` | string | **Required** | **Absolute path** of the PE boot file (`.wim` or `.iso`). |
| `volume_index` | number | `1` | Volume index within the image (selects the edition for multi-edition WIM/ESD). |
| `is_gho` | bool | Auto-detected by extension | Whether it is a GHO image. |
| `driver_action_mode` | number | `0` | Driver handling: `0` = do nothing, `1` = back up only, `2` = import automatically (imported from the `drivers\` folder in the data directory). |
| `unattended` | bool | `false` | Whether to generate an unattended configuration. |
| `auto_reboot` | bool | `true` | Whether to automatically reboot into PE after preparation completes. |
| `custom_unattend_path` | string | `""` | Absolute path of a custom unattended XML (it is copied into the data directory). |
| `data_partition` | string\|null | `null` (auto-select) | Drive letter of the data partition used to stage the configuration/image; if omitted, a partition with sufficient space that is not the target disk is selected automatically. |
| `pe_display_name` | string\|null | `"LetRecovery PE"` | Display name of the PE boot entry. |

Example:

```json
{
  "target_partition": "C:",
  "image_path": "D:\\Images\\Win11_24H2.wim",
  "pe_path": "D:\\LetRecovery\\bin\\pe\\LetRecovery_PE.wim",
  "volume_index": 1,
  "is_gho": false,
  "driver_action_mode": 2,
  "unattended": true,
  "auto_reboot": true,
  "custom_unattend_path": "",
  "data_partition": null,
  "pe_display_name": "LetRecovery PE"
}
```

## Advanced options advanced.json

The JSON pointed to by `--advanced`, corresponding to the program's "Advanced Options." **You may write only the fields you need**; the rest take their defaults.
The following fields **take effect** during the PE installation process (identical to the GUI's "reboot into PE to install" path):

| Field | Type | Description |
| --- | --- | --- |
| `bypass_nro` | bool | Bypass the OOBE forced network connection. |
| `remove_uwp_apps` | bool | Remove preinstalled UWP apps. |
| `import_storage_controller_drivers` | bool | Import disk controller drivers (Win10/11 x64). |
| `disable_windows_update` | bool | Disable Windows Update. |
| `disable_windows_defender` | bool | Disable Windows Security. |
| `disable_reserved_storage` | bool | Disable reserved storage. |
| `disable_uac` | bool | Disable User Account Control. |
| `disable_device_encryption` | bool | Disable automatic device encryption. |
| `remove_shortcut_arrow` | bool | Remove the shortcut overlay arrow. |
| `restore_classic_context_menu` | bool | Restore the classic context menu on Win11. |
| `custom_username` + `username` | bool + string | Custom username (when `custom_username=true`, `username` is used). |
| `custom_volume_label` + `volume_label` | bool + string | Custom system-disk volume label. |
| `win7_uefi_patch` | bool | Win7 UEFI patch (UefiSeven). |
| `win7_inject_usb3_driver` | bool | Inject USB3 drivers for Win7. |
| `win7_inject_nvme_driver` | bool | Inject NVMe drivers for Win7. |
| `win7_fix_acpi_bsod` | bool | Fix the ACPI BSOD on Win7. |
| `win7_fix_storage_bsod` | bool | Fix the storage-controller BSOD on Win7. |

Example:

```json
{
  "bypass_nro": true,
  "remove_uwp_apps": true,
  "import_storage_controller_drivers": true,
  "disable_device_encryption": true,
  "custom_username": true,
  "username": "User",
  "custom_volume_label": true,
  "volume_label": "OS"
}
```

::: warning Note
- The remaining, richer items in `AdvancedOptions` (custom scripts, custom driver directories, registry imports, custom files,
  Wi-Fi migration, etc.) **are not part of the "reboot into PE to install" process** (the same is true in the GUI), so even if you write them into advanced.json,
  they will not take effect in this command-line installation process.
- The command-line installation's **end-to-end process depends on a real reinstall environment** (PE boot + reboot + deployment), so please regression-test
  it on a physical machine or virtual machine.
- Absolute paths are recommended; backslashes in JSON must be escaped (`"D:\\Images\\x.wim"`).
:::
