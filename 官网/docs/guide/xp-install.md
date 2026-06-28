---
title: Windows XP / 2003 安装
description: 用 LetRecovery 安装 Windows XP / 2003，含 i386 文本模式与已 UEFI 化映像两条路径。
---

# Windows XP / 2003 安装

LetRecovery 对"老古董"系统也有相当完整的支持。XP/2003 与 Win7+ 完全不同——它没有
`install.wim`、原生不支持 UEFI、也没有 NVMe/USB3 驱动，因此走的是两条专门的路径。

::: warning 通用前提
- XP/2003 **不能**在"正在运行的系统盘"上原地安装，需先进 WinPE。
- 安装会格式化目标分区，请先备份。
:::

## 两条安装路径

| 路径 | 介质 | 引导 | 适用 |
| --- | --- | --- | --- |
| **i386 文本模式安装** | 原版 XP/2003 安装盘（根目录 `\I386`，无 `install.wim`） | **仅 Legacy / MBR** | 想用原汁原味的原版安装盘 |
| **已 UEFI 化映像** | 经社区改造、自带 `winload.efi` / 改核 / `bootxp64.efi` 的 XP x64 WIM | **GPT + UEFI**（也可 Legacy） | 现代 NVMe 机器、想用 UEFI 启动 |

程序会根据所选镜像**自动识别**走哪条路：选到的镜像 `major_version == 5`（NT 5.x）即判为
XP/2003；挂载 ISO 后若找不到 `install.wim/esd` 但存在 `\I386\setupldr.bin` 等文件，则识别为
i386 文本安装介质，界面会显示绿色提示"已识别为 Windows XP/2003 i386 文本安装介质"。

## 路径一：原版 i386 文本模式安装

针对**原版安装盘**，LetRecovery 照搬成熟的 NT5 硬盘安装做法（微软
`winnt32 /makelocalsource` 的 `$WIN_NT$.~LS` + `$WIN_NT$.~BT` 约定），把安装源拷到目标盘后，
写好引导文件，重启进入蓝底文本安装。

要点：

- **仅 Legacy/BIOS + MBR**——发起安装时会拦截 GPT/UEFI 目标。
- **架构优先 AMD64**——XP x64 / 2003 x64 介质同时含 `\AMD64`（完整 64 位源）与 `\I386`
  （残缺的 WOW 支持文件），程序优先选完整源、避免误用残缺的 `\I386`；纯 32 位介质只有
  `\I386` 时正常回落。
- **文本期存储驱动集成**——为了让蓝底文本阶段就能认 AHCI/NVMe，安装时会按架构扫描驱动
  目录并合并进 `txtsetup.sif`。

### 无人值守与产品密钥

落盘前会强制写入硬盘安装必需的几个键（最关键的是 `MsDosInitiated=1`，声明"从硬盘启动、
非光盘引导"）。是否全自动取决于产品密钥：

| 是否放 `bin\xp\productkey.txt` | 行为 |
| --- | --- |
| 否（默认） | 跳过 EULA/区域/欢迎、忽略驱动签名、管理员空密码 + 首次自动登录；图形阶段**只在"产品密钥"页停一下** |
| 是 | 全程不停顿，真·全自动 |

::: tip 工具不内置产品密钥
要全自动，请自行在 `bin\xp\productkey.txt` 放一行与所装版本/渠道匹配的密钥。出于安全，
文本阶段仍由用户确认"装到哪个分区"，不自动选盘以免抹错盘。
:::

## 路径二：已 UEFI 化的 XP x64 映像（GPT + UEFI）

这条路能在 **GPT+UEFI** 下部署 XP x64，并注入 NVMe / AHCI / USB3 驱动。

::: danger 必读：依赖"已 UEFI 化"的映像
XP 本身不支持 UEFI，工具**无法凭空合成**。本路径成立的前提是映像里已自带：

- `WINDOWS\system32\winload.efi`（Vista 的 EFI 加载器）+ 打补丁的 `ntoskrnl.exe`/`hal.dll`
- `WINDOWS\system32\drivers\ntoskrn8.sys`（给存储驱动补内核导出的"ntoskrnl 扩展器"）
- `WINDOWS\Boot\EFI\Microsoft\Boot\bootxp64.efi` + 其 BCD 存储

普通原版 XP 映像不满足这些条件，UEFI 引导步骤会失败并**自动回退 Legacy(ntldr)**。
:::

## 驱动注入

XP（NT 5.x）**不能用 DISM**。LetRecovery 改用"拷贝 `.sys`/`.inf` + 在离线 SYSTEM 配置单元
登记 boot-start 服务 + 写 `CriticalDeviceDatabase`"的方式离线注入。

把驱动放到程序的 **`bin\drivers\xp\`** 下（完整包已附带一套从测试映像提取的驱动）：

```
bin\drivers\xp\
├── ahci\   genahci + storport + ntoskrn8     （始终注入）
├── nvme\   stornvme + storport + ntoskrn8
├── usb3\   amdxhc + amdhub30
├── amd64\  （64 位 XP/2003 文本期用户驱动）
└── x86\    （32 位 XP 文本期用户驱动）
```

::: warning 32 位 XP 要 32 位驱动
随包的 genahci/stornvme 是 **x64**，只对 64 位 XP/2003 生效。**32 位 XP** 需要 32 位驱动
（自行放 `x86\`）；现代 NVMe 的 32 位 XP 驱动基本没有，多数 SATA 机器把 BIOS 的 SATA 模式
切到 IDE/Compatibility 即可免驱动安装。
:::

## 该选哪条路？

- 手头是**原版安装盘**、目标机是传统 SATA/BIOS → 用**路径一（i386 文本模式）**。
- 目标机是**现代 NVMe / 想 UEFI 启动** → 用**路径二（已 UEFI 化的 XP x64 映像）**更省心。

::: tip 实机回归
XP/2003 安装链路依赖真实重装环境，建议先在虚拟机/实机回归。反馈问题时请附上日志里的
`[XP-UEFI]`、`[XP-DRV]`、`[TXTDRV]` 等段落。
:::
