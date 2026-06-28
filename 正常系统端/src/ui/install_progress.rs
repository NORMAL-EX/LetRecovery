use egui;
use std::sync::mpsc;
use std::path::Path;

use crate::tr;
use crate::app::{App, BootModeSelection, InstallMode};
use crate::core::dism::DismProgress;
use crate::core::disk::{Partition, PartitionStyle};
use crate::core::ghost::Ghost;
use crate::core::install_config::{ConfigFileManager, InstallConfig};
use crate::ui::advanced_options::AdvancedOptions;

impl App {
    pub fn show_install_progress(&mut self, ui: &mut egui::Ui) {
        ui.heading(tr!("安装进度"));
        ui.separator();

        self.update_install_progress();

        if !self.is_installing {
            ui.label(tr!("没有正在进行的安装任务"));
            if ui.button(tr!("返回")).clicked() {
                self.current_panel = crate::app::Panel::SystemInstall;
            }
            return;
        }

        // 显示安装模式
        let mode_text = match self.install_mode {
            InstallMode::Direct => tr!("直接安装"),
            InstallMode::ViaPE => tr!("通过PE安装"),
        };
        ui.label(tr!("安装模式: {}", mode_text));

        ui.add_space(15.0);
        ui.label(tr!(
            "当前步骤: {}",
            self.install_progress.current_step
        ));

        ui.add(
            egui::ProgressBar::new(self.install_progress.step_progress as f32 / 100.0)
                .text(format!("{}%", self.install_progress.step_progress))
                .animate(true),
        );

        ui.add_space(10.0);

        ui.label(tr!("总体进度:"));
        ui.add(
            egui::ProgressBar::new(self.install_progress.total_progress as f32 / 100.0)
                .text(format!("{}%", self.install_progress.total_progress))
                .animate(true),
        );

        ui.add_space(20.0);

        // 安装步骤列表
        ui.label(tr!("安装步骤:"));
        egui::ScrollArea::vertical()
            .max_height(200.0)
            .show(ui, |ui| {
                let mut steps = match self.install_mode {
                    InstallMode::Direct => vec![
                        tr!("格式化分区"),
                        tr!("导出驱动"),
                        tr!("释放系统镜像"),
                        tr!("导入驱动"),
                        tr!("修复引导"),
                        tr!("应用高级选项"),
                        tr!("完成安装"),
                    ],
                    InstallMode::ViaPE => vec![
                        tr!("检查PE环境"),
                        tr!("安装PE引导"),
                        tr!("导出驱动"),
                        tr!("复制镜像文件"),
                        tr!("写入配置文件"),
                        tr!("准备重启"),
                    ],
                };

                // 如果需要 BitLocker 解密，插入解密步骤作为第一步
                if self.bitlocker_decryption_needed {
                    steps.insert(0, tr!("解密 BitLocker 分区"));
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
                        "●"
                    } else if is_current {
                        "→"
                    } else {
                        "○"
                    };

                    let color = if is_completed {
                        egui::Color32::from_rgb(102, 187, 106)
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
            ui.colored_label(egui::Color32::RED, tr!("错误: {}", error));
            ui.add_space(10.0);
        }

        // 安装完成后的操作
        if self.install_progress.total_progress >= 100 {
            match self.install_mode {
                InstallMode::Direct => {
                    ui.colored_label(egui::Color32::from_rgb(102, 187, 106), tr!("安装完成！"));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button(tr!("立即重启")).clicked() {
                            self.reboot_system();
                        }
                        if ui.button(tr!("返回主页")).clicked() {
                            self.is_installing = false;
                            self.current_panel = crate::app::Panel::SystemInstall;
                        }
                    });
                }
                InstallMode::ViaPE => {
                    ui.colored_label(egui::Color32::from_rgb(102, 187, 106), tr!("PE环境准备完成！"));
                    ui.label(tr!("系统将重启进入PE环境继续安装。"));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button(tr!("立即重启")).clicked() {
                            self.reboot_system();
                        }
                        if ui.button(tr!("稍后重启")).clicked() {
                            self.is_installing = false;
                            self.current_panel = crate::app::Panel::SystemInstall;
                        }
                    });
                }
            }
        } else {
            if ui.button(tr!("取消安装")).clicked() {
                log::info!("[INSTALL] 用户取消安装");
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
                    log::info!("[INSTALL UI] BitLocker 解密完成，准备开始安装");
                    self.decrypting_partitions.clear();
                    self.install_progress.current_step = tr!("准备开始安装...");
                    return;
                } else if progress.status.starts_with("DECRYPTING:") {
                    self.install_progress.current_step = progress.status.trim_start_matches("DECRYPTING:").to_string();
                    // 使用实际的解密进度（从加密百分比计算得出）
                    self.install_progress.step_progress = progress.percentage;
                    return;
                } else if let Some(msg) = progress.status.strip_prefix("ERROR:") {
                    // 引擎/格式化失败：写入 install_error → 渲染时显示醒目红色错误条（不再只在步骤名一闪）
                    log::error!("[INSTALL UI] 安装失败: {}", msg);
                    self.install_error = Some(msg.to_string());
                    self.install_progress.current_step = tr!("安装失败");
                    return;
                }

                if let Some((step, name)) = parse_step_from_status(&progress.status) {
                    self.install_progress.step_progress = progress.percentage;
                    
                    if step != self.install_step || self.install_progress.current_step != name {
                        self.install_step = step;
                        self.install_progress.current_step = name.clone();
                        log::info!("[INSTALL UI] 步骤更新: {} - {} ({}%)", step, name, progress.percentage);
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
                        log::info!("[INSTALL] 安装完成，用户已勾选立即重启，执行自动重启");
                        self.auto_reboot_triggered = true;
                        self.reboot_system();
                    }
                }
            }
        }
    }

    /// 直接安装线程
    fn start_direct_install_thread(&mut self) {
        log::info!("[INSTALL] ========== 开始直接安装 ==========");
        log::info!("[INSTALL] 目标分区: {}", self.install_target_partition);
        log::info!("[INSTALL] 镜像路径: {}", self.install_image_path);
        log::info!("[INSTALL] 镜像索引: {}", self.install_volume_index);

        let (progress_tx, progress_rx) = mpsc::channel::<DismProgress>();
        self.install_progress_rx = Some(progress_rx);

        let target_partition = self.install_target_partition.clone();
        let image_path = self.install_image_path.clone();
        let volume_index = self.install_volume_index;
        let options = self.install_options.clone();
        let advanced_options = self.advanced_options.clone();
        let partitions: Vec<Partition> = self.partitions.clone();
        
        // 选中目标分区的「磁盘号:分区号」——安装目标以此为准（照搬 DSI），盘符只是 PE 里临时的、会变。
        // 跑完 diskpart 脚本后用它把目标重新定位回来；分区表类型也一并取出（脚本后会刷新）。
        let selected_partition = self
            .partitions
            .iter()
            .find(|p| p.letter == target_partition)
            .cloned();
        let partition_style = selected_partition
            .as_ref()
            .map(|p| p.partition_style)
            .unwrap_or(PartitionStyle::Unknown);
        let target_disk = selected_partition.as_ref().and_then(|p| p.disk_number);
        let target_part_no = selected_partition.as_ref().and_then(|p| p.partition_number);

        self.install_step = 1;
        self.install_progress.current_step = tr!("格式化分区");
        self.install_error = None; // 清掉上一次失败的残留，避免新安装一开始就显示旧错误

        std::thread::spawn(move || {
            log::info!("[INSTALL THREAD] 安装线程启动");

            // 跑完 diskpart 脚本后，target_partition / partition_style 可能要按 disk:partition 重定位刷新。
            let mut target_partition = target_partition;
            let mut partition_style = partition_style;
            
            let temp_dir = std::env::temp_dir();
            let driver_backup_path = temp_dir.join("LetRecovery_DriverBackup");
            let driver_backup_str = driver_backup_path.to_string_lossy().to_string();

            // 装机前运行 diskpart 脚本（分区准备）——读 bin\diskpart\（旧位置/PE 暂存目录自动回退，当前已在 PE）。
            //
            // 不据脚本退出码中止：diskpart/cmd 分区脚本即便把活干完，也常返回非 0（遇到一条命令出错就停、
            // 或最后一条报 warning）；早先“失败即 return”会在脚本已经改了分区表【之后】把安装拦腰斩断，
            // 把盘留在“清了/重建一半、没装系统”的空盘状态（实机回归）。
            //
            // 目标分区按【磁盘号:分区号】重定位（照搬 DSI）：脚本会重建分区、盘符在 PE 里会变、新建分区
            // 常常没盘符——所以不靠盘符认，靠 disk:partition 找回那个分区，没盘符就自动分配一个空闲盘符，
            // 并刷新分区表类型（脚本可能把 GPT 改成 MBR）。只有该 disk:partition 真的不存在时才中止。
            if options.run_diskpart_scripts {
                send_step(&progress_tx, 1, &tr!("运行 Diskpart 脚本"), 0);
                let scripts_dir = crate::utils::path::get_diskpart_scripts_dir();
                log::info!("[INSTALL] 运行 Diskpart 脚本: {}", scripts_dir.display());

                let script_output = match lr_core::diskpart::run_scripts_in_dir(&scripts_dir) {
                    Ok(out) => {
                        log::info!("[INSTALL] Diskpart 脚本执行完成:\n{}", out);
                        out
                    }
                    Err(e) => {
                        log::warn!(
                            "[INSTALL] Diskpart 脚本返回非 0/未全部成功（不据此中止，按 disk:partition 重定位目标）：{}",
                            e
                        );
                        e
                    }
                };

                // 等卷管理器稳定，再按「磁盘号:分区号」把安装目标重新定位回来。
                std::thread::sleep(std::time::Duration::from_millis(800));
                match resolve_install_target(target_disk, target_part_no, &target_partition) {
                    Ok((letter, style)) => {
                        log::info!(
                            "[INSTALL] Diskpart 脚本后目标重定位: 磁盘{:?}:分区{:?}（原 {}）-> {}（分区表 {:?}）",
                            target_disk, target_part_no, target_partition, letter, style
                        );
                        target_partition = letter;
                        partition_style = style;
                        send_step(&progress_tx, 1, &tr!("运行 Diskpart 脚本"), 100);
                    }
                    Err(e) => {
                        log::error!("[INSTALL] Diskpart 脚本后无法定位安装目标，中止：{}", e);
                        send_error(
                            &progress_tx,
                            &tr!(
                                "Diskpart 脚本执行后无法定位安装目标：{}\n已中止，未格式化、未释放任何文件。请点「刷新分区」重新选择目标分区，或检查 diskpart 脚本。\n脚本输出：\n{}",
                                e,
                                script_output
                            ),
                        );
                        return;
                    }
                }
            }

            // Step 1: 格式化分区
            send_step(&progress_tx, 1, &tr!("格式化分区"), 0);
            std::thread::sleep(std::time::Duration::from_millis(50));
            if options.format_partition {
                log::info!("[INSTALL STEP 1] 开始格式化分区: {}", target_partition);
                send_step(&progress_tx, 1, &tr!("格式化分区"), 30);
                match format_partition(&target_partition) {
                    Ok(_) => log::info!("[INSTALL STEP 1] 格式化完成"),
                    Err(e) => {
                        // 用户勾了「格式化分区」却失败：一律中止（不再仅 XP 才拦）。否则会把镜像/安装
                        // 文件层叠到旧/脏/被占用的文件系统上，得到一个被污染、引导与系统都可能异常的盘
                        // （XP 尤其致命）。报真因，让用户处理（多为该卷被资源管理器/程序占用）后重试。
                        log::error!("[INSTALL STEP 1] 格式化失败，中止安装: {}", e);
                        send_error(
                            &progress_tx,
                            &tr!(
                                "格式化分区 {} 失败：{}。已中止，未释放镜像、未写入任何安装文件。请确认该分区未被占用（关闭正在浏览它的资源管理器/程序）后重试。",
                                target_partition, e
                            ),
                        );
                        return;
                    }
                }
                send_step(&progress_tx, 1, &tr!("格式化分区"), 100);
            } else {
                log::info!("[INSTALL STEP 1] 跳过格式化");
                send_step(&progress_tx, 1, &tr!("格式化分区"), 100);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));

            // XP/2003 i386 文本安装介质：不释放 WIM，改为从 i386 源准备「硬盘文本安装」
            // （复制 i386 → 目标盘 $WIN_NT$.~LS、写 NTLDR(setupldr)/NTDETECT/txtsetup.sif/winnt.sif、
            // bootsect /nt52 写 XP 引导）。引擎自行写引导，且此刻系统文件尚未释放（重启进文本安装
            // 阶段才复制），因此跳过 导出/导入驱动、引导修复、应用高级选项 等后续步骤。仅 Legacy/MBR。
            if options.is_xp_i386 {
                send_step(&progress_tx, 2, &tr!("导出驱动"), 100); // 跳过（XP 文本安装阶段不适用）
                send_step(&progress_tx, 3, &tr!("释放系统镜像"), 0);
                // 准备 XP 引导前：先清掉目标盘上【其他】分区的活动标志，确保目标成为唯一活动分区。
                // （微软文档：diskpart 的 active 不自动清旧的活动分区；同盘存在多个活动分区时 BIOS/XP 会
                //   引导分区表里第一个活动分区——往往是旧 C: → 出现“装到 W: 却进了 C:”。照搬「设新活动前先移除旧活动」。）
                deactivate_sibling_active_partitions(&target_partition, &partitions);
                let i386_src = std::path::PathBuf::from(&image_path);
                let bin_dir = crate::utils::path::get_bin_dir();
                // 用户自定义无人值守(XP 为 winnt.sif)：非空则传给引擎，原样覆盖内置生成的应答。
                let custom_sif = if options.custom_unattend_path.is_empty() {
                    None
                } else {
                    Some(std::path::PathBuf::from(&options.custom_unattend_path))
                };
                log::info!(
                    "[INSTALL i386] 从 {} 准备 XP/2003 文本安装到 {}",
                    i386_src.display(),
                    target_partition
                );
                match lr_core::xp_i386::install_from_i386(
                    &i386_src,
                    &target_partition,
                    &bin_dir,
                    custom_sif.as_deref(),
                ) {
                    Ok(log) => {
                        log::info!("[INSTALL i386] 文本安装准备完成:\n{}", log);
                        send_step(&progress_tx, 3, &tr!("释放系统镜像"), 100);
                        send_step(&progress_tx, 4, &tr!("导入驱动"), 100);
                        send_step(&progress_tx, 5, &tr!("修复引导"), 100);
                        send_step(&progress_tx, 6, &tr!("应用高级选项"), 100);
                        send_step(&progress_tx, 7, &tr!("完成安装"), 100);
                        log::info!("[INSTALL i386] 完成。重启后进入 XP/2003 蓝底文本安装阶段。");
                    }
                    Err(e) => {
                        log::error!("[INSTALL i386] 失败: {}", e);
                        // 失败原因写入 install_error → UI 显示醒目红色错误条（之前只塞进步骤名一闪而过）。
                        send_error(&progress_tx, &tr!("XP/2003 文本安装准备失败：{}", e));
                    }
                }
                return;
            }

            // Step 2: 导出驱动
            send_step(&progress_tx, 2, &tr!("导出驱动"), 0);
            std::thread::sleep(std::time::Duration::from_millis(50));
            if options.export_drivers {
                log::info!("[INSTALL STEP 2] 开始导出驱动到: {}", driver_backup_str);
                send_step(&progress_tx, 2, &tr!("导出驱动"), 20);
                
                match export_drivers(&driver_backup_str) {
                    Ok(_) => {
                        log::info!("[INSTALL STEP 2] 驱动导出成功");
                        send_step(&progress_tx, 2, &tr!("导出驱动"), 100);
                    }
                    Err(e) => {
                        log::warn!("[INSTALL STEP 2] 驱动导出失败: {} (继续安装)", e);
                        send_step(&progress_tx, 2, &tr!("导出驱动"), 100);
                    }
                }
            } else {
                log::info!("[INSTALL STEP 2] 跳过导出驱动");
                send_step(&progress_tx, 2, &tr!("导出驱动"), 100);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Step 3: 释放系统镜像
            send_step(&progress_tx, 3, &tr!("释放系统镜像"), 0);
            std::thread::sleep(std::time::Duration::from_millis(50));
            log::info!("[INSTALL STEP 3] 开始释放系统镜像");

            let image_lower = image_path.to_lowercase();
            let is_gho = image_lower.ends_with(".gho") || image_lower.ends_with(".ghs");

            if is_gho {
                log::info!("[INSTALL STEP 3] 检测到 GHO 镜像，使用 Ghost 恢复");
                
                let ghost = Ghost::new();
                
                if !ghost.is_available() {
                    log::error!("[INSTALL STEP 3] 错误: Ghost 可执行文件不存在");
                    send_step(&progress_tx, 3, &tr!("释放系统镜像"), 100);
                } else {
                    let ghost_tx = progress_tx.clone();
                    let (inner_tx, inner_rx) = mpsc::channel::<DismProgress>();
                    
                    std::thread::spawn(move || {
                        while let Ok(p) = inner_rx.recv() {
                            let _ = ghost_tx.send(p);
                        }
                    });
                    
                    match ghost.restore_image_to_letter(&image_path, &target_partition, &partitions, Some(inner_tx)) {
                        Ok(_) => log::info!("[INSTALL STEP 3] Ghost 镜像恢复成功"),
                        Err(e) => log::error!("[INSTALL STEP 3] Ghost 镜像恢复失败: {}", e),
                    }
                }
                
                send_step(&progress_tx, 3, &tr!("释放系统镜像"), 100);
            } else {
                log::info!("[INSTALL STEP 3] 使用 DISM 应用 WIM/ESD 镜像");
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
                    Ok(_) => log::info!("[INSTALL STEP 3] DISM 镜像释放成功"),
                    Err(e) => log::error!("[INSTALL STEP 3] DISM 镜像释放失败: {}", e),
                }
                send_step(&progress_tx, 3, &tr!("释放系统镜像"), 100);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Step 4: 导入驱动（仅在 AutoImport 模式下导入）
            send_step(&progress_tx, 4, &tr!("导入驱动"), 0);
            std::thread::sleep(std::time::Duration::from_millis(50));
            
            // 判断是否需要导入驱动（只有 AutoImport 模式才导入）
            let should_import = matches!(options.driver_action, crate::app::DriverAction::AutoImport);
            
            if should_import && driver_backup_path.exists() {
                log::info!("[INSTALL STEP 4] 开始导入驱动 (AutoImport模式)");
                send_step(&progress_tx, 4, &tr!("导入驱动"), 30);
                
                match import_drivers(&target_partition, &driver_backup_str) {
                    Ok(_) => {
                        log::info!("[INSTALL STEP 4] 驱动导入成功");
                        let _ = std::fs::remove_dir_all(&driver_backup_path);
                        send_step(&progress_tx, 4, &tr!("导入驱动"), 100);
                    }
                    Err(e) => {
                        log::error!("[INSTALL STEP 4] 驱动导入失败: {}", e);
                        let _ = std::fs::remove_dir_all(&driver_backup_path);
                        send_step(&progress_tx, 4, &tr!("导入驱动"), 100);
                    }
                }
            } else if matches!(options.driver_action, crate::app::DriverAction::SaveOnly) && driver_backup_path.exists() {
                // SaveOnly 模式：保留驱动备份到目标分区
                log::info!("[INSTALL STEP 4] 仅保存驱动 (SaveOnly模式)");
                send_step(&progress_tx, 4, &tr!("保存驱动"), 30);
                
                let target_driver_dir = format!("{}\\LetRecovery_Drivers", target_partition);
                if let Err(e) = copy_dir_recursive(&driver_backup_str, &target_driver_dir) {
                    log::error!("[INSTALL STEP 4] 保存驱动到目标分区失败: {}", e);
                } else {
                    log::info!("[INSTALL STEP 4] 驱动已保存到: {}", target_driver_dir);
                }
                
                let _ = std::fs::remove_dir_all(&driver_backup_path);
                send_step(&progress_tx, 4, &tr!("保存驱动"), 100);
            } else {
                log::info!("[INSTALL STEP 4] 跳过驱动处理 (driver_action: {:?})", options.driver_action);
                send_step(&progress_tx, 4, &tr!("导入驱动"), 100);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));

            // XP/2003 判定（Step 5 引导 与 Step 6 驱动注入 共用）：
            // 配置标记 或 释放后系统缺少 \Windows\Boot（该目录仅 Vista+ 才有）。
            let is_xp = options.is_xp
                || !std::path::Path::new(&format!("{}\\Windows\\Boot", target_partition)).exists();

            // Step 5: 修复引导
            send_step(&progress_tx, 5, &tr!("修复引导"), 0);
            std::thread::sleep(std::time::Duration::from_millis(50));
            if options.repair_boot {
                log::info!("[INSTALL STEP 5] 开始修复引导");
                send_step(&progress_tx, 5, &tr!("修复引导"), 20);
                
                let use_uefi = match options.boot_mode {
                    BootModeSelection::UEFI => true,
                    BootModeSelection::Legacy => false,
                    BootModeSelection::Auto => matches!(partition_style, PartitionStyle::GPT),
                };
                
                log::info!("[INSTALL STEP 5] 引导模式: {}", if use_uefi { "UEFI" } else { "Legacy" });
                send_step(&progress_tx, 5, &tr!("修复引导"), 50);

                // Legacy/MBR 写引导前：确保目标磁盘有【非 0 的唯一 MBR 签名】。
                // diskpart clean 后磁盘 MBR 签名会变成 00000000，而 Win7 的 BCD 是按【磁盘签名 + 分区偏移】
                // 定位"系统所在的卷"的——签名为 0 时 winload 解析不到该卷 → 开机 STOP 0x7B / 0xC0000034
                // (INACCESSIBLE_BOOT_DEVICE / OBJECT_NAME_NOT_FOUND)。只在签名【确为 0】时补写，绝不动已有有效签名。
                if !use_uefi {
                    if let Some(d) = target_disk {
                        match ensure_mbr_disk_signature(d) {
                            Ok(msg) => log::info!("[INSTALL STEP 5] MBR 签名: {}", msg),
                            Err(e) => log::warn!("[INSTALL STEP 5] MBR 签名检查失败（继续）: {}", e),
                        }
                    }
                }

                let boot_manager = crate::core::bcdedit::BootManager::new();
                // XP/2003 写 XP 引导；否则 bcdboot。is_xp 已在上方统一计算。
                let boot_result = if is_xp {
                    if use_uefi {
                        log::info!("[INSTALL STEP 5] 识别为 XP/2003 + UEFI，写入 XP UEFI/GPT 引导");
                        match boot_manager.write_xp_uefi_gpt_boot(&target_partition) {
                            Ok(()) => Ok(()),
                            Err(e) => {
                                log::warn!("[INSTALL STEP 5] XP UEFI 引导失败({})，回退 Legacy(ntldr)", e);
                                boot_manager.write_xp_boot(&target_partition)
                            }
                        }
                    } else {
                        log::info!("[INSTALL STEP 5] 识别为 XP/2003(Legacy)，写入 XP 引导(ntldr/boot.ini)");
                        boot_manager.write_xp_boot(&target_partition)
                    }
                } else {
                    boot_manager.repair_boot_advanced(&target_partition, use_uefi)
                };
                match boot_result {
                    Ok(_) => {
                        log::info!("[INSTALL STEP 5] 引导修复成功");
                        
                        // 如果是 Win7 + UEFI 模式，且启用了 UefiSeven 补丁
                        if use_uefi && advanced_options.win7_uefi_patch {
                            log::info!("[INSTALL STEP 5] 检测到 Win7 UEFI 补丁选项，开始应用 UefiSeven");
                            send_step(&progress_tx, 5, &tr!("应用Win7 UEFI补丁"), 70);
                            
                            match advanced_options.apply_uefiseven_patch(&target_partition) {
                                Ok(_) => log::info!("[INSTALL STEP 5] UefiSeven 补丁应用成功"),
                                Err(e) => log::warn!("[INSTALL STEP 5] UefiSeven 补丁应用失败: {} (继续安装)", e),
                            }
                        }
                    }
                    Err(e) => log::error!("[INSTALL STEP 5] 引导修复失败: {}", e),
                }
                send_step(&progress_tx, 5, &tr!("修复引导"), 100);
            } else {
                log::info!("[INSTALL STEP 5] 跳过修复引导");
                send_step(&progress_tx, 5, &tr!("修复引导"), 100);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Step 6: 应用高级选项
            send_step(&progress_tx, 6, &tr!("应用高级选项"), 0);
            std::thread::sleep(std::time::Duration::from_millis(50));
            log::info!("[INSTALL STEP 6] 应用高级选项");
            send_step(&progress_tx, 6, &tr!("应用高级选项"), 20);
            
            match advanced_options.apply_to_system(&target_partition, is_xp) {
                Ok(_) => log::info!("[INSTALL STEP 6] 高级选项应用成功"),
                Err(e) => log::error!("[INSTALL STEP 6] 高级选项应用失败: {}", e),
            }
            send_step(&progress_tx, 6, &tr!("应用高级选项"), 50);

            // 注入用户驱动：bin/drivers/<版本> 下用户放置的驱动，按目标系统版本自动注入
            // （win7/win8/win10/win11 走 DISM 离线注入；XP 由上方 apply_to_system 的 XP 注入处理）。
            inject_user_version_drivers(&target_partition, is_xp);
            send_step(&progress_tx, 6, &tr!("应用高级选项"), 70);
            
            if options.unattended_install {
                log::info!("[INSTALL STEP 6] 生成无人值守配置");
                match generate_unattend_xml(&target_partition, &advanced_options) {
                    Ok(_) => log::info!("[INSTALL STEP 6] 无人值守配置生成成功"),
                    Err(e) => log::error!("[INSTALL STEP 6] 无人值守配置生成失败: {}", e),
                }
            }
            send_step(&progress_tx, 6, &tr!("应用高级选项"), 100);
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Step 7: 完成
            send_step(&progress_tx, 7, &tr!("完成安装"), 100);
            log::info!("[INSTALL STEP 7] 安装完成!");
            log::info!("[INSTALL] ========== 安装结束 ==========");
        });
    }

    /// 通过PE安装线程
    fn start_pe_install_thread(&mut self) {
        log::info!("[INSTALL PE] ========== 开始PE安装准备 ==========");
        log::info!("[INSTALL PE] 目标分区: {}", self.install_target_partition);
        log::info!("[INSTALL PE] 镜像路径: {}", self.install_image_path);

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
        self.install_progress.current_step = tr!("检查PE环境");

        std::thread::spawn(move || {
            log::info!("[INSTALL PE THREAD] PE安装线程启动");

            // Step 1: 检查PE环境
            send_step(&progress_tx, 1, &tr!("检查PE环境"), 0);
            std::thread::sleep(std::time::Duration::from_millis(50));
            
            let pe_info = match pe_info {
                Some(pe) => pe,
                None => {
                    log::error!("[INSTALL PE STEP 1] 错误: 未选择PE环境");
                    send_step(&progress_tx, 1, &tr!("检查PE环境"), 100);
                    return;
                }
            };
            
            log::info!("[INSTALL PE STEP 1] 检查PE: {}", pe_info.display_name);
            send_step(&progress_tx, 1, &tr!("检查PE环境"), 50);
            
            let (pe_exists, pe_path) = crate::core::pe::PeManager::check_pe_exists(&pe_info.filename);
            if !pe_exists {
                log::info!("[INSTALL PE STEP 1] PE文件不存在，需要下载");
                // 这里应该触发下载，但为了简化，我们直接返回错误
                send_step(&progress_tx, 1, &tr!("检查PE环境"), 100);
                return;
            }
            
            log::info!("[INSTALL PE STEP 1] PE文件存在: {}", pe_path);
            send_step(&progress_tx, 1, &tr!("检查PE环境"), 100);
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Step 2: 安装PE引导
            send_step(&progress_tx, 2, &tr!("安装PE引导"), 0);
            std::thread::sleep(std::time::Duration::from_millis(50));
            
            log::info!("[INSTALL PE STEP 2] 安装PE引导");
            send_step(&progress_tx, 2, &tr!("安装PE引导"), 30);
            
            let pe_manager = crate::core::pe::PeManager::new();
            match pe_manager.boot_to_pe(&pe_path, &pe_info.display_name) {
                Ok(_) => log::info!("[INSTALL PE STEP 2] PE引导安装成功"),
                Err(e) => {
                    log::error!("[INSTALL PE STEP 2] PE引导安装失败: {}", e);
                    send_step(&progress_tx, 2, &tr!("安装PE引导"), 100);
                    return;
                }
            }
            send_step(&progress_tx, 2, &tr!("安装PE引导"), 100);
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Step 3: 导出驱动
            send_step(&progress_tx, 3, &tr!("导出驱动"), 0);
            std::thread::sleep(std::time::Duration::from_millis(50));
            
            // 找一个可用的数据分区来存储数据（传入镜像路径以检查空间）
            let (data_partition, _is_auto_created) = match find_data_partition(&target_partition, &image_path) {
                Ok(result) => result,
                Err(e) => {
                    log::error!("[INSTALL PE STEP 3] 查找数据分区失败: {}", e);
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
                log::info!("[INSTALL PE STEP 3] 导出驱动到: {} (driver_action: {:?})", data_dir, options.driver_action);
                send_step(&progress_tx, 3, &tr!("导出驱动"), 30);
                
                let driver_path = format!("{}\\drivers", data_dir);
                match export_drivers(&driver_path) {
                    Ok(_) => log::info!("[INSTALL PE STEP 3] 驱动导出成功"),
                    Err(e) => log::warn!("[INSTALL PE STEP 3] 驱动导出失败: {}", e),
                }
            } else {
                log::info!("[INSTALL PE STEP 3] 跳过驱动导出 (driver_action: {:?})", options.driver_action);
            }
            send_step(&progress_tx, 3, &tr!("导出驱动"), 100);
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Step 4 前置：校验源镜像完整性
            // 坏镜像在“复制几个 GB + 重启进 PE”之前就终止，省去白等；不动磁盘。
            {
                use crate::core::image_verify::{ImageVerifier, VerifyStatus};
                send_step(&progress_tx, 4, &tr!("校验镜像"), 0);
                log::info!("[INSTALL PE] 校验源镜像完整性: {}", image_path);
                let (vtx, vrx) = mpsc::channel::<crate::core::image_verify::VerifyProgress>();
                let ptx = progress_tx.clone();
                let vh = std::thread::spawn(move || {
                    while let Ok(p) = vrx.recv() {
                        send_step(&ptx, 4, &tr!("校验镜像"), p.percentage);
                    }
                });
                let vres = ImageVerifier::new().verify(&image_path, Some(vtx));
                let _ = vh.join();
                if vres.status != VerifyStatus::Valid {
                    log::error!("[INSTALL PE] 镜像校验失败: {} - {}", vres.status, vres.message);
                    let _ = progress_tx.send(DismProgress {
                        percentage: 0,
                        status: format!(
                            "ERROR:镜像校验失败：镜像可能已损坏或不完整（{}）。请重新获取镜像后重试。",
                            vres.message
                        ),
                    });
                    return;
                }
                log::info!("[INSTALL PE] 源镜像校验通过");
            }

            // Step 4: 复制镜像文件
            send_step(&progress_tx, 4, &tr!("复制镜像文件"), 0);
            std::thread::sleep(std::time::Duration::from_millis(50));

            log::info!("[INSTALL PE STEP 4] 复制镜像文件到数据分区");
            let image_filename = Path::new(&image_path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let target_image_path = format!("{}\\{}", data_dir, image_filename);
            
            // 使用带进度的复制函数
            match copy_file_with_progress(&image_path, &target_image_path, |progress| {
                send_step(&progress_tx, 4, &tr!("复制镜像文件"), progress);
            }) {
                Ok(_) => log::info!("[INSTALL PE STEP 4] 镜像复制成功: {}", target_image_path),
                Err(e) => {
                    log::error!("[INSTALL PE STEP 4] 镜像复制失败: {}", e);
                    // 发送错误状态，不是100%
                    let _ = progress_tx.send(DismProgress {
                        percentage: 0,
                        status: format!("ERROR:复制失败: {}", e),
                    });
                    return;
                }
            }
            send_step(&progress_tx, 4, &tr!("复制镜像文件"), 100);
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Step 4.5: 如果启用了 Win7 UEFI 补丁，复制 UefiSeven 文件到数据目录
            if advanced_options.win7_uefi_patch {
                log::info!("[INSTALL PE STEP 4.5] 复制 UefiSeven 文件到数据分区");
                let uefiseven_dir = format!("{}\\uefiseven", data_dir);
                let _ = std::fs::create_dir_all(&uefiseven_dir);
                
                // 从程序目录复制 UefiSeven 文件（bin\uefiseven）
                {
                    let source_uefiseven_dir = crate::utils::path::get_uefiseven_dir();
                    if source_uefiseven_dir.exists() {
                        // 复制 bootx64.efi
                        let src_efi = source_uefiseven_dir.join("bootx64.efi");
                        let dst_efi = format!("{}\\bootx64.efi", uefiseven_dir);
                        if src_efi.exists() {
                            match std::fs::copy(&src_efi, &dst_efi) {
                                Ok(_) => log::info!("[INSTALL PE STEP 4.5] 复制 UefiSeven bootx64.efi 成功"),
                                Err(e) => log::warn!("[INSTALL PE STEP 4.5] 复制 UefiSeven bootx64.efi 失败: {}", e),
                            }
                        }
                        
                        // 复制 UefiSeven.ini（如果存在）
                        let src_ini = source_uefiseven_dir.join("UefiSeven.ini");
                        let dst_ini = format!("{}\\UefiSeven.ini", uefiseven_dir);
                        if src_ini.exists() {
                            match std::fs::copy(&src_ini, &dst_ini) {
                                Ok(_) => log::info!("[INSTALL PE STEP 4.5] 复制 UefiSeven.ini 成功"),
                                Err(e) => log::warn!("[INSTALL PE STEP 4.5] 复制 UefiSeven.ini 失败: {}", e),
                            }
                        }
                    } else {
                        log::warn!("[INSTALL PE STEP 4.5] 警告: UefiSeven 源目录不存在: {}", source_uefiseven_dir.display());
                    }
                }
            }

            // 复制用户驱动文件夹（bin/drivers/{win7,win8,win10,win11}）到数据分区，
            // 供重启进 PE 后按目标系统版本注入。XP 驱动随 PE WIM 内置，无需复制。
            for ver in ["win7", "win8", "win10", "win11"] {
                let src = crate::utils::path::get_drivers_dir().join(ver);
                if !user_driver_dir_has_inf(&src) {
                    continue;
                }
                let dst = format!("{}\\user_drivers\\{}", data_dir, ver);
                match copy_dir_recursive(&src.to_string_lossy(), &dst) {
                    Ok(_) => log::info!("[INSTALL PE] 已复制用户驱动 {} -> {}", ver, dst),
                    Err(e) => log::warn!("[INSTALL PE] 复制用户驱动 {} 失败: {}", ver, e),
                }
            }

            // Step 5: 写入配置文件
            send_step(&progress_tx, 5, &tr!("写入配置文件"), 0);
            std::thread::sleep(std::time::Duration::from_millis(50));
            
            log::info!("[INSTALL PE STEP 5] 写入配置文件");
            
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
                custom_unattend_path: options.custom_unattend_path.clone(),
                win7_uefi_patch: advanced_options.win7_uefi_patch,
                win7_inject_usb3_driver: advanced_options.win7_inject_usb3_driver,
                win7_inject_nvme_driver: advanced_options.win7_inject_nvme_driver,
                win7_fix_acpi_bsod: advanced_options.win7_fix_acpi_bsod,
                win7_fix_storage_bsod: advanced_options.win7_fix_storage_bsod,
                wim_engine: lr_core::active_engine().as_u8(),
                is_xp: options.is_xp,
                xp_inject_usb3_driver: advanced_options.xp_inject_usb3_driver,
                xp_inject_nvme_driver: advanced_options.xp_inject_nvme_driver,
                run_diskpart_scripts: options.run_diskpart_scripts,
            };
            
            match ConfigFileManager::write_install_config(&target_partition, &data_partition, &install_config) {
                Ok(_) => log::info!("[INSTALL PE STEP 5] 配置文件写入成功"),
                Err(e) => log::error!("[INSTALL PE STEP 5] 配置文件写入失败: {}", e),
            }
            
            send_step(&progress_tx, 5, &tr!("写入配置文件"), 100);
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Step 6: 准备重启
            send_step(&progress_tx, 6, &tr!("准备重启"), 100);
            log::info!("[INSTALL PE STEP 6] PE安装准备完成，等待重启");
            log::info!("[INSTALL PE] ========== PE安装准备结束 ==========");
        });
    }

    fn reboot_system(&self) {
        log::info!("[INSTALL] 执行重启命令");
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

/// 发送失败消息（UI 收到后写入 install_error，显示红色错误条）。
fn send_error(tx: &mpsc::Sender<DismProgress>, msg: &str) {
    let _ = tx.send(DismProgress {
        percentage: 0,
        status: format!("ERROR:{}", msg),
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

/// 根据目标系统识别用户驱动文件夹名（win7/win8/win10/win11）。
/// XP 由现有 XP 注入机制处理（bin/drivers/xp/{ahci,nvme,usb3}），这里返回 None。
fn detect_user_driver_version(target_partition: &str, is_xp: bool) -> Option<&'static str> {
    if is_xp {
        return None;
    }
    // 读目标系统 \Windows\System32\ntdll.dll 版本：6.1=win7，6.2/6.3=win8/8.1，
    // 10.0 且 build<22000=win10，build>=22000=win11。
    let ntdll = Path::new(target_partition)
        .join("Windows")
        .join("System32")
        .join("ntdll.dll");
    let (major, minor, build, _) = crate::core::system_utils::get_file_version(&ntdll)?;
    match (major, minor) {
        (6, 1) => Some("win7"),
        (6, 2) | (6, 3) => Some("win8"),
        (10, _) => Some(if build >= 22000 { "win11" } else { "win10" }),
        _ => None,
    }
}

/// 递归判断目录下是否存在驱动 .inf 文件。
fn user_driver_dir_has_inf(dir: &Path) -> bool {
    if !dir.exists() {
        return false;
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("inf"))
                    .unwrap_or(false)
                {
                    return true;
                }
            }
        }
    }
    false
}

/// 注入 `bin/drivers/<版本>` 下用户放置的驱动到目标系统（win7/8/10/11，DISM 离线注入）。
/// 目录不存在或无 .inf 则静默跳过。失败仅记日志，不打断安装。
fn inject_user_version_drivers(target_partition: &str, is_xp: bool) {
    let version = match detect_user_driver_version(target_partition, is_xp) {
        Some(v) => v,
        None => return,
    };
    let dir = crate::utils::path::get_drivers_dir().join(version);
    if !user_driver_dir_has_inf(&dir) {
        return;
    }
    log::info!(
        "[USER DRV] 注入 bin/drivers/{} 用户驱动到 {} ...",
        version, target_partition
    );
    let dism = crate::core::dism::Dism::new();
    let image_path = format!("{}\\", target_partition);
    match dism.add_drivers_offline(&image_path, &dir.to_string_lossy()) {
        Ok(_) => log::info!("[USER DRV] bin/drivers/{} 注入成功", version),
        Err(e) => log::warn!("[USER DRV] bin/drivers/{} 注入失败: {}（继续安装）", version, e),
    }
}

/// 跑完 diskpart 脚本后，按「磁盘号:分区号」把安装目标重新定位回来（照搬 DSI 思路）。
///
/// 盘符在 PE 里会变、脚本新建的分区常常没盘符，所以安装目标不靠盘符认、靠 disk:partition 认：
/// 按 (disk_number, partition_number) 在最新分区表里找回那个分区；有盘符直接用，没盘符就分配一个
/// 空闲盘符（diskpart assign letter）后再用；该 disk:partition 真的不存在 → 返回 Err（调用方据此中止）。
/// 返回 (当前盘符如 "W:", 当前分区表类型)。拿不到磁盘/分区号（旧分区无编号信息）时回退按原盘符找。
fn resolve_install_target(
    target_disk: Option<u32>,
    target_part: Option<u32>,
    orig_letter: &str,
) -> Result<(String, PartitionStyle), String> {
    use crate::core::disk::DiskManager;

    let norm = |s: &str| s.trim_end_matches('\\').trim_end_matches(':').to_string();

    // 拿不到磁盘/分区号：回退——按原盘符在最新分区表里找（只能找到有盘符的卷）。
    let (td, tp) = match (target_disk, target_part) {
        (Some(d), Some(p)) => (d, p),
        _ => {
            let orig = norm(orig_letter);
            let parts = DiskManager::get_partitions().map_err(|e| format!("枚举分区失败: {}", e))?;
            return parts
                .iter()
                .find(|p| norm(&p.letter).eq_ignore_ascii_case(&orig))
                .map(|p| (format!("{}:", norm(&p.letter)), p.partition_style))
                .ok_or_else(|| format!("目标分区 {} 不存在，且无磁盘/分区号可重定位", orig_letter));
        }
    };

    // 1) 该 disk:partition 已经挂着盘符 → 直接用。
    //    注意 get_partitions() 是按 A-Z 盘符枚举的，只看得到【有盘符】的卷；无盘符分区在这里查不到。
    if let Ok(parts) = DiskManager::get_partitions() {
        if let Some(p) = parts
            .iter()
            .find(|p| p.disk_number == Some(td) && p.partition_number == Some(tp))
        {
            let letter = norm(&p.letter);
            if !letter.is_empty() {
                return Ok((format!("{}:", letter), p.partition_style));
            }
        }
    }

    // 2) 没盘符（diskpart 新建的分区常常没盘符，上面按盘符枚举就看不到）：
    //    直接按 disk:partition 用 diskpart 强制挂一个空闲盘符（remove 旧的再 assign，noerr 容错）。
    //    diskpart 的 select partition 作用于无盘符分区也有效；若该分区根本不存在，select 会失败、
    //    盘符挂不上，下面第 3 步据此中止。
    let free = DiskManager::find_available_drive_letter()
        .ok_or_else(|| "没有空闲盘符可分配给目标分区".to_string())?;
    let script = format!(
        "select disk {}\r\nselect partition {}\r\nremove noerr\r\nassign letter={}\r\nexit\r\n",
        td, tp, free
    );
    let tmp = std::env::temp_dir().join("lr_assign_target.txt");
    std::fs::write(&tmp, script.as_bytes()).map_err(|e| format!("写分配盘符脚本失败: {}", e))?;
    let tmp_str = tmp.to_string_lossy().into_owned();
    let dp_out = match crate::utils::cmd::create_command("diskpart")
        .args(["/s", tmp_str.as_str()])
        .output()
    {
        Ok(o) => crate::utils::encoding::gbk_to_utf8(&o.stdout),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("运行 diskpart 分配盘符失败: {}", e));
        }
    };
    let _ = std::fs::remove_file(&tmp);
    log::info!(
        "[INSTALL] 给目标 磁盘{}:分区{} 挂盘符 {}：\n{}",
        td,
        tp,
        free,
        dp_out.trim()
    );

    // 3) 验证刚挂的盘符是否生效（生效 = 该 disk:partition 确实存在且现在可用）。PE 里卷管理器可能慢，重试几次。
    let free_norm = free.to_string();
    for attempt in 0..4 {
        std::thread::sleep(std::time::Duration::from_millis(if attempt == 0 { 800 } else { 500 }));
        if let Ok(parts) = DiskManager::get_partitions() {
            if let Some(p) = parts
                .iter()
                .find(|p| norm(&p.letter).eq_ignore_ascii_case(&free_norm))
            {
                return Ok((format!("{}:", norm(&p.letter)), p.partition_style));
            }
        }
    }

    Err(format!(
        "目标分区 磁盘{}:分区{}（原 {}）在 Diskpart 脚本执行后不存在，或无法挂载盘符。\ndiskpart 输出：\n{}",
        td,
        tp,
        orig_letter,
        dp_out.trim()
    ))
}

