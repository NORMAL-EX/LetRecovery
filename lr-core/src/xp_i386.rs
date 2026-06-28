//! Windows XP / 2003 的「i386 硬盘文本模式安装」（仅 Legacy/MBR）。
//!
//! 照搬成熟工具 DSI-安装备份（PECMD `dsi.WCS`）的「WinPE 无光驱硬盘装 NT5」做法——即微软
//! `winnt32 /makelocalsource` 原生的 `$WIN_NT$.~LS` + `$WIN_NT$.~BT` 约定。原版 XP/2003 安装盘是
//! `\I386`（或 x64 的 `\AMD64`）文本安装结构，没有 Vista+ 的 `install.wim`，无法「释放 WIM」。
//! 流程：
//!
//!   1. 把 `<arch>`（I386/AMD64）整个复制到 `\$WIN_NT$.~LS\<arch>`（本地源）；建空 `$WIN_NT$.~LS\$OEM$`；
//!   2. 建 `$WIN_NT$.~BT`（文本启动阶段的 BootPath）：拷 `<arch>\SYSTEM32` 整目录 + 按内嵌
//!      `NT5.txt` 清单把 `<arch>\<名>` 原样（压缩名不解压）复制进去；
//!   3. 根目录：`setupldr.bin`→`NTLDR`（开机直接进文本安装）、`NTDETECT.COM`、`BOOTFONT.BIN`、
//!      `TXTSETUP.SIF`（含文本期存储驱动集成）；`TXTSETUP.SIF` 同样写一份进 `$WIN_NT$.~BT`；
//!   4. `WINNT.SIF` 写进 `$WIN_NT$.~BT\`，并【强制】`MsDosInitiated=1`/`Floppyless=1`/`AutoPartition=0`/
//!      `UnattendedInstall=Yes`/`OemPreinstall=Yes`（缺 `MsDosInitiated=1` 文本安装会去找光盘而失败）；
//!      不改 `txtsetup.sif` 的 `SetupSourcePath`（靠 `MsDosInitiated=1` + `$WIN_NT$.~BT` 约定）；
//!   5. 标记目标分区为「活动分区」+ `bootsect /nt52` 写 XP 引导码（MBR/引导扇区加载 NTLDR）。
//!
//! 重启后 → setupldr 据 `$WIN_NT$.~BT` 进入 XP/2003 文本安装（蓝底）→ 复制文件 → 再次重启 → 图形安装。
//!
//! 限制：**仅 Legacy/BIOS + MBR**。XP 不支持 GPT/UEFI（调用方在 UI 已拦截 GPT/UEFI 目标）。
//! 调用前目标盘需已格式化(NTFS/FAT32)、且应为目标磁盘上的主分区。

use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use crate::command::new_command;
use crate::encoding::gbk_to_utf8;

/// `$WIN_NT$.~BT` 引导文件清单（编译期嵌入，照搬 DSI nt5\NT5.txt）。
const NT5_BOOTFILES: &str = include_str!("xp_nt5_bootfiles.txt");

/// 遍历 `$WIN_NT$.~BT` 引导文件清单（去注释 `#`、去空行、去首尾空白）。
fn nt5_bootfiles() -> impl Iterator<Item = &'static str> {
    NT5_BOOTFILES
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
}

/// 判断某目录是否为有效的 XP/2003 i386 安装源（含 setupldr.bin + txtsetup.sif + ntdetect.com）。
pub fn is_valid_i386(dir: &Path) -> bool {
    dir.join("setupldr.bin").exists()
        && dir.join("txtsetup.sif").exists()
        && dir.join("ntdetect.com").exists()
}

