use anyhow::Result;
use std::path::Path;

#[cfg(windows)]
use windows::{
    core::PCWSTR,
    Win32::Foundation::{CloseHandle, HANDLE, WIN32_ERROR, INVALID_HANDLE_VALUE},
    Win32::Storage::FileSystem::{
        CreateFileW, GetDriveTypeW, GetLogicalDrives, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    },
    Win32::System::IO::DeviceIoControl,
    Win32::System::Ioctl::IOCTL_STORAGE_EJECT_MEDIA,
    Win32::Storage::Vhd::{
        AttachVirtualDisk, DetachVirtualDisk, GetVirtualDiskPhysicalPath, OpenVirtualDisk,
        ATTACH_VIRTUAL_DISK_FLAG_PERMANENT_LIFETIME, ATTACH_VIRTUAL_DISK_FLAG_READ_ONLY,
        DETACH_VIRTUAL_DISK_FLAG_NONE,
        OPEN_VIRTUAL_DISK_FLAG_NONE, OPEN_VIRTUAL_DISK_PARAMETERS, OPEN_VIRTUAL_DISK_VERSION_1,
        VIRTUAL_DISK_ACCESS_DETACH, VIRTUAL_DISK_ACCESS_READ, 
        VIRTUAL_STORAGE_TYPE, VIRTUAL_STORAGE_TYPE_DEVICE_ISO,
    },
};

#[cfg(windows)]
const DRIVE_CDROM: u32 = 5;

#[cfg(windows)]
const VIRTUAL_STORAGE_TYPE_VENDOR_MICROSOFT: windows::core::GUID = windows::core::GUID::from_u128(
    0xEC984AEC_A0F9_47e9_901F_71415A66345B,
);

pub struct IsoMounter {
    #[cfg(windows)]
    handle: Option<HANDLE>,
}

impl IsoMounter {
    pub fn new() -> Self {
        Self {
            #[cfg(windows)]
            handle: None,
        }
    }

    fn is_pe_environment() -> bool {
        crate::core::system_info::SystemInfo::check_pe_environment()
    }

    /// 获取当前所有逻辑驱动器的位掩码
    #[cfg(windows)]
    fn get_logical_drives_mask() -> u32 {
        unsafe { GetLogicalDrives() }
    }

    /// 根据位掩码找出新增的 CDROM 驱动器盘符
    #[cfg(windows)]
    fn find_new_cdrom_drive(before_mask: u32) -> Option<char> {
        let after_mask = Self::get_logical_drives_mask();
        let new_drives = after_mask & !before_mask; // 找出新增的盘符
        
        println!("[ISO] 挂载前盘符掩码: 0x{:08X}, 挂载后: 0x{:08X}, 新增: 0x{:08X}", 
                 before_mask, after_mask, new_drives);
        
        // 从 D 到 Z 检查新增的盘符
        for i in 3..26u8 { // D=3, E=4, ..., Z=25
            let bit = 1u32 << i;
            if new_drives & bit != 0 {
                let letter = (b'A' + i) as char;
                let drive_path = format!("{}:\\", letter);
                let wide_path: Vec<u16> = drive_path.encode_utf16().chain(std::iter::once(0)).collect();
                
                unsafe {
                    let drive_type = GetDriveTypeW(PCWSTR::from_raw(wide_path.as_ptr()));
                    println!("[ISO] 检查新盘符 {}: 类型={}", letter, drive_type);
                    
                    if drive_type == DRIVE_CDROM {
                        return Some(letter);
                    }
                }
            }
        }
        
        None
    }

