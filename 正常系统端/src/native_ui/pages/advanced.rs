//! Native presentation for installation advanced options.
//!
//! This page only mirrors [`AdvancedOptionsData`] into Win32 controls. It deliberately does not
//! browse files, capture Wi-Fi credentials, touch an offline registry, or start installation.

use std::cell::Cell;
use std::mem::size_of;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::HFONT;
use windows::Win32::UI::Controls::SetScrollInfo;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, GetScrollInfo, GetWindowTextLengthW, GetWindowTextW, MoveWindow, SendMessageW,
    ShowWindow, BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_OWNERDRAW, ES_AUTOHSCROLL, HMENU,
    SB_BOTTOM, SB_ENDSCROLL, SB_LINEDOWN, SB_LINEUP, SB_PAGEDOWN, SB_PAGEUP, SB_THUMBPOSITION,
    SB_THUMBTRACK, SB_TOP, SB_VERT, SCROLLINFO, SIF_PAGE, SIF_POS, SIF_RANGE, SIF_TRACKPOS,
    SW_HIDE, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_COMMAND, WM_CTLCOLORBTN, WM_CTLCOLOREDIT,
    WM_CTLCOLORSTATIC, WM_DRAWITEM, WM_ERASEBKGND, WM_MOUSEWHEEL, WM_NCDESTROY, WM_SETFONT,
    WM_VSCROLL, WS_CHILD, WS_CLIPCHILDREN, WS_EX_CLIENTEDGE, WS_TABSTOP, WS_VSCROLL,
};

use super::super::controls::{center_single_line_edit_in_row, child, wide, InnoMetrics};
use super::super::theme::{apply_control_theme, NativeControlKind, Palette};
use crate::core::ui_state::AdvancedOptionsData;

const ID_FIRST: u16 = 700;
const MIN_THREE_COLUMN_WIDTH: i32 = 320;
const MIN_TWO_COLUMN_WIDTH: i32 = 250;
const COLUMN_GAP: i32 = 16;
const VIEWPORT_SUBCLASS_ID: usize = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AdvancedGrid {
    columns: usize,
    column_width: i32,
    gap: i32,
}

impl AdvancedGrid {
    fn calculate(width: i32, dpi: u32) -> Self {
        let scale = |value: i32| ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32;
        let width = width.max(0);
        let gap = scale(COLUMN_GAP);
        // `width` is the page viewport already returned in the window's current coordinate
        // space. Scaling the breakpoints again made a 1270 px window collapse to one column.
        let three_column_minimum = MIN_THREE_COLUMN_WIDTH;
        let two_column_minimum = MIN_TWO_COLUMN_WIDTH;
        let columns = if width >= three_column_minimum * 3 + gap * 2 {
            3
        } else if width >= two_column_minimum * 2 + gap {
            2
        } else {
            1
        };
        let column_width = ((width - gap * (columns as i32 - 1)) / columns as i32).max(0);
        Self {
            columns,
            column_width,
            gap,
        }
    }

    fn x(self, left: i32, column: usize) -> i32 {
        left + column as i32 * (self.column_width + self.gap)
    }
}

