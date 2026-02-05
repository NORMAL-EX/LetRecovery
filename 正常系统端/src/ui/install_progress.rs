use egui;
use std::sync::mpsc;
use std::path::Path;

use crate::app::{App, BootModeSelection, InstallMode};
use crate::core::dism::DismProgress;
use crate::core::disk::{Partition, PartitionStyle};
use crate::core::ghost::Ghost;
use crate::core::install_config::{ConfigFileManager, InstallConfig};
use crate::ui::advanced_options::AdvancedOptions;

impl App {
    pub fn show_install_progress(&mut self, ui: &mut egui::Ui) {
        ui.heading("安装进度");
        ui.separator();

        self.update_install_progress();

        if !self.is_installing {
            ui.label("没有正在进行的安装任务");
            if ui.button("返回").clicked() {
                self.current_panel = crate::app::Panel::SystemInstall;
            }
            return;
        }

        // 显示安装模式
        let mode_text = match self.install_mode {
            InstallMode::Direct => "直接安装",
            InstallMode::ViaPE => "通过PE安装",
        };
        ui.label(format!("安装模式: {}", mode_text));

        ui.add_space(15.0);
        ui.label(format!(
            "当前步骤: {}",
            self.install_progress.current_step
        ));

        ui.add(
            egui::ProgressBar::new(self.install_progress.step_progress as f32 / 100.0)
                .text(format!("{}%", self.install_progress.step_progress))
                .animate(true),
        );

        ui.add_space(10.0);

        ui.label("总体进度:");
        ui.add(
            egui::ProgressBar::new(self.install_progress.total_progress as f32 / 100.0)
                .text(format!("{}%", self.install_progress.total_progress))
                .animate(true),
        );

        ui.add_space(20.0);

        // 安装步骤列表
        ui.label("安装步骤:");
        egui::ScrollArea::vertical()
            .max_height(200.0)
            .show(ui, |ui| {
                let mut steps = match self.install_mode {
                    InstallMode::Direct => vec![
                        "格式化分区",
                        "导出驱动",
                        "释放系统镜像",
                        "导入驱动",
                        "修复引导",
                        "应用高级选项",
                        "完成安装",
                    ],
                    InstallMode::ViaPE => vec![
                        "检查PE环境",
                        "安装PE引导",
                        "导出驱动",
                        "复制镜像文件",
                        "写入配置文件",
                        "准备重启",
                    ],
                };

                // 如果需要 BitLocker 解密，插入解密步骤作为第一步
                if self.bitlocker_decryption_needed {
                    steps.insert(0, "解密 BitLocker 分区");
                }

                // 计算有效步骤索引（用于显示）
                let effective_install_step = if self.bitlocker_decryption_needed {
                    if self.install_step == 0 {
                        1
                    } else {
                        self.install_step + 1
                    }
                } else {
                    self.install_step
                };

                for (i, step) in steps.iter().enumerate() {
                    let step_num = i + 1;
                    let is_current = effective_install_step == step_num;
                    let is_completed = effective_install_step > step_num;

                    let prefix = if is_completed {
                        "✓"
                    } else if is_current {
                        "→"
                    } else {
                        "○"
                    };

                    let color = if is_completed {
                        egui::Color32::GREEN
                    } else if is_current {
                        egui::Color32::from_rgb(255, 165, 0)
                    } else {
                        egui::Color32::GRAY
                    };

                    ui.colored_label(color, format!("{} {}. {}", prefix, step_num, step));
                }
            });

        ui.add_space(20.0);

        if let Some(ref error) = self.install_error {
            ui.colored_label(egui::Color32::RED, format!("错误: {}", error));
            ui.add_space(10.0);
        }

        // 安装完成后的操作
        if self.install_progress.total_progress >= 100 {
            match self.install_mode {
                InstallMode::Direct => {
                    ui.colored_label(egui::Color32::GREEN, "安装完成！");
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("立即重启").clicked() {
                            self.reboot_system();
                        }
                        if ui.button("返回主页").clicked() {
                            self.is_installing = false;
                            self.current_panel = crate::app::Panel::SystemInstall;
                        }
                    });
                }
                InstallMode::ViaPE => {
                    ui.colored_label(egui::Color32::GREEN, "PE环境准备完成！");
                    ui.label("系统将重启进入PE环境继续安装。");
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("立即重启").clicked() {
                            self.reboot_system();
                        }
                        if ui.button("稍后重启").clicked() {
                            self.is_installing = false;
                            self.current_panel = crate::app::Panel::SystemInstall;
                        }
                    });
                }
            }
        } else {
            if ui.button("取消安装").clicked() {
                println!("[INSTALL] 用户取消安装");
                self.is_installing = false;
                self.current_panel = crate::app::Panel::SystemInstall;
            }
        }

        // 启动安装线程
        if self.install_step == 0 && self.is_installing && self.decrypting_partitions.is_empty() {
            match self.install_mode {
                InstallMode::Direct => self.start_direct_install_thread(),
                InstallMode::ViaPE => self.start_pe_install_thread(),
            }
        }
    }

    fn update_install_progress(&mut self) {
        if let Some(ref rx) = self.install_progress_rx {
            while let Ok(progress) = rx.try_recv() {
                // 处理 BitLocker 解密状态
                if progress.status == "DECRYPTION_COMPLETE" {
                    println!("[INSTALL UI] BitLocker 解密完成，准备开始安装");
                    self.decrypting_partitions.clear();
                    self.install_progress.current_step = "准备开始安装...".to_string();
                    return;
                } else if progress.status.starts_with("DECRYPTING:") {
                    self.install_progress.current_step = progress.status.trim_start_matches("DECRYPTING:").to_string();
                    // 使用实际的解密进度（从加密百分比计算得出）
                    self.install_progress.step_progress = progress.percentage;
                    return;
                }

                if let Some((step, name)) = parse_step_from_status(&progress.status) {
                    self.install_progress.step_progress = progress.percentage;
                    
                    if step != self.install_step || self.install_progress.current_step != name {
                        self.install_step = step;
                        self.install_progress.current_step = name.clone();
                        println!("[INSTALL UI] 步骤更新: {} - {} ({}%)", step, name, progress.percentage);
                    }
                    
                    // 计算总进度
                    let (base_progress, step_weight) = match self.install_mode {
                        InstallMode::Direct => {
                            let base = match step {
                                1 => 0,
                                2 => 5,
                                3 => 10,
                                4 => 90,
                                5 => 93,
                                6 => 96,
                                7 => 100,
                                _ => 0,
                            };
                            let weight = if step == 3 { 80 } else { 3 };
                            (base, weight)
                        }
                        InstallMode::ViaPE => {
                            let base = match step {
                                1 => 0,
                                2 => 10,
                                3 => 30,
                                4 => 50,
                                5 => 90,
                                6 => 100,
                                _ => 0,
                            };
                            let weight = match step {
                                4 => 40,
                                _ => 10,
                            };
                            (base, weight)
                        }
                    };
                    
                    self.install_progress.total_progress = 
                        (base_progress + (progress.percentage as usize * step_weight / 100)).min(100) as u8;
                    
                    // 检查是否安装完成，并且用户勾选了自动重启
                    if self.install_progress.total_progress >= 100 
                        && self.install_options.auto_reboot 
                        && !self.auto_reboot_triggered 
                    {
                        println!("[INSTALL] 安装完成，用户已勾选立即重启，执行自动重启");
                        self.auto_reboot_triggered = true;
                        self.reboot_system();
                    }
                }
            }
        }
    }

    /// 直接安装线程
    fn start_direct_install_thread(&mut self) {
        println!("[INSTALL] ========== 开始直接安装 ==========");
        println!("[INSTALL] 目标分区: {}", self.install_target_partition);
        println!("[INSTALL] 镜像路径: {}", self.install_image_path);
        println!("[INSTALL] 镜像索引: {}", self.install_volume_index);

        let (progress_tx, progress_rx) = mpsc::channel::<DismProgress>();
        self.install_progress_rx = Some(progress_rx);

        let target_partition = self.install_target_partition.clone();
        let image_path = self.install_image_path.clone();
        let volume_index = self.install_volume_index;
        let options = self.install_options.clone();
        let advanced_options = self.advanced_options.clone();
        let partitions: Vec<Partition> = self.partitions.clone();
        
        let partition_style = self.partitions
            .iter()
            .find(|p| p.letter == target_partition)
            .map(|p| p.partition_style)
            .unwrap_or(PartitionStyle::Unknown);

        self.install_step = 1;
        self.install_progress.current_step = "格式化分区".to_string();

        std::thread::spawn(move || {
            println!("[INSTALL THREAD] 安装线程启动");
            
            let temp_dir = std::env::temp_dir();
            let driver_backup_path = temp_dir.join("LetRecovery_DriverBackup");
            let driver_backup_str = driver_backup_path.to_string_lossy().to_string();

            // Step 1: 格式化分区
            send_step(&progress_tx, 1, "格式化分区", 0);
            std::thread::sleep(std::time::Duration::from_millis(50));
            if options.format_partition {
                println!("[INSTALL STEP 1] 开始格式化分区: {}", target_partition);
                send_step(&progress_tx, 1, "格式化分区", 30);
                match format_partition(&target_partition) {
                    Ok(_) => println!("[INSTALL STEP 1] 格式化完成"),
                    Err(e) => println!("[INSTALL STEP 1] 格式化失败: {}", e),
                }
                send_step(&progress_tx, 1, "格式化分区", 100);
            } else {
                println!("[INSTALL STEP 1] 跳过格式化");
                send_step(&progress_tx, 1, "格式化分区", 100);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Step 2: 导出驱动
            send_step(&progress_tx, 2, "导出驱动", 0);
            std::thread::sleep(std::time::Duration::from_millis(50));
            if options.export_drivers {
                println!("[INSTALL STEP 2] 开始导出驱动到: {}", driver_backup_str);
                send_step(&progress_tx, 2, "导出驱动", 20);
                
                match export_drivers(&driver_backup_str) {
                    Ok(_) => {
                        println!("[INSTALL STEP 2] 驱动导出成功");
                        send_step(&progress_tx, 2, "导出驱动", 100);
                    }
                    Err(e) => {
                        println!("[INSTALL STEP 2] 驱动导出失败: {} (继续安装)", e);
                        send_step(&progress_tx, 2, "导出驱动", 100);
                    }
                }
            } else {
                println!("[INSTALL STEP 2] 跳过导出驱动");
                send_step(&progress_tx, 2, "导出驱动", 100);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Step 3: 释放系统镜像
            send_step(&progress_tx, 3, "释放系统镜像", 0);
            std::thread::sleep(std::time::Duration::from_millis(50));
            println!("[INSTALL STEP 3] 开始释放系统镜像");

            let image_lower = image_path.to_lowercase();
            let is_gho = image_lower.ends_with(".gho") || image_lower.ends_with(".ghs");

            if is_gho {
                println!("[INSTALL STEP 3] 检测到 GHO 镜像，使用 Ghost 恢复");
                
                let ghost = Ghost::new();
                
                if !ghost.is_available() {
                    println!("[INSTALL STEP 3] 错误: Ghost 可执行文件不存在");
                    send_step(&progress_tx, 3, "释放系统镜像", 100);
                } else {
                    let ghost_tx = progress_tx.clone();
                    let (inner_tx, inner_rx) = mpsc::channel::<DismProgress>();
                    
                    std::thread::spawn(move || {
                        while let Ok(p) = inner_rx.recv() {
                            let _ = ghost_tx.send(p);
                        }
                    });
                    
                    match ghost.restore_image_to_letter(&image_path, &target_partition, &partitions, Some(inner_tx)) {
                        Ok(_) => println!("[INSTALL STEP 3] Ghost 镜像恢复成功"),
                        Err(e) => println!("[INSTALL STEP 3] Ghost 镜像恢复失败: {}", e),
                    }
                }
                
                send_step(&progress_tx, 3, "释放系统镜像", 100);
            } else {
                println!("[INSTALL STEP 3] 使用 DISM 应用 WIM/ESD 镜像");
                let dism = crate::core::dism::Dism::new();
                let apply_dir = format!("{}\\", target_partition);
                
                let step_tx = progress_tx.clone();
                let (inner_tx, inner_rx) = mpsc::channel::<DismProgress>();
                
                std::thread::spawn(move || {
                    while let Ok(p) = inner_rx.recv() {
                        let _ = step_tx.send(DismProgress {
                            percentage: p.percentage,
                            status: "STEP:3:释放系统镜像".to_string(),
                        });
                    }
                });
                
                match dism.apply_image(&image_path, &apply_dir, volume_index, Some(inner_tx)) {
                    Ok(_) => println!("[INSTALL STEP 3] DISM 镜像释放成功"),
                    Err(e) => println!("[INSTALL STEP 3] DISM 镜像释放失败: {}", e),
                }
                send_step(&progress_tx, 3, "释放系统镜像", 100);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Step 4: 导入驱动（仅在 AutoImport 模式下导入）
            send_step(&progress_tx, 4, "导入驱动", 0);
            std::thread::sleep(std::time::Duration::from_millis(50));
            
            // 判断是否需要导入驱动（只有 AutoImport 模式才导入）
            let should_import = matches!(options.driver_action, crate::app::DriverAction::AutoImport);
            
            if should_import && driver_backup_path.exists() {
                println!("[INSTALL STEP 4] 开始导入驱动 (AutoImport模式)");
                send_step(&progress_tx, 4, "导入驱动", 30);
                
                match import_drivers(&target_partition, &driver_backup_str) {
                    Ok(_) => {
                        println!("[INSTALL STEP 4] 驱动导入成功");
                        let _ = std::fs::remove_dir_all(&driver_backup_path);
                        send_step(&progress_tx, 4, "导入驱动", 100);
                    }
                    Err(e) => {
                        println!("[INSTALL STEP 4] 驱动导入失败: {}", e);
                        let _ = std::fs::remove_dir_all(&driver_backup_path);
                        send_step(&progress_tx, 4, "导入驱动", 100);
                    }
                }
            } else if matches!(options.driver_action, crate::app::DriverAction::SaveOnly) && driver_backup_path.exists() {
                // SaveOnly 模式：保留驱动备份到目标分区
                println!("[INSTALL STEP 4] 仅保存驱动 (SaveOnly模式)");
                send_step(&progress_tx, 4, "保存驱动", 30);
                
                let target_driver_dir = format!("{}\\LetRecovery_Drivers", target_partition);
                if let Err(e) = copy_dir_recursive(&driver_backup_str, &target_driver_dir) {
                    println!("[INSTALL STEP 4] 保存驱动到目标分区失败: {}", e);
                } else {
                    println!("[INSTALL STEP 4] 驱动已保存到: {}", target_driver_dir);
                }
                
                let _ = std::fs::remove_dir_all(&driver_backup_path);
                send_step(&progress_tx, 4, "保存驱动", 100);
            } else {
                println!("[INSTALL STEP 4] 跳过驱动处理 (driver_action: {:?})", options.driver_action);
                send_step(&progress_tx, 4, "导入驱动", 100);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Step 5: 修复引导
            send_step(&progress_tx, 5, "修复引导", 0);
            std::thread::sleep(std::time::Duration::from_millis(50));
            if options.repair_boot {
                println!("[INSTALL STEP 5] 开始修复引导");
                send_step(&progress_tx, 5, "修复引导", 20);
                
                let use_uefi = match options.boot_mode {
                    BootModeSelection::UEFI => true,
                    BootModeSelection::Legacy => false,
                    BootModeSelection::Auto => matches!(partition_style, PartitionStyle::GPT),
                };
                
                println!("[INSTALL STEP 5] 引导模式: {}", if use_uefi { "UEFI" } else { "Legacy" });
                send_step(&progress_tx, 5, "修复引导", 50);
                
                let boot_manager = crate::core::bcdedit::BootManager::new();
                match boot_manager.repair_boot_advanced(&target_partition, use_uefi) {
                    Ok(_) => {
                        println!("[INSTALL STEP 5] 引导修复成功");
                        
                        // 如果是 Win7 + UEFI 模式，且启用了 UefiSeven 补丁
                        if use_uefi && advanced_options.win7_uefi_patch {
                            println!("[INSTALL STEP 5] 检测到 Win7 UEFI 补丁选项，开始应用 UefiSeven");
                            send_step(&progress_tx, 5, "应用Win7 UEFI补丁", 70);
                            
                            match advanced_options.apply_uefiseven_patch(&target_partition) {
                                Ok(_) => println!("[INSTALL STEP 5] UefiSeven 补丁应用成功"),
                                Err(e) => println!("[INSTALL STEP 5] UefiSeven 补丁应用失败: {} (继续安装)", e),
                            }
                        }
                    }
                    Err(e) => println!("[INSTALL STEP 5] 引导修复失败: {}", e),
                }
                send_step(&progress_tx, 5, "修复引导", 100);
            } else {
                println!("[INSTALL STEP 5] 跳过修复引导");
                send_step(&progress_tx, 5, "修复引导", 100);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Step 6: 应用高级选项
            send_step(&progress_tx, 6, "应用高级选项", 0);
            std::thread::sleep(std::time::Duration::from_millis(50));
            println!("[INSTALL STEP 6] 应用高级选项");
            send_step(&progress_tx, 6, "应用高级选项", 20);
            
            match advanced_options.apply_to_system(&target_partition) {
                Ok(_) => println!("[INSTALL STEP 6] 高级选项应用成功"),
                Err(e) => println!("[INSTALL STEP 6] 高级选项应用失败: {}", e),
            }
            send_step(&progress_tx, 6, "应用高级选项", 50);
            
            if options.unattended_install {
                println!("[INSTALL STEP 6] 生成无人值守配置");
                match generate_unattend_xml(&target_partition, &advanced_options) {
                    Ok(_) => println!("[INSTALL STEP 6] 无人值守配置生成成功"),
                    Err(e) => println!("[INSTALL STEP 6] 无人值守配置生成失败: {}", e),
                }
            }
            send_step(&progress_tx, 6, "应用高级选项", 100);
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Step 7: 完成
            send_step(&progress_tx, 7, "完成安装", 100);
            println!("[INSTALL STEP 7] 安装完成!");
            println!("[INSTALL] ========== 安装结束 ==========");
        });
    }

    /// 通过PE安装线程
    fn start_pe_install_thread(&mut self) {
        println!("[INSTALL PE] ========== 开始PE安装准备 ==========");
        println!("[INSTALL PE] 目标分区: {}", self.install_target_partition);
        println!("[INSTALL PE] 镜像路径: {}", self.install_image_path);

        let (progress_tx, progress_rx) = mpsc::channel::<DismProgress>();
        self.install_progress_rx = Some(progress_rx);

        let target_partition = self.install_target_partition.clone();
        let image_path = self.install_image_path.clone();
        let volume_index = self.install_volume_index;
        let options = self.install_options.clone();
        let advanced_options = self.advanced_options.clone();
        
        // 获取选中的PE信息
        let pe_info = self.selected_pe_for_install.and_then(|idx| {
            self.config.as_ref().and_then(|c| c.pe_list.get(idx).cloned())
        });

        self.install_step = 1;
        self.install_progress.current_step = "检查PE环境".to_string();

        std::thread::spawn(move || {
            println!("[INSTALL PE THREAD] PE安装线程启动");

            // Step 1: 检查PE环境
            send_step(&progress_tx, 1, "检查PE环境", 0);
            std::thread::sleep(std::time::Duration::from_millis(50));
            
            let pe_info = match pe_info {
                Some(pe) => pe,
                None => {
                    println!("[INSTALL PE STEP 1] 错误: 未选择PE环境");
                    send_step(&progress_tx, 1, "检查PE环境", 100);
                    return;
                }
            };
            
            println!("[INSTALL PE STEP 1] 检查PE: {}", pe_info.display_name);
            send_step(&progress_tx, 1, "检查PE环境", 50);
            
            let (pe_exists, pe_path) = crate::core::pe::PeManager::check_pe_exists(&pe_info.filename);
            if !pe_exists {
                println!("[INSTALL PE STEP 1] PE文件不存在，需要下载");
                // 这里应该触发下载，但为了简化，我们直接返回错误
                send_step(&progress_tx, 1, "检查PE环境", 100);
                return;
            }
            
            println!("[INSTALL PE STEP 1] PE文件存在: {}", pe_path);
            send_step(&progress_tx, 1, "检查PE环境", 100);
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Step 2: 安装PE引导
            send_step(&progress_tx, 2, "安装PE引导", 0);
            std::thread::sleep(std::time::Duration::from_millis(50));
            
            println!("[INSTALL PE STEP 2] 安装PE引导");
            send_step(&progress_tx, 2, "安装PE引导", 30);
            
            let pe_manager = crate::core::pe::PeManager::new();
            match pe_manager.boot_to_pe(&pe_path, &pe_info.display_name) {
                Ok(_) => println!("[INSTALL PE STEP 2] PE引导安装成功"),
                Err(e) => {
                    println!("[INSTALL PE STEP 2] PE引导安装失败: {}", e);
                    send_step(&progress_tx, 2, "安装PE引导", 100);
                    return;
                }
            }
            send_step(&progress_tx, 2, "安装PE引导", 100);
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Step 3: 导出驱动
            send_step(&progress_tx, 3, "导出驱动", 0);
            std::thread::sleep(std::time::Duration::from_millis(50));
            
            // 找一个可用的数据分区来存储数据（传入镜像路径以检查空间）
            let (data_partition, _is_auto_created) = match find_data_partition(&target_partition, &image_path) {
                Ok(result) => result,
                Err(e) => {
                    println!("[INSTALL PE STEP 3] 查找数据分区失败: {}", e);
                    let _ = progress_tx.send(DismProgress {
                        percentage: 0,
                        status: format!("ERROR:{}", e),
                    });
                    return;
                }
            };
            
            let data_dir = ConfigFileManager::get_data_dir(&data_partition);
            std::fs::create_dir_all(&data_dir).ok();
            
            // 根据driver_action决定是否导出驱动
            let should_export = matches!(
                options.driver_action, 
                crate::app::DriverAction::SaveOnly | crate::app::DriverAction::AutoImport
            );
            
            if should_export {
                println!("[INSTALL PE STEP 3] 导出驱动到: {} (driver_action: {:?})", data_dir, options.driver_action);
                send_step(&progress_tx, 3, "导出驱动", 30);
                
                let driver_path = format!("{}\\drivers", data_dir);
                match export_drivers(&driver_path) {
                    Ok(_) => println!("[INSTALL PE STEP 3] 驱动导出成功"),
                    Err(e) => println!("[INSTALL PE STEP 3] 驱动导出失败: {}", e),
                }
            } else {
                println!("[INSTALL PE STEP 3] 跳过驱动导出 (driver_action: {:?})", options.driver_action);
            }
            send_step(&progress_tx, 3, "导出驱动", 100);
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Step 4: 复制镜像文件
            send_step(&progress_tx, 4, "复制镜像文件", 0);
            std::thread::sleep(std::time::Duration::from_millis(50));
            
            println!("[INSTALL PE STEP 4] 复制镜像文件到数据分区");
            let image_filename = Path::new(&image_path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let target_image_path = format!("{}\\{}", data_dir, image_filename);
            
            // 使用带进度的复制函数
            match copy_file_with_progress(&image_path, &target_image_path, |progress| {
                send_step(&progress_tx, 4, "复制镜像文件", progress);
            }) {
                Ok(_) => println!("[INSTALL PE STEP 4] 镜像复制成功: {}", target_image_path),
                Err(e) => {
                    println!("[INSTALL PE STEP 4] 镜像复制失败: {}", e);
                    // 发送错误状态，不是100%
                    let _ = progress_tx.send(DismProgress {
                        percentage: 0,
                        status: format!("ERROR:复制失败: {}", e),
                    });
                    return;
                }
            }
            send_step(&progress_tx, 4, "复制镜像文件", 100);
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Step 4.5: 如果启用了 Win7 UEFI 补丁，复制 UefiSeven 文件到数据目录
            if advanced_options.win7_uefi_patch {
                println!("[INSTALL PE STEP 4.5] 复制 UefiSeven 文件到数据分区");
                let uefiseven_dir = format!("{}\\uefiseven", data_dir);
                let _ = std::fs::create_dir_all(&uefiseven_dir);
                
                // 从程序目录复制 UefiSeven 文件
                if let Some(program_dir) = std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                {
                    let source_uefiseven_dir = program_dir.join("uefiseven");
                    if source_uefiseven_dir.exists() {
                        // 复制 bootx64.efi
                        let src_efi = source_uefiseven_dir.join("bootx64.efi");
                        let dst_efi = format!("{}\\bootx64.efi", uefiseven_dir);
                        if src_efi.exists() {
                            match std::fs::copy(&src_efi, &dst_efi) {
                                Ok(_) => println!("[INSTALL PE STEP 4.5] 复制 UefiSeven bootx64.efi 成功"),
                                Err(e) => println!("[INSTALL PE STEP 4.5] 复制 UefiSeven bootx64.efi 失败: {}", e),
                            }
                        }
                        
                        // 复制 UefiSeven.ini（如果存在）
                        let src_ini = source_uefiseven_dir.join("UefiSeven.ini");
                        let dst_ini = format!("{}\\UefiSeven.ini", uefiseven_dir);
                        if src_ini.exists() {
                            match std::fs::copy(&src_ini, &dst_ini) {
                                Ok(_) => println!("[INSTALL PE STEP 4.5] 复制 UefiSeven.ini 成功"),
                                Err(e) => println!("[INSTALL PE STEP 4.5] 复制 UefiSeven.ini 失败: {}", e),
                            }
                        }
                    } else {
                        println!("[INSTALL PE STEP 4.5] 警告: UefiSeven 源目录不存在: {}", source_uefiseven_dir.display());
                    }
                }
            }

            // Step 5: 写入配置文件
            send_step(&progress_tx, 5, "写入配置文件", 0);
            std::thread::sleep(std::time::Duration::from_millis(50));
            
            println!("[INSTALL PE STEP 5] 写入配置文件");
            
            let is_gho = image_path.to_lowercase().ends_with(".gho") 
                || image_path.to_lowercase().ends_with(".ghs");
            
            let install_config = InstallConfig {
                unattended: options.unattended_install,
                restore_drivers: options.export_drivers,
                driver_action_mode: InstallConfig::driver_action_to_mode(options.driver_action),
                auto_reboot: options.auto_reboot,
                original_guid: String::new(),
                volume_index,
                target_partition: target_partition.clone(),
                image_path: image_filename,
                is_gho,
                remove_shortcut_arrow: advanced_options.remove_shortcut_arrow,
                restore_classic_context_menu: advanced_options.restore_classic_context_menu,
                bypass_nro: advanced_options.bypass_nro,
                disable_windows_update: advanced_options.disable_windows_update,
                disable_windows_defender: advanced_options.disable_windows_defender,
                disable_reserved_storage: advanced_options.disable_reserved_storage,
                disable_uac: advanced_options.disable_uac,
                disable_device_encryption: advanced_options.disable_device_encryption,
                remove_uwp_apps: advanced_options.remove_uwp_apps,
                import_storage_controller_drivers: advanced_options.import_storage_controller_drivers,
                custom_username: if advanced_options.custom_username {
                    advanced_options.username.clone()
                } else {
                    String::new()
                },
                volume_label: if advanced_options.custom_volume_label {
                    advanced_options.volume_label.clone()
                } else {
                    String::new()
                },
                win7_uefi_patch: advanced_options.win7_uefi_patch,
                win7_inject_usb3_driver: advanced_options.win7_inject_usb3_driver,
                win7_inject_nvme_driver: advanced_options.win7_inject_nvme_driver,
                win7_fix_acpi_bsod: advanced_options.win7_fix_acpi_bsod,
                win7_fix_storage_bsod: advanced_options.win7_fix_storage_bsod,
            };
            
            match ConfigFileManager::write_install_config(&target_partition, &data_partition, &install_config) {
                Ok(_) => println!("[INSTALL PE STEP 5] 配置文件写入成功"),
                Err(e) => println!("[INSTALL PE STEP 5] 配置文件写入失败: {}", e),
            }
            
            send_step(&progress_tx, 5, "写入配置文件", 100);
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Step 6: 准备重启
            send_step(&progress_tx, 6, "准备重启", 100);
            println!("[INSTALL PE STEP 6] PE安装准备完成，等待重启");
            println!("[INSTALL PE] ========== PE安装准备结束 ==========");
        });
    }

    fn reboot_system(&self) {
        println!("[INSTALL] 执行重启命令");
        let _ = crate::utils::cmd::create_command("shutdown")
            .args(["/r", "/t", "5", "/c", "LetRecovery 系统安装完成，即将重启..."])
            .spawn();
    }
}

/// 发送步骤消息
fn send_step(tx: &mpsc::Sender<DismProgress>, step: usize, name: &str, percentage: u8) {
    let _ = tx.send(DismProgress {
        percentage,
        status: format!("STEP:{}:{}", step, name),
    });
}

/// 从状态字符串解析步骤号和名称
fn parse_step_from_status(status: &str) -> Option<(usize, String)> {
    if status.starts_with("STEP:") {
        let parts: Vec<&str> = status.splitn(3, ':').collect();
        if parts.len() >= 3 {
            if let Ok(step) = parts[1].parse::<usize>() {
                return Some((step, parts[2].to_string()));
            }
        }
    }
    None
}

/// 格式化分区
fn format_partition(partition: &str) -> anyhow::Result<()> {
    use crate::utils::cmd::create_command;
    
    println!("[FORMAT] 格式化分区: {}", partition);
    
    let output = create_command("cmd")
        .args(["/c", &format!("format {} /FS:NTFS /Q /Y", partition)])
        .output()?;
    
    let stdout = crate::utils::encoding::gbk_to_utf8(&output.stdout);
    let stderr = crate::utils::encoding::gbk_to_utf8(&output.stderr);
    
    println!("[FORMAT] stdout: {}", stdout);
    println!("[FORMAT] stderr: {}", stderr);
    
    if !output.status.success() {
        anyhow::bail!("格式化失败: {}", stderr);
    }
    
    Ok(())
}

/// 导出驱动
fn export_drivers(destination: &str) -> anyhow::Result<()> {
    println!("[DRIVER EXPORT] 目标路径: {}", destination);
    
    if Path::new(destination).exists() {
        let _ = std::fs::remove_dir_all(destination);
    }
    
    std::fs::create_dir_all(destination)?;
    
    let dism = crate::core::dism::Dism::new();
    
    if dism.is_pe_environment() {
        println!("[DRIVER EXPORT] PE 环境，查找现有 Windows 系统...");
        
        for letter in ['C', 'D', 'E', 'F', 'G'] {
            let windows_path = format!("{}:\\Windows\\System32\\drivers", letter);
            if Path::new(&windows_path).exists() {
                println!("[DRIVER EXPORT] 尝试从 {}: 导出驱动", letter);
                let source = format!("{}:\\", letter);
                match dism.export_drivers_from_system(&source, destination) {
                    Ok(_) => {
                        println!("[DRIVER EXPORT] 成功从 {}: 导出驱动", letter);
                        return Ok(());
                    }
                    Err(e) => {
                        println!("[DRIVER EXPORT] 从 {}: 导出失败: {}", letter, e);
                    }
                }
            }
        }
        
        anyhow::bail!("PE 环境下未找到可用的 Windows 系统来导出驱动")
    } else {
        println!("[DRIVER EXPORT] 桌面环境，使用在线模式导出");
        dism.export_drivers(destination)
    }
}

/// 导入驱动到目标系统
fn import_drivers(target_partition: &str, driver_path: &str) -> anyhow::Result<()> {
    println!("[DRIVER IMPORT] 目标分区: {}, 驱动路径: {}", target_partition, driver_path);
    
    let dism = crate::core::dism::Dism::new();
    let image_path = format!("{}\\", target_partition);
    
    dism.add_drivers_offline(&image_path, driver_path)
}

/// 递归复制目录
fn copy_dir_recursive(src: &str, dst: &str) -> anyhow::Result<()> {
    use std::fs;
    use std::path::Path;
    
    let src_path = Path::new(src);
    let dst_path = Path::new(dst);
    
    if !src_path.exists() {
        anyhow::bail!("源目录不存在: {}", src);
    }
    
    // 创建目标目录
    fs::create_dir_all(dst_path)?;
    
    // 遍历源目录
    for entry in fs::read_dir(src_path)? {
        let entry = entry?;
        let src_file = entry.path();
        let dst_file = dst_path.join(entry.file_name());
        
        if src_file.is_dir() {
            // 递归复制子目录
            copy_dir_recursive(
                &src_file.to_string_lossy(),
                &dst_file.to_string_lossy(),
            )?;
        } else {
            // 复制文件
            fs::copy(&src_file, &dst_file)?;
        }
    }
    
    Ok(())
}

/// 生成无人值守 XML 文件
fn generate_unattend_xml(target_partition: &str, options: &AdvancedOptions) -> anyhow::Result<()> {
    use crate::core::system_utils::{get_file_version, get_system_architecture};
    
    println!("[UNATTEND] 生成无人值守配置文件");
    
    let username = if options.custom_username && !options.username.is_empty() {
        options.username.clone()
    } else {
        "User".to_string()
    };

    // 检测目标系统架构
    let arch = get_system_architecture(target_partition);
    let arch_str = arch.as_unattend_str();
    println!("[UNATTEND] 检测到目标系统架构: {}", arch_str);

    // 通过 ntdll.dll 文件版本检测目标系统版本
    // Windows 7: 6.1.x, Windows 8: 6.2.x, Windows 8.1: 6.3.x, Windows 10/11: 10.0.x
    let ntdll_path = Path::new(target_partition).join("Windows").join("System32").join("ntdll.dll");
    let (is_win7, is_win8) = match get_file_version(&ntdll_path) {
        Some((major, minor, build, _)) => {
            println!("[UNATTEND] 检测到目标系统版本 (ntdll.dll): {}.{}.{}", major, minor, build);
            
            let is_win7 = major == 6 && minor == 1;
            let is_win8 = major == 6 && (minor == 2 || minor == 3);
            (is_win7, is_win8)
        }
        None => {
            println!("[UNATTEND] 无法读取 ntdll.dll 版本: {:?}, 默认使用 Win10/11 配置", ntdll_path);
            (false, false)
        }
    };

    // 构建 FirstLogonCommands
    let mut first_logon_commands = String::new();
    let mut order = 1;

    // 首次登录脚本
    first_logon_commands.push_str(&format!(r#"
                <SynchronousCommand wcm:action="add">
                    <Order>{}</Order>
                    <CommandLine>cmd /c if exist %SystemDrive%\LetRecovery_Scripts\firstlogon.bat call %SystemDrive%\LetRecovery_Scripts\firstlogon.bat</CommandLine>
                    <Description>Run first login script</Description>
                </SynchronousCommand>"#, order));
    order += 1;

    // 如果需要删除UWP应用（仅Win10/11支持）
    if options.remove_uwp_apps && !is_win7 && !is_win8 {
        first_logon_commands.push_str(&format!(r#"
                <SynchronousCommand wcm:action="add">
                    <Order>{}</Order>
                    <CommandLine>powershell -ExecutionPolicy Bypass -File %SystemDrive%\LetRecovery_Scripts\remove_uwp.ps1</CommandLine>
                    <Description>Remove preinstalled UWP apps</Description>
                </SynchronousCommand>"#, order));
        order += 1;
    }

    // 清理脚本目录（最后执行）
    first_logon_commands.push_str(&format!(r#"
                <SynchronousCommand wcm:action="add">
                    <Order>{}</Order>
                    <CommandLine>cmd /c rd /s /q %SystemDrive%\LetRecovery_Scripts</CommandLine>
                    <Description>Cleanup scripts directory</Description>
                </SynchronousCommand>"#, order));
    
    // 根据系统版本生成不同的OOBE配置
    // Win7: 移除HideOEMRegistrationScreen（家庭版不支持）
    let oobe_section = if is_win7 {
        // Windows 7: 不支持 HideOnlineAccountScreens, HideWirelessSetupInOOBE, SkipMachineOOBE, SkipUserOOBE, HideLocalAccountScreen, HideOEMRegistrationScreen(家庭版)
        r#"<OOBE>
                <HideEULAPage>true</HideEULAPage>
                <ProtectYourPC>3</ProtectYourPC>
                <NetworkLocation>Home</NetworkLocation>
            </OOBE>"#.to_string()
    } else if is_win8 {
        // Windows 8/8.1: 支持 HideLocalAccountScreen，不支持其他新选项
        r#"<OOBE>
                <HideEULAPage>true</HideEULAPage>
                <HideLocalAccountScreen>true</HideLocalAccountScreen>
                <ProtectYourPC>3</ProtectYourPC>
                <NetworkLocation>Home</NetworkLocation>
            </OOBE>"#.to_string()
    } else {
        // Windows 10/11: 完整支持所有OOBE选项
        r#"<OOBE>
                <HideEULAPage>true</HideEULAPage>
                <HideLocalAccountScreen>true</HideLocalAccountScreen>
                <HideOnlineAccountScreens>true</HideOnlineAccountScreens>
                <HideWirelessSetupInOOBE>true</HideWirelessSetupInOOBE>
                <ProtectYourPC>3</ProtectYourPC>
                <SkipMachineOOBE>true</SkipMachineOOBE>
                <SkipUserOOBE>true</SkipUserOOBE>
            </OOBE>"#.to_string()
    };
    
    let xml_content = format!(r#"<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
    <settings pass="windowsPE">
        <component name="Microsoft-Windows-Setup" processorArchitecture="{arch}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
            <UserData>
                <ProductKey>
                    <WillShowUI>OnError</WillShowUI>
                </ProductKey>
                <AcceptEula>true</AcceptEula>
            </UserData>
        </component>
    </settings>
    <settings pass="specialize">
        <component name="Microsoft-Windows-Shell-Setup" processorArchitecture="{arch}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
            <ComputerName>*</ComputerName>
        </component>
        <component name="Microsoft-Windows-Deployment" processorArchitecture="{arch}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
            <RunSynchronous>
                <RunSynchronousCommand wcm:action="add">
                    <Order>1</Order>
                    <Path>cmd /c if exist %SystemDrive%\LetRecovery_Scripts\deploy.bat call %SystemDrive%\LetRecovery_Scripts\deploy.bat</Path>
                    <Description>Run custom deploy script</Description>
                </RunSynchronousCommand>
            </RunSynchronous>
        </component>
    </settings>
    <settings pass="oobeSystem">
        <component name="Microsoft-Windows-Shell-Setup" processorArchitecture="{arch}" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
            {oobe_section}
            <UserAccounts>
                <LocalAccounts>
                    <LocalAccount wcm:action="add">
                        <Password>
                            <Value></Value>
                            <PlainText>true</PlainText>
                        </Password>
                        <Description>Local User</Description>
                        <DisplayName>{username}</DisplayName>
                        <Group>Administrators</Group>
                        <n>{username}</n>
                    </LocalAccount>
                </LocalAccounts>
            </UserAccounts>
            <AutoLogon>
                <Password>
                    <Value></Value>
                    <PlainText>true</PlainText>
                </Password>
                <Enabled>true</Enabled>
                <LogonCount>1</LogonCount>
                <Username>{username}</Username>
            </AutoLogon>
            <FirstLogonCommands>{first_logon_commands}
            </FirstLogonCommands>
        </component>
    </settings>
</unattend>"#, arch = arch_str, oobe_section = oobe_section, username = username, first_logon_commands = first_logon_commands);

    let panther_dir = format!("{}\\Windows\\Panther", target_partition);
    std::fs::create_dir_all(&panther_dir)?;
    
    let unattend_path = format!("{}\\unattend.xml", panther_dir);
    std::fs::write(&unattend_path, &xml_content)?;
    println!("[UNATTEND] 已写入: {}", unattend_path);
    
    let sysprep_dir = format!("{}\\Windows\\System32\\Sysprep", target_partition);
    if Path::new(&sysprep_dir).exists() {
        let sysprep_unattend = format!("{}\\unattend.xml", sysprep_dir);
        let _ = std::fs::write(&sysprep_unattend, &xml_content);
        println!("[UNATTEND] 已写入: {}", sysprep_unattend);
    }
    
    Ok(())
}


/// 查找可用的数据分区（非系统分区）
/// 返回 (分区盘符, 是否自动创建)
fn find_data_partition(exclude_partition: &str, image_path: &str) -> Result<(String, bool), String> {
    use crate::core::disk::DiskManager;
    
    // 获取镜像文件大小
    let image_size = match std::fs::metadata(image_path) {
        Ok(meta) => meta.len(),
        Err(e) => {
            return Err(format!("无法获取镜像文件大小: {}", e));
        }
    };
    
    println!("[DATA PARTITION] 镜像文件大小: {} bytes ({:.2} GB)", 
        image_size, 
        image_size as f64 / 1024.0 / 1024.0 / 1024.0
    );

    // 调用 DiskManager 的新函数
    match DiskManager::find_suitable_data_partition(exclude_partition, image_size) {
        Ok(Some((partition, is_auto_created))) => {
            println!("[DATA PARTITION] 选择分区: {}, 自动创建: {}", partition, is_auto_created);
            Ok((partition, is_auto_created))
        }
        Ok(None) => {
            Err("没有找到可用的数据分区，且无法自动创建".to_string())
        }
        Err(e) => {
            Err(format!("{}", e))
        }
    }
}

/// 带进度回调的文件复制
fn copy_file_with_progress<F>(src: &str, dst: &str, mut progress_callback: F) -> anyhow::Result<()>
where
    F: FnMut(u8),
{
    use std::fs::File;
    use std::io::{BufReader, BufWriter, Read, Write};

    println!("[COPY] 开始复制: {} -> {}", src, dst);

    let src_file = File::open(src)?;
    let total_size = src_file.metadata()?.len();
    
    if total_size == 0 {
        // 空文件直接创建
        File::create(dst)?;
        progress_callback(100);
        return Ok(());
    }

    let mut reader = BufReader::with_capacity(1024 * 1024, src_file); // 1MB buffer
    let dst_file = File::create(dst)?;
    let mut writer = BufWriter::with_capacity(1024 * 1024, dst_file);

    let mut copied: u64 = 0;
    let mut buffer = vec![0u8; 1024 * 1024]; // 1MB chunks
    let mut last_progress: u8 = 0;

    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }

        writer.write_all(&buffer[..bytes_read])?;
        copied += bytes_read as u64;

        let progress = ((copied as f64 / total_size as f64) * 100.0) as u8;
        
        // 只在进度变化时回调，避免过多调用
        if progress != last_progress {
            progress_callback(progress);
            last_progress = progress;
            println!("[COPY] 进度: {}% ({}/{})", progress, copied, total_size);
        }
    }

    writer.flush()?;
    progress_callback(100);
    println!("[COPY] 复制完成: {}", dst);
    
    Ok(())
}
