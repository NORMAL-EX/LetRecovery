---
title: BitLocker 加密盘重装
description: LetRecovery 如何重装被 BitLocker 加密的系统盘。
---

# BitLocker 加密盘重装

重装被 **BitLocker** 保护的系统盘，在 WinPE 下通常会失败——加密卷处于锁定状态，既读
不了也格式化不了。LetRecovery 会自动处理这一点。

## 它如何决策

在加密的系统盘上开始安装时，LetRecovery 会判断能否拿到**目标盘的恢复密钥**
（48 位数字恢复密码）：

- **能拿到 → 密钥透传。** 把恢复密钥打包进 WinPE 引导镜像（写入 `boot.wim` 内的
  `\LR_BitLockerKeys.txt`）。重启进 WinPE 后用它**解锁**该盘，再格式化并部署。这条路很快
  ——不需要漫长的解密。
- **拿不到 → 回退彻底解密。** 如果目标盘只有 TPM 保护器、没有数字恢复密码（无法
  透传密钥），LetRecovery 会在重启前于在线系统里把 BitLocker 卷**彻底解密**。

无论哪条路，结果一致：WinPE 能访问该盘，且新装好的系统**不再加密**（需要的话装完后
自行重新开启 BitLocker）。

::: details PE 端如何用透传的密钥
PE 端读取 `X:\LR_BitLockerKeys.txt`，用每个密钥逐一尝试解锁每个盘符（密钥本身自带校验，
错配会被拒绝，所以无需把密钥和卷一一对应）。底层优先调用 `fveapi.dll`，失败再回退
`manage-bde` 命令行。
:::

## 解锁 ≠ 解密

在透传这条路上，WinPE 只是**解锁**卷（提供密钥让文件系统可读）——此刻它仍是完整的
BitLocker 卷。随后的**格式化**会把 BitLocker 元数据连同旧数据一起抹掉，所以新系统最终
是未加密的。

## 手动管理 BitLocker

[工具箱](/guide/toolbox)里有 **BitLocker 管理**：

- **解锁**（密码 / 恢复密钥）、**解密**整盘；
- **挂起 / 恢复**保护——挂起后密钥以明文留存、重启仍有效，常用于改 BIOS/固件前临时关闭，
  之后再恢复，**无需**重新加密整盘；
- **查看恢复密钥**。

::: tip Secure Boot 与随包 PE
LetRecovery 通过目标机自带的 **Windows 启动管理器（bootmgfw）** 引导 WinPE，而非自带
引导器，因此在开启 Secure Boot 的机器上也能用。

更进一步：当 Secure Boot 开启、且主机系统的 `winload.efi` 含 2023 证书、且与 PE 内核版本
匹配（`10.0.19041.` 系）时，程序会**尽力**用主机的双签名（2011+2023）`winload.efi` 覆盖 PE 的
对应文件，从而让 PE 在已吊销 2011 证书（CVE-2023-24932 DBX 更新）的机器上也能启动。这是
**尽力而为**的优化，满足上述条件才触发。
:::
