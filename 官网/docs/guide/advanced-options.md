---
title: 高级选项
description: 驱动、无人值守、注册表优化与系统优化。
---

# 高级选项

在系统安装页打开**高级选项**，可对部署做精细调整。

## 保留个人文件重装

启用后，LetRecovery 不格式化所选 Windows 分区，而是在受认证 PE 中把每个普通本地用户的**桌面、文档、下载、图片、音乐、视频**同卷移动到根目录下的 `LetRecovery_Preserved_<会话标识>`，再真实删除旧 `Windows`、`Program Files*`、`ProgramData` 和剩余旧用户配置以立即释放空间。未知顶层数据目录不会被删除；这不是完整系统备份，也不保留 `AppData`、应用或系统设置。

新系统首次登录会自动多重启一次（内置 Administrator 账户复用已有的账户切换重启）。第二次登录时 LetRecovery 会先显示恢复等待界面，把资料合并到当前登录用户实际的 Windows 已知文件夹并完成回读；只有恢复成功后才启动 Windows 桌面。Windows 自动生成的 `desktop.ini` 目录显示元数据会被忽略，不会作为个人文件恢复或在桌面产生同名冲突副本。恢复失败时桌面保持关闭，诊断材料和保留源不会被冒充为成功结果。

该选项只支持包含既有 Windows 的单分区重装和 Windows 7 及以上 WIM/ESD/SWM 镜像；程序会自动关闭格式化并进入 PE。GHO/XP、全盘重装和双系统不支持。个人目录包含重解析点、EFS 加密文件或仅在线占位数据时会在删除旧系统前停止。保留的 Desktop 中，明确指向原 C 盘或 PE 当前离线系统卷、且位于对应 `Users` 以外位置的 `.lnk` 快捷方式会在 PE 中删除；其他盘、网络、相对目标或无法可靠解析的快捷方式保持不动。

## 驱动

- **导出 / 导入驱动**——把第三方驱动保留到重装后的系统。在线导出优先使用 Windows 自带 DISM，失败时使用受支持的 SetupAPI 枚举；离线导出只使用 DISM，不手工拼装 DriverStore。恢复时先让微软 DISM 导入整个目录，失败后再逐个 INF 隔离；Wi-Fi、网卡、打印机、虚拟机等非启动存储可选包的真实失败会记录并跳过，启动存储驱动仍必须回读验证。程序不会在 DISM 前用另一套复杂验签器把普通驱动提前误判失败，也不会自动使用 `/ForceUnsigned`。
- **磁盘控制器驱动注入**——在目标系统支持时，可导入随包锁定的存储控制器驱动。程序通过 SetupAPI 读取当前机器的完整 PCI 硬件 ID，只导入唯一匹配且通过签名与哈希校验的包；匹配不唯一或无法确认时不会猜测，也不会递归注入整个驱动目录。

## 无人值守

- 使用内置生成的 `unattend.xml`，或选择你**自己**的无人值守文件。
- 选择普通自定义用户名，或启用并按 RID-500 身份处理内置 Administrator；也可自定义系统盘卷标。Administrator 密码只存在于当前安装会话和无人值守文件，不写入持久化偏好或日志。
- 程序还会**自动检测**目标分区、安装介质根目录、以及镜像内部是否已自带应答文件，并据此默认勾选无人值守。
- v4 软件目录可用且启用内置无人值守时，**预装应用选择**会打开按分类组织的复选列表。正常系统端会先下载所选安装包并把实际字节计入数据暂存分区；PE 端完成系统释放后，把经过认证的安装包复制到目标系统并在首次登录时逐项按目录声明的受检静默参数安装，随后删除安装包。单项安装失败写入目标系统日志并继续其它收尾。
- VMware Tools 不出现在通用选择窗口；只有明确检测到 VMware 且 v4 目录提供 `vm_tools=true` 条目时，高级页才显示独立且默认勾选的 **安装 VMware Tools**。

::: tip 自定义应答文件的生效范围
当前受认证的**经 PE 安装**流程不接受自定义 `unattend.xml`、自定义 `winnt.sif` 或 Administrator 密码；选择这些组合时会在写入目标前失败关闭。它们只有在支持的直接安装路径中才会被复制到目标系统。内置生成的无人值守文件仍由经 PE 安装流程支持。
:::

## 系统优化

应用到新部署的系统：

- 删除固定清单中的预装 AppX，并保留新 Outlook 与 OneDrive <Badge type="tip" text="依赖无人值守" />
- 绕过 OOBE "必须联网"（BypassNRO）<Badge type="tip" text="依赖无人值守" />
- **禁用 Windows 更新**——写入可逆策略并禁用 Windows Update 服务；不会删除更新组件，也不会把“永远无法手工更新、无法被企业策略恢复”当作保证。需要恢复时可使用命令行 `update restore`。
- **深度移除 Microsoft Defender Antivirus 杀毒引擎**——只处理 Defender Antivirus 引擎、驱动和自身计划任务；另外仅对 `Microsoft.SecHealthUI_8wekyb3d8bbwe` 与 `Microsoft.Windows.SecHealthUI_cw5n1h2txyewy` 两个精确包做尽力移除。保留 SecurityHealthService、Windows Security Center 服务、UAC、防火墙、SmartScreen、VBS 与 Defender for Endpoint；SecHealthUI 不可移除时只记录警告，不会把界面移除伪装成成功。
- Win11 恢复经典右键菜单、去除快捷方式小箭头
- 禁用 UAC、系统保留空间、自动设备加密。系统保留空间只在确认目标为 Windows 10/11 build 19041+、使用内置无人值守时调用微软支持的在线 DISM 接口；不支持或最终状态未确认时记录警告，不写内部离线注册表值。

AppX 项只处理共享固定清单中的精确 Name/PFN，离线撤销 provisioning，并在内置无人值守阶段处理所有用户注册；它明确保留新 Outlook、OneDrive Sync 与 Win32 OneDrive。Windows 11 还会在默认用户首次生成前关闭开始菜单推荐和预装内容投递，避免“入门”、纸牌、微软电脑管家等没有已安装 AppX 身份的动态入口重新生成；不会编辑不透明的开始菜单缓存。

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
