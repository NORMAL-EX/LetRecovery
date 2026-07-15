# LetRecovery Inno / Windows 11 UI 重构交接记录

> 最后整理：2026-07-16
> 工作分支：`codex/inno-ui-refactor`
> 仓库：`E:\LetRecovery-main\LetRecovery`

本文把本轮长对话中已经确认的目标、不可更改边界、实现事实、视觉验收结论和后续工作集中到一个入口。后续对话应先阅读根目录 `AGENTS.md`，再阅读本文；不要根据早期截图或旧 egui 实现猜测当前行为。

## 1. 当前架构事实

- 正常系统端已经从入口移除 egui，当前运行界面是 `正常系统端/src/native_ui/` 下的原生 Win32 UI。
- 正常系统端继续由 Rust 构建，业务核心仍在原有 Rust 模块及 `lr-core`，没有引入 Delphi、VCL、WebView、React、Tauri、Qt 或 WinUI。
- PE 端已经存在原生 Win32 UI 迁移代码，但仍需单独做完整视觉、低分辨率、DPI、语言和 WIM 集成验收。
- Inno Setup 6.7 仅作为视觉、布局、控件状态和交互规范参考，不复制或编译 Pascal/VCL 源码。
- 固定 Inno 参考版本：`eafc69c06f3b23bdccbf22d3fde83b499ddc4901`。
- 本地参考源码：`.cloudpe-work/references/inno-setup`；该目录永远不提交。

## 2. 绝对不能改变的产品行为

- 不得改变安装、备份、在线下载、扩容、工具箱、PE 启动和恢复工作流。
- 不得删减、替换或擅自简化原有工具箱功能；迁移 UI 时以 Git 历史中的业务状态、可用条件、默认目标和请求结构为准。
- 不得改变 PCA2011/PCA2023、BIOS/UEFI、MBR/GPT、BitLocker、XP/2003、GHO/GHS、WIM/ESD/SWM 逻辑。
- 不得改变现有配置字段、默认值、语言键、错误回退、服务端兼容和安全边界。
- `letrecovery.cloud-pe.cn` API v3 的单请求 JSON 方案必须继续兼容当前服务端真实返回；服务端数据全部视为不可信，URL、文件名和哈希仍须校验。
- 用户可见中文变化必须同步更新 `assets/release/lang/en-US.json`；中英文切换必须整页、整窗一致，不能只翻译标题或按钮。
- 工具箱窗口标题只显示工具名，不追加 ` - LetRecovery`；工具窗口标题栏不显示应用图标，主窗口保留发布图标。
- 正常系统端默认当前在线系统；PE 环境默认第一个离线 Windows。在线系统与相同的离线盘不得重复显示。

## 3. 绝对禁止的开发和测试操作

- 禁止在开发机真实执行格式化、DiskPart 写入、镜像释放/捕获、BCD/ESP 写入、分区调整、注册表注入、密码重置、重启或关机。
- 只允许纯逻辑测试、mock、`DryRunCommandExecutor`、只读枚举和无提权 UI 视觉测试。
- 不得运行会安装系统或修改真实磁盘的测试。
- 不得覆盖、回滚或清理用户已有未提交修改。
- 禁止使用 `git reset --hard`、`git checkout --` 或清理整个工作树。
- 禁止提交 `pkg/`、`target/`、`.cloudpe-work/`、WIM、7z、QA 截图、日志、本地 EXE、临时配置或安装器输出。
- WIM 操作只允许针对 `pkg/bin/pe` 下正式发布 WIM 的临时副本，并必须遵循原子替换、失败保留原件和完整卸载规则。

## 4. 已确认的视觉语言

- 目标是 Inno Setup 6.7 Modern Windows 11 的原生安装器观感：清晰标题区、无装饰主内容区、稳定命令区、克制圆角和可靠明暗模式。
- 保留 LetRecovery 左侧导航；长任务隐藏导航并让内容占满窗口。
- 主内容不使用卡片套卡片、模糊背景、渐变球、营销式大标题或过度留白。
- 中文、英文和数字统一使用 Microsoft YaHei UI；控件宽度和换行必须基于当前字体与 DPI 实测。
- 正常字段和命令按钮基线为 96 DPI 下 23px；DPI 通过统一缩放函数适配 100% 至 200%。
- 圆角通常 4 至 6px，不超过 8px；边框必须完整抗锯齿，不能出现黑角、蓝脚、颗粒、断线或悬浮后退回直角。
- 深色主题的选中导航、引导按钮和 ListView 真实选中行使用用户实测颜色 `#4CC2FF`。
- 按钮必须具备 Inno 对应的 normal / hot / pressed / disabled 状态；鼠标悬浮反馈不得延迟、闪烁或增加第二圈焦点框。
- ListView 保持原生行为，只统一真实选中行颜色和外框；ComboBox/ListBox 的原生弹层行为不得改为迟缓的全量自绘菜单。

