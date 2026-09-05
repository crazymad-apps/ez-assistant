//! 原生窗口、菜单、托盘与 Desktop 退出语义的单一协调入口。

use crate::user_terminal::{TerminalError, UserTerminalManager};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, Emitter as _, Manager as _, Runtime, State, Window, WindowEvent,
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::TrayIconBuilder,
};
use tauri_plugin_dialog::{
    DialogExt as _, MessageDialogButtons, MessageDialogKind, MessageDialogResult,
};
use thiserror::Error;

use crate::desktop_preferences::{DesktopCloseBehavior, load_close_behavior};
use crate::runtime_bootstrap::RuntimeBootstrapCoordinator;

const EVENT_LIFECYCLE_INTENT: &str = "desktop://lifecycle-intent";
const EVENT_NATIVE_RUNTIME_MUTATION: &str = "desktop://native-runtime-mutation";
const EVENT_WINDOW_MAXIMIZED: &str = "desktop://window-maximized";
const APP_SHOW_WINDOW: &str = "desktop.app.show-window";
const APP_RUNTIME_STATUS: &str = "desktop.app.runtime-status";
const APP_RESTART_RUNTIME: &str = "desktop.app.restart-runtime";
const APP_STOP_RUNTIME: &str = "desktop.app.stop-runtime";
const APP_QUIT_DESKTOP: &str = "desktop.app.quit";
const TRAY_SHOW_WINDOW: &str = "desktop.tray.show-window";
const TRAY_RUNTIME_STATUS: &str = "desktop.tray.runtime-status";
const TRAY_RESTART_RUNTIME: &str = "desktop.tray.restart-runtime";
const TRAY_STOP_RUNTIME: &str = "desktop.tray.stop-runtime";
const TRAY_QUIT_DESKTOP: &str = "desktop.tray.quit";
const STOP_RUNTIME_BUTTON: &str = "停止 Runtime";
const RESTART_RUNTIME_BUTTON: &str = "重启 Runtime";
const QUIT_DESKTOP_BUTTON: &str = "退出桌面客户端";
const QUIT_AND_STOP_BUTTON: &str = "退出并停止 Runtime";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NativeRuntimeState {
    Connecting,
    Connected,
    Reconnecting,
    Disconnected,
    Stopping,
    Restarting,
    Stopped,
}

