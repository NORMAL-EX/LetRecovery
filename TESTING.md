# LetRecovery 测试与待办清单（分支 workspace-refactor / PR #13）

> 本文件记录：① 本分支已完成的改动及**真机测试方法**；② 仍待做的重构及计划。
> 你（或测试者）在有真机 / 虚拟机时，按下面「测试方法」逐项打勾验证即可。
>
> 约定：标 ⚠️ 的项目改动了**安装主流程 / 离线注册表 / SAM** 等高风险路径，
> 合并前**必须**真机或虚拟机回归。

---

## 一、已完成改动 —— 需真机验证的测试清单

### 1. 镜像操作从 wimgapi 迁移到 wimlib（apply / capture / info / verify / SWM）⚠️
迁移点：`lr-core/wimlib.rs`、`lr-core/image_meta.rs`、两端 `core/dism.rs`、`正常系统端/core/image_verify.rs`。
两端的 `core/wimgapi.rs` 已**彻底删除**，不再依赖系统 `wimgapi.dll`。

测试方法：
- [ ] **读取信息**：用「镜像校验/选择镜像」打开一个 `install.wim`、一个 `install.esd`、一组 `*.swm`，确认正确列出**卷数、卷名、版本**（对照 `dism /Get-WimInfo`）。
- [ ] **应用/释放**：PE 端把 WIM 第 N 卷释放到某分区，确认释放成功、文件完整、可正常引导。
- [ ] **应用 ESD**：用 ESD（solid/LZMS）镜像，确认能正确解压。
- [ ] **应用 SWM**：传入第一片 `xxx.swm`，确认自动合并其余分卷并成功释放。
- [ ] **备份/捕获**：把一个分区捕获为 WIM（LZX）、ESD（LZMS）、SWM（分卷），确认产物可被 `dism`/`wimlib` 正常打开。
- [ ] **增量/追加**：对已存在的 WIM 追加一个镜像，确认卷数 +1。

### 2. 镜像校验进度修复（不卡 50% / 从 0% 开始 / CPU 占用正常）
代码：`lr-core/wimlib.rs`（`verify_progress_callback`，新增 `VERIFY_STREAMS`=29 分支，修正 `VERIFY_INTEGRITY` 偏移），`正常系统端/core/image_verify.rs`（`done` AtomicBool 让监控线程可靠退出，进度映射 0→5% 准备、5→100% 实际校验）。

测试方法：
- [ ] 校验一个大的正常 WIM/ESD：进度从 **0% 平滑推进到 100%**，不再"一上来 50%"也不再卡 50% 不动。
- [ ] 校验时**CPU 占用正常**（不是几乎 0），说明真在读数据校验。
- [ ] 故意损坏的 WIM（改几字节）→ 校验失败/损坏提示。
- [ ] SWM 多分卷 → 校验通过；缺一片时应报错。

### 3. 「其他用户」/ 装好要密码 —— 账户与登录相关 ⚠️
本类是反馈最多的问题，分三处修复：

**3a. 修复 unattend 模板 `<n>` → `<Name>`（issue #7 根因）**
代码：`正常系统端/main.rs`、`正常系统端/ui/install_progress.rs`、`PE端/main.rs`。
之前 `LocalAccount` 里写成了 `<n>用户名</n>`，Windows 忽略该账户 → OOBE 不创建账户 → 自动登录失败 → 出现"其他用户"。已改为正确的 `<Name>用户名</Name>`。

- [ ] 勾选无人值守 + 自定义用户名装机 → 进系统**自动登录、无需密码**，**不出现"其他用户"**登录界面。
- [ ] 目标系统 `C:\Windows\Panther\unattend.xml` 中 `LocalAccount` 为 `<Name>...</Name>`。

**3b. 移除已弃用的 OOBE 标志**
四套 Win10/11 模板（`PE端/app.rs`、`PE端/main.rs`、`正常系统端/main.rs`、`正常系统端/ui/install_progress.rs`）已去掉 `SkipMachineOOBE` / `SkipUserOOBE`（在 Win11 上不可靠），改用 `HideLocalAccountScreen` / `HideOnlineAccountScreens` / `HideWirelessSetupInOOBE`。

- [ ] Win11 镜像装机 → OOBE 不卡在联机账户/隐私页，直接进桌面。

**3c. 空密码登录兜底（account_fix）**
代码：`PE端/core/account_fix.rs`，接入 PE 端 GUI 与 CLI 两套流程。

