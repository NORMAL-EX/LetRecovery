//! 一键分区UI模块
//!
//! 提供可视化的分区规划和编辑界面

use std::sync::mpsc;

use crate::tr;
use crate::app::App;
use crate::core::disk::PartitionStyle;
use crate::core::quick_partition::{
    execute_quick_partition, get_next_available_drive_letter, get_physical_disks,
    get_recommended_partition_style, get_unallocated_space_after_partition_with_disk,
    get_used_drive_letters, resize_existing_partition, PartitionLayout, PhysicalDisk,
    ResizePartitionResult,
};

/// 分区编辑器状态
#[derive(Debug, Clone)]
pub struct PartitionEditorState {
    /// 当前选中的磁盘索引
    pub selected_disk_index: Option<usize>,
    /// 分区布局列表
    pub partition_layouts: Vec<EditablePartition>,
    /// 选择的分区表类型
    pub partition_style: PartitionStyle,
    /// 是否显示 ESP 分区创建按钮
    pub show_esp_button: bool,
    /// 当前正在编辑大小的分区索引
    pub editing_size_index: Option<usize>,
    /// 编辑中的大小文本
    pub editing_size_text: String,
    /// 悬停的分区索引
    pub hovered_partition_index: Option<usize>,
    /// 正在拖动的分隔线索引
    pub dragging_divider_index: Option<usize>,
    /// 拖动起始X位置
    pub drag_start_x: f32,
    /// 拖动起始时的分区大小（GB）
    pub drag_start_sizes: (f64, f64),
    /// 右键菜单目标分区索引
    pub context_menu_partition: Option<usize>,
    /// 是否显示调整大小对话框
    pub show_resize_dialog: bool,
    /// 调整大小对话框目标分区索引
    pub resize_partition_index: Option<usize>,
    /// 调整大小对话框中的新大小文本
    pub resize_size_text: String,
    /// 是否正在执行调整已有分区大小操作
    pub resizing_existing: bool,
    /// 调整已有分区的目标索引
    pub resize_existing_index: Option<usize>,
    /// 调整已有分区大小的最小值（GB）
    pub resize_existing_min_gb: f64,
    /// 调整已有分区大小的最大值（GB）
    pub resize_existing_max_gb: f64,
}

impl Default for PartitionEditorState {
    fn default() -> Self {
        Self {
            selected_disk_index: None,
            partition_layouts: Vec::new(),
            partition_style: PartitionStyle::GPT,
            show_esp_button: true,
            editing_size_index: None,
            editing_size_text: String::new(),
            hovered_partition_index: None,
            dragging_divider_index: None,
            drag_start_x: 0.0,
            drag_start_sizes: (0.0, 0.0),
            context_menu_partition: None,
            show_resize_dialog: false,
            resize_partition_index: None,
            resize_size_text: String::new(),
            resizing_existing: false,
            resize_existing_index: None,
            resize_existing_min_gb: 0.0,
            resize_existing_max_gb: 0.0,
        }
    }
}

/// 可编辑的分区信息
#[derive(Debug, Clone)]
pub struct EditablePartition {
    /// 分区大小（GB）
    pub size_gb: f64,
    /// 盘符
    pub drive_letter: Option<char>,
    /// 卷标
    pub label: String,
    /// 是否为 ESP 分区
    pub is_esp: bool,
    /// 是否为 MSR 分区
    pub is_msr: bool,
    /// 是否为恢复分区
    pub is_recovery: bool,
    /// 文件系统类型
    pub file_system: String,
    /// 唯一标识符
    pub id: u32,
    /// 是否为已存在的分区（true=已有分区，false=新规划的分区）
    pub is_existing: bool,
    /// 分区编号（仅已有分区）
    pub partition_number: Option<u32>,
    /// 已使用空间（GB）
    pub used_gb: f64,
    /// 空闲空间（GB）
    pub free_gb: f64,
    /// 磁盘编号（仅已有分区）
    pub disk_number: Option<u32>,
}

impl EditablePartition {
    /// 创建新规划的分区
    fn new(id: u32, size_gb: f64, letter: Option<char>) -> Self {
        Self {
            size_gb,
            drive_letter: letter,
            label: String::new(),
            is_esp: false,
            is_msr: false,
            is_recovery: false,
            file_system: "NTFS".to_string(),
            id,
            is_existing: false,
            partition_number: None,
            used_gb: 0.0,
            free_gb: size_gb,
            disk_number: None,
        }
    }

    /// 创建新规划的 ESP 分区
    fn new_esp(id: u32, size_gb: f64) -> Self {
        Self {
            size_gb,
            drive_letter: None,
            label: "EFI".to_string(),
            is_esp: true,
            is_msr: false,
            is_recovery: false,
            file_system: "FAT32".to_string(),
            id,
            is_existing: false,
            partition_number: None,
            used_gb: 0.0,
            free_gb: size_gb,
            disk_number: None,
        }
    }
    
    /// 从已有分区创建
    fn from_existing(id: u32, partition: &crate::core::quick_partition::DiskPartitionInfo, disk_number: u32) -> Self {
        Self {
            size_gb: partition.size_gb(),
            drive_letter: partition.drive_letter,
            label: partition.label.clone(),
            is_esp: partition.is_esp,
            is_msr: partition.is_msr,
            is_recovery: partition.is_recovery,
            file_system: partition.file_system.clone(),
            id,
            is_existing: true,
            partition_number: Some(partition.partition_number),
            used_gb: partition.used_gb(),
            free_gb: partition.free_gb(),
            disk_number: Some(disk_number),
        }
    }

    /// 转换为 PartitionLayout
    fn to_layout(&self) -> PartitionLayout {
        PartitionLayout {
            size_gb: self.size_gb,
            drive_letter: self.drive_letter,
            label: self.label.clone(),
            is_esp: self.is_esp,
            file_system: self.file_system.clone(),
        }
    }
    
    /// 获取显示名称
    fn display_name(&self) -> String {
        if self.is_esp {
            "ESP".to_string()
        } else if self.is_msr {
            "MSR".to_string()
        } else if self.is_recovery {
            tr!("恢复分区")
        } else if let Some(letter) = self.drive_letter {
            format!("{}:", letter)
        } else {
            tr!("未分配盘符")
        }
    }
    
