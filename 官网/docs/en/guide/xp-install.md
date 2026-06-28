---
title: Windows XP / 2003 Setup
description: Install Windows XP / 2003 with LetRecovery, covering both the i386 text-mode and UEFI-enabled image paths.
---

# Windows XP / 2003 Setup

LetRecovery offers fairly complete support even for these "ancient" systems. XP/2003 is entirely different from Win7+ — it has no
`install.wim`, no native UEFI support, and no NVMe/USB3 drivers, so it takes two dedicated paths.

::: warning General prerequisites
- XP/2003 **cannot** be installed in place on the "currently running system drive"; you must enter WinPE first.
- Setup formats the target partition, so back up first.
:::

## The two installation paths

| Path | Media | Boot | Best for |
| --- | --- | --- | --- |
| **i386 text-mode setup** | Original XP/2003 install disc (`\I386` at the root, no `install.wim`) | **Legacy / MBR only** | Wanting to use a genuine, original install disc |
| **UEFI-enabled image** | Community-modified XP x64 WIM that ships with `winload.efi` / a patched kernel / `bootxp64.efi` | **GPT + UEFI** (Legacy also works) | Modern NVMe machines, or wanting UEFI boot |

The program **automatically detects** which path to take based on the selected image: if the chosen image has `major_version == 5` (NT 5.x), it is treated as
XP/2003; after mounting the ISO, if no `install.wim/esd` is found but files such as `\I386\setupldr.bin` exist, it is recognized as
i386 text-setup media, and the UI shows a green notice: "Recognized as Windows XP/2003 i386 text-setup media."

## Path 1: original i386 text-mode setup

For **original install discs**, LetRecovery follows the mature NT5 hard-disk install approach (Microsoft's
`winnt32 /makelocalsource` convention of `$WIN_NT$.~LS` + `$WIN_NT$.~BT`): it copies the setup source to the target drive,
writes the boot files, and reboots into the blue-background text-mode setup.

Key points:

- **Legacy/BIOS + MBR only** — GPT/UEFI targets are blocked when setup is launched.
- **AMD64 architecture takes priority** — XP x64 / 2003 x64 media contain both `\AMD64` (the complete 64-bit source) and `\I386`
  (incomplete WOW support files); the program prefers the complete source and avoids mistakenly using the incomplete `\I386`. Pure 32-bit media with only
  `\I386` falls back normally.
- **Storage drivers integrated for the text phase** — so that AHCI/NVMe is recognized even in the blue-background text phase, setup scans the driver
  directories by architecture and merges them into `txtsetup.sif`.

### Unattended setup and product key

Before writing to disk, the program force-writes the few keys required for a hard-disk install (the most critical being `MsDosInitiated=1`, which declares "boot from hard disk,
not from CD"). Whether it is fully automatic depends on the product key:

| `bin\xp\productkey.txt` present? | Behavior |
| --- | --- |
| No (default) | Skips EULA/region/welcome, ignores driver signing, blank administrator password + first-time auto-logon; the graphical phase **only pauses on the "product key" page** |
| Yes | No pauses at all — truly fully automatic |

::: tip The tool ships no product key
For full automation, place a single line in `bin\xp\productkey.txt` with a key matching the edition/channel you are installing. For safety,
the text phase still has the user confirm "which partition to install to" and does not auto-select the drive, to avoid wiping the wrong one.
:::

## Path 2: UEFI-enabled XP x64 image (GPT + UEFI)

This path can deploy XP x64 under **GPT+UEFI** and inject NVMe / AHCI / USB3 drivers.

::: danger Must read: depends on a "UEFI-enabled" image
XP itself does not support UEFI, and the tool **cannot synthesize it out of thin air**. This path only works if the image already ships with:

- `WINDOWS\system32\winload.efi` (Vista's EFI loader) + a patched `ntoskrnl.exe`/`hal.dll`
- `WINDOWS\system32\drivers\ntoskrn8.sys` (an "ntoskrnl extender" that adds kernel exports for storage drivers)
- `WINDOWS\Boot\EFI\Microsoft\Boot\bootxp64.efi` + its BCD store

Ordinary original XP images do not meet these conditions, and the UEFI boot step will fail and **automatically fall back to Legacy (ntldr)**.
:::

## Driver injection

XP (NT 5.x) **cannot use DISM**. Instead, LetRecovery injects offline by "copying `.sys`/`.inf` + registering boot-start services in the offline SYSTEM hive
+ writing the `CriticalDeviceDatabase`."

Place drivers under the program's **`bin\drivers\xp\`** (the full package already ships with a set extracted from test images):

```
bin\drivers\xp\
├── ahci\   genahci + storport + ntoskrn8     （always injected）
├── nvme\   stornvme + storport + ntoskrn8
├── usb3\   amdxhc + amdhub30
├── amd64\  （64-bit XP/2003 text-phase user drivers）
└── x86\    （32-bit XP text-phase user drivers）
```

::: warning 32-bit XP needs 32-bit drivers
The bundled genahci/stornvme are **x64** and only work for 64-bit XP/2003. **32-bit XP** needs 32-bit drivers
(supply them yourself in `x86\`); 32-bit XP drivers for modern NVMe barely exist, and on most SATA machines switching the BIOS's SATA mode
to IDE/Compatibility lets you install without drivers.
:::

## Which path should you choose?

- You have an **original install disc** and the target machine is traditional SATA/BIOS → use **Path 1 (i386 text-mode)**.
- The target machine is **modern NVMe / you want UEFI boot** → **Path 2 (UEFI-enabled XP x64 image)** is more convenient.

::: tip Real-hardware regression
The XP/2003 install chain depends on a real reinstall environment, so we recommend regression testing on a VM/real machine first. When reporting issues, please attach the
`[XP-UEFI]`, `[XP-DRV]`, `[TXTDRV]` sections from the log.
:::
