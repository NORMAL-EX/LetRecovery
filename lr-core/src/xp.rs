//! Windows XP / 2003 x64 的 GPT+UEFI 部署支持（两端共享）。
//!
//! 背景：原版 XP/2003 既不支持 UEFI 引导，也没有 NVMe/通用 AHCI/USB3 驱动。
//! 但只要使用「已 UEFI 化」的 XP x64 映像（镜像内 `WINDOWS\Boot\EFI\` 自带
//! `bootxp64.efi` + `BCC` 引导库，`system32` 自带 `winload.efi` 与打补丁的
//! `ntoskrnl.exe`/`ntoskrn8.sys`），就能让 XP 走 UEFI/GPT 启动。本模块做两件事：
//!
//! 1. [`write_xp_uefi_gpt_boot`]：复刻社区方案（Gelip `startnet.cmd`）的 UEFI 引导
//!    写入 —— 把映像自带的 `WINDOWS\Boot\EFI` 释放到 ESP，并用 `bcdedit /store` 修正
//!    BCC 里各分区指向，再把 `bootxp64.efi` 落到 UEFI 回退路径 `\EFI\Boot\bootx64.efi`。
//!
//! 2. [`inject_xp_drivers`]：把 NVMe / 通用 AHCI / USB3(xHCI) 驱动**离线注入**到已释放
//!    的 XP 系统。XP 是 NT 5.x，**不能用 DISM** 离线注入；这里走经典手法：拷贝
//!    `.sys`/`.inf` + 在离线 SYSTEM 配置单元里登记 boot-start 服务 + 写
//!    CriticalDeviceDatabase（CDDB）让内核在 PnP 之前就认出启动盘。
//!
//! 设计约束：
//! - 注册表操作复用 [`crate::registry::OfflineRegistry`]（reg.exe 封装）。
//! - SYSTEM 配置单元由**调用方预先加载**（如 PE 端 `apply_advanced_options` 已把它
//!   加载为 `pc-sys`），本模块只在已加载的 hive 键上写，避免「同一 hive 文件二次加载」冲突。
//! - 驱动文件运行时从 `bin\drivers\xp\{ahci,nvme,usb3}\` 读取（调用方传入该根目录）。

use std::path::{Path, PathBuf};

use crate::command::new_command;
use crate::encoding::gbk_to_utf8;
use crate::registry::OfflineRegistry;

/// SCSIAdapter 设备类 GUID（NVMe/AHCI 微端口都属于此类）。
const CLASS_SCSIADAPTER: &str = "{4D36E97B-E325-11CE-BFC1-08002BE10318}";
/// USB 设备类 GUID（xHCI 主控 / USB3 集线器）。
const CLASS_USB: &str = "{36FC9E60-C465-11CF-8056-444553540000}";

/// 单个内核驱动服务的注册信息。
struct SvcSpec {
    /// 服务名（注册到 `...\Services\<name>`）
    name: &'static str,
    /// 主驱动文件名（最终位于 `system32\drivers\<sys>`）
    sys: &'static str,
    /// ServiceType：1 = SERVICE_KERNEL_DRIVER
    type_: u32,
    /// StartType：0 = boot-start（启动盘存储驱动必须），3 = demand（PnP 按需）
    start: u32,
    /// ErrorControl：1 = normal
    error: u32,
    /// LoadOrderGroup
    group: &'static str,
}

/// 一组驱动（一个 `bin\drivers\xp\<subdir>` 子目录）的注入规格。
struct DriverSet {
    /// 子目录名 + 人类可读名
    subdir: &'static str,
    label: &'static str,
    /// 需要登记的服务（按顺序）
    services: &'static [SvcSpec],
    /// CriticalDeviceDatabase 条目：(PCI 标识键名, 绑定的服务名, 设备类 GUID)
    cddb: &'static [(&'static str, &'static str, &'static str)],
}

