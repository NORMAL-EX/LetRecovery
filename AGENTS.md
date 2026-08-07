# LetRecovery Agent Development Guide

本文档适用于整个仓库。它既是开发者的架构索引，也是代码模型在修改 LetRecovery 时必须遵守的开发契约。

LetRecovery 是具有管理员权限的 Windows 系统安装、备份和磁盘维护工具。兼容性、可恢复性和防止误操作始终高于重构速度、代码简短程度或界面效果。

## 文档同步是完成定义的一部分

任何开发者或代码模型在结束一次功能开发前，都必须执行以下检查：

1. 每完成一个新功能，无论是否新增文件，都必须同步更新本文档中相关文件的职责描述、扩展路径或安全约束；不能以“仍然使用原文件”为理由跳过。
2. 检查本次是否新增、删除、移动、重命名了 `.rs` 文件，或改变了现有文件的主要职责、公开接口和安全边界。
3. 发生上述变化时，必须在同一批修改中更新本文档的“Rust 文件职责目录”和相关开发说明。
4. 新增的每个 `.rs` 文件必须在职责目录中出现且只出现一次；删除或移动文件时必须同步删除或修改旧条目。
5. 新功能涉及正常系统端和 PE 端时，必须同时检查两端和 `lr-core`，不得只更新一个副本。
6. 如果在开发或审查时发现本文档遗漏、描述过时或与代码不符，应立即补齐，不要等待单独的文档任务。
7. 向用户汇报完成前，应明确说明本文档更新了哪些内容；如果只是修复而没有改变职责，也必须说明已经复核且无需修改职责目录。

如果代码和本文档冲突，以经过验证的代码行为为准，但必须在当前修改中修正文档。不能为了符合文档而猜测或悄悄改变现有行为。

## 仓库结构与依赖方向

- `lr-core/`：两端共享的核心策略、纯逻辑、Windows 适配和可测试命令边界。
- `正常系统端/`：桌面环境主程序，负责系统安装、备份、在线下载、工具箱和写入 PE 启动配置。
- `PE端/`：WinPE 中运行的安装、备份、扩容和离线系统处理程序。
- `官网/`：React、TypeScript、Vite 官网和文档站。
- `assets/`：发布包资源、语言文件、工具和内置运行时文件。
- `docs/`：架构、安全、第三方二进制来源及用户文档。
- `.github/ISSUE_TEMPLATE/`：问题与功能反馈表单；Bug 表单只要求用户描述现象、复现步骤、上传对应阶段日志和截图，不得再要求手工抄写日志已经自动记录的软件版本、源/目标系统、固件、Secure Boot、BitLocker、磁盘结构、所用 PE 和实体机/虚拟机线索。
- `.github/workflows/`：PR、主分支和发布流水线；release 在替换 PE 原生 Win32 EXE 时必须同时从底包 WIM 删除旧 `opengl32.dll`，不得把已停用的 egui/glow 运行时重新带入发布包，并必须确保 WIM 中存在 `Users\Default\AppData\Local\Temp`，避免未显式指定 `/ScratchDir` 的 DISM 在 WinPE 中因默认 `%TEMP%` 目录缺失而以错误 3 失败；正常系统端的 `assets/release/bin/dprk_easter_egg.mp3` 必须经过大小和复制后 SHA-256 校验再同步到最终 `pkg/bin/`，不得注入 PE WIM；从外部底包生成正式或预发布包时必须由 `.github/scripts/normalize-release-config.ps1` 以仓库受控根级 `config.json` 为唯一默认模板重建整份 `pkg/config.json`，只能保留本次构建并重新计算的 `pe_cache` 元数据，严禁继承底包维护机的界面偏好、提示关闭状态、本地路径、账户选项或安装选项，并必须显式保持 `log_enabled=true`、`easy_mode_enabled=false`、`easy_mode_tip_dismissed=false`、`easy_mode_settings_tip_dismissed=false` 和 `enable_advanced_options=false`；重建必须原子替换并回读验证，最终 `LetRecovery.7z` 还必须重新解出 `config.json`，验证它与已审计 `pkg/config.json` 字节哈希一致且没有模板外字段，任何缺失、解析失败、残留或回读不一致都必须阻断发布，离线安装器也只能从同一份已审计 `pkg/` 构建；正式或预发布 Release 必须在最终 `pkg/` 生成后使用固定 Inno Setup 6.7.1 构建离线安装器，完整校验后只上传 `LetRecovery.7z` 和 `LetRecovery-Setup-x64.exe` 两个资产。Release 创建前必须用本次标签更新工作区中的 `官网/version.json` 并完成官网 lint、类型检查和构建；只有 Release 创建成功后才允许仅提交该版本文件并推回实际发布分支，失败流程不得留下版本提交，版本提交必须防止递归触发 CI。
- `installer/`：基于 Inno Setup 6.7 现代动态主题的 x64 离线安装器工程，从完整 `pkg/` 生成支持静默安装和卸载的单文件 `LetRecovery-Setup-x64.exe`；CI 与本地构建必须复用 `build-installer.ps1` 的包结构、版本和输出校验，不能在工作流中复制另一套编译逻辑。

官网 Markdown 由 `官网/plugins/markdown.ts` 在构建期生成 HTML、标题和纯正文 `searchText`；`DocsSearch` 必须同时索引页面标题、描述、标题层级和正文。页头中的搜索组件使用始终渲染的 lazy/Suspense 边界，并通过 `active` 控制宽度和透明度，禁止按 `isDocs` 条件挂载，否则进入/离开文档页的动画会消失。官网关于页的版本只能来自受版本控制的 `官网/version.json`，普通构建和部署不得根据当前时间动态改变版本；`.github/scripts/update-website-version.ps1` 是 Release 标签规范化、原子写入和回读验证的唯一入口。

依赖方向必须保持为：正常系统端和 PE 端可以依赖 `lr-core`，`lr-core` 不得反向依赖任一端。两端出现相同的纯逻辑、命令构建或 Windows API 适配时，应优先迁移到 `lr-core`，端内保留兼容再导出或很薄的环境适配。

## 不可违反的安全规则

### 禁止在开发机和普通 CI 中执行真实破坏性操作

自动测试、代码模型和普通 CI 禁止真实执行以下操作：

- `format`、`format.com`、DiskPart 写操作和卷删除；
- DISM 镜像释放、捕获或对宿主机、测试磁盘、VHD/VHDX 中离线系统的写入；
- BCD、ESP、引导扇区和活动分区修改；
- 分区创建、移动、扩容、写盘或镜像还原；
- 离线注册表注入、SAM 修改、重启或关机。

相关测试必须使用纯函数、`DryRunCommandExecutor`、mock、临时普通文件或显式 preview。必须进行真实验证时，只能使用可丢弃虚拟机、可丢弃 VHDX 和专用测试磁盘，并由人工明确启动。

发布包 WIM 维护是唯一例外：用户在当前任务中明确授权后，代码模型可以更新仓库工作区内用户明确指定的发布包 WIM；该操作属于构建产物维护，不得借此挂载或修改宿主机、测试磁盘、VHD/VHDX 中的离线系统。执行时必须同时满足以下条件：

- 先解析并复核 WIM、编译产物、挂载目录和最终目标的绝对路径；WIM 与最终目标必须位于当前仓库工作区，挂载目录必须是本次创建的碰撞安全空临时目录。
- 只能先复制到碰撞安全的临时 WIM，再对临时副本执行挂载、文件替换、过时运行库删除、提交和导出清理；原 WIM 在全部步骤验证成功前不得修改。
- 注入范围只能是本次已构建并验证的 PE 程序、发布流程规定的固定离线资源，以及用户明确要求删除的过时发布运行库；不得注入任意宿主文件或改变 PE 的系统配置、注册表、驱动和引导设置。
- 必须检查每条 DISM 命令的启动结果和退出码；任何失败都要卸载并丢弃临时副本，清理挂载目录，保留原 WIM 不变，禁止在状态不明时继续或提交。
- 提交后必须导出到新的干净 WIM，只读复核镜像索引、内部目标文件、已删除文件、PE 导入依赖和文件哈希；全部通过后才可原子替换原 WIM，并同步更新 `pkg/config.json` 中对应的 MD5、SHA-256 或大小字段。
- 需要管理员权限时必须通过显式提权审批，不得静默绕过；授权只覆盖本次指定的发布包 WIM，不覆盖任何真实磁盘、分区、引导或离线系统操作。

### 危险命令的实现要求

- 内置磁盘、分区、卷、盘符、格式化、扩缩、活动标记和 MBR 签名操作统一复用 `lr-core::windows_storage` 的参数化 VDS/WinAPI/IOCTL 边界，不得再生成或启动 DiskPart 脚本；历史 `RunDiskpartScripts` 配置字段只为旧配置可读，UI 和新配置必须固定关闭，发现旧 `.txt/.cmd/.bat` 脚本时失败关闭，禁止自动猜测转换或回退执行。
- 本机账户枚举和更新、注册表读写/配置单元装卸、固件模式探测、文件版本读取、当前 Wi-Fi profile 获取、系统重启安排、文件属性修改、文件树复制、可执行文件路径搜索和既有计划任务触发必须复用仓库内已核对微软文档的参数化 Win32/COM 边界，不得回退解析 `net.exe`、`reg.exe` 查询输出、PowerShell、`netsh wlan show/export`、`shutdown.exe`、`attrib.exe`、`xcopy.exe`、`where.exe` 或 `schtasks.exe` 的本地化文本。`.reg` 文件导入保留 `reg.exe import` 作为 Windows 自有格式解析兼容边界，调用前仍须验证普通文件和路径。
- 新进程执行优先通过 `lr_core::command::CommandRequest` 和 `CommandExecutor`。
- 程序及参数必须逐项传递，不得用字符串拼接构造 shell 命令。
- 只有确有兼容需求时才能使用 `cmd /c`，并必须严格验证所有可变输入和命令元字符。
- 盘符、磁盘号、分区号、文件系统、卷标、路径、URL 和服务端字段必须在进入系统命令前验证。
- 写操作必须检查进程启动结果、退出码、stderr、工具可能返回的文本错误以及操作后的可观察结果。
- 写操作应 fail-closed；查询或探测失败可返回 `Unknown`、跳过或使用既有安全回退，但不得伪装成成功。
- 临时盘符只能通过共享 `GetLogicalDrives` WinAPI 边界选择；API 返回零表示查询失败，必须按“无可用盘符”失败关闭，禁止用 `Path::exists` 把空光驱、断开的网络映射或不可访问卷误判为空闲盘符。对没有 `IVdsVolume` 对象的隐藏 OEM/ESP 分区，必须使用 `IVdsAdvancedDisk` 按精确磁盘号和起始偏移分配、查询并对称删除盘符；挂载后必须再次用卷范围 IOCTL 核对同一物理身份，已有盘符只借用不删除，本次创建的盘符在所有成功、错误和取消路径都必须显式清理，清理失败必须记录并向用户失败关闭。
- BCD、BCDBoot、Bootsect 和 XP/2003 引导写入属于承重步骤：进程启动、非零退出、关键文件缺失、活动分区写入或结果文件回读任一步失败都必须停止，禁止只记录警告后报告引导修复完成。正常端创建 PE 启动项时每条 `bcdedit` 都必须检查退出状态并保留 stdout/stderr；缺失 `boot.sdi` 时不得猜测或伪造文件格式，只能使用已存在的可信系统/介质原件，否则失败关闭。
- 临时脚本必须使用碰撞安全的临时文件并保证清理，禁止固定临时文件名。
- 新危险路径必须先把命令构建和结果判断提取成可测试逻辑，再接入真实执行器。

### 磁盘目标与恢复要求

- 执行前保留并复核目标磁盘号、容量、分区信息和可获得的稳定标识，避免扫描后磁盘插拔导致目标变化。
- 不得仅根据 UI 中缓存的盘符执行不可逆操作。
- 多步骤流程必须保留原有回退和清理语义；新增步骤应考虑重复执行、取消、中断和进程崩溃。
- 不能确认安全状态时，应停止并向用户显示简洁错误，同时在日志中保留足够诊断信息。

## 兼容性与配置边界

- 正常系统端发布物只允许生成一份覆盖 Windows 7、8、8.1、10、11 的 x64 EXE：Release 使用 Rust `x86_64-win7-windows-msvc` 最低装载 ABI、`.cargo/config.toml` 中仅对该目标生效的静态 CRT 和受控 `build-std` 构建，避免未安装 UCRT 平台更新的 Windows 7 出现 `api-ms-win-crt-*` 装载依赖；不得按系统拆分版本或把现代能力全局降级。共享兼容层必须在 Windows 10/11 上优先动态调用现代 WinAPI，仅当导出确实不存在时使用有文档依据的 Windows 7 回退。PE 端只运行在 Windows 10/11 系 WinPE，必须继续使用普通 `x86_64-pc-windows-msvc` 目标，禁止套用 Win7 目标、静态 CRT 配置或正常端的装载期 ABI 桥接。
- 正常端主程序及随包自有运行库的静态和延迟导入必须全部存在于未安装平台更新的 Windows 7 SP1；发布检查发现 `combase.dll`、Windows 8+ API-set 或其它 Win7 不存在的装载期模块时必须阻断打包。`CoTaskMemFree` 等在 Windows 7 已由传统系统 DLL 导出的兼容入口必须绕过可能被新版 SDK 重定向的 import library，运行时从其最低系统导出（当前为 `ole32.dll!CoTaskMemFree`）解析，不得接受新版 SDK 静默把同一符号改绑到较新的模块。运行时动态解析的现代 API 继续保留 Windows 10/11 优先路径和 Windows 7 有文档回退，禁止为消除静态依赖而全局关闭现代能力。
- 当前服务端入口是代码内固定 HTTPS 地址，普通用户不可在 UI 中修改；不得引入私密凭据或隐藏后门配置。
- 正常端读取固定 `v3/index.json` 时优先使用系统 TLS 与代理；仅当该固定 HTTPS 目录请求失败时，允许使用内置 WebPKI 根和禁用代理的 Rustls 客户端重试。重试不得放宽证书、主机、响应状态、JSON 架构或目录内容校验，也不得影响用户输入 URL 和其它下载入口；双路径错误必须完整写入日志。
- LetRecovery 发布回调只允许原子更新服务端 `v3/index.json` 的 `data.pe`、生成时间和 PE 计数；不得再写入 LetRecovery v1/v2 目录文件。正常端在线目录读取也必须以 `v3/index.json` 为唯一入口，v3 请求、解析或结构校验失败时直接失败并保留上下文，不得回退 v1/v2 或把部分目录伪装为加载成功。回调必须把 HTTPS URL、文件名、MD5、SHA-256 和字节数全部纳入签名绑定。`data.system_image_mode`（兼容根级字段及 `mode` 别名）只允许 1、2、3：1 表示每次程序启动或人工刷新时从微软 Update Metadata Service 获取 MCT `products.cab` 并使用官方长期 ESD，2 表示只使用 `data.system_images`，3 表示优先合并两者；字段缺失时固定默认为 2。模式 3 的微软目录临时失败可在 API 目录非空时安全降级到 API，模式 1 和两边都为空时必须失败关闭。
- 微软官方系统镜像目录不得通过网页 ISO connector 生成 24 小时签名链接。Windows 11 必须使用 HTTPS 请求 `fe3.delivery.mp.microsoft.com/UpdateMetadataService`，严格校验唯一更新身份、唯一 `products.cab`、声明大小、官方 delivery 主机和服务端 SHA-256；CAB 只能通过共享 `SetupIterateCabinetW` 边界解出唯一 `products.xml`。Windows 10 可附加使用微软固定 HTTPS fwlink；其当前 22H2 旧目录只声明 SHA-1 时必须验证该字段格式，但不得把 SHA-1 冒充 SHA-256、不得显示为已完成 SHA-256 校验，下载后仍须经过完整本地镜像校验。XML 只接受 `zh-cn`、x64、`CLIENTCONSUMER_RET`、无查询参数的 `dl.delivery.mp.microsoft.com` 长期 ESD，且文件名、大小、Build 和版本标识必须完整一致；Windows 11 还必须具有有效 SHA-256。重复卷条目只可在全部元数据相同时折叠，任何冲突、越界或非官方重定向都必须失败关闭。此目录只属于正常系统端的在线下载入口，PE 端和 `lr-core` 安装策略不复制该网络发现逻辑；下载后的既有本地镜像复核和交接安全边界保持不变。
- 简易模式目录允许只提供 WIM/ESD/ISO URL 而省略卷列表；系统安装页也允许用户手工输入这些格式的直链。直接 WIM/ESD 必须用两个有界的精确 HTTP Range 读取标准 WIM 头和 XML；ISO 必须先按 ECMA-119 用有界 Range 读取 ISO 9660/Joliet 卷描述符和 `sources/install.esd|wim` 的单段或多段 extent，再把内嵌镜像视为连续逻辑字节流读取头和 XML，禁止为探测元数据下载完整 ISO。每次请求都必须得到范围完全匹配的 `206 Partial Content`；服务器忽略 Range、返回完整 `200 OK`、内容编码、范围/extent 越界、实体大小、最终重定向地址或 ETag/Last-Modified 在请求间变化时必须失败关闭。最多跟随五次经过 URL/传输策略验证的重定向，首个 Range 请求解析出的最终 URL 只供当前元数据会话的后续 Range 使用，不得改写用户输入框或持久化配置中的原始链接。只显示 XML 中明确为 Client/Server、具有版本元数据的可安装卷，并按实际版本 6.1/6.2/6.3/10.0 与产品名区分 Windows 7/8/8.1/10/11；WindowsPE、Windows Setup、Setup Media、WinRE 和无法确认的镜像不得显示。成功读取远程卷后不得在状态栏额外提示 HTTP、重定向或续传能力，只保留错误与既有安装校验状态。简易模式启用时隐藏在线下载和工具箱入口。在线目录的系统“安装”动作必须先把原 URL 交给安装页读取卷，用户确认安装后才下载；下载必须启用续传，完成后必须重新读取完整本地镜像或挂载已下载 ISO，并比对所选卷的索引、版本、Build、架构和安装类型，一致后才能继续既有安装流程。
- PE 元数据必须继续兼容现有 MD5 字段；可选 SHA-256 存在时优先使用 SHA-256。
- 联网下载和受管 PE 缓存声明校验值后，计算失败或不匹配必须失败关闭。“未声明校验值”和“计算出错”必须是不同状态。服务端提供 MD5 或 SHA-256 时必须随目录项一起传递到 UI、下载器和缓存校验边界；服务端未声明哈希时 UI 不得显示“已校验/已验证”。随发布包提供的 `bin/pe` 是明确的用户管理边界，允许用户替换或修改 WIM，因此不强制匹配远端哈希，但仍必须限制为安全文件名和普通文件；联网文件不得下载到该目录来绕过校验。
- 正常端只允许为已经通过完整 libwim 校验的单文件 WIM/ESD 保存有界的本机 BLAKE3 验证缓存；再次校验必须在拒绝写入和删除共享的文件句柄仍被持有时重新计算完整 256 位指纹，只有指纹完全相同才能复用先前结果。不得仅依赖路径、大小或时间戳；缓存缺失、损坏、超限、指纹不匹配、读取或持久化失败都必须回退完整 libwim 校验，SWM 分卷不得使用该快路径。
- 自动目录等非手工下载默认只允许 HTTPS。用户在系统安装页手工输入 `http://` 镜像直链视为仅对该链接的明确兼容授权；不得据此放宽其他下载来源，也不得把原始链接静默改写成重定向后的地址。
- Windows 兼容性入口点故障发生在主程序启动和日志初始化之前，不得声称能由主程序自身记录；发布流水线必须通过 `.github/scripts/verify-win7-imports.ps1` 检查正常端产物，阻断 Windows 8+ API、API-set/UCRT 或 `combase.dll` 装载依赖。发布包根目录只允许携带正常端主程序这一份 EXE，文档和安装器不得要求已停用的临时开发诊断工具。
- 正常系统端和 PE 端已有配置格式需要向后兼容；新字段应有安全默认值，并为旧配置增加解析测试。
- ViaPE 必须把安装意图中的 `FormatPartition` 和 `RepairBoot` 原样写入交接配置，PE 原生进度窗口与 CLI 兼容路径都必须严格遵守；旧配置缺少字段时维持历史上的“格式化并添加引导”，显式关闭 `RepairBoot` 时还必须跳过 PCA/EFI 预检、BootEx/UefiSeven 注入和所有引导写入。
- 内置 Administrator 高级选项只允许在 Windows 7 及以上 WIM/ESD/SWM 的内置无人值守路径启用；不得与自定义无人值守文件、普通“自定义用户名”、GHO/GHS 或 XP/2003 路径并用。账户改名必须按 RID-500 身份定位，不能依赖本地化账户显示名；密码只允许存在于当前安装会话配置和 Windows Setup 无人值守文件中，持久化用户偏好、调试输出和日志必须脱敏，安装清理仍需删除会话文件。
- 历史配置字段 `disable_windows_defender` 的用户语义固定为“仅深度移除 Microsoft Defender Antivirus 杀毒引擎”。实现只能处理 `WinDefend`、`WdBoot`、`WdFilter`、`WdNisDrv`、`WdNisSvc` 及同目录下随引擎演进的 `WdAiNisDrv`、`WdDevFlt`、`KslD`，以及 Defender 引擎目录、驱动目录和 Defender 自身计划任务；必须保留 `SecurityHealthService`、`wscsvc`、`mpssvc`、UAC、VBS、SmartScreen、System Guard、Web Threat Defense、Pluton 和 Microsoft Defender for Endpoint (`Sense`)。两端必须复用 `lr-core::defender_removal`，仅允许对完整离线 Windows 目标执行；目标盘、控制集、任务 GUID、重解析点、ACL 修改和删除后状态都要严格验证，所选操作任何一步失败时安装流程必须失败关闭，不能再把旧策略键写入或删除失败伪装成成功。
- 随包存储控制器驱动只允许来自已记录 SHA-256 和 Microsoft Windows Hardware Compatibility Publisher 签名的 Microsoft Update Catalog 包；全部发布文件、大小、SHA-256、控制器 ID 和签名主体由 `docs/STORAGE_CONTROLLER_DRIVERS.lock.json` 固定，Release 必须先验证底包、同步到 PE WIM，再只读挂载最终 WIM 复核，禁止依赖底包碰巧带有正确版本。正常端、PE 端和存储驱动工具必须先通过 SetupAPI 获取当前机器完整 `REG_MULTI_SZ` PCI 硬件 ID，再由 `lr-core::storage_driver_match` 选择唯一匹配目录，禁止对 `bin/drivers/storage_controller` 做整目录递归注入。PE 启动后必须在 BitLocker、标记和分区扫描之前校验锁定包并用微软支持的 `drvload.exe <匹配 INF>` 把同一包只加载到当前 WinPE 会话，使 VMD 后的磁盘可见；枚举、包校验或 `drvload` 任一步失败必须在磁盘扫描前停止。当前 `9A0B` 使用 20.2.4.1019 兼容包，`467F/A77F/7D0B/AD0B` 使用 20.2.12.1036；`09AB` 只是 managed/dummy function，单独出现时不得猜测代际。正常端导出当前系统驱动时必须保存已绑定 OEM 启动存储驱动清单，PE 导入后必须回读离线 DriverStore 逐项确认覆盖；缺目录、缺清单、DISM 失败或回读不匹配全部失败关闭。AMD、Apple、VirtIO、仅凭机型名称或无法唯一确认时必须跳过或拒绝。旧通用存储控制器目录不得恢复；恢复的 Windows 7 USB3/NVMe 兼容资源必须由 `docs/WINDOWS7_DRIVERS.lock.json` 固定全部文件、大小和 SHA-256，发布前验证 USB3 WHQL CAT 与微软 NVMe CAB 签名，同步到正常端与 PE WIM，并只读挂载最终 WIM 再次复核。USB3 只能注入与 SetupAPI 当前硬件 ID 和离线目标架构匹配的锁定子目录；目标为 Windows 7 时 UI 默认启用 USB3。NVMe 只允许在同一个 DISM servicing 会话中按固定依赖顺序提交微软 x64 KB2990941/KB3087873 CAB；DISM 成功后还必须通过文件版本 WinAPI 回读离线目标中的 `stornvme.sys` 和 `storport.sys`，确认至少达到对应热修补 GDR 版本，禁止把进程退出码 0 单独当作导入完成。只有 `IOCTL_STORAGE_QUERY_PROPERTY` 明确返回目标物理盘 `BusTypeNvme` 且镜像为 x64 Windows 7 时默认启用；VMD/RAID、未知总线、查询失败和 x86 镜像不得猜测。旧包中不能通过内核签名策略的魔改驱动与散装 NVMe INF/SYS 不得恢复。XP/2003 专用驱动边界保持独立。
- 历史 `win7_fix_acpi_bsod` 仅作为 Windows 7 手工选择的旧式处理器电源驱动兼容尝试保留：它只禁用离线系统中的 `intelppm`、`amdppm` 和 `Processor` 服务，不修改 ACPI 表、`acpi.sys` 或固件，也不得宣传为通用 0xA5 修复。默认必须关闭，目标版本无法确认为 Windows 7 时必须忽略；正常端 Direct、ViaPE 配置和 PE 端必须保持同一语义。任何 ACPI 二进制替换或补丁属于独立高风险功能，未有可审计来源、签名和硬件矩阵前不得接入。
- Windows 7 USB3/NVMe 兼容处理属于自动安装策略，不得再在高级选项中暴露勾选框、自定义目录或浏览按钮。正常端必须在最终安装意图中重新按镜像版本、架构和同一目标磁盘快照的总线类型覆盖旧配置：已确认 Windows 7 自动评估锁定 USB3 包，只有 x64 且目标明确为原生 NVMe 时启用固定 CAB 组合；传给 PE 的自定义路径必须为空。历史 `Win7FixStorageBsod` 只为旧配置可解析且必须归零；`Win7UefiPatch` 继续作为正常端到 PE 的兼容交接字段，但只能由最终安装意图在已确认 Windows 7 x64、可能使用 UEFI 且启用引导修复时自动置位，UI 不得提供手工开关；PE 端不得把该兼容字段当作事实来源，必须在格式化前根据实际所选 WIM/ESD 卷的 6.1/x64 元数据重新判定。UefiSeven 固定资源由 `docs/UEFISEVEN.lock.json` 锁定，优先使用交接目录、否则使用随 PE 发布包提供的 `bin/uefiseven`，并在格式化前和写 ESP 前复验；缺失、篡改或原始 Microsoft 引导文件缺失都必须停止。重装时必须保留本次 BCDBoot 生成的 BCD；确认 VMware 客体时必须绕过 UefiSeven，并把 LetRecovery 既有锁定加载器从微软注册入口和标准固件 fallback 两处事务化恢复为各自保存的原生 Microsoft x64 EFI，不能只修一处或删除恢复副本；其它虚拟机、实体机和 Unknown 环境继续事务化部署 UefiSeven 双入口，任一入口验证、备份、替换或回读失败都必须整体回滚。Windows 7 离线 SYSTEM hive 中所有既存控制集的 `CrashControl\AutoReboot` 必须设为 0 并回读，避免把真实早期 bugcheck 隐藏成循环重启；该设置只改善诊断，不得伪装成 ACPI 或存储兼容修复。`Win7FixAcpiBsod` 仅按上一条的受限手工兼容语义保留。禁止把互不相关的 IDE/AHCI/RAID/NVMe 服务全部设为 Boot Start 来冒充通用蓝屏修复。
- PCA2011/PCA2023、BIOS/UEFI、MBR/GPT、BitLocker 和 XP/2003 路径都是兼容性边界，不得根据单一新系统环境简化掉旧路径。
- 正常端 Direct 与 PE 端必须复用同一套安装引导模式语义：显式 UEFI/Legacy 始终优先，Auto 仅可将已确认的 GPT 映射为 UEFI、已确认的 MBR 映射为 Legacy；分区表为 Unknown 时只能调用已核对的固件 WinAPI 探测，探测失败必须在写入引导前停止，严禁把 Unknown 默认为 Legacy。NT5/XP 路径只能来自已验证的安装意图或镜像元数据，不能通过目标系统缺少 `Windows\Boot` 等目录特征猜测；已确认为现代 Windows 却缺少必要引导资源时必须失败关闭。
- UEFI 安装在固件已撤销 PCA2011 或用户明确选择签名代际时，必须在分区、格式化等目标盘写操作前验证所选镜像卷内存在有效的对应 EFI 引导文件。无法安全预检的 GHO 等不透明格式必须失败关闭；不得把某台机器 ESP、Insider 构建或未经支持矩阵验证的 `bootmgfw.efi` 当作通用升级文件。
- PCA2023 自动升级使用发布包内固定的 x86/x64 离线资源族，不允许安装时联网下载或回退到其他架构。资源 WIM 只可注入 `Windows\Boot\EFI_EX`、`Windows\Boot\FONTS_EX` 和 `Windows\Boot\EFI\boot.stl`；必须验证包大小、普通文件属性、BootEx 微软签名、PE 架构和正常端到 PE 暂存副本的 SHA-256。缺包或验证失败应在写盘前停止。制作、支持矩阵和回滚流程见 `docs/PCA2023_COMPAT_PACKAGES.md`。
- PCA 选择只适用于 Windows 10/11 和 Server 2016+ 的 UEFI 安装；XP/2003、Vista、Windows 7、Windows 8/8.1、GHO/GHS 和 Legacy 安装不显示该选项。Windows 7 x64 UEFI 不得进入 PCA2011/PCA2023：经 CPUID、SMBIOS 和 SetupAPI 任一路正向确认的 VMware 客体使用 BCDBoot 生成的原生 Microsoft x64 EFI 双入口，并事务化清理 LetRecovery 既有 UefiSeven；其它环境继续把锁定 UefiSeven 事务化部署到 `EFI/Microsoft/Boot/bootmgfw.efi` 和固件 fallback `EFI/Boot/bootx64.efi` 两个入口，分别保留并验证 `bootmgfw.original.efi` 和 `bootx64.original.efi`。Unknown 不得猜测成 VMware；任一入口缺失或任一步失败都必须回滚并停止，禁止假定所有固件都会遵循 NVRAM 中的 Windows Boot Manager 项。若完成安装时检测到 Secure Boot 仍开启，PE GUI、PE CLI 以及正常端在 PE 内直接安装都必须明确提示用户进入 BIOS/UEFI 关闭 Secure Boot，并禁止自动重启。Vista、Windows 7 x86、Windows 8/8.1 遇到 Secure Boot 已启用且 PCA2011 已撤销时仍必须在写盘前停止，不能伪装成可升级 PCA2023；XP/2003 保持既有 NT5 专用路径。当前产品工具链只支持 x86/x64，ARM64 镜像必须在写盘前拒绝。
- PCA 写盘前预检不能把 Secure Boot 探测失败等同于“未启用”；只要安装可能使用 UEFI，状态为 Unknown 就必须在任何目标盘写入前停止并给出可诊断错误。

