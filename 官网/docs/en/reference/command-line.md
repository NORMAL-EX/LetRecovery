---
title: Command-Line Reference
description: Versioned install, backup, and configuration commands for the normal-Windows client.
---

# Command-Line Reference

The public CLI belongs only to the normal-Windows `LetRecovery.exe`. PE has no user CLI: the legacy `/PEINSTALL` and `/PEBACKUP` switches are retired and always rejected. Only `/AUTO` remains as an internal entry point for an authenticated handoff created by the normal-Windows client; external scripts must not construct or invoke it directly.

The GUI can expose its default-off automation exporter from the About page, then generate `cli\*.json` and a relative-path CMD from either Installation or Backup. Export only writes files; it does not run a task, and it uses the same strict schema and validation boundary described below.

```text
inspect disks|image|pe-cache ...
install plan --config <file> | install run --config <file> [--yes] [--dry-run]
backup plan --config <file> | backup run --config <file> [--yes] [--dry-run]
update restore
config generate|validate|normalize|show ...
```

`disable_windows_update` changes only reversible Windows Update policies and service start types. It does not delete services, tasks, files, or ACLs, does not modify BITS, and does not write legacy Defender-disable policies. It blocks automatic Windows Update/Microsoft Update delivery, including Defender platform and security-intelligence updates, but Store, Office, manual installation, enterprise management, or a feature upgrade can still redeploy components.

`update restore` restores only Windows Update values still owned by LetRecovery and unchanged by an administrator, domain policy, or MDM. It requires an already elevated console, never opens UAC or a message box, and reports conflicts or partial restoration through structured events and the final JSON result.

Advanced optimization never broadens into fuzzy package matching. `remove_uwp_apps` handles only exact Name/PFN identities from the shared curated list and explicitly preserves new Outlook, OneDrive Sync, and Win32 OneDrive. On Windows 11 it also disables Start recommendations and preinstalled content delivery before the default user is created, preventing dynamic Get Started, Solitaire, and Microsoft PC Manager entries from being regenerated. `disable_windows_defender` deep-removes the Defender Antivirus engine and makes a best-effort removal of only the two exact SecHealthUI PFNs; SecurityHealthService, the Windows Security Center service, Firewall, UAC, SmartScreen, VBS, and Defender for Endpoint remain. `disable_reserved_storage` uses online DISM only for a confirmed Windows 10/11 build 19041+ target with the built-in answer file; failure or an unconfirmed final state is warning-only.

`inspect disks`, `inspect image --path <image>`, and `inspect pe-cache` provide fresh read-only inventory for selecting a target, image index, and verified PE cache.

A real `run` requires an already elevated administrator console and explicit `--yes`; CLI execution never opens UAC or a message box. `plan` and `run --dry-run` are read-only and do not require administrator rights. Legacy `--install`/`--advanced` calls return a deterministic migration error.

Configurations are strict `schema_version: 1` JSON documents with `operation.type` set to `install` or `backup`; unknown fields, duplicate JSON keys at any depth, and duplicate arguments are rejected. `driver_action` is the sole driver behavior selector. Backup intent uses `execution_mode` (`auto|direct|via_pe`), `output_policy` (`create|replace|append`), and `auto_reboot`; the old `incremental` boolean is rejected. The normal-Windows CLI now permits WIM/ESD `auto|direct + create|replace|append + auto_reboot=false`. `create` rejects an existing target; `replace` and `append` require and fully bind an existing ordinary file. Execution copies from one old-file handle that denies write/delete sharing into private staging, verifies the completed image, then publishes through a durable PREPARED journal and handle CAS. System-volume backups that require PE, `via_pe`, and automatic reboot still fail closed during planning; PE exposes no public CLI. The wizard runs only with `--interactive`; its prompts are stderr JSON Lines and premature EOF fails explicitly. Configuration overwrite requires `--force`. After publication the exact protected DACL is checked again: only the current user, SYSTEM, and Administrators receive access, without changing the parent directory ACL. `show` and all events redact passwords.

