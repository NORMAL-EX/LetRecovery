//! 无损扩大C盘 UI 模块
//!
//! 在正常 Windows 环境中规划对当前系统 C 盘的无损扩容，
//! 实际的磁盘操作在重启进入 WinPE 后由 PE 端引擎执行。

use egui;
use std::sync::mpsc;

use crate::tr;
use crate::app::App;
use crate::core::install_config::{ConfigFileManager, ExpandConfig};
use crate::core::quick_partition::{get_physical_disks, query_shrink_max};

/// 异步加载 C 盘信息的结果
#[derive(Debug, Clone)]
pub struct ExpandCLoadResult {
    /// 是否找到了系统 C 盘
    pub found: bool,
    /// C 盘当前总大小（MB）
    pub current_size_mb: u64,
    /// C 盘已用空间（MB）
    pub used_mb: u64,
    /// C 盘空闲空间（MB）
    pub free_mb: u64,
    /// 计算得到的最大可扩容到的最终大小（MB）
    pub max_size_mb: u64,
    /// 不需要移动分区即可达到的最终大小（MB）= 当前大小 + C 盘后方相邻未分配空间。
    /// 目标 ≤ 此值走 Case 1(纯 extend)；超过则需移动后方分区(Case 2，搬数据)。
    pub no_move_max_mb: u64,
    /// 是否可以扩容
    pub can_expand: bool,
    /// 不可扩容（或提示）的原因
    pub reason: String,
}

impl Default for ExpandCLoadResult {
    fn default() -> Self {
        Self {
            found: false,
            current_size_mb: 0,
            used_mb: 0,
            free_mb: 0,
            max_size_mb: 0,
            no_move_max_mb: 0,
            can_expand: false,
            reason: String::new(),
        }
    }
}

/// 无损扩大C盘对话框状态
#[derive(Debug, Clone)]
pub struct ExpandCDialogState {
    /// 是否正在加载磁盘/C盘信息
    pub loading: bool,
    /// 是否正在执行（写配置 + 安装PE引导 + 重启）
    pub executing: bool,
    /// 状态消息
    pub message: String,
    /// C 盘当前总大小（MB）
    pub current_size_mb: u64,
    /// C 盘已用空间（MB）
    pub used_mb: u64,
    /// C 盘空闲空间（MB）
    pub free_mb: u64,
    /// 计算得到的最大可扩容到的最终大小（MB）
    pub max_size_mb: u64,
    /// 不需要移动分区即可达到的最终大小（MB）。目标超过此值需移动后方分区(搬数据)。
    pub no_move_max_mb: u64,
    /// 是否可以扩容
    pub can_expand: bool,
    /// 不可扩容（或提示）的原因
    pub reason: String,
    /// 目标大小文本输入
    pub target_size_text: String,
    /// 目标大小数值（MB）
    pub target_size_mb: u64,
    /// 是否显示确认对话框
    pub show_confirm_dialog: bool,
}

impl Default for ExpandCDialogState {
    fn default() -> Self {
        Self {
            loading: false,
            executing: false,
            message: String::new(),
            current_size_mb: 0,
            used_mb: 0,
            free_mb: 0,
            max_size_mb: 0,
            no_move_max_mb: 0,
            can_expand: false,
            reason: String::new(),
            target_size_text: String::new(),
            target_size_mb: 0,
            show_confirm_dialog: false,
        }
    }
}

impl ExpandCDialogState {
    /// 最小可设置的目标大小（MB）：已用空间 + 1GB 余量，且不小于当前大小
    /// （无损扩容不允许缩小，因此最小值就是当前大小）
    pub fn min_target_mb(&self) -> u64 {
        let safe_floor = self.used_mb + 1024;
        self.current_size_mb.max(safe_floor)
    }
}

impl App {
    /// 初始化无损扩大C盘对话框
    pub fn init_expand_c_dialog(&mut self) {
        self.show_expand_c_dialog = true;
        self.expand_c_state = ExpandCDialogState::default();
        self.expand_c_state.loading = true;
        self.expand_c_state.message = tr!("正在分析 C 盘可扩容空间...");

        // 启动后台加载
        self.start_load_expand_c_info();
    }