/// 确保目标磁盘有【非 0 的唯一 MBR 磁盘签名】（Legacy/MBR 写引导前调用）。
///
/// diskpart `clean` 之后 MBR 签名会是 0x00000000；而 Win7 的 BCD 是按【磁盘签名 + 分区偏移】来定位
/// "系统所在的卷"，签名为 0 时 winload 解析不到 → 开机 STOP 0x7B / 0xC0000034。
/// 这里先用 `uniqueid disk` 读当前签名，**只在确为 0 时**用 `uniqueid disk id=<非0 8位hex>` 补一个，
/// 绝不改动已有的有效签名（解析不到 8 位十六进制——例如 GPT 的 GUID——也一律跳过，保守处理）。
fn ensure_mbr_disk_signature(disk: u32) -> Result<String, String> {
    let run = |script: &str, tag: &str| -> Result<String, String> {
        let tmp = std::env::temp_dir().join(format!("lr_sig_{}.txt", tag));
        std::fs::write(&tmp, script.as_bytes()).map_err(|e| format!("写{}脚本失败: {}", tag, e))?;
        let tmp_str = tmp.to_string_lossy().into_owned();
        let out = crate::utils::cmd::create_command("diskpart")
            .args(["/s", tmp_str.as_str()])
            .output();
        let _ = std::fs::remove_file(&tmp);
        match out {
            Ok(o) => Ok(crate::utils::encoding::gbk_to_utf8(&o.stdout)),
            Err(e) => Err(format!("运行 diskpart({}) 失败: {}", tag, e)),
        }
    };

    // 1) 读当前磁盘 ID（MBR 签名）。
    let stdout = run(&format!("select disk {}\r\nuniqueid disk\r\nexit\r\n", disk), "read")?;
    // 解析输出里的 8 位十六进制磁盘 ID（含 "ID" 的行里取第一个 8-hex token）。
    let mut current: Option<String> = None;
    for line in stdout.lines() {
        if line.to_ascii_lowercase().contains("id") {
            for tok in line.split(|c: char| !c.is_ascii_alphanumeric()) {
                if tok.len() == 8 && tok.chars().all(|c| c.is_ascii_hexdigit()) {
                    current = Some(tok.to_ascii_uppercase());
                    break;
                }
            }
        }
        if current.is_some() {
            break;
        }
    }
    match current.as_deref() {
        Some("00000000") => { /* 签名为 0，下面补写 */ }
        Some(sig) => return Ok(format!("磁盘 {} 已有 MBR 签名 {}，无需处理", disk, sig)),
        None => {
            return Ok(format!(
                "磁盘 {} 未解析到 MBR 签名（可能是 GPT/输出异常），保守跳过。输出：{}",
                disk,
                stdout.trim()
            ))
        }
    }

    // 2) 签名为 0：生成一个非 0 的 8 位十六进制签名并写入。
    let new_id = {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() ^ (d.as_secs() as u32))
            .unwrap_or(0xA1B2_C3D4);
        n | 0x1000_0000 // 保证高位非 0，整体非 0
    };
    let new_hex = format!("{:08X}", new_id);
    let so = run(
        &format!("select disk {}\r\nuniqueid disk id={}\r\nexit\r\n", disk, new_hex),
        "set",
    )?;
    Ok(format!(
        "磁盘 {} 原 MBR 签名为 0，已写入新签名 {}（diskpart: {}）",
        disk,
        new_hex,
        so.trim()
    ))
}