/// 从 i386 源目录做硬盘文本安装准备。成功后重启即进入 XP 文本安装。
///
/// - `i386_src`：i386 目录（如挂载 ISO 的 `G:\I386`，或已复制到数据分区的副本）。
/// - `win_partition`：目标系统盘（如 `"C:"`），需已格式化且为目标磁盘的主分区。
/// - `bin_dir`：程序 bin 目录（取 `bootsect.exe`；可选 `bin\xp\productkey.txt` 提供产品密钥实现全自动）。
/// - `custom_sif`：用户自定义的 winnt.sif 应答文件路径；`Some` 且存在时直接用它（原样写入，
///   规整为 CRLF），否则用内置生成的应答（按是否有产品密钥决定 DefaultHide/FullUnattended）。
pub fn install_from_i386(
    i386_src: &Path,
    win_partition: &str,
    bin_dir: &Path,
    custom_sif: Option<&Path>,
) -> Result<String, String> {
    let win = win_partition.trim_end_matches('\\'); // "C:"
    let mut log = String::new();

    // 0) 源目录 + 源中三件套校验
    if !i386_src.exists() {
        return Err(format!("找不到 i386 源目录: {}", i386_src.display()));
    }
    let setupldr = i386_src.join("setupldr.bin");
    let txtsetup = i386_src.join("txtsetup.sif");
    let ntdetect = i386_src.join("ntdetect.com");
    for (p, n) in [
        (&setupldr, "setupldr.bin"),
        (&txtsetup, "txtsetup.sif"),
        (&ntdetect, "ntdetect.com"),
    ] {
        if !p.exists() {
            return Err(format!("i386 缺少 {}，不是有效的 XP/2003 安装源", n));
        }
    }

    // 0.4) 源完整性硬校验：文本安装阶段必需的核心文件必须在源里（每个正版 XP/2003 i386/AMD64 源都有）。
    //      精简/重封装介质若缺这些，重启会卡蓝屏「文件无法加载 / inf 损坏」——在动盘、拷几个 G 之前
    //      就明确报错。下面拷完后还会在【目标】里复测一遍，确保它们真拷过去了。
    //      每项列出可接受的名字（任一存在即可）：多数只有压缩名(.sy_/.ex_)，但 ntfs 在重封装/换过驱动的
    //      介质上常是解压名 ntfs.sys——与 iso.rs::xp_i386_dir 探测、NT5.txt 清单一致（都认 .sy_ 或 .sys）。
    const REQUIRED_FILES: [&[&str]; 5] = [
        &["biosinfo.inf"],
        &["setupdd.sy_"],
        &["ntkrnlmp.ex_"],
        &["ntfs.sy_", "ntfs.sys"],
        &["setupreg.hiv"],
    ];
    let missing_src: Vec<String> = REQUIRED_FILES
        .iter()
        .filter(|names| !names.iter().any(|&n| i386_src.join(n).exists()))
        .map(|names| names.join("/"))
        .collect();
    if !missing_src.is_empty() {
        return Err(format!(
            "源 {} 缺少文本安装必需文件: {}。这不是完整的 XP/2003 安装源（疑似精简/重封装介质），无法进入蓝屏文本安装。",
            i386_src.display(),
            missing_src.join(", ")
        ));
    }

    // 0.5) 关键修复：确认目标分区根目录此刻【真的可写】，带重试。
    //      之前实机报「创建 C:\$WIN_NT$.~LS\I386 失败: 系统找不到指定的路径 (os error 3)」即出在
    //      下一步的 create_dir：刚格式化结束时盘符可能短暂卸载/重挂，或所选盘符当前并未挂载。
    //      这里先带重试探测一遍，过不了就给出可读的原因，而不是让 create_dir 抛裸 os error 3。
    ensure_volume_ready(win).map_err(|e| {
        format!(
            "目标分区 {win} 当前不可写：{e}。请确认该分区已分配盘符、且已格式化为 NTFS/FAT32（若刚格式化完，请稍候重试）。XP 仅支持 Legacy/MBR，目标盘不能是 GPT。"
        )
    })?;
    log.push_str(&format!("目标分区 {win} 可写，开始准备文本安装\n"));

    // 1) 复制 源(I386/AMD64) → win\$WIN_NT$.~LS\<同名子目录>（文本安装本地源）。
    //    子目录名取源目录名(I386 或 AMD64)，与 txtsetup.sif 的 [SourceDisksNames] 路径一致；
    //    对原版 32 位 i386 介质即 I386(行为不变)，64 位 2003/XP x64 介质则为 AMD64。
    let src_sub_name = i386_src
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("I386")
        .to_uppercase();
    let ls_src = format!("{win}\\$WIN_NT$.~LS\\{src_sub_name}");
    log.push_str(&format!("复制 {src_sub_name} 源到 {ls_src} ...\n"));
    create_dir_all_retry(&ls_src).map_err(|e| format!("创建 {ls_src} 失败: {e}"))?;
    let src = i386_src.to_string_lossy().to_string();
    // /C：单个文件出错（被占用/锁定等）也继续拷其余文件，不因一个非关键文件失败就整盘中止
    //    （照搬 DSI 的容错——它直接忽略 xcopy 退出码）。但比 DSI 更稳：拷完后在【目标】里复测核心
    //    文件是否到位，而不是去猜「xcopy 加 /C 后非 0 退出」到底是部分跳过还是整体失败。
    let out = new_command("xcopy")
        .args([src.as_str(), ls_src.as_str(), "/E", "/I", "/H", "/C", "/R", "/Y", "/Q"])
        .output()
        .map_err(|e| format!("xcopy 执行失败: {e}"))?;
    log.push_str(&gbk_to_utf8(&out.stdout));
    if !out.status.success() {
        // 有 /C 时非 0 退出多半是「部分文件被占用已跳过」而非整体失败；不直接判错，交给下面的
        // 「目标核心文件实测」定夺，但记下来便于排查。
        log.push_str(&format!(
            "警告: xcopy 返回非 0（已用 /C 跳过出错文件继续）：{}\n",
            gbk_to_utf8(&out.stderr).trim()
        ));
    }
    // 拷贝结果硬校验（真正权威的成功判据，不依赖 xcopy 退出码）：核心文件必须真的落进了本地源目录。
    // 若因被占用/中断导致核心文件没拷过去，文本安装必然蓝屏，这里就要拦下。
    let ls_path = Path::new(&ls_src);
    let missing_dst: Vec<String> = REQUIRED_FILES
        .iter()
        .filter(|names| !names.iter().any(|&n| ls_path.join(n).exists()))
        .map(|names| names.join("/"))
        .collect();
    if !missing_dst.is_empty() {
        return Err(format!(
            "复制到本地源 {ls_src} 后仍缺少核心文件: {}（源文件可能被占用或复制中断）。已用 /C 跳过出错文件，但这些是必需的，无法继续。",
            missing_dst.join(", ")
        ));
    }

    // 1.4) XP x64 / Server 2003 x64：本地源要【两份都拷】——照搬 DSI（§四 同时 xcopy AMD64 与 I386）。
    //      x64 介质的 \I386 是 32 位 WoW 组件目录（残缺、非可引导源），GUI 安装阶段装 WoW64（32 位
    //      子系统）要从 LS\I386 取文件；只拷 \AMD64 会导致图形阶段缺 32 位组件。引导/BT 仍只用 AMD64。
    if src_sub_name == "AMD64" {
        if let Some(sib_i386) = i386_src.parent().map(|p| p.join("I386")) {
            if sib_i386.exists() {
                let s = sib_i386.to_string_lossy().to_string();
                let d = format!("{win}\\$WIN_NT$.~LS\\I386");
                let _ = create_dir_all_retry(&d);
                match new_command("xcopy")
                    .args([s.as_str(), d.as_str(), "/E", "/I", "/H", "/R", "/Y", "/Q"])
                    .output()
                {
                    Ok(o) if o.status.success() => {
                        log.push_str("已并拷同级 \\I386（32 位 WoW64 组件）→ $WIN_NT$.~LS\\I386\n")
                    }
                    Ok(o) => log.push_str(&format!(
                        "警告: 并拷 \\I386（WoW64）非 0：{}；XP x64 图形阶段可能缺 32 位组件\n",
                        gbk_to_utf8(&o.stderr)
                    )),
                    Err(e) => log.push_str(&format!("警告: 并拷 \\I386（WoW64）失败：{e}\n")),
                }
            }
        }
    }

    // 1.5) 建空 $OEM$（OemPreinstall=Yes 需要它存在；空目录无副作用）。失败要记日志：
    //      否则文本安装阶段会因 OemPreinstall=Yes 找不到 $OEM$ 而报错。
    if let Err(e) = create_dir_all_retry(&format!("{win}\\$WIN_NT$.~LS\\$OEM$")) {
        log.push_str(&format!("警告: 建 $WIN_NT$.~LS\\$OEM$ 失败（{e}）；OemPreinstall=Yes 可能在文本阶段报错\n"));
    }

    // 1.6) 建 $WIN_NT$.~BT（文本启动阶段的 BootPath）：照搬 DSI——
    //      a) 整个 <arch>\SYSTEM32 → $WIN_NT$.~BT\SYSTEM32；
    //      b) 按 NT5.txt 清单把 <arch>\<名> 原样（压缩名不解压）→ $WIN_NT$.~BT\<名>。
    //      setupldr(伪装 NTLDR) 据 $WIN_NT$.~BT 的存在进入「硬盘本地源安装」，从这里加载文本期内核/驱动。
    let bt = format!("{win}\\$WIN_NT$.~BT");
    create_dir_all_retry(&bt).map_err(|e| format!("创建 {bt} 失败: {e}"))?;
    let sys32_src = i386_src.join("SYSTEM32");
    if sys32_src.exists() {
        let s = sys32_src.to_string_lossy().to_string();
        let d = format!("{bt}\\SYSTEM32");
        let o = new_command("xcopy")
            .args([s.as_str(), d.as_str(), "/E", "/I", "/H", "/R", "/Y", "/Q"])
            .output()
            .map_err(|e| format!("拷 SYSTEM32 → $WIN_NT$.~BT 失败: {e}"))?;
        if o.status.success() {
            log.push_str("已复制 <源>\\SYSTEM32 → $WIN_NT$.~BT\\SYSTEM32\n");
        } else {
            log.push_str(&format!(
                "警告: 拷 SYSTEM32 → $WIN_NT$.~BT 非 0：{}\n",
                gbk_to_utf8(&o.stderr)
            ));
        }
    } else {
        log.push_str("警告: 源中无 SYSTEM32 子目录（部分重封装介质如此），$WIN_NT$.~BT\\SYSTEM32 跳过\n");
    }
    let (mut bt_copied, mut bt_missing) = (0usize, 0usize);
    for name in nt5_bootfiles() {
        let s = i386_src.join(name);
        if s.exists() {
            match std::fs::copy(&s, format!("{bt}\\{name}")) {
                Ok(_) => bt_copied += 1,
                Err(e) => log.push_str(&format!("警告: 拷 {name} → $WIN_NT$.~BT 失败: {e}\n")),
            }
        } else {
            bt_missing += 1;
        }
    }
    log.push_str(&format!(
        "$WIN_NT$.~BT 引导文件：按清单复制 {bt_copied} 个（源中缺 {bt_missing} 个，已跳过）\n"
    ));

    // 2) 引导文件落根目录：
    //    setupldr.bin -> \NTLDR（开机直接进文本安装）；ntdetect.com -> \NTDETECT.COM；
    //    biosinfo.inf / bootfont.bin 若源里有也一并复制（setupldr 需要 biosinfo；bootfont 为蓝底本地化字库）。
    // 用 copy_force/write_force：先清目标 +r+s+h 再写——之前装过 XP / 修过引导的盘上，根目录
    //    NTLDR/NTDETECT.COM/TXTSETUP.SIF 常带 只读+系统+隐藏，直接 std::fs::copy 会 os error 5（拒绝访问）。
    copy_force(&setupldr, &format!("{win}\\NTLDR")).map_err(|e| format!("写 NTLDR 失败: {e}"))?;
    copy_force(&ntdetect, &format!("{win}\\NTDETECT.COM"))
        .map_err(|e| format!("写 NTDETECT.COM 失败: {e}"))?;
    log.push_str("已写入 NTLDR(setupldr) / NTDETECT.COM\n");
    for opt in ["biosinfo.inf", "bootfont.bin"] {
        let s = i386_src.join(opt);
        if s.exists() {
            match copy_force(&s, &format!("{win}\\{opt}")) {
                Ok(_) => log.push_str(&format!("已复制 {opt}\n")),
                Err(e) => log.push_str(&format!("复制 {opt} 失败（忽略）: {e}\n")),
            }
        }
    }

    // 3) txtsetup.sif：照搬 DSI——不改 SetupSourcePath（靠 MsDosInitiated=1 + $WIN_NT$.~BT 约定，
    //    setupldr/setupdd 自会用 $WIN_NT$.~BT 作引导路径、$WIN_NT$.~LS 作源）。仅做文本期驱动集成，
    //    再写入 $WIN_NT$.~BT\TXTSETUP.SIF（setupldr 实际读这份）与根目录各一份。
    let raw = std::fs::read(&txtsetup).map_err(|e| format!("读 txtsetup.sif 失败: {e}"))?;

    // 文本期存储驱动集成（按架构）：驱动 .sys 同时拷进源($WIN_NT$.~LS\<arch>)与引导($WIN_NT$.~BT)。
    let xp_drv = bin_dir.join("drivers").join("xp");
    let roots = if src_sub_name == "AMD64" {
        vec![xp_drv.join("amd64"), xp_drv.join("ahci"), xp_drv.join("nvme")]
    } else {
        vec![xp_drv.join("x86")]
    };
    let drivers = crate::xp_textmode_drv::scan_driver_roots(&roots);
    log.push_str(&format!(
        "文本期存储驱动：架构={}，发现 {} 个可集成驱动\n",
        if src_sub_name == "AMD64" { "amd64" } else { "x86" },
        drivers.len()
    ));
    // 32 位 i386 介质却一个文本期存储驱动都没扫到：自带的 AHCI/NVMe 驱动是 64 位（仅 AMD64 介质可用），
    // bin\drivers\xp\x86 默认是空的。此时蓝底文本安装很可能「找不到硬盘」——醒目提示用户切 IDE 或自备 32 位驱动，
    // 避免装到一半才在蓝屏里发现没驱动（与 DSI 一样不内置 32 位 AHCI 驱动，这是已知能力缺口而非错误）。
    if drivers.is_empty() && src_sub_name != "AMD64" {
        log.push_str(
            "⚠ 警告: 未集成任何 32 位文本期存储驱动（bin\\drivers\\xp\\x86 为空，自带 AHCI/NVMe 驱动是 64 位、仅 AMD64 介质可用）。\
             若重启进蓝底文本安装时提示「Setup 找不到硬盘 / Setup did not find any hard disk drives」，\
             请进 BIOS 把 SATA/存储模式切到 IDE / Compatibility / Legacy 后重试；\
             或把 32 位 AHCI/NVMe 驱动（.inf+.sys）放进 bin\\drivers\\xp\\x86 再重做。\n",
        );
    }
    // 关键编码处理：txtsetup.sif 是 ANSI（中文版=GBK/CP936），setupdd 按 ANSI 读它。绝不能改写成 UTF-8——
    //   否则非 ASCII 行全乱 → 蓝屏「安装程序用在第 N 行上的 .SIF 文件中有一个语法错误」。
    //   · 无驱动要集成（原版 32 位 i386 即此路）：原样写源文件字节，一个字节都不动（最稳，且 NT5.txt 清单
    //     已把同一份字节拷进 $WIN_NT$.~BT，这里只是覆盖成相同内容 + 落一份到根目录）。
    //   · 有驱动要集成：解码→追加 ASCII 集成行→按【原编码】写回（原是 GBK 就编回 GBK；追加的都是 ASCII，无损）。
    let txtsetup_bytes: Vec<u8> = if drivers.is_empty() {
        log.push_str("文本期无驱动集成：TXTSETUP.SIF 原样写入（保持原 ANSI/GBK 编码不变）\n");
        raw.clone()
    } else {
        let was_utf8 = std::str::from_utf8(&raw).is_ok();
        let txt = if was_utf8 {
            String::from_utf8_lossy(&raw).into_owned()
        } else {
            gbk_to_utf8(&raw)
        };
        let txt = normalize_crlf(&txt);
        let (final_txtsetup, drvlog) =
            crate::xp_textmode_drv::integrate(&txt, &drivers, &[Path::new(&ls_src), Path::new(&bt)]);
        log.push_str(&drvlog);
        // 原是 GBK 就编回 GBK（集成追加的都是 ASCII，是 GBK 子集，无损）；原本就是纯 ASCII/UTF-8 才按 UTF-8 写。
        if was_utf8 {
            final_txtsetup.into_bytes()
        } else {
            crate::encoding::utf8_to_gbk(&final_txtsetup)
        }
    };
    write_force(&format!("{bt}\\TXTSETUP.SIF"), &txtsetup_bytes)
        .map_err(|e| format!("写 $WIN_NT$.~BT\\TXTSETUP.SIF 失败: {e}"))?;
    write_force(&format!("{win}\\TXTSETUP.SIF"), &txtsetup_bytes)
        .map_err(|e| format!("写根 TXTSETUP.SIF 失败: {e}"))?;
    log.push_str("已写入 TXTSETUP.SIF（$WIN_NT$.~BT 与根）\n");

    // 4) winnt.sif 应答：优先用户自定义；否则内置生成。无论哪种，都【强制写入硬盘安装必需的键】
    //    （照搬 DSI 的 NT5部署无人值守：MsDosInitiated=1 / Floppyless=1 / AutoPartition=0 /
    //    UnattendedInstall=Yes / OemPreinstall=Yes）——缺它们文本安装会去找光盘而失败。
    //    放在 $WIN_NT$.~BT\WINNT.SIF（文本安装阶段读这份）。
    let (sif_raw, sif_was_utf8) = match custom_sif {
        Some(p) if p.exists() => {
            let raw = std::fs::read(p)
                .map_err(|e| format!("读自定义 winnt.sif 失败 {}: {e}", p.display()))?;
            let was_utf8 = std::str::from_utf8(&raw).is_ok();
            let s = if was_utf8 {
                String::from_utf8_lossy(&raw).into_owned()
            } else {
                gbk_to_utf8(&raw)
            };
            log.push_str(&format!("使用自定义无人值守应答: {}\n", p.display()));
            (normalize_crlf(&s), was_utf8)
        }
        _ => {
            let product_key = read_product_key(bin_dir);
            match &product_key {
                Some(_) => {
                    log.push_str("检测到产品密钥（bin\\xp\\productkey.txt）→ 全自动无人值守\n")
                }
                None => log.push_str(
                    "未提供产品密钥（可放 bin\\xp\\productkey.txt 实现全自动）→ 仅「密钥」页停顿，其余无人值守\n",
                ),
            }
            // 内置生成的应答是纯 ASCII，按 UTF-8(=ASCII) 写即可。
            (winnt_sif(product_key.as_deref()), true)
        }
    };
    let sif_content = force_winnt_keys(&sif_raw);
    // 同 txtsetup：WINNT.SIF 也按 ANSI 读，原是 GBK（自定义中文应答）就编回 GBK，别改成 UTF-8。
    let sif_bytes = if sif_was_utf8 {
        sif_content.into_bytes()
    } else {
        crate::encoding::utf8_to_gbk(&sif_content)
    };
    write_force(&format!("{bt}\\WINNT.SIF"), &sif_bytes)
        .map_err(|e| format!("写 $WIN_NT$.~BT\\WINNT.SIF 失败: {e}"))?;
    log.push_str("已写入 $WIN_NT$.~BT\\WINNT.SIF（已强制 MsDosInitiated=1 等硬盘安装必需键）\n");

    // 4.5) 标记目标分区为「活动分区」。Legacy/MBR BIOS 只从活动分区加载 NTLDR；非活动 = 重启
    //      黑屏「Missing operating system」且无任何提示。故置活动失败视为【硬错误】，宁可现在就报。
    let letter = win.trim_end_matches(':');
    match set_volume_active(letter) {
        Ok(o) => {
            log.push_str(&format!("已标记 {win} 为活动分区\n"));
            let o = o.trim();
            if !o.is_empty() {
                log.push_str(o);
                log.push('\n');
            }
        }
        Err(e) => {
            return Err(format!(
                "标记 {win} 为活动分区失败（{e}）。Legacy/MBR 下不置活动会导致重启无法引导。请确认目标盘是 MBR 基本磁盘（非 GPT/动态盘）后重试。"
            ));
        }
    }

    // 5) bootsect /nt52 写 XP 引导码（使引导扇区/MBR 加载 NTLDR）。缺 bootsect.exe = 准备出一块
    //    不能开机的盘，故【硬错误】而非默默成功。
    let bootsect = bin_dir.join("bootsect.exe");
    if !bootsect.exists() {
        return Err(format!(
            "未找到 {}：无法写 NT5 引导码，准备好的盘重启进不去文本安装。请确认安装包 bin\\bootsect.exe 存在。",
            bootsect.display()
        ));
    }
    let out = new_command(&bootsect)
        .args(["/nt52", win, "/mbr", "/force"])
        .output()
        .map_err(|e| format!("bootsect 执行失败: {e}"))?;
    log.push_str(&gbk_to_utf8(&out.stdout));
    log.push_str(&gbk_to_utf8(&out.stderr));
    if !out.status.success() {
        // bootsect 偶有非 0 但实际已写成功，故不直接判错；但要醒目提示以便对照实机现象。
        log.push_str("⚠ 警告: bootsect 返回非 0——引导扇区可能未写成功，若重启进不去文本安装请重做此步\n");
    }
    log.push_str("已用 bootsect /nt52 写引导码\n");

    log.push_str("i386 硬盘文本安装准备完成，重启进入 XP/2003 蓝底文本安装阶段。\n");
    Ok(log)
}

