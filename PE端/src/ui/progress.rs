use egui::{Color32, RichText};

/// 安装/备份步骤
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallStep {
    FormatPartition,
    ApplyImage,
    ImportDrivers,
    RepairBoot,
    ApplyAdvancedOptions,
    GenerateUnattend,
    Cleanup,
    Complete,
}

impl InstallStep {
    pub fn name(&self) -> &'static str {
        match self {
            InstallStep::FormatPartition => "格式化分区",
            InstallStep::ApplyImage => "释放系统镜像",
            InstallStep::ImportDrivers => "导入驱动",
            InstallStep::RepairBoot => "修复引导",
            InstallStep::ApplyAdvancedOptions => "应用高级选项",
            InstallStep::GenerateUnattend => "生成无人值守配置",
            InstallStep::Cleanup => "清理临时文件",
            InstallStep::Complete => "完成安装",
        }
    }

    pub fn index(&self) -> usize {
        match self {
            InstallStep::FormatPartition => 0,
            InstallStep::ApplyImage => 1,
            InstallStep::ImportDrivers => 2,
            InstallStep::RepairBoot => 3,
            InstallStep::ApplyAdvancedOptions => 4,
            InstallStep::GenerateUnattend => 5,
            InstallStep::Cleanup => 6,
            InstallStep::Complete => 7,
        }
    }

    pub fn total() -> usize {
        8
    }

    pub fn all() -> Vec<InstallStep> {
        vec![
            InstallStep::FormatPartition,
            InstallStep::ApplyImage,
            InstallStep::ImportDrivers,
            InstallStep::RepairBoot,
            InstallStep::ApplyAdvancedOptions,
            InstallStep::GenerateUnattend,
            InstallStep::Cleanup,
            InstallStep::Complete,
        ]
    }
}

/// 备份步骤
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupStep {
    ReadConfig,
    CaptureImage,
    VerifyBackup,
    RepairBoot,
    Cleanup,
    Complete,
}

impl BackupStep {
    pub fn name(&self) -> &'static str {
        match self {
            BackupStep::ReadConfig => "读取配置",
            BackupStep::CaptureImage => "执行DISM备份",
            BackupStep::VerifyBackup => "验证备份文件",
            BackupStep::RepairBoot => "恢复引导",
            BackupStep::Cleanup => "清理临时文件",
            BackupStep::Complete => "备份完成",
        }
    }

    pub fn index(&self) -> usize {
        match self {
            BackupStep::ReadConfig => 0,
            BackupStep::CaptureImage => 1,
            BackupStep::VerifyBackup => 2,
            BackupStep::RepairBoot => 3,
            BackupStep::Cleanup => 4,
            BackupStep::Complete => 5,
        }
    }

    pub fn total() -> usize {
        6
    }

    pub fn all() -> Vec<BackupStep> {
        vec![
            BackupStep::ReadConfig,
            BackupStep::CaptureImage,
            BackupStep::VerifyBackup,
            BackupStep::RepairBoot,
            BackupStep::Cleanup,
            BackupStep::Complete,
        ]
    }
}

/// 步骤状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

/// 进度状态
#[derive(Debug, Clone)]
pub struct ProgressState {
    /// 是否为安装模式（否则为备份模式）
    pub is_install_mode: bool,
    /// 当前安装步骤
    pub current_install_step: InstallStep,
    /// 当前备份步骤
    pub current_backup_step: BackupStep,
    /// 当前步骤进度 (0-100)
    pub step_progress: u8,
    /// 总体进度 (0-100)
    pub overall_progress: u8,
    /// 状态消息
    pub status_message: String,
    /// 是否已完成
    pub is_completed: bool,
    /// 是否失败
    pub is_failed: bool,
    /// 错误信息
    pub error_message: Option<String>,
}

impl Default for ProgressState {
    fn default() -> Self {
        Self {
            is_install_mode: true,
            current_install_step: InstallStep::FormatPartition,
            current_backup_step: BackupStep::ReadConfig,
            step_progress: 0,
            overall_progress: 0,
            status_message: String::new(),
            is_completed: false,
            is_failed: false,
            error_message: None,
        }
    }
}

impl ProgressState {
    pub fn new_install() -> Self {
        Self {
            is_install_mode: true,
            ..Default::default()
        }
    }

    pub fn new_backup() -> Self {
        Self {
            is_install_mode: false,
            ..Default::default()
        }
    }

    /// 设置当前安装步骤
    pub fn set_install_step(&mut self, step: InstallStep) {
        self.current_install_step = step;
        self.step_progress = 0;
        self.update_overall_progress();
    }

    /// 设置当前备份步骤
    pub fn set_backup_step(&mut self, step: BackupStep) {
        self.current_backup_step = step;
        self.step_progress = 0;
        self.update_overall_progress();
    }

    /// 更新步骤进度
    pub fn set_step_progress(&mut self, progress: u8) {
        self.step_progress = progress.min(100);
        self.update_overall_progress();
    }

    /// 更新总体进度
    fn update_overall_progress(&mut self) {
        if self.is_install_mode {
            let step_idx = self.current_install_step.index();
            let total = InstallStep::total();
            let base = (step_idx * 100) / total;
            let step_contribution = (self.step_progress as usize) / total;
            self.overall_progress = (base + step_contribution).min(100) as u8;
        } else {
            let step_idx = self.current_backup_step.index();
            let total = BackupStep::total();
            let base = (step_idx * 100) / total;
            let step_contribution = (self.step_progress as usize) / total;
            self.overall_progress = (base + step_contribution).min(100) as u8;
        }
    }

