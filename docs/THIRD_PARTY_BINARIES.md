# Third-Party Binary Provenance

LetRecovery ships the following prebuilt binary. Update it only after reviewing
the upstream release notes, licenses, and hashes. Hashes in this file are for
the exact bytes committed to this repository.

## wimlib

| Field | Value |
| --- | --- |
| Repository path | `lr-core/vendor/libwim-15.dll` |
| Upstream project | [wimlib](https://wimlib.net/) |
| Upstream release | 1.14.4, Windows x86_64 binary distribution |
| Source archive | `wimlib-1.14.4-windows-x86_64-bin.zip` |
| Source URL | `https://wimlib.net/downloads/wimlib-1.14.4-windows-x86_64-bin.zip` |
| Source archive SHA-256 | `6D99E242BFBC6D36FC987D433D63772180551B7F2D8DE43E9561535A3E2C16D8` |
| Committed DLL SHA-256 | `6480B53D4ECD4423AF9E100FE15E3D2C3D114EFF33FBA07977E46C1AB124342E` |
| License used for `libwim` | GNU Lesser General Public License v3.0 or later |
| Additional bundled notice | `libdivsufsort-lite` license from the upstream binary distribution |

The DLL hash matches the `libwim-15.dll` contained in the official 1.14.4
Windows x86_64 archive. The Windows build does not link to `libntfs-3g`, so the
upstream license notice permits use of the LGPLv3-or-later option for `libwim`.
The original notices are retained under
`docs/third-party/wimlib-1.14.4/`.

### Update procedure

1. Download a specific upstream Windows x86_64 release over HTTPS.
2. Verify the archive SHA-256 against the checksum published by wimlib.
3. Extract `libwim-15.dll`, record its SHA-256, and replace the repository copy.
4. Copy the release's license notices without modification.
5. Update this document and run the Rust workspace tests before release.

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
