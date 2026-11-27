//! Serialized controller for the always-on-top `"teza-notify"` toast popup window.
//!
//! Eliminates concurrent `show` / `dismiss` / `click` / timer races that could
//! leave an always-on-top notification window permanently stuck on screen, and
//! guarantees the popup NEVER steals keyboard focus when revealed:
//! - Every lifecycle transition (`show_popup`, `dismiss_popup`, `click_popup`,
//!   `force_close_popup`, auto-hide timer, and watchdog timer) is serialized
//!   through `op_lock`.
//! - A monotonic generation counter is advanced inside `op_lock` and re-checked
//!   immediately before revealing the window, so an invalidated show can never
//!   pop back up after a dismiss, click, or newer notification.
//! - The window is built with `.focused(false)` and `.focusable(false)`. On
//!   Windows, revealing bypasses Tauri/tao's `win.show()` (which calls
//!   `ShowWindow(SW_SHOW)` and activates the HWND) and instead enforces
//!   `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST` (clearing
//!   `WS_EX_APPWINDOW`) and reveals with `ShowWindow(SW_SHOWNOACTIVATE)` plus
//!   `SetWindowPos(..., HWND_TOPMOST, ..., SWP_NOACTIVATE | SWP_SHOWWINDOW)`.
//! - Hiding clears the pending payload, strips topmost z-order, hides without
//!   activating, and escalates to `destroy()` if hiding fails or the window
//!   remains visible (hardening against hung WebView2 HWNDs on Windows).
//! - Each shown notification spawns both its normal auto-hide timer and an
//!   independent watchdog safety timer keyed to the same generation.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, WindowEvent,
};
use tokio::sync::Mutex;

use crate::{
    compute_notify_position_for_size, compute_notify_shake_offsets, compute_toast_logical_size,
    NotifyToastPayload,
};

pub const NOTIFY_WINDOW_LABEL: &str = "teza-notify";
pub const NOTIFY_PAYLOAD_EVENT: &str = "core://notify-payload";
pub const MIN_TOAST_DURATION_MS: u64 = 2_500;
pub const MAX_TOAST_DURATION_MS: u64 = 15_000;
pub const WATCHDOG_GRACE_MS: u64 = 3_000;
#[cfg(target_os = "windows")]
const MAIN_THREAD_OP_TIMEOUT_MS: u64 = 1_500;
const DESTROY_WAIT_TIMEOUT_MS: u64 = 400;
const DESTROY_POLL_INTERVAL_MS: u64 = 15;
const SHOW_TIMEOUT_ERR_MSG: &str = "notify popup show timed out on UI thread";

/// Win32 extended window style & positioning constants shared with unit tests
/// so focus-safety invariants are verified deterministically.
pub const WIN32_WS_EX_TOPMOST: isize = 0x0000_0008;
pub const WIN32_WS_EX_TOOLWINDOW: isize = 0x0000_0080;
pub const WIN32_WS_EX_APPWINDOW: isize = 0x0004_0000;
pub const WIN32_WS_EX_NOACTIVATE: isize = 0x0800_0000;
pub const WIN32_SW_HIDE: i32 = 0;
pub const WIN32_SW_SHOWNOACTIVATE: i32 = 4;
pub const WIN32_SWP_NOSIZE: u32 = 0x0001;
pub const WIN32_SWP_NOMOVE: u32 = 0x0002;
pub const WIN32_SWP_NOACTIVATE: u32 = 0x0010;
pub const WIN32_SWP_FRAMECHANGED: u32 = 0x0020;
pub const WIN32_SWP_SHOWWINDOW: u32 = 0x0040;
pub const WIN32_SWP_ASYNCWINDOWPOS: u32 = 0x4000;

/// Compute the Windows `GWL_EXSTYLE` value for the notification popup HWND:
/// adds `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST` and clears
/// `WS_EX_APPWINDOW` while preserving all other style bits.
pub fn apply_non_activating_popup_ex_style(ex_style: isize) -> isize {
    (ex_style | WIN32_WS_EX_NOACTIVATE | WIN32_WS_EX_TOOLWINDOW | WIN32_WS_EX_TOPMOST)
        & !WIN32_WS_EX_APPWINDOW
}

/// `SetWindowPos` flags used on Windows to flush extended-style frame changes
/// and position the popup as topmost without activating it.
pub fn popup_frame_sync_swp_flags() -> u32 {
    WIN32_SWP_NOACTIVATE | WIN32_SWP_NOSIZE | WIN32_SWP_FRAMECHANGED
}

/// `SetWindowPos` flags used on Windows to reveal/keep the popup topmost
/// without activating or stealing keyboard focus.
pub fn popup_reveal_swp_flags() -> u32 {
    WIN32_SWP_NOACTIVATE | WIN32_SWP_SHOWWINDOW | WIN32_SWP_NOSIZE
}

/// `SetWindowPos` flags used on Windows to drop topmost z-order when hiding
/// without activating any window.
pub fn popup_hide_swp_flags() -> u32 {
    WIN32_SWP_NOACTIVATE | WIN32_SWP_NOMOVE | WIN32_SWP_NOSIZE
}

/// `SetWindowPos` flags used on Windows when force-hiding from a non-UI thread
/// (e.g. Tokio worker timeout or shutdown path): includes `SWP_ASYNCWINDOWPOS`
/// so cross-thread calls post asynchronously instead of blocking on a wedged
/// UI thread while holding `op_lock`.
pub fn popup_force_hide_async_swp_flags() -> u32 {
    popup_hide_swp_flags() | WIN32_SWP_ASYNCWINDOWPOS
}

/// Whether a failed `show_popup_window` attempt on a reused existing window
/// should trigger one clean destroy-and-recreate retry (for example when an
/// asynchronous `w.destroy()` invalidated the existing HWND just before show).
///
/// Never retries on a UI-thread timeout, where recreating a window on a stalled
/// UI thread would compound contention instead of failing fast.
pub fn should_recreate_popup_after_show_error(reused_existing: bool, err: &str) -> bool {
    reused_existing && !err.contains(SHOW_TIMEOUT_ERR_MSG)
}

