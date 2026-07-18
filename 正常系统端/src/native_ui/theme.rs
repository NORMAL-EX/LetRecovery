use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    BOOL, COLORREF, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_DONOTROUND, DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, ClientToScreen, CreateCompatibleBitmap, CreateCompatibleDC,
    CreateDIBSection, CreatePen, CreateRectRgn, CreateSolidBrush, DeleteDC, DeleteObject, EndPaint,
    FillRect, GdiFlush, GetTextMetricsW, GetWindowDC, InvalidateRect, RedrawWindow, ReleaseDC,
    RoundRect, ScreenToClient, SelectObject, SetBkMode, SetDIBitsToDevice, SetStretchBltMode,
    SetTextColor, SetWindowRgn, StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, DT_CENTER, DT_END_ELLIPSIS, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER,
    DT_WORDBREAK, HALFTONE, HBRUSH, HDC, PAINTSTRUCT, PEN_STYLE, RDW_ERASE, RDW_FRAME,
    RDW_INVALIDATE, RDW_NOERASE, RDW_UPDATENOW, SRCCOPY, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    GetComboBoxInfo, SetWindowTheme, CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDRF_DODEFAULT,
    CDRF_NOTIFYITEMDRAW, CDRF_SKIPDEFAULT, CDRF_SKIPPOSTPAINT, COMBOBOXINFO, HDITEMW, HDI_TEXT,
    LVIF_TEXT, LVITEMW, NMLVCUSTOMDRAW, NM_CUSTOMDRAW,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, IsWindowEnabled, TrackMouseEvent, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT,
};
use windows::Win32::UI::Shell::{
    DefSubclassProc, GetWindowSubclass, RemoveWindowSubclass, SetWindowSubclass, SUBCLASSPROC,
};
#[cfg(test)]
use windows::Win32::UI::WindowsAndMessaging::WS_VSCROLL;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetClassNameW, GetClientRect, GetCursorPos, GetParent, GetPropW,
    GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, HideCaret,
    PostMessageW, RemovePropW, SendMessageW, SetPropW, SetWindowLongPtrW, SetWindowPos, ShowCaret,
    GWL_EXSTYLE, GWL_STYLE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    WM_CANCELMODE, WM_CAPTURECHANGED, WM_ENABLE, WM_ERASEBKGND, WM_GETFONT, WM_KEYDOWN, WM_KEYUP,
    WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCDESTROY, WM_NCPAINT, WM_NOTIFY,
    WM_PAINT, WM_SETCURSOR, WM_SETFOCUS, WM_SETTEXT, WM_SIZE, WM_THEMECHANGED, WS_BORDER,
    WS_CLIPCHILDREN, WS_EX_CLIENTEDGE, WS_EX_LAYERED,
};
use winreg::enums::HKEY_CURRENT_USER;
use winreg::RegKey;

use super::controls::{
    alpha_blend_premultiplied_bgra, button_visual, draw_alpha_composited_text,
    draw_antialiased_control_frame, draw_opaque_surface_text, draw_progress,
    fill_alpha_opaque_rect, fill_round_rect_antialiased, rounded_control_frame_geometry,
    single_line_edit_frame, single_line_edit_frame_owner, ButtonRole, ControlState, InnoMetrics,
    ProgressRole,
};

const fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    COLORREF((red as u32) | ((green as u32) << 8) | ((blue as u32) << 16))
}

