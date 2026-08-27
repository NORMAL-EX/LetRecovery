---
title: 命令行参考
description: 正常系统端安装、备份与版本化配置生成器。
---

# 命令行参考

公开 CLI 只适用于正常系统端 `LetRecovery.exe`。PE 不提供用户 CLI：旧 `/PEINSTALL`、`/PEBACKUP` 已停用并固定拒绝；只有 `/AUTO` 是正常系统端认证交接使用的内部入口，外部脚本不得直接构造或调用。

图形界面可在“关于”页启用默认关闭的自动化导出入口，再从安装或备份页生成 `cli\*.json` 与相对路径 CMD。导出只写文件、不执行任务；它复用下面同一份严格 schema 和验证边界。

```text
inspect disks|image|pe-cache ...
install plan --config <file> | install run --config <file> [--yes] [--dry-run]
backup plan --config <file> | backup run --config <file> [--yes] [--dry-run]
update restore
config generate|validate|normalize|show ...
```

`disable_windows_update` 只设置可逆的 Windows Update 策略和服务启动类型，不删除服务、任务、文件、ACL，也不改动 BITS 或写入 Defender 旧禁用策略。它会阻止 Windows Update/Microsoft Update 的自动投递，包括 Defender 平台和安全情报更新，但 Store、Office、手工安装、企业管理或功能升级仍可能重新部署组件。

`update restore` 只恢复仍由 LetRecovery 拥有且未被管理员、域策略或 MDM 改写的 Windows Update 设置。它要求已提权控制台，但不会自动触发 UAC 或弹出消息框；冲突和部分恢复通过结构化日志及最终 JSON 返回。

高级优化项不会扩大到模糊匹配：`remove_uwp_apps` 仅处理共享固定清单中的精确 Name/PFN，明确保留新 Outlook、OneDrive Sync 与 Win32 OneDrive；Windows 11 还会在默认用户首次生成前关闭开始菜单推荐和预装内容投递，避免“入门”、纸牌、微软电脑管家等动态入口重新出现。`disable_windows_defender` 深度移除 Defender Antivirus 引擎，并仅尽力移除两个精确 SecHealthUI PFN；SecurityHealthService、Windows Security Center 服务、防火墙、UAC、SmartScreen、VBS 和 Defender for Endpoint 保留。`disable_reserved_storage` 只在已确认 Windows 10/11 build 19041+、使用内置无人值守时调用在线 DISM，失败或最终状态未确认只记 warning。

`inspect disks`、`inspect image --path <image>` 和 `inspect pe-cache` 提供 fresh 只读库存，便于先选择目标、镜像卷索引和已验证 PE。

真正执行的 `run` 必须在已经提权的管理员控制台运行并显式使用 `--yes`；程序不会为 CLI 弹出 UAC 或消息框。`plan` 与 `run --dry-run` 不执行写盘，也不要求管理员权限。旧 `--install`/`--advanced` 会返回迁移错误。

配置是 `schema_version: 1` 的严格 JSON，`operation.type` 为 `install` 或 `backup`，未知字段、JSON 任意层级的重复键和重复命令行参数都会被拒绝。驱动行为只由 `driver_action` 决定。备份意图使用 `execution_mode`（`auto|direct|via_pe`）、`output_policy`（`create|replace|append`）和 `auto_reboot`；旧 `incremental` 布尔值被拒绝。当前正常系统端 CLI 放行 WIM/ESD 的 `auto|direct + create|replace|append + auto_reboot=false`；`create` 拒绝既有目标，`replace`/`append` 要求并完整绑定既有普通文件，执行时从同一拒绝写入/删除共享的旧文件句柄复制到私有暂存区，完整验证后通过 PREPARED journal 和句柄 CAS 发布。需要 PE 的系统卷、`via_pe` 和自动重启仍在 plan 阶段失败关闭；PE 端没有公共 CLI。只有显式 `--interactive` 才会启动向导，提示也是 stderr JSON Lines，输入提前结束会失败；覆盖配置必须 `--force`。配置文件发布后会再次核对 protected DACL 只授予当前用户、SYSTEM 和 Administrators，父目录 ACL 不会被修改；`show` 和所有事件都会隐藏密码。

