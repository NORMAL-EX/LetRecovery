use anyhow::Result;

#[cfg(windows)]
use windows::{
    core::PCWSTR,
    Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_DWORD, REG_SZ,
    },
};

#[derive(Debug, Clone)]
pub struct SystemInfo {
    pub boot_mode: BootMode,
    pub tpm_enabled: bool,
    pub tpm_version: String,
    pub secure_boot: bool,
    pub is_pe_environment: bool,
    pub is_64bit: bool,
    pub is_online: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BootMode {
    UEFI,
    Legacy,
}

impl std::fmt::Display for BootMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BootMode::UEFI => write!(f, "UEFI"),
            BootMode::Legacy => write!(f, "Legacy"),
        }
    }
}

/// 直接调用 kernel32.dll 的 GetFirmwareEnvironmentVariableW
#[cfg(windows)]
mod kernel32 {
    #[link(name = "kernel32")]
    extern "system" {
        pub fn GetFirmwareEnvironmentVariableW(
            lpName: *const u16,
            lpGuid: *const u16,
            pBuffer: *mut u8,
            nSize: u32,
        ) -> u32;
    }
}

impl SystemInfo {
    pub fn collect() -> Result<Self> {
        let is_pe = Self::check_pe_environment();
        let boot_mode = Self::get_boot_mode()?;
        let (tpm_enabled, tpm_version) = Self::get_tpm_info();
        let secure_boot = Self::get_secure_boot().unwrap_or(false);
        let is_online = Self::check_network();

        Ok(Self {
            boot_mode,
            tpm_enabled,
            tpm_version,
            secure_boot,
            is_pe_environment: is_pe,
            is_64bit: cfg!(target_arch = "x86_64"),
            is_online,
        })
    }

