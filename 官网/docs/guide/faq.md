---
title: 常见问题
description: 关于 LetRecovery 的常见问题。
---

# 常见问题

## LetRecovery 收费吗？

不收费——永久免费、开源。请仅从
[GitHub Releases](https://github.com/NORMAL-EX/LetRecovery/releases) 或
[官方网站 sysre.cn](https://sysre.cn) 获取。任何向你收费的都是与本项目无关的二道贩子。

## 官方网站到底是哪个？

- **官方主页**（三个域名，内容相同）——<https://sysre.cn> / <https://letrecovery.net> / <https://letrecovery.cn>
- **源代码**——<https://github.com/NORMAL-EX/LetRecovery>

除此之外的"官网""下载站"请谨慎甄别。

## 能在 WinPE 里运行吗？

能。桌面版 `LetRecovery.exe` 会自动识别 WinPE 并**就地安装**（无需重启）。在 PE 里请
运行**完整包**——别只拷出单个 exe，它需要随附的 DLL 和 `bin\`。

## 支持哪些镜像格式？能装哪些系统？

镜像格式：WIM、ESD、SWM、GHO、ISO，以及原版 XP/2003 的 i386 文本安装介质。
可安装的目标系统涵盖 Windows **XP / 2003 / 7 / 8 / 10 / 11**。

## 运行需要什么配置？真要 4 GB 内存吗？

运行桌面端需 Windows 10/11（64 位）+ 管理员权限。建议至少 4 GB 可用内存，但这只是**建议**
——程序**不会**强制检测内存。

## 支持 BitLocker 吗？

支持——重装前会自动解锁（密钥透传）或解密被加密的系统盘。详见
[BitLocker 加密盘重装](/guide/bitlocker)。

## 有没有更简单的"一键重装"？

有，试试[简易模式](/guide/easy-mode)：选系统、选版本、确认即可，常用优化项会自动套用。
（简易模式在 WinPE 下不可用。）

## 安装失败——日志在哪？

- 桌面：`<程序目录>\log\LetRecovery.<日期>.log`
- PE：`X:\Program Files\LetRecoveryPE\LetRecoveryPE.log`

如果重装 C:，请从**另一块盘**运行 LetRecovery，日志才不会被格式化掉。
[反馈问题](https://github.com/NORMAL-EX/LetRecovery/issues)时请附上日志。

## 部署时报 "invalid compressed data" / 错误码 2

镜像损坏或没下完整（这是 wimlib 的"解压缩失败"错误）。请重新下载镜像，并先用
**工具箱 → 镜像校验**检验后再安装。

## 能换界面语言吗？

可以。**关于**页有语言选择器（自带 简体中文，随包附带 English (US)）。把符合格式的
`<语言代码>.json` 放进 `lang\` 目录、点"刷新语言列表"即可新增语言。

## 镜像释放用的是哪个引擎？能换吗？

默认用内置的 **libwim**，也可在**关于**页切到系统原生 **wimgapi**（失败会自动回退 libwim）。
详见[镜像引擎](/guide/wim-engine)。

## 发布新版本会改动官网的版本号吗？

会。日常开发都在其他分支或独立仓库进行，平时并不影响线上站点；而一旦这些改动通过验证、
合并进 LetRecovery 仓库的 `main` 分支，就会触发 EdgeOne Pages 重新编译并部署官网。由于
官网版本号是按构建日期生成的（如 `v2026.06.07`），它也会随着这次重新编译一并更新。

## 为什么 PE 镜像里看不到 `libwim-15.dll`？

它被编译进了 exe（`LetRecoveryPE.exe` / `LetRecovery.exe` 共用同一机制），**运行时才释放到
exe 同目录**，所以静态看不到——首次运行后会出现在 exe 旁边。
