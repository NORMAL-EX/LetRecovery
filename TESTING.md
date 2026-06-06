# LetRecovery 测试与待办清单（分支 migrate-wimlib / PR #12）

> 本文件记录：① 本分支已完成的改动及**真机测试方法**；② 仍待做的重构及计划。
> 你现在无法测试，等有真机/虚拟机时，按下面「测试方法」逐项验证即可。

---

## 一、已完成改动 —— 需真机验证的测试清单

### 1. 镜像操作从 wimgapi 迁移到 wimlib（apply / capture / info / verify / SWM）
迁移点：`core/dism.rs`、`core/wimlib.rs`、`core/image_verify.rs`（两端）。

**前置条件**：运行目录（exe 同级）必须有 `libwim-15.dll`（已由 build.rs 自动复制到 target 目录；分发时也要带上）。

测试方法：
- [ ] **读取信息**：用「镜像校验/选择镜像」功能打开一个 `install.wim`、一个 `install.esd`、一组 `*.swm`，确认能正确列出**卷数、卷名、版本**（对照 `dism /Get-WimInfo`）。
- [ ] **应用/释放**：在 PE 端把 WIM 第 N 卷释放到某分区，确认释放成功、文件完整、可正常引导。
- [ ] **应用 ESD**：同上，用 ESD（solid/LZMS）镜像，确认能正确解压。
- [ ] **应用 SWM**：传入第一片 `xxx.swm`，确认自动合并其余分卷并成功释放。
- [ ] **备份/捕获**：把一个分区捕获为 WIM（LZX）、ESD（LZMS）、SWM（分卷），确认产物可被 `dism`/`wimlib` 正常打开。
- [ ] **增量/追加**：对已存在的 WIM 追加一个镜像，确认卷数 +1。
- [ ] **校验**：
  - [ ] 正常 WIM/ESD → 校验通过；
  - [ ] 故意损坏的 WIM（改几个字节）→ 校验失败（错误码 13/损坏提示）；
  - [ ] SWM 多分卷 → 校验通过（缺一片时应报错）。
- [ ] **进度条**：apply/capture/verify 过程中进度百分比正常推进，不卡 0% 或乱跳。

### 2. SAM/「其他用户」离线登录兜底
代码：`PE端/core/account_fix.rs`，接入 `PE端/app.rs`(GUI) 与 `PE端/main.rs`(命令行) 两套安装流程。

测试方法：
- [ ] 还原一个**已 sysprep 的安装镜像** + 勾选无人值守 + 设置自定义用户名 → 进系统**自动登录、无需密码**。
- [ ] 还原一个**整盘备份镜像（未 sysprep）**，其中存在**空密码账户** → 进系统能进入该账户（验证 `LimitBlankPasswordUse=0` 生效）。
- [ ] 检查目标系统注册表：`HKLM\SYSTEM\ControlSet001\Control\Lsa\LimitBlankPasswordUse` 应为 `0`；若设了用户名，`...\Winlogon\AutoAdminLogon` 应为 `1`。
- [ ] **已知限制**：备份镜像里账户**本身有非空密码**时，本兜底**无法清除密码**（仍需密码）。这种情况见「二、待做 - 非空密码离线清除」。

> 诊断「其他用户」问题：取故障机 `C:\Windows\Panther\setupact.log`，搜 `oobeSystem` / `LocalAccount`，确认账户创建那一步有没有执行。

### 3. 自定义无人值守文件 + 语法校验
代码：`正常系统端/ui/system_install.rs`、`core/install_config.rs`(validate_unattend_xml)、PE端 应用。

测试方法：
- [ ] 勾选无人值守 → 点「选择文件」选一个**正确**的 unattend.xml → 顶部显示绿色「语法校验通过」，安装按钮可用。
- [ ] 选一个**语法错误**的 xml（如缺闭合标签）→ 顶部红色提示错误，**安装按钮被禁用**。
- [ ] 用自定义文件完成安装 → 目标系统 `C:\Windows\Panther\unattend.xml` 内容与所选文件一致（不是内置生成的）。

### 4. PE 字体路径不再写死 X 盘
代码：`PE端/app.rs` setup_fonts。

测试方法：
- [ ] 在系统盘符**不是 X:** 的 PE 环境里启动 PE 端，确认中文显示正常（不是方块/乱码）。
- [ ] 日志里能看到「已加载中文字体: ...」指向实际系统盘的 Fonts 目录。

### 5. 日志文件后缀
代码：`正常系统端/utils/logger.rs`。

