use egui;
use std::path::Path;
use std::sync::Mutex;

use crate::app::{App, OnlineDownloadTab, PendingSoftDownload, SoftIconState};
use crate::download::config::{OnlineSystem, OnlineSoftware, OnlineGpuDriver};

/// 图标加载结果
struct IconLoadResult {
    url: String,
    data: Option<Vec<u8>>,
}

impl App {
    pub fn show_online_download(&mut self, ui: &mut egui::Ui) {
        ui.heading("在线下载");
        ui.separator();

        // 检查远程配置状态
        if let Some(ref remote_config) = self.remote_config {
            if !remote_config.loaded && !self.remote_config_loading {
                ui.colored_label(egui::Color32::from_rgb(255, 165, 0), "⚠ 远程配置加载失败");
                if let Some(ref error) = remote_config.error {
                    ui.label(format!("错误: {}", error));
                }
                ui.add_space(10.0);
                if ui.button("重试加载").clicked() {
                    self.start_remote_config_loading();
                }
                return;
            }
        }

        // 显示加载状态
        if self.remote_config_loading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("正在加载在线资源...");
            });
            ui.add_space(10.0);
        }

        // 选项卡
        ui.horizontal(|ui| {
            if ui.selectable_label(
                self.online_download_tab == OnlineDownloadTab::SystemImage,
                "📀 系统镜像"
            ).clicked() {
                self.online_download_tab = OnlineDownloadTab::SystemImage;
            }
            
            ui.add_space(10.0);
            
            if ui.selectable_label(
                self.online_download_tab == OnlineDownloadTab::Software,
                "📦 软件下载"
            ).clicked() {
                self.online_download_tab = OnlineDownloadTab::Software;
            }
            
            ui.add_space(10.0);
            
            if ui.selectable_label(
                self.online_download_tab == OnlineDownloadTab::GpuDriver,
                "🎮 显卡驱动"
            ).clicked() {
                self.online_download_tab = OnlineDownloadTab::GpuDriver;
            }
        });
        
        ui.separator();
        ui.add_space(5.0);

        // 根据选项卡显示不同内容
        match self.online_download_tab {
            OnlineDownloadTab::SystemImage => self.show_system_image_tab(ui),
            OnlineDownloadTab::Software => self.show_software_download_tab(ui),
            OnlineDownloadTab::GpuDriver => self.show_gpu_driver_tab(ui),
        }
        
        // 软件下载模态框
        self.show_soft_download_modal(ui);
    }
    
    /// 显示系统镜像选项卡
    fn show_system_image_tab(&mut self, ui: &mut egui::Ui) {
        if self.config.is_none() || self.config.as_ref().map(|c| c.systems.is_empty()).unwrap_or(true) {
            if !self.remote_config_loading {
                ui.colored_label(egui::Color32::from_rgb(255, 165, 0), "未找到在线系统镜像资源");
                ui.label("服务器可能暂时不可用，请稍后重试");

                if ui.button("刷新配置").clicked() {
                    self.start_remote_config_loading();
                }
            }
            return;
        }

        // 克隆配置以避免借用冲突
        let systems: Vec<OnlineSystem> = self
            .config
            .as_ref()
            .map(|c| c.systems.clone())
            .unwrap_or_default();

        let mut system_to_download: Option<usize> = None;
        let mut system_to_install: Option<usize> = None;
        let mut system_selected: Option<usize> = None;

        egui::ScrollArea::vertical()
            .max_height(350.0)
            .id_salt("system_list")
            .show(ui, |ui| {
                egui::Grid::new("system_grid")
                    .striped(true)
                    .min_col_width(150.0)
                    .show(ui, |ui| {
                        ui.label("系统名称");
                        ui.label("类型");
                        ui.label("操作");
                        ui.end_row();

                        for (i, system) in systems.iter().enumerate() {
                            if ui
                                .selectable_label(
                                    self.selected_online_system == Some(i),
                                    &system.display_name,
                                )
                                .clicked()
                            {
                                system_selected = Some(i);
                            }

                            ui.label(if system.is_win11 { "Win11" } else { "Win10" });

                            ui.horizontal(|ui| {
                                if ui.button("下载").clicked() {
                                    system_to_download = Some(i);
                                }
                                if ui.button("安装").clicked() {
                                    system_to_install = Some(i);
                                }
                            });
                            ui.end_row();
                        }
                    });
            });

        // 处理选择
        if let Some(i) = system_selected {
            self.selected_online_system = Some(i);
        }

        // 处理下载
        if let Some(i) = system_to_download {
            if let Some(system) = systems.get(i) {
                self.pending_download_url = Some(system.download_url.clone());
                self.pending_download_filename = None;
                self.download_then_install = false;
                self.download_then_install_path = None;
                self.current_panel = crate::app::Panel::DownloadProgress;
            }
        }

        // 处理安装（下载后跳转到安装页面）
        if let Some(i) = system_to_install {
            if let Some(system) = systems.get(i) {
                // 从URL提取文件名
                let filename = system.download_url
                    .split('/')
                    .last()
                    .unwrap_or("system.iso")
                    .to_string();
                
                // 设置下载路径
                let save_path = if self.download_save_path.is_empty() {
                    crate::utils::path::get_exe_dir()
                        .join("downloads")
                        .to_string_lossy()
                        .to_string()
                } else {
                    self.download_save_path.clone()
                };
                
                // 计算完整的文件路径
                let full_path = std::path::Path::new(&save_path)
                    .join(&filename)
                    .to_string_lossy()
                    .to_string();
                
                self.pending_download_url = Some(system.download_url.clone());
                self.pending_download_filename = Some(filename);
                self.download_then_install = true;
                self.download_then_install_path = Some(full_path);
                self.current_panel = crate::app::Panel::DownloadProgress;
            }
        }

        ui.add_space(15.0);
        ui.separator();

        // 下载保存位置
        ui.horizontal(|ui| {
            ui.label("保存位置:");
            ui.add(
                egui::TextEdit::singleline(&mut self.download_save_path).desired_width(400.0),
            );
            if ui.button("浏览...").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.download_save_path = path.to_string_lossy().to_string();
                }
            }
        });

        // 刷新按钮
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.remote_config_loading, egui::Button::new("刷新在线资源")).clicked() {
                self.start_remote_config_loading();
            }
            if self.remote_config_loading {
                ui.spinner();
            }
        });
    }
    
    /// 显示软件下载选项卡
    fn show_software_download_tab(&mut self, ui: &mut egui::Ui) {
        // 提示信息
        ui.horizontal(|ui| {
            ui.label("ℹ");
            ui.label("本页面提供的软件均由互联网收集整理，仅供学习交流使用，请于下载后24小时内删除。");
        });
        ui.add_space(5.0);
        ui.separator();
        ui.add_space(5.0);
        
        // 检查软件列表
        let software_list: Vec<OnlineSoftware> = self
            .config
            .as_ref()
            .map(|c| c.software_list.clone())
            .unwrap_or_default();
        
        if software_list.is_empty() {
            if !self.remote_config_loading {
                ui.colored_label(egui::Color32::from_rgb(255, 165, 0), "未找到在线软件资源");
                ui.label("服务器可能暂未提供软件列表，请稍后重试");

                if ui.button("刷新配置").clicked() {
                    self.start_remote_config_loading();
                }
            }
            return;
        }
        
        // 收集需要加载的图标URL
        let mut icons_to_load: Vec<String> = Vec::new();
        for soft in &software_list {
            if let Some(ref icon_url) = soft.icon_url {
                if !icon_url.is_empty() 
                    && !self.soft_icon_cache.contains_key(icon_url)
                    && !self.soft_icon_loading.contains(icon_url) 
                {
                    icons_to_load.push(icon_url.clone());
                }
            }
        }
        
        // 启动图标加载任务
        for url in icons_to_load {
            self.start_icon_loading(url, ui.ctx());
        }
        
        let mut soft_to_download: Option<usize> = None;
        
        // 软件列表
        egui::ScrollArea::vertical()
            .max_height(400.0)
            .id_salt("software_list")
            .show(ui, |ui| {
                for (i, soft) in software_list.iter().enumerate() {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            // 图标区域：58x58，内部居中显示图标
                            ui.allocate_ui(egui::vec2(58.0, 58.0), |ui| {
                                ui.centered_and_justified(|ui| {
                                    self.show_soft_icon(ui, soft);
                                });
                            });
                            
                            ui.add_space(10.0);
                            
                            // 软件信息
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    ui.strong(&soft.name);
                                    ui.label(format!("| {}", soft.file_size));
                                });
                                ui.label(&soft.description);
                                ui.small(format!("更新日期: {}", soft.update_date));
                            });
                            
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("下载").clicked() {
                                    soft_to_download = Some(i);
                                }
                            });
                        });
                    });
                    ui.add_space(5.0);
                }
            });
        
        // 处理下载请求
        if let Some(i) = soft_to_download {
            if let Some(soft) = software_list.get(i) {
                // 选择合适的下载URL（根据系统架构）
                let download_url = self.get_appropriate_download_url(soft);
                
                // 设置待下载信息
                self.pending_soft_download = Some(PendingSoftDownload {
                    name: soft.name.clone(),
                    download_url,
                    filename: soft.filename.clone(),
                });
                
                // 初始化下载保存路径
                if self.soft_download_save_path.is_empty() {
                    self.soft_download_save_path = self.get_default_software_download_path();
                }
                
                // 显示下载模态框
                self.show_soft_download_modal = true;
            }
        }
        
        // 刷新按钮
        ui.add_space(10.0);
        ui.separator();
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.remote_config_loading, egui::Button::new("刷新在线资源")).clicked() {
                self.start_remote_config_loading();
            }
            if self.remote_config_loading {
                ui.spinner();
            }
        });
    }
    
    /// 显示软件图标
    fn show_soft_icon(&mut self, ui: &mut egui::Ui, soft: &OnlineSoftware) {
        let icon_size = egui::vec2(48.0, 48.0);
        
        if let Some(ref icon_url) = soft.icon_url {
            if !icon_url.is_empty() {
                if let Some(state) = self.soft_icon_cache.get(icon_url) {
                    match state {
                        SoftIconState::Loaded(texture) => {
                            ui.add_sized(icon_size, egui::Image::new(texture).fit_to_exact_size(icon_size));
                            return;
                        }
                        SoftIconState::Loading => {
                            // 显示加载中的占位符
                            ui.add_sized(icon_size, egui::Spinner::new());
                            return;
                        }
                        SoftIconState::Failed => {
                            // 加载失败，显示默认图标
                        }
                    }
                } else if self.soft_icon_loading.contains(icon_url) {
                    // 正在加载中
                    ui.add_sized(icon_size, egui::Spinner::new());
                    return;
                }
            }
        }
        
        // 默认图标
        ui.add_sized(icon_size, egui::Label::new(
            egui::RichText::new("📦").size(32.0)
        ));
    }
    
    /// 开始异步加载图标
    fn start_icon_loading(&mut self, url: String, ctx: &egui::Context) {
        if self.soft_icon_loading.contains(&url) {
            return;
        }
        
        self.soft_icon_loading.insert(url.clone());
        
        let ctx = ctx.clone();
        let url_clone = url.clone();
        
        std::thread::spawn(move || {
            let result = Self::download_icon(&url_clone);
            
            // 使用ctx.request_repaint通知UI更新
            ctx.request_repaint();
            
            // Pass results via a static queue (simplified).
            let mut results = ICON_LOAD_RESULTS.lock().unwrap_or_else(|e| e.into_inner());
            results.push(IconLoadResult {
                url: url_clone,
                data: result,
            });
        });
    }
    
    /// 下载图标数据
    fn download_icon(url: &str) -> Option<Vec<u8>> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .ok()?;
        
        let response = client.get(url).send().ok()?;
        
        if !response.status().is_success() {
            return None;
        }
        
        response.bytes().ok().map(|b| b.to_vec())
    }
    
    /// 处理图标加载结果（在UI更新时调用）
    pub fn process_icon_load_results(&mut self, ctx: &egui::Context) {
        let results: Vec<IconLoadResult> = {
            let mut results = ICON_LOAD_RESULTS.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *results)
        };
        
        for result in results {
            self.soft_icon_loading.remove(&result.url);
            
            if let Some(data) = result.data {
                // 尝试解码图片
                if let Ok(image) = image::load_from_memory(&data) {
                    let rgba = image.to_rgba8();
                    let size = [rgba.width() as usize, rgba.height() as usize];
                    let pixels = rgba.into_raw();
                    
                    let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
                    let texture = ctx.load_texture(
                        &result.url,
                        color_image,
                        egui::TextureOptions::LINEAR,
                    );
                    
                    self.soft_icon_cache.insert(result.url, SoftIconState::Loaded(texture));
                } else {
                    self.soft_icon_cache.insert(result.url, SoftIconState::Failed);
                }
            } else {
                self.soft_icon_cache.insert(result.url, SoftIconState::Failed);
            }
        }
    }
    
    /// 获取合适的下载URL（根据系统架构）
    fn get_appropriate_download_url(&self, soft: &OnlineSoftware) -> String {
        let is_64bit = cfg!(target_arch = "x86_64");
        
        if is_64bit {
            soft.download_url.clone()
        } else {
            soft.download_url_x86.clone().unwrap_or_else(|| soft.download_url.clone())
        }
    }
    
    /// 获取默认的软件下载路径
    fn get_default_software_download_path(&self) -> String {
        let is_pe = self.system_info.as_ref().map(|s| s.is_pe_environment).unwrap_or(false);
        
        if is_pe {
            // PE环境下的默认路径逻辑
            self.get_pe_default_download_path()
        } else {
            // 正常系统下使用用户的Downloads目录
            dirs::download_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "C:\\".to_string())
        }
    }
    
    /// 获取PE环境下的默认下载路径
    fn get_pe_default_download_path(&self) -> String {
        // 统计有Windows的分区
        let windows_partitions: Vec<&crate::core::disk::Partition> = self.partitions
            .iter()
            .filter(|p| p.has_windows)
            .collect();
        
        if windows_partitions.len() == 1 {
            // 只有一个Windows分区
            let partition = windows_partitions[0];
            let users_path = format!("{}\\Users", partition.letter);
            
            if let Ok(entries) = std::fs::read_dir(&users_path) {
                let user_dirs: Vec<String> = entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        if let Ok(ft) = e.file_type() {
                            if ft.is_dir() {
                                let name = e.file_name().to_string_lossy().to_lowercase();
                                // 排除系统目录
                                return !matches!(name.as_str(), "public" | "default" | "default user" | "all users");
                            }
                        }
                        false
                    })
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect();
                
                if user_dirs.len() == 1 {
                    // 只有一个用户目录
                    return format!("{}\\Users\\{}\\Downloads", partition.letter, user_dirs[0]);
                } else {
                    // 多个用户目录，使用OSDownload
                    let os_download_path = format!("{}\\OSDownload", partition.letter);
                    let _ = std::fs::create_dir_all(&os_download_path);
                    return os_download_path;
                }
            }
            
            // 默认使用OSDownload
            let os_download_path = format!("{}\\OSDownload", partition.letter);
            let _ = std::fs::create_dir_all(&os_download_path);
            os_download_path
        } else if windows_partitions.is_empty() {
            // 没有Windows分区，返回空让用户选择
            String::new()
        } else {
            // 多个Windows分区，返回空让用户选择
            String::new()
        }
    }
    
    /// 显示软件下载模态框
    fn show_soft_download_modal(&mut self, ui: &mut egui::Ui) {
        if !self.show_soft_download_modal {
            return;
        }
        
        let pending = self.pending_soft_download.clone();
        if pending.is_none() {
            self.show_soft_download_modal = false;
            return;
        }
        let pending = pending.unwrap();
        
        let is_pe = self.system_info.as_ref().map(|s| s.is_pe_environment).unwrap_or(false);
        
        egui::Window::new(format!("下载 - {}", pending.name))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .min_width(450.0)
            .show(ui.ctx(), |ui| {
                ui.add_space(10.0);
                
                // 保存目录
                ui.horizontal(|ui| {
                    ui.label("保存目录:");
                });
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.soft_download_save_path)
                            .desired_width(350.0)
                    );
                    if ui.button("浏览...").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.soft_download_save_path = path.to_string_lossy().to_string();
                        }
                    }
                });
                
                // 路径为空时提示
                if self.soft_download_save_path.is_empty() {
                    ui.colored_label(egui::Color32::RED, "请选择下载保存目录");
                }
                
                ui.add_space(10.0);
                
                // 非PE环境下显示"下载后运行"选项
                if !is_pe {
                    ui.checkbox(&mut self.soft_download_run_after, "下载后运行软件");
                }
                
                ui.add_space(15.0);
                ui.separator();
                ui.add_space(10.0);
                
                // 按钮
                ui.horizontal(|ui| {
                    let can_download = !self.soft_download_save_path.is_empty();
                    
                    if ui.add_enabled(can_download, egui::Button::new("开始下载")).clicked() {
                        // 创建保存目录
                        let _ = std::fs::create_dir_all(&self.soft_download_save_path);
                        
                        // 设置下载任务
                        self.pending_download_url = Some(pending.download_url.clone());
                        self.pending_download_filename = Some(pending.filename.clone());
                        self.download_save_path = self.soft_download_save_path.clone();
                        self.download_then_install = false;
                        self.download_then_install_path = None;
                        
                        // 如果需要下载后运行
                        if !is_pe && self.soft_download_run_after {
                            let full_path = Path::new(&self.soft_download_save_path)
                                .join(&pending.filename)
                                .to_string_lossy()
                                .to_string();
                            self.soft_download_then_run = true;
                            self.soft_download_then_run_path = Some(full_path);
                        } else {
                            self.soft_download_then_run = false;
                            self.soft_download_then_run_path = None;
                        }
                        
                        // 关闭模态框并跳转到下载页面
                        self.show_soft_download_modal = false;
                        self.pending_soft_download = None;
                        self.current_panel = crate::app::Panel::DownloadProgress;
                    }
                    
                    if ui.button("取消").clicked() {
                        self.show_soft_download_modal = false;
                        self.pending_soft_download = None;
                    }
                });
                
                ui.add_space(10.0);
            });
    }
    
    /// 显示GPU驱动下载选项卡
    fn show_gpu_driver_tab(&mut self, ui: &mut egui::Ui) {
        // 显示本机显卡信息
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.strong("🖥️ 本机显卡信息");
            });
            ui.separator();
            
            if let Some(ref hw_info) = self.hardware_info {
                if hw_info.gpus.is_empty() {
                    ui.colored_label(egui::Color32::from_rgb(255, 165, 0), "未检测到显卡");
                } else {
                    for (i, gpu) in hw_info.gpus.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(format!("显卡 {}:", i + 1));
                            ui.strong(crate::core::hardware_info::beautify_gpu_name(&gpu.name));
                        });
                        
                        if !gpu.current_resolution.is_empty() {
                            ui.horizontal(|ui| {
                                ui.add_space(55.0);
                                ui.label(format!("分辨率: {} @ {}Hz", gpu.current_resolution, gpu.refresh_rate));
                            });
                        }
                        
                        if !gpu.driver_version.is_empty() {
                            ui.horizontal(|ui| {
                                ui.add_space(55.0);
                                ui.label(format!("驱动版本: {}", gpu.driver_version));
                            });
                        }
                        
                        if i < hw_info.gpus.len() - 1 {
                            ui.add_space(5.0);
                        }
                    }
                }
            } else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("正在检测显卡信息...");
                });
            }
        });
        
        ui.add_space(10.0);
        
        // 提示信息
        ui.horizontal(|ui| {
            ui.label("ℹ");
            ui.label("请根据您的显卡型号选择合适的驱动程序下载。");
        });
        ui.add_space(5.0);
        ui.separator();
        ui.add_space(5.0);
        
        // 检查GPU驱动列表
        let gpu_driver_list: Vec<OnlineGpuDriver> = self
            .config
            .as_ref()
            .map(|c| c.gpu_driver_list.clone())
            .unwrap_or_default();
        
        if gpu_driver_list.is_empty() {
            if !self.remote_config_loading {
                ui.colored_label(egui::Color32::from_rgb(255, 165, 0), "未找到在线显卡驱动资源");
                ui.label("服务器可能暂未提供显卡驱动列表，请稍后重试");

                if ui.button("刷新配置").clicked() {
                    self.start_remote_config_loading();
                }
            }
            return;
        }
        
        // 收集需要加载的图标URL
        let mut icons_to_load: Vec<String> = Vec::new();
        for driver in &gpu_driver_list {
            if let Some(ref icon_url) = driver.icon_url {
                if !icon_url.is_empty() 
                    && !self.soft_icon_cache.contains_key(icon_url)
                    && !self.soft_icon_loading.contains(icon_url) 
                {
                    icons_to_load.push(icon_url.clone());
                }
            }
        }
        
        // 启动图标加载任务
        for url in icons_to_load {
            self.start_icon_loading(url, ui.ctx());
        }
        
        let mut driver_to_download: Option<usize> = None;
        
        // 驱动列表
        egui::ScrollArea::vertical()
            .max_height(340.0)
            .id_salt("gpu_driver_list")
            .show(ui, |ui| {
                for (i, driver) in gpu_driver_list.iter().enumerate() {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            // 图标区域：58x58，内部居中显示图标
                            ui.allocate_ui(egui::vec2(58.0, 58.0), |ui| {
                                ui.centered_and_justified(|ui| {
                                    self.show_gpu_driver_icon(ui, driver);
                                });
                            });
                            
                            ui.add_space(10.0);
                            
                            // 驱动信息
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    ui.strong(&driver.name);
                                    ui.label(format!("| {}", driver.file_size));
                                });
                                ui.label(&driver.description);
                                ui.small(format!("更新日期: {}", driver.update_date));
                            });
                            
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("下载").clicked() {
                                    driver_to_download = Some(i);
                                }
                            });
                        });
                    });
                    ui.add_space(5.0);
                }
            });
        
        // 处理下载请求
        if let Some(i) = driver_to_download {
            if let Some(driver) = gpu_driver_list.get(i) {
                // 设置待下载信息
                self.pending_soft_download = Some(PendingSoftDownload {
                    name: driver.name.clone(),
                    download_url: driver.download_url.clone(),
                    filename: driver.filename.clone(),
                });
                
                // 初始化下载保存路径
                if self.soft_download_save_path.is_empty() {
                    self.soft_download_save_path = self.get_default_software_download_path();
                }
                
                // 显示下载模态框
                self.show_soft_download_modal = true;
            }
        }
        
        // 刷新按钮
        ui.add_space(10.0);
        ui.separator();
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.remote_config_loading, egui::Button::new("刷新在线资源")).clicked() {
                self.start_remote_config_loading();
            }
            if self.remote_config_loading {
                ui.spinner();
            }
        });
    }
    
    /// 显示GPU驱动图标
    fn show_gpu_driver_icon(&mut self, ui: &mut egui::Ui, driver: &OnlineGpuDriver) {
        let icon_size = egui::vec2(48.0, 48.0);
        
        if let Some(ref icon_url) = driver.icon_url {
            if !icon_url.is_empty() {
                if let Some(state) = self.soft_icon_cache.get(icon_url) {
                    match state {
                        SoftIconState::Loaded(texture) => {
                            ui.add_sized(icon_size, egui::Image::new(texture).fit_to_exact_size(icon_size));
                            return;
                        }
                        SoftIconState::Loading => {
                            // 显示加载中的占位符
                            ui.add_sized(icon_size, egui::Spinner::new());
                            return;
                        }
                        SoftIconState::Failed => {
                            // 加载失败，显示默认图标
                        }
                    }
                } else if self.soft_icon_loading.contains(icon_url) {
                    // 正在加载中
                    ui.add_sized(icon_size, egui::Spinner::new());
                    return;
                }
            }
        }
        
        // 默认图标 - 使用显卡图标
        ui.add_sized(icon_size, egui::Label::new(
            egui::RichText::new("🎮").size(32.0)
        ));
    }

    pub fn load_online_config(&mut self) {
        self.start_remote_config_loading();
    }
}

// 静态变量存储图标加载结果
static ICON_LOAD_RESULTS: Mutex<Vec<IconLoadResult>> = Mutex::new(Vec::new());
