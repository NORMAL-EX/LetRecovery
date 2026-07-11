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
- `.github/workflows/`：PR、主分支和发布流水线。

官网 Markdown 由 `官网/plugins/markdown.ts` 在构建期生成 HTML、标题和纯正文 `searchText`；`DocsSearch` 必须同时索引页面标题、描述、标题层级和正文。页头中的搜索组件使用始终渲染的 lazy/Suspense 边界，并通过 `active` 控制宽度和透明度，禁止按 `isDocs` 条件挂载，否则进入/离开文档页的动画会消失。

依赖方向必须保持为：正常系统端和 PE 端可以依赖 `lr-core`，`lr-core` 不得反向依赖任一端。两端出现相同的纯逻辑、命令构建或 Windows API 适配时，应优先迁移到 `lr-core`，端内保留兼容再导出或很薄的环境适配。

## 不可违反的安全规则

### 禁止在开发机和普通 CI 中执行真实破坏性操作

自动测试、代码模型和普通 CI 禁止真实执行以下操作：

- `format`、`format.com`、DiskPart 写操作和卷删除；
- DISM 镜像释放、捕获或离线系统写入；
- BCD、ESP、引导扇区和活动分区修改；
- 分区创建、移动、扩容、写盘或镜像还原；
- 离线注册表注入、SAM 修改、重启或关机。

相关测试必须使用纯函数、`DryRunCommandExecutor`、mock、临时普通文件或显式 preview。必须进行真实验证时，只能使用可丢弃虚拟机、可丢弃 VHDX 和专用测试磁盘，并由人工明确启动。

### 危险命令的实现要求

- 新进程执行优先通过 `lr_core::command::CommandRequest` 和 `CommandExecutor`。
- 程序及参数必须逐项传递，不得用字符串拼接构造 shell 命令。
- 只有确有兼容需求时才能使用 `cmd /c`，并必须严格验证所有可变输入和命令元字符。
- 盘符、磁盘号、分区号、文件系统、卷标、路径、URL 和服务端字段必须在进入系统命令前验证。
- 写操作必须检查进程启动结果、退出码、stderr、工具可能返回的文本错误以及操作后的可观察结果。
- 写操作应 fail-closed；查询或探测失败可返回 `Unknown`、跳过或使用既有安全回退，但不得伪装成成功。
- 临时脚本必须使用碰撞安全的临时文件并保证清理，禁止固定临时文件名。
- 新危险路径必须先把命令构建和结果判断提取成可测试逻辑，再接入真实执行器。

### 磁盘目标与恢复要求

- 执行前保留并复核目标磁盘号、容量、分区信息和可获得的稳定标识，避免扫描后磁盘插拔导致目标变化。
- 不得仅根据 UI 中缓存的盘符执行不可逆操作。
- 多步骤流程必须保留原有回退和清理语义；新增步骤应考虑重复执行、取消、中断和进程崩溃。
- 不能确认安全状态时，应停止并向用户显示简洁错误，同时在日志中保留足够诊断信息。

## 兼容性与配置边界