    /// 检查是否可以调整大小
    fn can_resize(&self) -> (bool, String) {
        if !self.is_existing {
            return (true, tr!("新规划的分区可以自由调整"));
        }

        if self.is_esp {
            return (false, tr!("ESP分区不支持调整大小"));
        }
        if self.is_msr {
            return (false, tr!("MSR分区不支持调整大小"));
        }
        if self.is_recovery {
            return (false, tr!("恢复分区不支持调整大小"));
        }
        if self.drive_letter.is_none() {
            return (false, tr!("分区没有盘符，无法调整大小"));
        }
        
        // 检查是否是当前系统盘
        let system_drive = std::env::var("SystemDrive")
            .unwrap_or_else(|_| "C:".to_string())
            .chars()
            .next()
            .unwrap_or('C');
            
        if self.drive_letter == Some(system_drive) {
            return (false, tr!("无法调整当前系统分区大小"));
        }

        (true, tr!("已用: {} GB / {} GB", format!("{:.1}", self.used_gb), format!("{:.1}", self.size_gb)))
    }
    
    /// 获取最小可调整大小（GB）
    fn min_resize_gb(&self) -> f64 {
        if self.is_existing {
            // 已有分区：最小大小 = 已用空间 + 0.1GB 余量
            (self.used_gb + 0.1).max(0.5)
        } else {
            // 新分区：最小 0.5GB
            0.5
        }
    }
}

/// 一键分区对话框的完整状态
#[derive(Debug, Clone, Default)]
pub struct QuickPartitionDialogState {
    /// 物理磁盘列表
    pub physical_disks: Vec<PhysicalDisk>,
    /// 分区编辑器状态
    pub editor: PartitionEditorState,
    /// 是否正在加载磁盘列表
    pub loading: bool,
    /// 是否正在执行分区
    pub executing: bool,
    /// 状态消息
    pub message: String,
    /// 分区ID计数器
    pub partition_id_counter: u32,
    /// 确认对话框是否显示
    pub show_confirm_dialog: bool,
}

impl App {
    /// 初始化一键分区对话框
    pub fn init_quick_partition_dialog(&mut self) {
        self.show_quick_partition_dialog = true;
        self.quick_partition_state.message.clear();
        self.quick_partition_state.loading = true;
        self.quick_partition_state.executing = false;
        self.quick_partition_state.editor = PartitionEditorState::default();
        self.quick_partition_state.show_confirm_dialog = false;

        // 设置默认分区表类型
        if let Some(info) = &self.system_info {
            self.quick_partition_state.editor.partition_style =
                get_recommended_partition_style(&info.boot_mode);
            self.quick_partition_state.editor.show_esp_button =
                self.quick_partition_state.editor.partition_style == PartitionStyle::GPT;
        }

        // 启动后台加载磁盘列表
        self.start_load_physical_disks();
    }

    /// 启动后台加载物理磁盘列表
    pub fn start_load_physical_disks(&mut self) {
        let (tx, rx) = mpsc::channel();
        self.quick_partition_disks_rx = Some(rx);

        std::thread::spawn(move || {
            let disks = get_physical_disks();
            let _ = tx.send(disks);
        });
    }

    /// 检查磁盘列表加载结果
    pub fn check_quick_partition_disk_load(&mut self) {
        if let Some(ref rx) = self.quick_partition_disks_rx {
            if let Ok(disks) = rx.try_recv() {
                self.quick_partition_state.physical_disks = disks;
                self.quick_partition_state.loading = false;
                self.quick_partition_disks_rx = None;

                // 如果只有一个磁盘，自动选择它
                if self.quick_partition_state.physical_disks.len() == 1 {
                    self.select_disk_for_partition(0);
                }
            }
        }

        // 检查分区执行结果
        if let Some(ref rx) = self.quick_partition_result_rx {
            if let Ok(result) = rx.try_recv() {
                self.quick_partition_state.executing = false;
                self.quick_partition_result_rx = None;

                if result.success {
                    self.quick_partition_state.message = tr!(
                        "分区成功！已创建分区: {}",
                        result.created_partitions.join(", ")
                    );
                    // 刷新磁盘列表
                    self.quick_partition_state.loading = true;
                    self.start_load_physical_disks();
                    // 刷新主分区列表
                    self.partitions = crate::core::disk::DiskManager::get_partitions().unwrap_or_default();
                } else {
                    self.quick_partition_state.message = tr!("分区失败: {}", result.message);
                }
            }
        }
        
        // 检查调整已有分区大小的结果
        self.check_resize_existing_result();
    }

    /// 选择要分区的磁盘
    fn select_disk_for_partition(&mut self, index: usize) {
        self.quick_partition_state.editor.selected_disk_index = Some(index);

        if let Some(disk) = self.quick_partition_state.physical_disks.get(index).cloned() {
            // 设置分区表类型
            if disk.is_initialized {
                self.quick_partition_state.editor.partition_style = disk.partition_style;
            } else {
                // 未初始化的磁盘，根据启动模式设置
                if let Some(info) = &self.system_info {
                    self.quick_partition_state.editor.partition_style =
                        get_recommended_partition_style(&info.boot_mode);
                }
            }

            // 更新 ESP 按钮显示
            self.quick_partition_state.editor.show_esp_button =
                self.quick_partition_state.editor.partition_style == PartitionStyle::GPT;

            // 加载该磁盘上已有的分区
            self.quick_partition_state.editor.partition_layouts.clear();
            self.quick_partition_state.partition_id_counter = 0;

            for partition in &disk.partitions {
                self.quick_partition_state.partition_id_counter += 1;
                self.quick_partition_state
                    .editor
                    .partition_layouts
                    .push(EditablePartition::from_existing(
                        self.quick_partition_state.partition_id_counter,
                        partition,
                        disk.disk_number,
                    ));
            }
        }
    }