## 代码组织规则

- UI 负责展示和收集用户意图；可复用业务规则、解析和命令构建放入核心模块。
- Windows API、FFI 和动态 DLL 调用应封装在边界模块中，调用方不要复制 `unsafe` 细节。
- 超过约 1,000 行且职责混杂的文件应渐进拆分，但每次只移动清晰边界，保持公开接口和用户行为。
- 不为“看起来更抽象”增加层级。新抽象必须减少真实重复、隔离危险边界或提高可测试性。
- 错误应保留底层上下文；用户文案简洁，日志详细。不要静默吞掉写操作失败。
- 日志不得写入密码、恢复密钥、访问令牌、完整鉴权 URL 或其他敏感值。
- 公共模块和复杂安全判断需要短而准确的注释，禁止无意义逐行复述。

## 国际化与用户界面

- 正常端 Windows 7–11 的主窗口和全部原生工具对话框统一请求系统自带 `Microsoft YaHei`，不得使用 Windows 7 不存在的 `Microsoft YaHei UI` 字体族；工具箱必须按当前宿主系统版本隐藏不可用功能，Windows 7/8/8.1 不得显示 AppX 等 Windows 10/11 专用入口，隐藏后必须紧凑重排且不能留下空槽。
- 正常端日志默认开启，发布模板和规范化脚本必须共同保证 `log_enabled=true`；文件日志必须写 CRLF，使 Windows 7 记事本也能逐条换行。进程启动时必须设置 `SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX`，卷容量探测还必须在调用 `GetDiskFreeSpaceExW` 前用 `GetDriveTypeW` 显式跳过空光驱，禁止因已弹出或未插入介质阻塞 UI。

- **严禁装眼瞎，必须严格按照用户的要求来，严禁欺骗用户，严禁戏耍用户，必须保质保量。** 对用户指出的可见问题必须先复现、测量并如实说明验证结果；没有通过截图逐像素检查和真实交互复核时，不得用“已修复”“已完成”或模糊措辞敷衍交付。
- **严禁装眼瞎**：用户提供的截图、程序自行生成的 QA 截图或实机界面中只要仍存在肉眼可见的断线、矩形残影、色差、黑块、白边、颗粒、错位或样式回退，就不得声称“已修复”“验收通过”，更不得继续构建或发布 Release。必须先明确指出仍可见的问题，按原始像素或放大裁图复核，并在交互状态和明暗主题下重新验证；不能用“整体看起来正常”掩盖局部缺陷。
- 正常端字段和命令按钮统一使用 23px 的 96-DPI 高度基线；ComboBox 的 Win10 固定底色覆盖必须读取整控件悬停态，使闭合选区与原生箭头同时变色，弹出列表仍保持系统原生行为。
- 正常端安装页的“引导模式”和“启动签名”必须按当前语言与字体测量两段标签，复用同一标签列宽和标准字段间距，使两个 ComboBox 的左边界严格对齐；不得为字数相同的中文标签配置不同固定宽度。
- 正常端闭合 ComboBox 在 WinPE 中必须由确定性自绘路径完整处理背景擦除和非客户区绘制，不能让精简版 USER32/UxTheme 的库存画刷在控件底部留下亮色横线；Windows 10/11 即使移除 `WS_BORDER`/`WS_EX_CLIENTEDGE` 仍可能保留主题客户区内缩，因此同步闭合重绘必须通过 window DC 先覆盖完整可见字段，再最后绘制唯一外框，禁止只用 `GetDC` 从内缩后的客户区开始绘制而在顶部遗留两像素库存灰白弧线；真实鼠标按下会进入 USER32 的嵌套下拉跟踪循环，默认过程可能在 `WM_LBUTTONDOWN` 返回前重新覆盖闭合字段，因此必须在该嵌套循环内部投递并执行一次确定性同步重画，不能只验证直接发送 `CB_SHOWDROPDOWN` 的程序化路径或把返回后的重画误认为展开期间已生效；`CBS_DROPDOWNLIST` 的内部闭合选区子窗必须在点击、焦点和 UI 状态变化后同步覆盖完整客户区及非客户区底边，且不得与父 ComboBox 重复绘制文字或叠加焦点线；保留展开列表高度的 ComboBox HWND 必须把自身窗口区域裁剪到共享 23px DPI 字段基线，不能采用包含库存非客户区余量的 `COMBOBOXINFO` 高度；首次主题事务、字体/DPI/主题变化及首次展开前必须同时通过 `CB_SETITEMHEIGHT` 固定闭合字段和弹出列表行高，再同步裁剪和重绘，禁止首次深色弹层比切换主题后的弹层更矮或在字段下方留下窄带。弹出的独立 ComboLBox 仍保持系统原生绘制、键盘和无障碍语义；热态只能在进入/离开真正改变时合并失效，禁止因 `WM_MOUSEMOVE` 或 UxTheme `WM_TIMER` 同步整控件重画。不得给保留展开高度的 ComboBox 强加 `WS_CLIPSIBLINGS`，否则它的完整布局矩形会裁掉后续同级控件。所有原生 ListView 必须统一启用 `LVS_EX_DOUBLEBUFFER`；圆角外框必须由位于真实 ListView 上方的同父 STATIC 独占，并用外矩形减去内圆角矩形的空心 HRGN 只保留外框像素，真实 ListView 保持原父级、控件 ID、通知、选择、键盘、滚动和无障碍语义并只缩入 DPI 缩放的外框宽度。覆盖层必须返回 `HTTRANSPARENT`，持续保持在对应 ListView 之上并跟随其 `WS_VISIBLE`、启用、位置、尺寸和父级 DPI 状态，隐藏页面初始化时严禁无条件创建可见 STATIC；带表头的 ListView 覆盖层必须在外框发布后用表头色修补上沿内侧接缝，禁止把表体底色刷到表头顶部形成黑线；滚动消息不得同步重画或失效表体、表头和外框，避免 comctl32 像素滚动把固定外框复制进项目行。表头绘制事务不得反向触发整个 ListView 重绘，避免形成反馈循环和闪烁。单行 Edit 的首帧原生客户区绘制完成后必须补画确定性外框，不能依赖鼠标悬浮才修复边框。
- 带表头的 ListView 覆盖层上沿及两个内圆角必须使用真实表头底色参与确定性抗锯齿，底部两个内圆角继续使用表体底色；覆盖层在真实表头高度内的左右空心边带必须先用表头色清除内侧表体窄带，再按 DPI 恢复最外侧连续描边，禁止在表头两端暴露表体黑线、加粗直边、缺失描边或通过滚动复制该接缝。带 `LVS_EX_CHECKBOXES` 的 ListView 必须保留原生 one-based 状态图像、键盘切换、通知和无障碍语义；共享主题层只替换最终可见图元，并复用普通 `BS_AUTOCHECKBOX` 相同的 13px Windows 11 明暗/DPI/禁用状态资源。状态图元槽必须按真实选中或未选中行背景清除，不得另画方框、勾号或叠出双重复选框。
- 正常端主窗口和全部工具对话框在首次显示前必须完成字体、主题、布局和后创建子控件准备；主窗口从创建到销毁都必须是普通不透明输入窗口，禁止添加 `WS_EX_LAYERED`、`WS_EX_TRANSPARENT`、颜色键或透明首帧屏障，因为 Windows 7/10/11 都可能把真实点击交给后方窗口。显示前必须在隐藏状态完成控件树准备，显示后同步重绘非客户区、客户区及全部子控件，并在进入消息循环前再次确认扩展样式不含任何点击穿透标志；发现异常必须销毁窗口并失败关闭。禁止让 WinPE 先呈现白色 Edit、ComboBox、ListView 或按钮中间帧后再靠定时器、鼠标悬停修复。语言切换等整树文本事务必须在 `WM_SETREDRAW(FALSE/TRUE)` 之间完成，并通过一次 `RedrawWindow(RDW_FRAME|RDW_ALLCHILDREN|RDW_UPDATENOW)` 发布完整帧，不能暴露逐控件更新的混合语言中间帧。

- 正常端单行 Edit 必须保持普通不透明、非分层子 HWND；禁止 `WS_EX_LAYERED`、颜色键和依赖系统保存的离屏图像。USER32 继续独占文本缓冲、光标、选择、IME、键盘命中与无障碍语义。单行 Edit 没有原生垂直对齐接口，必须保留原控件 ID 和直接父子关系，以当前字体 `tmHeight` 把真实 Edit 垂直居中到 23px DPI 字段行，并由同一父窗口下无 ID、无通知的同级 STATIC 只承载完整高度的底色与圆角外框；严禁重新引入父级包装 HWND、消息代理、`EM_SETRECT`、`WM_NCCALCSIZE` 或自绘正文。水平 `EM_SETMARGINS` 必须在字体和 DPI 确定后设置；首次显示、语言和主题事务不得依赖鼠标或计时器才显现。普通短 `SS_LEFT` 标签保持库存顶对齐，仅 `SS_CENTERIMAGE` 标签垂直居中。主窗口不得使用 `WS_EX_COMPOSITED`。
- 正常系统端深色主题的引导按钮、选中导航和 ListView 真实选中行必须共享用户实测的 Windows 11 强调色 `#4CC2FF`；悬停和按下态可作克制明度变化，但静止填充不得漂回系统蓝或旧深青色。
- 正常系统端在线资源目录状态栏必须由 `CatalogueState` 统一映射；刷新请求进入 `Loading` 时显示进行中，成功替换目录行并进入 `Ready` 后必须立即显示完成文案，失败时显示控制器保存的具体错误，禁止目录已可用而仍遗留“正在刷新”状态。
- 正常端“小白模式”开关必须立即隐藏并重排左侧“在线下载”和“工具箱”导航 HWND，关闭时立即恢复；不得只阻止点击、只改变安装页或等到重启后才同步可见性。已经移除的“修改引导命令”、DiskPart 脚本复选框和脚本目录入口不得创建隐藏控件、保留命令分发或占用布局位置。主窗口每次启动必须按实际显示器工作区重新居中，不得复用或推断上一次拖动位置。
- 正常端安装页的“开始安装”可用状态必须来自同一份完整安装意图校验。PCA/EFI 只读探测临时挂载或卸载 ESP 引发的设备变更不得与分区库存扫描并行：探测期间只记录刷新请求，探测终态后合并执行一次；刷新待处理、进行中或重新触发 PCA 探测时必须保持安装按钮禁用，所有状态稳定后再一次性发布最终可用状态，禁止先按旧目标快照短暂启用再回退。设备变更后的刷新失败必须持续失败关闭，直到人工或后续成功刷新接受新库存；不得恢复使用旧目标快照。若已启用的按钮因校验状态变化被禁用，状态栏和日志必须保留具体校验原因，不得仍显示“检测完成”掩盖阻断条件。
- 新增或修改用户可见中文字符串时，必须同步更新 `assets/release/lang/en-US.json`。
- 正常端可分发测试包必须显式启用 `dev-build` feature，并由共享构建信息模块的编译期 `DEV` 常量统一控制；测试包主窗口标题和关于页必须持续显示“测试版”“测试软件”“仅供测试使用”，正式构建不得读取外部配置临时开启或误带这些标识。
- 正常端和 PE 端关于页及 Windows 文件属性中的日期版本必须来自对应 `build.rs` 本次构建产生的 `BUILD_VERSION`；构建脚本必须监听端内 `src`、`Cargo.toml` 和 `SOURCE_DATE_EPOCH`，禁止源码已经重新编译却复用旧日期。设置 `SOURCE_DATE_EPOCH` 时保持可复现 UTC 日期，未设置时使用实际构建时刻的 UTC 日期。
- 两端相同文案应保持键和值语义一致；不允许只更新正常系统端或只更新 PE 端语言文件读取逻辑。
- `zh-TW` 是正常端和 PE 端共同内置的 `繁體中文 - 中國台灣` 选项：缺失外部词条时必须通过 `lr-core` 的 Windows NLS `LCMapStringEx(LCMAP_TRADITIONAL_CHINESE)` 和繁體中文常用术语表从简体源文案确定性生成，不得回退简体；可选 `zh-TW.json` 只覆盖同名词条，不能成为 PE 可用性的前置条件。
- `en-US`、`ja-JP`、`ko-KR`、`fr-FR` 和 `de-DE` 的完整发布词表必须同时内嵌到正常端和 PE 端 EXE，并由 `assets/release/lang` 的同名 JSON 作为唯一源文件；外部同名文件只覆盖已有键，缺失、无法读取或解析失败时必须使用内嵌词表。CI 必须验证五套词表与 `en-US` 的键集合一致，再同步到正常端 `pkg/bin/lang` 和 PE WIM 的程序语言目录；词条增删时必须测试键完整性、非空值和格式占位符一致性。
- UI 布局必须适应不同 DPI、分辨率和中英文文本长度，不使用会造成明显空白或裁切的固定宽度。
- 主窗口最小跟踪尺寸在高 DPI、低分辨率下必须直接钳制到显示器 `rcWork`，不得在已经排除任务栏的工作区上再次扣减 DPI 放大的标题栏余量；关于页等自然内容较高的页面不能与稳定状态栏和命令栏重叠。
- 长任务必须保持进度、取消和错误状态稳定，后台任务不得阻塞原生窗口消息线程；安装与备份总进度必须按预计耗时加权，镜像释放/捕获占绝大权重，校验、格式化、引导和清理等快速步骤不得按步骤数量平均分配后在长耗时阶段前跳到约 50%。
- 正常端复制安装源到 PE 数据分区时，复制、落盘、复读 SHA-256、镜像结构校验和原子发布必须位于同一条 0–100% 进度带内，并在复读与结构校验期间持续响应取消；不能在复制结束后先显示 100%，再无反馈地执行长时间同步或重复哈希。单文件 WIM/ESD 只有在源文件句柄拒绝写入和路径替换、完整内部校验与复制哈希共同覆盖同一份不可变字节流、目标落盘复读 SHA-256 与源字节流完全相同时，才允许继承源校验结果而省略目标端重复解压校验；SWM 等依赖外部分卷的格式不得套用该优化。正常端安装任务运行期间左下角状态槽保持为空，仅在取消、失败或终态需要明确提示时显示文案。
- 用户明确选择导出驱动时，Direct 流程必须在 DiskPart 和格式化之前完成在线或离线源系统驱动导出，ViaPE 暂存也必须验证目标目录至少包含一个 INF；导出 0 个驱动、导入 0 个驱动或 DISM/SetupAPI 失败都必须失败关闭，导入失败时必须保留备份供诊断或人工恢复。PE 加载圈不得依赖低优先级 `WM_TIMER` 产生动画帧；应由高精度 waitable timer（不支持时安全回退）向窗口投递合并后的单帧消息，并保证队列内最多一个待处理动画帧。
- 详细硬件检测工具必须保持只读并在后台线程收集数据：CPU 指令能力使用 Rust 的 CPUID intrinsic，主板/BIOS/内存模块使用 `GetSystemFirmwareTable('RSMB')` 返回的 SMBIOS 表，显卡使用 DXGI 并按 `AdapterLuid` 去重，磁盘容量与分区数使用标准磁盘 IOCTL，NVMe 标识和健康数据使用 `IOCTL_STORAGE_QUERY_PROPERTY` 的标准协议通道。不得为了显示厂商私有 S.M.A.R.T. 或 SPD 字段加载内核驱动、直接访问硬件端口、绕过系统访问控制，无法从标准接口取得的字段必须明确显示为无法读取，不能以零值或空白伪装成功。
- 不为工程质量任务顺便重写无关界面，不改变已有安装、备份和工具箱工作流。