/// Clamp a toast display duration to the safe `[2500ms, 15000ms]` range.
pub fn clamp_toast_duration_ms(duration_ms: u64) -> u64 {
    duration_ms.clamp(MIN_TOAST_DURATION_MS, MAX_TOAST_DURATION_MS)
}

/// Compute the watchdog deadline (`clamped_duration_ms + 3000ms`) used as a
/// secondary safety net for the same notification generation.
pub fn compute_watchdog_duration_ms(clamped_duration_ms: u64) -> u64 {
    clamped_duration_ms.saturating_add(WATCHDOG_GRACE_MS)
}

/// Whether the popup window itself should run the short physical buzz shake
/// (full-screen shake takes over at `buzz_streak >= 3`).
pub fn should_shake_popup(payload: &NotifyToastPayload) -> bool {
    payload.is_buzz && payload.buzz_streak < 3
}

/// Convert logical toast dimensions into clamped physical pixel dimensions.
pub fn compute_physical_toast_size(
    logical_w: f64,
    logical_h: f64,
    work_w: u32,
    work_h: u32,
    scale: f64,
) -> (u32, u32) {
    let safe_scale = scale.max(0.5);
    let pw = ((logical_w * safe_scale).round() as u32).clamp(1, work_w.max(1));
    let ph = ((logical_h * safe_scale).round() as u32).clamp(1, work_h.max(1));
    (pw, ph)
}

/// Decide whether a window handle must be escalated to `destroy()` after a
/// `hide()` attempt.
///
/// Escalates whenever `hide()` returned `Err`, or when `is_visible()` still
/// reports `true` or errors out (`None`), ensuring a hung/broken HWND can
/// never remain stuck on screen.
pub fn should_destroy_after_hide(hide_succeeded: bool, visible_after_hide: Option<bool>) -> bool {
    !hide_succeeded || visible_after_hide.unwrap_or(true)
}

struct ShownPopupMeta {
    generation: u64,
    work_x: i32,
    work_y: i32,
    work_w: u32,
    work_h: u32,
    scale: f64,
    px: i32,
    py: i32,
    actual_w: u32,
    actual_h: u32,
}

/// Cloneable serialized controller stored in `AppState` for the `"teza-notify"`
/// toast window.
#[derive(Debug, Clone)]
pub struct NotifyPopupController {
    op_lock: Arc<Mutex<()>>,
    generation: Arc<AtomicU64>,
    pending: Arc<Mutex<Option<NotifyToastPayload>>>,
    active_window_id: Arc<AtomicU64>,
}

impl Default for NotifyPopupController {
    fn default() -> Self {
        Self::new()
    }
}

impl NotifyPopupController {
    pub fn new() -> Self {
        Self {
            op_lock: Arc::new(Mutex::new(())),
            generation: Arc::new(AtomicU64::new(0)),
            pending: Arc::new(Mutex::new(None)),
            active_window_id: Arc::new(AtomicU64::new(0)),
        }
    }

    fn begin_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn current_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    fn is_generation_current(&self, expected_gen: u64) -> bool {
        self.current_generation() == expected_gen
    }

    fn next_window_instance_id(&self) -> u64 {
        self.active_window_id.fetch_add(1, Ordering::SeqCst) + 1
    }

    async fn stage_pending(&self, payload: NotifyToastPayload) {
        *self.pending.lock().await = Some(payload);
    }

    async fn invalidate_and_clear_pending(&self) -> u64 {
        let next_gen = self.begin_generation();
        *self.pending.lock().await = None;
        next_gen
    }

