//! Native prompts for the incremental self-update: the "new version available" question with the
//! changelog, a progress dialog driven by the worker's lock-free progress, and the final result.
//!
//! These are system TaskDialogs (comctl32 v6, available from Windows Vista), the same native family
//! as the shared error-log prompt in `lr_core::windows_diagnostics`, so DPI scaling, keyboard
//! handling and accessibility come from Windows. The worker never touches the UI: the dialog's
//! timer callback reads `UpdateProgress` and closes the dialog once the worker reports completion.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use lr_core::update_manifest::{changelog_text_for_language, ChangelogText};
use windows::core::{HRESULT, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, S_FALSE, S_OK, WPARAM};
use windows::Win32::UI::Controls::{
    TaskDialogIndirect, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TASKDIALOG_BUTTON,
    TASKDIALOG_NOTIFICATIONS, TDCBF_OK_BUTTON, TDE_CONTENT, TDF_ALLOW_DIALOG_CANCELLATION,
    TDF_CALLBACK_TIMER, TDF_POSITION_RELATIVE_TO_WINDOW, TDF_SHOW_PROGRESS_BAR, TDM_CLICK_BUTTON,
    TDM_ENABLE_BUTTON, TDM_SET_ELEMENT_TEXT, TDM_SET_PROGRESS_BAR_POS, TDM_SET_PROGRESS_BAR_RANGE,
    TDN_BUTTON_CLICKED, TDN_CREATED, TDN_TIMER, TD_ERROR_ICON, TD_INFORMATION_ICON,
};
use windows::Win32::UI::WindowsAndMessaging::SendMessageW;

use crate::core::app_update::{
    install_update, RollbackStatus, UpdateFailure, UpdateOffer, UpdatePhase, UpdateProgress,
    UpdateProgressSnapshot, UpdateSuccess,
};

const UPGRADE_BUTTON: i32 = 41_001;
const LATER_BUTTON: i32 = 41_002;
const CANCEL_BUTTON: i32 = 41_003;
const MAX_CHANGELOG_LINES: usize = 30;
const PROGRESS_RANGE: u32 = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateDialogOutcome {
    Declined,
    /// Files were replaced; the caller closes the main window and restarts the program.
    RestartRequired,
    NotInstalled,
}

thread_local! {
    /// Offer waiting until no modal dialog owns the main window (UI thread only).
    static DEFERRED_OFFER: RefCell<Option<UpdateOffer>> = const { RefCell::new(None) };
}

pub fn defer(offer: UpdateOffer) {
    DEFERRED_OFFER.with(|cell| *cell.borrow_mut() = Some(offer));
}

