---
title: System Backup
description: Use LetRecovery to back up a Windows partition into an image.
---

# System Backup

The **System Backup** page captures a partition into an image file.

## Formats

| Format | Use case | Compression |
| --- | --- | --- |
| **WIM** | Standard format with good compatibility (recommended). | LZX |
| **ESD** | Higher compression ratio, smaller files. | LZMS (solid) |
| **SWM** | Not enabled on the current production backup path. | — |
| **GHO** | Not enabled on the current production backup path. | — |

Only **WIM / ESD** currently have the complete stable-volume rebind, same-volume private staging, completed-image verification, and atomic publication chain. SWM/GHO fail closed instead of falling back to legacy drive-letter or non-transactional writes.

## Steps

1. Select the **source partition** to back up.
2. Choose a **format** and a **save location**.
3. Enter a **name** (required) and a **description** (optional).
4. For an existing target, choose whether to **append an index** or untick the option for a **full replacement** (see below).
5. Click **Start Backup**.

::: tip Name is required
The "Start Backup" button only becomes available once the source partition, save location, and name are all filled in; the description can be left empty.
:::

## Create, Replace, and Append

WIM/ESD has three explicit output policies:

- A missing target means **create**. If another program claims that name before capture, the task stops.
- When **Browse…** selects an existing WIM/ESD, “Incremental backup (append to an existing image)” is ticked automatically. Leave it ticked to **append a new image index** inside a private copy.
- Untick it for an existing target to request a **full replacement**. LetRecovery builds and verifies a complete new image instead of truncating the old file in place.

::: tip Existing images are never modified in place
Replace and append both begin from an old-file handle that denies write/delete sharing and build in same-volume private staging. Publication occurs only after complete verification, through a durable journal and handle CAS. Failure preserves the old target; crash recovery converges from live file identities and hashes rather than trusting path names alone.
:::

## Backing Up the Running System

The **current system partition** cannot be reliably backed up in place while it is in use, so the graphical client handles it **via WinPE**. The normal-Windows client creates an authenticated private PE session; after reboot, the native PE progress interface consumes the same WIM/ESD create, replace, or append intent. A non-system partition can be backed up directly.

The public normal-Windows CLI also supports WIM/ESD create, replace, and append, but only for `auto|direct` tasks that do not require PE and with `auto_reboot=false`. A system-volume task requiring PE, explicit `via_pe`, or automatic reboot fails closed during planning. PE exposes no public CLI.

After enabling **Show automation export (advanced)** on the About page, **Generate Automation** at the far left of the Backup command bar writes the current source, destination, name, format, and append/replace intent to `cli\backup.json` and `cli\run-backup.cmd`; it does not start a backup. The public CLI currently supports WIM/ESD only, so SWM/GHO selections produce an explicit export error instead of being silently changed.

::: tip Automatic verification
Both execution modes verify the completed image before atomic publication. A verification or publication failure is never presented as a successful backup or published as a partial image.
:::