/// 格式化分区（用 diskpart，而不是 format.com）
fn format_partition(partition: &str) -> anyhow::Result<()> {
    use crate::utils::cmd::create_command;

    let letter = partition.trim_end_matches('\\').trim_end_matches(':');
    log::info!("[FORMAT] 用 diskpart 格式化分区: {} (volume {})", partition, letter);

    // 用 diskpart 格式化，而不是 format.com 管道喂提示：
    // format.com 在【卷已有卷标】时会先问「输入驱动器 X: 的当前卷标」做安全确认，喂 y 会被当成卷标 →
    // 报「卷标不正确」中止（实机 C:(卷标 DATA) 就栽在这）。diskpart 不问卷标，直接快速格式化为 NTFS；
    // override 在卷被占用（如 PE 资源管理器正浏览该盘）时强制卸载文件系统后再格式化，且不丢盘符。
    let script =
        format!("select volume {letter}\r\nformat fs=ntfs quick override\r\nexit\r\n");
    let tmp = std::env::temp_dir().join("lr_xp_format.txt");
    std::fs::write(&tmp, script.as_bytes())?;
    let tmp_str = tmp.to_string_lossy().into_owned();
    let output = create_command("diskpart")
        .args(["/s", tmp_str.as_str()])
        .output()?;
    let _ = std::fs::remove_file(&tmp);

    let stdout = crate::utils::encoding::gbk_to_utf8(&output.stdout);
    let stderr = crate::utils::encoding::gbk_to_utf8(&output.stderr);
    log::info!("[FORMAT] diskpart stdout:\n{}", stdout);
    log::info!("[FORMAT] diskpart stderr:\n{}", stderr);

    // diskpart 即使出错也常返回 0，故按输出判断成功：含「成功格式化 / successfully formatted」才算成功。
    let lo = stdout.to_ascii_lowercase();
    if stdout.contains("成功格式化") || lo.contains("successfully formatted") {
        log::info!("[FORMAT] diskpart 格式化成功");
        return Ok(());
    }

    // 失败：diskpart 把具体原因打印在 stdout（如找不到卷/卷被锁定）。把完整输出回报，便于定位。
    let reason = format!("{}\n{}", stdout.trim(), stderr.trim());
    let reason = reason.trim();
    let reason = if reason.is_empty() {
        "diskpart 无输出，格式化失败。请确认该分区未被占用后重试。"
    } else {
        reason
    };
    anyhow::bail!("格式化失败（diskpart）: {}", reason);
}