pub fn take_deferred() -> Option<UpdateOffer> {
    DEFERRED_OFFER.with(|cell| cell.borrow_mut().take())
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn changelog_text(offer: &UpdateOffer) -> String {
    let language = crate::utils::i18n::current_language();
    let mut lines: Vec<String> = offer
        .manifest
        .changelog
        .iter()
        .filter_map(|entry| changelog_text_for_language(entry, &language))
        .map(|text| match text {
            ChangelogText::Native(text) => text.to_owned(),
            ChangelogText::NeedsTraditionalConversion(text) => {
                lr_core::traditional_chinese::to_traditional_chinese(text)
            }
        })
        .map(|text| format!("• {text}"))
        .collect();
    if lines.is_empty() {
        return crate::tr!("本次更新没有提供更新日志。");
    }
    if lines.len() > MAX_CHANGELOG_LINES {
        let hidden = lines.len() - MAX_CHANGELOG_LINES;
        lines.truncate(MAX_CHANGELOG_LINES);
        lines.push(crate::tr!("……以及另外 {} 项改进", hidden));
    }
    lines.join("\r\n")
}

fn ask_to_upgrade(owner: HWND, offer: &UpdateOffer) -> bool {
    let title = wide(&crate::tr!("LetRecovery 更新"));
    let instruction = wide(&crate::tr!("发现新版本 {}", offer.manifest.version));
    let content = wide(&format!(
        "{}\r\n\r\n{}\r\n{}",
        crate::tr!("当前版本：{}", offer.current_version),
        crate::tr!("更新内容："),
        changelog_text(offer)
    ));
    let footer = wide(&crate::tr!(
        "升级只下载有变化的文件，失败时会自动恢复原版本。"
    ));
    let upgrade = wide(&crate::tr!("立即升级"));
    let later = wide(&crate::tr!("以后再说"));
    let buttons = [
        TASKDIALOG_BUTTON {
            nButtonID: UPGRADE_BUTTON,
            pszButtonText: PCWSTR(upgrade.as_ptr()),
        },
        TASKDIALOG_BUTTON {
            nButtonID: LATER_BUTTON,
            pszButtonText: PCWSTR(later.as_ptr()),
        },
    ];
    let config = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        dwFlags: TDF_POSITION_RELATIVE_TO_WINDOW | TDF_ALLOW_DIALOG_CANCELLATION,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        Anonymous1: TASKDIALOGCONFIG_0 {
            pszMainIcon: TD_INFORMATION_ICON,
        },
        pszMainInstruction: PCWSTR(instruction.as_ptr()),
        pszContent: PCWSTR(content.as_ptr()),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        nDefaultButton: UPGRADE_BUTTON,
        pszFooter: PCWSTR(footer.as_ptr()),
        ..Default::default()
    };
    let mut pressed = 0;
    // SAFETY: every string and the button array outlive the synchronous call.
    match unsafe { TaskDialogIndirect(&config, Some(&mut pressed), None, None) } {
        Ok(()) => pressed == UPGRADE_BUTTON,
        Err(error) => {
            log::warn!("[UPDATE] 无法显示升级提示: {error}");
            false
        }
    }
}

fn status_text(snapshot: &UpdateProgressSnapshot) -> String {
    use crate::utils::logger::LogManager;
    match snapshot.phase {
        UpdatePhase::Preparing => crate::tr!("正在检查本地文件…"),
        UpdatePhase::Downloading => crate::tr!(
            "正在下载更新文件：{} / {}",
            LogManager::format_size(snapshot.completed_bytes),
            LogManager::format_size(snapshot.total_bytes)
        ),
        UpdatePhase::BuildingImage => crate::tr!("正在更新内置 PE 镜像…"),
        UpdatePhase::VerifyingImage => crate::tr!("正在校验内置 PE 镜像…"),
        UpdatePhase::Applying => crate::tr!("正在替换程序文件，请勿关闭电源…"),
        UpdatePhase::Finished => crate::tr!("升级完成"),
    }
}

struct ProgressContext {
    progress: Arc<UpdateProgress>,
    cancel: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    cancel_requested: bool,
    cancel_disabled: bool,
    /// The task dialog may keep a pointer to the last element text, so it lives here.
    last_text: Vec<u16>,
    last_permille: u32,
}

unsafe fn refresh(hwnd: HWND, context: &mut ProgressContext) {
    let snapshot = context.progress.snapshot();
    let mut permille = snapshot.permille;
    if snapshot.phase == UpdatePhase::VerifyingImage {
        let verified = u32::from(lr_core::wimlib::Wimlib::get_global_progress()).min(100);
        permille = permille.max(880 + verified * 80 / 100);
    }
    if permille != context.last_permille {
        context.last_permille = permille;
        SendMessageW(
            hwnd,
            TDM_SET_PROGRESS_BAR_POS.0 as u32,
            WPARAM(permille as usize),
            LPARAM(0),
        );
    }
    let text = if context.cancel_requested {
        crate::tr!("正在取消，请稍候…")
    } else {
        status_text(&snapshot)
    };
    let encoded = wide(&text);
    if encoded != context.last_text {
        context.last_text = encoded;
        SendMessageW(
            hwnd,
            TDM_SET_ELEMENT_TEXT.0 as u32,
            WPARAM(TDE_CONTENT.0 as usize),
            LPARAM(context.last_text.as_ptr() as isize),
        );
    }
    let committed = matches!(
        snapshot.phase,
        UpdatePhase::Applying | UpdatePhase::Finished
    );
    if committed && !context.cancel_disabled {
        // Files are being swapped: cancelling is no longer possible.
        context.cancel_disabled = true;
        SendMessageW(
            hwnd,
            TDM_ENABLE_BUTTON.0 as u32,
            WPARAM(CANCEL_BUTTON as usize),
            LPARAM(0),
        );
    }
}