impl NativeRuntimeState {
    fn label(self) -> &'static str {
        match self {
            Self::Connecting => "Runtime：连接中",
            Self::Connected => "Runtime：已连接",
            Self::Reconnecting => "Runtime：重连中",
            Self::Disconnected => "Runtime：已断开",
            Self::Stopping => "Runtime：停止中",
            Self::Restarting => "Runtime：重启中",
            Self::Stopped => "Runtime：已停止",
        }
    }

    fn can_stop(self) -> bool {
        matches!(self, Self::Connected | Self::Reconnecting)
    }

    fn can_restart(self) -> bool {
        !matches!(self, Self::Stopping | Self::Restarting)
    }

    fn accepts(self, next: Self) -> bool {
        !matches!((self, next), (Self::Connected, Self::Connecting))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DesktopLifecycleIntent {
    QuitDesktop,
    StopRuntime,
    RestartRuntime,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
pub(crate) struct NativeRuntimeImpact {
    active_runs: usize,
    queued_inputs: usize,
    pending_approvals: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NativeMenuAction {
    ShowWindow,
    WebIntent(DesktopLifecycleIntent),
    NativeIntent(DesktopLifecycleIntent),
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum NativeRuntimeMutationKind {
    Stop,
    Restart,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
enum NativeRuntimeMutationEvent {
    Preparing {
        kind: NativeRuntimeMutationKind,
    },
    Finished {
        kind: NativeRuntimeMutationKind,
        succeeded: bool,
    },
}

#[derive(Debug, Error, Serialize)]
#[error("desktop lifecycle action failed")]
pub(crate) enum DesktopLifecycleError {
    #[error("main window is unavailable")]
    MainWindowUnavailable,
    #[error("native window action failed")]
    WindowActionFailed,
}

#[derive(Clone)]
struct NativeRuntimeItems {
    status: Vec<MenuItem<tauri::Wry>>,
    stop: Vec<MenuItem<tauri::Wry>>,
    restart: Vec<MenuItem<tauri::Wry>>,
}

pub(crate) struct DesktopLifecycleCoordinator {
    allow_exit: AtomicBool,
    native_action_pending: AtomicBool,
    pending_intent: Mutex<Option<DesktopLifecycleIntent>>,
    native_runtime_impact: Mutex<NativeRuntimeImpact>,
    native_runtime_state: Mutex<NativeRuntimeState>,
    native_runtime_items: Mutex<Option<NativeRuntimeItems>>,
}

impl DesktopLifecycleCoordinator {
    pub(crate) fn new() -> Self {
        Self {
            allow_exit: AtomicBool::new(false),
            native_action_pending: AtomicBool::new(false),
            pending_intent: Mutex::new(None),
            native_runtime_impact: Mutex::new(NativeRuntimeImpact::default()),
            native_runtime_state: Mutex::new(NativeRuntimeState::Connecting),
            native_runtime_items: Mutex::new(None),
        }
    }

    pub(crate) fn install(&self, app_handle: &AppHandle) -> tauri::Result<()> {
        let application_status = MenuItem::with_id(
            app_handle,
            APP_RUNTIME_STATUS,
            NativeRuntimeState::Connecting.label(),
            false,
            None::<&str>,
        )?;
        let application_restart = MenuItem::with_id(
            app_handle,
            APP_RESTART_RUNTIME,
            "重启 Runtime…",
            true,
            None::<&str>,
        )?;
        let application_stop = MenuItem::with_id(
            app_handle,
            APP_STOP_RUNTIME,
            "停止 Runtime…",
            false,
            None::<&str>,
        )?;
        let application_show = MenuItem::with_id(
            app_handle,
            APP_SHOW_WINDOW,
            "显示主窗口",
            true,
            None::<&str>,
        )?;
        let application_quit = MenuItem::with_id(
            app_handle,
            APP_QUIT_DESKTOP,
            "退出桌面客户端…",
            true,
            None::<&str>,
        )?;
        let runtime_menu = Submenu::with_items(
            app_handle,
            "Runtime",
            true,
            &[
                &application_status,
                &PredefinedMenuItem::separator(app_handle)?,
                &application_show,
                &application_restart,
                &application_stop,
                &PredefinedMenuItem::separator(app_handle)?,
                &application_quit,
            ],
        )?;
        let application_menu = Menu::default(app_handle)?;
        application_menu.append(&runtime_menu)?;
        app_handle.set_menu(application_menu)?;

        let tray_status = MenuItem::with_id(
            app_handle,
            TRAY_RUNTIME_STATUS,
            NativeRuntimeState::Connecting.label(),
            false,
            None::<&str>,
        )?;
        let tray_show = MenuItem::with_id(
            app_handle,
            TRAY_SHOW_WINDOW,
            "显示主窗口",
            true,
            None::<&str>,
        )?;
        let tray_restart = MenuItem::with_id(
            app_handle,
            TRAY_RESTART_RUNTIME,
            "重启 Runtime…",
            true,
            None::<&str>,
        )?;
        let tray_stop = MenuItem::with_id(
            app_handle,
            TRAY_STOP_RUNTIME,
            "停止 Runtime…",
            false,
            None::<&str>,
        )?;
        let tray_quit = MenuItem::with_id(
            app_handle,
            TRAY_QUIT_DESKTOP,
            "退出桌面客户端…",
            true,
            None::<&str>,
        )?;
        let tray_menu = Menu::with_items(
            app_handle,
            &[
                &tray_status,
                &PredefinedMenuItem::separator(app_handle)?,
                &tray_show,
                &tray_restart,
                &tray_stop,
                &PredefinedMenuItem::separator(app_handle)?,
                &tray_quit,
            ],
        )?;
        let mut tray = TrayIconBuilder::with_id("ez-assistant-main")
            .menu(&tray_menu)
            .tooltip("EZ Assistant")
            .show_menu_on_left_click(true)
            .on_menu_event(handle_menu_event);
        #[cfg(target_os = "macos")]
        {
            let icon = Image::from_bytes(include_bytes!("../icons/macos/tray-icon.png"))?;
            tray = tray.icon(icon).icon_as_template(true);
        }
        #[cfg(not(target_os = "macos"))]
        if let Some(icon) = app_handle.default_window_icon().cloned() {
            tray = tray.icon(icon);
        }
        tray.build(app_handle)?;

        if let Ok(mut items) = self.native_runtime_items.lock() {
            *items = Some(NativeRuntimeItems {
                status: vec![application_status, tray_status],
                stop: vec![application_stop, tray_stop],
                restart: vec![application_restart, tray_restart],
            });
        }
        Ok(())
    }

    pub(crate) fn handle_window_event<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        window: &Window<R>,
        event: &WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = apply_close_behavior(app, load_close_behavior(app));
            }
            WindowEvent::Resized(_) => {
                if let Ok(maximized) = window.is_maximized() {
                    let _ = app.emit(EVENT_WINDOW_MAXIMIZED, maximized);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn should_prevent_exit(&self) -> bool {
        !self.allow_exit.load(Ordering::SeqCst)
    }

    fn queue_intent(&self, intent: DesktopLifecycleIntent) {
        if let Ok(mut pending_intent) = self.pending_intent.lock() {
            *pending_intent = Some(intent);
        }
    }

    fn take_pending_intent(&self) -> Option<DesktopLifecycleIntent> {
        self.pending_intent.lock().ok()?.take()
    }

    fn begin_native_action(&self) -> bool {
        self.native_action_pending
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    fn finish_native_action(&self) {
        self.native_action_pending.store(false, Ordering::SeqCst);
    }

    fn allow_exit(&self) {
        self.allow_exit.store(true, Ordering::SeqCst);
    }

    fn runtime_impact(&self) -> NativeRuntimeImpact {
        self.native_runtime_impact
            .lock()
            .map(|impact| *impact)
            .unwrap_or_default()
    }

    fn update_runtime_impact(&self, impact: NativeRuntimeImpact) {
        if let Ok(mut current_impact) = self.native_runtime_impact.lock() {
            *current_impact = impact;
        }
    }

    pub(crate) fn update_runtime_state(&self, state: NativeRuntimeState) {
        let Ok(mut current_state) = self.native_runtime_state.lock() else {
            return;
        };
        if !current_state.accepts(state) {
            return;
        }
        *current_state = state;
        drop(current_state);

        // Tauri menu mutations may synchronously hop to the main thread. Never
        // hold the coordinator mutex while calling them: a WebView invoke can
        // otherwise re-enter this method on the main thread and deadlock while
        // the worker is waiting for that same main thread to update the menu.
        let items = {
            let Ok(items) = self.native_runtime_items.lock() else {
                return;
            };
            items.clone()
        };
        let Some(items) = items else {
            return;
        };
        for item in &items.status {
            let _ = item.set_text(state.label());
        }
        for item in &items.stop {
            let _ = item.set_enabled(state.can_stop());
        }
        for item in &items.restart {
            let _ = item.set_enabled(state.can_restart());
        }
    }
}

fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, event: tauri::menu::MenuEvent) {
    match native_menu_action(event.id().as_ref()) {
        Some(NativeMenuAction::ShowWindow) => {
            let _ = show_main_window(app);
        }
        Some(NativeMenuAction::WebIntent(intent)) => request_intent(app, intent),
        Some(NativeMenuAction::NativeIntent(intent)) => request_native_intent(app, intent),
        None => {}
    }
}

fn native_menu_action(id: &str) -> Option<NativeMenuAction> {
    match id {
        APP_SHOW_WINDOW | TRAY_SHOW_WINDOW => Some(NativeMenuAction::ShowWindow),
        APP_STOP_RUNTIME => Some(NativeMenuAction::WebIntent(
            DesktopLifecycleIntent::StopRuntime,
        )),
        APP_RESTART_RUNTIME => Some(NativeMenuAction::WebIntent(
            DesktopLifecycleIntent::RestartRuntime,
        )),
        APP_QUIT_DESKTOP => Some(NativeMenuAction::WebIntent(
            DesktopLifecycleIntent::QuitDesktop,
        )),
        TRAY_STOP_RUNTIME => Some(NativeMenuAction::NativeIntent(
            DesktopLifecycleIntent::StopRuntime,
        )),
        TRAY_RESTART_RUNTIME => Some(NativeMenuAction::NativeIntent(
            DesktopLifecycleIntent::RestartRuntime,
        )),
        TRAY_QUIT_DESKTOP => Some(NativeMenuAction::NativeIntent(
            DesktopLifecycleIntent::QuitDesktop,
        )),
        _ => None,
    }
}

fn request_native_intent<R: Runtime>(app: &AppHandle<R>, intent: DesktopLifecycleIntent) {
    let coordinator = app.state::<DesktopLifecycleCoordinator>();
    if !coordinator.begin_native_action() {
        return;
    }
    let impact = coordinator.runtime_impact();
    match intent {
        DesktopLifecycleIntent::StopRuntime => {
            let app = app.clone();
            app.dialog()
                .message(runtime_impact_message(
                    impact,
                    "停止后，未完成的工作会被中断。",
                ))
                .title("停止 Runtime？")
                .kind(MessageDialogKind::Warning)
                .buttons(MessageDialogButtons::OkCancelCustom(
                    STOP_RUNTIME_BUTTON.to_owned(),
                    "取消".to_owned(),
                ))
                .show(move |confirmed| {
                    if confirmed {
                        spawn_runtime_action(app, DesktopLifecycleIntent::StopRuntime, false);
                    } else {
                        app.state::<DesktopLifecycleCoordinator>()
                            .finish_native_action();
                    }
                });
        }
        DesktopLifecycleIntent::RestartRuntime => {
            let app = app.clone();
            app.dialog()
                .message(runtime_impact_message(
                    impact,
                    "重启期间，未完成的工作会被中断。",
                ))
                .title("重启 Runtime？")
                .kind(MessageDialogKind::Warning)
                .buttons(MessageDialogButtons::OkCancelCustom(
                    RESTART_RUNTIME_BUTTON.to_owned(),
                    "取消".to_owned(),
                ))
                .show(move |confirmed| {
                    if confirmed {
                        spawn_runtime_action(app, DesktopLifecycleIntent::RestartRuntime, false);
                    } else {
                        app.state::<DesktopLifecycleCoordinator>()
                            .finish_native_action();
                    }
                });
        }
        DesktopLifecycleIntent::QuitDesktop => {
            let app = app.clone();
            app.dialog()
                .message(
                    "退出将关闭全部终端及其中运行的进程；Runtime 默认继续运行。\n\n如需同时停止 Runtime，请选择“退出并停止 Runtime”。",
                )
                .title("退出桌面客户端？")
                .kind(MessageDialogKind::Warning)
                .buttons(MessageDialogButtons::YesNoCancelCustom(
                    QUIT_DESKTOP_BUTTON.to_owned(),
                    QUIT_AND_STOP_BUTTON.to_owned(),
                    "取消".to_owned(),
                ))
                .show_with_result(move |result| match result {
                    MessageDialogResult::Yes => {
                        spawn_quit(app);
                    }
                    MessageDialogResult::No => {
                        spawn_runtime_action(app, DesktopLifecycleIntent::StopRuntime, true);
                    }
                    MessageDialogResult::Custom(value) if value == QUIT_DESKTOP_BUTTON => {
                        spawn_quit(app);
                    }
                    MessageDialogResult::Custom(value) if value == QUIT_AND_STOP_BUTTON => {
                        spawn_runtime_action(app, DesktopLifecycleIntent::StopRuntime, true);
                    }
                    _ => app
                        .state::<DesktopLifecycleCoordinator>()
                        .finish_native_action(),
                });
        }
    }
}

// 托盘/原生确认路径没有前端编排，仍须等 PTY 回收后才放行退出。
fn spawn_quit<R: Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        match app.state::<UserTerminalManager>().shutdown().await {
            Ok(()) => {
                let coordinator = app.state::<DesktopLifecycleCoordinator>();
                coordinator.finish_native_action();
                coordinator.allow_exit();
                app.exit(0);
            }
            Err(error) => report_quit_failure(&app, error),
        }
    });
}

fn report_quit_failure<R: Runtime>(app: &AppHandle<R>, error: TerminalError) {
    app.state::<DesktopLifecycleCoordinator>()
        .finish_native_action();
    app.dialog()
        .message(error.to_string())
        .title("终端清理失败，尚未退出")
        .kind(MessageDialogKind::Error)
        .buttons(MessageDialogButtons::Ok)
        .show(|_| {});
}

fn runtime_impact_message(impact: NativeRuntimeImpact, consequence: &str) -> String {
    format!(
        "活动 Run：{}\n排队输入：{}\n待审批：{}\n\n{consequence}",
        impact.active_runs, impact.queued_inputs, impact.pending_approvals
    )
}

fn spawn_runtime_action<R: Runtime>(
    app: AppHandle<R>,
    intent: DesktopLifecycleIntent,
    quit_after_success: bool,
) {
    tauri::async_runtime::spawn(async move {
        if quit_after_success
            && let Err(error) = app.state::<UserTerminalManager>().shutdown().await
        {
            report_quit_failure(&app, error);
            return;
        }
        let runtime = app.state::<RuntimeBootstrapCoordinator>().inner().clone();
        let mutation_kind = match intent {
            DesktopLifecycleIntent::StopRuntime => NativeRuntimeMutationKind::Stop,
            DesktopLifecycleIntent::RestartRuntime => NativeRuntimeMutationKind::Restart,
            DesktopLifecycleIntent::QuitDesktop => return,
        };
        let _ = app.emit(
            EVENT_NATIVE_RUNTIME_MUTATION,
            NativeRuntimeMutationEvent::Preparing {
                kind: mutation_kind,
            },
        );
        let result = match intent {
            DesktopLifecycleIntent::StopRuntime => {
                app.state::<DesktopLifecycleCoordinator>()
                    .update_runtime_state(NativeRuntimeState::Stopping);
                runtime
                    .shutdown()
                    .await
                    .map(|_| NativeRuntimeState::Stopped)
            }
            DesktopLifecycleIntent::RestartRuntime => {
                app.state::<DesktopLifecycleCoordinator>()
                    .update_runtime_state(NativeRuntimeState::Restarting);
                runtime
                    .restart()
                    .await
                    .map(|_| NativeRuntimeState::Connected)
            }
            DesktopLifecycleIntent::QuitDesktop => return,
        };
        let _ = app.emit(
            EVENT_NATIVE_RUNTIME_MUTATION,
            NativeRuntimeMutationEvent::Finished {
                kind: mutation_kind,
                succeeded: result.is_ok(),
            },
        );
        let coordinator = app.state::<DesktopLifecycleCoordinator>();
        coordinator.finish_native_action();
        match result {
            Ok(state) => {
                coordinator.update_runtime_state(state);
                if quit_after_success {
                    coordinator.allow_exit();
                    app.exit(0);
                }
            }
            Err(error) => {
                if quit_after_success {
                    app.state::<UserTerminalManager>().resume().await;
                }
                coordinator.update_runtime_state(NativeRuntimeState::Disconnected);
                let action = match intent {
                    DesktopLifecycleIntent::StopRuntime => "停止 Runtime",
                    DesktopLifecycleIntent::RestartRuntime => "重启 Runtime",
                    DesktopLifecycleIntent::QuitDesktop => "执行桌面操作",
                };
                app.dialog()
                    .message(error.to_string())
                    .title(format!("{action}失败"))
                    .kind(MessageDialogKind::Error)
                    .buttons(MessageDialogButtons::Ok)
                    .show(|_| {});
            }
        }
    });
}

pub(crate) fn request_intent<R: Runtime>(app: &AppHandle<R>, intent: DesktopLifecycleIntent) {
    app.state::<DesktopLifecycleCoordinator>()
        .queue_intent(intent);
    let _ = show_main_window(app);
    let _ = app.emit(EVENT_LIFECYCLE_INTENT, intent);
}

// 加入浏览器 child WebView 后窗口不再是单 WebviewWindow；窗口操作必须按 Window 身份查找。
fn show_main_window<R: Runtime>(app: &AppHandle<R>) -> Result<(), DesktopLifecycleError> {
    #[cfg(target_os = "macos")]
    app.set_dock_visibility(true)
        .map_err(|_| DesktopLifecycleError::WindowActionFailed)?;
    let window = app
        .get_window("main")
        .ok_or(DesktopLifecycleError::MainWindowUnavailable)?;
    window
        .show()
        .and_then(|_| window.unminimize())
        .and_then(|_| window.set_focus())
        .map_err(|_| DesktopLifecycleError::WindowActionFailed)
}

fn hide_main_window<R: Runtime>(app: &AppHandle<R>) -> Result<(), DesktopLifecycleError> {
    let window = app
        .get_window("main")
        .ok_or(DesktopLifecycleError::MainWindowUnavailable)?;
    window
        .hide()
        .map_err(|_| DesktopLifecycleError::WindowActionFailed)?;
    #[cfg(target_os = "macos")]
    app.set_dock_visibility(false)
        .map_err(|_| DesktopLifecycleError::WindowActionFailed)?;
    Ok(())
}

fn apply_close_behavior<R: Runtime>(
    app: &AppHandle<R>,
    behavior: DesktopCloseBehavior,
) -> Result<(), DesktopLifecycleError> {
    match behavior {
        DesktopCloseBehavior::HideToTray => hide_main_window(app),
        DesktopCloseBehavior::QuitDesktop => {
            request_native_intent(app, DesktopLifecycleIntent::QuitDesktop);
            Ok(())
        }
    }
}

#[tauri::command]
pub(crate) fn desktop_platform() -> &'static str {
    #[cfg(target_os = "macos")]
    return "macos";
    #[cfg(target_os = "linux")]
    return "linux";
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    "unsupported"
}

#[tauri::command]
pub(crate) fn show_desktop_window(app: AppHandle) -> Result<(), DesktopLifecycleError> {
    show_main_window(&app)
}

#[tauri::command]
pub(crate) fn minimize_desktop_window(app: AppHandle) -> Result<(), DesktopLifecycleError> {
    app.get_window("main")
        .ok_or(DesktopLifecycleError::MainWindowUnavailable)?
        .minimize()
        .map_err(|_| DesktopLifecycleError::WindowActionFailed)
}

#[tauri::command]
pub(crate) fn toggle_maximize_desktop_window(
    app: AppHandle,
) -> Result<bool, DesktopLifecycleError> {
    let window = app
        .get_window("main")
        .ok_or(DesktopLifecycleError::MainWindowUnavailable)?;
    let maximized = window
        .is_maximized()
        .map_err(|_| DesktopLifecycleError::WindowActionFailed)?;
    if maximized {
        window.unmaximize()
    } else {
        window.maximize()
    }
    .map_err(|_| DesktopLifecycleError::WindowActionFailed)?;
    Ok(!maximized)
}

#[tauri::command]
pub(crate) fn is_desktop_window_maximized(app: AppHandle) -> Result<bool, DesktopLifecycleError> {
    app.get_window("main")
        .ok_or(DesktopLifecycleError::MainWindowUnavailable)?
        .is_maximized()
        .map_err(|_| DesktopLifecycleError::WindowActionFailed)
}

#[tauri::command]
pub(crate) fn request_desktop_close(app: AppHandle) -> Result<(), DesktopLifecycleError> {
    apply_close_behavior(&app, load_close_behavior(&app))
}

#[tauri::command]
pub(crate) async fn quit_desktop(app: AppHandle) -> Result<(), TerminalError> {
    app.state::<UserTerminalManager>().shutdown().await?;
    app.state::<DesktopLifecycleCoordinator>().allow_exit();
    app.exit(0);
    Ok(())
}

#[tauri::command]
pub(crate) fn take_pending_desktop_lifecycle_intent(
    coordinator: State<'_, DesktopLifecycleCoordinator>,
) -> Option<DesktopLifecycleIntent> {
    coordinator.take_pending_intent()
}

#[tauri::command]
pub(crate) fn update_native_runtime_state(
    coordinator: State<'_, DesktopLifecycleCoordinator>,
    state: NativeRuntimeState,
    impact: Option<NativeRuntimeImpact>,
) {
    if let Some(impact) = impact {
        coordinator.update_runtime_impact(impact);
    }
    coordinator.update_runtime_state(state);
}

#[cfg(test)]
mod tests {
    use super::{
        DesktopLifecycleCoordinator, DesktopLifecycleIntent, NativeMenuAction, NativeRuntimeImpact,
        NativeRuntimeState, native_menu_action, runtime_impact_message,
    };

    #[test]
    fn connected_runtime_does_not_regress_to_transient_connecting_state() {
        assert!(!NativeRuntimeState::Connected.accepts(NativeRuntimeState::Connecting));
        assert!(NativeRuntimeState::Connected.accepts(NativeRuntimeState::Reconnecting));
        assert!(NativeRuntimeState::Connected.accepts(NativeRuntimeState::Disconnected));
    }

    #[test]
    fn lifecycle_intent_remains_pending_until_the_webview_claims_it() {
        let coordinator = DesktopLifecycleCoordinator::new();
        coordinator.queue_intent(DesktopLifecycleIntent::RestartRuntime);

        assert_eq!(
            coordinator.take_pending_intent(),
            Some(DesktopLifecycleIntent::RestartRuntime)
        );
        assert_eq!(coordinator.take_pending_intent(), None);
    }

    #[test]
    fn tray_lifecycle_items_do_not_route_through_the_webview() {
        assert_eq!(
            native_menu_action(super::TRAY_SHOW_WINDOW),
            Some(NativeMenuAction::ShowWindow)
        );
        assert_eq!(
            native_menu_action(super::TRAY_RESTART_RUNTIME),
            Some(NativeMenuAction::NativeIntent(
                DesktopLifecycleIntent::RestartRuntime
            ))
        );
        assert_eq!(
            native_menu_action(super::TRAY_STOP_RUNTIME),
            Some(NativeMenuAction::NativeIntent(
                DesktopLifecycleIntent::StopRuntime
            ))
        );
        assert_eq!(
            native_menu_action(super::TRAY_QUIT_DESKTOP),
            Some(NativeMenuAction::NativeIntent(
                DesktopLifecycleIntent::QuitDesktop
            ))
        );
    }

    #[test]
    fn native_runtime_confirmation_lists_the_cached_impact() {
        let message = runtime_impact_message(
            NativeRuntimeImpact {
                active_runs: 2,
                queued_inputs: 3,
                pending_approvals: 4,
            },
            "操作会中断未完成工作。",
        );

        assert!(message.contains("活动 Run：2"));
        assert!(message.contains("排队输入：3"));
        assert!(message.contains("待审批：4"));
    }
}