## 构建、测试与提交要求

仓库要求 Rust 1.88 或更高版本，并提交应用型 workspace 的 `Cargo.lock`。没有真实依赖变化时不得重建 lockfile。

从仓库根目录运行：

```text
cargo fmt --all --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked --features "LetRecovery/non-elevated-tests,letrecovery-pe/non-elevated-tests" -- -D warnings -A clippy::uninlined_format_args
cargo test --workspace --no-run --locked --features "LetRecovery/non-elevated-tests,letrecovery-pe/non-elevated-tests"
cargo test -p lr-core --locked
cargo test -p letrecovery-pe --locked --features non-elevated-tests
cargo test -p LetRecovery --locked --features non-elevated-tests
```

`clippy::uninlined_format_args` 是 Rust 1.88 与较新受支持工具链之间表现不一致的纯样式 lint，因此单独放行；其他 Clippy 警告仍由 `-D warnings` 阻断。CI 与本地验证必须使用相同参数。

`non-elevated-tests` 只允许测试程序以非管理员清单启动，并跳过正常端启动时的管理员检测和 `runas` 自动提权，供 UI 迭代和纯逻辑测试使用；release 构建会拒绝该 feature。读取真实 Windows 安装或宿主磁盘状态的测试必须带原因标记 `#[ignore]`，只能在可丢弃 VM 中手动运行。

`dev-build` 用于生成带正常管理员清单和 release 优化的可分发测试包；它只改变明确的构建身份与用户可见测试标识，不得跳过权限、安全校验或危险操作边界。正式发布构建必须省略该 feature。

修改 `官网/` 时，从该目录运行：

```text
npm ci
npm run lint
npm run type-check
npm run build
```

提交前还必须：

- 如果仓库存在 `.git`，每次修改完成并通过相应检查后都必须创建 Git 提交；提交信息应准确描述本次修改。只能暂存本次任务范围内的源代码、测试和文档，严禁把 `pkg/`、QA 截图、日志、压缩包、构建输出或用户文件加入提交，也不得顺带提交、覆盖或回滚用户无关的未提交修改。若目标文件已混有无法安全拆分的用户修改，应先报告冲突而不是假装已提交。
- 运行 `git diff --check`；
- 检查 `git status --short`，不得误提交 `pkg/`、本地 7z、`.cloudpe-work/`、构建输出或用户文件；
- 工作区存在用户维护的 `pkg/` 时，每次影响运行产物的修改完成并通过相应检查后，都必须执行受影响端的 release 构建并把产物同步到 `pkg/`：正常端原子替换根目录 `LetRecovery.exe` 并运行 Win7 导入边界检查；语言和固定发布资源只在源文件实际变化时同步，PE 端变化按本文“发布包 WIM 维护”授权流程更新 `bin/pe/LetRecovery_PE.wim`；同步时还必须删除发布包根目录中正常端主程序以外的 EXE。替换前后必须复核绝对路径、文件大小和 SHA-256，构建或校验失败时保留旧包。`pkg/` 仍属于本地发布产物，不得因此加入 Git 提交。
- 记录未运行测试和真实环境阻塞，不得把“无法执行”写成“测试通过”；
- 第三方 DLL 变化时同步更新 `docs/THIRD_PARTY_BINARIES.md`、许可证和 SHA-256；
- 不覆盖、不回滚用户未提交的修改。

PCA2023 离线资源必须从已维护的微软官方介质或动态更新包制作：可用 `.github/scripts/build-pca2023-pack.ps1` 从已维护的 `boot.wim` 提取，也可用 `lr-core/examples/build_pca2023_resource_pack.rs` 验证并捕获已经安全展开的固定白名单资源目录。两条路径都必须按 `docs/PCA2023_COMPAT_PACKAGES.md` 和 `docs/PCA2023_RESOURCES.lock.json` 记录来源、源哈希、关键文件与最终发布 WIM，并完成虚拟机矩阵。正式 release 会检查三份资源并把它们同时放入桌面端和 PE WIM；不得为了让流水线通过而使用空 WIM、某台机器的 ESP 备份或未经验证的 Insider 文件。

## 常见扩展应该修改的位置

### 新增安装高级选项

通常需要同时检查：

- `正常系统端/src/core/advanced_options.rs` 与 `正常系统端/src/core/ui_state.rs`：输入、默认状态、序列化兼容和离线应用边界；
- `正常系统端/src/core/install_config.rs`：传递给 PE 的配置结构和向后兼容默认值；
- `正常系统端/src/core/native_install_controller.rs`、`native_install_executor.rs` 与 `native_install_backend.rs`：正常端安装意图、执行状态机和生产后端；
- `PE端/src/core/config.rs`：PE 配置解析；
- `PE端/src/ui/advanced_options.rs` 或 `PE端/src/app.rs`：离线应用；
- `assets/release/lang/en-US.json`：英文翻译；
- 本文档：职责或文件变化说明。

### 新增工具箱功能

通常在 `正常系统端/src/core/tool_types.rs` 或对应 `native_*` 控制器定义状态，在 `core/native_tools_controller.rs` 接入入口，在独立核心文件实现业务，在 `native_ui/tools/` 放原生对话框。危险操作必须下沉到可测试核心边界，不能直接在按钮回调中拼命令。

### 新增在线下载类型或服务端字段

检查 `download/config.rs`、`download/server_config.rs`、下载管理器、缓存完整性、HTTPS 策略、旧配置默认值和 UI 展示。服务端输入一律视为不可信。

### 新增镜像引擎或第三方二进制

优先扩展 `lr-core/src/wim_engine.rs` 的统一入口，保留现有回退；记录来源、版本、许可证、SHA-256、可复现补丁、构建脚本和打包路径，不在调用方直接加载未验证 DLL。LetRecovery 自定义 wimlib 的并行读取必须保持有界内存、每个工作线程独立 Windows 文件句柄、按原顺序交付数据，并只在调用线程执行进度与安装回调；校验读取窗口必须计入同一内存预算，低内存时缩小或关闭，不得通过嵌套 chunk 并行、镜像索引或 CPU 厂商硬编码绕开通用路径；自动线程数必须遵守 Windows 处理器组/进程 affinity 与 Linux affinity。损坏镜像的 SHA-1 失败和取消语义不得因并行化弱化。

### 新增多步骤操作、断点或自动重试

- 状态、步骤定义、检查点和重试策略优先放在 `lr-core/src/operation/`，端内只做环境适配和消息映射。
- 每个步骤必须显式声明是否幂等；格式化、分区移动、镜像释放/捕获、引导写入等写操作默认视为非幂等，不得因进程崩溃自动续跑。
- 检查点必须使用同目录临时文件和原子替换，写入失败不能覆盖已有有效检查点，也不能阻断既有操作流程，除非该检查点本身是执行安全的前置条件。
- “断点记录”不等于“断点续做”。当前 PE 观察器只记录步骤、失败和中断，重启后保留诊断材料并从既有入口重新开始；以后实现恢复执行时必须增加目标指纹复核和专门测试。
- 自动重试仅用于明确幂等且被分类为瞬时失败的操作，必须有次数上限和退避；非幂等操作、校验失败和永久错误必须立即停止。
- 支持包只能包含脱敏环境摘要、检查点摘要和受大小限制的文本日志尾部，不得收集配置文件、源路径、密码、令牌或 BitLocker 恢复密钥。

### 新增 Rust 文件

选择最小职责目录，接入对应 `mod.rs`，增加针对性测试，并在下面的职责目录中添加条目。若文件承担多个不相关职责，应先重新划分边界。

## Rust 文件职责目录

以下目录应覆盖仓库当前全部 Rust 文件。描述的是主要职责，不代表可以跳过阅读调用点和测试。

### `lr-core` 共享核心

- `lr-core/src/lib.rs`：共享库根模块，声明并导出两端共用能力。
- `lr-core/src/data_staging.rs`：ViaPE 镜像暂存盘的纯选择策略；根据镜像大小、安全余量、SSD/HDD、内外置属性、物理磁盘关系及可缩卷上限返回现有卷、目标卷缩分区或不可用计划，固定存储有安全空间时必须优先于外置存储，不探测磁盘也不执行写操作；`shrink_is_safe` 只能由调用端在核对文件系统、介质和稳定 BitLocker 状态后设置。
- `lr-core/src/unattend_account.rs`：内置 RID-500 Administrator 高级选项、敏感字符串脱敏与非持久化边界、账户名/密码验证，以及两端复用的 AdministratorPassword、一次性 AutoLogon 跳过 OOBE 账户创建和安全编码改名无人值守片段生成；普通无人值守本地用户名必须复用其 Windows 语法与系统保留身份校验，禁止把 SYSTEM、TrustedInstaller、DWM/UMFD 虚拟账户等服务令牌写成待创建用户，同时不得用宽泛前缀误拒绝普通用户名。
- `lr-core/src/bl_passthrough.rs`：序列化和解析 BitLocker 恢复密钥透传文件；负责去重、注释和空项兼容。
- `lr-core/src/boot.rs`：共享 XP/2003 Legacy 引导写入；在修改引导扇区前验证 Bootsect、NTLDR、NTDETECT 和 boot.ini，命令启动或非零退出必须失败关闭，并提供可注入命令执行器的无写盘测试边界。已经移除 UI 授权入口的 `repair_boot.txt` 不得在两端恢复执行。
- `lr-core/src/boot_pca.rs`：PCA2011/PCA2023 签名与 EFI 架构识别、固件信任评估、ESP 访问与临时挂载、模式决策、BCDBoot 调用、完整 BootEx 兼容回退，以及按目标 `ntdll.dll` 版本分流的 Vista/7/8/8.1 标准 UEFI 引导与 Windows 7 x64 锁定 UefiSeven 校验、微软注册入口和固件 fallback 双入口事务部署/回滚及 VMware 原生双入口事务恢复；共享只读探测先使用卷 GUID 快路径，隐藏 ESP 无法映射时才进入带作用域的 VDS 临时盘符回退。临时 ESP 守卫必须区分“本次创建”与“原本已有”的盘符，已有盘符只借用不移除，本次创建的盘符在清理前必须复核物理磁盘与分区偏移并显式报告清理失败。EFI 签名优先使用宿主 `WinVerifyTrust` 完整验证；Windows 7 仅在返回纯链信任错误时才允许核对 WinTrust provider 中无其它错误的签名与时间戳链，并要求精确包含微软发布 SHA-256 的 `Windows UEFI CA 2023` 证书，摘要错误、无签名、过期、吊销、用途错误和非 PCA2023 签发全部失败关闭。
- `lr-core/src/cached_artifact.rs`：缓存文件的安全查找、常规文件约束、元数据解析和完整性验证状态。
- `lr-core/src/command.rs`：类型化进程请求、执行结果、系统/dry-run 执行器，以及把进程启动失败转换为共享操作错误的入口。
- `lr-core/src/defender_removal.rs`：两端共享的离线 Microsoft Defender Antivirus 引擎白名单移除边界；验证目标系统、活动控制集和计划任务 GUID，使用 Windows 安全 API 获取仅限目标文件树的删除权限，拒绝重解析点，删除后逐项复核，并显式排除安全中心、防火墙、SmartScreen、UAC、VBS 和 Defender for Endpoint 等保留组件。
- `lr-core/src/diskpart.rs`：已停用任意 DiskPart/批处理脚本的配置兼容守卫；缺失或空目录安全跳过，发现 `.txt/.cmd/.bat` 时列出文件并失败关闭，严禁重新启动 `diskpart.exe`、`cmd.exe` 或把任意脚本文本伪装成可等价转换的 WinAPI 操作。
- `lr-core/src/download_integrity.rs`：MD5/SHA-256 选择策略、哈希验证、HTTPS/HTTP URL 策略和下载文件名验证。
- `lr-core/src/driver.rs`：SetupAPI 驱动枚举、严格导出、在线导入及 DISM 离线驱动服务的共享 Windows 实现；按官方两次调用契约读取完整 `REG_MULTI_SZ`，SetupAPI 以字节返回的 Unicode 长度必须转换到对齐的 `u16` 缓冲区并复核返回边界，禁止把 `Vec<u8>` 强转为 UTF-16；设备信息集由 RAII 保证所有错误路径都释放，设备枚举只有 `ERROR_NO_MORE_ITEMS` 可正常结束。已绑定驱动的 INF 名称必须读取 `DEVPKEY_Device_DriverInfPath`，禁止把 `SPDRP_UI_NUMBER (0x10)` 当作 INF 路径；Vista 及以上必须使用 `DiInstallDriverW` 完成真实安装，只有明确探测为 pre-Vista 的系统可采用 `SetupCopyOEMInfW` 暂存兼容路径，`DiInstallDriverW` 只能传 `0` 或 `DIIRFLAG_FORCE_INF`，真实安装失败不得用仅暂存 INF 的回退伪装成功。在线 OEM 导出失败不得伪装成部分成功，离线导入/导出统一调用 DISM，禁止手工拼装 DriverStore、猜服务注册表或按 FileRepository 目录名猜测第三方包；共享导出树计数必须拒绝根目录或任一子项的重解析点并传播遍历/元数据错误，零个 INF 是可区分的完整枚举结果而不是默认成功；另负责原子生成当前 OEM 启动存储驱动清单，并在 PE 导入后回读离线 DriverStore 逐硬件 ID 验证覆盖。
- `lr-core/src/driver_package_trust.rs`：破坏性写盘前和 DISM 签名误判兜底前的驱动包信任边界；对每个具体 INF 调用微软 SetupAPI `SetupVerifyInfFileW` 验证对应目录 CAT，再动态优先使用 Windows 8+ 的 `CryptCATAdminAcquireContext2`、`CryptCATAdminCalcHashFromFileHandle2`，仅在导出不存在时成对回退 Windows 7 的 `CryptCATAdminAcquireContext`、`CryptCATAdminCalcHashFromFileHandle`，两条路径都必须继续通过 `WinVerifyTrust(WTD_CHOICE_CATALOG)` 核对 INF、SYS、DLL、EXE、EFI 及固件载荷确为该 CAT 的成员；严禁混用新旧上下文/哈希函数、把旧接口失败伪装成兼容成功或削弱 Windows 10/11 的 SHA-256 首选路径。拒绝目录、重解析点、空签名者和目录外 CAT，并用整个包目录内全部普通文件的相对路径、大小和 SHA-256 把验证结果绑定到随后的单包操作。只有普通 DISM 已失败为签名类错误、此边界重新验证同一 INF 成功且使用前完整包快照未变时，才允许对该具体 INF 执行一次 `/ForceUnsigned`；严禁与 `/Recurse`、任意目录、未验证包或真正未签名包组合。PE 图形和 CLI 流程必须在格式化之前审计整个待导入目录，并区分目录结构/遍历错误与单包信任失败：前者失败关闭，后者只记录并交给 DISM 精确 INF 导入隔离，不得让打印机、虚拟化等可选包在写盘前拖死安装。批量导入失败后必须逐 INF 隔离，可选非存储包最终失败只记录日志，不能拖死整个安装，捕获清单中的启动存储驱动仍须回读离线 DriverStore。若且仅若缺失要求全部为已确认的 Intel VMD 控制器，可继续完成引导和清理，但必须禁止自动重启并明确提示先在固件中关闭 VMD/Intel RST；其他启动存储驱动缺失继续失败关闭。
- `lr-core/src/driver_trust.rs`：WinPE 运行时存储驱动加载和离线驱动导入前的受控签名信任初始化；只允许把源码内 DER 与固定 SHA-256 完全一致的 Microsoft Root Certificate Authority 2010、Microsoft Windows Third Party Component CA 2012 和续期 Microsoft Time-Stamp PCA 2010 分别通过 CryptoAPI 加入 WinPE 本机 ROOT、CA 存储，使用 `CERT_STORE_ADD_USE_EXISTING` 保持幂等并逐字节回读返回的证书上下文，补齐离线环境验证已正确时间戳的 WHCP 启动级驱动所需的代码签名和时间戳证书链；不得导入驱动目录携带的任意证书、不得写入离线目标系统证书库；任何受控 DISM 兜底必须经过 `driver_package_trust`，本模块自身不得降低签名结果。
- `lr-core/src/encoding.rs`：Windows GBK 与 UTF-8 转换。
- `lr-core/src/format_command.rs`：共享格式化请求的盘符、文件系统和卷标纯验证；只描述意图，不构造或执行 `format.com`，实际格式化统一进入 `windows_storage`。
- `lr-core/src/fveapi.rs`：动态加载 FVEAPI 的 BitLocker 卷访问、状态、解锁和恢复密钥格式处理。
- `lr-core/src/hash.rs`：流式 SHA-256、兼容 MD5、普通及可失败/可取消进度回调、规范化和比对。
- `lr-core/src/image_meta.rs`：不依赖 DLL 的标准 WIM 头/XML 资源描述符边界校验、UTF-16LE XML 解码、镜像元数据解析、名称整理和镜像类型判断；本地与远程读取必须共同复用同一偏移和大小规则。
- `lr-core/src/operation/mod.rs`：多步骤操作基础设施的公开导出和统一毫秒时间戳。
- `lr-core/src/operation/checkpoint.rs`：安装、备份、扩容等操作的严格状态机、步骤顺序、目标指纹、原子 JSON 检查点和事务式 journal。
- `lr-core/src/operation/error.rs`：可序列化的统一错误类别、错误码、用户/日志消息和显式可重试属性。
- `lr-core/src/operation/retry.rs`：区分幂等/非幂等的有界重试、退避策略、可注入 sleeper 和纯单元测试。
- `lr-core/src/operation/support.rs`：自包含 JSON 支持包、操作摘要、日志尾部限制、文件名隔离和凭据/恢复密钥脱敏。
- `lr-core/src/offline_international.rs`：两端共享的已释放离线 Windows 国际化默认值只读回退；使用进程与序列绑定的临时 hive 名加载 SYSTEM/DEFAULT，解析活动控制集、安装语言、系统/用户区域、首选键盘和时区，通过 Windows NLS 转换 LCID，任一关键值缺失、格式异常或卸载失败都保留诊断且不得伪装为宿主机默认值。
- `lr-core/src/pca_compat.rs`：按目标 WIM 架构和启动资源族选择内置 PCA2023 离线 WIM；运行时内嵌解析 `PCA2023_RESOURCES.lock.json`，对 WIM 大小/SHA-256、索引、白名单路径、内部 BootEx 精确 SHA-256、PE 架构和 PCA2023 签发者执行全链路复核，再完成安全暂存及 EFI_EX/FONTS_EX/boot.stl 白名单注入，不能依赖宿主信任库是否已更新。
- `lr-core/src/pca_preflight.rs`：PCA2011/PCA2023 写盘前只读策略，检查受支持系统版本和 x86/x64 架构，提取并验证所选 WIM/ESD/SWM 卷的 EFI 引导源，对不可预检镜像失败关闭；WIM 管理器必须在验签和删除共享临时目录前释放文件句柄。
- `lr-core/src/reboot.rs`：结束 PE 的 `pecmd.exe`；名字为历史兼容，模块本身不执行系统重启。
- `lr-core/src/registry.rs`：通过参数化 Win32 Registry API 管理在线/离线注册表配置单元、键和值，提供带类型校验的字符串/DWORD/二进制/递归只读查询、子键枚举、键存在性判断及删除后复核，并为 Windows 7 离线 SYSTEM hive 提供逐既存控制集关闭崩溃自动重启且回读验证的诊断边界；装卸 hive 必须启用并核对备份/还原权限，只有 `.reg` 文件导入保留受限的 `reg.exe import` 兼容边界。
- `lr-core/src/sam.rs`：复用共享 Win32 Registry 边界完成离线 SAM 账户枚举、清空密码和启用账户，包含严格边界检查的二进制结构解析。
- `lr-core/src/scoped_temp_file.rs`：碰撞安全临时普通文件和目录、名称验证、Drop 清理、显式所有权移交，以及同目录原子替换工具；WIM 提取目录清理可在不跟随重解析点的前提下清除只读/系统/隐藏属性后重试，并详细记录最终清理失败。固定临时脚本、备份暂存和驱动解包目录不得绕过本模块自行拼接易冲突路径。
- `lr-core/src/storage_driver_match.rs`：把 SetupAPI 当前 PCI 硬件 ID 纯函数映射到第 11 代或当前 Intel VMD 随包目录；`09AB` 单独出现时报歧义而不猜包，严格匹配 `VEN/DEV` 边界，AMD、Apple、VirtIO、相似前缀和未知控制器返回空选择；同时按固定大小和 SHA-256 验证两套发布包，并提供有界 INF 树硬件 ID 覆盖检查供导出、离线注入和工具箱回读复核。
- `lr-core/src/win7_driver_package.rs`：解析编译期内嵌的 Windows 7 USB3/NVMe 锁定清单，逐文件验证普通文件属性、成员集合、大小和 SHA-256；按离线目标 x86/x64 架构与 SetupAPI 当前硬件 ID 只选择匹配 USB3 子包，并只向 x64 目标返回固定顺序的两个微软 NVMe CAB，缺失、篡改、额外文件和架构不支持均失败关闭。
- `lr-core/src/traditional_chinese.rs`：正常端与 PE 端共享的 Windows NLS 简体转繁体转换和繁體中文常用界面术语归一；保留 ASCII 占位符，NLS 失败时返回明确的繁体错误文案，不得回退显示简体源文案。
- `lr-core/src/windows_accounts.rs`：通过 NetAPI `NetUserEnum`/`NetUserGetInfo`/`NetUserSetInfo` 枚举当前本机普通账户、清空指定账户密码并仅清除禁用标志；逐缓冲释放，保留“密码已清空但启用失败”的部分完成状态，不解析 `net.exe` 输出。
- `lr-core/src/windows_cabinet.rs`：两端共享的 SetupAPI CAB 枚举与解压边界；严格按通知类型返回 `FILEOP_*` 或 Win32 错误码，拒绝非普通 CAB、路径穿越、绝对/UNC 条目、重解析目标和分卷包，并在返回成功前核对请求数、解压数及每个输出文件。
- `lr-core/src/windows_compat.rs`：正常系统端的 Win7-11 单文件兼容边界；运行时优先解析 Windows 10/11 的每窗口/系统 DPI、DPI awareness 与扩展固件变量 API，缺失时分别回退 GDI/System-DPI 与 Windows 7 固件变量 API，并通过 `GetSystemDirectoryW` 获取可信宿主系统工具目录。仅在 `x86_64-win7-windows-msvc` 正常端目标内提供 windows-core 所需 WinRT/COM 装载桥接，并在新系统上转发真实实现；普通现代目标和 PE 端不得编译这些桥接符号。
- `lr-core/src/windows_file_copy.rs`：基于 `CopyFileExW` 的普通文件与递归目录复制边界；拒绝跟随重解析目录，逐文件保留错误上下文并供 XP/2003 文件准备复用。
- `lr-core/src/windows_file_version.rs`：使用 Version Information API 读取文件固定版本块，校验返回长度与 `VS_FIXEDFILEINFO` 签名后提供两端共享的版本四元组。
- `lr-core/src/windows_firmware.rs`：优先动态解析 Windows 8+ 的 `GetFirmwareType` 直接判断当前系统实际以 UEFI 还是 Legacy BIOS 启动，保持 Windows 7 可加载；API 不存在时才严格启用并核对 `SeSystemEnvironmentPrivilege`，使用微软文档规定的空变量名/全零 GUID `GetFirmwareEnvironmentVariableW` 探针。目录、ESP、盘符、环境变量和本地化命令输出不得参与固件模式推断，所有无法确认的错误必须向调用方失败关闭。
- `lr-core/src/windows_hardware.rs`：两端共享的只读机器身份探测；用 x86/x64 CPUID intrinsic 解码 CPU vendor/family/model、hypervisor bit 和 hypervisor vendor，用 `GetSystemFirmwareTable(RSMB)` 有界读取 SMBIOS Type 1，并复用 SetupAPI 当前硬件 ID 作第三路交叉识别。只有三路探测完整且均无虚拟化特征才可确认实体机；VMware 等虚拟机即使透传或伪装为新款 Intel CPUID 也必须优先分类为虚拟机，失败状态保持 Unknown。Windows 7 x64 UEFI 策略只能在确认 VMware 时绕过 UefiSeven，实体机、其它虚拟机和 Unknown 均保留兼容加载器。
- `lr-core/src/windows_shutdown.rs`：严格启用并核对 `SeShutdownPrivilege`，通过 `InitiateSystemShutdownExW` 安排带计划原因码的本机重启，禁止忽略权限未分配或调度失败。
- `lr-core/src/windows_storage.rs`：两端共享的参数化 Windows 存储管理边界；为兼容 Windows 7/WinPE 使用 VDS COM 完成分区创建/删除、带文件系统、卷标、分配单元大小及快速/完整选项的 NTFS/FAT32/exFAT 格式化、卷扩缩、盘符分配和 MBR 活动标记，使用受文档支持的磁盘/卷 IOCTL 完成 RAW 盘初始化、分区表/MBR 签名、卷物理范围查询，并通过两阶段 `IOCTL_STORAGE_QUERY_PROPERTY` 读取目标物理盘 `STORAGE_DEVICE_DESCRIPTOR.BusType`；COM 返回内存固定运行时解析 Windows 7 已有的 `ole32.dll!CoTaskMemFree` 后释放，禁止经新版 SDK import library 把正常端静态依赖提升为 `combase.dll`。无盘符普通卷的只读访问优先使用 `FindFirstVolumeW`/`FindNextVolumeW` 枚举卷 GUID 路径，再以 `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS` 唯一匹配磁盘号和起始偏移，枚举句柄必须由守卫在全部路径调用 `FindVolumeClose`；不得假设隐藏 OEM/ESP 在 Windows 7 上一定具有可枚举的卷 GUID，必要时必须通过 `IVdsAdvancedDisk` 按精确磁盘号和偏移分配、查询和删除盘符。普通盘符删除必须使用与 VDS 分配对称的 `IVdsVolumeMF::DeleteAccessPath`，隐藏分区盘符必须使用 `IVdsAdvancedDisk::DeleteDriveLetter`，并通过 `GetLogicalDrives` 回读确认消失；临时盘符还必须先用卷物理范围确认仍对应预期磁盘与偏移、记录 `QueryDosDeviceW` 返回的当前目标、请求 VDS 强制删除，并只在盘符仍残留时以 `DefineDosDeviceW(DDD_RAW_TARGET_PATH | DDD_REMOVE_DEFINITION | DDD_EXACT_MATCH_ON_REMOVE)` 精确移除同一 DOS 映射，禁止无目标或前缀匹配删除。只有明确的 `BusTypeNvme` 才能驱动 NVMe 默认值，查询失败或其他总线不得猜测。离线块移动后重建普通 GPT 基础数据分区时允许显式保留原 partition GUID、attributes 和 name，普通新建分区必须继续生成新 GUID。异步 VDS 操作必须同时校验 `Wait` 调用与操作 HRESULT、核对输出类型，调用方必须在写入前复核稳定磁盘指纹并在写入后重新枚举验证，当前 Windows 卷只能由 `GetWindowsDirectoryW` 和卷范围确定，不得回退写死 `C:`；所有临时盘符选择复用本模块的 `GetLogicalDrives` 掩码并在 API 失败时停止。
- `lr-core/src/wimgapi.rs`：动态封装 Windows WIMGAPI 的镜像 apply、capture、元数据和进度回调；消息编号严格使用 `WM_APP + 0x1476` 契约，安装取消仅在官方规定的 `WIM_MSG_PROCESS` 回调中返回 `WIM_MSG_ABORT_IMAGE`，避免跨 FFI 展开或猜测非标准中止方式。
- `lr-core/src/wimlib.rs`：动态封装 `libwim-15.dll` 的打开、校验、释放、捕获、拆分和进度回调；打开 WIM 后优先启用自定义 DLL 的有界并行解压与并行校验扩展，显式把当前可提交内存的四分之一（最高 2 GiB）作为并行工作预算，旧 DLL 缺少可选导出时保持串行兼容；镜像应用与校验接受调用方持有的原子取消标记，并由 libwim 进度回调返回中止状态以真正停止当前操作。
- `lr-core/src/wimlib_dll.rs`：把内嵌 `libwim-15.dll` 与运行目录副本逐字节比较，在首次加载前通过同目录临时文件回读验证和原子替换同步新版本，避免旧 DLL 遮蔽并行扩展。
- `lr-core/src/wim_engine.rs`：wimlib/WIMGAPI 运行时选择、统一调用和失败回退；可取消的应用流程在阶段边界检查取消，libwim 路径可中止进行中的操作，取消状态不得触发另一引擎回退；WIMGAPI 进行中的调用仅在有官方可验证的中止接口后才可扩展，禁止猜测回调常量。
- `lr-core/src/xp.rs`：XP/2003 x64 的 GPT+UEFI 驱动注入、服务注册和引导文件准备；用户启用的 XP 驱动目录缺失、`.sys` 缺失、文件复制失败、必需服务或 CriticalDeviceDatabase 写入失败必须失败关闭，不能只记录日志后继续安装。
- `lr-core/src/xp_i386.rs`：XP/2003 Legacy/MBR 文本模式硬盘安装、NT5 文件准备、应答文件和活动分区设置；文件树复制复用 `CopyFileExW` 共享边界，覆盖根引导文件前通过文件属性 API 只清除只读/系统/隐藏位；提供不写盘的 I386/AMD64 目录名及关键文件完整性预检，供正常端暂存前和 PE 格式化前共同失败关闭。
- `lr-core/src/xp_textmode_drv.rs`：解析存储驱动 INF，并把 AHCI/NVMe 驱动集成到 XP 文本安装阶段。
- `lr-core/examples/build_pca2023_resource_pack.rs`：验证固定白名单中的微软 BootEx 资源签名、架构和字体集合，并通过内置 wimlib 从普通目录生成和复验离线资源 WIM；不挂载或维护系统镜像。

