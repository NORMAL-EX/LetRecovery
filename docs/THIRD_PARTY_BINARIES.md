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
| Patch SHA-256 | `6EEDE7B504B8ED905A7F86CEB681CA40256D642D2CDC17B65568D2E4FD0122BE` |
| Reproducible build script | `.github/scripts/build-wimlib-parallel.ps1` |
| Committed DLL size | `496640` bytes |
| Committed DLL SHA-256 | `78822F2CEF8FE4BD9EBD91943373BA8EA8A32ADA17C49AFC496123B2A938F4EF` |
| License used for `libwim` and the LetRecovery patch | GNU Lesser General Public License v3.0 or later |
| Additional bundled notice | `libdivsufsort-lite` license from upstream |

The committed DLL is built from the pinned official source plus the tracked
LetRecovery patch. The extension adds bounded ordered parallel decompression,
parallel verification of independent non-solid resources, and bounded
prefetch for application. Parallel verification sorts resources by decreasing
uncompressed size before workers claim them, reducing the long-resource tail,
and wakes only the caller waiting for completion. Each verification worker can
also reuse a bounded sliding read window, reducing fragmented reads without
changing resource parsing, decompression, SHA-1 comparison, or progress
callbacks. The window is charged to the same explicit memory budget and is
disabled when less than 1 MiB per worker remains.

Windows workers use independent read handles because wimlib's Windows `pread`
compatibility path changes a handle's shared file position. Automatic worker
selection respects the process affinity, including processor groups when the
Windows 7 APIs are available, and uses currently available commit memory rather
than installed physical memory. Linux uses the current process affinity and
available physical pages. Joined Windows worker handles are always closed.
Progress and extraction callbacks remain serialized on the caller thread, and
SHA-1 verification is unchanged.

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
5. Run serial/parallel verification against uncompressed, XPRESS, LZX and LZMS
   WIMs, solid ESDs, pipable WIMs and split SWMs. Repeat the parallel stress
   test, exercise an explicit low-memory budget, and confirm that payload,
   chunk-table, offset, metadata, integrity-table, truncation, solid-resource
   and split-part corruption remain rejected.
6. Run the Rust workspace checks before release.

## Intel Rapid Storage Technology VMD drivers

The release package contains two Microsoft Update Catalog driver packages under
`pkg/bin/drivers/storage_controller/`. LetRecovery never recursively stages the
whole directory: `lr-core::storage_driver_match` selects a package only when
SetupAPI reports a matching Intel PCI hardware ID. AMD, Apple, VirtIO and
unknown controllers select nothing. The managed/dummy `8086:09AB` function is
not generation-defining and is rejected when no primary VMD controller ID is
visible; it is never used to guess a package.

| Package path | Version | Microsoft Catalog update ID | Covered primary IDs | Source CAB SHA-256 |
| --- | --- | --- | --- | --- |
| `intel-vmd-11th` | 20.2.4.1019 | `d4c52691-b507-4a37-bce7-b018cd40b4d9` | `8086:9A0B` (plus managed `09AB`) | `913A94E9E292EA984F9150D093456FF8595E6CF4AEA3943801A5F2801781E00D` |
| `intel-vmd-current` | 20.2.12.1036 | `d3ccf9fc-2543-4b7b-9ff0-369264a693be` | `8086:467F`, `A77F`, `7D0B`, `AD0B` | `A5DCE6B59B3775D2F0519EECA69A5EF8754B0AB147474377C2684C6D9E8B47D9` |

The source catalog searches, exact per-file SHA-256 values, signature notes and
Intel license links are retained in each package's `NOTICE.txt`. The CAT and SYS
files were verified as `Valid` and issued by Microsoft Windows Hardware
Compatibility Publisher before packaging. The INF itself is catalog-signed and
therefore does not carry a standalone Authenticode signature.

`docs/STORAGE_CONTROLLER_DRIVERS.lock.json` is the machine-readable release
lock for every packaged file, size, SHA-256, supported primary controller ID and
expected signer. Release validation rejects missing, extra or modified files,
synchronizes the locked tree into the PE WIM and then mounts the final cleaned
WIM read-only to repeat the same hash and signature checks.

The retired blanket storage-controller package directories (`18`, `19`, `20`,
`AMD`, `Applessd`, `iastorE` and `viostor`) and UefiSeven must not be restored.

Windows 7 USB3 compatibility support is sourced from the user-supplied legacy
LetRecovery package, but only the 13 package directories whose catalogs validate
as Microsoft Windows Hardware Compatibility Publisher are retained. Modified or
expired kernel-policy packages (`amdXHCI`, `intel9th+`, `IntelCeousb3` and
`Intelcommonusb3`) were excluded. Runtime selection first verifies every byte,
then uses SetupAPI hardware IDs and the applied image architecture to inject only
matching package directories. The Windows 7 NVMe payload contains only Microsoft's
x64 KB2990941 v3 and KB3087873 v2 CABs and installs them in that order; loose
legacy INF/SYS files are excluded. Exact membership, sizes and SHA-256 values are
recorded in `docs/WINDOWS7_DRIVERS.lock.json`, and release validation checks the
source tree, normal package, injected PE WIM and signer subjects. XP/2003-specific
driver resources remain separate.
