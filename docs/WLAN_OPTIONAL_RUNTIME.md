# Optional WLAN runtime

WePE may omit `wlanapi.dll`. Direct calls through windows 0.58 generate a PE import
and prevent the normal executable from starting, before any feature check runs.
`native_wifi.rs` now resolves all six WLAN exports at runtime from the Windows
system directory. Missing DLLs, exports, or an unavailable WLAN service disable
Wi-Fi discovery/migration through the existing feature-availability callers.
Explicit capture returns an error and the callers clear the migration selection.

The library is owned by the WLAN client. Allocated buffers borrow that library;
`WlanFreeMemory` runs before the client closes and `WlanCloseHandle` runs before
the library unloads. No WLAN API is called from a loader callback.

Contract checked against Microsoft documentation and windows 0.58 declarations:

- [WlanOpenHandle](https://learn.microsoft.com/en-us/windows/win32/api/wlanapi/nf-wlanapi-wlanopenhandle): version 2 supports Vista and later, including Windows 7; reserved pointer is NULL; DWORD status is zero on success.
- [WlanCloseHandle](https://learn.microsoft.com/en-us/windows/win32/api/wlanapi/nf-wlanapi-wlanclosehandle): closes the opened session, reserved pointer NULL, never called from a notification callback or DllMain.
- [WlanEnumInterfaces](https://learn.microsoft.com/en-us/windows/win32/api/wlanapi/nf-wlanapi-wlanenuminterfaces): returned interface list belongs to WLAN and must be freed with WlanFreeMemory.
- [WlanQueryInterface](https://learn.microsoft.com/en-us/windows/win32/api/wlanapi/nf-wlanapi-wlanqueryinterface): current_connection returns WLAN_CONNECTION_ATTRIBUTES; size is in bytes; opcode-value-type output may be NULL.
- [WlanGetProfile](https://learn.microsoft.com/en-us/windows/win32/api/wlanapi/nf-wlanapi-wlangetprofile): UTF-16 name and XML, flags are in/out; requesting plaintext is supported on Windows 7+, but success can still return encrypted key material. Existing XML validation remains required.
- [WlanFreeMemory](https://learn.microsoft.com/en-us/windows/win32/api/wlanapi/nf-wlanapi-wlanfreememory): returns void; only frees the WLAN-owned allocation once.

These are synchronous calls; there is no asynchronous result or file-sharing mode.
The structs and opcode newtypes continue to come from the locked windows crate.
The first-logon C# WLAN imports in lr-core are resolved by the CLR only when that
optional restore code executes on the installed system; they do not enter the EXE
import table. The PE endpoint has no direct native WLAN calls.

Validation must inspect the production EXE import table (not only the test build,
whose non-elevated-tests feature excludes host WLAN discovery), and confirm that
`wlanapi.dll` is absent. Actual WePE startup still requires testing on that image.