/// 通用 AHCI（SATA）——始终注入。
const SET_AHCI: DriverSet = DriverSet {
    subdir: "ahci",
    label: "AHCI(SATA)",
    services: &[SvcSpec {
        name: "genahci",
        sys: "genahci.sys",
        type_: 1,
        start: 0,
        error: 1,
        group: "SCSI Miniport",
    }],
    // AHCI 类代码 01-06-01
    cddb: &[("PCI#CC_010601", "genahci", CLASS_SCSIADAPTER)],
};

/// NVMe（标准 NVM Express）。
const SET_NVME: DriverSet = DriverSet {
    subdir: "nvme",
    label: "NVMe",
    services: &[SvcSpec {
        name: "stornvme",
        sys: "stornvme.sys",
        type_: 1,
        start: 0,
        error: 1,
        group: "SCSI Miniport",
    }],
    // NVMe 类代码 01-08-02
    cddb: &[("PCI#CC_010802", "stornvme", CLASS_SCSIADAPTER)],
};

/// USB3 / xHCI（AMD 通用 xHCI 主控 + USB3 集线器）。
const SET_USB3: DriverSet = DriverSet {
    subdir: "usb3",
    label: "USB3(xHCI)",
    services: &[
        SvcSpec { name: "amdxhc", sys: "amdxhc.sys", type_: 1, start: 3, error: 1, group: "Base" },
        SvcSpec { name: "amdhub30", sys: "amdhub30.sys", type_: 1, start: 3, error: 1, group: "Base" },
    ],
    // 通用 xHCI 类代码 0C-03-30，绑定到 amdxhc
    cddb: &[("PCI#CC_0C0330", "amdxhc", CLASS_USB)],
};

