# 原版 Windows XP / 2003（i386 介质）硬盘文本安装

针对**原版 XP/2003 安装盘**（根目录是 `\I386`、没有 `\sources\install.wim` 的那种）。
这种介质无法像 Win7+ 那样「释放 WIM」，LetRecovery **照搬成熟工具 DSI-安装备份（PECMD
`dsi.WCS`）的 NT5 硬盘安装做法**——即微软 `winnt32 /makelocalsource` 原生的
`$WIN_NT$.~LS` + `$WIN_NT$.~BT` 约定，实现于 `lr-core/src/xp_i386.rs`（`install_from_i386`）。

## 识别

挂载 ISO 后找不到 `\sources\install.wim/esd`，但存在 `\I386\setupldr.bin` 等三件套，
即识别为 XP/2003 i386 文本安装介质（`iso::xp_i386_dir` / `is_valid_i386`），UI 显示绿色
「已识别为 Windows XP/2003 i386 文本安装介质」。

> **架构优先 AMD64**：XP x64 / Server 2003 x64 介质同时含 `\AMD64`（完整 64 位源）与 `\I386`
> （仅 WOW 支持文件、**残缺**、无 `ntfs.sy_` 等引导文件）。`xp_i386_dir` 先认完整源（要求
> `setupldr.bin` + `ntfs.sy_`）、AMD64 优先，避免误选残缺的 `\I386`；32 位介质只有 `\I386` 时正常回落。

## 准备流程（重启前在 PE/当前系统里做）

目标盘 `WIN`（如 `C:`），来源 `i386_src`（如挂载盘 `G:\I386`）：

