# PCA2023 离线兼容资源

LetRecovery 在 UEFI 安装 Windows 10、Windows 11 或 Server 2016 及更高版本时，可以为缺少 BootEx 的旧 WIM、ESD、SWM 镜像补齐 PCA2023 启动资源。该流程完全离线，不依赖服务端目录或安装时网络。

PCA 选择不会显示在 XP/2003、Vista、Windows 7、Windows 8/8.1、GHO/GHS 或 Legacy/BIOS 安装中。Vista/7/8/8.1 的 WIM 安装在 Secure Boot 关闭或固件仍信任 PCA2011 时保持原路径；若 Secure Boot 已启用且 PCA2011 已撤销，则在格式化前明确停止，因为这些系统不能通过 BootEx 资源包安全升级到 PCA2023。XP/2003 继续使用既有 NT5 专用引导路径。LetRecovery 的安装链只支持 x86 和 x64，不支持 ARM64 镜像。

## 资源族

发布包的 `bin/pca2023` 必须包含：

```text
pca2023-legacy-amd64.wim
pca2023-windows10-x86.wim
pca2023-modern-amd64.wim
```

- x64 Build 低于 26100 使用第一份资源；覆盖 Windows 10、Windows 11 21H2/22H2/23H2 和 Server 2016/2019/2022。
- x86 Windows 10 使用第二份资源；x86 Build 22000 及更高版本会被拒绝。
- x64 Build 26100 及更高版本使用第三份资源；覆盖 Windows 11 24H2+ 和 Server 2025+。

资源族不是任意跨版本复制。每次更换资源源版本时，必须重新完成本文末尾的虚拟机支持矩阵验证；没有验证记录不能扩大代码中的版本范围。

## WIM 白名单布局

每份资源 WIM 的卷索引固定为 `1`，只允许包含安装后需要写入离线 Windows 的以下路径：

```text
\Windows\Boot\EFI_EX\bootmgfw_EX.efi
\Windows\Boot\FONTS_EX\*_EX.ttf
\Windows\Boot\EFI_EX\bootmgr_EX.efi   # 可选
\Windows\Boot\EFI\boot.stl           # 可选
```

客户端在格式化前检查包是普通文件、大小不超过 256 MiB、核心路径完整、`bootmgfw_EX.efi` 具有有效 PCA2023 签名且 PE 架构与目标 WIM 一致。正常系统端传给 PE 的副本另以 SHA-256 绑定；缺失、计算失败或不匹配均失败关闭。微软官方媒体脚本将 `bootmgr_EX.efi` 和 `boot.stl` 视为可选资源，LetRecovery 同样只在存在时复制。

程序不会从资源包释放 BCD、脚本、注册表、日志、机器 ESP 或其他文件。注入后会再次检查离线系统中的 PCA2023 BootEx 签名。

## 制作资源包

使用已安装对应微软安全更新的官方 `boot.wim` 作为来源。不要使用某台机器 ESP 的备份，也不要使用 Insider Build 充当旧系统通用源。

```powershell
pwsh .github/scripts/build-pca2023-pack.ps1 `
  -SourceWim C:\Media\sources\boot.wim `
  -ImageIndex 1 `
  -Architecture amd64 `
  -OutputWim C:\Output\pca2023-legacy-amd64.wim
```

脚本只挂载源 WIM 为只读，从固定白名单复制资源，验证微软签名和架构，再捕获最小 WIM。它不格式化、分区或修改真实磁盘。正式构建从 `letrecovery-package` 底包取得三份 WIM；缺少任一文件时 release 会停止，并将同一套资源同时放入桌面端和 PE WIM。

## BCDBoot 部署

目标离线 Windows 获得完整 `EFI_EX`、`FONTS_EX` 和 `boot.stl` 后，LetRecovery 优先执行 `bcdboot /bootex`。若当前 BCDBoot 不支持该参数，程序先用普通 BCDBoot 创建 BCD 和标准目录，再执行受控回退：

- `bootmgfw_EX.efi` 验证为 PCA2023 后替换 ESP 主入口；
- `bootmgr_EX.efi` 验证微软签名后写入配套位置；
- `*_EX.ttf` 去掉 `_EX` 后写入 ESP 字体目录；
- ESP 缺少 `boot.stl` 时补齐；
- 根据 PE 机器类型写入 `bootia32.efi` 或 `bootx64.efi` fallback，并再次验签。

`bootmgr_EX.efi` 本身不一定由 Windows UEFI CA 2023 签发，因此这里只要求有效微软签名；固件直接验证的 `bootmgfw.efi` 和 fallback 入口必须匹配最终选择的 PCA 代际。

## 发布验证

普通 CI 只做纯逻辑、签名读取和命令建模，不写 ESP。真实验证必须使用可丢弃虚拟机和 VHDX，至少覆盖：

- Win10 1607/1809/21H2/22H2 x64；
- Win10 1607/1809 x86；
- Win11 21H2/22H2/23H2、24H2/25H2 x64；
- Server 2016/2019/2022/2025 x64；
- 固件仅信任 PCA2011、同时信任两代、已撤销 PCA2011 三种状态；
- 镜像原生包含 BootEx、使用离线资源、缺包、错架构、损坏包、错误 SHA-256；
- 支持 `/bootex` 和不支持 `/bootex` 的 BCDBoot。

发布前记录三份 WIM 的来源 KB、源系统版本、文件版本、SHA-256 和虚拟机结果。资源有问题时回滚整个应用 release 或恢复上一版底包，不能静默换成其他架构或未经验证的 Build。

参考：

- [Microsoft: BCDBoot command-line options](https://learn.microsoft.com/windows-hardware/manufacture/desktop/bcdboot-command-line-options-techref-di)
- [Microsoft: Updating Windows bootable media to use the PCA2023 signed boot manager](https://support.microsoft.com/topic/d4064779-0e4e-43ac-b2ce-24f434fcfa0f)
- [Microsoft secureboot_objects: Make2023BootableMedia.ps1](https://github.com/microsoft/secureboot_objects/blob/main/scripts/windows/Make2023BootableMedia.ps1)