/// 把选定的 XP 存储/USB 驱动离线注入到已释放的目标系统。
///
/// - `win_partition`：目标系统盘，如 `"C:"`。
/// - `drivers_xp_dir`：`bin\drivers\xp` 根目录（含 `ahci/`、`nvme/`、`usb3/` 子目录）。
/// - `sys_hive_key`：调用方**已加载**的离线 SYSTEM 配置单元键名（如 `"pc-sys"`，对应
///   `HKLM\pc-sys`）。本函数只在其上写，不负责加载/卸载。
/// - `inject_nvme` / `inject_usb3`：是否注入对应驱动。AHCI 始终注入（不可选）。
///
/// 返回执行日志。文件拷贝失败视为硬错误；注册表写入失败仅记日志不致命
/// （映像可能已自带对应项）。
pub fn inject_xp_drivers(
    win_partition: &str,
    drivers_xp_dir: &Path,
    sys_hive_key: &str,
    inject_nvme: bool,
    inject_usb3: bool,
) -> Result<String, String> {
    let win = win_partition.trim_end_matches('\\');
    let mut log = String::new();
    log.push_str(&format!(
        "[XP-DRV] 开始注入驱动到 {} （源: {}）\n",
        win,
        drivers_xp_dir.display()
    ));

    // 目标目录
    let drivers_dst = format!("{}\\WINDOWS\\system32\\drivers", win);
    let inf_dst = format!("{}\\WINDOWS\\inf", win);
    if let Err(e) = std::fs::create_dir_all(&drivers_dst) {
        return Err(format!("创建目录失败 {}: {}", drivers_dst, e));
    }
    let _ = std::fs::create_dir_all(&inf_dst);

    // 选择要注入的驱动集合：AHCI 始终；NVMe / USB3 可选
    let mut sets: Vec<&DriverSet> = vec![&SET_AHCI];
    if inject_nvme {
        sets.push(&SET_NVME);
    }
    if inject_usb3 {
        sets.push(&SET_USB3);
    }

    for set in sets {
        let src_dir = drivers_xp_dir.join(set.subdir);
        log.push_str(&format!("[XP-DRV] === {} === 源目录 {}\n", set.label, src_dir.display()));
        if !src_dir.exists() {
            // AHCI 缺失也只警告：映像可能已自带；其它盘上若没该控制器则无影响。
            log.push_str(&format!(
                "[XP-DRV] 警告: 驱动目录不存在，跳过 {}（如目标机无此控制器可忽略）\n",
                set.label
            ));
            continue;
        }

        // 1) 拷贝文件：所有 .sys -> system32\drivers（含 x64\ 子目录，扁平化）；所有 .inf -> WINDOWS\inf
        let mut copied_sys = 0usize;
        if let Err(e) = copy_drivers_recursive(&src_dir, &drivers_dst, &inf_dst, &mut copied_sys, &mut log) {
            return Err(format!("拷贝 {} 驱动文件失败: {}", set.label, e));
        }
        if copied_sys == 0 {
            log.push_str(&format!(
                "[XP-DRV] 警告: {} 目录里没有 .sys 文件，跳过注册\n",
                set.label
            ));
            continue;
        }

        // 2) 登记服务到 ControlSet001（current），并尽力同步到 ControlSet002
        for cs in ["ControlSet001", "ControlSet002"] {
            let must = cs == "ControlSet001"; // 001 失败才算问题，002 仅尽力
            for svc in set.services {
                let key = format!("HKLM\\{}\\{}\\Services\\{}", sys_hive_key, cs, svc.name);
                let r = register_service(&key, svc);
                match r {
                    Ok(_) => {
                        if must {
                            log.push_str(&format!(
                                "[XP-DRV] 登记服务 {}\\Services\\{} (Start={}, Group={})\n",
                                cs, svc.name, svc.start, svc.group
                            ));
                        }
                    }
                    Err(e) => {
                        if must {
                            log.push_str(&format!("[XP-DRV] 警告: 登记服务 {} 失败: {}\n", svc.name, e));
                        }
                    }
                }
            }
            // 3) CriticalDeviceDatabase：让内核在 PnP 前就为该 PCI 设备加载驱动
            for (pci, svc_name, class) in set.cddb {
                let key = format!(
                    "HKLM\\{}\\{}\\Control\\CriticalDeviceDatabase\\{}",
                    sys_hive_key, cs, pci
                );
                let ok = OfflineRegistry::create_key(&key).is_ok()
                    && OfflineRegistry::set_string(&key, "Service", svc_name).is_ok()
                    && OfflineRegistry::set_string(&key, "ClassGUID", class).is_ok();
                if must {
                    if ok {
                        log.push_str(&format!(
                            "[XP-DRV] 写入 CDDB {} -> {} ({})\n",
                            pci, svc_name, cs
                        ));
                    } else {
                        log.push_str(&format!("[XP-DRV] 警告: 写入 CDDB {} 失败\n", pci));
                    }
                }
            }
        }
    }

    log.push_str("[XP-DRV] 驱动注入完成。\n");
    Ok(log)
}

/// 登记单个内核驱动服务（Type/Start/ErrorControl/ImagePath/Group）。
fn register_service(key: &str, svc: &SvcSpec) -> anyhow::Result<()> {
    OfflineRegistry::create_key(key)?;
    OfflineRegistry::set_dword(key, "Type", svc.type_)?;
    OfflineRegistry::set_dword(key, "Start", svc.start)?;
    OfflineRegistry::set_dword(key, "ErrorControl", svc.error)?;
    // XP 标准：ImagePath 用相对 %SystemRoot% 的 REG_EXPAND_SZ
    OfflineRegistry::set_expand_string(key, "ImagePath", &format!("System32\\DRIVERS\\{}", svc.sys))?;
    OfflineRegistry::set_string(key, "Group", svc.group)?;
    Ok(())
}