/// 清掉目标文件的 只读/系统/隐藏 属性（若存在）。重装/修过引导的盘上根引导文件常带这些属性，
/// 不清会让 `std::fs::copy`/`write` 抛 os error 5（拒绝访问）。失败忽略（文件不存在或本就无属性）。
fn clear_file_attrs(path: &str) {
    if Path::new(path).exists() {
        let _ = new_command("attrib").args(["-R", "-S", "-H", path]).output();
    }
}

/// 先清属性再复制（应对目标带 +r+s+h）。
fn copy_force(src: &Path, dst: &str) -> std::io::Result<u64> {
    clear_file_attrs(dst);
    std::fs::copy(src, dst)
}

/// 先清属性再写（应对目标带 +r+s+h）。
fn write_force(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    clear_file_attrs(path);
    std::fs::write(path, bytes)
}

/// 带重试地探测目标卷此刻可写：根目录存在 + 能建/删一个探针目录。
///
/// 应对「刚格式化后盘符短暂卸载/重挂」的瞬时窗口；也能把「所选盘符当前根本没挂」这种情况
/// 转成可读错误，避免后续 `create_dir` 抛裸的 `os error 3`（系统找不到指定的路径）。
fn ensure_volume_ready(win: &str) -> Result<(), String> {
    let root = format!("{win}\\");
    let probe = format!("{win}\\$lr_xp_probe$");
    let mut last = String::from("未知");
    for _ in 0..10 {
        if Path::new(&root).exists() {
            match std::fs::create_dir(&probe) {
                Ok(_) => {
                    let _ = std::fs::remove_dir(&probe);
                    return Ok(());
                }
                // 上次残留的探针目录：能删即视为可写
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let _ = std::fs::remove_dir(&probe);
                    return Ok(());
                }
                Err(e) => last = e.to_string(),
            }
        } else {
            last = "盘符根目录不存在/未挂载".to_string();
        }
        sleep(Duration::from_millis(500));
    }
    Err(last)
}

