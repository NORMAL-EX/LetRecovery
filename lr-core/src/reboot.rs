//! PE 环境通过经过权限与返回值核对的 Win32 shutdown API 请求真实系统重启。

/// 请求本机立即重启。WinPE 没有需要保护的交互式用户文档，因此使用明确的自动化
/// force-apps-closed 边界，避免 shell 进程把已经完成的安装无限期卡在 PE 桌面。
pub fn reboot_pe() {
    log::info!("正在通过 Win32 shutdown API 请求系统重启...");
    match crate::windows_shutdown::schedule_restart_for_automation(
        0,
        "LetRecovery PE completed the requested operation; Windows will restart now.",
    ) {
        Ok(()) => log::info!("系统已接受立即重启请求"),
        Err(error) => {
            // Keep the completed PE session visible so the user can restart manually. Killing the
            // PE shell is not a reboot and can strand Winlogon on an initialization-error dialog.
            log::error!("系统拒绝重启请求；保留 PE 会话供手动处理: {error:#}");
        }
    }
}
