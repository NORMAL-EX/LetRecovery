---
title: Advanced Options
description: Drivers, unattended setup, registry tweaks, and system optimization.
---

# Advanced Options

Open **Advanced Options** on the System Installation page to fine-tune the deployment.

## Drivers

- **Export / import drivers**—keep third-party drivers around after the reinstall. Export uses the official **DISM API**
  (`DismExportDriver`), falling back to a manual DriverStore export on failure.
- **Disk controller driver injection**—inject NVMe / RAID / AHCI drivers so the new system can boot on modern storage
  (automatically checked for Win10/11 x64 images).

## Unattended

- Use the built-in generated `unattend.xml`, or pick your **own** unattended file.
- Customize the auto-created username and the system drive's volume label.
- The program also **auto-detects** the target partition, the installation media root, and whether the image already ships its own answer file,
  and checks unattended on by default accordingly.

::: tip Scope of a custom answer file
A custom `unattend.xml` is fully copied into the target system and takes effect during the **install via PE** flow (this is also the main path for reinstalling the system drive from the desktop). A custom `winnt.sif` for XP/2003 likewise takes effect during its text-mode setup flow.
:::

## System Optimization

Applied to the newly deployed system:

- Remove preinstalled UWP apps <Badge type="tip" text="Needs unattended" />
- Bypass the OOBE "must connect to the internet" requirement (BypassNRO) <Badge type="tip" text="Needs unattended" />
- Disable Windows Update
- Disable **Windows Security (Defender)**
- Restore the classic right-click menu on Win11, remove shortcut overlay arrows
- Disable UAC, the system reserved space, and automatic device encryption

::: warning Items that need unattended
"Remove preinstalled UWP apps", "Bypass the OOBE internet requirement", and "custom username" require unattended support. When the target partition **already ships** its own answer file, these items are disabled and forcibly unchecked (unless you also check format partition).
:::

## WiFi Configuration Migration

Bring the current machine's WiFi configuration into the new system. When no WiFi is detected (`netsh wlan show interfaces` returns no interface), this option is automatically hidden.

## Win7-Specific Switches

The following extra switches appear for Win7 images:

- **UEFI patch (UefiSeven)**—lets Win7, which has no native UEFI support, boot via UEFI.
- **Inject USB3 / NVMe drivers**—required for Win7 install/boot on modern motherboards.
- **Fix ACPI BSOD (0xA5)**—disables processor power services such as `intelppm`/`amdppm`/`Processor`.
- **Fix storage controller BSOD (0x7B)**—sets a long list of storage services like `msahci`/`storahci`/`pciide`/`iaStor*`/`stornvme`
  to boot-start (writing to both `ControlSet001` and `ControlSet002`).

::: tip CABs in the Win7 driver folder
If you drop a `.cab` update package into the Win7 USB3 / NVMe driver folder (for example certain required kernel updates), it is automatically extracted before injection and injected along with the drivers. This is a convenience feature of Win7 driver injection, **not** a general-purpose "install system updates" optimization.
:::

## Windows XP / 2003-Specific Switches

When an XP/2003 image is detected, a set of switches parallel to Win7's appears: inject USB3 / NVMe drivers (checked by default), AHCI drivers **always injected**, and UEFI/GPT boot support for "UEFI-enabled" images. See
[Windows XP / 2003 Installation](/guide/xp-install) for details.