    /// 添加新分区
    fn add_new_partition(&mut self) {
        // 获取当前选中的磁盘
        let disk_idx = match self.quick_partition_state.editor.selected_disk_index {
            Some(idx) => idx,
            None => return,
        };
        
        let disk = match self.quick_partition_state.physical_disks.get(disk_idx).cloned() {
            Some(d) => d,
            None => return,
        };
        
        let layouts = &mut self.quick_partition_state.editor.partition_layouts;
        
        // 计算已规划的总空间
        let planned_total: f64 = layouts.iter().map(|p| p.size_gb).sum();
        let disk_total = disk.size_gb();
        let unallocated = disk_total - planned_total;
        
        // 获取新盘符
        let mut used_letters: Vec<char> = layouts
            .iter()
            .filter_map(|p| p.drive_letter)
            .collect();
        used_letters.extend(get_used_drive_letters());
        let new_letter = get_next_available_drive_letter(&used_letters);
        
        // 如果有未分配空间（超过1GB），直接使用
        if unallocated >= 1.0 {
            self.quick_partition_state.partition_id_counter += 1;
            layouts.push(EditablePartition::new(
                self.quick_partition_state.partition_id_counter,
                (unallocated * 10.0).round() / 10.0, // 四舍五入到0.1GB
                new_letter,
            ));
            return;
        }
        
        // 如果没有足够的未分配空间，从最后一个非系统分区分割
        // 找到最后一个可分割的分区（非ESP、非MSR、非恢复分区，且是新规划的分区）
        let splittable_idx = layouts.iter().rposition(|p| {
            !p.is_esp && !p.is_msr && !p.is_recovery && !p.is_existing && p.size_gb >= 2.0
        });
        
        if let Some(idx) = splittable_idx {
            let last_size = layouts[idx].size_gb;
            let new_size = ((last_size / 5.0) * 10.0).floor() / 10.0; // 取整到0.1GB
            
            if new_size >= 1.0 {
                // 调整被分割分区的大小
                layouts[idx].size_gb = ((last_size - new_size) * 10.0).round() / 10.0;
                
                // 创建新分区
                self.quick_partition_state.partition_id_counter += 1;
                layouts.push(EditablePartition::new(
                    self.quick_partition_state.partition_id_counter,
                    new_size,
                    new_letter,
                ));
                return;
            }
        }
        
        self.quick_partition_state.message = tr!("无法创建新分区：没有足够的可用空间");
    }

    /// 添加 ESP 分区
    fn add_esp_partition(&mut self) {
        let layouts = &mut self.quick_partition_state.editor.partition_layouts;

        // 检查是否已有 ESP 分区
        if layouts.iter().any(|p| p.is_esp) {
            self.quick_partition_state.message = tr!("已存在 ESP 分区");
            return;
        }

        // ESP 分区大小固定为 500MB = 0.5GB
        let esp_size = 0.5;
        
        // 获取当前选中的磁盘
        let disk_idx = match self.quick_partition_state.editor.selected_disk_index {
            Some(idx) => idx,
            None => return,
        };
        
        let disk = match self.quick_partition_state.physical_disks.get(disk_idx).cloned() {
            Some(d) => d,
            None => return,
        };
        
        // 计算已规划的总空间
        let planned_total: f64 = layouts.iter().map(|p| p.size_gb).sum();
        let disk_total = disk.size_gb();
        let unallocated = disk_total - planned_total;
        
        // 如果有足够的未分配空间
        if unallocated >= esp_size {
            // 创建 ESP 分区并插入到开头
            self.quick_partition_state.partition_id_counter += 1;
            let esp = EditablePartition::new_esp(
                self.quick_partition_state.partition_id_counter,
                esp_size,
            );
            layouts.insert(0, esp);
            return;
        }

        // 否则从第一个非系统分区、新规划的分区中减去空间
        if let Some(first_data_idx) = layouts.iter().position(|p| {
            !p.is_esp && !p.is_msr && !p.is_recovery && !p.is_existing && p.size_gb > esp_size + 1.0
        }) {
            layouts[first_data_idx].size_gb -= esp_size;
            
            // 创建 ESP 分区并插入到开头
            self.quick_partition_state.partition_id_counter += 1;
            let esp = EditablePartition::new_esp(
                self.quick_partition_state.partition_id_counter,
                esp_size,
            );
            layouts.insert(0, esp);
            return;
        }
        
        self.quick_partition_state.message = tr!("无法创建 ESP 分区：没有足够的可用空间");
    }

    /// 删除指定分区
    fn delete_partition(&mut self, index: usize) {
        let layouts = &mut self.quick_partition_state.editor.partition_layouts;

        if index >= layouts.len() {
            return;
        }
        
        // 只允许删除新规划的分区，不能删除已有分区
        if layouts[index].is_existing {
            self.quick_partition_state.message = tr!("无法删除已有分区，一键分区会清除整个磁盘");
            return;
        }

        // 删除分区，空间会自动变为未分配
        layouts.remove(index);
    }

    /// 执行一键分区
    fn execute_quick_partition(&mut self) {
        let state = &self.quick_partition_state;

        let disk_index = match state.editor.selected_disk_index {
            Some(idx) => idx,
            None => {
                self.quick_partition_state.message = tr!("请先选择要分区的磁盘");
                return;
            }
        };

        let disk = match state.physical_disks.get(disk_index) {
            Some(d) => d.clone(),
            None => {
                self.quick_partition_state.message = tr!("无效的磁盘选择");
                return;
            }
        };

        // 只获取新规划的分区（排除已有分区）
        let new_partitions: Vec<&EditablePartition> = state
            .editor
            .partition_layouts
            .iter()
            .filter(|p| !p.is_existing)
            .collect();
            
        if new_partitions.is_empty() {
            self.quick_partition_state.message = tr!("请至少添加一个新分区");
            return;
        }

        // 转换分区布局
        let layouts: Vec<PartitionLayout> = new_partitions
            .iter()
            .map(|p| p.to_layout())
            .collect();

        let partition_style = state.editor.partition_style;
        let disk_number = disk.disk_number;

        self.quick_partition_state.executing = true;
        self.quick_partition_state.show_confirm_dialog = false;
        self.quick_partition_state.message = tr!("正在执行分区操作...");

        let (tx, rx) = mpsc::channel();
        self.quick_partition_result_rx = Some(rx);

        std::thread::spawn(move || {
            let result = execute_quick_partition(disk_number, partition_style, &layouts);
            let _ = tx.send(result);
        });
    }

