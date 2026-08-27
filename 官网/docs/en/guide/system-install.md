---
title: System Installation
description: Deploy a Windows image to a partition with LetRecovery.
---

# System Installation

The **System Installation** page is used to deploy a Windows image to a partition.

## Supported image formats

| Format | Description |
| --- | --- |
| **WIM / ESD / SWM** | Standard Windows images applied through wimlib. Every consecutive SWM span is locked and verified as one source set. |
| **GHO / GHS** | A single Ghost image or a consecutive split set restored through the bundled Ghost engine; every GHS span must be present. |
| **ISO** | Mounted automatically, using the `install.wim` / `install.esd` / `install.swm` inside. |
| **XP / 2003 i386** | An original XP/2003 installation disc (with `\I386` at the root and no `install.wim`) is recognized as text-mode setup media; see [Windows XP / 2003 Installation](/guide/xp-install). |

After picking an image, select the **edition** to install (the image index). The edition dropdown automatically filters out volumes that cannot be installed (such as WindowsPE / Setup media volumes) and selects the first installable edition by default.

Both direct mode and install via PE support complete SWM/GHS span sets. The normal-Windows client copies every consecutive span from retained original-file handles into a protected random session directory, then binds each file's order, length, and SHA-256 into the authenticated manifest. PE performs a fresh exact inventory and rejects missing, extra, out-of-order, or non-consecutive spans; it never rediscovers files from a public directory using only the primary span path.

## Selecting the target partition

Select the target partition from the list. The table shows capacity, free space, volume label, partition table (GPT/MBR), BitLocker status, and whether the partition already has a system.

For install via PE, the normal Windows endpoint writes a per-session random marker to the selected volume. After reboot, only the unique marker whose contents exactly match the authenticated session is selected. Same-name files with different contents, malformed files, and unreadable files on other volumes are ignored rather than treated as installation failures. Cross-reboot target discovery does not depend on cached disk numbers, partition numbers, capacity, labels, or a whole-disk layout.

When an automatic data partition is needed, its capacity is exactly the total logical size of every image span, online OEM driver package, PCA package, versioned user-driver file, and UefiSeven file that will be copied, plus 2 GiB. Driver size is measured in place by resolving existing Driver Store packages through Windows SetupAPI; LetRecovery does not copy drivers just to count them and does not estimate from an image percentage or a fixed 8–12 GiB guess.

## Partition reinstall, full-disk reinstall, and dual boot

- **Partition reinstall** changes only the target partition selected in the list.
- **Full-disk reinstall** lists every internal computer disk included in this task while still in normal Windows and requires a final explicit confirmation. Existing Windows installations, partitions, and personal files on those listed disks are deleted. Same-disk staging is retained until the image, drivers, configuration, and boot setup have completed; a later reclaim failure reports a completed installation with a warning rather than reversing the result.
- **Create dual boot** shrinks the user-selected source volume in normal Windows and pre-creates the new Windows volume in the same transaction. If no other data volume has enough space, that one shrink also creates a data volume whose minimum capacity is the exact installation-file total plus 2 GiB. WinPE consumes only the pre-created volume matched by this session's random marker; it never shrinks again or guesses a replacement target. A failure before the first destructive write removes the task-created volumes and extends the source back. Once deletion, formatting, or image writing starts, LetRecovery never pretends that the old system was restored.

The full-disk and dual-boot paths have passed code review and non-destructive simulation tests. GPT/MBR layouts, same-disk staging, non-integral-MiB provider extents, and the dual-boot menu still require disposable-VM validation; this is not a claim of physical-disk verification.

::: tip Is the target drive BitLocker-encrypted?
If the target is locked, installation first requires it to be unlocked, but “unlocked” does not mean it remains available across reboot. The current authenticated **install via PE** does not pass through a recovery key: the target must be fully decrypted to NotEncrypted before handoff or the task fails closed. See [Reinstalling on a BitLocker-encrypted drive](/guide/bitlocker).
:::

## Installation methods

LetRecovery decides how to install automatically:

- **Direct mode** — when the target is **not** the currently running system drive, or when LetRecovery **is already running inside WinPE**. It formats the partition and applies the image directly.
- **Install via PE** — when installing a system from the desktop onto the **currently running system drive**. LetRecovery stages the temporary WinPE payload on the actual system volume returned by Windows APIs, reboots into it, and then the [PE client](/guide/what-is-letrecovery) formats the target system volume and applies the image. The system volume does not have to be `C:`. A running system can't overwrite itself, so a reboot is required.