    /// 标记完成
    pub fn mark_completed(&mut self) {
        self.is_completed = true;
        self.overall_progress = 100;
        self.step_progress = 100;
        if self.is_install_mode {
            self.current_install_step = InstallStep::Complete;
        } else {
            self.current_backup_step = BackupStep::Complete;
        }
    }

    /// 标记失败
    pub fn mark_failed(&mut self, error: &str) {
        self.is_failed = true;
        self.error_message = Some(error.to_string());
    }
}

/// 进度界面组件
pub struct ProgressUI;

impl ProgressUI {
    /// 绘制进度界面
    pub fn show(ui: &mut egui::Ui, state: &ProgressState) {
        ui.vertical_centered(|ui| {
            ui.add_space(20.0);

            // 标题
            let title = if state.is_install_mode {
                "LetRecovery PE 安装助手"
            } else {
                "LetRecovery PE 备份助手"
            };
            ui.heading(RichText::new(title).size(24.0).strong());

            ui.add_space(30.0);

            // 当前步骤
            let current_step_name = if state.is_install_mode {
                state.current_install_step.name()
            } else {
                state.current_backup_step.name()
            };
            ui.label(
                RichText::new(format!("当前步骤: [{}]", current_step_name))
                    .size(16.0)
                    .color(Color32::from_rgb(100, 180, 255)),
            );

            ui.add_space(20.0);

            // 步骤进度条
            ui.horizontal(|ui| {
                ui.label("步骤进度:");
                let progress = state.step_progress as f32 / 100.0;
                ui.add(
                    egui::ProgressBar::new(progress)
                        .desired_width(400.0)
                        .show_percentage(),
                );
            });

            ui.add_space(10.0);

            // 总体进度条
            ui.horizontal(|ui| {
                ui.label("总体进度:");
                let progress = state.overall_progress as f32 / 100.0;
                ui.add(
                    egui::ProgressBar::new(progress)
                        .desired_width(400.0)
                        .show_percentage(),
                );
            });

            ui.add_space(30.0);

            // 分隔线
            ui.separator();

            ui.add_space(20.0);

            // 步骤列表
            if state.is_install_mode {
                Self::show_install_steps(ui, state);
            } else {
                Self::show_backup_steps(ui, state);
            }

            // 状态消息
            if !state.status_message.is_empty() {
                ui.add_space(20.0);
                ui.label(
                    RichText::new(&state.status_message)
                        .size(14.0)
                        .color(Color32::from_rgb(180, 180, 180)),
                );
            }

            // 错误信息
            if let Some(ref error) = state.error_message {
                ui.add_space(20.0);
                ui.label(
                    RichText::new(format!("错误: {}", error))
                        .size(14.0)
                        .color(Color32::from_rgb(255, 100, 100)),
                );
            }

            // 完成提示
            if state.is_completed {
                ui.add_space(30.0);
                let message = if state.is_install_mode {
                    "系统安装完成！即将重启..."
                } else {
                    "系统备份完成！即将重启..."
                };
                ui.label(
                    RichText::new(message)
                        .size(18.0)
                        .color(Color32::from_rgb(100, 255, 100))
                        .strong(),
                );
            }
        });
    }

    /// 显示安装步骤列表
    fn show_install_steps(ui: &mut egui::Ui, state: &ProgressState) {
        let current_idx = state.current_install_step.index();

        for step in InstallStep::all() {
            let idx = step.index();
            let status = if state.is_failed && idx == current_idx {
                StepStatus::Failed
            } else if idx < current_idx || (idx == current_idx && state.step_progress == 100) {
                StepStatus::Completed
            } else if idx == current_idx {
                StepStatus::InProgress
            } else {
                StepStatus::Pending
            };

            Self::show_step_item(ui, step.name(), status);
        }
    }

    /// 显示备份步骤列表
    fn show_backup_steps(ui: &mut egui::Ui, state: &ProgressState) {
        let current_idx = state.current_backup_step.index();

        for step in BackupStep::all() {
            let idx = step.index();
            let status = if state.is_failed && idx == current_idx {
                StepStatus::Failed
            } else if idx < current_idx || (idx == current_idx && state.step_progress == 100) {
                StepStatus::Completed
            } else if idx == current_idx {
                StepStatus::InProgress
            } else {
                StepStatus::Pending
            };

            Self::show_step_item(ui, step.name(), status);
        }
    }

    /// 显示单个步骤项
    fn show_step_item(ui: &mut egui::Ui, name: &str, status: StepStatus) {
        ui.horizontal(|ui| {
            ui.add_space(50.0);

            let (icon, color) = match status {
                StepStatus::Completed => ("OK", Color32::from_rgb(100, 255, 100)),
                StepStatus::InProgress => (">>", Color32::from_rgb(255, 180, 50)),
                StepStatus::Pending => ("  ", Color32::from_rgb(128, 128, 128)),
                StepStatus::Failed => ("!!", Color32::from_rgb(255, 100, 100)),
            };

            ui.label(RichText::new(icon).size(14.0).color(color).monospace());
            ui.add_space(10.0);
            ui.label(RichText::new(name).size(14.0).color(color));
        });
        ui.add_space(5.0);
    }
}
