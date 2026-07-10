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