If a power loss or abnormal exit leaves `LetRecovery_PE` artifacts from a previous task, the next task first rolls back its trusted boot transaction and removes only precisely named, ordinary LetRecovery files. Unknown files, directories, and links are never recursively deleted merely because they are inside a same-named folder.

## Pre-deployment image verification

Direct mode and **install via PE** both verify and lock the complete source set **before** the target is formatted (WIM/ESD/SWM via wimlib). A corrupt image or a changed, missing, or extra span is rejected **before the first target write**.

::: warning GHO/GHS has a different verification scope
- GHO images only undergo a structural head/tail check, not wimlib verification.
- The complete GHS set, ordering, lengths, and SHA-256 values are bound, but internal Ghost-format semantics remain the responsibility of the Ghost engine.
:::

## Distinguishing fresh images from existing accounts

Unattended setup, account changes, preinstalled software, and first-logon tasks are applied only when the Windows image can be confirmed as a fresh deployment template. PE actually opens the offline `SAM` through a read-only application-hive handle, cross-checks the RID records against the name index, and combines that result with Windows Setup image state. An image is treated as fresh only when it is resealed to OOBE, the SAM inventory succeeds, and no real user account is present.

Windows-owned and setup accounts such as Administrator, Guest, DefaultAccount, defaultuser0, and WDAGUtilityAccount are not mistaken for users. An account with RID 1000 or greater whose name is not in that narrow system-account set is user-owned. Captured/backup images, completed installations, images containing a real user, and images whose SAM cannot be read or whose two indexes disagree all preserve their accounts and passwords and skip state-overwriting options; `ImageState` alone never promotes them to fresh installs.

## Failure and automatic rollback

While the original system is still intact—before target-partition deletion or formatting starts and before the image engine receives its first target write—a failed preflight automatically removes this session's PE boot entry, authenticated control files, and any temporary partition that can be safely proven. Once deletion or formatting starts, or an unformatted install hands the target to the image engine, the operation is irreversible. Later failures never pretend to reconstruct the previous Windows installation or boot state; LetRecovery preserves diagnostics and requires a reinstall or manual recovery instead.

If the core installation succeeds but staging reclaim, an optional driver, or another non-bearing step fails, the result is shown as “completed with warnings.” Interactive PE remains on that warning page so the user can read and handle it; the ordinary auto-reboot preference no longer powers the machine off a few seconds later. Only an explicitly enabled unattended-automation terminal action may still reboot or shut down.

## Boot mode

LetRecovery automatically detects UEFI / Legacy (GPT→UEFI, MBR→Legacy) and writes the matching boot files. When needed, you can also manually specify `Auto / UEFI / Legacy` in the **Boot mode** dropdown. This dropdown is always visible; when unattended is enabled, it shares a row with the "Customize unattended" button, otherwise it occupies its own row.

For supported Windows 10/11 and Server 2016+ UEFI images, LetRecovery also checks the Secure Boot PCA2011/PCA2023 trust generation before disk writes and uses bundled offline resources when an older image lacks BootEx. **Auto** is the recommended setting; see [Secure Boot and PCA2011 / PCA2023](/guide/secure-boot-pca) for the full decision process and legacy Windows limits.

## Drivers and options

Before starting, you can enable driver export/import, disk-controller driver injection, unattended setup, registry tweaks, Wi-Fi migration, and more in [Advanced Options](/guide/advanced-options). The final install intent selects Windows 7 USB3/NVMe/UEFI resources automatically from the image, architecture, and hardware, leaving only the limited 0xA5 manual compatibility attempt; XP/2003 still expose their own dedicated options.

The first time the **Preinstalled software** dialog opens, LetRecovery performs a read-only inventory of machine-wide software in the selected target Windows installation. It preselects only the intersection of that inventory and the current server catalogue. Matching requires an exact product name or an explicit version/architecture suffix; broad substring matching is not used. Locally installed products absent from the server catalogue are not shown, and an explicit manual clear is never re-populated automatically.

## Generate an automation configuration

Enable **Show automation export (advanced)** on the About page to reveal **Generate Automation** at the far left of the Installation command bar. It writes the current target, image edition, install mode, driver, unattended, software, and advanced settings to `cli\install.json`, with `cli\run-install.cmd` as the launcher; generation does not start installation. Review the JSON first, then run the CMD as administrator. Disk numbers and paths inside a currently mounted ISO are specific to the current hardware and mount session, so regenerate after either changes. JSON containing a password or Wi-Fi profile is protected with a restricted ACL and CLI output is redacted, but the file should still be handled as a credential.

::: danger
A partition reinstall will **format the target partition**; a full-disk reinstall clears the internal disks listed in its confirmation. Back up first.
:::