    #[cfg(test)]
    async fn clear_pending_if_generation(&self, expected_gen: u64) -> bool {
        let mut pending = self.pending.lock().await;
        if self.is_generation_current(expected_gen) {
            *pending = None;
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    async fn serialized_hide_if_generation<F>(&self, expected_gen: u64, hide_fn: F) -> bool
    where
        F: FnOnce(),
    {
        self.serialized_hide_if_generation_async(expected_gen, || async move {
            hide_fn();
        })
        .await
    }

    async fn serialized_hide_if_generation_async<F, Fut>(
        &self,
        expected_gen: u64,
        hide_fn: F,
    ) -> bool
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = ()>,
    {
        let _op = self.op_lock.lock().await;
        if !self.is_generation_current(expected_gen) {
            return false;
        }
        *self.pending.lock().await = None;
        hide_fn().await;
        true
    }

    #[cfg(test)]
    async fn serialized_dismiss<F>(&self, hide_fn: F) -> u64
    where
        F: FnOnce(),
    {
        self.serialized_dismiss_async(|| async move {
            hide_fn();
        })
        .await
    }

    async fn serialized_dismiss_async<F, Fut>(&self, hide_fn: F) -> u64
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = ()>,
    {
        let _op = self.op_lock.lock().await;
        let next_gen = self.invalidate_and_clear_pending().await;
        hide_fn().await;
        next_gen
    }

    /// Return the currently staged notification payload, if any.
    pub async fn get_current(&self) -> Option<NotifyToastPayload> {
        self.pending.lock().await.clone()
    }

    /// Show or update the `"teza-notify"` popup window in a strictly serialized
    /// critical section and arm both normal auto-hide and watchdog timers.
    pub async fn show_popup(
        &self,
        app: &AppHandle,
        payload: NotifyToastPayload,
    ) -> Result<(), String> {
        let meta = {
            let _op = self.op_lock.lock().await;
            let generation = self.begin_generation();
            self.stage_pending(payload.clone()).await;

            let target_mon = app
                .get_window("main")
                .filter(|m| m.is_visible().unwrap_or(false) && !m.is_minimized().unwrap_or(false))
                .and_then(|m| m.current_monitor().ok().flatten())
                .or_else(|| app.primary_monitor().ok().flatten());

            let (work_x, work_y, work_w, work_h, scale) = target_mon
                .map(|mon| {
                    let work = mon.work_area();
                    (
                        work.position.x,
                        work.position.y,
                        work.size.width,
                        work.size.height,
                        mon.scale_factor().max(0.5),
                    )
                })
                .unwrap_or((0, 0, 1920, 1040, 1.0));

            let (logical_w, logical_h) = compute_toast_logical_size(&payload.body);
            let (pw, ph) = compute_physical_toast_size(logical_w, logical_h, work_w, work_h, scale);
            let (est_x, est_y) = compute_notify_position_for_size(
                work_x,
                work_y,
                work_w,
                work_h,
                pw,
                ph,
                scale,
                &payload.position,
            );

            let mut attempt: u8 = 0;
            let (px, py, actual_w, actual_h) = loop {
                let (win, reused_existing) =
                    if let Some(existing) = app.get_webview_window(NOTIFY_WINDOW_LABEL) {
                        (existing, true)
                    } else {
                        let built = match self.build_popup_window(
                            app,
                            logical_w,
                            logical_h,
                            est_x,
                            est_y,
                            scale,
                        ) {
                            Ok(w) => w,
                            Err(err) => {
                                self.invalidate_and_clear_pending().await;
                                return Err(err);
                            }
                        };
                        (built, false)
                    };

                if !self.is_generation_current(generation) {
                    return Ok(());
                }

                let _ = win.set_shadow(false);
                let _ = win.set_focusable(false);
                // Move to the target monitor first so DPI and outer_size reflect the destination monitor.
                let _ = win.set_position(PhysicalPosition::new(est_x, est_y));
                let _ = win.set_size(PhysicalSize::new(pw, ph));
                let (actual_w, actual_h) = win
                    .outer_size()
                    .ok()
                    .filter(|s| s.width > 0 && s.height > 0)
                    .map(|s| (s.width, s.height))
                    .unwrap_or((pw, ph));
                let (px, py) = compute_notify_position_for_size(
                    work_x,
                    work_y,
                    work_w,
                    work_h,
                    actual_w,
                    actual_h,
                    scale,
                    &payload.position,
                );
                let _ = win.set_position(PhysicalPosition::new(px, py));
                #[cfg(not(target_os = "windows"))]
                let _ = win.set_always_on_top(true);
                let _ = win.set_ignore_cursor_events(false);

                if !self.is_generation_current(generation) {
                    return Ok(());
                }

                let _ = app.emit_to(NOTIFY_WINDOW_LABEL, NOTIFY_PAYLOAD_EVENT, &payload);
                match show_popup_window(&win, px, py, self.generation.clone(), generation).await {
                    Ok(()) => break (px, py, actual_w, actual_h),
                    Err(err) => {
                        if attempt == 0 && should_recreate_popup_after_show_error(reused_existing, &err) {
                            attempt = 1;
                            // Advance active_window_id BEFORE destroying the dying HWND so its
                            // `WindowEvent::Destroyed` handler ignores cleanup and preserves our
                            // current generation & staged payload for the recreated window.
                            let _ = self.next_window_instance_id();
                            force_destroy_webview_window_async(app, &win).await;
                            if !self.is_generation_current(generation) {
                                return Ok(());
                            }
                            continue;
                        }
                        // Bump generation (not just clear pending) so a late-running UI-thread
                        // `show_popup_no_activate` closure after a timeout aborts instead of
                        // revealing an empty toast.
                        self.invalidate_and_clear_pending().await;
                        force_destroy_webview_window_async(app, &win).await;
                        return Err(err);
                    }
                }
            };

            ShownPopupMeta {
                generation,
                work_x,
                work_y,
                work_w,
                work_h,
                scale,
                px,
                py,
                actual_w,
                actual_h,
            }
        };

        if should_shake_popup(&payload) {
            let ctrl_shake = self.clone();
            let app_shake = app.clone();
            let max_x = meta.work_x + (meta.work_w as i32 - meta.actual_w as i32).max(0);
            let max_y = meta.work_y + (meta.work_h as i32 - meta.actual_h as i32).max(0);
            let offsets = compute_notify_shake_offsets(meta.scale);
            let shake_gen = meta.generation;
            let base_x = meta.px;
            let base_y = meta.py;
            let min_x = meta.work_x;
            let min_y = meta.work_y;
            tauri::async_runtime::spawn(async move {
                for (dx, dy) in offsets {
                    if !ctrl_shake.is_generation_current(shake_gen) {
                        return;
                    }
                    if let Ok(_op) = ctrl_shake.op_lock.try_lock() {
                        if !ctrl_shake.is_generation_current(shake_gen) {
                            return;
                        }
                        if let Some(w) = app_shake.get_webview_window(NOTIFY_WINDOW_LABEL) {
                            if !w.is_visible().unwrap_or(false) {
                                return;
                            }
                            let sx = (base_x + dx).clamp(min_x, max_x);
                            let sy = (base_y + dy).clamp(min_y, max_y);
                            let _ = w.set_position(PhysicalPosition::new(sx, sy));
                        } else {
                            return;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(28)).await;
                }
                let _op = ctrl_shake.op_lock.lock().await;
                if !ctrl_shake.is_generation_current(shake_gen) {
                    return;
                }
                if let Some(w) = app_shake.get_webview_window(NOTIFY_WINDOW_LABEL) {
                    if w.is_visible().unwrap_or(false) {
                        let _ = w.set_position(PhysicalPosition::new(base_x, base_y));
                    }
                }
            });
        }

        let hide_ms = clamp_toast_duration_ms(payload.duration_ms);
        let watchdog_ms = compute_watchdog_duration_ms(hide_ms);
        let timer_gen = meta.generation;

        // Primary auto-hide timer (does not bump `generation`, so the same-gen
        // watchdog still verifies the window remained hidden).
        let ctrl_hide = self.clone();
        let app_hide = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(hide_ms)).await;
            let _ = ctrl_hide
                .serialized_hide_if_generation_async(timer_gen, || async {
                    hide_or_destroy_window(&app_hide).await;
                })
                .await;
        });

        // Secondary non-cancellable-on-same-gen watchdog timer: acquires `op_lock`
        // and destroys the window if it is still visible or unresponsive.
        let ctrl_watchdog = self.clone();
        let app_watchdog = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(watchdog_ms)).await;
            let _ = ctrl_watchdog
                .serialized_hide_if_generation_async(timer_gen, || async {
                    watchdog_ensure_hidden_or_destroyed(&app_watchdog).await;
                })
                .await;
        });

        Ok(())
    }

    /// Dismiss the popup immediately: invalidate generation, clear pending
    /// payload, drop always-on-top, hide (or destroy if still visible).
    pub async fn dismiss_popup(&self, app: &AppHandle) -> Result<(), String> {
        let _ = self
            .serialized_dismiss_async(|| async {
                hide_or_destroy_window(app).await;
            })
            .await;
        Ok(())
    }

    /// Handle clicking the popup: hide/dismiss the popup, restore & focus
    /// `"main"`, and emit `"core://open-thread"`.
    pub async fn click_popup(&self, app: &AppHandle, thread_id: String) -> Result<(), String> {
        let _ = self
            .serialized_dismiss_async(|| async {
                hide_or_destroy_window(app).await;
            })
            .await;
        if let Some(main_win) = app.get_window("main") {
            let _ = main_win.show();
            let _ = main_win.unminimize();
            let _ = main_win.set_focus();
        }
        let _ = app.emit_to("main", "core://open-thread", thread_id);
        Ok(())
    }

    fn build_popup_window(
        &self,
        app: &AppHandle,
        logical_w: f64,
        logical_h: f64,
        est_x: i32,
        est_y: i32,
        scale: f64,
    ) -> Result<WebviewWindow, String> {
        let window_id = self.next_window_instance_id();
        let built = WebviewWindowBuilder::new(
            app,
            NOTIFY_WINDOW_LABEL,
            WebviewUrl::App("index.html?view=notify".into()),
        )
        .title("Teza Notification")
        .inner_size(logical_w, logical_h)
        .position(est_x as f64 / scale, est_y as f64 / scale)
        .resizable(false)
        .decorations(false)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .focusable(false)
        .visible(false)
        .build()
        .map_err(|err| err.to_string())?;
        #[cfg(target_os = "windows")]
        win32::ensure_non_activating_style(&built);
        attach_close_cleanup_handler(
            &built,
            window_id,
            self.active_window_id.clone(),
            self.generation.clone(),
            self.pending.clone(),
        );
        Ok(built)
    }

    /// Unconditionally invalidate state and destroy the `"teza-notify"` window
    /// (used on app quit and main window close).
    pub async fn force_close_popup(&self, app: &AppHandle) {
        let _ = self
            .serialized_dismiss_async(|| async {
                force_destroy_window_async(app).await;
            })
            .await;
    }
}