    /// 使用 Windows API 检测启动模式
    #[cfg(windows)]
    fn get_boot_mode() -> Result<BootMode> {
        // 使用 GetFirmwareEnvironmentVariableW API 检测
        // 这个 API 在 Legacy BIOS 下会返回 ERROR_INVALID_FUNCTION (1)
        // 在 UEFI 模式下会返回 ERROR_NOACCESS (998) 或其他错误（因为我们查询的是空变量）
        unsafe {
            let name: Vec<u16> = "".encode_utf16().chain(std::iter::once(0)).collect();
            let guid: Vec<u16> = "{00000000-0000-0000-0000-000000000000}"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let mut buffer = [0u8; 1];

            let result = kernel32::GetFirmwareEnvironmentVariableW(
                name.as_ptr(),
                guid.as_ptr(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
            );

            // 如果返回 0，检查错误码
            if result == 0 {
                let error = std::io::Error::last_os_error();
                let raw_error = error.raw_os_error().unwrap_or(0) as u32;
                
                // ERROR_INVALID_FUNCTION (1) 表示是 Legacy BIOS
                // 这是最可靠的判断方式
                if raw_error == 1 {
                    return Ok(BootMode::Legacy);
                }
                // 其他错误（如 ERROR_NOACCESS 998, ERROR_ENVVAR_NOT_FOUND 203）表示是 UEFI
                return Ok(BootMode::UEFI);
            }

            // 如果调用成功（不太可能发生，因为我们查询的是空变量），说明是 UEFI
            Ok(BootMode::UEFI)
        }
    }

    #[cfg(not(windows))]
    fn get_boot_mode() -> Result<BootMode> {
        Ok(BootMode::Legacy)
    }

    /// 获取 TPM 信息（使用 WMI 和注册表）
    #[cfg(windows)]
    fn get_tpm_info() -> (bool, String) {
        // 方法1: 使用 WMI 查询 Win32_Tpm 类（最可靠）
        if let Some((enabled, version)) = Self::get_tpm_via_wmi() {
            return (enabled, version);
        }
        
        // 方法2: 检查 TPM 设备注册表项
        if let Some((enabled, version)) = Self::get_tpm_via_registry() {
            return (enabled, version);
        }
        
        (false, String::new())
    }
    
    /// 通过 WMI 查询 TPM 状态
    #[cfg(windows)]
    fn get_tpm_via_wmi() -> Option<(bool, String)> {
        use windows::core::BSTR;
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CoSetProxyBlanket, CoUninitialize,
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, EOAC_NONE,
            RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE,
        };
        use windows::Win32::System::Wmi::{
            IWbemLocator, WbemLocator, WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY,
        };
        
        const RPC_C_AUTHN_DEFAULT: u32 = 0xFFFFFFFF;
        const RPC_C_AUTHZ_NONE: u32 = 0;
        
        unsafe {
            // 初始化 COM
            let com_init = CoInitializeEx(None, COINIT_MULTITHREADED);
            let should_uninit = com_init.is_ok();
            
            let result = (|| -> Option<(bool, String)> {
                // 创建 WMI 定位器
                let locator: IWbemLocator = CoCreateInstance(
                    &WbemLocator,
                    None,
                    CLSCTX_INPROC_SERVER,
                ).ok()?;
                
                // 连接到 TPM 命名空间
                let namespace = BSTR::from("ROOT\\CIMV2\\Security\\MicrosoftTpm");
                let services = locator.ConnectServer(
                    &namespace,
                    &BSTR::new(),
                    &BSTR::new(),
                    &BSTR::new(),
                    0,
                    &BSTR::new(),
                    None,
                ).ok()?;
                
                // 设置代理安全级别
                CoSetProxyBlanket(
                    &services,
                    RPC_C_AUTHN_DEFAULT,
                    RPC_C_AUTHZ_NONE,
                    None,
                    RPC_C_AUTHN_LEVEL_CALL,
                    RPC_C_IMP_LEVEL_IMPERSONATE,
                    None,
                    EOAC_NONE,
                ).ok()?;
                
                // 查询 Win32_Tpm
                let query_lang = BSTR::from("WQL");
                let query = BSTR::from("SELECT * FROM Win32_Tpm");
                
                let enumerator = services.ExecQuery(
                    &query_lang,
                    &query,
                    WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                    None,
                ).ok()?;
                
                let mut objects = [None];
                let mut returned: u32 = 0;
                
                if enumerator.Next(5000, &mut objects, &mut returned).is_ok() && returned > 0 {
                    if let Some(obj) = objects[0].take() {
                        // 获取 IsEnabled_InitialValue 属性
                        let prop_name = BSTR::from("IsEnabled_InitialValue");
                        let mut value = windows::core::VARIANT::default();
                        
                        let is_enabled = if obj.Get(&prop_name, 0, &mut value, None, None).is_ok() {
                            // 尝试获取布尔值
                            bool::try_from(&value).unwrap_or(false)
                        } else {
                            // 如果能查询到 Win32_Tpm 对象，说明 TPM 存在
                            true
                        };
                        
                        // 获取 SpecVersion 属性
                        let spec_prop = BSTR::from("SpecVersion");
                        let mut spec_value = windows::core::VARIANT::default();
                        
                        let version = if obj.Get(&spec_prop, 0, &mut spec_value, None, None).is_ok() {
                            if let Ok(bstr) = BSTR::try_from(&spec_value) {
                                let full_version = bstr.to_string();
                                // SpecVersion 格式通常是 "2.0, 0, 1.59" 取第一部分
                                full_version.split(',').next().unwrap_or("").trim().to_string()
                            } else {
                                "2.0".to_string()
                            }
                        } else {
                            "2.0".to_string()
                        };
                        
                        return Some((is_enabled, version));
                    }
                }
                
                None
            })();
            
            if should_uninit {
                CoUninitialize();
            }
            
            result
        }
    }
    
    /// 通过注册表检测 TPM
    #[cfg(windows)]
    fn get_tpm_via_registry() -> Option<(bool, String)> {
        unsafe {
            // 检查 TPM 2.0 设备
            let subkey_20: Vec<u16> = "SYSTEM\\CurrentControlSet\\Enum\\ACPI\\MSFT0101"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            
            let mut hkey = HKEY::default();
            let result = RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                PCWSTR::from_raw(subkey_20.as_ptr()),
                0,
                KEY_READ,
                &mut hkey,
            );
            
            if result.is_ok() {
                let _ = RegCloseKey(hkey);
                // TPM 2.0 设备存在
                // 检查是否启用
                let is_enabled = Self::check_tpm_enabled_registry();
                return Some((is_enabled, "2.0".to_string()));
            }
            
            // 检查 TPM 1.2 设备
            let subkey_12: Vec<u16> = "SYSTEM\\CurrentControlSet\\Enum\\Root\\SecurityDevices\\0000"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            
            let result = RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                PCWSTR::from_raw(subkey_12.as_ptr()),
                0,
                KEY_READ,
                &mut hkey,
            );
            
            if result.is_ok() {
                let _ = RegCloseKey(hkey);
                let is_enabled = Self::check_tpm_enabled_registry();
                return Some((is_enabled, "1.2".to_string()));
            }
            
