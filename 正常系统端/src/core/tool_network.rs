//! 工具箱网络信息与重置实现。
//!
//! 提供网络信息获取和网络重置等功能

use crate::tr;
use crate::utils::cmd::create_command;

/// 使用 Windows API 获取详细的网络信息
pub fn get_detailed_network_info() -> Vec<crate::core::hardware_info::NetworkAdapterInfo> {
    let mut adapters = Vec::new();

    #[cfg(windows)]
    {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;

        #[repr(C)]
        #[allow(non_snake_case, dead_code)]
        struct SOCKET_ADDRESS {
            lpSockaddr: *mut std::ffi::c_void,
            iSockaddrLength: i32,
        }

        #[repr(C)]
        #[allow(non_snake_case, dead_code)]
        struct IP_ADAPTER_UNICAST_ADDRESS {
            Length: u32,
            Flags: u32,
            Next: *mut IP_ADAPTER_UNICAST_ADDRESS,
            Address: SOCKET_ADDRESS,
            PrefixOrigin: i32,
            SuffixOrigin: i32,
            DadState: i32,
            ValidLifetime: u32,
            PreferredLifetime: u32,
            LeaseLifetime: u32,
            OnLinkPrefixLength: u8,
        }

        #[repr(C)]
        #[allow(non_snake_case, dead_code)]
        struct IP_ADAPTER_ADDRESSES {
            Length: u32,
            IfIndex: u32,
            Next: *mut IP_ADAPTER_ADDRESSES,
            AdapterName: *const i8,
            FirstUnicastAddress: *mut IP_ADAPTER_UNICAST_ADDRESS,
            FirstAnycastAddress: *mut std::ffi::c_void,
            FirstMulticastAddress: *mut std::ffi::c_void,
            FirstDnsServerAddress: *mut std::ffi::c_void,
            DnsSuffix: *const u16,
            Description: *const u16,
            FriendlyName: *const u16,
            PhysicalAddress: [u8; 8],
            PhysicalAddressLength: u32,
            Flags: u32,
            Mtu: u32,
            IfType: u32,
            OperStatus: i32,
            Ipv6IfIndex: u32,
            ZoneIndices: [u32; 16],
            FirstPrefix: *mut std::ffi::c_void,
            TransmitLinkSpeed: u64,
            ReceiveLinkSpeed: u64,
        }

        #[link(name = "iphlpapi")]
        extern "system" {
            fn GetAdaptersAddresses(
                Family: u32,
                Flags: u32,
                Reserved: *mut std::ffi::c_void,
                AdapterAddresses: *mut IP_ADAPTER_ADDRESSES,
                SizePointer: *mut u32,
            ) -> u32;
        }

        #[repr(C)]
        #[allow(non_snake_case, dead_code)]
        struct SOCKADDR_IN {
            sin_family: u16,
            sin_port: u16,
            sin_addr: [u8; 4],
            sin_zero: [u8; 8],
        }

        #[repr(C)]
        #[allow(non_snake_case, dead_code)]
        struct SOCKADDR_IN6 {
            sin6_family: u16,
            sin6_port: u16,
            sin6_flowinfo: u32,
            sin6_addr: [u8; 16],
            sin6_scope_id: u32,
        }

        const AF_UNSPEC: u32 = 0;
        const GAA_FLAG_INCLUDE_PREFIX: u32 = 0x0010;

        unsafe {
            let mut buf_len: u32 = 0;
            let result = GetAdaptersAddresses(
                AF_UNSPEC,
                GAA_FLAG_INCLUDE_PREFIX,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut buf_len,
            );

            // ERROR_BUFFER_OVERFLOW = 111
            if result != 111 && result != 0 {
                return adapters;
            }

            if buf_len == 0 {
                return adapters;
            }

            let mut buffer: Vec<u8> = vec![0u8; buf_len as usize];
            let adapter_addresses = buffer.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES;

            let result = GetAdaptersAddresses(
                AF_UNSPEC,
                GAA_FLAG_INCLUDE_PREFIX,
                std::ptr::null_mut(),
                adapter_addresses,
                &mut buf_len,
            );

            if result != 0 {
                return adapters;
            }

            let mut current = adapter_addresses;
            while !current.is_null() {
                let adapter = &*current;

                // 获取友好名称
                let friendly_name = if !adapter.FriendlyName.is_null() {
                    let mut len = 0;
                    let mut ptr = adapter.FriendlyName;
                    while *ptr != 0 {
                        len += 1;
                        ptr = ptr.add(1);
                    }
                    let slice = std::slice::from_raw_parts(adapter.FriendlyName, len);
                    OsString::from_wide(slice).to_string_lossy().to_string()
                } else {
                    String::new()
                };

                // 获取描述
                let description = if !adapter.Description.is_null() {
                    let mut len = 0;
                    let mut ptr = adapter.Description;
                    while *ptr != 0 {
                        len += 1;
                        ptr = ptr.add(1);
                    }
                    let slice = std::slice::from_raw_parts(adapter.Description, len);
                    OsString::from_wide(slice).to_string_lossy().to_string()
                } else {
                    String::new()
                };

                // 获取MAC地址
                let mac = if adapter.PhysicalAddressLength > 0 {
                    adapter.PhysicalAddress[..adapter.PhysicalAddressLength as usize]
                        .iter()
                        .map(|b| format!("{:02X}", b))
                        .collect::<Vec<_>>()
                        .join(":")
                } else {
                    String::new()
                };

                // 获取IP地址
                let mut ip_addresses = Vec::new();
                let mut unicast = adapter.FirstUnicastAddress;
                while !unicast.is_null() {
                    let unicast_addr = &*unicast;
                    if !unicast_addr.Address.lpSockaddr.is_null() {
                        let family = *(unicast_addr.Address.lpSockaddr as *const u16);

                        // AF_INET = 2 (IPv4)
                        if family == 2 {
                            let sockaddr = unicast_addr.Address.lpSockaddr as *const SOCKADDR_IN;
                            let addr = (*sockaddr).sin_addr;
                            let ip = format!("{}.{}.{}.{}", addr[0], addr[1], addr[2], addr[3]);
                            if ip != "0.0.0.0" {
                                ip_addresses.push(ip);
                            }
                        }
                        // AF_INET6 = 23 (IPv6)
                        else if family == 23 {
                            let sockaddr = unicast_addr.Address.lpSockaddr as *const SOCKADDR_IN6;
                            let addr = (*sockaddr).sin6_addr;
                            let ipv6 = format!(
                                "{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}",
                                addr[0], addr[1], addr[2], addr[3], addr[4], addr[5], addr[6], addr[7],
                                addr[8], addr[9], addr[10], addr[11], addr[12], addr[13], addr[14], addr[15]
                            );
                            // 过滤全零地址
                            if !ipv6.starts_with("0000:0000:0000:0000") {
                                ip_addresses.push(ipv6);
                            }
                        }
                    }
                    unicast = unicast_addr.Next;
                }

                // 获取适配器类型
                let adapter_type = match adapter.IfType {
                    6 => tr!("以太网"),
                    71 => tr!("无线网络"),
                    24 => tr!("回环"),
                    131 => tr!("隧道"),
                    _ => tr!("类型 {}", adapter.IfType),
                };

                // 获取状态
                let status = match adapter.OperStatus {
                    1 => tr!("已连接"),
                    2 => tr!("已断开"),
                    3 => tr!("测试中"),
                    4 => tr!("未知"),
                    5 => tr!("休眠"),
                    6 => tr!("未启用"),
                    7 => tr!("下层关闭"),
                    _ => tr!("未知"),
                };

                // 过滤掉回环适配器和空描述的适配器
                if adapter.IfType != 24 && !description.is_empty() {
                    let speed = query_interface_link_speed(
                        adapter.IfIndex,
                        adapter.TransmitLinkSpeed,
                        adapter.ReceiveLinkSpeed,
                    );
                    adapters.push(crate::core::hardware_info::NetworkAdapterInfo {
                        name: friendly_name,
                        description,
                        mac_address: mac,
                        ip_addresses,
                        adapter_type,
                        status,
                        speed,
                    });
                }

                current = adapter.Next;
            }
        }
    }

    adapters
}

