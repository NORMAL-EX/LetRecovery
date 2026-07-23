---
title: Secure Boot 与 PCA2011 / PCA2023
description: 了解两代引导签名、自动选择逻辑、离线兼容资源及旧版 Windows 的限制。
---

# Secure Boot 与 PCA2011 / PCA2023

PCA2011 与 PCA2023 指的是 Windows EFI 启动组件使用的**签名信任代际**，不是 Windows 版本号，也不是简单比较 `winload.efi` 文件版本。开启 Secure Boot 后，UEFI 固件会依据自身的允许数据库（DB）和撤销数据库（DBX）验证 EFI 启动组件；固件是否信任相应证书，决定了这台机器能否启动对应代际的 Windows Boot Manager。

::: tip 通常保持“自动”即可
LetRecovery 会结合固件信任状态、现有 ESP、所选镜像版本与架构自动选择。只有明确了解目标机器的 Secure Boot 配置时，才建议手动指定 PCA2011 或 PCA2023。
:::

## 两代签名有什么区别

| 项目 | PCA2011 | PCA2023 |
| --- | --- | --- |
| 常见签发链 | Windows Production PCA 2011 | Windows UEFI CA 2023 / Windows Production PCA 2023 |
| 主要用途 | 兼容既有 Windows 启动组件和旧固件 | 新一代 Windows Secure Boot 启动组件 |
| 固件要求 | DB 中仍信任 2011 代证书，且目标组件未被 DBX 阻止 | DB 中包含对应的 2023 代证书 |
| LetRecovery 行为 | 保留兼容的旧启动链 | 验证并部署 BootEx 资源 |

证书年代变化不等于“日期一到所有旧系统立刻无法启动”。真正影响启动的是固件中的 DB/DBX、具体 EFI 文件的签名和撤销状态，以及机器是否开启 Secure Boot。因此 LetRecovery 不根据日期猜测，而是读取当前机器能够观察到的实际状态。

## LetRecovery 如何自动判断

在 UEFI 安装现代 Windows 时，LetRecovery 会在格式化目标分区**之前**完成以下只读检查：

1. 读取所选 WIM、ESD 或 SWM 卷的 Windows 主版本、Build 和架构。
2. 检查 Secure Boot 状态、固件对两代证书的信任以及 PCA2011 是否已不可用。
3. 检查现有 ESP 的启动代际，并读取镜像里的 `bootmgfw.efi` 与 `EFI_EX\bootmgfw_EX.efi`。
4. 验证 EFI 文件签名和 x86/x64 架构，不使用文件名或单个版本字符串代替验签。
5. 在自动模式下优先保留当前仍兼容的启动链；若 PCA2011 已不可用，则只有确认固件信任 PCA2023 后才继续。

无法确认固件信任、资源缺失、签名无效、架构错误或完整性校验失败时，安装会在写盘前停止，不会把“未知”当成成功。

## 镜像没有 PCA2023 文件怎么办

Windows 10、Windows 11 和 Server 2016 及更高版本的旧镜像可能没有完整 BootEx 目录。LetRecovery 随完整发布包携带三套经过锁定的离线资源：

- Windows 10、Windows 11 21H2–23H2 与 Server 2016/2019/2022 x64；
- Windows 10 x86；
- Windows 11 24H2 及更高版本与 Server 2025 及更高版本 x64。

程序会按目标镜像 Build 和架构选择对应 WIM，只释放固定白名单中的 `EFI_EX`、`FONTS_EX` 和可选 `boot.stl`。资源包、暂存副本和关键 EFI 文件分别经过大小、SHA-256、签名、架构和路径检查。这一过程**完全离线**，安装时不需要从服务器下载引导文件。

## BCDBoot 与兼容回退

资源准备完成后，LetRecovery 优先调用支持 `/bootex` 的 BCDBoot。若当前 PE 或系统中的 BCDBoot 较旧、不认识该参数，程序会：

1. 使用普通 BCDBoot 创建标准 BCD 和目录；
2. 从已验证的 BootEx 资源写入正确架构的启动管理器；
3. 部署配套字体和 fallback 入口；
4. 再次验证最终 ESP 中的启动代际。

回退只是兼容旧 BCDBoot，并不会降低签名、架构或资源白名单检查。

## 哪些系统会显示 PCA 选项

| 目标系统或启动方式 | PCA 选项 | 处理方式 |
| --- | --- | --- |
| Windows 10 / 11、Server 2016+，UEFI，x86/x64 | 显示 | 自动检测，也可手动覆盖 |
| Legacy / BIOS 安装 | 不显示 | Secure Boot 与 EFI PCA 不参与该启动链 |
| XP / 2003、Vista、Windows 7、Windows 8/8.1 | 不显示 | 保留原有兼容路径，不注入现代 BootEx |
| ARM64 镜像 | 不支持 | LetRecovery 当前工具链仅支持 x86/x64 |

## Windows 7 与 UefiSeven

UefiSeven 用来在缺少 CSM 的 UEFI Class 3 机器上模拟 Windows 7 需要的 Int10h 环境。它解决的是旧系统的显示启动兼容问题，**不会**把 Windows 7 的引导链升级为 PCA2023。

LetRecovery 随包提供的 UefiSeven 启动文件没有微软 PCA2023 签名，因此使用这条路径时必须关闭 Secure Boot。即使自行注册证书给 UefiSeven 签名，后续 Windows 7 原版启动组件仍属于旧信任链，也不能成为适用于普通用户和任意固件的 PCA2023 方案。

若目标是 Windows 7/8.1，Secure Boot 已开启且固件不再允许旧启动链，LetRecovery 会在格式化前停止。不要把 Windows 10/11 的 BootEx 文件直接复制进 Windows 7，这不能安全地升级整个启动链。

## `winload.efi` 需要替换吗

通常不需要。UEFI 固件首先验证 ESP 中的 Windows Boot Manager，`winload.efi` 由后续 Windows 启动链加载。仅凭 `winload.efi` 是否出现“2011”或“2023”不能判断固件应选择哪种 PCA 模式。

LetRecovery 不再尝试用当前系统的 `winload.efi` 覆盖随包 PE，也不要求目标 Windows 与 PE 的该文件版本相同。PCA2023 兼容处理集中在经过验证的 Boot Manager、BootEx 资源和最终 ESP 上。

## 遇到启动兼容问题

- 首先把 PCA 模式恢复为**自动**，并安装主板厂商最新稳定固件。
- 不要从其他电脑手工复制整个 EFI 目录；机器的 BCD、固件变量和磁盘标识并不相同。
- Windows 7 + UefiSeven 请确认 Secure Boot 已关闭。
- 记录报错页面，并保留桌面端或 PE 端日志；签名、固件状态、资源选择和 BCDBoot 回退都会写入日志。

参考资料：

- [Microsoft：BCDBoot 命令行选项](https://learn.microsoft.com/windows-hardware/manufacture/desktop/bcdboot-command-line-options-techref-di)
- [Microsoft：更新可启动介质以使用 PCA2023 签名的启动管理器](https://support.microsoft.com/topic/d4064779-0e4e-43ac-b2ce-24f434fcfa0f)
- [Microsoft：Windows Secure Boot 密钥创建与管理指南](https://learn.microsoft.com/windows-hardware/manufacture/desktop/windows-secure-boot-key-creation-and-management-guidance)
