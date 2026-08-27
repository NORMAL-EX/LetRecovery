//! 系统工具函数模块
//!
//! 提供各种系统级别的工具函数，包括：
//! - 系统架构检测

use std::path::Path;

/// 获取文件的版本信息
///
/// 返回 (major, minor, build, revision) 元组
///
/// # 参数
/// - `path`: 文件路径
pub fn get_file_version(path: &Path) -> Option<(u32, u32, u32, u32)> {
    lr_core::windows_file_version::query_file_version(path)
        .ok()
        .map(|version| {
            (
                u32::from(version.major),
                u32::from(version.minor),
                u32::from(version.build),
                u32::from(version.revision),
            )
        })
        .or_else(|| get_file_version_from_pe(path))
}

/// 从 PE 文件资源段直接读取版本信息
fn get_file_version_from_pe(path: &Path) -> Option<(u32, u32, u32, u32)> {
    let data = std::fs::read(path).ok()?;

    // 验证 DOS 头
    if data.len() < 0x40 || &data[0..2] != b"MZ" {
        return None;
    }

    // 获取 PE 头偏移
    let pe_offset = u32::from_le_bytes([data[0x3C], data[0x3D], data[0x3E], data[0x3F]]) as usize;

    if data.len() < pe_offset + 4 || &data[pe_offset..pe_offset + 4] != b"PE\0\0" {
        return None;
    }

    // COFF 文件头
    let coff_header_offset = pe_offset + 4;
    if data.len() < coff_header_offset + 20 {
        return None;
    }

    let num_sections =
        u16::from_le_bytes([data[coff_header_offset + 2], data[coff_header_offset + 3]]) as usize;
    let optional_header_size =
        u16::from_le_bytes([data[coff_header_offset + 16], data[coff_header_offset + 17]]) as usize;

    // 可选头
    let optional_header_offset = coff_header_offset + 20;
    if data.len() < optional_header_offset + optional_header_size {
        return None;
    }

    // 判断是 PE32 还是 PE32+
    let magic = u16::from_le_bytes([
        data[optional_header_offset],
        data[optional_header_offset + 1],
    ]);
    let (data_dir_offset, num_data_dirs) = match magic {
        0x10b => (optional_header_offset + 96, 16usize), // PE32
        0x20b => (optional_header_offset + 112, 16usize), // PE32+
        _ => return None,
    };

    // 资源目录是数据目录的第3项 (索引2)
    if num_data_dirs < 3 {
        return None;
    }

    let resource_dir_rva_offset = data_dir_offset + 2 * 8;
    if data.len() < resource_dir_rva_offset + 8 {
        return None;
    }

    let resource_rva = u32::from_le_bytes([
        data[resource_dir_rva_offset],
        data[resource_dir_rva_offset + 1],
        data[resource_dir_rva_offset + 2],
        data[resource_dir_rva_offset + 3],
    ]) as usize;

    if resource_rva == 0 {
        return None;
    }

    // 读取节表找到资源节
    let section_table_offset = optional_header_offset + optional_header_size;

    for i in 0..num_sections {
        let section_offset = section_table_offset + i * 40;
        if data.len() < section_offset + 40 {
            continue;
        }

        let virtual_address = u32::from_le_bytes([
            data[section_offset + 12],
            data[section_offset + 13],
            data[section_offset + 14],
            data[section_offset + 15],
        ]) as usize;

        let virtual_size = u32::from_le_bytes([
            data[section_offset + 8],
            data[section_offset + 9],
            data[section_offset + 10],
            data[section_offset + 11],
        ]) as usize;

        let raw_data_ptr = u32::from_le_bytes([
            data[section_offset + 20],
            data[section_offset + 21],
            data[section_offset + 22],
            data[section_offset + 23],
        ]) as usize;

        // 检查资源 RVA 是否在这个节内
        if resource_rva >= virtual_address && resource_rva < virtual_address + virtual_size {
            let resource_file_offset = raw_data_ptr + (resource_rva - virtual_address);
            return parse_version_resource(
                &data,
                resource_file_offset,
                raw_data_ptr,
                virtual_address,
            );
        }
    }

    None
}

/// 解析版本资源
fn parse_version_resource(
    data: &[u8],
    resource_offset: usize,
    section_raw: usize,
    section_rva: usize,
) -> Option<(u32, u32, u32, u32)> {
    // 遍历资源目录查找 VS_VERSION_INFO (类型 16)
    if data.len() < resource_offset + 16 {
        return None;
    }

    let num_named_entries =
        u16::from_le_bytes([data[resource_offset + 12], data[resource_offset + 13]]) as usize;
    let num_id_entries =
        u16::from_le_bytes([data[resource_offset + 14], data[resource_offset + 15]]) as usize;

    let entries_offset = resource_offset + 16;

    for i in 0..(num_named_entries + num_id_entries) {
        let entry_offset = entries_offset + i * 8;
        if data.len() < entry_offset + 8 {
            continue;
        }

        let id = u32::from_le_bytes([
            data[entry_offset],
            data[entry_offset + 1],
            data[entry_offset + 2],
            data[entry_offset + 3],
        ]);

        let offset_or_dir = u32::from_le_bytes([
            data[entry_offset + 4],
            data[entry_offset + 5],
            data[entry_offset + 6],
            data[entry_offset + 7],
        ]);

        // RT_VERSION = 16
        if id == 16 && (offset_or_dir & 0x80000000) != 0 {
            let sub_dir_offset =
                resource_offset.wrapping_add((offset_or_dir & 0x7FFFFFFF) as usize);
            if let Some(version) = find_version_in_subdir(
                data,
                sub_dir_offset,
                resource_offset,
                section_raw,
                section_rva,
            ) {
                return Some(version);
            }
        }
    }

    None
}

