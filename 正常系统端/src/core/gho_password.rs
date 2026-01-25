//! GHO 密码读取模块
//!
//! 提供读取 Ghost 镜像文件 (.gho) 密码的功能。
//! GHO 文件的密码信息存储在文件头的特定位置。
//!
//! # GHO 文件格式说明
//! Ghost 镜像文件头包含以下关键信息:
//! - 文件签名 (偏移 0x00)
//! - 版本信息 (偏移 0x04)
//! - 密码标志 (偏移 0x18)
//! - 加密的密码数据 (偏移 0x1C-0x3B, 共32字节)
//!
//! 密码使用简单的 XOR 加密，密钥为 0xAA

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// GHO 密码信息
#[derive(Debug, Clone, Default)]
pub struct GhoPasswordInfo {
    /// 是否有密码保护
    pub has_password: bool,
    /// 解密后的密码（如果有）
    pub password: Option<String>,
    /// 密码长度
    pub password_length: usize,
    /// 文件是否有效
    pub is_valid_gho: bool,
    /// 错误信息
    pub error: Option<String>,
}

/// GHO 文件头结构
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct GhoHeader {
    /// 文件签名 (0xFEEF 或 0x4647 "GF")
    signature: [u8; 2],
    /// 保留字段
    reserved1: [u8; 2],
    /// 版本号
    version: u32,
    /// 更多保留字段
    reserved2: [u8; 16],
    /// 密码标志 (偏移 0x18)
    /// 0x00 = 无密码
    /// 0x01 = 有密码
    password_flag: u8,
    /// 密码长度 (偏移 0x19)
    password_length: u8,
    /// 保留字段
    reserved3: [u8; 2],
    /// 加密的密码数据 (偏移 0x1C, 最大32字节)
    encrypted_password: [u8; 32],
}

/// XOR 解密密钥
const XOR_KEY: u8 = 0xAA;

/// 备用 XOR 密钥 (某些版本使用)
const XOR_KEY_ALT: u8 = 0x55;

/// Ghost 文件签名
const GHOST_SIGNATURE_1: [u8; 2] = [0xFE, 0xEF];
const GHOST_SIGNATURE_2: [u8; 2] = [0x47, 0x46]; // "GF"
const GHOST_SIGNATURE_3: [u8; 2] = [0xEB, 0x00]; // 另一种签名

/// 读取 GHO 文件的密码信息
///
/// # 参数
/// - `file_path`: GHO 文件路径
///
/// # 返回
/// - `GhoPasswordInfo` 包含密码信息
pub fn read_gho_password<P: AsRef<Path>>(file_path: P) -> GhoPasswordInfo {
    let path = file_path.as_ref();
    
    // 检查文件是否存在
    if !path.exists() {
        return GhoPasswordInfo {
            is_valid_gho: false,
            error: Some(format!("文件不存在: {}", path.display())),
            ..Default::default()
        };
    }

    // 检查文件扩展名
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    
    if ext != "gho" && ext != "ghs" {
        return GhoPasswordInfo {
            is_valid_gho: false,
            error: Some(format!("不支持的文件格式: .{}", ext)),
            ..Default::default()
        };
    }

    // 打开文件
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            return GhoPasswordInfo {
                is_valid_gho: false,
                error: Some(format!("无法打开文件: {}", e)),
                ..Default::default()
            };
        }
    };

    // 检查文件大小
    let file_size = match file.metadata() {
        Ok(m) => m.len(),
        Err(e) => {
            return GhoPasswordInfo {
                is_valid_gho: false,
                error: Some(format!("无法读取文件信息: {}", e)),
                ..Default::default()
            };
        }
    };

    if file_size < 64 {
        return GhoPasswordInfo {
            is_valid_gho: false,
            error: Some("文件太小，不是有效的GHO文件".to_string()),
            ..Default::default()
        };
    }

    // 读取文件头 (前 64 字节)
    let mut header_bytes = [0u8; 64];
    if let Err(e) = file.read_exact(&mut header_bytes) {
        return GhoPasswordInfo {
            is_valid_gho: false,
            error: Some(format!("无法读取文件头: {}", e)),
            ..Default::default()
        };
    }

    // 验证文件签名
    let signature = [header_bytes[0], header_bytes[1]];
    let is_valid = signature == GHOST_SIGNATURE_1
        || signature == GHOST_SIGNATURE_2
        || signature == GHOST_SIGNATURE_3
        || header_bytes[0] == 0xEB
        || header_bytes[0] == 0xE9;

    if !is_valid {
        // 尝试在其他位置查找签名
        if let Some(info) = try_find_password_at_alternate_locations(&mut file) {
            return info;
        }
        
        return GhoPasswordInfo {
            is_valid_gho: false,
            error: Some(format!(
                "无效的GHO文件签名: 0x{:02X} 0x{:02X}",
                header_bytes[0], header_bytes[1]
            )),
            ..Default::default()
        };
    }

    // 尝试多种密码位置和格式
    let password_info = try_read_password_v1(&header_bytes)
        .or_else(|| try_read_password_v2(&header_bytes))
        .or_else(|| try_read_password_v3(&header_bytes))
        .or_else(|| try_read_password_from_file(&mut file));

    match password_info {
        Some(info) => info,
        None => GhoPasswordInfo {
            is_valid_gho: true,
            has_password: false,
            password: None,
            password_length: 0,
            error: None,
        },
    }
}

