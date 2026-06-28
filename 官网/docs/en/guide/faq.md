---
title: FAQ
description: Frequently asked questions about LetRecovery.
---

# FAQ

## Does LetRecovery cost money?

No—it's free forever and open source. Please only get it from
[GitHub Releases](https://github.com/NORMAL-EX/LetRecovery/releases) or
the [official website sysre.cn](https://sysre.cn). Anyone charging you is a third-party reseller with no connection to this project.

## Which is the real official website?

- **Official homepage** (three domains, same content)—<https://sysre.cn> / <https://letrecovery.net> / <https://letrecovery.cn>
- **Source code**—<https://github.com/NORMAL-EX/LetRecovery>

Be wary of any other so-called "official sites" or "download sites."

## Can it run inside WinPE?

Yes. The desktop `LetRecovery.exe` automatically detects WinPE and **installs in place** (no reboot needed). Inside PE, run the **full package**—don't copy out just the single exe; it needs the accompanying DLLs and `bin\`.

## Which image formats are supported? Which systems can it install?

Image formats: WIM, ESD, SWM, GHO, ISO, and the i386 text-mode setup media of original XP/2003.
Installable target systems cover Windows **XP / 2003 / 7 / 8 / 10 / 11**.

## What configuration is required to run? Do I really need 4 GB of RAM?

Running the desktop client requires Windows 10/11 (64-bit) plus administrator privileges. At least 4 GB of free memory is recommended, but this is only a **recommendation**—the program does **not** enforce a memory check.

## Is BitLocker supported?

Yes—before reinstalling, it automatically unlocks (key pass-through) or decrypts an encrypted system drive. See
[Reinstalling on a BitLocker-Encrypted Drive](/guide/bitlocker).

## Is there a simpler "one-click reinstall"?

Yes, try [Easy Mode](/guide/easy-mode): pick the system, pick the edition, confirm—done, and common optimizations are applied automatically.
(Easy Mode is not available under WinPE.)

## Installation failed—where are the logs?

- Desktop: `<program directory>\log\LetRecovery.<date>.log`
- PE: `X:\Program Files\LetRecoveryPE\LetRecoveryPE.log`

If you're reinstalling C:, run LetRecovery from **another drive** so the log isn't wiped by formatting.
When [reporting an issue](https://github.com/NORMAL-EX/LetRecovery/issues), please attach the log.

## Deployment reports "invalid compressed data" / error code 2

The image is corrupted or wasn't downloaded completely (this is wimlib's "decompression failure" error). Please re-download the image and verify it first with
**Toolbox → Image Verification** before installing.

## Can I change the interface language?

Yes. The **About** page has a language selector (it ships with Simplified Chinese, plus English (US) bundled in). To add a language, drop a properly formatted `<language code>.json` into the `lang\` directory and click "Refresh language list."

## Which engine is used to apply images? Can I switch it?

By default it uses the built-in **libwim**, and you can switch to the system-native **wimgapi** on the **About** page (on failure it automatically falls back to libwim).
See [Image Engine](/guide/wim-engine).

## Will releasing a new version change the website's version number?

Yes. Day-to-day development happens on separate branches or repositories and doesn't affect the live site; but once those changes pass review and are merged into the LetRecovery repo's `main` branch, EdgeOne Pages rebuilds and redeploys the website. Since the website's version number is generated from the build date (e.g. `v2026.06.07`), it updates along with that rebuild.

## Why don't I see `libwim-15.dll` inside the PE image?

It's compiled into the exe (`LetRecoveryPE.exe` / `LetRecovery.exe` share the same mechanism) and is **only extracted to the exe's own directory at runtime**, so it's not visible statically—it appears next to the exe after the first run.
