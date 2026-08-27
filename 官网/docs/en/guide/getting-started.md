---
title: Getting Started
description: Download and run LetRecovery, and complete your first reinstall.
---

# Getting Started

## Requirements

To run the LetRecovery **desktop client**:

- Windows 7 / 8 / 8.1 / 10 / 11 (64-bit, one universal build) — 32-bit systems cannot run it
- Administrator privileges (launching as a non-administrator automatically triggers a UAC elevation prompt)
- UEFI or Legacy BIOS, either works

::: tip About memory
The official recommendation is at least **4 GB of free memory**, but this is just a rule of thumb — the program does **not** enforce a memory check. With limited memory, applying a large image may simply be slower or fail.
:::

The **target systems** you can install also include much older releases such as XP / 2003. The desktop host requirement and the target-system range are separate; see [Which systems can it install](/guide/what-is-letrecovery) for details.

## 1. Download

Get the latest **full package** from [GitHub Releases](https://github.com/NORMAL-EX/LetRecovery/releases) — it's a single `LetRecovery.7z` with WinPE built in.

::: warning Use the full package
Extract the **entire** `LetRecovery.7z`; don't copy out `LetRecovery.exe` on its own — it also needs the packaged `bin\` directory, `libwim-15.dll`, VC++ runtimes, and WinPE resources. Both current clients use native Win32 interfaces and **no longer depend on `opengl32.dll`**. Do not copy that DLL from an old package or mix files from different releases.
:::

## 2. Run as administrator

Extract the archive to a folder, then right-click `LetRecovery.exe` → **Run as administrator**.

::: tip Reinstalling the system drive?
If you're reinstalling **C:**, extract LetRecovery to **another drive** (for example `D:`). During installation C: gets formatted, and everything on it — including LetRecovery's own logs — is wiped.
:::

## 3. Choose an image

On the **System Installation** page:

1. Pick a local image (`Browse…`), or get one via **Online Download**.
2. Select the **edition** within the image (such as Pro / Home).
3. Select the **target partition**.

> Want the most hassle-free option? Skip these details and use [Easy Mode](/guide/easy-mode) directly: pick a system, pick an edition, confirm.

## 4. Start the installation

Click **Start Installation**.

- Installing to a **non-system** partition, or running **inside WinPE** → format and deploy **directly**.
- Installing from the desktop to the **currently running system drive** → WinPE boot is prepared automatically, then the machine reboots into WinPE to finish.

::: danger Back up first
Installation formats the target partition, so **back up important data first**.
:::

## Where are the logs

When reporting an issue, please attach:

- **After a successful ViaPE installation (preferred attachment)**: the normal-system and PE logs are merged into `<new-system drive>\LetRecovery\Logs\LetRecovery-install-<SessionId>.log`.
- **If installation fails after formatting or the first target write**: the combined log is written, when possible, to `<data drive>\LetRecovery_Data\LetRecovery\Logs\LetRecovery-install-<SessionId>.log`.
- **If failure occurs before handoff or no combined log exists**: use **About → Open log directory** for `<directory containing LetRecovery.exe>\log\LetRecovery.<date>.log` (for example, `LetRecovery.2026-06-26.log`). The raw PE log is `<directory containing LetRecoveryPE.exe>\LetRecoveryPE.log`; the official package defaults to `X:\Program Files\LetRecoveryPE\LetRecoveryPE.log`.

## How to check the version number

The software version number is generated from the **build date**, in the form `v2026.8.7`. You can find it on the **About** page or in the “version” line at the start of the log. Including it when reporting a problem helps with diagnosis.

## Next steps

- [System Installation](/guide/system-install)
- [System Backup](/guide/system-backup)
- [Reinstalling a BitLocker-encrypted drive](/guide/bitlocker)
- [Command line and unattended installation](/reference/command-line)
- [FAQ](/guide/faq)