/// 在子目录中查找版本信息
fn find_version_in_subdir(
    data: &[u8],
    dir_offset: usize,
    resource_base: usize,
    section_raw: usize,
    section_rva: usize,
) -> Option<(u32, u32, u32, u32)> {
    if data.len() < dir_offset + 16 {
        return None;
    }

    let num_named = u16::from_le_bytes([data[dir_offset + 12], data[dir_offset + 13]]) as usize;
    let num_id = u16::from_le_bytes([data[dir_offset + 14], data[dir_offset + 15]]) as usize;

    for i in 0..(num_named + num_id) {
        let entry_offset = dir_offset + 16 + i * 8;
        if data.len() < entry_offset + 8 {
            continue;
        }

        let offset_or_dir = u32::from_le_bytes([
            data[entry_offset + 4],
            data[entry_offset + 5],
            data[entry_offset + 6],
            data[entry_offset + 7],
        ]);

        if (offset_or_dir & 0x80000000) != 0 {
            // 还是目录，继续递归
            let sub_offset = resource_base.wrapping_add((offset_or_dir & 0x7FFFFFFF) as usize);
            if let Some(v) =
                find_version_in_subdir(data, sub_offset, resource_base, section_raw, section_rva)
            {
                return Some(v);
            }
        } else {
            // 数据入口
            let data_entry_offset = resource_base.wrapping_add(offset_or_dir as usize);
            if data.len() < data_entry_offset + 16 {
                continue;
            }

            let data_rva = u32::from_le_bytes([
                data[data_entry_offset],
                data[data_entry_offset + 1],
                data[data_entry_offset + 2],
                data[data_entry_offset + 3],
            ]) as usize;

            let data_size = u32::from_le_bytes([
                data[data_entry_offset + 4],
                data[data_entry_offset + 5],
                data[data_entry_offset + 6],
                data[data_entry_offset + 7],
            ]) as usize;

            // 转换 RVA 到文件偏移
            let data_file_offset = section_raw + (data_rva - section_rva);

            if data.len() >= data_file_offset + data_size && data_size >= 52 {
                // 解析 VS_FIXEDFILEINFO
                // 跳过 VS_VERSION_INFO 头部, 查找 VS_FIXEDFILEINFO 签名 0xFEEF04BD
                for offset in (0..data_size.saturating_sub(52)).step_by(4) {
                    let pos = data_file_offset + offset;
                    if data.len() < pos + 52 {
                        break;
                    }

                    let signature = u32::from_le_bytes([
                        data[pos],
                        data[pos + 1],
                        data[pos + 2],
                        data[pos + 3],
                    ]);

                    if signature == 0xFEEF04BD {
                        // 找到 VS_FIXEDFILEINFO
                        let file_version_ms = u32::from_le_bytes([
                            data[pos + 8],
                            data[pos + 9],
                            data[pos + 10],
                            data[pos + 11],
                        ]);
                        let file_version_ls = u32::from_le_bytes([
                            data[pos + 12],
                            data[pos + 13],
                            data[pos + 14],
                            data[pos + 15],
                        ]);

                        let major = (file_version_ms >> 16) & 0xFFFF;
                        let minor = file_version_ms & 0xFFFF;
                        let build = (file_version_ls >> 16) & 0xFFFF;
                        let revision = file_version_ls & 0xFFFF;

                        return Some((major, minor, build, revision));
                    }
                }
            }
        }
    }

    None
}

// =============================================================================
// 系统架构
// =============================================================================

/// 系统架构
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemArchitecture {
    X86,
    X64,
    Arm64,
    Unknown,
}

impl SystemArchitecture {
    /// 获取处理器架构字符串 (用于 unattend.xml)
    pub fn processor_architecture(&self) -> &'static str {
        match self {
            SystemArchitecture::X86 => "x86",
            SystemArchitecture::X64 => "amd64",
            SystemArchitecture::Arm64 => "arm64",
            SystemArchitecture::Unknown => "amd64",
        }
    }

    /// 获取用于 unattend.xml 的架构字符串
    /// 这是 processor_architecture 的别名，提供更明确的命名
    #[inline]
    pub fn as_unattend_str(&self) -> &'static str {
        self.processor_architecture()
    }
}

/// 检测离线系统的架构
pub fn get_offline_system_architecture(system_root: &Path) -> SystemArchitecture {
    // 检查 System32 目录下的 kernel32.dll 是 32 位还是 64 位
    let kernel32_path = system_root
        .join("Windows")
        .join("System32")
        .join("kernel32.dll");

    if let Ok(data) = std::fs::read(&kernel32_path) {
        // PE 文件头检测
        if data.len() > 0x40 {
            // DOS 头的 e_lfanew 字段在偏移 0x3C
            let pe_offset =
                u32::from_le_bytes([data[0x3C], data[0x3D], data[0x3E], data[0x3F]]) as usize;

            if data.len() > pe_offset + 6 {
                // PE 签名后的 Machine 字段
                let machine = u16::from_le_bytes([data[pe_offset + 4], data[pe_offset + 5]]);

                return match machine {
                    0x014c => SystemArchitecture::X86,   // IMAGE_FILE_MACHINE_I386
                    0x8664 => SystemArchitecture::X64,   // IMAGE_FILE_MACHINE_AMD64
                    0xAA64 => SystemArchitecture::Arm64, // IMAGE_FILE_MACHINE_ARM64
                    _ => SystemArchitecture::Unknown,
                };
            }
        }
    }

    // 如果无法检测，检查是否存在 SysWOW64 目录
    if system_root.join("Windows").join("SysWOW64").exists() {
        SystemArchitecture::X64
    } else {
        SystemArchitecture::X86
    }
}