```json
{"schema_version":1,"operation":{"type":"install","target_partition":"C:","image_path":"D:\\install.wim","volume_index":1,"format_partition":true,"repair_boot":true,"auto_reboot":false}}
```

Install fields also include `image_backing_path`, `unattended`, `automation_shutdown_on_terminal`, `driver_action`, `boot_mode`, `boot_pca_mode`, `custom_unattend_path`, `inherit_app_install_prefs`, `preinstalled_software_ids`, and `advanced`. When inheritance is explicit, a valid `config.json` adjacent to the EXE is the sole source of install preferences; a missing or malformed file fails instead of silently producing defaults. Every software ID is resolved uniquely from the current v4 catalogue on each plan, so URLs and silent commands are never inherited from stale preferences. The generator accepts `--inherit-app-install-prefs true` and comma-separated `--preinstalled-software-ids todesk,7zip-x64,bandizip-x64`. `--automation-shutdown-on-terminal true` is limited to disposable-VM automation: an acknowledged normal/PE terminal failure schedules power-off, while success continues into the new OS and powers off only after the authenticated first-logon finalizer has attempted every package; individual software failures remain warnings. Local sources support WIM, ESD, SWM, GHO, and GHS; the controller selects Direct or authenticated ViaPE from the target state. Every consecutive ViaPE SWM/GHS span is entered into LRHM3 and PE's fresh inventory rejects missing, extra, or out-of-order entries. The current ViaPE path fails closed for a custom answer file or Administrator password; supported Direct combinations are unaffected by that gate. `advanced` covers shortcut arrows, classic context menus, NRO, Windows Update, Defender/SecHealthUI, Reserved Storage, UAC, device encryption, curated AppX, deployment/first-logon scripts, custom and storage-controller drivers, registry/files, username, volume label, built-in Administrator, VMware Tools only when the guest is positively detected as VMware, and the guarded Windows 7 ACPI, USB3/NVMe, storage-fix, UEFI-patch, and XP USB3/NVMe options. GUI export can also carry the exact current-session Wi-Fi profile in the protected JSON; SSID, profile XML, and passwords are redacted, and credential-bearing fields are not accepted as command-line arguments. The generator accepts `--image-backing-path`, `--install-vmware-tools`, the matching Windows 7 switches/paths, and the built-in Administrator non-secret fields. Backup fields are `source_partition`, `save_path`, `name`, `description`, `format` (`wim|esd`), `execution_mode`, `output_policy`, and `auto_reboot`.

```json
{"schema_version":1,"operation":{"type":"backup","source_partition":"D:","save_path":"E:\\Backups\\data.wim","name":"Data","description":"Fresh direct backup","format":"wim","execution_mode":"direct","output_policy":"create","auto_reboot":false}}
```

Equivalent non-interactive generation:

```bat
LetRecovery.exe config generate --operation backup --output D:\lr-backup.json --source-partition D: --save-path E:\Backups\data.wim --name Data --format wim --execution-mode direct --output-policy create --auto-reboot false
```

stdout receives one final JSON object; stderr receives sanitized JSON Lines progress. Plans contain the effective configuration selected by the planner, fresh inventory bindings, and warnings rather than merely echoing input. Retired normal-endpoint PE switches are rejected before configuration loading or any administrator request. Exit codes: 0 success, 2 usage/permission/confirmation, 3 configuration, 4 preflight, 5 execution.

Batch files must use `start /wait "" LetRecovery.exe ...` before reading `%ERRORLEVEL%`. PowerShell should use `Start-Process -Wait -PassThru -NoNewWindow` and read `ExitCode`. The Windows-subsystem single EXE reuses inherited standard handles or attaches to the parent console; no second CLI executable is shipped.

See `docs/命令行参数.md` in the repository for the complete schema and examples.
