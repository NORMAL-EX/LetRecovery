/*
 * Generated/audited inventory of APIs resolved at runtime by LetRecovery.
 * Keep entries grouped by the DLL passed to Library::new/LoadLibrary/GetProcAddress.
 * FEATURE means the named feature cannot work without the export; OPTIONAL means the
 * source has an explicit fallback. Neither category is a process-loader dependency.
 */
#define DYNAMIC_FEATURE 1
#define DYNAMIC_OPTIONAL 0

static const DYNAMIC_API dynamic_apis[] = {
    {"setupapi.dll", "SetupDiGetClassDevsW", DYNAMIC_FEATURE, "device and driver enumeration"},
    {"setupapi.dll", "SetupDiEnumDeviceInfo", DYNAMIC_FEATURE, "device and driver enumeration"},
    {"setupapi.dll", "SetupDiGetDeviceRegistryPropertyW", DYNAMIC_FEATURE, "device and driver enumeration"},
    {"setupapi.dll", "SetupDiGetDevicePropertyW", DYNAMIC_OPTIONAL, "modern device properties"},
    {"setupapi.dll", "SetupDiDestroyDeviceInfoList", DYNAMIC_FEATURE, "device and driver enumeration"},
    {"setupapi.dll", "SetupCopyOEMInfW", DYNAMIC_FEATURE, "driver package staging"},
    {"setupapi.dll", "SetupGetInfDriverStoreLocationW", DYNAMIC_OPTIONAL, "OEM driver export"},
    {"newdev.dll", "DiInstallDriverW", DYNAMIC_FEATURE, "online driver installation"},

    {"wintrust.dll", "CryptCATAdminAcquireContext2", DYNAMIC_FEATURE, "driver catalog verification", "wintrust.dll", "CryptCATAdminAcquireContext"},
    {"wintrust.dll", "CryptCATAdminCalcHashFromFileHandle2", DYNAMIC_FEATURE, "driver catalog verification", "wintrust.dll", "CryptCATAdminCalcHashFromFileHandle"},
    {"wintrust.dll", "CryptCATAdminReleaseContext", DYNAMIC_FEATURE, "driver catalog verification"},

    {"fveapi.dll", "FveOpenVolumeW", DYNAMIC_FEATURE, "BitLocker recovery"},
    {"fveapi.dll", "FveCloseVolume", DYNAMIC_FEATURE, "BitLocker recovery"},
    {"fveapi.dll", "FveUnlockVolume", DYNAMIC_FEATURE, "BitLocker recovery"},
    {"fveapi.dll", "FveAuthElementFromRecoveryPasswordW", DYNAMIC_FEATURE, "BitLocker recovery-password unlock"},
    {"fveapi.dll", "FveGetStatusW", DYNAMIC_OPTIONAL, "BitLocker status query"},
    {"fveapi.dll", "FveGetStatus", DYNAMIC_OPTIONAL, "BitLocker legacy status query"},
    {"fveapi.dll", "FveConversionDecrypt", DYNAMIC_OPTIONAL, "BitLocker decryption"},
    {"fveapi.dll", "FveAuthElementFromPassPhraseW", DYNAMIC_OPTIONAL, "BitLocker passphrase unlock"},
    {"fveapi.dll", "FveUnlockVolumeWithAccessMode", DYNAMIC_OPTIONAL, "BitLocker access-mode unlock"},
    {"fveapi.dll", "InternalFveIsVolumeEncrypted", DYNAMIC_OPTIONAL, "BitLocker encryption probe"},
    {"advapi32.dll", "OpenProcessToken", DYNAMIC_FEATURE, "BitLocker privilege enablement"},
    {"advapi32.dll", "LookupPrivilegeValueW", DYNAMIC_FEATURE, "BitLocker privilege enablement"},
    {"advapi32.dll", "AdjustTokenPrivileges", DYNAMIC_FEATURE, "BitLocker privilege enablement"},
    {"kernel32.dll", "CloseHandle", DYNAMIC_FEATURE, "BitLocker privilege enablement"},
    {"kernel32.dll", "GetCurrentProcess", DYNAMIC_FEATURE, "BitLocker privilege enablement"},
    {"kernel32.dll", "GetLastError", DYNAMIC_OPTIONAL, "BitLocker diagnostics"},
    {"kernel32.dll", "GetFirmwareType", DYNAMIC_OPTIONAL, "firmware mode probe"},
    {"ole32.dll", "CoTaskMemFree", DYNAMIC_FEATURE, "VDS COM memory release"},

    {"wimgapi.dll", "WIMCreateFile", DYNAMIC_FEATURE, "Microsoft WIMGAPI engine"},
    {"wimgapi.dll", "WIMCloseHandle", DYNAMIC_FEATURE, "Microsoft WIMGAPI engine"},
    {"wimgapi.dll", "WIMSetTemporaryPath", DYNAMIC_FEATURE, "Microsoft WIMGAPI engine"},
    {"wimgapi.dll", "WIMLoadImage", DYNAMIC_FEATURE, "Microsoft WIMGAPI engine"},
    {"wimgapi.dll", "WIMApplyImage", DYNAMIC_FEATURE, "Microsoft WIMGAPI apply"},
    {"wimgapi.dll", "WIMCaptureImage", DYNAMIC_FEATURE, "Microsoft WIMGAPI capture"},
    {"wimgapi.dll", "WIMGetImageCount", DYNAMIC_FEATURE, "Microsoft WIMGAPI metadata"},
    {"wimgapi.dll", "WIMSetImageInformation", DYNAMIC_FEATURE, "Microsoft WIMGAPI metadata"},
    {"wimgapi.dll", "WIMRegisterMessageCallback", DYNAMIC_FEATURE, "Microsoft WIMGAPI progress"},
    {"wimgapi.dll", "WIMUnregisterMessageCallback", DYNAMIC_FEATURE, "Microsoft WIMGAPI progress"},

    {"@app:libwim-15.dll", "wimlib_global_init", DYNAMIC_FEATURE, "built-in libwim engine"},
    {"@app:libwim-15.dll", "wimlib_global_cleanup", DYNAMIC_FEATURE, "built-in libwim engine"},
    {"@app:libwim-15.dll", "wimlib_open_wim_with_progress", DYNAMIC_FEATURE, "built-in libwim open"},
    {"@app:libwim-15.dll", "wimlib_create_new_wim", DYNAMIC_FEATURE, "built-in libwim capture"},
    {"@app:libwim-15.dll", "wimlib_free", DYNAMIC_FEATURE, "built-in libwim engine"},
    {"@app:libwim-15.dll", "wimlib_verify_wim", DYNAMIC_FEATURE, "built-in libwim verification"},
    {"@app:libwim-15.dll", "wimlib_get_error_string", DYNAMIC_FEATURE, "built-in libwim diagnostics"},
    {"@app:libwim-15.dll", "wimlib_get_wim_info", DYNAMIC_FEATURE, "built-in libwim metadata"},
    {"@app:libwim-15.dll", "wimlib_get_image_name", DYNAMIC_FEATURE, "built-in libwim metadata"},
    {"@app:libwim-15.dll", "wimlib_get_image_description", DYNAMIC_FEATURE, "built-in libwim metadata"},
    {"@app:libwim-15.dll", "wimlib_reference_resource_files", DYNAMIC_FEATURE, "built-in libwim split images"},
    {"@app:libwim-15.dll", "wimlib_register_progress_function", DYNAMIC_FEATURE, "built-in libwim progress"},
    {"@app:libwim-15.dll", "wimlib_extract_image", DYNAMIC_FEATURE, "built-in libwim apply"},
    {"@app:libwim-15.dll", "wimlib_extract_paths", DYNAMIC_FEATURE, "built-in libwim extraction"},
    {"@app:libwim-15.dll", "wimlib_add_image", DYNAMIC_FEATURE, "built-in libwim capture"},
    {"@app:libwim-15.dll", "wimlib_write", DYNAMIC_FEATURE, "built-in libwim write"},
    {"@app:libwim-15.dll", "wimlib_overwrite", DYNAMIC_FEATURE, "built-in libwim write"},
    {"@app:libwim-15.dll", "wimlib_set_output_compression_type", DYNAMIC_FEATURE, "built-in libwim compression"},
    {"@app:libwim-15.dll", "wimlib_set_image_property", DYNAMIC_FEATURE, "built-in libwim metadata"},
    {"@app:libwim-15.dll", "wimlib_split", DYNAMIC_FEATURE, "built-in libwim split"},
    {"@app:libwim-15.dll", "wimlib_iterate_dir_tree", DYNAMIC_FEATURE, "built-in libwim tree iteration"},
    {"@app:libwim-15.dll", "wimlib_get_xml_data", DYNAMIC_FEATURE, "built-in libwim XML metadata"},
    {"@app:libwim-15.dll", "wimlib_update_image", DYNAMIC_FEATURE, "built-in libwim update"},
    {"@app:libwim-15.dll", "wimlib_set_parallel_decompression", DYNAMIC_OPTIONAL, "parallel libwim decompression"},

    {"d3d11.dll", "D3D11CreateDevice", DYNAMIC_OPTIONAL, "composited scrollbar"},
    {"d2d1.dll", "D2D1CreateDevice", DYNAMIC_OPTIONAL, "composited scrollbar"},
    {"dcomp.dll", "DCompositionCreateDevice", DYNAMIC_OPTIONAL, "composited scrollbar"}
};

static const WCHAR *owned_modules[] = {
    L"libwim-15.dll",
    L"vcruntime140.dll",
    L"vcruntime140_1.dll"
};
