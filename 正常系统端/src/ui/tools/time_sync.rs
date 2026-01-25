//! 系统时间校准模块
//!
//! 使用NTP协议从网络服务器同步系统时间

use std::net::UdpSocket;
use std::time::Duration;

#[cfg(windows)]
use windows::Win32::Foundation::SYSTEMTIME;

/// NTP时间戳起始点: 1900-01-01 00:00:00 UTC
const NTP_EPOCH_OFFSET: u64 = 2_208_988_800;

/// NTP服务器列表（中国）
const NTP_SERVERS: &[&str] = &[
    "ntp.aliyun.com",
    "ntp.tencent.com", 
    "cn.ntp.org.cn",
    "time.windows.com",
    "pool.ntp.org",
];

/// NTP包结构（简化版本）
#[repr(C, packed)]
#[derive(Clone, Copy, Debug)]
struct NtpPacket {
    /// LI (2 bits) | VN (3 bits) | Mode (3 bits)
    li_vn_mode: u8,
    /// Stratum
    stratum: u8,
    /// Poll interval
    poll: u8,
    /// Precision
    precision: i8,
    /// Root delay
    root_delay: u32,
    /// Root dispersion
    root_dispersion: u32,
    /// Reference identifier
    ref_id: u32,
    /// Reference timestamp (seconds)
    ref_timestamp_sec: u32,
    /// Reference timestamp (fraction)
    ref_timestamp_frac: u32,
    /// Origin timestamp (seconds)
    orig_timestamp_sec: u32,
    /// Origin timestamp (fraction)
    orig_timestamp_frac: u32,
    /// Receive timestamp (seconds)
    recv_timestamp_sec: u32,
    /// Receive timestamp (fraction)
    recv_timestamp_frac: u32,
    /// Transmit timestamp (seconds)
    tx_timestamp_sec: u32,
    /// Transmit timestamp (fraction)
    tx_timestamp_frac: u32,
}

impl NtpPacket {
    fn new_request() -> Self {
        NtpPacket {
            // LI = 0, VN = 4 (NTPv4), Mode = 3 (Client)
            li_vn_mode: 0b00_100_011,
            stratum: 0,
            poll: 0,
            precision: 0,
            root_delay: 0,
            root_dispersion: 0,
            ref_id: 0,
            ref_timestamp_sec: 0,
            ref_timestamp_frac: 0,
            orig_timestamp_sec: 0,
            orig_timestamp_frac: 0,
            recv_timestamp_sec: 0,
            recv_timestamp_frac: 0,
            tx_timestamp_sec: 0,
            tx_timestamp_frac: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                self as *const Self as *const u8,
                std::mem::size_of::<Self>(),
            )
        }
    }

    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < std::mem::size_of::<Self>() {
            return None;
        }
        unsafe {
            Some(std::ptr::read_unaligned(bytes.as_ptr() as *const Self))
        }
    }

    /// 获取传输时间戳（NTP时间，秒数）
    fn get_transmit_timestamp_secs(&self) -> u64 {
        u32::from_be(self.tx_timestamp_sec) as u64
    }
}

/// 时间同步结果
#[derive(Debug)]
pub struct TimeSyncResult {
    /// 是否成功
    pub success: bool,
    /// 消息
    pub message: String,
    /// 同步前的本地时间
    pub old_time: Option<String>,
    /// 同步后的时间
    pub new_time: Option<String>,
}

/// 从NTP服务器获取当前时间
/// 
/// 返回Unix时间戳（秒）
fn get_ntp_time(server: &str) -> Result<u64, String> {
    let addr = format!("{}:123", server);
    
    // 创建UDP socket
    let socket = UdpSocket::bind("0.0.0.0:0")
        .map_err(|e| format!("无法创建套接字: {}", e))?;
    
    // 设置超时
    socket.set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| format!("设置超时失败: {}", e))?;
    socket.set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| format!("设置超时失败: {}", e))?;
    
    // 发送NTP请求
    let request = NtpPacket::new_request();
    socket.send_to(request.as_bytes(), &addr)
        .map_err(|e| format!("发送请求失败: {}", e))?;
    
    // 接收响应
    let mut buffer = [0u8; 48];
    let (len, _) = socket.recv_from(&mut buffer)
        .map_err(|e| format!("接收响应失败: {}", e))?;
    
    if len < 48 {
        return Err("响应数据不完整".to_string());
    }
    
    // 解析响应
    let response = NtpPacket::from_bytes(&buffer)
        .ok_or_else(|| "解析响应失败".to_string())?;
    
    // 获取传输时间戳并转换为Unix时间戳
    let ntp_secs = response.get_transmit_timestamp_secs();
    if ntp_secs < NTP_EPOCH_OFFSET {
        return Err("时间戳无效".to_string());
    }
    
    let unix_secs = ntp_secs - NTP_EPOCH_OFFSET;
    Ok(unix_secs)
}

