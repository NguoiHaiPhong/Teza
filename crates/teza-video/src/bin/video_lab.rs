//! `video_lab` — Standalone CLI harness for independently testing `teza-video`
//! (`cameras`, `loopback` over mock UDP relay, and live 1-1 `call` / `listen` via `teza-call`).

use std::io::BufRead;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use teza_call::{
    ensure_callable_session, format_trace_json, poll_voip_controls_with_trace,
    AudioDeviceSelection, CallEngine, CallEngineConfig, CallError, CallEvent, CallType,
    CtrlPollState, SignalingClient, TraceSink,
};
use teza_video::{
    is_ffmpeg_available, list_cameras, run_local_video_loopback, FfmpegCameraCapture,
    H264FileRecorder, InboundFrameCallback, VideoMediaHook, VideoSessionConfig,
};
use zalo_core::chat::parser::{parse_ws_event, VoipControlEvent, WsParsedAction};
use zalo_core::data_dir::StoragePaths;
use zalo_core::relay::RelayConfig;
use zalo_core::session::Session;
use zalo_core::socket::{self, WsSignal, WsStatus};

const VIDEO_VALUE_FLAGS: &[&str] = &[
    "--secs",
    "--dump",
    "--dump-rx",
    "--camera",
    "--zpw",
    "--imei",
    "--input",
    "--output",
    "--data-dir",
    "--profile",
    "--auto-answer",
    "--reject",
    "--hangup-after",
];

fn print_usage() {
    println!(
        "\
teza-video `video_lab` — Standalone 1-1 Video Call verification CLI

USAGE:
  video_lab cameras
  video_lab loopback [--secs 2] [--dump PATH.h264]
  video_lab call <UID> [--secs 45] [--cam] [--camera NAME] [--no-audio]
                 [--poll] [--trace] [--dump-rx PATH.h264]
                 [--zpw VER:TYPE] [--imei STR | --raw-imei]
                 [--input NAME] [--output NAME] [--data-dir PATH] [--profile NAME]
  video_lab listen [--secs 60] [--auto-answer [SECS]] [--once] [--cam] [--camera NAME]
                   [--no-audio] [--poll] [--trace] [--dump-rx PATH.h264]
                   [--zpw VER:TYPE] [--imei STR | --raw-imei]
                   [--input NAME] [--output NAME] [--data-dir PATH] [--profile NAME]

INTERACTIVE KEYS (during `call` and `listen`, press Enter after key):
  a = accept incoming call    d = decline incoming call
  m = toggle mic mute (12006) c = toggle camera (12006)
  s = status & media metrics  h / q = hangup / cancel     ? = show help
"
    );
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn flag_val(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|w| w[0] == flag && !w[1].starts_with("--"))
        .map(|w| w[1].clone())
}

fn parse_optional_delay_flag(args: &[String], flag: &str, default_secs: f64) -> Option<f64> {
    flag_val(args, flag)
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .or_else(|| has_flag(args, flag).then_some(default_secs))
}

fn first_positional(args: &[String], value_flags: &[&str]) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        let cur = &args[i];
        if cur.starts_with("--") {
            if value_flags.contains(&cur.as_str())
                && args.get(i + 1).is_some_and(|v| !v.starts_with("--"))
            {
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        return Some(cur.clone());
    }
    None
}

fn make_trace_sink(enabled: bool) -> Option<TraceSink> {
    if !enabled {
        return None;
    }
    let started = Instant::now();
    Some(TraceSink::new(move |msg| {
        println!("[TRACE +{:>6.3}s] {msg}", started.elapsed().as_secs_f64());
    }))
}

fn resolve_storage_paths(args: &[String]) -> Result<StoragePaths> {
    if let Some(dir) = flag_val(args, "--data-dir") {
        StoragePaths::from_dir(PathBuf::from(dir))
    } else {
        let profile = flag_val(args, "--profile");
        StoragePaths::resolve_default(profile.as_deref())
    }
}

