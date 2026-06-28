---
title: 命令行参数与无人值守安装
description: LetRecovery 与 PE 端的命令行参数、安装/高级选项 JSON 配置。
---

# 命令行参数与无人值守安装

LetRecovery 由两个可执行程序组成，各自支持一组命令行参数：

- **`LetRecovery.exe`**（正常系统端，在已安装的 Windows 中运行）
- **`LetRecoveryPE.exe`**（PE 端，在 WinPE 中运行）

参数名**大小写不敏感**，同时提供 `/FLAG` 与 `--flag` 两种写法。

::: tip 关于 PE 端文件名
PE 端的可执行文件名是 **`LetRecoveryPE.exe`**（Cargo 包名 `letrecovery-pe` 仅为内部名称）。
:::

## 一、LetRecovery.exe（正常系统端）

| 参数 | 别名 | 说明 |
| --- | --- | --- |
| *(无参数)* | | 启动图形界面。非管理员运行时会自动请求 UAC 提权（并转发原命令行参数）。 |
| `--install` | `/INSTALL` | **命令行无人值守安装**（从桌面一键驱动重装，见下文）。需配合 `--config`。 |
| `--config <路径>` | `/CONFIG`，`--config=<路径>` | 指定**安装配置 JSON**（见 [install.json](#安装配置-install-json)）。 |
| `--advanced <路径>` | `/ADVANCED`，`--advanced=<路径>` | 指定**高级选项 JSON**（可选，见 [advanced.json](#高级选项-advanced-json)）。 |
| `/PEINSTALL` | `--pe-install` | 读取数据分区中已写好的安装配置并执行安装（通常由程序自身在准备后调用）。 |
| `/PEBACKUP` | `--pe-backup` | 读取数据分区中已写好的备份配置并执行备份。 |

### 命令行无人值守安装

```bat
LetRecovery.exe --install --config <install.json> [--advanced <advanced.json>]
```

**行为**：在**正常 Windows 桌面**执行；读取 `--config`（与可选的 `--advanced`）→ 把镜像放进
自动选定的数据分区 → 写入安装配置与目标盘标记 → 设置下次重启进 PE →（默认）重启。重启后
进入 PE 由 PE 端读取配置完成实际部署（格式化目标盘、释放镜像、导入驱动、应用高级选项、
修复引导）。

::: warning 必须在已提权环境运行
需在"已提权（管理员）"环境运行（如以管理员身份打开的命令行 / 计划任务）。若从非提权环境
启动，会弹 UAC；同意后参数会被转发并继续。
:::

## 二、LetRecoveryPE.exe（PE 端）

| 参数 | 别名 | 说明 |
| --- | --- | --- |
| *(无参数)* | | 启动 PE 图形界面。 |
| `/PEINSTALL` | `--pe-install` | 命令行模式自动安装（读取数据分区配置，无 GUI）。 |
| `/PEBACKUP` | `--pe-backup` | 命令行模式自动备份。 |
| `/AUTO` | `--auto` | 自动检测数据分区中的配置类型（安装/备份）并启动相应界面；未找到配置则提示。 |

## 安装配置 install.json

`--config` 指向的 JSON。**前三项必填**，其余可省略（取下表默认值）。

| 字段 | 类型 | 默认 | 说明 |
| --- | --- | --- | --- |
| `target_partition` | string | **必填** | 要重装的系统盘盘符，如 `"C:"`。进 PE 后会被格式化。 |
| `image_path` | string | **必填** | 镜像**绝对路径**（`.wim`/`.esd`/`.swm` 或 `.gho`/`.ghs`）。 |
| `pe_path` | string | **必填** | PE 启动文件**绝对路径**（`.wim` 或 `.iso`）。 |
| `volume_index` | number | `1` | 镜像内分卷索引（WIM/ESD 多版本时选择）。 |
| `is_gho` | bool | 按扩展名自动判断 | 是否 GHO 镜像。 |
| `driver_action_mode` | number | `0` | 驱动处理：`0`=不处理，`1`=仅备份，`2`=自动导入（从数据目录 `drivers\` 导入）。 |
| `unattended` | bool | `false` | 是否生成无人值守配置。 |
| `auto_reboot` | bool | `true` | 准备完成后是否自动重启进 PE。 |
| `custom_unattend_path` | string | `""` | 自定义无人值守 XML 绝对路径（会被复制进数据目录）。 |
| `data_partition` | string\|null | `null`（自动选择） | 暂存配置/镜像的数据分区盘符；缺省自动选一个空间足够、非目标盘的分区。 |
| `pe_display_name` | string\|null | `"LetRecovery PE"` | PE 启动项显示名。 |

示例：

```json
{
  "target_partition": "C:",
  "image_path": "D:\\Images\\Win11_24H2.wim",
  "pe_path": "D:\\LetRecovery\\bin\\pe\\LetRecovery_PE.wim",
  "volume_index": 1,
  "is_gho": false,
  "driver_action_mode": 2,
  "unattended": true,
  "auto_reboot": true,
  "custom_unattend_path": "",
  "data_partition": null,
  "pe_display_name": "LetRecovery PE"
}
```

## 高级选项 advanced.json

`--advanced` 指向的 JSON，对应程序内「高级选项」。**可只写需要的字段**，其余取默认值。
下列字段会在 PE 安装流程中**生效**（与图形界面"重启进 PE 安装"路径完全一致）：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `bypass_nro` | bool | OOBE 绕过强制联网。 |
| `remove_uwp_apps` | bool | 删除预装 UWP 应用。 |
| `import_storage_controller_drivers` | bool | 导入磁盘控制器驱动（Win10/11 x64）。 |
| `disable_windows_update` | bool | 禁用 Windows 更新。 |
| `disable_windows_defender` | bool | 禁用 Windows 安全中心。 |
| `disable_reserved_storage` | bool | 禁用系统保留空间。 |
| `disable_uac` | bool | 禁用用户账户控制。 |
| `disable_device_encryption` | bool | 禁用自动设备加密。 |
| `remove_shortcut_arrow` | bool | 移除快捷方式小箭头。 |
| `restore_classic_context_menu` | bool | Win11 恢复经典右键菜单。 |
| `custom_username` + `username` | bool + string | 自定义用户名（`custom_username=true` 时取 `username`）。 |
| `custom_volume_label` + `volume_label` | bool + string | 自定义系统盘卷标。 |
| `win7_uefi_patch` | bool | Win7 UEFI 补丁（UefiSeven）。 |
| `win7_inject_usb3_driver` | bool | Win7 注入 USB3 驱动。 |
| `win7_inject_nvme_driver` | bool | Win7 注入 NVMe 驱动。 |
| `win7_fix_acpi_bsod` | bool | Win7 修复 ACPI 蓝屏。 |
| `win7_fix_storage_bsod` | bool | Win7 修复存储控制器蓝屏。 |

示例：

```json
{
  "bypass_nro": true,
  "remove_uwp_apps": true,
  "import_storage_controller_drivers": true,
  "disable_device_encryption": true,
  "custom_username": true,
  "username": "User",
  "custom_volume_label": true,
  "volume_label": "OS"
}
```

::: warning 注意
- `AdvancedOptions` 中其余更丰富的项（自定义脚本、自定义驱动目录、注册表导入、自定义文件、
  迁移 WiFi 等）**不属于"重启进 PE 安装"流程**（图形界面同样如此），即便写进 advanced.json
  也不会在本命令行安装流程中生效。
- 命令行安装的**端到端流程依赖真实重装环境**（PE 启动 + 重启 + 部署），请在真机/虚拟机回归
  验证。
- 路径建议使用绝对路径；JSON 中的反斜杠需转义（`"D:\\Images\\x.wim"`）。
:::