### 正常系统端入口与核心

- `正常系统端/build.rs`：生成 Windows 资源、程序清单、包含 16 至 256px 多 DPI 帧的 PNG 派生 ICO 和可复现的构建日期/版本信息；日期版本随端内源码、清单和 `SOURCE_DATE_EPOCH` 重新求值，避免新二进制复用旧关于页版本；并把固定 Windows 11 `Aero.msstyles` 提取的复选框明暗及 96/120/144/192 DPI 状态 PNG 转换为编译期 BGRA 图元；单选框不再读取已预合成且丢失透明覆盖率的捕获 PNG；release 禁止测试权限 feature。
- `正常系统端/src/build_info.rs`：正常端编译期构建身份和用户可见构建标签的唯一入口；`DEV` 仅在显式启用 `dev-build` feature 时为 `true`，测试包的主窗口标题、关于页标题、产品名、版本和说明必须由此统一显示测试身份，正式构建不得出现测试标识。
- `正常系统端/src/main.rs`：桌面端进程入口、权限与依赖检查、窗口显示前并行完成系统/硬件摘要及正式版分区只读预加载、CLI 分派、PE 安装/备份入口和原生 Win32 窗口启动；启动日志必须记录构建/包版本、源系统、固件、Secure Boot、运行平台、物理磁盘与分区数，并在预加载后逐卷记录稳定磁盘/分区身份及 BitLocker 状态，使反馈者无需手工抄写环境；PCA 固件兼容性只读探测必须与启动预加载同时开始，通过一次性接收器在 HWND 创建后接入消息循环，既不得阻塞首窗显示也不得因窗口稍后创建而重复探测；所有危险 CLI（包括历史 `/PEINSTALL`/`/PEBACKUP`）必须在管理员边界之后分派，历史 PE 安装入口只接受安全文件名、唯一会话绑定的标记/配置并在格式化前完成镜像完整性校验，其 Windows 10/11 默认无人值守必须复用 `lr-core::offline_international` 读取目标而非 PE 宿主的语言、区域、键盘和时区，读取或写入失败必须阻断完成而不得丢弃错误；网络目录由窗口异步加载并回传错误，正常端不再声明或链接 egui 模块，`non-elevated-tests` 下使用隔离互斥锁并禁止危险 CLI 入口、联网和安装目标分区预加载；该测试 feature 专用的 `--ui-preview`/`LETRECOVERY_UI_SKIP_PRELOAD` 入口只跳过单实例和供应商 WMI/SetupAPI 只读预加载，用真实配置、原生控件和消息循环提供确定性视觉回归，release 中不得存在。
- `正常系统端/src/win7_import_compat.rs`：正常端 x64 Windows 7-11 通用产物的进程加载兼容边界；在 EXE 内提供 SDK 依赖所引用的 `__imp_CoTaskMemFree` 导入槽并动态转发到 Windows 7-11 均存在的 `ole32.dll!CoTaskMemFree`，防止现代 SDK 把单个调用重定向为 Windows 7 不存在的 `combase.dll` 硬依赖；不得扩展为伪造系统 DLL、吞掉释放或影响独立的 Windows 10/11 PE 构建。
- `正常系统端/src/native_ui/mod.rs`：正常端原生 Win32 UI 模块边界和窗口运行入口。
- `正常系统端/src/native_ui/redraw.rs`：主窗口和工具对话框共用的合成重绘事务；页面切换只暂停当前可见顶层根窗口，冻结期沿用既有子控件显隐和布局逻辑，恢复时用带 `RDW_ERASE|RDW_ALLCHILDREN` 的异步 `RedrawWindow` 排队一个完整客户帧，避免逐子 HWND 的 `WM_SETREDRAW` 产生空重定向表面，也避免点击处理同步等待整树绘制；主题、语言和首次显示等必须立即稳定的事务仍使用同步完整树发布。主窗口禁止使用 `WS_EX_COMPOSITED`。
- `正常系统端/src/native_ui/controls.rs`：原生子控件创建、UTF-16 字符串、库存下拉框空选择哨兵与无偏移索引校验、按 Inno DFM 的 23px 高/75px 最小宽基线缩放的 DPI 尺寸，以及按钮、分隔线和进度条的明暗/悬停/按下/禁用/单焦点边框状态绘制；ComboBox 保持 Common Controls 6.0 的库存字符串、键盘、无障碍和原生弹层语义，不得全局强加 `CBS_OWNERDRAWFIXED` 或在每个弹出项重绘时同步读取文本和分配缓冲；所有自绘按钮在共用子类中用 `TrackMouseEvent(TME_LEAVE)` 跟踪悬停，只局部重绘自身并复用 normal/hot/pressed/disabled 调色板，跟踪失败、取消模式、隐藏复用和禁用时立即清除热态，不因焦点增加双边框；按钮、长任务进度条及闭合字段/列表外框使用限域抗锯齿绘制，圆弧厚度必须随 DPI 与直边同步，重复重绘使用绝对调色板颜色以避免颗粒、断线和逐帧变暗；单行编辑框从创建期保留 `WS_CHILD|WS_VISIBLE|WS_TABSTOP|ES_AUTOHSCROLL` 和 `WS_EX_NOPARENTNOTIFY`，移除宿主版本会复活的 `WS_BORDER`/`WS_EX_CLIENTEDGE`，文本、光标、选择、IME 与无障碍仍完全由原生 Edit 负责；布局子类按当前字体 `tmHeight` 和 DPI 把真实 Edit 垂直居中到 23px 字段行，由同一父窗口下无 ID、无通知的同级 STATIC 保持完整字段几何，禁止父级包装 HWND 改变通知路由或造成嵌套 Edit 宽度坍塌；ListView 的固定圆角外框同样由无 ID、无通知的同父 STATIC 承载，使用空心 HRGN 保留真实列表的表体与滚动条命中区，并由布局子类同步可见性、位置、尺寸、启用、DPI 和相对 Z 序，真实报告缩入外框但不改变通知和滚动语义；外层闭合字段视觉与 ComboBox 共享确定性 Win11 圆角框，闭合 ComboBox 箭头必须使用子像素覆盖率 BGRA 图元；直接创建的安装镜像和高级选项字段也必须接入，不得用 `EM_SETRECT`、`WM_NCCALCSIZE` 或自绘文字伪造居中；进度条保持深色轨道、统一 Inno 绿色与 5px 内克制圆角，并在同一 BGRA 像素表面按覆盖率合成背景、外框、轨道和填充，填充圆角外部不得以矩形轨道色覆盖外框或留下黑块。
- 正常端客户区只使用普通不透明明暗主题，不得重新引入全客户区 DWM 背景或与之绑定的透明控件分支。原生 Edit 继续独占文本缓冲、光标、选择、IME、键盘命中和无障碍语义；普通短 `SS_LEFT` 标签保持库存顶对齐，只有明确带 `SS_CENTERIMAGE` 的单行标签才垂直居中。
- `正常系统端/src/native_ui/layout.rs`：正常端原生窗口共享的纯几何布局层，统一 100% 至 200% DPI 的外边距、紧凑/普通/分节间距和控件基线；标签、编辑框和闭合下拉框在奇数剩余像素时统一向下取整到视觉中心，避免 23px 字段与 24px 行之间出现一像素上偏；用当前窗口实际 Microsoft YaHei 字体测量中英文单行、换行文字与按钮宽度，决定标签/字段同行或堆叠、库存列表 3 至 8 行的自然高度以及隐藏控件零占位，不创建控件、不读取磁盘或产生业务副作用。
- `正常系统端/src/native_ui/dialog.rs`：Inno 风格原生模态/非模态对话框壳、主动按当前明暗主题绘制且把内容容器收到的命令、通知、颜色及 `WM_HSCROLL` 原样转发给所有者、ComboBox `WM_DRAWITEM` 和 ListView 表头的内容容器、按 DFM 约 46px 基线收紧的命令栏、工具弹窗同级命令按钮保持 10px 逻辑间距、以 75px 为下限并按实际微软雅黑 UI 字体/DPI 测量译文扩宽的命令按钮、仅响应 `BN_CLICKED` 且完整抑制擦除白闪的稳定自绘按钮、已有非模态窗口激活、按显示器工作区约束的高 DPI 尺寸、长说明实测换行与内容自然高度收紧、工作区钳制和消息转发；非模态命令默认仍关闭窗口，但允许具体工具把刷新/重新分析设为原位命令，并在主请求通过自身校验后显式隐藏，标题栏取消始终关闭；首次显示前统一为遗漏字体的后创建子控件补充微软雅黑并预先应用 Edit/ComboBox/ListBox/ListView/按钮明暗主题，主题变化通过共享整树重绘事务一次发布；顶层对话框保持普通不透明命中测试且不得使用分层 alpha 暂存，工具弹窗专用窗口类的 `hIcon/hIconSm` 固定为空且“关闭”操作只保留标题栏 X（取消操作仍保留命令按钮），主窗口独立类继续使用发布图标；异步结果不得清除尚未消费的关闭/命令结果或复活已隐藏窗口，不承载业务副作用。
- `正常系统端/src/native_ui/driver_transfer_dialog.rs`：驱动备份/恢复的专用 Inno 原生对话框，恢复导出/导入单选模式、按桌面当前系统/PE 首个离线系统语义选择的 Windows 分区、条件路径标签、目录浏览意图和本地输入校验，并为单选、标签、字段和状态统一应用微软雅黑 UI 与当前明暗主题；按可见内容紧凑布局，空状态不占位，100% 至 200% DPI 保持行距和命令区稳定；只产生强类型意图，不打开文件对话框或运行驱动命令。
- `正常系统端/src/native_ui/tool_dialogs.rs`：网络信息、软件清单、GHO 密码读取、镜像完整性和文件 SHA-256 的 Inno 原生对话框控件，按字段/滚动报告角色应用 DPI 与明暗主题，并完成紧凑状态展示与无副作用意图映射；GHO、镜像校验和文件哈希的浏览按钮是不会关闭或隐藏窗口的专用内容命令，必须复用普通 Inno 次要按钮的统一自绘、悬停、按下、禁用和焦点状态，并在窗口最终显示布局后仍与对应路径 Edit 同行且紧邻右侧，命令栏只保留实际执行操作；软件清单恢复迁移前的启动即加载和“名称/版本/发布者”三列 ListView，保存结构化记录并生成旧格式 TXT，不显示无业务接线的筛选编辑框，TXT 导出按钮按微软雅黑译文与 DPI 测量最小宽度；GHO 仅在有效且确有可解码密码时产生独立复制意图，镜像校验运行态提供显式取消意图，镜像与哈希结果保留大小、镜像数、分卷数和匹配状态等结构化字段；所有工具窗口标题只使用工具名称，不追加产品名。
- `正常系统端/src/native_ui/tool_dialogs_mutating.rs`：其余工具箱入口及安装/备份 BitLocker 解锁门禁的 Inno 原生输入、只包含真实库存且空选择不插入提示伪项的下拉/多选列表、按控件角色覆盖明暗主题、枚举库存复核、风险提示和二次确认对话框；门禁按可见字段、库存行数和状态文本自然收紧，隐藏凭据行不保留空槽；危险操作只产生经确认的意图，不在窗口过程中执行。
- `正常系统端/src/native_ui/tools/mod.rs`：需要专属状态与安全边界的原生工具对话框模块声明。
- `正常系统端/src/native_ui/tools/appx.rs`：移除预装 UWP/APPX 的专属 Inno 原生目标下拉和应用复选列表，桌面默认当前系统、PE 默认首个离线系统并异步加载对应库存，目标下拉只包含真实库存且空选择保持为空，保留全选、全不选、反选、计数与刷新；按钮只接受 `BN_CLICKED`、目标下拉只接受 `CBN_SELCHANGE`，程序批量写入 ListView 复选状态时必须屏蔽同步 `LVN_ITEMCHANGED`，避免半完成状态反写业务选择；只接受库存值并产生加载、确认或关闭意图，不提供目录/部署模式输入或执行删除。
- `正常系统端/src/native_ui/tools/boot_repair.rs`：一键修复引导的专属 Inno 原生 Windows 分区下拉、版本/架构详情、刷新和状态展示；下拉只包含真实 Windows 库存并直接映射索引，桌面优先当前系统分区、PE 优先首个离线系统，不暴露 Auto/UEFI/Legacy 输入，只产生刷新、确认或关闭意图，不写引导。
- `正常系统端/src/native_ui/tools/batch_format.rs`：批量格式化的专属 Inno 原生安全卷复选表、文件系统/容量/可用空间展示及全选、全不选、反选、刷新状态；严格生成固定 NTFS 与“新加卷”卷标的既有强类型确认意图，不枚举或格式化卷。
- `正常系统端/src/native_ui/tools/bitlocker_manage.rs`：BitLocker 管理的专属 Inno 原生加密卷状态表和条件操作区；按锁定、解锁、加密中、解密中状态流式排列实际可见的密码、恢复密钥、警告与操作控件，隐藏条件行不占位；恢复密码/恢复密钥解锁、读取与导出恢复密钥、挂起/恢复保护及解密选择，敏感值调试输出脱敏，只产生刷新、强类型操作、导出或关闭意图，不读取保护器、不写文件、不修改卷。
- `正常系统端/src/native_ui/tools/expand_c.rs`：无损扩大 C 盘的专属 Inno 原生分析结果、目标容量输入、明暗一致的双缓冲抗锯齿自绘滑块、移动分区警告和确认意图；滑块使用分析范围内的绝对十分之一 GB 值，拖动时单调换算并同步目标容量文本，初始化或异常的零位置通知必须钳制到安全最小值；“重新分析”是保持同一对话框可见的原位命令，只有经过校验的扩容请求才显式隐藏窗口进入确认；按分析结果紧凑流式布局，无有效分析时隐藏范围与安全说明，无移动分区警告或状态时不保留空槽，状态切换和自适应改高后按对话框首帧相同路径整窗重绘，避免保留 USER32 的白色静态区及命令按钮底面；只展示只读分析并产生请求，不枚举磁盘、不写扩容配置、不安装 PE 引导或重启。
- `正常系统端/src/native_ui/tools/hardware_inspector.rs`：详细硬件检测的专属非模态 Inno 原生窗口，使用左侧六类导航和双缓冲三列 ListView 展示后台只读快照，刷新通过带代次的窗口消息接收结果并保持界面响应；语言切换同步更新对话框壳、导航、列名、状态和当前内容，只产生刷新或关闭意图，不直接读取硬件或执行系统修改。
- `正常系统端/src/native_ui/tools/time_sync.rs`：系统时间校准的无输入 Inno 原生确认对话框，按微软雅黑 UI 与 DPI 为标题、空行及五个固定可信 NTP 回退地址保留完整高度，不得裁掉最后一项或说明文字；只产生确认或关闭意图，不联网或设置系统时间。
- `正常系统端/src/native_ui/tools/network_reset.rs`：重置网络的无输入 Inno 原生确认对话框，逐项披露既有后端的 Winsock、TCP/IP、DNS 缓存和 Windows 防火墙重置命令及真实风险，只产生确认或关闭意图，不执行命令。
- `正常系统端/src/native_ui/tools/nvidia_removal.rs`：英伟达显卡驱动卸载的专属 Inno 原生目标下拉、启动时仅展示检测到的 NVIDIA 显卡型号和实际全量清理范围；零/单显卡状态使用紧凑只读文字，多显卡才显示按实际数量自适应高度的列表；目标标签与下拉紧凑对齐，下拉只包含真实库存而不插入“请选择”伪选项，桌面默认当前系统、PE 默认首个离线系统且只接受对应 Windows 库存；不提供会被后端忽略的硬件 ID/CPU/内存明细、设备多选、软件卸载或自由输入，只产生加载、刷新、确认或关闭意图。
- `正常系统端/src/native_ui/tools/password_reset.rs`：密码重置的专属 Inno 原生目标下拉与按真实账户数保持 3 至 7 行的紧凑单选账户列表，目标下拉只包含真实库存并直接映射索引，桌面默认当前系统、PE 默认首个离线 Windows，操作固定为清空一个账户密码并启用该账户；异步库存带目标防串回校验，只产生加载、刷新、确认或关闭意图，不提供批量语义或执行修改。
- `正常系统端/src/native_ui/tools/partition_copy.rs`：分区对拷的专属 Inno 原生源/目标库存下拉与明细表，打开时立即由宿主异步预载库存并保留不会关闭窗口的手动刷新重试，恢复容量、已用、卷标、系统状态、断点续传状态、明暗一致的扁平进度和 100,000 字节受限日志；对话框被显示器工作区钳制后必须从真实内容客户区重新分配两张明细表和日志的弹性高度，底部刷新/开始命令始终保留完整可见；下拉和明细表只包含真实库存并双向排除另一侧已选分区，过滤后的控件索引必须重新映射到原库存盘符，默认以 USER32 空选择表示未选分区且不引入索引偏移；只产生刷新、既有强类型 `PartitionCopyRequest` 确认或关闭意图，不枚举磁盘、不复制文件。
- `正常系统端/src/native_ui/tools/quick_partition.rs`：一键分区的专属 Inno 原生物理磁盘下拉、BIOS(MBR)/UEFI(GPT) 单选、已有/新规划分区明细和紧凑低圆角分区图；分区图必须是一个连续长方形，分区按容量紧贴切块、只以竖线分隔并双行显示卷标/类型与大小，不得恢复分离卡片或过高留白。每个非特殊、非当前运行 Windows 的 NTFS 卷，只要自身可缩小、右侧有连续未分配空间或右侧紧邻另一可移动 NTFS 数据卷，右边界就显示小型竖向圆角拖柄：右侧是未分配空间时仍在该卷与未分配空间之间调整；右侧是另一数据卷时，向任一方向拖动都必须直接预览“左卷与右卷的总容量守恒转移”，不得在两卷之间插入虚假的未分配块。拖柄抗锯齿圆角的外侧像素必须分别与分割线左右真实分区底色做不透明合成，禁止把透明角、窗口底色或黑块覆盖到分区条上；柄内竖线必须按柄的实际像素宽度严格居中。拖动提交消息必须在子类过程返回后异步交给父窗口处理，禁止同步重入并在仍持有分区图独占状态时重绘或改写同一模型。连续拖动必须以当前暂存布局为基线：右侧卷先吸收全部或部分未分配空间后再向左侧卷转移时，必须保留前置扩容并按扩大后的两卷总容量重算，拖回基线则只撤销成对转移而保留其他暂存修改，禁止删除前一步或从原始库存重建导致未分配空间回弹。成对转移应用时必须交给强类型 WinPE 搬移流程；禁止在当前 Windows 会话中把缩小某卷伪装成另一卷已得到连续空间。拖动帧必须在内存 DC 完整绘制后一次发布、拦截库存背景擦除并合并未变化的鼠标位置，禁止直接逐图元刷到屏幕造成闪烁；拖动只更新暂存方案，提交后不得删除和重建内容未变化的明细 ListView。异步库存替换后必须重新按真实行数计算 ListView 和窗口高度，少量行完整展开，超过上限后固定高度滚动；底部刷新、应用和清盘主操作之间始终保留标准控件间距。系统卷判定必须来自当前 Windows 卷身份，禁止写死 `C:`。右键菜单必须使用紧凑确定性 owner-draw 适配明暗主题，按真实文字测量宽度，并为 USER32 `#32768` 弹层设置 DWM 小圆角和 Win10/WinPE 的圆角窗口区域回退；同时提供删除、可配置文件系统/卷标/分配单元/快速格式化、盘符分配/移除、MBR 活动标记、未分配空间建卷及 RAW 盘按当前固件模式初始化；已有 ESP 或无足够未分配空间时隐藏不适用的创建按钮。“应用修改”只提交手工编辑；会清空整盘的主操作必须单独标为“清空整盘并分区”，不得与普通应用显示为同义按钮。关闭存在未应用修改的窗口必须提供应用、放弃和取消选择。下拉只包含真实磁盘并保持默认空选择，库存由宿主提供，模块不枚举磁盘或执行存储写入；所有危险意图必须携带磁盘/分区指纹并由宿主二次确认。
- `正常系统端/src/native_ui/tools/storage_driver.rs`：导入随包存储控制器驱动的专属紧凑 Inno 原生 Windows 下拉选择、条件状态和确认意图，下拉只包含真实 Windows 库存并直接映射索引，按桌面当前系统分区/PE 首个离线系统的库存顺序选择默认目标；空状态不扩张内容区，不显示或接受驱动目录、递归选项，不读取文件或调用 DISM。
- `正常系统端/src/native_ui/theme.rs`：固定从 Inno 6.7 Modern Windows 11 明暗参考截图逐像素核对的窗口、普通/引导按钮和边框色，负责系统应用主题检测、GDI 画刷及主题资源生命周期；正常端客户区只允许普通不透明明暗主题，不得重新引入全客户区 DWM 背景或透明控件分支。单行 Edit 保留原生文本、光标、选择、IME 和无障碍语义，以字体度量后的紧凑客户区配合同级 STATIC 绘制完整 23px 字段底色和确定性圆角外框；闭合 ComboBox 与独立 ListBox 在原生绘制完成后最后覆盖唯一确定性外框，ListView 的唯一外框则绘制在不参与 comctl32 滚动、保持在真实列表上方且命中透明的空心同级 STATIC 上，库存弹层、选择、滚动、键盘和无障碍语义保持不变。ListView 未选中行继续由 comctl32 原生绘制，真实选中行只通过 `NM_CUSTOMDRAW` 最小接管背景与文字；非空列表仅覆盖最后一条真实行以下的无项目客户区，不得重画项目行、表头或滚动条。工具进度条、滑块、复选框和单选框使用确定性明暗/DPI 图元，读取 GDI DIB 像素前必须执行 `GdiFlush`，禁止透明角被写成黑色或重复插值产生颗粒。
- 明暗主题复选框必须继续使用固定 Windows 11 主题资源的原始半径、勾号、状态和配色；单选框按当前物理像素尺寸以固定 Windows 11 状态色和至少 8×8 子像素覆盖率生成四向对称的圆环或选中圆点。两类图元的透明及半透明边缘都必须与当前不透明窗口底色正确合成，禁止把 alpha=0 或半透明角直接写成黑色；USER32 仍独占切换、分组、键盘、通知和无障碍语义。
- `正常系统端/src/native_ui/window.rs`：正常端原生窗口类、显式大/小应用图标、消息循环、Per-Monitor V2 DPI、统一 Microsoft YaHei 字体、受当前显示器工作区约束的 DPI 最小跟踪尺寸、国际化导航/页头/命令栏和页面布局；导航栏使用 168px 逻辑宽度并在全部 DPI 下同步驱动按钮、内容起点、底色和分隔线，既保证最长英文导航可读，也不得在按钮右侧留下过宽空栏；主窗口统一转发 ComboBox owner-draw，ListView 表头只由 `theme.rs` 的唯一入口绘制；页面切换批量暂停重绘并在显隐和布局稳定后一次性重绘全部子控件，避免暴露中间布局造成闪烁；首次显示前必须用当前配置完成初始页面显隐事务，简易模式不得遗留仍可见但被布局到客户区右边界外的普通安装命令 HWND；安装页对驱动、重启、引导模式及无人值守控件做响应式分行，宽窗口中立即重启按实测译文宽度紧跟驱动下拉保持标准字段间距，取消无人值守后不再保留隐藏的文件选择和提示槽位；已移除的全局高级模式、修改引导命令以及旧 DiskPart 复选框和目录入口均不得创建或显示，但标准安装页必须创建并提供安装高级选项页入口，启动签名标签与下拉框只保留标准字段间距；底部命令栏保留稳定内边距，硬件页隐藏刷新后把保存和复制从右向左紧密排列，复制成功只局部刷新按钮为本地化“已复制”并用非阻塞三秒定时器稳定恢复，不使用缩放、弹性动画或整页重绘；分区动态状态随语言切换重新翻译。窗口统一过滤 Button 的非点击通知，工具对话框只响应一次真实点击且已有窗口优先激活，避免焦点通知导致重复窗口和后台任务；GHO、镜像和哈希浏览内容命令直接打开文件选择器并回填原对话框，不借用会隐藏窗口的命令栏结果；工具对话框关闭结果先于异步完成消息处理，轮询存活覆盖全部专用窗口和后台任务，关闭 A 后打开 B 不得复活 A；分区对拷首次库存仍在后台只读枚举，完成后额外投递窗口唤醒消息立即消费结果，定时轮询仅作为失败安全回退，不要求用户点击刷新；系统目标在桌面默认当前系统、PE 默认首个离线系统，并按规范化 `SystemDrive` 排除与在线系统重复的离线盘符项；物理磁盘和普通数据卷等不可逆目标仍不得自动预选。窗口把原生页面意图路由到安装、备份、异步下载目录及工具控制器、带代次校验的异步只读库存和强类型后台请求边界（包括区分在线/离线目标的 APPX 请求）；在线目录刷新后以同一 PE 快照驱动自动安装、备份和扩容，固定使用首个可用条目且不创建 PE 选择控件；安装或备份在首次点击、BitLocker 解锁和 PE 下载续做后都重新枚举并按完整磁盘/分区几何复核稳定身份、路由及锁定状态，BitLocker 门禁存续期间禁用主窗口，续做前不得复用可变列表索引，简易模式下载完成后重新读取真实镜像卷、架构和目标几何；安装意图通过稳定身份复核后必须把镜像文件名/格式/大小、目标卷名称/build/架构、目标和非目标 BitLocker 状态、磁盘结构及实际使用的 PE 名称写入诊断日志；安装页同时保留源/目标无人值守探测、多 PE 目录兼容、与引导修复和实际 UEFI 条件一致的 PCA2011/PCA2023 检测与选择，自动启动签名项必须显示当前实际推荐代次，以代次及目标磁盘/分区身份隔离异步同盘 ESP 只读检测结果，目标 ESP/EFI 检测错误始终失败关闭；Windows 7 USB3/NVMe 自动策略不得创建高级选项控件，USB3 在准确识别 6.1 镜像时自动启用，NVMe 仅在目标物理盘 WinAPI 总线查询明确为 NVMe 且镜像为 x64 时自动启用，目标镜像或磁盘身份变化后按新快照重算且查询失败不得猜测；UefiSeven 仍保持隐藏，存储驱动工具必须按当前 SetupAPI PCI ID 选出唯一 Intel VMD 子目录且禁止递归；`non-elevated-tests` 的确定性安装页和一键分区视觉夹具只提供只读虚拟库存，不得进入 release 行为或调用物理磁盘 API；执行失败时仅展示本地化摘要而把底层诊断留在日志；安装、备份、下载、扩容或镜像校验仍在运行时拦截窗口直接关闭，先请求取消并等待安全停止点；工具页负责把专属一键分区与 BitLocker 对话框接入 fresh 库存、二次确认和后台安全边界，并把镜像校验的实时进度及取消标志贯通到既有校验器；一键分区的连续直接扩容与离线成对转移必须在后台按序执行，直接扩容后重新枚举并只把 fresh 指纹和供体最终大小交给 PE，任一步失败都停止后续交接且不得伪装成原子成功；Wi-Fi 迁移只在已连接网络时抓取本次运行所需的瞬态配置；无损扩大 C 盘使用专属分析、二次确认、PE 下载及交接状态机。
  正常端在 WinPE 中运行时必须把简易模式的有效状态固定为关闭，并在关于页显示为未勾选且禁用；该有效状态必须同时用于首次显隐、完整布局、快速命令栏布局、条件控件、异步目录更新和命令路由，不能修改或覆盖用户持久化配置。启动 PCA 固件探测在安装选择尚不相关时不得覆盖首屏“启动模式/TPM/安全启动”摘要；镜像和目标使 PCA 校验相关后，未完成的固件探测或目标 ESP 探测才显示对应状态并持续阻断安装，固件既有结果必须复用而不得重新探测；已完成的目标 ESP 结果按目标磁盘/分区身份缓存，重新选择镜像时复用，目标身份变化时才重新探测。安装页启动后在后台枚举当前存在盘符，仅当全局恰有一个 `sources\install.esd` 或 `sources\install.wim` 且镜像输入仍为空、异步代次未变化时静默接受；没有结果或存在多个结果时不得填入，也不得显示“自动填入”类提示。可编辑镜像路径一旦发生用户修改，必须立即撤销旧异步代次、旧有效路径、卷列表、挂载 ISO 和安装可用状态；离开输入框后通过既有异步镜像检查重新接受新路径，禁止显示路径与执行源分离。安装页和备份页不得创建 PE 选择控件；远端非空 PE 目录刷新后替换当前快照，自动安装、备份和扩容始终使用当前快照首项，当前快照为空时失败关闭，不保留或恢复用户 PE 选择状态。
  安装页布局取决于异步镜像库存：每次从任意其他页面进入安装页，都必须在冻结提交期间按已接受库存执行完整布局，使条件镜像卷行始终位于分区标题之前；不得只恢复 HWND 可见性并假定启动布局仍然有效。`non-elevated-tests` 只允许用显式 `LETRECOVERY_UI_TEST_IMAGE_VOLUME` 注入确定性镜像卷视觉夹具，release 不得包含该入口。
  主窗口首次显示不得使用临时 `WS_EX_LAYERED`、`WS_EX_TRANSPARENT`、全局 alpha 或颜色键首帧屏障；所有受支持系统都必须在隐藏状态准备完整子控件树，以普通不透明窗口显示并同步完成非客户区、客户区与全部子控件绘制，显示前后都要验证扩展样式仍拥有输入，异常时销毁窗口并失败关闭。普通页面导航必须只冻结可见顶层根窗口，在既有子控件显隐和布局稳定后恢复根窗口，并以 `RDW_ERASE|RDW_ALLCHILDREN` 异步排队一个完整客户帧，禁止逐子 HWND 发送 `WM_SETREDRAW` 或改写其 `WS_VISIBLE`；主题、语言和首次显示等必须立即稳定的整树事务仍用一次同步 `RedrawWindow` 发布。
