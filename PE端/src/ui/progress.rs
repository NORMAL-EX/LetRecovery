/// 安装/备份步骤
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallStep {
    VerifyImage,
    FormatPartition,
    ApplyImage,
    ImportDrivers,
    InstallCabPackages,
    RepairBoot,
    ApplyAdvancedOptions,
    GenerateUnattend,
    Cleanup,
    Complete,
}

impl InstallStep {
    pub fn name(&self) -> &'static str {
        match self {
            InstallStep::VerifyImage => "校验镜像",
            InstallStep::FormatPartition => "格式化分区",
            InstallStep::ApplyImage => "释放系统镜像",
            InstallStep::ImportDrivers => "导入驱动",
            InstallStep::InstallCabPackages => "安装更新包",
            InstallStep::RepairBoot => "修复引导",
            InstallStep::ApplyAdvancedOptions => "应用高级选项",
            InstallStep::GenerateUnattend => "生成无人值守配置",
            InstallStep::Cleanup => "清理临时文件",
            InstallStep::Complete => "完成安装",
        }
    }

    pub fn index(&self) -> usize {
        match self {
            InstallStep::VerifyImage => 0,
            InstallStep::FormatPartition => 1,
            InstallStep::ApplyImage => 2,
            InstallStep::ImportDrivers => 3,
            InstallStep::InstallCabPackages => 4,
            InstallStep::RepairBoot => 5,
            InstallStep::ApplyAdvancedOptions => 6,
            InstallStep::GenerateUnattend => 7,
            InstallStep::Cleanup => 8,
            InstallStep::Complete => 9,
        }
    }

    pub fn all() -> Vec<InstallStep> {
        vec![
            InstallStep::VerifyImage,
            InstallStep::FormatPartition,
            InstallStep::ApplyImage,
            InstallStep::ImportDrivers,
            InstallStep::InstallCabPackages,
            InstallStep::RepairBoot,
            InstallStep::ApplyAdvancedOptions,
            InstallStep::GenerateUnattend,
            InstallStep::Cleanup,
            InstallStep::Complete,
        ]
    }

    /// Overall progress is weighted by expected wall-clock cost. Image application owns most of
    /// the bar; fast validation/format/boot steps must not make the UI jump to 50% before the long
    /// operation has even started.
    const fn overall_range(self) -> (u8, u8) {
        match self {
            Self::VerifyImage => (0, 3),
            Self::FormatPartition => (3, 7),
            Self::ApplyImage => (7, 82),
            Self::ImportDrivers => (82, 88),
            Self::InstallCabPackages => (88, 92),
            Self::RepairBoot => (92, 96),
            Self::ApplyAdvancedOptions => (96, 98),
            Self::GenerateUnattend => (98, 99),
            Self::Cleanup => (99, 100),
            Self::Complete => (100, 100),
        }
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

    /// Capture dominates backup duration; setup and cleanup deliberately occupy only a few points.
    const fn overall_range(self) -> (u8, u8) {
        match self {
            Self::ReadConfig => (0, 3),
            Self::CaptureImage => (3, 93),
            Self::VerifyBackup => (93, 97),
            Self::RepairBoot => (97, 99),
            Self::Cleanup => (99, 100),
            Self::Complete => (100, 100),
        }
    }
}

const fn weighted_progress(range: (u8, u8), step_progress: u8) -> u8 {
    let (start, end) = range;
    let span = end.saturating_sub(start) as u16;
    let progress = if step_progress > 100 {
        100
    } else {
        step_progress
    };
    let value = start as u16 + (span * progress as u16) / 100;
    if value > 100 {
        100
    } else {
        value as u8
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
    /// 是否为扩容模式（无损扩大系统盘）。为真时只显示状态+总进度，不显示安装/备份步骤列表。
    pub is_expand_mode: bool,
    /// 当前安装步骤
    pub current_install_step: InstallStep,
    /// 当前备份步骤
    pub current_backup_step: BackupStep,
    /// Worker 尚未发出首个步骤消息时保持 false，避免首帧显示错误步骤。
    pub has_current_step: bool,
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
            is_expand_mode: false,
            current_install_step: InstallStep::VerifyImage,
            current_backup_step: BackupStep::ReadConfig,
            has_current_step: false,
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

    pub fn new_expand() -> Self {
        Self {
            is_install_mode: false,
            is_expand_mode: true,
            ..Default::default()
        }
    }

    /// 设置当前安装步骤
    pub fn set_install_step(&mut self, step: InstallStep) {
        self.current_install_step = step;
        self.has_current_step = true;
        self.step_progress = 0;
        self.update_overall_progress();
    }

    /// 设置当前备份步骤
    pub fn set_backup_step(&mut self, step: BackupStep) {
        self.current_backup_step = step;
        self.has_current_step = true;
        self.step_progress = 0;
        self.update_overall_progress();
    }

    /// 更新步骤进度
    pub fn set_step_progress(&mut self, progress: u8) {
        self.step_progress = progress.min(100);
        if self.is_expand_mode {
            self.overall_progress = self.step_progress;
        } else {
            self.update_overall_progress();
        }
    }

    /// 更新总体进度
    fn update_overall_progress(&mut self) {
        if !self.has_current_step {
            self.overall_progress = 0;
            return;
        }
        if self.is_install_mode {
            self.overall_progress = weighted_progress(
                self.current_install_step.overall_range(),
                self.step_progress,
            );
        } else {
            self.overall_progress =
                weighted_progress(self.current_backup_step.overall_range(), self.step_progress);
        }
    }

    /// 标记完成
    pub fn mark_completed(&mut self) {
        self.is_completed = true;
        self.overall_progress = 100;
        self.step_progress = 100;
        self.has_current_step = !self.is_expand_mode;
        if !self.is_expand_mode {
            if self.is_install_mode {
                self.current_install_step = InstallStep::Complete;
            } else {
                self.current_backup_step = BackupStep::Complete;
            }
        }
    }

    /// 标记失败
    pub fn mark_failed(&mut self, error: &str) {
        self.is_failed = true;
        self.error_message = Some(error.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::{BackupStep, InstallStep, ProgressState};
    use crate::utils::i18n::LanguageFile;

    #[test]
    fn english_catalog_covers_every_dynamic_progress_step() {
        let catalog: LanguageFile =
            serde_json::from_str(include_str!("../../../assets/release/lang/en-US.json"))
                .expect("embedded en-US catalog must be valid JSON");

        for key in InstallStep::all()
            .into_iter()
            .map(|step| step.name())
            .chain(BackupStep::all().into_iter().map(|step| step.name()))
        {
            let translation = catalog
                .data
                .get(key)
                .unwrap_or_else(|| panic!("missing en-US progress step translation: {key}"));
            assert_ne!(translation, key, "progress step must not remain Chinese");
        }
    }

    #[test]
    fn image_work_owns_the_majority_of_install_and_backup_progress() {
        let mut install = ProgressState::new_install();
        install.set_install_step(InstallStep::ApplyImage);
        assert_eq!(install.overall_progress, 7);
        install.set_step_progress(50);
        assert_eq!(install.overall_progress, 44);
        install.set_step_progress(100);
        assert_eq!(install.overall_progress, 82);

        let mut backup = ProgressState::new_backup();
        backup.set_backup_step(BackupStep::CaptureImage);
        assert_eq!(backup.overall_progress, 3);
        backup.set_step_progress(50);
        assert_eq!(backup.overall_progress, 48);
        backup.set_step_progress(100);
        assert_eq!(backup.overall_progress, 93);
    }
}
