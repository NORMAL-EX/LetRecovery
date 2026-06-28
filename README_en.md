<div align="center">

# LetRecovery

**A Free and Open-Source Windows System Reinstallation Tool**

English | [简体中文](README.md)

[![License](https://img.shields.io/badge/License-PolyForm%20NC-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange.svg)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/Platform-Windows-lightgrey.svg)](https://www.microsoft.com/windows)

<img width="803" height="600" alt="image" src="https://github.com/user-attachments/assets/8760ea53-785c-48ba-a6ce-dc3e154d3926" />

</div>

---

> 💡 **LetRecovery is free and open-source, forever.** Please get it only from the official channels listed below — beware of third-party paid resellers.

## ✨ Features

### 🖥️ System Installation
- **Multi-format images** - WIM / ESD / SWM / GHO / ISO (including Windows XP / 2003 i386 text-mode setup), auto mount & parse, multi-edition selection
- **Desktop & WinPE** - One-click deploy from desktop; when reinstalling the current system drive it auto-writes boot and reboots into WinPE to finish
- **BitLocker-encrypted system reinstall** - Automatically unlocks/decrypts the BitLocker-encrypted system drive before deployment
- **Unattended install** - Built-in generated or custom unattend.xml, and auto-detects answer files embedded in the source image / install media to default the checkbox accordingly
- **Boot mode** - UEFI / Legacy auto-detected, manually selectable

### 💾 System Backup
- **Full / incremental backup** - Back up the system partition to WIM / ESD / SWM / GHO
- **Custom name & description**

### 🌐 Online Download
- **System images / common software** - Fetched online, accelerated by multi-threaded Aria2

### 🔧 Advanced Options
- Format partition, boot repair (UEFI / Legacy)
- Driver export (DISM API) / import, storage-controller driver injection
- Registry injection, remove preinstalled UWP apps, OOBE bypass, disable Update / Defender and other tweaks
- WiFi profile migration

### 🛠️ Toolbox
- **BitLocker management** - unlock / decrypt / suspend·resume protection / view recovery key
- **Password reset** - clear account password online (current system) or offline (other systems)
- **Image verify / file hash verify** - check image integrity before deployment
- **Quick partition / partition clone / batch format**
- **Losslessly expand C: drive** - losslessly expand the current system C: drive: auto-downloads WinPE if missing, installs PE boot, then reboots into WinPE to resize
- **Driver backup & restore, import storage drivers**
- **Remove APPX apps, NVIDIA driver uninstall, time sync, view GHO password, SpaceSniffer disk analysis, one-click boot repair**

---

## 🚀 Quick Start

### System Requirements

- Windows 10/11 (64-bit)
- Administrator privileges
- At least 4GB available memory
- UEFI or Legacy BIOS boot support

### Usage

1. **Download** - Get the latest version from [Releases](https://github.com/NORMAL-EX/LetRecovery/releases)
2. **Run as Administrator** - Right-click the program and select "Run as administrator"
3. **Select Image** - Choose local or online image in "System Install" page
4. **Select Target Partition** - Choose the target partition for system installation
5. **Start Installation** - Click the "Start Install" button

> ⚠️ **Warning**: System installation will format the target partition. Please backup important data first!

---

## 📁 Project Structure

```
LetRecovery/
├── 正常系统端/          # Windows Desktop Environment Version
│   ├── src/
│   │   ├── app.rs       # Main application
│   │   ├── core/        # Core modules
│   │   │   ├── bcdedit.rs   # BCD boot editing
│   │   │   ├── disk.rs      # Disk partition management
│   │   │   ├── dism.rs      # DISM image operations
│   │   │   ├── ghost.rs     # GHO image restoration
│   │   │   └── registry.rs  # Registry operations
│   │   ├── download/    # Download management
│   │   │   ├── aria2.rs     # Aria2 download engine
│   │   │   └── manager.rs   # Download manager
│   │   ├── ui/          # User interface
│   │   └── utils/       # Utility functions
│   └── Cargo.toml
├── PE端/               # WinPE Environment Version
│   ├── src/
│   │   ├── app.rs
│   │   ├── core/
│   │   ├── ui/
│   │   └── utils/
│   └── Cargo.toml
└── LICENSE
```

---

## 🛠️ Tech Stack

| Technology | Purpose |
|------------|---------|
| **Rust** | Primary programming language |
| **egui/eframe** | Cross-platform GUI framework |
| **tokio** | Async runtime |
| **windows-rs** | Windows API bindings |
| **aria2** | High-speed download engine |
| **DISM** | System image deployment |
| **Ghost** | GHO image restoration |

---

## 🏗️ Building from Source

### Prerequisites

- Rust 1.75 or higher
- Visual Studio Build Tools (Windows)

### Build Steps

```bash
# Clone the repository
git clone https://github.com/NORMAL-EX/LetRecovery.git
cd LetRecovery

# Build Normal System Version
cd 正常系统端
cargo build --release

# Build PE Version
cd ../PE端
cargo build --release
```

---

## 📄 License

This project is licensed under the [PolyForm Noncommercial License 1.0.0](LICENSE).

- ✅ Personal learning, research, and non-commercial use allowed
- ✅ Modification and distribution allowed (with copyright notice)
- ❌ Commercial use prohibited

---

## 🙏 Acknowledgments

- System images and PE download services provided by **Cloud-PE**
- Thanks to **[电脑病毒爱好者](https://github.com/HelloWin10-19045)** for providing WinPE

---

## 👤 Author

**NORMAL-EX** (also known as dddffgg)

- GitHub: [@NORMAL-EX](https://github.com/NORMAL-EX)

---

## 🔗 Links

- 🌐 **Website**: [sysre.cn](https://sysre.cn)
- 📦 **Releases**: [GitHub Releases](https://github.com/NORMAL-EX/LetRecovery/releases)
- 🐛 **Issues**: [GitHub Issues](https://github.com/NORMAL-EX/LetRecovery/issues)

---

<div align="center">

**If you find this project helpful, please give it a ⭐ Star!**

</div>