- [ ] 还原**整盘备份（未 sysprep）**且账户为**空密码** → 能进入该账户（`LimitBlankPasswordUse=0` 生效）。
- [ ] 检查目标注册表：`HKLM\SYSTEM\ControlSet001\Control\Lsa\LimitBlankPasswordUse` = `0`；设了用户名时 `...\Winlogon\AutoAdminLogon` = `1`。
- [ ] **非空密码**：设了用户名且该账户在备份里带密码 → 已离线清除（见下「3d」）。

**3d. ⚠️ 非空密码离线清除（account_fix::clear_account_password）**
代码：`PE端/core/account_fix.rs`，已接入 `ensure_offline_login`（仅在**指定了用户名**时触发）。
思路（chntpw）：离线把 SAM 中目标账户 V 结构的 NT/LM hash **长度字段清零** = 空密码；并清除 F 结构 `ACB_DISABLED` 位启用账户。
安全：**操作前强制把 SAM 复制为 `SAM.lrbak`**；只覆盖 4 字节长度字段，不改 hive 结构 / 不挪数据；V 解析失败或越界即跳过，绝不写回可疑数据；sysprep 镜像里目标账户不存在 → 无匹配 → 安全空操作。

- [ ] 虚拟机还原"账户带**非空密码**"的备份，且装机时填了该用户名 → 该账户可**空密码登录**且系统正常。
- [ ] 目标盘存在 `SAM.lrbak` 备份；查 `log` 有 `[SAM] 已备份` / `已清除账户` 记录。
- [ ] sysprep 安装镜像 → 日志显示"未找到匹配账户，SAM 未改动"，OOBE 正常建号。
- [ ] 不指定用户名 → 不动 SAM（仅空密码策略 + 跳过自动登录）。

> 诊断："其他用户"取故障机 `C:\Windows\Panther\setupact.log`，搜 `oobeSystem` / `LocalAccount`，看账户创建那步是否执行。

### 4. 自定义无人值守文件 + roxmltree 语法校验
代码：`正常系统端/ui/system_install.rs`、`正常系统端/core/install_config.rs::validate_unattend_xml`（已改用 `roxmltree` 完整解析）、PE 端应用。

测试方法：
- [ ] 选一个**正确**的 unattend.xml → 顶部显示「语法校验通过」，安装按钮可用。
- [ ] 选一个**语法错误**的 xml（缺闭合标签 / 引号未闭合）→ 顶部红色提示，**含行列号**，**安装按钮被禁用**。
- [ ] 选一个根元素不是 `<unattend>` 的 xml → 提示"根元素应为 <unattend>"。
- [ ] 用自定义文件完成安装 → 目标系统 `Panther\unattend.xml` 内容与所选文件一致（不是内置生成的）。

### 5. PE 环境路径不再写死 X 盘
代码：`PE端/app.rs`（字体）、`PE端/core/system_utils.rs`（临时目录）、`PE端/core/disk.rs`、`PE端/core/bcdedit.rs`。

测试方法：
- [ ] 在系统盘符**不是 X:** 的 PE 里启动 PE 端：中文显示正常（非方块），临时文件/引导修复都能在实际盘符上完成。
- [ ] 日志可见「已加载中文字体: ...」指向实际系统盘 Fonts 目录。

### 6. 日志改进
代码：`正常系统端/utils/logger.rs`、`正常系统端/main.rs::log_machine_info`。

