//! XP 引导写入 + 可编辑修复引导脚本（两端共享）。
//!
//! - [`write_xp_boot`]：为已释放的 XP/2003 系统写入引导（ntldr/boot.ini + MBR，仅 Legacy）。
//! - [`run_repair_script`]：执行用户可编辑的 `bin\repair_boot.txt`，覆盖默认修复引导逻辑。

use std::path::Path;

use crate::command::new_command;
use crate::encoding::gbk_to_utf8;

/// 为应用好的 XP/2003 系统写入引导（仅 Legacy/MBR）。
///
/// 步骤：`bootsect /nt52 <盘> /mbr` 写 XP 引导码 → 校验 ntldr/ntdetect.com →
/// 缺失 boot.ini 时写入一份默认（不覆盖镜像自带的）。返回执行日志。
pub fn write_xp_boot(bin_dir: &Path, win_partition: &str) -> Result<String, String> {
    let win = win_partition.trim_end_matches('\\'); // 形如 "C:"
    let mut log = String::new();

    // 1) bootsect /nt52：写 XP（ntldr）引导码到分区引导扇区 + MBR
    let bootsect = bin_dir.join("bootsect.exe");
    if bootsect.exists() {
        log.push_str(&format!("执行: bootsect /nt52 {} /mbr\n", win));
        match new_command(&bootsect).args(["/nt52", win, "/mbr"]).output() {
            Ok(o) => {
                log.push_str(&gbk_to_utf8(&o.stdout));
                log.push_str(&gbk_to_utf8(&o.stderr));
                if !o.status.success() {
                    log.push_str("[bootsect 返回非 0]\n");
                }
            }
            Err(e) => return Err(format!("bootsect 执行失败: {}", e)),
        }
    } else {
        log.push_str("警告: 未找到 bootsect.exe，跳过 MBR/引导扇区写入\n");
    }

    // 2) 校验 ntldr / ntdetect.com（XP 镜像应为整盘/系统分区镜像，根目录自带这两文件）
    let ntldr = format!("{}\\ntldr", win);
    let ntdetect = format!("{}\\ntdetect.com", win);
    if !Path::new(&ntldr).exists() || !Path::new(&ntdetect).exists() {
        log.push_str(
            "警告: 系统盘根目录缺少 ntldr / ntdetect.com，请使用整盘/系统分区的 XP 镜像(GHO 优先)，否则可能无法引导\n",
        );
    }

    // 3) boot.ini（仅在不存在时写入，避免覆盖镜像自带配置）
    let boot_ini = format!("{}\\boot.ini", win);
    if !Path::new(&boot_ini).exists() {
        let content = "[boot loader]\r\n\
timeout=10\r\n\
default=multi(0)disk(0)rdisk(0)partition(1)\\WINDOWS\r\n\
[operating systems]\r\n\
multi(0)disk(0)rdisk(0)partition(1)\\WINDOWS=\"Windows XP\" /noexecute=optin /fastdetect\r\n";
        match std::fs::write(&boot_ini, content) {
            Ok(_) => log.push_str("已写入默认 boot.ini\n"),
            Err(e) => log.push_str(&format!("写 boot.ini 失败: {}\n", e)),
        }
    } else {
        log.push_str("boot.ini 已存在，保留镜像自带配置\n");
    }

    Ok(log)
}

/// 执行用户可编辑的修复引导脚本 `bin\repair_boot.txt`。
///
/// 文件支持 `[UEFI]` / `[Legacy]` 分节（无分节则全部命令通用）；按当前引导模式选取命令，
/// 逐行经 `cmd /c` 执行。支持占位符：
/// - `{WINDIR}` 系统的 Windows 目录（如 `C:\Windows`）
/// - `{WIN}` 系统盘（如 `C:`）
/// - `{ESP}` 已挂载的 EFI 分区盘符（仅 UEFI；调用方传入）
/// - `{BIN}` 程序 bin 目录
pub fn run_repair_script(
    script_path: &Path,
    bin_dir: &Path,
    win_partition: &str,
    use_uefi: bool,
    esp_letter: Option<&str>,
) -> Result<String, String> {
    let content = std::fs::read_to_string(script_path)
        .map_err(|e| format!("读取 {} 失败: {}", script_path.display(), e))?;

    let win = win_partition.trim_end_matches('\\');
    let windir = format!("{}\\Windows", win);
    let esp = esp_letter.unwrap_or("").trim_end_matches('\\').to_string();
    let bin = bin_dir.to_string_lossy().to_string();

    let want = if use_uefi { "uefi" } else { "legacy" };
    let mut has_sections = false;
    let mut section = String::new();
    let mut lines: Vec<String> = Vec::new();

    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            has_sections = true;
            section = line.trim_matches(|c| c == '[' || c == ']').to_lowercase();
            continue;
        }
        if !has_sections || section == want {
            lines.push(line.to_string());
        }
    }

    if lines.is_empty() {
        return Err(format!("repair_boot.txt 中没有适用于 {} 模式的命令", want));
    }

    let mut log = String::new();
    let mut any_fail = false;
    for line in lines {
        // UEFI 模式下 ESP 没挂上（{ESP} 为空）：跳过用到它的命令并标记失败，
        // 让上层回退到内置默认逻辑（默认逻辑有更完整的 ESP 挂载/创建处理）。
        if line.contains("{ESP}") && esp.is_empty() {
            log.push_str(&format!("[跳过：ESP 未挂载] {}\n", line));
            any_fail = true;
            continue;
        }
        let cmd_line = line
            .replace("{WINDIR}", &windir)
            .replace("{WIN}", win)
            .replace("{ESP}", &esp)
            .replace("{BIN}", &bin);
        log.push_str(&format!(">>> {}\n", cmd_line));
        match new_command("cmd").args(["/c", &cmd_line]).output() {
            Ok(o) => {
                log.push_str(&gbk_to_utf8(&o.stdout));
                log.push_str(&gbk_to_utf8(&o.stderr));
                if !o.status.success() {
                    log.push_str(&format!("[命令返回非 0] {}\n", cmd_line));
                    any_fail = true;
                }
            }
            Err(e) => return Err(format!("执行失败: {} ({})\n{}", cmd_line, e, log)),
        }
    }
    // 只要有命令失败（非 0 退出或 ESP 缺失被跳过），就视为修复失败并返回 Err，
    // 由调用方回退到内置默认修复逻辑——避免“命令报错却显示修复成功、实际没修”。
    if any_fail {
        return Err(format!("部分修复引导命令未成功，回退默认逻辑:\n{}", log));
    }
    Ok(log)
}