/// 把目标分区所在【同一物理盘】上其他分区的活动标志清掉，确保目标成为唯一活动分区。
///
/// 微软文档：diskpart 的 `active` 命令不会自动清掉旧的活动分区，而 MBR 盘只应有一个活动分区；
/// 同盘存在多个活动分区时，BIOS/XP 安装程序会引导/认定分区表里【第一个枚举到的活动分区】(往往是旧 C:)，
/// 于是“装到 W: 却进了 C:”。这里在准备 XP 引导前，照搬微软「设新活动前先移除旧活动」，先清同盘其他活动标志。
///
/// 逐个分区单独跑 diskpart（一条失败不影响其他）；`inactive` 对逻辑盘/非活动分区会被 diskpart 忽略或报错，
/// 均按【尽力而为】处理、失败仅记日志不阻断。只处理【有盘符、同物理盘、非目标】的分区。
fn deactivate_sibling_active_partitions(target: &str, partitions: &[Partition]) {
    let tl = target.trim_end_matches('\\').trim_end_matches(':');
    let target_disk = partitions
        .iter()
        .find(|p| {
            p.letter
                .trim_end_matches('\\')
                .trim_end_matches(':')
                .eq_ignore_ascii_case(tl)
        })
        .and_then(|p| p.disk_number);
    let target_disk = match target_disk {
        Some(d) => d,
        None => {
            log::warn!("[ACTIVE] 未能确定 {} 的物理磁盘号，跳过清同盘其他活动标志", target);
            return;
        }
    };
    for p in partitions {
        let pl = p.letter.trim_end_matches('\\').trim_end_matches(':');
        if p.disk_number == Some(target_disk) && !pl.is_empty() && !pl.eq_ignore_ascii_case(tl) {
            let script = format!("select volume {}\r\ninactive\r\nexit\r\n", pl);
            let tmp = std::env::temp_dir().join("lr_xp_deactivate.txt");
            if std::fs::write(&tmp, script.as_bytes()).is_err() {
                continue;
            }
            let tmp_str = tmp.to_string_lossy().into_owned();
            let out = crate::utils::cmd::create_command("diskpart")
                .args(["/s", tmp_str.as_str()])
                .output();
            let _ = std::fs::remove_file(&tmp);
            match out {
                Ok(o) => log::info!(
                    "[ACTIVE] 已尝试清同盘分区 {}: 的活动标志（disk {}）：{}",
                    pl,
                    target_disk,
                    crate::utils::encoding::gbk_to_utf8(&o.stdout).trim()
                ),
                Err(e) => log::warn!("[ACTIVE] 清 {}: 活动标志失败（忽略）：{}", pl, e),
            }
        }
    }
}

