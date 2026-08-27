---
title: Advanced Options
description: Drivers, unattended setup, registry tweaks, and system optimization.
---

# Advanced Options

Open **Advanced Options** on the System Installation page to fine-tune the deployment.

## Reinstall while preserving personal files

When enabled, LetRecovery does not format the selected Windows partition. In authenticated PE it moves each ordinary local profile's **Desktop, Documents, Downloads, Pictures, Music, and Videos** on the same volume into `LetRecovery_Preserved_<session-id>` at the drive root, then really deletes the old `Windows`, `Program Files*`, `ProgramData`, and remaining old-profile data so their space is immediately reusable. Unknown top-level data directories are left alone. This is not a full system backup and does not preserve `AppData`, applications, or system settings.

The new system automatically restarts once during first sign-in (the built-in Administrator option reuses its existing account-transition restart). On the second sign-in, LetRecovery shows a restore waiting screen, merges the files into the signed-in user's actual Windows Known Folders, and reads the result back. The Windows desktop starts only after that succeeds. Windows-generated `desktop.ini` folder-presentation metadata is ignored; it is not restored as personal data and does not become a visible conflict copy. If restore fails, the desktop remains closed and the preserved source plus diagnostics are not misreported as success.

This option supports only a single-partition reinstall over an existing Windows installation using a Windows 7 or later WIM/ESD/SWM image. LetRecovery turns formatting off and enters PE automatically. GHO/XP, full-disk, and dual-boot installations are unsupported. A reparse point, EFS-encrypted file, or online-only placeholder inside a preserved directory stops the operation before old-system deletion. In preserved Desktop folders, PE removes `.lnk` files positively identified as targeting either the original C: drive or the authenticated offline-system volume currently assigned by PE, outside that volume's `Users` directory. Links to other drives, networks, relative targets, or targets that cannot be parsed reliably are retained.

## Drivers

- **Export / import drivers** — keep third-party drivers around after reinstall. Online export prefers Windows' DISM and falls back to supported SetupAPI enumeration; offline export remains DISM-only and never reconstructs DriverStore by hand. Restore first asks Microsoft DISM to import the directory, then isolates exact INFs if the batch fails. Real failures of optional non-boot packages such as Wi-Fi, network, printer, or virtual-machine drivers are recorded and skipped, while boot-storage coverage is still read back and enforced. LetRecovery does not put a second complex signature verifier in front of DISM for ordinary saved drivers, and it never enables `/ForceUnsigned` automatically.
- **Disk controller driver injection** — on supported targets, import a locked storage-controller package. LetRecovery reads the current machine's complete PCI hardware IDs through SetupAPI and imports only one uniquely matched package whose signature and hashes pass verification. It never guesses on ambiguous hardware or recursively injects the whole driver directory.

## Unattended

- Use the built-in generated `unattend.xml`, or pick your **own** unattended file.
- Choose an ordinary custom username, or enable the built-in Administrator identified by RID 500; you can also customize the system-drive volume label. The Administrator password exists only in the current install session and answer file, never in persistent preferences or logs.
- The program also **auto-detects** the target partition, the installation media root, and whether the image already ships its own answer file, and checks unattended on by default accordingly.
- When the v4 software catalogue is available and the built-in unattended path is enabled, **Select preinstalled applications** opens a categorized checkbox list. The normal endpoint downloads the selected installers and includes their actual bytes in the data-staging budget; after applying Windows, PE copies the authenticated installers into the target and first logon runs each validated silent argument list, then deletes the installer. An individual installer failure is logged in the target system and does not skip the remaining cleanup.
- VMware Tools is excluded from the general picker. A separate, default-checked **Install VMware Tools** option appears only when VMware is positively detected and the v4 catalogue contains an item marked `vm_tools=true`.

::: tip Scope of a custom answer file
The current authenticated **install via PE** path does not accept a custom `unattend.xml`, custom `winnt.sif`, or Administrator password; those combinations fail closed before the first target write. They are copied only on supported direct-install paths. The built-in generated answer file remains supported through the PE path.
:::

## System Optimization

Applied to the newly deployed system:

- Remove the fixed curated AppX list while preserving new Outlook and OneDrive <Badge type="tip" text="Needs unattended" />
- Bypass the OOBE "must connect to the internet" requirement (BypassNRO) <Badge type="tip" text="Needs unattended" />
- **Disable Windows Update** — writes reversible policies and disables the Windows Update service. It does not delete update components or promise that manual or enterprise-managed updating can never be restored. Use the CLI `update restore` command to restore the managed settings.
- **Deep-remove the Microsoft Defender Antivirus engine** — affects only the antivirus engine, its drivers, and Defender's own scheduled tasks. It also makes a best-effort removal of only the exact `Microsoft.SecHealthUI_8wekyb3d8bbwe` and `Microsoft.Windows.SecHealthUI_cw5n1h2txyewy` packages. SecurityHealthService, the Windows Security Center service, UAC, Firewall, SmartScreen, VBS, and Defender for Endpoint remain intact. A non-removable SecHealthUI package is reported as a warning; UI removal is never presented as guaranteed success.
- Restore the classic right-click menu on Win11, remove shortcut overlay arrows
- Disable UAC, Reserved Storage, and automatic device encryption. Reserved Storage uses Microsoft's supported online DISM interface only when the target is confirmed as Windows 10/11 build 19041+ and the built-in answer file is in use. Unsupported or unconfirmed final state is warning-only; LetRecovery never writes the internal offline registry values.

The AppX option handles only the exact Name/PFN identities in the shared curated list: offline provisioning is revoked and all-user registrations are handled during the built-in unattended phase. New Outlook, OneDrive Sync, and Win32 OneDrive are explicitly preserved. On Windows 11, Start recommendations and preinstalled content delivery are disabled before the default user is created so dynamic Get Started, Solitaire, and Microsoft PC Manager entries are not regenerated; opaque Start cache files are never edited.

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