1. **可写探测（带重试）**：先确认 `WIN\` 此刻真的可建目录。刚格式化完盘符可能短暂
   卸载/重挂，过去会在下一步 `create_dir` 抛裸 `os error 3（系统找不到指定的路径）`；
   现在带 ~5s 重试，过不了就给出可读原因（盘符未挂载 / 非 NTFS / GPT 等）。
2. **本地源**：`xcopy <arch>`（I386/AMD64）→ `WIN\$WIN_NT$.~LS\<arch>`；建空 `$WIN_NT$.~LS\$OEM$`。
   - **XP x64 双拷**（照搬 DSI §四）：当源是 `\AMD64` 时，同级若有 `\I386`（32 位 WoW 组件）一并
     拷到 `LS\I386`——GUI 安装阶段装 WoW64 子系统要用。引导/BT 仍只用 `\AMD64`（完整可引导源）。
3. **`$WIN_NT$.~BT`（BootPath）**：拷 `<arch>\SYSTEM32` 整目录 → `$WIN_NT$.~BT\SYSTEM32`；
   按内嵌清单 `xp_nt5_bootfiles.txt`（照搬 DSI `nt5\NT5.txt`，~150 文件）把 `<arch>\<名>`
   **原样**（压缩名 `.SY_/.DL_/.EX_` 不解压，setupdd 自理）复制进 `$WIN_NT$.~BT\`。源里缺的条目跳过。
4. **根引导文件**：`setupldr.bin` → `WIN\NTLDR`（开机直接进文本安装）；`ntdetect.com` →
   `WIN\NTDETECT.COM`；源里若有 `biosinfo.inf` / `bootfont.bin` 一并落根。
5. **txtsetup.sif**：做文本期驱动集成后，写 `$WIN_NT$.~BT\TXTSETUP.SIF`（setupldr 实际读这份）
   与根 `WIN\TXTSETUP.SIF` 各一份。**不改 `SetupSourcePath`**（靠 `MsDosInitiated=1` +
   `$WIN_NT$.~BT` 约定，setupdd 自会用 `$WIN_NT$.~LS` 作源）。
6. **winnt.sif 应答**（见下）→ `$WIN_NT$.~BT\WINNT.SIF`。
7. **置活动分区**（diskpart `active`）+ **`bootsect /nt52 WIN /mbr /force`** 写 XP 引导码。

重启 → setupldr 据 `$WIN_NT$.~BT` 进入蓝底文本安装 → 复制文件 → 再次重启 → 图形安装。

## 无人值守（winnt.sif）

**强制键（照搬 DSI `NT5部署无人值守`）**：无论内置生成还是用户自定义 `.sif`，落盘前都强制写入
硬盘安装必需的 5 个键——`MsDosInitiated=1`、`Floppyless=1`、`AutoPartition=0`、
`UnattendedInstall=Yes`、`OemPreinstall=Yes`。其中 **`MsDosInitiated=1` 是关键**：声明「从已有
系统/硬盘启动、非光盘引导」，缺它文本安装会去找光盘而失败（DSI 模板里是 `"0"`，部署时被强制改 `1`）。

| 是否放 `bin\xp\productkey.txt` | UnattendMode | 行为 |
|---|---|---|
| 否（默认） | `DefaultHide` | 跳过 EULA/区域/欢迎、忽略驱动签名、管理员空密码+首次自动登录；图形阶段**只在「产品密钥」页停一下**，其余全自动 |
| 是 | `FullUnattended` | 全程不停顿，真·全自动 |

统一项：`UnattendSwitch=Yes`、`OemSkipEula=Yes`、`DriverSigningPolicy=Ignore`（不拦未签名/注入的
存储驱动）、`TargetPath=\WINDOWS`、`FileSystem=LeaveAlone`（沿用已格式化的盘，不动文件系统）。

> 工具**不内置产品密钥**。要全自动就自己在 `bin\xp\productkey.txt` 放一行与所装版本/渠道匹配的密钥。
> 出于安全，文本阶段仍由用户确认「装到哪个分区」（`AutoPartition=0`），不自动选盘以免抹错盘。

## 文本期存储驱动集成（arch-aware）

让文本安装（蓝底）阶段就能认 AHCI/NVMe，实现于 `lr-core/src/xp_textmode_drv.rs`，由
`install_from_i386` 在写 txtsetup.sif 时调用。做法（nLite/WinNTSetup 那一套）：

1. 据源目录名判架构：`\I386`→x86、`\AMD64`→amd64，本地源也拷到 `$WIN_NT$.~LS\<同名>`。
2. 按架构扫描驱动目录：
   - x86（32 位 XP）：`bin\drivers\xp\x86\`
   - amd64（64 位 XP/2003）：`bin\drivers\xp\amd64\` + 随包的 `bin\drivers\xp\ahci`、`nvme`
     （魔改 genahci/stornvme，**x64**）
3. 逐个解析 `.inf`：取服务名、miniport 的 `.sys`、硬件 ID（`PCI\...`）；目录里所有 `.sys`
   （含 storport/ntoskrn8 依赖）拷进源。
4. 合并进 txtsetup.sif：`[SourceDisksFiles]`（缺键才加，避免与原版 storport.sys 重复）、
   `[SCSI.Load]`、`[SCSI]`、`[HardwareIdsDatabase]`。文本引擎据此加载驱动认盘并登记进系统。

用户可自行往 `bin\drivers\xp\<arch>\` 丢驱动（每个驱动一个子目录，含 .inf+.sys），见各目录
README。`.inf` 信息不全的目录会被静默跳过。

> 架构必须匹配：随包的 genahci/stornvme 是 **x64**，只对 64 位 XP/2003（\AMD64）文本安装生效；
> **32 位 XP（i386）需要 32 位驱动**（自行放 `x86\`）。32 位 XP 的现代 NVMe 驱动基本没有，
> 多数 SATA 机器把 BIOS 的 SATA 模式切 IDE/Compatibility 即可免驱动安装。

## 边界 / 已知限制

- **仅 Legacy/BIOS + MBR**。XP 不支持 GPT/UEFI——调用方在发起安装时已拦截 GPT/UEFI 目标。
- 不支持在「正在运行的系统盘」上原地安装，需先进 PE。
- 未替换 `ntoskrnl.exe`（魔改 `ntoskrn8.sys` 仅随驱动拷入，与 WIM 路 `inject_xp_drivers`
  行为一致）。文本期驱动集成的 txtsetup.sif 字段采用 MS 自带 boot 存储驱动（atapi 等）的模板，
  实机若仍认不到盘，请回报日志（含 `[TXTDRV]` 行）以便迭代。
- 现代 NVMe 机器更推荐「已 UEFI 化的 XP x64 WIM 镜像」路径（见 `docs/xp-gpt-uefi.md`）。

## 改动文件

- `lr-core/src/xp_i386.rs`：`install_from_i386` 照搬 DSI 的 `$WIN_NT$.~LS`+`$WIN_NT$.~BT` 法——
  可写探测+重试、建 `$OEM$`、按清单建 `$WIN_NT$.~BT`、强制 `MsDosInitiated=1` 等 5 键、
  `WINNT.SIF`/`TXTSETUP.SIF` 落 `$WIN_NT$.~BT`、源子目录 arch-aware、可选产品密钥/自定义应答。
- `lr-core/src/xp_nt5_bootfiles.txt`：**新增**——`$WIN_NT$.~BT` 引导文件清单（编译期 `include_str!`
  嵌入，照搬 DSI `nt5\NT5.txt`）。
- `lr-core/src/xp_textmode_drv.rs`：**新增**——解析驱动 `.inf` + 合并 txtsetup.sif 的文本期
  存储驱动集成（含单元测试）。
- `正常系统端/src/ui/install_progress.rs`：`format_partition` 修无效 `/Y`（改管道确认）；
  i386 分支把自定义无人值守路径传入引擎。
- `正常系统端/src/ui/system_install.rs` + `core/install_config.rs`：自定义无人值守对 XP 用
  `*.sif` 筛选 + `validate_winnt_sif` 校验。
- `bin/xp/README.txt`（可选产品密钥）、`bin/drivers/xp/{x86,amd64}/README.txt`（用户驱动目录）；
  `.github/workflows/build-and-release.yml`：打包 `bin/xp/` 与整个 `bin/drivers/xp/`。