    /// 渲染一键分区对话框
    pub fn render_quick_partition_dialog(&mut self, ui: &mut egui::Ui) {
        use egui;

        if !self.show_quick_partition_dialog {
            return;
        }

        // 检查异步操作
        self.check_quick_partition_disk_load();

        // 使用延迟操作模式来避免借用冲突
        let mut should_close = false;
        let mut should_add_partition = false;
        let mut should_add_esp = false;
        let mut should_delete_partition: Option<usize> = None;
        let mut should_execute = false;
        let mut should_show_confirm = false;
        let mut should_select_disk: Option<usize> = None;
        let mut should_refresh = false;
        let mut should_show_resize_dialog: Option<usize> = None;
        let mut should_show_resize_existing_dialog: Option<usize> = None;
        let mut should_execute_resize_existing = false;
        
        // 使用局部变量控制窗口开关，避免借用冲突
        let mut window_open = self.show_quick_partition_dialog;

        egui::Window::new(tr!("一键分区"))
            .open(&mut window_open)
            .resizable(true)
            .default_width(700.0)
            .min_width(600.0)
            .default_height(500.0)
            .show(ui.ctx(), |ui| {
                // 加载中
                if self.quick_partition_state.loading {
                    ui.vertical_centered(|ui| {
                        ui.add_space(50.0);
                        ui.spinner();
                        ui.label(tr!("正在加载磁盘列表..."));
                    });
                    return;
                }

                // 执行中
                if self.quick_partition_state.executing {
                    ui.vertical_centered(|ui| {
                        ui.add_space(50.0);
                        ui.spinner();
                        ui.label(tr!("正在执行分区操作，请勿中断..."));
                    });
                    return;
                }

                // 检查是否有磁盘
                if self.quick_partition_state.physical_disks.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(50.0);
                        ui.colored_label(egui::Color32::RED, tr!("未检测到可用磁盘"));
                        ui.add_space(20.0);
                        if ui.button(tr!("刷新")).clicked() {
                            should_refresh = true;
                        }
                    });
                    return;
                }

                ui.vertical(|ui| {
                    // 磁盘选择
                    ui.horizontal(|ui| {
                        ui.label(tr!("选择磁盘:"));

                        let selected_text = self.quick_partition_state.editor.selected_disk_index
                            .and_then(|idx| self.quick_partition_state.physical_disks.get(idx))
                            .map(|d| d.display_name())
                            .unwrap_or_else(|| tr!("请选择..."));

                        // 先克隆磁盘列表用于显示
                        let disks_for_display: Vec<(usize, String)> = self.quick_partition_state.physical_disks
                            .iter()
                            .enumerate()
                            .map(|(idx, d)| (idx, d.display_name()))
                            .collect();
                        
                        let current_selection = self.quick_partition_state.editor.selected_disk_index;

                        egui::ComboBox::from_id_salt("disk_select")
                            .width(400.0)
                            .selected_text(selected_text)
                            .show_ui(ui, |ui| {
                                for (idx, display_name) in &disks_for_display {
                                    let is_selected = current_selection == Some(*idx);
                                    if ui.selectable_label(is_selected, display_name).clicked() {
                                        should_select_disk = Some(*idx);
                                    }
                                }
                            });

                        if ui.button(tr!("刷新")).clicked() {
                            should_refresh = true;
                        }
                    });

                    ui.add_space(10.0);

                    // 只有选择了磁盘才显示分区编辑器
                    if let Some(disk_idx) = self.quick_partition_state.editor.selected_disk_index {
                        if let Some(disk) = self.quick_partition_state.physical_disks.get(disk_idx).cloned() {
                            // 分区表类型选择
                            ui.horizontal(|ui| {
                                ui.label(tr!("分区表类型:"));

                                let mut style = self.quick_partition_state.editor.partition_style;
                                
                                if ui.radio_value(&mut style, PartitionStyle::MBR, "MBR").clicked() {
                                    self.quick_partition_state.editor.partition_style = PartitionStyle::MBR;
                                    self.quick_partition_state.editor.show_esp_button = false;
                                    // 删除 ESP 分区（如果有）
                                    self.quick_partition_state.editor.partition_layouts.retain(|p| !p.is_esp);
                                }
                                
                                if ui.radio_value(&mut style, PartitionStyle::GPT, "GPT (GUID)").clicked() {
                                    self.quick_partition_state.editor.partition_style = PartitionStyle::GPT;
                                    self.quick_partition_state.editor.show_esp_button = true;
                                }

                                if disk.is_initialized {
                                    ui.label(tr!("(当前: {})", disk.partition_style));
                                } else {
                                    if let Some(info) = &self.system_info {
                                        let recommended = get_recommended_partition_style(&info.boot_mode);
                                        ui.label(tr!("(推荐: {}，基于{}启动模式)", recommended, info.boot_mode));
                                    }
                                }
                            });

                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);

                            // 工具栏
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(&disk.display_name()).strong());
                                ui.add_space(20.0);

                                if ui.button(tr!("添加分区")).clicked() {
                                    should_add_partition = true;
                                }

                                if self.quick_partition_state.editor.show_esp_button {
                                    let has_esp = self.quick_partition_state.editor.partition_layouts.iter().any(|p| p.is_esp);
                                    if ui.add_enabled(!has_esp, egui::Button::new(tr!("创建ESP分区 (500MB)"))).clicked() {
                                        should_add_esp = true;
                                    }
                                }
                            });

                            ui.add_space(15.0);

                            // 分区可视化编辑器
                            let total_size_gb = disk.size_gb();
                            let available_width = ui.available_width() - 20.0;
                            let bar_height = 60.0;

                            // 计算每个分区的位置
                            let layouts = &self.quick_partition_state.editor.partition_layouts;
                            let total_layout_size: f64 = layouts.iter().map(|p| p.size_gb).sum();
                            let unallocated_size = total_size_gb - total_layout_size;
                            
                            // 使用磁盘总大小来计算比例
                            let pixels_per_gb = if total_size_gb > 0.0 {
                                (available_width as f64 - 4.0) / total_size_gb
                            } else {
                                0.0
                            };

                            // 已有分区颜色（灰色系）
                            let existing_color = egui::Color32::from_rgb(100, 100, 100);
                            let existing_esp_color = egui::Color32::from_rgb(80, 120, 100);
                            let existing_msr_color = egui::Color32::from_rgb(80, 80, 100);
                            let existing_recovery_color = egui::Color32::from_rgb(120, 80, 80);
                            