/// `create_dir_all` 带几次重试（应对刚格式化后盘符重挂的瞬时窗口）。
fn create_dir_all_retry(path: &str) -> std::io::Result<()> {
    let mut last: Option<std::io::Error> = None;
    for _ in 0..8 {
        match std::fs::create_dir_all(path) {
            Ok(_) => return Ok(()),
            Err(e) => {
                last = Some(e);
                sleep(Duration::from_millis(500));
            }
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("create_dir_all 重试失败")))
}

/// 从 `bin\xp\productkey.txt`（或 `bin\xp_productkey.txt`）读取产品密钥。
///
/// 取第一行非注释（`#`/`;` 开头为注释）、长度足够像密钥（≥20，形如 `XXXXX-XXXXX-XXXXX-XXXXX-XXXXX`）的内容。
/// 没有文件或没有合法行时返回 `None`（→ winnt.sif 用 DefaultHide，仅在密钥页停顿）。
fn read_product_key(bin_dir: &Path) -> Option<String> {
    let candidates = [
        bin_dir.join("xp").join("productkey.txt"),
        bin_dir.join("xp_productkey.txt"),
    ];
    for p in candidates {
        if let Ok(s) = std::fs::read_to_string(&p) {
            for line in s.lines() {
                let t = line.trim();
                if t.is_empty() || t.starts_with('#') || t.starts_with(';') {
                    continue;
                }
                if t.len() >= 20 {
                    return Some(t.to_string());
                }
            }
        }
    }
    None
}

