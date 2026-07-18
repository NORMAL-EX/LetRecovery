use lr_core::boot_pca::BootPcaMode;
use lr_core::operation::OperationStatus;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::HFONT;
use windows::Win32::UI::Controls::{
    LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_TEXT, LVITEMW, LVM_DELETEALLITEMS, LVM_INSERTCOLUMNW,
    LVM_INSERTITEMW, LVM_SETCOLUMNWIDTH, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMTEXTW,
    LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, MoveWindow, SendMessageW, SetWindowTextW, ShowWindow, HMENU, SW_HIDE, SW_SHOW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_SETFONT, WS_CHILD, WS_VISIBLE,
};

use crate::app::WorkflowRecoverySnapshot;
use crate::core::config::{DriverActionMode, InstallConfig};
use crate::ui::progress::ProgressState;

use super::controls::{create_control, wide, NativeControlKind};
use super::state::{NativePage, WorkflowKind};
use super::theme::ThemeContext;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DetailRow {
    pub label: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AdvancedOptionsSummary {
    pub rows: Vec<DetailRow>,
}

impl AdvancedOptionsSummary {
    pub(crate) fn from_install_config(config: &InstallConfig) -> Self {
        let mut rows = vec![
            row(
                crate::tr!("无人值守安装"),
                enabled_text(config.unattended || !config.custom_unattend_file.is_empty()),
            ),
            row(crate::tr!("驱动处理"), driver_action_text(config)),
            row(
                crate::tr!("安装 CAB 更新包"),
                enabled_text(config.install_cab_packages),
            ),
            row(
                crate::tr!("移除快捷方式小箭头"),
                enabled_text(config.remove_shortcut_arrow),
            ),
            row(
                crate::tr!("恢复经典右键菜单"),
                enabled_text(config.restore_classic_context_menu),
            ),
            row(
                crate::tr!("跳过 Windows 11 联网要求"),
                enabled_text(config.bypass_nro),
            ),
            row(
                crate::tr!("禁用 Windows Update"),
                enabled_text(config.disable_windows_update),
            ),
            row(
                crate::tr!("深度移除 Defender 杀毒引擎"),
                enabled_text(config.disable_windows_defender),
            ),
            row(
                crate::tr!("禁用保留存储"),
                enabled_text(config.disable_reserved_storage),
            ),
            row(
                crate::tr!("禁用用户账户控制 (UAC)"),
                enabled_text(config.disable_uac),
            ),
            row(
                crate::tr!("禁用设备自动加密"),
                enabled_text(config.disable_device_encryption),
            ),
            row(
                crate::tr!("移除预装 UWP 应用"),
                enabled_text(config.remove_uwp_apps),
            ),
            row(
                crate::tr!("导入存储控制器驱动"),
                enabled_text(config.import_storage_controller_drivers),
            ),
            row(
                crate::tr!("自定义用户名"),
                configured_text(&config.custom_username),
            ),
            row(
                crate::tr!("自定义系统盘卷标"),
                configured_text(&config.volume_label),
            ),
            row(
                crate::tr!("自定义无人值守文件"),
                configured_text(&config.custom_unattend_file),
            ),
            row(crate::tr!("启动模式"), boot_mode_text(config.boot_mode)),
            row(crate::tr!("启动签名"), pca_mode_text(config.boot_pca_mode)),
        ];
        if config.is_xp {
            rows.push(row(
                crate::tr!("XP 注入 USB3 驱动"),
                enabled_text(config.xp_inject_usb3_driver),
            ));
            rows.push(row(
                crate::tr!("XP 注入 NVMe 驱动"),
                enabled_text(config.xp_inject_nvme_driver),
            ));
        } else if config.win7_uefi_patch
            || config.win7_inject_usb3_driver
            || config.win7_inject_nvme_driver
            || config.win7_fix_acpi_bsod
            || config.win7_fix_storage_bsod
        {
            rows.extend([
                row(
                    crate::tr!("Windows 7 UEFI 补丁"),
                    enabled_text(config.win7_uefi_patch),
                ),
                row(
                    crate::tr!("Windows 7 注入 USB3 驱动"),
                    enabled_text(config.win7_inject_usb3_driver),
                ),
                row(
                    crate::tr!("Windows 7 注入 NVMe 驱动"),
                    enabled_text(config.win7_inject_nvme_driver),
                ),
                row(
                    crate::tr!("Windows 7 修复 ACPI 蓝屏"),
                    enabled_text(config.win7_fix_acpi_bsod),
                ),
                row(
                    crate::tr!("Windows 7 修复存储控制器蓝屏"),
                    enabled_text(config.win7_fix_storage_bsod),
                ),
            ]);
        }
        Self { rows }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DetailPageContent {
    pub title: String,
    pub subtitle: String,
    pub rows: Vec<DetailRow>,
    pub note: String,
}

pub(crate) fn page_content(
    page: NativePage,
    workflow: WorkflowKind,
    progress: &ProgressState,
    recovery: &WorkflowRecoverySnapshot,
    advanced: Option<&AdvancedOptionsSummary>,
) -> DetailPageContent {
    match page {
        NativePage::AdvancedOptions => DetailPageContent {
            title: crate::tr!("高级选项"),
            subtitle: crate::tr!("以下选项来自本次已校验的安装配置，PE 端仅供核对。"),
            rows: advanced
                .map(|summary| summary.rows.clone())
                .unwrap_or_else(|| {
                    vec![row(
                        crate::tr!("状态"),
                        crate::tr!("本次任务没有可显示的安装高级选项。"),
                    )]
                }),
            note: crate::tr!("此页面不会修改配置，也不会重新启动安装任务。"),
        },
        NativePage::Error => DetailPageContent {
            title: crate::tr!("操作失败"),
            subtitle: crate::tr!("任务已停止，请根据以下信息检查环境。"),
            rows: vec![
                row(crate::tr!("任务类型"), workflow_text(workflow)),
                row(
                    crate::tr!("失败阶段"),
                    current_step_text(progress, workflow),
                ),
                row(crate::tr!("结果"), crate::tr!("操作未完成")),
                row(
                    crate::tr!("诊断信息"),
                    crate::tr!("详细错误已写入 LetRecoveryPE.log。"),
                ),
            ],
            note: crate::tr!("请修正问题后从原入口重新开始；程序不会自动重试非幂等步骤。"),
        },
        NativePage::Recovery => recovery_content(workflow, progress, recovery),
        NativePage::Overview | NativePage::Progress => DetailPageContent {
            title: String::new(),
            subtitle: String::new(),
            rows: Vec::new(),
            note: String::new(),
        },
    }
}

fn recovery_content(
    workflow: WorkflowKind,
    progress: &ProgressState,
    recovery: &WorkflowRecoverySnapshot,
) -> DetailPageContent {
    let mut rows = vec![
        row(crate::tr!("任务类型"), workflow_text(workflow)),
        row(
            crate::tr!("当前步骤"),
            current_step_text(progress, workflow),
        ),
        row(
            crate::tr!("总体进度"),
            format!("{}%", progress.overall_progress),
        ),
        row(crate::tr!("工作线程"), worker_state_text(recovery)),
    ];
    if let Some(checkpoint) = &recovery.checkpoint {
        rows.extend([
            row(
                crate::tr!("检查点状态"),
                checkpoint_status_text(checkpoint.status),
            ),
            row(
                crate::tr!("检查点步骤"),
                checkpoint
                    .current_step
                    .as_deref()
                    .map(|step| crate::tr!(step))
                    .unwrap_or_else(|| crate::tr!("尚未记录步骤")),
            ),
            row(crate::tr!("检查点修订"), checkpoint.revision.to_string()),
            row(
                crate::tr!("上次任务中断记录"),
                if checkpoint.previous_interrupted {
                    crate::tr!("已生成脱敏诊断摘要")
                } else {
                    crate::tr!("未检测到")
                },
            ),
            row(
                crate::tr!("支持信息"),
                if checkpoint.support_bundle_available {
                    crate::tr!("已生成 LetRecovery-support.json")
                } else {
                    crate::tr!("尚未生成")
                },
            ),
        ]);
    } else {
        rows.push(row(
            crate::tr!("检查点状态"),
            crate::tr!("当前没有可用的检查点摘要"),
        ));
    }
    DetailPageContent {
        title: crate::tr!("恢复信息"),
        subtitle: crate::tr!("查看当前任务的只读检查点与安全收尾状态。"),
        rows,
        note: crate::tr!("检查点仅用于诊断，不表示可以从断点继续执行。"),
    }
}

fn row(label: impl Into<String>, value: impl Into<String>) -> DetailRow {
    DetailRow {
        label: label.into(),
        value: value.into(),
    }
}

fn enabled_text(enabled: bool) -> String {
    if enabled {
        crate::tr!("已启用")
    } else {
        crate::tr!("未启用")
    }
}

fn configured_text(value: &str) -> String {
    if value.trim().is_empty() {
        crate::tr!("未配置")
    } else {
        crate::tr!("已配置（内容已隐藏）")
    }
}

fn driver_action_text(config: &InstallConfig) -> String {
    match config.driver_action_mode {
        DriverActionMode::None if config.restore_drivers => crate::tr!("自动导入（兼容配置）"),
        DriverActionMode::None => crate::tr!("无"),
        DriverActionMode::SaveOnly => crate::tr!("仅保存"),
        DriverActionMode::AutoImport => crate::tr!("自动导入"),
    }
}

fn boot_mode_text(mode: u8) -> String {
    match mode {
        1 => crate::tr!("UEFI"),
        2 => crate::tr!("Legacy"),
        _ => crate::tr!("自动"),
    }
}

fn pca_mode_text(mode: BootPcaMode) -> String {
    match mode {
        BootPcaMode::Auto => crate::tr!("自动"),
        BootPcaMode::Pca2011 => crate::tr!("PCA2011"),
        BootPcaMode::Pca2023 => crate::tr!("PCA2023"),
    }
}

fn workflow_text(workflow: WorkflowKind) -> String {
    match workflow {
        WorkflowKind::Install => crate::tr!("系统安装"),
        WorkflowKind::Backup => crate::tr!("系统备份"),
        WorkflowKind::Expand => crate::tr!("无损扩大系统盘"),
        WorkflowKind::Missing => crate::tr!("未知任务"),
    }
}

fn current_step_text(progress: &ProgressState, workflow: WorkflowKind) -> String {
    if !progress.has_current_step {
        return crate::tr!("正在准备操作...");
    }
    match workflow {
        WorkflowKind::Install => crate::tr!(progress.current_install_step.name()),
        WorkflowKind::Backup => crate::tr!(progress.current_backup_step.name()),
        WorkflowKind::Expand => progress
            .status_message
            .split(['\r', '\n'])
            .next()
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| crate::tr!("无损扩大分区")),
        WorkflowKind::Missing => crate::tr!("未知"),
    }
}

fn worker_state_text(recovery: &WorkflowRecoverySnapshot) -> String {
    if recovery.worker_finished {
        crate::tr!("工作线程已结束")
    } else if recovery.worker_started {
        crate::tr!("正在运行或安全收尾")
    } else {
        crate::tr!("尚未启动")
    }
}

fn checkpoint_status_text(status: OperationStatus) -> String {
    match status {
        OperationStatus::Pending => crate::tr!("等待中"),
        OperationStatus::Running => crate::tr!("进行中"),
        OperationStatus::Interrupted => crate::tr!("已中断"),
        OperationStatus::Failed => crate::tr!("失败"),
        OperationStatus::Cancelled => crate::tr!("已取消"),
        OperationStatus::Succeeded => crate::tr!("已完成"),
    }
}

pub(crate) struct DetailsPane {
    title: HWND,
    subtitle: HWND,
    list: HWND,
    note: HWND,
}

impl DetailsPane {
    pub(crate) unsafe fn create(parent: HWND, theme: ThemeContext) -> windows::core::Result<Self> {
        let pane = Self {
            title: create_static(parent, 2301, "")?,
            subtitle: create_static(parent, 2302, "")?,
            list: create_control(parent, 2303, NativeControlKind::List, "", theme)?,
            note: create_static(parent, 2304, "")?,
        };
        initialize_columns(pane.list);
        let _ = SendMessageW(
            pane.list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            WPARAM(0),
            LPARAM((LVS_EX_DOUBLEBUFFER | LVS_EX_FULLROWSELECT) as isize),
        );
        pane.set_visible(false);
        Ok(pane)
    }

    pub(crate) unsafe fn apply_fonts(&self, body_font: HFONT, title_font: HFONT) {
        for (control, font) in [
            (self.title, title_font),
            (self.subtitle, body_font),
            (self.list, body_font),
            (self.note, body_font),
        ] {
            let _ = SendMessageW(control, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        }
    }

    pub(crate) unsafe fn render(&self, content: &DetailPageContent) {
        set_text(self.title, &content.title);
        set_text(self.subtitle, &content.subtitle);
        set_text(self.note, &content.note);
        let _ = SendMessageW(self.list, LVM_DELETEALLITEMS, WPARAM(0), LPARAM(0));
        for (index, row) in content.rows.iter().enumerate() {
            insert_cell(self.list, index, 0, &row.label);
            insert_cell(self.list, index, 1, &row.value);
        }
    }

    pub(crate) unsafe fn layout(&self, bounds: RECT, scale: impl Fn(i32) -> i32) {
        let width = (bounds.right - bounds.left).max(1);
        let height = (bounds.bottom - bounds.top).max(1);
        let gap = scale(10).min(12).min(height / 8).max(1);
        let title_height = scale(30).min((height / 5).max(1));
        let subtitle_height = scale(24).min((height / 6).max(1));
        let note_height = scale(46).min((height / 5).max(1));
        move_control(self.title, bounds.left, bounds.top, width, title_height);
        move_control(
            self.subtitle,
            bounds.left,
            bounds.top + title_height,
            width,
            subtitle_height,
        );
        let list_top = bounds.top + title_height + subtitle_height + gap;
        let list_height = (bounds.bottom - note_height - gap - list_top).max(1);
        move_control(self.list, bounds.left, list_top, width, list_height);
        move_control(
            self.note,
            bounds.left,
            list_top + list_height + gap,
            width,
            note_height,
        );
        let column_gap = 1;
        let first = (width * 34 / 100).clamp(1, (width - column_gap).max(1));
        let second = (width - first - column_gap).max(1);
        set_column_width(self.list, 0, first);
        set_column_width(self.list, 1, second);
    }

    pub(crate) unsafe fn set_visible(&self, visible: bool) {
        let command = if visible { SW_SHOW } else { SW_HIDE };
        for control in [self.title, self.subtitle, self.list, self.note] {
            let _ = ShowWindow(control, command);
        }
    }
}

unsafe fn create_static(parent: HWND, id: u16, text: &str) -> windows::core::Result<HWND> {
    let text = wide(text);
    CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("STATIC"),
        PCWSTR(text.as_ptr()),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0),
        0,
        0,
        0,
        0,
        parent,
        HMENU(id as isize as *mut _),
        HINSTANCE::default(),
        None,
    )
}

unsafe fn initialize_columns(list: HWND) {
    for (index, title) in [crate::tr!("项目"), crate::tr!("值")]
        .into_iter()
        .enumerate()
    {
        let mut title = wide(title);
        let mut column = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH,
            cx: 200,
            pszText: PWSTR(title.as_mut_ptr()),
            ..Default::default()
        };
        let _ = SendMessageW(
            list,
            LVM_INSERTCOLUMNW,
            WPARAM(index),
            LPARAM((&mut column as *mut LVCOLUMNW) as isize),
        );
    }
}