fn load_session(args: &[String]) -> Result<(StoragePaths, Session)> {
    let paths = resolve_storage_paths(args)?;
    let relay = RelayConfig::load_from_file(&paths.relay_file());
    let sess = Session::load_from_file_with_relay(&paths.session_file(), relay)
        .with_context(|| format!("Không tải được session tại {}", paths.session_file().display()))?;
    ensure_callable_session(&sess)?;
    Ok((paths, sess))
}

fn parse_signaling_client(args: &[String], sess: &Session, trace: Option<TraceSink>) -> SignalingClient {
    let mut client = SignalingClient::new();
    if let Some(raw) = flag_val(args, "--zpw") {
        if let Some((ver, ty)) = raw.split_once(':') {
            let (ver, ty) = (ver.trim(), ty.trim());
            if !ver.is_empty() && !ty.is_empty() {
                client = client.with_zpw_override(ver, ty);
            }
        }
    }
    if let Some(imei) = flag_val(args, "--imei") {
        let trimmed = imei.trim();
        if !trimmed.is_empty() {
            client = client.with_imei_override(trimmed.to_string());
        }
    } else if has_flag(args, "--raw-imei") {
        client = client.with_imei_override(sess.imei.clone());
    }
    if let Some(sink) = trace {
        client = client.with_trace(sink);
    }
    client
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InteractiveCommand {
    Accept,
    Decline,
    ToggleMute,
    ToggleCam,
    Status,
    Hangup,
    Help,
}

fn parse_interactive_command(line: &str) -> Option<InteractiveCommand> {
    match line.trim().to_ascii_lowercase().as_str() {
        "a" | "accept" | "answer" => Some(InteractiveCommand::Accept),
        "d" | "decline" | "reject" => Some(InteractiveCommand::Decline),
        "m" | "mute" => Some(InteractiveCommand::ToggleMute),
        "c" | "cam" | "camera" => Some(InteractiveCommand::ToggleCam),
        "s" | "status" => Some(InteractiveCommand::Status),
        "h" | "hangup" | "q" | "quit" => Some(InteractiveCommand::Hangup),
        "?" | "help" => Some(InteractiveCommand::Help),
        _ => None,
    }
}

fn feed_stdin_commands<R: BufRead>(
    reader: &mut R,
    tx: &tokio::sync::mpsc::UnboundedSender<InteractiveCommand>,
) {
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if let Some(cmd) = parse_interactive_command(&line) {
                    if tx.send(cmd).is_err() || cmd == InteractiveCommand::Hangup {
                        break;
                    }
                } else if !line.trim().is_empty() {
                    println!(
                        "[KEYS] Unknown `{}`. Keys: a=accept d=decline m=mute c=cam s=status h/q=hangup ?=help",
                        line.trim()
                    );
                }
            }
            Err(_) => break,
        }
    }
}

fn spawn_stdin_commands() -> tokio::sync::mpsc::UnboundedReceiver<InteractiveCommand> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = std::thread::Builder::new()
        .name("video-lab-stdin".to_string())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut locked = stdin.lock();
            feed_stdin_commands(&mut locked, &tx);
        });
    rx
}

fn spawn_poll_task(
    sess: Session,
    initial_state: CtrlPollState,
    trace: Option<TraceSink>,
) -> (
    tokio::sync::mpsc::UnboundedReceiver<VoipControlEvent>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = tokio::spawn(async move {
        let mut state = initial_state;
        let mut err_backoff_ms = 2_000u64;
        loop {
            match poll_voip_controls_with_trace(&sess, &mut state, trace.as_ref()).await {
                Ok(events) => {
                    err_backoff_ms = 2_000;
                    for ev in events {
                        if tx.send(ev).is_err() {
                            return;
                        }
                    }
                    let delay_ms = if state.last_more { 50 } else { 400 };
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                }
                Err(
                    e @ (CallError::SessionExpired { .. }
                    | CallError::RelayUnsupported
                    | CallError::NotAuthenticated),
                ) => {
                    eprintln!("[POLL] stopping poller: {e}");
                    return;
                }
                Err(e) if e.is_timeout() => {
                    err_backoff_ms = 2_000;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) => {
                    eprintln!("[POLL] error (backing off {}ms): {e}", err_backoff_ms);
                    tokio::time::sleep(Duration::from_millis(err_backoff_ms)).await;
                    err_backoff_ms = (err_backoff_ms * 2).min(8_000);
                }
            }
        }
    });
    (rx, handle)
}