- 当前服务端入口是代码内固定 HTTPS 地址，普通用户不可在 UI 中修改；不得引入私密凭据或隐藏后门配置。
- PE 元数据必须继续兼容现有 MD5 字段；可选 SHA-256 存在时优先使用 SHA-256。
- 元数据声明校验值后，计算失败或不匹配必须失败关闭。“未声明校验值”和“计算出错”必须是不同状态。
- 自动下载默认只允许 HTTPS。若确需 HTTP，必须通过明确兼容选项启用并显示警告。
- 正常系统端和 PE 端已有配置格式需要向后兼容；新字段应有安全默认值，并为旧配置增加解析测试。
- PCA2011/PCA2023、BIOS/UEFI、MBR/GPT、BitLocker 和 XP/2003 路径都是兼容性边界，不得根据单一新系统环境简化掉旧路径。
- UEFI 安装在固件已撤销 PCA2011 或用户明确选择签名代际时，必须在 DiskPart、格式化等目标盘写操作前验证所选镜像卷内存在有效的对应 EFI 引导文件。无法安全预检的 GHO 等不透明格式必须失败关闭；不得把某台机器 ESP、Insider 构建或未经支持矩阵验证的 `bootmgfw.efi` 当作通用升级文件。
- PCA2023 自动升级使用发布包内固定的 x86/x64 离线资源族，不允许安装时联网下载或回退到其他架构。资源 WIM 只可注入 `Windows\Boot\EFI_EX`、`Windows\Boot\FONTS_EX` 和 `Windows\Boot\EFI\boot.stl`；必须验证包大小、普通文件属性、BootEx 微软签名、PE 架构和正常端到 PE 暂存副本的 SHA-256。缺包或验证失败应在写盘前停止。制作、支持矩阵和回滚流程见 `docs/PCA2023_COMPAT_PACKAGES.md`。
- PCA 选择只适用于 Windows 10/11 和 Server 2016+ 的 UEFI 安装；XP/2003、Vista、Windows 7、Windows 8/8.1、GHO/GHS 和 Legacy 安装不显示该选项。Vista/7/8/8.1 WIM 遇到 Secure Boot 已启用且 PCA2011 已撤销时必须在写盘前停止，不能伪装成可升级 PCA2023；XP/2003 保持既有 NT5 专用路径。当前产品工具链只支持 x86/x64，ARM64 镜像必须在写盘前拒绝。

## 代码组织规则

- UI 负责展示和收集用户意图；可复用业务规则、解析和命令构建放入核心模块。
- Windows API、FFI 和动态 DLL 调用应封装在边界模块中，调用方不要复制 `unsafe` 细节。
- 超过约 1,000 行且职责混杂的文件应渐进拆分，但每次只移动清晰边界，保持公开接口和用户行为。
- 不为“看起来更抽象”增加层级。新抽象必须减少真实重复、隔离危险边界或提高可测试性。
- 错误应保留底层上下文；用户文案简洁，日志详细。不要静默吞掉写操作失败。
- 日志不得写入密码、恢复密钥、访问令牌、完整鉴权 URL 或其他敏感值。
- 公共模块和复杂安全判断需要短而准确的注释，禁止无意义逐行复述。

## 国际化与用户界面

- 新增或修改用户可见中文字符串时，必须同步更新 `assets/release/lang/en-US.json`。
- 两端相同文案应保持键和值语义一致；不允许只更新正常系统端或只更新 PE 端语言文件读取逻辑。
- UI 布局必须适应不同 DPI、分辨率和中英文文本长度，不使用会造成明显空白或裁切的固定宽度。
- 长任务必须保持进度、取消和错误状态稳定，后台任务不得阻塞 egui 渲染线程。
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

`non-elevated-tests` 只允许测试程序以非管理员清单启动，release 构建会拒绝该 feature。读取真实 Windows 安装或宿主磁盘状态的测试必须带原因标记 `#[ignore]`，只能在可丢弃 VM 中手动运行。

修改 `官网/` 时，从该目录运行：

```text
npm ci
npm run lint
npm run type-check
npm run build
```

提交前还必须：

- 运行 `git diff --check`；
- 检查 `git status --short`，不得误提交 `pkg/`、本地 7z、`.cloudpe-work/`、构建输出或用户文件；
- 记录未运行测试和真实环境阻塞，不得把“无法执行”写成“测试通过”；
- 第三方 DLL 变化时同步更新 `docs/THIRD_PARTY_BINARIES.md`、许可证和 SHA-256；
- 不覆盖、不回滚用户未提交的修改。

PCA2023 离线资源必须从已维护的微软官方介质或动态更新包制作：可用 `.github/scripts/build-pca2023-pack.ps1` 从已维护的 `boot.wim` 提取，也可用 `lr-core/examples/build_pca2023_resource_pack.rs` 验证并捕获已经安全展开的固定白名单资源目录。两条路径都必须按 `docs/PCA2023_COMPAT_PACKAGES.md` 和 `docs/PCA2023_RESOURCES.lock.json` 记录来源、源哈希、关键文件与最终发布 WIM，并完成虚拟机矩阵。正式 release 会检查三份资源并把它们同时放入桌面端和 PE WIM；不得为了让流水线通过而使用空 WIM、某台机器的 ESP 备份或未经验证的 Insider 文件。

