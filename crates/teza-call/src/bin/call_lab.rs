//! `call_lab` — Standalone CLI harness for independently testing `teza-call`
//! (UDP Echo, UDP ZRTP relay probe, local Opus/cpal audio loopback, VoIP WS+lp_control watch,
//! and interactive 1-1 calls with sanitized `--trace` diagnostics).

use std::io::BufRead;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use teza_call::{
    ensure_callable_session, format_trace_json, list_input_devices, list_output_devices,
    normalize_call_imei, poll_voip_controls_with_trace, probe_udp_echo, resolve_voice_call_host,
    session_uid_u32, AudioDeviceSelection, AudioLoopbackSession, CallEngine, CallEngineConfig,
    CallError, CallEvent, CallType, CtrlPollState, RelayServer, SignalingClient, TraceSink,
    ZrtpProbeConfig, ZrtpRelaySession,
};
use zalo_core::chat::parser::{parse_ws_event, VoipControlEvent, WsParsedAction};
use zalo_core::data_dir::StoragePaths;
use zalo_core::relay::RelayConfig;
use zalo_core::session::{Session, ZPW_TYPE, ZPW_VER};
use zalo_core::socket::{self, WsSignal, WsStatus};

fn print_usage() {
    println!(
        "\
teza-call `call_lab` — Standalone VoIP verification CLI

USAGE:
  call_lab info [--data-dir PATH] [--profile NAME]
  call_lab devices
  call_lab audio-loopback [--secs 5] [--input NAME] [--output NAME] [--loss-pct 0] [--tone [HZ]]
  call_lab echo <IP:PORT> [--count 6] [--timeout-ms 1500] [--trace]
  call_lab zrtp-probe <IP:PORT> --from-uid U32 --partner-uid U32 --call-id U32 --sess STR
                      [--callee] [--rtcp-addr IP:PORT] [--hold-ms 600] [--trace]
  call_lab watch [--secs 120] [--poll] [--trace] [--data-dir PATH] [--profile NAME]
  call_lab call <UID> [--secs 60] [--video] [--no-audio] [--poll] [--trace]
                [--zpw VER:TYPE] [--imei STR | --raw-imei]
                [--input NAME] [--output NAME] [--data-dir PATH] [--profile NAME]
  call_lab serve [--auto-answer [SECS]] [--reject [SECS]] [--hangup-after SECS] [--once]
                 [--no-audio] [--poll] [--trace] [--secs 180]
                 [--zpw VER:TYPE] [--imei STR | --raw-imei]
                 [--input NAME] [--output NAME] [--data-dir PATH] [--profile NAME]

INTERACTIVE KEYS (during `call` and `serve`, press Enter after key):
  a = accept incoming call    d = decline incoming call
  m = toggle mic mute (12006) s = status & audio telemetry
  h / q = hangup / cancel     ? = show key help
"
    );
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn flag_val(args: &[String], flag: &str) -> Option<String> {
    for i in 0..args.len() {
        if args[i] == flag {
            if let Some(v) = args.get(i + 1) {
                if !v.starts_with("--") {
                    return Some(v.clone());
                }
            }
        }
    }
    None
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

const CALL_VALUE_FLAGS: &[&str] = &[
    "--secs",
    "--zpw",
    "--imei",
    "--input",
    "--output",
    "--data-dir",
    "--profile",
];

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
    Ok((paths, sess))
}

fn parse_device_selection(args: &[String]) -> AudioDeviceSelection {
    AudioDeviceSelection {
        input_device_name: flag_val(args, "--input"),
        output_device_name: flag_val(args, "--output"),
    }
}

fn parse_signaling_client(
    args: &[String],
    sess: &Session,
    trace: Option<TraceSink>,
) -> SignalingClient {
    let mut client = SignalingClient::new();
    if let Some(raw) = flag_val(args, "--zpw") {
        if let Some((ver, ty)) = raw.split_once(':') {
            let ver = ver.trim();
            let ty = ty.trim();
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
    Status,
    Hangup,
    Help,
}

fn parse_interactive_command(line: &str) -> Option<InteractiveCommand> {
    match line.trim().to_ascii_lowercase().as_str() {
        "a" | "accept" | "answer" => Some(InteractiveCommand::Accept),
        "d" | "decline" | "reject" => Some(InteractiveCommand::Decline),
        "m" | "mute" => Some(InteractiveCommand::ToggleMute),
        "s" | "status" => Some(InteractiveCommand::Status),
        "h" | "hangup" | "q" | "quit" => Some(InteractiveCommand::Hangup),
        "?" | "help" => Some(InteractiveCommand::Help),
        _ => None,
    }
}

/// Spawn a non-blocking stdin line reader thread for interactive controls.
///
/// When stdin is closed or piped to EOF, the channel closes cleanly (returning `None`) so
/// automated/non-interactive runs never block or abort prematurely.
fn spawn_stdin_commands() -> tokio::sync::mpsc::UnboundedReceiver<InteractiveCommand> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = std::thread::Builder::new()
        .name("call-lab-stdin".to_string())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut locked = stdin.lock();
            feed_stdin_commands(&mut locked, &tx);
        });
    rx
}

