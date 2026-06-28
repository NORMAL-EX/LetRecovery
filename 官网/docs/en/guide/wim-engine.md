---
title: Image Engine
description: The engines LetRecovery uses to apply/capture WIM·ESD images, and how to switch between them.
---

# Image Engine (libwim / wimgapi)

When applying and capturing WIM / ESD images, LetRecovery lets you choose between two engines. Most users can simply keep the default;
this page explains the difference so you can switch when needed.

## The two engines

| Engine | Source | Characteristics |
| --- | --- | --- |
| **libwim** | Bundled `libwim-15.dll` | **Default**. Consistent across environments, behaves the same on the desktop and in WinPE, and depends on no system components. |
| **wimgapi** | The system's own `wimgapi.dll` | Windows-native image API. Optional, and a closer match to system behavior in some scenarios. |

## Where to switch

Choose the image engine on the **About** page. The choice is **process-wide global**:

- The desktop client reads and sets it from `config.json`;
- When you [install/back up via PE](/guide/system-install), the chosen engine is **passed to the PE client across the reboot** —
  in other words, "after switching to wimgapi, the PE client also uses wimgapi", consistently before and after.

## Automatic fallback, never stuck

Choosing wimgapi does not mean "putting all eggs in one basket":

- If wimgapi **fails to load/initialize**, it **automatically falls back to libwim** at construction time;
- If wimgapi **fails during apply or capture**, it also falls back to libwim and retries (before falling back from a failed capture, it first cleans up
  the half-finished file left by wimgapi, to avoid it being mistaken for an incremental append).

libwim is always initialized, either as the primary engine or as the fallback engine, so functionality is **always available**.

::: tip Which one should I use?
When in doubt, use **the default libwim** — it is consistent across environments and the most stable. Only switch to wimgapi when you clearly run into
a specific image that libwim handles abnormally and you want to try the system-native API.
:::

::: details A read-only exception
Read-only metadata operations such as probing "whether an image has a built-in answer file" always go through libwim (read only, no mounting), regardless of the engine selected for apply/capture.
:::
