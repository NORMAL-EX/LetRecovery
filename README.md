<div align="center">

# LetRecovery

**一款免费开源的 Windows 系统重装工具**

[English](README_en.md) | 简体中文

[![License](https://img.shields.io/badge/License-PolyForm%20NC-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange.svg)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/Platform-Windows-lightgrey.svg)](https://www.microsoft.com/windows)

<img width="803" height="600" alt="image" src="https://github.com/user-attachments/assets/8760ea53-785c-48ba-a6ce-dc3e154d3926" />

</div>

---

> 💡 **LetRecovery 永久免费、开源。** 请仅从本页下方的官方渠道获取，谨防第三方付费倒卖。

## ✨ 功能特性

### 🖥️ 系统安装
- **多格式镜像** - WIM / ESD / SWM / GHO / ISO，自动挂载解析、多分卷选择
- **桌面 & WinPE 双端** - 桌面端一键部署；重装当前系统盘时自动写引导、重启进 WinPE 完成
- **BitLocker 加密盘重装** - 自动解锁/解密 BitLocker 加密的系统盘后再部署
- **无人值守** - 内置生成或自定义 unattend.xml
- **引导模式** - UEFI / Legacy 自动识别，可手动指定

### 💾 系统备份
- **完整 / 增量备份** - 备份系统分区为 WIM / ESD / SWM / GHO
- **自定义命名与描述**

### 🌐 在线下载
- **系统镜像 / 常用软件** - 在线获取，Aria2 多线程加速

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
- **驱动备份还原、导入存储驱动**
- **移除 APPX 应用、英伟达驱动卸载、系统时间校准、查看 GHO 密码、SpaceSniffer 磁盘分析、一键修复引导**

---

## 🚀 快速开始

### 系统要求

- Windows 10/11 (64位)
- 管理员权限
- 至少 4GB 可用内存
- 支持 UEFI 或 Legacy BIOS 启动

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
├── 正常系统端/          # Windows 桌面环境版本
│   ├── src/
│   │   ├── app.rs       # 主应用程序
│   │   ├── core/        # 核心功能模块
│   │   │   ├── bcdedit.rs   # BCD 引导编辑
│   │   │   ├── disk.rs      # 磁盘分区管理
│   │   │   ├── dism.rs      # DISM 镜像操作
│   │   │   ├── ghost.rs     # GHO 镜像恢复
│   │   │   └── registry.rs  # 注册表操作
│   │   ├── download/    # 下载管理模块
│   │   │   ├── aria2.rs     # Aria2 下载引擎
│   │   │   └── manager.rs   # 下载管理器
│   │   ├── ui/          # 用户界面
│   │   └── utils/       # 工具函数
│   └── Cargo.toml
├── PE端/               # WinPE 环境版本
│   ├── src/
│   │   ├── app.rs
│   │   ├── core/
│   │   ├── ui/
│   │   └── utils/
│   └── Cargo.toml
└── LICENSE
```

---

## 🛠️ 技术栈

| 技术 | 用途 |
|------|------|
| **Rust** | 主要编程语言 |
| **egui/eframe** | 跨平台 GUI 框架 |
| **tokio** | 异步运行时 |
| **windows-rs** | Windows API 绑定 |
| **aria2** | 高速下载引擎 |
| **DISM** | 系统镜像部署 |
| **Ghost** | GHO 镜像恢复 |

---

## 🏗️ 从源码构建

### 前置条件

- Rust 1.75 或更高版本
- Visual Studio Build Tools (Windows)

### 构建步骤

```bash
# 克隆仓库
git clone https://github.com/NORMAL-EX/LetRecovery.git
cd LetRecovery

# 构建正常系统端
cd 正常系统端
cargo build --release

# 构建 PE 端
cd ../PE端
cargo build --release
```

---

## 📄 许可证

本项目采用 [PolyForm Noncommercial License 1.0.0](LICENSE) 许可证。

- ✅ 允许个人学习、研究和非商业使用
- ✅ 允许修改和分发（需保留版权声明）
- ❌ 禁止商业用途

---

## 🙏 致谢

- 部分系统镜像及 PE 下载服务由 **Cloud-PE 云盘** 提供
- 感谢 **[电脑病毒爱好者](https://github.com/HelloWin10-19045)** 提供 WinPE

---

## 👤 作者

**NORMAL-EX** (又称 dddffgg)

- GitHub: [@NORMAL-EX](https://github.com/NORMAL-EX)

---

## 🔗 相关链接

- 🌐 **官网**: [sysre.cn](https://sysre.cn)
- 📦 **发布页**: [GitHub Releases](https://github.com/NORMAL-EX/LetRecovery/releases)
- 🐛 **问题反馈**: [GitHub Issues](https://github.com/NORMAL-EX/LetRecovery/issues)

---

<div align="center">

**如果觉得这个项目有帮助，欢迎给个 ⭐ Star！**

</div>
