use egui;
use std::sync::mpsc;

use crate::app::{App, BootModeSelection, InstallMode};
use crate::core::disk::{Partition, PartitionStyle};
use crate::core::dism::ImageInfo;

/// ISO 挂载结果
pub enum IsoMountResult {
    Success(String),
    Error(String),
}

/// 镜像信息加载结果
pub enum ImageInfoResult {
    Success(Vec<ImageInfo>),
    Error(String),
}

impl App {
    pub fn show_system_install(&mut self, ui: &mut egui::Ui) {
        ui.heading("系统安装");
        ui.separator();

        let is_pe = self.is_pe_environment();
        
        // 显示小白模式提示（非PE环境下，且未关闭提示）
        if !is_pe && !self.app_config.easy_mode_tip_dismissed {
            ui.horizontal(|ui| {
                ui.colored_label(
                    egui::Color32::from_rgb(100, 181, 246),
                    "💡 新手用户？可以在\"关于\"页面中开启小白模式，获得更简单的操作体验",
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("×").clicked() {
                        self.app_config.dismiss_easy_mode_tip();
                    }
                });
            });
            ui.add_space(10.0);
        }
        
        // 判断是否需要通过PE安装
        let needs_pe = self.check_if_needs_pe_for_install();
        
        // 检查PE配置是否可用（仅在需要PE时检查）
        let pe_available = self.is_pe_config_available();
        
        // 在非PE环境且目标是系统分区时，需要显示PE选择
        let show_pe_selector = !is_pe && needs_pe;
        
        // 安装按钮是否可用
        let install_blocked = show_pe_selector && !pe_available;

        // 检查ISO挂载状态
        self.check_iso_mount_status();