fn feed_stdin_commands<R: BufRead>(
    reader: &mut R,
    tx: &tokio::sync::mpsc::UnboundedSender<InteractiveCommand>,
) {
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF (stdin closed or piped empty)
            Ok(_) => {
                if let Some(cmd) = parse_interactive_command(&line) {
                    if tx.send(cmd).is_err() {
                        break;
                    }
                    if cmd == InteractiveCommand::Hangup {
                        break;
                    }
                } else if !line.trim().is_empty() {
                    println!(
                        "[KEYS] Unknown command `{}`. Keys: a=accept d=decline m=mute s=status h/q=hangup ?=help",
                        line.trim()
                    );
                }
            }
            Err(_) => break,
        }
    }
}

async fn print_engine_status(engine: &CallEngine) {
    let snap = engine.snapshot().await;
    let telem = engine.audio_telemetry().await;
    let has_zrtp = engine.has_active_zrtp().await;
    let has_media = engine.has_active_media().await;
    println!(
        "[STATUS] phase={:?} zrtp={} media={}",
        engine.phase().await,
        has_zrtp,
        has_media
    );
    if let Some(s) = snap {
        println!(
            "[STATUS] snapshot={}",
            serde_json::to_string(&s).unwrap_or_default()
        );
    }
    if let Some(t) = telem {
        let mode = if t.has_input_device { "duplex" } else { "rx-only" };
        println!(
            "[STATUS] audio({mode}) muted={} mic={}‰ remote={}‰ speaking={} tx={} rx={} fec={} plc={} tx_err={}",
            t.mic_muted,
            t.mic_level_permille,
            t.remote_level_permille,
            t.remote_speaking,
            t.tx_packets,
            t.rx_packets,
            t.fec_frames,
            t.plc_frames,
            t.tx_errors,
        );
    }
}

async fn handle_interactive_command(cmd: InteractiveCommand, engine: &CallEngine) -> bool {
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
            let cur_muted = engine
                .snapshot()
                .await
                .map(|s| s.mic_muted)
                .unwrap_or(false);
            let next = !cur_muted;
            match engine.set_mic_muted(next).await {
                Ok(()) => println!("[CMD] mic_muted -> {next}"),
                Err(e) => eprintln!("[CMD] set_mic_muted failed: {e}"),
            }
            false
        }
        InteractiveCommand::Status => {
            print_engine_status(engine).await;
            false
        }
        InteractiveCommand::Hangup => {
            println!("[CMD] Hangup requested; ending call...");
            true
        }
        InteractiveCommand::Help => {
            println!(
                "[KEYS] a=accept | d=decline | m=toggle mute (12006) | s=status | h/q=hangup | ?=help"
            );
            false
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(String::as_str) else {
        print_usage();
        return Ok(());
    };

    match cmd {
        "help" | "-h" | "--help" => {
            print_usage();
            Ok(())
        }
        "info" => cmd_info(&args[1..]),
        "devices" => cmd_devices(),
        "audio-loopback" => cmd_audio_loopback(&args[1..]).await,
        "echo" => cmd_echo(&args[1..]).await,
        "zrtp-probe" => cmd_zrtp_probe(&args[1..]).await,
        "watch" => cmd_watch(&args[1..]).await,
        "call" => cmd_call(&args[1..]).await,
        "serve" => cmd_serve(&args[1..]).await,
        other => {
            print_usage();
            bail!("Lệnh không hợp lệ: `{other}`");
        }
    }
}

fn cmd_info(args: &[String]) -> Result<()> {
    let (paths, sess) = load_session(args)?;
    let raw_imei_len = sess.imei.trim().len();
    let voicecall_imei_len = normalize_call_imei(&sess.imei).len();
    println!("=== TEZA CALL_LAB SESSION INFO ===");
    println!("data_dir:       {}", paths.root.display());
    println!("uid:            {}", sess.profile.uid);
    println!("uid_u32:        {}", session_uid_u32(&sess));
    println!("display_name:   {}", sess.profile.display_name);
    println!("authenticated:  {}", sess.is_authenticated());
    println!("relay_enabled:  {}", sess.relay.enabled);
    println!("zpw:            {ZPW_VER}:{ZPW_TYPE}");
    println!("imei_len:       raw={raw_imei_len} -> voicecall={voicecall_imei_len}");
    println!("voice_call:     {}", resolve_voice_call_host(&sess));
    println!("lp_control:     {}", sess.domains.lp_control);
    println!("ws_endpoints:   {}", sess.domains.ws.len());
    match ensure_callable_session(&sess) {
        Ok(()) => println!("call_ready:     YES (direct network)"),
        Err(e) => println!("call_ready:     NO ({e})"),
    }
    Ok(())
}

