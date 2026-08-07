---
title: Online Download
description: Download system images, software, and graphics drivers directly inside LetRecovery.
---

# Online Download

The **Online Download** page lets you grab resources without leaving LetRecovery, organized into three category tabs:

| Tab | Contents |
| --- | --- |
| **System Images** | Curated, ready-to-install Windows images. |
| **Software** | Common tools for a fresh install. |
| **Graphics Drivers** | Driver packages for common graphics cards (GPUs). |

## Aria2 Acceleration

Downloads use the built-in **Aria2** engine with **resume support** enabled. The default parallel split count is **16**, selectable as 8 / 16 / 32, while connections to any one server remain capped at 16. Retrying after an interruption continues the partial download.

::: tip Slow or failing download?
The download service can get busy at times. If a download fails, retry it, or grab the full package directly from [GitHub Releases](https://github.com/NORMAL-EX/LetRecovery/releases).
:::

## Catalogue Source and Verification

LetRecovery reads only the fixed HTTPS entry point `https://letrecovery.cloud-pe.cn/v3/index.json`; it no longer falls back to the old v1/v2 catalogues. The catalogue can be configured to use API images, Microsoft's official catalogue, or both. Microsoft entries come from Update Metadata Service for Windows 11 and a controlled fwlink for Windows 10, with strict checks for official hosts, metadata, declared size, and available hashes.

Any declared MD5 or SHA-256 travels with the item into download and cache validation. If a declared digest cannot be calculated or does not match, the operation fails closed. When the server declares no digest, the UI does not claim that the file is verified.

## From Catalogue to Installation

Clicking **Install** on a system image does not blindly download the whole file first. The original URL is handed to **System Installation**, which reads remote WIM/ESD metadata with bounded HTTP Range requests. For an ISO, it reads only the necessary ISO 9660/Joliet directory data and ranges for `sources/install.wim` or `install.esd`. The probe stops if the server does not honor exact ranges, the resource changes between requests, or a redirect is unsafe.

The resumable full download starts only after you choose an edition and confirm. When it finishes, LetRecovery reads the complete local image again and compares the selected index, version, build, architecture, and installation type before continuing.

::: tip Easy Mode
Online Download and Toolbox navigation are hidden while Easy Mode is enabled. Easy Mode's own system cards still use the controlled catalogue to select and download images.
:::