    /// 使用 Windows API 挂载 ISO 并返回盘符
    #[cfg(windows)]
    pub fn mount_iso_winapi(iso_path: &str) -> Result<char> {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Storage::Vhd::{
            ATTACH_VIRTUAL_DISK_PARAMETERS, ATTACH_VIRTUAL_DISK_VERSION_1,
        };

        println!("[ISO] 使用 Windows API 挂载 ISO: {}", iso_path);

        // 1. 记录挂载前的盘符掩码
        let before_mask = Self::get_logical_drives_mask();
        println!("[ISO] 挂载前盘符掩码: 0x{:08X}", before_mask);

        // 转换路径为宽字符
        let wide_path: Vec<u16> = OsStr::new(iso_path)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            // 设置存储类型为 ISO
            let storage_type = VIRTUAL_STORAGE_TYPE {
                DeviceId: VIRTUAL_STORAGE_TYPE_DEVICE_ISO,
                VendorId: VIRTUAL_STORAGE_TYPE_VENDOR_MICROSOFT,
            };

            // 设置打开参数 (ISO 必须使用 V1)
            let mut open_params: OPEN_VIRTUAL_DISK_PARAMETERS = std::mem::zeroed();
            open_params.Version = OPEN_VIRTUAL_DISK_VERSION_1;

            // 打开虚拟磁盘
            let mut handle: HANDLE = HANDLE::default();
            let result = OpenVirtualDisk(
                &storage_type,
                PCWSTR::from_raw(wide_path.as_ptr()),
                VIRTUAL_DISK_ACCESS_READ,
                OPEN_VIRTUAL_DISK_FLAG_NONE,
                Some(&open_params),
                &mut handle,
            );

            if result != WIN32_ERROR(0) {
                println!("[ISO] OpenVirtualDisk 失败: {:?}", result);
                anyhow::bail!("OpenVirtualDisk 失败: {:?}", result);
            }

            println!("[ISO] OpenVirtualDisk 成功, handle: {:?}", handle);

            // 设置挂载参数
            let mut attach_params: ATTACH_VIRTUAL_DISK_PARAMETERS = std::mem::zeroed();
            attach_params.Version = ATTACH_VIRTUAL_DISK_VERSION_1;

            // 挂载虚拟磁盘 (只读, 自动分配盘符, 永久生命周期)
            use windows::Win32::Storage::Vhd::ATTACH_VIRTUAL_DISK_FLAG;
            let attach_flags = ATTACH_VIRTUAL_DISK_FLAG(
                ATTACH_VIRTUAL_DISK_FLAG_READ_ONLY.0 | ATTACH_VIRTUAL_DISK_FLAG_PERMANENT_LIFETIME.0
            );

            let result = AttachVirtualDisk(
                handle,
                None,
                attach_flags,
                0,
                Some(&attach_params),
                None,
            );

            if result != WIN32_ERROR(0) {
                println!("[ISO] AttachVirtualDisk 失败: {:?}", result);
                let _ = CloseHandle(handle);
                anyhow::bail!("AttachVirtualDisk 失败: {:?}", result);
            }

            println!("[ISO] AttachVirtualDisk 成功");

            // 获取挂载的物理路径 (可选，用于调试)
            let mut path_buffer = [0u16; 260];
            let mut path_size = (path_buffer.len() * 2) as u32;
            let result = GetVirtualDiskPhysicalPath(
                handle, 
                &mut path_size, 
                windows::core::PWSTR::from_raw(path_buffer.as_mut_ptr())
            );

            if result == WIN32_ERROR(0) {
                let path = String::from_utf16_lossy(&path_buffer[..path_size as usize / 2]);
                println!("[ISO] 物理路径: {}", path.trim_end_matches('\0'));
            }

            // 关闭句柄 (因为使用了 PERMANENT_LIFETIME，ISO 会保持挂载)
            let _ = CloseHandle(handle);

            // 2. 轮询等待新盘符出现（最多10次，每次500ms，共5秒）
            for i in 0..10 {
                std::thread::sleep(std::time::Duration::from_millis(500));
                
                if let Some(letter) = Self::find_new_cdrom_drive(before_mask) {
                    // 3. 验证是否包含 Windows 安装文件
                    let sources_path = format!("{}:\\sources", letter);
                    if Path::new(&sources_path).exists() {
                        println!("[ISO] 挂载成功，盘符: {}:，第 {} 次检测", letter, i + 1);
                        return Ok(letter);
                    } else {
                        println!("[ISO] 找到新 CDROM 盘符 {}: 但不包含 sources 目录", letter);
                    }
                }
                
                println!("[ISO] 等待盘符分配... ({}/10)", i + 1);
            }

            // 4. 如果轮询失败，使用后备方案：遍历所有 CDROM 盘符
            println!("[ISO] 轮询超时，尝试后备方案...");
            if let Some(drive) = Self::find_iso_drive() {
                let letter = drive.chars().next().unwrap();
                println!("[ISO] 后备方案找到盘符: {}", drive);
                return Ok(letter);
            }

            anyhow::bail!("ISO 挂载后无法找到盘符，请手动检查")
        }
    }

    /// 使用 Windows API 卸载指定 ISO
    #[cfg(windows)]
    pub fn unmount_iso_by_path(iso_path: &str) -> Result<()> {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;

        println!("[ISO] 使用 Windows API 卸载 ISO: {}", iso_path);

        let wide_path: Vec<u16> = OsStr::new(iso_path)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            let storage_type = VIRTUAL_STORAGE_TYPE {
                DeviceId: VIRTUAL_STORAGE_TYPE_DEVICE_ISO,
                VendorId: VIRTUAL_STORAGE_TYPE_VENDOR_MICROSOFT,
            };

            let mut open_params: OPEN_VIRTUAL_DISK_PARAMETERS = std::mem::zeroed();
            open_params.Version = OPEN_VIRTUAL_DISK_VERSION_1;

            let mut handle: HANDLE = HANDLE::default();
            let result = OpenVirtualDisk(
                &storage_type,
                PCWSTR::from_raw(wide_path.as_ptr()),
                VIRTUAL_DISK_ACCESS_DETACH,
                OPEN_VIRTUAL_DISK_FLAG_NONE,
                Some(&open_params),
                &mut handle,
            );

            if result != WIN32_ERROR(0) {
                anyhow::bail!("OpenVirtualDisk 失败: {:?}", result);
            }

            let result = DetachVirtualDisk(handle, DETACH_VIRTUAL_DISK_FLAG_NONE, 0);
            let _ = CloseHandle(handle);

            if result != WIN32_ERROR(0) {
                anyhow::bail!("DetachVirtualDisk 失败: {:?}", result);
            }

            println!("[ISO] 卸载成功: {}", iso_path);
            Ok(())
        }
    }

    /// 使用 IOCTL 弹出 CDROM 类型的驱动器
    #[cfg(windows)]
    pub fn eject_cdrom_drive(letter: char) -> Result<()> {
        unsafe {
            let device_path = format!("\\\\.\\{}:", letter);
            let wide_path: Vec<u16> = device_path.encode_utf16().chain(std::iter::once(0)).collect();

            let handle = CreateFileW(
                PCWSTR::from_raw(wide_path.as_ptr()),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                Default::default(),
                None,
            );

            let handle = match handle {
                Ok(h) => h,
                Err(e) => anyhow::bail!("无法打开驱动器 {}: {:?}", letter, e),
            };

            if handle == INVALID_HANDLE_VALUE {
                anyhow::bail!("无效的驱动器句柄: {}", letter);
            }

            let result = DeviceIoControl(
                handle,
                IOCTL_STORAGE_EJECT_MEDIA,
                None,
                0,
                None,
                0,
                None,
                None,
            );

            let _ = CloseHandle(handle);

            if result.is_err() {
                anyhow::bail!("弹出驱动器 {} 失败", letter);
            }

            println!("[ISO] 已弹出驱动器: {}:", letter);
            Ok(())
        }
    }

    /// 使用 Windows API 卸载所有挂载的 ISO
    #[cfg(windows)]
    pub fn unmount_all_iso() -> Result<()> {
        println!("[ISO] 使用 Windows API 卸载所有挂载的 ISO");

        // 遍历所有盘符 D-Z，查找 CDROM 类型的驱动器并弹出
        for letter in b'D'..=b'Z' {
            let letter = letter as char;
            let drive_path = format!("{}:\\", letter);
            let wide_path: Vec<u16> = drive_path.encode_utf16().chain(std::iter::once(0)).collect();

            unsafe {
                let drive_type = GetDriveTypeW(PCWSTR::from_raw(wide_path.as_ptr()));
                
                if drive_type == DRIVE_CDROM {
                    // 检查是否包含 Windows 安装文件（确认是挂载的 ISO）
                    let sources_path = format!("{}:\\sources", letter);
                    if Path::new(&sources_path).exists() {
                        println!("[ISO] 发现挂载的 ISO: {}:", letter);
                        match Self::eject_cdrom_drive(letter) {
                            Ok(_) => println!("[ISO] 成功弹出: {}:", letter),
                            Err(e) => println!("[ISO] 弹出失败 {}: {} (将继续执行)", letter, e),
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// 挂载 ISO 并返回盘符 (如 "F:")
    pub fn mount_iso(iso_path: &str) -> Result<String> {
        println!("[ISO] ========== 挂载 ISO ==========");
        println!("[ISO] 路径: {}", iso_path);

        // 先尝试卸载已存在的挂载
        let _ = Self::unmount();
        std::thread::sleep(std::time::Duration::from_millis(300));

        let is_pe = Self::is_pe_environment();
        println!("[ISO] PE 环境: {}", is_pe);

        #[cfg(windows)]
        {
            println!("[ISO] 使用 Windows Virtual Disk API");
            match Self::mount_iso_winapi(iso_path) {
                Ok(letter) => {
                    let drive = format!("{}:", letter);
                    println!("[ISO] 挂载成功，盘符: {}", drive);
                    return Ok(drive);
                }
                Err(e) => {
                    println!("[ISO] Windows API 挂载失败: {}", e);
                    return Err(e);
                }
            }
        }

        #[cfg(not(windows))]
        {
            anyhow::bail!("ISO 挂载仅支持 Windows 系统")
        }
    }

    /// 卸载 ISO
    pub fn unmount() -> Result<()> {
        println!("[ISO] ========== 卸载 ISO ==========");

        #[cfg(windows)]
        {
            let _ = Self::unmount_all_iso();
        }

        Ok(())
    }

    /// 查找已挂载的 ISO 驱动器盘符（后备方案，遍历 D-Z）
    pub fn find_iso_drive() -> Option<String> {
        // 遍历 D 到 Z 所有盘符
        for letter in b'D'..=b'Z' {
            let letter = letter as char;
            let drive = format!("{}:", letter);
            let sources_path = format!("{}\\sources", drive);
            
            // 检查是否是 Windows 安装介质
            if Path::new(&sources_path).exists() {
                let install_wim = format!("{}\\install.wim", sources_path);
                let install_esd = format!("{}\\install.esd", sources_path);
                
                if Path::new(&install_wim).exists() || Path::new(&install_esd).exists() {
                    println!("[ISO] find_iso_drive 找到: {}", drive);
                    return Some(drive);
                }
            }
        }
        None
    }

    /// 在挂载的 ISO 中查找系统镜像文件
    /// 如果传入 drive 参数，则只在该盘符下查找
    /// 否则遍历所有盘符
    pub fn find_install_image_in_drive(drive: &str) -> Option<String> {
        let paths = [
            format!("{}\\sources\\install.wim", drive),
            format!("{}\\sources\\install.esd", drive),
            format!("{}\\sources\\install.swm", drive),
        ];

        for path in &paths {
            if Path::new(path).exists() {
                println!("[ISO] 在 {} 找到安装镜像: {}", drive, path);
                return Some(path.clone());
            }
        }

        println!("[ISO] 在 {} 未找到安装镜像", drive);
        None
    }

    /// 在挂载的 ISO 中查找系统镜像文件（遍历所有盘符）
    pub fn find_install_image() -> Option<String> {
        // 先查找动态挂载的盘符
        if let Some(drive) = Self::find_iso_drive() {
            return Self::find_install_image_in_drive(&drive);
        }

        println!("[ISO] 未找到安装镜像");
        None
    }

    /// 检查 ISO 是否已挂载
    pub fn is_mounted() -> bool {
        Self::find_iso_drive().is_some()
    }

    /// 获取挂载的 ISO 的卷标
    #[cfg(windows)]
    pub fn get_volume_label() -> Option<String> {
        use windows::Win32::Storage::FileSystem::GetVolumeInformationW;

        let drive = Self::find_iso_drive()?;
        let path = format!("{}\\", drive);
        let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

        let mut volume_name = [0u16; 261];
        
        unsafe {
            let result = GetVolumeInformationW(
                PCWSTR::from_raw(wide_path.as_ptr()),
                Some(&mut volume_name),
                None,
                None,
                None,
                None,
            );

            if result.is_ok() {
                let label = String::from_utf16_lossy(&volume_name)
                    .trim_end_matches('\0')
                    .to_string();
                if !label.is_empty() {
                    return Some(label);
                }
            }
        }

        None
    }

    #[cfg(not(windows))]
    pub fn get_volume_label() -> Option<String> {
        None
    }
}

impl Default for IsoMounter {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for IsoMounter {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(handle) = self.handle.take() {
            unsafe {
                let _ = CloseHandle(handle);
            }
        }
    }
}
