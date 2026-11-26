//! Serialized controller for the always-on-top `"teza-notify"` toast popup window.
//!
//! Eliminates concurrent `show` / `dismiss` / `click` / timer races that could
//! leave an always-on-top notification window permanently stuck on screen:
//! - Every lifecycle transition (`show_popup`, `dismiss_popup`, `click_popup`,
//!   `force_close_popup`, auto-hide timer, and watchdog timer) is serialized
//!   through `op_lock`.
//! - A monotonic generation counter is advanced inside `op_lock` and re-checked
//!   immediately before `win.show()`, so an invalidated show can never pop back
//!   up after a dismiss, click, or newer notification.
//! - Hiding clears the pending payload, strips `always_on_top(false)`, calls
//!   `hide()`, and escalates to `destroy()` if `hide()` fails or the window
//!   remains visible (hardening against hung WebView2 HWNDs on Windows).
//! - Each shown notification spawns both its normal auto-hide timer and an
//!   independent watchdog safety timer keyed to the same generation.

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

    async fn clear_pending_if_generation(&self, expected_gen: u64) -> bool {
        let mut pending = self.pending.lock().await;
        if self.is_generation_current(expected_gen) {
            *pending = None;
            true
        } else {
            false
        }
    }

    async fn serialized_hide_if_generation<F>(&self, expected_gen: u64, hide_fn: F) -> bool
    where
        F: FnOnce(),
    {
        let _op = self.op_lock.lock().await;
        if !self.is_generation_current(expected_gen) {
            return false;
        }
        *self.pending.lock().await = None;
        hide_fn();
        true
    }

    async fn serialized_dismiss<F>(&self, hide_fn: F) -> u64
    where
        F: FnOnce(),
    {
        let _op = self.op_lock.lock().await;
        let next_gen = self.invalidate_and_clear_pending().await;
        hide_fn();
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

            let win = if let Some(existing) = app.get_webview_window(NOTIFY_WINDOW_LABEL) {
                existing
            } else {
                let window_id = self.next_window_instance_id();
                let built = match WebviewWindowBuilder::new(
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
                .visible(false)
                .build()
                {
                    Ok(w) => w,
                    Err(err) => {
                        self.clear_pending_if_generation(generation).await;
                        return Err(err.to_string());
                    }
                };
                attach_close_cleanup_handler(
                    &built,
                    window_id,
                    self.active_window_id.clone(),
                    self.generation.clone(),
                    self.pending.clone(),
                );
                built
            };

            if !self.is_generation_current(generation) {
                return Ok(());
            }

            let _ = win.set_shadow(false);
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
            let _ = win.set_always_on_top(true);
            let _ = win.set_ignore_cursor_events(false);

            if !self.is_generation_current(generation) {
                return Ok(());
            }

            let _ = app.emit_to(NOTIFY_WINDOW_LABEL, NOTIFY_PAYLOAD_EVENT, &payload);
            if let Err(err) = win.show() {
                self.clear_pending_if_generation(generation).await;
                let _ = win.set_always_on_top(false);
                let _ = win.destroy();
                return Err(err.to_string());
            }

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
                .serialized_hide_if_generation(timer_gen, || {
                    hide_or_destroy_window(&app_hide);
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
                .serialized_hide_if_generation(timer_gen, || {
                    watchdog_ensure_hidden_or_destroyed(&app_watchdog);
                })
                .await;
        });

        Ok(())
    }

    /// Dismiss the popup immediately: invalidate generation, clear pending
    /// payload, drop always-on-top, hide (or destroy if still visible).
    pub async fn dismiss_popup(&self, app: &AppHandle) -> Result<(), String> {
        let _ = self
            .serialized_dismiss(|| {
                hide_or_destroy_window(app);
            })
            .await;
        Ok(())
    }

    /// Handle clicking the popup: hide/dismiss the popup, restore & focus
    /// `"main"`, and emit `"core://open-thread"`.
    pub async fn click_popup(&self, app: &AppHandle, thread_id: String) -> Result<(), String> {
        let _ = self
            .serialized_dismiss(|| {
                hide_or_destroy_window(app);
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

    /// Unconditionally invalidate state and destroy the `"teza-notify"` window
    /// (used on app quit and main window close).
    pub async fn force_close_popup(&self, app: &AppHandle) {
        let _ = self
            .serialized_dismiss(|| {
                force_destroy_window(app);
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

fn hide_or_destroy_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(NOTIFY_WINDOW_LABEL) {
        let _ = w.set_always_on_top(false);
        let hide_ok = w.hide().is_ok();
        let visible_after = w.is_visible().ok();
        if should_destroy_after_hide(hide_ok, visible_after) {
            let _ = w.destroy();
        }
    }
}

fn watchdog_ensure_hidden_or_destroyed(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(NOTIFY_WINDOW_LABEL) {
        if w.is_visible().unwrap_or(true) {
            let _ = w.set_always_on_top(false);
            let hide_ok = w.hide().is_ok();
            let visible_after = w.is_visible().ok();
            if should_destroy_after_hide(hide_ok, visible_after) {
                let _ = w.destroy();
            }
        }
    }
}

fn force_destroy_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(NOTIFY_WINDOW_LABEL) {
        let _ = w.set_always_on_top(false);
        let _ = w.hide();
        let _ = w.destroy();
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
}