## 常见扩展应该修改的位置

### 新增安装高级选项

通常需要同时检查：

- `正常系统端/src/ui/advanced_options.rs`：输入、默认状态和桌面端交互；
- `正常系统端/src/core/install_config.rs`：传递给 PE 的配置结构和向后兼容默认值；
- `正常系统端/src/ui/install_progress.rs`：正常端直接安装路径；
- `PE端/src/core/config.rs`：PE 配置解析；
- `PE端/src/ui/advanced_options.rs` 或 `PE端/src/app.rs`：离线应用；
- `assets/release/lang/en-US.json`：英文翻译；
- 本文档：职责或文件变化说明。

### 新增工具箱功能

通常在 `正常系统端/src/ui/tools/types.rs` 定义状态，在 `tools/mod.rs` 接入入口，在独立逻辑文件实现业务，在 `tools/dialogs/` 放渲染。危险操作必须下沉到可测试核心边界，不能直接在按钮回调中拼命令。

### 新增在线下载类型或服务端字段

检查 `download/config.rs`、`download/server_config.rs`、下载管理器、缓存完整性、HTTPS 策略、旧配置默认值和 UI 展示。服务端输入一律视为不可信。

### 新增镜像引擎或第三方二进制

优先扩展 `lr-core/src/wim_engine.rs` 的统一入口，保留现有回退；记录来源、版本、许可证、SHA-256 和打包路径，不在调用方直接加载未验证 DLL。

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
- `lr-core/src/bl_passthrough.rs`：序列化和解析 BitLocker 恢复密钥透传文件；负责去重、注释和空项兼容。
- `lr-core/src/boot.rs`：共享 XP 引导写入及可编辑修复引导脚本执行。
- `lr-core/src/boot_pca.rs`：PCA2011/PCA2023 签名与 EFI 架构识别、固件信任评估、ESP 临时挂载、模式决策、BCDBoot 调用和完整 BootEx 兼容回退。
- `lr-core/src/cached_artifact.rs`：缓存文件的安全查找、常规文件约束、元数据解析和完整性验证状态。
- `lr-core/src/command.rs`：类型化进程请求、执行结果、系统/dry-run 执行器，以及把进程启动失败转换为共享操作错误的入口。
- `lr-core/src/diskpart.rs`：DiskPart 临时脚本生命周期、独立参数执行、类型化校验、中英文文本错误判断和目录脚本运行。
- `lr-core/src/download_integrity.rs`：MD5/SHA-256 选择策略、哈希验证、HTTPS/HTTP URL 策略和下载文件名验证。
- `lr-core/src/driver.rs`：SetupAPI 驱动枚举、导出、导入及离线驱动安装的共享 Windows 实现。
- `lr-core/src/encoding.rs`：Windows GBK 与 UTF-8 转换。
- `lr-core/src/format_command.rs`：`format.com` 直接调用的盘符、文件系统、卷标验证，命令构建、preview 和输出判断。
- `lr-core/src/fveapi.rs`：动态加载 FVEAPI 的 BitLocker 卷访问、状态、解锁和恢复密钥格式处理。
- `lr-core/src/hash.rs`：流式 SHA-256、兼容 MD5、进度回调、规范化和比对。
- `lr-core/src/image_meta.rs`：不依赖 DLL 的 WIM XML 元数据解析、镜像名称整理和镜像类型判断。
- `lr-core/src/operation/mod.rs`：多步骤操作基础设施的公开导出和统一毫秒时间戳。
- `lr-core/src/operation/checkpoint.rs`：安装、备份、扩容等操作的严格状态机、步骤顺序、目标指纹、原子 JSON 检查点和事务式 journal。
- `lr-core/src/operation/error.rs`：可序列化的统一错误类别、错误码、用户/日志消息和显式可重试属性。
- `lr-core/src/operation/retry.rs`：区分幂等/非幂等的有界重试、退避策略、可注入 sleeper 和纯单元测试。
- `lr-core/src/operation/support.rs`：自包含 JSON 支持包、操作摘要、日志尾部限制、文件名隔离和凭据/恢复密钥脱敏。
- `lr-core/src/pca_compat.rs`：按目标 WIM 架构和启动资源族选择内置 PCA2023 离线 WIM，执行大小、SHA-256、签名、架构校验、安全暂存及 EFI_EX/FONTS_EX/boot.stl 白名单注入。
- `lr-core/src/pca_preflight.rs`：PCA2011/PCA2023 写盘前只读策略，检查受支持系统版本和 x86/x64 架构，提取并验证所选 WIM/ESD/SWM 卷的 EFI 引导源，对不可预检镜像失败关闭。
- `lr-core/src/reboot.rs`：结束 PE 的 `pecmd.exe`；名字为历史兼容，模块本身不执行系统重启。
- `lr-core/src/registry.rs`：通过 `reg.exe` 管理离线注册表配置单元和值。
- `lr-core/src/sam.rs`：离线 SAM 账户枚举、清空密码和启用账户，包含二进制结构解析。
- `lr-core/src/scoped_temp_file.rs`：碰撞安全临时普通文件、名称验证和 Drop 清理。
- `lr-core/src/wimgapi.rs`：动态封装 Windows WIMGAPI 的镜像 apply、capture、元数据和进度回调。
- `lr-core/src/wimlib.rs`：动态封装 `libwim-15.dll` 的打开、校验、释放、捕获、拆分和进度回调。
- `lr-core/src/wimlib_dll.rs`：确保内嵌 `libwim-15.dll` 在运行目录可用的兜底逻辑。
- `lr-core/src/wim_engine.rs`：wimlib/WIMGAPI 运行时选择、统一调用和失败回退。
- `lr-core/src/xp.rs`：XP/2003 x64 的 GPT+UEFI 驱动注入、服务注册和引导文件准备。
- `lr-core/src/xp_i386.rs`：XP/2003 Legacy/MBR 文本模式硬盘安装、NT5 文件准备、应答文件和活动分区设置。
- `lr-core/src/xp_textmode_drv.rs`：解析存储驱动 INF，并把 AHCI/NVMe 驱动集成到 XP 文本安装阶段。
- `lr-core/examples/build_pca2023_resource_pack.rs`：验证固定白名单中的微软 BootEx 资源签名、架构和字体集合，并通过内置 wimlib 从普通目录生成和复验离线资源 WIM；不挂载或维护系统镜像。

