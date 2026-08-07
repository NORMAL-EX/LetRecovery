---
title: 高级选项
description: 驱动、无人值守、注册表优化与系统优化。
---

# 高级选项

在系统安装页打开**高级选项**，可对部署做精细调整。

## 驱动

- **导出 / 导入驱动**——把第三方驱动保留到重装后的系统。导出使用官方 **DISM API** （`DismExportDriver`），失败时回退到手动 DriverStore 导出。
- **磁盘控制器驱动注入**——在目标系统支持时，可导入随包锁定的存储控制器驱动。程序通过 SetupAPI 读取当前机器的完整 PCI 硬件 ID，只导入唯一匹配且通过签名与哈希校验的包；匹配不唯一或无法确认时不会猜测，也不会递归注入整个驱动目录。

## 无人值守

- 使用内置生成的 `unattend.xml`，或选择你**自己**的无人值守文件。
- 选择普通自定义用户名，或启用并按 RID-500 身份处理内置 Administrator；也可自定义系统盘卷标。Administrator 密码只存在于当前安装会话和无人值守文件，不写入持久化偏好或日志。
- 程序还会**自动检测**目标分区、安装介质根目录、以及镜像内部是否已自带应答文件，并据此默认勾选无人值守。

::: tip 自定义应答文件的生效范围
自定义的 `unattend.xml` 在**经 PE 安装**流程里会被完整复制到目标系统生效（这也是从桌面重装系统盘的主路径）。XP/2003 的自定义 `winnt.sif` 在其文本安装流程里同样生效。
:::

## 系统优化

应用到新部署的系统：

- 删除预装 UWP 应用 <Badge type="tip" text="依赖无人值守" />
- 绕过 OOBE "必须联网"（BypassNRO）<Badge type="tip" text="依赖无人值守" />
- 禁用 Windows 更新
- **深度移除 Microsoft Defender Antivirus 杀毒引擎**——只处理 Defender Antivirus 引擎、驱动和自身计划任务；保留 Windows 安全中心、UAC、防火墙、SmartScreen、VBS 与 Defender for Endpoint
- Win11 恢复经典右键菜单、去除快捷方式小箭头
- 禁用 UAC、系统保留空间、自动设备加密

::: warning 依赖无人值守的项目
"删除预装 UWP 应用""绕过 OOBE 必须联网""自定义用户名"需要无人值守支持。当目标分区**已自带**应答文件时，这几项会被禁用并强制取消（除非你勾选了格式化分区）。
:::

## WiFi 配置迁移

把当前机器的 WiFi 配置带进新系统。程序通过受控的 Windows WLAN API 获取当前 profile；检测不到可迁移的当前配置时，该选项会自动隐藏，不依赖解析本地化的 `netsh` 输出。

## Windows 7 兼容策略

Windows 7 的 USB3、NVMe 和 UEFI 兼容处理已经改为**由最终安装意图自动决定**，不再提供手工勾选框、自定义目录或浏览按钮：

- **USB3**——已确认是 Windows 7 镜像时，自动按当前硬件 ID、目标架构和锁定清单选择受校验驱动。
- **NVMe**——只有 Windows 7 x64 且目标物理盘明确报告为原生 NVMe 时，才按固定依赖顺序应用锁定的微软热修补 CAB；VMD/RAID、未知总线和 x86 不会猜测启用。
- **UEFI**——Windows 7 x64、可能使用 UEFI 且启用引导修复时自动评估。确认 VMware 客体时保留原生微软双入口；其它环境按事务化流程部署受校验的 UefiSeven 双入口，并要求关闭 Secure Boot。

仍保留一个默认关闭的手工兼容尝试：**尝试修复 0xA5（禁用处理器电源驱动）**。它只禁用离线系统的 `intelppm`、`amdppm` 和 `Processor` 服务，不修改 ACPI 表、`acpi.sys` 或固件，因此不是通用 0xA5 修复。

::: warning 已停用旧 0x7B 开关
历史“修复存储控制器蓝屏”字段只为旧配置可解析，当前会被强制关闭。程序不会把一长串互不相关的 IDE/AHCI/RAID/NVMe 服务全部设为 Boot Start。
:::

## Windows XP / 2003 专用开关

检测到 XP/2003 镜像时，会显示其独立的 USB3 / NVMe 选项；AHCI 驱动**始终注入**，对“已 UEFI 化”映像另有 UEFI/GPT 引导路径。详见[Windows XP / 2003 安装](/guide/xp-install)。
