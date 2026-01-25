//! 网络功能模块
//!
//! 提供网络信息获取和网络重置等功能

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
                    6 => "以太网".to_string(),
                    71 => "无线网络".to_string(),
                    24 => "回环".to_string(),
                    131 => "隧道".to_string(),
                    _ => format!("类型 {}", adapter.IfType),
                };

                // 获取状态
                let status = match adapter.OperStatus {
                    1 => "已连接".to_string(),
                    2 => "已断开".to_string(),
                    3 => "测试中".to_string(),
                    4 => "未知".to_string(),
                    5 => "休眠".to_string(),
                    6 => "未启用".to_string(),
                    7 => "下层关闭".to_string(),
                    _ => "未知".to_string(),
                };

                // 过滤掉回环适配器和空描述的适配器
                if adapter.IfType != 24 && !description.is_empty() {
                    adapters.push(crate::core::hardware_info::NetworkAdapterInfo {
                        name: friendly_name,
                        description,
                        mac_address: mac,
                        ip_addresses,
                        adapter_type,
                        status,
                        speed: adapter.TransmitLinkSpeed,
                    });
                }

                current = adapter.Next;
            }
        }
    }

    adapters
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
