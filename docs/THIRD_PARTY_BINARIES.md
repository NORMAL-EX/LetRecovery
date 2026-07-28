# Third-Party Binary Provenance

LetRecovery ships the following third-party binary. Update it only after
reviewing the upstream release notes, licenses, local patches, and hashes.
Hashes in this file are for the exact bytes committed to this repository.

## wimlib

| Field | Value |
| --- | --- |
| Repository path | `lr-core/vendor/libwim-15.dll` |
| Upstream project | [wimlib](https://wimlib.net/) |
| Upstream release | 1.14.5 |
| Upstream Git URL | `https://wimlib.net/git/wimlib` |
| Pinned upstream commit | `cd5e231c348c255ae5088873b5a66ee0eb96fa07` |
| LetRecovery patch | `docs/third-party/wimlib-1.14.5/letrecovery-parallel-decompression.patch` |
| Patch SHA-256 | `149788349D1C3317FBDEF63DF578DCCB5044D93FE3BA65C13B869C24A6659240` |
| Reproducible build script | `.github/scripts/build-wimlib-parallel.ps1` |
| Committed DLL size | `493056` bytes |
| Committed DLL SHA-256 | `E7AA66972B27701A5991108396AD32CE11B60147ECD1D98B70013BC816A61099` |
| License used for `libwim` and the LetRecovery patch | GNU Lesser General Public License v3.0 or later |
| Additional bundled notice | `libdivsufsort-lite` license from upstream |

The committed DLL is built from the pinned official source plus the tracked
LetRecovery patch. The extension adds bounded ordered parallel decompression,
parallel verification of independent non-solid resources, and bounded
prefetch for application. Windows workers use independent read handles because
wimlib's Windows `pread` compatibility path changes a handle's shared file
position. Progress and extraction callbacks remain serialized on the caller
thread, and SHA-1 verification is unchanged.

The Windows build uses `--without-fuse --without-ntfs-3g`. Upstream 1.14.5
permits LGPLv2.1-or-later; this repository continues to distribute both libwim
and the LetRecovery additions under the already-recorded LGPLv3-or-later
option. The corresponding LGPLv3 and `libdivsufsort-lite` notices are retained
under `docs/third-party/wimlib-1.14.4/`; the locally maintained patch and the
exact 1.14.5 source pin are retained under
`docs/third-party/wimlib-1.14.5/`.

The recorded build used WSL with MinGW-w64 GCC 13-win32, Autoconf 2.71,
Automake 1.16.5, GNU Libtool 2.4.7, and GNU Make 4.3. The script exports the
pinned source commit without Git metadata before bootstrapping, so the DLL
reports upstream version 1.14.5 instead of inheriting the enclosing LetRecovery
worktree version.

### Update procedure

1. Review the pinned upstream commit and the tracked patch.
2. Install the WSL build dependencies: `autoconf`, `automake`, `libtool`,
   `pkg-config`, `make`, and `mingw-w64`.
3. Run `.github/scripts/build-wimlib-parallel.ps1`. The script verifies the
   exact upstream commit, checks that the patch applies cleanly, builds x86_64,
   verifies the PE machine and the
   `wimlib_set_parallel_decompression` export, then stages and replaces the
   requested output.
4. Record the patch, DLL size, and DLL SHA-256 above. Retain the upstream
   notices without modification.
5. Run read-only serial/parallel verification against a known WIM, repeat the
   parallel stress test, and confirm that a deliberately corrupted ordinary
   test copy still fails SHA-1 verification.
6. Run the Rust workspace checks before release.

## Intel Rapid Storage Technology VMD drivers

The release package contains two Microsoft Update Catalog driver packages under
`pkg/bin/drivers/storage_controller/`. LetRecovery never recursively stages the
whole directory: `lr-core::storage_driver_match` selects a package only when
SetupAPI reports a matching Intel PCI hardware ID. AMD, Apple, VirtIO and
unknown controllers select nothing.

| Package path | Version | Microsoft Catalog update ID | Covered primary IDs | Source CAB SHA-256 |
| --- | --- | --- | --- | --- |
| `intel-vmd-11th` | 20.2.4.1019 | `d4c52691-b507-4a37-bce7-b018cd40b4d9` | `8086:9A0B` (plus managed `09AB`) | `913A94E9E292EA984F9150D093456FF8595E6CF4AEA3943801A5F2801781E00D` |
| `intel-vmd-current` | 20.2.12.1036 | `d3ccf9fc-2543-4b7b-9ff0-369264a693be` | `8086:467F`, `A77F`, `7D0B`, `AD0B` | `A5DCE6B59B3775D2F0519EECA69A5EF8754B0AB147474377C2684C6D9E8B47D9` |

The source catalog searches, exact per-file SHA-256 values, signature notes and
Intel license links are retained in each package's `NOTICE.txt`. The CAT and SYS
files were verified as `Valid` and issued by Microsoft Windows Hardware
Compatibility Publisher before packaging. The INF itself is catalog-signed and
therefore does not carry a standalone Authenticode signature.

The retired blanket package directories (`18`, `19`, `20`, `AMD`, `Applessd`,
`iastorE` and `viostor`) must not be restored. Windows 7 UefiSeven, USB3 and NVMe
compatibility payloads were also removed; XP/2003-specific driver resources are
separate and remain supported.
