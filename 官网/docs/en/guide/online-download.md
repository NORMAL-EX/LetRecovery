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

Downloads use the built-in **Aria2** engine: multithreaded by default (`--split=32`, up to 16 connections per server) with **resumable downloads** enabled (`--continue=true`), making it faster and more stable than single-threaded downloading; after an interruption, retrying picks up from where it left off.

::: tip Slow or failing download?
The download service can get busy at times. If a download fails, retry it, or grab the full package directly from [GitHub Releases](https://github.com/NORMAL-EX/LetRecovery/releases).
:::

Once a system image finishes downloading, switch to the **System Installation** page and select it as your local image.

## Where Do the Resources Come From?

The online list is served by `https://letrecovery.cloud-pe.cn/v2/`, which includes multiple manifests for system images, software, graphics drivers, and more. Some system image and PE download services are provided by **Cloud-PE Cloud Storage** (see the project acknowledgments).
