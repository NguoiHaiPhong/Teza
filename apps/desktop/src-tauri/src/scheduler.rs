//! Background scheduled-message delivery worker (Item #2).
//!
//! Sleeps efficiently until the earliest pending schedule timestamp (capped at 60s)
//! or until woken immediately via [`SchedulerHandle::notify_changed`]. Claims due
//! schedules atomically from SQLite so duplicate sends cannot occur, handles misfire
//! policies (`skip`, `ask`, `send_on_resume`), paces batch sends, and applies bounded
//! retry backoff on transient errors.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;

use zalo_core::chat::messaging;
use zalo_core::chat::parser::format_thread_preview;
use zalo_core::chat::ThreadKind;
use zalo_core::session::{Session, SessionError};
use zalo_core::store::{MessageStore, ScheduledMessageDto};

/// Upper bound on idle sleep between schedule checks when no sooner item exists.
pub const MAX_SCHEDULER_SLEEP_SECS: u64 = 60;
/// Maximum number of due schedules claimed in a single worker tick.
pub const SCHEDULE_BATCH_SIZE: usize = 10;
/// Minimum pacing interval between consecutive outgoing scheduled sends in a batch.
pub const SCHEDULE_PACING_MS: u64 = 1_500;
/// Maximum retry attempts before marking a schedule permanently `failed`.
pub const MAX_SCHEDULE_RETRIES: u32 = 2;
/// Fallback grace window (seconds) if an item carries a non-positive value.
pub const DEFAULT_MISFIRE_GRACE_SECS: i64 = 600;
/// Guard delay used if the store returns an error or an unexpected empty claim.
const ERROR_BACKOFF_SECS: u64 = 2;

/// Cloneable handle stored in `AppState` so IPC commands and Local API endpoints
/// can wake the background scheduler immediately on any schedule mutation.
#[derive(Debug, Clone)]
pub struct SchedulerHandle {
    notify: Arc<Notify>,
}

impl Default for SchedulerHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl SchedulerHandle {
    pub fn new() -> Self {
        Self {
            notify: Arc::new(Notify::new()),
        }
    }

    #[allow(dead_code)]
    pub fn from_notify(notify: Arc<Notify>) -> Self {
        Self { notify }
    }

    pub fn notify_arc(&self) -> Arc<Notify> {
        self.notify.clone()
    }

    /// Wake the background scheduler task immediately so newly created, updated,
    /// rescheduled, or cancelled entries take effect without waiting for the 60s cap.
    pub fn notify_changed(&self) {
        self.notify.notify_one();
    }
}

/// Decision produced by [`classify_misfire`] for a claimed scheduled message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MisfireDecision {
    /// Within grace window, or policy is `send_on_resume` — send immediately.
    Proceed,
    /// Past grace window with policy `skip` — mark missed (and advance if recurring).
    Skip { advance_recurring: bool },
    /// Past grace window with policy `ask` — mark missed so user can 1-click "Gửi ngay".
    Ask,
}

/// Returns `true` for recurring schedule cadences (`daily`, `weekly`, `monthly`).
pub fn is_recurring(recurrence: &str) -> bool {
    matches!(
        recurrence.trim().to_ascii_lowercase().as_str(),
        "daily" | "weekly" | "monthly"
    )
}

/// Classify whether a due scheduled item should send now, be skipped, or ask the user.
pub fn classify_misfire(
    send_at_ts: i64,
    now_ms: i64,
    misfire_policy: &str,
    misfire_grace_secs: i64,
    recurrence: &str,
) -> MisfireDecision {
    let grace_ms = misfire_grace_secs.max(0).saturating_mul(1_000);
    let lateness_ms = now_ms.saturating_sub(send_at_ts);
    if lateness_ms <= grace_ms {
        return MisfireDecision::Proceed;
    }

    match misfire_policy.trim().to_ascii_lowercase().as_str() {
        "skip" => MisfireDecision::Skip {
            advance_recurring: is_recurring(recurrence),
        },
        "ask" => MisfireDecision::Ask,
        "send_on_resume" => MisfireDecision::Proceed,
        _ => MisfireDecision::Proceed,
    }
}

