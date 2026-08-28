<div align="center">

# LetRecovery

**一款免费用于非商业场景、源代码公开的 Windows 系统重装工具**

[English](README_en.md) | 简体中文

[![License](https://img.shields.io/badge/License-PolyForm%20NC-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.88%2B-orange.svg)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/Platform-Windows-lightgrey.svg)](https://www.microsoft.com/windows)

<img width="803" height="561" alt="image" src="https://github.com/user-attachments/assets/72720b82-416d-4fdc-9fbe-b2a63f906e73" />

</div>

---

> 💡 **LetRecovery 在 PolyForm Noncommercial 1.0.0 许可范围内免费使用并公开源代码。** 该许可证不是 OSI 认可的传统开源许可证，禁止商业用途。请仅从本页下方的官方渠道获取。

> ⚠️ **严禁将本项目用于任何违反[用户协议](USER_TERMS.md)的行为。** 对侵权、违规分发、未经授权的商业定制及其他违约使用，作者保留依法追究责任的权利；作者同时保留在适用法律及既有许可约束允许的范围内，随时调整后续版本授权方式或停止公开后续版本源代码的权利。

## ✨ 功能特性

### 🖥️ 系统安装
- **多格式镜像** - WIM / ESD / SWM / GHO / ISO（含 Windows XP / 2003 i386 文本模式安装），自动挂载解析、多分卷选择
- **桌面 & WinPE 双端** - 桌面端一键部署；重装当前系统盘时自动写引导、重启进 WinPE 完成
- **BitLocker 加密盘重装** - 自动解锁/解密 BitLocker 加密的系统盘后再部署
- **无人值守** - 内置生成或自定义 unattend.xml，并自动检测源镜像/安装介质内嵌的应答文件，按检测结果默认勾选
- **引导模式** - UEFI / Legacy 自动识别，可手动指定

### 💾 系统备份
- **完整 / 增量备份** - 备份系统分区为 WIM / ESD / SWM / GHO
- **自定义命名与描述**

### 🌐 在线下载
- **系统镜像 / 常用软件** - 在线获取，Aria2 多线程加速
- **PE 完整性与定制兼容** - 联网下载优先校验 SHA-256，并兼容现有 MD5 配置；随包 `bin/pe` 保持用户可定制，不会因修改 WIM 被远端哈希阻断

### 🔧 高级选项
- 格式化分区、引导修复（UEFI / Legacy）
- 驱动导出（DISM API）/ 导入、磁盘控制器驱动注入
- 注册表注入、移除预装 UWP、OOBE 绕过联网、禁用更新 / Defender 等系统优化
- WiFi 配置迁移

### 🛠️ 工具箱
- **BitLocker 管理** - 解锁 / 解密 / 挂起·恢复保护 / 查看恢复密钥
- **密码重置** - 在线（当前系统）或离线（其他系统）清除账户密码
- **镜像校验 / 文件哈希校验** - 部署前校验镜像完整性
- **一键分区 / 分区对拷 / 批量格式化**
- **无损扩大 C 盘** - 无损扩大当前系统 C 盘：若本机缺少 WinPE 会自动下载，安装 PE 引导后重启进 WinPE 完成扩容
- **驱动备份还原、导入存储驱动**
- **移除 APPX 应用、英伟达驱动卸载、系统时间校准、查看 GHO 密码、SpaceSniffer 磁盘分析、一键修复引导**

---

## 🚀 快速开始

### 系统要求

- 正常系统端：Windows 7–11（64 位，单一通用版本）
- WinPE 端：与项目发布包配套的 64 位 WinPE
- 管理员权限
- 至少 4GB 可用内存
- 支持 UEFI 或 Legacy BIOS 启动

可部署的目标镜像范围比运行环境更广，包含功能列表中列出的 Windows XP/2003 及较新 Windows 版本；旧系统能否正常启动仍取决于硬件、固件模式和驱动支持。

### 使用方法

1. **下载软件** - 从 [Releases](https://github.com/NORMAL-EX/LetRecovery/releases) 页面下载最新版本
2. **以管理员身份运行** - 右键点击程序，选择"以管理员身份运行"
3. **选择镜像** - 在"系统安装"页面选择本地或在线镜像
4. **选择目标分区** - 选择要安装系统的目标分区
5. **开始安装** - 点击"开始安装"按钮

> ⚠️ **警告**: 安装系统会格式化目标分区，请提前备份重要数据！

---

## 📁 项目结构

```
LetRecovery/
├── lr-core/             # 两端共享的纯逻辑与 Windows 适配层
├── 正常系统端/          # Windows 桌面环境版本
├── PE端/                # WinPE 环境版本
├── assets/              # 发布时使用的语言和资源文件
├── docs/                # 设计与第三方二进制溯源文档
├── Cargo.toml           # Rust workspace
├── Cargo.lock           # 已锁定的应用依赖图
└── LICENSE              # PolyForm Noncommercial 1.0.0
```

---

## 🛠️ 技术栈

| 技术 | 用途 |
|------|------|
| **Rust** | 主要编程语言 |
| **原生 Win32 / windows-rs** | 正常系统端与 PE 端界面、Windows API 边界 |
| **tokio** | 异步运行时 |
| **aria2** | 高速下载引擎 |
| **wimlib / WIMGAPI / DISM** | 系统镜像部署、捕获与驱动维护 |
| **Ghost** | GHO 镜像恢复 |

---

## 🏗️ 从源码构建

### 前置条件

- Rust 1.88.0 与该工具链的 `rust-src` 组件（正常端 Win7 构建与 CI 固定使用该版本）
- Visual Studio Build Tools 2022，并安装“使用 C++ 的桌面开发”和 Windows 10/11 SDK
- 完整发布打包还需要 7-Zip 与 Windows DISM/ADK 环境

### 构建步骤

```bash
# 克隆仓库
git clone https://github.com/NORMAL-EX/LetRecovery.git
cd LetRecovery

# 正常系统端必须使用 Win7 目标重编译标准库，并在构建后检查导入表
rustup toolchain install 1.88.0 --component rust-src
powershell -ExecutionPolicy Bypass -File .github/scripts/build-win7-normal.ps1

# PE 端仅运行在现代 WinPE，继续使用普通 Windows MSVC 目标单独构建
cargo +1.88.0 build -p letrecovery-pe --release --locked --target x86_64-pc-windows-msvc

```

> 不要用 `cargo build --workspace --release` 生成要在 Windows 7 上运行的正常端程序。该命令会使用宿主机的 `x86_64-pc-windows-msvc` 标准库，可能静态导入 Windows 8+ API。本地与 Release 必须统一通过 `build-win7-normal.ps1` 生成并验证 `target/x86_64-win7-windows-msvc/release/LetRecovery.exe`。

提交前应运行：

```bash
cargo fmt --all --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked --features "LetRecovery/non-elevated-tests,letrecovery-pe/non-elevated-tests" -- -D warnings -A clippy::uninlined_format_args
cargo test --workspace --no-run --locked --features "LetRecovery/non-elevated-tests,letrecovery-pe/non-elevated-tests"
cargo test -p lr-core --locked
cargo test -p letrecovery-pe --locked --features non-elevated-tests
cargo test -p LetRecovery --locked --features non-elevated-tests
```

CI 会在 Pull Request 和 `main` push 上编译全部测试目标并运行确定性单元测试。官网由独立私有仓库维护；本仓库创建 Release 成功后会同步更新其 `version.json`。CI 不会执行真实格式化、分区、BCD、DISM 写盘或重启；这些流程必须在隔离虚拟机和专用测试盘上另行验证。内置 `libwim-15.dll` 的版本、许可证和哈希见 [第三方二进制清单](docs/THIRD_PARTY_BINARIES.md)。

---

## 📄 许可证

本项目采用 [PolyForm Noncommercial License 1.0.0](LICENSE) 许可证。

这是一个 source-available（源代码公开）许可证，不是 OSI 批准的开源许可证，也不应笼统描述为传统意义上的“开源软件”。

- ✅ 允许个人学习、研究和非商业使用
- ✅ 允许修改和分发（需保留版权声明）
- ❌ 禁止商业用途

安全问题请按 [SECURITY.md](SECURITY.md) 中的方式私下报告。参与开发前请阅读 [CONTRIBUTING.md](CONTRIBUTING.md)。

---

## 🙏 致谢

- PE 下载服务由 **Cloud-PE 云盘** 提供
- 感谢 **[电脑病毒爱好者](https://github.com/HelloWin10-19045)** 提供 WinPE

---

## 👤 作者

**NORMAL-EX** (又称 dddffgg)

- GitHub: [@NORMAL-EX](https://github.com/NORMAL-EX)

---

## 🔗 相关链接

- 🌐 **官网**: [letrecovery.net](https://letrecovery.net)
- 📦 **发布页**: [GitHub Releases](https://github.com/NORMAL-EX/LetRecovery/releases)
- 🐛 **问题反馈**: [GitHub Issues](https://github.com/NORMAL-EX/LetRecovery/issues)

---

<div align="center">

**如果觉得这个项目有帮助，欢迎给个 ⭐ Star！**

</div>
