//! 内嵌资源模块
//! 管理编译时内嵌到程序中的静态资源（如 SVG 图标）
//!
//! 重要说明：
//! 为了在高 DPI 显示器上清晰显示，渲染尺寸必须考虑 pixels_per_point 因子。
//! 例如：显示 72x72 逻辑像素的图像，在 pixels_per_point=1.5 的显示器上
//! 需要渲染 108x108 物理像素才能清晰。

use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use std::collections::HashMap;
use std::sync::OnceLock;

/// 内嵌的 SVG 原始数据
pub mod svg_data {
    /// Windows 10 深色模式 Logo
    pub const WIN10_DARK: &[u8] = include_bytes!("../../assets/win10_dark.svg");
    /// Windows 10 浅色模式 Logo
    pub const WIN10_LIGHT: &[u8] = include_bytes!("../../assets/win10_light.svg");
    /// Windows 11 深色模式 Logo
    pub const WIN11_DARK: &[u8] = include_bytes!("../../assets/win11_dark.svg");
    /// Windows 11 浅色模式 Logo
    pub const WIN11_LIGHT: &[u8] = include_bytes!("../../assets/win11_light.svg");
}

/// 内嵌 Logo 的类型标识
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EmbeddedLogoType {
    Windows10Dark,
    Windows10Light,
    Windows11Dark,
    Windows11Light,
}

impl EmbeddedLogoType {
    /// 获取该类型对应的 SVG 数据
    pub fn svg_data(&self) -> &'static [u8] {
        match self {
            EmbeddedLogoType::Windows10Dark => svg_data::WIN10_DARK,
            EmbeddedLogoType::Windows10Light => svg_data::WIN10_LIGHT,
            EmbeddedLogoType::Windows11Dark => svg_data::WIN11_DARK,
            EmbeddedLogoType::Windows11Light => svg_data::WIN11_LIGHT,
        }
    }

    /// 获取缓存键名
    pub fn cache_key(&self) -> &'static str {
        match self {
            EmbeddedLogoType::Windows10Dark => "embedded_win10_dark",
            EmbeddedLogoType::Windows10Light => "embedded_win10_light",
            EmbeddedLogoType::Windows11Dark => "embedded_win11_dark",
            EmbeddedLogoType::Windows11Light => "embedded_win11_light",
        }
    }

    /// 根据配置字符串和深色模式标志获取对应的 Logo 类型
    /// 返回 None 表示这不是一个内嵌 logo 标识符
    pub fn from_config_string(s: &str, is_dark_mode: bool) -> Option<Self> {
        match s {
            "LOGO_WINDOWS10" => Some(if is_dark_mode {
                EmbeddedLogoType::Windows10Dark
            } else {
                EmbeddedLogoType::Windows10Light
            }),
            "LOGO_WINDOWS11" => Some(if is_dark_mode {
                EmbeddedLogoType::Windows11Dark
            } else {
                EmbeddedLogoType::Windows11Light
            }),
            _ => None,
        }
    }

    /// 检查字符串是否是内嵌 logo 标识符
    pub fn is_embedded_logo_identifier(s: &str) -> bool {
        matches!(s, "LOGO_WINDOWS10" | "LOGO_WINDOWS11")
    }
}

/// 预渲染的 SVG 图像数据（RGBA 格式）
struct RenderedSvg {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
}

/// 全局 SVG 树缓存（存储解析后的 SVG 树，用于按需渲染）
static SVG_TREE_CACHE: OnceLock<HashMap<EmbeddedLogoType, resvg::usvg::Tree>> = OnceLock::new();

/// 初始化 SVG 树缓存
fn init_svg_tree_cache() -> HashMap<EmbeddedLogoType, resvg::usvg::Tree> {
    let mut cache = HashMap::new();
    let opt = resvg::usvg::Options::default();

    let logo_types = [
        EmbeddedLogoType::Windows10Dark,
        EmbeddedLogoType::Windows10Light,
        EmbeddedLogoType::Windows11Dark,
        EmbeddedLogoType::Windows11Light,
    ];

    for logo_type in logo_types {
        match resvg::usvg::Tree::from_data(logo_type.svg_data(), &opt) {
            Ok(tree) => {
                log::info!(
                    "解析内嵌 SVG {:?}: 原始尺寸 {}x{}",
                    logo_type,
                    tree.size().width(),
                    tree.size().height()
                );
                cache.insert(logo_type, tree);
            }
            Err(e) => {
                log::error!("无法解析内嵌 SVG {:?}: {}", logo_type, e);
            }
        }
    }

    cache
}

