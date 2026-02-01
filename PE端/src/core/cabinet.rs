//! Windows Cabinet (.cab) 文件解压模块
//!
//! 使用 Windows SetupAPI (setupapi.dll) 的 SetupIterateCabinet 函数实现 .cab 文件解压。
//! 主要用于解压 Windows 更新包（如 KB2990941、KB3087873 等 NVMe 驱动补丁）。

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use libloading::Library;

// ============================================================================
// Windows API 类型定义
// ============================================================================

/// BOOL 类型
#[repr(transparent)]
#[derive(Clone, Copy, Default)]
struct BOOL(pub i32);

/// UINT 类型
type UINT = u32;

/// PSP_FILE_CALLBACK_W 回调函数类型
/// 
/// 参数:
/// - Context: 用户定义的上下文
/// - Notification: 通知类型
/// - Param1: 通知参数1
/// - Param2: 通知参数2
type SpFileCallbackW = unsafe extern "system" fn(
    Context: *mut c_void,
    Notification: UINT,
    Param1: usize,
    Param2: usize,
) -> UINT;

/// SetupIterateCabinetW 函数类型
type FnSetupIterateCabinetW = unsafe extern "system" fn(
    CabinetFile: *const u16,
    Reserved: u32,
    MsgHandler: SpFileCallbackW,
    Context: *mut c_void,
) -> BOOL;

// SetupAPI 通知常量
const SPFILENOTIFY_FILEINCABINET: UINT = 0x00000011;
const SPFILENOTIFY_FILEEXTRACTED: UINT = 0x00000013;
const SPFILENOTIFY_NEEDNEWCABINET: UINT = 0x00000014;

// 回调返回值
const FILEOP_DOIT: UINT = 1;
const FILEOP_SKIP: UINT = 2;
const FILEOP_ABORT: UINT = 0;

/// FILE_IN_CABINET_INFO_W 结构
#[repr(C)]
struct FileInCabinetInfoW {
    NameInCabinet: *const u16,
    FileSize: u32,
    Win32Error: u32,
    DosDate: u16,
    DosTime: u16,
    DosAttribs: u16,
    FullTargetName: [u16; 260],
}

/// FILEPATHS_W 结构
#[repr(C)]
struct FilePathsW {
    Target: *const u16,
    Source: *const u16,
    Win32Error: u32,
    Flags: u32,
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 从 null 终止的宽字符指针读取字符串
/// 
/// # Safety
/// 调用者必须确保 ptr 指向有效的 null 终止宽字符串
unsafe fn wide_ptr_to_string(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    
    // 查找 null 终止符
    let mut len = 0;
    while *ptr.add(len) != 0 {
        len += 1;
        // 防止无限循环，设置最大长度
        if len >= 32768 {
            break;
        }
    }
    
    let slice = std::slice::from_raw_parts(ptr, len);
    String::from_utf16_lossy(slice)
}

/// 将 Path 转换为宽字符
fn path_to_wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

// ============================================================================
// 全局解压上下文（用于回调函数）
// ============================================================================

/// 解压上下文
struct ExtractContext {
    dest_dir: PathBuf,
    extracted_files: Vec<PathBuf>,
}

static EXTRACT_CONTEXT: Mutex<Option<ExtractContext>> = Mutex::new(None);

// ============================================================================
// SetupAPI 回调函数
// ============================================================================

/// SetupIterateCabinet 回调函数
unsafe extern "system" fn cabinet_callback(
    _context: *mut c_void,
    notification: UINT,
    param1: usize,
    _param2: usize,
) -> UINT {
    match notification {
        SPFILENOTIFY_FILEINCABINET => {
            // 文件在 cabinet 中被发现
            let info = &mut *(param1 as *mut FileInCabinetInfoW);
            
            // 获取文件名（使用安全的指针读取）
            let name_in_cabinet = wide_ptr_to_string(info.NameInCabinet);
            
            // 获取目标目录
            let mut ctx = EXTRACT_CONTEXT.lock().unwrap();
            if let Some(ref mut context) = *ctx {
                // 构建完整目标路径
                let target_path = context.dest_dir.join(&name_in_cabinet);
                
                // 创建父目录
                if let Some(parent) = target_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                
                // 设置目标文件名
                let target_wide = path_to_wide(&target_path);
                let copy_len = target_wide.len().min(259);
                info.FullTargetName[..copy_len].copy_from_slice(&target_wide[..copy_len]);
                info.FullTargetName[copy_len] = 0;
                
                FILEOP_DOIT
            } else {
                FILEOP_SKIP
            }
        }
        SPFILENOTIFY_FILEEXTRACTED => {
            // 文件已解压
            let paths = &*(param1 as *const FilePathsW);
            
            if paths.Win32Error == 0 {
                // 使用安全的指针读取
                let target = wide_ptr_to_string(paths.Target);
                
                let mut ctx = EXTRACT_CONTEXT.lock().unwrap();
                if let Some(ref mut context) = *ctx {
                    context.extracted_files.push(PathBuf::from(&target));
                }
            }
            
            FILEOP_DOIT
        }
        SPFILENOTIFY_NEEDNEWCABINET => {
            // 需要新的 cabinet 文件（多卷 cab）
            // 暂不支持，跳过
            FILEOP_ABORT
        }
        _ => FILEOP_DOIT,
    }
}

// ============================================================================
// Cabinet 解压器
// ============================================================================

/// Cabinet 文件解压器
/// 
/// 使用 Windows SetupAPI 来解压 .cab 文件。
pub struct CabinetExtractor {
    _lib: Library,
    iterate_cabinet: FnSetupIterateCabinetW,
}

impl CabinetExtractor {
    /// 创建 Cabinet 解压器实例
    pub fn new() -> Result<Self> {
        let lib = unsafe { Library::new("setupapi.dll") }
            .context("无法加载 setupapi.dll")?;
        
        unsafe {
            let iterate_cabinet: FnSetupIterateCabinetW = 
                *lib.get(b"SetupIterateCabinetW")?;
            
            Ok(Self {
                _lib: lib,
                iterate_cabinet,
            })
        }
    }
    
