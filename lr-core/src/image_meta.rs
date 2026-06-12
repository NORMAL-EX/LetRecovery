//! 镜像元数据类型与 WIM XML 解析（两端共享）。
//!
//! 从原 `core/wimgapi.rs` 抽取的纯逻辑部分（不依赖任何 DLL），用于解析
//! WIM/ESD 的 XML 元数据并推断镜像类型。

/// 压缩类型常量（与 wimlib/wimgapi 取值一致：NONE=0 / XPRESS=1 / LZX=2 / LZMS=3）
pub const WIM_COMPRESS_NONE: u32 = 0;
pub const WIM_COMPRESS_XPRESS: u32 = 1;
pub const WIM_COMPRESS_LZX: u32 = 2;
pub const WIM_COMPRESS_LZMS: u32 = 3;

/// WIM 镜像类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WimImageType {
    /// 标准 Windows 安装镜像（有完整元数据）
    StandardInstall,
    /// 整盘备份型 WIM（直接包含 Windows 目录）
    FullBackup,
    /// PE 环境镜像
    WindowsPE,
    /// 未知类型
    Unknown,
}

impl std::fmt::Display for WimImageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WimImageType::StandardInstall => write!(f, "标准安装镜像"),
            WimImageType::FullBackup => write!(f, "整盘备份镜像"),
            WimImageType::WindowsPE => write!(f, "PE环境镜像"),
            WimImageType::Unknown => write!(f, "未知类型"),
        }
    }
}

/// 镜像信息
#[derive(Debug, Clone)]
pub struct ImageInfo {
    /// 镜像索引
    pub index: u32,
    /// 镜像名称
    pub name: String,
    /// 镜像大小（字节）
    pub size_bytes: u64,
    /// 安装类型（如 "Client" / "WindowsPE" / "Server"）
    pub installation_type: String,
    /// 镜像描述
    pub description: String,
    /// Windows 主版本号
    pub major_version: Option<u16>,
    /// Windows 次版本号
    pub minor_version: Option<u16>,
    /// 镜像类型
    pub image_type: WimImageType,
    /// 是否已验证可安装
    pub verified_installable: bool,
}

/// 操作进度
#[derive(Debug, Clone)]
pub struct WimProgress {
    /// 进度百分比 (0-100)
    pub percentage: u8,
    /// 状态描述
    pub status: String,
}

/// 解析 WIM/ESD 的 XML 元数据，返回镜像信息列表。
///
/// 优先用 roxmltree 做完整 XML 解析；若解析失败或没解析出镜像，回退到
/// 旧的字符串扫描解析（兜底，保证遇到非常规/截断 XML 仍能尽力提取）。
pub fn parse_image_info_from_xml(xml: &str) -> Vec<ImageInfo> {
    let mut images = parse_image_info_roxml(xml).unwrap_or_default();

    // roxmltree 没解析出内容时回退到字符串扫描
    if images.is_empty() {
        images = parse_image_info_fallback(xml);
    }

    // 对解析结果进行后处理，确定镜像类型
    for img in &mut images {
        img.image_type = determine_image_type(img);
    }

    images
}

/// 用 roxmltree 完整解析 WIM XML 的 `<IMAGE>` 块。
fn parse_image_info_roxml(xml: &str) -> Option<Vec<ImageInfo>> {
    let trimmed = xml.trim_start_matches('\u{feff}');
    let doc = roxmltree::Document::parse(trimmed).ok()?;

    let mut images = Vec::new();
    for image in doc
        .descendants()
        .filter(|n| n.is_element() && n.has_tag_name("IMAGE"))
    {
        let index: u32 = image
            .attribute("INDEX")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        if index == 0 {
            continue;
        }

        let size_bytes = node_text(image, "TOTALBYTES")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let installation_type = node_text(image, "INSTALLATIONTYPE").unwrap_or_default();
        let description = node_text(image, "DESCRIPTION").unwrap_or_default();
        let major_version = node_text(image, "MAJOR").and_then(|s| s.parse::<u16>().ok());
        let minor_version = node_text(image, "MINOR").and_then(|s| s.parse::<u16>().ok());
        let name = build_image_name_node(image, &description, index);

        images.push(ImageInfo {
            index,
            name,
            size_bytes,
            installation_type,
            description,
            major_version,
            minor_version,
            image_type: WimImageType::Unknown,
            verified_installable: false,
        });
    }

    if images.is_empty() {
        None
    } else {
        Some(images)
    }
}