/// 递归拷贝驱动文件：`.sys` -> `drivers_dst`（扁平化），`.inf` -> `inf_dst`，`.cat` -> `drivers_dst`。
fn copy_drivers_recursive(
    src: &Path,
    drivers_dst: &str,
    inf_dst: &str,
    copied_sys: &mut usize,
    log: &mut String,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            copy_drivers_recursive(&path, drivers_dst, inf_dst, copied_sys, log)?;
            continue;
        }
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let lower = name.to_ascii_lowercase();
        let dst = if lower.ends_with(".sys") {
            *copied_sys += 1;
            format!("{}\\{}", drivers_dst, name)
        } else if lower.ends_with(".inf") {
            format!("{}\\{}", inf_dst, name)
        } else if lower.ends_with(".cat") {
            // 目录数据库放到 drivers 旁；XP 离线场景下不强制校验，仅备用
            format!("{}\\{}", drivers_dst, name)
        } else {
            continue;
        };
        match std::fs::copy(&path, &dst) {
            Ok(_) => log.push_str(&format!("[XP-DRV]   拷贝 {} -> {}\n", name, dst)),
            Err(e) => log.push_str(&format!("[XP-DRV]   警告: 拷贝 {} 失败: {}\n", name, e)),
        }
    }
    Ok(())
}

