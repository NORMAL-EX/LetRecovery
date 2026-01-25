<div align="center">

# LetRecovery

**A Free and Open-Source Windows System Reinstallation Tool**

English | [简体中文](README.md)

[![License](https://img.shields.io/badge/License-PolyForm%20NC-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange.svg)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/Platform-Windows-lightgrey.svg)](https://www.microsoft.com/windows)

<img width="1429" height="1067" alt="image" src="https://github.com/user-attachments/assets/687be36e-4cfd-4cc1-8e49-5a8bf7309463" />

</div>

---

## ✨ Features

### 🖥️ System Installation
- **WIM/ESD Image Deployment** - Support for official Windows image formats
- **GHO Image Restoration** - Compatible with Ghost backup images
- **ISO Image Mounting** - Automatic mounting and parsing of ISO files
- **Multi-Volume Selection** - Choose different system editions from images

### 💾 System Backup
- **Full Backup** - Backup system partition to WIM image
- **Incremental Backup** - Append backups to existing image files
- **Custom Naming** - Support for custom backup names and descriptions

### 🌐 Online Download
- **System Image Download** - Download Windows system images online
- **Common Software Download** - Built-in common installation software downloads
- **Aria2 Acceleration** - Multi-threaded high-speed download with Aria2

### 🔧 Advanced Options
- **Format Partition** - Option to format target partition before installation
- **Boot Repair** - Automatic UEFI/Legacy boot repair
- **Driver Import** - Export and import system drivers
- **Unattended Install** - Support for unattended installation configuration
- **Registry Injection** - Automatic registry settings injection after installation

### 🛠️ Toolbox
- **Boot Repair Tool** - Standalone BCD boot repair
- **Disk Management** - View and manage disk partitions
- **Hardware Info** - View detailed hardware information

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
