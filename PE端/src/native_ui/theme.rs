use windows::Win32::Foundation::COLORREF;
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, DeleteObject, HBRUSH};

const fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    COLORREF((red as u32) | ((green as u32) << 8) | ((blue as u32) << 16))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeMode {
    Light,
    #[default]
    Dark,
}

impl ThemeMode {
    /// PE has no reliable shell theme service. A deployment may opt into light mode explicitly;
    /// otherwise use the compact dark Inno variant that remains readable in minimal WinPE images.
    pub fn detect() -> Self {
        match std::env::var("LETRECOVERY_PE_THEME") {
            Ok(value) if value.eq_ignore_ascii_case("light") => Self::Light,
            _ => Self::Dark,
        }
    }
}

/// Inno Setup 6.7 Modern Windows 11 colour roles for the PE native window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub dark: bool,
    pub window: COLORREF,
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
    pub progress: COLORREF,
    pub error: COLORREF,
    pub warning: COLORREF,
}

impl Palette {
    pub const LIGHT: Self = Self {
        dark: false,
        window: rgb(249, 249, 249),
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
        progress: rgb(113, 199, 132),
        error: rgb(196, 43, 28),
        warning: rgb(181, 108, 0),
    };

    pub const DARK: Self = Self {
        dark: true,
        window: rgb(43, 43, 43),
        edit: rgb(31, 31, 31),
        button: rgb(55, 55, 55),
        button_hot: rgb(62, 62, 62),
        button_pressed: rgb(47, 47, 47),
        text: rgb(255, 255, 255),
        text_secondary: rgb(214, 214, 214),
        text_disabled: rgb(120, 120, 120),
        border: rgb(67, 67, 67),
        separator: rgb(81, 81, 81),
        accent_fill: rgb(49, 72, 83),
        accent_border: rgb(66, 149, 192),
        progress: rgb(113, 199, 132),
        error: rgb(232, 87, 74),
        warning: rgb(247, 153, 52),
    };

    pub const fn for_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Light => Self::LIGHT,
            ThemeMode::Dark => Self::DARK,
        }
    }
}

/// DPI-scaled metrics shared by the eventual PE window and its dialogs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InnoMetrics {
    pub control_height: i32,
    pub button_height: i32,
    pub button_min_width: i32,
    pub horizontal_padding: i32,
    pub item_gap: i32,
    pub section_gap: i32,
    pub corner_radius: i32,
    pub progress_height: i32,
    pub separator_thickness: i32,
}

impl InnoMetrics {
    pub fn for_dpi(dpi: u32) -> Self {
        let scale = |value: i32| ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96) as i32;
        Self {
            control_height: scale(24),
            button_height: scale(30),
            button_min_width: scale(75),
            horizontal_padding: scale(14),
            item_gap: scale(8),
            section_gap: scale(16),
            corner_radius: scale(4).clamp(2, scale(6).max(2)),
            progress_height: scale(10),
            separator_thickness: scale(1).max(1),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeContext {
    pub mode: ThemeMode,
    pub palette: Palette,
    pub metrics: InnoMetrics,
    pub dpi: u32,
}

impl ThemeContext {
    pub fn detect(dpi: u32) -> Self {
        Self::new(ThemeMode::detect(), dpi)
    }

    pub fn new(mode: ThemeMode, dpi: u32) -> Self {
        Self {
            mode,
            palette: Palette::for_mode(mode),
            metrics: InnoMetrics::for_dpi(dpi),
            dpi: dpi.max(1),
        }
    }
}

pub struct ThemeBrushes {
    pub window: HBRUSH,
    pub edit: HBRUSH,
    pub button: HBRUSH,
}

impl ThemeBrushes {
    /// The returned brushes are owned by this value and released on drop.
    ///
    /// # Safety
    ///
    /// The returned brushes must not be selected into a device context when this value is dropped.
    /// Any window that receives one of these brushes from a colour-message handler must stop using
    /// it before the `ThemeBrushes` owner is destroyed.
    pub unsafe fn new(palette: Palette) -> Self {
        Self {
            window: CreateSolidBrush(palette.window),
            edit: CreateSolidBrush(palette.edit),
            button: CreateSolidBrush(palette.button),
        }
    }
}

impl Drop for ThemeBrushes {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.window);
            let _ = DeleteObject(self.edit);
            let _ = DeleteObject(self.button);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_scale_stably_from_100_to_200_percent() {
        let normal = InnoMetrics::for_dpi(96);
        let doubled = InnoMetrics::for_dpi(192);
        assert_eq!(normal.control_height, 24);
        assert_eq!(doubled.control_height, 48);
        assert_eq!(doubled.item_gap, normal.item_gap * 2);
        assert!(doubled.corner_radius <= 12);
    }

    #[test]
    fn light_and_dark_keep_one_progress_accent() {
        assert_ne!(Palette::LIGHT.window, Palette::DARK.window);
        assert_ne!(Palette::LIGHT.text, Palette::DARK.text);
        assert_eq!(Palette::LIGHT.progress, Palette::DARK.progress);
    }
}
