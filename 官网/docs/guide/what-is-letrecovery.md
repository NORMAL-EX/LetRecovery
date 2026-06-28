---
title: LetRecovery 是什么？
description: LetRecovery 简介——一款免费开源的 Windows 系统重装与备份工具。
---

# LetRecovery 是什么？

**LetRecovery** 是一款免费、开源的 **Windows 系统重装与备份**工具。它可以部署系统
镜像（WIM / ESD / SWM / GHO / ISO，含 Windows XP / 2003 文本模式安装）、备份现有系统、
在线下载镜像，并内置一整套常用维护工具——全部集中在一个干净、无广告的界面里。

它基于 **Rust + egui** 开发，既能在正常 Windows 桌面下运行，也能直接在 **WinPE**
中使用，目标是让电脑小白也能安全地重装系统。

::: tip 永久免费
LetRecovery 永久免费，请仅从[官方渠道](#从哪里获取)获取。任何向你收费的，都是与本
项目无关的二道贩子。
:::

## 亮点

- **多格式安装**——WIM / ESD / SWM / GHO / ISO，自动挂载 ISO、可选镜像内任意版本；
  还支持原版 [Windows XP / 2003 的 i386 文本模式安装](/guide/xp-install)。
- **桌面 & WinPE 双端**——在桌面下重装*当前*系统盘时，自动写引导并重启进 WinPE；
  直接在 WinPE 里运行则就地安装。
- **BitLocker 加密盘可重装**——部署前自动解锁或解密 BitLocker 卷。
- **完整 / 增量备份**为 WIM / ESD / SWM / GHO。
- **在线下载**镜像、常用软件与显卡驱动，Aria2 多线程加速。
- **简易模式**——[一键重装](/guide/easy-mode)，只需选系统、选版本、确认。
- **完整工具箱**——BitLocker 管理、密码重置、镜像/哈希校验、一键分区、驱动备份还原、
  [无损扩大 C 盘](/guide/expand-c-drive)、引导修复等。

## 工作原理

LetRecovery 有两个共享同一核心（`lr-core`）的前端：

| 组件 | 可执行文件 | 作用 |
| --- | --- | --- |
| **正常系统端** | `LetRecovery.exe` | 主界面。在正常 Windows **和** WinPE 里都能跑（自动识别 PE）。 |
| **PE 端** | `LetRecoveryPE.exe` | 给"自动重启进 WinPE"流程用的精简无人值守安装/备份界面。 |

安装到**非系统**分区（或你本来就在 WinPE 里）时，**直接**部署；从桌面安装到**正在运行
的系统盘**时，LetRecovery 会先准备 WinPE 引导、重启进入、再在 PE 里完成——因为运行
中的系统无法覆盖自身。

::: details PE 是怎么识别的？
正常系统端启动时会综合多项特征判断当前是否身处 WinPE：系统盘是否为 `X:`、是否存在
`X:\MININT`、`winpeshl.ini`、`fbwf.sys`、以及 `MiniNT` 注册表项等。识别为 PE 后，界面会
自动启用/禁用相应功能（例如"一键修复引导"仅在 PE 可用）。
:::

## 可以装哪些系统？

- **运行 LetRecovery 桌面端**需要 **Windows 10 / 11（64 位）**，或直接在 WinPE 里运行。
- **可安装的目标系统**则覆盖很广：Windows **XP / 2003 / 7 / 8 / 8.1 / 10 / 11**。
  不同年代的系统会自动启用对应的兼容处理（如 Win7 的 USB3/NVMe 注入与蓝屏修复、XP 的
  文本模式安装与存储驱动集成）。

## 从哪里获取

- **GitHub Releases**——<https://github.com/NORMAL-EX/LetRecovery/releases>
- **官方网站**（三个域名，内容相同）——<https://sysre.cn> / <https://letrecovery.net> / <https://letrecovery.cn>
- **官方文档**（即本站）——<https://docs.letrecovery.net>

GitHub 发布的是**完整包**（已内置 WinPE）。详见[快速开始](/guide/getting-started)。

::: warning 认准官方域名
本项目有三个官方主页、内容完全相同：**sysre.cn**、**letrecovery.net**、**letrecovery.cn**；
文档在 **docs.letrecovery.net**，源码在 **github.com/NORMAL-EX/LetRecovery**。除此之外的
"官网""下载站"请谨慎甄别。
:::

## 许可证

LetRecovery 基于
[PolyForm Noncommercial License 1.0.0](https://github.com/NORMAL-EX/LetRecovery/blob/main/LICENSE)
发布：允许个人、研究与非商业使用；**禁止商业用途**。