### 正常系统端入口与核心

- `正常系统端/build.rs`：生成 Windows 资源、程序清单、图标和可复现的构建日期/版本信息；release 禁止测试权限 feature。
- `正常系统端/src/main.rs`：桌面端进程入口、权限与依赖检查、配置预加载、CLI 分派、PE 安装/备份入口和窗口启动。
- `正常系统端/src/app.rs`：主应用状态、页面路由、安装/备份选项、异步信息加载和顶层 egui 渲染。
- `正常系统端/src/core/mod.rs`：正常端核心模块声明。
- `正常系统端/src/core/app_config.rs`：`config.json` 用户偏好、语言、日志、外观和默认选项的读取保存。
- `正常系统端/src/core/bcdedit.rs`：定位和挂载 ESP、BCD/BCDBoot 修复、活动分区处理及 PCA 引导源选择。
- `正常系统端/src/core/bitlocker.rs`：BitLocker 卷枚举、状态解析、解锁、暂停/恢复保护、解密及恢复密钥处理。
- `正常系统端/src/core/cabinet.rs`：通过 SetupAPI 解压 CAB、递归发现 CAB 文件。
- `正常系统端/src/core/cli_install.rs`：解析命令行无人值守安装配置并启动与 GUI 相同的安装入口。
- `正常系统端/src/core/disk.rs`：分区枚举、样式和磁盘关系查询、缩小/创建/删除恢复分区及 DiskPart 安全调用。
- `正常系统端/src/core/dism.rs`：正常端高层镜像查询、释放、捕获和进度模型，完整透传版本、Build、架构元数据并接入统一 WIM 引擎。
- `正常系统端/src/core/dismapi.rs`：动态加载 DISM API，枚举及导出离线驱动。
- `正常系统端/src/core/dism_cmd.rs`：DISM.exe 参数封装、进度解析、离线驱动和更新包操作。
- `正常系统端/src/core/driver.rs`：共享驱动实现的兼容再导出，以及 DISM 优先的离线驱动导入策略。
- `正常系统端/src/core/ghost.rs`：Ghost 镜像信息、备份、还原、进度、取消和错误分类。
- `正常系统端/src/core/gho_password.rs`：读取和解码多种 GHO 头部中的密码信息。
- `正常系统端/src/core/hardware_info.rs`：使用 WinAPI/WMI 收集 CPU、内存、主板、BIOS、磁盘、GPU、网络、电池和系统信息。
- `正常系统端/src/core/hardware_info/names.rs`：硬件厂商和 GPU 名称的纯规范化及占位符识别。
- `正常系统端/src/core/image_verify.rs`：识别 WIM/ESD/SWM/GHO 等镜像并执行校验、进度和结果汇总。
- `正常系统端/src/core/install_config.rs`：正常端到 PE 的安装、备份、扩容配置，安装标记、资源暂存和无人值守文件验证。
- `正常系统端/src/core/iso.rs`：通过 Windows Virtual Disk/COM 能力挂载、查找和卸载 ISO。
- `正常系统端/src/core/nvidia_driver.rs`：GPU 枚举、厂商识别、NVIDIA 驱动设备和软件清理支持。
- `正常系统端/src/core/pe.rs`：PE 文件准备、缓存/下载使用、启动项安装和进入 PE 的流程协调。
- `正常系统端/src/core/pca_preflight.rs`：正常端 PCA 写盘前预检适配、固件读取、匹配兼容包准备及共享错误到本地化用户提示的映射。
- `正常系统端/src/core/quick_partition.rs`：物理磁盘枚举、分区布局计算、活动分区识别和一键分区执行。
- `正常系统端/src/core/registry.rs`：共享离线注册表实现的兼容再导出。
- `正常系统端/src/core/system_info.rs`：当前机器启动模式、Secure Boot、TPM 和环境摘要。
- `正常系统端/src/core/system_utils.rs`：PE 文件架构、离线 Windows 版本/架构、权限和系统路径等通用 Windows 工具。

