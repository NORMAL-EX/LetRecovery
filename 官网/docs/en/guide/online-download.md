---
title: Online Download
description: Download system images and categorized software directly inside LetRecovery.
---

# Online Download

The **Online Download** page lets you grab resources without leaving LetRecovery:

| Tab | Contents |
| --- | --- |
| **System Images** | Curated, ready-to-install Windows images. |
| **Software** | Common fresh-install tools grouped by the v4 catalogue categories. |

## Aria2 Acceleration

Downloads use the built-in **Aria2** engine with **resume support** enabled. The default parallel split count is **16**, selectable as 8 / 16 / 32, while connections to any one server remain capped at 16. Retrying after an interruption continues the partial download.

::: tip Slow or failing download?
The download service can get busy at times. If a download fails, retry it, or grab the full package directly from [GitHub Releases](https://github.com/NORMAL-EX/LetRecovery/releases).
:::

## Catalogue Source and Verification

LetRecovery reads only the fixed HTTPS entry point `https://letrecovery.cloud-pe.cn/v4/`; it no longer falls back to the old v1/v2 catalogues. The catalogue can be configured to use API images, Microsoft's official catalogue, or both. Microsoft entries come from Update Metadata Service for Windows 11 and a controlled fwlink for Windows 10, with strict checks for official hosts, metadata, declared size, and available hashes.

Any declared MD5 or SHA-256 travels with the item into download and cache validation. If a declared digest cannot be calculated or does not match, the operation fails closed. When the server declares no digest, the UI does not claim that the file is verified.

## From Catalogue to Installation

When you click **Install**, the original URL is handed to **System Installation**, which first tries bounded HTTP Range requests for remote WIM/ESD metadata. For an ISO, it reads only the necessary ISO 9660/Joliet directory data and ranges for `sources/install.wim` or `install.esd`. If the server clearly does not support Range, the probe does not swallow the full response; LetRecovery switches to a normal full download. A malformed range response, a changing resource, or an unsafe redirect still stops the operation.

When Range works, the resumable full download starts only after you choose an edition and confirm, and the downloaded file must match that edition's index, version, build, architecture, and installation type. When Range is unavailable, LetRecovery fully downloads first, filters out PE/Setup/WinRE and other non-installable volumes with the local inspector, selects the first installable edition by default, and returns to the install page for your confirmation before installation begins.

::: tip Easy Mode
Online Download and Toolbox navigation are hidden while Easy Mode is enabled. Easy Mode's own system cards still use the controlled catalogue to select and download images.
:::
