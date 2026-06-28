---
title: Toolbox
description: Maintenance tools built into LetRecovery.
---

# Toolbox

The **Toolbox** page brings together commonly used maintenance tools. Some are available only on the desktop, others only in WinPE — the interface automatically enables/disables (greys out) each tool based on the current environment.

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
  - **Online** (the current system): uses `net user` to clear the password and enable the account;
  - **Offline** (another system): edits its SAM directly. Before making changes, it force-backs up the SAM as `SAM.lrbak`, then deletes that backup on success (to avoid leaving a copy containing hashes on the target drive), keeping the backup only on error so recovery is possible.
- **One-click boot repair** *(PE only)* — rebuilds the BCD / repairs UEFI·Legacy boot.

## Drivers and apps

- **Driver backup and restore**, **Import storage drivers**
- **Remove APPX apps** (ships with a whitelist of critical system components so nothing essential is deleted by mistake), **NVIDIA driver uninstall**

## System expansion and maintenance

- **Lossless C: Expansion** — losslessly expands the current system's C: drive; if the machine lacks WinPE, it is downloaded automatically, PE boot is set up, and the machine reboots into WinPE to finish. See [Lossless C: Expansion](/guide/expand-c-drive). *(Desktop only)*
- **Local network info** — view the machine's network configuration.
- **Reset network settings** — resets the network stack. *(Desktop only)*
- **Software list** — a list of commonly used software. *(Desktop only)*

## Others

- **System time sync** — syncs to **Beijing time (UTC+8)** via NTP (trying Alibaba Cloud, Tencent, `cn.ntp.org.cn`, `time.windows.com`, `pool.ntp.org`, etc., in order).
- **SpaceSniffer** — disk space usage analysis.
- **Run Ghost manually** — launches `Ghost64.exe` directly.

::: warning
Many operations in the Toolbox modify disks or the registry. Read the dialog descriptions carefully before confirming.
:::
