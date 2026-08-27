---
title: BitLocker Reinstall
description: How LetRecovery reinstalls a system disk encrypted with BitLocker.
---

# BitLocker Reinstall

Reinstalling a system disk protected by **BitLocker** can fail under WinPE because an encrypted volume may lock again after reboot. Install and backup handoffs never place recovery passwords in public configuration. The default-off PE maintenance entry uses a separate authenticated private payload which install and backup tasks cannot consume.

## How it decides

The current production boundary requires an install-via-PE target to reach **NotEncrypted** before handoff:

- When the target is unlocked and can be safely decrypted, the normal-Windows client starts and waits for **full decryption** before creating the handoff.
- A locked target, encryption/decryption in progress, unknown state, or a volume still encrypted at handoff fails closed. LetRecovery does not continue by scanning drive letters or placing a recovery key in public configuration.
- ViaPE backup likewise requires both source and destination to be positively NotEncrypted; it neither reads nor persists a recovery key.

When `pe_maintenance_entry_enabled` is enabled in `config.json`, the normal-Windows Toolbox shows **Enter PE maintenance environment**. The normal client makes a best-effort attempt to collect only currently exportable 48-digit recovery passwords, strictly normalizes and deduplicates the bounded set, and places it in this session's private LRPE4 boot artifact. Public config and manifest bytes contain only its length and SHA-256. After authenticating that exact task, PE tries `unlock` against currently locked, lettered volumes. A missing password, mismatched password, or status-query failure never prevents access to the maintenance desktop.

This maintenance path never calls `manage-bde -off`, removes or suspends protectors, or starts full-volume decryption. The password set carries no drive letter, label, capacity, disk number, or cross-boot fingerprint; malformed content, stale-session files, and unauthenticated payloads are rejected. The NotEncrypted boundary for installation and ViaPE backup remains unchanged. A freshly installed system does not inherit the old volume's BitLocker state; enable BitLocker again after installation if needed.

## Unlock ≠ decrypt

**Unlocking** only makes the encrypted volume readable for the current boot session; it may lock again after reboot. **Decrypting** removes BitLocker encryption from the volume. Because these are different states, the current ViaPE production path never treats “unlocked in the online OS” as cross-reboot authorization.

## Managing BitLocker manually

The [Toolbox](/guide/toolbox) includes **BitLocker management**:

- **Unlock** (password / recovery key) and **decrypt** the entire disk;
- **Suspend / resume** protection—after suspending, the key is stored in plaintext and remains valid across reboots; this is commonly used to temporarily turn protection off before changing the BIOS/firmware and then resume afterward, with **no** need to re-encrypt the whole disk;
- **View the recovery key**.

::: tip Secure Boot and the bundled PE
LetRecovery boots WinPE through the target machine's own **Windows Boot Manager (bootmgfw)** rather than bundling its own bootloader, so it also works on machines with Secure Boot enabled.
:::