                            // 新规划分区颜色（彩色）
                            let new_colors = [
                                egui::Color32::from_rgb(52, 152, 219),  // 蓝色
                                egui::Color32::from_rgb(46, 204, 113),  // 绿色
                                egui::Color32::from_rgb(155, 89, 182),  // 紫色
                                egui::Color32::from_rgb(241, 196, 15),  // 黄色
                                egui::Color32::from_rgb(230, 126, 34),  // 橙色
                            ];
                            let new_esp_color = egui::Color32::from_rgb(26, 188, 156); // 青色
                            
                            // 未分配空间颜色
                            let unallocated_color = egui::Color32::from_gray(30);

                            // 收集分区信息用于绘制
                            let partition_infos: Vec<(usize, f64, String, String, egui::Color32, bool)> = {
                                let mut new_partition_idx = 0;
                                layouts.iter().enumerate().map(|(idx, partition)| {
                                    let color = if partition.is_existing {
                                        if partition.is_esp {
                                            existing_esp_color
                                        } else if partition.is_msr {
                                            existing_msr_color
                                        } else if partition.is_recovery {
                                            existing_recovery_color
                                        } else {
                                            existing_color
                                        }
                                    } else {
                                        if partition.is_esp {
                                            new_esp_color
                                        } else {
                                            let c = new_colors[new_partition_idx % new_colors.len()];
                                            new_partition_idx += 1;
                                            c
                                        }
                                    };
                                    
                                    let display_name = partition.display_name();
                                    let name_with_status = if partition.is_existing {
                                        tr!("{} (已有)", display_name)
                                    } else {
                                        tr!("{} (新)", display_name)
                                    };
                                    let size_text = format!("{:.1}GB", partition.size_gb);
                                    
                                    (idx, partition.size_gb, name_with_status, size_text, color, partition.is_existing)
                                }).collect()
                            };

                            // 绘制分区条
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 2.0;
                                
                                for (idx, size_gb, name, size_text, color, is_existing) in &partition_infos {
                                    let width = (*size_gb * pixels_per_gb) as f32;
                                    if width < 10.0 {
                                        continue;
                                    }
                                    
                                    let (rect, response) = ui.allocate_exact_size(
                                        egui::vec2(width, bar_height),
                                        egui::Sense::click(),
                                    );
                                    
                                    let is_hovered = response.hovered();
                                    let fill_color = if is_hovered {
                                        color.linear_multiply(1.2)
                                    } else {
                                        *color
                                    };
                                    
                                    // 绘制分区矩形
                                    ui.painter().rect_filled(rect, 3.0, fill_color);
                                    
                                    // 绘制文字
                                    ui.painter().text(
                                        egui::pos2(rect.center().x, rect.top() + 15.0),
                                        egui::Align2::CENTER_CENTER,
                                        name,
                                        egui::FontId::proportional(12.0),
                                        egui::Color32::WHITE,
                                    );
                                    
                                    ui.painter().text(
                                        egui::pos2(rect.center().x, rect.bottom() - 15.0),
                                        egui::Align2::CENTER_CENTER,
                                        size_text,
                                        egui::FontId::proportional(12.0),
                                        egui::Color32::from_gray(220),
                                    );
                                    
                                    // 右键菜单
                                    response.context_menu(|ui| {
                                        if *is_existing {
                                            // 获取分区信息检查是否可调整大小
                                            let partition_info = self.quick_partition_state.editor.partition_layouts.get(*idx).cloned();
                                            let (can_resize, reason) = if let Some(ref p) = partition_info {
                                                p.can_resize()
                                            } else {
                                                (false, tr!("分区信息不可用"))
                                            };

                                            ui.label(tr!("已有分区: {}", name));
                                            if let Some(ref p) = partition_info {
                                                ui.label(tr!("已用: {} GB / {} GB", format!("{:.1}", p.used_gb), format!("{:.1}", p.size_gb)));
                                            }
                                            ui.separator();

                                            if can_resize {
                                                if ui.button(tr!("调整分区大小")).clicked() {
                                                    should_show_resize_existing_dialog = Some(*idx);
                                                    ui.close_menu();
                                                }
                                            } else {
                                                ui.add_enabled(false, egui::Button::new(tr!("调整分区大小")));
                                                ui.label(egui::RichText::new(&reason).small().color(egui::Color32::GRAY));
                                            }

                                            ui.separator();
                                            ui.label(egui::RichText::new(tr!("提示: 一键分区会清除整个磁盘")).small().color(egui::Color32::from_rgb(241, 196, 15)));
                                        } else {
                                            if ui.button(tr!("调整大小")).clicked() {
                                                should_show_resize_dialog = Some(*idx);
                                                ui.close_menu();
                                            }
                                            if ui.button(tr!("删除分区")).clicked() {
                                                should_delete_partition = Some(*idx);
                                                ui.close_menu();
                                            }
                                        }
                                    });
                                }
                                
