---
title: "Lossless C: Expansion"
description: "Expand the current system's C: drive without reinstalling or losing data."
---

# Lossless C: Expansion

C: drive full but you don't want to reinstall? **Lossless C: Expansion** in the [Toolbox](/guide/toolbox) can expand the current system's C: drive while **preserving your data**.

::: warning Desktop only
This feature only plans the operation from within normal Windows; the actual disk operation runs **after rebooting into WinPE** (a running system drive cannot be expanded online).
:::

## Two expansion methods

After opening it, enter the target size, and the program will calculate two "ceilings":

| Method | Description | Risk |
| --- | --- | --- |
| **Method 1: Pure extend** | Target = current size + the **unallocated space immediately after** the C: drive. No data is moved. | Low (recommended) |
| **Method 2: Experimental** | Building on Method 1, it further **shrinks / moves** the data partition behind it (e.g. D:) to free up space. This moves data on that partition. | High, slower |

When the target size you enter exceeds the "Method 1" ceiling, the interface displays a prominent warning, indicating that this will trigger data movement.

::: tip Minimum target size
The target size cannot be smaller than the current size, and must be at least **used space + 1 GB**.
:::

## Procedure

1. From the desktop, open **Toolbox → Lossless C: Expansion** and enter the target size.
2. The program plans the operation; if **no usable WinPE** is available on this machine, it will **download** PE automatically first.
3. It installs a temporary PE boot entry and **reboots into WinPE** to perform the actual expansion.
4. Once expansion is complete, it reboots back into the system, with the C: drive enlarged and your data preserved.

::: tip No free space after the C: drive?
If there is no unallocated space immediately after the C: drive at all, you can first use [One-Click Partition](/guide/toolbox) to free up space and then expand; or use "Method 2" directly (note its data-movement risk).
:::

::: danger Back up important data first
Although this feature is designed to be lossless, any operation involving the partition table and data movement carries risk. Back up important data before expanding, and make sure the power does not go out during the process.
:::
