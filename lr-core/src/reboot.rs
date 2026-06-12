//! PE 环境结束 pecmd.exe（两端共享）。
//!
//! 只尝试结束 pecmd.exe 进程，不执行系统重启。

/// 尝试结束 pecmd.exe 进程（不重启系统）。
pub fn reboot_pe() {
    log::info!("正在尝试结束pecmd.exe进程...");

    #[cfg(windows)]
    {
        if kill_pecmd_winapi() {
            log::info!("已成功终止pecmd.exe进程");
        } else {
            log::warn!("终止pecmd.exe进程失败或进程不存在");
        }
    }

    #[cfg(not(windows))]
    {
        log::warn!("非Windows环境，无法终止pecmd.exe");
    }
}

/// 使用 Windows API 终止 pecmd.exe 进程
#[cfg(windows)]
fn kill_pecmd_winapi() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

    unsafe {
        let snapshot = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(h) => h,
            Err(e) => {
                log::error!("CreateToolhelp32Snapshot失败: {:?}", e);
                return false;
            }
        };

        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        let mut found = false;

        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let process_name: String = entry
                    .szExeFile
                    .iter()
                    .take_while(|&&c| c != 0)
                    .map(|&c| char::from_u32(c as u32).unwrap_or('?'))
                    .collect();

                if process_name.eq_ignore_ascii_case("pecmd.exe") {
                    log::info!("找到pecmd.exe进程, PID: {}", entry.th32ProcessID);

                    if let Ok(process_handle) =
                        OpenProcess(PROCESS_TERMINATE, false, entry.th32ProcessID)
                    {
                        if TerminateProcess(process_handle, 0).is_ok() {
                            log::info!("成功终止pecmd.exe (PID: {})", entry.th32ProcessID);
                            found = true;
                        } else {
                            log::warn!("TerminateProcess失败");
                        }
                        let _: Result<(), _> = CloseHandle(process_handle);
                    } else {
                        log::warn!("OpenProcess失败");
                    }
                }

                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }

        let _: Result<(), _> = CloseHandle(snapshot);
        found
    }
}