fn preferred_link_speed(
    address_tx: u64,
    address_rx: u64,
    interface_tx: u64,
    interface_rx: u64,
    legacy_speed: u32,
) -> u64 {
    address_tx
        .max(address_rx)
        .max(interface_tx.max(interface_rx))
        .max(u64::from(legacy_speed))
}

#[cfg(windows)]
unsafe fn query_interface_link_speed(if_index: u32, address_tx: u64, address_rx: u64) -> u64 {
    use windows::Win32::NetworkManagement::IpHelper::{
        GetIfEntry, GetIfEntry2, MIB_IFROW, MIB_IF_ROW2,
    };

    // Keep GetAdaptersAddresses as the zero-extra-call path on modern Windows. Windows 7 network
    // drivers can leave those two fields at zero even though the interface table reports the
    // negotiated link rate.
    let address_speed = address_tx.max(address_rx);
    if address_speed != 0 {
        return address_speed;
    }

    let mut row2 = MIB_IF_ROW2 {
        InterfaceIndex: if_index,
        ..Default::default()
    };
    let (interface_tx, interface_rx) = if GetIfEntry2(&mut row2).0 == 0 {
        (row2.TransmitLinkSpeed, row2.ReceiveLinkSpeed)
    } else {
        (0, 0)
    };
    if interface_tx != 0 || interface_rx != 0 {
        return interface_tx.max(interface_rx);
    }

    // The legacy table is available before Windows 7 and remains a final compatibility fallback.
    // Its 32-bit bits-per-second field can saturate above 4 Gbps, so it is never preferred over the
    // 64-bit modern values.
    let mut legacy = MIB_IFROW {
        dwIndex: if_index,
        ..Default::default()
    };
    let legacy_speed = if GetIfEntry(&mut legacy) == 0 {
        legacy.dwSpeed
    } else {
        0
    };
    preferred_link_speed(
        address_tx,
        address_rx,
        interface_tx,
        interface_rx,
        legacy_speed,
    )
}

