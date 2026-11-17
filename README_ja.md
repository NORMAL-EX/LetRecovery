<div align="center">

# LetRecovery

**非商用利用は無料の、ソースコード公開型 Windows 再インストールツール**

[English](README_en.md) | [简体中文](README.md) | 日本語

[![License](https://img.shields.io/badge/License-PolyForm%20NC-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.88%2B-orange.svg)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/Platform-Windows-lightgrey.svg)](https://www.microsoft.com/windows)

<img width="803" height="561" alt="image" src="https://github.com/user-attachments/assets/72720b82-416d-4fdc-9fbe-b2a63f906e73" />

</div>

---

> 💡 **LetRecovery はソースコードが公開されており、PolyForm Noncommercial 1.0.0 の条件の範囲内で無料で利用できます。** これは OSI 承認のオープンソースライセンスではなく、商用利用は禁止されています。リリースは下記の公式チャネルからのみ入手してください。

> ⚠️ **[利用規約](USER_TERMS.md)に違反する方法で本プロジェクトを使用することは固く禁じられています。** 作者は、権利侵害、無許可の配布、無許可の商用カスタマイズ、その他の違反に対して法的措置を取る権利を留保します。また作者は、適用法令およびすでに付与されたライセンスの範囲内で、将来のバージョンのライセンスを変更する権利、またはそのソースコードの公開をいつでも停止する権利を留保します。

> 📢 **メンテナンスの引き継ぎ：** 原作者は高校に進学したため、今後のメンテナンスは [**hwyyds-skidder-team**](https://github.com/hwyyds-skidder-team) が引き継ぎます。既存の作者表記、ライセンス、利用規約は変わりません。

## ✨ 機能

### 🖥️ システムインストール
- **多様なイメージ形式** - WIM / ESD / SWM / GHO / ISO（Windows XP / 2003 i386 のテキストモードセットアップを含む）。自動マウントと解析、複数エディションの選択に対応
- **デスクトップ & WinPE** - デスクトップからワンクリックでデプロイできます。現在のシステムドライブを再インストールする場合は、ブートを自動で書き込み、WinPE で再起動して完了させます
- **BitLocker で暗号化されたシステムの再インストール** - BitLocker で暗号化されたシステムドライブを、デプロイ前に自動でロック解除 / 復号します
- **無人インストール** - 内蔵の生成機能またはカスタムの unattend.xml を使用でき、ソースイメージ / インストールメディアに埋め込まれた応答ファイルを自動検出して、チェックボックスの既定値をそれに合わせます
- **ブートモード** - UEFI / Legacy を自動検出し、手動でも選択できます

### 💾 システムバックアップ
- **フル / 増分バックアップ** - システムパーティションを WIM / ESD / SWM / GHO にバックアップします
- **名前と説明のカスタマイズ**

### 🌐 オンラインダウンロード
- **システムイメージ / よく使うソフトウェア** - オンラインで取得し、マルチスレッドの Aria2 で高速化します
- **PE の整合性とカスタマイズ** - オンラインダウンロードでは SHA-256 を優先しつつ、従来の MD5 との互換性も維持します。`bin/pe` 内のパッケージファイルは引き続きユーザーがカスタマイズでき、ローカルで変更した後もリモートのハッシュによってブロックされることはありません

### 🔧 詳細オプション
- パーティションのフォーマット、ブート修復（UEFI / Legacy）
- ドライバーのエクスポート（DISM API）/ インポート、ストレージコントローラードライバーの注入
- レジストリの注入、プリインストールされた UWP アプリの削除、OOBE のバイパス、Update / Defender の無効化などの調整
- WiFi プロファイルの移行

### 🛠️ ツールボックス
- **BitLocker 管理** - ロック解除 / 復号 / 保護の一時停止·再開 / 回復キーの表示
- **パスワードのリセット** - アカウントのパスワードをオンライン（現在のシステム）またはオフライン（他のシステム）で消去します
- **イメージ検証 / ファイルハッシュ検証** - デプロイ前にイメージの整合性を確認します
- **クイックパーティション / パーティションのクローン / 一括フォーマット**
- **C: ドライブの無損失拡張** - 現在のシステムの C: ドライブを無損失で拡張します。WinPE がなければ自動でダウンロードし、PE ブートをインストールしてから、WinPE で再起動してサイズを変更します
- **ドライバーのバックアップと復元、ストレージドライバーのインポート**
- **APPX アプリの削除、NVIDIA ドライバーのアンインストール、時刻同期、GHO パスワードの表示、SpaceSniffer によるディスク分析、ワンクリックブート修復**

---

## 🚀 クイックスタート

### システム要件

- 通常システム用アプリケーション：Windows 7〜11（64 ビット、1 つの共通ビルド）
- WinPE 用アプリケーション：リリースパッケージに同梱されている 64 ビット WinPE
- 管理者権限
- 4GB 以上の空きメモリ
- UEFI または Legacy BIOS ブートのサポート

デプロイ可能なターゲットイメージの範囲は、サポートされるホスト環境よりも広く、上記の Windows XP/2003 およびそれ以降の Windows のパスを含みます。古いシステムで正常に起動できるかどうかは、引き続きハードウェア、ファームウェアのモード、ドライバーのサポートに依存します。

### 使い方

1. **ダウンロード** - [Releases](https://github.com/NORMAL-EX/LetRecovery/releases) から最新版を入手します
2. **管理者として実行** - プログラムを右クリックして「管理者として実行」を選択します
3. **イメージの選択** - 「システム インストール」ページでローカルまたはオンラインのイメージを選択します
4. **ターゲットパーティションの選択** - システムをインストールするターゲットパーティションを選択します
5. **インストールの開始** - 「インストールの開始」ボタンをクリックします

> ⚠️ **警告**：システムのインストールではターゲットパーティションがフォーマットされます。重要なデータは先にバックアップしてください！

---

## 📁 プロジェクト構成

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

## 🛠️ 技術スタック

| 技術 | 用途 |
|------------|---------|
| **Rust** | 主要なプログラミング言語 |
| **Native Win32 / windows-rs** | デスクトップおよび PE のインターフェースと Windows API の境界 |
| **tokio** | 非同期ランタイム |
| **aria2** | 高速ダウンロードエンジン |
| **wimlib / WIMGAPI / DISM** | イメージのデプロイ、キャプチャ、ドライバーのサービス |
| **Ghost** | GHO イメージの復元 |
| **React / TypeScript / Vite** | Web サイトとドキュメントサイト |

---

## 🏗️ ソースからのビルド

### 前提条件

- Rust 1.88.0 と、そのツールチェーンの `rust-src` コンポーネント（通常エンドポイントの Win7 ビルドと CI はこのバージョンに固定されています）
- Desktop development with C++ と Windows 10/11 SDK を含む Visual Studio Build Tools 2022
- Web サイト用の Node.js 22 と npm
- 完全なリリースパッケージの作成には、さらに 7-Zip と Windows DISM/ADK ツールが必要です

### ビルド手順

```bash
# Clone the repository
git clone https://github.com/NORMAL-EX/LetRecovery.git
cd LetRecovery

# Rebuild the standard library for the Win7 normal endpoint and verify its imports
rustup toolchain install 1.88.0 --component rust-src
powershell -ExecutionPolicy Bypass -File .github/scripts/build-win7-normal.ps1

# The PE endpoint runs only in modern WinPE and is built separately with the normal MSVC target
cargo +1.88.0 build -p letrecovery-pe --release --locked --target x86_64-pc-windows-msvc

# Build the website
cd 官网
npm ci
npm run lint
npm run type-check
npm run build
```

> Windows 7 向けの通常エンドポイントを作成するのに `cargo build --workspace --release` を使わないでください。このコマンドはホストの `x86_64-pc-windows-msvc` 標準ライブラリを使用するため、Windows 8 以降の API を静的にインポートしてしまう可能性があります。ローカルビルドと Release ビルドはどちらも `build-win7-normal.ps1` を使用する必要があり、これにより `target/x86_64-win7-windows-msvc/release/LetRecovery.exe` が生成・検証されます。

変更を提出する前に、次のチェックを実行してください。

```bash
cargo fmt --all --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked --features "LetRecovery/non-elevated-tests,letrecovery-pe/non-elevated-tests" -- -D warnings -A clippy::uninlined_format_args
cargo test --workspace --no-run --locked --features "LetRecovery/non-elevated-tests,letrecovery-pe/non-elevated-tests"
cargo test -p lr-core --locked
cargo test -p letrecovery-pe --locked --features non-elevated-tests
cargo test -p LetRecovery --locked --features non-elevated-tests
```

CI は、プルリクエストと `main` への push に対して、すべてのテストターゲットをコンパイルし、決定的なユニットテストを実行し、Web サイトをビルドします。CI が実際のフォーマット、パーティション操作、BCD の変更、DISM の書き込み、再起動を行うことはありません。これらのワークフローには、別途隔離された VM と専用のテストディスクが必要です。同梱されている `libwim-15.dll` のバージョン、ライセンス、ハッシュについては、[サードパーティバイナリの出所](docs/THIRD_PARTY_BINARIES.md)を参照してください。

---

## 📄 ライセンス

本プロジェクトは [PolyForm Noncommercial License 1.0.0](LICENSE) の下でライセンスされています。

これはソースコード公開型のライセンスであり、OSI 承認のオープンソースライセンスではありません。この但し書きなしに、本プロジェクトを従来のオープンソースソフトウェアと表現すべきではありません。

- ✅ 個人の学習、研究、非商用での利用は許可されています
- ✅ 改変と配布は許可されています（著作権表示が必要です）
- ❌ 商用利用は禁止されています

セキュリティの問題は、[SECURITY.md](SECURITY.md) に記載の方法で非公開で報告してください。コントリビュートする前に [CONTRIBUTING.md](CONTRIBUTING.md) をお読みください。

---

## 🙏 謝辞

- PE ダウンロードサービスは **Cloud-PE** が提供しています
- WinPE を提供してくださった **[电脑病毒爱好者](https://github.com/HelloWin10-19045)** に感謝します

---

## 👥 作者とメンテナー

- **原作者：** [NORMAL-EX](https://github.com/NORMAL-EX)（dddffgg としても知られています）
- **現在のメンテナンスチーム：** [hwyyds-skidder-team](https://github.com/hwyyds-skidder-team)

コード、テスト、フィードバックで貢献してくださるすべての方に感謝します。

---

## 🔗 リンク

- 🌐 **Web サイト**: [sysre.cn](https://sysre.cn)
- 📦 **リリース**: [GitHub Releases](https://github.com/NORMAL-EX/LetRecovery/releases)
- 🐛 **Issue**: [GitHub Issues](https://github.com/NORMAL-EX/LetRecovery/issues)

---

<div align="center">

**このプロジェクトが役に立ったら、ぜひ ⭐ Star をお願いします！**

</div>