/// 把任意换行规整为 CRLF（winnt.sif 应为 DOS 换行；用户自定义文件可能是 LF）。同时去掉行首 UTF-8 BOM
/// ——否则带 BOM 的自定义 .sif 首行会变成 `\u{feff}[Data]`，节头识别失败，强制键被追加到被忽略的重复节。
fn normalize_crlf(s: &str) -> String {
    let s = s.strip_prefix('\u{feff}').unwrap_or(s);
    let mut out = String::with_capacity(s.len() + 16);
    for line in s.split('\n') {
        out.push_str(line.strip_suffix('\r').unwrap_or(line));
        out.push_str("\r\n");
    }
    out
}

/// 取 INI 节标题行的节名（去首尾空白、容忍行尾注释/多余内容，如 `[Data]  ; 注释`）。非节标题返回 None。
fn ini_header_name(line: &str) -> Option<&str> {
    let t = line.trim();
    if !t.starts_with('[') {
        return None;
    }
    let close = t.find(']')?;
    Some(t[1..close].trim())
}

/// 用 diskpart 把指定盘符（如 `"C"`）的卷标记为「活动分区」。仅 MBR 有意义。
fn set_volume_active(letter: &str) -> Result<String, String> {
    use std::io::Write;
    let script = format!("select volume {letter}\r\nactive\r\nexit\r\n");
    let tmp = std::env::temp_dir().join("lr_xp_set_active.txt");
    {
        let mut f =
            std::fs::File::create(&tmp).map_err(|e| format!("创建 diskpart 脚本失败: {e}"))?;
        f.write_all(script.as_bytes())
            .map_err(|e| format!("写 diskpart 脚本失败: {e}"))?;
    }
    let tmp_str = tmp.to_string_lossy().into_owned();
    let out = new_command("diskpart")
        .args(["/s", tmp_str.as_str()])
        .output()
        .map_err(|e| format!("diskpart 执行失败: {e}"))?;
    let _ = std::fs::remove_file(&tmp);
    let so = gbk_to_utf8(&out.stdout);
    if !out.status.success() {
        return Err(format!("diskpart 返回非 0: {}", so.trim()));
    }
    // diskpart 即使内部失败（目标是 GPT/动态盘/逻辑分区，无法设活动）也常返回 0，故再按输出里的错误标志判一次。
    // 用【否定检测】：只有命中已知错误词才算失败，绝不会把成功误判为失败 → 不会挡住本能正常设活动的盘。
    if diskpart_reported_failure(&so) {
        return Err(format!(
            "diskpart 未能把 {letter}: 设为活动分区（可能目标是 GPT/动态盘/逻辑分区，无法设活动）：\n{}",
            so.trim()
        ));
    }
    Ok(so)
}