### 正常系统端下载层

- `正常系统端/src/download/mod.rs`：下载模块声明。
- `正常系统端/src/download/aria2.rs`：aria2 生命周期、WebSocket 下载控制、状态和速度进度。
- `正常系统端/src/download/config.rs`：在线系统、PE、软件、驱动及简易模式配置数据模型和本地配置加载。
- `正常系统端/src/download/manager.rs`：下载任务队列和下载管理器状态。
- `正常系统端/src/download/pe_url_resolver.rs`：PE 服务端响应解析、直链解析、连接预热和请求头处理。
- `正常系统端/src/download/server_config.rs`：从固定服务端获取和解析远程配置、相对 URL 解析及配置缓存。

### 正常系统端主要 UI

- `正常系统端/src/ui/mod.rs`：正常端 UI 模块声明。
- `正常系统端/src/ui/about.rs`：关于页面、版本、项目链接、许可信息和支持包导出入口渲染。
- `正常系统端/src/ui/advanced_options.rs`：安装高级选项状态、校验、对话框和硬件相关默认建议。
- `正常系统端/src/ui/download_progress.rs`：下载任务进度、完整性校验状态、取消和完成后的动作。
- `正常系统端/src/ui/easy_mode.rs`：简易模式配置、Logo 加载、系统选择和一键安装界面。
- `正常系统端/src/ui/embedded_assets.rs`：内嵌 SVG/Logo 类型、缓存、缩放和像素渲染。
- `正常系统端/src/ui/hardware_info.rs`：硬件信息页面的分组、刷新和展示。
- `正常系统端/src/ui/install_progress.rs`：正常端直接安装工作流、格式化 fallback、目标解析、镜像释放、驱动、引导和高级选项进度。
- `正常系统端/src/ui/online_download.rs`：在线镜像/PE/软件列表、图标异步加载和下载入口。
- `正常系统端/src/ui/pe_preparation.rs`：PE 缓存查找、完整性验证和可用路径的 fail-closed 准备。
- `正常系统端/src/ui/progress.rs`：正常端安装/备份步骤枚举、状态模型和通用进度 UI。
- `正常系统端/src/ui/system_backup.rs`：备份页面输入、目标选择、格式选项及备份任务启动。
- `正常系统端/src/ui/system_install.rs`：系统安装页面、ISO/镜像选择、卷索引、模式选择、仅现代 x86/x64 UEFI 可见的 PCA 选项、架构阻断和安装确认。

