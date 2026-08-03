use anyhow::{Context, Result};
use std::path::Path;

use crate::tr;

#[cfg(windows)]
use windows::{
    core::PCWSTR,
    Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WIN32_ERROR},
    Win32::Storage::FileSystem::{
        CreateFileW, GetDriveTypeW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    },
    Win32::Storage::Vhd::{
        AttachVirtualDisk, DetachVirtualDisk, GetVirtualDiskPhysicalPath, OpenVirtualDisk,
        ATTACH_VIRTUAL_DISK_FLAG_PERMANENT_LIFETIME, ATTACH_VIRTUAL_DISK_FLAG_READ_ONLY,
        DETACH_VIRTUAL_DISK_FLAG_NONE, OPEN_VIRTUAL_DISK_FLAG_NONE, OPEN_VIRTUAL_DISK_PARAMETERS,
        OPEN_VIRTUAL_DISK_VERSION_1, VIRTUAL_DISK_ACCESS_DETACH, VIRTUAL_DISK_ACCESS_READ,
        VIRTUAL_STORAGE_TYPE, VIRTUAL_STORAGE_TYPE_DEVICE_ISO,
    },
    Win32::System::Ioctl::{IOCTL_STORAGE_EJECT_MEDIA, IOCTL_STORAGE_GET_DEVICE_NUMBER},
    Win32::System::IO::DeviceIoControl,
};

#[cfg(windows)]
const DRIVE_CDROM: u32 = 5;

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StorageDeviceNumber {
    device_type: u32,
    device_number: u32,
    partition_number: u32,
}

