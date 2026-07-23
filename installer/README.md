# LetRecovery Installer

This directory builds the complete `pkg/` release tree into one modern offline
x64 installer with Inno Setup 6.7. The generated setup supports interactive
installation, silent installation with `/VERYSILENT`, and silent
uninstallation with `/VERYSILENT`.

## Build

Install Inno Setup 6.7, or place its compiler at
`installer/tools/inno/ISCC.exe`, then run:

```powershell
powershell -ExecutionPolicy Bypass -File .\installer\build-installer.ps1
```

Output is written to `installer/output/LetRecovery-Setup-x64.exe`.

The build script reads the version from `pkg/LetRecovery.exe`, validates the
minimum package layout, and prints the final size, SHA-256, and Authenticode
status. Use `-RequireSignature` in a production pipeline after signing is
configured.

## Packaging behavior

- Installs per-machine to `Program Files\LetRecovery` and requires UAC.
- Includes the complete offline `pkg/` tree; it is not a web installer.
- Preserves an existing `config.json` during upgrades.
- Creates Start Menu shortcuts and enables the desktop shortcut by default.
- Uses Inno Setup's modern dynamic Windows 11 style, follows the system light
  or dark mode, and uses Microsoft YaHei UI throughout the wizard.
- Presents a dedicated, scrollable backup/risk notice before the separate
  Chinese reference translation of the license.
- Registers a clean silent-capable uninstaller.
- Supports only 64-bit Windows 10 and Windows 11, matching the desktop app.

Local unsigned builds are for testing only. A Microsoft Store submission must
use a versioned HTTPS URL and a trusted code-signing certificate for the
installer and applicable bundled PE files.

`languages/ChineseSimplified.isl` is the official Inno Setup Simplified Chinese
translation maintained by Zhenghan Yang and linked from the Inno Setup
translations page. The pinned file currently has SHA-256
`6753BE2C5E2740D859900FD902824DB2EC568DA5C5B52486524C9762D778B0B0`.
