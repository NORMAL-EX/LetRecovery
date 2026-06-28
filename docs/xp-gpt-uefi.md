# Windows XP / 2003 x64 GPT+UEFI 安装 + 驱动注入

本分支（`feat/xp-gpt-uefi`）为 LetRecovery 增加：在 **GPT+UEFI** 下部署「已 UEFI 化」的
Windows XP x64 映像，并在部署时把 **NVMe / 通用 AHCI / USB3(xHCI)** 驱动离线注入到目标系统。

## 必读：能成的前提

XP 本身既不支持 UEFI 引导、也没有 NVMe/通用 AHCI/USB3 驱动。本功能能成立，**完全依赖一个
「已 UEFI 化」的 XP x64 映像**——映像里必须自带：

- `WINDOWS\system32\winload.efi`（Vista 的 EFI 加载器）+ 打补丁的 `ntoskrnl.exe`/`hal.dll`
- `WINDOWS\system32\drivers\ntoskrn8.sys`（「ntoskrnl 扩展器」，给 Win7 storport 补内核导出）
- `WINDOWS\Boot\EFI\Microsoft\Boot\bootxp64.efi` + `BCC`（XP 专用 UEFI 引导器 + 其 BCD 存储）

> 本仓库根目录的 `xp64_nvme_uefisysprep_v4_fix.iso`（社区 Gelip 方案）即满足以上条件，可直接作为
> 测试映像。**普通原版 XP 映像无法 UEFI 启动**，工具无法凭空合成 winload/改核，遇到这种映像
> UEFI 引导步骤会失败并自动回退 Legacy(ntldr)。

## 行为概述

- **检测**：选择的镜像 `major_version == 5`（NT 5.x）即判定为 XP/2003。PE 端额外用
  「释放后缺少 `\Windows\Boot`」兜底（覆盖 CLI 路径）。
- **高级选项（检测到 XP 时显示，默认勾选）**：
  - ☑ 注入USB3驱动
  - ☑ 注入NVMe驱动
  - SATA(AHCI) 驱动 **始终注入**，无开关。
- **引导**：UEFI 模式 → 复刻社区 `startnet.cmd`：把映像自带 `WINDOWS\Boot\EFI` 释放到 ESP、
  把 `bootxp64.efi` 落到 UEFI 回退路径 `\EFI\Boot\bootx64.efi`、用 `bcdedit /store BCC` 修正
  `{bootmgr}/{ntldr}` 及两个自定义 GUID 的分区指向。Legacy 模式 → 原有 `ntldr/boot.ini`。
- **驱动注入**：XP(NT5.x) **不能用 DISM**。改为「拷贝 `.sys`/`.inf` 到目标系统 +
  在离线 SYSTEM 配置单元登记 boot-start 服务 + 写 `CriticalDeviceDatabase`」。

## 驱动放置（运行时从 bin 读取）

把驱动放到程序 **`bin\drivers\xp\`** 下（本分支已附带从测试 ISO 提取的一套）：

```
bin\drivers\xp\
├── ahci\   genahci.sys + genahci.inf/.cat + storport.sys + ntoskrn8.sys   （始终注入）
├── nvme\   stornvme.sys + stornvme.inf/.cat + storport.sys + ntoskrn8.sys
└── usb3\   amdxhc.inf/.cat + amdhub30.inf/.cat + x64\{amdxhc,amdhub30}.sys
```

PE 端运行时优先找 `exe\drivers\xp\`，找不到再回退 `exe\bin\drivers\xp\`（与 Win7 驱动同源）。
注入时：所有 `.sys`（含 `x64\` 子目录）复制到 `WINDOWS\system32\drivers`，`.inf` 复制到
`WINDOWS\inf`，并在 SYSTEM 配置单元写入：

| 集合 | 服务 | Start | CriticalDeviceDatabase |
|---|---|---|---|
| AHCI | `genahci` | 0(boot) | `PCI#CC_010601` → genahci |
| NVMe | `stornvme` | 0(boot) | `PCI#CC_010802` → stornvme |
| USB3 | `amdxhc`,`amdhub30` | 3(demand) | `PCI#CC_0C0330` → amdxhc |

`storport.sys` / `ntoskrn8.sys` 只复制文件、不单独建服务（由 stornvme/genahci 的导入表级联加载）。

## 改动文件

- `lr-core/src/xp.rs`（新增）：`inject_xp_drivers()` + `write_xp_uefi_gpt_boot()`（两端共享核心）
- `lr-core/src/lib.rs`：注册 `pub mod xp;`
- PE 端：`core/config.rs`（字段+解析）、`core/bcdedit.rs`（`write_xp_uefi_gpt_boot` 包装）、
  `app.rs` / `main.rs`（RepairBoot 分支）、`ui/advanced_options.rs`（注入块）
- 正常系统端：`core/install_config.rs`（字段+序列化）、`core/bcdedit.rs`、
  `ui/advanced_options.rs`（结构体+`get_xp_driver_dirs`+`show_ui`+`apply_to_system`+注入块）、
  `ui/system_install.rs`/`app.rs`（XP 检测+默认勾选）、`ui/install_progress.rs`、
  `core/cli_install.rs`、`main.rs`

## 实机测试步骤（PE 端，清盘 UEFI 全新装）

1. 用 UEFI 启动目标机进入 WinPE（含本分支编译的 PE 端 + 其 `bin\` 工具 + `bin\drivers\xp\`）。
2. 先用一键分区把目标盘做成 **GPT + ESP(≥100MB FAT32) + NTFS 系统分区**。
3. 选择「已 UEFI 化」的 XP x64 映像（如本仓库 ISO 内 `sources\install.wim`），引导模式选 UEFI/Auto。
4. 高级选项确认「注入USB3/NVMe」已勾选（默认即勾），开始安装。
5. 安装日志关注：`[XP-UEFI]`（引导写入）与 `[XP-DRV]`（驱动注入）段落。
6. 重启，固件经 `\EFI\Boot\bootx64.efi`(=bootxp64) → BCC → winload.efi → XP 从 NVMe/AHCI 启动。

## 已知边界

- 仅当映像「已 UEFI 化」时 UEFI 引导才成立；否则自动回退 Legacy(ntldr)。
- USB3 用 AMD `amdxhc`（按通用 xHCI 类代码 `CC_0C0330` 通配，绝大多数 xHCI 主控可用）。
- 注入服务默认写 `ControlSet001` 并尽力同步 `ControlSet002`；`Select\Current` 指向 001 的标准
  sysprep 映像即可。
