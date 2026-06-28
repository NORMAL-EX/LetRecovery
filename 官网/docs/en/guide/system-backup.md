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
| **SWM** | Split storage (captured as WIM first, then split), easier to transfer. | LZX, split by size |
| **GHO** | Ghost format (requires the Ghost engine). | Ghost's own |

When you choose **SWM**, you must enter a **split size** (in MB, valid range **512–8192**).

## Steps

1. Select the **source partition** to back up.
2. Choose a **format** and a **save location**.
3. Enter a **name** (required) and a **description** (optional).
4. Tick **incremental backup** if needed (see below).
5. Click **Start Backup**.

::: tip Name is required
The "Start Backup" button only becomes available once the source partition, save location, and name are all filled in; the description can be left empty.
:::

## Incremental Backup

"Incremental backup (append to an existing image)" is a checkbox option that behaves as follows:

- When you use **Browse…** to select an **existing** image file, this option is **ticked automatically**; you can also tick it manually.
- In incremental mode, LetRecovery **appends a new image index (INDEX)** to the existing image file,
  rather than rewriting the whole file or adding a new "split".
- Appending is only valid for **WIM / ESD**; SWM and GHO are always captured fresh each time.

::: tip Incremental ≠ split
A "split" is the concept of SWM dividing one image into multiple parts; "incremental" stores an additional version (index) inside the same WIM/ESD.
Don't confuse the two.
:::

## Backing Up the Running System

The **current system partition** can't be reliably backed up in place while it's in use, so LetRecovery does it **via WinPE**:
it first prepares the PE boot, writes this backup's name/format/incremental and other options into a config file, and after rebooting into WinPE,
the PE client reads that config and then captures the image. Backing up a **non-system** partition (or running inside PE) proceeds directly.

::: tip Automatic verification
Before capturing/deploying, the PE client verifies image integrity, so a corrupted image fails early and you avoid redoing the whole job.
:::
