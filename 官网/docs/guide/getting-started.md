---
title: 快速开始
description: 下载、运行 LetRecovery 并完成第一次重装。
---

# 快速开始

## 环境要求

运行 LetRecovery **桌面端**：

- Windows 10 / 11（64 位）——桌面端会强制检查 64 位，32 位系统无法运行
- 管理员权限（非管理员启动会自动请求 UAC 提权）
- UEFI 或 Legacy BIOS 均可

::: tip 关于内存
官方建议至少有 **4 GB 可用内存**，但这只是经验值——程序**不会**强制检测内存，内存偏小时
释放大镜像可能更慢或失败而已。
:::

可安装的**目标系统**范围远不止 10/11，详见
[可以装哪些系统](/guide/what-is-letrecovery#可以装哪些系统)。

## 1. 下载

从 [GitHub Releases](https://github.com/NORMAL-EX/LetRecovery/releases) 获取最新
**完整包**——就是一个内置了 WinPE 的 `LetRecovery.7z`。

::: warning 请用完整包
请解压**整个** `LetRecovery.7z`，不要只把 `LetRecovery.exe` 单独拷出来——它需要随附的
`bin\`、`opengl32.dll`、`libwim-15.dll` 等运行时 DLL 才能工作（尤其在 WinPE 里）。
:::

## 2. 以管理员身份运行

把压缩包解压到一个文件夹，右键 `LetRecovery.exe` → **以管理员身份运行**。

::: tip 要重装系统盘？
如果你要重装 **C:**，请把 LetRecovery 解压到**另一块盘**（比如 `D:`）。安装时 C: 会被
格式化，盘上的东西——包括 LetRecovery 自己的日志——都会被清掉。
:::

## 3. 选择镜像

在**系统安装**页：

1. 选本地镜像（`浏览…`），或从**在线下载**获取一个。
2. 选择镜像内的**版本**（如 专业版 / 家庭版）。
3. 选择**目标分区**。

> 想要最省心？跳过这些细节，直接用[简易模式](/guide/easy-mode)：选系统、选版本、确认即可。

## 4. 开始安装

点击**开始安装**。

- 安装到**非系统**分区，或**在 WinPE 里**运行 → **直接**格式化并部署。
- 从桌面安装到**正在运行的系统盘** → 自动准备 WinPE 引导并重启进入 WinPE 完成。

::: danger 先备份
安装会格式化目标分区，**请先备份重要数据**。
:::

## 日志在哪里？

出问题反馈时请附上：

- **正常系统端**：`<程序目录>\log\LetRecovery.<日期>.log`（如 `LetRecovery.2026-06-26.log`）
- **PE 端**：PE exe 同目录的 `X:\Program Files\LetRecoveryPE\LetRecoveryPE.log`

## 怎么看版本号？

软件版本号按**构建日期**生成，形如 `v2026.06.07`，可在**关于**页或日志开头的"版本"那行看到。
反馈问题时带上它能帮助定位。

## 下一步

- [系统安装](/guide/system-install)
- [系统备份](/guide/system-backup)
- [BitLocker 加密盘重装](/guide/bitlocker)
- [命令行与无人值守安装](/reference/command-line)
- [常见问题](/guide/faq)