测试方法：
- [ ] 运行后查看 `{程序目录}\log\`，文件名应为 `LetRecovery.2026-XX-XX.log`（**以 .log 结尾**），不再是 `LetRecovery.log.2026-XX-XX`。

### 6. libwim-15.dll 运行时打包
测试方法：
- [ ] `cargo build` 后，`target\debug\`（或 release）目录里应有 `libwim-15.dll`。
- [ ] 把 exe + dll 拷到干净目录运行，镜像功能正常（不报「找不到 wimlib」）。

### 7. wimlib 全局 init 只执行一次（#6）
- [ ] 连续多次执行「校验 → 释放 → 备份」，确认不崩溃（验证移除 Drop cleanup 后多实例安全）。

### 8. 设置界面已删除「免费声明/使用条款」
- [ ] 打开「关于/设置」，确认不再显示「免费声明」「使用条款（允许/禁止）」，许可证、致谢、说明仍在。

---

## 二、待做的重构（需要你配合 / 真机测试）

### #2 拆 cargo workspace + 共享 core 库（消除两端复制粘贴）
- **价值**：`PE端` 与 `正常系统端` 的 `core/*` 大量重复，共享后改一处即可。
- **关于 Cargo.lock**：你说得对，**可以用 GitHub Actions 重新生成**。做法二选一：
  1. 在 CI 加一步 `cargo generate-lockfile`（或去掉 `--locked`），让 CI 用新依赖图构建；或
  2. 加一个一次性 workflow：`cargo update` 后用 `git` 把更新后的 `Cargo.lock` 提交回分支（用 token 写权限）。
- **真正的难点（不是 lock）**：两端「同名」模块其实有**差异**（`dism.rs`、`config`、`wimgapi.rs`、`system_utils.rs` 等不完全一样），共享前必须**逐个调和差异**，这会改变运行时行为，**必须真机回归测试**。
- **建议**：分步做——先抽**字节完全相同**的模块（如 `wimlib.rs`）到共享 crate，再逐个调和其余。每步真机测。

测试方法（每抽一个模块后）：
- [ ] 两端编译通过（GHA）。
- [ ] 该模块相关功能真机回归（如抽 wimlib → 重跑上面「镜像操作」全部用例）。

### #7 手写 XML 解析换成 roxmltree
- **现状**：`wimgapi.rs::parse_image_info_from_xml` 用字符串 `find` 手解析，遇到属性里含 `>`、实体转义可能误判。
- **关于依赖**：需加 `roxmltree`，同 #2 用 GHA 重新生成 `Cargo.lock`。
- **风险**：改变解析行为，需真机验证 WIM/ESD 信息显示是否仍正确。

测试方法：
- [ ] 用多种 WIM/ESD（单卷、多卷、带 DISPLAYNAME/WINDOWS 块、Win7 老格式）确认卷名/版本解析与现在一致。

### #1 统一 PE 两套安装流程（main.rs CLI ↔ app.rs GUI）
- **现状**：`PE端` 有命令行 `run_cli_mode` 和 GUI `execute_install_workflow` 两套几乎重复的安装流程，已出现分叉（unattend 模板曾不一致）。本分支已把「无人值守 + 登录兜底」同步进两边，但**完整去重未做**。
- **风险**：动安装主流程，必须真机测试两种启动方式（`/PEINSTALL` 命令行 与 GUI 自动）。

测试方法：
- [ ] 命令行 `LetRecovery.exe /PEINSTALL` 完整装一遍；
- [ ] GUI 自动检测配置完整装一遍；
- [ ] 两者结果一致。

### 非空密码的离线清除 / 凭空创建账户（彻底治「其他用户」）
- **现状**：本分支只做了零风险的「放开空密码策略 + 自动登录」兜底；**无法清除账户已有的非空密码**，也无法在目标系统**新建**账户。
- **方案**：离线编辑目标 SAM（chntpw 方式：改 F 值启用账户、改 V 值清空 NT hash），或打包 chntpw 调用。
- **风险高**：写错会损坏 SAM 导致**无法启动**。实现时必须：先**备份 SAM hive**、严格校验结构、任何不匹配即放弃。
- **必须真机/虚拟机充分测试**后才能合并。

测试方法：
- [ ] 虚拟机还原一个「账户有非空密码」的备份镜像 → 运行清除 → 确认能空密码登录且系统正常。
- [ ] 故意用结构异常的 SAM → 确认程序**放弃操作且不损坏**原 hive。

---

## 三、构建产物分发提醒
- 发布时，`LetRecovery.exe` 与 `libwim-15.dll` 必须放同一目录；PE 端打包进 PE 时也要带上该 DLL。
- 许可证：libwim 为 LGPLv3，动态链接对闭源/商用友好，保留许可证文本并允许用户替换该 DLL 即可。