fn cmd_devices() -> Result<()> {
    println!("=== INPUT DEVICES (MICROPHONES) ===");
    match list_input_devices() {
        Ok(list) if !list.is_empty() => {
            for (i, d) in list.iter().enumerate() {
                let mark = if d.is_default { " [DEFAULT]" } else { "" };
                println!("  [{i}] {}{mark}", d.name);
            }
        }
        Ok(_) => println!("  (none detected)"),
        Err(e) => println!("  error: {e}"),
    }

    println!("=== OUTPUT DEVICES (SPEAKERS) ===");
    match list_output_devices() {
        Ok(list) if !list.is_empty() => {
            for (i, d) in list.iter().enumerate() {
                let mark = if d.is_default { " [DEFAULT]" } else { "" };
                println!("  [{i}] {}{mark}", d.name);
            }
        }
        Ok(_) => println!("  (none detected)"),
        Err(e) => println!("  error: {e}"),
    }
    Ok(())
}

async fn cmd_audio_loopback(args: &[String]) -> Result<()> {
    let secs: u64 = flag_val(args, "--secs")
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let loss_pct: u8 = flag_val(args, "--loss-pct")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let tone_hz: Option<f32> = flag_val(args, "--tone")
        .and_then(|v| v.parse::<f32>().ok())
        .filter(|hz| hz.is_finite() && *hz > 20.0 && *hz < 7_500.0)
        .or_else(|| has_flag(args, "--tone").then_some(440.0));
    let devices = parse_device_selection(args);

    println!(
        "=== AUDIO LOOPBACK (16kHz mono 20ms Opus, secs={secs}, loss_pct={loss_pct}%, tone={tone_hz:?}) ==="
    );
    let session = AudioLoopbackSession::start_with_tone(devices, loss_pct, tone_hz)?;
    let started = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    while started.elapsed() < Duration::from_secs(secs) {
        tick.tick().await;
        let t = session.telemetry();
        println!(
            "[{:>5.1}s] mic={:>4}‰ remote={:>4}‰ tx={} rx={} fec={} plc={}",
            started.elapsed().as_secs_f64(),
            t.mic_level_permille,
            t.remote_level_permille,
            t.tx_packets,
            t.rx_packets,
            t.fec_frames,
            t.plc_frames,
        );
    }
    let final_t = session.telemetry();
    session.stop();
    println!(
        "Loopback stopped: tx={} rx={} fec={} plc={}",
        final_t.tx_packets, final_t.rx_packets, final_t.fec_frames, final_t.plc_frames
    );
    Ok(())
}

async fn cmd_echo(args: &[String]) -> Result<()> {
    let raw_addr = first_positional(args, &["--count", "--timeout-ms"])
        .context("Thiếu địa chỉ <IP:PORT> (vd: call_lab echo 42.113.206.4:10490)")?;
    let target: SocketAddr = raw_addr
        .parse()
        .with_context(|| format!("Địa chỉ UDP không hợp lệ: {raw_addr}"))?;
    let count: usize = flag_val(args, "--count")
        .and_then(|v| v.parse().ok())
        .unwrap_or(6);
    let timeout_ms: u64 = flag_val(args, "--timeout-ms")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1500);
    let trace = make_trace_sink(has_flag(args, "--trace"));
    if let Some(ref sink) = trace {
        sink.emit(&format!(
            "[ECHO TX] target={target} count={count} timeout_ms={timeout_ms}"
        ));
    }

    let summary = probe_udp_echo(target, count, Duration::from_millis(timeout_ms)).await?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

