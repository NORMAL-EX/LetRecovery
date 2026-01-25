use egui;
use std::process::Command;

use crate::app::App;
use crate::utils::cmd::create_command;
use crate::utils::path::get_tools_dir;

impl App {
    pub fn show_tools(&mut self, ui: &mut egui::Ui) {
        ui.heading("工具箱");
        ui.separator();

        let is_pe = self.system_info.as_ref().map(|s| s.is_pe_environment).unwrap_or(false);

        ui.label("常用工具");
        ui.add_space(10.0);

        egui::Grid::new("tools_grid")
            .num_columns(3)
            .spacing([20.0, 15.0])
            .show(ui, |ui| {
                // BOOTICE
                if ui
                    .add(egui::Button::new("BOOTICE\n引导修复工具").min_size(egui::vec2(120.0, 60.0)))
                    .clicked()
                {
                    self.launch_tool("BOOTICE.exe");
                }

                // 显示隐藏分区
                if ui
                    .add(
                        egui::Button::new("显示隐藏分区").min_size(egui::vec2(120.0, 60.0)),
                    )
                    .clicked()
                {
                    self.launch_tool("ShowDrives_Gui.exe");
                }

                // 磁盘管理
                if ui
                    .add(
                        egui::Button::new("磁盘管理").min_size(egui::vec2(120.0, 60.0)),
                    )
                    .clicked()
                {
                    let _ = Command::new("mmc.exe")
                        .arg("diskmgmt.msc")
                        .spawn();
                }

                ui.end_row();

                // 设备管理器
                if ui
                    .add(
                        egui::Button::new("设备管理器").min_size(egui::vec2(120.0, 60.0)),
                    )
                    .clicked()
                {
                    let _ = Command::new("mmc.exe")
                        .arg("devmgmt.msc")
                        .spawn();
                }

                // 命令提示符
                if ui
                    .add(egui::Button::new("命令提示符").min_size(egui::vec2(120.0, 60.0)))
                    .clicked()
                {
                    let _ = Command::new("cmd.exe").spawn();
                }

                // 资源管理器
                if ui
                    .add(
                        egui::Button::new("资源管理器")
                            .min_size(egui::vec2(120.0, 60.0)),
                    )
                    .clicked()
                {
                    let _ = Command::new("explorer.exe").spawn();
                }

                ui.end_row();

                // 注册表编辑器
                if ui
                    .add(
                        egui::Button::new("注册表编辑器")
                            .min_size(egui::vec2(120.0, 60.0)),
                    )
                    .clicked()
                {
                    let _ = Command::new("regedit.exe").spawn();
                }

                // 任务管理器
                if ui
                    .add(
                        egui::Button::new("任务管理器")
                            .min_size(egui::vec2(120.0, 60.0)),
                    )
                    .clicked()
                {
                    let _ = Command::new("taskmgr.exe").spawn();
                }

                // 记事本
                if ui
                    .add(egui::Button::new("记事本").min_size(egui::vec2(120.0, 60.0)))
                    .clicked()
                {
                    let _ = Command::new("notepad.exe").spawn();
                }

                ui.end_row();

                // Ghost 工具
                if ui
                    .add(egui::Button::new("Ghost 工具").min_size(egui::vec2(120.0, 60.0)))
                    .clicked()
                {
                    self.launch_ghost_tool();
                }

                // ImDisk 虚拟磁盘
                if ui
                    .add(egui::Button::new("ImDisk\n虚拟磁盘").min_size(egui::vec2(120.0, 60.0)))
                    .clicked()
                {
                    self.launch_tool("imdisk.cpl");
                }

                ui.end_row();
            });

        ui.add_space(20.0);
        ui.separator();

        ui.label("系统操作");
        ui.add_space(10.0);

        // PE 环境下显示分区选择
        if is_pe {
            // 筛选有系统的分区
            let system_partitions: Vec<_> = self.partitions.iter()
                .filter(|p| p.has_windows && p.letter.to_uppercase() != "X:")
                .collect();

            if system_partitions.is_empty() {
                // 没有找到有系统的分区，显示警告
                ui.colored_label(
                    egui::Color32::from_rgb(255, 165, 0),
                    "⚠ 未找到包含 Windows 系统的分区！"
                );
                ui.label("修复引导和导出驱动需要选择一个包含 Windows 系统的分区。");
                ui.add_space(5.0);
            } else {
                ui.horizontal(|ui| {
                    ui.label("目标系统分区:");
                    egui::ComboBox::from_id_salt("target_partition_tools")
                        .selected_text(
                            self.tool_target_partition
                                .as_ref()
                                .unwrap_or(&"请选择".to_string()),
                        )
                        .show_ui(ui, |ui| {
                            for partition in system_partitions {
                                let label = format!(
                                    "{} {} ({:.1} GB) [有系统]",
                                    partition.letter,
                                    partition.label,
                                    partition.total_size_mb as f64 / 1024.0
                                );
                                ui.selectable_value(
                                    &mut self.tool_target_partition,
                                    Some(partition.letter.clone()),
                                    label,
                                );
                            }
                        });
                });
                ui.add_space(5.0);
            }
        }

        ui.horizontal(|ui| {
            if ui.button("修复系统引导").clicked() {
                self.repair_boot_action(is_pe);
            }

            if ui.button("导出系统驱动").clicked() {
                self.export_drivers_action(is_pe);
            }
            
            if ui.button("查看网络信息").clicked() {
                self.show_network_info_dialog = true;
                // 使用 WinAPI 获取网络信息
                self.network_info_cache = Some(get_detailed_network_info());
            }
        });

        ui.add_space(10.0);

        ui.horizontal(|ui| {
            if ui.button("重启计算机").clicked() {
                let _ = create_command("shutdown")
                    .args(["/r", "/t", "0"])
                    .spawn();
            }

            if ui.button("关闭计算机").clicked() {
                let _ = create_command("shutdown")
                    .args(["/s", "/t", "0"])
                    .spawn();
            }
        });

        // 网络信息对话框
        if self.show_network_info_dialog {
            egui::Window::new("本机网络信息")
                .open(&mut self.show_network_info_dialog)
                .resizable(true)
                .default_width(500.0)
                .default_height(400.0)
                .show(ui.ctx(), |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if let Some(ref adapters) = self.network_info_cache {
                            if adapters.is_empty() {
                                ui.label("未检测到网络适配器");
                            } else {
                                for (i, adapter) in adapters.iter().enumerate() {
                                    egui::CollapsingHeader::new(format!("适配器 {}: {}", i + 1, adapter.description))
                                        .default_open(true)
                                        .show(ui, |ui| {
                                            egui::Grid::new(format!("net_info_grid_{}", i))
                                                .num_columns(2)
                                                .spacing([20.0, 4.0])
                                                .show(ui, |ui| {
                                                    ui.label("名称:");
                                                    ui.label(&adapter.name);
                                                    ui.end_row();
                                                    
                                                    ui.label("描述:");
                                                    ui.label(&adapter.description);
                                                    ui.end_row();
                                                    
                                                    if !adapter.adapter_type.is_empty() {
                                                        ui.label("类型:");
                                                        ui.label(&adapter.adapter_type);
                                                        ui.end_row();
                                                    }
                                                    
                                                    if !adapter.mac_address.is_empty() {
                                                        ui.label("MAC 地址:");
                                                        ui.label(&adapter.mac_address);
                                                        ui.end_row();
                                                    }
                                                    
                                                    if !adapter.ip_addresses.is_empty() {
                                                        ui.label("IP 地址:");
                                                        for ip in &adapter.ip_addresses {
                                                            ui.label(ip);
                                                            ui.end_row();
                                                            ui.label(""); // 占位
                                                        }
                                                    }
                                                    
                                                    if !adapter.status.is_empty() {
                                                        ui.label("状态:");
                                                        ui.label(&adapter.status);
                                                        ui.end_row();
                                                    }
                                                    
                                                    if adapter.speed > 0 {
                                                        ui.label("速度:");
                                                        let speed_mbps = adapter.speed / 1_000_000;
                                                        ui.label(format!("{} Mbps", speed_mbps));
                                                        ui.end_row();
                                                    }
                                                });
                                        });
                                    ui.add_space(10.0);
                                }
                            }
                        } else {
                            ui.spinner();
                            ui.label("正在获取网络信息...");
                        }
                    });
                });
        }

        // 显示工具状态
        if !self.tool_message.is_empty() {
            ui.add_space(15.0);
            ui.separator();
            ui.label(&self.tool_message);
        }
    }

    fn launch_tool(&mut self, tool_name: &str) {
        let tools_dir = get_tools_dir();
        let tool_path = tools_dir.join(tool_name);

        if tool_path.exists() {
            // 检查文件扩展名，对.cpl文件使用特殊处理
            let result = if tool_name.to_lowercase().ends_with(".cpl") {
                // .cpl 文件是控制面板扩展，需要通过 control.exe 或 rundll32 打开
                // 使用 control.exe 是最可靠的方式
                Command::new("control.exe")
                    .arg(&tool_path)
                    .spawn()
            } else {
                Command::new(&tool_path).spawn()
            };

            match result {
                Ok(_) => {
                    self.tool_message = format!("已启动: {}", tool_name);
                }
                Err(e) => {
                    self.tool_message = format!("启动失败: {} - {}", tool_name, e);
                }
            }
        } else {
            self.tool_message = format!("工具不存在: {:?}", tool_path);
        }
    }

    fn launch_ghost_tool(&mut self) {
        let bin_dir = crate::utils::path::get_bin_dir();
        let ghost_path = bin_dir.join("ghost").join("Ghost64.exe");

        if ghost_path.exists() {
            match Command::new(&ghost_path).spawn() {
                Ok(_) => {
                    self.tool_message = "已启动: Ghost64.exe".to_string();
                }
                Err(e) => {
                    self.tool_message = format!("启动失败: Ghost64.exe - {}", e);
                }
            }
        } else {
            self.tool_message = format!("工具不存在: {:?}", ghost_path);
        }
    }

    fn repair_boot_action(&mut self, is_pe: bool) {
        let target_partition = if is_pe {
            // PE环境下使用用户选择的分区
            match &self.tool_target_partition {
                Some(p) => p.clone(),
                None => {
                    self.tool_message = "请先选择目标系统分区".to_string();
                    return;
                }
            }
        } else {
            // 正常环境下使用当前系统盘
            std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string())
        };

        let boot_manager = crate::core::bcdedit::BootManager::new();

        match boot_manager.repair_boot(&target_partition) {
            Ok(_) => {
                self.tool_message = format!("引导修复成功: {}", target_partition);
            }
            Err(e) => {
                self.tool_message = format!("引导修复失败: {}", e);
            }
        }
    }

    fn export_drivers_action(&mut self, is_pe: bool) {
        let dism = crate::core::dism::Dism::new();
        let export_dir = crate::utils::path::get_exe_dir()
            .join("drivers_backup")
            .to_string_lossy()
            .to_string();

        self.tool_message = "正在导出驱动...".to_string();

        if is_pe {
            // PE环境下使用离线方式导出
            let source_partition = match &self.tool_target_partition {
                Some(p) => p.clone(),
                None => {
                    self.tool_message = "请先选择源系统分区".to_string();
                    return;
                }
            };

            match dism.export_drivers_from_system(&source_partition, &export_dir) {
                Ok(_) => {
                    self.tool_message = format!("驱动导出成功: {} -> {}", source_partition, export_dir);
                }
                Err(e) => {
                    self.tool_message = format!("驱动导出失败: {}", e);
                }
            }
        } else {
            // 正常环境下使用在线方式导出
            match dism.export_drivers(&export_dir) {
                Ok(_) => {
                    self.tool_message = format!("驱动导出成功: {}", export_dir);
                }
                Err(e) => {
                    self.tool_message = format!("驱动导出失败: {}", e);
                }
            }
        }
    }
}