## 5. 当前 Edit / ComboBox 实现是锁定边界

### 单行 Edit

- 真实文本、光标、选择、IME、键盘和无障碍始终由原生 Win32 `Edit` 控件负责。
- 不得用静态文本伪造输入框，不得改成多行 Edit，不得向单行 Edit 发送 `EM_SETRECT`/`EM_SETRECTNP`。
- 控件创建保持 `WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL`，不叠加 `WS_BORDER`；自定义外框只负责表面，不得覆盖原生文字。
- 当前文本垂直基线通过字体 `TEXTMETRIC`、窗口客户区和受限的内部留白补偿计算；2026-07-16 用户已确认最后一版“现在总算是可以了”。
- 不得恢复此前造成顶部/底部白线的非客户区漏填实现，也不得重新把 `tmInternalLeading / 2` 全量作为偏移；后者会让文字偏下、底部留白过薄。
- 修改后必须在有文字、有光标、无焦点、深色和浅色至少四种状态截图放大检查；上下可见留白目标差不超过 1px。

### ComboBox

- 闭合选区保持统一字段高度，原生弹出列表、键盘、选择和无障碍语义不变。
- 必须用微软正式 `CB_SETITEMHEIGHT` 接口设置闭合选择区；本实现的 selection-field 组件参数是 `wParam = 1`。
- `GetComboBoxInfo` 只用于读取 USER32 的 `rcItem`、`rcButton` 和弹层句柄，不能用一次较小的系统实测值覆盖项目的 23px@96-DPI 基线。
- `combo_closed_height` 不得低于统一 DPI 基线；此前直接采用较小 measured height 会把下拉框压扁，用户已明确否决。
- 闭合选区绘制必须一次覆盖完整控件，不能只使用 `BeginPaint` 的局部裁剪区，否则悬浮、焦点或展开会漏掉另一半或下边框。
- 弹出列表保持系统原生直角菜单；不要重新引入全局 `CBS_OWNERDRAWFIXED`。

### CheckBox / RadioButton / ListView

- CheckBox 和 RadioButton 使用从当前 Windows 11 BUTTON 主题状态提取并嵌入的多 DPI、明暗状态资源；资源目录是 `正常系统端/assets/win11_button_theme/`，它是构建输入，必须提交。
- 不得恢复粗糙手绘对号、黑色四角、切换后额外焦点圈或 Win10/Win11 随系统版本漂移的样式。
- ListView 外框、表头和滚动条不得在横向滚动后残留重复圆角或重复竖线；滚动后应整控件失效重绘，而不是只画暴露条带。

## 6. 布局和响应式规则

- 所有字段行使用统一 label / tight gap / control gap / section gap；不能按页面手写不同的魔法间距。
- 标签宽度、按钮最小宽度和长文本高度必须使用 Microsoft YaHei UI 实测；中英文切换后重新布局。
- 条件隐藏的控件必须零占位。取消“无人值守”后，其后可见组件应自动前移；未识别镜像卷时不保留镜像卷空行。
- 系统安装第一行的“格式化、添加引导、无人值守、驱动、立即重启”需要统一基线和统一间距；宽窗口不能把“立即重启”贴在驱动下拉框上。
- 系统安装、系统备份、关于页面和工具弹窗的下拉框、编辑框、标签必须按同一行中心线对齐。
- 底部状态文字不得被命令区或窗口边缘裁切；命令按钮与边缘保留稳定间距。
- 工具弹窗按可见内容收紧，不能为了隐藏行保留大片空白；命令按钮间保持 10px 逻辑间距。
- 主窗口必须设置合理最小宽高；低分辨率和 200% DPI 下通过滚动或响应式换行保持可用。

## 7. 已恢复或实现的重要行为

