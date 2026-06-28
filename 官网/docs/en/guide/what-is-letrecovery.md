---
title: What is LetRecovery?
description: An introduction to LetRecovery — a free and open-source tool for reinstalling and backing up Windows.
---

# What is LetRecovery?

**LetRecovery** is a free, open-source tool for **reinstalling and backing up Windows**. It can deploy system
images (WIM / ESD / SWM / GHO / ISO, including text-mode setup for Windows XP / 2003), back up your existing system,
download images online, and ships with a full set of common maintenance tools — all in one clean, ad-free interface.

It is built on **Rust + egui** and runs both under the normal Windows desktop and directly inside **WinPE**.
Its goal is to let even non-technical users reinstall Windows safely.

::: tip Always free
LetRecovery is free forever. Please obtain it only from [official channels](#where-to-get-it). Anyone charging you for it is a third-party reseller with no connection to this project.
:::

## Highlights

- **Multi-format installation** — WIM / ESD / SWM / GHO / ISO, automatic ISO mounting, and your pick of any edition inside the image;
  it also supports original [i386 text-mode setup for Windows XP / 2003](/guide/xp-install).
- **Dual desktop & WinPE clients** — when reinstalling the *current* system drive from the desktop, it automatically writes the boot entry and reboots into WinPE;
  run it directly inside WinPE and it installs in place.
- **Reinstall onto BitLocker-encrypted drives** — automatically unlocks or decrypts the BitLocker volume before deployment.
- **Full / incremental backup** to WIM / ESD / SWM / GHO.
- **Online download** of images, common software, and graphics drivers, with Aria2 multi-threaded acceleration.
- **Easy Mode** — [one-click reinstall](/guide/easy-mode): just pick the system, pick the edition, and confirm.
- **Complete Toolbox** — BitLocker management, password reset, image/hash verification, one-click partitioning, driver backup and restore,
  [Lossless C: Expansion](/guide/expand-c-drive), boot repair, and more.

## How it works

LetRecovery has two frontends that share the same core (`lr-core`):

| Component | Executable | Role |
| --- | --- | --- |
| **Desktop client** | `LetRecovery.exe` | The main interface. Runs both under normal Windows **and** in WinPE (PE is auto-detected). |
| **PE client** | `LetRecoveryPE.exe` | A streamlined, unattended install/backup interface used by the "auto-reboot into WinPE" flow. |

When installing to a **non-system** partition (or when you are already in WinPE), it deploys **directly**; when installing from the desktop to the **running
system drive**, LetRecovery first prepares the WinPE boot entry, reboots into it, and finishes inside PE — because a running
system cannot overwrite itself.

::: details How is PE detected?
On startup, the desktop client weighs several signals to decide whether it is currently running inside WinPE: whether the system drive is `X:`, whether
`X:\MININT`, `winpeshl.ini`, or `fbwf.sys` exist, the `MiniNT` registry key, and so on. Once PE is detected, the interface
automatically enables or disables the relevant features (for example, "One-click boot repair" is only available in PE).
:::

## Which systems can I install?

- **Running the LetRecovery desktop client** requires **Windows 10 / 11 (64-bit)**, or simply run it inside WinPE.
- **The installable target systems** cover a very wide range: Windows **XP / 2003 / 7 / 8 / 8.1 / 10 / 11**.
  Systems from different eras automatically enable the matching compatibility handling (such as USB3/NVMe injection and blue-screen fixes for Win7, and
  text-mode setup with storage-driver integration for XP).

## Where to get it

- **GitHub Releases** — <https://github.com/NORMAL-EX/LetRecovery/releases>
- **Official website** (three domains, same content) — <https://sysre.cn> / <https://letrecovery.net> / <https://letrecovery.cn>
- **Official documentation** (this site) — <https://docs.letrecovery.net>

The GitHub release is the **full package** (WinPE already bundled). See [Getting Started](/guide/getting-started) for details.

::: warning Verify the official domains
This project has three official homepages with identical content — **sysre.cn**, **letrecovery.net**, and **letrecovery.cn**;
its documentation is at **docs.letrecovery.net**, and its source code is at **github.com/NORMAL-EX/LetRecovery**.
Be cautious about any other "official site" or "download mirror."
:::

## License

LetRecovery is released under the
[PolyForm Noncommercial License 1.0.0](https://github.com/NORMAL-EX/LetRecovery/blob/main/LICENSE):
personal, research, and non-commercial use is permitted; **commercial use is prohibited**.