    /// 解压 .cab 文件到指定目录
    ///
    /// # 参数
    /// - `cab_path`: .cab 文件路径
    /// - `dest_dir`: 目标目录
    ///
    /// # 返回
    /// - 成功解压的文件列表
    pub fn extract(&self, cab_path: &Path, dest_dir: &Path) -> Result<Vec<PathBuf>> {
        // 确保目标目录存在
        std::fs::create_dir_all(dest_dir)?;
        
        // 设置解压上下文
        {
            let mut ctx = EXTRACT_CONTEXT.lock().unwrap();
            *ctx = Some(ExtractContext {
                dest_dir: dest_dir.to_path_buf(),
                extracted_files: Vec::new(),
            });
        }
        
        // 转换路径为宽字符
        let cab_wide = path_to_wide(cab_path);
        
        // 调用 SetupIterateCabinetW
        let result = unsafe {
            (self.iterate_cabinet)(
                cab_wide.as_ptr(),
                0,
                cabinet_callback,
                null_mut(),
            )
        };
        
        // 获取解压的文件列表
        let extracted = {
            let mut ctx = EXTRACT_CONTEXT.lock().unwrap();
            ctx.take()
                .map(|c| c.extracted_files)
                .unwrap_or_default()
        };
        
        if result.0 == 0 && extracted.is_empty() {
            bail!("SetupIterateCabinetW 失败");
        }
        
        println!("[CABINET] 成功解压 {} 个文件到 {:?}", extracted.len(), dest_dir);
        
        Ok(extracted)
    }
    
    /// 检查文件是否为 .cab 文件
    pub fn is_cab_file(path: &Path) -> bool {
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("cab"))
            .unwrap_or(false)
    }
}

// ============================================================================
// 便捷函数
// ============================================================================

/// 解压 .cab 文件到指定目录
///
/// # 参数
/// - `cab_path`: .cab 文件路径
/// - `dest_dir`: 目标目录
///
/// # 返回
/// - 成功解压的文件列表
pub fn extract_cab(cab_path: &Path, dest_dir: &Path) -> Result<Vec<PathBuf>> {
    let extractor = CabinetExtractor::new()?;
    extractor.extract(cab_path, dest_dir)
}

/// 解压目录中的所有 .cab 文件
///
/// # 参数
/// - `source_dir`: 包含 .cab 文件的源目录
/// - `dest_dir`: 目标目录（每个 cab 会解压到以 cab 文件名命名的子目录）
///
/// # 返回
/// - 成功解压的 cab 文件数量
pub fn extract_all_cabs(source_dir: &Path, dest_dir: &Path) -> Result<usize> {
    let extractor = CabinetExtractor::new()?;
    let mut count = 0;
    
    for entry in std::fs::read_dir(source_dir)? {
        let entry = entry?;
        let path = entry.path();
        
        if CabinetExtractor::is_cab_file(&path) {
            let cab_name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown");
            
            let cab_dest = dest_dir.join(cab_name);
            
            match extractor.extract(&path, &cab_dest) {
                Ok(files) => {
                    println!("[CABINET] 解压 {:?}: {} 个文件", path.file_name(), files.len());
                    count += 1;
                }
                Err(e) => {
                    println!("[CABINET] 解压 {:?} 失败: {}", path.file_name(), e);
                }
            }
        }
    }
    
    Ok(count)
}

/// 查找目录中的所有 .cab 文件
pub fn find_cab_files(dir: &Path) -> Vec<PathBuf> {
    let mut cab_files = Vec::new();
    
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if CabinetExtractor::is_cab_file(&path) {
                cab_files.push(path);
            }
        }
    }
    
    cab_files
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_is_cab_file() {
        assert!(CabinetExtractor::is_cab_file(Path::new("test.cab")));
        assert!(CabinetExtractor::is_cab_file(Path::new("test.CAB")));
        assert!(CabinetExtractor::is_cab_file(Path::new("Windows6.1-KB2990941-v3-x64.cab")));
        assert!(!CabinetExtractor::is_cab_file(Path::new("test.inf")));
        assert!(!CabinetExtractor::is_cab_file(Path::new("test.sys")));
    }
}