/// Inno Setup 6.7 `WizardStyle=modern ... windows11` color roles.
/// Values are taken from the fixed-reference screenshots and its Windows 11 VCL styles.
#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    pub window: COLORREF,
    pub nav: COLORREF,
    pub edit: COLORREF,
    pub button: COLORREF,
    pub button_hot: COLORREF,
    pub button_pressed: COLORREF,
    pub text: COLORREF,
    pub text_secondary: COLORREF,
    pub text_disabled: COLORREF,
    pub border: COLORREF,
    pub separator: COLORREF,
    pub accent_fill: COLORREF,
    pub accent_border: COLORREF,
    /// Highlighted Next/install action, selected navigation entry and selected report/list row.
    pub highlight_fill: COLORREF,
    pub highlight_border: COLORREF,
    /// Inno Modern Windows 11 task progress and checked-state accent.
    pub progress: COLORREF,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MaterialSurfaceState {
    Normal,
    Hot,
    Pressed,
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MaterialSurfaceVisual {
    pub fill: COLORREF,
    pub border: COLORREF,
    pub fill_alpha: u8,
    pub border_alpha: u8,
}

const fn composite_channel(source: u32, alpha: u32, background: u32) -> u32 {
    (source * alpha + background * (255 - alpha) + 127) / 255
}

const fn composite_color(source: COLORREF, alpha: u8, background: COLORREF) -> COLORREF {
    let alpha = alpha as u32;
    rgb(
        composite_channel(source.0 & 0xff, alpha, background.0 & 0xff) as u8,
        composite_channel((source.0 >> 8) & 0xff, alpha, (background.0 >> 8) & 0xff) as u8,
        composite_channel((source.0 >> 16) & 0xff, alpha, (background.0 >> 16) & 0xff) as u8,
    )
}

impl Palette {
    pub const LIGHT: Self = Self {
        dark: false,
        window: rgb(249, 249, 249),
        nav: rgb(249, 249, 249),
        edit: rgb(255, 255, 255),
        button: rgb(253, 253, 253),
        button_hot: rgb(249, 249, 249),
        button_pressed: rgb(233, 233, 233),
        text: rgb(0, 0, 0),
        text_secondary: rgb(59, 59, 59),
        text_disabled: rgb(157, 157, 157),
        border: rgb(230, 230, 230),
        separator: rgb(222, 222, 222),
        accent_fill: rgb(0, 95, 184),
        accent_border: rgb(0, 96, 184),
        highlight_fill: rgb(0, 95, 184),
        highlight_border: rgb(0, 96, 184),
        progress: rgb(113, 199, 132),
    };

    pub const DARK: Self = Self {
        dark: true,
        window: rgb(43, 43, 43),
        nav: rgb(43, 43, 43),
        edit: rgb(28, 28, 28),
        button: rgb(48, 48, 48),
        button_hot: rgb(55, 55, 55),
        button_pressed: rgb(41, 41, 41),
        text: rgb(255, 255, 255),
        text_secondary: rgb(214, 214, 214),
        text_disabled: rgb(120, 120, 120),
        border: rgb(61, 61, 61),
        separator: rgb(72, 72, 72),
        accent_fill: rgb(49, 72, 83),
        accent_border: rgb(66, 149, 192),
        // User-audited Windows 11 selection colour from the supplied RGB sample.
        highlight_fill: rgb(76, 194, 255),
        highlight_border: rgb(76, 194, 255),
        progress: rgb(113, 199, 132),
    };

    /// Uses DWM's black glass key for window/nav pixels that should reveal a system backdrop.
    /// Ordinary buttons use the material overlay directly. Classic Edit, ComboBox, ListBox and
    /// ListView child HWNDs cannot publish per-pixel alpha into the parent DWM surface reliably, so
    /// they use the exact same overlay resolved against the documented Mica fallback neutral.
    /// Highlighted actions and selected rows remain opaque.
    pub const fn with_system_backdrop_surface(mut self) -> Self {
        let background = self.system_backdrop_edge_fallback();
        let normal = self.material_surface_visual(MaterialSurfaceState::Normal);
        let hot = self.material_surface_visual(MaterialSurfaceState::Hot);
        let pressed = self.material_surface_visual(MaterialSurfaceState::Pressed);
        self.edit = composite_color(normal.fill, normal.fill_alpha, background);
        self.button = self.edit;
        self.button_hot = composite_color(hot.fill, hot.fill_alpha, background);
        self.button_pressed = composite_color(pressed.fill, pressed.fill_alpha, background);
        self.border = composite_color(normal.border, normal.border_alpha, background);
        self.separator = self.border;
        self.window = COLORREF(0);
        self.nav = COLORREF(0);
        self
    }

    pub(crate) const fn material_surface_visual(
        self,
        state: MaterialSurfaceState,
    ) -> MaterialSurfaceVisual {
        if self.dark {
            let (fill, fill_alpha) = match state {
                // Mica is the base layer, not the control surface itself. Keep enough opacity
                // that dark controls remain distinct from the wallpaper while preserving the
                // cool blue-grey material tint used by the original audited UI.
                MaterialSurfaceState::Normal => (rgb(72, 88, 120), 180),
                MaterialSurfaceState::Hot => (rgb(85, 105, 145), 200),
                MaterialSurfaceState::Pressed => (rgb(60, 75, 105), 190),
                MaterialSurfaceState::Disabled => (rgb(64, 78, 108), 154),
            };
            MaterialSurfaceVisual {
                fill,
                border: rgb(145, 165, 205),
                fill_alpha,
                border_alpha: if matches!(state, MaterialSurfaceState::Disabled) {
                    104
                } else {
                    130
                },
            }
        } else {
            let (fill, fill_alpha) = match state {
                // Pure white over light Mica reads as a solid rectangle. A cooler, lower-alpha
                // surface leaves the material perceptible without sacrificing field contrast.
                MaterialSurfaceState::Normal => (rgb(232, 239, 248), 118),
                MaterialSurfaceState::Hot => (rgb(238, 244, 252), 145),
                MaterialSurfaceState::Pressed => (rgb(214, 224, 236), 138),
                MaterialSurfaceState::Disabled => (rgb(224, 233, 244), 104),
            };
            MaterialSurfaceVisual {
                fill,
                border: rgb(113, 131, 154),
                fill_alpha,
                border_alpha: if matches!(state, MaterialSurfaceState::Disabled) {
                    52
                } else {
                    64
                },
            }
        }
    }

    const fn uses_system_backdrop_surface(self) -> bool {
        self.window.0 == 0 && self.nav.0 == 0
    }

    /// Classic child HWNDs are not independent DWM alpha surfaces. Their Edit client is published
    /// as one opaque BGRA frame below, so both background and text must use final theme colours.
    /// Treating them as glass contributions makes black light-theme glyphs transparent/white.
    pub(crate) const fn edit_brush_color(self) -> COLORREF {
        self.edit
    }

    pub(crate) unsafe fn edit_brush_color_for(self, _control: HWND) -> COLORREF {
        // A compatible bitmap plus BitBlt makes the Edit update atomic, but it does not turn a
        // classic child HWND into an independent opaque-alpha surface. DWM still resolves those
        // RGB values over the extended frame, so always submit the material contribution rather
        // than the already-resolved field colour; otherwise dark fields are composited twice.
        self.edit_brush_color()
    }

    /// The atomic Edit painter repairs every output pixel to opaque BGRA after USER32 renders it,
    /// but the classic child target DC still treats exact black as the extended-frame glass key.
    /// Use the same near-black foreground as material labels in light mode; visually it matches
    /// ordinary button text while remaining an opaque DWM contribution.
    pub(crate) const fn edit_text_color(self) -> COLORREF {
        if self.uses_system_backdrop_surface() && !self.dark {
            self.foreground_black()
        } else {
            self.text
        }
    }

    pub(crate) unsafe fn edit_text_color_for(self, _control: HWND) -> COLORREF {
        self.edit_text_color()
    }

    /// Opaque colour used only while rasterizing an antialiased edge on a stock child HWND.
    ///
    /// DWM resolves the real system material on the top-level window, but a classic Edit,
    /// ComboBox or ListView is not an independent per-pixel-alpha surface.  Publishing partially
    /// transparent BGRA into those child DCs turns the edge into black/white corner pixels.  The
    /// Windows 11 Mica fallback neutrals are deliberately close to the resolved material and let
    /// us premix the few boundary pixels without replacing the material behind the control.
    const fn system_backdrop_edge_fallback(self) -> COLORREF {
        if self.dark {
            rgb(32, 32, 32)
        } else {
            rgb(243, 243, 243)
        }
    }

    /// A light material needs a slightly clearer one-pixel field stroke than the opaque page.
    /// The same absolute colour is used for both straight edges and rounded samples.
    const fn control_border(self) -> COLORREF {
        if self.uses_system_backdrop_surface() && !self.dark {
            self.separator
        } else {
            self.border
        }
    }

    pub const fn foreground_black(self) -> COLORREF {
        if self.uses_system_backdrop_surface() {
            if self.dark {
                rgb(1, 1, 1)
            } else {
                rgb(24, 24, 24)
            }
        } else {
            rgb(0, 0, 0)
        }
    }

    pub fn system() -> Self {
        #[cfg(feature = "non-elevated-tests")]
        match std::env::var("LETRECOVERY_UI_THEME").as_deref() {
            Ok("dark") => return Self::DARK,
            Ok("light") => return Self::LIGHT,
            _ => {}
        }

        let personalization = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize")
            .ok();
        let apps_use_light: Option<u32> = personalization
            .as_ref()
            .and_then(|key| key.get_value("AppsUseLightTheme").ok());
        if apps_use_light == Some(0) {
            Self::DARK
        } else {
            Self::LIGHT
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeControlKind {
    General,
    Field,
    ScrollableField,
    List,
    ListView,
    Header,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeThemeClass {
    Explorer,
    DarkExplorer,
    Cfd,
    DarkCfd,
    ItemsView,
    DarkItemsView,
}

const fn native_theme_class(kind: NativeControlKind, dark: bool) -> NativeThemeClass {
    match (kind, dark) {
        (NativeControlKind::Header, false) => NativeThemeClass::ItemsView,
        (NativeControlKind::Header, true) => NativeThemeClass::DarkItemsView,
        (NativeControlKind::Field, false) => NativeThemeClass::Cfd,
        (NativeControlKind::Field, true) => NativeThemeClass::DarkCfd,
        (
            NativeControlKind::General
            | NativeControlKind::ScrollableField
            | NativeControlKind::ListView
            | NativeControlKind::List,
            true,
        ) => NativeThemeClass::DarkExplorer,
        _ => NativeThemeClass::Explorer,
    }
}

/// Applies the native theme class that covers both the control client area and its non-client
/// scrollbar. Multiline edits deliberately use Explorer rather than CFD in dark mode because the
/// latter leaves a light Win32 scrollbar on several supported Windows builds.
///
/// Field frames (Edit / ComboBox / ListBox) use the host Windows 11 visual styles only. A previous
/// owner-drawn rounded overlay left residual system-accent “blue feet” at the four rectangular
/// corners and fought the Fluent control chrome, so it is no longer installed on those HWNDs.
pub unsafe fn apply_control_theme(control: HWND, palette: Palette, kind: NativeControlKind) {
    if let Some(edit) = single_line_edit_frame_owner(control) {
        apply_single_line_edit_frame_theme(control, edit, palette);
        return;
    }
    let class = match native_theme_class(kind, palette.dark) {
        NativeThemeClass::Explorer => w!("Explorer"),
        NativeThemeClass::DarkExplorer => w!("DarkMode_Explorer"),
        NativeThemeClass::Cfd => w!("CFD"),
        NativeThemeClass::DarkCfd => w!("DarkMode_CFD"),
        NativeThemeClass::ItemsView => w!("ItemsView"),
        NativeThemeClass::DarkItemsView => w!("DarkMode_ItemsView"),
    };
    let _ = SetWindowTheme(control, class, PCWSTR::null());
    let class_name = control_class_name(control);
    let is_edit = is_edit_class(&class_name);
    let is_combo = is_combo_class(&class_name);
    let control_style = GetWindowLongPtrW(control, GWL_STYLE);
    if is_auto_checkbox(&class_name, control_style) {
        // Inno's themed checkbox state table is the Windows BUTTON theme: BP_CHECKBOX/CBS_*.
        // Keep USER32's state machine, keyboard handling, accessibility and BN_CLICKED semantics,
        // while the subclass asks UxTheme for the actual current Windows 11 glyph for every state.
        // Do not disable the theme here: the previous hand-drawn replacement is what produced the
        // coarse check mark and visibly different Win10/Win11 geometry reported by the user.
        let _ = SetWindowSubclass(
            control,
            Some(check_box_subclass),
            CHECK_BOX_SUBCLASS_ID,
            palette_reference(palette),
        );
        let _ = InvalidateRect(control, None, false);
    } else if is_auto_radio_button(&class_name, control_style) {
        // Radio buttons use the same Windows BUTTON theme, with BP_RADIOBUTTON/RBS_* states.
        // This also removes the extra focus ring created by the former custom ellipse renderer.
        let _ = SetWindowSubclass(
            control,
            Some(radio_button_subclass),
            RADIO_BUTTON_SUBCLASS_ID,
            palette_reference(palette),
        );
        let _ = InvalidateRect(control, None, false);
    }
    if is_edit && is_single_line_edit(control) {
        // The direct child Edit keeps native text/caret/selection/IME and accessibility. Its
        // borderless client is vertically centred over a separate sibling surface, so USER32
        // never paints into the full-height rounded frame or controls its optical baseline.
        let _ = SetWindowTheme(control, w!(""), w!(""));
        apply_borderless_style(control);
        disable_edit_layered_redirection(control);
        let _ = RemoveWindowSubclass(
            control,
            Some(rounded_control_subclass),
            ROUNDED_CONTROL_SUBCLASS_ID,
        );
        let _ = SetWindowSubclass(
            control,
            Some(single_line_edit_subclass),
            SINGLE_LINE_EDIT_SUBCLASS_ID,
            palette_reference(palette),
        );
        if let Some(frame) = single_line_edit_frame(control) {
            apply_single_line_edit_frame_theme(frame, control, palette);
        }
        let _ = SetWindowPos(
            control,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        apply_material_rounded_control_region(control, palette);
        let _ = RedrawWindow(
            control,
            None,
            None,
            RDW_FRAME | RDW_INVALIDATE | RDW_NOERASE | RDW_UPDATENOW,
        );
    } else if is_edit
        && matches!(kind, NativeControlKind::ScrollableField)
        && is_read_only_edit(control)
    {
        // A fixed report is still a native multiline EDIT so USER32 keeps text selection,
        // keyboard navigation and its WS_VSCROLL scrollbar.  Only remove the competing square
        // non-client edge and paint the same deterministic rounded frame used by list surfaces.
        apply_borderless_style(control);
        let _ = SetWindowSubclass(
            control,
            Some(rounded_control_subclass),
            ROUNDED_CONTROL_SUBCLASS_ID,
            palette_reference(palette),
        );
        apply_material_rounded_control_region(control, palette);
        let _ = InvalidateRect(control, None, false);
    } else if is_edit {
        apply_single_border_style(control);
    } else if is_combo && matches!(kind, NativeControlKind::Field) {
        // The closed selection field is fully painted by rounded_control_subclass. Leaving the
        // active UxTheme renderer enabled lets its hover timer briefly compose a rectangular frame
        // before our rounded overlay. Disable that renderer on the closed HWND only; the separate
        // ComboLBox popup is themed below and remains entirely native.
        let _ = SetWindowTheme(control, w!(""), w!(""));
        apply_borderless_style(control);
        let _ = SetWindowSubclass(
            control,
            Some(rounded_control_subclass),
            ROUNDED_CONTROL_SUBCLASS_ID,
            palette_reference(palette),
        );
        set_combo_selection_field_height(
            control,
            InnoMetrics::for_dpi(GetDpiForWindow(control).max(96)).field_height,
        );
        set_combo_popup_row_height(
            control,
            InnoMetrics::for_dpi(GetDpiForWindow(control).max(96)).field_height,
        );
        clip_combo_to_closed_field(control, palette);
        let _ = InvalidateRect(control, None, false);
    } else if matches!(kind, NativeControlKind::List) && is_list_box(control) {
        // Standalone ListBoxes retain their existing Inno row palette, but the HWND itself has one
        // clipped, borderless surface so USER32 cannot expose square blue/black corner pixels.
        apply_borderless_style(control);
        let _ = SetWindowSubclass(
            control,
            Some(rounded_control_subclass),
            ROUNDED_CONTROL_SUBCLASS_ID,
            palette_reference(palette),
        );
        apply_material_rounded_control_region(control, palette);
        let _ = InvalidateRect(control, None, false);
    } else if matches!(
        kind,
        NativeControlKind::Field | NativeControlKind::ScrollableField | NativeControlKind::List
    ) {
        // ComboBox and other field HWNDs: drop any earlier rounded-frame subclass so only the
        // Windows 11 themed border remains.
        let _ = RemoveWindowSubclass(
            control,
            Some(rounded_control_subclass),
            ROUNDED_CONTROL_SUBCLASS_ID,
        );
        clear_control_window_region(control);
        let _ = InvalidateRect(control, None, false);
    }

    // A native ComboBox owns a separate top-level ComboLBox window. The list is not covered by
    // theming the ComboBox HWND itself, which otherwise leaves a white popup in dark mode.
    let mut info = COMBOBOXINFO {
        cbSize: std::mem::size_of::<COMBOBOXINFO>() as u32,
        ..Default::default()
    };
    let _ = GetComboBoxInfo(control, &mut info);
    if is_combo && matches!(kind, NativeControlKind::Field) && is_drop_down_list(control) {
        install_combo_selection_item_subclass(control, palette);
    }
    if !info.hwndList.0.is_null() {
        // The popup is a ListBox, not another field frame.  DarkMode_CFD is correct for the
        // closed ComboBox but corrupts the popup/arrow painting on some Windows 11 builds (the
        // selected string is drawn a second time in the arrow area).  Explorer keeps the popup
        // dark without changing the closed ComboBox renderer.
        let popup_class = if palette.dark {
            w!("DarkMode_Explorer")
        } else {
            w!("Explorer")
        };
        let _ = SetWindowTheme(info.hwndList, popup_class, PCWSTR::null());
        set_combo_popup_row_height(
            control,
            InnoMetrics::for_dpi(GetDpiForWindow(control).max(96)).field_height,
        );
        // Inno's TNewComboBox is a stock TComboBox. Preserve USER32's normal rectangular popup
        // renderer; this removes the slower WM_DRAWITEM/rounded-overlay paths and keeps keyboard,
        // hover and accessibility behaviour identical to the native control.
        let _ = RemoveWindowSubclass(
            info.hwndList,
            Some(rounded_control_subclass),
            ROUNDED_CONTROL_SUBCLASS_ID,
        );
        let _ = RemovePropW(info.hwndList, LIST_BOX_HOT_PROPERTY);
        apply_combo_popup_native_chrome(info.hwndList, palette);
        let _ = InvalidateRect(info.hwndList, None, false);
    }
}

/// Installs the compatibility painter on USER32's read-only closed selection child.
///
/// Some WinPE USER32 builds create or recreate this child only after the first focus/drop-down
/// transition, so this helper is intentionally idempotent and is called both during initial theme
/// application and from the parent ComboBox state transitions.
unsafe fn install_combo_selection_item_subclass(combo: HWND, palette: Palette) {
    let mut info = COMBOBOXINFO {
        cbSize: std::mem::size_of::<COMBOBOXINFO>() as u32,
        ..Default::default()
    };
    if GetComboBoxInfo(combo, &mut info).is_err()
        || info.hwndItem.0.is_null()
        || info.hwndItem == combo
    {
        return;
    }
    if GetPropW(info.hwndItem, COMBO_SELECTION_ITEM_PREPARED_PROPERTY).is_invalid() {
        let _ = SetWindowTheme(info.hwndItem, w!(""), w!(""));
        apply_borderless_style(info.hwndItem);
        let _ = SetPropW(
            info.hwndItem,
            COMBO_SELECTION_ITEM_PREPARED_PROPERTY,
            HANDLE(std::ptr::dangling_mut()),
        );
    }
    let _ = SetWindowSubclass(
        info.hwndItem,
        Some(combo_selection_item_subclass),
        COMBO_SELECTION_ITEM_SUBCLASS_ID,
        palette_reference(palette),
    );
    repaint_combo_selection_item_now(info.hwndItem, palette);
}

const BACKDROP_STATIC_SUBCLASS_ID: usize = 0x4c52_4253;

/// Installs alpha-aware text painting on every plain STATIC descendant. Direct labels retain
/// UxTheme's glass compositor; labels inside the tool-dialog content child use a deterministic
/// premultiplied glyph mask because nested HWND redirection otherwise discards their text alpha.
pub unsafe fn apply_backdrop_composition_to_descendants(root: HWND, palette: Palette) {
    let reference = palette_reference(palette);
    let _ = EnumChildWindows(
        root,
        Some(prepare_backdrop_descendant),
        LPARAM(reference as isize),
    );
}

unsafe extern "system" fn prepare_backdrop_descendant(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let palette = palette_from_reference(lparam.0 as usize);
    let class_name = control_class_name(hwnd);
    let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
    // SS_LEFT/SS_CENTER/SS_RIGHT are the only text-only STATIC types. Icons, bitmaps, frames and
    // owner-draw statics retain their existing renderer.
    if class_name.eq_ignore_ascii_case("Static") && style & 0x1f <= 2 {
        let _ = SetWindowSubclass(
            hwnd,
            Some(backdrop_static_subclass),
            BACKDROP_STATIC_SUBCLASS_ID,
            palette_reference(palette),
        );
        let _ = InvalidateRect(hwnd, None, false);
    }
    BOOL(1)
}

unsafe extern "system" fn backdrop_static_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    match message {
        WM_PAINT => {
            let palette = palette_from_reference(reference_data);
            if palette.uses_system_backdrop_surface() && !palette.dark {
                paint_backdrop_static(hwnd, palette);
                LRESULT(0)
            } else {
                DefSubclassProc(hwnd, message, wparam, lparam)
            }
        }
        WM_ENABLE | WM_SETTEXT | WM_THEMECHANGED => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            let _ = InvalidateRect(hwnd, None, false);
            result
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(
                hwnd,
                Some(backdrop_static_subclass),
                BACKDROP_STATIC_SUBCLASS_ID,
            );
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

unsafe fn paint_backdrop_static(hwnd: HWND, palette: Palette) {
    let mut paint = PAINTSTRUCT::default();
    let dc = BeginPaint(hwnd, &mut paint);
    let mut rect = RECT::default();
    let _ = GetClientRect(hwnd, &mut rect);
    let length = GetWindowTextLengthW(hwnd).max(0) as usize;
    let mut text = vec![0u16; length + 1];
    let copied = GetWindowTextW(hwnd, &mut text).max(0) as usize;
    text.truncate(copied);
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if width > 0 && height > 0 {
        // A solid fallback fill turns every label HWND into the white rectangles visible in the
        // user's light-Mica screenshot whenever DWM drops the dark glyph contribution. Clear the
        // child to the real glass key, then publish only a premultiplied glyph mask. The parent
        // backdrop remains visible around the text and no opaque intermediate label can exist.
        let glass = CreateSolidBrush(COLORREF(0));
        let _ = FillRect(dc, &rect, glass);
        let _ = DeleteObject(glass);
        let font = SendMessageW(hwnd, WM_GETFONT, WPARAM(0), LPARAM(0));
        let old_font = (font.0 != 0)
            .then(|| SelectObject(dc, windows::Win32::Graphics::Gdi::HGDIOBJ(font.0 as *mut _)));
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        let mut flags = DT_NOPREFIX;
        flags |= match style & 0x3 {
            1 => DT_CENTER,
            2 => DT_RIGHT,
            _ => windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT(0),
        };
        let dpi = GetDpiForWindow(hwnd).max(96);
        if style & 0x0200 != 0 {
            flags |= DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS;
        } else if height <= scale(36, dpi) {
            // Match the stock SS_LEFT baseline used without Mica. Vertically centring every short
            // STATIC only in the material painter made all labels jump down on activation.
            flags |= DT_SINGLELINE | DT_END_ELLIPSIS;
        } else {
            flags |= DT_WORDBREAK;
        }
        draw_alpha_composited_text(
            hwnd,
            dc,
            &text,
            &mut rect,
            flags,
            if IsWindowEnabled(hwnd).as_bool() {
                palette.foreground_black()
            } else {
                palette.text_disabled
            },
            true,
        );
        if let Some(old_font) = old_font {
            let _ = SelectObject(dc, old_font);
        }
    }
    let _ = EndPaint(hwnd, &paint);
}

unsafe fn apply_combo_popup_native_chrome(popup: HWND, palette: Palette) {
    // ComboLBox is a separate top-level HWND and does not inherit dark mode from the owner.
    // Explicitly opt out of DWM rounding: the user requested the stock rectangular Windows popup,
    // while its client rows, keyboard navigation and accessibility remain entirely USER32-owned.
    let corner_preference = DWMWCP_DONOTROUND;
    let _ = DwmSetWindowAttribute(
        popup,
        DWMWA_WINDOW_CORNER_PREFERENCE,
        (&corner_preference as *const DWM_WINDOW_CORNER_PREFERENCE).cast(),
        std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
    );
    let immersive_dark = if palette.dark { 1i32 } else { 0i32 };
    let _ = DwmSetWindowAttribute(
        popup,
        DWMWA_USE_IMMERSIVE_DARK_MODE,
        (&immersive_dark as *const i32).cast(),
        std::mem::size_of_val(&immersive_dark) as u32,
    );
}

unsafe fn control_class_name(control: HWND) -> String {
    let mut buffer = [0u16; 64];
    let length = GetClassNameW(control, &mut buffer);
    String::from_utf16_lossy(&buffer[..usize::try_from(length.max(0)).unwrap_or(0)])
}

fn is_edit_class(class_name: &str) -> bool {
    class_name.eq_ignore_ascii_case("Edit")
}

fn is_combo_class(class_name: &str) -> bool {
    class_name.eq_ignore_ascii_case("ComboBox")
}

const fn button_style_is_auto_radio(style: isize) -> bool {
    const BUTTON_TYPE_MASK: isize = 0x000f;
    const BS_AUTORADIOBUTTON_VALUE: isize = 0x0009;
    style & BUTTON_TYPE_MASK == BS_AUTORADIOBUTTON_VALUE
}

fn is_auto_radio_button(class_name: &str, style: isize) -> bool {
    class_name.eq_ignore_ascii_case("Button") && button_style_is_auto_radio(style)
}

const fn button_style_is_auto_checkbox(style: isize) -> bool {
    const BUTTON_TYPE_MASK: isize = 0x000f;
    const BS_AUTOCHECKBOX_VALUE: isize = 0x0003;
    style & BUTTON_TYPE_MASK == BS_AUTOCHECKBOX_VALUE
}

fn is_auto_checkbox(class_name: &str, style: isize) -> bool {
    class_name.eq_ignore_ascii_case("Button") && button_style_is_auto_checkbox(style)
}

unsafe fn is_single_line_edit(control: HWND) -> bool {
    const ES_MULTILINE: isize = 0x0004;
    GetWindowLongPtrW(control, GWL_STYLE) & ES_MULTILINE == 0
}

unsafe fn is_read_only_edit(control: HWND) -> bool {
    const ES_READONLY: isize = 0x0800;
    GetWindowLongPtrW(control, GWL_STYLE) & ES_READONLY != 0
}

fn borderless_style_bits(style: isize, ex_style: isize) -> (isize, isize) {
    (
        style & !(WS_BORDER.0 as isize),
        ex_style & !(WS_EX_CLIENTEDGE.0 as isize),
    )
}

fn single_border_style_bits(style: isize, ex_style: isize) -> (isize, isize) {
    (
        style | WS_BORDER.0 as isize,
        ex_style & !(WS_EX_CLIENTEDGE.0 as isize),
    )
}

unsafe fn apply_borderless_style(control: HWND) {
    let style = GetWindowLongPtrW(control, GWL_STYLE);
    let ex_style = GetWindowLongPtrW(control, GWL_EXSTYLE);
    let (style, ex_style) = borderless_style_bits(style, ex_style);
    apply_control_frame_styles(control, style, ex_style);
}

unsafe fn apply_single_border_style(control: HWND) {
    let style = GetWindowLongPtrW(control, GWL_STYLE);
    let ex_style = GetWindowLongPtrW(control, GWL_EXSTYLE);
    let (style, ex_style) = single_border_style_bits(style, ex_style);
    apply_control_frame_styles(control, style, ex_style);
}

unsafe fn apply_control_frame_styles(control: HWND, style: isize, ex_style: isize) {
    let current_style = GetWindowLongPtrW(control, GWL_STYLE);
    let current_ex_style = GetWindowLongPtrW(control, GWL_EXSTYLE);
    if current_style == style && current_ex_style == ex_style {
        return;
    }
    let _ = SetWindowLongPtrW(control, GWL_STYLE, style);
    let _ = SetWindowLongPtrW(control, GWL_EXSTYLE, ex_style);
    let _ = SetWindowPos(
        control,
        None,
        0,
        0,
        0,
        0,
        SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
    );
    let _ = InvalidateRect(control, None, false);
}

/// Themes both halves of a report ListView. The header is a separate HWND and otherwise retains a
/// light background even when the list client colors are explicitly dark.
pub unsafe fn apply_list_view_theme(list: HWND, palette: Palette) -> Option<HWND> {
    // A report's header is a real child HWND. Microsoft documents that a parent without
    // WS_CLIPCHILDREN may draw over a child and make the child repaint afterward. ListView theme
    // timers then expose precisely that intermediate frame (body colour covering the header).
    // Apply the style centrally to every report before installing either painter.
    let style = GetWindowLongPtrW(list, GWL_STYLE);
    if style & WS_CLIPCHILDREN.0 as isize == 0 {
        let _ = SetWindowLongPtrW(list, GWL_STYLE, style | WS_CLIPCHILDREN.0 as isize);
        let _ = SetWindowPos(
            list,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    // USER32/comctl32 can repaint a square non-client border after hover, focus or header paint.
    // Keep the report implementation native, but reserve its outer frame for the deterministic
    // antialiased overlay installed below.
    apply_borderless_style(list);
    apply_control_theme(list, palette, NativeControlKind::ListView);
    apply_material_rounded_control_region(list, palette);
    // Comctl32 v6 explicitly provides double-buffered report painting for this purpose.  Some
    // callers already request it while creating their ListView, but applying it here as well keeps
    // every report (including the main install list in reduced WinPE builds) on the same path.
    const LVM_GETEXTENDEDLISTVIEWSTYLE: u32 = 0x1037;
    const LVM_SETEXTENDEDLISTVIEWSTYLE: u32 = 0x1036;
    const LVS_EX_DOUBLEBUFFER: isize = 0x0001_0000;
    let extended = SendMessageW(list, LVM_GETEXTENDEDLISTVIEWSTYLE, WPARAM(0), LPARAM(0)).0;
    let _ = SendMessageW(
        list,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        WPARAM(0),
        LPARAM(extended | LVS_EX_DOUBLEBUFFER),
    );
    // Do not make every caller remember the three independent ListView colour messages.  In
    // particular, an empty report has no item custom-draw callback and otherwise exposes the
    // class default white body in dark mode.
    set_list_view_colors(list, palette);
    let _ = InvalidateRect(list, None, false);
    let _ = SetWindowSubclass(
        list,
        Some(list_view_subclass),
        LIST_VIEW_SUBCLASS_ID,
        palette_reference(palette),
    );
    // Selection colour is delivered as NM_CUSTOMDRAW to the ListView parent rather than the
    // ListView itself. Install one keyed parent subclass per list, so dialogs containing two
    // reports remain independent and a real selected row uses the Inno highlighted-button fill.
    if let Ok(parent) = GetParent(list) {
        let list_value = list.0 as usize;
        let dark_flag = usize::from(palette.dark) << (usize::BITS - 1);
        let backdrop_flag =
            usize::from(palette.uses_system_backdrop_surface()) << (usize::BITS - 2);
        let _ = SetWindowSubclass(
            parent,
            Some(list_view_parent_subclass),
            LIST_VIEW_PARENT_SUBCLASS_ID ^ list_value,
            list_value | dark_flag | backdrop_flag,
        );
    }
    let header = SendMessageW(list, 0x101F, WPARAM(0), LPARAM(0)); // LVM_GETHEADER
    if header.0 == 0 {
        return None;
    }
    let header = HWND(header.0 as *mut _);
    // The header is completely painted by `header_subclass`. Leaving ItemsView active at the same
    // time lets UxTheme run a buffered hot/focus transition over that finished frame: the custom
    // text disappears for several timer ticks and then returns, which looks like a permanently
    // flickering hardware table. Disable only the header's visual-style renderer; the ListView
    // body and its non-client scrollbar retain ItemsView, sizing/hit-testing stay native, and one
    // deterministic painter owns every visible header pixel.
    let _ = SetWindowTheme(header, w!(""), w!(""));
    // A report header is parented to the ListView itself, so HDF_OWNERDRAW sends WM_DRAWITEM to
    // the ListView instead of our dialog content window.  Subclassing the header is the only
    // deterministic way to avoid dark ItemsView drawing black text on a black header.
    let _ = SetWindowSubclass(
        header,
        Some(header_subclass),
        HEADER_SUBCLASS_ID,
        palette_reference(palette),
    );
    let _ = InvalidateRect(header, None, false);
    Some(header)
}

unsafe fn set_list_view_colors(list: HWND, palette: Palette) {
    for (message, color) in [
        (0x1001, palette.edit), // LVM_SETBKCOLOR
        (0x1026, palette.edit), // LVM_SETTEXTBKCOLOR
        (0x1024, palette.text), // LVM_SETTEXTCOLOR
    ] {
        let _ = SendMessageW(list, message, WPARAM(0), LPARAM(color.0 as isize));
    }
}

/// Applies a deterministic Inno-style paint path to the one native progress control still used by
/// a tool dialog.  UxTheme's progress class ignores the app dark mode and otherwise leaves a light
/// trough in the partition-copy window.
pub unsafe fn apply_progress_theme(control: HWND, palette: Palette) {
    let _ = SetWindowTheme(control, PCWSTR::null(), PCWSTR::null());
    let _ = SetWindowSubclass(
        control,
        Some(progress_subclass),
        PROGRESS_SUBCLASS_ID,
        palette_reference(palette),
    );
    let _ = InvalidateRect(control, None, false);
}

/// Applies a deterministic dark/light paint path to the horizontal target-size trackbar.  The
/// standard trackbar theme has no supported dark variant and paints a nearly white channel/thumb.
pub unsafe fn apply_trackbar_theme(control: HWND, palette: Palette) {
    let _ = SetWindowTheme(control, PCWSTR::null(), PCWSTR::null());
    let _ = SetWindowSubclass(
        control,
        Some(trackbar_subclass),
        TRACKBAR_SUBCLASS_ID,
        palette_reference(palette),
    );
    let _ = InvalidateRect(control, None, false);
}

const HEADER_SUBCLASS_ID: usize = 0x4c52_4844;
const LIST_VIEW_SUBCLASS_ID: usize = 0x4c52_4c56;
const LIST_VIEW_PARENT_SUBCLASS_ID: usize = 0x4c52_4c50;
const PROGRESS_SUBCLASS_ID: usize = 0x4c52_5052;
const TRACKBAR_SUBCLASS_ID: usize = 0x4c52_5442;
const CHECK_BOX_SUBCLASS_ID: usize = 0x4c52_4342;
const RADIO_BUTTON_SUBCLASS_ID: usize = 0x4c52_5242;
const ROUNDED_CONTROL_SUBCLASS_ID: usize = 0x4c52_5243;
const SINGLE_LINE_EDIT_SUBCLASS_ID: usize = 0x4c52_4544;
const SINGLE_LINE_EDIT_FRAME_SUBCLASS_ID: usize = 0x4c52_4546;
const COMBO_SELECTION_ITEM_SUBCLASS_ID: usize = 0x4c52_4353;
const LIST_BOX_HOT_PROPERTY: PCWSTR = w!("LetRecovery.InnoListBox.HotItem");
const ROUNDED_CONTROL_HOT_PROPERTY: PCWSTR = w!("LetRecovery.InnoControl.Hot");
const COMBO_CARET_HIDDEN_PROPERTY: PCWSTR = w!("LetRecovery.InnoCombo.CaretHidden");
const COMBO_TRACKING_DROPPED_PROPERTY: PCWSTR = w!("LetRecovery.InnoCombo.TrackingDropped");
const COMBO_SELECTION_ITEM_PREPARED_PROPERTY: PCWSTR =
    w!("LetRecovery.InnoCombo.SelectionPrepared");
const RADIO_BUTTON_HOT_PROPERTY: PCWSTR = w!("LetRecovery.InnoRadio.Hot");
const PALETTE_REFERENCE_DARK: usize = 0x1;
const PALETTE_REFERENCE_SYSTEM_BACKDROP: usize = 0x2;
const CHECK_BOX_HOT_PROPERTY: PCWSTR = w!("LetRecovery.InnoCheck.Hot");
const WM_MOUSELEAVE_MESSAGE: u32 = 0x02a3;
const WM_NCMOUSEMOVE_MESSAGE: u32 = 0x00a0;
const WM_NCMOUSELEAVE_MESSAGE: u32 = 0x02a2;
const WM_REPAINT_TRACKING_COMBO: u32 = 0x8000 + 0x4c5;

unsafe fn update_existing_subclass_reference(
    hwnd: HWND,
    procedure: SUBCLASSPROC,
    subclass_id: usize,
    reference_data: usize,
) {
    let mut current_reference = 0usize;
    if GetWindowSubclass(hwnd, procedure, subclass_id, Some(&mut current_reference)).as_bool() {
        let _ = SetWindowSubclass(hwnd, procedure, subclass_id, reference_data);
    }
}

/// Refreshes only palette references after a top-level Mica activation transition.
///
/// The light/dark system theme has not changed, so reapplying UxTheme classes, non-client styles
/// and `SWP_FRAMECHANGED` is both unnecessary and visibly harmful: Edit recalculates its client
/// metrics and STATIC text changes rasterization between the inactive and active frames.  Keep the
/// existing native control structure and update only subclasses/colours that actually depend on
/// the material palette.
pub unsafe fn refresh_material_palette_to_descendants(root: HWND, palette: Palette) {
    let _ = EnumChildWindows(
        root,
        Some(refresh_material_palette_descendant),
        LPARAM(palette_reference(palette) as isize),
    );
}

unsafe extern "system" fn refresh_material_palette_descendant(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let reference = lparam.0 as usize;
    let palette = palette_from_reference(reference);
    let palette_subclasses: [(SUBCLASSPROC, usize); 10] = [
        (Some(check_box_subclass), CHECK_BOX_SUBCLASS_ID),
        (Some(radio_button_subclass), RADIO_BUTTON_SUBCLASS_ID),
        (Some(rounded_control_subclass), ROUNDED_CONTROL_SUBCLASS_ID),
        (
            Some(single_line_edit_subclass),
            SINGLE_LINE_EDIT_SUBCLASS_ID,
        ),
        (
            Some(single_line_edit_frame_subclass),
            SINGLE_LINE_EDIT_FRAME_SUBCLASS_ID,
        ),
        (
            Some(combo_selection_item_subclass),
            COMBO_SELECTION_ITEM_SUBCLASS_ID,
        ),
        (Some(backdrop_static_subclass), BACKDROP_STATIC_SUBCLASS_ID),
        (Some(list_view_subclass), LIST_VIEW_SUBCLASS_ID),
        (Some(progress_subclass), PROGRESS_SUBCLASS_ID),
        (Some(trackbar_subclass), TRACKBAR_SUBCLASS_ID),
    ];
    for (procedure, subclass_id) in palette_subclasses {
        update_existing_subclass_reference(hwnd, procedure, subclass_id, reference);
    }

    let class_name = control_class_name(hwnd);
    if class_name.eq_ignore_ascii_case("SysListView32") {
        set_list_view_colors(hwnd, palette);
        if let Ok(parent) = GetParent(hwnd) {
            let list_value = hwnd.0 as usize;
            let dark_flag = usize::from(palette.dark) << (usize::BITS - 1);
            let backdrop_flag =
                usize::from(palette.uses_system_backdrop_surface()) << (usize::BITS - 2);
            update_existing_subclass_reference(
                parent,
                Some(list_view_parent_subclass),
                LIST_VIEW_PARENT_SUBCLASS_ID ^ list_value,
                list_value | dark_flag | backdrop_flag,
            );
        }
        let header = HWND(SendMessageW(hwnd, 0x101f, WPARAM(0), LPARAM(0)).0 as *mut _);
        if !header.is_invalid() {
            update_existing_subclass_reference(
                header,
                Some(header_subclass),
                HEADER_SUBCLASS_ID,
                reference,
            );
        }
    } else if class_name.eq_ignore_ascii_case("ComboBox") {
        let mut info = COMBOBOXINFO {
            cbSize: std::mem::size_of::<COMBOBOXINFO>() as u32,
            ..Default::default()
        };
        if GetComboBoxInfo(hwnd, &mut info).is_ok() {
            if !info.hwndItem.is_invalid() && info.hwndItem != hwnd {
                update_existing_subclass_reference(
                    info.hwndItem,
                    Some(combo_selection_item_subclass),
                    COMBO_SELECTION_ITEM_SUBCLASS_ID,
                    reference,
                );
            }
            if !info.hwndList.is_invalid() {
                update_existing_subclass_reference(
                    info.hwndList,
                    Some(rounded_control_subclass),
                    ROUNDED_CONTROL_SUBCLASS_ID,
                    reference,
                );
            }
        }
    }
    BOOL(1)
}

/// Messages whose USER32/comctl32 default handling may repaint a native non-client scrollbar.
/// The rounded frame must be overlaid only after that handling finishes; otherwise the scrollbar
/// hover animation can restore square right-hand corners until the next full control repaint.
const fn native_scrollbar_may_repaint_frame(message: u32) -> bool {
    matches!(
        message,
        0x00a1 // WM_NCLBUTTONDOWN
            | 0x00a2 // WM_NCLBUTTONUP
            | 0x00a3 // WM_NCLBUTTONDBLCLK
            | 0x0113 // WM_TIMER (UxTheme scrollbar hover animation)
            | 0x0114 // WM_HSCROLL
            | 0x0115 // WM_VSCROLL
            | 0x020a // WM_MOUSEWHEEL
            | 0x020e // WM_MOUSEHWHEEL
            | 0x02a0 // WM_NCMOUSEHOVER
    )
}

/// Messages for which comctl32 can move existing ListView client pixels instead of repainting
/// every visible row. The deterministic rounded frame is overlaid on that same window surface, so
/// the moved pixels must be discarded after the native scroll completes or copies of the frame can
/// remain between rows and beside the scrollbars.
const fn list_view_scrolls_client_pixels(message: u32) -> bool {
    matches!(
        message,
        0x0114 // WM_HSCROLL
            | 0x0115 // WM_VSCROLL
            | 0x020a // WM_MOUSEWHEEL
            | 0x020e // WM_MOUSEHWHEEL
    )
}

const fn palette_reference(palette: Palette) -> usize {
    let dark = if palette.dark {
        PALETTE_REFERENCE_DARK
    } else {
        0
    };
    let backdrop = if palette.uses_system_backdrop_surface() {
        PALETTE_REFERENCE_SYSTEM_BACKDROP
    } else {
        0
    };
    dark | backdrop
}

const fn palette_from_reference(reference_data: usize) -> Palette {
    let palette = if reference_data & PALETTE_REFERENCE_DARK != 0 {
        Palette::DARK
    } else {
        Palette::LIGHT
    };
    if reference_data & PALETTE_REFERENCE_SYSTEM_BACKDROP != 0 {
        palette.with_system_backdrop_surface()
    } else {
        palette
    }
}

unsafe fn redraw_control_frame(control: HWND) {
    let _ = RedrawWindow(
        control,
        None,
        None,
        RDW_FRAME | RDW_INVALIDATE | RDW_NOERASE,
    );
}

unsafe fn repaint_list_view_header_now(list: HWND) {
    let header = SendMessageW(list, 0x101f, WPARAM(0), LPARAM(0)); // LVM_GETHEADER
    if header.0 == 0 {
        return;
    }
    let _ = RedrawWindow(
        HWND(header.0 as *mut _),
        None,
        None,
        RDW_INVALIDATE | RDW_NOERASE | RDW_UPDATENOW,
    );
}

unsafe fn invalidate_control_visual(control: HWND) {
    // Mouse hot-state changes are delivered in bursts (WM_SETCURSOR followed by one or more
    // WM_MOUSEMOVE messages).  Queue one paint transaction instead of synchronously forcing every
    // message through WM_PAINT; USER32 can then coalesce the update without an intermediate frame.
    let _ = RedrawWindow(
        control,
        None,
        None,
        RDW_FRAME | RDW_INVALIDATE | RDW_NOERASE,
    );
}

unsafe fn ensure_hot_tracking(hwnd: HWND, property: PCWSTR, non_client: bool) {
    if !GetPropW(hwnd, property).is_invalid() {
        return;
    }
    let mut tracking = TRACKMOUSEEVENT {
        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE
            | if non_client {
                TME_NONCLIENT
            } else {
                Default::default()
            },
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    if TrackMouseEvent(&mut tracking).is_ok()
        && SetPropW(hwnd, property, HANDLE(std::ptr::dangling_mut())).is_ok()
    {
        redraw_control_frame(hwnd);
    }
}

unsafe fn clear_hot_tracking(hwnd: HWND, property: PCWSTR) {
    if RemovePropW(hwnd, property).is_ok_and(|handle| !handle.is_invalid()) {
        redraw_control_frame(hwnd);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CheckBoxGeometry {
    glyph: RECT,
    text: RECT,
}

fn check_box_geometry(width: i32, height: i32, dpi: u32) -> Option<CheckBoxGeometry> {
    let width = width.max(0);
    let height = height.max(0);
    if width == 0 || height == 0 {
        return None;
    }
    let size = scale(13, dpi).max(1).min(width).min(height);
    let top = (height - size) / 2;
    Some(CheckBoxGeometry {
        glyph: RECT {
            left: 0,
            top,
            right: size,
            bottom: top + size,
        },
        text: RECT {
            left: (size + scale(5, dpi)).min(width),
            top: 0,
            right: width,
            bottom: height,
        },
    })
}

unsafe extern "system" fn check_box_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    const BM_GETCHECK_MESSAGE: u32 = 0x00f0;
    const BM_SETCHECK_MESSAGE: u32 = 0x00f1;
    const BM_GETSTATE_MESSAGE: u32 = 0x00f2;
    const BST_CHECKED_VALUE: isize = 0x0001;
    const BST_PUSHED_VALUE: isize = 0x0004;

    match message {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let checked = SendMessageW(hwnd, BM_GETCHECK_MESSAGE, WPARAM(0), LPARAM(0)).0
                == BST_CHECKED_VALUE;
            let button_state = SendMessageW(hwnd, BM_GETSTATE_MESSAGE, WPARAM(0), LPARAM(0)).0;
            paint_check_box(
                hwnd,
                palette_from_reference(reference_data),
                ControlState {
                    hot: !GetPropW(hwnd, CHECK_BOX_HOT_PROPERTY).is_invalid(),
                    pressed: button_state & BST_PUSHED_VALUE != 0,
                    disabled: !IsWindowEnabled(hwnd).as_bool(),
                    focused: GetFocus() == hwnd,
                },
                checked,
            );
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            ensure_hot_tracking(hwnd, CHECK_BOX_HOT_PROPERTY, false);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_MOUSELEAVE_MESSAGE => {
            clear_hot_tracking(hwnd, CHECK_BOX_HOT_PROPERTY);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_ENABLE | WM_SETFOCUS | WM_KILLFOCUS | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_KEYDOWN
        | WM_KEYUP | WM_CAPTURECHANGED | WM_THEMECHANGED | WM_SETTEXT | BM_SETCHECK_MESSAGE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if message == WM_ENABLE && wparam.0 == 0 {
                let _ = RemovePropW(hwnd, CHECK_BOX_HOT_PROPERTY);
            }
            invalidate_control_visual(hwnd);
            result
        }
        WM_CANCELMODE => {
            let _ = RemovePropW(hwnd, CHECK_BOX_HOT_PROPERTY);
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            let _ = InvalidateRect(hwnd, None, false);
            result
        }
        WM_NCDESTROY => {
            let _ = RemovePropW(hwnd, CHECK_BOX_HOT_PROPERTY);
            let _ = RemoveWindowSubclass(hwnd, Some(check_box_subclass), CHECK_BOX_SUBCLASS_ID);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

unsafe fn paint_check_box(hwnd: HWND, palette: Palette, state: ControlState, checked: bool) {
    paint_embedded_windows11_checkbox(hwnd, palette, state, checked);
}

#[derive(Clone, Copy)]
struct EmbeddedButtonGlyph {
    width: i32,
    height: i32,
    bgra: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/win11_button_theme.rs"));

const fn embedded_theme_dpi_index(dpi: u32) -> usize {
    if dpi < 108 {
        0
    } else if dpi < 132 {
        1
    } else if dpi < 168 {
        2
    } else {
        3
    }
}

const fn themed_button_state(state: ControlState, checked: bool) -> usize {
    let base = if checked { 4 } else { 0 };
    base + if state.disabled {
        3
    } else if state.pressed {
        2
    } else if state.hot {
        1
    } else {
        0
    }
}

fn embedded_button_glyph(
    dark: bool,
    dpi: u32,
    state: ControlState,
    checked: bool,
) -> &'static EmbeddedButtonGlyph {
    let mode = usize::from(dark);
    let dpi = embedded_theme_dpi_index(dpi);
    let state = themed_button_state(state, checked);
    &WIN11_CHECKBOX_THEME_GLYPHS[((mode * 4 + dpi) * 8) + state]
}

const fn preserve_visible_black_on_system_backdrop(
    background: u32,
    alpha: u32,
    blue: u32,
    green: u32,
    red: u32,
) -> (u32, u32, u32) {
    if background == 0 && alpha != 0 && blue == 0 && green == 0 && red == 0 {
        (1, 1, 1)
    } else {
        (blue, green, red)
    }
}

const fn checkbox_material_fallback(palette: Palette) -> Option<COLORREF> {
    if palette.uses_system_backdrop_surface() {
        Some(palette.system_backdrop_edge_fallback())
    } else {
        None
    }
}

unsafe fn draw_embedded_button_glyph(
    dc: HDC,
    rect: RECT,
    glyph: &EmbeddedButtonGlyph,
    background: COLORREF,
    material_fallback: Option<COLORREF>,
) {
    let width = (rect.right - rect.left).max(0);
    let height = (rect.bottom - rect.top).max(0);
    if width == 0 || height == 0 || glyph.width <= 0 || glyph.height <= 0 {
        return;
    }
    let background = background.0;
    let blend_background = if background == 0 {
        // Premix the complete fixed Win11 glyph tile against the documented neutral material
        // fallback. Mixing partially covered and fully transparent texels against literal black
        // is what made the original rounded checkbox look square and left four dark feet in light
        // mode. This changes only the tile's backdrop pixels, never its geometry or state image.
        material_fallback.map_or(background, |color| color.0)
    } else {
        background
    };
    let background_red = blend_background & 0xff;
    let background_green = (blend_background >> 8) & 0xff;
    let background_blue = (blend_background >> 16) & 0xff;
    let mut composed = Vec::with_capacity(glyph.bgra.len());
    for pixel in glyph.bgra.chunks_exact(4) {
        let alpha = u32::from(pixel[3]);
        let inverse = 255 - alpha;
        // UxTheme's buffered BUTTON renders are premultiplied BGRA. Multiplying the stored RGB a
        // second time darkens the partially covered corner pixels, which is why an unchecked box
        // showed four dark feet on a light page. Fully transparent PNG pixels carry an arbitrary
        // white RGB value, so treat alpha=0 as the destination background explicitly.
        let compose = |source: u8, destination: u32| {
            if alpha == 0 {
                destination
            } else {
                (u32::from(source) + (destination * inverse + 127) / 255).min(255)
            }
        };
        let blue = compose(pixel[0], background_blue);
        let green = compose(pixel[1], background_green);
        let red = compose(pixel[2], background_red);
        // Prevent an opaque black check mark or outline from being interpreted as another DWM
        // transparent hole when the surrounding page itself uses the black glass key.
        let (blue, green, red) =
            preserve_visible_black_on_system_backdrop(background, alpha, blue, green, red);
        composed.extend_from_slice(&[blue as u8, green as u8, red as u8, 255]);
    }

    let bitmap = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: glyph.width,
            // Negative height declares the generated BGRA rows as top-down.
            biHeight: -glyph.height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (glyph.width * glyph.height * 4) as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    if width == glyph.width && height == glyph.height {
        // Every supported DPI bucket is generated at its final physical size.  Use the
        // non-scaling DIB path so GDI cannot sample the transparent corner texels back into the
        // rounded Win11 glyph (which showed up as four dark pixels on a light page).
        let _ = SetDIBitsToDevice(
            dc,
            rect.left,
            rect.top,
            width as u32,
            height as u32,
            0,
            0,
            0,
            glyph.height as u32,
            composed.as_ptr().cast(),
            &bitmap,
            DIB_RGB_COLORS,
        );
    } else {
        let _ = SetStretchBltMode(dc, HALFTONE);
        let _ = StretchDIBits(
            dc,
            rect.left,
            rect.top,
            width,
            height,
            0,
            0,
            glyph.width,
            glyph.height,
            Some(composed.as_ptr().cast()),
            &bitmap,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
    }
}

/// USER32 continues to own the real checkbox state machine, keyboard handling, accessibility and
/// BN_CLICKED semantics. Only the visible checkbox glyph comes from the fixed Windows 11
/// `Aero.msstyles` reference, so Win10 and Win11 do not silently select different host themes.
unsafe fn paint_embedded_windows11_checkbox(
    hwnd: HWND,
    palette: Palette,
    state: ControlState,
    checked: bool,
) {
    let mut paint = PAINTSTRUCT::default();
    let dc = BeginPaint(hwnd, &mut paint);
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    fill(dc, &client, palette.window);
    let dpi = GetDpiForWindow(hwnd).max(96);
    let width = (client.right - client.left).max(0);
    let height = (client.bottom - client.top).max(0);
    if width == 0 || height == 0 {
        let _ = EndPaint(hwnd, &paint);
        return;
    }

    let Some(geometry) = check_box_geometry(width, height, dpi) else {
        let _ = EndPaint(hwnd, &paint);
        return;
    };
    let (glyph_rect, caption_rect) = (geometry.glyph, geometry.text);
    draw_embedded_button_glyph(
        dc,
        glyph_rect,
        embedded_button_glyph(palette.dark, dpi, state, checked),
        palette.window,
        checkbox_material_fallback(palette),
    );

    let text_length = GetWindowTextLengthW(hwnd).max(0) as usize;
    if text_length > 0 && caption_rect.right > caption_rect.left {
        let mut text = vec![0u16; text_length + 1];
        let copied = GetWindowTextW(hwnd, &mut text).max(0) as usize;
        text.truncate(copied);
        let font = SendMessageW(hwnd, WM_GETFONT, WPARAM(0), LPARAM(0));
        let old_font = (font.0 != 0)
            .then(|| SelectObject(dc, windows::Win32::Graphics::Gdi::HGDIOBJ(font.0 as *mut _)));
        let mut text_rect = caption_rect;
        draw_alpha_composited_text(
            hwnd,
            dc,
            &text,
            &mut text_rect,
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
            if state.disabled {
                palette.text_disabled
            } else {
                palette.text
            },
            palette.uses_system_backdrop_surface() && !palette.dark,
        );
        if let Some(old_font) = old_font {
            let _ = SelectObject(dc, old_font);
        }
    }
    let _ = EndPaint(hwnd, &paint);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RadioGeometry {
    glyph: RECT,
    text: RECT,
}

/// Computes the complete radio geometry from the real client rectangle. The separate left/right
/// halves keep odd, DPI-scaled glyphs centred without allowing the last pixel to escape the HWND.
fn radio_geometry(width: i32, height: i32, dpi: u32) -> Option<RadioGeometry> {
    let width = width.max(0);
    let height = height.max(0);
    if width == 0 || height == 0 {
        return None;
    }
    // The fixed Inno reference is roughly 19-20 physical pixels at 150% scaling.  A 13px
    // logical baseline matches that footprint; 18px made the glyph a conspicuous 27px disc.
    let preferred = scale(13, dpi).max(1);
    let glyph_size = preferred.min(height).min(width);
    let glyph_top = (height - glyph_size) / 2;
    let glyph = RECT {
        left: 0,
        top: glyph_top,
        right: glyph_size,
        bottom: glyph_top + glyph_size,
    };
    let text_left = (glyph.right + scale(5, dpi)).min(width);
    Some(RadioGeometry {
        glyph,
        text: RECT {
            left: text_left,
            top: 0,
            right: width,
            bottom: height,
        },
    })
}

/// Returns the fixed Windows 11 radio colours for the real USER32 state. The old captured PNGs
/// were already composited against a theme surface and exposed only binary alpha, so their dark
/// edge texels could never be recomposited correctly on the light page. Keep the audited colours,
/// but generate true coverage at the final physical size instead of stretching those captures.
fn radio_state_colors(
    palette: Palette,
    state: ControlState,
    checked: bool,
) -> (COLORREF, COLORREF) {
    if checked {
        let fill = if palette.dark {
            if state.disabled {
                rgb(74, 74, 74)
            } else if state.pressed {
                rgb(85, 172, 212)
            } else if state.hot {
                rgb(91, 189, 233)
            } else {
                rgb(96, 205, 255)
            }
        } else if state.disabled {
            rgb(195, 195, 195)
        } else if state.pressed {
            rgb(50, 126, 197)
        } else if state.hot {
            rgb(25, 110, 191)
        } else {
            palette.accent_fill
        };
        let centre = if palette.dark {
            palette.foreground_black()
        } else {
            rgb(255, 255, 255)
        };
        (fill, centre)
    } else if palette.dark {
        if state.disabled {
            (rgb(74, 74, 74), rgb(55, 55, 55))
        } else if state.pressed {
            (rgb(69, 69, 69), rgb(50, 50, 50))
        } else if state.hot {
            (rgb(170, 170, 170), rgb(49, 49, 49))
        } else {
            (rgb(170, 170, 170), rgb(36, 36, 36))
        }
    } else if state.disabled {
        (rgb(195, 195, 195), palette.window)
    } else if state.pressed {
        (rgb(195, 195, 195), rgb(226, 226, 226))
    } else if state.hot {
        (rgb(98, 98, 98), rgb(234, 234, 234))
    } else {
        (rgb(98, 98, 98), rgb(243, 243, 243))
    }
}

fn weighted_radio_color(
    background: COLORREF,
    background_samples: u32,
    primary: COLORREF,
    primary_samples: u32,
    secondary: COLORREF,
    secondary_samples: u32,
) -> COLORREF {
    let total = background_samples + primary_samples + secondary_samples;
    let channel = |shift: u32| {
        ((((background.0 >> shift) & 0xff) * background_samples
            + ((primary.0 >> shift) & 0xff) * primary_samples
            + ((secondary.0 >> shift) & 0xff) * secondary_samples
            + total / 2)
            / total)
            << shift
    };
    COLORREF(channel(0) | channel(8) | channel(16))
}

/// Produces a top-down BGRA glyph. Material mode keeps pixels outside the circular coverage fully
/// transparent and premultiplies the antialiased edge; ordinary mode remains opaque against the
/// normal page background. Both paths evaluate eight-by-eight coverage at the final DPI size.
fn radio_glyph_bgra(side: i32, palette: Palette, state: ControlState, checked: bool) -> Vec<u8> {
    const SAMPLES: i32 = 8;
    let side = side.max(1);
    let centre = f64::from(side) / 2.0;
    let outer_radius = centre;
    let ring_width = (f64::from(side) / 13.0).max(1.0);
    let inner_radius = (outer_radius - ring_width).max(0.0);
    let dot_radius = f64::from(side) * 2.5 / 13.0;
    let (primary, secondary) = radio_state_colors(palette, state, checked);
    let mut pixels = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            let mut background_samples = 0u32;
            let mut primary_samples = 0u32;
            let mut secondary_samples = 0u32;
            for sample_y in 0..SAMPLES {
                for sample_x in 0..SAMPLES {
                    let px = f64::from(x) + (f64::from(sample_x) + 0.5) / f64::from(SAMPLES);
                    let py = f64::from(y) + (f64::from(sample_y) + 0.5) / f64::from(SAMPLES);
                    let dx = px - centre;
                    let dy = py - centre;
                    let distance_squared = dx * dx + dy * dy;
                    if distance_squared > outer_radius * outer_radius {
                        background_samples += 1;
                    } else if checked {
                        if distance_squared <= dot_radius * dot_radius {
                            secondary_samples += 1;
                        } else {
                            primary_samples += 1;
                        }
                    } else if distance_squared >= inner_radius * inner_radius {
                        primary_samples += 1;
                    } else {
                        secondary_samples += 1;
                    }
                }
            }
            if palette.uses_system_backdrop_surface() {
                let covered = primary_samples + secondary_samples;
                let premultiplied_channel = |shift: u32| {
                    ((((primary.0 >> shift) & 0xff) * primary_samples
                        + ((secondary.0 >> shift) & 0xff) * secondary_samples
                        + 32)
                        / 64) as u8
                };
                pixels.extend_from_slice(&[
                    premultiplied_channel(16),
                    premultiplied_channel(8),
                    premultiplied_channel(0),
                    ((covered * 255 + 32) / 64) as u8,
                ]);
            } else {
                let color = weighted_radio_color(
                    palette.window,
                    background_samples,
                    primary,
                    primary_samples,
                    secondary,
                    secondary_samples,
                )
                .0;
                pixels.extend_from_slice(&[
                    ((color >> 16) & 0xff) as u8,
                    ((color >> 8) & 0xff) as u8,
                    (color & 0xff) as u8,
                    255,
                ]);
            }
        }
    }
    pixels
}

unsafe fn draw_radio_glyph(
    dc: HDC,
    rect: RECT,
    palette: Palette,
    state: ControlState,
    checked: bool,
) {
    let side = (rect.right - rect.left)
        .max(0)
        .min((rect.bottom - rect.top).max(0));
    if side == 0 {
        return;
    }
    let pixels = radio_glyph_bgra(side, palette, state, checked);
    if palette.uses_system_backdrop_surface()
        && alpha_blend_premultiplied_bgra(dc, rect.left, rect.top, side, side, &pixels)
    {
        return;
    }
    let bitmap = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: side,
            biHeight: -side,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (side * side * 4) as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    let _ = SetDIBitsToDevice(
        dc,
        rect.left,
        rect.top,
        side as u32,
        side as u32,
        0,
        0,
        0,
        side as u32,
        pixels.as_ptr().cast(),
        &bitmap,
        DIB_RGB_COLORS,
    );
}

unsafe extern "system" fn radio_button_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    const BM_GETCHECK_MESSAGE: u32 = 0x00f0;
    const BM_SETCHECK_MESSAGE: u32 = 0x00f1;
    const BM_GETSTATE_MESSAGE: u32 = 0x00f2;
    const BST_CHECKED_VALUE: isize = 0x0001;
    const BST_PUSHED_VALUE: isize = 0x0004;
    const WM_MOUSELEAVE_LOCAL: u32 = 0x02a3;

    match message {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let palette = palette_from_reference(reference_data);
            let checked = SendMessageW(hwnd, BM_GETCHECK_MESSAGE, WPARAM(0), LPARAM(0)).0
                == BST_CHECKED_VALUE;
            let button_state = SendMessageW(hwnd, BM_GETSTATE_MESSAGE, WPARAM(0), LPARAM(0)).0;
            let state = ControlState {
                hot: !GetPropW(hwnd, RADIO_BUTTON_HOT_PROPERTY).is_invalid(),
                pressed: button_state & BST_PUSHED_VALUE != 0,
                disabled: !IsWindowEnabled(hwnd).as_bool(),
                focused: GetFocus() == hwnd,
            };
            paint_radio_button(hwnd, palette, state, checked);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            ensure_hot_tracking(hwnd, RADIO_BUTTON_HOT_PROPERTY, false);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_MOUSELEAVE_LOCAL => {
            clear_hot_tracking(hwnd, RADIO_BUTTON_HOT_PROPERTY);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_ENABLE | WM_SETFOCUS | WM_KILLFOCUS | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_KEYDOWN
        | WM_KEYUP | WM_CAPTURECHANGED | WM_THEMECHANGED | WM_SETTEXT | BM_SETCHECK_MESSAGE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if message == WM_ENABLE && wparam.0 == 0 {
                let _ = RemovePropW(hwnd, RADIO_BUTTON_HOT_PROPERTY);
            }
            invalidate_control_visual(hwnd);
            result
        }
        WM_NCDESTROY => {
            let _ = RemovePropW(hwnd, RADIO_BUTTON_HOT_PROPERTY);
            let _ =
                RemoveWindowSubclass(hwnd, Some(radio_button_subclass), RADIO_BUTTON_SUBCLASS_ID);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

unsafe fn paint_radio_button(hwnd: HWND, palette: Palette, state: ControlState, checked: bool) {
    let mut paint = PAINTSTRUCT::default();
    let dc = BeginPaint(hwnd, &mut paint);
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    fill(dc, &client, palette.window);
    let dpi = GetDpiForWindow(hwnd).max(96);
    let width = (client.right - client.left).max(0);
    let height = (client.bottom - client.top).max(0);
    let Some(geometry) = radio_geometry(width, height, dpi) else {
        let _ = EndPaint(hwnd, &paint);
        return;
    };
    draw_radio_glyph(dc, geometry.glyph, palette, state, checked);

    let text_length = GetWindowTextLengthW(hwnd).max(0) as usize;
    if text_length > 0 && geometry.text.right > geometry.text.left {
        let mut text = vec![0u16; text_length + 1];
        let copied = GetWindowTextW(hwnd, &mut text).max(0) as usize;
        text.truncate(copied);
        let font = SendMessageW(hwnd, WM_GETFONT, WPARAM(0), LPARAM(0));
        let old_font = (font.0 != 0)
            .then(|| SelectObject(dc, windows::Win32::Graphics::Gdi::HGDIOBJ(font.0 as *mut _)));
        let mut text_rect = geometry.text;
        draw_alpha_composited_text(
            hwnd,
            dc,
            &text,
            &mut text_rect,
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
            if state.disabled {
                palette.text_disabled
            } else {
                palette.text
            },
            palette.uses_system_backdrop_surface() && !palette.dark,
        );
        if let Some(old_font) = old_font {
            let _ = SelectObject(dc, old_font);
        }
    }
    let _ = EndPaint(hwnd, &paint);
}

unsafe extern "system" fn header_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    match message {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint_header(hwnd, palette_from_reference(reference_data));
            LRESULT(0)
        }
        WM_THEMECHANGED => {
            let _ = InvalidateRect(hwnd, None, false);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(header_subclass), HEADER_SUBCLASS_ID);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

unsafe fn paint_header(hwnd: HWND, palette: Palette) {
    let mut paint = PAINTSTRUCT::default();
    let dc = BeginPaint(hwnd, &mut paint);
    let mut client = RECT::default();
    let _ = GetClientRect(hwnd, &mut client);
    fill(dc, &client, palette.button);
    let font = SendMessageW(hwnd, WM_GETFONT, WPARAM(0), LPARAM(0));
    let old_font = if font.0 != 0 {
        Some(SelectObject(
            dc,
            windows::Win32::Graphics::Gdi::HGDIOBJ(font.0 as *mut _),
        ))
    } else {
        None
    };
    let _ = SetBkMode(dc, TRANSPARENT);
    let _ = SetTextColor(dc, palette.text);
    let dpi = GetDpiForWindow(hwnd).max(96);
    let inset = scale(8, dpi);
    let count = SendMessageW(hwnd, 0x1200, WPARAM(0), LPARAM(0)).0 as i32; // HDM_GETITEMCOUNT
    for index in 0..count.max(0) {
        let mut rect = RECT::default();
        if SendMessageW(
            hwnd,
            0x1207, // HDM_GETITEMRECT
            WPARAM(index as usize),
            LPARAM((&mut rect as *mut RECT) as isize),
        )
        .0 == 0
        {
            continue;
        }
        let mut text = vec![0u16; 256];
        let mut item = HDITEMW {
            mask: HDI_TEXT,
            pszText: windows::core::PWSTR(text.as_mut_ptr()),
            cchTextMax: text.len() as i32,
            ..Default::default()
        };
        let _ = SendMessageW(
            hwnd,
            0x120B, // HDM_GETITEMW
            WPARAM(index as usize),
            LPARAM((&mut item as *mut HDITEMW) as isize),
        );
        text.truncate(
            text.iter()
                .position(|value| *value == 0)
                .unwrap_or(text.len()),
        );
        let mut text_rect = rect;
        text_rect.left += inset;
        text_rect.right -= inset.min((text_rect.right - text_rect.left).max(0));
        draw_alpha_composited_text(
            hwnd,
            dc,
            &text,
            &mut text_rect,
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
            palette.text,
            palette.uses_system_backdrop_surface() && !palette.dark,
        );
        let separator = RECT {
            left: rect.right - 1,
            top: rect.top + scale(4, dpi),
            right: rect.right,
            bottom: rect.bottom - scale(4, dpi),
        };
        fill(dc, &separator, palette.separator);
    }
    if let Some(old_font) = old_font {
        let _ = SelectObject(dc, old_font);
    }
    // The header is a child of the report and covers the report's top edge. Restore only the
    // authoritative frame after the header transaction, without invalidating either HWND; queuing
    // another report paint here would form a header/report feedback loop.
    let list = GetParent(hwnd).ok();
    let _ = EndPaint(hwnd, &paint);
    if let Some(list) = list {
        paint_rounded_control_frame(list, palette);
    }
}

unsafe extern "system" fn list_view_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    match message {
        WM_ERASEBKGND => {
            // An empty or temporarily disabled report receives no item custom-draw callbacks.
            // Fill the complete client area here so loading never exposes comctl32's white class
            // brush before the first row exists.
            let mut client = RECT::default();
            let _ = GetClientRect(hwnd, &mut client);
            fill(
                HDC(wparam.0 as *mut _),
                &client,
                palette_from_reference(reference_data).edit,
            );
            LRESULT(1)
        }
        WM_PAINT => {
            // A report with no rows never reaches NM_CUSTOMDRAW.  Some themed/disabled
            // comctl32 paths repaint the empty body with the class brush after WM_ERASEBKGND,
            // undoing the LVM_SETBKCOLOR value and exposing a white loading rectangle.  Own the
            // complete empty paint transaction so there is no later default fill to overwrite it.
            let item_count = SendMessageW(hwnd, 0x1004, WPARAM(0), LPARAM(0)).0; // LVM_GETITEMCOUNT
            if list_view_needs_empty_body_paint(item_count) {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut paint);
                let mut client = RECT::default();
                let _ = GetClientRect(hwnd, &mut client);
                fill(dc, &client, palette_from_reference(reference_data).edit);
                let _ = EndPaint(hwnd, &paint);
                repaint_list_view_header_now(hwnd);
                paint_rounded_control_frame(hwnd, palette_from_reference(reference_data));
                return LRESULT(0);
            }
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            // ItemsView can repaint the client area below the final row with its stock white
            // class brush after both LVM_SETBKCOLOR and item custom draw. Item callbacks cannot
            // cover an area that has no item, so restore only that trailing body rectangle after
            // the native report has finished; rows, header and scrollbars remain native.
            paint_list_view_trailing_body(hwnd, palette_from_reference(reference_data));
            // The v6 ListView double buffer can publish its body after the child header has
            // painted, temporarily replacing only the header background. Repaint that single
            // child synchronously after the body transaction; `paint_header` never invalidates
            // the report, so this ordering cannot form a feedback loop.
            repaint_list_view_header_now(hwnd);
            // Checkbox glyphs only — the list frame stays under the Windows 11 ItemsView /
            // Explorer theme so corners match other Fluent controls without blue residual feet.
            paint_list_view_checkboxes(hwnd, palette_from_reference(reference_data));
            paint_rounded_control_frame(hwnd, palette_from_reference(reference_data));
            result
        }
        WM_NCPAINT => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            paint_rounded_control_frame(hwnd, palette_from_reference(reference_data));
            result
        }
        message if native_scrollbar_may_repaint_frame(message) => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if list_view_scrolls_client_pixels(message) {
                // Native report scrolling may move already-painted client pixels and invalidate
                // only the newly exposed strip. Our final rounded frame also touches that surface,
                // so the optimisation otherwise copies the top/bottom edge into rows during a
                // vertical drag and the side edge across the body during a horizontal drag.
                // Queue one complete client repaint after default handling. InvalidateRect does
                // not paint synchronously; USER32 merges repeated SB_THUMBTRACK invalidations and
                // the existing LVS_EX_DOUBLEBUFFER path publishes the repaired body atomically.
                // Do not paint the frame or Header directly from this high-frequency branch.
                let _ = InvalidateRect(hwnd, None, false);
            }
            result
        }
        WM_ENABLE | WM_SETFOCUS | WM_KILLFOCUS | WM_SIZE | WM_THEMECHANGED => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            // Comctl32 can restore class-default colours while changing enabled/theme state.
            // Reassert all three ListView colours together; partial updates cause a white empty
            // body or black text background until the next full refresh.
            let palette = palette_from_reference(reference_data);
            set_list_view_colors(hwnd, palette);
            if matches!(message, WM_SIZE | WM_THEMECHANGED) {
                apply_material_rounded_control_region(hwnd, palette);
            }
            let _ = InvalidateRect(hwnd, None, false);
            result
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(list_view_subclass), LIST_VIEW_SUBCLASS_ID);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

const fn list_view_needs_empty_body_paint(item_count: isize) -> bool {
    item_count == 0
}

fn list_view_trailing_body_rect(
    client: RECT,
    item_count: isize,
    last_item_bottom: Option<i32>,
) -> Option<RECT> {
    if item_count <= 0 {
        return None;
    }
    let top = last_item_bottom?.clamp(client.top, client.bottom);
    (top < client.bottom).then_some(RECT {
        left: client.left,
        top,
        right: client.right,
        bottom: client.bottom,
    })
}

unsafe fn paint_list_view_trailing_body(hwnd: HWND, palette: Palette) {
    const LVM_GETITEMCOUNT: u32 = 0x1004;
    const LVM_GETITEMRECT: u32 = 0x100e;
    const LVIR_BOUNDS: i32 = 0;

    let item_count = SendMessageW(hwnd, LVM_GETITEMCOUNT, WPARAM(0), LPARAM(0)).0;
    if item_count <= 0 {
        return;
    }
    let mut client = RECT::default();
    if GetClientRect(hwnd, &mut client).is_err() {
        return;
    }
    let mut last = RECT {
        left: LVIR_BOUNDS,
        ..Default::default()
    };
    let last_bottom = (SendMessageW(
        hwnd,
        LVM_GETITEMRECT,
        WPARAM((item_count - 1) as usize),
        LPARAM((&mut last as *mut RECT) as isize),
    )
    .0 != 0)
        .then_some(last.bottom);
    let Some(rect) = list_view_trailing_body_rect(client, item_count, last_bottom) else {
        return;
    };
    let dc = windows::Win32::Graphics::Gdi::GetDC(hwnd);
    if !dc.is_invalid() {
        fill(dc, &rect, palette.edit);
        let _ = ReleaseDC(hwnd, dc);
    }
}

unsafe fn apply_single_line_edit_frame_theme(frame: HWND, edit: HWND, palette: Palette) {
    let _ = SetWindowTheme(frame, w!(""), w!(""));
    apply_borderless_style(frame);
    let _ = SetWindowSubclass(
        frame,
        Some(single_line_edit_frame_subclass),
        SINGLE_LINE_EDIT_FRAME_SUBCLASS_ID,
        palette_reference(palette),
    );
    paint_single_line_edit_frame(frame, edit, palette);
}

unsafe fn repaint_single_line_edit_frame(edit: HWND, immediate: bool) {
    let Some(frame) = single_line_edit_frame(edit) else {
        return;
    };
    let flags = RDW_INVALIDATE
        | RDW_NOERASE
        | if immediate {
            RDW_UPDATENOW
        } else {
            Default::default()
        };
    let _ = RedrawWindow(frame, None, None, flags);
}

unsafe fn paint_single_line_edit_frame(frame: HWND, edit: HWND, palette: Palette) {
    let mut paint = PAINTSTRUCT::default();
    let _ = BeginPaint(frame, &mut paint);
    let _ = EndPaint(frame, &paint);

    let dc = GetWindowDC(frame);
    if dc.is_invalid() {
        return;
    }
    let mut window = RECT::default();
    if GetWindowRect(frame, &mut window).is_ok() {
        let rect = RECT {
            left: 0,
            top: 0,
            right: (window.right - window.left).max(0),
            bottom: (window.bottom - window.top).max(0),
        };
        if let Some(geometry) =
            rounded_control_frame_geometry(rect.right, rect.bottom, GetDpiForWindow(edit).max(96))
        {
            let interior = palette.edit_brush_color_for(edit);
            fill(dc, &rect, interior);
            let hot = !GetPropW(edit, ROUNDED_CONTROL_HOT_PROPERTY).is_invalid()
                || !GetPropW(frame, ROUNDED_CONTROL_HOT_PROPERTY).is_invalid();
            let border = if !IsWindowEnabled(edit).as_bool() {
                palette.control_border()
            } else if GetFocus() == edit {
                palette.accent_border
            } else if hot {
                palette.separator
            } else {
                palette.control_border()
            };
            draw_antialiased_control_frame(
                dc,
                rect,
                geometry,
                interior,
                border,
                rounded_control_exterior(palette),
            );
        }
    }
    let _ = ReleaseDC(frame, dc);
}

unsafe fn forward_edit_frame_pointer_message(
    frame: HWND,
    edit: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if !IsWindowEnabled(edit).as_bool() {
        return LRESULT(0);
    }
    let packed = lparam.0 as u32;
    let mut point = POINT {
        x: (packed as u16 as i16) as i32,
        y: ((packed >> 16) as u16 as i16) as i32,
    };
    if !ClientToScreen(frame, &mut point).as_bool() || !ScreenToClient(edit, &mut point).as_bool() {
        return LRESULT(0);
    }
    let mut client = RECT::default();
    if GetClientRect(edit, &mut client).is_err() {
        return LRESULT(0);
    }
    point.x = point
        .x
        .clamp(client.left, (client.right - 1).max(client.left));
    point.y = point
        .y
        .clamp(client.top, (client.bottom - 1).max(client.top));
    let forwarded = (u32::from(point.x as u16)) | (u32::from(point.y as u16) << 16);
    SendMessageW(edit, message, wparam, LPARAM(forwarded as isize))
}

unsafe extern "system" fn single_line_edit_frame_subclass(
    frame: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    let owner = single_line_edit_frame_owner(frame);
    match message {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            if let Some(edit) = owner {
                paint_single_line_edit_frame(frame, edit, palette_from_reference(reference_data));
            } else {
                let mut paint = PAINTSTRUCT::default();
                let _ = BeginPaint(frame, &mut paint);
                let _ = EndPaint(frame, &paint);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            ensure_hot_tracking(frame, ROUNDED_CONTROL_HOT_PROPERTY, false);
            if let Some(edit) = owner {
                let result =
                    forward_edit_frame_pointer_message(frame, edit, message, wparam, lparam);
                repaint_single_line_edit_frame(edit, false);
                result
            } else {
                DefSubclassProc(frame, message, wparam, lparam)
            }
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP => owner.map_or_else(
            || DefSubclassProc(frame, message, wparam, lparam),
            |edit| forward_edit_frame_pointer_message(frame, edit, message, wparam, lparam),
        ),
        WM_MOUSELEAVE_MESSAGE => {
            clear_hot_tracking(frame, ROUNDED_CONTROL_HOT_PROPERTY);
            if let Some(edit) = owner {
                repaint_single_line_edit_frame(edit, false);
            }
            DefSubclassProc(frame, message, wparam, lparam)
        }
        WM_THEMECHANGED | WM_ENABLE => {
            let result = DefSubclassProc(frame, message, wparam, lparam);
            if let Some(edit) = owner {
                repaint_single_line_edit_frame(edit, true);
            }
            result
        }
        WM_NCDESTROY => {
            let _ = RemovePropW(frame, ROUNDED_CONTROL_HOT_PROPERTY);
            let _ = RemoveWindowSubclass(
                frame,
                Some(single_line_edit_frame_subclass),
                SINGLE_LINE_EDIT_FRAME_SUBCLASS_ID,
            );
            DefSubclassProc(frame, message, wparam, lparam)
        }
        _ => DefSubclassProc(frame, message, wparam, lparam),
    }
}

unsafe extern "system" fn single_line_edit_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    match message {
        WM_PAINT => {
            let palette = palette_from_reference(reference_data);
            if palette.uses_system_backdrop_surface() {
                paint_single_line_edit_client_atomic(hwnd, palette);
                LRESULT(0)
            } else {
                // Ordinary opaque windows must keep USER32's native Edit client metrics.  The
                // material-only WM_PRINTCLIENT buffer does not preserve the stock left inset and
                // vertical baseline while the user is editing, which makes focused text jump up
                // and touch the left frame even though the idle field is laid out correctly.
                DefSubclassProc(hwnd, message, wparam, lparam)
            }
        }
        WM_NCPAINT => DefSubclassProc(hwnd, message, wparam, lparam),
        WM_MOUSEMOVE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            ensure_hot_tracking(hwnd, ROUNDED_CONTROL_HOT_PROPERTY, false);
            repaint_single_line_edit_frame(hwnd, false);
            result
        }
        WM_MOUSELEAVE_MESSAGE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            clear_hot_tracking(hwnd, ROUNDED_CONTROL_HOT_PROPERTY);
            repaint_single_line_edit_frame(hwnd, false);
            result
        }
        message if edit_message_may_change_visible_text(message, wparam.0) => {
            // USER32 performs user-initiated Edit operations synchronously and can draw the new
            // glyphs directly before the next WM_PAINT. Let it update the native text/selection
            // state first, then synchronously replace the visible field with one complete BGRA
            // frame. Do not send WM_SETREDRAW(FALSE) here: Windows removes WS_VISIBLE while redraw
            // is disabled, which makes the immediate final paint miss this child altogether.
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            let palette = palette_from_reference(reference_data);
            if palette.uses_system_backdrop_surface() {
                let _ = RedrawWindow(
                    hwnd,
                    None,
                    None,
                    RDW_FRAME | RDW_INVALIDATE | RDW_NOERASE | RDW_UPDATENOW,
                );
            } else {
                // DefWindowProc already published the native glyphs, selection and caret inside
                // the centred child. The sibling frame is independent and needs no text repaint.
            }
            result
        }
        WM_ENABLE | WM_SETFOCUS | WM_KILLFOCUS | WM_SIZE | WM_THEMECHANGED => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            let palette = palette_from_reference(reference_data);
            if palette.uses_system_backdrop_surface() {
                let _ = RedrawWindow(
                    hwnd,
                    None,
                    None,
                    RDW_FRAME | RDW_INVALIDATE | RDW_NOERASE | RDW_UPDATENOW,
                );
            } else {
                repaint_single_line_edit_frame(hwnd, true);
            }
            if palette.uses_system_backdrop_surface() {
                repaint_single_line_edit_frame(hwnd, true);
            }
            result
        }
        WM_NCDESTROY => {
            let _ = RemovePropW(hwnd, ROUNDED_CONTROL_HOT_PROPERTY);
            let _ = RemoveWindowSubclass(
                hwnd,
                Some(single_line_edit_subclass),
                SINGLE_LINE_EDIT_SUBCLASS_ID,
            );
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

const fn edit_message_may_change_visible_text(message: u32, wparam: usize) -> bool {
    matches!(
        message,
        0x000c // WM_SETTEXT
            | 0x00c2 // EM_REPLACESEL
            | 0x0102 // WM_CHAR (including Backspace)
            | 0x0109 // WM_UNICHAR
            | 0x010f // WM_IME_COMPOSITION
            | 0x0300 // WM_CUT
            | 0x0302 // WM_PASTE
            | 0x0303 // WM_CLEAR
            | 0x0304 // WM_UNDO
    ) || (message == WM_KEYDOWN && wparam == 0x2e) // VK_DELETE
}

/// Publishes the complete Edit background and current native text buffer as one opaque BGRA frame.
/// USER32 continues to own input state, selection, caret, IME, hit testing and accessibility.
unsafe fn paint_single_line_edit_client_atomic(hwnd: HWND, palette: Palette) {
    const WM_PRINTCLIENT: u32 = 0x0318;
    const PRF_CLIENT: isize = 0x0000_0004;

    let mut paint = PAINTSTRUCT::default();
    let target_dc = BeginPaint(hwnd, &mut paint);
    if target_dc.is_invalid() {
        return;
    }
    let mut client = RECT::default();
    let client_valid = GetClientRect(hwnd, &mut client).is_ok()
        && client.right > client.left
        && client.bottom > client.top;
    if client_valid {
        let width = client.right - client.left;
        let height = client.bottom - client.top;
        let buffer_dc = CreateCompatibleDC(target_dc);
        let bitmap_info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: (width * height * 4) as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut::<core::ffi::c_void>();
        let bitmap = if buffer_dc.is_invalid() {
            None
        } else {
            CreateDIBSection(
                buffer_dc,
                &bitmap_info,
                DIB_RGB_COLORS,
                &mut bits,
                HANDLE::default(),
                0,
            )
            .ok()
        };
        if let Some(bitmap) = bitmap {
            let old_bitmap = SelectObject(buffer_dc, bitmap);
            let background = CreateSolidBrush(palette.edit);
            let _ = FillRect(buffer_dc, &client, background);
            let _ = DeleteObject(background);
            // Ask USER32 to draw the current visible Edit state into the same opaque surface. This
            // preserves horizontal scrolling, selection, caret-related text positioning and IME
            // composition; manually drawing GetWindowText from x=0 duplicates or shifts the text
            // as soon as the native Edit scrolls while the user is typing.
            let _ = DefSubclassProc(
                hwnd,
                WM_PRINTCLIENT,
                WPARAM(buffer_dc.0 as usize),
                LPARAM(PRF_CLIENT),
            );
            let _ = GdiFlush();
            // GDI leaves the alpha byte undefined/zero. On an extended DWM frame that turns the
            // correctly black light-theme glyphs into transparent holes. The complete Edit client
            // is an opaque resolved material surface, so repair every pixel before publishing it.
            for pixel in std::slice::from_raw_parts_mut(
                bits.cast::<u8>(),
                width as usize * height as usize * 4,
            )
            .chunks_exact_mut(4)
            {
                pixel[3] = 255;
            }
            let _ = SetDIBitsToDevice(
                target_dc,
                client.left,
                client.top,
                width as u32,
                height as u32,
                0,
                0,
                0,
                height as u32,
                bits.cast_const(),
                &bitmap_info,
                DIB_RGB_COLORS,
            );
            let _ = SelectObject(buffer_dc, old_bitmap);
        } else {
            let _ = DefSubclassProc(
                hwnd,
                WM_PRINTCLIENT,
                WPARAM(target_dc.0 as usize),
                LPARAM(PRF_CLIENT),
            );
        }
        if let Some(bitmap) = bitmap {
            let _ = DeleteObject(bitmap);
        }
        if !buffer_dc.is_invalid() {
            let _ = DeleteDC(buffer_dc);
        }
    }
    let _ = EndPaint(hwnd, &paint);
}

unsafe extern "system" fn combo_selection_item_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    const WM_PRINTCLIENT: u32 = 0x0318;
    let palette = palette_from_reference(reference_data);
    match message {
        WM_ERASEBKGND | WM_PRINTCLIENT => {
            let dc = HDC(wparam.0 as *mut _);
            if !dc.is_invalid() {
                paint_combo_selection_item_to_dc(hwnd, palette, dc);
            }
            LRESULT(1)
        }
        WM_PAINT => {
            // Always validate the child update region. Returning without BeginPaint leaves a
            // permanent WM_PAINT loop on reduced WinPE USER32 builds.
            let mut paint = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut paint);
            let _ = EndPaint(hwnd, &paint);
            paint_combo_selection_item_window(hwnd, palette);
            LRESULT(0)
        }
        WM_NCPAINT => {
            paint_combo_selection_item_window(hwnd, palette);
            LRESULT(0)
        }
        WM_ENABLE
        | WM_SETTEXT
        | WM_SETFOCUS
        | WM_KILLFOCUS
        | WM_THEMECHANGED
        | WM_LBUTTONDOWN
        | WM_LBUTTONUP
        | WM_KEYDOWN
        | WM_KEYUP
        | 0x0127 // WM_CHANGEUISTATE
        | 0x0128 // WM_UPDATEUISTATE
        => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            // USER32 paints a focus underline directly while processing focus/click/UI-state
            // messages on the read-only selection child. An asynchronous invalidation leaves that
            // underline visible until another pointer event, especially in reduced WinPE builds.
            // Publish the complete child surface and its parent frame in the same transaction.
            repaint_combo_selection_item_now(hwnd, palette);
            result
        }
        WM_NCDESTROY => {
            let _ = RemovePropW(hwnd, COMBO_SELECTION_ITEM_PREPARED_PROPERTY);
            let _ = RemoveWindowSubclass(
                hwnd,
                Some(combo_selection_item_subclass),
                COMBO_SELECTION_ITEM_SUBCLASS_ID,
            );
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

unsafe extern "system" fn rounded_control_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    const CB_SHOWDROPDOWN: u32 = 0x014f;
    match message {
        WM_REPAINT_TRACKING_COMBO if is_drop_down_list(hwnd) => {
            // A real mouse click enters USER32's nested drop-list tracking loop. The stock theme
            // can repaint the closed selection after WM_LBUTTONDOWN but before that call returns,
            // so a conventional after-default overlay runs too late. This posted message executes
            // inside the nested loop and restores the complete deterministic surface while the
            // popup is actually visible.
            repaint_combo_closed_now(hwnd, palette_from_reference(reference_data));
            LRESULT(0)
        }
        WM_ERASEBKGND if is_drop_down_list(hwnd) => {
            // WinPE's reduced USER32/UxTheme stack can still erase the borderless ComboBox with
            // the stock class brush before WM_PAINT. That erase survives as a bright underline
            // because the PE renderer does not agree with the closed-field height reported by
            // COMBOBOXINFO. Paint the deterministic closed surface into the supplied erase DC and
            // report the erase as complete. The separate ComboLBox popup remains native.
            let dc = HDC(wparam.0 as *mut _);
            if !dc.is_invalid() {
                paint_combo_closed_to_dc(hwnd, palette_from_reference(reference_data), dc);
            }
            paint_rounded_control_frame(hwnd, palette_from_reference(reference_data));
            LRESULT(1)
        }
        WM_PAINT => {
            if is_drop_down_list(hwnd) {
                paint_combo_closed(hwnd, palette_from_reference(reference_data));
                return LRESULT(0);
            }
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if is_list_box(hwnd) {
                paint_list_box_rows(hwnd, palette_from_reference(reference_data));
            }
            paint_rounded_control_frame(hwnd, palette_from_reference(reference_data));
            result
        }
        WM_NCPAINT => {
            if is_drop_down_list(hwnd) {
                // WS_BORDER and WS_EX_CLIENTEDGE are removed when this subclass is installed.
                // Calling the stock non-client painter anyway is harmless on full Windows, but
                // WinPE paints a legacy bright bottom edge after our client surface. This frame
                // is the complete non-client result, so do not run that incompatible renderer.
                paint_rounded_control_frame(hwnd, palette_from_reference(reference_data));
                return LRESULT(0);
            }
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            paint_rounded_control_frame(hwnd, palette_from_reference(reference_data));
            result
        }
        WM_SETCURSOR if is_drop_down_list(hwnd) => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            ensure_hot_tracking(hwnd, ROUNDED_CONTROL_HOT_PROPERTY, false);
            result
        }
        WM_MOUSEMOVE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if is_list_box(hwnd) {
                update_list_box_hot_item(hwnd, lparam);
            }
            ensure_hot_tracking(hwnd, ROUNDED_CONTROL_HOT_PROPERTY, false);
            if !is_drop_down_list(hwnd) {
                paint_rounded_control_frame(hwnd, palette_from_reference(reference_data));
            }
            result
        }
        WM_NCMOUSEMOVE_MESSAGE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            ensure_hot_tracking(hwnd, ROUNDED_CONTROL_HOT_PROPERTY, true);
            paint_rounded_control_frame(hwnd, palette_from_reference(reference_data));
            result
        }
        WM_MOUSELEAVE_MESSAGE | WM_NCMOUSELEAVE_MESSAGE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if is_list_box(hwnd) {
                clear_list_box_hot_item(hwnd);
            }
            clear_hot_tracking(hwnd, ROUNDED_CONTROL_HOT_PROPERTY);
            if !is_drop_down_list(hwnd) {
                paint_rounded_control_frame(hwnd, palette_from_reference(reference_data));
            }
            result
        }
        message if native_scrollbar_may_repaint_frame(message) => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if is_drop_down_list(hwnd) {
                // A closed ComboBox has no scrollbar of its own; its popup ComboLBox is a
                // separate HWND.  UxTheme nevertheless posts WM_TIMER while the pointer crosses
                // adjacent controls.  Synchronously repainting the complete field for every
                // timer tick produces the visible WinPE flash.  Ignore animation-only timers and
                // coalesce the remaining state/content changes into the normal paint queue.
                if message != 0x0113 {
                    invalidate_control_visual(hwnd);
                }
            } else {
                paint_rounded_control_frame(hwnd, palette_from_reference(reference_data));
            }
            result
        }
        WM_SETFOCUS if is_drop_down_list(hwnd) => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            install_combo_selection_item_subclass(hwnd, palette_from_reference(reference_data));
            // USER32 creates and shows a caret when a CBS_DROPDOWNLIST receives focus. The closed
            // field is read-only and fully painted by this subclass, so that caret has no editing
            // meaning and otherwise appears as a one-frame vertical line after a click.
            if GetPropW(hwnd, COMBO_CARET_HIDDEN_PROPERTY).is_invalid()
                && HideCaret(hwnd).is_ok()
                && SetPropW(
                    hwnd,
                    COMBO_CARET_HIDDEN_PROPERTY,
                    HANDLE(std::ptr::dangling_mut()),
                )
                .is_err()
            {
                let _ = ShowCaret(hwnd);
            }
            repaint_combo_closed_now(hwnd, palette_from_reference(reference_data));
            result
        }
        WM_KILLFOCUS if is_drop_down_list(hwnd) => {
            if RemovePropW(hwnd, COMBO_CARET_HIDDEN_PROPERTY)
                .is_ok_and(|handle| !handle.is_invalid())
            {
                let _ = ShowCaret(hwnd);
            }
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            repaint_combo_closed_now(hwnd, palette_from_reference(reference_data));
            result
        }
        WM_LBUTTONDOWN if is_drop_down_list(hwnd) => {
            let marked = SetPropW(
                hwnd,
                COMBO_TRACKING_DROPPED_PROPERTY,
                HANDLE(std::ptr::dangling_mut()),
            )
            .is_ok();
            repaint_combo_closed_now(hwnd, palette_from_reference(reference_data));
            let _ = PostMessageW(hwnd, WM_REPAINT_TRACKING_COMBO, WPARAM(0), LPARAM(0));
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if marked {
                let _ = RemovePropW(hwnd, COMBO_TRACKING_DROPPED_PROPERTY);
            }
            repaint_combo_closed_now(hwnd, palette_from_reference(reference_data));
            result
        }
        WM_ENABLE | WM_SETFOCUS | WM_KILLFOCUS | WM_SIZE | WM_THEMECHANGED | WM_LBUTTONUP => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if is_drop_down_list(hwnd) {
                if matches!(message, WM_SIZE | WM_THEMECHANGED) {
                    let height = InnoMetrics::for_dpi(GetDpiForWindow(hwnd).max(96)).field_height;
                    set_combo_selection_field_height(hwnd, height);
                    set_combo_popup_row_height(hwnd, height);
                    clip_combo_to_closed_field(hwnd, palette_from_reference(reference_data));
                }
                install_combo_selection_item_subclass(hwnd, palette_from_reference(reference_data));
                if matches!(message, WM_LBUTTONDOWN | WM_LBUTTONUP | WM_SETFOCUS) {
                    repaint_combo_closed_now(hwnd, palette_from_reference(reference_data));
                } else {
                    invalidate_control_visual(hwnd);
                }
            } else {
                if matches!(message, WM_SIZE | WM_THEMECHANGED) {
                    apply_material_rounded_control_region(
                        hwnd,
                        palette_from_reference(reference_data),
                    );
                }
                let _ = InvalidateRect(hwnd, None, false);
            }
            result
        }
        CB_SHOWDROPDOWN => {
            if wparam.0 != 0 {
                let height = InnoMetrics::for_dpi(GetDpiForWindow(hwnd).max(96)).field_height;
                set_combo_selection_field_height(hwnd, height);
                set_combo_popup_row_height(hwnd, height);
                clip_combo_to_closed_field(hwnd, palette_from_reference(reference_data));
            }
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            install_combo_selection_item_subclass(hwnd, palette_from_reference(reference_data));
            let mut info = COMBOBOXINFO {
                cbSize: std::mem::size_of::<COMBOBOXINFO>() as u32,
                ..Default::default()
            };
            if wparam.0 != 0
                && GetComboBoxInfo(hwnd, &mut info).is_ok()
                && !info.hwndList.0.is_null()
            {
                apply_combo_popup_native_chrome(
                    info.hwndList,
                    palette_from_reference(reference_data),
                );
            }
            repaint_combo_closed_now(hwnd, palette_from_reference(reference_data));
            result
        }
        WM_NCDESTROY => {
            if RemovePropW(hwnd, COMBO_CARET_HIDDEN_PROPERTY)
                .is_ok_and(|handle| !handle.is_invalid())
            {
                let _ = ShowCaret(hwnd);
            }
            let _ = RemovePropW(hwnd, LIST_BOX_HOT_PROPERTY);
            let _ = RemovePropW(hwnd, ROUNDED_CONTROL_HOT_PROPERTY);
            let _ = RemovePropW(hwnd, COMBO_TRACKING_DROPPED_PROPERTY);
            let _ = RemoveWindowSubclass(
                hwnd,
                Some(rounded_control_subclass),
                ROUNDED_CONTROL_SUBCLASS_ID,
            );
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

unsafe fn paint_rounded_control_frame(hwnd: HWND, palette: Palette) {
    let dc = GetWindowDC(hwnd);
    if dc.0.is_null() {
        return;
    }
    let class_name = control_class_name(hwnd);
    let interior = if is_combo_class(&class_name) {
        // The antialiased inner edge must blend against the same stateful surface as the closed
        // selection field. Using the normal button colour here leaves a white/grey crescent when
        // the field has already switched to its hot or dropped colour.
        combo_closed_surface(hwnd, palette)
    } else {
        palette.edit
    };
    if is_edit_class(&class_name) && is_single_line_edit(hwnd) {
        paint_single_line_edit_nonclient_bands(dc, hwnd, palette.edit_brush_color_for(hwnd));
    }
    draw_rounded_control_frame_to_dc(dc, hwnd, palette, interior);
    let _ = ReleaseDC(hwnd, dc);
}

/// Covers USER32's stock single-border band before the deterministic rounded frame is published.
/// The client rectangle remains untouched, so USER32 keeps its native formatting rectangle and
/// continues to own text, selection, caret, horizontal scrolling and IME.
unsafe fn paint_single_line_edit_nonclient_bands(dc: HDC, hwnd: HWND, interior: COLORREF) {
    let mut window = RECT::default();
    if GetWindowRect(hwnd, &mut window).is_err() {
        return;
    }
    let width = (window.right - window.left).max(0);
    let height = (window.bottom - window.top).max(0);
    if rounded_control_frame_geometry(width, height, GetDpiForWindow(hwnd).max(96)).is_none() {
        return;
    }
    let mut client = RECT::default();
    if GetClientRect(hwnd, &mut client).is_err() {
        return;
    }
    let mut client_top_left = POINT {
        x: client.left,
        y: client.top,
    };
    let mut client_bottom_right = POINT {
        x: client.right,
        y: client.bottom,
    };
    if !ClientToScreen(hwnd, &mut client_top_left).as_bool()
        || !ClientToScreen(hwnd, &mut client_bottom_right).as_bool()
    {
        return;
    }
    let client_left = (client_top_left.x - window.left).clamp(0, width);
    let client_top = (client_top_left.y - window.top).clamp(0, height);
    let client_right = (client_bottom_right.x - window.left).clamp(client_left, width);
    let client_bottom = (client_bottom_right.y - window.top).clamp(client_top, height);
    let brush = CreateSolidBrush(interior);
    if brush.0.is_null() {
        return;
    }
    for rect in [
        RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: client_top,
        },
        RECT {
            left: 0,
            top: client_bottom,
            right: width,
            bottom: height,
        },
        RECT {
            left: 0,
            top: client_top,
            right: client_left,
            bottom: client_bottom,
        },
        RECT {
            left: client_right,
            top: client_top,
            right: width,
            bottom: client_bottom,
        },
    ] {
        let _ = FillRect(dc, &rect, brush);
    }
    let _ = DeleteObject(brush);
}

unsafe fn is_drop_down_list(hwnd: HWND) -> bool {
    const COMBO_TYPE_MASK: isize = 0x0003;
    const CBS_DROPDOWNLIST_VALUE: isize = 0x0003;
    is_combo_class(&control_class_name(hwnd))
        && GetWindowLongPtrW(hwnd, GWL_STYLE) & COMBO_TYPE_MASK == CBS_DROPDOWNLIST_VALUE
}

/// Sets the closed selection field to the shared Inno control baseline.  This is deliberately
/// independent from the native popup row height: Microsoft documents the selection field as
/// component 1 for CB_SETITEMHEIGHT, while popup items remain component 0.
unsafe fn set_combo_selection_field_height(hwnd: HWND, height: i32) {
    const CB_SETITEMHEIGHT: u32 = 0x0153;
    const SELECTION_FIELD: usize = 1;
    const CB_ERR: isize = -1;

    if height <= 0 {
        return;
    }
    let result = SendMessageW(
        hwnd,
        CB_SETITEMHEIGHT,
        WPARAM(SELECTION_FIELD),
        LPARAM(height as isize),
    );
    if result.0 != CB_ERR {
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// Fixes stock popup rows to the same DPI-scaled baseline on both the initial theme transaction
/// and later theme changes. USER32 otherwise keeps the pre-font row height until the first theme
/// switch, which makes the initial dark popup visibly shorter than the same popup afterwards.
unsafe fn set_combo_popup_row_height(hwnd: HWND, height: i32) {
    const CB_SETITEMHEIGHT: u32 = 0x0153;
    const POPUP_ROWS: usize = 0;
    if height > 0 {
        let _ = SendMessageW(
            hwnd,
            CB_SETITEMHEIGHT,
            WPARAM(POPUP_ROWS),
            LPARAM(height as isize),
        );
    }
}

/// Restricts the visible/hit-test region of a closed drop-down ComboBox to its selection field.
///
/// The height passed to `MoveWindow` is the fully expanded list height. Full Windows clips the
/// closed control internally, but reduced WinPE USER32 builds can expose pixels from that retained
/// height below the field. The popup list is a separate top-level ComboLBox, so clipping this HWND
/// does not change native popup, keyboard or accessibility behaviour.
unsafe fn clip_combo_to_closed_field(hwnd: HWND, _palette: Palette) {
    if !is_drop_down_list(hwnd) {
        return;
    }
    let mut window = RECT::default();
    if GetWindowRect(hwnd, &mut window).is_err() {
        return;
    }
    let width = (window.right - window.left).max(0);
    let full_height = (window.bottom - window.top).max(0);
    if width == 0 || full_height == 0 {
        return;
    }
    let dpi = GetDpiForWindow(hwnd).max(96);
    let closed_height =
        combo_closed_height(hwnd, InnoMetrics::for_dpi(dpi).field_height).clamp(1, full_height);
    // Keep this region rectangular. CreateRoundRectRgn is a binary pixel mask; using it as the
    // visible silhouette clips away partially covered pixels and leaves a stair-stepped bracket.
    // The painter masks the four corners; this region only hides USER32's retained list height.
    let region = CreateRectRgn(0, 0, width, closed_height);
    // Microsoft recommends redrawing a visible window when changing its region. Without that
    // redraw, the pixels removed from USER32's retained drop-list height survive as a narrow line
    // immediately below the deterministic field until some unrelated parent repaint occurs.
    if !region.is_invalid() {
        if SetWindowRgn(hwnd, region, true) == 0 {
            let _ = DeleteObject(region);
        } else if full_height > closed_height {
            // The removed tail belonged to the child before SetWindowRgn, but becomes parent
            // surface afterwards. SetWindowRgn redraws the child only; explicitly invalidate that
            // parent strip or its last stock UxTheme row can survive as the reported bottom line.
            let parent = GetParent(hwnd).unwrap_or_default();
            let mut parent_origin = POINT::default();
            if !parent.is_invalid() && ClientToScreen(parent, &mut parent_origin).as_bool() {
                let exposed_tail = RECT {
                    left: window.left - parent_origin.x,
                    top: window.top - parent_origin.y + closed_height,
                    right: window.right - parent_origin.x,
                    bottom: window.bottom - parent_origin.y,
                };
                let _ = RedrawWindow(
                    parent,
                    Some(&exposed_tail),
                    None,
                    RDW_INVALIDATE | RDW_ERASE | RDW_FRAME,
                );
            }
        }
    }
}

/// Removes any earlier binary rounded region from a stock child HWND.
///
/// A GDI region has no partial coverage and cannot represent the visible antialiased edge. The
/// deterministic last-paint transaction replaces the small exterior corner pixels instead.
unsafe fn apply_material_rounded_control_region(hwnd: HWND, _palette: Palette) {
    clear_control_window_region(hwnd);
}

unsafe fn disable_edit_layered_redirection(hwnd: HWND) {
    let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
    let desired = edit_ex_style_without_layering(current);
    if desired != current {
        let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, desired);
    }
}

const fn edit_ex_style_without_layering(ex_style: isize) -> isize {
    ex_style & !(WS_EX_LAYERED.0 as isize)
}

unsafe fn clear_control_window_region(hwnd: HWND) {
    let _ = SetWindowRgn(hwnd, windows::Win32::Graphics::Gdi::HRGN::default(), true);
}

/// Returns the height of the visible, closed selection field of a stock drop-down list.
///
/// `CB_SETITEMHEIGHT(1)` establishes the selection field independently from popup rows. Some
/// USER32 builds nevertheless report `COMBOBOXINFO` item/button rectangles a few pixels taller
/// than the requested field because they include stock non-client padding. Our borderless custom
/// painter owns that padding, so using the reported height would expose a second bottom strip. The
/// shared Inno field baseline is therefore the authoritative visible and layout height.
pub(crate) unsafe fn combo_closed_height(hwnd: HWND, fallback: i32) -> i32 {
    combo_closed_visual_height(fallback, GetDpiForWindow(hwnd).max(96))
}

fn combo_closed_visual_height(requested: i32, dpi: u32) -> i32 {
    requested.max(1).clamp(scale(18, dpi), scale(36, dpi))
}

/// Paints the complete closed CBS_DROPDOWNLIST in one WM_PAINT transaction. USER32 continues to own
/// hit testing, selection, keyboard navigation, accessibility and the separate native popup. The
/// closed HWND never calls its default WM_PAINT renderer, so a Windows 10/11 UxTheme animation cannot
/// briefly compose a rectangular frame underneath the rounded Inno field.
unsafe fn paint_combo_closed(hwnd: HWND, palette: Palette) {
    let mut paint = PAINTSTRUCT::default();
    let _ = BeginPaint(hwnd, &mut paint);
    let _ = EndPaint(hwnd, &paint);

    // BeginPaint clips its HDC to the current update region. Hover/focus changes can invalidate only
    // the selection or arrow half, so painting the complete compatibility surface through that HDC
    // leaves the other half stale and can omit part of an edge. Validate the update above, then
    // publish the whole closed client through an unclipped client DC and its frame through an
    // unclipped window DC.
    // A stock ComboBox can retain a themed two-pixel client inset even after WS_BORDER and
    // WS_EX_CLIENTEDGE are removed. GetDC starts inside that inset, leaving USER32's pale top arc
    // untouched. Paint the complete closed window surface through the window DC, then publish our
    // single deterministic frame last.
    let dc = GetWindowDC(hwnd);
    if !dc.is_invalid() {
        paint_combo_closed_window_to_dc(hwnd, palette, dc);
        let _ = ReleaseDC(hwnd, dc);
    }
    paint_rounded_control_frame(hwnd, palette);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ComboClosedState {
    Normal,
    Hot,
    Dropped,
}

unsafe fn combo_closed_state(hwnd: HWND) -> ComboClosedState {
    const CB_GETDROPPEDSTATE: u32 = 0x0157;
    const STATE_SYSTEM_PRESSED: u32 = 0x0000_0008;
    let mut info = COMBOBOXINFO {
        cbSize: std::mem::size_of::<COMBOBOXINFO>() as u32,
        ..Default::default()
    };
    let _ = GetComboBoxInfo(hwnd, &mut info);
    let pointer_inside = {
        let mut pointer = POINT::default();
        let mut window = RECT::default();
        GetCursorPos(&mut pointer).is_ok()
            && GetWindowRect(hwnd, &mut window).is_ok()
            && pointer.x >= window.left
            && pointer.x < window.right
            && pointer.y >= window.top
            && pointer.y < window.bottom
    };
    let hot = pointer_inside || !GetPropW(hwnd, ROUNDED_CONTROL_HOT_PROPERTY).is_invalid();
    let dropped = !GetPropW(hwnd, COMBO_TRACKING_DROPPED_PROPERTY).is_invalid()
        || info.stateButton.0 & STATE_SYSTEM_PRESSED != 0
        || SendMessageW(hwnd, CB_GETDROPPEDSTATE, WPARAM(0), LPARAM(0)).0 != 0;
    if dropped {
        ComboClosedState::Dropped
    } else if hot && IsWindowEnabled(hwnd).as_bool() {
        ComboClosedState::Hot
    } else {
        ComboClosedState::Normal
    }
}

unsafe fn combo_closed_surface(hwnd: HWND, palette: Palette) -> COLORREF {
    match combo_closed_state(hwnd) {
        ComboClosedState::Dropped => {
            if palette.dark {
                palette.button_pressed
            } else {
                rgb(204, 228, 247)
            }
        }
        ComboClosedState::Hot => {
            if palette.dark {
                palette.button_hot
            } else {
                rgb(229, 241, 251)
            }
        }
        ComboClosedState::Normal => palette.button,
    }
}

unsafe fn draw_combo_selected_text(hwnd: HWND, dc: HDC, mut text_rect: RECT, palette: Palette) {
    const CB_GETCURSEL: u32 = 0x0147;
    const CB_GETLBTEXT: u32 = 0x0148;
    const CB_GETLBTEXTLEN: u32 = 0x0149;
    const CB_ERR: isize = -1;
    let selected = SendMessageW(hwnd, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
    if selected == CB_ERR {
        return;
    }
    let length = SendMessageW(hwnd, CB_GETLBTEXTLEN, WPARAM(selected as usize), LPARAM(0)).0;
    if length < 0 {
        return;
    }
    let mut text = vec![0u16; length as usize + 1];
    let copied = SendMessageW(
        hwnd,
        CB_GETLBTEXT,
        WPARAM(selected as usize),
        LPARAM(text.as_mut_ptr() as isize),
    )
    .0
    .max(0) as usize;
    text.truncate(copied.min(text.len()));
    let font = SendMessageW(hwnd, WM_GETFONT, WPARAM(0), LPARAM(0));
    let old_font = (font.0 != 0)
        .then(|| SelectObject(dc, windows::Win32::Graphics::Gdi::HGDIOBJ(font.0 as *mut _)));
    let _ = SetBkMode(dc, TRANSPARENT);
    let _ = SetTextColor(
        dc,
        if IsWindowEnabled(hwnd).as_bool() {
            palette.text
        } else {
            palette.text_disabled
        },
    );
    let dpi = GetDpiForWindow(hwnd).max(96);
    text_rect.left += scale(6, dpi);
    text_rect.right -= scale(3, dpi);
    let mut text_metrics = windows::Win32::Graphics::Gdi::TEXTMETRICW::default();
    let measured = GetTextMetricsW(dc, &mut text_metrics).as_bool();
    if measured {
        let available = (text_rect.bottom - text_rect.top).max(0);
        let text_height = text_metrics.tmHeight.clamp(1, available.max(1));
        let spare = available.saturating_sub(text_height);
        text_rect.top += (spare + 1) / 2;
        text_rect.bottom = text_rect.top + text_height;
    }
    let flags = if measured {
        DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX
    } else {
        DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX
    };
    draw_alpha_composited_text(
        hwnd,
        dc,
        &text,
        &mut text_rect,
        flags,
        if IsWindowEnabled(hwnd).as_bool() {
            palette.text
        } else {
            palette.text_disabled
        },
        palette.uses_system_backdrop_surface() && !palette.dark,
    );
    if let Some(old_font) = old_font {
        let _ = SelectObject(dc, old_font);
    }
}

unsafe fn paint_combo_selection_item_to_dc(item: HWND, palette: Palette, dc: HDC) {
    let Ok(combo) = GetParent(item) else {
        return;
    };
    if combo.0.is_null() || !is_drop_down_list(combo) {
        return;
    }
    let mut client = RECT::default();
    if GetClientRect(item, &mut client).is_err() || client.right <= client.left {
        return;
    }
    fill(dc, &client, combo_closed_surface(combo, palette));
    draw_combo_selected_text(combo, dc, client, palette);
}

unsafe fn repaint_combo_selection_item_now(item: HWND, palette: Palette) {
    paint_combo_selection_item_window(item, palette);
    if let Ok(combo) = GetParent(item) {
        if !combo.0.is_null() && is_drop_down_list(combo) {
            repaint_combo_closed_now(combo, palette);
        }
    }
}

/// Paints both the client and non-client pixels of USER32's closed selection child.
///
/// Reduced WinPE USER32 builds keep a focus underline in the child's non-client bottom band.
/// Painting only `GetDC(item)` therefore leaves a blue line while focused and a dark line after
/// focus moves away. A window DC lets the deterministic closed-field surface replace that band in
/// the same synchronous transaction as the parent ComboBox frame.
unsafe fn paint_combo_selection_item_window(item: HWND, palette: Palette) {
    let Ok(combo) = GetParent(item) else {
        return;
    };
    if combo.0.is_null() || !is_drop_down_list(combo) {
        return;
    }

    let mut window = RECT::default();
    if GetWindowRect(item, &mut window).is_err() {
        return;
    }
    let width = window.right - window.left;
    let height = window.bottom - window.top;
    if width <= 0 || height <= 0 {
        return;
    }

    let dc = GetWindowDC(item);
    if dc.is_invalid() {
        return;
    }
    let bounds = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };
    fill(dc, &bounds, combo_closed_surface(combo, palette));
    draw_combo_selected_text(combo, dc, bounds, palette);
    let _ = ReleaseDC(item, dc);
}

unsafe fn repaint_combo_closed_now(combo: HWND, palette: Palette) {
    let dc = GetWindowDC(combo);
    if !dc.is_invalid() {
        paint_combo_closed_window_to_dc(combo, palette, dc);
        let _ = ReleaseDC(combo, dc);
    }
    paint_rounded_control_frame(combo, palette);
}

unsafe fn paint_combo_closed_to_dc(hwnd: HWND, palette: Palette, dc: HDC) {
    let dpi = GetDpiForWindow(hwnd).max(96);
    let mut client = RECT::default();
    if GetClientRect(hwnd, &mut client).is_err() {
        return;
    }
    let available_height = (client.bottom - client.top).max(1);
    client.bottom = client.top
        + combo_closed_height(hwnd, InnoMetrics::for_dpi(dpi).field_height).min(available_height);
    paint_combo_closed_bounds_to_dc(hwnd, palette, dc, client);
}

unsafe fn paint_combo_closed_window_to_dc(hwnd: HWND, palette: Palette, dc: HDC) {
    let dpi = GetDpiForWindow(hwnd).max(96);
    let mut window = RECT::default();
    if GetWindowRect(hwnd, &mut window).is_err() {
        return;
    }
    let width = (window.right - window.left).max(1);
    let available_height = (window.bottom - window.top).max(1);
    let bounds = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: combo_closed_height(hwnd, InnoMetrics::for_dpi(dpi).field_height)
            .min(available_height),
    };
    paint_combo_closed_bounds_to_dc(hwnd, palette, dc, bounds);
}

unsafe fn paint_combo_closed_bounds_to_dc(hwnd: HWND, palette: Palette, dc: HDC, client: RECT) {
    let mut info = COMBOBOXINFO {
        cbSize: std::mem::size_of::<COMBOBOXINFO>() as u32,
        ..Default::default()
    };
    if GetComboBoxInfo(hwnd, &mut info).is_err() {
        return;
    }
    let dpi = GetDpiForWindow(hwnd).max(96);
    // The native arrow width is stable, but COMBOBOXINFO rectangle origins differ across the
    // USER32 implementations we support. Rebuild client-local rectangles from the actual HWND
    // so the compatibility surface always covers the complete closed control.
    let arrow_width = (info.rcButton.right - info.rcButton.left)
        .max(scale(17, dpi))
        .min((client.right - client.left).max(0) / 2);
    let button = RECT {
        left: client.right - arrow_width,
        top: client.top + scale(1, dpi),
        right: client.right - scale(1, dpi),
        bottom: client.bottom - scale(1, dpi),
    };
    let field = RECT {
        left: client.left + scale(1, dpi),
        top: client.top + scale(1, dpi),
        right: button.left,
        bottom: client.bottom - scale(1, dpi),
    };
    if field.right <= field.left || field.bottom <= field.top {
        return;
    }

    // Use one surface decision for the parent, its selection child and the native-arrow-sized
    // compatibility glyph. This prevents WinPE from showing a differently coloured seam.
    let surface = combo_closed_surface(hwnd, palette);
    fill(dc, &client, surface);
    fill(dc, &field, surface);
    fill(dc, &button, surface);

    // Keep the popup itself native. Only the closed chevron is repainted so its hot/pressed
    // feedback changes in the same frame as the selection field, without UxTheme transition lag.
    let glyph_width = scale(8, dpi).max(7);
    let glyph_height = scale(5, dpi).max(5);
    let glyph_x = (button.left + button.right - glyph_width) / 2;
    let glyph_y = (button.top + button.bottom - glyph_height) / 2;
    let glyph = combo_chevron_pixels(
        glyph_width,
        glyph_height,
        if IsWindowEnabled(hwnd).as_bool() {
            palette.text_secondary
        } else {
            palette.text_disabled
        },
    );
    let _ = alpha_blend_premultiplied_bgra(dc, glyph_x, glyph_y, glyph_width, glyph_height, &glyph);

    draw_combo_selected_text(hwnd, dc, field, palette);
}

fn combo_chevron_pixels(width: i32, height: i32, color: COLORREF) -> Vec<u8> {
    const SAMPLES: i32 = 4;
    let width = width.max(1);
    let height = height.max(1);
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    let left = (0.75f64, 0.75f64);
    let middle = (f64::from(width - 1) / 2.0, f64::from(height - 1) - 0.5);
    let right = (f64::from(width - 1) - 0.75, 0.75f64);
    let radius = 0.72f64;
    for y in 0..height {
        for x in 0..width {
            let mut covered = 0u32;
            for sample_y in 0..SAMPLES {
                for sample_x in 0..SAMPLES {
                    let px = f64::from(x) + (f64::from(sample_x) + 0.5) / f64::from(SAMPLES);
                    let py = f64::from(y) + (f64::from(sample_y) + 0.5) / f64::from(SAMPLES);
                    if point_segment_distance(px, py, left, middle) <= radius
                        || point_segment_distance(px, py, middle, right) <= radius
                    {
                        covered += 1;
                    }
                }
            }
            let alpha = ((covered * 255 + (SAMPLES * SAMPLES / 2) as u32)
                / (SAMPLES * SAMPLES) as u32) as u8;
            let index = (y as usize * width as usize + x as usize) * 4;
            pixels[index] = premultiply_channel(((color.0 >> 16) & 0xff) as u8, alpha);
            pixels[index + 1] = premultiply_channel(((color.0 >> 8) & 0xff) as u8, alpha);
            pixels[index + 2] = premultiply_channel((color.0 & 0xff) as u8, alpha);
            pixels[index + 3] = alpha;
        }
    }
    pixels
}

fn point_segment_distance(x: f64, y: f64, start: (f64, f64), end: (f64, f64)) -> f64 {
    let dx = end.0 - start.0;
    let dy = end.1 - start.1;
    let length_squared = dx * dx + dy * dy;
    let projection = if length_squared <= f64::EPSILON {
        0.0
    } else {
        (((x - start.0) * dx + (y - start.1) * dy) / length_squared).clamp(0.0, 1.0)
    };
    let nearest_x = start.0 + projection * dx;
    let nearest_y = start.1 + projection * dy;
    (x - nearest_x).hypot(y - nearest_y)
}

const fn premultiply_channel(channel: u8, alpha: u8) -> u8 {
    ((channel as u16 * alpha as u16 + 127) / 255) as u8
}

unsafe fn draw_rounded_control_frame_to_dc(
    dc: HDC,
    hwnd: HWND,
    palette: Palette,
    interior: COLORREF,
) {
    let mut window = RECT::default();
    if GetWindowRect(hwnd, &mut window).is_err() {
        return;
    }
    let class_name = control_class_name(hwnd);
    let full_height = (window.bottom - window.top).max(0);
    let visible_height = if is_combo_class(&class_name) && is_drop_down_list(hwnd) {
        combo_closed_height(
            hwnd,
            InnoMetrics::for_dpi(GetDpiForWindow(hwnd).max(96)).field_height,
        )
        .min(full_height)
    } else {
        full_height
    };
    let rect = RECT {
        left: 0,
        top: 0,
        right: (window.right - window.left).max(0),
        bottom: visible_height,
    };
    let Some(geometry) =
        rounded_control_frame_geometry(rect.right, rect.bottom, GetDpiForWindow(hwnd).max(96))
    else {
        return;
    };
    let interactive_field =
        is_combo_class(&class_name) || (is_edit_class(&class_name) && is_single_line_edit(hwnd));
    let hot = !GetPropW(hwnd, ROUNDED_CONTROL_HOT_PROPERTY).is_invalid();
    let focus = GetFocus();
    let focused = focus == hwnd
        || if is_combo_class(&class_name) && is_drop_down_list(hwnd) {
            let mut info = COMBOBOXINFO {
                cbSize: std::mem::size_of::<COMBOBOXINFO>() as u32,
                ..Default::default()
            };
            GetComboBoxInfo(hwnd, &mut info).is_ok()
                && !info.hwndItem.0.is_null()
                && focus == info.hwndItem
        } else {
            false
        };
    let combo_active = is_combo_class(&class_name)
        && matches!(
            combo_closed_state(hwnd),
            ComboClosedState::Hot | ComboClosedState::Dropped
        );
    let border = if !IsWindowEnabled(hwnd).as_bool() {
        palette.control_border()
    } else if interactive_field && (focused || combo_active) {
        palette.accent_border
    } else if interactive_field && hot {
        palette.separator
    } else {
        palette.control_border()
    };
    // CreateRoundRectRgn/FrameRgn is an integer region operation and therefore cannot be the
    // visible outline: at 96-200 DPI it produces the grainy staircase reported by the user. The
    // deterministic coverage calculation paints the straight stroke and every corner sample.
    // Stock Edit children remain ordinary non-layered HWNDs, so the outer glass-key pixels expose
    // the same parent material as ComboBox and ListView instead of relying on a cached bitmap.
    let exterior = rounded_control_exterior(palette);
    draw_antialiased_control_frame(dc, rect, geometry, interior, border, exterior);
}

const fn rounded_control_exterior(palette: Palette) -> COLORREF {
    palette.window
}

unsafe fn is_list_box(hwnd: HWND) -> bool {
    matches!(control_class_name(hwnd).as_str(), "ListBox" | "ComboLBox")
}

unsafe fn update_list_box_hot_item(hwnd: HWND, lparam: LPARAM) {
    const LB_ITEMFROMPOINT: u32 = 0x01a9;
    let packed = SendMessageW(hwnd, LB_ITEMFROMPOINT, WPARAM(0), lparam).0 as u32;
    let outside = packed >> 16 != 0;
    let hot = (!outside).then_some((packed & 0xffff) as usize);
    let previous = GetPropW(hwnd, LIST_BOX_HOT_PROPERTY);
    let previous = (!previous.is_invalid()).then_some(previous.0 as usize - 1);
    if hot == previous {
        return;
    }
    let _ = RemovePropW(hwnd, LIST_BOX_HOT_PROPERTY);
    if let Some(index) = hot {
        let _ = SetPropW(
            hwnd,
            LIST_BOX_HOT_PROPERTY,
            HANDLE((index + 1) as *mut core::ffi::c_void),
        );
        let mut tracking = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: hwnd,
            dwHoverTime: 0,
        };
        let _ = TrackMouseEvent(&mut tracking);
    }
    let _ = InvalidateRect(hwnd, None, false);
}

unsafe fn clear_list_box_hot_item(hwnd: HWND) {
    if RemovePropW(hwnd, LIST_BOX_HOT_PROPERTY).is_ok_and(|handle| !handle.is_invalid()) {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

unsafe fn paint_list_box_rows(hwnd: HWND, palette: Palette) {
    const LB_GETCOUNT: u32 = 0x018b;
    const LB_GETCURSEL: u32 = 0x0188;
    const LB_GETITEMRECT: u32 = 0x0198;
    const LB_GETTEXT: u32 = 0x0189;
    const LB_GETTEXTLEN: u32 = 0x018a;
    const LB_GETTOPINDEX: u32 = 0x018e;
    let count = SendMessageW(hwnd, LB_GETCOUNT, WPARAM(0), LPARAM(0)).0;
    if count <= 0 {
        return;
    }
    let selected = SendMessageW(hwnd, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
    let hot = GetPropW(hwnd, LIST_BOX_HOT_PROPERTY);
    let hot = (!hot.is_invalid()).then_some(hot.0 as usize - 1);
    let top = SendMessageW(hwnd, LB_GETTOPINDEX, WPARAM(0), LPARAM(0))
        .0
        .max(0) as usize;
    let dc = windows::Win32::Graphics::Gdi::GetDC(hwnd);
    if dc.is_invalid() {
        return;
    }
    let font = SendMessageW(hwnd, WM_GETFONT, WPARAM(0), LPARAM(0));
    let old_font = (font.0 != 0)
        .then(|| SelectObject(dc, windows::Win32::Graphics::Gdi::HGDIOBJ(font.0 as *mut _)));
    let _ = SetBkMode(dc, TRANSPARENT);
    let inset = scale(7, GetDpiForWindow(hwnd).max(96));
    for index in top..count as usize {
        let mut row = RECT::default();
        if SendMessageW(
            hwnd,
            LB_GETITEMRECT,
            WPARAM(index),
            LPARAM((&mut row as *mut RECT) as isize),
        )
        .0 < 0
        {
            break;
        }
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        if row.top >= client.bottom {
            break;
        }
        let is_selected = selected >= 0 && selected as usize == index;
        let is_hot = hot == Some(index);
        let (text_color, background) = if is_selected || is_hot {
            navigation_selection_colors(palette, is_hot)
        } else {
            (palette.text, palette.edit)
        };
        fill(dc, &row, background);
        let length = SendMessageW(hwnd, LB_GETTEXTLEN, WPARAM(index), LPARAM(0)).0;
        if length < 0 {
            continue;
        }
        let mut text = vec![0u16; length as usize + 1];
        let _ = SendMessageW(
            hwnd,
            LB_GETTEXT,
            WPARAM(index),
            LPARAM(text.as_mut_ptr() as isize),
        );
        let _ = SetTextColor(dc, text_color);
        row.left += inset;
        row.right -= inset;
        text.truncate(
            text.iter()
                .position(|value| *value == 0)
                .unwrap_or(text.len()),
        );
        draw_alpha_composited_text(
            hwnd,
            dc,
            &text,
            &mut row,
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
            text_color,
            palette.uses_system_backdrop_surface() && !palette.dark,
        );
    }
    if let Some(old_font) = old_font {
        let _ = SelectObject(dc, old_font);
    }
    let _ = windows::Win32::Graphics::Gdi::ReleaseDC(hwnd, dc);
}

unsafe extern "system" fn list_view_parent_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    match message {
        WM_NOTIFY if lparam.0 != 0 => {
            let draw = &mut *(lparam.0 as *mut NMLVCUSTOMDRAW);
            let dark_flag = 1usize << (usize::BITS - 1);
            let backdrop_flag = 1usize << (usize::BITS - 2);
            let list = HWND((reference_data & !(dark_flag | backdrop_flag)) as *mut _);
            if draw.nmcd.hdr.hwndFrom == list && draw.nmcd.hdr.code == NM_CUSTOMDRAW {
                let palette = palette_from_reference(
                    (usize::from(reference_data & dark_flag != 0) * PALETTE_REFERENCE_DARK)
                        | (usize::from(reference_data & backdrop_flag != 0)
                            * PALETTE_REFERENCE_SYSTEM_BACKDROP),
                );
                if draw.nmcd.dwDrawStage == CDDS_PREPAINT {
                    return LRESULT(CDRF_NOTIFYITEMDRAW as isize);
                }
                if draw.nmcd.dwDrawStage == CDDS_ITEMPREPAINT {
                    // Always remove the native selected bit from this transient paint snapshot.
                    // Clearing it only for the currently selected row allows a stale focused row
                    // to be overpainted with the system-blue selection after the real selection
                    // has already moved elsewhere.
                    draw.nmcd.uItemState.0 = list_view_custom_draw_state(draw.nmcd.uItemState.0);
                    // `uItemState` is a custom-draw state snapshot, not the authoritative
                    // ListView selection state. Depending on comctl32 version and focus changes it
                    // can retain CDIS_SELECTED for rows that are no longer selected. Query the
                    // row itself so only the actual LVIS_SELECTED item receives the highlight.
                    const LVM_GETITEMSTATE: u32 = 0x102c;
                    const LVIS_SELECTED: isize = 0x0002;
                    let item_state = SendMessageW(
                        list,
                        LVM_GETITEMSTATE,
                        WPARAM(draw.nmcd.dwItemSpec),
                        LPARAM(LVIS_SELECTED),
                    )
                    .0;
                    let selected = item_state & LVIS_SELECTED != 0;
                    let alpha_composited = palette.uses_system_backdrop_surface() && !palette.dark;
                    if (selected || alpha_composited)
                        && paint_list_view_row(list, draw, palette, selected)
                    {
                        // Windows 11's v6 ItemsView theme paints COLOR_HIGHLIGHT over clrTextBk
                        // after NM_CUSTOMDRAW.  Skip only that one selected row after reproducing
                        // its report-mode text layout; every unselected row remains native.
                        return LRESULT((CDRF_SKIPDEFAULT | CDRF_SKIPPOSTPAINT) as isize);
                    }
                    return LRESULT(CDRF_DODEFAULT as isize);
                }
                return LRESULT(CDRF_DODEFAULT as isize);
            }
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(list_view_parent_subclass), subclass_id);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => DefSubclassProc(hwnd, message, wparam, lparam),
    }
}

unsafe fn paint_list_view_row(
    list: HWND,
    draw: &mut NMLVCUSTOMDRAW,
    palette: Palette,
    selected: bool,
) -> bool {
    const LVM_GETHEADER: u32 = 0x101f;
    const LVM_GETITEMRECT: u32 = 0x100e;
    const LVM_GETITEMSTATE: u32 = 0x102c;
    const LVM_GETSUBITEMRECT: u32 = 0x1038;
    const LVM_GETITEMTEXTW: u32 = 0x1073;
    const HDM_GETITEMCOUNT: u32 = 0x1200;
    const LVIR_BOUNDS: i32 = 0;
    const LVIS_STATEIMAGEMASK: isize = 0xf000;

    let item_index = draw.nmcd.dwItemSpec;
    let mut row = RECT {
        left: LVIR_BOUNDS,
        ..Default::default()
    };
    if SendMessageW(
        list,
        LVM_GETITEMRECT,
        WPARAM(item_index),
        LPARAM((&mut row as *mut RECT) as isize),
    )
    .0 == 0
    {
        return false;
    }

    let mut client = RECT::default();
    if GetClientRect(list, &mut client).is_err() {
        return false;
    }
    row.left = row.left.max(client.left);
    row.top = row.top.max(client.top);
    row.right = row.right.min(client.right);
    row.bottom = row.bottom.min(client.bottom);
    if row.right <= row.left || row.bottom <= row.top {
        return false;
    }

    let (text_color, selection_fill) = list_view_row_colors(palette, selected);
    fill(draw.nmcd.hdc, &row, selection_fill);

    let font = SendMessageW(list, WM_GETFONT, WPARAM(0), LPARAM(0));
    let old_font = (font.0 != 0).then(|| {
        SelectObject(
            draw.nmcd.hdc,
            windows::Win32::Graphics::Gdi::HGDIOBJ(font.0 as *mut _),
        )
    });
    let _ = SetBkMode(draw.nmcd.hdc, TRANSPARENT);
    let _ = SetTextColor(draw.nmcd.hdc, text_color);

    let header = HWND(SendMessageW(list, LVM_GETHEADER, WPARAM(0), LPARAM(0)).0 as *mut _);
    let column_count = if header.0.is_null() {
        1
    } else {
        SendMessageW(header, HDM_GETITEMCOUNT, WPARAM(0), LPARAM(0))
            .0
            .max(1) as i32
    };
    let dpi = GetDpiForWindow(list).max(96);
    let inset = scale(7, dpi);
    let state_image = SendMessageW(
        list,
        LVM_GETITEMSTATE,
        WPARAM(item_index),
        LPARAM(LVIS_STATEIMAGEMASK),
    )
    .0 as u32;

    for subitem in 0..column_count {
        let mut text_rect = RECT {
            left: LVIR_BOUNDS,
            top: subitem,
            ..Default::default()
        };
        if SendMessageW(
            list,
            LVM_GETSUBITEMRECT,
            WPARAM(item_index),
            LPARAM((&mut text_rect as *mut RECT) as isize),
        )
        .0 == 0
        {
            continue;
        }
        text_rect.left = text_rect.left.max(client.left);
        text_rect.right = text_rect.right.min(client.right);
        if text_rect.right <= text_rect.left {
            continue;
        }

        let mut text = vec![0u16; 1024];
        let mut item = LVITEMW {
            mask: LVIF_TEXT,
            iSubItem: subitem,
            pszText: PWSTR(text.as_mut_ptr()),
            cchTextMax: text.len() as i32,
            ..Default::default()
        };
        let copied = SendMessageW(
            list,
            LVM_GETITEMTEXTW,
            WPARAM(item_index),
            LPARAM((&mut item as *mut LVITEMW) as isize),
        )
        .0
        .max(0) as usize;
        text.truncate(copied.min(text.len()));

        text_rect.left += inset;
        if subitem == 0 && state_image & LVIS_STATEIMAGEMASK as u32 != 0 {
            // The deterministic checkbox painter replaces the native state image after WM_PAINT.
            // Reserve the same leading slot so selected-row text never overlaps its glyph.
            text_rect.left += scale(24, dpi);
        }
        text_rect.right -= inset.min((text_rect.right - text_rect.left).max(0));
        draw_opaque_surface_text(
            draw.nmcd.hdc,
            &text,
            &mut text_rect,
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
            text_color,
            selection_fill,
        );
    }

    if let Some(old_font) = old_font {
        let _ = SelectObject(draw.nmcd.hdc, old_font);
    }
    true
}

fn list_view_row_colors(palette: Palette, selected: bool) -> (COLORREF, COLORREF) {
    if selected {
        // A selected report row must stay identical to the resting selected navigation button.
        // Pointer hover must not silently switch it to the brighter hot-button colour.
        navigation_selection_colors(palette, false)
    } else {
        (palette.text, palette.edit)
    }
}

/// Reuses the exact normal/hot palette of the selected left navigation entry. Keeping this as the
/// single source of truth prevents report rows and standalone lists drifting back to system blue.
fn navigation_selection_colors(palette: Palette, hot: bool) -> (COLORREF, COLORREF) {
    let visual = button_visual(
        palette,
        ButtonRole::Navigation { selected: true },
        ControlState {
            hot,
            ..ControlState::default()
        },
    );
    (visual.text, visual.fill)
}

/// Clears native selection, hot and focus paint bits from the transient custom-draw snapshot. The
/// authoritative ListView item state is queried separately and remains unchanged; suppressing the
/// snapshot bits prevents the system light theme from painting a white/focus overlay after the
/// application supplied the selected navigation colour.
const fn list_view_custom_draw_state(snapshot: u32) -> u32 {
    const CDIS_SELECTED: u32 = 0x0001;
    const CDIS_FOCUS: u32 = 0x0010;
    const CDIS_HOT: u32 = 0x0040;
    snapshot & !(CDIS_SELECTED | CDIS_FOCUS | CDIS_HOT)
}

fn list_view_checkbox_rect(row: RECT, dpi: u32) -> RECT {
    let slot_width = scale(24, dpi).max(1);
    let row_height = (row.bottom - row.top).max(1);
    let size = scale(13, dpi).max(1).min(slot_width).min(row_height);
    let left = row.left + (slot_width - size) / 2;
    let top = row.top + (row_height - size) / 2;
    RECT {
        left,
        top,
        right: left + size,
        bottom: top + size,
    }
}

unsafe fn paint_list_view_checkboxes(hwnd: HWND, palette: Palette) {
    const LVIS_STATEIMAGEMASK: isize = 0xf000;
    let top = SendMessageW(hwnd, 0x1027, WPARAM(0), LPARAM(0)).0.max(0) as i32; // TOPINDEX
    let visible = SendMessageW(hwnd, 0x1028, WPARAM(0), LPARAM(0)).0.max(0) as i32; // PERPAGE
    let count = SendMessageW(hwnd, 0x1004, WPARAM(0), LPARAM(0)).0.max(0) as i32; // ITEMCOUNT
    if count == 0 {
        return;
    }
    let dc = windows::Win32::Graphics::Gdi::GetDC(hwnd);
    if dc.is_invalid() {
        return;
    }
    let dpi = GetDpiForWindow(hwnd).max(96);
    let control_state = ControlState {
        disabled: !IsWindowEnabled(hwnd).as_bool(),
        ..ControlState::default()
    };
    for index in top..(top + visible + 1).min(count) {
        let state = SendMessageW(
            hwnd,
            0x102C, // LVM_GETITEMSTATE
            WPARAM(index as usize),
            LPARAM(LVIS_STATEIMAGEMASK),
        )
        .0 as u32;
        let state_image = (state >> 12) & 0xf;
        if state_image == 0 {
            continue;
        }
        let mut row = RECT {
            left: 0, // LVIR_BOUNDS
            ..Default::default()
        };
        if SendMessageW(
            hwnd,
            0x100E, // LVM_GETITEMRECT
            WPARAM(index as usize),
            LPARAM((&mut row as *mut RECT) as isize),
        )
        .0 == 0
        {
            continue;
        }
        // The state image precedes LVIR_ICON. Painting inside LVIR_ICON produced the visible
        // double-checkbox regression (native white state image plus our dark box). Clear and
        // replace the actual leading state-image slot instead.
        let selected = SendMessageW(
            hwnd,
            0x102C, // LVM_GETITEMSTATE
            WPARAM(index as usize),
            LPARAM(0x0002), // LVIS_SELECTED
        )
        .0 != 0;
        let slot = RECT {
            left: row.left,
            top: row.top,
            right: row.left + scale(24, dpi),
            bottom: row.bottom,
        };
        let background = list_view_row_colors(palette, selected).1;
        fill(dc, &slot, background);
        let box_rect = list_view_checkbox_rect(row, dpi);
        let checked = state_image == 2;
        draw_embedded_button_glyph(
            dc,
            box_rect,
            embedded_button_glyph(palette.dark, dpi, control_state, checked),
            background,
            None,
        );
    }
    let _ = windows::Win32::Graphics::Gdi::ReleaseDC(hwnd, dc);
}

unsafe extern "system" fn progress_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    match message {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint_progress(hwnd, palette_from_reference(reference_data));
            LRESULT(0)
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(progress_subclass), PROGRESS_SUBCLASS_ID);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if (0x0401..=0x0410).contains(&message) {
                let _ = InvalidateRect(hwnd, None, false);
            }
            result
        }
    }
}

unsafe fn paint_progress(hwnd: HWND, palette: Palette) {
    let mut paint = PAINTSTRUCT::default();
    let dc = BeginPaint(hwnd, &mut paint);
    let mut rect = RECT::default();
    let _ = GetClientRect(hwnd, &mut rect);
    let position = SendMessageW(hwnd, 0x0408, WPARAM(0), LPARAM(0)).0.max(0) as u64;
    let maximum = SendMessageW(hwnd, 0x0407, WPARAM(0), LPARAM(0)).0.max(1) as u64;
    draw_progress(dc, rect, position, maximum, ProgressRole::Normal, palette);
    let _ = EndPaint(hwnd, &paint);
}

unsafe extern "system" fn trackbar_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    reference_data: usize,
) -> LRESULT {
    match message {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint_trackbar(hwnd, palette_from_reference(reference_data));
            LRESULT(0)
        }
        WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_CAPTURECHANGED | WM_KEYDOWN
        | WM_KEYUP | WM_SETFOCUS | WM_KILLFOCUS | WM_ENABLE => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            let _ = InvalidateRect(hwnd, None, false);
            result
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(trackbar_subclass), TRACKBAR_SUBCLASS_ID);
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
        _ => {
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            // Only setters invalidate. paint_trackbar reads TBM_GETPOS,
            // TBM_GETRANGEMIN and TBM_GETRANGEMAX; treating those queries as mutations causes
            // WM_PAINT -> TBM_GET* -> synchronous WM_PAINT recursion and leaves every child after
            // the slider with its initial white USER32 surface.
            if matches!(message, 0x0405..=0x0408) {
                let _ = InvalidateRect(hwnd, None, false);
            }
            result
        }
    }
}

unsafe fn paint_trackbar(hwnd: HWND, palette: Palette) {
    let mut paint = PAINTSTRUCT::default();
    let dc = BeginPaint(hwnd, &mut paint);
    let mut rect = RECT::default();
    let _ = GetClientRect(hwnd, &mut rect);
    let width = (rect.right - rect.left).max(0);
    let height = (rect.bottom - rect.top).max(0);
    if width == 0 || height == 0 {
        let _ = EndPaint(hwnd, &paint);
        return;
    }
    let memory_dc = CreateCompatibleDC(dc);
    let bitmap = CreateCompatibleBitmap(dc, width, height);
    if memory_dc.is_invalid() || bitmap.is_invalid() {
        if !memory_dc.is_invalid() {
            let _ = DeleteDC(memory_dc);
        }
        if !bitmap.is_invalid() {
            let _ = DeleteObject(bitmap);
        }
        let _ = EndPaint(hwnd, &paint);
        return;
    }
    let old_bitmap = SelectObject(memory_dc, bitmap);
    let local = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };
    fill(memory_dc, &local, palette.window);
    let dpi = GetDpiForWindow(hwnd).max(96);
    let minimum = SendMessageW(hwnd, 0x0401, WPARAM(0), LPARAM(0)).0 as i64;
    let maximum = SendMessageW(hwnd, 0x0402, WPARAM(0), LPARAM(0)).0 as i64;
    let position = SendMessageW(hwnd, 0x0400, WPARAM(0), LPARAM(0)).0 as i64;
    let Some(geometry) = trackbar_geometry(width, height, dpi, minimum, maximum, position) else {
        let _ = BitBlt(dc, 0, 0, width, height, memory_dc, 0, 0, SRCCOPY);
        let _ = SelectObject(memory_dc, old_bitmap);
        let _ = DeleteObject(bitmap);
        let _ = DeleteDC(memory_dc);
        let _ = EndPaint(hwnd, &paint);
        return;
    };
    let enabled = IsWindowEnabled(hwnd).as_bool();
    let visual = trackbar_visual(palette, enabled);
    let channel = geometry.channel.as_rect();
    fill_round_rect_antialiased(
        memory_dc,
        channel,
        geometry.channel_radius,
        visual.track_fill,
        visual.track_border,
        visual.background,
    );
    // The progress fill stays inside the channel's final outline.  The previous paint path drew a
    // second rounded control over the complete channel and could erase the outer stroke at the
    // split or maximum endpoint.  Keeping the fill inside a DPI-scaled inset makes the right-hand
    // track line stable in both themes and across repeated drag paints.
    let selected = geometry
        .channel
        .inset(geometry.channel_border)
        .with_right(geometry.position_x);
    if selected.right > selected.left {
        fill_round_rect_antialiased(
            memory_dc,
            selected.as_rect(),
            ((selected.bottom - selected.top) / 2).max(1),
            visual.progress,
            visual.progress,
            visual.track_fill,
        );
    }
    fill_round_rect_antialiased(
        memory_dc,
        geometry.thumb.as_rect(),
        geometry.thumb_radius,
        visual.thumb_fill,
        visual.thumb_border,
        visual.background,
    );
    let _ = BitBlt(dc, 0, 0, width, height, memory_dc, 0, 0, SRCCOPY);
    let _ = SelectObject(memory_dc, old_bitmap);
    let _ = DeleteObject(bitmap);
    let _ = DeleteDC(memory_dc);
    let _ = EndPaint(hwnd, &paint);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TrackbarRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl TrackbarRect {
    const fn as_rect(self) -> RECT {
        RECT {
            left: self.left,
            top: self.top,
            right: self.right,
            bottom: self.bottom,
        }
    }

    fn inset(self, value: i32) -> Self {
        let value = value.max(0);
        let horizontal = value.min((self.right - self.left).max(0) / 2);
        let vertical = value.min((self.bottom - self.top).max(0) / 2);
        Self {
            left: self.left + horizontal,
            top: self.top + vertical,
            right: self.right - horizontal,
            bottom: self.bottom - vertical,
        }
    }

    fn with_right(mut self, right: i32) -> Self {
        self.right = right.clamp(self.left, self.right);
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TrackbarGeometry {
    channel: TrackbarRect,
    thumb: TrackbarRect,
    position_x: i32,
    channel_radius: i32,
    channel_border: i32,
    thumb_radius: i32,
}

fn trackbar_geometry(
    width: i32,
    height: i32,
    dpi: u32,
    minimum: i64,
    maximum: i64,
    position: i64,
) -> Option<TrackbarGeometry> {
    if width <= 0 || height <= 0 {
        return None;
    }
    let thumb_width = scale(14, dpi).max(10).min(width);
    let thumb_height = scale(22, dpi).max(1).min(height);
    // Split odd dimensions asymmetrically so the exclusive right/bottom edge remains inside the
    // client area at 125%, 150% and other DPI values that produce odd-sized thumbs.
    let thumb_left_half = thumb_width / 2;
    let thumb_right_half = thumb_width - thumb_left_half;
    let thumb_top_half = thumb_height / 2;
    let thumb_bottom_half = thumb_height - thumb_top_half;
    let endpoint_left = thumb_left_half;
    let endpoint_right = (width - thumb_right_half).max(endpoint_left);
    let span = (maximum - minimum).max(1);
    let position_x = endpoint_left
        + (((endpoint_right - endpoint_left) as i64 * (position - minimum).clamp(0, span)) / span)
            as i32;
    let center_y = height / 2;
    let channel_half_height = scale(3, dpi).max(2).min((height / 2).max(1));
    let channel = TrackbarRect {
        left: endpoint_left,
        top: (center_y - channel_half_height).max(0),
        // RECT uses an exclusive right edge. Include the maximum endpoint so its rounded cap and
        // outline are not clipped one pixel before the thumb centre.
        right: (endpoint_right + 1).min(width),
        bottom: (center_y + channel_half_height).min(height),
    };
    let thumb = TrackbarRect {
        left: position_x - thumb_left_half,
        top: center_y - thumb_top_half,
        right: position_x + thumb_right_half,
        bottom: center_y + thumb_bottom_half,
    };
    Some(TrackbarGeometry {
        channel,
        thumb,
        position_x,
        channel_radius: ((channel.bottom - channel.top) / 2).max(1),
        channel_border: scale(1, dpi).max(1),
        thumb_radius: (thumb_width / 2).max(3).min((thumb_height / 2).max(1)),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TrackbarVisual {
    background: COLORREF,
    track_fill: COLORREF,
    track_border: COLORREF,
    progress: COLORREF,
    thumb_fill: COLORREF,
    thumb_border: COLORREF,
}

fn trackbar_visual(palette: Palette, enabled: bool) -> TrackbarVisual {
    TrackbarVisual {
        background: palette.window,
        // A white edit-field trough made the light track look unthemed.  Inno's restrained light
        // channel is closer to the pressed-button/separator pair, while dark mode keeps its deep
        // edit surface and visible border.
        track_fill: if palette.dark {
            palette.edit
        } else {
            palette.button_pressed
        },
        track_border: if palette.dark {
            palette.border
        } else {
            palette.separator
        },
        progress: if enabled {
            palette.progress
        } else {
            palette.separator
        },
        thumb_fill: if enabled {
            palette.button
        } else {
            palette.window
        },
        thumb_border: if enabled {
            palette.text_secondary
        } else {
            palette.text_disabled
        },
    }
}

fn scale(value: i32, dpi: u32) -> i32 {
    ((value as i64 * dpi.max(1) as i64 + 48) / 96) as i32
}

unsafe fn fill(dc: windows::Win32::Graphics::Gdi::HDC, rect: &RECT, color: COLORREF) {
    if color.0 != 0 {
        fill_alpha_opaque_rect(dc, rect, color);
        return;
    }
    let brush = CreateSolidBrush(color);
    let _ = FillRect(dc, rect, brush);
    let _ = DeleteObject(brush);
}

unsafe fn stroke(dc: windows::Win32::Graphics::Gdi::HDC, rect: RECT, color: COLORREF) {
    let pen = CreatePen(PEN_STYLE(0), 1, color);
    let hollow =
        windows::Win32::Graphics::Gdi::GetStockObject(windows::Win32::Graphics::Gdi::NULL_BRUSH);
    let old_pen = SelectObject(dc, pen);
    let old_brush = SelectObject(dc, hollow);
    let _ =
        windows::Win32::Graphics::Gdi::Rectangle(dc, rect.left, rect.top, rect.right, rect.bottom);
    let _ = SelectObject(dc, old_brush);
    let _ = SelectObject(dc, old_pen);
    let _ = DeleteObject(pen);
}

unsafe fn round_rect(
    dc: windows::Win32::Graphics::Gdi::HDC,
    rect: RECT,
    radius: i32,
    background: COLORREF,
    border: COLORREF,
) {
    let brush = CreateSolidBrush(background);
    let pen = CreatePen(PEN_STYLE(0), 1, border);
    let old_brush = SelectObject(dc, brush);
    let old_pen = SelectObject(dc, pen);
    let diameter = radius.saturating_mul(2);
    let _ = RoundRect(
        dc,
        rect.left,
        rect.top,
        rect.right,
        rect.bottom,
        diameter,
        diameter,
    );
    let _ = SelectObject(dc, old_pen);
    let _ = SelectObject(dc, old_brush);
    let _ = DeleteObject(pen);
    let _ = DeleteObject(brush);
}

pub struct Brushes {
    pub window: HBRUSH,
    pub nav: HBRUSH,
    pub edit: HBRUSH,
    pub edit_opaque: HBRUSH,
    pub list: HBRUSH,
}

impl Brushes {
    pub fn new(palette: Palette) -> Self {
        unsafe {
            Self {
                window: CreateSolidBrush(palette.window),
                nav: CreateSolidBrush(palette.nav),
                edit: CreateSolidBrush(palette.edit_brush_color()),
                edit_opaque: CreateSolidBrush(palette.edit),
                list: CreateSolidBrush(palette.edit),
            }
        }
    }
}

impl Drop for Brushes {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.window);
            let _ = DeleteObject(self.nav);
            let _ = DeleteObject(self.edit);
            let _ = DeleteObject(self.edit_opaque);
            let _ = DeleteObject(self.list);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inno_windows11_reference_colors_are_stable() {
        assert_eq!(Palette::LIGHT.window, rgb(249, 249, 249));
        assert_eq!(Palette::LIGHT.edit, rgb(255, 255, 255));
        assert_eq!(Palette::LIGHT.border, rgb(230, 230, 230));
        assert_eq!(Palette::LIGHT.separator, rgb(222, 222, 222));
        assert_eq!(Palette::LIGHT.accent_fill, rgb(0, 95, 184));
        assert_eq!(Palette::DARK.window, rgb(43, 43, 43));
        assert_eq!(Palette::DARK.edit, rgb(28, 28, 28));
        assert_eq!(Palette::DARK.button, rgb(48, 48, 48));
        assert_eq!(Palette::DARK.separator, rgb(72, 72, 72));
        assert_eq!(Palette::DARK.accent_border, rgb(66, 149, 192));
        assert_eq!(Palette::DARK.highlight_fill, rgb(76, 194, 255));
        assert_eq!(Palette::DARK.highlight_border, rgb(76, 194, 255));
    }

    #[test]
    fn subclass_palette_reference_preserves_system_backdrop_surface() {
        for base in [Palette::LIGHT, Palette::DARK] {
            let normal = palette_from_reference(palette_reference(base));
            assert_eq!(normal.dark, base.dark);
            assert_eq!(normal.window, base.window);
            assert_eq!(normal.nav, base.nav);
            assert_eq!(normal.edit, base.edit);

            let material = base.with_system_backdrop_surface();
            let background = base.system_backdrop_edge_fallback();
            let normal_surface = base.material_surface_visual(MaterialSurfaceState::Normal);
            let hot_surface = base.material_surface_visual(MaterialSurfaceState::Hot);
            let pressed_surface = base.material_surface_visual(MaterialSurfaceState::Pressed);
            assert_eq!(
                material.edit,
                composite_color(normal_surface.fill, normal_surface.fill_alpha, background)
            );
            assert_eq!(material.button, material.edit);
            assert_eq!(
                material.button_hot,
                composite_color(hot_surface.fill, hot_surface.fill_alpha, background)
            );
            assert_eq!(
                material.button_pressed,
                composite_color(pressed_surface.fill, pressed_surface.fill_alpha, background)
            );
            assert_eq!(
                material.border,
                composite_color(
                    normal_surface.border,
                    normal_surface.border_alpha,
                    background
                )
            );
            assert_eq!(material.separator, material.border);
            assert_eq!(material.text, base.text);
            let restored = palette_from_reference(palette_reference(material));
            assert_eq!(restored.dark, base.dark);
            assert_eq!(restored.window, COLORREF(0));
            assert_eq!(restored.nav, COLORREF(0));
            assert_eq!(restored.edit, material.edit);
            assert_eq!(restored.button, material.button);
            assert_eq!(restored.text, material.text);
            assert_eq!(
                material.system_backdrop_edge_fallback(),
                if base.dark {
                    rgb(32, 32, 32)
                } else {
                    rgb(243, 243, 243)
                }
            );
            assert_eq!(material.control_border(), material.border);
            assert_eq!(material.edit_brush_color(), material.edit);
            assert_eq!(
                material.foreground_black(),
                if base.dark {
                    rgb(1, 1, 1)
                } else {
                    rgb(24, 24, 24)
                }
            );
        }
        assert_eq!(Palette::LIGHT.foreground_black(), rgb(0, 0, 0));
        assert_eq!(Palette::DARK.foreground_black(), rgb(0, 0, 0));
    }

    #[test]
    fn material_controls_keep_theme_specific_depth_without_pure_white_surfaces() {
        let dark = Palette::DARK.material_surface_visual(MaterialSurfaceState::Normal);
        let light = Palette::LIGHT.material_surface_visual(MaterialSurfaceState::Normal);
        assert!(dark.fill_alpha >= 170);
        assert!(dark.border_alpha >= 100);
        assert!(light.fill_alpha < dark.fill_alpha);
        assert_ne!(light.fill, rgb(255, 255, 255));
        assert!(light.border_alpha >= 60);
    }

    #[test]
    fn native_edit_uses_the_parent_glass_key_without_a_binary_region() {
        for base in [Palette::LIGHT, Palette::DARK] {
            let material = base.with_system_backdrop_surface();
            assert_eq!(rounded_control_exterior(material), COLORREF(0));
            assert_eq!(rounded_control_exterior(base), base.window);
        }
    }

    #[test]
    fn native_edit_text_stays_visible_on_light_and_dark_material() {
        let light = Palette::LIGHT.with_system_backdrop_surface();
        let dark = Palette::DARK.with_system_backdrop_surface();
        assert_eq!(light.edit_text_color(), rgb(24, 24, 24));
        assert_eq!(light.edit_brush_color(), light.edit);
        assert_eq!(dark.edit_text_color(), dark.text);
    }

    #[test]
    fn light_material_combo_chevron_has_real_alpha_and_dark_rgb() {
        let color = Palette::LIGHT.text_secondary;
        let pixels = combo_chevron_pixels(8, 5, color);
        assert_eq!(pixels.len(), 8 * 5 * 4);
        let bottom_left_alpha = pixels[((5 - 1) * 8 * 4) + 3];
        assert_eq!(
            bottom_left_alpha, 0,
            "pixels outside the V silhouette must remain transparent"
        );
        let strongest = pixels
            .chunks_exact(4)
            .max_by_key(|pixel| pixel[3])
            .expect("chevron pixels");
        assert!(strongest[3] >= 240, "chevron needs an opaque visible core");
        assert!(strongest[0] < 96 && strongest[1] < 96 && strongest[2] < 96);
    }

    #[test]
    fn native_edit_never_keeps_layered_redirection() {
        let other_styles = 0x0000_0004isize | 0x0001_0000isize;
        assert_eq!(
            edit_ex_style_without_layering(other_styles | WS_EX_LAYERED.0 as isize),
            other_styles
        );
        assert_eq!(edit_ex_style_without_layering(other_styles), other_styles);
    }

    #[test]
    fn opaque_black_glyph_pixels_do_not_become_dwm_glass_holes() {
        assert_eq!(
            preserve_visible_black_on_system_backdrop(0, 255, 0, 0, 0),
            (1, 1, 1)
        );
        assert_eq!(
            preserve_visible_black_on_system_backdrop(0, 0, 0, 0, 0),
            (0, 0, 0)
        );
        assert_eq!(
            preserve_visible_black_on_system_backdrop(0, 255, 3, 4, 5),
            (3, 4, 5)
        );
        assert_eq!(
            preserve_visible_black_on_system_backdrop(rgb(249, 249, 249).0, 255, 0, 0, 0),
            (0, 0, 0)
        );
    }

    #[test]
    fn native_theme_classes_cover_headers_scrollbars_and_fields() {
        assert_eq!(
            native_theme_class(NativeControlKind::Header, true),
            NativeThemeClass::DarkItemsView
        );
        assert_eq!(
            native_theme_class(NativeControlKind::ScrollableField, true),
            NativeThemeClass::DarkExplorer
        );
        assert_eq!(
            native_theme_class(NativeControlKind::List, true),
            NativeThemeClass::DarkExplorer
        );
        assert_eq!(
            native_theme_class(NativeControlKind::Field, true),
            NativeThemeClass::DarkCfd
        );
        assert_eq!(
            native_theme_class(NativeControlKind::Field, false),
            NativeThemeClass::Cfd
        );
    }

    #[test]
    fn trackbar_geometry_keeps_minimum_and_maximum_endpoints_inside_the_client() {
        for dpi in [96, 120, 144, 192] {
            let minimum = trackbar_geometry(801, 34, dpi, 10, 90, 10).unwrap();
            let maximum = trackbar_geometry(801, 34, dpi, 10, 90, 90).unwrap();

            for geometry in [minimum, maximum] {
                assert!(geometry.channel.left >= 0);
                assert!(geometry.channel.right <= 801);
                assert!(geometry.channel.top >= 0);
                assert!(geometry.channel.bottom <= 34);
                assert!(geometry.thumb.left >= 0);
                assert!(geometry.thumb.right <= 801);
                assert!(geometry.thumb.top >= 0);
                assert!(geometry.thumb.bottom <= 34);
                assert!(geometry.channel.right > geometry.channel.left);
                assert!(geometry.thumb.right > geometry.thumb.left);

                let inner = geometry.channel.inset(geometry.channel_border);
                let selected = inner.with_right(geometry.position_x);
                assert!(selected.left >= geometry.channel.left + geometry.channel_border);
                assert!(selected.right <= geometry.channel.right - geometry.channel_border);
            }

            let minimum_inner = minimum.channel.inset(minimum.channel_border);
            let maximum_inner = maximum.channel.inset(maximum.channel_border);
            assert_eq!(
                minimum_inner.with_right(minimum.position_x).right,
                minimum_inner.left
            );
            assert_eq!(
                maximum_inner.with_right(maximum.position_x).right,
                maximum_inner.right
            );
        }
    }

    #[test]
    fn trackbar_geometry_clamps_positions_and_handles_tiny_surfaces() {
        let below = trackbar_geometry(300, 24, 96, 20, 40, -100).unwrap();
        let above = trackbar_geometry(300, 24, 96, 20, 40, 999).unwrap();
        assert_eq!(below.position_x, below.channel.left);
        assert_eq!(above.position_x, above.channel.right - 1);
        assert!(trackbar_geometry(0, 24, 96, 0, 1, 0).is_none());
        assert!(trackbar_geometry(300, 0, 96, 0, 1, 0).is_none());

        let tiny = trackbar_geometry(1, 1, 144, 0, 0, 0).unwrap();
        assert_eq!(
            tiny.thumb,
            TrackbarRect {
                left: 0,
                top: 0,
                right: 1,
                bottom: 1
            }
        );
        assert!(tiny.channel.top >= 0 && tiny.channel.bottom <= 1);
    }

    #[test]
    fn trackbar_visuals_are_theme_specific_and_idempotent() {
        let light = trackbar_visual(Palette::LIGHT, true);
        let dark = trackbar_visual(Palette::DARK, true);
        assert_eq!(light, trackbar_visual(Palette::LIGHT, true));
        assert_eq!(dark, trackbar_visual(Palette::DARK, true));
        assert_eq!(light.track_fill, Palette::LIGHT.button_pressed);
        assert_eq!(light.track_border, Palette::LIGHT.separator);
        assert_ne!(light.track_fill, Palette::LIGHT.edit);
        assert_ne!(light.track_fill, Palette::LIGHT.window);
        assert_eq!(dark.track_fill, Palette::DARK.edit);
        assert_eq!(dark.track_border, Palette::DARK.border);
        assert_eq!(light.progress, Palette::LIGHT.progress);
        assert_eq!(dark.progress, Palette::DARK.progress);

        let disabled = trackbar_visual(Palette::DARK, false);
        assert_eq!(disabled.progress, Palette::DARK.separator);
        assert_eq!(disabled.thumb_border, Palette::DARK.text_disabled);
    }

    #[test]
    fn field_styles_remove_competing_native_edges() {
        assert!(is_edit_class("Edit"));
        assert!(is_edit_class("EDIT"));
        assert!(!is_edit_class("ComboBox"));
        assert!(is_combo_class("ComboBox"));
        let (style, ex_style) = borderless_style_bits(
            0x1000 | WS_BORDER.0 as isize,
            WS_EX_CLIENTEDGE.0 as isize | 0x2000,
        );
        assert_eq!(style & WS_BORDER.0 as isize, 0);
        assert_eq!(ex_style & WS_EX_CLIENTEDGE.0 as isize, 0);
        assert_ne!(ex_style & 0x2000, 0);

        let (list_style, list_ex_style) =
            single_border_style_bits(0x1000, WS_EX_CLIENTEDGE.0 as isize | 0x2000);
        assert_ne!(list_style & WS_BORDER.0 as isize, 0);
        assert_eq!(list_ex_style & WS_EX_CLIENTEDGE.0 as isize, 0);

        let (scrollable_style, _) =
            borderless_style_bits(WS_BORDER.0 as isize | WS_VSCROLL.0 as isize, 0);
        assert_eq!(scrollable_style & WS_BORDER.0 as isize, 0);
        assert_ne!(scrollable_style & WS_VSCROLL.0 as isize, 0);
    }

    #[test]
    fn combo_closed_height_uses_the_shared_field_baseline_at_every_supported_dpi() {
        for dpi in [96, 120, 144, 168, 192] {
            let requested = InnoMetrics::for_dpi(dpi).field_height;
            assert_eq!(combo_closed_visual_height(requested, dpi), requested);
            // A taller COMBOBOXINFO measurement must never leak into the visible HWND region.
            assert_eq!(combo_closed_visual_height(requested, dpi), scale(23, dpi));
        }
        assert_eq!(combo_closed_visual_height(0, 96), scale(18, 96));
        assert_eq!(combo_closed_visual_height(1000, 192), scale(36, 192));
    }

    #[test]
    fn native_scrollbar_messages_require_the_rounded_frame_to_be_painted_last() {
        for message in [
            0x00a1, 0x00a2, 0x00a3, 0x0113, 0x0114, 0x0115, 0x020a, 0x020e, 0x02a0,
        ] {
            assert!(native_scrollbar_may_repaint_frame(message));
        }
        for message in [WM_SETTEXT, WM_ENABLE, WM_SIZE, WM_PAINT] {
            assert!(!native_scrollbar_may_repaint_frame(message));
        }
    }

    #[test]
    fn list_view_scroll_messages_discard_moved_frame_pixels() {
        for message in [0x0114, 0x0115, 0x020a, 0x020e] {
            assert!(list_view_scrolls_client_pixels(message));
        }
        for message in [0x00a1, 0x00a2, 0x00a3, 0x0113, 0x02a0, WM_PAINT] {
            assert!(!list_view_scrolls_client_pixels(message));
        }
    }

    #[test]
    fn list_view_selection_matches_navigation_and_does_not_shift_on_hover() {
        let dark_nav = button_visual(
            Palette::DARK,
            ButtonRole::Navigation { selected: true },
            ControlState::default(),
        );
        let light_nav = button_visual(
            Palette::LIGHT,
            ButtonRole::Navigation { selected: true },
            ControlState::default(),
        );
        assert_eq!(
            list_view_row_colors(Palette::DARK, false),
            (Palette::DARK.text, Palette::DARK.edit)
        );
        assert_eq!(
            list_view_row_colors(Palette::DARK, true),
            (dark_nav.text, dark_nav.fill)
        );
        assert_eq!(dark_nav.fill, rgb(76, 194, 255));
        assert_eq!(
            list_view_row_colors(Palette::LIGHT, true),
            (light_nav.text, light_nav.fill)
        );
        assert_eq!(
            list_view_row_colors(Palette::DARK, false),
            (Palette::DARK.text, Palette::DARK.edit)
        );
        assert_eq!(
            list_view_row_colors(Palette::LIGHT, false),
            (Palette::LIGHT.text, Palette::LIGHT.edit)
        );
        assert_eq!(
            list_view_row_colors(Palette::DARK, true),
            (dark_nav.text, dark_nav.fill)
        );
        assert_ne!(light_nav.fill, Palette::LIGHT.edit);
        assert_ne!(light_nav.fill, Palette::LIGHT.window);
        assert_ne!(Palette::DARK.accent_fill, Palette::DARK.progress);
    }

    #[test]
    fn empty_list_view_owns_its_body_paint() {
        assert!(list_view_needs_empty_body_paint(0));
        assert!(!list_view_needs_empty_body_paint(1));
        assert!(!list_view_needs_empty_body_paint(32));
    }

    #[test]
    fn stale_list_view_snapshot_never_overrides_real_selection_colors() {
        const CDIS_SELECTED: u32 = 0x0001;
        const CDIS_FOCUS: u32 = 0x0010;
        const CDIS_HOT: u32 = 0x0040;
        let stale_snapshot = CDIS_SELECTED | CDIS_FOCUS | CDIS_HOT;

        assert_eq!(list_view_custom_draw_state(stale_snapshot), 0);
        assert_eq!(
            list_view_row_colors(Palette::DARK, false),
            (Palette::DARK.text, Palette::DARK.edit)
        );
        let selected = button_visual(
            Palette::DARK,
            ButtonRole::Navigation { selected: true },
            ControlState::default(),
        );
        assert_eq!(
            list_view_row_colors(Palette::DARK, true),
            (selected.text, selected.fill)
        );
    }

    #[test]
    fn native_theme_still_supplies_control_content_beneath_deterministic_frames() {
        assert_eq!(
            native_theme_class(NativeControlKind::Field, true),
            NativeThemeClass::DarkCfd
        );
        assert_eq!(
            native_theme_class(NativeControlKind::ListView, true),
            NativeThemeClass::DarkExplorer
        );
        // Keep the report's native non-client scrollbar in the supported dark Explorer family.
        // FlatSB colour overrides are unavailable in comctl32 v6 and ItemsView produces a bright
        // white scrollbar trough on the dark Mica surface.
        assert_eq!(
            native_theme_class(NativeControlKind::Field, false),
            NativeThemeClass::Cfd
        );
    }

    #[test]
    fn shared_radio_painter_is_limited_to_real_auto_radio_buttons() {
        assert!(button_style_is_auto_radio(0x0009));
        assert!(button_style_is_auto_radio(0x5001_0009));
        assert!(!button_style_is_auto_radio(0x0003)); // auto checkbox
        assert!(!is_auto_radio_button("Static", 0x0009));
        assert!(is_auto_radio_button("BUTTON", 0x0009));
    }

    #[test]
    fn radio_geometry_is_centered_and_bounded_at_supported_dpi() {
        for dpi in [96, 120, 144, 192] {
            let width = scale(300, dpi);
            let height = scale(24, dpi);
            let geometry = radio_geometry(width, height, dpi).unwrap();
            let glyph_size = geometry.glyph.right - geometry.glyph.left;
            assert_eq!(glyph_size, scale(13, dpi));
            assert_eq!(geometry.glyph.bottom - geometry.glyph.top, glyph_size);
            assert_eq!(geometry.glyph.top, (height - glyph_size) / 2);
            assert!(geometry.glyph.left >= 0 && geometry.glyph.right <= width);
            assert!(geometry.glyph.top >= 0 && geometry.glyph.bottom <= height);
            assert_eq!(geometry.text.left, geometry.glyph.right + scale(5, dpi));
            assert_eq!(geometry.text.right, width);
        }
    }

    #[test]
    fn light_material_checkbox_keeps_the_embedded_shape_but_not_black_corner_texels() {
        assert_eq!(
            checkbox_material_fallback(Palette::LIGHT.with_system_backdrop_surface()),
            Some(rgb(243, 243, 243))
        );
        assert_eq!(
            checkbox_material_fallback(Palette::DARK.with_system_backdrop_surface()),
            Some(rgb(32, 32, 32))
        );
        assert_eq!(checkbox_material_fallback(Palette::LIGHT), None);
    }

    #[test]
    fn radio_geometry_fails_closed_for_empty_and_clamps_tiny_controls() {
        assert_eq!(radio_geometry(0, 24, 96), None);
        assert_eq!(radio_geometry(100, 0, 96), None);
        let geometry = radio_geometry(7, 5, 192).unwrap();
        assert_eq!(geometry.glyph.right, 5);
        assert_eq!(geometry.glyph.bottom, 5);
        assert!(geometry.text.left <= 7);
    }

    #[test]
    fn list_view_checkbox_uses_the_regular_checkbox_size_and_stays_in_its_slot() {
        for dpi in [96, 120, 144, 168, 192] {
            let row = RECT {
                left: 7,
                top: 11,
                right: 407,
                bottom: 11 + scale(24, dpi),
            };
            let rect = list_view_checkbox_rect(row, dpi);
            let expected_size = scale(13, dpi);
            assert_eq!(rect.right - rect.left, expected_size);
            assert_eq!(rect.bottom - rect.top, expected_size);
            assert_eq!(rect.top - row.top, (scale(24, dpi) - expected_size) / 2);
            assert!(rect.left >= row.left);
            assert!(rect.right <= row.left + scale(24, dpi));
            assert!(rect.top >= row.top);
            assert!(rect.bottom <= row.bottom);
        }
    }

    #[test]
    fn embedded_win11_button_theme_maps_all_states_and_dpi_buckets() {
        let normal = ControlState {
            hot: false,
            pressed: false,
            disabled: false,
            focused: false,
        };
        assert_eq!(embedded_theme_dpi_index(96), 0);
        assert_eq!(embedded_theme_dpi_index(120), 1);
        assert_eq!(embedded_theme_dpi_index(144), 2);
        assert_eq!(embedded_theme_dpi_index(192), 3);
        assert_eq!(themed_button_state(normal, false), 0);
        assert_eq!(themed_button_state(normal, true), 4);
        assert_eq!(
            themed_button_state(
                ControlState {
                    hot: true,
                    ..normal
                },
                true
            ),
            5
        );
        assert_eq!(
            themed_button_state(
                ControlState {
                    pressed: true,
                    ..normal
                },
                true
            ),
            6
        );
        assert_eq!(
            themed_button_state(
                ControlState {
                    disabled: true,
                    ..normal
                },
                true
            ),
            7
        );
        for dark in [false, true] {
            for dpi in [96, 120, 144, 192] {
                let glyph = embedded_button_glyph(dark, dpi, normal, true);
                assert_eq!(glyph.width, scale(13, dpi));
                assert_eq!(glyph.height, scale(13, dpi));
                assert_eq!(glyph.bgra.len(), (glyph.width * glyph.height * 4) as usize);
            }
        }
    }

    #[test]
    fn generated_radio_glyphs_are_symmetric_and_light_mode_has_no_black_spurs() {
        for side in [13, 16, 17, 20, 23, 26] {
            for palette in [Palette::LIGHT, Palette::DARK] {
                for checked in [false, true] {
                    for state in [
                        ControlState::default(),
                        ControlState {
                            hot: true,
                            ..ControlState::default()
                        },
                        ControlState {
                            pressed: true,
                            ..ControlState::default()
                        },
                        ControlState {
                            disabled: true,
                            ..ControlState::default()
                        },
                    ] {
                        let pixels = radio_glyph_bgra(side, palette, state, checked);
                        let pixel = |x: i32, y: i32| {
                            let offset = ((y * side + x) * 4) as usize;
                            &pixels[offset..offset + 4]
                        };
                        assert_eq!(pixels.len(), (side * side * 4) as usize);
                        for y in 0..side {
                            for x in 0..side {
                                assert_eq!(pixel(x, y), pixel(side - 1 - x, y));
                                assert_eq!(pixel(x, y), pixel(x, side - 1 - y));
                                assert_eq!(pixel(x, y)[3], 255);
                                if !palette.dark {
                                    assert_ne!(&pixel(x, y)[0..3], &[0, 0, 0]);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn material_radio_glyph_keeps_the_rectangular_tile_fully_transparent() {
        for base in [Palette::LIGHT, Palette::DARK] {
            let palette = base.with_system_backdrop_surface();
            for side in [13, 20, 26] {
                for checked in [false, true] {
                    let pixels = radio_glyph_bgra(side, palette, ControlState::default(), checked);
                    let pixel = |x: i32, y: i32| {
                        let offset = ((y * side + x) * 4) as usize;
                        &pixels[offset..offset + 4]
                    };
                    for (x, y) in [(0, 0), (side - 1, 0), (0, side - 1), (side - 1, side - 1)] {
                        assert_eq!(pixel(x, y), &[0, 0, 0, 0]);
                    }
                    assert_eq!(pixel(side / 2, side / 2)[3], 255);
                }
            }
        }
    }

    #[test]
    fn edit_mutation_messages_are_republished_before_returning_to_dwm() {
        for message in [
            0x000c, 0x00c2, 0x0102, 0x0109, 0x010f, 0x0300, 0x0302, 0x0303, 0x0304,
        ] {
            assert!(edit_message_may_change_visible_text(message, 0));
        }
        assert!(edit_message_may_change_visible_text(WM_KEYDOWN, 0x2e));
        assert!(!edit_message_may_change_visible_text(WM_KEYDOWN, 0x25));
        assert!(!edit_message_may_change_visible_text(WM_MOUSEMOVE, 0));
    }

    #[test]
    fn list_view_trailing_material_fill_begins_after_the_final_row() {
        let client = RECT {
            left: 0,
            top: 0,
            right: 900,
            bottom: 180,
        };
        assert_eq!(
            list_view_trailing_body_rect(client, 3, Some(72)),
            Some(RECT {
                left: 0,
                top: 72,
                right: 900,
                bottom: 180,
            })
        );
        assert_eq!(list_view_trailing_body_rect(client, 0, None), None);
        assert_eq!(list_view_trailing_body_rect(client, 3, Some(180)), None);
        assert_eq!(list_view_trailing_body_rect(client, 3, Some(240)), None);
    }
}