/// Retry backoff delay in milliseconds for a given 0-based `retry_count`:
/// - attempt 0 failure -> retry after 30 seconds
/// - attempt 1 failure -> retry after 120 seconds
/// - attempt 2+ failure -> `None` (mark permanently `failed`)
pub fn compute_retry_backoff_ms(retry_count: u32) -> Option<i64> {
    if retry_count >= MAX_SCHEDULE_RETRIES {
        return None;
    }
    match retry_count {
        0 => Some(30_000),
        1 => Some(120_000),
        _ => None,
    }
}

/// Compute the absolute epoch-ms timestamp for the next retry, or `None` when retries
/// are exhausted.
pub fn next_retry_ts(retry_count: u32, now_ms: i64) -> Option<i64> {
    compute_retry_backoff_ms(retry_count).map(|delay| now_ms.saturating_add(delay))
}

/// Compute how long the scheduler loop should sleep before checking again:
/// `min(next_ts - now_ms, 60s)` when a future schedule exists, `0` if already due,
/// or `60s` when no pending schedules exist.
pub fn compute_sleep_duration(next_pending_ts: Option<i64>, now_ms: i64) -> Duration {
    let max_sleep = Duration::from_secs(MAX_SCHEDULER_SLEEP_SECS);
    match next_pending_ts {
        None => max_sleep,
        Some(ts) if ts <= now_ms => Duration::ZERO,
        Some(ts) => Duration::from_millis((ts - now_ms) as u64).min(max_sleep),
    }
}

/// Compute anti-spam pacing delay before sending the `sent_index`-th message in a batch.
/// The first message (`sent_index == 0`) sends without extra delay; subsequent messages
/// wait `pacing_ms`.
pub fn compute_batch_pacing_delay(sent_index: usize, pacing_ms: u64) -> Duration {
    if sent_index == 0 || pacing_ms == 0 {
        Duration::ZERO
    } else {
        Duration::from_millis(pacing_ms)
    }
}

/// Parse persisted thread-kind strings (`"direct"`, `"group"`, `"cloud"`) into [`ThreadKind`].
pub fn parse_thread_kind(raw: &str) -> ThreadKind {
    match raw.trim().to_ascii_lowercase().as_str() {
        "group" => ThreadKind::Group,
        "cloud" => ThreadKind::Cloud,
        _ => ThreadKind::Direct,
    }
}

/// Returns `true` if `err` (or any error in its cause chain) is [`SessionError::Expired`].
pub fn is_expired_err(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<SessionError>(),
            Some(SessionError::Expired { .. })
        )
    })
}

fn now_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn emit_schedule_updated(app: &AppHandle, updated: Option<&ScheduledMessageDto>) {
    if let Some(dto) = updated {
        let _ = app.emit("core://schedule-updated", dto);
    } else {
        let _ = app.emit("core://schedule-updated", serde_json::Value::Null);
    }
}