/// Async entry point for Phase-2 shutdown hooks (tray quit / main window close).
pub async fn force_hide_or_destroy(app: &AppHandle, ctrl: &NotifyPopupController) {
    ctrl.force_close_popup(app).await;
}

/// Non-blocking synchronous cleanup helper for `WindowEvent::CloseRequested`
/// callbacks where `.await` is unavailable and the process may exit immediately.
#[allow(dead_code)]
pub fn force_destroy_sync(app: &AppHandle, ctrl: &NotifyPopupController) {
    ctrl.begin_generation();
    if let Ok(mut pending) = ctrl.pending.try_lock() {
        *pending = None;
    }
    force_destroy_window(app);
}

fn attach_close_cleanup_handler(
    win: &WebviewWindow,
    my_window_id: u64,
    active_window_id: Arc<AtomicU64>,
    gen_ev: Arc<AtomicU64>,
    pending_ev: Arc<Mutex<Option<NotifyToastPayload>>>,
) {
    // Allow OS/Alt+F4 close to proceed cleanly (never call `api.prevent_close()`),
    // while invalidating generation & clearing stale payload for this window instance.
    win.on_window_event(move |event| {
        if !matches!(
            event,
            WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed
        ) {
            return;
        }
        if active_window_id.load(Ordering::SeqCst) != my_window_id {
            return;
        }
        let next_gen = gen_ev.fetch_add(1, Ordering::SeqCst) + 1;
        if let Ok(mut guard) = pending_ev.try_lock() {
            if active_window_id.load(Ordering::SeqCst) == my_window_id
                && gen_ev.load(Ordering::SeqCst) == next_gen
            {
                *guard = None;
            }
        } else {
            let pending_clear = pending_ev.clone();
            let gen_clear = gen_ev.clone();
            let active_id_clear = active_window_id.clone();
            tauri::async_runtime::spawn(async move {
                let mut guard = pending_clear.lock().await;
                if active_id_clear.load(Ordering::SeqCst) == my_window_id
                    && gen_clear.load(Ordering::SeqCst) == next_gen
                {
                    *guard = None;
                }
            });
        }
    });
}

#[cfg(target_os = "windows")]
mod win32 {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use tauri::WebviewWindow;

    use super::{
        apply_non_activating_popup_ex_style, popup_force_hide_async_swp_flags,
        popup_frame_sync_swp_flags, popup_hide_swp_flags, popup_reveal_swp_flags,
        MAIN_THREAD_OP_TIMEOUT_MS, SHOW_TIMEOUT_ERR_MSG, WIN32_SW_HIDE, WIN32_SW_SHOWNOACTIVATE,
    };

    const GWL_EXSTYLE: i32 = -20;
    const HWND_TOPMOST: *mut c_void = -1isize as *mut c_void;
    const HWND_NOTOPMOST: *mut c_void = -2isize as *mut c_void;