/// diskpart 输出里是否报了失败（中/英 PE）。diskpart 内部失败常仍返回 0，故按输出里的错误词判断。
/// 仅否定检测：命中已知错误词才算失败；成功串（「…标记为活动分区」/「marked … as active」）不含这些词，故不会误判。
fn diskpart_reported_failure(output: &str) -> bool {
    let lo = output.to_ascii_lowercase();
    output.contains("无法")
        || output.contains("错误")
        || lo.contains("cannot")
        || lo.contains("is not")
        || lo.contains("no volume")
        || lo.contains("error")
}

/// 强制写入硬盘安装必需的应答键（照搬 DSI 的 `NT5部署无人值守`）。无论用户自定义 .sif 怎么写，
/// 都把这 5 个键设成硬盘安装能跑通的值——尤其 `MsDosInitiated=1`（缺它文本安装会找光盘失败）。
fn force_winnt_keys(content: &str) -> String {
    let mut s = normalize_crlf(content);
    for (section, key, value) in [
        ("[Data]", "MsDosInitiated", "1"),
        ("[Data]", "Floppyless", "1"),
        ("[Data]", "AutoPartition", "0"),
        ("[Data]", "UnattendedInstall", "Yes"),
        ("[Unattended]", "OemPreinstall", "Yes"),
    ] {
        s = set_ini_key(&s, section, key, value);
    }
    s
}