        // 镜像文件选择
        ui.horizontal(|ui| {
            ui.label("系统镜像:");
            
            let text_edit = egui::TextEdit::singleline(&mut self.local_image_path)
                .desired_width(400.0);
            ui.add_enabled(!self.iso_mounting, text_edit);
            
            if ui.add_enabled(!self.iso_mounting, egui::Button::new("浏览...")).clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("系统镜像", &["wim", "esd", "swm", "iso", "gho"])
                    .pick_file()
                {
                    self.local_image_path = path.to_string_lossy().to_string();
                    self.iso_mount_error = None;
                    self.load_image_volumes();
                }
            }
        });

        // 显示ISO挂载状态
        if self.iso_mounting {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("正在挂载 ISO 镜像，请稍候...");
            });
        }

        // 显示镜像信息加载状态
        if self.image_info_loading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("正在加载镜像信息，请稍候...");
            });
        }

        // 显示ISO挂载错误
        if let Some(ref error) = self.iso_mount_error {
            ui.colored_label(egui::Color32::RED, format!("ISO 挂载失败: {}", error));
        }

        // 镜像分卷选择（过滤掉 WindowsPE 等非系统镜像）
        if !self.image_volumes.is_empty() {
            // 过滤出可安装的系统镜像
            let installable_volumes: Vec<(usize, &ImageInfo)> = self.image_volumes
                .iter()
                .enumerate()
                .filter(|(_, vol)| Self::is_installable_image(vol))
                .collect();
            
            // 如果过滤后没有可安装的版本，使用原始列表并选择最后一项
            let (volumes_to_show, use_original): (Vec<(usize, &ImageInfo)>, bool) = if installable_volumes.is_empty() {
                // 过滤后无结果，显示原始列表
                let original_volumes: Vec<(usize, &ImageInfo)> = self.image_volumes
                    .iter()
                    .enumerate()
                    .collect();
                (original_volumes, true)
            } else {
                (installable_volumes, false)
            };
            
            if volumes_to_show.is_empty() {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 165, 0),
                    "⚠ 该镜像中没有可用的系统版本",
                );
            } else {
                // 获取要选择的默认索引
                let default_index = if use_original {
                    // 使用原始列表时，默认选择最后一项
                    volumes_to_show.last().map(|(i, _)| *i)
                } else {
                    // 使用过滤列表时，默认选择第一项
                    volumes_to_show.first().map(|(i, _)| *i)
                };
                
                // 如果显示的是原始列表，显示提示
                if use_original {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 165, 0),
                        "⚠ 未检测到标准系统镜像，显示所有分卷",
                    );
                }
                
                ui.horizontal(|ui| {
                    ui.label("系统版本:");
                    egui::ComboBox::from_id_salt("volume_select")
                        .selected_text(
                            self.selected_volume
                                .and_then(|i| self.image_volumes.get(i))
                                .map(|v| v.name.as_str())
                                .unwrap_or("请选择版本"),
                        )
                        .show_ui(ui, |ui| {
                            for (i, vol) in &volumes_to_show {
                                ui.selectable_value(
                                    &mut self.selected_volume,
                                    Some(*i),
                                    format!("{} - {}", vol.index, vol.name),
                                );
                            }
                        });
                });
                
                // 如果当前没有选中有效项，或选中的不在显示列表中，自动选择默认项
                let current_valid = self.selected_volume
                    .map(|idx| volumes_to_show.iter().any(|(i, _)| *i == idx))
                    .unwrap_or(false);
                
                if !current_valid {
                    self.selected_volume = default_index;
                }
            }
        }
        
        // 选择 Win10/11 镜像后，自动默认勾选磁盘控制器驱动
        self.update_storage_controller_driver_default();

        ui.add_space(10.0);
        ui.separator();

        // 分区选择表格
        ui.label("选择安装分区:");

        let partitions_clone: Vec<Partition> = self.partitions.clone();
        let mut partition_clicked: Option<usize> = None;

        egui::ScrollArea::vertical()
            .max_height(200.0)
            .show(ui, |ui| {
                egui::Grid::new("partition_grid")
                    .striped(true)
                    .min_col_width(60.0)
                    .show(ui, |ui| {
                        ui.label("分区卷");
                        ui.label("总空间");
                        ui.label("可用空间");
                        ui.label("卷标");
                        ui.label("分区表");
                        ui.label("状态");
                        ui.end_row();

                        for (i, partition) in partitions_clone.iter().enumerate() {
                            let label = if is_pe {
                                if partition.has_windows {
                                    format!("{} (有系统)", partition.letter)
                                } else {
                                    partition.letter.clone()
                                }
                            } else {
                                if partition.is_system_partition {
                                    format!("{} (当前系统)", partition.letter)
                                } else if partition.has_windows {
                                    format!("{} (有系统)", partition.letter)
                                } else {
                                    partition.letter.clone()
                                }
                            };

                            if ui
                                .selectable_label(self.selected_partition == Some(i), &label)
                                .clicked()
                            {
                                partition_clicked = Some(i);
                            }

                            ui.label(Self::format_size(partition.total_size_mb));
                            ui.label(Self::format_size(partition.free_size_mb));
                            ui.label(&partition.label);
                            ui.label(format!("{}", partition.partition_style));
                            
                            let status = if partition.has_windows {
                                "已有系统"
                            } else {
                                "空闲"
                            };
                            ui.label(status);
                            
                            ui.end_row();
                        }
                    });
            });

        // 处理分区选择
        if let Some(i) = partition_clicked {
            self.selected_partition = Some(i);
            self.update_install_options_for_partition();
        }

        ui.add_space(10.0);
        ui.separator();

        // 安装选项
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.format_partition, "格式化分区");
            ui.checkbox(&mut self.repair_boot, "添加引导");
            ui.checkbox(&mut self.unattended_install, "无人值守");
            
            // 驱动操作下拉框
            ui.label("驱动:");
            egui::ComboBox::from_id_salt("driver_action_select")
                .selected_text(format!("{}", self.driver_action))
                .width(80.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.driver_action,
                        crate::app::DriverAction::None,
                        "无",
                    );
                    ui.selectable_value(
                        &mut self.driver_action,
                        crate::app::DriverAction::SaveOnly,
                        "仅保存",
                    );
                    ui.selectable_value(
                        &mut self.driver_action,
                        crate::app::DriverAction::AutoImport,
                        "自动导入",
                    );
                });
            
            ui.checkbox(&mut self.auto_reboot, "立即重启");
        });

        // 引导模式选择
        ui.horizontal(|ui| {
            ui.label("引导模式:");
            egui::ComboBox::from_id_salt("boot_mode_select")
                .selected_text(format!("{}", self.selected_boot_mode))
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.selected_boot_mode,
                        BootModeSelection::Auto,
                        "自动 (根据分区表)",
                    );
                    ui.selectable_value(
                        &mut self.selected_boot_mode,
                        BootModeSelection::UEFI,
                        "UEFI",
                    );
                    ui.selectable_value(
                        &mut self.selected_boot_mode,
                        BootModeSelection::Legacy,
                        "Legacy (BIOS)",
                    );
                });

            if let Some(idx) = self.selected_partition {
                if let Some(partition) = self.partitions.get(idx) {
                    let actual_mode = Self::get_actual_boot_mode(self.selected_boot_mode, partition.partition_style);
                    ui.label(format!("( 将使用: {} )", actual_mode));
                }
            }
        });

        // PE选择（仅在需要通过PE安装时显示）
        if show_pe_selector {
            ui.add_space(10.0);
            ui.separator();
            
            ui.horizontal(|ui| {
                ui.label("🔧 PE环境:");
                
                if pe_available {
                    if let Some(ref config) = self.config {
                        egui::ComboBox::from_id_salt("pe_select_install")
                            .selected_text(
                                self.selected_pe_for_install
                                    .and_then(|i| config.pe_list.get(i))
                                    .map(|p| p.display_name.as_str())
                                    .unwrap_or("请选择PE"),
                            )
                            .show_ui(ui, |ui| {
                                for (i, pe) in config.pe_list.iter().enumerate() {
                                    ui.selectable_value(
                                        &mut self.selected_pe_for_install,
                                        Some(i),
                                        &pe.display_name,
                                    );
                                }
                            });
                        
                        // 显示PE就绪状态
                        if let Some(idx) = self.selected_pe_for_install {
                            if let Some(pe) = config.pe_list.get(idx) {
                                let (exists, _) = crate::core::pe::PeManager::check_pe_exists(&pe.filename);
                                if exists {
                                    ui.colored_label(egui::Color32::GREEN, "✓ 已就绪");
                                } else {
                                    ui.colored_label(egui::Color32::from_rgb(255, 165, 0), "需下载");
                                }
                            }
                        }
                    }
                } else {
                    ui.colored_label(egui::Color32::RED, "未找到PE配置");
                }
            });
            
            ui.colored_label(
                egui::Color32::from_rgb(255, 165, 0),
                "⚠ 安装到当前系统分区需要先重启到PE环境",
            );
        }

        // PE配置缺失警告
        if install_blocked {
            ui.add_space(5.0);
            ui.colored_label(
                egui::Color32::RED,
                "❌ 无法获取PE配置，无法安装到当前系统分区。请检查网络连接后重试。",
            );
        }

        ui.horizontal(|ui| {
            if ui.button("高级选项...").clicked() {
                self.show_advanced_options = true;
            }
            if ui.button("刷新分区").clicked() {
                self.refresh_partitions();
            }
        });

        ui.add_space(20.0);

        // 开始安装按钮
        let can_install = self.selected_partition.is_some()
            && !self.local_image_path.is_empty()
            && (self.local_image_path.ends_with(".gho") || self.selected_volume.is_some())
            && !install_blocked
            && (!show_pe_selector || self.selected_pe_for_install.is_some());

        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    can_install && !self.is_installing,
                    egui::Button::new("开始安装").min_size(egui::vec2(120.0, 35.0)),
                )
                .clicked()
            {
                self.start_installation();
            }

            // 显示安装模式提示
            if can_install {
                if needs_pe && !is_pe {
                    ui.label("(将通过PE环境安装)");
                } else {
                    ui.label("(直接安装)");
                }
            }
        });

        // 警告：安装到有系统的分区
        if let Some(idx) = self.selected_partition {
            if let Some(partition) = self.partitions.get(idx) {
                if partition.has_windows && !self.format_partition {
                    ui.add_space(5.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 165, 0),
                        "⚠ 目标分区已有系统，建议勾选\"格式化分区\"",
                    );
                }
            }
        }
    }

    /// 检查是否需要通过PE安装
    fn check_if_needs_pe_for_install(&self) -> bool {
        // 如果已经在PE环境中，不需要再进PE
        if self.is_pe_environment() {
            return false;
        }
        
        // 检查目标分区是否是当前系统分区
        if let Some(idx) = self.selected_partition {
            if let Some(partition) = self.partitions.get(idx) {
                return partition.is_system_partition;
            }
        }
        
        false
    }

    /// 根据选择和分区表类型获取实际的引导模式
    fn get_actual_boot_mode(selection: BootModeSelection, partition_style: PartitionStyle) -> &'static str {
        match selection {
            BootModeSelection::UEFI => "UEFI",
            BootModeSelection::Legacy => "Legacy",
            BootModeSelection::Auto => {
                match partition_style {
                    PartitionStyle::GPT => "UEFI",
                    PartitionStyle::MBR => "Legacy",
                    PartitionStyle::Unknown => "UEFI",
                }
            }
        }
    }

    pub fn load_image_volumes(&mut self) {
        if self.local_image_path.to_lowercase().ends_with(".iso") {
            self.start_iso_mount();
            return;
        }

        // 其他格式直接后台加载
        self.start_image_info_loading(&self.local_image_path.clone());
    }

    fn start_image_info_loading(&mut self, image_path: &str) {
        let path_lower = image_path.to_lowercase();
        
        if path_lower.ends_with(".wim") || path_lower.ends_with(".esd") || path_lower.ends_with(".swm") {
            println!("[IMAGE INFO] 开始后台加载镜像信息: {}", image_path);
            
            self.image_info_loading = true;
            self.image_volumes.clear();
            self.selected_volume = None;

            let (tx, rx) = mpsc::channel::<ImageInfoResult>();
            
            unsafe {
                IMAGE_INFO_RESULT_RX = Some(rx);
            }

            let path = image_path.to_string();

            std::thread::spawn(move || {
                println!("[IMAGE INFO THREAD] 线程启动，加载: {}", path);
                
                let dism = crate::core::dism::Dism::new();
                match dism.get_image_info(&path) {
                    Ok(volumes) => {
                        println!("[IMAGE INFO THREAD] 成功加载 {} 个卷", volumes.len());
                        let _ = tx.send(ImageInfoResult::Success(volumes));
                    }
                    Err(e) => {
                        println!("[IMAGE INFO THREAD] 加载失败: {}", e);
                        let _ = tx.send(ImageInfoResult::Error(e.to_string()));
                    }
                }
            });
        } else if path_lower.ends_with(".gho") || path_lower.ends_with(".ghs") {
            // GHO 文件不需要加载卷信息
            self.image_volumes.clear();
            self.selected_volume = Some(0);
        }
    }

    fn start_iso_mount(&mut self) {
        println!("[ISO MOUNT] 开始后台挂载 ISO: {}", self.local_image_path);
        
        self.iso_mounting = true;
        self.iso_mount_error = None;

        let (tx, rx) = mpsc::channel::<IsoMountResult>();
        
        unsafe {
            ISO_MOUNT_RESULT_RX = Some(rx);
        }

        let iso_path = self.local_image_path.clone();

        std::thread::spawn(move || {
            println!("[ISO MOUNT THREAD] 线程启动，挂载: {}", iso_path);
            
            match crate::core::iso::IsoMounter::mount_iso(&iso_path) {
                Ok(drive) => {
                    println!("[ISO MOUNT THREAD] 挂载成功，盘符: {}，查找安装镜像...", drive);
                    // 使用刚挂载的盘符查找镜像，而不是遍历所有盘符
                    if let Some(image_path) = crate::core::iso::IsoMounter::find_install_image_in_drive(&drive) {
                        println!("[ISO MOUNT THREAD] 找到镜像: {}", image_path);
                        let _ = tx.send(IsoMountResult::Success(image_path));
                    } else {
                        println!("[ISO MOUNT THREAD] 未找到安装镜像");
                        let _ = tx.send(IsoMountResult::Error("ISO 中未找到 install.wim/esd".to_string()));
                    }
                }
                Err(e) => {
                    println!("[ISO MOUNT THREAD] 挂载失败: {}", e);
                    let _ = tx.send(IsoMountResult::Error(e.to_string()));
                }
            }
        });
    }

    pub fn check_iso_mount_status(&mut self) {
        // 检查 ISO 挂载状态
        if self.iso_mounting {
            unsafe {
                if let Some(ref rx) = ISO_MOUNT_RESULT_RX {
                    if let Ok(result) = rx.try_recv() {
                        self.iso_mounting = false;
                        ISO_MOUNT_RESULT_RX = None;

                        match result {
                            IsoMountResult::Success(image_path) => {
                                println!("[ISO MOUNT] 挂载完成，镜像路径: {}", image_path);
                                self.local_image_path = image_path.clone();
                                self.iso_mount_error = None;
                                // 开始后台加载镜像信息
                                self.start_image_info_loading(&image_path);
                            }
                            IsoMountResult::Error(error) => {
                                println!("[ISO MOUNT] 挂载失败: {}", error);
                                self.iso_mount_error = Some(error);
                            }
                        }
                    }
                }
            }
        }

        // 检查镜像信息加载状态
        if self.image_info_loading {
            unsafe {
                if let Some(ref rx) = IMAGE_INFO_RESULT_RX {
                    if let Ok(result) = rx.try_recv() {
                        self.image_info_loading = false;
                        IMAGE_INFO_RESULT_RX = None;

                        match result {
                            ImageInfoResult::Success(volumes) => {
                                println!("[IMAGE INFO] 加载完成，找到 {} 个卷", volumes.len());
                                self.image_volumes = volumes;
                                
                                // 检查是否需要小白模式自动安装
                                if self.easy_mode_pending_auto_start {
                                    log::info!("[EASY MODE] 镜像加载完成，准备自动安装");
                                    
                                    // 根据预设的 install_volume_index 找到对应的分卷索引
                                    let target_volume_index = self.install_volume_index;
                                    self.selected_volume = self.image_volumes
                                        .iter()
                                        .enumerate()
                                        .find(|(_, vol)| vol.index == target_volume_index)
                                        .map(|(i, _)| i);
                                    
                                    if self.selected_volume.is_some() {
                                        log::info!("[EASY MODE] 找到目标分卷 {}，开始安装", target_volume_index);
                                        
                                        // 重置标志
                                        self.easy_mode_pending_auto_start = false;
                                        
                                        // 开始安装
                                        self.start_installation();
                                    } else {
                                        log::error!("[EASY MODE] 未找到目标分卷 {}，自动安装失败", target_volume_index);
                                        self.easy_mode_pending_auto_start = false;
                                        self.show_error(&format!("未找到目标分卷 {}，请手动选择", target_volume_index));
                                    }
                                } else {
                                    // 普通模式：自动选择第一个可安装的系统镜像
                                    self.selected_volume = self.image_volumes
                                        .iter()
                                        .enumerate()
                                        .find(|(_, vol)| Self::is_installable_image(vol))
                                        .map(|(i, _)| i);
                                    
                                    if self.selected_volume.is_none() && !self.image_volumes.is_empty() {
                                        // 如果没有可用的系统版本，仍然设为 None
                                        log::warn!("镜像中没有可安装的系统版本（全部为 PE 环境或安装媒体）");
                                    }
                                }
                            }
                            ImageInfoResult::Error(error) => {
                                println!("[IMAGE INFO] 加载失败: {}", error);
                                self.image_volumes.clear();
                                self.selected_volume = None;
                                // 保存错误信息供UI显示
                                self.iso_mount_error = Some(format!("镜像信息加载失败: {}", error));
                            }
                        }
                    }
                }
            }
        }
    }

    /// 判断镜像是否为可安装的系统镜像
    /// 排除以下类型：
    /// 1. installation_type 为 "WindowsPE" 的镜像
    /// 2. 名称包含 "Windows PE" 或 "Windows Setup" 的镜像（PE环境/安装程序）
    /// 3. 名称为 "Windows Setup Media" 的镜像（安装媒体元数据）
    fn is_installable_image(vol: &ImageInfo) -> bool {
        let name_lower = vol.name.to_lowercase();
        let install_type_lower = vol.installation_type.to_lowercase();
        
        // 1. 排除 installation_type 为 WindowsPE 的
        if install_type_lower == "windowspe" {
            return false;
        }
        
        // 2. 排除名称包含特定关键词的（PE环境、安装程序、安装媒体）
        let excluded_keywords = [
            "windows pe",
            "windows setup",
            "setup media",
            "winpe",
        ];
        
        for keyword in &excluded_keywords {
            if name_lower.contains(keyword) {
                return false;
            }
        }
        
        // 3. 如果 installation_type 为空，进行额外检查
        if vol.installation_type.is_empty() {
            // 名称必须包含系统版本标识（Windows 10/11/Server 等）
            let is_valid_system = name_lower.contains("windows 10") 
                || name_lower.contains("windows 11")
                || name_lower.contains("windows server")
                || name_lower.contains("windows 8")
                || name_lower.contains("windows 7");
            
            if !is_valid_system {
                return false;
            }
        }
        
        // 4. 如果 installation_type 明确是 Client 或 Server，直接通过
        if install_type_lower == "client" || install_type_lower == "server" {
            return true;
        }
        
        // 5. 其他情况（installation_type 为空但名称包含有效系统标识），通过
        true
    }

    fn update_storage_controller_driver_default(&mut self) {
        let mut target_id: Option<String> = None;
        let mut is_win10_or_11: bool = false;

        if let Some(idx) = self.selected_volume {
            if let Some(vol) = self.image_volumes.get(idx) {
                target_id = Some(format!(
                    "{}::{}::{}",
                    self.local_image_path, vol.index, vol.name
                ));
                // 直接使用 wimgapi 解析出的版本号
                // major_version >= 10 表示 Windows 10 或更高版本
                is_win10_or_11 = vol.major_version.map(|v| v >= 10).unwrap_or(false);
            }
        }

        // 只有当选择的镜像变化时才更新设置
        if target_id != self.storage_driver_default_target {
            self.storage_driver_default_target = target_id;
            self.advanced_options.import_storage_controller_drivers = is_win10_or_11;
            
            // 只在变化时打印日志
            if let Some(idx) = self.selected_volume {
                if let Some(vol) = self.image_volumes.get(idx) {
                    if let Some(v) = vol.major_version {
                        println!(
                            "[STORAGE DRIVER] 镜像版本: major_version={}, is_win10_or_11={}",
                            v, is_win10_or_11
                        );
                    } else {
                        println!("[STORAGE DRIVER] 未检测到版本信息，不自动勾选磁盘控制器驱动");
                    }
                }
            }
        }
    }

    pub fn update_install_options_for_partition(&mut self) {
        if let Some(idx) = self.selected_partition {
            if let Some(partition) = self.partitions.get(idx) {
                if partition.has_windows || partition.is_system_partition {
                    self.format_partition = true;
                    self.repair_boot = true;
                }
            }
        }
    }

    pub fn format_size(size_mb: u64) -> String {
        if size_mb >= 1024 {
            format!("{:.1} GB", size_mb as f64 / 1024.0)
        } else {
            format!("{} MB", size_mb)
        }
    }

    pub fn refresh_partitions(&mut self) {
        if let Ok(partitions) = crate::core::disk::DiskManager::get_partitions() {
            self.partitions = partitions;
            
            // 判断是否为PE环境
            let is_pe = self.system_info.as_ref().map(|s| s.is_pe_environment).unwrap_or(false);
            
            if is_pe {
                // PE环境下，统计有系统的分区
                let windows_partitions: Vec<usize> = self.partitions
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.has_windows)
                    .map(|(i, _)| i)
                    .collect();
                
                if windows_partitions.len() == 1 {
                    // 只有一个系统分区，默认选择它
                    self.selected_partition = Some(windows_partitions[0]);
                } else {
                    // 有多个或没有系统分区，不默认选择
                    self.selected_partition = None;
                }
            } else {
                // 非PE环境，选择当前系统分区
                self.selected_partition = self
                    .partitions
                    .iter()
                    .position(|p| p.is_system_partition);
            }
        }
    }

    pub fn start_installation(&mut self) {
        let partition = self
            .partitions
            .get(self.selected_partition.unwrap())
            .cloned();
        if partition.is_none() {
            return;
        }
        let partition = partition.unwrap();

        let image_path = self.local_image_path.clone();
        let volume_index = self
            .selected_volume
            .and_then(|i| self.image_volumes.get(i).map(|v| v.index))
            .unwrap_or(1);

        let is_system_partition = partition.is_system_partition;
        let is_pe = self.is_pe_environment();

        // 确定安装模式
        self.install_mode = if is_pe || !is_system_partition {
            InstallMode::Direct
        } else {
            InstallMode::ViaPE
        };

        // 如果需要通过PE安装，先检查PE是否存在
        if self.install_mode == InstallMode::ViaPE {
            let pe_info = self.selected_pe_for_install.and_then(|idx| {
                self.config.as_ref().and_then(|c| c.pe_list.get(idx).cloned())
            });
            
            if let Some(pe) = pe_info {
                let (pe_exists, _) = crate::core::pe::PeManager::check_pe_exists(&pe.filename);
                if !pe_exists {
                    // PE不存在，先下载PE
                    println!("[INSTALL] PE文件不存在，开始下载: {}", pe.filename);
                    self.pending_download_url = Some(pe.download_url.clone());
                    self.pending_download_filename = Some(pe.filename.clone());
                    self.pending_pe_md5 = pe.md5.clone();  // 设置MD5校验值
                    let pe_dir = crate::utils::path::get_exe_dir()
                        .join("PE")
                        .to_string_lossy()
                        .to_string();
                    self.download_save_path = pe_dir;
                    self.pe_download_then_action = Some(crate::app::PeDownloadThenAction::Install);
                    self.current_panel = crate::app::Panel::DownloadProgress;
                    return;
                }
            }
        }

        self.install_options = crate::app::InstallOptions {
            format_partition: self.format_partition,
            repair_boot: self.repair_boot,
            unattended_install: self.unattended_install,
            // 根据driver_action设置export_drivers（仅SaveOnly和AutoImport需要导出）
            export_drivers: matches!(self.driver_action, crate::app::DriverAction::SaveOnly | crate::app::DriverAction::AutoImport),
            auto_reboot: self.auto_reboot,
            boot_mode: self.selected_boot_mode,
            advanced_options: self.advanced_options.clone(),
            driver_action: self.driver_action,
        };

        self.is_installing = true;
        self.current_panel = crate::app::Panel::InstallProgress;
        self.install_progress = crate::app::InstallProgress::default();
        
        // 重置自动重启标志
        self.auto_reboot_triggered = false;

        self.install_target_partition = partition.letter.clone();
        self.install_image_path = image_path;
        self.install_volume_index = volume_index;
        self.install_is_system_partition = is_system_partition;

        self.install_step = 0;
    }
}

static mut ISO_MOUNT_RESULT_RX: Option<mpsc::Receiver<IsoMountResult>> = None;
static mut IMAGE_INFO_RESULT_RX: Option<mpsc::Receiver<ImageInfoResult>> = None;