    /// 启动后台加载 C 盘信息并计算最大可扩容大小
    pub fn start_load_expand_c_info(&mut self) {
        let (tx, rx) = mpsc::channel::<ExpandCLoadResult>();
        self.expand_c_load_rx = Some(rx);

        std::thread::spawn(move || {
            let result = compute_expand_c_info();
            let _ = tx.send(result);
        });
    }

    /// 轮询后台加载结果（每帧调用）
    pub fn check_expand_c_load(&mut self) {
        if let Some(ref rx) = self.expand_c_load_rx {
            if let Ok(result) = rx.try_recv() {
                self.expand_c_load_rx = None;
                self.expand_c_state.loading = false;
                self.expand_c_state.current_size_mb = result.current_size_mb;
                self.expand_c_state.used_mb = result.used_mb;
                self.expand_c_state.free_mb = result.free_mb;
                self.expand_c_state.max_size_mb = result.max_size_mb;
                self.expand_c_state.no_move_max_mb = result.no_move_max_mb;
                self.expand_c_state.can_expand = result.can_expand;
                self.expand_c_state.reason = result.reason.clone();

                if !result.found {
                    self.expand_c_state.message = tr!("未找到当前系统 C 盘");
                } else if !result.can_expand {
                    self.expand_c_state.message = result.reason.clone();
                } else {
                    self.expand_c_state.message.clear();
                    // 默认目标大小设为最大值
                    self.expand_c_state.target_size_mb = result.max_size_mb;
                    self.expand_c_state.target_size_text =
                        format!("{:.1}", result.max_size_mb as f64 / 1024.0);
                }
            }
        }
    }

