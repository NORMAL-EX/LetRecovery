---
title: Toolbox
description: Maintenance tools built into LetRecovery.
---

# Toolbox

The **Toolbox** page brings together commonly used maintenance tools. Some are desktop-only and others are WinPE-only. The interface hides unsupported entries for the current environment and Windows version, then compacts the layout. For example, Windows 7/8/8.1 never show Windows 10/11-only APPX tools.

## Disks and partitions

- **One-click partitioning** — visual partition planning (GPT/MBR, automatic scheme recommendation based on boot mode, a fixed 500 MB FAT32 ESP, capacity-bar preview).
- **Partition copy** — copies the files in one partition to another **one by one** (preserving attributes and timestamps, with **resume support**; before starting, it checks whether the target has enough free space to hold the source's used space). Note that this is a **file-level** copy, not a sector/block clone.
- **Batch format** — formats multiple partitions at once. **The system drive does not appear in the list** (under WinPE, `X:` is excluded as well), eliminating any chance of accidentally formatting the current system.

## Images and integrity

- **Image verification** — checks the integrity of **WIM / ESD / SWM / GHO / ISO** images before use.
- **File hash check** — computes a file's **SHA-256** and compares it against the expected value you paste in (to confirm download integrity).
- **View GHO password** — reads the password set in a Ghost image.

## System and security

- **BitLocker management** — unlock / decrypt / suspend·resume protection, view the recovery key (see [Reinstalling on a BitLocker-encrypted drive](/guide/bitlocker)).
- **Password reset** — clears a local account's password:
  - **Online** (the current system): uses a parameterized Windows account API to clear the password and enable the account by identity, without parsing localized `net user` output;
  - **Offline** (another system): uses a controlled registry/SAM boundary. Before making changes, it force-backs up the SAM as `SAM.lrbak`, then deletes that backup on success (to avoid leaving a copy containing hashes on the target drive), keeping the backup only on error so recovery is possible.
- **One-click boot repair** *(PE only)* — rebuilds the BCD / repairs UEFI·Legacy boot.

## Drivers and apps

- **Driver backup and restore**, **Import storage drivers**
- **Remove APPX apps** (shown only on supported Windows 10/11 environments and protected by a critical-component allowlist), **NVIDIA driver uninstall**

## System expansion and maintenance

- **Lossless C: Expansion** — losslessly expands the current system's C: drive; if the machine lacks WinPE, it is downloaded automatically, PE boot is set up, and the machine reboots into WinPE to finish. See [Lossless C: Expansion](/guide/expand-c-drive). *(Desktop only)*
- **Enter PE Maintenance** — an advanced desktop entry hidden by default. It appears only when `config.json` beside the EXE contains `"pe_maintenance_entry_enabled": true`. Clicking it immediately opens an animated preparation window showing the live stages: locating the local WIM, creating a private copy, collecting BitLocker keys, creating the one-shot boot entry, and scheduling restart. LetRecovery's PE task window stays hidden so the PE desktop remains available for manual maintenance. The desktop client makes a best-effort read of 48-digit BitLocker recovery passwords for currently lettered volumes, and PE only tries those passwords against currently locked volumes. Missing or rejected passwords are skipped; this **does not disable BitLocker, remove protectors, or start decryption**. Passwords are carried only in the private per-session boot WIM, bound by the authenticated manifest, and never written to public configuration or logs. A PE WIM already present in the bundled directory or download cache may be replaced or customized by the user. Catalogue MD5/SHA-256 values are checked only while downloading a new WIM; maintenance, install, and backup launch do not use them to reject a local WIM. *(Normal Windows only)*
- **Local network info** — view the machine's network configuration.
- **Reset network settings** — resets the network stack. *(Desktop only)*
- **Software list** — a list of commonly used software. *(Desktop only)*

## Others

- **System time sync** — syncs to **Beijing time (UTC+8)** via NTP (trying Alibaba Cloud, Tencent, `cn.ntp.org.cn`, `time.windows.com`, `pool.ntp.org`, etc., in order).
- **SpaceSniffer** — disk space usage analysis.
- **Run Ghost manually** — launches `Ghost64.exe` directly.

## Command line

The normal-Windows EXE exposes JSON CLI commands for all 22 Toolbox entries. Run `LetRecovery.exe tool list` first to see availability in the current environment, safety classes, and full usage. Read-only commands and `plan` do not modify the system. A real `run` / `remove` must be launched from an already elevated console and must include `--yes`; the public CLI never triggers UAC automatically.

| Tool | CLI |
| --- | --- |
| NVIDIA driver removal | `tool nvidia-driver inventory\|plan\|remove [--target current\|X:] [--yes]` |
| Partition copy | `tool partition-copy inventory\|plan\|run --source X: --target Y: [--yes]` |
| Batch format | `tool batch-format inventory\|plan\|run --drives X:,Y: --file-system NTFS\|FAT32\|exFAT [--label text] [--yes]` |
| Import storage driver | `tool storage-driver inventory\|plan\|run --target X: [--yes]` |
| Quick partition | `tool quick-partition inventory\|plan\|run --disk-number N --style GPT\|MBR --layout-file file [--yes]` |
| Remove APPX | `tool appx inventory --target current\|X:`; `tool appx plan\|run --target current\|X: --packages-file file [--yes]` |
| Driver backup/restore | `tool driver-transfer inventory\|plan\|run --mode backup\|restore --target current\|X: --directory directory [--yes]` |
| Boot repair | `tool repair-boot inventory\|plan\|run --target X: [--yes]` |
| Network information | `tool network-info inspect` |
| Software list | `tool software-list inspect` |
| Time synchronization | `tool time-sync plan\|run [--yes]` |
| Run Ghost | `tool ghost plan\|run [--yes]` |
| Read GHO password | `tool gho-password read --path file [--show-secret]` |
| Reset network | `tool reset-network plan\|run [--yes]` |
| SpaceSniffer | `tool space-sniffer plan\|run [--yes]` |
| Verify image | `tool verify-image inspect --path file` |
| BitLocker | `tool bitlocker inventory`; `tool bitlocker read-key --volume X: [--show-secret]`; `tool bitlocker plan\|run --volume X: --operation unlock-password\|unlock-recovery\|decrypt\|suspend\|resume [--secret-stdin] [--yes]` |
| File hash | `tool file-hash inspect --path file [--expected SHA256]` |
| Reset password | `tool reset-password inventory --target current\|X:`; `tool reset-password plan\|run --target current\|X: --account name [--yes]` |
| Expand C: | `tool expand-c analyze`; `tool expand-c plan\|run --target-size-mb N [--yes]` |
| Detailed hardware inspection | `tool hardware-inspect inspect` |
| Enter PE maintenance | `tool pe-maintenance plan\|run [--yes]` |

BitLocker passwords and recovery passwords cannot be command-line values; unlock accepts them only from standard input with `--secret-stdin`. GHO passwords and BitLocker recovery passwords are omitted from JSON by default and appear only with explicit `--show-secret`; avoid redirecting sensitive output into shared logs.

::: warning
Many operations in the Toolbox modify disks or the registry. Read the dialog descriptions carefully before confirming.
:::
