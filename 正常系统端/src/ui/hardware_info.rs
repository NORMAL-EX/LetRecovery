use egui;

use crate::app::App;
use crate::core::hardware_info::BitLockerStatus;

impl App {
    pub fn show_hardware_info(&mut self, ui: &mut egui::Ui) {
        ui.heading("系统与硬件信息");
        ui.separator();

        // PE 环境提示
        if let Some(info) = &self.system_info {
            if info.is_pe_environment {
                ui.colored_label(
                    egui::Color32::from_rgb(100, 200, 255),
                    "当前运行在 PE 环境中",
                );
                ui.add_space(5.0);
            }
        }

        // 操作按钮区域
        ui.horizontal(|ui| {
            // 复制按钮
            if ui.button("复制全部信息").clicked() {
                if let Some(hw_info) = &self.hardware_info {
                    let formatted_text = hw_info.to_formatted_text(self.system_info.as_ref());
                    ui.ctx().copy_text(formatted_text);
                }
            }
            
            // 导出按钮
            if ui.button("导出为TXT").clicked() {
                self.export_hardware_info_to_txt();
            }
        });
        
        ui.add_space(10.0);

        egui::ScrollArea::vertical()
            .id_salt("hardware_scroll")
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                
                if let Some(hw_info) = &self.hardware_info.clone() {
                    let sys_info = self.system_info.as_ref();
                    
                    // 系统信息
                    egui::CollapsingHeader::new("系统信息")
                        .default_open(true)
                        .show(ui, |ui| {
                            egui::Grid::new("system_grid")
                                .num_columns(2)
                                .spacing([20.0, 4.0])
                                .striped(true)
                                .show(ui, |ui| {
                                    let arch_str = match hw_info.os.architecture.as_str() {
                                        "64 位" => "X64", "32 位" => "X86", "ARM64" => "ARM64", _ => &hw_info.os.architecture,
                                    };
                                    
                                    ui.label("系统名称:");
                                    ui.label(format!("{} {} [10.0.{} ({})]", hw_info.os.name, arch_str, hw_info.os.build_number, hw_info.os.version));
                                    ui.end_row();
                                    
                                    ui.label("计算机名:");
                                    ui.label(&hw_info.computer_name);
                                    ui.end_row();
                                    
                                    if !hw_info.os.install_date.is_empty() {
                                        ui.label("安装日期:");
                                        ui.label(&hw_info.os.install_date);
                                        ui.end_row();
                                    }
                                    
                                    let boot_mode = sys_info.map(|s| format!("{}", s.boot_mode)).unwrap_or_else(|| "未知".to_string());
                                    ui.label("启动模式:");
                                    ui.label(format!("{}  设备类型: {}", boot_mode, hw_info.device_type));
                                    ui.end_row();
                                    
                                    let tpm_str = if let Some(s) = sys_info { 
                                        if s.tpm_enabled { format!("已开启 v{}", s.tpm_version) } else { "未开启".to_string() } 
                                    } else { "未知".to_string() };
                                    ui.label("TPM模块:");
                                    ui.label(&tpm_str);
                                    ui.end_row();
                                    
                                    let secure_boot_str = if let Some(s) = sys_info { 
                                        if s.secure_boot { "已启用" } else { "未启用" } 
                                    } else { "未知" };
                                    ui.label("安全启动:");
                                    ui.label(secure_boot_str);
                                    ui.end_row();
                                    
                                    let bitlocker_str = match hw_info.system_bitlocker_status { 
                                        BitLockerStatus::Encrypted => "是", 
                                        BitLockerStatus::NotEncrypted => "否", 
                                        BitLockerStatus::EncryptionInProgress => "加密中", 
                                        BitLockerStatus::DecryptionInProgress => "解密中", 
                                        BitLockerStatus::Unknown => "未知", 
                                    };
                                    ui.label("BitLocker:");
                                    ui.label(bitlocker_str);
                                    ui.end_row();
                                });
                        });
                    
                    ui.add_space(5.0);
                    
                    // 电脑信息
                    egui::CollapsingHeader::new("电脑信息")
                        .default_open(true)
                        .show(ui, |ui| {
                            egui::Grid::new("computer_grid")
                                .num_columns(2)
                                .spacing([20.0, 4.0])
                                .striped(true)
                                .show(ui, |ui| {
                                    let mfr = crate::core::hardware_info::beautify_manufacturer_name(&hw_info.computer_manufacturer);
                                    
                                    ui.label("电脑型号:");
                                    ui.label(format!("{} {}", mfr, hw_info.computer_model));
                                    ui.end_row();
                                    
                                    ui.label("制造商:");
                                    ui.label(&mfr);
                                    ui.end_row();
                                    
                                    if !hw_info.system_serial_number.is_empty() {
                                        ui.label("设备编号:");
                                        ui.label(&hw_info.system_serial_number);
                                        ui.end_row();
                                    }
                                });
                        });
                    
                    ui.add_space(5.0);
                    
                    // 主板信息
                    egui::CollapsingHeader::new("主板信息")
                        .default_open(true)
                        .show(ui, |ui| {
                            egui::Grid::new("motherboard_grid")
                                .num_columns(2)
                                .spacing([20.0, 4.0])
                                .striped(true)
                                .show(ui, |ui| {
                                    ui.label("主板型号:");
                                    ui.label(if !hw_info.motherboard.product.is_empty() { &hw_info.motherboard.product } else { "未知" });
                                    ui.end_row();
                                    
                                    ui.label("主板编号:");
                                    ui.label(if !hw_info.motherboard.serial_number.is_empty() { &hw_info.motherboard.serial_number } else { "未知" });
                                    ui.end_row();
                                    
                                    ui.label("主板版本:");
                                    ui.label(if !hw_info.motherboard.version.is_empty() && !crate::core::hardware_info::is_placeholder_str(&hw_info.motherboard.version) { &hw_info.motherboard.version } else { "N/A" });
                                    ui.end_row();
                                    
                                    ui.label("BIOS版本:");
                                    ui.label(if !hw_info.bios.version.is_empty() { &hw_info.bios.version } else { "未知" });
                                    ui.end_row();
                                    
                                    ui.label("更新日期:");
                                    ui.label(if !hw_info.bios.release_date.is_empty() { &hw_info.bios.release_date } else { "未知" });
                                    ui.end_row();
                                });
                        });
                    
                    ui.add_space(5.0);
                    
                    // CPU信息
                    egui::CollapsingHeader::new("CPU信息")
                        .default_open(true)
                        .show(ui, |ui| {
                            egui::Grid::new("cpu_grid")
                                .num_columns(2)
                                .spacing([20.0, 4.0])
                                .striped(true)
                                .show(ui, |ui| {
                                    ui.label("CPU型号:");
                                    ui.label(&hw_info.cpu.name);
                                    ui.end_row();
                                    
                                    ui.label("核心/线程:");
                                    let ai_str = if hw_info.cpu.supports_ai { " [支持AI人工智能]" } else { "" };
                                    ui.label(format!("{} 核心 / {} 线程{}", hw_info.cpu.cores, hw_info.cpu.logical_processors, ai_str));
                                    ui.end_row();
                                    
                                    if hw_info.cpu.max_clock_speed > 0 {
                                        ui.label("最大频率:");
                                        ui.label(format!("{} MHz", hw_info.cpu.max_clock_speed));
                                        ui.end_row();
                                    }
                                });
                        });
                    
                    ui.add_space(5.0);
                    
                    // 内存信息
                    egui::CollapsingHeader::new("内存信息")
                        .default_open(true)
                        .show(ui, |ui| {
                            let total_gb = hw_info.memory.total_physical as f64 / (1024.0 * 1024.0 * 1024.0);
                            let available_gb = hw_info.memory.available_physical as f64 / (1024.0 * 1024.0 * 1024.0);
                            
                            ui.label(format!("总大小: {:.0} GB ({:.1} GB可用) 插槽数: {}", 
                                total_gb.round(), available_gb, hw_info.memory.slot_count));
                            
                            if !hw_info.memory.sticks.is_empty() {
                                ui.add_space(5.0);
                                egui::Grid::new("memory_sticks_grid")
                                    .num_columns(2)
                                    .spacing([20.0, 4.0])
                                    .striped(true)
                                    .show(ui, |ui| {
                                        for (i, stick) in hw_info.memory.sticks.iter().enumerate() {
                                            let mfr = crate::core::hardware_info::beautify_memory_manufacturer(&stick.manufacturer);
                                            let capacity_gb = stick.capacity / (1024 * 1024 * 1024);
                                            let mem_type = if !stick.memory_type.is_empty() { &stick.memory_type } else { "DDR" };
                                            let part = if !stick.part_number.is_empty() { &stick.part_number } else { "Unknown" };
                                            
                                            ui.label(format!("插槽 {}:", i + 1));
                                            ui.label(format!("{} {}/{}GB/{} {}", mfr, part, capacity_gb, mem_type, stick.speed));
                                            ui.end_row();
                                        }
                                    });
                            }
                        });
                    
                    ui.add_space(5.0);
                    
                    // 显卡信息
                    if !hw_info.gpus.is_empty() {
                        egui::CollapsingHeader::new("显卡信息")
                            .default_open(true)
                            .show(ui, |ui| {
                                egui::Grid::new("gpu_grid")
                                    .num_columns(2)
                                    .spacing([20.0, 4.0])
                                    .striped(true)
                                    .show(ui, |ui| {
                                        for (i, gpu) in hw_info.gpus.iter().enumerate() {
                                            ui.label(format!("显卡 {}:", i + 1));
                                            ui.label(crate::core::hardware_info::beautify_gpu_name(&gpu.name));
                                            ui.end_row();
                                        }
                                    });
                            });
                        
                        ui.add_space(5.0);
                    }
                    
                    // 网卡信息
                    if !hw_info.network_adapters.is_empty() {
                        egui::CollapsingHeader::new("网卡信息")
                            .default_open(true)
                            .show(ui, |ui| {
                                egui::Grid::new("network_grid")
                                    .num_columns(2)
                                    .spacing([20.0, 4.0])
                                    .striped(true)
                                    .show(ui, |ui| {
                                        for (i, adapter) in hw_info.network_adapters.iter().enumerate() {
                                            ui.label(format!("网卡 {}:", i + 1));
                                            ui.label(&adapter.description);
                                            ui.end_row();
                                        }
                                    });
                            });
                        
                        ui.add_space(5.0);
                    }
                    
                    // 电池信息
                    if let Some(battery) = &hw_info.battery {
                        egui::CollapsingHeader::new("电池信息")
                            .default_open(true)
                            .show(ui, |ui| {
                                egui::Grid::new("battery_grid")
                                    .num_columns(2)
                                    .spacing([20.0, 4.0])
                                    .striped(true)
                                    .show(ui, |ui| {
                                        let charging_str = if battery.is_charging { "充电中" } 
                                            else if battery.is_ac_connected { "未充电" } 
                                            else { "放电中" };
                                        
                                        ui.label("当前电量:");
                                        ui.label(format!("{}%  充电状态: {}", battery.charge_percent, charging_str));
                                        ui.end_row();
                                        
                                        if !battery.model.is_empty() {
                                            ui.label("型号:");
                                            ui.label(&battery.model);
                                            ui.end_row();
                                        }
                                        
                                        if !battery.manufacturer.is_empty() {
                                            ui.label("制造商:");
                                            ui.label(crate::core::hardware_info::beautify_manufacturer_name(&battery.manufacturer));
                                            ui.end_row();
                                        }
                                        
                                        if battery.design_capacity_mwh > 0 {
                                            ui.label("设计容量:");
                                            ui.label(format!("{} mWh", battery.design_capacity_mwh));
                                            ui.end_row();
                                        }
                                        
                                        if battery.full_charge_capacity_mwh > 0 {
                                            ui.label("最大容量:");
                                            ui.label(format!("{} mWh", battery.full_charge_capacity_mwh));
                                            ui.end_row();
                                        }
                                        
                                        if battery.current_capacity_mwh > 0 {
                                            ui.label("当前容量:");
                                            ui.label(format!("{} mWh", battery.current_capacity_mwh));
                                            ui.end_row();
                                        }
                                    });
                            });
                        
                        ui.add_space(5.0);
                    }
                    
                    // 硬盘信息
                    if !hw_info.disks.is_empty() {
                        egui::CollapsingHeader::new("硬盘信息")
                            .default_open(true)
                            .show(ui, |ui| {
                                egui::Grid::new("disk_grid")
                                    .num_columns(2)
                                    .spacing([20.0, 4.0])
                                    .striped(true)
                                    .show(ui, |ui| {
                                        for (i, disk) in hw_info.disks.iter().enumerate() {
                                            let size_gb = disk.size as f64 / (1024.0 * 1024.0 * 1024.0);
                                            let ssd_str = if disk.is_ssd { "固态" } else { "机械" };
                                            let partition_style = if !disk.partition_style.is_empty() { &disk.partition_style } else { "未知" };
                                            
                                            ui.label(format!("硬盘 {}:", i + 1));
                                            ui.label(format!("{} [{:.1}GB-{}-{}-{}]", 
                                                disk.model, size_gb, disk.interface_type, partition_style, ssd_str));
                                            ui.end_row();
                                        }
                                    });
                            });
                        
                        ui.add_space(5.0);
                    }
                    
                    // 磁盘分区信息
                    egui::CollapsingHeader::new("磁盘分区详情")
                        .default_open(true)
                        .show(ui, |ui| {
                            let is_pe = self.system_info.as_ref().map(|s| s.is_pe_environment).unwrap_or(false);
                            
                            egui::Grid::new("partition_grid")
                                .striped(true)
                                .min_col_width(60.0)
                                .show(ui, |ui| {
                                    ui.label("分区");
                                    ui.label("卷标");
                                    ui.label("总容量");
                                    ui.label("可用");
                                    ui.label("使用率");
                                    ui.end_row();

                                    for partition in &self.partitions {
                                        let used = partition.total_size_mb - partition.free_size_mb;
                                        let usage = if partition.total_size_mb > 0 {
                                            (used as f64 / partition.total_size_mb as f64) * 100.0
                                        } else {
                                            0.0
                                        };

                                        let label = if is_pe {
                                            if partition.letter.to_uppercase() == "X:" {
                                                format!("{} (PE)", partition.letter)
                                            } else if partition.has_windows {
                                                format!("{} (Win)", partition.letter)
                                            } else {
                                                partition.letter.clone()
                                            }
                                        } else {
                                            if partition.is_system_partition {
                                                format!("{} (系统)", partition.letter)
                                            } else {
                                                partition.letter.clone()
                                            }
                                        };

                                        ui.label(label);
                                        ui.label(&partition.label);
                                        ui.label(Self::format_size(partition.total_size_mb));
                                        ui.label(Self::format_size(partition.free_size_mb));
                                        ui.label(format!("{:.0}%", usage));
                                        ui.end_row();
                                    }
                                });
                        });

                } else {
                    ui.spinner();
                    ui.label("正在加载硬件信息...");
                }
            });
    }
    
    /// 导出硬件信息为TXT文件
    fn export_hardware_info_to_txt(&self) {
        let Some(hw_info) = &self.hardware_info else {
            return;
        };
        
        // 生成完整的硬件信息文本（包含分区信息）
        let export_content = self.generate_full_hardware_report(hw_info);
        
        // 生成默认文件名（包含计算机名和日期）
        let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let computer_name = if hw_info.computer_name.is_empty() {
            "Computer"
        } else {
            &hw_info.computer_name
        };
        let default_filename = format!("硬件信息_{}_{}.txt", computer_name, timestamp);
        
        // 显示文件保存对话框
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("文本文件", &["txt"])
            .set_file_name(&default_filename)
            .save_file()
        {
            // 写入文件
            if let Err(e) = std::fs::write(&path, export_content) {
                log::error!("导出硬件信息失败: {}", e);
            } else {
                log::info!("硬件信息已导出至: {}", path.display());
            }
        }
    }
    
    /// 生成完整的硬件信息报告文本
    fn generate_full_hardware_report(&self, hw_info: &crate::core::hardware_info::HardwareInfo) -> String {
        use std::fmt::Write;
        
        let mut report = String::with_capacity(4096);
        
        // 报告头部
        let _ = writeln!(report, "╔══════════════════════════════════════════════════════════════╗");
        let _ = writeln!(report, "║                      系统与硬件信息报告                      ║");
        let _ = writeln!(report, "╠══════════════════════════════════════════════════════════════╣");
        let _ = writeln!(report, "║  生成时间: {}                          ║", 
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
        let _ = writeln!(report, "╚══════════════════════════════════════════════════════════════╝");
        let _ = writeln!(report);
        
        // 基础硬件信息
        let _ = writeln!(report, "{}", hw_info.to_formatted_text(self.system_info.as_ref()));
        
        // 分区信息
        if !self.partitions.is_empty() {
            let _ = writeln!(report);
            let _ = writeln!(report, "═══════════════════════════════════════════════════════════════");
            let _ = writeln!(report, "                         磁盘分区详情");
            let _ = writeln!(report, "═══════════════════════════════════════════════════════════════");
            let _ = writeln!(report);
            let _ = writeln!(report, "{:<10} {:<15} {:>12} {:>12} {:>10}", 
                "分区", "卷标", "总容量", "可用", "使用率");
            let _ = writeln!(report, "{}", "-".repeat(63));
            
            let is_pe = self.system_info.as_ref().map(|s| s.is_pe_environment).unwrap_or(false);
            
            for partition in &self.partitions {
                let used = partition.total_size_mb - partition.free_size_mb;
                let usage = if partition.total_size_mb > 0 {
                    (used as f64 / partition.total_size_mb as f64) * 100.0
                } else {
                    0.0
                };
                
                let label = if is_pe {
                    if partition.letter.to_uppercase() == "X:" {
                        format!("{} (PE)", partition.letter)
                    } else if partition.has_windows {
                        format!("{} (Win)", partition.letter)
                    } else {
                        partition.letter.clone()
                    }
                } else if partition.is_system_partition {
                    format!("{} (系统)", partition.letter)
                } else {
                    partition.letter.clone()
                };
                
                let _ = writeln!(report, "{:<10} {:<15} {:>12} {:>12} {:>9.0}%",
                    label,
                    Self::truncate_string(&partition.label, 13),
                    Self::format_size(partition.total_size_mb),
                    Self::format_size(partition.free_size_mb),
                    usage
                );
            }
        }
        
        // 报告尾部
        let _ = writeln!(report);
        let _ = writeln!(report, "═══════════════════════════════════════════════════════════════");
        let _ = writeln!(report, "                    由 LetRecovery 生成");
        let _ = writeln!(report, "═══════════════════════════════════════════════════════════════");
        
        report
    }
    
    /// 截断字符串到指定长度，超出部分用省略号表示
    fn truncate_string(s: &str, max_len: usize) -> String {
        if s.chars().count() <= max_len {
            s.to_string()
        } else {
            let truncated: String = s.chars().take(max_len.saturating_sub(2)).collect();
            format!("{}…", truncated)
        }
    }
}