async fn wait_for_ws_connected(
    ws_rx: &mut tokio::sync::mpsc::UnboundedReceiver<WsSignal>,
    timeout: Duration,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                eprintln!("[WS] timed out waiting for WebSocket auth; continuing anyway...");
                return Ok(());
            }
            sig = ws_rx.recv() => {
                match sig {
                    Some(WsSignal::Status(WsStatus::Connected)) => {
                        println!("[WS] Connected.");
                        return Ok(());
                    }
                    Some(WsSignal::Status(st)) => println!("[WS] status={st:?}"),
                    Some(WsSignal::Expired { code, message }) => {
                        bail!("WebSocket session expired ({code}): {message}");
                    }
                    Some(WsSignal::Event(_)) => {}
                    None => bail!("WebSocket worker stopped before connecting"),
                }
            }
        }
    }
}

async fn print_status(engine: &CallEngine, video_hook: &VideoMediaHook) {
    let snap = engine.snapshot().await;
    let aud = engine.audio_telemetry().await;
    let vid = video_hook.telemetry();
    println!(
        "[STATUS] phase={:?} zrtp={} audio_media={} aux_media={}",
        engine.phase().await,
        engine.has_active_zrtp().await,
        engine.has_active_media().await,
        engine.has_active_aux_media().await,
    );
    if let Some(s) = snap {
        println!("[STATUS] snapshot={}", serde_json::to_string(&s).unwrap_or_default());
    }
    if let Some(a) = aud {
        println!(
            "[STATUS] audio mic={}‰ remote={}‰ tx={} rx={} err={}",
            a.mic_level_permille,
            a.remote_level_permille,
            a.tx_packets,
            a.rx_packets,
            a.tx_errors,
        );
    }
    if let Some(v) = vid {
        println!("[STATUS] video={}", serde_json::to_string(&v).unwrap_or_default());
    }
}

async fn handle_interactive_command(
    cmd: InteractiveCommand,
    engine: &CallEngine,
    video_hook: &VideoMediaHook,
) -> bool {
    match cmd {
        InteractiveCommand::Accept => {
            println!("[CMD] Accepting incoming call...");
            if let Err(e) = engine.answer_call(true).await {
                eprintln!("[CMD] accept failed: {e}");
            }
            false
        }
        InteractiveCommand::Decline => {
            println!("[CMD] Declining incoming call...");
            if let Err(e) = engine.answer_call(false).await {
                eprintln!("[CMD] decline failed: {e}");
            }
            false
        }
        InteractiveCommand::ToggleMute => {
            let next = !engine.snapshot().await.map(|s| s.mic_muted).unwrap_or(false);
            match engine.set_mic_muted(next).await {
                Ok(()) => println!("[CMD] mic_muted -> {next}"),
                Err(e) => eprintln!("[CMD] set_mic_muted failed: {e}"),
            }
            false
        }
        InteractiveCommand::ToggleCam => {
            let next = !engine.snapshot().await.map(|s| s.cam_enabled).unwrap_or(true);
            match engine.set_cam_enabled(next).await {
                Ok(()) => println!("[CMD] cam_enabled -> {next}"),
                Err(e) => eprintln!("[CMD] set_cam_enabled failed: {e}"),
            }
            false
        }
        InteractiveCommand::Status => {
            print_status(engine, video_hook).await;
            false
        }
        InteractiveCommand::Hangup => {
            println!("[CMD] Hangup requested; ending call...");
            true
        }
        InteractiveCommand::Help => {
            println!(
                "[KEYS] a=accept d=decline m=toggle mic c=toggle cam s=status h/q=hangup"
            );
            false
        }
    }
}

