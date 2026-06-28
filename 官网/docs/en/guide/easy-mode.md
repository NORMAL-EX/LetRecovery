---
title: Easy Mode
description: One-click reinstall for beginners — pick a system, pick an edition, confirm, and you're done.
---

# Easy Mode (One-Click Reinstall)

If you don't want to deal with partitions, drivers, or unattended setup, **Easy Mode** (beginner mode) boils the entire reinstall down to
three steps: **pick a system → pick an edition → confirm**, with common optimizations applied automatically.

## How to Enable

The Easy Mode toggle lives on the **About** page. Once enabled, the main interface switches to a simplified, card-based flow.

::: warning Desktop only
Easy Mode is **not available under WinPE** — it's built for the scenario of "reinstalling the system drive with one click from a Windows that can still boot normally."
:::

## Workflow

1. Pick the system you want to install from the system cards.
2. Pick the specific **edition** (such as Pro / Home).
3. Confirm to start — leave the rest to the program.

The image can be one you already have locally, or you can grab it via [Online Download](/guide/online-download).

## What It Does for You

Easy Mode automatically applies a set of settings that suit most people, saving you from ticking each one in [Advanced Options](/guide/advanced-options):

- **Bypass the OOBE online requirement** (BypassNRO)
- **Remove preinstalled UWP apps**
- **Import disk controller drivers** (NVMe / AHCI / RAID) and **auto-import** this machine's drivers
- Set the system drive's **volume label to `OS`**
- Take the **username** from the currently logged-in account
- Enable **unattended** setup, format the target drive, **repair boot**, and **reboot automatically** when finished

Because the target is usually the **system drive currently in use**, Easy Mode goes through the [install via PE](/guide/system-install)
flow: it first prepares the WinPE boot environment and reboots into it, then formats, applies the image, imports drivers, and repairs boot inside PE.

::: danger The system drive is still formatted
"Easy" refers to the operation being simple — it does **not** mean nothing gets wiped. Reinstalling the system drive still formats C:, so back up your important data first.
:::

## Want Finer Control?

When you need custom partitions, want to keep specific drivers, supply your own `unattend.xml`, or do more registry tweaks, turn off
Easy Mode and switch to the standard [System Installation](/guide/system-install) + [Advanced Options](/guide/advanced-options).