/// 在 INI 文本里把 `section` 节的 `key` 设为 `value`（CRLF）：键存在则替换（并去重），
/// 节存在但缺键则在节内补，节不存在则在文末新建节再补。大小写不敏感匹配节名/键名。
fn set_ini_key(content: &str, section: &str, key: &str, value: &str) -> String {
    let nl = "\r\n";
    let kv = format!("{key}={value}{nl}");
    // section 形如 "[Data]" → 取内部名 "Data" 与节标题名比对（容忍行尾注释/BOM）。
    let sec_inner = section.trim().trim_start_matches('[').trim_end_matches(']');
    let mut out = String::with_capacity(content.len() + 64);
    let mut in_target = false;
    let mut inserted = false;
    let mut seen_section = false;
    for line in content.split_inclusive('\n') {
        let t = line.trim();
        if let Some(name) = ini_header_name(t) {
            if in_target && !inserted {
                out.push_str(&kv);
                inserted = true;
            }
            in_target = name.eq_ignore_ascii_case(sec_inner);
            if in_target {
                seen_section = true;
                inserted = false;
            }
            out.push_str(line);
            continue;
        }
        if in_target {
            if let Some((k, _)) = t.split_once('=') {
                if k.trim().eq_ignore_ascii_case(key) {
                    if !inserted {
                        out.push_str(&kv);
                        inserted = true;
                    }
                    continue; // 丢弃原键行/重复键
                }
            }
        }
        out.push_str(line);
    }
    if in_target && !inserted {
        if !out.ends_with('\n') {
            out.push_str(nl);
        }
        out.push_str(&kv);
    } else if !seen_section {
        if !out.ends_with('\n') {
            out.push_str(nl);
        }
        out.push_str(section);
        out.push_str(nl);
        out.push_str(&kv);
    }
    out
}

