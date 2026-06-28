---
title: BitLocker Reinstall
description: How LetRecovery reinstalls a system disk encrypted with BitLocker.
---

# BitLocker Reinstall

Reinstalling a system disk protected by **BitLocker** usually fails under WinPE—the encrypted volume is locked, so it can neither be read nor formatted. LetRecovery handles this automatically.

## How it decides

When you start an installation on an encrypted system disk, LetRecovery determines whether it can obtain the **target disk's recovery key** (the 48-digit numeric recovery password):

- **It can → key pass-through.** The recovery key is packed into the WinPE boot image (written to `\LR_BitLockerKeys.txt` inside `boot.wim`). After rebooting into WinPE, it is used to **unlock** the disk, which is then formatted and deployed. This path is fast—no lengthy decryption required.
- **It can't → fall back to full decryption.** If the target disk has only a TPM protector and no numeric recovery password (so no key can be passed through), LetRecovery **fully decrypts** the BitLocker volume from within the running system before rebooting.

Either way, the result is the same: WinPE can access the disk, and the freshly installed system is **no longer encrypted** (re-enable BitLocker yourself after installation if you need it).

::: details How the PE client uses the passed-through keys
The PE client reads `X:\LR_BitLockerKeys.txt` and tries each key against every drive letter one by one (each key carries its own checksum, so a mismatch is rejected—there's no need to map keys to volumes individually). Under the hood it calls `fveapi.dll` first, falling back to the `manage-bde` command line if that fails.
:::

## Unlock ≠ decrypt

On the pass-through path, WinPE merely **unlocks** the volume (supplying the key so the file system becomes readable)—at this point it is still a full BitLocker volume. The subsequent **format** wipes the BitLocker metadata along with the old data, so the new system ends up unencrypted.

## Managing BitLocker manually

The [Toolbox](/guide/toolbox) includes **BitLocker management**:

- **Unlock** (password / recovery key) and **decrypt** the entire disk;
- **Suspend / resume** protection—after suspending, the key is stored in plaintext and remains valid across reboots; this is commonly used to temporarily turn protection off before changing the BIOS/firmware and then resume afterward, with **no** need to re-encrypt the whole disk;
- **View the recovery key**.

::: tip Secure Boot and the bundled PE
LetRecovery boots WinPE through the target machine's own **Windows Boot Manager (bootmgfw)** rather than bundling its own bootloader, so it also works on machines with Secure Boot enabled.

Going further: when Secure Boot is enabled, the host system's `winload.efi` contains the 2023 certificate, and it matches the PE kernel version (the `10.0.19041.` series), the program will **make a best effort** to overwrite PE's corresponding file with the host's dual-signed (2011+2023) `winload.efi`, so that PE can boot even on machines where the 2011 certificate has been revoked (the CVE-2023-24932 DBX update). This is a **best-effort** optimization that triggers only when the above conditions are met.
:::
