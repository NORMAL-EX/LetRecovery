//! 小白模式UI模块
//! 提供简化的系统重装界面

use egui;

use crate::app::{App, EasyModeLogoState, Panel};
use crate::download::config::EasyModeSystem;

/// Logo加载结果
pub struct LogoLoadResult {
    pub url: String,
    pub data: Result<Vec<u8>, String>,
}

impl App {
    /// 显示小白模式系统安装界面
    pub fn show_easy_mode_install(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // 检查ISO挂载状态和镜像信息加载状态（支持小白模式自动安装）
        self.check_iso_mount_status();
        
        ui.heading("系统重装");
        ui.separator();
        
        // 显示设置提示
        if !self.app_config.easy_mode_settings_tip_dismissed {
            ui.horizontal(|ui| {
                ui.colored_label(
                    egui::Color32::from_rgb(100, 181, 246),
                    "💡 您可以在\"关于\"页面中关闭小白模式",
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("×").clicked() {
                        self.app_config.dismiss_easy_mode_settings_tip();
                    }
                });
            });
            ui.add_space(10.0);
        }
        
        // 获取小白模式配置
        let easy_config = self.config.as_ref()
            .and_then(|c| c.easy_mode_config.as_ref());
        
        if easy_config.is_none() {
            if self.remote_config_loading {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("正在加载系统列表...");
                });
            } else {
                ui.colored_label(
                    egui::Color32::RED,
                    "❌ 无法获取系统列表，请检查网络连接后重启程序",
                );
            }
            return;
        }
        
        let systems = easy_config.unwrap().get_systems();
        
        if systems.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(255, 165, 0),
                "⚠ 暂无可用的系统镜像",
            );
            return;
        }
        
        ui.add_space(10.0);
        ui.label("请选择要安装的系统：");
        ui.add_space(15.0);
        
        // 显示系统选择卡片
        let available_width = ui.available_width();
        let card_width = 200.0;
        let card_height = 260.0;
        let spacing = 15.0;
        let cards_per_row = ((available_width + spacing) / (card_width + spacing)).floor() as usize;
        let cards_per_row = cards_per_row.max(1);
        
        // 计算实际卡片数量和居中所需的左边距
        let total_systems = systems.len();
        let actual_cards_in_first_row = total_systems.min(cards_per_row);
        let total_cards_width = actual_cards_in_first_row as f32 * card_width 
            + (actual_cards_in_first_row.saturating_sub(1)) as f32 * spacing;
        let left_margin = ((available_width - total_cards_width) / 2.0).max(0.0);
        
        // 存储需要处理的点击事件
        let mut clicked_system_idx: Option<usize> = None;
        let mut should_show_confirm = false;
        
        egui::ScrollArea::vertical()
            .max_height(ui.available_height() - 50.0)
            .show(ui, |ui| {
                // 添加左边距实现居中
                ui.horizontal(|ui| {
                    ui.add_space(left_margin);
                    ui.vertical(|ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(spacing, spacing);
                            
                            for (idx, (name, system)) in systems.iter().enumerate() {
                                let is_selected = self.easy_mode_selected_system == Some(idx);
                                
                                // 绘制系统卡片并获取交互结果
                                let (card_clicked, install_clicked) = self.draw_system_card_v2(
                                    ui,
                                    ctx,
                                    idx,
                                    name,
                                    system,
                                    is_selected,
                                    card_width,
                                    card_height,
                                );
                                
                                if card_clicked {
                                    clicked_system_idx = Some(idx);
                                }
                                
                                if install_clicked {
                                    should_show_confirm = true;
                                }
                                
                                // 每行显示指定数量的卡片后换行
                                if (idx + 1) % cards_per_row == 0 {
                                    ui.end_row();
                                }
                            }
                        });
                    });
                });
            });
        
        // 在循环外处理状态更新
        if let Some(idx) = clicked_system_idx {
            if self.easy_mode_selected_system != Some(idx) {
                self.easy_mode_selected_system = Some(idx);
                // 默认选择第一个分卷
                if let Some((_, system)) = systems.get(idx) {
                    if !system.volume.is_empty() {
                        self.easy_mode_selected_volume = Some(0);
                    }
                }
            }
        }
        
        if should_show_confirm {
            self.easy_mode_show_confirm_dialog = true;
        }
        
        // 显示确认对话框
        if self.easy_mode_show_confirm_dialog {
            self.show_easy_mode_confirm_dialog(ctx, &systems);
        }
    }
    
    /// 绘制系统选择卡片（新版本，正确处理交互）
    /// 返回 (卡片被点击, 安装按钮被点击)
    fn draw_system_card_v2(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        idx: usize,
        name: &str,
        system: &EasyModeSystem,
        is_selected: bool,
        width: f32,
        height: f32,
    ) -> (bool, bool) {
        let mut card_clicked = false;
        let mut install_clicked = false;
        
        // 使用 egui 原版风格的 Frame
        let frame = if is_selected {
            egui::Frame::none()
                .fill(ui.visuals().selection.bg_fill)
                .stroke(egui::Stroke::new(2.0, ui.visuals().selection.stroke.color))
                .rounding(6.0)
                .inner_margin(12.0)
        } else {
            egui::Frame::none()
                .fill(ui.visuals().widgets.noninteractive.bg_fill)
                .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
                .rounding(6.0)
                .inner_margin(12.0)
        };
        
        frame.show(ui, |ui| {
            // 只设置宽度，高度自适应内容
            ui.set_width(width - 24.0);
            
            ui.vertical(|ui| {
                // 上半部分：可点击区域（Logo + 名称）
                let clickable_rect = ui.available_rect_before_wrap();
                let top_area_height = 130.0;
                
                let top_rect = egui::Rect::from_min_size(
                    clickable_rect.min,
                    egui::vec2(clickable_rect.width(), top_area_height),
                );
                
                // 为点击区域分配响应
                let top_response = ui.allocate_rect(top_rect, egui::Sense::click());
                
                // 在点击区域内绘制内容
                ui.allocate_ui_at_rect(top_rect, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(5.0);
                        
                        // 系统Logo
                        let logo_size = 72.0;
                        self.draw_system_logo(ui, ctx, &system.os_logo, logo_size);
                        
                        ui.add_space(10.0);
                        
                        // 系统名称
                        let text_color = if is_selected {
                            ui.visuals().strong_text_color()
                        } else {
                            ui.visuals().text_color()
                        };
                        ui.label(egui::RichText::new(name).size(15.0).strong().color(text_color));
                    });
                });
                
                // 检测上半部分点击
                if top_response.clicked() {
                    card_clicked = true;
                }
                
                // 悬停效果
                if top_response.hovered() && !is_selected {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                
                // 下半部分：仅在选中时显示版本选择和安装按钮
                if is_selected {
                    ui.add_space(5.0);
                    ui.separator();
                    ui.add_space(8.0);
                    
                    ui.vertical_centered(|ui| {
                        if !system.volume.is_empty() {
                            // 版本选择下拉框
                            let selected_vol_name = self.easy_mode_selected_volume
                                .and_then(|vol_idx| system.volume.get(vol_idx))
                                .map(|v| v.name.as_str())
                                .unwrap_or("请选择版本");
                            
                            // 使用唯一的 ID
                            let combo_id = egui::Id::new(format!("easy_vol_combo_{}", idx));
                            
                            egui::ComboBox::from_id_source(combo_id)
                                .selected_text(selected_vol_name)
                                .width(width - 50.0)
                                .show_ui(ui, |ui| {
                                    for (vol_idx, vol) in system.volume.iter().enumerate() {
                                        let is_vol_selected = self.easy_mode_selected_volume == Some(vol_idx);
                                        if ui.selectable_label(is_vol_selected, &vol.name).clicked() {
                                            self.easy_mode_selected_volume = Some(vol_idx);
                                        }
                                    }
                                });
                            
                            ui.add_space(12.0);
                            
                            // 安装按钮 - 检查是否选择了版本
                            let can_install = self.easy_mode_selected_volume.is_some();
                            
                            let button = egui::Button::new(
                                egui::RichText::new("开始安装").strong()
                            );
                            
                            if ui.add_enabled(can_install, button).clicked() {
                                install_clicked = true;
                            }
                            
                            if !can_install {
                                ui.label(egui::RichText::new("请先选择版本").small().weak());
                            }
                        } else {
                            ui.label(egui::RichText::new("无可用版本").weak());
                        }
                    });
                }
            });
        });
        
        (card_clicked, install_clicked)
    }
    
    /// 绘制系统Logo
    fn draw_system_logo(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, logo_url: &str, size: f32) {
        // 首先检查是否是内嵌 Logo 标识符
        if crate::ui::EmbeddedLogoType::is_embedded_logo_identifier(logo_url) {
            // 获取当前是否为深色模式
            let is_dark_mode = ui.visuals().dark_mode;
            
            // 尝试获取内嵌 logo 纹理
            if let Some(texture) = self.embedded_assets.get_logo_by_config_string(
                ctx,
                logo_url,
                is_dark_mode,
                size as u32,
            ) {
                // 使用内嵌的 SVG 纹理
                ui.image(egui::load::SizedTexture::new(texture.id(), egui::vec2(size, size)));
                return;
            } else {
                // 内嵌 logo 加载失败，显示默认图标
                ui.label(egui::RichText::new("💻").size(size * 0.6));
                return;
            }
        }
        
        // 检查缓存（URL 形式的 logo）
        if let Some(state) = self.easy_mode_system_logo_cache.get(logo_url) {
            match state {
                EasyModeLogoState::Loaded(texture) => {
                    ui.image(egui::load::SizedTexture::new(texture.id(), egui::vec2(size, size)));
                    return;
                }
                EasyModeLogoState::Loading => {
                    ui.add_sized([size, size], egui::Spinner::new());
                    return;
                }
                EasyModeLogoState::Failed => {
                    // 显示默认图标
                    ui.label(egui::RichText::new("💻").size(size * 0.6));
                    return;
                }
            }
        }
        
        // 开始加载
        if !self.easy_mode_logo_loading.contains(logo_url) {
            self.easy_mode_logo_loading.insert(logo_url.to_string());
            self.easy_mode_system_logo_cache.insert(
                logo_url.to_string(),
                EasyModeLogoState::Loading,
            );
            
            let url = logo_url.to_string();
            let ctx_clone = ctx.clone();
            
            std::thread::spawn(move || {
                let result = load_logo_from_url(&url);
                ctx_clone.request_repaint();
                
                // 通过静态变量传递结果
                unsafe {
                    LOGO_LOAD_RESULTS.push(LogoLoadResult {
                        url,
                        data: result,
                    });
                }
            });
        }
        
        ui.add_sized([size, size], egui::Spinner::new());
    }
    
    /// 处理Logo加载结果
    pub fn process_easy_mode_logo_results(&mut self, ctx: &egui::Context) {
        let results: Vec<LogoLoadResult> = unsafe {
            std::mem::take(&mut LOGO_LOAD_RESULTS)
        };
        
        for result in results {
            self.easy_mode_logo_loading.remove(&result.url);
            
            match result.data {
                Ok(data) => {
                    // 尝试加载图像
                    if let Ok(image) = image::load_from_memory(&data) {
                        let image = image.to_rgba8();
                        let size = [image.width() as usize, image.height() as usize];
                        let pixels = image.into_raw();
                        
                        let texture = ctx.load_texture(
                            &result.url,
                            egui::ColorImage::from_rgba_unmultiplied(size, &pixels),
                            egui::TextureOptions::LINEAR,
                        );
                        
                        self.easy_mode_system_logo_cache.insert(
                            result.url,
                            EasyModeLogoState::Loaded(texture),
                        );
                    } else {
                        self.easy_mode_system_logo_cache.insert(
                            result.url,
                            EasyModeLogoState::Failed,
                        );
                    }
                }
                Err(_) => {
                    self.easy_mode_system_logo_cache.insert(
                        result.url,
                        EasyModeLogoState::Failed,
                    );
                }
            }
        }
    }
    
    /// 显示小白模式确认对话框
    fn show_easy_mode_confirm_dialog(
        &mut self,
        ctx: &egui::Context,
        systems: &[(String, EasyModeSystem)],
    ) {
        let selected_system = self.easy_mode_selected_system
            .and_then(|idx| systems.get(idx));
        let selected_volume = selected_system
            .and_then(|(_, sys)| {
                self.easy_mode_selected_volume.and_then(|idx| sys.volume.get(idx))
            });
        
        if selected_system.is_none() || selected_volume.is_none() {
            self.easy_mode_show_confirm_dialog = false;
            return;
        }
        
        let (system_name, system) = selected_system.unwrap();
        let volume = selected_volume.unwrap();
        
        let window_width = 420.0;
        
        egui::Window::new("确认重装系统")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .fixed_size([window_width, 320.0])
            .show(ctx, |ui| {
                ui.add_space(10.0);
                
                // 警告标题
                ui.horizontal(|ui| {
                    let text = egui::RichText::new("⚠️ 警告").size(20.0).strong();
                    let text_width = 80.0;
                    ui.add_space((window_width - text_width) / 2.0 - 16.0);
                    ui.colored_label(egui::Color32::from_rgb(255, 193, 7), text);
                });
                
                ui.add_space(15.0);
                
                // 安装信息
                ui.horizontal(|ui| {
                    let text = format!("您即将安装: {} - {}", system_name, volume.name);
                    let text_width = text.len() as f32 * 7.0;
                    ui.add_space((window_width - text_width) / 2.0 - 16.0);
                    ui.label(&text);
                });
                
                ui.add_space(10.0);
                
                // 警告文字
                ui.horizontal(|ui| {
                    let text = "此操作将清除 C 盘（系统盘）上的所有数据！";
                    let text_width = 280.0;
                    ui.add_space((window_width - text_width) / 2.0 - 16.0);
                    ui.colored_label(egui::Color32::RED, text);
                });
                
                ui.add_space(5.0);
                
                // 备份提示
                ui.horizontal(|ui| {
                    let text = "请确保已备份重要文件。";
                    let text_width = 150.0;
                    ui.add_space((window_width - text_width) / 2.0 - 16.0);
                    ui.label(text);
                });
                
                ui.add_space(15.0);
                ui.separator();
                ui.add_space(10.0);
                
                // 优化标题
                ui.horizontal(|ui| {
                    let text_width = 130.0;
                    ui.add_space((window_width - text_width) / 2.0 - 16.0);
                    ui.label(egui::RichText::new("将自动应用以下优化：").small().strong());
                });
                
                ui.add_space(5.0);
                
                // 优化选项 - Grid宽度约280
                ui.horizontal(|ui| {
                    let grid_width = 280.0;
                    ui.add_space((window_width - grid_width) / 2.0 - 16.0);
                    egui::Grid::new("easy_mode_options_grid")
                        .num_columns(2)
                        .spacing([20.0, 4.0])
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new("• OOBE绕过强制联网").small());
                            ui.label(egui::RichText::new("• 删除预装UWP应用").small());
                            ui.end_row();
                            ui.label(egui::RichText::new("• 导入磁盘控制器驱动").small());
                            ui.label(egui::RichText::new("• 自动导入当前驱动").small());
                            ui.end_row();
                        });
                });
                
                ui.add_space(20.0);
                
                // 按钮 - 两个按钮约150宽
                ui.horizontal(|ui| {
                    let buttons_width = 150.0;
                    ui.add_space((window_width - buttons_width) / 2.0 - 16.0);
                    
                    if ui.button("取消").clicked() {
                        self.easy_mode_show_confirm_dialog = false;
                    }
                    
                    ui.add_space(20.0);
                    
                    let confirm_btn = egui::Button::new(
                        egui::RichText::new("确认安装").color(egui::Color32::WHITE)
                    ).fill(egui::Color32::from_rgb(200, 60, 60));
                    
                    if ui.add(confirm_btn).clicked() {
                        self.easy_mode_show_confirm_dialog = false;
                        self.start_easy_mode_install(
                            system_name,
                            system,
                            volume.number,
                        );
                    }
                });
                
                ui.add_space(10.0);
            });
    }
    
    /// 开始小白模式安装
    fn start_easy_mode_install(
        &mut self,
        system_name: &str,
        system: &EasyModeSystem,
        volume_number: u32,
    ) {
        log::info!("[EASY MODE] 开始安装 {} 分卷 {}", system_name, volume_number);
        
        // 设置安装参数
        let download_url = system.os_download.clone();
        let filename = download_url.split('/').last()
            .unwrap_or("system.esd")
            .to_string();
        
        // 设置高级选项（小白模式默认选项）
        self.advanced_options.bypass_nro = true;  // OOBE绕过强制联网
        self.advanced_options.remove_uwp_apps = true;  // 删除预装UWP应用
        self.advanced_options.import_storage_controller_drivers = true;  // 导入磁盘控制器驱动
        self.advanced_options.custom_volume_label = true;  // 自定义卷标
        self.advanced_options.volume_label = "OS".to_string();  // 系统盘卷标设置为"OS"
        
        // 设置用户名
        let username = crate::core::app_config::get_current_username()
            .unwrap_or_else(|| "User".to_string());
        self.advanced_options.custom_username = true;
        self.advanced_options.username = username;
        
        // 设置安装选项
        self.format_partition = true;
        self.repair_boot = true;
        self.unattended_install = true;
        self.driver_action = crate::app::DriverAction::AutoImport;
        self.auto_reboot = true;
        
        // 选择系统分区
        let system_partition_idx = self.partitions.iter()
            .position(|p| p.is_system_partition);
        
        if system_partition_idx.is_none() {
            self.show_error("未找到系统分区，无法进行安装");
            return;
        }
        
        self.selected_partition = system_partition_idx;
        
        // 保存分卷号
        self.install_volume_index = volume_number;
        
        // 开始下载系统镜像
        let pe_dir = crate::utils::path::get_exe_dir()
            .join("downloads")
            .to_string_lossy()
            .to_string();
        let _ = std::fs::create_dir_all(&pe_dir);
        
        self.pending_download_url = Some(download_url);
        self.pending_download_filename = Some(filename.clone());
        self.download_save_path = pe_dir.clone();
        self.download_then_install = true;
        self.download_then_install_path = Some(format!("{}\\{}", pe_dir, filename));
        
        // 设置小白模式自动安装标志，下载完成后自动开始安装
        self.easy_mode_auto_install = true;
        
        // 切换到下载进度页面
        self.current_panel = Panel::DownloadProgress;
    }
}

/// 从URL加载Logo
fn load_logo_from_url(url: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    
    let response = client.get(url)
        .send()
        .map_err(|e| e.to_string())?;
    
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    
    response.bytes()
        .map(|b| b.to_vec())
        .map_err(|e| e.to_string())
}

// 静态变量存储Logo加载结果
static mut LOGO_LOAD_RESULTS: Vec<LogoLoadResult> = Vec::new();