    /// 渲染无损扩大C盘对话框
    pub fn render_expand_c_dialog(&mut self, ui: &mut egui::Ui) {
        if !self.show_expand_c_dialog {
            return;
        }

        // 轮询异步加载
        self.check_expand_c_load();

        let mut should_close = false;
        let mut should_show_confirm = false;
        let mut should_start = false;

        let mut window_open = self.show_expand_c_dialog;

        egui::Window::new(tr!("无损扩大C盘"))
            .open(&mut window_open)
            .resizable(true)
            .default_width(520.0)
            .min_width(460.0)
            .show(ui.ctx(), |ui| {
                // 加载中
                if self.expand_c_state.loading {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.spinner();
                        ui.label(tr!("正在分析 C 盘可扩容空间..."));
                    });
                    return;
                }

                // 执行中
                if self.expand_c_state.executing {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.spinner();
                        ui.label(tr!("正在准备扩容环境，请勿中断..."));
                    });
                    return;
                }

                ui.vertical(|ui| {
                    // 当前 C 盘信息
                    let cur_gb = self.expand_c_state.current_size_mb as f64 / 1024.0;
                    let used_gb = self.expand_c_state.used_mb as f64 / 1024.0;
                    let free_gb = self.expand_c_state.free_mb as f64 / 1024.0;
                    let max_gb = self.expand_c_state.max_size_mb as f64 / 1024.0;

                    ui.label(egui::RichText::new(tr!("当前系统盘 (C:)")).strong());
                    ui.add_space(5.0);
                    egui::Grid::new("expand_c_info_grid")
                        .num_columns(2)
                        .spacing([15.0, 6.0])
                        .show(ui, |ui| {
                            ui.label(tr!("当前总大小:"));
                            ui.label(format!("{:.1} GB", cur_gb));
                            ui.end_row();

                            ui.label(tr!("已用空间:"));
                            ui.colored_label(
                                egui::Color32::from_rgb(241, 196, 15),
                                format!("{:.1} GB", used_gb),
                            );
                            ui.end_row();

                            ui.label(tr!("空闲空间:"));
                            ui.colored_label(
                                egui::Color32::from_rgb(46, 204, 113),
                                format!("{:.1} GB", free_gb),
                            );
                            ui.end_row();

                            ui.label(tr!("最大可扩容到:"));
                            ui.colored_label(
                                egui::Color32::from_rgb(52, 152, 219),
                                format!("{:.1} GB", max_gb),
                            );
                            ui.end_row();
                        });

                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(10.0);

                    if !self.expand_c_state.can_expand {
                        ui.colored_label(
                            egui::Color32::from_rgb(231, 76, 60),
                            if self.expand_c_state.reason.is_empty() {
                                tr!("C 盘后方没有可用于扩容的空间")
                            } else {
                                self.expand_c_state.reason.clone()
                            },
                        );
                        ui.add_space(10.0);
                        if ui.button(tr!("关闭")).clicked() {
                            should_close = true;
                        }
                        return;
                    }

                    // 目标大小
                    let min_mb = self.expand_c_state.min_target_mb();
                    let max_mb = self.expand_c_state.max_size_mb;
                    let min_gb = min_mb as f64 / 1024.0;

                    ui.horizontal(|ui| {
                        ui.label(tr!("目标大小 (GB):"));
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut self.expand_c_state.target_size_text)
                                    .desired_width(100.0),
                            )
                            .changed()
                        {
                            if let Ok(gb) = self.expand_c_state.target_size_text.parse::<f64>() {
                                self.expand_c_state.target_size_mb = (gb * 1024.0) as u64;
                            }
                        }
                    });

                    // 滑块（GB）
                    ui.add_space(5.0);
                    let mut slider_gb: f64 = self.expand_c_state.target_size_mb as f64 / 1024.0;
                    if ui
                        .add(
                            egui::Slider::new(&mut slider_gb, min_gb..=(max_mb as f64 / 1024.0))
                                .suffix(" GB"),
                        )
                        .changed()
                    {
                        self.expand_c_state.target_size_mb = (slider_gb * 1024.0) as u64;
                        self.expand_c_state.target_size_text = format!("{:.1}", slider_gb);
                    }

                    ui.add_space(5.0);
                    ui.label(
                        egui::RichText::new(tr!(
                            "可设置范围: {} GB - {} GB",
                            format!("{:.1}", min_gb),
                            format!("{:.1}", max_mb as f64 / 1024.0)
                        ))
                        .small()
                        .color(egui::Color32::GRAY),
                    );

                    ui.add_space(10.0);

                    // 说明
                    ui.colored_label(
                        egui::Color32::from_rgb(52, 152, 219),
                        tr!("提示: 此操作为无损扩容，C 盘数据会保留。"),
                    );
                    ui.colored_label(
                        egui::Color32::from_rgb(241, 196, 15),
                        tr!("若本机没有 WinPE，将先自动下载 WinPE；随后会安装 PE 引导并重启进入 WinPE 完成扩容。"),
                    );

                    // 当目标超过“相邻未分配空间”可达上限时，需要移动后方分区(搬数据)，给出醒目警告。
                    let no_move_max = self.expand_c_state.no_move_max_mb;
                    if self.expand_c_state.target_size_mb > no_move_max && no_move_max > 0 {
                        ui.add_space(6.0);
                        ui.colored_label(
                            egui::Color32::from_rgb(231, 76, 60),
                            tr!(
                                "⚠ 超过 {} GB 的部分需要移动 C 盘后方分区的数据来腾挪空间：\n· 该过程会搬移后方分区(如 D:)的数据，耗时较长；\n· 进行中切勿断电/强制关机，否则可能损坏后方分区；\n· 此为实验性功能，建议先在测试机/虚拟机验证。\n若只想要稳妥的纯扩展，请把目标控制在 {} GB 以内。",
                                format!("{:.1}", no_move_max as f64 / 1024.0),
                                format!("{:.1}", no_move_max as f64 / 1024.0),
                            ),
                        );
                    }

                    // 状态消息
                    if !self.expand_c_state.message.is_empty() {
                        ui.add_space(8.0);
                        let m = &self.expand_c_state.message;
                        let color = if m.contains("失败") || m.contains("错误") || m.contains("无法") {
                            egui::Color32::from_rgb(231, 76, 60)
                        } else if m.contains("成功") || m.contains("完成") {
                            egui::Color32::from_rgb(102, 187, 106)
                        } else {
                            egui::Color32::GRAY
                        };
                        ui.colored_label(color, m);
                    }

                    ui.add_space(15.0);

                    ui.horizontal(|ui| {
                        if ui
                            .add(egui::Button::new(tr!("开始扩容")).min_size(egui::vec2(120.0, 35.0)))
                            .clicked()
                        {
                            should_show_confirm = true;
                        }
                        if ui.button(tr!("关闭")).clicked() {
                            should_close = true;
                        }
                    });
                });
            });

        // 确认对话框
        if self.expand_c_state.show_confirm_dialog {
            egui::Window::new(tr!("确认扩容"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(10.0);
                        let target_gb = self.expand_c_state.target_size_mb as f64 / 1024.0;
                        ui.label(tr!("确定要将 C 盘扩容到 {} GB 吗？", format!("{:.1}", target_gb)));
                        ui.label(tr!("电脑将重启进入 WinPE 完成无损扩容。"));
                        ui.add_space(20.0);
                        ui.horizontal(|ui| {
                            if ui.button(tr!("确定执行")).clicked() {
                                should_start = true;
                            }
                            if ui.button(tr!("取消")).clicked() {
                                self.expand_c_state.show_confirm_dialog = false;
                            }
                        });
                        ui.add_space(10.0);
                    });
                });
        }

        if should_show_confirm {
            self.expand_c_state.show_confirm_dialog = true;
        }

        if should_start {
            self.expand_c_state.show_confirm_dialog = false;
            self.start_expand_c_drive();
        }

        if should_close {
            self.show_expand_c_dialog = false;
        }

        if !window_open {
            self.show_expand_c_dialog = false;
        }
    }

    /// 开始扩容（触发器）：必要时先下载 PE，否则直接执行交接
    pub fn start_expand_c_drive(&mut self) {
        // 校验目标大小
        let min_mb = self.expand_c_state.min_target_mb();
        let max_mb = self.expand_c_state.max_size_mb;
        let target_mb = self.expand_c_state.target_size_mb;

        if target_mb < min_mb || target_mb > max_mb {
            self.expand_c_state.message = tr!(
                "目标大小必须在 {} GB 到 {} GB 之间",
                format!("{:.1}", min_mb as f64 / 1024.0),
                format!("{:.1}", max_mb as f64 / 1024.0)
            );
            return;
        }

        // 获取选中的 PE 信息
        let pe_info = self.selected_pe_for_install.and_then(|idx| {
            self.config.as_ref().and_then(|c| c.pe_list.get(idx).cloned())
        });

        if let Some(pe) = pe_info {
            let (pe_exists, _) = crate::core::pe::PeManager::check_pe_exists(&pe.filename);
            if !pe_exists {
                // PE 不存在，先下载 PE，下载完成后继续扩容交接
                log::info!("[EXPAND] PE文件不存在，开始下载: {}", pe.filename);
                self.pending_download_url = Some(pe.download_url.clone());
                self.pending_download_filename = Some(pe.filename.clone());
                self.pending_pe_md5 = pe.md5.clone();
                let pe_dir = crate::utils::path::get_pe_dir().to_string_lossy().to_string();
                self.download_save_path = pe_dir;
                self.pe_download_then_action = Some(crate::app::PeDownloadThenAction::Expand);
                self.show_expand_c_dialog = false;
                self.current_panel = crate::app::Panel::DownloadProgress;
                return;
            }
        } else {
            self.expand_c_state.message = tr!("未选择 PE 环境，无法扩容");
            return;
        }

        // PE 已存在，直接执行交接
        self.start_expand_pe_handoff();
    }

    /// 执行扩容交接：写配置 + 安装 PE 引导 + 重启进入 PE
    pub fn start_expand_pe_handoff(&mut self) {
        log::info!("[EXPAND PE] ========== 开始扩容PE准备 ==========");

        // 取消下载页面残留状态（如果是下载后进来的）
        self.show_expand_c_dialog = true;
        self.expand_c_state.executing = true;
        self.expand_c_state.message = tr!("正在准备扩容环境...");

        let target_size_mb = if self.expand_c_state.target_size_mb >= self.expand_c_state.max_size_mb
        {
            // 用户选择了最大值，则写 0 表示扩容到最大可用
            0
        } else {
            self.expand_c_state.target_size_mb
        };

        // 获取选中的 PE 信息
        let pe_info = self.selected_pe_for_install.and_then(|idx| {
            self.config.as_ref().and_then(|c| c.pe_list.get(idx).cloned())
        });

        let pe_info = match pe_info {
            Some(p) => p,
            None => {
                self.expand_c_state.executing = false;
                self.expand_c_state.message = tr!("未选择 PE 环境，无法扩容");
                return;
            }
        };

        let (pe_exists, pe_path) = crate::core::pe::PeManager::check_pe_exists(&pe_info.filename);
        if !pe_exists {
            self.expand_c_state.executing = false;
            self.expand_c_state.message = tr!("PE 文件不存在，无法扩容");
            return;
        }

        // 扩容不格式化 C 盘，配置文件写到 C: 自身即可（C: 始终存在）
        let data_partition = "C:".to_string();

        let cfg = ExpandConfig {
            target_partition: "C:".to_string(),
            target_size_mb,
            wim_engine: lr_core::active_engine().as_u8(),
        };

        log::info!(
            "[EXPAND PE] 写入扩容配置: target=C:, target_size_mb={}, wim_engine={}",
            cfg.target_size_mb, cfg.wim_engine
        );

        match ConfigFileManager::write_expand_config("C:", &data_partition, &cfg) {
            Ok(_) => log::info!("[EXPAND PE] 扩容配置写入成功"),
            Err(e) => {
                log::error!("[EXPAND PE] 扩容配置写入失败: {}", e);
                self.expand_c_state.executing = false;
                self.expand_c_state.message = tr!("写入扩容配置失败: {}", e);
                return;
            }
        }

        // 安装 PE 引导
        log::info!("[EXPAND PE] 安装PE引导: {}", pe_path);
        let pe_manager = crate::core::pe::PeManager::new();
        match pe_manager.boot_to_pe(&pe_path, "LetRecovery PE") {
            Ok(_) => log::info!("[EXPAND PE] PE引导安装成功"),
            Err(e) => {
                log::error!("[EXPAND PE] PE引导安装失败: {}", e);
                self.expand_c_state.executing = false;
                self.expand_c_state.message = tr!("安装 PE 引导失败: {}", e);
                return;
            }
        }

        // 重启进入 PE
        log::info!("[EXPAND PE] 执行重启命令");
        let _ = crate::utils::cmd::create_command("shutdown")
            .args([
                "/r",
                "/t",
                "5",
                "/c",
                "LetRecovery 即将重启进入 WinPE 进行无损扩容...",
            ])
            .spawn();

        self.expand_c_state.message = tr!("准备完成，即将重启进入 WinPE...");
        log::info!("[EXPAND PE] ========== 扩容PE准备结束 ==========");
    }
}