/// 导出驱动
fn export_drivers(destination: &str) -> anyhow::Result<()> {
    log::info!("[DRIVER EXPORT] 目标路径: {}", destination);
    
    if Path::new(destination).exists() {
        let _ = std::fs::remove_dir_all(destination);
    }
    
    std::fs::create_dir_all(destination)?;
    
    let dism = crate::core::dism::Dism::new();
    
    if dism.is_pe_environment() {
        log::info!("[DRIVER EXPORT] PE 环境，查找现有 Windows 系统...");
        
        for letter in ['C', 'D', 'E', 'F', 'G'] {
            let windows_path = format!("{}:\\Windows\\System32\\drivers", letter);
            if Path::new(&windows_path).exists() {
                log::info!("[DRIVER EXPORT] 尝试从 {}: 导出驱动", letter);
                let source = format!("{}:\\", letter);
                match dism.export_drivers_from_system(&source, destination) {
                    Ok(_) => {
                        log::info!("[DRIVER EXPORT] 成功从 {}: 导出驱动", letter);
                        return Ok(());
                    }
                    Err(e) => {
                        log::warn!("[DRIVER EXPORT] 从 {}: 导出失败: {}", letter, e);
                    }
                }
            }
        }
        
        anyhow::bail!("PE 环境下未找到可用的 Windows 系统来导出驱动")
    } else {
        log::info!("[DRIVER EXPORT] 桌面环境，使用在线模式导出");
        dism.export_drivers(destination)
    }
}