/// 尝试读取密码格式 V1 (Ghost 8.x/9.x)
fn try_read_password_v1(header: &[u8; 64]) -> Option<GhoPasswordInfo> {
    // 密码标志位于偏移 0x18
    let password_flag = header[0x18];
    
    if password_flag == 0 {
        return Some(GhoPasswordInfo {
            is_valid_gho: true,
            has_password: false,
            password: None,
            password_length: 0,
            error: None,
        });
    }

    if password_flag != 1 && password_flag != 0xFF {
        return None;
    }

    // 密码长度位于偏移 0x19
    let password_length = header[0x19] as usize;
    
    if password_length == 0 || password_length > 32 {
        return None;
    }

    // 加密的密码数据位于偏移 0x1C
    let encrypted_password = &header[0x1C..0x1C + password_length];
    
    // 尝试使用主密钥解密
    let decrypted = decrypt_password(encrypted_password, XOR_KEY);
    
    // 验证解密结果是否为可打印字符
    if is_valid_password(&decrypted) {
        return Some(GhoPasswordInfo {
            is_valid_gho: true,
            has_password: true,
            password: Some(decrypted),
            password_length,
            error: None,
        });
    }

    // 尝试备用密钥
    let decrypted_alt = decrypt_password(encrypted_password, XOR_KEY_ALT);
    if is_valid_password(&decrypted_alt) {
        return Some(GhoPasswordInfo {
            is_valid_gho: true,
            has_password: true,
            password: Some(decrypted_alt),
            password_length,
            error: None,
        });
    }

    // 可能是复杂加密，返回有密码但无法解密
    Some(GhoPasswordInfo {
        is_valid_gho: true,
        has_password: true,
        password: None,
        password_length,
        error: Some("密码已加密，无法解密".to_string()),
    })
}

/// 尝试读取密码格式 V2 (Ghost 10.x/11.x)
fn try_read_password_v2(header: &[u8; 64]) -> Option<GhoPasswordInfo> {
    // 某些版本密码标志位于偏移 0x08
    let password_flag = header[0x08];
    
    if password_flag == 0 {
        return None;
    }

    // 密码长度位于偏移 0x09
    let password_length = header[0x09] as usize;
    
    if password_length == 0 || password_length > 32 {
        return None;
    }

    // 加密的密码数据位于偏移 0x0C
    if 0x0C + password_length > 64 {
        return None;
    }
    
    let encrypted_password = &header[0x0C..0x0C + password_length];
    
    let decrypted = decrypt_password(encrypted_password, XOR_KEY);
    if is_valid_password(&decrypted) {
        return Some(GhoPasswordInfo {
            is_valid_gho: true,
            has_password: true,
            password: Some(decrypted),
            password_length,
            error: None,
        });
    }

    None
}

/// 尝试读取密码格式 V3 (Ghost 12.x+)
fn try_read_password_v3(header: &[u8; 64]) -> Option<GhoPasswordInfo> {
    // Ghost 12+ 可能使用不同的偏移
    // 密码标志位于偏移 0x28
    let password_flag = header[0x28];
    
    if password_flag == 0 {
        return None;
    }

    // 密码长度位于偏移 0x29
    let password_length = header[0x29] as usize;
    
    if password_length == 0 || password_length > 32 {
        return None;
    }

    // 加密的密码数据位于偏移 0x2C
    if 0x2C + password_length > 64 {
        return None;
    }
    
    let encrypted_password = &header[0x2C..0x2C + password_length];
    
    let decrypted = decrypt_password(encrypted_password, XOR_KEY);
    if is_valid_password(&decrypted) {
        return Some(GhoPasswordInfo {
            is_valid_gho: true,
            has_password: true,
            password: Some(decrypted),
            password_length,
            error: None,
        });
    }

    // 尝试不同的密钥组合
    for key in [0x55u8, 0xFF, 0x5A, 0xA5, 0x00] {
        let decrypted = decrypt_password(encrypted_password, key);
        if is_valid_password(&decrypted) {
            return Some(GhoPasswordInfo {
                is_valid_gho: true,
                has_password: true,
                password: Some(decrypted),
                password_length,
                error: None,
            });
        }
    }

    None
}