- 安装页主命令状态补充约束：切回安装页时必须在控件显隐稳定后，从真实镜像、目标分区、PE、PCA 和无人值守状态完整重算“开始安装”，不得沿用页面切换时的临时禁用态或直接伪造启用；`CB_SETCURSEL` 和 `BM_SETCHECK` 的程序化默认值不会产生用户通知，因此每次重算前必须从当前可见控件同步安装偏好，并调用与真实点击完全相同的 `install_intent()` 权威校验，禁止维护另一套近似启用条件。标准安装页必须创建安装高级选项页并由“高级选项...”进入、由“保存并返回”原子回写当前会话偏好；不得把该入口重新解释为已移除的全局高级模式，硬件页仍可把共享命令按钮作为“保存”使用。
- 主窗口分区库存实时性约束：使用顶层窗口收到的 `WM_DEVICECHANGE` 到达、移除、配置和设备节点变化通知触发后台只读枚举，以短时一次性定时器合并同一设备变化产生的通知突发，禁止常驻秒级轮询；消息线程只负责调度和应用完成结果，不得同步枚举磁盘。替换库存时按磁盘号、分区号优先并以盘符和容量安全回退来恢复安装及备份选择，目标消失时清空选择，且必须屏蔽批量替换期间的半完成 `LVN_ITEMCHANGED`。
- 安装分区选择通知必须检查 `NMLISTVIEW.uChanged` 的 `LVIF_STATE` 及新旧 `LVIS_SELECTED` 位，只把同一次移动产生的旧行取消与新行选中合并成一条队列消息；依赖状态和布局只能在 ListView 同步通知返回后的一轮根窗口重绘事务中更新，文本、焦点及未改变选中位的通知不得重绘页面。
- ListView 表头外框补充约束：表头只能绘制自身背景、文字和列分隔线，不得在 Header DC 或父 ListView DC 上补画圆角外框；固定外框只由同父空心 STATIC 覆盖层负责。Header 与 ListView 的坐标系和水平滚动裁剪范围不同，混用会在最后可见列表头旁产生第二个圆角收尾。
- Win10 控件主题补充约束：库存 `CBS_DROPDOWNLIST` 的弹出 ListBox、键盘和无障碍语义保持原生，闭合选择字段由共用主题层覆盖为固定 Inno 明暗底色，浅色不得随 Win10 系统强调色漂移而变灰；`BS_AUTOCHECKBOX` 保留 USER32 自动切换、焦点和通知语义，仅由共用子类确定性绘制 Inno 明暗字形与微软雅黑文本，Win10 深色不得出现浅色方框或黑色标题。
- 安装页镜像卷条件布局补充约束：未识别到可安装卷时卷选择行必须零占位；识别成功后用约 120ms、三帧确定性线性布局过渡为后续分区和选项让出空间。过渡必须使用 UI Timer 和逐帧批量暂停重绘，不改变 ComboBox 选择或焦点，不使用弹性动画，窗口销毁或页面切换时必须清理定时器。
- 闭合 ComboBox 裁剪补充约束：为保留弹出列表高度而创建的 HWND 在设置矩形闭合字段区域后，必须立即失效并擦除父窗口刚暴露的尾部矩形；`SetWindowRgn` 只重绘被裁剪的子窗口，不会清掉此前位于新区域之外的 UxTheme 像素，遗漏父区重绘会在深色冷启动时留下字段下横线。
- 共享原生控件回归约束：Inno 自动单选标记按约 13px 的 96-DPI 逻辑基线缩放，保留 USER32 分组、键盘、通知和无障碍语义；所有自绘导航及命令按钮稳定使用原生箭头光标，跨越抗锯齿圆角不得在手形与箭头之间闪烁；固定只读多行报告保留原生文本、选择和 `WS_VSCROLL`，但移除 Edit 方形非客户区并由列表式明暗圆角外框收口；ListView、ListBox 和只读滚动报告的非客户区滚动条继续由 USER32/comctl32 原生绘制，悬停动画、滚轮、拖动和滚动消息必须先完成默认处理，再在最外层最后覆盖确定性明暗圆角边框，禁止接管滚动条或让右侧圆角退化为方角、黑块和颗粒；无损扩大 C 盘的普通滑块 tick 只更新字段、滑块和命令状态，仅在跨越移动分区警告阈值时重新布局；密码重置账户选择不得重新填充库存或重新测量窗口，一键分区“调整大小”按钮的启禁变化只局部无擦除重绘并保持四边同帧闭合。
- 所有 report ListView 必须启用 `LVS_EX_DOUBLEBUFFER`、`WS_CLIPCHILDREN` 与 `WS_CLIPSIBLINGS`；Header 禁用库存主题动画并由单一确定性入口绘制，Header 绘制不得反向失效整表。固定圆角外框必须由真实 ListView 上方的同父 STATIC 独占，以空心 HRGN 只覆盖外框像素，并返回 `HTTRANSPARENT` 把命中测试交还给真实列表；覆盖层必须持续同步真实列表的可见性、启用、位置、尺寸与 DPI，并在列表窗口位置变化后重新升到其上方。真实 ListView 保持原父级、控件 ID、通知、选择、键盘、滚动和无障碍语义，只缩入 DPI 缩放的外框宽度。`WM_HSCROLL`、`WM_VSCROLL`、滚轮和 `SB_THUMBTRACK` 必须交由 comctl32 默认处理，高频分支不得逐消息失效整表、调用 `UpdateWindow`、同步补画 Header/外框或直接竞争重定向表面，避免原生像素位移把固定外框复制进项目行。UxTheme 滚动条定时器仍只调用默认过程；深色滚动条保留 comctl32 v6 原生语义并使用 `DarkMode_Explorer`，不得使用只支持旧 comctl32 的 FlatSB 颜色覆盖。
- `正常系统端/src/native_ui/window.rs` 状态补充约束：工具页固定说明不得被“时间同步成功”等加载或完成结果覆盖，具体结果只进入对应对话框和日志；PCA 固件/目标 ESP 探测文案只能出现在安装页，切换到工具、硬件等页面时必须清空，返回安装页后按当前 pending、错误或完成状态重建；PCA pending 必须独立阻断“开始安装”，禁止残留“正在检测”与已启用安装按钮矛盾。
- `正常系统端/src/native_ui/pages/mod.rs`：正常端原生页面模块声明。
- `正常系统端/src/native_ui/scrollbar_compositor.rs`：安装高级选项页滚动条的 Composition/GDI 双缓冲发布边界；随标准安装页创建并在高级选项可见时进入运行时窗口树，必须保持主题、DPI、滚轮与拖动回退一致。
- `正常系统端/src/native_ui/pages/advanced.rs`：安装高级选项页的原生控件与 `AdvancedOptionsData` 映射；标准安装页启动时创建、默认隐藏，通过全局命令栏进入和保存返回，只修改当前安装会话的高级选项，不恢复已删除的全局高级模式配置或 DiskPart 执行能力；必须按当前镜像的 major/minor/build 能力掩码隐藏并即时重排不适用的系统设置，切换镜像时不能遗留空行或继续读取隐藏控件。
- `正常系统端/src/native_ui/pages/backup.rs`：系统备份原生输入页、可刷新且保留稳定选择的分区库存、运行期列头、动态行与控件本地化，表格列按 DPI 最低可读宽度和可用剩余空间响应式分配，避免数值被挤成省略号而右侧闲置；默认名称和描述使用同一稳定时间戳按当前语言生成，切换语言时只更新仍等于旧默认值的字段，逐字段保留用户输入；负责格式/路径状态读取、明确校验及到既有 `BackupConfig` 的无副作用转换；PE 路由由窗口和备份控制器从已验证目录首项自动决定，本页不创建 PE 选择控件；保留桌面端默认当前系统分区、PE 中仅有一个 Windows 分区时才默认选择的规则，并公开所选非 Windows 源警告刷新入口，不显示对用户无决策价值的“将通过 PE 环境备份”路由提示。
- `正常系统端/src/native_ui/pages/download.rs`：在线下载原生分类、资源列表和保存位置控件，按分类隐藏空白或重复的类型/大小列并响应式分配剩余列宽，支持运行期完整本地化，只产生下载/安装意图。
- `正常系统端/src/native_ui/pages/easy_mode.rs`：简易模式的原生系统/卷选择、可选 Logo、说明和主操作控件，支持明暗主题、DPI 与运行期本地化；组合框仅在目录实际变化时重建内容且选择路由只响应 `CBN_SELCHANGE`，程序化选择后完整重绘闭合字段，避免展开期间重建或暗色箭头区域造成旧项残影；异步快照更新必须服从页面可见状态，不能把提示或 Logo 重新显示到普通安装页；启用状态只在关于设置页修改，系统安装页不显示关闭开关，只产生页面命令。
- `正常系统端/src/native_ui/pages/tools.rs`：工具箱 21 个原生入口及正常端/PE 可用性约束；无损扩大 C 盘和详细硬件检测作为末尾稳定命令 ID 的正常端专属入口，只产生工具意图。
- `正常系统端/src/native_ui/pages/info.rs`：把启动时已读取的 `HardwareInfo` 映射为响应式“类别/项目/值”原生 ListView，同组类别只在首行显示且类别列保持紧凑；复制与 UTF-8 TXT 导出使用清晰的分组标题和逐行“项目: 值”格式，兼容中英文标签、续行缩进和完整硬件数据，不重复输出同组类别或把多条内容粘连成一行；网卡速率为零时必须显示本地化“未知”，不得把探测失败伪装为 0 Mbps；`non-elevated-tests` 可用 `LETRECOVERY_UI_QA_LONG_LIST=1` 把库存替换为 120 条确定性只读行，仅供滚动视觉回归，release 不得存在该入口；硬件报告占满页面主体，保存和复制位于稳定底部命令栏；同时承载紧凑且不侵入状态栏的关于页，以及语言、日志、小白模式、8/16/32 下载线程档位和 WIM 引擎；正常端不再显示或处理高级模式开关，保留运行期完整本地化、PE 环境禁用规则、版权致谢、单一许可入口、纯按钮链接与日志目录意图。
  硬件库存更新时，必须在同一窗口消息内完成整批删除和插入，再执行一次包含非客户区与子窗口的同步 `RedrawWindow`；不得对子 ListView 使用会由 `DefWindowProc` 切换 `WS_VISIBLE` 的 `WM_SETREDRAW`，否则 DWM 可能发布暂时缺少真实 Header 子窗的重定向帧。禁止按行、按列或用定时器逐项发布中间帧。