unsafe extern "system" fn progress_callback(
    hwnd: HWND,
    message: TASKDIALOG_NOTIFICATIONS,
    wparam: WPARAM,
    _lparam: LPARAM,
    data: isize,
) -> HRESULT {
    // SAFETY: `data` is the `ProgressContext` owned by `run_progress`, which outlives the dialog.
    let context = &mut *(data as *mut ProgressContext);
    match message {
        TDN_CREATED => {
            SendMessageW(
                hwnd,
                TDM_SET_PROGRESS_BAR_RANGE.0 as u32,
                WPARAM(0),
                LPARAM((PROGRESS_RANGE << 16) as isize),
            );
            refresh(hwnd, context);
            S_OK
        }
        TDN_TIMER => {
            refresh(hwnd, context);
            if context.finished.load(Ordering::SeqCst) {
                SendMessageW(
                    hwnd,
                    TDM_CLICK_BUTTON.0 as u32,
                    WPARAM(CANCEL_BUTTON as usize),
                    LPARAM(0),
                );
            }
            S_OK
        }
        TDN_BUTTON_CLICKED => {
            if context.finished.load(Ordering::SeqCst) {
                return S_OK;
            }
            if wparam.0 as i32 == CANCEL_BUTTON && !context.cancel_disabled {
                context.cancel.store(true, Ordering::SeqCst);
                context.cancel_requested = true;
                context.cancel_disabled = true;
                SendMessageW(
                    hwnd,
                    TDM_ENABLE_BUTTON.0 as u32,
                    WPARAM(CANCEL_BUTTON as usize),
                    LPARAM(0),
                );
                refresh(hwnd, context);
            }
            // Stay open until the worker has stopped at a safe point.
            S_FALSE
        }
        _ => S_OK,
    }
}

fn run_progress(owner: HWND, offer: &UpdateOffer) -> Result<UpdateSuccess, UpdateFailure> {
    let progress = Arc::new(UpdateProgress::default());
    let cancel = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let outcome: Arc<Mutex<Option<Result<UpdateSuccess, UpdateFailure>>>> =
        Arc::new(Mutex::new(None));
    let worker = {
        let (progress, cancel, finished, outcome) = (
            progress.clone(),
            cancel.clone(),
            finished.clone(),
            outcome.clone(),
        );
        let offer = offer.clone();
        std::thread::Builder::new()
            .name("lr-app-update".to_owned())
            .spawn(move || {
                let result = install_update(&offer, &progress, &cancel);
                *outcome
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(result);
                finished.store(true, Ordering::SeqCst);
            })
    };
    let worker = match worker {
        Ok(worker) => worker,
        Err(error) => {
            return Err(UpdateFailure::Failed {
                message: format!("{}\r\n{error}", crate::tr!("准备升级失败。")),
                rollback: RollbackStatus::NotNeeded,
            })
        }
    };
    let mut context = ProgressContext {
        progress,
        cancel: cancel.clone(),
        finished,
        cancel_requested: false,
        cancel_disabled: false,
        last_text: Vec::new(),
        last_permille: u32::MAX,
    };
    let title = wide(&crate::tr!("LetRecovery 更新"));
    let instruction = wide(&crate::tr!("正在升级到 {}", offer.manifest.version));
    let content = wide(&crate::tr!("正在检查本地文件…"));
    let cancel_caption = wide(&crate::tr!("取消"));
    let button = TASKDIALOG_BUTTON {
        nButtonID: CANCEL_BUTTON,
        pszButtonText: PCWSTR(cancel_caption.as_ptr()),
    };
    let config = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        dwFlags: TDF_SHOW_PROGRESS_BAR | TDF_CALLBACK_TIMER | TDF_POSITION_RELATIVE_TO_WINDOW,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        Anonymous1: TASKDIALOGCONFIG_0 {
            pszMainIcon: TD_INFORMATION_ICON,
        },
        pszMainInstruction: PCWSTR(instruction.as_ptr()),
        pszContent: PCWSTR(content.as_ptr()),
        cButtons: 1,
        pButtons: &button,
        nDefaultButton: CANCEL_BUTTON,
        pfCallback: Some(progress_callback),
        lpCallbackData: &mut context as *mut ProgressContext as isize,
        ..Default::default()
    };
    // SAFETY: strings, the button and the callback context outlive the synchronous call.
    if let Err(error) = unsafe { TaskDialogIndirect(&config, None, None, None) } {
        // Without a visible dialog the user cannot follow or cancel the work; stop it at the next
        // safe point instead of replacing files silently.
        log::error!("[UPDATE] 无法显示升级进度窗口，已请求停止升级: {error}");
        cancel.store(true, Ordering::SeqCst);
    }
    let _ = worker.join();
    let result = outcome
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    result.unwrap_or_else(|| {
        Err(UpdateFailure::Failed {
            message: crate::tr!("升级线程异常结束。"),
            rollback: RollbackStatus::NotNeeded,
        })
    })
}