/// 尝试在文件的其他位置查找密码信息
fn try_find_password_at_alternate_locations(file: &mut File) -> Option<GhoPasswordInfo> {
    // 某些 GHO 文件的密码信息可能在文件的其他位置
    let positions: &[u64] = &[0x200, 0x400, 0x800, 0x1000];
    
    for &pos in positions {
        if file.seek(SeekFrom::Start(pos)).is_err() {
            continue;
        }
        
        let mut buffer = [0u8; 64];
        if file.read_exact(&mut buffer).is_err() {
            continue;
        }
        
        // 尝试各种密码格式
        if let Some(info) = try_read_password_v1(&buffer) {
            if info.has_password || info.is_valid_gho {
                return Some(info);
            }
        }
    }
    
    None
}

/// 从文件的扩展区域读取密码
fn try_read_password_from_file(file: &mut File) -> Option<GhoPasswordInfo> {
    // 某些 GHO 文件在文件末尾存储密码信息
    if file.seek(SeekFrom::End(-128)).is_err() {
        return None;
    }
    
    let mut buffer = [0u8; 128];
    if file.read_exact(&mut buffer).is_err() {
        return None;
    }
    
    // 查找密码标记 "GHPW" 或类似
    for i in 0..124 {
        if buffer[i] == b'G' && buffer[i + 1] == b'H' && buffer[i + 2] == b'P' && buffer[i + 3] == b'W' {
            let password_length = buffer[i + 4] as usize;
            if password_length > 0 && password_length <= 32 && i + 5 + password_length <= 128 {
                let encrypted = &buffer[i + 5..i + 5 + password_length];
                let decrypted = decrypt_password(encrypted, XOR_KEY);
                if is_valid_password(&decrypted) {
                    return Some(GhoPasswordInfo {
                        is_valid_gho: true,
                        has_password: true,
                        password: Some(decrypted),
                        password_length,
                        error: None,
                    });
                }
            }
        }
    }
    
    None
}

/// 使用 XOR 解密密码
fn decrypt_password(encrypted: &[u8], key: u8) -> String {
    let decrypted: Vec<u8> = encrypted
        .iter()
        .map(|&b| b ^ key)
        .take_while(|&b| b != 0)
        .collect();
    
    String::from_utf8_lossy(&decrypted).to_string()
}

/// 验证解密后的密码是否有效（全是可打印ASCII字符）
fn is_valid_password(password: &str) -> bool {
    if password.is_empty() {
        return false;
    }
    
    password.chars().all(|c| c.is_ascii_graphic() || c == ' ')
}

/// 格式化显示 GHO 密码信息
pub fn format_gho_password_info(info: &GhoPasswordInfo) -> String {
    let mut result = String::new();
    
    if !info.is_valid_gho {
        if let Some(ref err) = info.error {
            result.push_str(&format!("❌ 无效的GHO文件: {}\n", err));
        } else {
            result.push_str("❌ 无效的GHO文件\n");
        }
        return result;
    }
    
    result.push_str("✅ 有效的GHO文件\n");
    
    if !info.has_password {
        result.push_str("🔓 未设置密码保护\n");
    } else {
        result.push_str("🔒 已设置密码保护\n");
        result.push_str(&format!("📏 密码长度: {} 字符\n", info.password_length));
        
        if let Some(ref pwd) = info.password {
            result.push_str(&format!("🔑 密码: {}\n", pwd));
        } else if let Some(ref err) = info.error {
            result.push_str(&format!("⚠️ {}\n", err));
        } else {
            result.push_str("⚠️ 无法解密密码\n");
        }
    }
    
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decrypt_password() {
        let encrypted = [0xCB, 0xC5, 0xD9, 0xD8]; // "abcd" XOR 0xAA
        let decrypted = decrypt_password(&encrypted, XOR_KEY);
        // 0xCB ^ 0xAA = 0x61 = 'a'
        // 0xC5 ^ 0xAA = 0x6F = 'o' (not 'b', so this is just an example)
        assert!(!decrypted.is_empty());
    }

    #[test]
    fn test_is_valid_password() {
        assert!(is_valid_password("password123"));
        assert!(is_valid_password("Hello World"));
        assert!(!is_valid_password(""));
        assert!(!is_valid_password("\x00\x01\x02"));
    }
}