- `正常系统端/src/native_ui/pages/progress.rs`：原生长任务标题、说明、双层进度、运行/取消/成功/失败状态和稳定命令栏；只更新实际变化的文本、整数百分比、命令状态和进度区域，运行与完成保持同一 Inno 绿色；长任务使用完整客户区且不得透出普通页面的导航分隔线，下载无后续动作时显示本地化返回按钮而不暴露内部枚举，终态只生成重启、打开下载文件或返回等显式后续意图，不自行执行系统操作。
- `正常系统端/src/core/mod.rs`：正常端核心模块声明。
- `正常系统端/src/core/app_config.rs`：`config.json` 用户偏好、语言、日志、外观和默认选项的读取保存；所有写入必须在共享进程锁内通过原子替换完成，普通偏好保存必须重新读取并保留在线目录异步写入的最新 PE 缓存，PE 缓存更新只能替换自身字段并保留最新用户偏好，禁止由窗口持有的旧配置快照造成字段丢失；已移除的正常端全局高级模式字段只为旧配置反序列化兼容，加载时必须固定关闭且保存时不再写回，旧 DiskPart 开关必须清空，但仍受支持的安装高级选项继续保存普通偏好，密码、当前用户名和 Wi-Fi 瞬态材料不得持久化；当前会话用户名探测必须复用共享本地账户策略，PE 中的 SYSTEM/TrustedInstaller 等服务身份不得成为安装用户名；下载线程字段向后兼容旧配置并在加载与保存入口归一到 8/16/32 三档，旧配置保持原有 16 连接默认值。
- `正常系统端/src/core/advanced_options.rs`：安装高级选项的序列化兼容、离线系统应用、脚本/文件暂存、Wi-Fi 瞬态配置、内置 Administrator 非持久化密码选项和历史 XP/Win7 兼容业务边界；内置 Win7 USB3 必须先验证锁定资源，再按离线目标架构和当前 SetupAPI 硬件 ID 逐包注入，内置 NVMe 必须只按锁定顺序使用 DISM `/Add-Package` 安装两个微软 CAB；离线 hive 必须由清理守卫覆盖所有提前返回，临时卸载后必须在继续执行前完整重载，用户选择的驱动目录缺失、CAB 解包或 DISM 注入失败都必须失败关闭；不再包含 egui 或其他渲染代码，正常端 `native_ui/pages/advanced.rs` 只负责采集当前会话意图，默认兼容选项仍由安装意图按镜像和硬件安全推导。
- `正常系统端/src/core/appx_legacy_impl.rs`：历史在线/离线 APPX 库存、关键包判断和移除实现，仅由受限兼容桥调用；新的执行入口必须优先经过 `native_appx.rs` 的 fresh 库存与关键包复核。
- `正常系统端/src/core/bcdedit.rs`：定位和挂载 ESP、BCD/BCDBoot 修复、活动分区处理及 PCA 引导源选择；自动 PCA/EFI 只读检测先通过 `FindFirstVolumeW`/`FindNextVolumeW` 枚举并以 `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS` 复核物理身份后的卷 GUID 根路径读取，Windows 7 隐藏 ESP 无法映射时才允许使用共享 `GetLogicalDrives` 和 `IVdsAdvancedDisk` 分配真正空闲的临时盘符。回退挂载必须在读取前后复核磁盘号与分区偏移，已有盘符只借用，所有正常、错误和取消路径都显式关闭守卫，卸载失败必须详细记录、明确提示并阻止安装；UI 必须区分“磁盘上没有 ESP”与“ESP 已存在但无法解析或挂载”。正常端 BCD 查询和 PE 启动项维护使用 `GetSystemDirectoryW` 定位的宿主 `bcdedit.exe`，不得让随包现代副本成为 Win7 装载前提；为离线新系统保留随包 BCDBoot。已经移除 UI 授权入口的 `bin/repair_boot.txt` 和 `bin/diskpart/` 不得出现在发布包或恢复执行。ESP 盘符分配和 MBR 活动标记必须复用共享 WinAPI 存储边界，不得生成或解析 DiskPart 文本；Legacy 修复必须同时确认 Bootsect 成功和活动位写入成功后才能报告完成。
- `正常系统端/src/core/bitlocker.rs`：BitLocker 卷枚举、状态解析、解锁、暂停/恢复保护、解密及恢复密钥处理。
- `正常系统端/src/core/cabinet.rs`：兼容再导出共享 `lr-core::windows_cabinet`，正常端不得另建 `expand.exe` 或本地化输出解析路径。
- `正常系统端/src/core/cli_install.rs`：解析命令行无人值守安装配置并启动与 GUI 相同的安装入口。
- `正常系统端/src/core/disk.rs`：分区枚举、样式和磁盘关系查询、SSD/HDD 与内外置介质探测、ViaPE 暂存策略接入，以及通过共享 WinAPI 边界缩小/创建/删除/回收暂存与恢复分区；枚举结果还必须从物理磁盘 IOCTL 库存补齐磁盘容量、分区偏移和精确长度，缺失这些几何身份时安装意图失败关闭。当前运行 Windows 卷必须由共享 `GetWindowsDirectoryW` 边界确定，禁止读取可能陈旧的 `SystemDrive` 或回退写死 `C:`/`X:`。ViaPE 暂存可在固定存储不足时回退到可写外置存储，但必须在候选入口排除光驱、网络盘、RAM 盘和只读卷；微软支持在不完整解密的情况下在线缩小 BitLocker NTFS 卷，因此当前系统卷只有在已解锁、转换状态稳定且位于非外置磁盘时可进入原生缩卷路径，已锁定、加密/解密中和未知状态继续失败关闭；自动缩卷前必须重新复核 BitLocker 状态及当前启动会话内的物理磁盘和分区身份，创建失败时只允许按已验证的实际回收量回滚扩容。
- `正常系统端/src/core/dism.rs`：正常端高层镜像查询、释放、捕获和进度模型，完整透传版本、Build、架构元数据并接入统一 WIM 引擎，同时把安装执行器持有的原子取消标记传递到可取消的镜像应用入口；在线驱动导出优先走受支持的 DISM 命令，失败时才严格回退 SetupAPI，离线导出只允许 DISM 且不得回退手工复制 DriverStore；用户显式“仅保存”仍必须验证非零 INF，安装默认“自动导入”则允许 SetupAPI 完整枚举后得到零个第三方 OEM 包，但仅在启动存储要求也为空、空清单已原子生成并回读时作为明确 no-op；在线与 PE 离线导出都必须在覆盖验证后原子生成启动存储驱动清单，禁止出现“导出成功但后续必需清单缺失”的半完成状态，错误必须保留底层 DISM 命令链以便日志定位。
- `正常系统端/src/core/dism_cmd.rs`：DISM.exe 参数封装、进度解析、在线/离线驱动导出、离线驱动和更新包操作；优先使用当前 Windows/WinPE 自带 DISM，仅在不可用时回退随包兼容副本；stdout/stderr 必须并发持续排空以避免管道互锁，逐行按 UTF-8/GBK 解码，错误摘要有界且非零退出必须携带可用诊断；驱动源目录枚举拒绝重解析根并传播遍历错误，INF/CAB 混合目录或多 CAB 处理只要任一子操作失败就整体返回失败，禁止部分成功伪装成导入完成；批量驱动导入失败时逐 INF 隔离，只有 `driver_package_trust` 为同一具体 INF 授权后才可执行不带 `/Recurse` 的受控 `/ForceUnsigned` 重试。
- `正常系统端/src/core/driver.rs`：共享驱动实现的兼容再导出，以及只允许 DISM、任何失败均停止的离线驱动导入策略。
- `正常系统端/src/core/ghost.rs`：Ghost 镜像信息、备份、还原、进度、取消和错误分类。
- `正常系统端/src/core/gho_password.rs`：读取和解码多种 GHO 头部中的密码信息。
- `正常系统端/src/core/hardware_info.rs`：使用 WinAPI/WMI 收集 CPU、内存、主板、BIOS、磁盘、GPU、网络、电池和系统信息；网络摘要必须复用 `tool_network` 的统一 IP Helper 枚举和 Windows 7 链路速率回退，禁止保留固定返回 0 Mbps 的第二套旧枚举；物理磁盘容量使用长度查询并以扩展几何查询安全回退，分区布局查询同时保留真实分区数；诊断日志按制造商与机型识别常见 VMware、Hyper-V、VirtualBox、QEMU/KVM、Parallels 和 Xen，未知指纹只能写“可能为实体机”，不得断言为实体机。
- `正常系统端/src/core/hardware_info/names.rs`：硬件厂商和 GPU 名称的纯规范化及占位符识别。
- `正常系统端/src/core/hardware_inspector.rs`：详细硬件检测的只读快照边界，组合既有硬件摘要、CPUID intrinsic、严格边界检查的 `RSMB` 固件表解析、按 LUID 去重的 DXGI 适配器，以及标准 NVMe Identify/SMART 健康日志查询；解析 128 位寿命计数器并显示累计读写、健康度、温度、通电和错误统计，不加载驱动、不访问硬件端口、不执行磁盘写入。
- `正常系统端/src/core/image_verify.rs`：识别 WIM/ESD/SWM/GHO 等镜像并编排验证缓存探测、完整校验、进度和结果汇总；只有完整 libwim 校验成功后才授权写入当前指纹，支持调用方持有且不会被内部复位的取消标记，并把它传入 WIM/ESD/SWM 的 libwim 校验回调。
- `正常系统端/src/core/image_verification_cache.rs`：正常端 WIM/ESD 重复校验的有界持久化 BLAKE3 缓存；以拒绝写入/删除共享的源句柄、只读映射、官方 BLAKE3 的 Ryzen/Intel 运行时 SIMD 分派与 Rayon 分块、原子缓存替换和严格回退保证快路径不把变化或缓存错误误判为成功。
- `正常系统端/src/core/install_config.rs`：正常端到 PE 的安装、备份、扩容配置，安装标记、资源暂存和无人值守文件验证；相邻分区空间转移必须随扩容配置写入桌面端 fresh 复核后的磁盘号/容量、目标与供体分区号、偏移和长度，以及供体精确最终大小，PE 缺失或不匹配时失败关闭。当前安装会话可暂存内置 Administrator 账户名、密码与自动登录选择，但密码不得进入持久化应用配置或日志；XP 文本模式交接显式记录目录型源标记和 I386/AMD64 安全子目录；扫描覆盖 A-Z 并用 `SessionId` 唯一绑定安装标记与配置，旧空会话配置仅在单一无歧义任务时兼容，所有写入 INI 的值在落盘前拒绝 CR/LF/NUL 注入；备份与扩容交接配置使用同目录原子替换并返回可精确恢复旧文件或仅删除本次新文件的事务凭据，不删除数据目录中的其他用户文件；回滚会独立尝试恢复配置与标记并合并错误，避免单项失败阻断另一项恢复。
- `正常系统端/src/core/iso.rs`：通过 Windows Virtual Disk API 挂载和卸载 ISO；挂载后必须用 `GetVirtualDiskPhysicalPath` 与 `IOCTL_STORAGE_GET_DEVICE_NUMBER` 精确关联本次附加设备和盘符，映射失败立即按同一句柄回滚。卸载只能按 LetRecovery 持有的原始 ISO 路径执行，不得扫描、猜测或弹出其他 ISO、物理光驱和用户介质，成功操作后的卸载失败也必须向上传播。
- `正常系统端/src/core/nvidia_driver.rs`：GPU 枚举、厂商识别、NVIDIA 驱动设备和软件清理支持。
- `正常系统端/src/core/pe.rs`：区分用户可定制的随包 PE 与严格校验的受管下载缓存，协调 PE 文件准备、启动项安装和进入 PE；所有 BCD 写入逐条检查启动及退出状态，GUID 文件缺失或无可信 `boot.sdi` 时停止，不得用伪造占位文件继续重启。
- `正常系统端/src/core/pca_preflight.rs`：正常端 PCA 写盘前预检适配、固件读取、匹配兼容包准备及共享错误到本地化用户提示的映射。
- `正常系统端/src/core/partition_copy_impl.rs`：历史分区对拷库存、断点标记、空间检查和逐文件复制实现；只有 `native_partition_copy.rs` 在 fresh 复核与请求校验后才能进入该执行边界，部分失败必须保留可恢复标记。
- `正常系统端/src/core/quick_partition.rs`：通过存储属性/卷/磁盘 IOCTL 一次进程内枚举物理磁盘、分区布局、卷标/文件系统/使用量和活动标记，避免逐盘启动 PowerShell/WMIC；一键分区及已有分区创建、删除、格式化和扩缩复用共享 WinAPI 边界，执行前后复核磁盘身份、分区偏移、大小和结果状态。相邻未分配空间必须保留精确字节数，与分区原始字节数合并后才能统一向下转换为 MiB，禁止分别取整造成合法的末端拖动被误拒绝并回弹。
- `正常系统端/src/core/registry.rs`：共享离线注册表实现的兼容再导出。
- `正常系统端/src/core/system_info.rs`：当前机器启动模式、Secure Boot、TPM 和环境摘要。
- `正常系统端/src/core/system_utils.rs`：PE 文件架构、离线 Windows 版本/架构、权限和系统路径等通用 Windows 工具，并通过 Task Scheduler 2.0 COM API 连接本机服务、定位和触发系统自带的 `StartComponentCleanup` 任务。
- `正常系统端/src/core/ui_state.rs`：原生 UI、配置文件和 CLI 共用的安装偏好、启动模式、驱动动作及高级选项纯数据模型，保持旧 JSON 默认值和字段兼容，并显式转换到旧高级选项业务类型以保留不可序列化的 Wi-Fi 瞬态数据；首次缺省启用移除快捷方式箭头、跳过 Windows 11 联网要求、禁用保留存储和禁用设备自动加密，PE/服务账户令牌不得成为自动填充用户名；同时集中维护 XP、Windows 7/8/8.1/10/11 的高级选项能力矩阵，界面隐藏不能破坏其他镜像的持久偏好，但进入安装意图的克隆必须清除目标版本不支持的字段。
- `正常系统端/src/core/tool_actions.rs`：历史外部工具启动、Ghost/SpaceSniffer、引导修复和驱动导出兼容入口；只允许由原生工具控制器生成并确认的固定动作调用，不接受服务端自由命令。
- `正常系统端/src/core/tool_driver.rs`：历史在线/离线驱动导出、导入及固定存储控制器目录兼容实现；新的存储驱动入口仍须经过硬件 ID 唯一匹配与 `native_storage_driver.rs` 安全计划。
- `正常系统端/src/core/tool_network.rs`：网络适配器只读详情和既有网络重置步骤实现；链路速率优先使用 `GetAdaptersAddresses` 的 64 位值，只有返回零时才按接口索引回退 Windows Vista+ 的 `GetIfEntry2`，最后使用旧 `GetIfEntry`，不得把全部接口未知误显示为 0 Mbps；修改性入口必须经过原生确认与工具后端，不在渲染回调中执行。
- `正常系统端/src/core/tool_time_sync.rs`：固定可信 NTP 服务器查询、北京时间换算、Windows 系统时间写入和结果模型；网络与写时钟操作只从已确认的工具后端进入。
- `正常系统端/src/core/tool_types.rs`：原生工具控制器、库存与兼容业务共用的驱动模式、APPX、软件、Windows 分区、GHO、NVIDIA 和镜像校验数据类型的唯一模块实例。
- `正常系统端/src/core/windows_version_detect.rs`：离线 Windows 注册表、文件版本与架构回退检测，以及工具库存使用的分区版本摘要；不承担界面渲染。
- `正常系统端/src/core/native_wifi.rs`：使用 Native Wi-Fi API 枚举已连接接口、读取当前连接属性并按显式明文密钥标志取得本次安装会话需要的 profile XML；XML 只在内存中传递，不落临时文件，开发测试构建禁止宿主探测和获取。
- `正常系统端/src/core/native_expand_c_controller.rs`：旧入口通过 `GetWindowsDirectoryW` 动态确定当前系统卷，同时只读分析任意指定盘符、相邻未分配空间及后方可收缩分区，区分直接扩展与需要搬移数据的容量上限；为成对分区边界拖动提供“目标从左侧相邻普通数据卷借空间”的独立只读分析，左侧借空间的直接扩展上限固定为当前大小。两种方向都通过同一 WinAPI 身份拒绝把当前运行 Windows 卷作为待移动卷，不得回退 `SystemDrive` 或写死 `C:`；不执行缩小、移动、PE 准备或重启，开发测试构建在读取宿主磁盘前拒绝。
- `正常系统端/src/core/native_expand_c_executor.rs`：指定盘符无损扩大的强类型 PE 交接 worker（旧 C 盘界面继续复用）；执行前按供体方向重新分析并比对完整磁盘/目标分区身份、当前大小和安全容量，复验受管 PE，把 `BorrowFromLeft`、供体精确最终大小及 fresh 磁盘容量、目标和左右供体精确几何写入向后兼容 marker/INI，保持最大容量编码为零、旧配置未指定供体最终大小时按需收缩的兼容语义并安装既有 PE 启动项；PE/BCD 启动项安装失败时精确回滚本次扩容 marker/INI，开发测试构建在建线程和任何宿主 I/O 前拒绝。
- `正常系统端/src/core/native_install_controller.rs`：原生安装页输入快照的校验、Direct/ViaPE 路由，以及保持 XP/GHO/PCA/驱动/无人值守字段兼容的安装意图与配置转换；目标身份必须同时包含磁盘号、分区号、磁盘容量、分区偏移和精确长度，任一缺失不得产生安装意图。ViaPE 只校验目录是否可用并固定产生首项索引，不接受 UI 选择状态；内置 Administrator 在写盘前校验无人值守、镜像族、外部应答文件、普通自定义用户名、账户名与密码的兼容边界，普通自定义用户名必须通过共享系统保留身份策略；只有经库存确认仍包含 Windows 的目标才允许计划导出旧系统驱动，新建或已清空分区不得因默认 AutoImport 在镜像应用前触发离线 DISM 导出；原版 XP I386/AMD64 选择当前系统盘时走标准 ViaPE 路由而不是桌面原地写盘；PCA 检测只在镜像支持、启用引导修复且目标可能使用 UEFI 时阻断，不能让持久化选择误拦 GHO、XP 或 Legacy 安装；生成意图前还必须以选中镜像 major/minor/build 对高级选项克隆再次过滤，禁止隐藏的现代 Windows 默认项流入 Win7/旧系统或 ViaPE 配置，并只为 Windows 7 x64 UEFI 自动置位 UefiSeven 交接字段。
- `正常系统端/src/core/native_install_executor.rs`：原生安装意图的 Direct/ViaPE 有序阶段计划、BitLocker/PCA/稳定目标安全门、Direct 驱动导出先于目标分区写入和格式化的保护顺序、按实际耗时把镜像应用和源复制置于主要区间的加权总进度、进度取消消息和可注入副作用后端；稳定目标身份包含磁盘/分区编号和完整几何，生产后端在 BitLocker 检查与每次重新定位目标时都必须完整比较，不能接受编号复用。历史分区脚本阶段名只为旧状态兼容，不得由新意图启用。`InstallExecutionError::user_message` 按失败类别和阶段提供不泄露底层诊断细节的本地化用户摘要，源镜像校验失败还必须展示不含路径的常见原因与固定诊断代码，使单张截图可供支持判断，完整 code/detail 继续保留在 `Display` 日志边界，开发测试构建强制拒绝执行。
- `正常系统端/src/core/native_partition_copy.rs`：原生分区对拷的只读展示库存，以及严格盘符、fresh 卷库存、目标空间、断点来源验证、受限日志和可注入执行边界；展示库存不授予执行权限，确认后仍须 fresh 复核，执行时可向原生窗口流式回报进度，并把完成、部分失败和断点续传结果显式区分；开发测试构建在宿主 I/O 前拒绝。
- `正常系统端/src/core/native_nvidia_removal.rs`：恢复迁移前英伟达驱动清理语义的强类型当前/离线 Windows 目标、仅含真实 NVIDIA GPU 型号的启动硬件摘要和既有后端请求适配；执行范围固定为原业务支持的全部 NVIDIA 驱动，不虚构单设备或 NVIDIA 软件卸载能力，开发测试构建在宿主硬件读取前拒绝。
- `正常系统端/src/core/native_password_reset.rs`：当前系统/离线 Windows 的强类型单账户密码重置请求、只读账户库存和执行边界；当前系统复用共享 NetAPI 边界清空密码并启用账户，离线系统复用带 SAM 备份的共享实现，执行前 fresh 复核账户且开发测试构建在任何宿主 I/O 前拒绝。
- `正常系统端/src/core/native_quick_partition.rs`：原生一键分区的磁盘号、型号、容量及现有布局强类型指纹，GPT/MBR 与分区大小、盘符、卷标、文件系统计划校验，执行前 fresh 枚举 fail-closed 复核及可注入测试边界；开发测试构建在任何磁盘 I/O 前拒绝。
- `正常系统端/src/core/native_quick_partition_dialog.rs`：一键分区专属原生对话框的纯编辑状态、可用盘符和当前 Windows 卷保护信息；生成 `QuickPartitionRequest` 与暂存的已有分区扩缩、相邻两个 NTFS 数据卷总容量守恒转移、删除、参数化格式化、盘符、活动标记、未分配空间创建及 RAW 盘初始化操作批次。已有分区的原地扩大上限和 fresh 复核必须先合并分区与右侧连续未分配区的精确字节数，再统一转换为 MiB。成对转移请求必须同时绑定左右分区指纹、原始/目标大小、使用量和完整磁盘指纹，拒绝非紧邻、特殊、当前 Windows、无盘符、非 NTFS 或剩余空间不足的目标；“右侧分区先吸收未分配空间、再向左侧分区转移”的连续计划必须保留为有序的直接扩容与 PE 转移两步，第一步完成后重新枚举、重建指纹并验证两卷最终总容量，禁止从原始布局重算或删除前置扩容。批次执行前必须 fresh 复核原始完整磁盘指纹，每一步都按当前库存重新定位目标、动态识别运行 Windows 卷、拒绝危险系统目标并在共享 WinAPI 调用后重新枚举验证；需要搬移数据的成对转移只能由 PE 强类型交接执行，桌面直接执行边界必须失败关闭。开发测试构建在任何宿主磁盘 I/O 前拒绝。主窗口收到合并后的设备/卷变更消息时自动刷新无暂存修改的对话框；已有暂存修改时必须标记库存过期并禁用应用，禁止静默覆盖编辑方案。
- `正常系统端/src/core/native_storage_driver.rs`：原生存储控制器驱动工具的固定随包 `bin/drivers/storage_controller` 来源、离线 Windows 目标及 fresh 库存复核计划；只读检查可注入且开发测试构建在宿主 I/O 前拒绝，本模块不调用 DISM 或写入驱动。
- `正常系统端/src/core/native_install_backend.rs`：原生安装执行器的生产后端阶段实现；复用既有镜像、WinAPI 格式化/盘符/活动标记/MBR 签名、驱动、引导和高级选项边界，原版 XP/2003 当前系统盘重装会递归拒绝链接和特殊项、统计空间、把 I386/AMD64 及 x64 同级 I386 复制到会话唯一临时目录、复验后原子提交给 PE；ViaPE 安装源若与目标数据区路径相同必须按已存在源复核而非删除或复制自己，其他路径必须复制到会话唯一暂存文件，在复制、落盘、可取消复读哈希和镜像结构校验期间连续汇报进度，验证完成后再原子发布且不做无价值的第三次发布后哈希。单文件 WIM/ESD 在拒绝源写入和替换的句柄锁内并行执行完整校验与复制/源 SHA-256，目标同步落盘并复读 SHA-256 完全相等后继承源校验结果；SWM、GHO、ISO 和 XP 路径继续使用原有独立校验语义。Direct 安装的 Windows 10/11 默认无人值守在写入前必须通过 `lr-core::offline_international` 读取已释放目标的国际化设置，不能从当前正常系统或 WinPE 宿主继承；Direct 只在确有显式高级操作时开启离线 hive 事务，所选 Wi-Fi、注册表、自定义文件、驱动、UefiSeven、无人值守或 Win7 兼容步骤的输入缺失和执行失败必须终止，不得记录后进入 Finish。显式保留 Wi-Fi 瞬态 profile，恢复后台 fresh BitLocker 门禁、无恢复密钥时的完全解密等待、分区变化后按完整几何稳定身份重定位/分配盘符，以及 WIM/GHO/校验长任务的实时进度与安全取消反馈；历史任意 DiskPart/批处理脚本选项保持配置可读但不得执行，发现脚本必须失败关闭。WIM/ESD/SWM 应用和校验贯穿调用方原子取消标记，libwim 可中止当前操作，取消后禁止引擎回退和后续安装阶段；用户显式“仅保存”的主机驱动导出和后续保存必须验证非零结果并失败关闭；默认“自动导入”在 INF 与启动存储要求均为空且清单有效时必须跳过 Direct DISM 导入，并在 ViaPE 配置中显式关闭旧 PE 的驱动导入，任何清单缺失、矛盾、导入或保存失败仍保留暂存备份并停止，开发测试构建仍保持无副作用。
- `正常系统端/src/core/native_install_compat.rs`：Direct 安装的默认无人值守 XML、版本化用户驱动目录、MBR 非零签名生成、同盘 XP active 清理目标筛选及 WinAPI 格式化参数的无副作用验证；不得恢复命令文本生成或解析。普通本地用户名和内置 Administrator 无人值守片段必须复用 `lr-core::unattend_account`，验证发生在 XML 转义和写文件之前；Windows 10/11 渲染必须显式接收已验证国际化设置并完整写入 `Microsoft-Windows-International-Core` 与时区，缺失时失败关闭，不得恢复只对 Server 生效的 `HideLocalAccountScreen`。
- `正常系统端/src/core/native_image_source.rs`：原生安装页的 WIM/ESD/SWM/GHO/ISO 分类、镜像卷读取、只读 ISO 挂载与 XP/2003 文本模式源识别，并只读探测介质根、i386 及 WIM 卷内自带无人值守应答；启动发现只枚举当前存在盘符根目录下精确的 `sources\install.esd` 和 `sources\install.wim`，仅返回全局唯一候选，多个候选保持歧义；开发测试构建拒绝 ISO 挂载。
- `正常系统端/src/core/native_backup_controller.rs`：原生备份输入的 Direct/ViaPE 路由、WIM/ESD/SWM/GHO 任务规划及 PE 交接意图，ViaPE 接收调用方提供的首个可用 PE 而不建模用户选择，兼容盘符和 UNC 绝对备份文件路径并校验所选格式扩展名，不执行备份或重启。
- `正常系统端/src/core/native_backup_executor.rs`：执行已验证备份意图的后台 worker、WIM/ESD/SWM/GHO 分发、PE 缓存复验和交接、强类型进度/取消/真实失败消息；Ghost 使用既有取消标志并把取消引起的后端错误归类为“已取消”而不是“失败”，WIM 系列在没有安全中断 API 时明确提示仍可能运行；Direct 后端成功后通过可注入元数据边界复验输出为非空普通文件，SWM 明确复验首卷；PE 提交前要求安全数据分区、原子暂存配置并在启动项失败时精确回滚，开发测试构建在建线程前拒绝。
- `正常系统端/src/core/native_batch_format.rs`：原生批量格式化的固定卷重新枚举、按 `GetWindowsDirectoryW` 实际身份排除当前运行卷、强类型 `FormatCommandSpec` 计划、共享 VDS 格式化边界和逐卷结果；不得写死 `C:`/`X:`，开发测试构建在进入任何格式化 API 前拒绝。
- `正常系统端/src/core/native_bitlocker_gate.rs`：原生安装与备份共用的 BitLocker 锁定卷纯规划、严格盘符及凭据校验、脱敏凭据和受控解锁边界；开发测试构建在管理器初始化及宿主 I/O 前拒绝。
- `正常系统端/src/core/native_bitlocker_manage.rs`：原生 BitLocker 管理工具的只读加密卷库存、状态允许操作矩阵、严格盘符、密码/恢复密钥复用校验和脱敏强类型意图，并提供 fresh 状态复核后的专用恢复密钥读取与后台操作执行入口；缓存库存不授予执行权限，开发测试构建在管理器初始化及宿主 I/O 前拒绝。
- `正常系统端/src/core/native_boot_repair.rs`：把一键修复引导的单个已检测 Windows 分区转换为既有强类型工具后端请求，执行前要求目标仍在 fresh 检测列表中并固定使用 `BootRepairMode::Auto`，UEFI/Legacy 继续由后端按分区样式自动判定；本模块无副作用。
- `正常系统端/src/core/native_download_controller.rs`：原生在线资源目录状态、分类选择、HTTPS/文件名/保存路径校验、架构 URL 选择、显式下载线程数和下载后动作计划；只有经固定 HTTPS 服务端目录装载并原样选中的历史条目可兼容其 HTTP URL，本地或任意构造目录仍默认拒绝 HTTP，显式兼容开关语义保留。
- `正常系统端/src/core/native_download_executor.rs`：执行已验证下载计划的后台 worker，把计划中的下载线程数显式交给 aria2，并负责进度/取消/完整性校验及显式完成后动作；开发测试构建禁止网络和文件写入。
- `正常系统端/src/core/remote_wim_metadata.rs`：通过精确且有界的 HTTP Range 读取远程 WIM/ESD 的头和 XML，或解析远程 ISO 的 ISO 9660/Joliet 目录及内嵌安装镜像单段/多段 extent；绑定最终重定向地址、Content-Range、实体验证器和 ISO/WIM 资源边界，过滤非安装卷并按实际 Windows 代际补全简易模式目录；不得在探测阶段下载完整镜像或 ISO。
- `正常系统端/src/core/native_driver_transfer.rs`：驱动导出/导入对话框的纯状态、条件目录角色、按桌面当前系统/PE 首个离线系统排序的 Windows 目标库存复核、输入校验和强类型执行/浏览意图；不读取驱动目录、不调用 DISM。
- `正常系统端/src/core/native_tools_controller.rs`：工具箱 21 项的稳定原生动作映射、桌面/PE 可用性、预加载请求、对话框/外部工具路由和安全分类；无损扩大 C 盘使用专属对话框、只读分析预加载与危险存储分类，详细硬件检测使用后台只读快照，仅生成计划。
- `正常系统端/src/core/native_tool_executor.rs`：工具箱原生意图的类型化安全执行边界；只读任务可进入既有读取实现，镜像校验在独立工作线程中实时转发进度并接受外部原子取消标志，修改性与外部工具操作仅在明确确认后生成计划，开发测试构建保持无危险副作用。
- `正常系统端/src/core/native_appx.rs`：原生 APPX 移除安全边界；在线路径使用 PackageManager，离线路径使用 DISM `/Get-ProvisionedAppxPackages` 与 `/Remove-ProvisionedAppxPackage`，两者都在修改前 fresh 枚举、复核选择子集并过滤关键包；命令参数逐项传递并检查退出码和文本错误，严禁删除 WindowsApps 目录，开发测试入口在构造生产后端前拒绝。
- `正常系统端/src/core/native_appx_legacy.rs`：旧 APPX 实现的唯一兼容加载桥，只向原生代码公开固定当前系统的 PackageManager 库存、移除和关键包判断，禁止暴露旧离线目录删除入口。
- `正常系统端/src/core/native_appx_selection.rs`：专属 APPX 对话框的纯目标/包库存、桌面当前系统/PE 首个离线系统默认目标、关键包与重复项过滤、全选/全不选/反选、刷新清空和安全移除请求映射；只产生现有 `native_appx` 边界可复核的强类型意图，不读取目录或执行部署操作。
- `正常系统端/src/core/native_tool_inventory.rs`：原生修改性工具对话框的异步只读库存边界，枚举 Windows 版本/架构、账户、在线 PackageManager APPX、离线 DISM provisioned APPX、NVIDIA 设备、物理磁盘和带版本/架构信息的引导修复目标，按 `SystemDrive` 将桌面当前 Windows 排到系统目标库存首位（PE 中自然保留首个离线系统），提供在线当前系统入口时按忽略大小写及尾部分隔符的规范化盘符去除重复离线项，并保持显示标签与严格内部值分离；开发测试构建在宿主读取前拒绝。
- `正常系统端/src/core/native_tool_backend.rs`：执行已二次确认的原生工具计划，复用既有 Ghost、SpaceSniffer、时间同步、网络重置、NVIDIA 清理以及强类型批量格式化/分区对拷/在线与离线 APPX 安全边界，并返回逐项结果；开发测试构建在任何宿主 I/O 前拒绝。
- `正常系统端/src/core/native_easy_mode_controller.rs`：把已补全远程卷元数据的简易模式配置展平为原生页面状态，目录加载或启用时把首个可安装系统和卷同步为真实控制器选择，并生成保持旧默认安装选项、URL 查询参数安全剥离后的文件名、下载目录与自动安装语义的无副作用意图；下载前后必须按磁盘号、分区号、磁盘容量、分区偏移、精确长度和显示容量重新匹配目标，不能保存列表索引或仅凭盘符续装。