fn show_message(owner: HWND, icon: PCWSTR, instruction: &str, content: &str) {
    let title = wide(&crate::tr!("LetRecovery 更新"));
    let instruction = wide(instruction);
    let content = wide(content);
    let config = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        dwFlags: TDF_POSITION_RELATIVE_TO_WINDOW | TDF_ALLOW_DIALOG_CANCELLATION,
        dwCommonButtons: TDCBF_OK_BUTTON,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        Anonymous1: TASKDIALOGCONFIG_0 { pszMainIcon: icon },
        pszMainInstruction: PCWSTR(instruction.as_ptr()),
        pszContent: PCWSTR(content.as_ptr()),
        ..Default::default()
    };
    // SAFETY: every string outlives the synchronous call.
    if let Err(error) = unsafe { TaskDialogIndirect(&config, None, None, None) } {
        log::warn!("[UPDATE] 无法显示升级结果: {error}");
    }
}

/// Asks whether to upgrade, runs the update with progress and reports the result.
pub fn run(owner: HWND, offer: &UpdateOffer) -> UpdateDialogOutcome {
    if !ask_to_upgrade(owner, offer) {
        log::info!("[UPDATE] 用户选择暂不升级到 {}", offer.manifest.version);
        return UpdateDialogOutcome::Declined;
    }
    match run_progress(owner, offer) {
        Ok(success) => {
            let mut content = crate::tr!(
                "LetRecovery 已升级到 {}，将立即重新启动以完成升级。",
                success.version
            );
            if success.preserved_custom_pe {
                content.push_str("\r\n\r\n");
                content.push_str(&crate::tr!(
                    "bin\\pe 中的 PE 镜像已被自定义，已保留该文件，内置 PE 未随本次升级更新。"
                ));
            }
            show_message(
                owner,
                TD_INFORMATION_ICON,
                &crate::tr!("升级完成"),
                &content,
            );
            UpdateDialogOutcome::RestartRequired
        }
        Err(UpdateFailure::Cancelled) => UpdateDialogOutcome::NotInstalled,
        Err(UpdateFailure::Failed { message, rollback }) => {
            let recovery = match rollback {
                RollbackStatus::NotNeeded => {
                    crate::tr!("程序文件没有被修改，可以继续正常使用。")
                }
                RollbackStatus::RolledBack => {
                    crate::tr!("已自动恢复到升级前的版本，程序可以继续正常使用。")
                }
                RollbackStatus::Incomplete(detail) => crate::tr!(
                    "自动恢复未能全部完成：{}\r\n请重新下载完整安装包覆盖安装。",
                    detail
                ),
            };
            show_message(
                owner,
                TD_ERROR_ICON,
                &crate::tr!("升级失败"),
                &format!("{message}\r\n\r\n{recovery}"),
            );
            UpdateDialogOutcome::NotInstalled
        }
    }
}
