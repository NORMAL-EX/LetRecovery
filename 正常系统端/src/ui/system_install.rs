use egui;
use std::sync::mpsc;

use crate::tr;
use crate::app::{App, BootModeSelection, UnattendCheckResult};
use crate::core::disk::{Partition, PartitionStyle};
use crate::core::dism::ImageInfo;

/// ISO 挂载结果
pub enum IsoMountResult {
    /// 找到 Vista+ 安装镜像（install.wim/esd/swm 的完整路径）
    Success(String),
    /// 识别为 XP/2003 的 i386 文本安装介质，携带挂载盘上的 i386 源目录（如 `F:\I386`）
    XpI386(String),
    Error(String),
}

/// 镜像信息加载结果
pub enum ImageInfoResult {
    Success(Vec<ImageInfo>),
    Error(String),
}

impl App {
    pub fn show_system_install(&mut self, ui: &mut egui::Ui) {
        ui.heading(tr!("系统安装"));
        ui.separator();

        // 整页套一层垂直滚动：窗口调小时也能滚动到底部的「开始安装」按钮等控件。
        egui::ScrollArea::vertical()
            .id_salt("system_install_page_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
        let is_pe = self.is_pe_environment();
        
        // 显示小白模式提示（非PE环境下，且未关闭提示）
        if !is_pe && !self.app_config.easy_mode_tip_dismissed {
            ui.horizontal(|ui| {
                ui.colored_label(
                    egui::Color32::from_rgb(100, 181, 246),
                    tr!("新手用户？可以在\"关于\"页面中开启小白模式，获得更简单的操作体验"),
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
            ui.label(tr!("系统镜像:"));

            let text_edit = egui::TextEdit::singleline(&mut self.local_image_path)
                .desired_width(400.0);
            ui.add_enabled(!self.iso_mounting, text_edit);

            if ui.add_enabled(!self.iso_mounting, egui::Button::new(tr!("浏览..."))).clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter(tr!("系统镜像"), &["wim", "esd", "swm", "iso", "gho"])
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
                ui.label(tr!("正在挂载 ISO 镜像，请稍候..."));
            });
        }

        // 显示镜像信息加载状态
        if self.image_info_loading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tr!("正在加载镜像信息，请稍候..."));
            });
        }

        // 显示ISO挂载错误
        if let Some(ref error) = self.iso_mount_error {
            ui.colored_label(egui::Color32::RED, tr!("ISO 挂载失败: {}", error));
        }

        // 识别为 XP/2003 i386 文本安装介质的提示（原版 XP ISO 无 install.wim，走文本安装）
        if self.xp_i386_source.is_some() {
            ui.colored_label(
                egui::Color32::from_rgb(0, 160, 0),
                tr!("已识别为 Windows XP/2003 i386 文本安装介质（仅支持 Legacy/MBR；重启后进入蓝底文本安装阶段）"),
            );
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
                    tr!("该镜像中没有可用的系统版本"),
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
                        tr!("未检测到标准系统镜像，显示所有分卷"),
                    );
                }
                
                ui.horizontal(|ui| {
                    ui.label(tr!("系统版本:"));
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
        ui.label(tr!("选择安装分区:"));

        let partitions_clone: Vec<Partition> = self.partitions.clone();
        let mut partition_clicked: Option<usize> = None;

        egui::ScrollArea::vertical()
            .max_height(200.0)
            .show(ui, |ui| {
                egui::Grid::new("partition_grid")
                    .striped(true)
                    .min_col_width(60.0)
                    .show(ui, |ui| {
                        ui.label(tr!("分区卷"));
                        ui.label(tr!("总空间"));
                        ui.label(tr!("可用空间"));
                        ui.label(tr!("卷标"));
                        ui.label(tr!("分区表"));
                        ui.label("BitLocker");
                        ui.label(tr!("状态"));
                        ui.end_row();

                        for (i, partition) in partitions_clone.iter().enumerate() {
                            let label = if is_pe {
                                if partition.has_windows {
                                    tr!("{} (有系统)", partition.letter)
                                } else {
                                    partition.letter.clone()
                                }
                            } else {
                                if partition.is_system_partition {
                                    tr!("{} (当前系统)", partition.letter)
                                } else if partition.has_windows {
                                    tr!("{} (有系统)", partition.letter)
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
                            
                            // 显示 BitLocker 状态
                            let status_color = match partition.bitlocker_status {
                                crate::core::bitlocker::VolumeStatus::EncryptedLocked => egui::Color32::RED,
                                crate::core::bitlocker::VolumeStatus::EncryptedUnlocked => egui::Color32::from_rgb(102, 187, 106),
                                crate::core::bitlocker::VolumeStatus::Encrypting | 
                                crate::core::bitlocker::VolumeStatus::Decrypting => egui::Color32::YELLOW,
                                _ => ui.visuals().text_color(),
                            };
                            ui.colored_label(status_color, tr!(partition.bitlocker_status.as_str()));

                            let status = if partition.has_windows {
                                tr!("已有系统")
                            } else {
                                tr!("空闲")
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
            // 触发无人值守检测
            self.start_unattend_check_for_partition(i);
        }
        
        // 检查无人值守检测状态
        self.check_unattend_status();

        ui.add_space(10.0);
        ui.separator();

        // 安装选项
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.format_partition, tr!("格式化分区"));
            ui.checkbox(&mut self.repair_boot, tr!("添加引导"));
            
            // 无人值守选项 - 根据检测结果处理
            // 如果勾选了格式化分区，则无人值守不受限制（因为格式化会清除现有配置）
            let unattend_disabled = self.partition_has_unattend && !self.format_partition;
            let unattend_tooltip = if self.partition_has_unattend && !self.format_partition {
                tr!("目标分区已存在无人值守配置文件，无法启用此选项以避免冲突。\n勾选「格式化分区」可解除此限制。")
            } else if self.partition_has_unattend && self.format_partition {
                tr!("格式化将清除现有配置文件，可以启用无人值守")
            } else {
                tr!("启用无人值守安装")
            };
            
            if unattend_disabled {
                // 显示禁用状态的复选框
                let response = ui.add_enabled(false, egui::Checkbox::new(&mut false, tr!("无人值守")))
                    .on_disabled_hover_text(unattend_tooltip);
                
                // 如果用户点击了禁用的复选框，显示提示对话框
                if response.clicked() {
                    self.show_unattend_conflict_modal = true;
                }
            } else {
                ui.checkbox(&mut self.unattended_install, tr!("无人值守"))
                    .on_hover_text(unattend_tooltip);
            }
            
            // 驱动操作下拉框
            ui.label(tr!("驱动:"));
            egui::ComboBox::from_id_salt("driver_action_select")
                .selected_text(format!("{}", self.driver_action))
                .width(80.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.driver_action,
                        crate::app::DriverAction::None,
                        tr!("无"),
                    );
                    ui.selectable_value(
                        &mut self.driver_action,
                        crate::app::DriverAction::SaveOnly,
                        tr!("仅保存"),
                    );
                    ui.selectable_value(
                        &mut self.driver_action,
                        crate::app::DriverAction::AutoImport,
                        tr!("自动导入"),
                    );
                });

            ui.checkbox(&mut self.auto_reboot, tr!("立即重启"));

            // 运行 Diskpart 脚本（仅在「高级选项」开启时显示）
            if self.app_config.enable_advanced_options {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.run_diskpart_scripts, tr!("运行Diskpart脚本"))
                        .on_hover_text(
                            tr!("安装前运行 程序目录\\bin\\diskpart\\ 下的所有脚本(.cmd/.bat 走 cmd，.txt 走 diskpart)，\n在 PE 中、格式化/释放镜像之前执行，可用于自定义分区。"),
                        );
                    // 打开 diskpart 脚本目录（不存在则先创建，避免 explorer 打开失败）
                    if ui
                        .button(tr!("打开目录"))
                        .on_hover_text(tr!("打开程序目录 bin\\diskpart 脚本文件夹"))
                        .clicked()
                    {
                        let dir = crate::utils::path::get_bin_dir().join("diskpart");
                        let _ = std::fs::create_dir_all(&dir);
                        #[cfg(windows)]
                        {
                            let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                        }
                    }
                    // 用记事本编辑自定义引导修复命令（bin\repair_boot.txt）
                    if ui
                        .button(tr!("修改引导命令"))
                        .on_hover_text(tr!("用记事本编辑自定义引导修复脚本 bin\\repair_boot.txt"))
                        .clicked()
                    {
                        let file = crate::utils::path::get_bin_dir().join("repair_boot.txt");
                        if let Some(parent) = file.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        #[cfg(windows)]
                        {
                            let _ = std::process::Command::new("notepad").arg(&file).spawn();
                        }
                    }
                });
            }
        });

        // 自定义无人值守文件 + 引导模式（启用无人值守时两者并列；否则引导模式单独一行）
        if self.unattended_install {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(tr!("自定义无人值守:"));
                if ui.button(tr!("选择文件…")).clicked() {
                    // XP/2003 i386 介质的应答文件是 winnt.sif(INI),不是 Vista+ 的 unattend.xml。
                    // 据所选介质切换筛选器与校验方式,否则 *.xml 筛选会把 .sif 全过滤掉(列表空)。
                    let is_xp = self.xp_i386_source.is_some();
                    let mut dlg = rfd::FileDialog::new();
                    if is_xp {
                        dlg = dlg
                            .add_filter(tr!("XP 应答文件 winnt.sif"), &["sif"])
                            .add_filter(tr!("所有文件"), &["*"]);
                    } else {
                        dlg = dlg
                            .add_filter(tr!("无人值守文件"), &["xml"])
                            .add_filter(tr!("所有文件"), &["*"]);
                    }
                    if let Some(path) = dlg.pick_file() {
                        let p = path.to_string_lossy().to_string();
                        match std::fs::read_to_string(&path) {
                            Ok(content) => {
                                self.custom_unattend_path = p.clone();
                                // 据【文件本身】判类型分发校验：扩展名 .sif，或内容是 INI 风格（去 BOM 后以
                                // `[节]` 开头）→ winnt.sif(INI) 校验；否则按 unattend.xml 校验。
                                // 不依赖介质检测状态——否则「先选应答文件、后认 XP 介质」会把 .sif 错当
                                // XML 校验（报 unknown token at 1:1），且切介质后不会重新校验、错误赖着不走。
                                let body = content.trim_start_matches('\u{feff}');
                                let is_sif = p.to_ascii_lowercase().ends_with(".sif")
                                    || body.trim_start().starts_with('[');
                                self.custom_unattend_error = if is_sif {
                                    crate::core::install_config::validate_winnt_sif(&content).err()
                                } else {
                                    crate::core::install_config::validate_unattend_xml(&content)
                                        .err()
                                };
                            }
                            Err(e) => {
                                self.custom_unattend_path = p;
                                self.custom_unattend_error = Some(tr!("无法读取文件: {}", e));
                            }
                        }
                    }
                }
                if !self.custom_unattend_path.is_empty()
                    && ui.button(tr!("清除")).clicked()
                {
                    self.custom_unattend_path.clear();
                    self.custom_unattend_error = None;
                }

                // 引导模式与“自定义无人值守”并列在同一行
                ui.separator();
                ui.label(tr!("引导模式:"));
                egui::ComboBox::from_id_salt("boot_mode_select")
                    .selected_text(format!("{}", self.selected_boot_mode))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.selected_boot_mode,
                            BootModeSelection::Auto,
                            tr!("自动 (根据分区表)"),
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
                        let actual_mode = Self::get_actual_boot_mode(
                            self.selected_boot_mode,
                            partition.partition_style,
                        );
                        ui.label(tr!("( 将使用: {} )", actual_mode));
                    }
                }
            });

            if self.custom_unattend_path.is_empty() {
                ui.label(
                    egui::RichText::new(tr!("未选择则使用内置生成的无人值守配置")).weak(),
                );
            } else {
                ui.horizontal(|ui| {
                    ui.label(tr!("已选:"));
                    ui.monospace(self.custom_unattend_path.clone());
                });
                match &self.custom_unattend_error {
                    Some(err) => {
                        ui.colored_label(
                            egui::Color32::from_rgb(220, 50, 47),
                            tr!("无人值守文件语法错误：{}（已禁用安装）", err),
                        );
                    }
                    None => {
                        ui.colored_label(
                            egui::Color32::from_rgb(0, 160, 0),
                            tr!("无人值守文件语法校验通过"),
                        );
                    }
                }
            }
            ui.add_space(6.0);
        } else {
            // 未启用无人值守：引导模式单独一行
            ui.horizontal(|ui| {
                ui.label(tr!("引导模式:"));
                egui::ComboBox::from_id_salt("boot_mode_select")
                    .selected_text(format!("{}", self.selected_boot_mode))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.selected_boot_mode,
                            BootModeSelection::Auto,
                            tr!("自动 (根据分区表)"),
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
                        let actual_mode = Self::get_actual_boot_mode(
                            self.selected_boot_mode,
                            partition.partition_style,
                        );
                        ui.label(tr!("( 将使用: {} )", actual_mode));
                    }
                }
            });
        }

        // PE选择（仅在需要通过PE安装时显示）
        if show_pe_selector {
            ui.add_space(10.0);
            ui.separator();

            if pe_available {
                let pe_count = self.config.as_ref().map(|c| c.pe_list.len()).unwrap_or(0);
                // 只有一个 PE 环境时自动选中
                if pe_count == 1 && self.selected_pe_for_install.is_none() {
                    self.selected_pe_for_install = Some(0);
                }
                // 仅在存在多个 PE 时才显示选择行；只有一个 PE 时隐藏，
                // 只保留下方“重启到 PE 环境”的提示即可。
                if pe_count > 1 {
                    if let Some(ref config) = self.config {
                        ui.horizontal(|ui| {
                            ui.label(tr!("PE环境:"));
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
                        });
                    }
                }
            } else {
                ui.colored_label(egui::Color32::RED, tr!("未找到PE配置"));
            }

            ui.colored_label(
                egui::Color32::from_rgb(255, 165, 0),
                tr!("安装到当前系统分区需要先重启到PE环境"),
            );
        }

        // PE配置缺失警告
        if install_blocked {
            ui.add_space(5.0);
            ui.colored_label(
                egui::Color32::RED,
                tr!("无法获取PE配置，无法安装到当前系统分区。请检查网络连接后重试。"),
            );
        }

        ui.horizontal(|ui| {
            if ui.button(tr!("高级选项...")).clicked() {
                self.show_advanced_options = true;
                // 每次打开重新检测 WiFi，决定是否显示“迁移当前 WiFi”选项
                self.advanced_options.wifi_detected = None;
            }
            if ui.button(tr!("刷新分区")).clicked() {
                self.refresh_partitions();
            }
        });

        ui.add_space(20.0);

        ui.add_space(10.0);

        // 开始安装按钮
        let can_install = self.selected_partition.is_some()
            && !self.local_image_path.is_empty()
            && (self.local_image_path.ends_with(".gho") || self.selected_volume.is_some())
            && !install_blocked
            && (!show_pe_selector || self.selected_pe_for_install.is_some())
            // 选择了自定义无人值守但语法有误 -> 禁用安装
            && self.custom_unattend_error.is_none();

        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    can_install && !self.is_installing,
                    egui::Button::new(tr!("开始安装")).min_size(egui::vec2(120.0, 35.0)),
                )
                .clicked()
            {
                self.start_installation();
            }

            // 显示安装模式提示
            if can_install {
                if needs_pe && !is_pe {
                    ui.label(tr!("(将通过PE环境安装)"));
                } else {
                    ui.label(tr!("(直接安装)"));
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
                        tr!("目标分区已有系统，建议勾选\"格式化分区\""),
                    );
                }
            }
        }
            }); // end ScrollArea
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
        // 切换镜像时先清掉上一次的 XP i386 识别状态（重新识别）
        self.xp_i386_source = None;

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
            log::info!("[IMAGE INFO] 开始后台加载镜像信息: {}", image_path);
            
            self.image_info_loading = true;
            self.image_volumes.clear();
            self.selected_volume = None;

            let (tx, rx) = mpsc::channel::<ImageInfoResult>();

            *IMAGE_INFO_RESULT_RX.lock().unwrap() = Some(rx);

            let path = image_path.to_string();

            std::thread::spawn(move || {
                log::info!("[IMAGE INFO THREAD] 线程启动，加载: {}", path);
                
                let dism = crate::core::dism::Dism::new();
                match dism.get_image_info(&path) {
                    Ok(volumes) => {
                        log::info!("[IMAGE INFO THREAD] 成功加载 {} 个卷", volumes.len());
                        let _ = tx.send(ImageInfoResult::Success(volumes));
                    }
                    Err(e) => {
                        log::error!("[IMAGE INFO THREAD] 加载失败: {}", e);
                        let _ = tx.send(ImageInfoResult::Error(e.to_string()));
                    }
                }
            });
        } else if path_lower.ends_with(".gho") || path_lower.ends_with(".ghs") {
            // GHO 文件不需要加载卷信息
            self.image_volumes.clear();
            self.selected_volume = Some(0);
            // 重新评估源镜像自带应答（GHO 同目录基本不会有，主要用于切换镜像时复位）
            self.refresh_source_unattend();
        }
    }

    fn start_iso_mount(&mut self) {
        log::info!("[ISO MOUNT] 开始后台挂载 ISO: {}", self.local_image_path);
        
        self.iso_mounting = true;
        self.iso_mount_error = None;

        let (tx, rx) = mpsc::channel::<IsoMountResult>();

        *ISO_MOUNT_RESULT_RX.lock().unwrap() = Some(rx);

        let iso_path = self.local_image_path.clone();

        std::thread::spawn(move || {
            log::info!("[ISO MOUNT THREAD] 线程启动，挂载: {}", iso_path);
            
            match crate::core::iso::IsoMounter::mount_iso(&iso_path) {
                Ok(drive) => {
                    log::info!("[ISO MOUNT THREAD] 挂载成功，盘符: {}，查找安装镜像...", drive);
                    // 使用刚挂载的盘符查找镜像，而不是遍历所有盘符
                    if let Some(image_path) = crate::core::iso::IsoMounter::find_install_image_in_drive(&drive) {
                        log::info!("[ISO MOUNT THREAD] 找到镜像: {}", image_path);
                        let _ = tx.send(IsoMountResult::Success(image_path));
                    } else if let Some(i386_dir) = crate::core::iso::IsoMounter::xp_i386_dir(&drive) {
                        // 无 \sources\install.wim/esd，但有 \I386(\AMD64) 文本安装结构 → XP/2003 介质
                        log::info!("[ISO MOUNT THREAD] 识别为 XP/2003 i386 文本安装介质: {}", i386_dir);
                        let _ = tx.send(IsoMountResult::XpI386(i386_dir));
                    } else {
                        log::info!("[ISO MOUNT THREAD] 未找到安装镜像");
                        let _ = tx.send(IsoMountResult::Error(tr!("ISO 中未找到 install.wim/esd")));
                    }
                }
                Err(e) => {
                    log::error!("[ISO MOUNT THREAD] 挂载失败: {}", e);
                    let _ = tx.send(IsoMountResult::Error(e.to_string()));
                }
            }
        });
    }

    pub fn check_iso_mount_status(&mut self) {
        // 检查 ISO 挂载状态
        if self.iso_mounting {
            // 仅在轮询该 static 时短暂持锁：try_recv 成功即取出结果并清空 static，随后立刻释放
            // guard（作用域结束），再处理结果——避免在持锁期间调用 self 方法（如
            // start_image_info_loading 会再锁 IMAGE_INFO_RESULT_RX）导致重入/死锁。
            let received = {
                let mut guard = ISO_MOUNT_RESULT_RX.lock().unwrap();
                if let Some(rx) = guard.as_ref() {
                    match rx.try_recv() {
                        Ok(result) => {
                            *guard = None;
                            Some(result)
                        }
                        Err(_) => None,
                    }
                } else {
                    None
                }
            };
            if let Some(result) = received {
                self.iso_mounting = false;

                match result {
                    IsoMountResult::Success(image_path) => {
                        log::info!("[ISO MOUNT] 挂载完成，镜像路径: {}", image_path);
                        self.local_image_path = image_path.clone();
                        self.iso_mount_error = None;
                        self.xp_i386_source = None;
                        // 开始后台加载镜像信息
                        self.start_image_info_loading(&image_path);
                    }
                    IsoMountResult::XpI386(i386_dir) => {
                        // XP/2003 i386 文本安装介质：无 WIM 可枚举，参照 GHO 的处理方式——
                        // 不加载分卷信息，合成单一可安装项（selected_volume=Some(0)）。
                        log::info!("[ISO MOUNT] 识别为 XP/2003 i386 介质，i386 源: {}", i386_dir);
                        self.local_image_path = i386_dir.clone();
                        self.xp_i386_source = Some(i386_dir);
                        self.iso_mount_error = None;
                        self.image_volumes.clear();
                        self.selected_volume = Some(0);
                        // i386 源自带 winnt.sif 时默认取消勾选无人值守
                        self.refresh_source_unattend();
                    }
                    IsoMountResult::Error(error) => {
                        log::error!("[ISO MOUNT] 挂载失败: {}", error);
                        self.iso_mount_error = Some(error);
                    }
                }
            }
        }

        // 检查镜像信息加载状态
        if self.image_info_loading {
            // 同上：只在 try_recv 期间短暂持锁，取出结果后立刻释放 guard 再处理，
            // 因为下方分支会调用 self.refresh_source_unattend / self.start_installation 等方法。
            let received = {
                let mut guard = IMAGE_INFO_RESULT_RX.lock().unwrap();
                if let Some(rx) = guard.as_ref() {
                    match rx.try_recv() {
                        Ok(result) => {
                            *guard = None;
                            Some(result)
                        }
                        Err(_) => None,
                    }
                } else {
                    None
                }
            };
            if let Some(result) = received {
                self.image_info_loading = false;

                match result {
                    ImageInfoResult::Success(volumes) => {
                        log::info!("[IMAGE INFO] 加载完成，找到 {} 个卷", volumes.len());
                        self.image_volumes = volumes;
                        // 介质根/镜像目录自带应答文件时默认取消勾选无人值守
                        self.refresh_source_unattend();

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
                                self.show_error(&tr!("未找到目标分卷 {}，请手动选择", target_volume_index));
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
                        log::error!("[IMAGE INFO] 加载失败: {}", error);
                        self.image_volumes.clear();
                        self.selected_volume = None;
                        // 保存错误信息供UI显示
                        self.iso_mount_error = Some(tr!("镜像信息加载失败: {}", error));
                    }
                }
            }
        }
    }

    /// 判断镜像是否为可安装的系统镜像
    /// 
    /// 使用新的 image_type 字段进行快速判断，同时保留传统的关键词检测作为后备
    /// 
    /// 可安装的类型：
    /// - StandardInstall: 标准Windows安装镜像
    /// - FullBackup: 整盘备份镜像 (包含完整Windows目录结构)
    /// - Unknown: 未知类型但满足基本条件
    /// 
    /// 排除的类型：
    /// - WindowsPE: PE环境镜像
    fn is_installable_image(vol: &ImageInfo) -> bool {
        use lr_core::image_meta::WimImageType;
        
        // 1. 优先使用 image_type 字段判断
        match vol.image_type {
            WimImageType::StandardInstall => return true,
            WimImageType::FullBackup => return true,
            WimImageType::WindowsPE => return false,
            WimImageType::Unknown => {
                // 继续使用传统检测方法
            }
        }
        
        let name_lower = vol.name.to_lowercase();
        let install_type_lower = vol.installation_type.to_lowercase();
        
        // 2. 排除 installation_type 为 WindowsPE 的
        if install_type_lower == "windowspe" {
            return false;
        }
        
        // 3. 排除名称包含特定关键词的（PE环境、安装程序、安装媒体）
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
        
        // 4. 如果 installation_type 为空，进行额外检查
        // 整盘备份型WIM通常缺失 INSTALLATIONTYPE / DISPLAYNAME
        // 这时如果能拿到版本号（MAJOR/MINOR），就认为它是可安装系统镜像
        if vol.installation_type.is_empty() {
            if vol.major_version.is_some() {
                return true;
            }

            // 名称包含系统版本标识（Windows 10/11/Server 等）或备份标识
            let is_valid_system = name_lower.contains("windows 10") 
                || name_lower.contains("windows 11")
                || name_lower.contains("windows server")
                || name_lower.contains("windows 8")
                || name_lower.contains("windows 7")
                || name_lower.contains("backup")
                || name_lower.contains("备份")
                || name_lower.contains("系统镜像")
                || name_lower.contains("镜像");  // 默认生成的名称
            
            if !is_valid_system {
                return false;
            }
        }
        
        // 5. 如果 installation_type 明确是 Client 或 Server，直接通过
        if install_type_lower == "client" || install_type_lower == "server" {
            return true;
        }
        
        // 6. 其他情况（installation_type 为空但名称包含有效系统标识），通过
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
                // 直接使用 wimlib 解析出的版本号
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
                        log::info!(
                            "[STORAGE DRIVER] 镜像版本: major_version={}, is_win10_or_11={}",
                            v, is_win10_or_11
                        );
                    } else {
                        log::info!("[STORAGE DRIVER] 未检测到版本信息，不自动勾选磁盘控制器驱动");
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
                    // 触发无人值守检测
                    self.start_unattend_check_for_partition(windows_partitions[0]);
                } else {
                    // 有多个或没有系统分区，不默认选择
                    self.selected_partition = None;
                    self.partition_has_unattend = false;
                }
            } else {
                // 非PE环境，选择当前系统分区
                self.selected_partition = self
                    .partitions
                    .iter()
                    .position(|p| p.is_system_partition);
                // 触发无人值守检测
                if let Some(idx) = self.selected_partition {
                    self.start_unattend_check_for_partition(idx);
                }
            }
        }
    }

    /// 检查安装相关分区的BitLocker状态
    /// 返回需要解锁的分区列表
    fn check_bitlocker_for_install(&self) -> Vec<crate::ui::tools::BitLockerPartition> {
        use crate::core::bitlocker::BitLockerManager;
        
        let manager = BitLockerManager::new();
        if !manager.is_available() {
            return Vec::new();
        }
        
        let mut locked_partitions = Vec::new();
        
        // 检查目标安装分区
        if let Some(idx) = self.selected_partition {
            if let Some(partition) = self.partitions.get(idx) {
                let letter = partition.letter.chars().next().unwrap_or('C');
                if manager.needs_unlock(letter) {
                    let status = manager.get_status(letter);
                    locked_partitions.push(crate::ui::tools::BitLockerPartition {
                        letter: partition.letter.clone(),
                        label: partition.label.clone(),
                        total_size_mb: partition.total_size_mb,
                        status,
                        protection_method: "密码/恢复密钥".to_string(),
                        encryption_percentage: None,
                    });
                }
            }
        }
        
        // 检查所有可能用于存储数据的分区（非系统分区、非PE分区）
        for partition in &self.partitions {
            // 跳过已经添加的分区
            if locked_partitions.iter().any(|p| p.letter == partition.letter) {
                continue;
            }
            
            // 跳过X:盘（PE系统盘）
            if partition.letter.to_uppercase().starts_with('X') {
                continue;
            }
            
            let letter = partition.letter.chars().next().unwrap_or('C');
            if manager.needs_unlock(letter) {
                let status = manager.get_status(letter);
                locked_partitions.push(crate::ui::tools::BitLockerPartition {
                    letter: partition.letter.clone(),
                    label: partition.label.clone(),
                    total_size_mb: partition.total_size_mb,
                    status,
                    protection_method: "密码/恢复密钥".to_string(),
                    encryption_percentage: None,
                });
            }
        }
        
        locked_partitions
    }

    /// 启动 BitLocker 解密流程
    /// 在正常系统环境下，检测所有已解锁的加密分区，发送解密指令，并记录需要等待解密的分区
    /// 注意：因为要进入PE环境安装系统，PE无法访问加密分区，所以必须等待完全解密完成
    /// 返回是否启动了解密流程
    fn initiate_bitlocker_decryption(&mut self) -> bool {
        if self.is_pe_environment() {
            return false;
        }

        let manager = crate::core::bitlocker::BitLockerManager::new();

        // BitLocker 密钥透传现为默认行为（无开关）：正常端【不预解密】，改为把恢复密钥注入 PE、
        // 由 PE 启动后用恢复密钥解锁锁定卷（见 PeManager::maybe_inject_bitlocker_keys 与 PE 端
        // unlock_bitlocker_passthrough）。带 BitLocker 的系统盘重装无需漫长预解密。
        //
        // 前提：必须能拿到【目标系统盘】的恢复密钥——PE 要靠它解锁目标盘才能部署。
        // 若目标盘加密但取不到恢复密钥（例如只有 TPM 保护器、无数字恢复密码），
        // 透传进 PE 也解不开 → 回退到"彻底解密 BitLocker"方案（在正常端把卷全部解密）。
        let target_letter = self
            .selected_partition
            .and_then(|i| self.partitions.get(i))
            .and_then(|p| p.letter.chars().next())
            .unwrap_or('C')
            .to_ascii_uppercase();
        let target_drive = format!("{}:", target_letter);
        let target_status = manager.get_status(target_letter);
        let target_encrypted = matches!(
            target_status,
            crate::core::bitlocker::VolumeStatus::EncryptedUnlocked
                | crate::core::bitlocker::VolumeStatus::EncryptedLocked
        );

        if !target_encrypted {
            // 目标盘未加密：无需任何 BitLocker 处理（数据盘若加密，由密钥注入一并处理）。
            log::info!("[BITLOCKER] 目标盘 {} 未加密，无需预解密，正常继续", target_drive);
            self.decrypting_partitions.clear();
            return false;
        }

        match manager.get_recovery_key(&target_drive) {
            Ok(_) => {
                log::info!(
                    "[BITLOCKER] 已成功获取目标盘 {} 恢复密钥 → 走密钥透传（不预解密，进 PE 解锁）",
                    target_drive
                );
                self.decrypting_partitions.clear();
                return false;
            }
            Err(e) => {
                log::warn!(
                    "[BITLOCKER] 获取目标盘 {} 恢复密钥失败（{}）→ 回退彻底解密 BitLocker 方案",
                    target_drive, e
                );
                // 落入下方预解密流程
            }
        }

        log::info!("[BITLOCKER] 开始检测并强制解密分区（透传回退方案）...");
        self.decrypting_partitions.clear();

        // 回退：PE 无法用密钥解锁目标盘，必须在正常端预解密所有【已解锁】的加密分区
        // （含目标盘），否则进 PE 后这些卷处于锁定状态、无法访问/格式化。
        let mut decryption_started = false;

        for partition in &self.partitions {
            let drive_letter = partition.letter.chars().next().unwrap_or('C');
            let drive_str = format!("{}:", drive_letter);

            // 获取实时状态
            let current_status = manager.get_status(drive_letter);

            // 情况1: 已加密且已解锁 -> 发送解密指令并等待
            if current_status == crate::core::bitlocker::VolumeStatus::EncryptedUnlocked {
                log::info!("[BITLOCKER] 检测到已解锁的加密分区 {}，正在尝试彻底解密...", drive_str);

                let result = manager.decrypt(&drive_str);

                if result.success {
                    log::info!("[BITLOCKER] 分区 {} 解密指令已发送: {}", drive_str, result.message);
                    self.decrypting_partitions.push(drive_str);
                    decryption_started = true;
                } else {
                    log::error!("[BITLOCKER] 分区 {} 解密失败: {} (Code: {:?})",
                        drive_str, result.message, result.error_code);
                    // 即使失败，如果是因为已经在解密中，也应该等待
                }
            }
            // 情况2: 正在解密中 -> 直接加入等待列表
            else if current_status == crate::core::bitlocker::VolumeStatus::Decrypting {
                log::info!("[BITLOCKER] 分区 {} 已经在解密过程中，加入等待列表", drive_str);
                self.decrypting_partitions.push(drive_str);
                decryption_started = true;
            }
        }

        decryption_started
    }

    pub fn start_installation(&mut self) {
        let selected_index = match self.selected_partition {
            Some(idx) => idx,
            None => {
                log::error!("[INSTALL] 未选择目标分区，无法开始安装");
                return;
            }
        };
        let partition = self
            .partitions
            .get(selected_index)
            .cloned();
        if partition.is_none() {
            return;
        }
        let partition = partition.unwrap();

        // XP/2003 i386 文本安装介质的前置校验：
        // (1) 仅 Legacy/BIOS + MBR——XP 不支持 GPT/UEFI 启动；
        // (2) 不支持在运行中的系统上原地安装到当前系统盘（需进 PE）。
        if self.xp_i386_source.is_some() {
            // XP i386 仅 Legacy/MBR。拦截条件：目标盘【确为 GPT】，或用户【显式选了 UEFI 引导】。
            // 分区表「未知」(探测失败) 不再当 UEFI 拦——之前一块其实是 MBR 的盘，只因探测打了个嗝
            // 被判「未知」就被挡死。「高级选项」开启时整体放行（供 UEFI 化魔改镜像，或先用 Diskpart 脚本改分区）。
            let target_is_uefi = partition.partition_style == PartitionStyle::GPT
                || self.selected_boot_mode == BootModeSelection::UEFI;
            if target_is_uefi && !self.app_config.enable_advanced_options {
                self.show_error(
                    &tr!("原版 Windows XP/2003（i386 介质）仅支持 Legacy/BIOS + MBR 启动，无法安装到 GPT/UEFI 目标。\n\
                     如确需（例如使用 UEFI 化的魔改镜像，或先用 Diskpart 脚本把盘改成 MBR），\n\
                     请到「关于 → 高级选项」开启后重试。"),
                );
                return;
            }
            if !self.is_pe_environment() && partition.is_system_partition {
                self.show_error(
                    &tr!("从 XP i386 介质安装不支持在运行中的系统上原地安装到当前系统盘。\n\
                     请先重启进入 PE 环境，再选择目标分区进行安装。"),
                );
                return;
            }
        }

        // 1. 检查是否有需要解锁的 BitLocker 分区 (优先级最高)
        let locked_partitions = self.check_bitlocker_for_install();
        if !locked_partitions.is_empty() {
            log::info!("[INSTALL] 检测到 {} 个BitLocker锁定的分区，需要先解锁", locked_partitions.len());
            self.install_bitlocker_partitions = locked_partitions;
            self.install_bitlocker_current = self.install_bitlocker_partitions.first().map(|p| p.letter.clone());
            self.install_bitlocker_message.clear();
            self.install_bitlocker_password.clear();
            self.install_bitlocker_recovery_key.clear();
            self.install_bitlocker_mode = crate::app::BitLockerUnlockMode::Password;
            self.install_bitlocker_continue_after = true;
            self.show_install_bitlocker_dialog = true;
            return;
        }

        // 2. 尝试启动 BitLocker 解密
        // 如果有分区正在解密或开始解密，进入解密等待流程
        if self.initiate_bitlocker_decryption() {
            log::info!("[INSTALL] 检测到 BitLocker 分区需要解密，进入解密等待流程");
            
            self.bitlocker_decryption_needed = true;
            
            // 初始化安装状态，但步骤设为 0 (解密阶段)
            self.initialize_install_state(&partition, self.local_image_path.clone());
            self.install_step = 0; // 0 表示预处理/解密阶段
            
            return;
        }

        // 3. 正常继续安装
        self.bitlocker_decryption_needed = false;
        self.continue_installation_after_bitlocker();
    }
    
    /// 初始化安装状态变量
    fn initialize_install_state(&mut self, partition: &crate::core::disk::Partition, image_path: String) {
        let volume_index = self
            .selected_volume
            .and_then(|i| self.image_volumes.get(i).map(|v| v.index))
            .unwrap_or(1);

        let is_system_partition = partition.is_system_partition;
        let is_pe = self.is_pe_environment();

        self.install_mode = if is_pe || !is_system_partition {
            crate::app::InstallMode::Direct
        } else {
            crate::app::InstallMode::ViaPE
        };

        // XP/2003 i386 文本安装介质（无 WIM 可枚举版本）。
        let is_xp_i386 = self.xp_i386_source.is_some();
        // XP/2003 检测（与下方 is_xp 一致）：选中镜像主版本号为 5，或 i386 文本安装介质。
        let selected_is_xp = is_xp_i386
            || self
                .selected_volume
                .and_then(|i| self.image_volumes.get(i))
                .map(|v| v.major_version == Some(5))
                .unwrap_or(false);
        // 即使用户没打开过高级面板，也在发起安装时按「检测到 XP」应用一次默认勾选
        // （注入USB3/NVMe）。已应用过则尊重用户手动取消，不覆盖。
        self.advanced_options.apply_xp_defaults_if_needed(selected_is_xp);

        self.install_options = crate::app::InstallOptions {
            format_partition: self.format_partition,
            repair_boot: self.repair_boot,
            unattended_install: self.unattended_install,
            export_drivers: matches!(self.driver_action, crate::app::DriverAction::SaveOnly | crate::app::DriverAction::AutoImport),
            auto_reboot: self.auto_reboot,
            boot_mode: self.selected_boot_mode,
            advanced_options: self.advanced_options.clone(),
            driver_action: self.driver_action,
            custom_unattend_path: if self.unattended_install {
                self.custom_unattend_path.clone()
            } else {
                String::new()
            },
            // XP/2003 检测：选中镜像主版本号为 5（WIM/ESD 可识别；GHO 由 PE 端按 \Windows\Boot 兜底判断），
            // 或 i386 文本安装介质（selected_is_xp 已并入 is_xp_i386）。
            is_xp: selected_is_xp,
            is_xp_i386,
            // 仅在「高级选项」开启时才让其生效
            run_diskpart_scripts: self.app_config.enable_advanced_options
                && self.run_diskpart_scripts,
        };

        self.is_installing = true;
        self.current_panel = crate::app::Panel::InstallProgress;
        self.install_progress = crate::app::InstallProgress::default();
        self.auto_reboot_triggered = false;

        self.install_target_partition = partition.letter.clone();
        self.install_image_path = image_path;
        self.install_volume_index = volume_index;
        self.install_is_system_partition = is_system_partition;
        
        // 创建进度通道
        let (tx, rx) = std::sync::mpsc::channel();
        self.install_progress_rx = Some(rx);
        
        // 如果有正在解密的分区，启动监控线程
        if !self.decrypting_partitions.is_empty() {
            log::info!("[INSTALL] 启动 BitLocker 解密监控线程...");
            let partitions = self.decrypting_partitions.clone();
            
            std::thread::spawn(move || {
                let manager = crate::core::bitlocker::BitLockerManager::new();
                
                loop {
                    let mut all_decrypted = true;
                    let mut waiting_list = Vec::new();
                    let mut max_percentage = 0.0f32;

                    for part in &partitions {
                        let letter = part.chars().next().unwrap_or('C');
                        let (status, percentage) = manager.get_status_with_percentage(letter);

                        // 因为要进入PE环境安装系统，PE无法访问加密分区
                        // 所以必须等待完全解密完成（状态变为 NotEncrypted）
                        if status != crate::core::bitlocker::VolumeStatus::NotEncrypted {
                            all_decrypted = false;
                            waiting_list.push(format!("{} ({:.1}%)", part, percentage));

                            // 记录最大的加密百分比（用于显示进度）
                            if percentage > max_percentage {
                                max_percentage = percentage;
                            }
                        }
                    }

                    if all_decrypted {
                        let _ = tx.send(crate::core::dism::DismProgress {
                            percentage: 100,
                            status: "DECRYPTION_COMPLETE".to_string(),
                        });
                        break;
                    } else {
                        // 将加密百分比转换为解密进度（100% - 加密百分比）
                        let decryption_progress = (100.0 - max_percentage).max(0.0).min(100.0) as u8;

                        let _ = tx.send(crate::core::dism::DismProgress {
                            percentage: decryption_progress,
                            status: format!("DECRYPTING:{}", tr!("正在解密: {}", waiting_list.join(", "))),
                        });
                    }

                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
            });
        }
    }

    /// BitLocker解锁完成后继续安装
    pub fn continue_installation_after_bitlocker(&mut self) {
        let selected_index = match self.selected_partition {
            Some(idx) => idx,
            None => {
                log::error!("[INSTALL] 未选择目标分区，无法继续安装");
                return;
            }
        };
        let partition = self
            .partitions
            .get(selected_index)
            .cloned();
        if partition.is_none() {
            return;
        }
        let partition = partition.unwrap();

        // 解锁完成后，再次尝试启动解密流程
        // 如果有分区需要解密，转入解密等待流程
        if self.initiate_bitlocker_decryption() {
            log::info!("[INSTALL] 解锁后检测到 BitLocker 分区需要解密，进入解密等待流程");
            self.bitlocker_decryption_needed = true;
            self.initialize_install_state(&partition, self.local_image_path.clone());
            self.install_step = 0; // 解密阶段
            return;
        }

        // 如果不需要通过PE安装，或者已经在PE环境，直接初始化并开始
        self.bitlocker_decryption_needed = false;
        self.initialize_install_state(&partition, self.local_image_path.clone());

        // 如果需要通过PE安装，检查PE是否存在
        if self.install_mode == crate::app::InstallMode::ViaPE {
            let pe_info = self.selected_pe_for_install.and_then(|idx| {
                self.config.as_ref().and_then(|c| c.pe_list.get(idx).cloned())
            });
            
            if let Some(pe) = pe_info {
                let (pe_exists, _) = crate::core::pe::PeManager::check_pe_exists(&pe.filename);
                if !pe_exists {
                    log::info!("[INSTALL] PE文件不存在，开始下载: {}", pe.filename);
                    self.pending_download_url = Some(pe.download_url.clone());
                    self.pending_download_filename = Some(pe.filename.clone());
                    self.pending_pe_md5 = pe.md5.clone();
                    let pe_dir = crate::utils::path::get_pe_dir()
                        .to_string_lossy()
                        .to_string();
                    self.download_save_path = pe_dir;
                    self.pe_download_then_action = Some(crate::app::PeDownloadThenAction::Install);
                    self.current_panel = crate::app::Panel::DownloadProgress;
                    
                    // 因为转到了下载页面，需要重置 is_installing
                    self.is_installing = false;
                    return;
                }
            }
        }

        // 正常开始步骤 1 (或 0 如果是 ViaPE 的话，但这里我们统一用 0 作为特殊解密步骤)
        // InstallProgress UI 里的 start_xxx_thread 会在 step == 0 时启动
        // 但我们需要区分 "解密等待中(step=0)" 和 "刚初始化准备开始(step=0)"
        // 为了区分，我们将 install_step 设为 1 表示准备好开始安装了 (对于 Direct 模式)
        // 或者保持 0，但在 UI update 中判断 decrypting_partitions 是否为空
        
        // 这里的 install_step = 0 会触发 show_install_progress 里的启动线程逻辑
        // 我们只需确保 decrypting_partitions 为空，这样 UI 就不会卡在解密界面
        self.install_step = 0;
    }
    
    /// 开始异步检测分区中的无人值守配置文件
    fn start_unattend_check_for_partition(&mut self, partition_index: usize) {
        let partition = match self.partitions.get(partition_index) {
            Some(p) => p,
            None => return,
        };
        
        // 如果分区没有 Windows 系统，不需要检测
        if !partition.has_windows {
            self.partition_has_unattend = false;
            self.last_unattend_check_partition = Some(partition.letter.clone());
            // 默认勾选无人值守
            self.unattended_install = true;
            return;
        }
        
        // 避免重复检测同一分区
        let partition_id = partition.letter.clone();
        if self.last_unattend_check_partition.as_ref() == Some(&partition_id) {
            return;
        }
        
        log::info!("[UNATTEND CHECK] 开始检测分区 {} 的无人值守配置", partition_id);
        
        self.unattend_check_loading = true;
        self.last_unattend_check_partition = Some(partition_id.clone());
        
        let (tx, rx) = mpsc::channel::<UnattendCheckResult>();

        *UNATTEND_CHECK_RESULT_RX.lock().unwrap() = Some(rx);

        let partition_letter = partition_id;
        
        std::thread::spawn(move || {
            let result = Self::check_unattend_files_in_partition(&partition_letter);
            let _ = tx.send(result);
        });
    }
    
    /// 检查分区中的无人值守配置文件（在后台线程执行）
    fn check_unattend_files_in_partition(partition_letter: &str) -> UnattendCheckResult {
        use std::path::Path;
        
        // 常见的无人值守配置文件位置
        let unattend_locations = [
            // Windows 安装后的位置
            format!("{}\\Windows\\Panther\\unattend.xml", partition_letter),
            format!("{}\\Windows\\Panther\\Unattend.xml", partition_letter),
            format!("{}\\Windows\\Panther\\autounattend.xml", partition_letter),
            format!("{}\\Windows\\Panther\\Autounattend.xml", partition_letter),
            // Sysprep 位置
            format!("{}\\Windows\\System32\\Sysprep\\unattend.xml", partition_letter),
            format!("{}\\Windows\\System32\\Sysprep\\Unattend.xml", partition_letter),
            format!("{}\\Windows\\System32\\Sysprep\\Panther\\unattend.xml", partition_letter),
            // 根目录位置（安装媒体）
            format!("{}\\unattend.xml", partition_letter),
            format!("{}\\Unattend.xml", partition_letter),
            format!("{}\\autounattend.xml", partition_letter),
            format!("{}\\Autounattend.xml", partition_letter),
            format!("{}\\AutoUnattend.xml", partition_letter),
        ];
        
        let mut detected_paths = Vec::new();
        
        for location in &unattend_locations {
            if Path::new(location).exists() {
                log::info!("[UNATTEND CHECK] 发现无人值守配置: {}", location);
                detected_paths.push(location.clone());
            }
        }
        
        let has_unattend = !detected_paths.is_empty();
        
        if has_unattend {
            log::info!("[UNATTEND CHECK] 分区 {} 存在 {} 个无人值守配置文件",
                partition_letter, detected_paths.len());
        } else {
            log::info!("[UNATTEND CHECK] 分区 {} 无无人值守配置文件", partition_letter);
        }
        
        UnattendCheckResult {
            partition_letter: partition_letter.to_string(),
            has_unattend,
            detected_paths,
        }
    }
    
    /// 检查无人值守检测状态
    fn check_unattend_status(&mut self) {
        if !self.unattend_check_loading {
            return;
        }
        
        // 同上：只在 try_recv 期间短暂持锁，取出结果后释放 guard 再处理（下方会调用
        // self.apply_unattend_default 等方法）。
        let received = {
            let mut guard = UNATTEND_CHECK_RESULT_RX.lock().unwrap();
            if let Some(rx) = guard.as_ref() {
                match rx.try_recv() {
                    Ok(result) => {
                        *guard = None;
                        Some(result)
                    }
                    Err(_) => None,
                }
            } else {
                None
            }
        };
        if let Some(result) = received {
            self.unattend_check_loading = false;

            // 确保结果对应当前选中的分区
            let current_partition = self.selected_partition
                .and_then(|idx| self.partitions.get(idx))
                .map(|p| p.letter.clone());

            if current_partition.as_ref() == Some(&result.partition_letter) {
                self.partition_has_unattend = result.has_unattend;
                // 目标分区或源镜像任一自带无人值守 → 默认取消勾选。
                self.apply_unattend_default();
            }
        }
    }
    
    /// 根据「目标分区」与「源镜像」是否自带无人值守，决定勾选框默认值。
    /// 任一存在即默认取消勾选；都不存在则默认勾选。仅改默认，不禁用——
    /// 分区冲突的禁用由 is_unattend_option_disabled 单独处理。
    fn apply_unattend_default(&mut self) {
        if self.partition_has_unattend || self.source_has_unattend {
            self.unattended_install = false;
            log::info!(
                "[UNATTEND CHECK] 检测到自带无人值守(分区={}, 源镜像={})，默认取消勾选",
                self.partition_has_unattend, self.source_has_unattend
            );
        } else {
            self.unattended_install = true;
            log::info!("[UNATTEND CHECK] 未检测到自带无人值守，默认勾选");
        }
    }

    /// 检测源镜像/安装介质是否自带无人值守应答，结果写入 self.source_has_unattend 并刷新默认勾选。
    /// 两层探测，均不挂载：
    /// 1) 文件系统层（detect_source_unattend）：i386\winnt.sif、介质根/镜像目录的 autounattend.xml 等；
    /// 2) WIM 内置层（wim_has_embedded_unattend）：用 wimlib 读元数据查 WIM/ESD 内
    ///    \Windows\Panther\unattend.xml 等（魔改/已 sysprep 镜像常见）。
    fn refresh_source_unattend(&mut self) {
        let mut detected = Self::detect_source_unattend(&self.local_image_path);
        if !detected {
            let lower = self.local_image_path.to_lowercase();
            if lower.ends_with(".wim") || lower.ends_with(".esd") || lower.ends_with(".swm") {
                // 内置应答一般对所有版本一致，取首个可安装卷的索引探测即可。
                let index = self
                    .image_volumes
                    .iter()
                    .find(|v| Self::is_installable_image(v))
                    .or_else(|| self.image_volumes.first())
                    .map(|v| v.index)
                    .unwrap_or(1);
                detected = Self::wim_has_embedded_unattend(&self.local_image_path, index);
            }
        }
        self.source_has_unattend = detected;
        self.apply_unattend_default();
    }

    /// 用 wimlib 廉价探测 WIM/ESD 卷内是否内置无人值守应答（只读元数据，不挂载）。
    /// 失败/不可用时按"无"处理（不阻断流程）。
    fn wim_has_embedded_unattend(image_file: &str, index: u32) -> bool {
        const PATHS: [&str; 4] = [
            "\\Windows\\Panther\\unattend.xml",
            "\\Windows\\Panther\\Autounattend.xml",
            "\\Windows\\System32\\Sysprep\\unattend.xml",
            "\\unattend.xml",
        ];
        match lr_core::WimEngineManager::new_current() {
            Ok(mgr) => match mgr.image_contains_any_path(image_file, index, &PATHS) {
                Ok(found) => {
                    if found {
                        log::info!("[UNATTEND CHECK] WIM 卷内置无人值守应答 (index={})", index);
                    }
                    found
                }
                Err(e) => {
                    log::warn!("[UNATTEND CHECK] WIM 内置应答探测失败(忽略): {}", e);
                    false
                }
            },
            Err(e) => {
                log::warn!("[UNATTEND CHECK] WIM 引擎初始化失败(忽略): {}", e);
                false
            }
        }
    }

    /// 见 refresh_source_unattend。纯函数，便于后台/同步调用。
    fn detect_source_unattend(image_path: &str) -> bool {
        use std::path::Path;
        if image_path.trim().is_empty() {
            return false;
        }
        let p = Path::new(image_path);

        // XP/2003 i386 源：local_image_path 指向 i386 目录。
        if lr_core::xp_i386::is_valid_i386(p) {
            let in_i386 = p.join("winnt.sif").exists();
            let at_media_root = p
                .parent()
                .map(|root| root.join("winnt.sif").exists())
                .unwrap_or(false);
            if in_i386 || at_media_root {
                log::info!("[UNATTEND CHECK] 源 i386 自带 winnt.sif");
                return true;
            }
            return false;
        }

        // Windows 安装介质/本地镜像：检查介质根或镜像所在目录下的应答文件。
        // 介质根：image_path 形如 X:\sources\install.wim → 取盘符根 X:\；
        // 本地文件：取镜像所在目录。
        let base = if image_path.to_lowercase().contains("\\sources\\") {
            // 取盘符根（X:\）
            image_path
                .split_once('\\')
                .map(|(drive, _)| format!("{}\\", drive))
                .unwrap_or_else(|| image_path.to_string())
        } else {
            p.parent()
                .map(|d| d.to_string_lossy().to_string())
                .unwrap_or_default()
        };
        if base.is_empty() {
            return false;
        }
        for name in ["autounattend.xml", "Autounattend.xml", "AutoUnattend.xml", "unattend.xml", "Unattend.xml"] {
            if Path::new(&format!("{}\\{}", base.trim_end_matches('\\'), name)).exists() {
                log::info!("[UNATTEND CHECK] 源介质根/镜像目录自带 {}", name);
                return true;
            }
        }
        false
    }

    /// 判断无人值守选项是否被禁用（考虑格式化状态）
    pub fn is_unattend_option_disabled(&self) -> bool {
        self.partition_has_unattend && !self.format_partition
    }
    
    /// 获取依赖无人值守的高级选项提示
    pub fn get_unattend_dependent_options_hint(&self) -> &'static str {
        "以下选项依赖无人值守配置：\n\
         • OOBE绕过强制联网\n\
         • 自定义用户名\n\
         • 删除预装UWP应用\n\n\
         由于目标分区已存在无人值守配置文件，这些选项可能无法正常生效。"
    }
}

static ISO_MOUNT_RESULT_RX: std::sync::Mutex<Option<mpsc::Receiver<IsoMountResult>>> = std::sync::Mutex::new(None);
static IMAGE_INFO_RESULT_RX: std::sync::Mutex<Option<mpsc::Receiver<ImageInfoResult>>> = std::sync::Mutex::new(None);
static UNATTEND_CHECK_RESULT_RX: std::sync::Mutex<Option<mpsc::Receiver<UnattendCheckResult>>> = std::sync::Mutex::new(None);
