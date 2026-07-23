# Defender 杀毒引擎深度移除边界

## 用户语义

高级选项中的“深度移除 Defender 杀毒引擎”只处理 Microsoft Defender Antivirus。它不是“删除 Windows 安全体系”，也不是让 LetRecovery 绕过第三方安全软件。

配置文件为兼容旧版本仍使用字段 `disable_windows_defender`。该字段为 `true` 时，正常端直接安装路径和 PE 端都调用 `lr_core::defender_removal::remove_offline_defender_engine`；不得在任一端复制一份独立删除脚本。

## 为什么会出现“文件包含病毒或潜在的垃圾软件”

资源管理器显示“无法成功完成操作，因为文件包含病毒或潜在的垃圾软件”时，对应的是文件 I/O 被反恶意软件引擎阻止。Microsoft Defender Antivirus 的 `WdFilter` 是文件系统微型筛选器，`WinDefend`/`MsMpEng.exe` 负责检测和处置。文件系统筛选器能够在创建、打开或复制文件时阻止请求，因此这类错误与 SmartScreen 的信誉提示不是同一个界面或执行边界。

启用本选项并成功完成后，Microsoft Defender Antivirus 的筛选器和引擎不应再加载，因此由该引擎产生的阻止会消失。以下情况仍可能阻止文件：

- 用户后来安装了其他杀毒软件；
- SmartScreen 对带有网络来源标记的未签名程序显示独立的信誉提示；
- Windows 功能更新或修复安装重新部署了 Defender Antivirus；
- 企业策略、EDR 或 Microsoft Defender for Endpoint 施加了另一层控制。

因此验收必须区分“Defender Antivirus 的 0x800700E1 文件 I/O 阻止”和“SmartScreen/第三方安全软件的独立提示”，不能把两者混为一谈。

## 删除白名单

共享核心只允许处理以下 Defender Antivirus 引擎服务：

- `WinDefend`
- `WdBoot`
- `WdFilter`
- `WdNisDrv`
- `WdNisSvc`
- `WdAiNisDrv`
- `WdDevFlt`
- `KslD`

只允许处理以下离线目标内路径：

- `ProgramData\Microsoft\Windows Defender`
- `Program Files\Windows Defender`
- `Program Files (x86)\Windows Defender`
- `Windows\System32\drivers\wd`
- `Windows\System32\drivers\WdBoot.sys`
- `Windows\System32\drivers\WdFilter.sys`
- `Windows\System32\drivers\WdNisDrv.sys`
- `Windows\System32\Tasks\Microsoft\Windows\Windows Defender`

同时删除 Defender 自身计划任务的 `TaskCache` 树和经过 GUID 格式验证的对应任务记录，以及 `SOFTWARE\Microsoft\Windows Defender` 引擎状态树。保留的 Defender 策略键用于在缺失文件被系统维护重新放回时继续阻止引擎自动启用；现代 Windows 可能忽略旧 `DisableAntiSpyware` 值，因此策略只属于纵深防御，不能代替文件、驱动、服务和结果复核。

## 明确保留

实现不得删除或禁用以下组件：

- Windows Security UI 与 `SecurityHealthService`
- Windows Security Center `wscsvc`
- Windows Defender Firewall `mpssvc`
- UAC
- VBS、Credential Guard 和 System Guard
- SmartScreen
- Web Threat Defense
- Pluton
- Microsoft Defender for Endpoint，包括 `Sense`
- 与 Defender Antivirus 无关的第三方安全软件

## 安全执行约束

1. 目标只能是带盘符的完整离线 Windows 根目录，必须存在 `SYSTEM` 和 `SOFTWARE` 配置单元。
2. 服务键只按离线 `SYSTEM\Select` 的 `Current`、`Default` 和 `LastKnownGood` 控制集生成，不能固定猜测 `ControlSet001`。
3. 所有待删除路径必须保持在已验证目标根目录下；根路径和任何子项出现 reparse point、junction 或符号链接时立即停止。
4. 文件权限通过 Windows 安全 API 赋予 `BUILTIN\Administrators`，不调用 `cmd.exe`、`takeown.exe`、`icacls.exe`、PowerRun 或外部删除器。
5. 计划任务 ID 必须是规范的带大括号 GUID，随后只能删除该 GUID 对应的 `Tasks`、`Plain`、`Boot`、`Logon` 或 `Maintenance` 子键。
6. 注册表写入、ACL 修改、文件删除、服务键删除和删除后复核任何一步失败，都必须返回错误。用户选择了该选项时，两端安装流程必须停止，不能记录警告后继续完成。
7. 自动测试不得对宿主系统执行真实删除；生产行为只能在可丢弃虚拟机的离线目标上验证。

## 验证矩阵

至少在原版 Windows 11 x64 的可丢弃虚拟机中验证以下项目：

1. 安装前确认高级选项已明确勾选。
2. PE 日志中出现移除报告，且没有 ACL、注册表或删除后复核错误。
3. 首次启动后 `sc query WinDefend`、`sc query WdFilter`、`sc query WdBoot`、`sc query WdNisSvc` 和 `sc query WdNisDrv` 均报告服务不存在。
4. `fltmc filters` 不包含 `WdFilter`。
5. 删除白名单中的目录和驱动不存在。
6. `SecurityHealthService`、`wscsvc`、`mpssvc` 和 `Sense`（镜像原本包含时）仍存在。
7. Windows 防火墙、UAC、SmartScreen 和 Windows Security UI 仍可用。
8. 复制此前触发 0x800700E1 的同一 LetRecovery 构建，确认不再由 Microsoft Defender Antivirus 阻止；同时记录是否出现独立 SmartScreen 提示。
9. 再次运行 LetRecovery，确认程序能够启动并读取配置。

## 参考与来源边界

- Microsoft 文件系统筛选器文档说明反病毒微型筛选器可以监视、修改或阻止文件 I/O：<https://learn.microsoft.com/windows-hardware/drivers/ifs/about-file-system-filter-drivers>
- Microsoft Defender 服务排障文档列出 Defender Antivirus 的核心服务，并说明现代版本会忽略旧 `DisableAntiSpyware` 策略：<https://learn.microsoft.com/defender-endpoint/troubleshoot-onboarding>
- 设计时审计了开源 DefenderRemover：<https://github.com/ionuttbara/windows-defender-remover>。该项目的“仅移除杀毒”资源仍会删除 Security Center、System Guard、Web Threat Defense、Pluton 等超出本功能范围的组件，因此 LetRecovery 不执行、不捆绑也不复制其注册表包、PowerRun 或可执行文件，只把它作为范围审计参考。

LetRecovery 的实现是独立的严格白名单边界；本功能没有新增第三方二进制。