/// 为已释放的 UEFI 化 XP 系统写入 UEFI/GPT 引导。
///
/// 复刻 Gelip `startnet.cmd` 的 `:manual` 修复逻辑：
/// 1. 把映像自带的 `{win}\WINDOWS\Boot\EFI\*` 释放到 `{esp}\EFI\*`；
/// 2. 把 `bootxp64.efi` 复制到 UEFI 回退路径 `{esp}\EFI\Boot\bootx64.efi`（无需改 NVRAM 即可被固件自动引导）；
/// 3. 用 `bcdedit /store {esp}\EFI\Microsoft\Boot\BCC` 把 `{bootmgr}` 指向 ESP、把 `{ntldr}` 及两个
///    自定义 XP 加载项 GUID 指向系统盘。
///
/// - `win_partition`：XP 系统盘，如 `"C:"`。
/// - `esp_letter`：已挂载的 ESP 盘符，如 `"S:"`（调用方负责查找/挂载）。
/// - `bcdedit_path`：bcdedit.exe 路径（PE 端通常为系统自带）。
///
/// 前置条件：映像必须是「UEFI 化」的 XP（含 `WINDOWS\Boot\EFI\Microsoft\Boot\bootxp64.efi` 与 `BCC`）。
/// 若映像缺这些文件，返回 Err 让调用方回退到 Legacy 引导并告警。
pub fn write_xp_uefi_gpt_boot(
    win_partition: &str,
    esp_letter: &str,
    bcdedit_path: &Path,
) -> Result<String, String> {
    let win = win_partition.trim_end_matches('\\');
    let esp = esp_letter.trim_end_matches('\\');
    let mut log = String::new();

    // 0) 校验映像是否自带 UEFI 引导文件
    let img_efi = format!("{}\\WINDOWS\\Boot\\EFI", win);
    let img_bootxp = format!("{}\\Microsoft\\Boot\\bootxp64.efi", img_efi);
    let img_bcc = format!("{}\\Microsoft\\Boot\\BCC", img_efi);
    if !Path::new(&img_bootxp).exists() || !Path::new(&img_bcc).exists() {
        return Err(format!(
            "映像不含 UEFI 引导文件（需要 {} 与 {}）。该映像可能不是 UEFI 化的 XP，无法 UEFI/GPT 启动。",
            img_bootxp, img_bcc
        ));
    }
    log.push_str(&format!("[XP-UEFI] 映像 UEFI 引导文件就绪: {}\n", img_efi));

    // 1) 把 WINDOWS\Boot\EFI\* 释放到 {esp}\EFI\*
    let esp_efi = format!("{}\\EFI", esp);
    if let Err(e) = copy_tree(Path::new(&img_efi), Path::new(&esp_efi)) {
        return Err(format!("复制 EFI 引导文件到 ESP 失败: {}", e));
    }
    log.push_str(&format!("[XP-UEFI] 已释放引导文件到 {}\n", esp_efi));

    // 2) bootxp64.efi -> {esp}\EFI\Boot\bootx64.efi（UEFI 回退路径，固件自动引导）
    let esp_boot_dir = format!("{}\\EFI\\Boot", esp);
    let _ = std::fs::create_dir_all(&esp_boot_dir);
    let fallback = format!("{}\\bootx64.efi", esp_boot_dir);
    let src_bootxp = format!("{}\\EFI\\Microsoft\\Boot\\bootxp64.efi", esp);
    match std::fs::copy(&src_bootxp, &fallback) {
        Ok(_) => log.push_str(&format!("[XP-UEFI] 已写入 UEFI 回退引导 {}\n", fallback)),
        Err(e) => log.push_str(&format!("[XP-UEFI] 警告: 写回退引导失败 {}: {}\n", fallback, e)),
    }

    // 3) bcdedit /store BCC：修正各分区指向（GUID 来自映像作者预置的 BCC）
    let store = format!("{}\\EFI\\Microsoft\\Boot\\BCC", esp);
    // (对象, 元素, 值) —— 元素 custom:21000001 是 XP 加载项的「OS 设备」元素
    let cmds: &[(&str, &str, String)] = &[
        ("{bootmgr}", "device", format!("partition={}", esp)),
        ("{ntldr}", "device", format!("partition={}", win)),
        ("{ntldr}", "custom:21000001", format!("partition={}", win)),
        ("{9eb8f329-85ae-4953-aba6-2c7d803aa4fe}", "device", format!("partition={}", win)),
        ("{9eb8f329-85ae-4953-aba6-2c7d803aa4fe}", "custom:21000001", format!("partition={}", win)),
        ("{0ef2423f-74c2-4688-b906-99c3dc77d8ba}", "device", format!("partition={}", win)),
        ("{0ef2423f-74c2-4688-b906-99c3dc77d8ba}", "custom:21000001", format!("partition={}", win)),
    ];
    for (obj, elem, val) in cmds {
        let out = new_command(bcdedit_path)
            .args(["/store", &store, "/set", obj, elem, val])
            .output();
        match out {
            Ok(o) => {
                if o.status.success() {
                    log.push_str(&format!("[XP-UEFI] bcdedit /set {} {} {}\n", obj, elem, val));
                } else {
                    log.push_str(&format!(
                        "[XP-UEFI] 警告: bcdedit /set {} {} {} 返回非 0: {}\n",
                        obj, elem, val,
                        gbk_to_utf8(&o.stdout).trim()
                    ));
                }
            }
            Err(e) => {
                return Err(format!("执行 bcdedit 失败（{:?}）: {}", bcdedit_path, e));
            }
        }
    }

    // 4) 把（已修正的）BCC 也放一份到 \EFI\Boot\，以防 bootxp64 相对自身目录查找存储
    let bcc_fallback = format!("{}\\BCC", esp_boot_dir);
    match std::fs::copy(&store, &bcc_fallback) {
        Ok(_) => log.push_str(&format!("[XP-UEFI] 已同步 BCC 到 {}\n", bcc_fallback)),
        Err(e) => log.push_str(&format!("[XP-UEFI] 警告: 同步 BCC 到回退目录失败: {}\n", e)),
    }

    log.push_str("[XP-UEFI] UEFI/GPT 引导写入完成。固件将经 \\EFI\\Boot\\bootx64.efi 引导 XP。\n");
    Ok(log)
}

/// 递归复制目录树（保留结构）。
fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            // 覆盖式拷贝
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// 解析 `bin\drivers\xp` 根目录（相对某个 exe/bin 目录）。
///
/// 供两端复用：`base_bin_dir` 传 `get_exe_dir()\bin` 或等价目录。
pub fn xp_drivers_dir(base_bin_dir: &Path) -> PathBuf {
    base_bin_dir.join("drivers").join("xp")
}