### 正常系统端工具箱逻辑

- `正常系统端/src/ui/tools/mod.rs`：工具箱状态、工具入口和对话框调度。
- `正常系统端/src/ui/tools/types.rs`：工具箱共享状态和结果类型。
- `正常系统端/src/ui/tools/actions.rs`：外部工具启动、引导修复和驱动导出的命令入口。
- `正常系统端/src/ui/tools/appx.rs`：在线/离线 APPX 枚举、友好名称解析和批量移除。
- `正常系统端/src/ui/tools/batch_format.rs`：可格式化分区枚举、严格参数验证、批量格式化和结果汇总。
- `正常系统端/src/ui/tools/bitlocker.rs`：工具箱使用的 BitLocker 类型兼容导出。
- `正常系统端/src/ui/tools/driver.rs`：在线/离线驱动导出、导入和存储控制器驱动目录处理。
- `正常系统端/src/ui/tools/expand_c.rs`：无损扩大 C 盘信息加载、状态和操作触发。
- `正常系统端/src/ui/tools/gho_password.rs`：GHO 密码查看工具的状态和交互。
- `正常系统端/src/ui/tools/hash_verify.rs`：文件 SHA-256 计算、输入比对、进度和结果。
- `正常系统端/src/ui/tools/image_verify.rs`：镜像校验工具的异步任务和结果展示状态。
- `正常系统端/src/ui/tools/network.rs`：网络适配器详情和网络重置操作。
- `正常系统端/src/ui/tools/nvidia_uninstall.rs`：NVIDIA 驱动卸载目标选择、确认和结果状态。
- `正常系统端/src/ui/tools/partition_copy.rs`：分区对拷、恢复标记、断点信息、文件清单和复制进度。
- `正常系统端/src/ui/tools/password_reset.rs`：当前/离线 Windows 账户枚举、清空密码和启用账户。
- `正常系统端/src/ui/tools/quick_partition.rs`：一键分区编辑器、布局预览、确认和执行状态。
- `正常系统端/src/ui/tools/software.rs`：已安装软件枚举、列表截断和导出。
- `正常系统端/src/ui/tools/time_sync.rs`：NTP 查询、北京时间换算、系统时间设置和结果状态。
- `正常系统端/src/ui/tools/version_detect.rs`：离线注册表和文件回退的 Windows 版本/构建识别。

### 正常系统端工具箱对话框

