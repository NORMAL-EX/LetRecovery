---
title: "Lossless C: Expansion"
description: "Expand the current system's C: drive without reinstalling or losing data."
---

# Lossless C: Expansion

C: drive full but you don't want to reinstall? **Lossless C: Expansion** in the [Toolbox](/guide/toolbox) can expand the current system's C: drive while **preserving your data**.

::: warning Desktop only
This feature only plans the operation from within normal Windows; the actual disk operation runs **after rebooting into WinPE** (a running system drive cannot be expanded online).
:::

## Currently Enabled Expansion Method

The current production path exposes pure extend only:

| Method | Description | Risk |
| --- | --- | --- |
| **Method 1: Pure extend** | Target = current size + the **unallocated space immediately after** the C: drive. No data is moved. | Low (recommended) |

The target must be the authenticated single-extent C: volume, with already-existing contiguous unallocated space immediately after it on the same disk. A requested size beyond that space fails closed before disk writes.

::: warning Partition moving is not enabled
Shrinking or moving a following partition, borrowing from a left-side donor, and every raw block move currently have no production entry point. Until canonical identities, one retained PhysicalDrive handle, and a recoverable consume journal span every stage, LetRecovery does not fall back to bare drive-letter authorization or experimental moves.
:::

::: tip Minimum target size
The target size cannot be smaller than the current size, and must be at least **used space + 1 GB**.
:::

## Procedure

1. From the desktop, open **Toolbox → Lossless C: Expansion** and enter the target size.
2. The program plans the operation; if **no usable WinPE** is available on this machine, it will **download** PE automatically first.
3. It installs a temporary PE boot entry and **reboots into WinPE** to perform the actual expansion.
4. Once expansion is complete, it reboots back into the system, with the C: drive enlarged and your data preserved.

::: tip No free space after the C: drive?
If no unallocated space exists immediately after C:, the current lossless-expansion task stops. Back up first and use another verified method to re-plan the partition layout so contiguous free space follows the target, then retry pure extend. Do not treat this version as a partition-moving tool.
:::

::: danger Back up important data first
Although the current path does not move file data, extending a volume still changes the partition table and volume boundary. Back up important data before expanding, and make sure the power does not go out during the process.
:::