    #[link(name = "user32")]
    extern "system" {
        #[cfg(target_pointer_width = "64")]
        fn GetWindowLongPtrW(hwnd: *mut c_void, index: i32) -> isize;
        #[cfg(target_pointer_width = "64")]
        fn SetWindowLongPtrW(hwnd: *mut c_void, index: i32, new_long: isize) -> isize;
        #[cfg(target_pointer_width = "32")]
        fn GetWindowLongW(hwnd: *mut c_void, index: i32) -> i32;
        #[cfg(target_pointer_width = "32")]
        fn SetWindowLongW(hwnd: *mut c_void, index: i32, new_long: i32) -> i32;

        fn ShowWindow(hwnd: *mut c_void, cmd_show: i32) -> i32;
        fn ShowWindowAsync(hwnd: *mut c_void, cmd_show: i32) -> i32;
        fn SetWindowPos(
            hwnd: *mut c_void,
            hwnd_insert_after: *mut c_void,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            flags: u32,
        ) -> i32;
        fn IsWindow(hwnd: *mut c_void) -> i32;
        fn IsWindowVisible(hwnd: *mut c_void) -> i32;
    }

    unsafe fn get_ex_style(hwnd: *mut c_void) -> isize {
        #[cfg(target_pointer_width = "64")]
        {
            GetWindowLongPtrW(hwnd, GWL_EXSTYLE)
        }
        #[cfg(target_pointer_width = "32")]
        {
            GetWindowLongW(hwnd, GWL_EXSTYLE) as isize
        }
    }

    unsafe fn set_ex_style(hwnd: *mut c_void, ex_style: isize) {
        #[cfg(target_pointer_width = "64")]
        {
            let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex_style);
        }
        #[cfg(target_pointer_width = "32")]
        {
            let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, ex_style as i32);
        }
    }

    unsafe fn apply_ex_style_if_needed(hwnd: *mut c_void) {
        let cur_ex = get_ex_style(hwnd);
        let next_ex = apply_non_activating_popup_ex_style(cur_ex);
        if next_ex != cur_ex {
            set_ex_style(hwnd, next_ex);
        }
    }

    pub(super) fn ensure_non_activating_style(win: &WebviewWindow) {
        let Ok(hwnd) = win.hwnd() else {
            return;
        };
        let hwnd_raw = hwnd.0 as usize;
        let _ = win.run_on_main_thread(move || {
            let hwnd_ptr = hwnd_raw as *mut c_void;
            unsafe {
                if IsWindow(hwnd_ptr) != 0 {
                    apply_ex_style_if_needed(hwnd_ptr);
                    let _ = SetWindowPos(
                        hwnd_ptr,
                        HWND_TOPMOST,
                        0,
                        0,
                        0,
                        0,
                        popup_hide_swp_flags() | super::WIN32_SWP_FRAMECHANGED,
                    );
                }
            }
        });
    }

    pub(super) async fn show_popup_no_activate(
        win: &WebviewWindow,
        px: i32,
        py: i32,
        gen_counter: Arc<AtomicU64>,
        expected_gen: u64,
    ) -> Result<(), String> {
        let hwnd = win.hwnd().map_err(|e| e.to_string())?;
        let hwnd_raw = hwnd.0 as usize;
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
        win.run_on_main_thread(move || {
            if gen_counter.load(Ordering::SeqCst) != expected_gen {
                let _ = tx.send(Ok(()));
                return;
            }
            let hwnd_ptr = hwnd_raw as *mut c_void;
            let res = unsafe {
                if IsWindow(hwnd_ptr) == 0 {
                    Err("notify popup HWND is no longer valid".to_string())
                } else {
                    apply_ex_style_if_needed(hwnd_ptr);
                    let _ = SetWindowPos(
                        hwnd_ptr,
                        HWND_TOPMOST,
                        px,
                        py,
                        0,
                        0,
                        popup_frame_sync_swp_flags(),
                    );
                    let _ = ShowWindow(hwnd_ptr, WIN32_SW_SHOWNOACTIVATE);
                    let _ = SetWindowPos(
                        hwnd_ptr,
                        HWND_TOPMOST,
                        px,
                        py,
                        0,
                        0,
                        popup_reveal_swp_flags(),
                    );
                    if IsWindowVisible(hwnd_ptr) != 0 {
                        Ok(())
                    } else {
                        Err("notify popup window failed to become visible".to_string())
                    }
                }
            };
            let _ = tx.send(res);
        })
        .map_err(|e| e.to_string())?;

        match tokio::time::timeout(Duration::from_millis(MAIN_THREAD_OP_TIMEOUT_MS), rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err("notify popup show channel closed".to_string()),
            Err(_) => Err(SHOW_TIMEOUT_ERR_MSG.to_string()),
        }
    }

    pub(super) async fn hide_popup_no_activate(win: &WebviewWindow) -> (bool, Option<bool>) {
        let Ok(hwnd) = win.hwnd() else {
            return (false, None);
        };
        let hwnd_raw = hwnd.0 as usize;
        let (tx, rx) = tokio::sync::oneshot::channel::<(bool, Option<bool>)>();
        if win
            .run_on_main_thread(move || {
                let hwnd_ptr = hwnd_raw as *mut c_void;
                let result = unsafe {
                    if IsWindow(hwnd_ptr) == 0 {
                        (true, Some(false))
                    } else {
                        let _ = SetWindowPos(
                            hwnd_ptr,
                            HWND_NOTOPMOST,
                            0,
                            0,
                            0,
                            0,
                            popup_hide_swp_flags(),
                        );
                        let _ = ShowWindow(hwnd_ptr, WIN32_SW_HIDE);
                        (true, Some(IsWindowVisible(hwnd_ptr) != 0))
                    }
                };
                let _ = tx.send(result);
            })
            .is_err()
        {
            return (false, None);
        }

        match tokio::time::timeout(Duration::from_millis(MAIN_THREAD_OP_TIMEOUT_MS), rx).await {
            Ok(Ok(res)) => res,
            _ => (false, None),
        }
    }

    pub(super) fn force_hide_sync(win: &WebviewWindow) {
        if let Ok(hwnd) = win.hwnd() {
            let hwnd_ptr = hwnd.0;
            unsafe {
                if IsWindow(hwnd_ptr) != 0 {
                    let _ = SetWindowPos(
                        hwnd_ptr,
                        HWND_NOTOPMOST,
                        0,
                        0,
                        0,
                        0,
                        popup_force_hide_async_swp_flags(),
                    );
                    let _ = ShowWindowAsync(hwnd_ptr, WIN32_SW_HIDE);
                }
            }
        }
    }
}