- `正常系统端/src/ui/tools/dialogs/mod.rs`：工具箱对话框模块声明。
- `正常系统端/src/ui/tools/dialogs/common.rs`：分区显示文本和消息颜色等对话框共享渲染辅助。
- `正常系统端/src/ui/tools/dialogs/appx.rs`：APPX 列表、筛选、选择和卸载对话框。
- `正常系统端/src/ui/tools/dialogs/backup_bitlocker.rs`：备份前 BitLocker 状态、密钥和处理确认。
- `正常系统端/src/ui/tools/dialogs/batch_format.rs`：批量格式化分区选择、卷标/文件系统输入、二次确认和结果。
- `正常系统端/src/ui/tools/dialogs/bitlocker_manage.rs`：BitLocker 卷管理、解锁、保护和解密交互。
- `正常系统端/src/ui/tools/dialogs/driver_backup.rs`：驱动备份/恢复来源、目标和进度对话框。
- `正常系统端/src/ui/tools/dialogs/install_bitlocker.rs`：安装目标 BitLocker 解锁和风险提示。
- `正常系统端/src/ui/tools/dialogs/network.rs`：网络信息和重置确认对话框。
- `正常系统端/src/ui/tools/dialogs/partition_copy.rs`：源/目标分区、覆盖确认、复制和恢复进度对话框。
- `正常系统端/src/ui/tools/dialogs/repair_boot.rs`：目标系统选择和引导修复结果对话框。
- `正常系统端/src/ui/tools/dialogs/software.rs`：软件列表查看和导出对话框。
- `正常系统端/src/ui/tools/dialogs/storage_driver.rs`：存储控制器驱动目录检查和导入对话框。
- `正常系统端/src/ui/tools/dialogs/time_sync.rs`：当前时间、同步操作和结果对话框。

### 正常系统端工具模块

- `正常系统端/src/utils/mod.rs`：正常端工具模块声明。
- `正常系统端/src/utils/cmd.rs`：隐藏控制台窗口的历史 Command 辅助；新危险命令应优先使用 `lr-core` 类型化边界。
- `正常系统端/src/utils/command.rs`：共享命令边界的兼容再导出。
- `正常系统端/src/utils/encoding.rs`：共享编码转换的兼容再导出。
- `正常系统端/src/utils/i18n.rs`：语言文件扫描、加载、切换、翻译和参数替换。
- `正常系统端/src/utils/logger.rs`：日志目录、滚动保留、格式、最新日志选择和脱敏 JSON 支持包导出。
- `正常系统端/src/utils/path.rs`：exe、bin、PE、工具、驱动、DiskPart 脚本及临时目录定位。
- `正常系统端/src/utils/privilege.rs`：管理员权限检查和以管理员身份重启。

### PE 端入口与核心

- `PE端/build.rs`：生成 PE 程序资源、清单、图标和可复现构建版本；release 禁止测试权限 feature。
- `PE端/src/main.rs`：PE 进程入口、文件日志、panic 记录、语言检测、BitLocker 密钥透传解锁、CLI、工作流模块声明和窗口启动。
- `PE端/src/app.rs`：PE 顶层 egui 状态、worker 消息通道、安装主工作流、无人值守生成及持久化工作流观察器接入。
- `PE端/src/workflow_journal.rs`：把 PE 安装/备份/扩容消息映射到原子检查点，识别上次中断，并在失败时生成脱敏支持包；记录失败不阻断原流程。
- `PE端/src/workflows/mod.rs`：PE worker 工作流模块边界和受限再导出。
- `PE端/src/workflows/backup.rs`：PE 备份配置读取、WIM/ESD/SWM/GHO 分发、进度转发、产物验证、引导清理和重启协调。
- `PE端/src/workflows/expand.rs`：PE 无损扩容配置与标记定位、扩容调用、成功/失败共用清理和重启协调。
- `PE端/src/core/mod.rs`：PE 核心模块声明。
- `PE端/src/core/account_fix.rs`：修复离线系统登录账户相关注册表状态。
- `PE端/src/core/bcdedit.rs`：PE 中 ESP 定位挂载、BCD/BCDBoot 修复、活动分区和 PCA 引导选择。
- `PE端/src/core/cabinet.rs`：PE 中通过 SetupAPI 解压和发现 CAB 文件。
- `PE端/src/core/config.rs`：读取正常端写入的安装、备份、扩容配置和操作标记，提供旧配置默认值。
- `PE端/src/core/disk.rs`：PE 分区枚举、样式判断、格式化 fallback、删除、扩容、清理和 DiskPart 安全执行。
- `PE端/src/core/dism.rs`：PE 高层镜像信息、释放、捕获和统一 WIM 引擎进度。
- `PE端/src/core/dism_exe.rs`：DISM.exe 参数、子进程输出、进度和错误解析。
- `PE端/src/core/driver.rs`：共享驱动实现的兼容再导出。
- `PE端/src/core/expand_move.rs`：仅 PE 使用的块级分区移动扩容、几何对齐、阶段日志和恢复信息。
- `PE端/src/core/ghost.rs`：PE 中 Ghost 镜像备份、还原、进度、取消和错误处理。
- `PE端/src/core/pca_preflight.rs`：PE 图形和 CLI 安装共用的 PCA 写盘前预检适配，验证正常端暂存的兼容包或安全获取匹配包，并映射本地化失败提示。
- `PE端/src/core/registry.rs`：共享离线注册表实现的兼容再导出。
- `PE端/src/core/system_utils.rs`：PE/离线 Windows 版本、架构、文件版本、临时目录、scratch 和环境检测。