- 系统安装与备份页面使用真实分区库存和原有业务状态；高级选项返回后不得丢失开始按钮可用状态。
- 高级选项中需要路径的复选项若路径为空，返回时自动取消对应选中状态。
- 硬件信息在启动时异步预加载，以结构化行展示；支持保存 TXT 和复制，复制按钮短暂显示“已复制”后平滑恢复。
- 在线下载目录异步加载；HTTP/HTTPS、错误显示、任务取消、完成返回和服务端兼容仍由原有下载层负责。
- 工具箱窗口重复打开不会复活之前已关闭的其他工具。
- 工具目标下拉按在线/离线环境选择默认系统并去重。
- 分区对拷启动即预载，源/目标相互排除同一分区，保留手动刷新重试。
- GHO 密码、镜像校验和文件哈希的浏览按钮与路径 Edit 同行，按钮不关闭窗口且保持统一普通按钮状态。
- 无损扩大 C 盘滑块使用绝对 0.1GB 值映射，拖动同步目标值，避免接近最小值时跳回零；警告区按条件显示。
- API v3 目录按单个 JSON 获取并保留旧服务端数据兼容；CI 的 PE 更新入口只面向 v3 元数据。

## 8. 仍需继续验收/完成

- 完成正常端全部页面的中英文逐项审计，尤其是运行时生成文本、工具箱按钮、硬件字段、下载状态和底部环境状态。
- 完成 PE 端原生 UI 的全部页面迁移与逐项英文化；必须单独编译、注入临时 WIM、验证 Index 1 内部 EXE 后原子替换。
- 用户侧真实环境专项：Windows 10 浅色/深色在线切换、Windows 10 深色库存控件、Win11PE 下拉箭头、100%/125%/150%/200% DPI。
- 正常端整体 DPI、低分辨率、长英文、动态主题切换和长任务无闪烁收尾。
- 在开始新视觉修改前先复现并截图；修改后由开发者先截图放大检查，不能把第一轮视觉 QA 推给用户。

## 9. 构建、交付和 Git 流程

### 正常系统端

```powershell
cargo fmt --all --check
cargo check -p LetRecovery --all-targets --locked
cargo clippy -p LetRecovery --all-targets --locked --features non-elevated-tests -- -D warnings -A clippy::uninlined_format_args
cargo test -p LetRecovery --locked --features non-elevated-tests
cargo build -p LetRecovery --release --locked
git diff --check
```

- release 输出：`target/release/LetRecovery.exe`
- 本地测试替换：`pkg/LetRecovery.exe`
- 替换后报告路径、大小、SHA-256 和修改时间。
- `pkg/LetRecovery.exe` 只用于本地测试，不提交 Git。

### PE 端

```powershell
cargo fmt --all --check
cargo check -p letrecovery-pe --all-targets --locked
cargo clippy -p letrecovery-pe --all-targets --locked --features non-elevated-tests -- -D warnings -A clippy::uninlined_format_args
cargo test -p letrecovery-pe --locked --features non-elevated-tests
cargo build -p letrecovery-pe --release --locked
git diff --check
```

- release 输出：`target/release/LetRecoveryPE.exe`。
- WIM 更新必须走临时副本、挂载、替换、提交、卸载、`/Export-Image` 清理、Index 1 与内部 EXE 验证，再原子替换正式 WIM。
- 失败时保留原 WIM并清理挂载目录，不得改动驱动、语言、PCA2023 或其他文件。

### Git

- 所有工作在 `codex/inno-ui-refactor` 分支进行，不直接提交到 `main`。
- 每个用户确认的清晰阶段单独提交；未经明确要求不 push。本次用户已明确要求提交并 push 最新代码。
- 提交前再次运行 `git status --short`，只暂存已跟踪源码/文档、语言文件和必要的主题构建资源。

## 10. 后续对话开始时的检查顺序

1. 阅读 `AGENTS.md` 和本文。
2. 运行 `git status --short --branch`、`git diff --stat`、`git diff --check`。
3. 记录并保护用户未提交文件。
4. 确认当前分支及远端状态。
5. 复现目标页面，保留修改前截图。
6. 只修改一个清晰区域；业务逻辑与 UI 变更分开审计。
7. 用无提权调试 feature 做视觉测试；不得运行危险 CLI。
8. 修改后自行截图检查，再运行对应测试和 release 构建。
