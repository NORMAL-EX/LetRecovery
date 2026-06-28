---
title: System Installation
description: Deploy a Windows image to a partition with LetRecovery.
---

# System Installation

The **System Installation** page is used to deploy a Windows image to a partition.

## Supported image formats

| Format | Description |
| --- | --- |
| **WIM / ESD / SWM** | Standard Windows images, applied via wimlib (SWM is split; the remaining split files in the same directory are detected automatically). |
| **GHO** | Ghost images, restored via the built-in Ghost engine. |
| **ISO** | Mounted automatically, using the `install.wim` / `install.esd` / `install.swm` inside. |
| **XP / 2003 i386** | An original XP/2003 installation disc (with `\I386` at the root and no `install.wim`) is recognized as text-mode setup media; see [Windows XP / 2003 Installation](/guide/xp-install). |

After picking an image, select the **edition** to install (the image's split index). The edition dropdown automatically filters out volumes that can't be installed (such as WindowsPE / Setup media volumes) and selects the first installable edition by default.

## Selecting the target partition

Select the target partition from the list. The table shows capacity, free space, volume label, partition table (GPT/MBR), BitLocker status, and whether the partition already has a system.

::: tip Is the target drive BitLocker-encrypted?
If the target or data partition is locked, an unlock dialog appears before installation. For an encrypted **system drive**, LetRecovery first tries to pass the recovery key through into PE to unlock it (fast), and only falls back to a full decrypt when no key is available — see [Reinstalling on a BitLocker-encrypted drive](/guide/bitlocker).
:::

## Installation methods

LetRecovery decides how to install automatically:

- **Direct mode** — when the target is **not** the currently running system drive, or when LetRecovery **is already running inside WinPE**. It formats the partition and applies the image directly.
- **Install via PE** — when installing a system from the desktop onto the **currently running system drive**. LetRecovery writes a temporary WinPE boot entry and reboots into it, then the [PE client](/guide/what-is-letrecovery) formats C: and applies the image. A running system can't overwrite itself, so a reboot is required.

## Pre-deployment image verification

In the **install via PE** and **PE client** installation flows, image integrity is verified (WIM/ESD via wimlib) **before** the target drive is formatted. A bad image is caught **before any disk is touched**, so a wrongly downloaded or corrupted image won't ruin your data.

::: warning Direct mode and GHO are excluded
- GHO images only undergo a structural head/tail check, not wimlib verification.
- "Direct mode," when installing from the desktop **directly** onto a non-system drive, is not currently covered by automatic pre-deployment verification. In that case, it's recommended to first verify the image manually via [Toolbox → Image Verification](/guide/toolbox) before installing.
:::

## Boot mode

LetRecovery automatically detects UEFI / Legacy (GPT→UEFI, MBR→Legacy) and writes the matching boot files. When needed, you can also manually specify `Auto / UEFI / Legacy` in the **Boot mode** dropdown. This dropdown is always visible; when unattended is enabled, it shares a row with the "Customize unattended" button, otherwise it occupies its own row.

## Custom partition script (advanced)

Once [Advanced Options](/guide/advanced-options) is enabled, an extra "Run Diskpart script" option appears: place `.cmd` / `.bat` / `.txt` scripts into `<program directory>\diskpart\`, and the installation runs them **before formatting** (installation is aborted if a script fails). This is useful for scenarios that need a special partition layout.

## Drivers and options

Before starting, you can enable driver export/import, disk controller driver injection, unattended, registry tweaks, WiFi configuration migration, and more in [Advanced Options](/guide/advanced-options). Win7 / XP images also automatically show their respective compatibility switches.

::: danger
Installation will **format the target partition**, so back up first.
:::