unsafe fn insert_cell(list: HWND, row: usize, column: usize, value: &str) {
    let mut value = wide(value);
    let mut item = LVITEMW {
        mask: LVIF_TEXT,
        iItem: row as i32,
        iSubItem: column as i32,
        pszText: PWSTR(value.as_mut_ptr()),
        ..Default::default()
    };
    let message = if column == 0 {
        LVM_INSERTITEMW
    } else {
        LVM_SETITEMTEXTW
    };
    let _ = SendMessageW(
        list,
        message,
        WPARAM(0),
        LPARAM((&mut item as *mut LVITEMW) as isize),
    );
}

unsafe fn set_column_width(list: HWND, column: usize, width: i32) {
    let _ = SendMessageW(
        list,
        LVM_SETCOLUMNWIDTH,
        WPARAM(column),
        LPARAM(width as isize),
    );
}

unsafe fn set_text(control: HWND, value: &str) {
    let value = wide(value);
    let _ = SetWindowTextW(control, PCWSTR(value.as_ptr()));
}

unsafe fn move_control(control: HWND, x: i32, y: i32, width: i32, height: i32) {
    let _ = MoveWindow(control, x, y, width.max(0), height.max(0), true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::WorkflowRecoverySnapshot;
    use crate::utils::i18n::LanguageFile;

    fn contains_han(text: &str) -> bool {
        text.chars().any(|ch| {
            matches!(
                ch,
                '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}'
            )
        })
    }

    fn literal_translation_keys(source: &str) -> Vec<&str> {
        source
            .split("crate::tr!(\"")
            .skip(1)
            .filter_map(|tail| tail.split('"').next())
            .collect()
    }

    #[test]
    fn advanced_summary_never_exposes_paths_ids_or_hashes() {
        let config = InstallConfig {
            session_id: "secret-session".into(),
            image_path: "D:\\secret\\system.wim".into(),
            original_guid: "{secret-guid}".into(),
            custom_unattend_file: "private.xml".into(),
            pca_compat_package: "private\\pca.wim".into(),
            pca_compat_sha256: "deadbeef".into(),
            ..InstallConfig::default()
        };
        let rendered = format!("{:?}", AdvancedOptionsSummary::from_install_config(&config));
        for secret in [
            "secret-session",
            "secret\\system.wim",
            "secret-guid",
            "private.xml",
            "private\\pca.wim",
            "deadbeef",
        ] {
            assert!(!rendered.contains(secret), "summary leaked {secret}");
        }
        assert!(rendered.contains("内容已隐藏"));
    }

    #[test]
    fn recovery_page_explicitly_disclaims_resume() {
        let progress = ProgressState::new_expand();
        let recovery = WorkflowRecoverySnapshot {
            checkpoint: None,
            worker_started: true,
            worker_finished: false,
        };
        let content = page_content(
            NativePage::Recovery,
            WorkflowKind::Expand,
            &progress,
            &recovery,
            None,
        );
        assert!(content.note.contains("不表示可以从断点继续"));
        assert!(content
            .rows
            .iter()
            .any(|row| row.value.contains("安全收尾")));
    }

    #[test]
    fn error_page_does_not_render_raw_worker_error() {
        let mut progress = ProgressState::new_install();
        progress.mark_failed("raw secret diagnostic path D:\\private");
        let recovery = WorkflowRecoverySnapshot {
            checkpoint: None,
            worker_started: true,
            worker_finished: true,
        };
        let content = page_content(
            NativePage::Error,
            WorkflowKind::Install,
            &progress,
            &recovery,
            None,
        );
        assert!(!format!("{content:?}").contains("raw secret diagnostic"));
    }

    #[test]
    fn every_literal_detail_translation_has_english_without_han_text() {
        let catalog: LanguageFile =
            serde_json::from_str(include_str!("../../../assets/release/lang/en-US.json"))
                .expect("the bundled English catalogue must parse");
        let source = include_str!("details.rs");

        for key in literal_translation_keys(source) {
            let english = catalog
                .data
                .get(key)
                .unwrap_or_else(|| panic!("missing English detail translation for {key:?}"));
            assert!(
                !contains_han(english),
                "English detail translation still contains Han text: {key:?} => {english:?}"
            );
        }
    }

    #[test]
    fn detail_rows_do_not_bypass_the_translation_macro() {
        let source = include_str!("details.rs");
        assert!(
            !source.contains("row(\""),
            "user-visible row labels must be passed through crate::tr! at their call sites"
        );
    }
}