```json
{"schema_version":1,"operation":{"type":"install","target_partition":"C:","image_path":"D:\\install.wim","volume_index":1,"format_partition":true,"repair_boot":true,"auto_reboot":false}}
```

安装字段还包括 `image_backing_path`、`unattended`、`automation_shutdown_on_terminal`、`driver_action`、`boot_mode`、`boot_pca_mode`、`custom_unattend_path`、`inherit_app_install_prefs`、`preinstalled_software_ids` 和 `advanced`。显式继承时，EXE 相邻的有效 `config.json` 是所用安装偏好的唯一来源；缺失或损坏会失败。软件 ID 每次从当前 v4 目录唯一解析，URL 和静默命令不从旧偏好继承。生成器对应接受 `--inherit-app-install-prefs true` 与逗号分隔的 `--preinstalled-software-ids todesk,7zip-x64,bandizip-x64`。`--automation-shutdown-on-terminal true` 专供可丢弃虚拟机：已确认执行的 normal/PE 失败会安排关机；成功会继续启动新系统并等首登逐项尝试所有软件后关机，软件单项失败仍只作为 warning。本地源支持 WIM、ESD、SWM、GHO、GHS；控制器按目标状态选择 Direct 或受认证 ViaPE。ViaPE 的 SWM/GHS 全套连续分卷逐项进入 LRHM3，PE fresh 枚举拒绝 missing/extra/乱序项。当前 ViaPE 对自定义 answer 或 Administrator 密码失败关闭，Direct 支持的组合不受该 gate 影响。`advanced` 支持快捷方式箭头、经典右键、NRO、Windows Update、Defender/SecHealthUI、保留存储、UAC、设备加密、精选 AppX，部署/首次登录脚本、自定义驱动、存储控制器驱动、注册表、文件、用户名、卷标、内置 Administrator、仅在 VMware 来宾中规划的 VMware Tools，以及受控的 Windows 7 ACPI、USB3/NVMe、存储修复、UEFI 补丁和 XP USB3/NVMe 项。图形界面还能把本次 Wi-Fi profile 写入受保护 JSON；SSID、profile XML 和密码都会脱敏，含凭据的字段不接受命令行参数。生成器可设置 `--image-backing-path`、`--install-vmware-tools`、Windows 7 对应开关/路径及内置 Administrator 非密码字段；密码不接受命令行参数。备份字段为 `source_partition`、`save_path`、`name`、`description`、`format`（`wim|esd`）、`execution_mode`、`output_policy` 和 `auto_reboot`。

```json
{"schema_version":1,"operation":{"type":"backup","source_partition":"D:","save_path":"E:\\Backups\\data.wim","name":"Data","description":"Fresh direct backup","format":"wim","execution_mode":"direct","output_policy":"create","auto_reboot":false}}
```

对应的非交互生成命令：

```bat
LetRecovery.exe config generate --operation backup --output D:\lr-backup.json --source-partition D: --save-path E:\Backups\data.wim --name Data --format wim --execution-mode direct --output-policy create --auto-reboot false
```

最终 stdout 是单个 JSON；脱敏进度为 stderr JSON Lines。计划结果包含实际采用的 `effective_config`、fresh 库存绑定和 `warnings`，不是简单回显输入。旧正常端 PE 开关会在读取配置或请求管理员权限前固定拒绝。退出码为：0 成功、2 用法/权限/确认错误、3 配置错误、4 预检错误、5 执行错误。

批处理需用 `start /wait "" LetRecovery.exe ...` 后读取 `%ERRORLEVEL%`；PowerShell 使用 `Start-Process -Wait -PassThru -NoNewWindow` 后读取 `ExitCode`。这是 Windows 子系统单 EXE，程序会复用标准句柄或连接父控制台，不会新增 CLI EXE。

完整字段和示例请参阅仓库文档 `docs/命令行参数.md`。