async fn cmd_zrtp_probe(args: &[String]) -> Result<()> {
    let raw_addr = first_positional(
        args,
        &[
            "--from-uid",
            "--partner-uid",
            "--call-id",
            "--sess",
            "--rtcp-addr",
            "--hold-ms",
        ],
    )
    .context("Thiếu địa chỉ relay <IP:PORT>")?;
    let from_uid: u32 = flag_val(args, "--from-uid")
        .and_then(|v| v.parse().ok())
        .context("Thiếu --from-uid <U32>")?;
    let partner_uid: u32 = flag_val(args, "--partner-uid")
        .and_then(|v| v.parse().ok())
        .context("Thiếu --partner-uid <U32>")?;
    let call_id: u32 = flag_val(args, "--call-id")
        .and_then(|v| v.parse().ok())
        .context("Thiếu --call-id <U32>")?;
    let sess_id = flag_val(args, "--sess").context("Thiếu --sess <STR>")?;
    let is_caller = !has_flag(args, "--callee");
    let rtcp_addr = flag_val(args, "--rtcp-addr").unwrap_or_else(|| raw_addr.clone());
    let hold_ms: u64 = flag_val(args, "--hold-ms")
        .and_then(|v| v.parse().ok())
        .unwrap_or(600);
    let trace = make_trace_sink(has_flag(args, "--trace"));

    println!(
        "=== ZRTP PROBE target={raw_addr} rtcp={rtcp_addr} caller={is_caller} call_id={call_id} ==="
    );
    let session = ZrtpRelaySession::start_with_config(
        vec![RelayServer::new(raw_addr.clone(), rtcp_addr)],
        from_uid,
        partner_uid,
        call_id,
        is_caller,
        &sess_id,
        ZrtpProbeConfig {
            trace,
            ..Default::default()
        },
    )
    .await?;

    println!("{}", serde_json::to_string_pretty(session.probe())?);
    if hold_ms > 0 {
        tokio::time::sleep(Duration::from_millis(hold_ms)).await;
    }
    let rtcp_inbox = session.drain_rtcp_inbox();
    println!("rtcp_inbox_packets: {}", rtcp_inbox.len());
    session.shutdown().await;
    println!("ZRTP session shut down (End sent).");
    Ok(())
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
                    // Normal idle long-poll timeout: re-arm cleanly without error backoff.
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

async fn cmd_watch(args: &[String]) -> Result<()> {
    let (_paths, sess) = load_session(args)?;
    ensure_callable_session(&sess)?;
    let secs: u64 = flag_val(args, "--secs")
        .and_then(|v| v.parse().ok())
        .unwrap_or(120);
    let use_poll = has_flag(args, "--poll");
    let trace_enabled = has_flag(args, "--trace");
    let trace = make_trace_sink(trace_enabled);

    println!("=== VOIP WATCH (secs={secs}, http_poll={use_poll}, trace={trace_enabled}) ===");
    let ws_sess = sess.clone_shallow();
    let mut ws_rx = socket::spawn(ws_sess);
    let (mut poll_rx, poll_task) = if use_poll {
        let (rx, handle) = spawn_poll_task(sess.clone_shallow(), CtrlPollState::default(), trace.clone());
        (Some(rx), Some(handle))
    } else {
        (None, None)
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    let my_uid = sess.profile.uid.clone();
    let send2me = sess.profile.send2me_id.clone();
    let mut expired_err: Option<anyhow::Error> = None;

    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            _ = tokio::signal::ctrl_c() => {
                println!("Interrupted by Ctrl+C");
                break;
            }
            poll_ev = async {
                match poll_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match poll_ev {
                    Some(ev) => {
                        println!(
                            "[POLL] act={} call_id={} status={:?}\n{}",
                            ev.act,
                            ev.call_id(),
                            ev.status(),
                            format_trace_json(&ev.data, 1200)
                        );
                    }
                    None => {
                        poll_rx = None;
                    }
                }
            }
            sig = ws_rx.recv() => {
                match sig {
                    Some(WsSignal::Status(st)) => println!("[WS] status={st:?}"),
                    Some(WsSignal::Expired { code, message }) => {
                        expired_err = Some(anyhow::anyhow!(
                            "WebSocket session expired ({code}): {message}"
                        ));
                        break;
                    }
                    Some(WsSignal::Event(ev)) => {
                        let parsed = parse_ws_event(&ev, &my_uid, &send2me);
                        let mut saw_voip = false;
                        for act in parsed {
                            if let WsParsedAction::Voip(v) = act {
                                saw_voip = true;
                                println!(
                                    "[WS cmd={}] act={} call_id={} status={:?}\n{}",
                                    ev.cmd,
                                    v.act,
                                    v.call_id(),
                                    v.status(),
                                    format_trace_json(&v.data, 1200)
                                );
                            }
                        }
                        if !saw_voip {
                            if let Some(ref sink) = trace {
                                sink.emit(&format!(
                                    "[WS FRAME] cmd={} sub_cmd={} ver={}",
                                    ev.cmd, ev.sub_cmd, ev.version
                                ));
                            }
                        }
                    }
                    None => break,
                }
            }
        }
    }
    if let Some(h) = poll_task {
        h.abort();
    }
    if let Some(err) = expired_err {
        return Err(err);
    }
    Ok(())
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

async fn cmd_call(args: &[String]) -> Result<()> {
    let uid = first_positional(args, CALL_VALUE_FLAGS)
        .context("Thiếu <UID> người nhận (vd: call_lab call 1000000000000000002)")?;
    let secs: u64 = flag_val(args, "--secs")
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    let enable_audio = !has_flag(args, "--no-audio");
    let use_poll = has_flag(args, "--poll");
    let trace_enabled = has_flag(args, "--trace");
    let trace = make_trace_sink(trace_enabled);
    let call_type = CallType::from_video_flag(has_flag(args, "--video"));
    let devices = parse_device_selection(args);

    let (_paths, sess) = load_session(args)?;
    let sig_client = parse_signaling_client(args, &sess, trace.clone());
    let ws_sess = sess.clone_shallow();
    let poll_sess = sess.clone_shallow();
    let my_uid = sess.profile.uid.clone();
    let send2me = sess.profile.send2me_id.clone();

    let engine = Arc::new(CallEngine::with_signaling_client(
        sess,
        sig_client,
        CallEngineConfig {
            enable_audio,
            devices,
            trace: trace.clone(),
        },
    )?);
    let mut ev_rx = engine.subscribe();
    let mut ws_rx = socket::spawn(ws_sess);
    wait_for_ws_connected(&mut ws_rx, Duration::from_secs(5)).await?;

    let (mut poll_rx, poll_task) = if use_poll {
        // Sync initial `lp_control` cursor backlog (bounded to 3s) before dialing so stale
        // multi-page history does not flood the call log while live `ring_ring`/`answer` come cleanly.
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
                Ok(Ok(_)) => {
                    if !init_state.is_backlog_synced() {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
                Ok(Err(e)) => {
                    eprintln!("[POLL] initial cursor prime warning: {e}");
                    break;
                }
                Err(_) => {
                    eprintln!("[POLL] initial cursor prime timed out after 3s; continuing to dial");
                    break;
                }
            }
        }
        let (rx, handle) = spawn_poll_task(poll_sess, init_state, trace.clone());
        (Some(rx), Some(handle))
    } else {
        (None, None)
    };

    println!(
        "=== CALL_LAB 1-1 CALL -> {uid} (type={call_type:?}, audio={enable_audio}, http_poll={use_poll}, trace={trace_enabled}, secs={secs}) ==="
    );
    let call_id = match engine
        .make_call(uid.clone(), uid.clone(), call_type)
        .await
    {
        Ok(id) => id,
        Err(e) => {
            if let Some(h) = poll_task {
                h.abort();
            }
            return Err(e.into());
        }
    };
    println!(
        "Dialed call_id={call_id}. Keys: m=mute, s=status, h/q=hangup, ?=help (or Ctrl+C)."
    );

    let mut stdin_rx = Some(spawn_stdin_commands());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    let mut stats_tick = tokio::time::interval(Duration::from_secs(2));
    stats_tick.tick().await;
    let mut ended_by_event = false;
    let mut expired_err: Option<anyhow::Error> = None;

    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                println!("Timeout reached; hanging up...");
                break;
            }
            _ = tokio::signal::ctrl_c() => {
                println!("Ctrl+C received; hanging up...");
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
                        if handle_interactive_command(cmd, &engine).await {
                            break;
                        }
                    }
                    None => {
                        stdin_rx = None;
                    }
                }
            }
            _ = stats_tick.tick() => {
                if let Some(t) = engine.audio_telemetry().await {
                    let mode = if t.has_input_device { "duplex" } else { "rx-only" };
                    println!(
                        "[AUDIO {mode}] mic={}‰ remote={}‰ tx={} rx={} fec={} plc={}",
                        t.mic_level_permille,
                        t.remote_level_permille,
                        t.tx_packets,
                        t.rx_packets,
                        t.fec_frames,
                        t.plc_frames,
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
                        if trace_enabled {
                            println!(
                                "[POLL] act={} call_id={} status={:?} data={}",
                                v.act,
                                v.call_id(),
                                v.status(),
                                format_trace_json(&v.data, 800)
                            );
                        } else {
                            println!(
                                "[POLL] act={} call_id={} status={:?}",
                                v.act,
                                v.call_id(),
                                v.status()
                            );
                        }
                        if let Err(e) = engine.handle_voip_event(&v).await {
                            eprintln!("[POLL] handle_voip_event error: {e}");
                        }
                    }
                    None => {
                        poll_rx = None;
                    }
                }
            }
            res = ev_rx.recv() => {
                match res {
                    Ok(ev) => {
                        println!("[EVENT] {}", serde_json::to_string(&ev)?);
                        if matches!(ev, CallEvent::CallEnded { .. }) {
                            ended_by_event = true;
                            break;
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
                        let _ = engine.end_call().await;
                        expired_err = Some(anyhow::anyhow!(
                            "WebSocket session expired ({code}): {message}"
                        ));
                        break;
                    }
                    Some(WsSignal::Event(ws_ev)) => {
                        for parsed in parse_ws_event(&ws_ev, &my_uid, &send2me) {
                            if let WsParsedAction::Voip(v) = parsed {
                                if trace_enabled {
                                    println!(
                                        "[WS cmd={}] act={} call_id={} status={:?} data={}",
                                        ws_ev.cmd,
                                        v.act,
                                        v.call_id(),
                                        v.status(),
                                        format_trace_json(&v.data, 800)
                                    );
                                } else {
                                    println!(
                                        "[WS cmd={}] act={} call_id={} status={:?}",
                                        ws_ev.cmd,
                                        v.act,
                                        v.call_id(),
                                        v.status()
                                    );
                                }
                                if let Err(e) = engine.handle_voip_event(&v).await {
                                    eprintln!("[WS] handle_voip_event error: {e}");
                                }
                            }
                        }
                    }
                    None => {
                        eprintln!("[WS] connection stream closed");
                        break;
                    }
                }
            }
        }
    }

    if let Some(h) = poll_task {
        h.abort();
    }
    if let Some(err) = expired_err {
        return Err(err);
    }
    if ended_by_event {
        return Ok(());
    }

    engine.end_call().await?;
    println!("Call ended cleanly.");
    Ok(())
}

