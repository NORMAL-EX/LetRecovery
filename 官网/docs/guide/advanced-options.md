---
title: 高级选项
description: 驱动、无人值守、注册表优化与系统优化。
---

# 高级选项

在系统安装页打开**高级选项**，可对部署做精细调整。

## 驱动

- **导出 / 导入驱动**——把第三方驱动保留到重装后的系统。导出使用官方 **DISM API**
  （`DismExportDriver`），失败时回退到手动 DriverStore 导出。
- **磁盘控制器驱动注入**——注入 NVMe / RAID / AHCI 驱动，让新系统能在现代存储上启动
  （Win10/11 x64 镜像会自动勾选）。

## 无人值守

- 使用内置生成的 `unattend.xml`，或选择你**自己**的无人值守文件。
- 自定义自动创建的用户名、系统盘卷标。
- 程序还会**自动检测**目标分区、安装介质根目录、以及镜像内部是否已自带应答文件，
  并据此默认勾选无人值守。

::: tip 自定义应答文件的生效范围
自定义的 `unattend.xml` 在**经 PE 安装**流程里会被完整复制到目标系统生效（这也是从桌面
重装系统盘的主路径）。XP/2003 的自定义 `winnt.sif` 在其文本安装流程里同样生效。
:::

## 系统优化

应用到新部署的系统：

- 删除预装 UWP 应用 <Badge type="tip" text="依赖无人值守" />
- 绕过 OOBE "必须联网"（BypassNRO）<Badge type="tip" text="依赖无人值守" />
- 禁用 Windows 更新
- 禁用 **Windows 安全中心（Defender）**
- Win11 恢复经典右键菜单、去除快捷方式小箭头
- 禁用 UAC、系统保留空间、自动设备加密

::: warning 依赖无人值守的项目
"删除预装 UWP 应用""绕过 OOBE 必须联网""自定义用户名"需要无人值守支持。当目标分区
**已自带**应答文件时，这几项会被禁用并强制取消（除非你勾选了格式化分区）。
:::

## WiFi 配置迁移

把当前机器的 WiFi 配置带进新系统。检测不到 WiFi 时（`netsh wlan show interfaces` 无接口）
该选项会自动隐藏。

## Win7 专用开关

针对 Win7 镜像会额外显示这些开关：

- **UEFI 补丁（UefiSeven）**——让不原生支持 UEFI 的 Win7 也能 UEFI 引导。
- **注入 USB3 / NVMe 驱动**——现代主板上 Win7 安装/启动所需。
- **修复 ACPI 蓝屏（0xA5）**——禁用 `intelppm`/`amdppm`/`Processor` 等处理器电源服务。
- **修复存储控制器蓝屏（0x7B）**——把 `msahci`/`storahci`/`pciide`/`iaStor*`/`stornvme`
  等一长串存储服务设为 boot-start（同时写入 `ControlSet001` 与 `ControlSet002`）。

::: tip Win7 驱动目录里的 CAB
如果你往 Win7 USB3 / NVMe 驱动目录里放了 `.cab` 更新包（例如某些必备的内核更新），
注入前会自动解压并随驱动一起注入。这属于 Win7 驱动注入的便利功能，**不是**通用的"安装
系统更新"优化项。
:::

## Windows XP / 2003 专用开关

检测到 XP/2003 镜像时，会显示与 Win7 平行的一组开关：注入 USB3 / NVMe 驱动（默认勾选）、
AHCI 驱动**始终注入**、以及对"已 UEFI 化"映像的 UEFI/GPT 引导支持。详见
[Windows XP / 2003 安装](/guide/xp-install)。