/// 执行网络重置
pub fn reset_network() -> (usize, usize) {
    let commands = [
        ("netsh", &["winsock", "reset"][..]),
        ("netsh", &["int", "ip", "reset"][..]),
        ("ipconfig", &["/flushdns"][..]),
        ("netsh", &["advfirewall", "reset"][..]),
    ];

    let mut success_count = 0;
    let mut fail_count = 0;

    for (cmd, args) in &commands {
        match create_command(cmd).args(*args).output() {
            Ok(output) => {
                if output.status.success() {
                    success_count += 1;
                } else {
                    fail_count += 1;
                }
            }
            Err(_) => {
                fail_count += 1;
            }
        }
    }

    (success_count, fail_count)
}

#[cfg(test)]
mod tests {
    use super::preferred_link_speed;

    #[test]
    fn adapter_address_speed_remains_the_fast_preferred_value() {
        assert_eq!(
            preferred_link_speed(2_500_000_000, 1_000_000_000, 0, 0, 100_000_000),
            2_500_000_000
        );
    }

    #[test]
    fn windows7_interface_and_legacy_fallbacks_replace_zero_mbps() {
        assert_eq!(
            preferred_link_speed(0, 0, 1_000_000_000, 1_000_000_000, 100_000_000),
            1_000_000_000
        );
        assert_eq!(preferred_link_speed(0, 0, 0, 0, 100_000_000), 100_000_000);
    }
}