/// 将 SVG 树渲染为指定物理像素尺寸的 RGBA 数据
fn render_svg_tree_to_pixels(tree: &resvg::usvg::Tree, physical_width: u32, physical_height: u32) -> Option<RenderedSvg> {
    if physical_width == 0 || physical_height == 0 {
        return None;
    }

    // 获取 SVG 原始尺寸
    let svg_size = tree.size();
    let svg_width = svg_size.width();
    let svg_height = svg_size.height();

    if svg_width <= 0.0 || svg_height <= 0.0 {
        log::error!("SVG 尺寸无效: {}x{}", svg_width, svg_height);
        return None;
    }

    // 计算缩放比例
    let scale_x = physical_width as f32 / svg_width;
    let scale_y = physical_height as f32 / svg_height;
    let scale = scale_x.min(scale_y);

    // 计算实际渲染尺寸
    let render_width = ((svg_width * scale).ceil() as u32).max(1);
    let render_height = ((svg_height * scale).ceil() as u32).max(1);

    // 创建像素缓冲区
    let mut pixmap = resvg::tiny_skia::Pixmap::new(render_width, render_height)?;

    // 创建变换矩阵
    let transform = resvg::tiny_skia::Transform::from_scale(scale, scale);

    // 渲染 SVG
    resvg::render(tree, transform, &mut pixmap.as_mut());

    // tiny_skia 使用预乘 alpha 的 RGBA 格式，需要转换为直接 alpha
    let pixels = pixmap.data();
    let mut rgba_pixels = Vec::with_capacity(pixels.len());

    for chunk in pixels.chunks_exact(4) {
        let r = chunk[0];
        let g = chunk[1];
        let b = chunk[2];
        let a = chunk[3];

        // 反预乘 alpha
        if a == 0 {
            rgba_pixels.extend_from_slice(&[0, 0, 0, 0]);
        } else if a == 255 {
            // 完全不透明，无需转换
            rgba_pixels.extend_from_slice(&[r, g, b, a]);
        } else {
            let alpha_f = a as f32 / 255.0;
            let r_unpremul = ((r as f32 / alpha_f).round().min(255.0)) as u8;
            let g_unpremul = ((g as f32 / alpha_f).round().min(255.0)) as u8;
            let b_unpremul = ((b as f32 / alpha_f).round().min(255.0)) as u8;
            rgba_pixels.extend_from_slice(&[r_unpremul, g_unpremul, b_unpremul, a]);
        }
    }

    Some(RenderedSvg {
        pixels: rgba_pixels,
        width: render_width,
        height: render_height,
    })
}

/// 内嵌资源管理器
/// 负责管理和缓存内嵌的 Logo 纹理
pub struct EmbeddedAssets {
    /// 纹理缓存，key 是 (logo类型, 物理像素尺寸)
    texture_cache: HashMap<(EmbeddedLogoType, u32), TextureHandle>,
}

impl Default for EmbeddedAssets {
    fn default() -> Self {
        Self::new()
    }
}

impl EmbeddedAssets {
    /// 创建新的资源管理器
    pub fn new() -> Self {
        // 确保全局 SVG 树缓存已初始化
        SVG_TREE_CACHE.get_or_init(init_svg_tree_cache);

        Self {
            texture_cache: HashMap::new(),
        }
    }