fn shortest_column(columns: &[i32]) -> usize {
    columns
        .iter()
        .enumerate()
        .min_by_key(|(_, height)| *height)
        .map(|(index, _)| index)
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ScrollModel {
    offset: i32,
    content_height: i32,
    viewport_height: i32,
}

impl ScrollModel {
    fn maximum(self) -> i32 {
        (self.content_height - self.viewport_height).max(0)
    }

    fn clamped_offset(self, requested: i32) -> i32 {
        requested.clamp(0, self.maximum())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdvancedPageContext {
    pub unattended_enabled: bool,
    pub wifi_available: bool,
    pub show_windows_7: bool,
    pub show_windows_7_uefi: bool,
    pub show_xp: bool,
}

impl Default for AdvancedPageContext {
    fn default() -> Self {
        Self {
            unattended_enabled: true,
            wifi_available: false,
            show_windows_7: false,
            show_windows_7_uefi: false,
            show_xp: false,
        }
    }
}

#[derive(Clone, Copy)]
struct CheckEdit {
    check: HWND,
    edit: HWND,
    browse: Option<BrowseControl>,
}

#[derive(Clone, Copy)]
struct BrowseControl {
    button: HWND,
    id: u16,
    target: AdvancedBrowseTarget,
}

/// Identifies which advanced-option path the window controller should browse for.
///
/// The page deliberately returns an intent instead of opening a dialog itself so the owning
/// window remains responsible for modality, filters and filesystem access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdvancedBrowseTarget {
    DeployScript,
    FirstLoginScript,
    CustomDriversDirectory,
    RegistryFile,
    CustomFilesDirectory,
    Windows7Usb3Drivers,
    Windows7NvmeDrivers,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdvancedPageIntent {
    Browse(AdvancedBrowseTarget),
}

fn browse_intent_for_id(
    control_id: u16,
    controls: impl IntoIterator<Item = (u16, AdvancedBrowseTarget)>,
) -> Option<AdvancedPageIntent> {
    controls
        .into_iter()
        .find(|(id, _)| *id == control_id)
        .map(|(_, target)| AdvancedPageIntent::Browse(target))
}

#[derive(Clone)]
pub struct AdvancedPageHandles {
    pub system_header: HWND,
    pub system_checks: [HWND; 10],
    pub scripts_header: HWND,
    deploy_script: CheckEdit,
    first_login_script: CheckEdit,
    pub content_header: HWND,
    custom_drivers: CheckEdit,
    pub storage_drivers: HWND,
    registry_file: CheckEdit,
    custom_files: CheckEdit,
    pub identity_header: HWND,
    username: CheckEdit,
    volume_label: CheckEdit,
    pub windows_7_header: HWND,
    windows_7_usb3: CheckEdit,
    windows_7_nvme: CheckEdit,
    pub windows_7_acpi: HWND,
    pub windows_7_storage: HWND,
    pub windows_7_uefi: HWND,
    pub xp_header: HWND,
    pub xp_usb3: HWND,
    pub xp_nvme: HWND,
}

pub struct AdvancedPage {
    handles: AdvancedPageHandles,
    context: AdvancedPageContext,
    viewport: HWND,
    width: Cell<i32>,
    height: Cell<i32>,
    dpi: Cell<u32>,
    scroll_offset: Cell<i32>,
    content_height: Cell<i32>,
}

impl AdvancedPage {
    pub unsafe fn create(
        parent: HWND,
        initial: &AdvancedOptionsData,
        context: AdvancedPageContext,
    ) -> windows::core::Result<Self> {
        let viewport = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            PCWSTR::null(),
            WINDOW_STYLE((WS_CHILD | WS_CLIPCHILDREN | WS_VSCROLL).0),
            0,
            0,
            0,
            0,
            parent,
            HMENU(799_isize as *mut _),
            HINSTANCE::default(),
            None,
        )?;
        let _ = SetWindowSubclass(
            viewport,
            Some(advanced_viewport_proc),
            VIEWPORT_SUBCLASS_ID,
            parent.0 as usize,
        );
        let parent = viewport;
        let mut id = ID_FIRST;
        let mut next_id = || {
            let result = id;
            id += 1;
            result
        };

        let system_header = label(parent, &crate::tr!("系统设置"), next_id())?;
        let system_checks = [
            checkbox(parent, &crate::tr!("移除快捷方式箭头"), next_id())?,
            checkbox(parent, &crate::tr!("恢复经典右键菜单"), next_id())?,
            checkbox(parent, &crate::tr!("跳过 Windows 11 联网要求"), next_id())?,
            checkbox(parent, &crate::tr!("禁用 Windows Update"), next_id())?,
            checkbox(parent, &crate::tr!("禁用 Windows Defender"), next_id())?,
            checkbox(parent, &crate::tr!("禁用保留存储"), next_id())?,
            checkbox(parent, &crate::tr!("禁用用户账户控制 (UAC)"), next_id())?,
            checkbox(parent, &crate::tr!("禁用设备自动加密"), next_id())?,
            checkbox(parent, &crate::tr!("移除预装 UWP 应用"), next_id())?,
            checkbox(parent, &crate::tr!("迁移当前 Wi-Fi 配置"), next_id())?,
        ];

        let scripts_header = label(parent, &crate::tr!("部署脚本"), next_id())?;
        let deploy_script = check_edit(
            parent,
            &crate::tr!("部署过程中运行脚本"),
            &initial.deploy_script_path,
            next_id(),
            next_id(),
            Some((next_id(), AdvancedBrowseTarget::DeployScript)),
        )?;
        let first_login_script = check_edit(
            parent,
            &crate::tr!("首次登录时运行脚本"),
            &initial.first_login_script_path,
            next_id(),
            next_id(),
            Some((next_id(), AdvancedBrowseTarget::FirstLoginScript)),
        )?;

        let content_header = label(parent, &crate::tr!("驱动与自定义内容"), next_id())?;
        let custom_drivers = check_edit(
            parent,
            &crate::tr!("导入自定义驱动目录"),
            &initial.custom_drivers_path,
            next_id(),
            next_id(),
            Some((next_id(), AdvancedBrowseTarget::CustomDriversDirectory)),
        )?;
        let storage_drivers = checkbox(parent, &crate::tr!("导入存储控制器驱动"), next_id())?;
        let registry_file = check_edit(
            parent,
            &crate::tr!("导入注册表文件"),
            &initial.registry_file_path,
            next_id(),
            next_id(),
            Some((next_id(), AdvancedBrowseTarget::RegistryFile)),
        )?;
        let custom_files = check_edit(
            parent,
            &crate::tr!("复制自定义文件目录"),
            &initial.custom_files_path,
            next_id(),
            next_id(),
            Some((next_id(), AdvancedBrowseTarget::CustomFilesDirectory)),
        )?;

        let identity_header = label(parent, &crate::tr!("用户与系统盘"), next_id())?;
        let username = check_edit(
            parent,
            &crate::tr!("自定义用户名"),
            &initial.username,
            next_id(),
            next_id(),
            None,
        )?;
        let volume_label = check_edit(
            parent,
            &crate::tr!("自定义系统盘卷标"),
            &initial.volume_label,
            next_id(),
            next_id(),
            None,
        )?;

        let windows_7_header = label(parent, &crate::tr!("Windows 7 兼容选项"), next_id())?;
        let windows_7_usb3 = check_edit(
            parent,
            &crate::tr!("注入 USB 3.x 驱动"),
            &initial.win7_usb3_driver_path,
            next_id(),
            next_id(),
            Some((next_id(), AdvancedBrowseTarget::Windows7Usb3Drivers)),
        )?;
        let windows_7_nvme = check_edit(
            parent,
            &crate::tr!("注入 NVMe 驱动"),
            &initial.win7_nvme_driver_path,
            next_id(),
            next_id(),
            Some((next_id(), AdvancedBrowseTarget::Windows7NvmeDrivers)),
        )?;
        let windows_7_acpi = checkbox(parent, &crate::tr!("修复 ACPI 兼容蓝屏"), next_id())?;
        let windows_7_storage =
            checkbox(parent, &crate::tr!("修复 0x7B 存储控制器蓝屏"), next_id())?;
        let windows_7_uefi = checkbox(parent, &crate::tr!("启用 Windows 7 UEFI 补丁"), next_id())?;

        let xp_header = label(parent, &crate::tr!("Windows XP / 2003 选项"), next_id())?;
        let xp_usb3 = checkbox(parent, &crate::tr!("注入 USB 3.x 驱动"), next_id())?;
        let xp_nvme = checkbox(parent, &crate::tr!("注入 NVMe 驱动"), next_id())?;

        let page = Self {
            handles: AdvancedPageHandles {
                system_header,
                system_checks,
                scripts_header,
                deploy_script,
                first_login_script,
                content_header,
                custom_drivers,
                storage_drivers,
                registry_file,
                custom_files,
                identity_header,
                username,
                volume_label,
                windows_7_header,
                windows_7_usb3,
                windows_7_nvme,
                windows_7_acpi,
                windows_7_storage,
                windows_7_uefi,
                xp_header,
                xp_usb3,
                xp_nvme,
            },
            context,
            viewport,
            width: Cell::new(0),
            height: Cell::new(0),
            dpi: Cell::new(96),
            scroll_offset: Cell::new(0),
            content_height: Cell::new(0),
        };
        page.apply(initial);
        page.apply_context();
        page.show(false);
        Ok(page)
    }

    pub fn handles(&self) -> &AdvancedPageHandles {
        &self.handles
    }

    /// Replaces all captions in place while preserving every option value.
    pub unsafe fn relocalize(&self) {
        let h = &self.handles;
        set_text(h.system_header, &crate::tr!("系统设置"));
        for (control, label) in h.system_checks.into_iter().zip([
            crate::tr!("移除快捷方式箭头"),
            crate::tr!("恢复经典右键菜单"),
            crate::tr!("跳过 Windows 11 联网要求"),
            crate::tr!("禁用 Windows Update"),
            crate::tr!("禁用 Windows Defender"),
            crate::tr!("禁用保留存储"),
            crate::tr!("禁用用户账户控制 (UAC)"),
            crate::tr!("禁用设备自动加密"),
            crate::tr!("移除预装 UWP 应用"),
            crate::tr!("迁移当前 Wi-Fi 配置"),
        ]) {
            set_text(control, &label);
        }

        set_text(h.scripts_header, &crate::tr!("部署脚本"));
        relocalize_check_edit(h.deploy_script, &crate::tr!("部署过程中运行脚本"));
        relocalize_check_edit(h.first_login_script, &crate::tr!("首次登录时运行脚本"));
        set_text(h.content_header, &crate::tr!("驱动与自定义内容"));
        relocalize_check_edit(h.custom_drivers, &crate::tr!("导入自定义驱动目录"));
        set_text(h.storage_drivers, &crate::tr!("导入存储控制器驱动"));
        relocalize_check_edit(h.registry_file, &crate::tr!("导入注册表文件"));
        relocalize_check_edit(h.custom_files, &crate::tr!("复制自定义文件目录"));
        set_text(h.identity_header, &crate::tr!("用户与系统盘"));
        relocalize_check_edit(h.username, &crate::tr!("自定义用户名"));
        relocalize_check_edit(h.volume_label, &crate::tr!("自定义系统盘卷标"));

        set_text(h.windows_7_header, &crate::tr!("Windows 7 兼容选项"));
        relocalize_check_edit(h.windows_7_usb3, &crate::tr!("注入 USB 3.x 驱动"));
        relocalize_check_edit(h.windows_7_nvme, &crate::tr!("注入 NVMe 驱动"));
        set_text(h.windows_7_acpi, &crate::tr!("修复 ACPI 兼容蓝屏"));
        set_text(h.windows_7_storage, &crate::tr!("修复 0x7B 存储控制器蓝屏"));
        set_text(h.windows_7_uefi, &crate::tr!("启用 Windows 7 UEFI 补丁"));
        set_text(h.xp_header, &crate::tr!("Windows XP / 2003 选项"));
        set_text(h.xp_usb3, &crate::tr!("注入 USB 3.x 驱动"));
        set_text(h.xp_nvme, &crate::tr!("注入 NVMe 驱动"));
        self.apply_context();
    }

    /// Converts a forwarded `WM_COMMAND` control id into a side-effect-free browse intent.
    pub fn intent_for_command(&self, control_id: u16) -> Option<AdvancedPageIntent> {
        browse_intent_for_id(
            control_id,
            self.check_edits()
                .into_iter()
                .filter_map(|pair| pair.browse.map(|browse| (browse.id, browse.target))),
        )
    }

    /// Applies a path selected by the owning window and enables the corresponding option.
    /// Passing an empty path clears and disables it, preserving the required-value invariant.
    pub unsafe fn set_path(&self, target: AdvancedBrowseTarget, path: &str) {
        let Some(pair) = self.pair_for_browse_target(target) else {
            return;
        };
        let path = path.trim();
        set_text(pair.edit, path);
        set_checked(pair.check, !path.is_empty());
        self.update_dependencies();
    }

    pub unsafe fn set_wifi_caption(&self, ssid: Option<&str>) {
        let caption = ssid.map_or_else(
            || crate::tr!("迁移当前 Wi-Fi 配置"),
            |ssid| crate::tr!("迁移当前 Wi-Fi 配置（SSID：{}）", ssid),
        );
        set_text(self.handles.system_checks[9], &caption);
    }

    pub unsafe fn apply(&self, data: &AdvancedOptionsData) {
        let h = &self.handles;
        for (control, checked) in h.system_checks.into_iter().zip([
            data.remove_shortcut_arrow,
            data.restore_classic_context_menu,
            data.bypass_nro,
            data.disable_windows_update,
            data.disable_windows_defender,
            data.disable_reserved_storage,
            data.disable_uac,
            data.disable_device_encryption,
            data.remove_uwp_apps,
            data.migrate_wifi,
        ]) {
            set_checked(control, checked);
        }
        apply_check_edit(
            h.deploy_script,
            data.run_script_during_deploy,
            &data.deploy_script_path,
        );
        apply_check_edit(
            h.first_login_script,
            data.run_script_first_login,
            &data.first_login_script_path,
        );
        apply_check_edit(
            h.custom_drivers,
            data.import_custom_drivers,
            &data.custom_drivers_path,
        );
        set_checked(h.storage_drivers, data.import_storage_controller_drivers);
        apply_check_edit(
            h.registry_file,
            data.import_registry_file,
            &data.registry_file_path,
        );
        apply_check_edit(
            h.custom_files,
            data.import_custom_files,
            &data.custom_files_path,
        );
        apply_check_edit(h.username, data.custom_username, &data.username);
        apply_check_edit(h.volume_label, data.custom_volume_label, &data.volume_label);
        apply_check_edit(
            h.windows_7_usb3,
            data.win7_inject_usb3_driver,
            &data.win7_usb3_driver_path,
        );
        apply_check_edit(
            h.windows_7_nvme,
            data.win7_inject_nvme_driver,
            &data.win7_nvme_driver_path,
        );
        set_checked(h.windows_7_acpi, data.win7_fix_acpi_bsod);
        set_checked(h.windows_7_storage, data.win7_fix_storage_bsod);
        set_checked(h.windows_7_uefi, data.win7_uefi_patch);
        set_checked(h.xp_usb3, data.xp_inject_usb3_driver);
        set_checked(h.xp_nvme, data.xp_inject_nvme_driver);
        self.update_dependencies();
    }

    pub unsafe fn read(&self) -> AdvancedOptionsData {
        let mut data = AdvancedOptionsData::default();
        self.read_into(&mut data);
        data
    }

    /// Updates persistent fields while preserving runtime-only Wi-Fi material and the XP
    /// one-shot marker already held by the controller.
    pub unsafe fn read_into(&self, data: &mut AdvancedOptionsData) {
        let h = &self.handles;
        data.remove_shortcut_arrow = is_checked(h.system_checks[0]);
        data.restore_classic_context_menu = is_checked(h.system_checks[1]);
        data.bypass_nro = is_checked(h.system_checks[2]);
        data.disable_windows_update = is_checked(h.system_checks[3]);
        data.disable_windows_defender = is_checked(h.system_checks[4]);
        data.disable_reserved_storage = is_checked(h.system_checks[5]);
        data.disable_uac = is_checked(h.system_checks[6]);
        data.disable_device_encryption = is_checked(h.system_checks[7]);
        data.remove_uwp_apps = is_checked(h.system_checks[8]);
        data.migrate_wifi = is_checked(h.system_checks[9]);
        (data.run_script_during_deploy, data.deploy_script_path) =
            read_required_pair(h.deploy_script);
        (data.run_script_first_login, data.first_login_script_path) =
            read_required_pair(h.first_login_script);
        (data.import_custom_drivers, data.custom_drivers_path) =
            read_required_pair(h.custom_drivers);
        data.import_storage_controller_drivers = is_checked(h.storage_drivers);
        (data.import_registry_file, data.registry_file_path) = read_required_pair(h.registry_file);
        (data.import_custom_files, data.custom_files_path) = read_required_pair(h.custom_files);
        (data.custom_username, data.username) = read_required_pair(h.username);
        (data.custom_volume_label, data.volume_label) = read_required_pair(h.volume_label);
        (data.win7_inject_usb3_driver, data.win7_usb3_driver_path) =
            read_required_pair(h.windows_7_usb3);
        (data.win7_inject_nvme_driver, data.win7_nvme_driver_path) =
            read_required_pair(h.windows_7_nvme);
        data.win7_fix_acpi_bsod = is_checked(h.windows_7_acpi);
        data.win7_fix_storage_bsod = is_checked(h.windows_7_storage);
        data.win7_uefi_patch = is_checked(h.windows_7_uefi);
        data.xp_inject_usb3_driver = is_checked(h.xp_usb3);
        data.xp_inject_nvme_driver = is_checked(h.xp_nvme);
    }

    pub unsafe fn set_context(&mut self, context: AdvancedPageContext) {
        self.context = context;
        self.apply_context();
    }

    pub unsafe fn update_dependencies(&self) {
        let h = &self.handles;
        for pair in [
            h.deploy_script,
            h.first_login_script,
            h.custom_drivers,
            h.registry_file,
            h.custom_files,
            h.username,
            h.volume_label,
            h.windows_7_usb3,
            h.windows_7_nvme,
        ] {
            let enabled = is_checked(pair.check);
            let _ = EnableWindow(pair.edit, enabled);
            if let Some(browse) = pair.browse {
                let _ = EnableWindow(browse.button, enabled);
            }
        }
    }

    pub unsafe fn show(&self, visible: bool) {
        let command = if visible { SW_SHOW } else { SW_HIDE };
        for control in self.all_controls() {
            let _ = ShowWindow(control, command);
        }
        if visible {
            self.apply_context();
        }
        let _ = ShowWindow(self.viewport, command);
    }

    pub unsafe fn apply_theme(&self, palette: Palette) {
        for control in self.checkbox_controls() {
            // Reuse the shared checkbox/radio subclass instead of relying on USER32 to recolour
            // captions after a live light/dark switch.  The host theme commonly updates the
            // glyph while leaving the BUTTON caption cached in the previous (black) colour.
            apply_control_theme(control, palette, NativeControlKind::General);
        }
        for pair in self.check_edits() {
            apply_control_theme(pair.edit, palette, NativeControlKind::Field);
        }
    }

    /// Returns whether `control` toggles one of the conditional Edit/Browse rows.
    ///
    /// The checkbox is parented to the page viewport, so its `BN_CLICKED` notification is
    /// forwarded to the top-level window.  Keeping ownership testing here avoids coupling the
    /// controller to the page's generated control IDs.
    pub fn owns_dependency_toggle(&self, control: HWND) -> bool {
        self.check_edits()
            .into_iter()
            .any(|pair| pair.check == control)
    }

    pub unsafe fn apply_font(&self, font: HFONT, heading_font: HFONT) {
        for control in self.all_controls() {
            let _ = SendMessageW(control, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        }
        for heading in self.headings() {
            let _ = SendMessageW(
                heading,
                WM_SETFONT,
                WPARAM(heading_font.0 as usize),
                LPARAM(1),
            );
        }
    }

    /// Compact responsive layout. Columns keep enough logical width for the longest English
    /// captions instead of forcing three narrow columns and clipping standard Win32 checkboxes.
    pub unsafe fn layout(&self, left: i32, top: i32, width: i32, height: i32, dpi: u32) {
        let width = width.max(0);
        let height = height.max(0);
        let _ = MoveWindow(self.viewport, left, top, width, height, true);
        self.width.set(width);
        self.height.set(height);
        self.dpi.set(dpi.max(1));

        let content_height = self.layout_content(width, dpi, 0);
        self.content_height.set(content_height);
        let model = ScrollModel {
            offset: self.scroll_offset.get(),
            content_height,
            viewport_height: height,
        };
        self.scroll_offset
            .set(model.clamped_offset(self.scroll_offset.get()));
        self.layout_content(width, dpi, -self.scroll_offset.get());
        self.update_scrollbar();
    }

    unsafe fn layout_content(&self, width: i32, dpi: u32, origin_y: i32) -> i32 {
        let s = |value: i32| ((value as i64 * dpi.max(1) as i64 + 48) / 96) as i32;
        let grid = AdvancedGrid::calculate(width, dpi);
        let mut bottoms = vec![origin_y; grid.columns];
        let section_gap = s(5);
        let h = &self.handles;

        let column = shortest_column(&bottoms);
        let x = grid.x(0, column);
        layout_heading(
            h.system_header,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        for (index, check) in h.system_checks.into_iter().enumerate() {
            if index != 9 || self.context.wifi_available {
                layout_check(check, x, &mut bottoms[column], grid.column_width, dpi);
            }
        }
        bottoms[column] += section_gap;

        let column = shortest_column(&bottoms);
        let x = grid.x(0, column);
        layout_heading(
            h.scripts_header,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        layout_pair(
            h.deploy_script,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        layout_pair(
            h.first_login_script,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        bottoms[column] += section_gap;

        let column = shortest_column(&bottoms);
        let x = grid.x(0, column);
        layout_heading(
            h.content_header,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        layout_pair(
            h.custom_drivers,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        layout_check(
            h.storage_drivers,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        layout_pair(
            h.registry_file,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        layout_pair(
            h.custom_files,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        bottoms[column] += section_gap;

        let column = shortest_column(&bottoms);
        let x = grid.x(0, column);
        layout_heading(
            h.identity_header,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        layout_pair(h.username, x, &mut bottoms[column], grid.column_width, dpi);
        layout_pair(
            h.volume_label,
            x,
            &mut bottoms[column],
            grid.column_width,
            dpi,
        );
        bottoms[column] += section_gap;

        if self.context.show_windows_7 {
            let column = shortest_column(&bottoms);
            let x = grid.x(0, column);
            layout_heading(
                h.windows_7_header,
                x,
                &mut bottoms[column],
                grid.column_width,
                dpi,
            );
            layout_pair(
                h.windows_7_usb3,
                x,
                &mut bottoms[column],
                grid.column_width,
                dpi,
            );
            layout_pair(
                h.windows_7_nvme,
                x,
                &mut bottoms[column],
                grid.column_width,
                dpi,
            );
            layout_check(
                h.windows_7_acpi,
                x,
                &mut bottoms[column],
                grid.column_width,
                dpi,
            );
            layout_check(
                h.windows_7_storage,
                x,
                &mut bottoms[column],
                grid.column_width,
                dpi,
            );
            if self.context.show_windows_7_uefi {
                layout_check(
                    h.windows_7_uefi,
                    x,
                    &mut bottoms[column],
                    grid.column_width,
                    dpi,
                );
            }
            bottoms[column] += section_gap;
        }

        if self.context.show_xp {
            let column = shortest_column(&bottoms);
            let x = grid.x(0, column);
            layout_heading(h.xp_header, x, &mut bottoms[column], grid.column_width, dpi);
            layout_check(h.xp_usb3, x, &mut bottoms[column], grid.column_width, dpi);
            layout_check(h.xp_nvme, x, &mut bottoms[column], grid.column_width, dpi);
        }
        bottoms.into_iter().max().unwrap_or(origin_y) - origin_y + s(8)
    }

    pub fn viewport(&self) -> HWND {
        self.viewport
    }

    pub unsafe fn scroll_wheel(&self, wheel_delta: i16) -> bool {
        if self.height.get() <= 0 || self.content_height.get() <= self.height.get() {
            return false;
        }
        let line = ((32_i64 * i64::from(self.dpi.get()) + 48) / 96) as i32;
        let steps = (i32::from(wheel_delta) / 120).clamp(-3, 3);
        self.set_scroll_offset(self.scroll_offset.get() - steps * line * 3)
    }

    pub unsafe fn handle_vscroll(&self, request: usize) -> bool {
        let code = (request & 0xffff) as u32;
        let model = ScrollModel {
            offset: self.scroll_offset.get(),
            content_height: self.content_height.get(),
            viewport_height: self.height.get(),
        };
        let line = ((32_i64 * i64::from(self.dpi.get()) + 48) / 96) as i32;
        let requested = match code {
            value if value == SB_TOP.0 as u32 => 0,
            value if value == SB_BOTTOM.0 as u32 => model.maximum(),
            value if value == SB_LINEUP.0 as u32 => model.offset - line,
            value if value == SB_LINEDOWN.0 as u32 => model.offset + line,
            value if value == SB_PAGEUP.0 as u32 => model.offset - model.viewport_height,
            value if value == SB_PAGEDOWN.0 as u32 => model.offset + model.viewport_height,
            value if value == SB_THUMBPOSITION.0 as u32 || value == SB_THUMBTRACK.0 as u32 => {
                let mut info = SCROLLINFO {
                    cbSize: size_of::<SCROLLINFO>() as u32,
                    fMask: SIF_TRACKPOS,
                    ..Default::default()
                };
                let _ = GetScrollInfo(self.viewport, SB_VERT, &mut info);
                info.nTrackPos
            }
            value if value == SB_ENDSCROLL.0 as u32 => return false,
            _ => return false,
        };
        self.set_scroll_offset(requested)
    }

    unsafe fn set_scroll_offset(&self, requested: i32) -> bool {
        let model = ScrollModel {
            offset: self.scroll_offset.get(),
            content_height: self.content_height.get(),
            viewport_height: self.height.get(),
        };
        let offset = model.clamped_offset(requested);
        if offset == model.offset {
            return false;
        }
        self.scroll_offset.set(offset);
        self.layout_content(self.width.get(), self.dpi.get(), -offset);
        self.update_scrollbar();
        true
    }

    unsafe fn update_scrollbar(&self) {
        let model = ScrollModel {
            offset: self.scroll_offset.get(),
            content_height: self.content_height.get(),
            viewport_height: self.height.get(),
        };
        let info = SCROLLINFO {
            cbSize: size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
            nMin: 0,
            nMax: (model.content_height - 1).max(0),
            nPage: model.viewport_height.max(0) as u32,
            nPos: model.clamped_offset(model.offset),
            ..Default::default()
        };
        let _ = SetScrollInfo(self.viewport, SB_VERT, &info, true);
    }

    unsafe fn apply_context(&self) {
        let h = &self.handles;
        let unattended = self.context.unattended_enabled;
        for control in [h.system_checks[2], h.system_checks[8], h.system_checks[9]] {
            let _ = EnableWindow(control, unattended);
        }
        let _ = ShowWindow(
            h.system_checks[9],
            if self.context.wifi_available {
                SW_SHOW
            } else {
                SW_HIDE
            },
        );
        if !unattended {
            for control in [h.system_checks[2], h.system_checks[8], h.system_checks[9]] {
                set_checked(control, false);
            }
        }

        for control in self.windows_7_controls() {
            let _ = ShowWindow(
                control,
                if self.context.show_windows_7 {
                    SW_SHOW
                } else {
                    SW_HIDE
                },
            );
        }
        let _ = ShowWindow(
            h.windows_7_uefi,
            if self.context.show_windows_7 && self.context.show_windows_7_uefi {
                SW_SHOW
            } else {
                SW_HIDE
            },
        );
        for control in self.xp_controls() {
            let _ = ShowWindow(
                control,
                if self.context.show_xp {
                    SW_SHOW
                } else {
                    SW_HIDE
                },
            );
        }
        self.update_dependencies();
    }

    fn headings(&self) -> [HWND; 6] {
        let h = &self.handles;
        [
            h.system_header,
            h.scripts_header,
            h.content_header,
            h.identity_header,
            h.windows_7_header,
            h.xp_header,
        ]
    }

    fn check_edits(&self) -> [CheckEdit; 9] {
        let h = &self.handles;
        [
            h.deploy_script,
            h.first_login_script,
            h.custom_drivers,
            h.registry_file,
            h.custom_files,
            h.username,
            h.volume_label,
            h.windows_7_usb3,
            h.windows_7_nvme,
        ]
    }

    fn pair_for_browse_target(&self, target: AdvancedBrowseTarget) -> Option<CheckEdit> {
        self.check_edits()
            .into_iter()
            .find(|pair| pair.browse.is_some_and(|browse| browse.target == target))
    }

    fn checkbox_controls(&self) -> Vec<HWND> {
        let h = &self.handles;
        let mut controls = h.system_checks.to_vec();
        controls.extend(self.check_edits().into_iter().map(|pair| pair.check));
        controls.extend([
            h.storage_drivers,
            h.windows_7_acpi,
            h.windows_7_storage,
            h.windows_7_uefi,
            h.xp_usb3,
            h.xp_nvme,
        ]);
        controls
    }

    fn windows_7_controls(&self) -> [HWND; 8] {
        let h = &self.handles;
        [
            h.windows_7_header,
            h.windows_7_usb3.check,
            h.windows_7_usb3.edit,
            h.windows_7_nvme.check,
            h.windows_7_nvme.edit,
            h.windows_7_acpi,
            h.windows_7_storage,
            h.windows_7_uefi,
        ]
    }

    fn xp_controls(&self) -> [HWND; 3] {
        [
            self.handles.xp_header,
            self.handles.xp_usb3,
            self.handles.xp_nvme,
        ]
    }

    fn all_controls(&self) -> Vec<HWND> {
        let mut controls = self.headings().to_vec();
        controls.extend(self.checkbox_controls());
        controls.extend(self.check_edits().into_iter().map(|pair| pair.edit));
        controls.extend(
            self.check_edits()
                .into_iter()
                .filter_map(|pair| pair.browse.map(|browse| browse.button)),
        );
        controls
    }
}

unsafe extern "system" fn advanced_viewport_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    let owner = HWND(reference_data as *mut _);
    match message {
        WM_COMMAND | WM_DRAWITEM | WM_CTLCOLORBTN | WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC
        | WM_MOUSEWHEEL => SendMessageW(owner, message, wparam, lparam),
        WM_VSCROLL => SendMessageW(owner, message, wparam, LPARAM(hwnd.0 as isize)),
        WM_ERASEBKGND => LRESULT(1),
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(advanced_viewport_proc), VIEWPORT_SUBCLASS_ID);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

unsafe fn label(parent: HWND, text: &str, id: u16) -> windows::core::Result<HWND> {
    child(parent, w!("STATIC"), text, 0, id)
}

unsafe fn checkbox(parent: HWND, text: &str, id: u16) -> windows::core::Result<HWND> {
    child(
        parent,
        w!("BUTTON"),
        text,
        BS_AUTOCHECKBOX | WS_TABSTOP.0 as i32,
        id,
    )
}

unsafe fn edit(parent: HWND, text: &str, id: u16) -> windows::core::Result<HWND> {
    let text = wide(text);
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE(WS_EX_CLIENTEDGE.0 | 0x0000_0004),
        w!("EDIT"),
        PCWSTR(text.as_ptr()),
        WINDOW_STYLE((WS_CHILD | WS_TABSTOP).0 | ES_AUTOHSCROLL as u32),
        0,
        0,
        0,
        0,
        parent,
        HMENU(id as isize as *mut _),
        HINSTANCE::default(),
        None,
    )?;
    center_single_line_edit_in_row(hwnd);
    Ok(hwnd)
}

unsafe fn check_edit(
    parent: HWND,
    label: &str,
    text: &str,
    check_id: u16,
    edit_id: u16,
    browse: Option<(u16, AdvancedBrowseTarget)>,
) -> windows::core::Result<CheckEdit> {
    Ok(CheckEdit {
        check: checkbox(parent, label, check_id)?,
        edit: edit(parent, text, edit_id)?,
        browse: browse
            .map(|(id, target)| {
                Ok::<BrowseControl, windows::core::Error>(BrowseControl {
                    button: child(
                        parent,
                        w!("BUTTON"),
                        &crate::tr!("浏览..."),
                        BS_OWNERDRAW | WS_TABSTOP.0 as i32,
                        id,
                    )?,
                    id,
                    target,
                })
            })
            .transpose()?,
    })
}

unsafe fn set_checked(control: HWND, checked: bool) {
    let _ = SendMessageW(
        control,
        BM_SETCHECK,
        WPARAM(usize::from(checked)),
        LPARAM(0),
    );
}

unsafe fn is_checked(control: HWND) -> bool {
    SendMessageW(control, BM_GETCHECK, WPARAM(0), LPARAM(0)).0 == 1
}

unsafe fn apply_check_edit(pair: CheckEdit, checked: bool, text: &str) {
    set_checked(pair.check, checked);
    set_text(pair.edit, text);
}

unsafe fn read_text(control: HWND) -> String {
    let length = GetWindowTextLengthW(control).max(0) as usize;
    let mut buffer = vec![0_u16; length + 1];
    let copied = GetWindowTextW(control, &mut buffer).max(0) as usize;
    String::from_utf16_lossy(&buffer[..copied])
}

/// Checked options backed by a required value must never persist as active with an empty value.
/// Returning from the page normalizes both the visible checkbox and the stored model.
unsafe fn read_required_pair(pair: CheckEdit) -> (bool, String) {
    let (enabled, value) = normalize_required_value(is_checked(pair.check), &read_text(pair.edit));
    if !enabled {
        set_checked(pair.check, false);
    }
    (enabled, value)
}

fn normalize_required_value(checked: bool, value: &str) -> (bool, String) {
    let value = value.trim().to_owned();
    (checked && !value.is_empty(), value)
}

unsafe fn set_text(control: HWND, text: &str) {
    let text = wide(text);
    let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(control, PCWSTR(text.as_ptr()));
}

unsafe fn relocalize_check_edit(pair: CheckEdit, label: &str) {
    set_text(pair.check, label);
    if let Some(browse) = pair.browse {
        set_text(browse.button, &crate::tr!("浏览..."));
    }
}

unsafe fn layout_heading(control: HWND, x: i32, y: &mut i32, width: i32, dpi: u32) {
    let s = |value: i32| ((value as i64 * dpi.max(1) as i64 + 48) / 96) as i32;
    let _ = MoveWindow(control, x, *y, width, s(22), true);
    *y += s(27);
}

unsafe fn layout_check(control: HWND, x: i32, y: &mut i32, width: i32, dpi: u32) {
    let s = |value: i32| ((value as i64 * dpi.max(1) as i64 + 48) / 96) as i32;
    // Match the 24 px checkbox HWND used by the main install page. The shared 13 px glyph is then
    // centred against the same client height instead of looking vertically tighter on this page.
    let _ = MoveWindow(control, x, *y, width, s(24), true);
    *y += s(24);
}

unsafe fn layout_pair(pair: CheckEdit, x: i32, y: &mut i32, width: i32, dpi: u32) {
    let s = |value: i32| ((value as i64 * dpi.max(1) as i64 + 48) / 96) as i32;
    let field_height = InnoMetrics::for_dpi(dpi).field_height;
    let _ = MoveWindow(pair.check, x, *y, width, s(24), true);
    *y += s(24);
    let browse_width = pair.browse.map_or(0, |_| s(76));
    let browse_gap = pair.browse.map_or(0, |_| s(6));
    let edit_width = (width - s(20) - browse_width - browse_gap).max(0);
    let _ = MoveWindow(pair.edit, x + s(20), *y, edit_width, field_height, true);
    if let Some(browse) = pair.browse {
        let _ = MoveWindow(
            browse.button,
            x + s(20) + edit_width + browse_gap,
            *y,
            browse_width,
            field_height,
            true,
        );
    }
    *y += s(30);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_defaults_do_not_expose_version_specific_options() {
        let context = AdvancedPageContext::default();
        assert!(context.unattended_enabled);
        assert!(!context.wifi_available);
        assert!(!context.show_windows_7);
        assert!(!context.show_xp);
    }

    #[test]
    fn every_advanced_data_field_has_a_native_control_mapping() {
        let data = AdvancedOptionsData {
            remove_shortcut_arrow: true,
            restore_classic_context_menu: true,
            bypass_nro: true,
            disable_windows_update: true,
            disable_windows_defender: true,
            disable_reserved_storage: true,
            disable_uac: true,
            disable_device_encryption: true,
            remove_uwp_apps: true,
            migrate_wifi: true,
            run_script_during_deploy: true,
            run_script_first_login: true,
            import_custom_drivers: true,
            import_storage_controller_drivers: true,
            import_registry_file: true,
            import_custom_files: true,
            custom_username: true,
            custom_volume_label: true,
            win7_inject_usb3_driver: true,
            win7_inject_nvme_driver: true,
            win7_fix_acpi_bsod: true,
            win7_fix_storage_bsod: true,
            win7_uefi_patch: true,
            xp_inject_usb3_driver: true,
            xp_inject_nvme_driver: true,
            ..AdvancedOptionsData::default()
        };
        let mapped_flags = [
            data.remove_shortcut_arrow,
            data.restore_classic_context_menu,
            data.bypass_nro,
            data.disable_windows_update,
            data.disable_windows_defender,
            data.disable_reserved_storage,
            data.disable_uac,
            data.disable_device_encryption,
            data.remove_uwp_apps,
            data.migrate_wifi,
            data.run_script_during_deploy,
            data.run_script_first_login,
            data.import_custom_drivers,
            data.import_storage_controller_drivers,
            data.import_registry_file,
            data.import_custom_files,
            data.custom_username,
            data.custom_volume_label,
            data.win7_inject_usb3_driver,
            data.win7_inject_nvme_driver,
            data.win7_fix_acpi_bsod,
            data.win7_fix_storage_bsod,
            data.win7_uefi_patch,
            data.xp_inject_usb3_driver,
            data.xp_inject_nvme_driver,
        ];
        assert!(mapped_flags.into_iter().all(|value| value));
    }

    #[test]
    fn responsive_grid_uses_three_two_and_one_columns() {
        assert_eq!(AdvancedGrid::calculate(1_200, 96).columns, 3);
        assert_eq!(AdvancedGrid::calculate(820, 96).columns, 2);
        assert_eq!(AdvancedGrid::calculate(480, 96).columns, 1);
    }

    #[test]
    fn viewport_width_is_not_scaled_twice_at_high_dpi() {
        for (width, expected_columns) in [(1_200, 3), (820, 2), (480, 1)] {
            for dpi in [96, 120, 144, 192] {
                assert_eq!(
                    AdvancedGrid::calculate(width, dpi).columns,
                    expected_columns,
                    "viewport width {width} at {dpi} DPI"
                );
            }
        }
    }

    #[test]
    fn every_column_stays_inside_the_available_width() {
        for (width, dpi) in [(1_200, 96), (820, 96), (600, 96), (1_640, 192)] {
            let grid = AdvancedGrid::calculate(width, dpi);
            for column in 0..grid.columns {
                let x = grid.x(0, column);
                assert!(x >= 0);
                assert!(x + grid.column_width <= width);
            }
            assert!(grid.column_width >= 0);
        }
    }

    #[test]
    fn shortest_column_balances_sections_without_reordering_inside_them() {
        assert_eq!(shortest_column(&[280, 130]), 1);
        assert_eq!(shortest_column(&[280, 350]), 0);
        assert_eq!(shortest_column(&[210, 210, 300]), 0);
    }

    #[test]
    fn browse_command_ids_map_to_explicit_targets_and_ignore_other_controls() {
        let controls = [
            (801, AdvancedBrowseTarget::DeployScript),
            (802, AdvancedBrowseTarget::FirstLoginScript),
            (803, AdvancedBrowseTarget::CustomDriversDirectory),
            (804, AdvancedBrowseTarget::RegistryFile),
            (805, AdvancedBrowseTarget::CustomFilesDirectory),
            (806, AdvancedBrowseTarget::Windows7Usb3Drivers),
            (807, AdvancedBrowseTarget::Windows7NvmeDrivers),
        ];
        for (id, target) in controls {
            assert_eq!(
                browse_intent_for_id(id, controls),
                Some(AdvancedPageIntent::Browse(target))
            );
        }
        assert_eq!(browse_intent_for_id(999, controls), None);
    }

    #[test]
    fn required_path_options_disable_when_the_path_is_empty() {
        assert_eq!(
            normalize_required_value(true, "   "),
            (false, String::new())
        );
        assert_eq!(
            normalize_required_value(true, " C:\\drivers "),
            (true, String::from("C:\\drivers"))
        );
        assert_eq!(
            normalize_required_value(false, "C:\\drivers"),
            (false, String::from("C:\\drivers"))
        );
    }

    #[test]
    fn scroll_model_clamps_to_the_visible_content_range() {
        let model = ScrollModel {
            offset: 0,
            content_height: 900,
            viewport_height: 420,
        };
        assert_eq!(model.maximum(), 480);
        assert_eq!(model.clamped_offset(-20), 0);
        assert_eq!(model.clamped_offset(240), 240);
        assert_eq!(model.clamped_offset(900), 480);
    }

    #[test]
    fn scroll_model_disables_scrolling_when_content_fits() {
        let model = ScrollModel {
            offset: 100,
            content_height: 360,
            viewport_height: 420,
        };
        assert_eq!(model.maximum(), 0);
        assert_eq!(model.clamped_offset(100), 0);
    }
}