            None
        }
    }
    
    /// 检查 TPM 是否启用（通过 SOFTWARE\Microsoft\Tpm 注册表键）
    #[cfg(windows)]
    fn check_tpm_enabled_registry() -> bool {
        unsafe {
            let subkey: Vec<u16> = "SOFTWARE\\Microsoft\\Tpm"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            
            let mut hkey = HKEY::default();
            let result = RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                PCWSTR::from_raw(subkey.as_ptr()),
                0,
                KEY_READ,
                &mut hkey,
            );
            
            if result.is_err() {
                return false;
            }
            
            // 检查 SpecVersion 是否存在（如果存在说明 TPM 已被初始化/启用）
            let value_name: Vec<u16> = "SpecVersion"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            
            let mut buffer = [0u8; 256];
            let mut buffer_size = buffer.len() as u32;
            let mut value_type = REG_SZ;
            
            let result = RegQueryValueExW(
                hkey,
                PCWSTR::from_raw(value_name.as_ptr()),
                None,
                Some(&mut value_type),
                Some(buffer.as_mut_ptr()),
                Some(&mut buffer_size),
            );
            
            let _ = RegCloseKey(hkey);
            
            result.is_ok() && buffer_size > 0
        }
    }
    
    #[cfg(not(windows))]
    fn get_tpm_info() -> (bool, String) {
        (false, String::new())
    }

    /// 使用注册表 API 检测安全启动状态
    #[cfg(windows)]
    fn get_secure_boot() -> Result<bool> {
        unsafe {
            let subkey: Vec<u16> = "SYSTEM\\CurrentControlSet\\Control\\SecureBoot\\State"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();

            let mut hkey = HKEY::default();
            let result = RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                PCWSTR::from_raw(subkey.as_ptr()),
                0,
                KEY_READ,
                &mut hkey,
            );

            if result.is_err() {
                return Ok(false);
            }

            let value_name: Vec<u16> = "UEFISecureBootEnabled"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();

            let mut data = [0u32; 1];
            let mut data_size = std::mem::size_of::<u32>() as u32;
            let mut data_type = REG_DWORD;

            let result = RegQueryValueExW(
                hkey,
                PCWSTR::from_raw(value_name.as_ptr()),
                None,
                Some(&mut data_type),
                Some(data.as_mut_ptr() as *mut u8),
                Some(&mut data_size),
            );

            let _ = RegCloseKey(hkey);

            if result.is_ok() {
                Ok(data[0] == 1)
            } else {
                Ok(false)
            }
        }
    }

    #[cfg(not(windows))]
    fn get_secure_boot() -> Result<bool> {
        Ok(false)
    }

    pub fn check_pe_environment() -> bool {
        // 特征1: fbwf.sys (File-Based Write Filter)
        if std::path::Path::new("X:\\Windows\\System32\\drivers\\fbwf.sys").exists() {
            return true;
        }

        // 特征2: winpeshl.ini
        if std::path::Path::new("X:\\Windows\\System32\\winpeshl.ini").exists() {
            return true;
        }

        // 特征3: 系统盘是 X:
        if let Ok(system_drive) = std::env::var("SystemDrive") {
            if system_drive.to_uppercase() == "X:" {
                return true;
            }
        }

        // 特征4: 检查 MININT 目录
        if std::path::Path::new("X:\\MININT").exists() {
            return true;
        }

        // 特征5: 检查 MiniNT 注册表键
        #[cfg(windows)]
        {
            if Self::check_minint_registry() {
                return true;
            }
        }

        // 特征6: 检查 SystemDrive 下的 PE 特征文件
        if let Ok(system_drive) = std::env::var("SystemDrive") {
            let fbwf_path = format!("{}\\Windows\\System32\\drivers\\fbwf.sys", system_drive);
            let winpeshl_path = format!("{}\\Windows\\System32\\winpeshl.ini", system_drive);
            if std::path::Path::new(&fbwf_path).exists()
                || std::path::Path::new(&winpeshl_path).exists()
            {
                return true;
            }
        }

        false
    }

    /// 检查 MiniNT 注册表键（PE 环境特征）
    #[cfg(windows)]
    fn check_minint_registry() -> bool {
        unsafe {
            let subkey: Vec<u16> = "SYSTEM\\CurrentControlSet\\Control\\MiniNT"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();

            let mut hkey = HKEY::default();
            let result = RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                PCWSTR::from_raw(subkey.as_ptr()),
                0,
                KEY_READ,
                &mut hkey,
            );

            if result.is_ok() {
                let _ = RegCloseKey(hkey);
                return true;
            }

            false
        }
    }

    fn check_network() -> bool {
        let addresses = [
            "223.5.5.5:53",
            "119.29.29.29:53",
            "8.8.8.8:53",
            "1.1.1.1:53",
        ];

        for addr in &addresses {
            if let Ok(addr) = addr.parse() {
                if std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(2))
                    .is_ok()
                {
                    return true;
                }
            }
        }

        false
    }
}