fn build_video_hook_and_camera(args: &[String]) -> Result<(Arc<VideoMediaHook>, Option<FfmpegCameraCapture>)> {
    let recorder = match flag_val(args, "--dump-rx") {
        Some(path) => Some(Arc::new(H264FileRecorder::create(path)?)),
        None => None,
    };
    let on_inbound: Option<InboundFrameCallback> = recorder.map(|rec| -> InboundFrameCallback {
        Arc::new(move |frame| {
            let _ = rec.write_au(&frame.annexb_au);
            if frame.just_synced {
                println!(
                    "[VIDEO SYNC] call_id={} ssrc=0x{:08x} first keyframe len={}B",
                    frame.call_id,
                    frame.ssrc,
                    frame.annexb_au.len()
                );
            }
        })
    });

    let hook = Arc::new(VideoMediaHook::new(VideoSessionConfig {
        on_inbound_frame: on_inbound,
        ..Default::default()
    }));

    let cam_dev = flag_val(args, "--camera");
    let use_real_cam = has_flag(args, "--cam") || cam_dev.is_some();
    let cam_handle = if use_real_cam {
        let hook_cb = hook.clone();
        match FfmpegCameraCapture::start(
            cam_dev.as_deref(),
            Arc::new(move |au| {
                let _ = hook_cb.push_outbound_au(&au);
            }),
        ) {
            Ok(cap) => {
                println!("[CAM] Started ffmpeg webcam capture.");
                Some(cap)
            }
            Err(e) => {
                eprintln!("[CAM] Real webcam capture unavailable ({e}); falling back to synthetic H.264 stream.");
                None
            }
        }
    } else {
        None
    };

    Ok((hook, cam_handle))
}

struct LoopModeOptions {
    secs: u64,
    trace_enabled: bool,
    once: bool,
    auto_answer: Option<f64>,
    reject_delay: Option<f64>,
    hangup_after: Option<f64>,
    my_uid: String,
    send2me: String,
}