#[cfg(windows)]
const VIRTUAL_STORAGE_TYPE_VENDOR_MICROSOFT: windows::core::GUID =
    windows::core::GUID::from_u128(0xEC984AEC_A0F9_47E9_901F_71415A66345B);

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

    #[cfg(windows)]
    unsafe fn query_device_number(device_path: &str) -> Result<StorageDeviceNumber> {
        let wide: Vec<u16> = device_path
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let handle = CreateFileW(
            PCWSTR::from_raw(wide.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            Default::default(),
            None,
        )?;
        let mut number = StorageDeviceNumber::default();
        let result = DeviceIoControl(
            handle,
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            None,
            0,
            Some((&mut number as *mut StorageDeviceNumber).cast()),
            std::mem::size_of::<StorageDeviceNumber>() as u32,
            None,
            None,
        );
        let _ = CloseHandle(handle);
        result?;
        Ok(number)
    }

    #[cfg(windows)]
    unsafe fn attached_device_path(handle: HANDLE) -> Result<String> {
        let mut buffer = [0u16; 1024];
        let mut size_bytes = std::mem::size_of_val(&buffer) as u32;
        let result = GetVirtualDiskPhysicalPath(
            handle,
            &mut size_bytes,
            windows::core::PWSTR::from_raw(buffer.as_mut_ptr()),
        );
        if result != WIN32_ERROR(0) {
            anyhow::bail!("GetVirtualDiskPhysicalPath 失败: {result:?}");
        }
        let length = (size_bytes as usize / 2).min(buffer.len());
        Ok(String::from_utf16_lossy(&buffer[..length])
            .trim_end_matches('\0')
            .to_owned())
    }

    #[cfg(windows)]
    unsafe fn find_drive_for_attached_device(device_path: &str) -> Result<char> {
        let expected = Self::query_device_number(device_path)?;
        for _ in 0..20 {
            let mask = lr_core::windows_storage::assigned_drive_letter_mask()
                .context("GetLogicalDrives 失败")?;
            for index in 0..26u8 {
                if mask & (1u32 << index) == 0 {
                    continue;
                }
                let letter = (b'A' + index) as char;
                let root = format!("{letter}:\\");
                let wide_root: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
                if GetDriveTypeW(PCWSTR::from_raw(wide_root.as_ptr())) != DRIVE_CDROM {
                    continue;
                }
                let volume_path = format!("\\\\.\\{letter}:");
                if Self::query_device_number(&volume_path).ok() == Some(expected) {
                    return Ok(letter);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        anyhow::bail!("ISO 已附加，但系统未为对应设备分配盘符")
    }

    /// 使用 Windows API 挂载 ISO 并返回盘符
    #[cfg(windows)]
    pub fn mount_iso_winapi(iso_path: &str) -> Result<char> {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Storage::Vhd::{
            ATTACH_VIRTUAL_DISK_PARAMETERS, ATTACH_VIRTUAL_DISK_VERSION_1,
        };

        log::info!("[ISO] 使用 Windows API 挂载 ISO: {}", iso_path);

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
                log::error!("[ISO] OpenVirtualDisk 失败: {:?}", result);
                anyhow::bail!(
                    "{}",
                    tr!("OpenVirtualDisk 失败: {}", format!("{:?}", result))
                );
            }

            log::info!("[ISO] OpenVirtualDisk 成功, handle: {:?}", handle);

            // 设置挂载参数
            let mut attach_params: ATTACH_VIRTUAL_DISK_PARAMETERS = std::mem::zeroed();
            attach_params.Version = ATTACH_VIRTUAL_DISK_VERSION_1;

            // 挂载虚拟磁盘 (只读, 自动分配盘符, 永久生命周期)
            use windows::Win32::Storage::Vhd::ATTACH_VIRTUAL_DISK_FLAG;
            let attach_flags = ATTACH_VIRTUAL_DISK_FLAG(
                ATTACH_VIRTUAL_DISK_FLAG_READ_ONLY.0
                    | ATTACH_VIRTUAL_DISK_FLAG_PERMANENT_LIFETIME.0,
            );

            let result =
                AttachVirtualDisk(handle, None, attach_flags, 0, Some(&attach_params), None);

            if result != WIN32_ERROR(0) {
                log::error!("[ISO] AttachVirtualDisk 失败: {:?}", result);
                let _ = CloseHandle(handle);
                anyhow::bail!(
                    "{}",
                    tr!("AttachVirtualDisk 失败: {}", format!("{:?}", result))
                );
            }

            log::info!("[ISO] AttachVirtualDisk 成功");

            let mapped = Self::attached_device_path(handle).and_then(|device_path| {
                log::info!("[ISO] 附加设备路径: {device_path}");
                Self::find_drive_for_attached_device(&device_path)
            });
            match mapped {
                Ok(letter) => {
                    let _ = CloseHandle(handle);
                    log::info!("[ISO] 精确匹配到附加设备盘符: {letter}:");
                    Ok(letter)
                }
                Err(error) => {
                    let detach = DetachVirtualDisk(handle, DETACH_VIRTUAL_DISK_FLAG_NONE, 0);
                    let _ = CloseHandle(handle);
                    if detach != WIN32_ERROR(0) {
                        anyhow::bail!("ISO 盘符映射失败: {error}; 回滚卸载同时失败: {detach:?}");
                    }
                    Err(error).context("ISO 盘符映射失败，已回滚卸载")
                }
            }
        }
    }

    /// 使用 Windows API 卸载指定 ISO
    #[cfg(windows)]
    pub fn unmount_iso_by_path(iso_path: &str) -> Result<()> {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;

        log::info!("[ISO] 使用 Windows API 卸载 ISO: {}", iso_path);

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
                anyhow::bail!(
                    "{}",
                    tr!("OpenVirtualDisk 失败: {}", format!("{:?}", result))
                );
            }

            let result = DetachVirtualDisk(handle, DETACH_VIRTUAL_DISK_FLAG_NONE, 0);
            let _ = CloseHandle(handle);

            if result != WIN32_ERROR(0) {
                anyhow::bail!(
                    "{}",
                    tr!("DetachVirtualDisk 失败: {}", format!("{:?}", result))
                );
            }

            log::info!("[ISO] 卸载成功: {}", iso_path);
            Ok(())
        }
    }

    /// 使用 IOCTL 弹出 CDROM 类型的驱动器
    #[cfg(windows)]
    pub fn eject_cdrom_drive(letter: char) -> Result<()> {
        unsafe {
            let device_path = format!("\\\\.\\{}:", letter);
            let wide_path: Vec<u16> = device_path
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();

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

            log::info!("[ISO] 已弹出驱动器: {}:", letter);
            Ok(())
        }
    }

    /// 使用 Windows API 卸载所有挂载的 ISO
    #[cfg(windows)]
    pub fn unmount_all_iso() -> Result<()> {
        anyhow::bail!(
            "拒绝卸载所有光盘或 ISO：必须使用原始 ISO 路径卸载 LetRecovery 自己挂载的镜像"
        )
    }

    /// 挂载 ISO 并返回盘符 (如 "F:")
    pub fn mount_iso(iso_path: &str) -> Result<String> {
        log::info!("[ISO] ========== 挂载 ISO ==========");
        log::info!("[ISO] 路径: {}", iso_path);

        let is_pe = Self::is_pe_environment();
        log::info!("[ISO] PE 环境: {}", is_pe);

        #[cfg(windows)]
        {
            log::info!("[ISO] 使用 Windows Virtual Disk API");
            match Self::mount_iso_winapi(iso_path) {
                Ok(letter) => {
                    let drive = format!("{}:", letter);
                    log::info!("[ISO] 挂载成功，盘符: {}", drive);
                    Ok(drive)
                }
                Err(e) => {
                    log::error!("[ISO] Windows API 挂载失败: {}", e);
                    Err(e)
                }
            }
        }

        #[cfg(not(windows))]
        {
            anyhow::bail!("{}", tr!("ISO 挂载仅支持 Windows 系统"))
        }
    }

    /// Run a read-only operation against an ISO mounted by LetRecovery and always detach the
    /// exact image path before returning. The operation never scans or ejects unrelated media.
    pub fn with_mounted_iso<T>(
        iso_path: &str,
        operation: impl FnOnce(&str) -> Result<T>,
    ) -> Result<T> {
        let drive = Self::mount_iso(iso_path)?;
        let operation_result = operation(&drive);
        let detach_result = Self::unmount_iso_by_path(iso_path);
        match (operation_result, detach_result) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(detach)) => Err(detach).context("ISO 操作成功，但卸载失败"),
            (Err(error), Err(detach)) => Err(error).context(format!("ISO 卸载同时失败: {detach}")),
        }
    }

    /// 卸载 ISO
    pub fn unmount() -> Result<()> {
        anyhow::bail!("拒绝无所有权信息的 ISO 卸载：请按原始 ISO 路径卸载")
    }

    /// 判断盘符是否为 Windows 安装介质：
    /// - Vista+/Win10：`\sources\install.wim|esd|swm`
    /// - XP/2003：`\I386`（x86）或 `\AMD64`（x64）文本安装结构
    pub fn is_windows_install_media(drive: &str) -> bool {
        let d = drive.trim_end_matches('\\');
        for f in ["install.wim", "install.esd", "install.swm"] {
            if Path::new(&format!("{}\\sources\\{}", d, f)).exists() {
                return true;
            }
        }
        // XP/2003：有 i386/amd64 且含 setupldr.bin 即视为文本安装介质
        for arch in ["I386", "AMD64"] {
            if Path::new(&format!("{}\\{}\\setupldr.bin", d, arch)).exists() {
                return true;
            }
        }
        false
    }

    /// 该盘符是否为 XP/2003 文本安装介质（无 \sources，有 \AMD64 或 \I386 的 setupldr.bin）。
    /// 返回该 arch 目录路径。
    ///
    /// 关键：**优先 AMD64**。XP x64 / Server 2003 x64 介质同时含 `\AMD64`（真正完整的 64 位安装源）
    /// 和 `\I386`（仅 32 位 WOW 支持文件，**残缺**、没有 ntfs.sy_ 等引导文件）。若按 I386 优先会选中
    /// 残缺目录导致安装失败。故先认完整源：除 setupldr.bin 外还要求 ntfs.sy_ 存在（残缺的 \I386 缺它）。
    pub fn xp_i386_dir(drive: &str) -> Option<String> {
        let d = drive.trim_end_matches('\\');
        // 第一轮：完整可引导源（setupldr.bin + ntfs 驱动）。AMD64 优先。
        // ntfs.sy_（压缩名，retail）或 ntfs.sys（解压重封装）任一即可——残缺的 x64 \I386 两者都没有。
        for arch in ["AMD64", "I386"] {
            let dir = format!("{}\\{}", d, arch);
            let has_setupldr = Path::new(&format!("{}\\setupldr.bin", dir)).exists();
            let has_ntfs = Path::new(&format!("{}\\ntfs.sy_", dir)).exists()
                || Path::new(&format!("{}\\ntfs.sys", dir)).exists();
            if has_setupldr && has_ntfs {
                return Some(dir);
            }
        }
        // 兜底：只有 setupldr.bin 的目录（个别重封装介质 ntfs.sy_ 名字不同）。仍 AMD64 优先；
        // 真残缺时交由引擎的「必需文件校验」给出明确报错（缺哪个文件），而不是默默跑挂。
        for arch in ["AMD64", "I386"] {
            let dir = format!("{}\\{}", d, arch);
            if Path::new(&format!("{}\\setupldr.bin", dir)).exists() {
                return Some(dir);
            }
        }
        None
    }

    /// 查找已挂载的 ISO 驱动器盘符（后备方案，遍历 D-Z）
    pub fn find_iso_drive() -> Option<String> {
        // 遍历 D 到 Z 所有盘符
        for letter in b'D'..=b'Z' {
            let letter = letter as char;
            let drive = format!("{}:", letter);
            // Vista+ 或 XP/2003 安装介质都接受
            if Self::is_windows_install_media(&drive) {
                log::info!("[ISO] find_iso_drive 找到: {}", drive);
                return Some(drive);
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
                log::info!("[ISO] 在 {} 找到安装镜像: {}", drive, path);
                return Some(path.clone());
            }
        }

        log::info!("[ISO] 在 {} 未找到安装镜像", drive);
        None
    }

    /// 在挂载的 ISO 中查找系统镜像文件（遍历所有盘符）
    pub fn find_install_image() -> Option<String> {
        // 先查找动态挂载的盘符
        if let Some(drive) = Self::find_iso_drive() {
            return Self::find_install_image_in_drive(&drive);
        }

        log::info!("[ISO] 未找到安装镜像");
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