                                // 绘制未分配空间
                                if unallocated_size >= 0.5 {
                                    let unalloc_width = (unallocated_size * pixels_per_gb) as f32;
                                    if unalloc_width >= 30.0 {
                                        let (rect, _response) = ui.allocate_exact_size(
                                            egui::vec2(unalloc_width, bar_height),
                                            egui::Sense::hover(),
                                        );
                                        
                                        ui.painter().rect_filled(rect, 3.0, unallocated_color);
                                        
                                        ui.painter().text(
                                            egui::pos2(rect.center().x, rect.top() + 15.0),
                                            egui::Align2::CENTER_CENTER,
                                            tr!("未分配"),
                                            egui::FontId::proportional(12.0),
                                            egui::Color32::from_gray(150),
                                        );
                                        
                                        ui.painter().text(
                                            egui::pos2(rect.center().x, rect.bottom() - 15.0),
                                            egui::Align2::CENTER_CENTER,
                                            &format!("{:.1}GB", unallocated_size),
                                            egui::FontId::proportional(12.0),
                                            egui::Color32::from_gray(120),
                                        );
                                    }
                                }
                            });

                            ui.add_space(15.0);

                            // 分区详细列表
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(tr!("分区列表:")).strong());
                                ui.label(egui::RichText::new(tr!("(右键点击分区方框可调整大小或删除)")).small().color(egui::Color32::GRAY));
                            });
                            ui.add_space(5.0);

                            egui::ScrollArea::vertical()
                                .max_height(150.0)
                                .show(ui, |ui| {
                                    egui::Grid::new("partition_list")
                                        .num_columns(6)
                                        .spacing([15.0, 8.0])
                                        .striped(true)
                                        .show(ui, |ui| {
                                            ui.label(egui::RichText::new(tr!("状态")).strong());
                                            ui.label(egui::RichText::new(tr!("盘符")).strong());
                                            ui.label(egui::RichText::new(tr!("大小")).strong());
                                            ui.label(egui::RichText::new(tr!("已用/空闲")).strong());
                                            ui.label(egui::RichText::new(tr!("卷标")).strong());
                                            ui.label(egui::RichText::new(tr!("文件系统")).strong());
                                            ui.end_row();

                                            let layouts_clone = self.quick_partition_state.editor.partition_layouts.clone();
                                            for (_idx, partition) in layouts_clone.iter().enumerate() {
                                                // 状态
                                                if partition.is_existing {
                                                    ui.colored_label(egui::Color32::GRAY, tr!("已有"));
                                                } else {
                                                    ui.colored_label(egui::Color32::from_rgb(46, 204, 113), tr!("新建"));
                                                }
                                                
                                                // 盘符
                                                let name = partition.display_name();
                                                ui.label(&name);

                                                // 大小
                                                ui.label(format!("{:.1} GB", partition.size_gb));
                                                
                                                // 已用/空闲
                                                if partition.is_existing && partition.used_gb > 0.0 {
                                                    ui.label(format!("{:.1}/{:.1} GB", partition.used_gb, partition.free_gb));
                                                } else {
                                                    ui.label("-");
                                                }

                                                // 卷标
                                                if partition.label.is_empty() {
                                                    ui.label("-");
                                                } else {
                                                    ui.label(&partition.label);
                                                }

                                                // 文件系统
                                                ui.label(&partition.file_system);

                                                ui.end_row();
                                            }
                                        });
                                });

                            ui.add_space(10.0);

                            // 状态消息
                            if !self.quick_partition_state.message.is_empty() {
                                let m = &self.quick_partition_state.message;
                                let color = if m.contains("失败") || m.contains("错误") || m.contains("无法") {
                                    egui::Color32::from_rgb(231, 76, 60)
                                } else if m.contains("成功") || m.contains("完成") {
                                    egui::Color32::from_rgb(102, 187, 106)
                                } else {
                                    egui::Color32::GRAY
                                };
                                ui.colored_label(color, &self.quick_partition_state.message);
                                ui.add_space(10.0);
                            }

                            // 警告信息
                            ui.horizontal(|ui| {
                                ui.colored_label(
                                    egui::Color32::from_rgb(241, 196, 15),
                                    tr!("警告: 一键分区将清除所选磁盘上的所有数据！请先备份重要文件。")
                                );
                            });

                            ui.add_space(15.0);

                            // 操作按钮
                            ui.horizontal(|ui| {
                                if ui.add(
                                    egui::Button::new(tr!("一键分区"))
                                        .min_size(egui::vec2(120.0, 35.0))
                                ).clicked() {
                                    should_show_confirm = true;
                                }

                                if ui.button(tr!("关闭")).clicked() {
                                    should_close = true;
                                }
                            });
                        }
                    }
                });
            });

        // 确认对话框
        if self.quick_partition_state.show_confirm_dialog {
            egui::Window::new(tr!("确认分区"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(10.0);
                        ui.colored_label(egui::Color32::from_rgb(241, 196, 15), "");
                        ui.add_space(10.0);
                        ui.label(tr!("确定要执行一键分区吗？"));
                        ui.label(tr!("此操作将清除所选磁盘上的所有数据！"));
                        ui.add_space(20.0);
                        ui.horizontal(|ui| {
                            if ui.button(tr!("确定执行")).clicked() {
                                should_execute = true;
                            }
                            if ui.button(tr!("取消")).clicked() {
                                self.quick_partition_state.show_confirm_dialog = false;
                            }
                        });
                        ui.add_space(10.0);
                    });
                });
        }
        
        // 调整大小对话框
        if self.quick_partition_state.editor.show_resize_dialog {
            let mut close_resize_dialog = false;
            let mut apply_resize = false;
            
            egui::Window::new(tr!("调整分区大小"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ui.vertical(|ui| {
                        ui.add_space(10.0);

                        if let Some(idx) = self.quick_partition_state.editor.resize_partition_index {
                            if let Some(partition) = self.quick_partition_state.editor.partition_layouts.get(idx) {
                                ui.label(tr!("分区: {}", partition.display_name()));
                                ui.label(tr!("当前大小: {} GB", format!("{:.1}", partition.size_gb)));
                                ui.add_space(10.0);

                                ui.horizontal(|ui| {
                                    ui.label(tr!("新大小 (GB):"));
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.quick_partition_state.editor.resize_size_text)
                                            .desired_width(100.0)
                                    );
                                });
                                
                                ui.add_space(10.0);
                                
                                // 获取磁盘总大小用于验证
                                let disk_total = self.quick_partition_state.editor.selected_disk_index
                                    .and_then(|disk_idx| self.quick_partition_state.physical_disks.get(disk_idx))
                                    .map(|d| d.size_gb())
                                    .unwrap_or(0.0);
                                
                                // 计算其他分区占用的空间
                                let other_partitions_size: f64 = self.quick_partition_state.editor.partition_layouts
                                    .iter()
                                    .enumerate()
                                    .filter(|(i, _)| *i != idx)
                                    .map(|(_, p)| p.size_gb)
                                    .sum();
                                
                                let max_size = disk_total - other_partitions_size;
                                ui.label(tr!("最大可用: {} GB", format!("{:.1}", max_size)));

                                ui.add_space(15.0);

                                ui.horizontal(|ui| {
                                    if ui.button(tr!("确定")).clicked() {
                                        apply_resize = true;
                                    }
                                    if ui.button(tr!("取消")).clicked() {
                                        close_resize_dialog = true;
                                    }
                                });
                            }
                        }
                        
                        ui.add_space(10.0);
                    });
                });
            
            if apply_resize {
                if let Some(idx) = self.quick_partition_state.editor.resize_partition_index {
                    if let Ok(new_size) = self.quick_partition_state.editor.resize_size_text.parse::<f64>() {
                        // 获取磁盘总大小
                        let disk_total = self.quick_partition_state.editor.selected_disk_index
                            .and_then(|disk_idx| self.quick_partition_state.physical_disks.get(disk_idx))
                            .map(|d| d.size_gb())
                            .unwrap_or(0.0);
                        
                        // 计算其他分区占用的空间
                        let other_partitions_size: f64 = self.quick_partition_state.editor.partition_layouts
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| *i != idx)
                            .map(|(_, p)| p.size_gb)
                            .sum();
                        
                        let max_size = disk_total - other_partitions_size;
                        
                        if new_size >= 0.5 && new_size <= max_size {
                            self.quick_partition_state.editor.partition_layouts[idx].size_gb = new_size;
                            close_resize_dialog = true;
                        } else {
                            self.quick_partition_state.message = tr!(
                                "大小必须在 0.5 GB 到 {} GB 之间", format!("{:.1}", max_size)
                            );
                        }
                    } else {
                        self.quick_partition_state.message = tr!("请输入有效的数字");
                    }
                }
            }
            
            if close_resize_dialog {
                self.quick_partition_state.editor.show_resize_dialog = false;
                self.quick_partition_state.editor.resize_partition_index = None;
                self.quick_partition_state.editor.resize_size_text.clear();
            }
        }

        // 调整已有分区大小对话框
        if self.quick_partition_state.editor.resizing_existing {
            let mut close_dialog = false;
            let mut apply_resize = false;
            
            egui::Window::new(tr!("调整已有分区大小"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ui.vertical(|ui| {
                        ui.add_space(10.0);

                        if let Some(idx) = self.quick_partition_state.editor.resize_existing_index {
                            if let Some(partition) = self.quick_partition_state.editor.partition_layouts.get(idx) {
                                ui.label(egui::RichText::new(tr!("分区: {}", partition.display_name())).strong());
                                ui.add_space(5.0);

                                ui.horizontal(|ui| {
                                    ui.label(tr!("当前大小:"));
                                    ui.label(format!("{:.1} GB", partition.size_gb));
                                });

                                ui.horizontal(|ui| {
                                    ui.label(tr!("已使用空间:"));
                                    ui.colored_label(
                                        egui::Color32::from_rgb(241, 196, 15),
                                        format!("{:.1} GB", partition.used_gb)
                                    );
                                });
                                
                                ui.horizontal(|ui| {
                                    ui.label(tr!("空闲空间:"));
                                    ui.colored_label(
                                        egui::Color32::from_rgb(46, 204, 113),
                                        format!("{:.1} GB", partition.free_gb)
                                    );
                                });
                                
                                ui.add_space(10.0);
                                ui.separator();
                                ui.add_space(10.0);
                                
                                let min_gb = self.quick_partition_state.editor.resize_existing_min_gb;
                                let max_gb = self.quick_partition_state.editor.resize_existing_max_gb;
                                
                                // 判断是否只能缩小
                                let can_extend = max_gb > partition.size_gb + 0.1;
                                let can_shrink = min_gb < partition.size_gb - 0.1;
                                
                                ui.horizontal(|ui| {
                                    ui.label(tr!("可调整范围:"));
                                    ui.label(format!("{:.1} GB - {:.1} GB", min_gb, max_gb));
                                });

                                // 显示提示信息
                                if !can_extend && can_shrink {
                                    ui.colored_label(
                                        egui::Color32::from_rgb(52, 152, 219),
                                        tr!("ℹ 分区后方无未分配空间，只能缩小")
                                    );
                                } else if can_extend && !can_shrink {
                                    ui.colored_label(
                                        egui::Color32::from_rgb(52, 152, 219),
                                        tr!("ℹ 分区已用空间接近总容量，只能扩大")
                                    );
                                }
                                
                                ui.add_space(10.0);
                                
                                ui.horizontal(|ui| {
                                    ui.label(tr!("新大小 (GB):"));
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.quick_partition_state.editor.resize_size_text)
                                            .desired_width(100.0)
                                    );
                                });

                                // 显示大小滑块
                                ui.add_space(5.0);
                                let mut slider_value: f64 = self.quick_partition_state.editor.resize_size_text
                                    .parse()
                                    .unwrap_or(partition.size_gb);
                                
                                if ui.add(
                                    egui::Slider::new(&mut slider_value, min_gb..=max_gb)
                                        .suffix(" GB")
                                ).changed() {
                                    self.quick_partition_state.editor.resize_size_text = format!("{:.1}", slider_value);
                                }
                                
                                ui.add_space(10.0);
                                
                                // 提示信息
                                ui.colored_label(
                                    egui::Color32::from_rgb(46, 204, 113),
                                    tr!("此操作会立即执行，分区数据会保留")
                                );
                                ui.colored_label(
                                    egui::Color32::from_rgb(241, 196, 15),
                                    tr!("调整可能需要一些时间，请勿中断！")
                                );

                                ui.add_space(15.0);

                                ui.horizontal(|ui| {
                                    if ui.button(tr!("执行调整")).clicked() {
                                        apply_resize = true;
                                    }
                                    if ui.button(tr!("取消")).clicked() {
                                        close_dialog = true;
                                    }
                                });
                            } else {
                                ui.label(tr!("分区信息不可用"));
                                if ui.button(tr!("关闭")).clicked() {
                                    close_dialog = true;
                                }
                            }
                        }
                        
                        ui.add_space(10.0);
                    });
                });
            
            if apply_resize {
                should_execute_resize_existing = true;
            }
            
            if close_dialog {
                self.quick_partition_state.editor.resizing_existing = false;
                self.quick_partition_state.editor.resize_existing_index = None;
                self.quick_partition_state.editor.resize_size_text.clear();
            }
        }

        // 处理操作
        if should_add_partition {
            self.add_new_partition();
        }

        if should_add_esp {
            self.add_esp_partition();
        }

        if let Some(idx) = should_delete_partition {
            self.delete_partition(idx);
        }

        if should_show_confirm {
            self.quick_partition_state.show_confirm_dialog = true;
        }

        if should_execute {
            self.execute_quick_partition();
        }

        if should_close {
            self.show_quick_partition_dialog = false;
        }
        
        // 处理磁盘选择
        if let Some(idx) = should_select_disk {
            self.select_disk_for_partition(idx);
        }
        
        // 处理刷新
        if should_refresh {
            self.quick_partition_state.loading = true;
            self.start_load_physical_disks();
        }
        
        // 处理显示调整大小对话框
        if let Some(idx) = should_show_resize_dialog {
            if let Some(partition) = self.quick_partition_state.editor.partition_layouts.get(idx) {
                self.quick_partition_state.editor.show_resize_dialog = true;
                self.quick_partition_state.editor.resize_partition_index = Some(idx);
                self.quick_partition_state.editor.resize_size_text = format!("{:.1}", partition.size_gb);
            }
        }
        
        // 处理显示调整已有分区大小对话框
        if let Some(idx) = should_show_resize_existing_dialog {
            if let Some(partition) = self.quick_partition_state.editor.partition_layouts.get(idx).cloned() {
                // 获取磁盘信息来计算可调整范围
                if let Some(disk_idx) = self.quick_partition_state.editor.selected_disk_index {
                    if let Some(disk) = self.quick_partition_state.physical_disks.get(disk_idx) {
                        // 计算最小大小（已用空间 + 0.1GB 余量）
                        let min_gb = partition.min_resize_gb();
                        
                        // 计算最大大小 = 当前分区大小 + 分区右侧的未分配空间
                        // 重要：DiskPart 的 extend 命令只能使用紧邻分区右侧的未分配空间
                        // 不能使用磁盘上其他位置的未分配空间
                        let max_gb = if let Some(part_num) = partition.partition_number {
                            let unallocated_after_mb = get_unallocated_space_after_partition_with_disk(disk, part_num);
                            let unallocated_after_gb = unallocated_after_mb as f64 / 1024.0;
                            partition.size_gb + unallocated_after_gb
                        } else {
                            // 如果没有分区编号，则无法扩展
                            partition.size_gb
                        };
                        
                        self.quick_partition_state.editor.resizing_existing = true;
                        self.quick_partition_state.editor.resize_existing_index = Some(idx);
                        self.quick_partition_state.editor.resize_existing_min_gb = min_gb;
                        self.quick_partition_state.editor.resize_existing_max_gb = max_gb;
                        self.quick_partition_state.editor.resize_size_text = format!("{:.1}", partition.size_gb);
                    }
                }
            }
        }
        
        // 处理执行调整已有分区大小
        if should_execute_resize_existing {
            self.execute_resize_existing_partition();
        }
        
        // 同步窗口开关状态
        if !window_open {
            self.show_quick_partition_dialog = false;
        }
    }
    
    /// 执行调整已有分区大小
    fn execute_resize_existing_partition(&mut self) {
        let idx = match self.quick_partition_state.editor.resize_existing_index {
            Some(i) => i,
            None => {
                self.quick_partition_state.message = tr!("未选择分区");
                return;
            }
        };

        let partition = match self.quick_partition_state.editor.partition_layouts.get(idx).cloned() {
            Some(p) => p,
            None => {
                self.quick_partition_state.message = tr!("分区信息不可用");
                return;
            }
        };

        let new_size_gb: f64 = match self.quick_partition_state.editor.resize_size_text.parse() {
            Ok(s) => s,
            Err(_) => {
                self.quick_partition_state.message = tr!("请输入有效的数字");
                return;
            }
        };
        
        // 验证大小范围
        let min_gb = self.quick_partition_state.editor.resize_existing_min_gb;
        let max_gb = self.quick_partition_state.editor.resize_existing_max_gb;
        
        if new_size_gb < min_gb || new_size_gb > max_gb {
            self.quick_partition_state.message = tr!(
                "大小必须在 {} GB 到 {} GB 之间",
                format!("{:.1}", min_gb), format!("{:.1}", max_gb)
            );
            return;
        }
        
        // 获取必要信息
        let disk_number = match partition.disk_number {
            Some(d) => d,
            None => {
                self.quick_partition_state.message = tr!("无法获取磁盘编号");
                return;
            }
        };

        let partition_number = match partition.partition_number {
            Some(p) => p,
            None => {
                self.quick_partition_state.message = tr!("无法获取分区编号");
                return;
            }
        };
        
        let current_size_mb = (partition.size_gb * 1024.0) as u64;
        let new_size_mb = (new_size_gb * 1024.0) as u64;
        let used_mb = (partition.used_gb * 1024.0) as u64;
        
        // 关闭对话框
        self.quick_partition_state.editor.resizing_existing = false;
        self.quick_partition_state.editor.resize_existing_index = None;
        
        // 显示执行中状态
        self.quick_partition_state.executing = true;
        self.quick_partition_state.message = tr!("正在调整分区大小，请稍候...");
        
        // 在后台线程执行
        let drive_letter = partition.drive_letter;
        let (tx, rx) = std::sync::mpsc::channel::<ResizePartitionResult>();
        
        std::thread::spawn(move || {
            let result = resize_existing_partition(
                disk_number,
                partition_number,
                drive_letter,
                current_size_mb,
                new_size_mb,
                used_mb,
            );
            let _ = tx.send(result);
        });
        
        // 存储接收器以便后续检查结果
        self.resize_existing_result_rx = Some(rx);
    }
    
    /// 检查调整已有分区大小的结果
    pub fn check_resize_existing_result(&mut self) {
        if let Some(ref rx) = self.resize_existing_result_rx {
            if let Ok(result) = rx.try_recv() {
                self.quick_partition_state.executing = false;
                self.resize_existing_result_rx = None;
                
                if result.success {
                    self.quick_partition_state.message = tr!("操作成功：{}", result.message);
                    // 刷新磁盘列表
                    self.quick_partition_state.loading = true;
                    self.start_load_physical_disks();
                    // 刷新主分区列表
                    self.partitions = crate::core::disk::DiskManager::get_partitions().unwrap_or_default();
                } else {
                    self.quick_partition_state.message = tr!("操作失败：{}", result.message);
                }
            }
        }
    }
}