/// 导入驱动到目标系统
fn import_drivers(target_partition: &str, driver_path: &str) -> anyhow::Result<()> {
    log::info!("[DRIVER IMPORT] 目标分区: {}, 驱动路径: {}", target_partition, driver_path);
    
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
    
    log::info!("[UNATTEND] 生成无人值守配置文件");
    
    let username = if options.custom_username && !options.username.is_empty() {
        options.username.clone()
    } else {
        "User".to_string()
    };

    // 检测目标系统架构
    let arch = get_system_architecture(target_partition);
    let arch_str = arch.as_unattend_str();
    log::info!("[UNATTEND] 检测到目标系统架构: {}", arch_str);

    // 通过 ntdll.dll 文件版本检测目标系统版本
    // Windows 7: 6.1.x, Windows 8: 6.2.x, Windows 8.1: 6.3.x, Windows 10/11: 10.0.x
    let ntdll_path = Path::new(target_partition).join("Windows").join("System32").join("ntdll.dll");
    let (is_win7, is_win8) = match get_file_version(&ntdll_path) {
        Some((major, minor, build, _)) => {
            log::info!("[UNATTEND] 检测到目标系统版本 (ntdll.dll): {}.{}.{}", major, minor, build);
            
            let is_win7 = major == 6 && minor == 1;
            let is_win8 = major == 6 && (minor == 2 || minor == 3);
            (is_win7, is_win8)
        }
        None => {
            log::warn!("[UNATTEND] 无法读取 ntdll.dll 版本: {:?}, 默认使用 Win10/11 配置", ntdll_path);
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
                        <Name>{username}</Name>
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
    log::info!("[UNATTEND] 已写入: {}", unattend_path);
    
    let sysprep_dir = format!("{}\\Windows\\System32\\Sysprep", target_partition);
    if Path::new(&sysprep_dir).exists() {
        let sysprep_unattend = format!("{}\\unattend.xml", sysprep_dir);
        match std::fs::write(&sysprep_unattend, &xml_content) {
            Ok(_) => log::info!("[UNATTEND] 已写入: {}", sysprep_unattend),
            Err(e) => log::warn!("[UNATTEND] 警告：写入 Sysprep unattend 失败: {} ({})", sysprep_unattend, e),
        }
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
            return Err(tr!("无法获取镜像文件大小: {}", e));
        }
    };
    
    log::info!("[DATA PARTITION] 镜像文件大小: {} bytes ({:.2} GB)",
        image_size,
        image_size as f64 / 1024.0 / 1024.0 / 1024.0
    );

    // 调用 DiskManager 的新函数
    match DiskManager::find_suitable_data_partition(exclude_partition, image_size) {
        Ok(Some((partition, is_auto_created))) => {
            log::info!("[DATA PARTITION] 选择分区: {}, 自动创建: {}", partition, is_auto_created);
            Ok((partition, is_auto_created))
        }
        Ok(None) => {
            Err(tr!("没有找到可用的数据分区，且无法自动创建"))
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

    log::info!("[COPY] 开始复制: {} -> {}", src, dst);

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
            log::debug!("[COPY] 进度: {}% ({}/{})", progress, copied, total_size);
        }
    }

    writer.flush()?;
    progress_callback(100);
    log::info!("[COPY] 复制完成: {}", dst);
    
    Ok(())
}