    /// 获取指定类型的 Logo 纹理
    /// 
    /// # 参数
    /// - `ctx`: egui 上下文
    /// - `logo_type`: Logo 类型
    /// - `logical_size`: 逻辑像素尺寸（显示尺寸）
    /// 
    /// # 重要
    /// 此函数会根据 `ctx.pixels_per_point()` 计算实际需要的物理像素尺寸，
    /// 确保在高 DPI 显示器上也能清晰显示。
    pub fn get_logo_texture(
        &mut self,
        ctx: &Context,
        logo_type: EmbeddedLogoType,
        logical_size: u32,
    ) -> Option<TextureHandle> {
        // 关键：计算物理像素尺寸 = 逻辑尺寸 * pixels_per_point
        let pixels_per_point = ctx.pixels_per_point();
        let physical_size = ((logical_size as f32 * pixels_per_point).ceil() as u32).max(1);
        
        // 规范化物理尺寸，避免为每个像素创建新纹理
        let normalized_physical_size = normalize_physical_size(physical_size);
        let cache_key = (logo_type, normalized_physical_size);

        // 检查缓存
        if let Some(texture) = self.texture_cache.get(&cache_key) {
            return Some(texture.clone());
        }

        // 从全局 SVG 树缓存获取解析后的树
        let tree_cache = SVG_TREE_CACHE.get()?;
        let tree = tree_cache.get(&logo_type)?;

        // 按需渲染为物理像素尺寸
        let rendered = render_svg_tree_to_pixels(tree, normalized_physical_size, normalized_physical_size)?;

        log::debug!(
            "渲染 {:?}: 逻辑{}px -> 物理{}px (ppp={:.2}), 实际渲染 {}x{}",
            logo_type,
            logical_size,
            physical_size,
            pixels_per_point,
            rendered.width,
            rendered.height
        );

        // 创建 egui 颜色图像
        let color_image = ColorImage::from_rgba_unmultiplied(
            [rendered.width as usize, rendered.height as usize],
            &rendered.pixels,
        );

        // 创建纹理，使用线性采样以获得平滑的缩放效果
        let texture_name = format!("{}_{}", logo_type.cache_key(), normalized_physical_size);
        let texture = ctx.load_texture(
            texture_name,
            color_image,
            TextureOptions::LINEAR,
        );

        // 缓存纹理
        self.texture_cache.insert(cache_key, texture.clone());

        Some(texture)
    }

    /// 根据配置字符串获取 Logo 纹理
    /// 如果字符串不是内嵌 logo 标识符，返回 None
    pub fn get_logo_by_config_string(
        &mut self,
        ctx: &Context,
        config_string: &str,
        is_dark_mode: bool,
        logical_size: u32,
    ) -> Option<TextureHandle> {
        let logo_type = EmbeddedLogoType::from_config_string(config_string, is_dark_mode)?;
        self.get_logo_texture(ctx, logo_type, logical_size)
    }

    /// 清除纹理缓存
    /// 通常在主题切换或 DPI 变化时调用
    pub fn clear_cache(&mut self) {
        self.texture_cache.clear();
    }
}

/// 规范化物理像素尺寸到预定义的级别
/// 避免为每个像素创建新纹理
fn normalize_physical_size(size: u32) -> u32 {
    // 对于高 DPI，使用更细粒度的尺寸级别
    if size <= 64 {
        64
    } else if size <= 96 {
        96
    } else if size <= 128 {
        128
    } else if size <= 144 {
        144
    } else if size <= 192 {
        192
    } else if size <= 256 {
        256
    } else if size <= 384 {
        384
    } else {
        512
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedded_logo_type_from_config_string() {
        assert_eq!(
            EmbeddedLogoType::from_config_string("LOGO_WINDOWS10", true),
            Some(EmbeddedLogoType::Windows10Dark)
        );
        assert_eq!(
            EmbeddedLogoType::from_config_string("LOGO_WINDOWS10", false),
            Some(EmbeddedLogoType::Windows10Light)
        );
        assert_eq!(
            EmbeddedLogoType::from_config_string("LOGO_WINDOWS11", true),
            Some(EmbeddedLogoType::Windows11Dark)
        );
        assert_eq!(
            EmbeddedLogoType::from_config_string("LOGO_WINDOWS11", false),
            Some(EmbeddedLogoType::Windows11Light)
        );
        assert_eq!(
            EmbeddedLogoType::from_config_string("https://example.com/logo.png", true),
            None
        );
    }

    #[test]
    fn test_is_embedded_logo_identifier() {
        assert!(EmbeddedLogoType::is_embedded_logo_identifier("LOGO_WINDOWS10"));
        assert!(EmbeddedLogoType::is_embedded_logo_identifier("LOGO_WINDOWS11"));
        assert!(!EmbeddedLogoType::is_embedded_logo_identifier("https://example.com/logo.png"));
        assert!(!EmbeddedLogoType::is_embedded_logo_identifier(""));
    }

    #[test]
    fn test_normalize_physical_size() {
        // 测试各种输入尺寸
        assert_eq!(normalize_physical_size(50), 64);
        assert_eq!(normalize_physical_size(72), 96);  // 72 * 1.25 = 90 -> 96
        assert_eq!(normalize_physical_size(108), 128); // 72 * 1.5 = 108 -> 128
        assert_eq!(normalize_physical_size(144), 144); // 72 * 2.0 = 144
        assert_eq!(normalize_physical_size(200), 256);
        assert_eq!(normalize_physical_size(300), 384);
        assert_eq!(normalize_physical_size(400), 512);
    }
}