async fn cmd_serve(args: &[String]) -> Result<()> {
    let secs: u64 = flag_val(args, "--secs")
        .and_then(|v| v.parse().ok())
        .unwrap_or(180);
    let auto_answer = parse_optional_delay_flag(args, "--auto-answer", 1.5);
    let reject_delay = parse_optional_delay_flag(args, "--reject", 0.0);
    let hangup_after: Option<f64> = flag_val(args, "--hangup-after")
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0);
    let once = has_flag(args, "--once");
    let enable_audio = !has_flag(args, "--no-audio");
    let use_poll = has_flag(args, "--poll");
    let trace_enabled = has_flag(args, "--trace");
    let trace = make_trace_sink(trace_enabled);
    let devices = parse_device_selection(args);

    let (_paths, sess) = load_session(args)?;
    let sig_client = parse_signaling_client(args, &sess, trace.clone());
    let ws_sess = sess.clone_shallow();
    let poll_sess = sess.clone_shallow();
    let my_uid = sess.profile.uid.clone();
    let send2me = sess.profile.send2me_id.clone();

    let engine = Arc::new(CallEngine::with_signaling_client(
        sess,
        sig_client,
        CallEngineConfig {
            enable_audio,
            devices,
            trace: trace.clone(),
        },
    )?);
    let mut ev_rx = engine.subscribe();
    let mut ws_rx = socket::spawn(ws_sess);
    let (mut poll_rx, poll_task) = if use_poll {
        let (rx, handle) = spawn_poll_task(poll_sess, CtrlPollState::new_live_only(), trace.clone());
        (Some(rx), Some(handle))
    } else {
        (None, None)
    };

    println!(
        "=== CALL_LAB SERVE (secs={secs}, auto_answer={auto_answer:?}, reject_delay={reject_delay:?}, hangup_after={hangup_after:?}, once={once}, audio={enable_audio}, http_poll={use_poll}, trace={trace_enabled}) ==="
    );
    println!("Interactive keys: a=accept, d=decline, m=mute, s=status, h/q=hangup, ?=help (or Ctrl+C).");

    let mut stdin_rx = Some(spawn_stdin_commands());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    let mut stats_tick = tokio::time::interval(Duration::from_secs(2));
    stats_tick.tick().await;
    let mut expired_err: Option<anyhow::Error> = None;

    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            _ = tokio::signal::ctrl_c() => {
                println!("Ctrl+C received; cleaning up...");
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
                        if handle_interactive_command(cmd, &engine).await {
                            break;
                        }
                    }
                    None => {
                        stdin_rx = None;
                    }
                }
            }
            _ = stats_tick.tick() => {
                if let Some(t) = engine.audio_telemetry().await {
                    let mode = if t.has_input_device { "duplex" } else { "rx-only" };
                    println!(
                        "[AUDIO {mode}] mic={}‰ remote={}‰ tx={} rx={} fec={} plc={}",
                        t.mic_level_permille,
                        t.remote_level_permille,
                        t.tx_packets,
                        t.rx_packets,
                        t.fec_frames,
                        t.plc_frames,
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
                        if trace_enabled {
                            println!(
                                "[POLL] act={} call_id={} status={:?} data={}",
                                v.act,
                                v.call_id(),
                                v.status(),
                                format_trace_json(&v.data, 800)
                            );
                        } else {
                            println!(
                                "[POLL] act={} call_id={} status={:?}",
                                v.act,
                                v.call_id(),
                                v.status()
                            );
                        }
                        if let Err(e) = engine.handle_voip_event(&v).await {
                            eprintln!("[POLL] handle_voip_event error: {e}");
                        }
                    }
                    None => {
                        poll_rx = None;
                    }
                }
            }
            res = ev_rx.recv() => {
                match res {
                    Ok(ev) => {
                        println!("[EVENT] {}", serde_json::to_string(&ev)?);
                        match ev {
                            CallEvent::IncomingCall { call_id, .. } => {
                                if let Some(delay) = reject_delay {
                                    let eng = engine.clone();
                                    tokio::spawn(async move {
                                        if delay > 0.0 {
                                            tokio::time::sleep(Duration::from_secs_f64(delay.clamp(0.0, 60.0))).await;
                                        }
                                        println!("Auto-declining call_id={call_id}...");
                                        if let Err(e) = eng.answer_call_by_id(Some(call_id), false).await {
                                            eprintln!("Auto-decline failed: {e}");
                                        }
                                    });
                                } else if let Some(delay) = auto_answer {
                                    let eng = engine.clone();
                                    tokio::spawn(async move {
                                        tokio::time::sleep(Duration::from_secs_f64(delay.clamp(0.0, 60.0))).await;
                                        println!("Auto-answering call_id={call_id}...");
                                        if let Err(e) = eng.answer_call_by_id(Some(call_id), true).await {
                                            eprintln!("Auto-answer failed: {e}");
                                        }
                                    });
                                } else {
                                    println!(
                                        "[INCOMING] call_id={call_id} — type `a` + Enter to accept or `d` + Enter to decline."
                                    );
                                }
                            }
                            CallEvent::Connected { call_id, .. } => {
                                if let Some(delay) = hangup_after {
                                    let eng = engine.clone();
                                    tokio::spawn(async move {
                                        if delay > 0.0 {
                                            tokio::time::sleep(Duration::from_secs_f64(delay.clamp(0.0, 3600.0))).await;
                                        }
                                        if eng.snapshot().await.is_some_and(|s| s.call_id == call_id) {
                                            println!("Auto-hanging up call_id={call_id} after {delay}s...");
                                            if let Err(e) = eng.end_call().await {
                                                eprintln!("Auto-hangup failed: {e}");
                                            }
                                        }
                                    });
                                }
                            }
                            CallEvent::CallEnded { .. } if once => {
                                break;
                            }
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
                    Some(WsSignal::Status(WsStatus::Connected)) => {
                        println!("[WS] Connected — ready for incoming calls.");
                    }
                    Some(WsSignal::Status(st)) => println!("[WS] status={st:?}"),
                    Some(WsSignal::Expired { code, message }) => {
                        expired_err = Some(anyhow::anyhow!(
                            "WebSocket session expired ({code}): {message}"
                        ));
                        break;
                    }
                    Some(WsSignal::Event(ws_ev)) => {
                        for parsed in parse_ws_event(&ws_ev, &my_uid, &send2me) {
                            if let WsParsedAction::Voip(v) = parsed {
                                if trace_enabled {
                                    println!(
                                        "[WS cmd={}] act={} call_id={} status={:?} data={}",
                                        ws_ev.cmd,
                                        v.act,
                                        v.call_id(),
                                        v.status(),
                                        format_trace_json(&v.data, 800)
                                    );
                                } else {
                                    println!(
                                        "[WS cmd={}] act={} call_id={} status={:?}",
                                        ws_ev.cmd,
                                        v.act,
                                        v.call_id(),
                                        v.status()
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

    if let Some(h) = poll_task {
        h.abort();
    }
    let _ = engine.end_call().await;
    if let Some(err) = expired_err {
        return Err(err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn s(arr: &[&str]) -> Vec<String> {
        arr.iter().map(|v| (*v).to_string()).collect()
    }

    #[test]
    fn cli_arg_helpers_handle_poll_trace_imei_and_positional_ordering() {
        // Default: --poll / --trace off
        let a1 = s(&["1000000000000000002", "--secs", "30"]);
        assert!(!has_flag(&a1, "--poll"));
        assert!(!has_flag(&a1, "--trace"));
        assert_eq!(
            first_positional(&a1, CALL_VALUE_FLAGS).as_deref(),
            Some("1000000000000000002")
        );

        // --poll and --trace after UID
        let a2 = s(&["1000000000000000002", "--poll", "--trace", "--no-audio"]);
        assert!(has_flag(&a2, "--poll"));
        assert!(has_flag(&a2, "--trace"));
        assert_eq!(
            first_positional(&a2, CALL_VALUE_FLAGS).as_deref(),
            Some("1000000000000000002")
        );

        // --imei and value-taking flags BEFORE UID
        let a3 = s(&[
            "--secs",
            "45",
            "--poll",
            "--imei",
            "custom-imei-val",
            "--input",
            "Mic 1",
            "1000000000000000009",
        ]);
        assert!(has_flag(&a3, "--poll"));
        assert_eq!(flag_val(&a3, "--secs").as_deref(), Some("45"));
        assert_eq!(flag_val(&a3, "--imei").as_deref(), Some("custom-imei-val"));
        assert_eq!(flag_val(&a3, "--input").as_deref(), Some("Mic 1"));
        assert_eq!(
            first_positional(&a3, CALL_VALUE_FLAGS).as_deref(),
            Some("1000000000000000009")
        );

        // serve --auto-answer --poll (boolean-style --auto-answer followed by --poll)
        let a4 = s(&["--auto-answer", "--poll", "--raw-imei", "--reject", "--once"]);
        assert!(has_flag(&a4, "--poll"));
        assert!(has_flag(&a4, "--raw-imei"));
        assert!(has_flag(&a4, "--once"));
        assert_eq!(flag_val(&a4, "--auto-answer"), None);
        assert!(has_flag(&a4, "--auto-answer"));
        assert_eq!(parse_optional_delay_flag(&a4, "--auto-answer", 1.5), Some(1.5));
        assert_eq!(parse_optional_delay_flag(&a4, "--reject", 0.0), Some(0.0));

        // serve --reject 2 --hangup-after 2 --auto-answer 1
        let a5 = s(&["--reject", "2", "--hangup-after", "2", "--auto-answer", "1"]);
        assert_eq!(parse_optional_delay_flag(&a5, "--reject", 0.0), Some(2.0));
        assert_eq!(parse_optional_delay_flag(&a5, "--auto-answer", 1.5), Some(1.0));
        assert_eq!(
            flag_val(&a5, "--hangup-after")
                .and_then(|v| v.parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v >= 0.0),
            Some(2.0)
        );

        // parse_signaling_client respects --raw-imei and --imei overrides
        let sess = Session::new().unwrap();
        let c_raw = parse_signaling_client(&s(&["--raw-imei"]), &sess, None);
        assert_eq!(c_raw.effective_imei(&sess), sess.imei);
        let c_custom = parse_signaling_client(&s(&["--imei", "explicit-imei"]), &sess, None);
        assert_eq!(c_custom.effective_imei(&sess), "explicit-imei");
    }

    #[test]
    fn interactive_command_parser_and_eof_reader_are_non_blocking() {
        assert_eq!(parse_interactive_command("a\n"), Some(InteractiveCommand::Accept));
        assert_eq!(parse_interactive_command(" ACCEPT "), Some(InteractiveCommand::Accept));
        assert_eq!(parse_interactive_command("d"), Some(InteractiveCommand::Decline));
        assert_eq!(parse_interactive_command("M\r\n"), Some(InteractiveCommand::ToggleMute));
        assert_eq!(parse_interactive_command("s"), Some(InteractiveCommand::Status));
        assert_eq!(parse_interactive_command("h"), Some(InteractiveCommand::Hangup));
        assert_eq!(parse_interactive_command("q"), Some(InteractiveCommand::Hangup));
        assert_eq!(parse_interactive_command("?"), Some(InteractiveCommand::Help));
        assert_eq!(parse_interactive_command(""), None);
        assert_eq!(parse_interactive_command("unknown"), None);

        // Empty EOF reader exits cleanly without sending anything.
        let (tx_empty, mut rx_empty) = tokio::sync::mpsc::unbounded_channel();
        let mut empty_cursor = Cursor::new(b"".as_slice());
        feed_stdin_commands(&mut empty_cursor, &tx_empty);
        drop(tx_empty);
        assert!(rx_empty.try_recv().is_err());

        // Piped commands emit in order and stop cleanly on Hangup.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut cursor = Cursor::new(b"s\nm\nq\na\n".as_slice());
        feed_stdin_commands(&mut cursor, &tx);
        drop(tx);
        assert_eq!(rx.try_recv().ok(), Some(InteractiveCommand::Status));
        assert_eq!(rx.try_recv().ok(), Some(InteractiveCommand::ToggleMute));
        assert_eq!(rx.try_recv().ok(), Some(InteractiveCommand::Hangup));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn spawn_poll_task_exits_cleanly_on_unauthenticated_session_without_hanging() {
        let sess = Session::new().unwrap();
        let (mut rx, handle) = spawn_poll_task(sess, CtrlPollState::default(), None);
        let recv = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("poller should exit cleanly on NotAuthenticated");
        assert!(recv.is_none());
        handle.await.unwrap();
    }
}