测试方法：
- [ ] `{程序目录}\log\` 文件名为 `LetRecovery.YYYY-MM-DD.log`（**以 .log 结尾**），不再是 `....log.YYYY-MM-DD`。
- [ ] 日志开头有「本机配置信息」段（CPU/内存/磁盘/启动模式/安全启动/TPM 等），便于反馈排查。
- [ ] 设置界面文案引导用户"软件问题请提供日志"；已**移除**日志目录/占用显示、日志保留天数设置。

### 7. libwim-15.dll 内置释放 + 全局 init 只执行一次
代码：`lr-core/wimlib_dll.rs`（`include_bytes!` 内置 DLL，运行时释放到 exe 目录）、`lr-core/wimlib.rs`（`std::sync::Once` 单次 `wimlib_global_init`，移除 Drop 里的 cleanup）。

测试方法：
- [ ] 把 exe 拷到**没有 libwim-15.dll** 的干净目录运行 → 启动后目录里自动出现该 DLL，镜像功能正常。
- [ ] PE 里（PE 默认不带 libwim）跑备份/安装 → 不报"找不到 wimlib"。
- [ ] 连续多次「校验→释放→备份」不崩溃（验证移除 Drop cleanup 后多实例安全）。

### 8. fveapi.rs（BitLocker）结构修正
代码：`正常系统端/core/fveapi.rs`，对齐 `normal-ex/fve` 参考实现：`FVE_GET_STATUS_OUTPUT` 的 `dwVersion` 2→8、`dwSize` 0x80→0x78，并修正锁定状态判断与 `unlock` 行为。

测试方法：
- [ ] 对一个 BitLocker **已加密/已锁定**的分区读取状态 → 正确识别"已锁定"。
- [ ] 提供正确恢复密钥/密码解锁 → 解锁成功且能读写。
- [ ] 未加密分区 → 状态正常显示，不误报。

### 9. UI 调整
- [ ] 工具界面**没有**「万能驱动」按钮；网格**无空缺**（「镜像校验」补到原空位）。
- [ ] 系统安装界面：「自定义无人值守文件」在「引导模式选择」**上方**。
- [ ] 「立即重启」选项**默认勾选**。
- [ ] 设置界面与工具界面均**已删除免责声明**。

### 10. 版本号自动按编译日期（含文件版本）
代码：两端 `build.rs`（按编译日生成 `BUILD_VERSION`，并用 `winres::set_version_info` 覆盖 **FIXEDFILEINFO** 的 FILEVERSION/PRODUCTVERSION）、`正常系统端/ui/about.rs` 用 `env!("BUILD_VERSION")`。

测试方法：
- [ ] 右键 exe → 属性 → 详细信息：**文件版本**和**产品版本**都为编译当天日期（如 `2026.6.7.0`），不再停在旧日期。
- [ ] 设置/关于界面显示的版本号与编译日期一致。

### 11. 目录结构整理到 bin/（仅正常系统端）⚠️
代码：`正常系统端/utils/path.rs`（`get_pe_dir`→`bin/pe`、`get_tools_dir`→`bin`、新增 `get_drivers_dir`/`get_uefiseven_dir`）及各调用点。

新布局：根目录只剩 `bin/`、`log/` 两个文件夹 + exe + 运行库 DLL + `config.json`。
`bin/` 下含：6 个核心 exe、`SpaceSniffer.exe`、`Dism/`、`ghost/`、`drivers/`、`uefiseven/`、`pe/`（小写）。

测试方法：
- [ ] 用新布局目录启动：PE 能从 `bin\pe` 找到、不再去根目录找。
- [ ] 工具里启动 SpaceSniffer（在 `bin\` 根）正常。
- [ ] 驱动注入能从 `bin\drivers\{nvme,usb3,storage_controller}` 读到。
- [ ] Win7 UEFI 补丁能从 `bin\uefiseven` 复制 `bootx64.efi`/`UefiSeven.ini`。
- [ ] 旧布局（根目录直接放 PE/）也仍能识别（保留了兜底路径）。

### 12. pe_cache.json 并入 config.json
代码：`正常系统端/core/app_config.rs`（新增 `pe_cache` 字段）、`正常系统端/download/config.rs`（`PeCache::save/load` 走 config.json）。

测试方法：
- [ ] 首次联网拉取 PE 列表后，`config.json` 出现 `pe_cache` 字段；**不再生成 `pe_cache.json`**。
- [ ] 断网重开 → PE 列表能从 `config.json` 的 `pe_cache` 读出。
- [ ] 切换语言/日志开关后 `config.json` 正常回写，`pe_cache` 不丢。

### 13. 离线注册表 load/unload 失败记日志
代码：`lr-core/registry.rs::load_hive/unload_hive`（失败时 `log::warn!`，两端调用点都受益）。

测试方法：
- [ ] 正常装机日志可见「已加载离线注册表配置单元 [pc-sys] <- ...」。
- [ ] 人为制造 hive 加载失败（如占用/路径错）→ 日志出现「加载离线注册表配置单元失败 [...]」，便于定位为何注册表修改未生效。

### 14. cargo workspace + lr-core 共享库
- 仓库为 cargo workspace：`lr-core`（共享库）+ `PE端` + `正常系统端`。
- 已移入 `lr-core` 的共享模块：`wimlib`、`image_meta`、`wimlib_dll`、`command`、`reboot`、`encoding`、`registry`。
- CI 用 `cargo generate-lockfile` 重新生成锁文件后构建整个 workspace（`Cargo.lock` 不入库）。

测试方法：
- [ ] 两端在 GitHub Actions 编译通过。
- [ ] 镜像「读取信息/校验/释放/备份」全部用例重跑（解析器搬到 `lr-core` 后逻辑应等价）。

---

## 二、待做（需配合 / 真机测试）

### A. lr-core 进一步收纳"差异"模块
`dism`、`system_utils`、`bcdedit`、`disk`、`config`、`driver`、`ghost`、`cabinet` 两端同名但**行为有差异**，合并需逐个调和，会改运行时行为，**必须真机回归**。建议分步：先抽字节相同的，再调和其余，每步真机测。

### B. 镜像信息 XML 解析换 roxmltree ✅ 已完成
- `lr-core/image_meta.rs::parse_image_info_from_xml` 已改为 **roxmltree 优先**解析 WIM 的 `<WIM>/<IMAGE>` 块；旧的手写 `.find` 仅作**兜底**（roxmltree 解析失败 / 未解析出镜像时回退），对截断或非常规 XML 仍尽力提取。
- [ ] 回归：单卷/多卷/带 DISPLAYNAME/WINDOWS 块/Win7 老格式 WIM、ESD，确认卷名/版本/类型解析与之前一致。

### C. ⚠️ 统一 PE 两套安装流程（CLI `run_cli_mode` ↔ GUI `execute_install_workflow`）
两套几乎重复、易分叉。完整去重未做。改安装主流程，需真机测两种启动：
- [ ] `LetRecovery.exe /PEINSTALL` 命令行装一遍；
- [ ] GUI 自动检测配置装一遍；两者结果一致。

### D. ⚠️ 非空密码离线清除 ✅ 已实现（见上「3d」，待真机回归）
已实现"指定用户名时离线清空其 NT/LM hash 长度 + 启用账户"，并强制先备份 SAM。
**尚未实现**「凭空创建全新账户」（需在 SAM 里新建 RID + V/F/C 结构，更复杂）；当前依赖目标账户已存在。
- [x] 代码已实现并通过 CI 编译。
- [ ] 真机/虚拟机回归（见「3d」勾选项）。
- [ ] 结构异常的 SAM → 程序放弃且不损坏原 hive（已有 `SAM.lrbak` 备份兜底）。

### E. PE CLI 架构 `amd64` 写死
`PE端/main.rs` 的 `generate_unattend_xml` 把架构写死为 `amd64`，需与版本感知生成统一（ARM64/x86 场景）。

### F. PE 端路径也对齐 bin/ 布局
本次只改了正常系统端。`PE端/ui/advanced_options.rs` 仍从 `exe目录\drivers\{usb3,nvme}` 读取。若 PE 端也按 bin/ 布局分发，需同步（先确认 PE 端实际打包/运行目录）。

### G. 其余小项
- [ ] 继续细化错误吞没路径日志（本次只补了高危的 hive 加载/unattend 写入）。
- [ ] 日志开关/保留天数运行时热重载。
- [ ] `gho_password.rs` GHO 密码读取的可靠性与安全性。
- [ ] 官网 `官网/src/pages/About.tsx` 版本号仍写死 `v2026.2.6`，可改为构建期注入。

---

## 三、构建产物分发提醒
- **目录布局**（正常系统端，新）：
  ```
  LetRecovery\
  ├─ bin\ {bcdedit,bcdboot,bootsect,format.com,aria2c,mountvol}.exe
  │   ├─ SpaceSniffer.exe
  │   ├─ Dism\   (Dism.exe + DismApi.dll + 按需保留的 provider)
  │   ├─ ghost\  (Ghost64.exe + V2iDiskLib.dll + GhostImageFile64.dll)
  │   ├─ drivers\{nvme,usb3,storage_controller}\
  │   ├─ uefiseven\{bootx64.efi,UefiSeven.ini}
  │   └─ pe\LetRecovery_PE.wim
  ├─ log\
  ├─ LetRecovery.exe
  ├─ libwim-15.dll / opengl32.dll / vcruntime140.dll / vcruntime140_1.dll
  └─ config.json   (含 pe_cache 字段)
  ```
- `LetRecovery.exe` 与 `libwim-15.dll` 必须同目录；PE 打包时也带上（或靠内置 DLL 自动释放）。
- 许可证：libwim 为 LGPLv3，动态链接对闭源/商用友好，保留许可证文本并允许用户替换该 DLL 即可。
- DISM 裁剪：只用到 `/Add-Driver` 与 `/Add-Package`，可删 Appx/Assoc/Ffu/Vhd/Imaging/Wim/Msi/Prov/Sysprep/Transmog/Unattend/IBS 等 provider；裁剪后**必须**真机验证一次驱动注入与 KB 补丁安装。