async fn run_video_call_event_loop(
    engine: Arc<CallEngine>,
    video_hook: Arc<VideoMediaHook>,
    mut ws_rx: tokio::sync::mpsc::UnboundedReceiver<WsSignal>,
    mut poll_rx: Option<tokio::sync::mpsc::UnboundedReceiver<VoipControlEvent>>,
    poll_task: Option<tokio::task::JoinHandle<()>>,
    opts: LoopModeOptions,
) -> Result<()> {
    let mut ev_rx = engine.subscribe();
    let mut stdin_rx = Some(spawn_stdin_commands());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(opts.secs);
    let mut stats_tick = tokio::time::interval(Duration::from_secs(2));
    stats_tick.tick().await;
    let mut expired_err: Option<anyhow::Error> = None;

    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            _ = tokio::signal::ctrl_c() => {
                println!("Ctrl+C received; hanging up cleanly...");
                break;
            }
            stdin_cmd = async {
                match stdin_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match stdin_cmd {
                    Some(cmd) => {
                        if handle_interactive_command(cmd, &engine, &video_hook).await {
                            break;
                        }
                    }
                    None => stdin_rx = None,
                }
            }
            _ = stats_tick.tick() => {
                if let Some(v) = video_hook.telemetry() {
                    let a_str = if let Some(a) = engine.audio_telemetry().await {
                        format!("audio(tx={},rx={},mic={}‰,rem={}‰)", a.tx_packets, a.rx_packets, a.mic_level_permille, a.remote_level_permille)
                    } else {
                        "audio(off)".to_string()
                    };
                    println!(
                        "[MEDIA] {a_str} video(cam={} peer_vid={} synced={} tx_f={}/kf={} rx_f={}/kf={} pre_sync_drop={} drop_inc={} pli_tx={} pli_rx={})",
                        v.cam_enabled,
                        v.peer_video_active,
                        v.video_synced,
                        v.tx_frames,
                        v.tx_keyframes,
                        v.rx_frames,
                        v.rx_keyframes,
                        v.rx_dropped_before_sync,
                        v.rx_dropped_incomplete,
                        v.pli_sent,
                        v.pli_received,
                    );
                }
            }
            poll_ev = async {
                match poll_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match poll_ev {
                    Some(v) => {
                        if opts.trace_enabled {
                            println!(
                                "[POLL] act={} call_id={} status={:?} data={}",
                                v.act,
                                v.call_id(),
                                v.status(),
                                format_trace_json(&v.data, 800)
                            );
                        }
                        if let Err(e) = engine.handle_voip_event(&v).await {
                            eprintln!("[POLL] handle_voip_event error: {e}");
                        }
                    }
                    None => poll_rx = None,
                }
            }
            res = ev_rx.recv() => {
                match res {
                    Ok(ev) => {
                        println!("[EVENT] {}", serde_json::to_string(&ev)?);
                        match ev {
                            CallEvent::IncomingCall { call_id, .. } => {
                                if let Some(delay) = opts.reject_delay {
                                    let eng = engine.clone();
                                    tokio::spawn(async move {
                                        if delay > 0.0 {
                                            tokio::time::sleep(Duration::from_secs_f64(delay.clamp(0.0, 60.0))).await;
                                        }
                                        let _ = eng.answer_call_by_id(Some(call_id), false).await;
                                    });
                                } else if let Some(delay) = opts.auto_answer {
                                    let eng = engine.clone();
                                    tokio::spawn(async move {
                                        if delay > 0.0 {
                                            tokio::time::sleep(Duration::from_secs_f64(delay.clamp(0.0, 60.0))).await;
                                        }
                                        let _ = eng.answer_call_by_id(Some(call_id), true).await;
                                    });
                                } else {
                                    println!("[INCOMING] call_id={call_id} — type `a` + Enter to accept or `d` + Enter to decline.");
                                }
                            }
                            CallEvent::Connected { call_id, .. } => {
                                if let Some(delay) = opts.hangup_after {
                                    let eng = engine.clone();
                                    tokio::spawn(async move {
                                        if delay > 0.0 {
                                            tokio::time::sleep(Duration::from_secs_f64(delay.clamp(0.0, 3600.0))).await;
                                        }
                                        if eng.snapshot().await.is_some_and(|s| s.call_id == call_id) {
                                            let _ = eng.end_call().await;
                                        }
                                    });
                                }
                            }
                            CallEvent::CallEnded { .. } if opts.once => break,
                            _ => {}
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        eprintln!("[EVENT] lagged {n} events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            sig = ws_rx.recv() => {
                match sig {
                    Some(WsSignal::Status(st)) => println!("[WS] status={st:?}"),
                    Some(WsSignal::Expired { code, message }) => {
                        expired_err = Some(anyhow::anyhow!("WebSocket session expired ({code}): {message}"));
                        break;
                    }
                    Some(WsSignal::Event(ws_ev)) => {
                        for parsed in parse_ws_event(&ws_ev, &opts.my_uid, &opts.send2me) {
                            if let WsParsedAction::Voip(v) = parsed {
                                if opts.trace_enabled {
                                    println!(
                                        "[WS cmd={}] act={} call_id={} status={:?} data={}",
                                        ws_ev.cmd,
                                        v.act,
                                        v.call_id(),
                                        v.status(),
                                        format_trace_json(&v.data, 800)
                                    );
                                }
                                if let Err(e) = engine.handle_voip_event(&v).await {
                                    eprintln!("[WS] handle_voip_event error: {e}");
                                }
                            }
                        }
                    }
                    None => break,
                }
            }
        }
    }

    let _ = engine.end_call().await;
    if let Some(h) = poll_task {
        h.abort();
    }
    if let Some(err) = expired_err {
        return Err(err);
    }
    Ok(())
}

fn cmd_cameras() {
    println!("ffmpeg_in_path: {}", is_ffmpeg_available());
    let cams = list_cameras();
    println!("=== VIDEO CAMERAS ({}) ===", cams.len());
    for (idx, name) in cams.iter().enumerate() {
        println!("  [{idx}] {name}");
    }
}

fn cmd_loopback(args: &[String]) -> Result<()> {
    let secs: u64 = flag_val(args, "--secs")
        .and_then(|v| v.parse().ok())
        .unwrap_or(2)
        .clamp(1, 30);
    let dump_path = flag_val(args, "--dump").map(PathBuf::from);
    let telem = run_local_video_loopback(Duration::from_secs(secs), dump_path.as_deref())?;
    println!("{}", serde_json::to_string_pretty(&telem)?);
    if let Some(p) = dump_path {
        println!("Saved Annex-B H.264 capture to {}", p.display());
    }
    Ok(())
}

async fn cmd_call(args: &[String]) -> Result<()> {
    let uid = first_positional(args, VIDEO_VALUE_FLAGS)
        .context("Thiếu <UID> người nhận (vd: video_lab call 1000000000000000002)")?;
    let secs: u64 = flag_val(args, "--secs").and_then(|v| v.parse().ok()).unwrap_or(45);
    let enable_audio = !has_flag(args, "--no-audio");
    let use_poll = has_flag(args, "--poll");
    let trace_enabled = has_flag(args, "--trace");
    let trace = make_trace_sink(trace_enabled);
    let devices = AudioDeviceSelection {
        input_device_name: flag_val(args, "--input"),
        output_device_name: flag_val(args, "--output"),
    };

    let (_paths, sess) = load_session(args)?;
    let sig_client = parse_signaling_client(args, &sess, trace.clone());
    let (ws_sess, poll_sess) = (sess.clone_shallow(), sess.clone_shallow());
    let (my_uid, send2me) = (sess.profile.uid.clone(), sess.profile.send2me_id.clone());

    let (video_hook, cam_capture) = build_video_hook_and_camera(args)?;
    let engine = Arc::new(
        CallEngine::with_signaling_client(
            sess,
            sig_client,
            CallEngineConfig {
                enable_audio,
                devices,
                trace: trace.clone(),
            },
        )?
        .with_aux_media_hook(video_hook.clone()),
    );

    let mut ws_rx = socket::spawn(ws_sess);
    wait_for_ws_connected(&mut ws_rx, Duration::from_secs(5)).await?;

    let (poll_rx, poll_task) = if use_poll {
        let mut init_state = CtrlPollState::new_live_only();
        let prime_deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while !init_state.is_backlog_synced() && tokio::time::Instant::now() < prime_deadline {
            let remain = prime_deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(
                remain,
                poll_voip_controls_with_trace(&poll_sess, &mut init_state, trace.as_ref()),
            )
            .await
            {
                Ok(Ok(_)) if !init_state.is_backlog_synced() => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                _ => break,
            }
        }
        let (rx, handle) = spawn_poll_task(poll_sess, init_state, trace);
        (Some(rx), Some(handle))
    } else {
        (None, None)
    };

    println!(
        "=== VIDEO_LAB 1-1 CALL -> {uid} (audio={enable_audio}, http_poll={use_poll}, trace={trace_enabled}, secs={secs}) ==="
    );
    let call_id = engine
        .make_call(uid.clone(), uid.clone(), CallType::DirectVideo)
        .await?;
    println!("Dialed video call_id={call_id}. Keys: m=mute, c=cam, s=status, h/q=hangup, ?=help.");

    let res = run_video_call_event_loop(
        engine,
        video_hook.clone(),
        ws_rx,
        poll_rx,
        poll_task,
        LoopModeOptions {
            secs,
            trace_enabled,
            once: true,
            auto_answer: None,
            reject_delay: None,
            hangup_after: None,
            my_uid,
            send2me,
        },
    )
    .await;

    drop(cam_capture);
    video_hook.stop_active();
    res
}

async fn cmd_listen(args: &[String]) -> Result<()> {
    let secs: u64 = flag_val(args, "--secs").and_then(|v| v.parse().ok()).unwrap_or(60);
    let enable_audio = !has_flag(args, "--no-audio");
    let use_poll = has_flag(args, "--poll");
    let trace_enabled = has_flag(args, "--trace");
    let trace = make_trace_sink(trace_enabled);
    let devices = AudioDeviceSelection {
        input_device_name: flag_val(args, "--input"),
        output_device_name: flag_val(args, "--output"),
    };

    let (_paths, sess) = load_session(args)?;
    let sig_client = parse_signaling_client(args, &sess, trace.clone());
    let (ws_sess, poll_sess) = (sess.clone_shallow(), sess.clone_shallow());
    let (my_uid, send2me) = (sess.profile.uid.clone(), sess.profile.send2me_id.clone());

    let (video_hook, cam_capture) = build_video_hook_and_camera(args)?;
    let engine = Arc::new(
        CallEngine::with_signaling_client(
            sess,
            sig_client,
            CallEngineConfig {
                enable_audio,
                devices,
                trace: trace.clone(),
            },
        )?
        .with_aux_media_hook(video_hook.clone()),
    );

    let ws_rx = socket::spawn(ws_sess);
    let (poll_rx, poll_task) = if use_poll {
        let (rx, handle) = spawn_poll_task(poll_sess, CtrlPollState::new_live_only(), trace);
        (Some(rx), Some(handle))
    } else {
        (None, None)
    };

    println!(
        "=== VIDEO_LAB LISTEN (secs={secs}, audio={enable_audio}, http_poll={use_poll}, trace={trace_enabled}) ==="
    );
    let res = run_video_call_event_loop(
        engine,
        video_hook.clone(),
        ws_rx,
        poll_rx,
        poll_task,
        LoopModeOptions {
            secs,
            trace_enabled,
            once: has_flag(args, "--once"),
            auto_answer: parse_optional_delay_flag(args, "--auto-answer", 1.0),
            reject_delay: parse_optional_delay_flag(args, "--reject", 0.0),
            hangup_after: flag_val(args, "--hangup-after")
                .and_then(|v| v.parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v >= 0.0),
            my_uid,
            send2me,
        },
    )
    .await;

    drop(cam_capture);
    video_hook.stop_active();
    res
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(String::as_str) else {
        print_usage();
        return Ok(());
    };
    let rest = &args[1..];
    match cmd {
        "cameras" => {
            cmd_cameras();
            Ok(())
        }
        "loopback" => cmd_loopback(rest),
        "call" => cmd_call(rest).await,
        "listen" => cmd_listen(rest).await,
        "-h" | "--help" | "help" => {
            print_usage();
            Ok(())
        }
        other => {
            print_usage();
            bail!("Lệnh không hợp lệ: `{other}`")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_video_lab_interactive_keys_and_cli_flags() {
        assert_eq!(parse_interactive_command("a"), Some(InteractiveCommand::Accept));
        assert_eq!(parse_interactive_command("c"), Some(InteractiveCommand::ToggleCam));
        assert_eq!(parse_interactive_command("cam"), Some(InteractiveCommand::ToggleCam));
        assert_eq!(parse_interactive_command("m"), Some(InteractiveCommand::ToggleMute));
        assert_eq!(parse_interactive_command("q"), Some(InteractiveCommand::Hangup));

        let args = vec![
            "--secs".to_string(),
            "12".to_string(),
            "--cam".to_string(),
            "2000".to_string(),
        ];
        assert!(has_flag(&args, "--cam"));
        assert_eq!(flag_val(&args, "--secs").as_deref(), Some("12"));
        assert_eq!(first_positional(&args, VIDEO_VALUE_FLAGS).as_deref(), Some("2000"));
    }
}
