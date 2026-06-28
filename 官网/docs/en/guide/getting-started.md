---
title: Getting Started
description: Download and run LetRecovery, and complete your first reinstall.
---

# Getting Started

## Requirements

To run the LetRecovery **desktop client**:

- Windows 10 / 11 (64-bit) — the desktop client enforces a 64-bit check, so it cannot run on 32-bit systems
- Administrator privileges (launching as a non-administrator automatically triggers a UAC elevation prompt)
- UEFI or Legacy BIOS, either works

::: tip About memory
The official recommendation is at least **4 GB of free memory**, but this is just a rule of thumb — the program does **not** enforce a memory check. With limited memory, applying a large image may simply be slower or fail.
:::

The range of **target systems** you can install goes well beyond 10/11. See
[Which systems can it install](/guide/what-is-letrecovery) for details.

## 1. Download

Get the latest **full package** from [GitHub Releases](https://github.com/NORMAL-EX/LetRecovery/releases) — it's a single `LetRecovery.7z` with WinPE built in.

::: warning Use the full package
Extract the **entire** `LetRecovery.7z`; don't copy out `LetRecovery.exe` on its own — it needs the accompanying runtime DLLs such as `bin\`, `opengl32.dll`, and `libwim-15.dll` to work (especially inside WinPE).
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

- **Desktop client**: `<program directory>\log\LetRecovery.<date>.log` (e.g. `LetRecovery.2026-06-26.log`)
- **PE client**: `X:\Program Files\LetRecoveryPE\LetRecoveryPE.log`, in the same directory as the PE exe

## How to check the version number

The software version number is generated from the **build date**, in the form `v2026.06.07`. You can find it on the **About** page or in the "version" line at the start of the log. Including it when reporting a problem helps with diagnosis.

## Next steps

- [System Installation](/guide/system-install)
- [System Backup](/guide/system-backup)
- [Reinstalling a BitLocker-encrypted drive](/guide/bitlocker)
- [Command line and unattended installation](/reference/command-line)
- [FAQ](/guide/faq)