/// 在某节点的所有后代里查找第一个指定标签元素的文本（去空白、过滤空串）。
fn node_text(node: roxmltree::Node, tag: &str) -> Option<String> {
    node.descendants()
        .find(|n| n.is_element() && n.has_tag_name(tag))
        .and_then(|n| n.text())
        .map(|t| t.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// roxmltree 版的镜像名称构建（DISPLAYNAME > NAME > WINDOWS(PRODUCTNAME+EDITIONID) > DESCRIPTION+FLAGS > ...）。
fn build_image_name_node(image: roxmltree::Node, description: &str, index: u32) -> String {
    if let Some(display_name) = node_text(image, "DISPLAYNAME") {
        return display_name;
    }
    if let Some(name) = node_text(image, "NAME") {
        return name;
    }

    if let Some(windows) = image
        .descendants()
        .find(|n| n.is_element() && n.has_tag_name("WINDOWS"))
    {
        let product_name = node_text(windows, "PRODUCTNAME");
        let edition_id = node_text(windows, "EDITIONID");
        match (product_name, edition_id) {
            (Some(prod), Some(ed)) => {
                if prod.to_lowercase().contains(&ed.to_lowercase()) {
                    return prod;
                }
                return format!("{} {}", prod, ed);
            }
            (Some(prod), _) => return prod,
            (_, Some(ed)) => return format!("Windows {}", ed),
            _ => {}
        }
    }

    let flags = node_text(image, "FLAGS").unwrap_or_default();
    if !description.is_empty() && !flags.is_empty() {
        if description.to_lowercase().contains(&flags.to_lowercase()) {
            return description.to_string();
        }
        return format!("{} {}", description, flags);
    }
    if !description.is_empty() {
        return description.to_string();
    }
    if !flags.is_empty() {
        return format!("Windows {}", flags);
    }

    format!("镜像 {}", index)
}

/// 智能构建镜像名称（DISPLAYNAME > NAME > PRODUCTNAME+EDITIONID > DESCRIPTION+FLAGS > ...）
fn build_image_name(image_block: &str, description: &str, index: u32) -> String {
    if let Some(display_name) = extract_xml_tag(image_block, "DISPLAYNAME") {
        if !display_name.is_empty() {
            return display_name;
        }
    }

    if let Some(name) = extract_xml_tag(image_block, "NAME") {
        if !name.is_empty() {
            return name;
        }
    }

    if let Some(windows_block) = extract_xml_tag(image_block, "WINDOWS") {
        let product_name = extract_xml_tag(&windows_block, "PRODUCTNAME");
        let edition_id = extract_xml_tag(&windows_block, "EDITIONID");

        match (product_name, edition_id) {
            (Some(prod), Some(ed)) if !prod.is_empty() && !ed.is_empty() => {
                if prod.to_lowercase().contains(&ed.to_lowercase()) {
                    return prod;
                }
                return format!("{} {}", prod, ed);
            }
            (Some(prod), _) if !prod.is_empty() => return prod,
            (_, Some(ed)) if !ed.is_empty() => return format!("Windows {}", ed),
            _ => {}
        }
    }

    let flags = extract_xml_tag(image_block, "FLAGS").unwrap_or_default();

    if !description.is_empty() && !flags.is_empty() {
        if description.to_lowercase().contains(&flags.to_lowercase()) {
            return description.to_string();
        }
        return format!("{} {}", description, flags);
    }

    if !description.is_empty() {
        return description.to_string();
    }

    if !flags.is_empty() {
        return format!("Windows {}", flags);
    }

    format!("镜像 {}", index)
}

fn extract_version_number(image_block: &str, tag: &str) -> Option<u16> {
    extract_xml_tag(image_block, "VERSION")
        .and_then(|version_block| extract_xml_tag(&version_block, tag))
        .or_else(|| {
            extract_xml_tag(image_block, "WINDOWS")
                .and_then(|win_block| extract_xml_tag(&win_block, "VERSION"))
                .and_then(|ver_block| extract_xml_tag(&ver_block, tag))
        })
        .or_else(|| extract_xml_tag(image_block, tag))
        .and_then(|s| s.parse::<u16>().ok())
}

fn parse_image_info_fallback(xml: &str) -> Vec<ImageInfo> {
    let mut images = Vec::new();

    let image_count = xml.matches("<IMAGE ").count();
    if image_count == 0 {
        return images;
    }

    let mut backup_pos = 0;
    let mut backup_index = 1u32;

    while let Some(img_start) = xml[backup_pos..].find("<IMAGE ") {
        let abs_img_start = backup_pos + img_start;

        let block_end = xml[abs_img_start..]
            .find("</IMAGE>")
            .map(|e| abs_img_start + e + 8)
            .or_else(|| {
                xml[abs_img_start + 7..]
                    .find("<IMAGE ")
                    .map(|e| abs_img_start + 7 + e)
                    .or_else(|| xml[abs_img_start..].find("</WIM>").map(|e| abs_img_start + e))
            })
            .unwrap_or(xml.len());

        let image_block = &xml[abs_img_start..block_end];

        let parsed_index = extract_index_from_attributes(image_block).unwrap_or(backup_index);

        let size_bytes = extract_xml_tag(image_block, "TOTALBYTES")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let installation_type =
            extract_xml_tag(image_block, "INSTALLATIONTYPE").unwrap_or_default();
        let description = extract_xml_tag(image_block, "DESCRIPTION").unwrap_or_default();
        let major_version = extract_version_number(image_block, "MAJOR");
        let minor_version = extract_version_number(image_block, "MINOR");
        let name = build_image_name(image_block, &description, parsed_index);

        images.push(ImageInfo {
            index: parsed_index,
            name,
            size_bytes,
            installation_type,
            description,
            major_version,
            minor_version,
            image_type: WimImageType::Unknown,
            verified_installable: false,
        });

        backup_index += 1;
        backup_pos = block_end;
    }

    images
}

fn extract_index_from_attributes(image_block: &str) -> Option<u32> {
    if let Some(idx_pos) = image_block.find("INDEX=\"") {
        let idx_start = idx_pos + 7;
        if let Some(idx_end) = image_block[idx_start..].find('"') {
            return image_block[idx_start..idx_start + idx_end].parse().ok();
        }
    }

    if let Some(idx_pos) = image_block.find("INDEX='") {
        let idx_start = idx_pos + 7;
        if let Some(idx_end) = image_block[idx_start..].find('\'') {
            return image_block[idx_start..idx_start + idx_end].parse().ok();
        }
    }

    if let Some(idx_pos) = image_block.find("INDEX=") {
        let idx_start = idx_pos + 6;
        let idx_str: String = image_block[idx_start..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if !idx_str.is_empty() {
            return idx_str.parse().ok();
        }
    }

    None
}

/// 根据镜像信息推断镜像类型
pub fn determine_image_type(info: &ImageInfo) -> WimImageType {
    let name_lower = info.name.to_lowercase();
    let install_type_lower = info.installation_type.to_lowercase();

    if install_type_lower == "windowspe"
        || name_lower.contains("windows pe")
        || name_lower.contains("winpe")
        || name_lower.contains("windows setup")
    {
        return WimImageType::WindowsPE;
    }

    if !info.installation_type.is_empty()
        && info.major_version.is_some()
        && (install_type_lower == "client" || install_type_lower == "server")
    {
        return WimImageType::StandardInstall;
    }

    if info.installation_type.is_empty() && info.size_bytes > 1_000_000_000 {
        return WimImageType::FullBackup;
    }

    if name_lower.contains("backup")
        || name_lower.contains("备份")
        || name_lower.contains("ghost")
        || name_lower.contains("clone")
    {
        return WimImageType::FullBackup;
    }

    if info.major_version.is_some() && info.installation_type.is_empty() {
        return WimImageType::FullBackup;
    }

    WimImageType::Unknown
}

fn extract_xml_tag(xml: &str, tag: &str) -> Option<String> {
    let open_tag = format!("<{}>", tag);
    let close_tag = format!("</{}>", tag);

    if let Some(start) = xml.find(&open_tag) {
        let content_start = start + open_tag.len();
        if let Some(end) = xml[content_start..].find(&close_tag) {
            let content = &xml[content_start..content_start + end];
            return Some(content.trim().to_string());
        }
    }
    None
}