### 正常系统端下载层

- `正常系统端/src/download/mod.rs`：下载模块声明。
- `正常系统端/src/download/aria2.rs`：aria2 生命周期、WebSocket 下载控制、状态和速度进度；RPC 只监听回环地址并为每次进程启动生成高熵随机 secret，WebSocket 客户端的全部调用必须携带该 token，禁止开放跨源无鉴权 RPC；按每个已验证计划的 8/16/32 档位设置 `split`，并把 aria2 的 `max-connection-per-server` 安全限制为 16，不再硬编码单一线程配置。
- `正常系统端/src/download/config.rs`：在线系统、PE、软件、驱动及简易模式配置数据模型和本地配置加载；PE 目录缓存必须通过 `AppConfig` 的字段级原子更新入口保存，不得加载整份配置后再用可能过期的快照覆盖；系统与 PE 目录优先接受 v3 JSON 数组，同时保留 v2 CSV 文本解析兼容。
- `正常系统端/src/download/manager.rs`：下载任务队列和下载管理器状态。
- `正常系统端/src/download/microsoft_catalog.rs`：每次启动或人工刷新时从微软 Update Metadata Service 获取当前 Windows 11 MCT 产品 CAB，并可附加 Windows 10 官方 CAB；验证更新身份、CAB 大小与 SHA-256、官方主机和重定向后，复用 `lr-core::windows_cabinet` 的 `SetupIterateCabinetW` 边界解析唯一 `products.xml`，只发布简中 x64 `CLIENTCONSUMER_RET` 长期 ESD；Windows 11 强制携带官方 SHA-256，Windows 10 22H2 旧目录只提供 SHA-1 时仅验证其格式并保持未声明 SHA-256 状态。
- `正常系统端/src/download/pe_url_resolver.rs`：PE 服务端响应解析、直链解析、连接预热和请求头处理。
- `正常系统端/src/download/server_config.rs`：只通过固定 HTTPS `v3/index.json` 单请求获取 PE、API 系统镜像、软件、简易模式和显卡驱动目录，严格校验 schema、过滤禁用项并映射到既有配置契约；解析 `system_image_mode` 的 1=微软官方、2=API、3=合并语义，缺失时默认为 2，并在启动/人工刷新事务中解析官方目录或执行确定性合并；v3 请求、解析或结构校验失败时整体失败并保留上下文，禁止回退 v1/v2 多文件目录，也禁止把全空或部分吞错响应标记为加载成功。


### 正常系统端工具模块

- `正常系统端/src/utils/mod.rs`：正常端工具模块声明。
- `正常系统端/src/utils/cmd.rs`：隐藏控制台窗口的历史 Command 辅助；新危险命令应优先使用 `lr-core` 类型化边界。
- `正常系统端/src/utils/command.rs`：共享命令边界的兼容再导出。
- `正常系统端/src/utils/encoding.rs`：共享编码转换的兼容再导出。
- `正常系统端/src/utils/i18n.rs`：语言文件扫描、加载、切换、翻译和参数替换；始终列出内置 `zh-CN` 与 `zh-TW`，繁体缺失词条调用共享 NLS 转换，可选外部 `zh-TW.json` 仅作覆盖；同时内嵌 `en-US`、`ja-JP`、`ko-KR`、`fr-FR`、`de-DE` 完整发布词表，外部同名 JSON 只覆盖键且缺失或损坏时回退内嵌表；内置 `ko-KP` 彩蛋只替换经国际化层展示的界面文案，不得改写路径、分区名或安装配置业务值。
- `正常系统端/src/utils/dprk_easter_egg.rs`：正常端 `ko-KP` 彩蛋的可逆桌面副作用边界；从 EXE 内嵌 JPEG 原子发布到 LocalAppData，首次启用前保存当前壁纸路径，切回其他语言后恢复；同时校验并通过 Windows MCI 循环播放 `bin/dprk_easter_egg.mp3`，切换语言或退出窗口时关闭别名并释放文件；PE 端不得调用这些系统副作用，也不得打包该音频。
- `正常系统端/src/utils/logger.rs`：日志目录、滚动保留、格式、最新日志选择和脱敏 JSON 支持包导出。
- `正常系统端/src/utils/path.rs`：exe、bin、用户管理 PE、受管 PE 下载缓存、工具、驱动、只读旧分区脚本兼容目录及临时目录定位；兼容目录不得被新配置暂存或执行。
- `正常系统端/src/utils/privilege.rs`：管理员权限检查和以管理员身份重启；`ShellExecuteW(runas)` 返回值小于等于 32 时必须报告提权失败，只有成功创建提升进程后当前进程才可退出。

### PE 端入口与核心