### PE 端 UI 与工具

- `PE端/src/ui/mod.rs`：PE UI 模块声明。
- `PE端/src/ui/advanced_options.rs`：把高级选项应用到离线系统，包括驱动、CAB、注册表、无人值守和兼容修复。
- `PE端/src/ui/progress.rs`：PE 安装/备份步骤、状态模型、响应式进度条、步骤列表和错误展示。
- `PE端/src/utils/mod.rs`：PE 工具模块声明。
- `PE端/src/utils/cmd.rs`：隐藏控制台窗口的历史 Command 创建辅助。
- `PE端/src/utils/command.rs`：共享命令边界的兼容再导出。
- `PE端/src/utils/encoding.rs`：共享编码转换的兼容再导出。
- `PE端/src/utils/i18n.rs`：PE 语言文件扫描、加载、切换、翻译和参数替换。
- `PE端/src/utils/path.rs`：PE exe 和 bin 目录定位。
- `PE端/src/utils/reboot.rs`：共享 `pecmd.exe` 结束逻辑的兼容再导出。

## 维护热点与后续拆分方向

以下文件超过约 1,000 行或承担多个职责。修改时需要额外审阅，但不要一次性重写：

- `lr-core/src/boot_pca.rs`、`driver.rs`、`fveapi.rs`、`wimlib.rs`；
- `正常系统端/src/app.rs`、`core/bitlocker.rs`、`core/disk.rs`、`core/hardware_info.rs`、`core/image_verify.rs`、`core/quick_partition.rs`；
- `正常系统端/src/ui/advanced_options.rs`、`ui/install_progress.rs`、`ui/system_install.rs`、`ui/tools/quick_partition.rs`；
- `PE端/src/app.rs`、`ui/advanced_options.rs`。

优先拆分纯解析/策略、状态模型、Windows API 适配、命令执行和 UI 渲染。拆分后必须更新上面的文件职责目录、模块导出和相关测试。

## 面向用户自定义的稳定边界

- 用户可通过 `assets/release/lang/` 增加或修改语言，但语言文件缺失键时必须安全回退。
- 安装前脚本和 DiskPart 脚本只允许从既有受控目录加载；不要把任意服务端字符串直接当脚本执行。
- 用户驱动、CAB、无人值守文件和镜像属于不可信输入，必须验证路径、类型和存在性，错误不能影响其他磁盘。
- 自定义下载源仍受 URL、文件名和完整性策略约束，不能通过自定义功能绕过 HTTPS 或已声明哈希。
- 兼容再导出文件虽然很短，但用于保持两端调用接口稳定，不得因“只有几行”随意删除。

最后更新本文件时，应重新统计所有 Rust 文件，并确认职责目录没有遗漏。本文档本身的修改也必须接受 `git diff --check`。
