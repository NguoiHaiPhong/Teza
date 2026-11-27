//! Background scheduled-message delivery worker (Item #2).
//!
//! Sleeps efficiently until the earliest pending schedule timestamp (capped at 60s)
//! or until woken immediately via [`SchedulerHandle::notify_changed`]. Claims due
//! schedules atomically from SQLite so duplicate sends cannot occur, handles misfire
//! policies (`skip`, `ask`, `send_on_resume`), paces batch sends, and applies bounded
//! retry backoff on transient errors.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex, Notify, RwLock};
use tokio::task::JoinHandle;

use zalo_core::chat::messaging;
use zalo_core::chat::parser::format_thread_preview;
use zalo_core::chat::ThreadKind;
use zalo_core::media_vault::MediaVault;
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

/// Resolve the effective grace window in seconds: honors explicit `0` (strict 0s grace)
/// and positive values, only falling back to [`DEFAULT_MISFIRE_GRACE_SECS`] if negative.
pub fn effective_misfire_grace_secs(raw_grace_secs: i64) -> i64 {
    if raw_grace_secs < 0 {
        DEFAULT_MISFIRE_GRACE_SECS
    } else {
        raw_grace_secs
    }
}

/// Evaluate misfire decision for a claimed [`ScheduledMessageDto`].
/// Always returns [`MisfireDecision::Proceed`] when `item.bypass_misfire == true` ("Gửi ngay"),
/// honors explicit `misfire_grace_secs == 0`, and measures lateness against canonical `item.send_at_ts`.
pub fn evaluate_misfire_decision(item: &ScheduledMessageDto, now_ms: i64) -> MisfireDecision {
    if item.bypass_misfire {
        return MisfireDecision::Proceed;
    }
    let grace_secs = effective_misfire_grace_secs(item.misfire_grace_secs);
    classify_misfire(
        item.send_at_ts,
        now_ms,
        &item.misfire_policy,
        grace_secs,
        &item.recurrence,
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

enum ScheduleStepOutcome {
    NoNetworkAttempt,
    Attempted,
    Unauthenticated,
    SessionExpired { expected_uid: String, reason: String },
}

/// Spawn the background scheduled-message worker task.
///
/// On startup it recovers any crash-interrupted `'sending'` rows (when authenticated), then
/// loops using event-driven waits (`tokio::select!` on `sleep` vs `notify.notified()`) with
/// zero busy-waiting. When unauthenticated, it parks cleanly on `notify.notified()` without
/// claiming due schedules or burning retries.
pub fn spawn_scheduler(
    app: AppHandle,
    sess_arc: Arc<Mutex<Session>>,
    store_arc: Arc<Mutex<MessageStore>>,
    media_vault_arc: Arc<RwLock<Arc<MediaVault>>>,
    gdrive_arc: Arc<crate::GDriveRuntime>,
    notify: Arc<Notify>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        {
            let is_authed = sess_arc.lock().await.is_authenticated();
            if is_authed {
                let now = now_epoch_ms();
                let store = store_arc.lock().await;
                let _ = store.recover_stuck_sending_schedules(now);
            }
        }

        loop {
            // Register waiter before checking auth/SQLite so a `notify_changed()` racing with
            // the query (e.g. right after QR login) still wakes the loop immediately.
            let notified = notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            // #3: Never claim due schedules or burn retries while unauthenticated (QR login
            // calls `notify_changed()`, waking this task cleanly so `misfire_policy` evaluates
            // against canonical `send_at_ts`).
            let is_authed = {
                let sess = sess_arc.lock().await;
                sess.is_authenticated()
            };
            if !is_authed {
                notified.await;
                continue;
            }

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

            // Re-verify authentication right before atomically claiming due rows.
            if !sess_arc.lock().await.is_authenticated() {
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
            let mut iter = due_items.into_iter();
            while let Some(item) = iter.next() {
                match process_claimed_schedule(
                    &app,
                    &sess_arc,
                    &store_arc,
                    &media_vault_arc,
                    &gdrive_arc,
                    item,
                    sent_in_batch,
                )
                .await
                {
                    ScheduleStepOutcome::Attempted => {
                        sent_in_batch = sent_in_batch.saturating_add(1);
                    }
                    ScheduleStepOutcome::NoNetworkAttempt => {}
                    ScheduleStepOutcome::Unauthenticated => {
                        // Revert any remaining claimed rows in this batch without burning retries.
                        let remaining: Vec<ScheduledMessageDto> = iter.collect();
                        if !remaining.is_empty() {
                            let store = store_arc.lock().await;
                            for rem in remaining {
                                let _ = store.revert_scheduled_message_to_pending(&rem.id);
                            }
                        }
                        break;
                    }
                    ScheduleStepOutcome::SessionExpired {
                        expected_uid,
                        reason,
                    } => {
                        // Still same account? Revert any remaining claimed rows BEFORE
                        // `clear_expired_session_for_app` swaps `store_arc` to anonymous history.db.
                        let same_account = {
                            let sess = sess_arc.lock().await;
                            sess.is_authenticated() && sess.profile.uid == expected_uid
                        };
                        if same_account {
                            let remaining: Vec<ScheduledMessageDto> = iter.collect();
                            if !remaining.is_empty() {
                                let reverted: Vec<Option<ScheduledMessageDto>> = {
                                    let store = store_arc.lock().await;
                                    remaining
                                        .into_iter()
                                        .map(|rem| {
                                            store
                                                .revert_scheduled_message_to_pending(&rem.id)
                                                .ok()
                                                .flatten()
                                        })
                                        .collect()
                                };
                                for updated in reverted {
                                    if updated.is_some() {
                                        emit_schedule_updated(&app, updated.as_ref());
                                    }
                                }
                            }
                            crate::clear_expired_session_for_app(&app, reason, true).await;
                        }
                        break;
                    }
                }
            }
        }
    })
}

/// Process a single atomically-claimed scheduled item.
async fn process_claimed_schedule(
    app: &AppHandle,
    sess_arc: &Arc<Mutex<Session>>,
    store_arc: &Arc<Mutex<MessageStore>>,
    media_vault_arc: &Arc<RwLock<Arc<MediaVault>>>,
    gdrive_arc: &Arc<crate::GDriveRuntime>,
    item: ScheduledMessageDto,
    sent_in_batch: usize,
) -> ScheduleStepOutcome {
    let now = now_epoch_ms();
    match evaluate_misfire_decision(&item, now) {
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
            return ScheduleStepOutcome::NoNetworkAttempt;
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
            return ScheduleStepOutcome::NoNetworkAttempt;
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
        let updated = {
            let store = store_arc.lock().await;
            store
                .revert_scheduled_message_to_pending(&item.id)
                .ok()
                .flatten()
        };
        if updated.is_some() {
            emit_schedule_updated(app, updated.as_ref());
        }
        return ScheduleStepOutcome::Unauthenticated;
    }

    let expected_uid = sess_snapshot.profile.uid.clone();

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
                return ScheduleStepOutcome::NoNetworkAttempt;
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
            return ScheduleStepOutcome::NoNetworkAttempt;
        }
        messaging::send_text(&sess_snapshot, &item.thread_id, kind, text_trimmed).await
    };

    // #6: Re-check that the active account UID has not changed mid-send before mutating
    // `store_arc`, ingesting into `media_vault`, emitting UI events, or clearing session.
    let same_account = {
        let sess = sess_arc.lock().await;
        sess.is_authenticated() && sess.profile.uid == expected_uid
    };
    if !same_account {
        eprintln!(
            "[teza][scheduler] account switched mid-send for schedule {}; aborting DB/UI updates",
            item.id
        );
        return ScheduleStepOutcome::Attempted;
    }

    let finish_now = now_epoch_ms();
    match send_result {
        Ok(msg) => {
            let fallback_title = Some(item.thread_title.trim()).filter(|s| !s.is_empty());
            let updated_sched = {
                let mut store = store_arc.lock().await;
                let _ = store.upsert_message(&msg);
                let preview = format_thread_preview(&msg.text, true, "", false);
                let _ = store.update_thread_preview_with_fallback_kind_and_meta(
                    &msg.thread_id,
                    &preview,
                    msg.ts,
                    false,
                    true,
                    Some(kind),
                    fallback_title,
                    None,
                );
                store
                    .complete_scheduled_message(&item.id, finish_now)
                    .ok()
                    .flatten()
            };
            let _ = app.emit("core://message-new", &msg);
            let _ = app.emit("core://threads-updated", ());
            emit_schedule_updated(app, updated_sched.as_ref());

            // #31: Ingest sent attachment into MediaVault and notify Google Drive sync.
            if !att_trimmed.is_empty() {
                let src_path = PathBuf::from(att_trimmed);
                let file_name = src_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or("attachment.bin")
                    .to_string();
                let media_kind = crate::infer_media_kind_for_attachment(&file_name, &msg.msg_type);
                let mime = crate::infer_mime_from_filename_and_bytes(&file_name, None);
                let url_opt = msg.attachment_url.clone();
                let tid = msg.thread_id.clone();
                let mid = msg.msg_id.clone();
                let orig_name = msg.attachment_name.clone().or(Some(file_name));
                let vault = media_vault_arc.read().await.clone();
                let store_ingest = store_arc.clone();
                let gdrive_ingest = gdrive_arc.clone();
                let sess_ingest = sess_arc.clone();
                let expected_uid_ingest = expected_uid.clone();
                tokio::spawn(async move {
                    let still_same = {
                        let sess = sess_ingest.lock().await;
                        sess.is_authenticated() && sess.profile.uid == expected_uid_ingest
                    };
                    if !still_same {
                        return;
                    }
                    let _ = vault
                        .ingest_local_file(
                            &store_ingest,
                            &src_path,
                            media_kind,
                            mime,
                            url_opt.as_deref(),
                            Some((tid.as_str(), mid.as_str())),
                            orig_name.as_deref(),
                        )
                        .await;
                    gdrive_ingest.notify_sync();
                });
            }
            gdrive_arc.notify_sync();
            ScheduleStepOutcome::Attempted
        }
        Err(err) => {
            if is_expired_err(&err) {
                // #5: Revert schedule back to 'pending' WITHOUT incrementing retry_count,
                // BEFORE `clear_expired_session_for_app` swaps `store_arc` to anonymous DB.
                let updated = {
                    let store = store_arc.lock().await;
                    store
                        .revert_scheduled_message_to_pending(&item.id)
                        .ok()
                        .flatten()
                };
                emit_schedule_updated(app, updated.as_ref());
                return ScheduleStepOutcome::SessionExpired {
                    expected_uid,
                    reason: "Phiên đăng nhập đã hết hạn. Vui lòng quét lại mã QR.".to_string(),
                };
            }
            let err_text = format!("Gửi tin nhắn hẹn giờ thất bại: {err}");
            let retry_at = next_retry_ts(item.retry_count, finish_now);
            let updated = {
                let store = store_arc.lock().await;
                store
                    .fail_or_retry_scheduled_message(&item.id, &err_text, retry_at, finish_now)
                    .ok()
                    .flatten()
            };
            emit_schedule_updated(app, updated.as_ref());
            ScheduleStepOutcome::Attempted
        }
    }
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

    #[test]
    fn evaluate_misfire_respects_bypass_and_explicit_zero_grace_secs() {
        let send_at = 1_700_000_000_000_i64;
        let base_item = ScheduledMessageDto {
            id: "sched_1".into(),
            thread_id: "u_1".into(),
            thread_title: "User 1".into(),
            thread_kind: "direct".into(),
            text: "Hello".into(),
            attachment_path: String::new(),
            send_at_ts: send_at,
            next_attempt_ts: Some(send_at + 120_000),
            bypass_misfire: false,
            anchor_day: Some(15),
            recurrence: "daily".into(),
            misfire_policy: "ask".into(),
            misfire_grace_secs: 0,
            status: "sending".into(),
            retry_count: 1,
            last_error: String::new(),
            created_at: send_at - 60_000,
            updated_at: send_at,
            sent_at_ts: 0,
        };

        // Explicit 0s grace must be honored (NOT overridden to 600s!):
        // even 1ms late triggers Ask (and lateness is measured against canonical send_at_ts).
        assert_eq!(effective_misfire_grace_secs(0), 0);
        assert_eq!(effective_misfire_grace_secs(-1), DEFAULT_MISFIRE_GRACE_SECS);
        assert_eq!(effective_misfire_grace_secs(30), 30);

        assert_eq!(
            evaluate_misfire_decision(&base_item, send_at),
            MisfireDecision::Proceed
        );
        assert_eq!(
            evaluate_misfire_decision(&base_item, send_at + 1),
            MisfireDecision::Ask
        );

        let mut skip_zero = base_item.clone();
        skip_zero.misfire_policy = "skip".into();
        assert_eq!(
            evaluate_misfire_decision(&skip_zero, send_at + 1),
            MisfireDecision::Skip {
                advance_recurring: true
            }
        );

        // bypass_misfire == true ("Gửi ngay") must ALWAYS Proceed regardless of lateness/policy.
        let mut send_now_item = base_item.clone();
        send_now_item.bypass_misfire = true;
        assert_eq!(
            evaluate_misfire_decision(&send_now_item, send_at + 86_400_000),
            MisfireDecision::Proceed
        );

        let mut send_now_skip = skip_zero;
        send_now_skip.bypass_misfire = true;
        assert_eq!(
            evaluate_misfire_decision(&send_now_skip, send_at + 86_400_000),
            MisfireDecision::Proceed
        );
    }
}