/// Spawn the background scheduled-message worker task.
///
/// On startup it recovers any crash-interrupted `'sending'` rows, then loops using
/// event-driven waits (`tokio::select!` on `sleep` vs `notify.notified()`) with zero
/// busy-waiting.
pub fn spawn_scheduler(
    app: AppHandle,
    sess_arc: Arc<Mutex<Session>>,
    store_arc: Arc<Mutex<MessageStore>>,
    notify: Arc<Notify>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        {
            let now = now_epoch_ms();
            let store = store_arc.lock().await;
            let _ = store.recover_stuck_sending_schedules(now);
        }

        loop {
            // Register waiter before querying SQLite so a `notify_changed()` racing with
            // the query still wakes the loop immediately.
            let notified = notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            let now = now_epoch_ms();
            let next_ts_res = {
                let store = store_arc.lock().await;
                store.next_pending_schedule_ts()
            };

            let next_ts = match next_ts_res {
                Ok(ts) => ts,
                Err(err) => {
                    eprintln!("[teza][scheduler] failed to query next schedule: {err}");
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(ERROR_BACKOFF_SECS)) => {}
                        _ = &mut notified => {}
                    }
                    continue;
                }
            };

            let sleep_dur = compute_sleep_duration(next_ts, now);
            if !sleep_dur.is_zero() {
                tokio::select! {
                    _ = tokio::time::sleep(sleep_dur) => {}
                    _ = &mut notified => {}
                }
                continue;
            }

            let claim_now = now_epoch_ms();
            let due_items = {
                let store = store_arc.lock().await;
                match store.claim_due_scheduled_messages(claim_now, SCHEDULE_BATCH_SIZE) {
                    Ok(items) => items,
                    Err(err) => {
                        eprintln!("[teza][scheduler] failed to claim due schedules: {err}");
                        Vec::new()
                    }
                }
            };

            if due_items.is_empty() {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(ERROR_BACKOFF_SECS)) => {}
                    _ = &mut notified => {}
                }
                continue;
            }

            let mut sent_in_batch: usize = 0;
            for item in due_items {
                let did_attempt_send =
                    process_claimed_schedule(&app, &sess_arc, &store_arc, item, sent_in_batch)
                        .await;
                if did_attempt_send {
                    sent_in_batch = sent_in_batch.saturating_add(1);
                }
            }
        }
    })
}

