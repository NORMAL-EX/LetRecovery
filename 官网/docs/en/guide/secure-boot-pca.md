---
title: Secure Boot and PCA2011 / PCA2023
description: Understand the two boot-signing generations, automatic selection, offline compatibility assets, and legacy Windows limits.
---

# Secure Boot and PCA2011 / PCA2023

PCA2011 and PCA2023 identify **signing trust generations** used by Windows EFI boot components. They are not Windows version numbers, and they cannot be selected safely by comparing only the version of `winload.efi`. With Secure Boot enabled, UEFI firmware validates EFI components against its allowed database (DB) and forbidden database (DBX). Whether the firmware trusts a signing generation determines which Windows Boot Manager it can start.

::: tip Keep Auto selected in normal use
LetRecovery combines firmware trust, the existing ESP, and the selected image version and architecture. Manually select PCA2011 or PCA2023 only when you understand the target firmware's Secure Boot configuration.
:::

## How the generations differ

| Item | PCA2011 | PCA2023 |
| --- | --- | --- |
| Common signing chain | Windows Production PCA 2011 | Windows UEFI CA 2023 / Windows Production PCA 2023 |
| Main purpose | Compatibility with existing Windows boot components and older firmware | The new Windows Secure Boot generation |
| Firmware requirement | The 2011 generation remains trusted in DB and the component is not blocked by DBX | The matching 2023-generation certificate is present in DB |
| LetRecovery behavior | Preserve a compatible legacy boot chain | Validate and deploy BootEx resources |

A certificate transition does not mean every old system stops booting on a particular date. The actual outcome depends on firmware DB/DBX contents, the signature and revocation state of the specific EFI component, and whether Secure Boot is enabled. LetRecovery therefore inspects observable machine state instead of guessing from the calendar.

## How automatic selection works

For a modern Windows UEFI installation, LetRecovery performs these read-only checks **before formatting the target partition**:

1. Read the Windows major version, build, and architecture of the selected WIM, ESD, or SWM image.
2. Inspect Secure Boot state, firmware trust for both generations, and whether PCA2011 is unusable.
3. Inspect the existing ESP and the image's `bootmgfw.efi` and `EFI_EX\bootmgfw_EX.efi`.
4. Validate EFI signatures and x86/x64 architecture instead of trusting filenames or one version string.
5. In Auto mode, preserve an existing compatible chain when possible. If PCA2011 is unusable, continue only after PCA2023 trust has been confirmed.

Unknown firmware trust, missing resources, invalid signatures, wrong architecture, or failed integrity checks stop the installation before disk writes. An unknown result is never presented as success.

## Images without PCA2023 files

Older Windows 10, Windows 11, and Server 2016 or later images may not contain a complete BootEx directory. The full LetRecovery package includes three locked offline resource families:

- Windows 10, Windows 11 21H2–23H2, and Server 2016/2019/2022 x64;
- Windows 10 x86;
- Windows 11 24H2 or later and Server 2025 or later x64.

LetRecovery selects a WIM by target build and architecture and extracts only the allowlisted `EFI_EX`, `FONTS_EX`, and optional `boot.stl` resources. Package size, SHA-256, signatures, architecture, and paths are checked at separate boundaries. The process is **fully offline** and does not download boot files during installation.

## BCDBoot compatibility fallback

After preparing the resources, LetRecovery first uses BCDBoot with `/bootex`. If the BCDBoot available in the current system or PE is too old to recognize that option, LetRecovery:

1. Uses standard BCDBoot to create the BCD and directory layout.
2. Writes the correct-architecture boot manager from verified BootEx resources.
3. Deploys the matching fonts and fallback entry.
4. Verifies the resulting boot generation in the ESP again.

This fallback supports older BCDBoot versions; it does not relax signature, architecture, or resource allowlist checks.

## Where the PCA control appears

| Target or boot mode | PCA control | Behavior |
| --- | --- | --- |
| Windows 10 / 11 or Server 2016+, UEFI, x86/x64 | Shown | Automatic detection with an explicit override |
| Legacy / BIOS installation | Hidden | Secure Boot and EFI PCA are not part of this chain |
| XP / 2003, Vista, Windows 7, Windows 8/8.1 | Hidden | Preserve the existing compatibility path; do not inject modern BootEx |
| ARM64 image | Unsupported | LetRecovery's current toolchain supports x86/x64 only |

## Windows 7 and UefiSeven

UefiSeven emulates the Int10h environment that Windows 7 expects on UEFI Class 3 machines without a CSM. It addresses legacy display boot compatibility; it **does not** upgrade the Windows 7 boot chain to PCA2023.

The UefiSeven loader bundled with LetRecovery is not Microsoft PCA2023-signed, so Secure Boot must be disabled for this path. Even if an owner enrolls a custom key and signs UefiSeven, the original Windows 7 components that follow still belong to the legacy trust chain. That is not a portable PCA2023 solution for normal users or arbitrary firmware.

For Windows 7/8.1, if Secure Boot is enabled and firmware no longer permits the old chain, LetRecovery stops before formatting. Copying Windows 10/11 BootEx files directly into Windows 7 cannot safely upgrade the entire boot chain.

## Should `winload.efi` be replaced?

Normally, no. UEFI firmware first validates Windows Boot Manager in the ESP; `winload.efi` is loaded later by the Windows boot chain. Seeing “2011” or “2023” in `winload.efi` alone does not determine which PCA mode the firmware should use.

LetRecovery no longer attempts to overwrite the bundled PE's `winload.efi` with a file from the running system, and it does not require those files to have matching versions. PCA2023 compatibility is focused on verified Boot Manager and BootEx resources and the final ESP.

## Troubleshooting boot compatibility

- Restore PCA mode to **Auto** first and install the latest stable firmware from the motherboard vendor.
- Do not copy an entire EFI directory from another computer. Its BCD, firmware variables, and disk identifiers are machine-specific.
- For Windows 7 with UefiSeven, confirm that Secure Boot is disabled.
- Keep the desktop or PE log. Signature inspection, firmware state, resource selection, and BCDBoot fallback decisions are recorded there.

References:

- [Microsoft: BCDBoot command-line options](https://learn.microsoft.com/windows-hardware/manufacture/desktop/bcdboot-command-line-options-techref-di)
- [Microsoft: Updating bootable media to use the PCA2023-signed boot manager](https://support.microsoft.com/topic/d4064779-0e4e-43ac-b2ce-24f434fcfa0f)
- [Microsoft: Windows Secure Boot key creation and management guidance](https://learn.microsoft.com/windows-hardware/manufacture/desktop/windows-secure-boot-key-creation-and-management-guidance)