/// 在后台线程计算 C 盘扩容信息
fn compute_expand_c_info() -> ExpandCLoadResult {
    let mut result = ExpandCLoadResult::default();

    let disks = get_physical_disks();

    // 在所有磁盘中查找盘符为 C 的分区
    let mut found_disk: Option<&crate::core::quick_partition::PhysicalDisk> = None;
    let mut c_index: Option<usize> = None;

    for disk in &disks {
        if let Some(idx) = disk
            .partitions
            .iter()
            .position(|p| p.drive_letter == Some('C'))
        {
            found_disk = Some(disk);
            c_index = Some(idx);
            break;
        }
    }

    let (disk, c_idx) = match (found_disk, c_index) {
        (Some(d), Some(i)) => (d, i),
        _ => {
            result.reason = tr!("未找到当前系统 C 盘");
            return result;
        }
    };

    let c_part = &disk.partitions[c_idx];
    let bytes_per_mb: u64 = 1024 * 1024;

    result.found = true;
    result.current_size_mb = c_part.size_bytes / bytes_per_mb;
    result.used_mb = c_part.used_bytes / bytes_per_mb;
    result.free_mb = c_part.free_bytes / bytes_per_mb;

    // C 盘结束位置（字节）
    let c_end = c_part.offset_bytes + c_part.size_bytes;

    // 1) 计算 C 盘正后方紧邻的未分配空间
    //    找到所有 offset >= c_end 的分区，按 offset 排序，取最靠近 C 盘的那个。
    let mut following: Vec<&crate::core::quick_partition::DiskPartitionInfo> = disk
        .partitions
        .iter()
        .filter(|p| p.offset_bytes >= c_end)
        .collect();
    following.sort_by_key(|p| p.offset_bytes);

    // 紧邻 C 盘之后的未分配空间（C 盘结束到下一个分区起点之间）
    let unallocated_after_bytes: u64 = match following.first() {
        Some(next) => next.offset_bytes.saturating_sub(c_end),
        None => {
            // C 盘是磁盘上最后一个分区：使用磁盘尾部的未分配空间
            disk.size_bytes.saturating_sub(c_end)
        }
    };
    let unallocated_after_mb = unallocated_after_bytes / bytes_per_mb;

    // 2) 紧邻 C 盘之后的“下一个分区可缩小让出的空间”。PE 端 Case 2 会把该分区整体右移、
    //    在 C 之后腾出未分配空间再 extend，因此该部分**计入**可扩上限——但需要搬移数据。
    //    只有紧贴 C 之后(中间无未分配空间)的、有盘符的基础数据分区才可移动。
    let mut next_shrinkable_mb: u64 = 0;
    if let Some(next) = following.first() {
        // 紧贴：下一个分区起点≈C 盘结束（允许极小对齐间隙）。否则中间是未分配空间，归 Case 1。
        let adjacent = next.offset_bytes.saturating_sub(c_end) < (2 * 1024 * 1024);
        let movable = adjacent && !next.is_esp && !next.is_msr && !next.is_recovery;
        if movable {
            if let Some(letter) = next.drive_letter {
                if let Ok(mb) = query_shrink_max(letter) {
                    next_shrinkable_mb = mb;
                    log::info!("[EXPAND] 后方分区 {}: 可让出 {} MB（需移动数据，Case 2）", letter, mb);
                }
            }
        }
    }

    // 不移动数据即可达到的上限 = 当前 + 相邻未分配空间（Case 1，纯 extend）。
    let no_move_max_mb = result.current_size_mb + unallocated_after_mb;
    // 总可扩上限 = Case 1 + 后方分区可让出空间（Case 2，搬数据）。
    let max_size_mb = no_move_max_mb + next_shrinkable_mb;
    result.no_move_max_mb = no_move_max_mb;
    result.max_size_mb = max_size_mb;

    // 至少要能扩 1GB 才认为可以扩容
    if max_size_mb > result.current_size_mb + 1024 {
        result.can_expand = true;
        if next_shrinkable_mb > 1024 {
            result.reason = tr!(
                "可无损并入：相邻未分配约 {} GB（直接扩）+ 后方分区可让出约 {} GB（需移动该分区的数据）。",
                format!("{:.1}", unallocated_after_mb as f64 / 1024.0),
                format!("{:.1}", next_shrinkable_mb as f64 / 1024.0),
            );
        }
    } else {
        result.can_expand = false;
        result.reason = tr!("C 盘后方没有可用于扩容的空间。可先用「一键分区」在 C 盘后方腾出未分配空间。");
    }

    result
}