/// 生成 winnt.sif 应答文件。
///
/// - 有 `product_key`：`UnattendMode=FullUnattended` 全自动（文本+图形全程无停顿）。
/// - 无密钥：`UnattendMode=DefaultHide`（隐藏已答页，仅在「产品密钥」页停一下，其余无人值守）。
///
/// 统一项：跳过 EULA/区域/欢迎；`DriverSigningPolicy=Ignore`（不拦未签名/注入的存储驱动）；
/// 管理员空密码 + 首次自动登录；不分区/不格式化（沿用已格式化的目标盘）；目标 `\WINDOWS`。
/// 出于安全，文本阶段仍由用户确认安装分区（`AutoPartition=0`），避免自动选错盘抹掉数据。
fn winnt_sif(product_key: Option<&str>) -> String {
    let (mode, key_line) = match product_key {
        Some(k) => ("FullUnattended", format!("ProductKey=\"{k}\"\r\n")),
        None => ("DefaultHide", String::new()),
    };
    format!(
        "[Data]\r\n\
AutoPartition=0\r\n\
MsDosInitiated=1\r\n\
UnattendedInstall=Yes\r\n\
Floppyless=1\r\n\
\r\n\
[Unattended]\r\n\
UnattendMode={mode}\r\n\
UnattendSwitch=Yes\r\n\
OemPreinstall=Yes\r\n\
OemSkipEula=Yes\r\n\
TargetPath=\\WINDOWS\r\n\
FileSystem=LeaveAlone\r\n\
WaitForReboot=No\r\n\
DriverSigningPolicy=Ignore\r\n\
\r\n\
[GuiUnattended]\r\n\
AdminPassword=*\r\n\
EncryptedAdminPassword=No\r\n\
AutoLogon=Yes\r\n\
AutoLogonCount=1\r\n\
OEMSkipRegional=1\r\n\
OemSkipWelcome=1\r\n\
TimeZone=210\r\n\
\r\n\
[UserData]\r\n\
FullName=\"User\"\r\n\
OrgName=\"\"\r\n\
ComputerName=*\r\n\
{key_line}\
\r\n\
[Identification]\r\n\
JoinWorkgroup=WORKGROUP\r\n\
\r\n\
[Networking]\r\n\
InstallDefaultComponents=Yes\r\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diskpart_reported_failure_negative_detection() {
        // 成功串（中/英）→ 不算失败（关键：成功串里没有「无法/错误/cannot/is not/error」）
        assert!(!diskpart_reported_failure("DiskPart 已将当前分区标记为活动分区。"));
        assert!(!diskpart_reported_failure(
            "DiskPart marked the current partition as active."
        ));
        assert!(!diskpart_reported_failure(""));
        // 失败串 → 算失败
        assert!(diskpart_reported_failure(
            "DiskPart 无法在所选磁盘上标记活动分区。"
        ));
        assert!(diskpart_reported_failure(
            "The selected disk is not a fixed MBR disk."
        ));
        assert!(diskpart_reported_failure("There is no volume selected."));
    }

    #[test]
    fn winnt_sif_without_key_is_defaulthide() {
        let s = winnt_sif(None);
        assert!(s.contains("UnattendMode=DefaultHide"));
        assert!(!s.contains("ProductKey"));
        assert!(s.contains("DriverSigningPolicy=Ignore"));
        assert!(s.contains("Floppyless=1"));
        assert!(s.contains("OemSkipEula=Yes"));
    }

    #[test]
    fn winnt_sif_with_key_is_fullunattended() {
        let s = winnt_sif(Some("AAAAA-BBBBB-CCCCC-DDDDD-EEEEE"));
        assert!(s.contains("UnattendMode=FullUnattended"));
        assert!(s.contains("ProductKey=\"AAAAA-BBBBB-CCCCC-DDDDD-EEEEE\""));
    }

    #[test]
    fn force_keys_overrides_msdosinitiated() {
        // 用户自定义 .sif 里 MsDosInitiated="0" → 必须被强制改成 1（照搬 DSI）
        let input = ";c\r\n[Data]\r\n    AutoPartition=1\r\n    MsDosInitiated=\"0\"\r\n    UnattendedInstall=\"Yes\"\r\n\r\n[Unattended]\r\n    OemPreinstall=No\r\n    TargetPath=\\WINDOWS\r\n";
        let out = force_winnt_keys(input);
        assert!(out.contains("MsDosInitiated=1"));
        assert!(!out.contains("MsDosInitiated=\"0\""));
        assert!(out.contains("AutoPartition=0") && !out.contains("AutoPartition=1"));
        assert!(out.contains("OemPreinstall=Yes") && !out.contains("OemPreinstall=No"));
        assert!(out.contains("Floppyless=1")); // 原本缺 → 补进 [Data]
        assert!(out.contains("TargetPath=\\WINDOWS")); // 无关行保留
    }

    #[test]
    fn force_keys_handles_bom_and_commented_header() {
        // 带 UTF-8 BOM 的自定义 .sif + 节头带行尾注释：必须仍能命中 [Data]，强制键改对，不产生重复节
        let input = "\u{feff}[Data]  ; partition data\r\nMsDosInitiated=\"0\"\r\n";
        let out = force_winnt_keys(input);
        assert!(out.contains("MsDosInitiated=1"));
        assert!(!out.contains("MsDosInitiated=\"0\""));
        // 不能追加出第二个 [Data]（否则 XP 只读第一个，强制键落到被忽略的尾节）
        assert_eq!(out.matches("[Data]").count(), 1);
    }

    #[test]
    fn ini_header_name_tolerates_comment() {
        assert_eq!(ini_header_name("[Data]"), Some("Data"));
        assert_eq!(ini_header_name("  [Unattended]  ; x"), Some("Unattended"));
        assert_eq!(ini_header_name("Key=Val"), None);
    }

    #[test]
    fn set_ini_key_creates_missing_section() {
        let out = set_ini_key("[Foo]\r\nx=1\r\n", "[Data]", "MsDosInitiated", "1");
        assert!(out.contains("[Data]\r\nMsDosInitiated=1\r\n"));
    }

    #[test]
    fn set_ini_key_dedups_existing_key() {
        let out = set_ini_key("[Data]\r\nk=a\r\nk=b\r\n", "[Data]", "k", "z");
        assert_eq!(out.matches("k=").count(), 1);
        assert!(out.contains("k=z"));
    }

    #[test]
    fn nt5_bootfiles_parses_manifest() {
        let v: Vec<&str> = nt5_bootfiles().collect();
        assert!(v.contains(&"ATAPI.SY_"));
        assert!(v.contains(&"NTKRNLMP.EX_"));
        assert!(v.contains(&"TXTSETUP.SIF"));
        assert!(v.iter().all(|l| !l.starts_with('#') && !l.is_empty()));
        assert!(v.len() > 100);
    }

    #[test]
    fn normalize_crlf_converts_lf_and_keeps_crlf() {
        assert_eq!(normalize_crlf("a\nb"), "a\r\nb\r\n");
        assert_eq!(normalize_crlf("a\r\nb\r\n"), "a\r\nb\r\n\r\n");
        assert_eq!(normalize_crlf("[Data]\nAutoPartition=0"), "[Data]\r\nAutoPartition=0\r\n");
    }

    #[test]
    fn winnt_sif_baseline_has_msdosinitiated_1() {
        // 基线生成的应答即应是 1（force 再保险一次）
        let s = winnt_sif(None);
        assert!(s.contains("MsDosInitiated=1"));
        assert!(s.contains("UnattendSwitch=Yes"));
        assert!(s.contains("FileSystem=LeaveAlone"));
        assert!(force_winnt_keys(&s).contains("MsDosInitiated=1"));
    }
}
