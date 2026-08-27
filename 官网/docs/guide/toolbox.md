---
title: 工具箱
description: LetRecovery 内置的维护工具。
---

# 工具箱

**工具箱**页汇集了常用维护工具。有的仅在桌面可用、有的仅在 WinPE 可用；界面会按当前环境和 Windows 版本隐藏不适用的项目并紧凑重排。比如 Windows 7/8/8.1 不会显示 APPX 等 Windows 10/11 专用入口。

## 磁盘与分区

- **一键分区**——可视化分区规划（GPT/MBR、自动按引导模式推荐方案、ESP 固定 500 MB FAT32、容量条预览）。
- **分区对拷**——把一个分区里的文件**逐一复制**到另一个分区（保留属性与时间戳，支持**断点续传**；开始前会检查目标可用空间是否够装下源已用空间）。注意它是**文件级**复制，不是按扇区/块克隆。
- **批量格式化**——一次格式化多个分区。**系统盘不会出现在列表里**（在 WinPE 下 `X:` 也会被排除），从根上避免误格当前系统。

## 镜像与完整性

- **镜像校验**——使用前检查 **WIM / ESD / SWM / GHO / ISO** 镜像完整性。
- **文件哈希校验**——计算文件的 **SHA-256** 并与你粘贴的期望值比对（核对下载完整性）。
- **查看 GHO 密码**——读取 Ghost 镜像里设置的密码。

## 系统与安全

- **BitLocker 管理**——解锁 / 解密 / 挂起·恢复保护、查看恢复密钥（详见[BitLocker 加密盘重装](/guide/bitlocker#手动管理-bitlocker)）。
- **密码重置**——清除本地账户密码：
  - **在线**（当前系统）：通过参数化的 Windows 账户 API 按账户身份清空密码并启用账户，不解析 `net user` 的本地化输出；
  - **离线**（另一个系统）：通过受控注册表/SAM 边界修改。改之前会先把 SAM 强制备份为 `SAM.lrbak`，成功后删除该备份（避免在目标盘留下带哈希的副本），仅在出错时保留备份以便恢复。
- **一键修复引导** *(仅 PE)*——重建 BCD / 修复 UEFI·Legacy 引导。

## 驱动与应用

- **驱动备份还原**、**导入存储驱动**
- **移除 APPX 应用**（仅支持的 Windows 10/11 环境显示，内置系统关键组件白名单）、**英伟达驱动卸载**

## 系统扩容与维护

- **无损扩大 C 盘**——无损扩大当前系统 C 盘；本机若缺 WinPE 会自动下载、装好 PE 引导后 重启进 WinPE 完成。详见[无损扩大 C 盘](/guide/expand-c-drive)。*(仅桌面)*
- **进入 PE 维护环境**——高级、默认隐藏的桌面入口。仅当 EXE 同目录的 `config.json` 含 `"pe_maintenance_entry_enabled": true` 时显示；点击后会立即打开带旋转动画的准备窗口，逐项显示查找本地 WIM、制作私有副本、收集 BitLocker 密钥、创建一次性启动项和安排重启。LetRecovery 的 PE 任务窗口保持隐藏，用户可直接使用 PE 桌面维护。正常系统端会尽力读取当前有盘符卷的 BitLocker 48 位恢复密码，PE 只尝试用它们解锁当前锁定卷；取不到或解锁失败会跳过，**不会关闭 BitLocker、移除保护器或启动解密**。恢复密码只放入本次私有启动 WIM，并由认证清单绑定，不写入公开配置和日志。已经位于随包目录或下载缓存中的 PE WIM 可由用户自行替换/定制；目录中的 MD5/SHA-256 只在下载新 WIM 时校验，启动维护、安装或备份时不会再用它阻止本地 WIM。*(仅正常系统端)*
- **本机网络信息**——查看本机网络配置。
- **重置网络设置**——重置网络栈。*(仅桌面)*
- **软件列表**——常用软件清单。*(仅桌面)*

## 其他

- **系统时间校准**——通过 NTP（按顺序尝试阿里云、腾讯、`cn.ntp.org.cn`、`time.windows.com`、 `pool.ntp.org` 等）校准为**北京时间（UTC+8）**。
- **SpaceSniffer**——磁盘空间占用分析。
- **手动运行 Ghost**——直接调起 `Ghost64.exe`。

## 命令行使用

正常系统端 EXE 为工具箱全部 22 项提供 JSON CLI。先运行 `LetRecovery.exe tool list` 查看当前环境是否可用、风险分类和完整用法。只读命令及 `plan` 不修改系统；真正的 `run` / `remove` 必须从已经提权的控制台执行并显式带 `--yes`，程序不会为公开 CLI 自动弹 UAC。

| 工具 | CLI |
| --- | --- |
| NVIDIA 驱动卸载 | `tool nvidia-driver inventory\|plan\|remove [--target current\|X:] [--yes]` |
| 分区对拷 | `tool partition-copy inventory\|plan\|run --source X: --target Y: [--yes]` |
| 批量格式化 | `tool batch-format inventory\|plan\|run --drives X:,Y: --file-system NTFS\|FAT32\|exFAT [--label 名称] [--yes]` |
| 导入存储驱动 | `tool storage-driver inventory\|plan\|run --target X: [--yes]` |
| 一键分区 | `tool quick-partition inventory\|plan\|run --disk-number N --style GPT\|MBR --layout-file 文件 [--yes]` |
| 移除 APPX | `tool appx inventory --target current\|X:`；`tool appx plan\|run --target current\|X: --packages-file 文件 [--yes]` |
| 驱动备份/还原 | `tool driver-transfer inventory\|plan\|run --mode backup\|restore --target current\|X: --directory 目录 [--yes]` |
| 修复引导 | `tool repair-boot inventory\|plan\|run --target X: [--yes]` |
| 网络信息 | `tool network-info inspect` |
| 软件列表 | `tool software-list inspect` |
| 时间同步 | `tool time-sync plan\|run [--yes]` |
| 运行 Ghost | `tool ghost plan\|run [--yes]` |
| 查看 GHO 密码 | `tool gho-password read --path 文件 [--show-secret]` |
| 重置网络 | `tool reset-network plan\|run [--yes]` |
| SpaceSniffer | `tool space-sniffer plan\|run [--yes]` |
| 镜像校验 | `tool verify-image inspect --path 文件` |
| BitLocker | `tool bitlocker inventory`；`tool bitlocker read-key --volume X: [--show-secret]`；`tool bitlocker plan\|run --volume X: --operation unlock-password\|unlock-recovery\|decrypt\|suspend\|resume [--secret-stdin] [--yes]` |
| 文件哈希 | `tool file-hash inspect --path 文件 [--expected SHA256]` |
| 重置密码 | `tool reset-password inventory --target current\|X:`；`tool reset-password plan\|run --target current\|X: --account 用户名 [--yes]` |
| 扩大 C 盘 | `tool expand-c analyze`；`tool expand-c plan\|run --target-size-mb N [--yes]` |
| 详细硬件检测 | `tool hardware-inspect inspect` |
| 进入 PE 维护 | `tool pe-maintenance plan\|run [--yes]` |

BitLocker 密码或恢复密码不能作为命令行参数，只能在解锁时经 `--secret-stdin` 从标准输入提供。GHO 密码和 BitLocker 恢复密码默认不出现在 JSON 中，只有明确使用 `--show-secret` 才显示；请避免把这类输出写入共享日志。

::: warning
工具箱里不少操作会改动磁盘或注册表，确认前请先看清对话框说明。
:::