/// 使用 Windows API 获取详细的网络信息
/// 使用 GetAdaptersAddresses 获取更完整的信息
fn get_detailed_network_info() -> Vec<crate::core::hardware_info::NetworkAdapterInfo> {
    let mut adapters = Vec::new();

    #[cfg(windows)]
    {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;

        // IP_ADAPTER_ADDRESSES 结构体（简化版）
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

        // SOCKADDR_IN 结构体
        #[repr(C)]
        #[allow(non_snake_case, dead_code)]
        struct SOCKADDR_IN {
            sin_family: u16,
            sin_port: u16,
            sin_addr: [u8; 4],
            sin_zero: [u8; 8],
        }

        // SOCKADDR_IN6 结构体
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
            // 首先获取所需的缓冲区大小
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

            // 分配缓冲区
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

            // 遍历适配器
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

                // 获取 MAC 地址
                let mac = if adapter.PhysicalAddressLength > 0 {
                    adapter.PhysicalAddress[..adapter.PhysicalAddressLength as usize]
                        .iter()
                        .map(|b| format!("{:02X}", b))
                        .collect::<Vec<_>>()
                        .join(":")
                } else {
                    String::new()
                };

                // 获取 IP 地址
                let mut ip_addresses = Vec::new();
                let mut unicast = adapter.FirstUnicastAddress;
                while !unicast.is_null() {
                    let unicast_addr = &*unicast;
                    if !unicast_addr.Address.lpSockaddr.is_null() {
                        let family = *(unicast_addr.Address.lpSockaddr as *const u16);
                        
                        if family == 2 {
                            // AF_INET (IPv4)
                            let sockaddr = unicast_addr.Address.lpSockaddr as *const SOCKADDR_IN;
                            let addr = (*sockaddr).sin_addr;
                            let ip = format!("{}.{}.{}.{}", addr[0], addr[1], addr[2], addr[3]);
                            if ip != "0.0.0.0" {
                                ip_addresses.push(ip);
                            }
                        } else if family == 23 {
                            // AF_INET6 (IPv6)
                            let sockaddr = unicast_addr.Address.lpSockaddr as *const SOCKADDR_IN6;
                            let addr = (*sockaddr).sin6_addr;
                            // 简化的 IPv6 地址格式化
                            let ipv6 = format!(
                                "{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}",
                                addr[0], addr[1], addr[2], addr[3], addr[4], addr[5], addr[6], addr[7],
                                addr[8], addr[9], addr[10], addr[11], addr[12], addr[13], addr[14], addr[15]
                            );
                            if !ipv6.starts_with("0000:0000:0000:0000") {
                                ip_addresses.push(ipv6);
                            }
                        }
                    }
                    unicast = unicast_addr.Next;
                }

                // 适配器类型
                let adapter_type = match adapter.IfType {
                    6 => "以太网".to_string(),
                    71 => "无线网络".to_string(),
                    24 => "回环".to_string(),
                    131 => "隧道".to_string(),
                    _ => format!("类型 {}", adapter.IfType),
                };

                // 操作状态
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

                // 跳过回环适配器和无描述的适配器
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
