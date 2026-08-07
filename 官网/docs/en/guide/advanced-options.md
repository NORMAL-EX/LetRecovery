---
title: Advanced Options
description: Drivers, unattended setup, registry tweaks, and system optimization.
---

# Advanced Options

Open **Advanced Options** on the System Installation page to fine-tune the deployment.

## Drivers

- **Export / import drivers**—keep third-party drivers around after the reinstall. Export uses the official **DISM API** (`DismExportDriver`), falling back to a manual DriverStore export on failure.
- **Disk controller driver injection** — on supported targets, import a locked storage-controller package. LetRecovery reads the current machine's complete PCI hardware IDs through SetupAPI and imports only one uniquely matched package whose signature and hashes pass verification. It never guesses on ambiguous hardware or recursively injects the whole driver directory.

## Unattended

- Use the built-in generated `unattend.xml`, or pick your **own** unattended file.
- Choose an ordinary custom username, or enable the built-in Administrator identified by RID 500; you can also customize the system-drive volume label. The Administrator password exists only in the current install session and answer file, never in persistent preferences or logs.
- The program also **auto-detects** the target partition, the installation media root, and whether the image already ships its own answer file, and checks unattended on by default accordingly.

::: tip Scope of a custom answer file
A custom `unattend.xml` is fully copied into the target system and takes effect during the **install via PE** flow (this is also the main path for reinstalling the system drive from the desktop). A custom `winnt.sif` for XP/2003 likewise takes effect during its text-mode setup flow.
:::

## System Optimization

Applied to the newly deployed system:

- Remove preinstalled UWP apps <Badge type="tip" text="Needs unattended" />
- Bypass the OOBE "must connect to the internet" requirement (BypassNRO) <Badge type="tip" text="Needs unattended" />
- Disable Windows Update
- **Deep-remove the Microsoft Defender Antivirus engine** — this affects only the antivirus engine, its drivers, and Defender's own scheduled tasks; Windows Security, UAC, Firewall, SmartScreen, VBS, and Defender for Endpoint remain intact
- Restore the classic right-click menu on Win11, remove shortcut overlay arrows
- Disable UAC, the system reserved space, and automatic device encryption

::: warning Items that need unattended
"Remove preinstalled UWP apps", "Bypass the OOBE internet requirement", and "custom username" require unattended support. When the target partition **already ships** its own answer file, these items are disabled and forcibly unchecked (unless you also check format partition).
:::

## WiFi Configuration Migration

Bring the current machine's Wi-Fi configuration into the new system. LetRecovery obtains the current profile through a controlled Windows WLAN API; when there is no current profile to migrate, the option is hidden. It does not parse localized `netsh` output.

## Windows 7 Compatibility Policy

USB3, NVMe, and UEFI handling for Windows 7 is now **selected automatically by the final install intent**. There are no manual checkboxes, custom folders, or browse buttons for these resources:

- **USB3** — once the image is confirmed as Windows 7, a verified driver is selected from the locked manifest by current hardware ID and target architecture.
- **NVMe** — only Windows 7 x64 on a target disk positively identified as native NVMe receives the locked Microsoft hotfix CABs in their fixed dependency order. VMD/RAID, an unknown bus, and x86 never enable this by guesswork.
- **UEFI** — evaluated automatically for Windows 7 x64 when boot repair is enabled and UEFI may be used. A positively identified VMware guest keeps native Microsoft dual boot entries; other environments use a transactional, verified UefiSeven dual-entry deployment and require Secure Boot to be disabled.

One manual compatibility attempt remains and is off by default: **try the 0xA5 workaround (disable processor power drivers)**. It disables only the offline `intelppm`, `amdppm`, and `Processor` services. It does not modify ACPI tables, `acpi.sys`, or firmware and is not a general 0xA5 fix.

::: warning Retired legacy 0x7B switch
The historical “fix storage-controller BSOD” field remains parseable only for old configurations and is forced off. LetRecovery does not set a broad list of unrelated IDE/AHCI/RAID/NVMe services to Boot Start.
:::

## Windows XP / 2003-Specific Switches

When an XP/2003 image is detected, its own USB3 / NVMe options appear; AHCI drivers are **always injected**, and “UEFI-enabled” images use a separate UEFI/GPT boot path. See
[Windows XP / 2003 Installation](/guide/xp-install) for details.
