<div align="center">

# LetRecovery

**A Source-Available Windows Reinstallation Tool Free for Noncommercial Use**

English | [简体中文](README.md)

[![License](https://img.shields.io/badge/License-PolyForm%20NC-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.88%2B-orange.svg)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/Platform-Windows-lightgrey.svg)](https://www.microsoft.com/windows)

<img width="803" height="600" alt="image" src="https://github.com/user-attachments/assets/8760ea53-785c-48ba-a6ce-dc3e154d3926" />

</div>

---

> 💡 **LetRecovery is source-available and free to use within the PolyForm Noncommercial 1.0.0 terms.** This is not an OSI-approved open-source license, and commercial use is prohibited. Obtain releases only from the official channels below.

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
- **PE integrity and customization** - Online downloads prefer SHA-256 while preserving legacy MD5 compatibility; packaged files in `bin/pe` remain user-customizable and are not blocked by remote hashes after local modification

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

- Normal system application: Windows 10/11 (64-bit)
- WinPE application: the 64-bit WinPE shipped with the release package
- Administrator privileges
- At least 4GB available memory
- UEFI or Legacy BIOS boot support

The set of deployable target images is broader than the supported host environment and includes the Windows XP/2003 and newer Windows paths listed above. Successful boot on older systems still depends on hardware, firmware mode, and driver support.

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
├── lr-core/             # Shared pure logic and Windows adapters
├── 正常系统端/          # Windows desktop application
├── PE端/                # WinPE application
├── 官网/                # React/Vite website
├── assets/              # Language and release assets
├── docs/                # Design and binary provenance records
├── Cargo.toml           # Rust workspace
├── Cargo.lock           # Locked application dependency graph
└── LICENSE              # PolyForm Noncommercial 1.0.0
```

---

## 🛠️ Tech Stack

| Technology | Purpose |
|------------|---------|
| **Rust** | Primary programming language |
| **Native Win32 / windows-rs** | Desktop and PE interfaces plus Windows API boundaries |
| **tokio** | Async runtime |
| **aria2** | High-speed download engine |
| **wimlib / WIMGAPI / DISM** | Image deployment, capture, and driver servicing |
| **Ghost** | GHO image restoration |
| **React / TypeScript / Vite** | Website and documentation site |

---

## 🏗️ Building from Source

### Prerequisites

- Rust 1.88 or higher (CI uses 1.88.0)
- Visual Studio Build Tools 2022 with Desktop development with C++ and a Windows 10/11 SDK
- Node.js 22 and npm for the website
- Full release packaging additionally needs 7-Zip and Windows DISM/ADK tooling

### Build Steps

```bash
# Clone the repository
git clone https://github.com/NORMAL-EX/LetRecovery.git
cd LetRecovery

# Build both Rust applications from the workspace lockfile
cargo build --workspace --release --locked

# Build the website
cd 官网
npm ci
npm run lint
npm run type-check
npm run build
```

Run these checks before submitting changes:

```bash
cargo fmt --all --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked --features "LetRecovery/non-elevated-tests,letrecovery-pe/non-elevated-tests" -- -D warnings -A clippy::uninlined_format_args
cargo test --workspace --no-run --locked --features "LetRecovery/non-elevated-tests,letrecovery-pe/non-elevated-tests"
cargo test -p lr-core --locked
cargo test -p letrecovery-pe --locked --features non-elevated-tests
cargo test -p LetRecovery --locked --features non-elevated-tests
```

CI compiles all test targets, runs deterministic unit tests, and builds the website for pull requests and pushes to `main`. CI never performs real formatting, partitioning, BCD changes, DISM writes, or reboots; those workflows require a separate isolated VM and dedicated test disk. See [Third-Party Binary Provenance](docs/THIRD_PARTY_BINARIES.md) for the bundled `libwim-15.dll` version, license, and hashes.

---

## 📄 License

This project is licensed under the [PolyForm Noncommercial License 1.0.0](LICENSE).

This is a source-available license, not an OSI-approved open-source license, and the project should not be described as traditional open-source software without that qualification.

- ✅ Personal learning, research, and non-commercial use allowed
- ✅ Modification and distribution allowed (with copyright notice)
- ❌ Commercial use prohibited

Report security issues privately as described in [SECURITY.md](SECURITY.md). See [CONTRIBUTING.md](CONTRIBUTING.md) before contributing.

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