- `PE端/build.rs`：生成 PE 程序资源、清单、图标和可复现构建版本；日期版本随 PE 源码、清单和 `SOURCE_DATE_EPOCH` 重新求值，避免新二进制复用旧关于页版本；release 禁止测试权限 feature。
- `PE端/src/main.rs`：PE 进程入口、文件日志、panic 记录、语言检测、BitLocker 密钥透传解锁、CLI、工作流模块声明和原生 Win32 窗口启动；PE 日志开头必须同时记录构建日期版本、Cargo 包版本、架构，以及只读固件探测得到的 Secure Boot/PCA 状态或明确的未知与探测错误；在任何卷、BitLocker 或任务标记扫描前枚举包括未绑定 INF 设备在内的当前 PCI 硬件 ID，只对每个唯一匹配且已通过锁定哈希验证的 Intel VMD 包调用一次受控 `drvload.exe`，枚举、选择、包校验或加载失败必须保留诊断并在磁盘扫描前停止；CLI 与 GUI 共用安全暂存路径解析、XML 转义无人值守生成、配置卷标和旧分区脚本拒绝守卫，原版 XP 目录源在格式化前复验并调用共享文本模式引擎；安装临时分区必须先成功删除并把空间扩展回目标卷，才能删除 marker/配置并报告完成或重启，禁止把清理失败降级为警告；Windows 7 x64 UEFI 完成后若 Secure Boot 仍开启，CLI 必须提示关闭并禁止自动重启；安装、备份、扩容 GUI 只路由到原生进度页，不再编译或接受 egui/eframe/OpenGL 兼容回退参数，窗口启动或消息循环失败时必须记录并停止，禁止重复启动 worker；`non-elevated-tests` 独有的 `--ui-progress-preview` 必须在驱动加载、BitLocker、任务发现和 worker 之前进入无副作用预览，release 不得包含该分支。
- `PE端/src/app.rs`：PE 安装主工作流、对用户名等动态值执行 XML 转义的共享无人值守生成和共享 `WorkflowSession`；普通自定义用户名必须在生成无人值守文件前复用 `lr-core::unattend_account` 拒绝 Windows 系统保留身份，内置 Administrator 的启用、RID-500 改名、密码与可选自动登录片段同样复用该模块，日志只能记录“密码已设置”而不能记录值；读取安装配置后必须在写盘前记录目标卷、镜像安全文件名、分卷索引、格式、引导模式、启动签名及可用的 PCA 兼容资源目标 build/架构，兼容资源元数据缺失时必须记为未提供，禁止把零值伪装成 x86，也不得记录密码、恢复密钥或其他敏感配置；Windows 10/11 内置无人值守必须读取已释放镜像的 UI 语言、系统/用户区域、键盘和时区并完整写入 `oobeSystem`，不得使用只对 Server 生效的 `HideLocalAccountScreen` 或已弃用的 SkipOOBE 项；镜像与自定义无人值守文件只能由配置数据目录和经安全文件名校验的相对文件名解析，XP 目录型源必须额外校验会话根和 I386/AMD64 子目录并在格式化前复验关键文件，禁止绝对路径或目录穿越；XP/NT5 身份只能来自已验证配置或镜像元数据，不得根据目标缺少 `Windows\Boot` 猜测，XP UEFI 专用写入失败不得回退成 Legacy 后继续；Windows 7 x64 UEFI 必须在标准引导成功后验证并部署锁定 UefiSeven，完成时若 Secure Boot 仍开启则提示关闭并禁止自动重启；XP 文本模式部署完成后跳过不适用的 WIM 驱动、PCA、BCDBoot 和 Vista+ 无人值守阶段；内置/自定义无人值守生成和安装临时分区回收失败必须进入失败终态，不得继续报告完成或重启；该会话集中持有单一 worker 启动门、消息接收器、进度状态、只读恢复摘要、可查询完成状态并仅在完成后回收的 worker 句柄及持久化工作流观察器，只由原生 Win32 进度页消费且不得由渲染层另行启动工作流；UI 线程的单次消息轮询必须同时受消息数和短时间片约束，持续镜像进度洪峰不得无限清空队列而饿死 16ms 动画时钟；同一轮中的普通百分比和状态文本必须按最新值合并，DISM 同一次样本的百分比与文本必须作为单条原子消息传递，语义步骤和终态边界前必须先提交已有样本，禁止为每一行工具输出分别抢锁和重绘；收到完成/失败消息只代表展示终态，不能在清理、延迟或重启等 worker 尾部退出前关闭进程；消息通道在未收到终态时断开必须合成本地化失败并记录 journal，避免把 worker 崩溃伪装成成功。
- `PE端/src/workflow_journal.rs`：把 PE 安装/备份/扩容消息映射到原子检查点，识别上次中断，并在失败时生成脱敏支持包；成功终态在删除检查点前把内存盘运行日志原子保存为数据分区根目录 `LetRecoveryPE-last.log`，使首次启动存储故障仍可追溯；向原生恢复页只暴露状态、步骤、修订号、中断标记和支持摘要可用性，不暴露保存路径或业务敏感值；诊断日志保存失败只记录警告，不改变已经完成的安装结果。
- `PE端/src/workflows/mod.rs`：PE worker 工作流模块边界和受限再导出。
- `PE端/src/workflows/backup.rs`：PE 备份配置读取、WIM/ESD/SWM/GHO 分发、进度转发、产物验证、引导清理和重启协调。
- `PE端/src/workflows/expand.rs`：PE 无损扩容配置与标记定位、扩容调用、成功/失败共用清理和重启协调。
- `PE端/src/core/mod.rs`：PE 核心模块声明。
- `PE端/src/core/account_fix.rs`：为完整备份、GHO 和 XP/2003 修复离线系统登录账户相关注册表/SAM 状态；安装镜像处于 reseal-to-OOBE 或状态未知时必须跳过 Winlogon 自动登录兜底，避免与 unattend 账户创建竞争。
- `PE端/src/core/bcdedit.rs`：PE 中 ESP 定位挂载、BCD/BCDBoot 修复、活动分区和按目标系统版本选择 PCA 或 Vista/7/8/8.1 标准 UEFI 引导；ESP 枚举与盘符分配必须复用共享 VDS/WinAPI 边界并核对物理磁盘与偏移，已有盘符只复用不移除，无盘符时必须由带预期物理身份的临时守卫覆盖所有返回路径并显式关闭，卸载失败必须阻止继续，不得在多次修复后留下额外盘符，也不得调用 DiskPart。已经移除 UI 授权入口的 `repair_boot.txt` 不再执行，PE 必须始终走内置参数化引导边界。
- `PE端/src/core/config.rs`：读取正常端写入的安装、备份、扩容配置和操作标记，提供旧配置默认值；扩容配置中的供体最终大小缺失时按 0 保持旧版按需收缩语义，非零时由分区移动边界精确执行；内置 Administrator 凭据只作为当前安装会话数据读取，日志和调试输出不得泄露密码；配置扫描覆盖 A-Z 并按 `GetWindowsDirectoryW` 身份排除当前 PE 系统盘，身份探测失败时不得扫描任何卷，暂存文件统一通过安全文件名校验后在数据目录内解析，XP 目录型源仅允许会话根与 I386/AMD64 两级安全分量且拒绝链接或非目录目标。
- `PE端/src/core/disk.rs`：PE 分区枚举、样式判断、格式化、删除、扩容和临时分区回收；当前 PE 卷由共享 `GetWindowsDirectoryW` 边界识别，不得假定为 `X:`。所有写操作复用共享 VDS/WinAPI 边界，分区身份与物理相邻关系由卷句柄 IOCTL 的磁盘号、起始偏移和长度确认，不能依赖本地化工具输出或只比较分区号；格式化必须校验卷标并在操作后复核卷状态。
- `PE端/src/core/dism.rs`：PE 高层镜像信息、释放、捕获和统一 WIM 引擎进度；非增量 WIM/ESD 备份必须捕获到同目录会话唯一暂存文件、完成 libwim 校验和元数据复核后原子替换目标，SWM 必须使用会话唯一暂存目录先捕获并验证临时 WIM 后再分卷，禁止固定 `.tmp.wim`。
- `PE端/src/core/dism_exe.rs`：DISM.exe 参数、子进程输出、进度和错误解析，固定路径均不可用时用 `SearchPathW` 查找可执行文件而不启动 `where.exe`；CAB 目录遍历必须拒绝重解析根、不跟随链接并传播枚举错误，批量包任一失败必须整体返回错误；通过只读 `/Get-Intl` 查询已释放镜像的国际化默认值，当旧 WinPE 无法加载目标镜像的国际化 provider 时，使用碰撞安全的临时 hive 名只读加载目标 SYSTEM/DEFAULT 注册表，严格验证安装语言、系统/用户区域、键盘和时区后供无人值守生成，DISM 与注册表两条路径都失败时必须保留双重错误并失败关闭；批量驱动失败后逐 INF 隔离，未经 `driver_package_trust` 授权的包严禁 `/ForceUnsigned`，授权后的精确单 INF 重试也不得使用 `/Recurse`。
- `PE端/src/core/driver.rs`：共享驱动实现的兼容再导出。
- `PE端/src/core/expand_move.rs`：仅 PE 使用的块级分区移动扩容、几何对齐、阶段日志和恢复信息；支持把目标后的普通数据卷向右搬移后扩目标，也支持先收缩目标左侧紧邻普通数据卷、将目标卷按重叠安全顺序向左搬移并从尾部扩展，从而让分区图边界拖动直接在左右卷之间转移总容量。两侧转移都必须在写入前复核正常端保存的磁盘号/容量以及目标和供体分区号、偏移、长度，重新确认两卷直接相邻且都为 NTFS；右侧成对转移还必须验证目标与供体最终大小总和守恒并精确收缩到指定供体最终大小，禁止优先吞掉供体后方本应保留的未分配空间。缺失身份字段只允许旧的普通右侧扩容兼容路径，新的成对转移必须失败关闭。缩卷、锁定与卸载、原始块复制、删除旧表项、按精确偏移/大小重建、恢复盘符/MBR 活动标记、保留普通 GPT 分区 GUID/attributes/name 和最终扩容必须复用共享 WinAPI 边界并逐阶段复核；特殊/非 NTFS MBR 类型、原始块复制失败或非 1 MiB 对齐时失败关闭。
- `PE端/src/core/ghost.rs`：PE 中 Ghost 镜像备份、还原、进度、取消和错误处理；GHO/GHS 在格式化或备份完成后进入下一阶段前必须通过 Ghost 官方 `-chkimg,<file> -batch` 完整性检查，进程启动失败、非零退出码或文本错误都必须失败关闭，子进程输出按原始字节使用 GBK 兼容解码。
- `PE端/src/core/pca_preflight.rs`：PE 图形和 CLI 安装共用的 PCA 写盘前预检适配，验证正常端暂存的兼容包或安全获取匹配包，并映射本地化失败提示。
- `PE端/src/core/registry.rs`：共享离线注册表实现的兼容再导出。
- `PE端/src/core/system_utils.rs`：PE/离线 Windows 版本、架构、文件版本、临时目录、scratch 和环境检测。

### PE 端 UI 与工具

- `PE端/src/native_ui/mod.rs`：PE 生产环境原生 Win32 UI 的模块边界，汇总局部主题、基础控件、窗口状态、共享窗口壳、长任务进度页、只读详情页和低分辨率/DPI 纯几何层；不得重新引入 egui、eframe、glow 或 OpenGL 回退。
- `PE端/src/native_ui/theme.rs`：PE 端 Inno Setup 6.7 Modern Windows 11 明暗色、统一绿色进度、与正常端主按钮一致的浅色 `#005FB8`/深色 `#4CC2FF` 强调色、DPI 尺寸和 GDI 画刷生命周期；PE 无可靠 Shell 主题服务时默认深色，并允许部署环境显式选择浅色。
- `PE端/src/native_ui/controls.rs`：PE 原生按钮、编辑框、下拉框、列表、复选框、单选框、进度条和分隔线的类型化创建与主题入口，统一使用 Microsoft YaHei UI、紧凑 DPI 尺寸和可测试状态映射，并提供按实际微软雅黑字体测量且受当前视口上限约束的命令按钮宽度；按钮以单次离屏合成实现 normal/hot/pressed/disabled 和不叠加第二焦点框的单边框，`TrackMouseEvent` 失败、取消、隐藏、禁用及销毁时清除热态；Edit 移除 `WS_BORDER`、统一安装 `WS_EX_CLIENTEDGE` 并通过 `SWP_FRAMECHANGED` 生效，完整保留 USER32 原生单行垂直居中和多行文字排版，不叠加手绘边框、圆角或单行强制绘制；ComboBox 闭合字段、独立 ComboLBox 弹层和 ListView 以 5px 逻辑圆角、固定 4 倍超采样及仅回贴四角/1px 外缘的方式绘制，不设置 HRGN、不裁滚动条、箭头或命中区，圆角外部始终回填当前页面背景；ListView 在主题入口显式设置表体、文字背景和文字色，即使空表也不得露出系统白底；下拉弹层与列表项通过所有者/自定义绘制复用主操作按钮 normal/hot 颜色，只有真实选中或当前悬停项获得强调，不得回退到系统蓝色，弹层重复展开或销毁时必须清理悬停状态；抗锯齿缓冲区尺寸使用溢出检查、小尺寸圆角受控件边界约束；进度条按调用方几何和完整胶囊圆角在同一 BGRA 像素表面一次性合成窗口背景、无独立描边的轨道和填充，长任务页保持紧凑的 10px 逻辑高度，轨道边缘只保留抗锯齿覆盖率过渡，填充圆角外部不得用矩形轨道色回贴并产生黑块；加载圈必须逐项复用 Cloud-MGR `ProgressRing` 的 16×16 视口、半径 7、1.5px 线宽、圆头和 2 秒 linear 关键帧：0% 为 0.01/43.97 dash 与 0°，50% 为 21.99/21.99 dash 与 450°，100% 回到 0.01/43.97 dash 且旋转继续到 1080°，伸缩与旋转必须在两段内同时线性推进，不得改成余弦缓动或视觉上先收缩再重新起转，并使用解析覆盖率而非 GDI 折线绘制；完成与失败徽标使用高分辨率下采样和高对比粗线条，确保勾号与叉号清晰，尚未执行的 Pending 状态必须只显示步骤文字而不绘制空心圆或其他占位图元；模块不执行任何安装、备份或磁盘命令。
- `PE端/src/native_ui/state.rs`：PE 原生 UI 的页面、命令栏和工作流类型边界；`NativeWindowState<W>` 分离高级选项、进度、错误、恢复页面导航与宿主持有的 worker/接收器状态，切页不得替换或重启工作流；后端选择和旧渲染器路由已经删除。
- `PE端/src/native_ui/details.rs`：PE P4 高级选项、错误与恢复信息的原生只读详情区；高级选项仅映射已校验 `InstallConfig` 并隐藏路径、凭据和配置内容，错误页只显示本地化摘要而把原始诊断留在日志，恢复页只显示检查点和安全收尾摘要并明确不承诺断点续做；P5 在低高度下按真实可用区收紧标题、说明、列表和备注，列表高度与两列总宽不得反向撑出内容区；不执行或重启工作流。
- `PE端/src/native_ui/layout.rs`：PE P5 不依赖 HWND 的纯窗口几何，计算壳、进度、详情命令栏、monitor work-area 居中和夹取；步骤进度与总体进度必须使用相同的 30px 逻辑行结构，接收调用方按当前语言和实际 9px 逻辑字体测得的共同标签宽度，使右对齐标签、3px 逻辑间距和同宽 10px 逻辑高度胶囊进度条组成的完整可见块水平居中；不显示条内或条外百分比，禁止把总体标签单独放在进度条上方；宽高足够且全部步骤可见时，把标题、两行进度和使用 24px 逻辑行距的完整步骤列表作为一个略向上偏置的整体分配可用高度，不允许固定贴顶或把未使用高度全部堆在列表下方；纯进度页不得绘制步骤列表分隔线，也不得为已删除的底部状态文案或命令栏保留间距，详情页仍保留真实按钮所需命令栏；除 440×430、480×440 外还以低分辨率高 DPI 保守客户端区域覆盖 96–192 DPI，并以 144 DPI 的紧凑默认客户端验证十个安装步骤保持单列、两行标签和进度条严格对齐，保证标题、两组进度、步骤列表和命令栏不相交；只有真实空间不足时才由调用方进入最多两列或只保留总体进度的 compact 展示。
- `PE端/src/native_ui/progress.rs`：PE P3/P4/P5 安装、备份和无损扩容的原生 Win32 长任务页，复用共享 `WorkflowSession` 单次启动 worker，以 50ms 定时器合并既有消息并只更新变化的文字/进度区域；加载圈另由约 16ms 的高精度 waitable timer 独立生产帧（不支持时回退常规 waitable timer 或线程时钟），通过合并的 `WM_APP` 消息保证队列内最多一个待处理帧，只使步骤列表中真实 `InProgress` 行的 16px 状态图元失效，首次布局尚无当前步骤时必须在后续真实步骤转换中重新绑定非空动画矩形并重置该步骤的动画起点；加载圈、两条进度条、状态图元和父窗口底色必须统一经 `BeginPaint`/`EndPaint` 的 `WM_PAINT` 路径写入与无效区同尺寸的兼容内存位图并只 `BitBlt` 发布一次，禁止用 `GetDC` 绕过更新区直接画屏、先清背景再向屏幕补画进度条或在高频步骤切换中同步 `UpdateWindow`；内存位图创建失败时才允许在同一 `WM_PAINT` 内直接绘制回退，且每次只绘制与脏矩形相交的父窗口图元，不得让 16px 动画帧重复生成整条进度图；动态安装/备份步骤保留无分配的静态键，仅在步骤列表实际重绘时本地化，英语模式不得残留中文且轮询不得为翻译反复分配；顶部以 16px 逻辑标题字体显示助手名称，不创建具体步骤名称或“当前步骤”标题，两条进度信息分别以独立 9px 逻辑字体显示“步骤进度:”和“总体进度:”，运行时按当前语言测量共同标签宽度，使标签与进度条的完整可见块水平居中；标签向进度条右对齐，百分比数字完全隐藏，进度变化只重绘胶囊条；步骤文字必须使用 `SS_CENTERIMAGE` 在 24px 逻辑行内垂直居中，使其与同样按行中心定位的 16px 状态图元对齐；完成步骤文字和徽标使用统一绿色，只有真实 `InProgress` 步骤使用当前主题主按钮同款蓝色加载圈与文字，状态切换后必须先发布新语义再异步重绘子 `STATIC`，禁止同步逐控件重画阻塞 UI 线程或只重绘父窗口图标而遗留旧文字颜色；无损扩容的总进度直接跟随既有百分比；纯进度页不绘制步骤列表分隔线，不创建只读状态 Edit、不显示高级选项或客户区关闭按钮，也不创建“操作进行中”“正在完成清理和收尾操作”等底部状态文案或空状态栏，详细诊断写入日志；工作流运行时必须灰显标题栏系统关闭命令并继续拒绝 `WM_CLOSE`，只有终态且 worker 已安全结束后才能恢复，禁止用警示标语代替关闭边界；顶层样式必须移除 `WS_MAXIMIZEBOX`，不得显示或响应最大化按钮；默认窗口使用 480×440 逻辑尺寸和 440×430 逻辑最小尺寸，初始窗口通过当前 HWND 最近显示器的工作区居中，`WM_DPICHANGED` 建议矩形仍夹到同一工作区；窗口类不设置图标且顶层窗口使用无标题栏图标的对话框框架扩展样式；`non-elevated-tests` 的运行态预览必须复用生产加载圈、高精度动画调度、绘制路径和受限 `WorkflowSession` 消息接收路径，并注入不访问磁盘的合成进度洪峰与真实步骤转换，另提供失败状态视觉入口以验收叉号；两者均不启动 worker 或访问磁盘；消息循环异常仍等待同一 worker 真正结束；检查点、完成重启和底层错误语义保持不变。
- `PE端/src/native_ui/window.rs`：PE 原生 Win32 共享窗口壳、标题与说明区、稳定底部命令栏、深浅标题栏及内容绘制、Per-Monitor V2 DPI、微软雅黑 UI、DPI 重排和受显示器工作区约束的 640×440 逻辑最小尺寸，并向生产进度页提供工作区居中/夹取、无标题栏图标的对话框框架扩展样式、标题栏主题和消息清理辅助；窗口类和顶层 HWND 均不得设置应用图标，避免标题文字左侧保留图标槽；保留的壳预览不属于运行入口且不执行安装、备份、扩容或磁盘操作。
- `PE端/src/ui/mod.rs`：PE UI 模块声明。
- `PE端/src/ui/advanced_options.rs`：把高级选项应用到离线系统，包括驱动、CAB、注册表、无人值守和兼容修复；Defender 选项复用共享引擎白名单移除边界且失败关闭；随包存储驱动必须先按 PE 当前 SetupAPI PCI 硬件 ID 选择 Intel VMD 目录，未匹配、包缺失或 AMD/Apple/VirtIO 控制器均不得跨过 DISM 边界。内置 Win7 USB3 必须验证锁定资源并按目标架构与当前硬件 ID 选择子包，内置 NVMe 必须只按固定依赖顺序安装两个微软 CAB；用户明确启用的驱动、CAB、XP 注入和 Win7 兼容包失败必须返回错误并停止安装；CAB 驱动只允许解包后交由 DISM 完整导入，禁止直接复制 SYS/INF/CAT 或猜测并手写服务注册表；临时解包目录必须使用碰撞安全目录且遍历错误不得静默忽略，注册表 hive 中途失败也必须尝试卸载并保留操作与重载错误上下文。
- `PE端/src/ui/progress.rs`：PE 安装/备份步骤及共享 `ProgressState` 纯状态模型，以英语资源完整性测试覆盖全部动态步骤键；安装把镜像释放作为主要总进度区间，备份把镜像捕获作为主要区间，快速前后置步骤只占少量权重；以显式 `has_current_step` 区分准备首帧和真实步骤，无损扩容的总进度直接采用后端百分比；不再包含渲染代码，可见进度统一由 `native_ui/progress.rs` 渲染。
- `PE端/src/utils/mod.rs`：PE 工具模块声明。
- `PE端/src/utils/cmd.rs`：隐藏控制台窗口的历史 Command 创建辅助。
- `PE端/src/utils/command.rs`：共享命令边界的兼容再导出。
- `PE端/src/utils/encoding.rs`：共享编码转换的兼容再导出。
- `PE端/src/utils/i18n.rs`：PE 语言文件扫描、加载、切换、翻译和参数替换；PE EXE 内嵌 `en-US`、`ja-JP`、`ko-KR`、`fr-FR`、`de-DE` 完整发布词表作为缺失键兜底，WIM 中的外部同名文件仍可覆盖同名键；内置 `zh-TW` 即使 WIM 没有外部语言表也通过共享 Windows NLS 完整转换，并允许外部同名词条覆盖；正常端传入 `ko-KP` 时只启用统一朝鲜文彩蛋文案，不执行桌面壁纸副作用。
- `PE端/src/utils/path.rs`：PE exe 和 bin 目录定位。
- `PE端/src/utils/reboot.rs`：共享 `pecmd.exe` 结束逻辑的兼容再导出。

## 维护热点与后续拆分方向

以下文件超过约 1,000 行或承担多个职责。修改时需要额外审阅，但不要一次性重写：

- `lr-core/src/boot_pca.rs`、`driver.rs`、`fveapi.rs`、`wimlib.rs`；
- `正常系统端/src/core/advanced_options.rs`、`core/bitlocker.rs`、`core/disk.rs`、`core/hardware_info.rs`、`core/image_verify.rs`、`core/quick_partition.rs`；
- `正常系统端/src/native_ui/window.rs`、`native_ui/theme.rs`；
- `PE端/src/app.rs`、`ui/advanced_options.rs`。

优先拆分纯解析/策略、状态模型、Windows API 适配、命令执行和 UI 渲染。拆分后必须更新上面的文件职责目录、模块导出和相关测试。

## 面向用户自定义的稳定边界

- 用户可通过 `assets/release/lang/` 增加或修改语言，但语言文件缺失键时必须安全回退。
- 安装前脚本仍只允许从既有受控目录加载；历史 DiskPart/批处理脚本不得再加载或执行，旧配置仅能触发明确的失败关闭提示。不要把任意服务端字符串直接当脚本执行。
- 用户驱动、CAB、无人值守文件和镜像属于不可信输入，必须验证路径、类型和存在性，错误不能影响其他磁盘。
- 正常端无人值守默认状态只依据 `config.json` 中的用户偏好和已选择的源镜像/安装介质；未选择镜像时必须保留配置偏好，目标分区旧系统中的 Panther/Sysprep 文件不得参与判断，源镜像自动探测也不得反向覆盖持久化偏好。
- 自定义下载源仍受 URL、文件名和完整性策略约束，不能通过自定义功能绕过 HTTPS 或已声明哈希。
- 兼容再导出文件虽然很短，但用于保持两端调用接口稳定，不得因“只有几行”随意删除。

最后更新本文件时，应重新统计所有 Rust 文件，并确认职责目录没有遗漏。本文档本身的修改也必须接受 `git diff --check`。