async fn destroy_and_await_removed(app: &AppHandle, w: &WebviewWindow) {
    let _ = w.destroy();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(DESTROY_WAIT_TIMEOUT_MS);
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    if app
        .run_on_main_thread(move || {
            let _ = tx.send(());
        })
        .is_ok()
    {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let _ = tokio::time::timeout(remaining, rx).await;
    }
    while app.get_webview_window(NOTIFY_WINDOW_LABEL).is_some() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        tokio::time::sleep(remaining.min(Duration::from_millis(DESTROY_POLL_INTERVAL_MS))).await;
    }
}

async fn show_popup_window(
    win: &WebviewWindow,
    px: i32,
    py: i32,
    gen_counter: Arc<AtomicU64>,
    expected_gen: u64,
) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        win32::show_popup_no_activate(win, px, py, gen_counter, expected_gen).await
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (px, py, gen_counter, expected_gen);
        win.show().map_err(|e| e.to_string())
    }
}

async fn hide_or_destroy_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(NOTIFY_WINDOW_LABEL) {
        #[cfg(target_os = "windows")]
        let (hide_ok, visible_after) = win32::hide_popup_no_activate(&w).await;
        #[cfg(not(target_os = "windows"))]
        let (hide_ok, visible_after) = {
            let _ = w.set_always_on_top(false);
            let hide_ok = w.hide().is_ok();
            let visible_after = w.is_visible().ok();
            (hide_ok, visible_after)
        };
        if should_destroy_after_hide(hide_ok, visible_after) {
            #[cfg(target_os = "windows")]
            if !hide_ok {
                win32::force_hide_sync(&w);
            }
            destroy_and_await_removed(app, &w).await;
        }
    }
}

async fn watchdog_ensure_hidden_or_destroyed(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(NOTIFY_WINDOW_LABEL) {
        if w.is_visible().unwrap_or(true) {
            #[cfg(target_os = "windows")]
            let (hide_ok, visible_after) = win32::hide_popup_no_activate(&w).await;
            #[cfg(not(target_os = "windows"))]
            let (hide_ok, visible_after) = {
                let _ = w.set_always_on_top(false);
                let hide_ok = w.hide().is_ok();
                let visible_after = w.is_visible().ok();
                (hide_ok, visible_after)
            };
            if should_destroy_after_hide(hide_ok, visible_after) {
                #[cfg(target_os = "windows")]
                if !hide_ok {
                    win32::force_hide_sync(&w);
                }
                destroy_and_await_removed(app, &w).await;
            }
        }
    }
}

fn force_destroy_webview_window(w: &WebviewWindow) {
    #[cfg(target_os = "windows")]
    {
        win32::force_hide_sync(w);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = w.set_always_on_top(false);
        let _ = w.hide();
    }
    let _ = w.destroy();
}

async fn force_destroy_webview_window_async(app: &AppHandle, w: &WebviewWindow) {
    #[cfg(target_os = "windows")]
    {
        win32::force_hide_sync(w);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = w.set_always_on_top(false);
        let _ = w.hide();
    }
    destroy_and_await_removed(app, w).await;
}

fn force_destroy_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(NOTIFY_WINDOW_LABEL) {
        force_destroy_webview_window(&w);
    }
}