/// Process a single atomically-claimed scheduled item.
/// Returns `true` if a network send was attempted (so pacing applies to the next item).
async fn process_claimed_schedule(
    app: &AppHandle,
    sess_arc: &Arc<Mutex<Session>>,
    store_arc: &Arc<Mutex<MessageStore>>,
    item: ScheduledMessageDto,
    sent_in_batch: usize,
) -> bool {
    let now = now_epoch_ms();
    let grace_secs = if item.misfire_grace_secs > 0 {
        item.misfire_grace_secs
    } else {
        DEFAULT_MISFIRE_GRACE_SECS
    };

    match classify_misfire(
        item.send_at_ts,
        now,
        &item.misfire_policy,
        grace_secs,
        &item.recurrence,
    ) {
        MisfireDecision::Ask => {
            let updated = {
                let store = store_arc.lock().await;
                store
                    .mark_scheduled_message_missed(
                        &item.id,
                        "Đã quá giờ hẹn gửi — nhấn \"Gửi ngay\" để gửi bù",
                        now,
                    )
                    .ok()
                    .flatten()
            };
            emit_schedule_updated(app, updated.as_ref());
            return false;
        }
        MisfireDecision::Skip { .. } => {
            // `mark_scheduled_message_missed` atomically rolls `skip` + recurring
            // schedules forward to the next occurrence (status = "pending"), and marks
            // one-off schedules as "missed".
            let updated = {
                let store = store_arc.lock().await;
                store
                    .mark_scheduled_message_missed(
                        &item.id,
                        "Đã bỏ qua vì quá thời gian trễ cho phép",
                        now,
                    )
                    .ok()
                    .flatten()
            };
            emit_schedule_updated(app, updated.as_ref());
            return false;
        }
        MisfireDecision::Proceed => {}
    }

    // Pace consecutive sends within a batch to respect anti-spam limits.
    let pacing = compute_batch_pacing_delay(sent_in_batch, SCHEDULE_PACING_MS);
    if !pacing.is_zero() {
        tokio::time::sleep(pacing).await;
    }

    let sess_snapshot = {
        let sess = sess_arc.lock().await;
        sess.clone_shallow()
    };

    if !sess_snapshot.is_authenticated() {
        let retry_at = next_retry_ts(item.retry_count, now_epoch_ms());
        let updated = {
            let store = store_arc.lock().await;
            store
                .fail_or_retry_scheduled_message(
                    &item.id,
                    "Chưa đăng nhập tài khoản",
                    retry_at,
                    now_epoch_ms(),
                )
                .ok()
                .flatten()
        };
        emit_schedule_updated(app, updated.as_ref());
        return false;
    }

    let kind = if item.thread_kind.trim().is_empty() {
        let store = store_arc.lock().await;
        store
            .get_thread_kind(&item.thread_id)
            .ok()
            .flatten()
            .unwrap_or(ThreadKind::Direct)
    } else {
        parse_thread_kind(&item.thread_kind)
    };

    let att_trimmed = item.attachment_path.trim();
    let text_trimmed = item.text.trim();

    let send_result = if !att_trimmed.is_empty() {
        let att_path = Path::new(att_trimmed);
        match tokio::fs::metadata(att_path).await {
            Ok(meta) if meta.is_file() => {
                messaging::send_attachment_file(
                    &sess_snapshot,
                    &item.thread_id,
                    kind,
                    att_path,
                    text_trimmed,
                )
                .await
            }
            _ => {
                // Missing local attachment will not recover on automatic retry.
                let updated = {
                    let store = store_arc.lock().await;
                    store
                        .fail_or_retry_scheduled_message(
                            &item.id,
                            "Tệp đính kèm không còn tồn tại trên máy",
                            None,
                            now_epoch_ms(),
                        )
                        .ok()
                        .flatten()
                };
                emit_schedule_updated(app, updated.as_ref());
                return false;
            }
        }
    } else {
        if text_trimmed.is_empty() {
            let updated = {
                let store = store_arc.lock().await;
                store
                    .fail_or_retry_scheduled_message(
                        &item.id,
                        "Nội dung tin nhắn hẹn giờ trống",
                        None,
                        now_epoch_ms(),
                    )
                    .ok()
                    .flatten()
            };
            emit_schedule_updated(app, updated.as_ref());
            return false;
        }
        messaging::send_text(&sess_snapshot, &item.thread_id, kind, text_trimmed).await
    };

    let finish_now = now_epoch_ms();
    match send_result {
        Ok(msg) => {
            let updated_sched = {
                let mut store = store_arc.lock().await;
                let _ = store.upsert_message(&msg);
                let preview = format_thread_preview(&msg.text, true, "", false);
                let _ = store.update_thread_preview_returning_created(
                    &msg.thread_id,
                    &preview,
                    msg.ts,
                    false,
                    true,
                );
                store
                    .complete_scheduled_message(&item.id, finish_now)
                    .ok()
                    .flatten()
            };
            let _ = app.emit("core://message-new", &msg);
            let _ = app.emit("core://threads-updated", ());
            emit_schedule_updated(app, updated_sched.as_ref());
        }
        Err(err) => {
            let (err_text, retry_at) = if is_expired_err(&err) {
                (
                    "Phiên đăng nhập đã hết hạn. Vui lòng đăng nhập lại.".to_string(),
                    None,
                )
            } else {
                (
                    format!("Gửi tin nhắn hẹn giờ thất bại: {err}"),
                    next_retry_ts(item.retry_count, finish_now),
                )
            };
            let updated = {
                let store = store_arc.lock().await;
                store
                    .fail_or_retry_scheduled_message(&item.id, &err_text, retry_at, finish_now)
                    .ok()
                    .flatten()
            };
            emit_schedule_updated(app, updated.as_ref());
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_misfire_respects_grace_window_and_all_policies() {
        let send_at = 1_700_000_000_000_i64;
        let grace = 600_i64; // 10 minutes = 600_000 ms

        // Exactly at scheduled time and exactly on grace boundary -> Proceed regardless of policy
        assert_eq!(
            classify_misfire(send_at, send_at, "skip", grace, "once"),
            MisfireDecision::Proceed
        );
        assert_eq!(
            classify_misfire(send_at, send_at + 600_000, "ask", grace, "daily"),
            MisfireDecision::Proceed
        );

        // Past grace window (600_001 ms late)
        let late = send_at + 600_001;
        assert_eq!(
            classify_misfire(send_at, late, "skip", grace, "once"),
            MisfireDecision::Skip {
                advance_recurring: false
            }
        );
        assert_eq!(
            classify_misfire(send_at, late, "skip", grace, "daily"),
            MisfireDecision::Skip {
                advance_recurring: true
            }
        );
        assert_eq!(
            classify_misfire(send_at, late, "SKIP", grace, "weekly"),
            MisfireDecision::Skip {
                advance_recurring: true
            }
        );
        assert_eq!(
            classify_misfire(send_at, late, "skip", grace, "monthly"),
            MisfireDecision::Skip {
                advance_recurring: true
            }
        );
        assert_eq!(
            classify_misfire(send_at, late, "ask", grace, "once"),
            MisfireDecision::Ask
        );
        assert_eq!(
            classify_misfire(send_at, late, "ask", grace, "daily"),
            MisfireDecision::Ask
        );
        assert_eq!(
            classify_misfire(send_at, late, "send_on_resume", grace, "once"),
            MisfireDecision::Proceed
        );
        assert_eq!(
            classify_misfire(send_at, late, "unknown_policy", grace, "once"),
            MisfireDecision::Proceed
        );
    }

    #[test]
    fn retry_backoff_follows_30s_120s_then_exhausted() {
        let now = 1_700_000_000_000_i64;
        assert_eq!(compute_retry_backoff_ms(0), Some(30_000));
        assert_eq!(compute_retry_backoff_ms(1), Some(120_000));
        assert_eq!(compute_retry_backoff_ms(2), None);
        assert_eq!(compute_retry_backoff_ms(99), None);

        assert_eq!(next_retry_ts(0, now), Some(now + 30_000));
        assert_eq!(next_retry_ts(1, now), Some(now + 120_000));
        assert_eq!(next_retry_ts(2, now), None);
    }

    #[test]
    fn sleep_duration_and_batch_pacing_behave_accurately() {
        let now = 1_700_000_000_000_i64;
        assert_eq!(
            compute_sleep_duration(None, now),
            Duration::from_secs(MAX_SCHEDULER_SLEEP_SECS)
        );
        assert_eq!(compute_sleep_duration(Some(now), now), Duration::ZERO);
        assert_eq!(
            compute_sleep_duration(Some(now - 5_000), now),
            Duration::ZERO
        );
        assert_eq!(
            compute_sleep_duration(Some(now + 12_345), now),
            Duration::from_millis(12_345)
        );
        assert_eq!(
            compute_sleep_duration(Some(now + 300_000), now),
            Duration::from_secs(MAX_SCHEDULER_SLEEP_SECS)
        );

        // Batch pacing: first item immediate, subsequent items paced
        assert_eq!(
            compute_batch_pacing_delay(0, SCHEDULE_PACING_MS),
            Duration::ZERO
        );
        assert_eq!(
            compute_batch_pacing_delay(1, SCHEDULE_PACING_MS),
            Duration::from_millis(SCHEDULE_PACING_MS)
        );
        assert_eq!(
            compute_batch_pacing_delay(5, SCHEDULE_PACING_MS),
            Duration::from_millis(SCHEDULE_PACING_MS)
        );
        assert_eq!(compute_batch_pacing_delay(2, 0), Duration::ZERO);
    }

    #[test]
    fn expired_error_detection_handles_wrapped_session_errors() {
        let direct = anyhow::Error::new(SessionError::Expired { code: 401 });
        assert!(is_expired_err(&direct));

        let wrapped = direct.context("outer context");
        assert!(is_expired_err(&wrapped));

        let other = anyhow::anyhow!("network timeout");
        assert!(!is_expired_err(&other));
    }

    #[test]
    fn scheduler_handle_notify_wakes_waiter_immediately() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let handle = SchedulerHandle::new();
            let notify = handle.notify_arc();
            handle.notify_changed();
            let woke = tokio::time::timeout(Duration::from_millis(200), notify.notified()).await;
            assert!(woke.is_ok(), "stored notify permit should wake immediately");
        });
    }
}