/// 将Unix时间戳转换为北京时间（UTC+8）
fn unix_to_beijing_time(unix_secs: u64) -> (u16, u16, u16, u16, u16, u16, u16) {
    // 转换为北京时间（UTC+8）
    let beijing_secs = unix_secs + 8 * 3600;
    
    // 计算年月日时分秒
    let days_since_1970 = beijing_secs / 86400;
    let time_of_day = beijing_secs % 86400;
    
    let hour = (time_of_day / 3600) as u16;
    let minute = ((time_of_day % 3600) / 60) as u16;
    let second = (time_of_day % 60) as u16;
    
    // 计算日期（简化算法）
    let mut year: i32 = 1970;
    let mut remaining_days = days_since_1970 as i32;
    
    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        year += 1;
    }
    
    let days_in_months: [i32; 12] = if is_leap_year(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    
    let mut month: i32 = 1;
    for days in days_in_months.iter() {
        if remaining_days < *days {
            break;
        }
        remaining_days -= *days;
        month += 1;
    }
    
    let day = remaining_days + 1;
    
    // 计算星期几（0 = 周日）
    let day_of_week = ((days_since_1970 + 4) % 7) as u16; // 1970-01-01是周四
    
    (year as u16, month as u16, day as u16, hour, minute, second, day_of_week)
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// 设置系统时间（Windows）
#[cfg(windows)]
fn set_system_time(year: u16, month: u16, day: u16, hour: u16, minute: u16, second: u16, day_of_week: u16) -> Result<(), String> {
    use windows::Win32::System::SystemInformation::SetLocalTime;
    
    // 使用SetLocalTime设置本地时间（北京时间）
    let st = SYSTEMTIME {
        wYear: year,
        wMonth: month,
        wDayOfWeek: day_of_week,
        wDay: day,
        wHour: hour,
        wMinute: minute,
        wSecond: second,
        wMilliseconds: 0,
    };
    
    unsafe {
        SetLocalTime(&st)
            .map_err(|e| format!("设置系统时间失败: {}", e))?;
    }
    
    Ok(())
}

#[cfg(not(windows))]
fn set_system_time(_year: u16, _month: u16, _day: u16, _hour: u16, _minute: u16, _second: u16, _day_of_week: u16) -> Result<(), String> {
    Err("仅支持Windows系统".to_string())
}

/// 获取当前本地时间字符串
#[cfg(windows)]
fn get_local_time_string() -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    
    let st = unsafe { GetLocalTime() };
    
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    )
}

#[cfg(not(windows))]
fn get_local_time_string() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 同步系统时间到北京时间
pub fn sync_time_to_beijing() -> TimeSyncResult {
    let old_time = get_local_time_string();
    
    // 尝试从多个NTP服务器获取时间
    let mut last_error = String::new();
    
    for server in NTP_SERVERS {
        log::info!("正在尝试NTP服务器: {}", server);
        
        match get_ntp_time(server) {
            Ok(unix_secs) => {
                let (year, month, day, hour, minute, second, day_of_week) = 
                    unix_to_beijing_time(unix_secs);
                
                let new_time_str = format!(
                    "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                    year, month, day, hour, minute, second
                );
                
                log::info!("从 {} 获取到时间: {}", server, new_time_str);
                
                // 设置系统时间
                match set_system_time(year, month, day, hour, minute, second, day_of_week) {
                    Ok(_) => {
                        let actual_new_time = get_local_time_string();
                        return TimeSyncResult {
                            success: true,
                            message: format!("时间同步成功！服务器: {}", server),
                            old_time: Some(old_time),
                            new_time: Some(actual_new_time),
                        };
                    }
                    Err(e) => {
                        log::error!("设置系统时间失败: {}", e);
                        return TimeSyncResult {
                            success: false,
                            message: format!("设置系统时间失败: {}。可能需要管理员权限。", e),
                            old_time: Some(old_time),
                            new_time: None,
                        };
                    }
                }
            }
            Err(e) => {
                log::warn!("从 {} 获取时间失败: {}", server, e);
                last_error = format!("{}: {}", server, e);
            }
        }
    }
    
    TimeSyncResult {
        success: false,
        message: format!("无法连接到任何NTP服务器。最后错误: {}", last_error),
        old_time: Some(old_time),
        new_time: None,
    }
}

/// 检查是否有网络连接
pub fn check_network_for_ntp() -> bool {
    for server in NTP_SERVERS.iter().take(2) {
        let addr = format!("{}:123", server);
        if let Ok(socket) = UdpSocket::bind("0.0.0.0:0") {
            socket.set_read_timeout(Some(Duration::from_secs(2))).ok();
            if socket.connect(&addr).is_ok() {
                return true;
            }
        }
    }
    false
}