async fn force_destroy_window_async(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(NOTIFY_WINDOW_LABEL) {
        force_destroy_webview_window_async(app, &w).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn sample_payload(
        thread_id: &str,
        duration_ms: u64,
        is_buzz: bool,
        buzz_streak: u32,
    ) -> NotifyToastPayload {
        NotifyToastPayload {
            thread_id: thread_id.to_string(),
            title: "Teza".to_string(),
            body: "Xin chào".to_string(),
            avatar: String::new(),
            theme: "triforce".to_string(),
            position: "bottom-right".to_string(),
            duration_ms,
            is_buzz,
            buzz_streak,
        }
    }

    #[test]
    fn duration_clamping_and_watchdog_deadline_stay_within_bounds() {
        assert_eq!(clamp_toast_duration_ms(0), MIN_TOAST_DURATION_MS);
        assert_eq!(clamp_toast_duration_ms(1_200), MIN_TOAST_DURATION_MS);
        assert_eq!(clamp_toast_duration_ms(2_500), 2_500);
        assert_eq!(clamp_toast_duration_ms(5_000), 5_000);
        assert_eq!(clamp_toast_duration_ms(15_000), MAX_TOAST_DURATION_MS);
        assert_eq!(clamp_toast_duration_ms(99_999), MAX_TOAST_DURATION_MS);

        assert_eq!(
            compute_watchdog_duration_ms(clamp_toast_duration_ms(5_000)),
            5_000 + WATCHDOG_GRACE_MS
        );
        assert_eq!(compute_watchdog_duration_ms(u64::MAX), u64::MAX);
    }

    #[test]
    fn should_shake_popup_only_triggers_for_buzz_below_streak_three() {
        assert!(!should_shake_popup(&sample_payload("t1", 5000, false, 1)));
        assert!(should_shake_popup(&sample_payload("t1", 5000, true, 0)));
        assert!(should_shake_popup(&sample_payload("t1", 5000, true, 1)));
        assert!(should_shake_popup(&sample_payload("t1", 5000, true, 2)));
        assert!(!should_shake_popup(&sample_payload("t1", 5000, true, 3)));
        assert!(!should_shake_popup(&sample_payload("t1", 5000, true, 6)));
    }

    #[test]
    fn should_destroy_after_hide_escalates_on_error_or_stuck_visibility() {
        assert!(!should_destroy_after_hide(true, Some(false)));
        assert!(should_destroy_after_hide(true, Some(true)));
        assert!(should_destroy_after_hide(true, None));
        assert!(should_destroy_after_hide(false, Some(false)));
        assert!(should_destroy_after_hide(false, Some(true)));
        assert!(should_destroy_after_hide(false, None));
    }

    #[test]
    fn physical_toast_size_scales_and_clamps_to_work_area() {
        assert_eq!(
            compute_physical_toast_size(320.0, 72.0, 1920, 1040, 1.0),
            (320, 72)
        );
        assert_eq!(
            compute_physical_toast_size(320.0, 86.0, 1920, 1040, 1.25),
            (400, 108)
        );
        assert_eq!(
            compute_physical_toast_size(320.0, 86.0, 200, 60, 1.5),
            (200, 60)
        );
        assert_eq!(compute_physical_toast_size(320.0, 72.0, 0, 0, 0.1), (1, 1));
    }

    #[test]
    fn controller_generation_state_machine_prevents_stale_hide_and_clears_pending() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let ctrl = NotifyPopupController::new();
            assert_eq!(ctrl.current_generation(), 0);
            assert!(ctrl.get_current().await.is_none());

            // Notification #1 staged at gen 1
            let g1 = ctrl.begin_generation();
            assert_eq!(g1, 1);
            ctrl.stage_pending(sample_payload("thread-1", 4000, false, 0))
                .await;
            assert_eq!(
                ctrl.get_current().await.map(|p| p.thread_id),
                Some("thread-1".to_string())
            );

            // Notification #2 supersedes #1 at gen 2
            let g2 = ctrl.begin_generation();
            assert_eq!(g2, 2);
            ctrl.stage_pending(sample_payload("thread-2", 4000, false, 0))
                .await;

            // Stale auto-hide timer from gen 1 must be ignored completely
            let hide_calls = Arc::new(AtomicUsize::new(0));
            let c1 = hide_calls.clone();
            let ran_stale = ctrl
                .serialized_hide_if_generation(g1, move || {
                    c1.fetch_add(1, Ordering::SeqCst);
                })
                .await;
            assert!(!ran_stale);
            assert_eq!(hide_calls.load(Ordering::SeqCst), 0);
            assert_eq!(
                ctrl.get_current().await.map(|p| p.thread_id),
                Some("thread-2".to_string())
            );

            // Primary auto-hide for gen 2 runs, clears pending, and keeps gen == 2
            // so the same-gen watchdog can still run its verification check.
            let c2 = hide_calls.clone();
            let ran_primary = ctrl
                .serialized_hide_if_generation(g2, move || {
                    c2.fetch_add(1, Ordering::SeqCst);
                })
                .await;
            assert!(ran_primary);
            assert_eq!(hide_calls.load(Ordering::SeqCst), 1);
            assert!(ctrl.get_current().await.is_none());
            assert_eq!(ctrl.current_generation(), g2);

            // Watchdog for gen 2 still passes gen check while no newer toast arrived.
            let c3 = hide_calls.clone();
            let ran_watchdog = ctrl
                .serialized_hide_if_generation(g2, move || {
                    c3.fetch_add(1, Ordering::SeqCst);
                })
                .await;
            assert!(ran_watchdog);
            assert_eq!(hide_calls.load(Ordering::SeqCst), 2);
        });
    }

    #[test]
    fn dismiss_invalidates_generation_and_blocks_pending_timers() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let ctrl = NotifyPopupController::new();
            let g1 = ctrl.begin_generation();
            ctrl.stage_pending(sample_payload("thread-a", 5000, true, 1))
                .await;

            let dismissed = Arc::new(AtomicUsize::new(0));
            let d1 = dismissed.clone();
            let g_after_dismiss = ctrl
                .serialized_dismiss(move || {
                    d1.fetch_add(1, Ordering::SeqCst);
                })
                .await;
            assert!(g_after_dismiss > g1);
            assert_eq!(dismissed.load(Ordering::SeqCst), 1);
            assert!(ctrl.get_current().await.is_none());
            assert!(!ctrl.is_generation_current(g1));

            // Neither primary auto-hide nor watchdog for g1 can run after dismiss.
            let timer_ran = ctrl.serialized_hide_if_generation(g1, || {}).await;
            assert!(!timer_ran);

            // Clearing pending for an outdated generation must not clobber a newer payload.
            let g_new = ctrl.begin_generation();
            ctrl.stage_pending(sample_payload("thread-b", 5000, false, 0))
                .await;
            assert!(!ctrl.clear_pending_if_generation(g1).await);
            assert_eq!(
                ctrl.get_current().await.map(|p| p.thread_id),
                Some("thread-b".to_string())
            );
            assert!(ctrl.clear_pending_if_generation(g_new).await);
            assert!(ctrl.get_current().await.is_none());
        });
    }

    #[test]
    fn serialized_operations_execute_without_overlapping_on_op_lock() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let ctrl = NotifyPopupController::new();
            let g1 = ctrl.begin_generation();
            ctrl.stage_pending(sample_payload("thread-race", 5000, false, 0))
                .await;

            // Hold op_lock to simulate an in-flight show/dismiss operation.
            let guard = ctrl.op_lock.lock().await;
            let ctrl_bg = ctrl.clone();
            let bg_task = tokio::spawn(async move { ctrl_bg.serialized_dismiss(|| {}).await });

            // Yield briefly: dismiss must remain blocked while op_lock is held.
            tokio::task::yield_now().await;
            assert!(ctrl.is_generation_current(g1));
            assert!(ctrl.get_current().await.is_some());

            drop(guard);
            let next_gen = bg_task.await.unwrap();
            assert_eq!(next_gen, g1 + 1);
            assert!(ctrl.get_current().await.is_none());
        });
    }

    #[test]
    fn window_instance_ids_are_strictly_monotonic() {
        let ctrl = NotifyPopupController::new();
        let id1 = ctrl.next_window_instance_id();
        let id2 = ctrl.next_window_instance_id();
        let id3 = ctrl.next_window_instance_id();
        assert_eq!((id1, id2, id3), (1, 2, 3));
    }

    #[test]
    fn non_activating_popup_ex_style_enforces_noactivate_toolwindow_topmost_and_clears_appwindow() {
        // Simulate tao default top-level window ex-style (WS_EX_APPWINDOW + WS_EX_WINDOWEDGE | WS_EX_ACCEPTFILES).
        let ws_ex_windowedge: isize = 0x0000_0100;
        let ws_ex_acceptfiles: isize = 0x0000_0010;
        let ws_ex_layered: isize = 0x0008_0000;
        let initial = WIN32_WS_EX_APPWINDOW | ws_ex_windowedge | ws_ex_acceptfiles | ws_ex_layered;

        let updated = apply_non_activating_popup_ex_style(initial);
        assert_ne!(updated & WIN32_WS_EX_NOACTIVATE, 0, "must set WS_EX_NOACTIVATE");
        assert_ne!(updated & WIN32_WS_EX_TOOLWINDOW, 0, "must set WS_EX_TOOLWINDOW");
        assert_ne!(updated & WIN32_WS_EX_TOPMOST, 0, "must set WS_EX_TOPMOST");
        assert_eq!(updated & WIN32_WS_EX_APPWINDOW, 0, "must clear WS_EX_APPWINDOW");

        // Unrelated style bits remain preserved, and applying twice is idempotent.
        assert_ne!(updated & ws_ex_windowedge, 0);
        assert_ne!(updated & ws_ex_acceptfiles, 0);
        assert_ne!(updated & ws_ex_layered, 0);
        assert_eq!(apply_non_activating_popup_ex_style(updated), updated);
    }

    #[test]
    fn win32_show_and_hide_flags_never_activate_popup_window() {
        // SW_SHOWNOACTIVATE is 4 (never SW_SHOW=5 or SW_SHOWNORMAL=1).
        assert_eq!(WIN32_SW_SHOWNOACTIVATE, 4);
        assert_eq!(WIN32_SW_HIDE, 0);

        let frame_sync = popup_frame_sync_swp_flags();
        assert_ne!(frame_sync & WIN32_SWP_NOACTIVATE, 0);
        assert_ne!(frame_sync & WIN32_SWP_FRAMECHANGED, 0);
        assert_ne!(frame_sync & WIN32_SWP_NOSIZE, 0);

        let reveal = popup_reveal_swp_flags();
        assert_ne!(reveal & WIN32_SWP_NOACTIVATE, 0);
        assert_ne!(reveal & WIN32_SWP_SHOWWINDOW, 0);
        assert_ne!(reveal & WIN32_SWP_NOSIZE, 0);

        let hide = popup_hide_swp_flags();
        assert_ne!(hide & WIN32_SWP_NOACTIVATE, 0);
        assert_ne!(hide & WIN32_SWP_NOMOVE, 0);
        assert_ne!(hide & WIN32_SWP_NOSIZE, 0);
        assert_eq!(hide & WIN32_SWP_SHOWWINDOW, 0);
        assert_eq!(hide & WIN32_SWP_ASYNCWINDOWPOS, 0);

        // Cross-thread force_hide_sync must set SWP_ASYNCWINDOWPOS (0x4000) so
        // non-UI threads post asynchronously instead of deadlocking on a wedged UI thread.
        assert_eq!(WIN32_SWP_ASYNCWINDOWPOS, 0x4000);
        let force_hide = popup_force_hide_async_swp_flags();
        assert_ne!(force_hide & WIN32_SWP_ASYNCWINDOWPOS, 0);
        assert_ne!(force_hide & WIN32_SWP_NOACTIVATE, 0);
        assert_ne!(force_hide & WIN32_SWP_NOMOVE, 0);
        assert_ne!(force_hide & WIN32_SWP_NOSIZE, 0);
        assert_eq!(force_hide & WIN32_SWP_SHOWWINDOW, 0);
    }

    #[test]
    fn show_error_retry_policy_recreates_only_on_non_timeout_reused_window_errors() {
        assert!(should_recreate_popup_after_show_error(
            true,
            "notify popup HWND is no longer valid"
        ));
        assert!(should_recreate_popup_after_show_error(
            true,
            "notify popup window failed to become visible"
        ));
        assert!(!should_recreate_popup_after_show_error(
            true,
            SHOW_TIMEOUT_ERR_MSG
        ));
        assert!(!should_recreate_popup_after_show_error(
            false,
            "notify popup HWND is no longer valid"
        ));
    }

    #[test]
    fn show_timeout_invalidation_bumps_generation_so_late_ui_closure_skips_reveal() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let ctrl = NotifyPopupController::new();
            let gen_at_show = ctrl.begin_generation();
            ctrl.stage_pending(sample_payload("thread-timeout", 4000, false, 0))
                .await;

            // Simulate the show timeout branch in `show_popup`: calling
            // `invalidate_and_clear_pending()` clears pending AND bumps generation.
            let gen_after_timeout = ctrl.invalidate_and_clear_pending().await;
            assert!(gen_after_timeout > gen_at_show);
            assert!(ctrl.get_current().await.is_none());

            // When the stalled UI thread later executes the queued `show_popup_no_activate`
            // closure, its generation guard rejects the stale `gen_at_show`.
            assert!(!ctrl.is_generation_current(gen_at_show));
        });
    }

    #[test]
    fn async_serialized_hide_and_dismiss_respect_generation_and_op_lock() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let ctrl = NotifyPopupController::new();
            let g1 = ctrl.begin_generation();
            ctrl.stage_pending(sample_payload("thread-async-1", 4000, false, 0))
                .await;

            let calls = Arc::new(AtomicUsize::new(0));
            let c1 = calls.clone();
            let ran = ctrl
                .serialized_hide_if_generation_async(g1, move || async move {
                    tokio::task::yield_now().await;
                    c1.fetch_add(1, Ordering::SeqCst);
                })
                .await;
            assert!(ran);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert!(ctrl.get_current().await.is_none());

            let c2 = calls.clone();
            let g_after = ctrl
                .serialized_dismiss_async(move || async move {
                    tokio::task::yield_now().await;
                    c2.fetch_add(1, Ordering::SeqCst);
                })
                .await;
            assert!(g_after > g1);
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            assert!(!ctrl.is_generation_current(g1));
        });
    }
}
