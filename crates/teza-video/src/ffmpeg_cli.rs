//! Optional CLI webcam capture (`ffmpeg -f dshow/v4l2/avfoundation`) and Annex-B file recording.
//!
//! Used by `video_lab` for headless hardware testing; desktop GUI uses zero-dependency
//! WebView2 `WebCodecs` + `getUserMedia` in Phase 2 without invoking external processes.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::framing::{find_aud_delimiter, strip_leading_aud, H264_AUD};

const LIST_CAMERAS_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_LIST_CAMERAS_STDERR_BYTES: u64 = 256 * 1024;

/// Drop guard that kills and reaps a spawned child stored in an `Arc<Mutex<Option<Child>>>`
/// unless explicitly disarmed after full ownership transfer.
struct ChildSlotKillGuard {
    slot: Arc<Mutex<Option<Child>>>,
    armed: bool,
}

impl ChildSlotKillGuard {
    fn new(slot: Arc<Mutex<Option<Child>>>) -> Self {
        Self { slot, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ChildSlotKillGuard {
    fn drop(&mut self) {
        if self.armed {
            if let Ok(mut g) = self.slot.lock() {
                if let Some(mut c) = g.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
            }
        }
    }
}

/// Spawn `cmd`, drain stderr concurrently up to `MAX_LIST_CAMERAS_STDERR_BYTES`, and wait up to
/// `timeout`. If the child does not exit in time, kill & wait it and return `None`.
fn run_command_stderr_with_timeout(cmd: &mut Command, timeout: Duration) -> Option<Vec<u8>> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().ok()?;
    let stderr_pipe = child.stderr.take();
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(1);
    let reader_thread = stderr_pipe.map(|pipe| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.take(MAX_LIST_CAMERAS_STDERR_BYTES).read_to_end(&mut buf);
            let _ = tx.send(buf);
        })
    });

    let deadline = Instant::now() + timeout;
    let exited = loop {
        match child.try_wait() {
            Ok(Some(_)) => break true,
            Ok(None) => {
                if Instant::now() >= deadline {
                    break false;
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break false,
        }
    };

    if !exited {
        let _ = child.kill();
        let _ = child.wait();
        if let Some(h) = reader_thread {
            let _ = rx.recv_timeout(Duration::from_millis(200));
            drop(h);
        }
        return None;
    }

    if let Some(h) = reader_thread {
        let _ = h.join();
    }
    rx.try_recv().ok()
}

fn run_command_success_with_timeout(cmd: &mut Command, timeout: Duration) -> bool {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// Filter out IR / Windows Hello / virtual depth cameras that produce unusable video streams.
pub fn is_ignored_camera_device(name: &str) -> bool {
    let low = name.trim().to_ascii_lowercase();
    low.contains("ir camera")
        || low.contains("infrared")
        || low.contains("windows hello")
        || low.contains("depth camera")
}

/// Return `true` when `ffmpeg` is installed and reachable in `PATH` (cached after first probe).
pub fn is_ffmpeg_available() -> bool {
    static FFMPEG_OK: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        let mut cmd = Command::new("ffmpeg");
        cmd.arg("-version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        run_command_success_with_timeout(&mut cmd, LIST_CAMERAS_TIMEOUT)
    });
    *FFMPEG_OK
}

/// Enumerate usable local camera device names via `ffmpeg` (never fails when `ffmpeg` is absent).
pub fn list_cameras() -> Vec<String> {
    if !is_ffmpeg_available() {
        return Vec::new();
    }
    #[cfg(target_os = "windows")]
    {
        let mut cmd = Command::new("ffmpeg");
        cmd.args([
            "-hide_banner",
            "-list_devices",
            "true",
            "-f",
            "dshow",
            "-i",
            "dummy",
        ]);
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let Some(stderr_bytes) = run_command_stderr_with_timeout(&mut cmd, LIST_CAMERAS_TIMEOUT)
        else {
            return Vec::new();
        };
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        let mut cams = Vec::new();
        for tag in ["(video)", "(none)"] {
            for line in stderr.lines() {
                if line.contains("Alternative name") || !line.contains(tag) {
                    continue;
                }
                if let Some((_, rest)) = line.split_once('"') {
                    if let Some((name, _)) = rest.split_once('"') {
                        let clean = name.trim().to_string();
                        if !clean.is_empty()
                            && !is_ignored_camera_device(&clean)
                            && !cams.contains(&clean)
                        {
                            cams.push(clean);
                        }
                    }
                }
            }
            if !cams.is_empty() {
                break;
            }
        }
        cams
    }
    #[cfg(target_os = "macos")]
    {
        let mut cmd = Command::new("ffmpeg");
        cmd.args([
            "-hide_banner",
            "-f",
            "avfoundation",
            "-list_devices",
            "true",
            "-i",
            "",
        ]);
        if let Some(stderr_bytes) = run_command_stderr_with_timeout(&mut cmd, LIST_CAMERAS_TIMEOUT) {
            let stderr = String::from_utf8_lossy(&stderr_bytes);
            let mut cams = Vec::new();
            let mut in_video = false;
            for line in stderr.lines() {
                if line.contains("AVFoundation video devices:") {
                    in_video = true;
                    continue;
                }
                if line.contains("AVFoundation audio devices:") {
                    break;
                }
                if in_video {
                    if let Some((_, right)) = line.rsplit_once(']') {
                        let clean = right.trim().to_string();
                        if !clean.is_empty()
                            && !is_ignored_camera_device(&clean)
                            && !cams.contains(&clean)
                        {
                            cams.push(clean);
                        }
                    }
                }
            }
            return cams;
        }
        Vec::new()
    }
    #[cfg(target_os = "linux")]
    {
        let mut cams = Vec::new();
        for idx in 0..8 {
            let dev = format!("/dev/video{idx}");
            if Path::new(&dev).exists() {
                cams.push(dev);
            }
        }
        cams
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        Vec::new()
    }
}

/// Live webcam H.264 Annex-B capture subprocess (`ffmpeg` Constrained Baseline 320x240 15fps).
pub struct FfmpegCameraCapture {
    stop: Arc<AtomicBool>,
    child_pid: Arc<Mutex<Option<Child>>>,
    worker: Option<JoinHandle<()>>,
}

impl FfmpegCameraCapture {
    pub fn start(
        device: Option<&str>,
        on_au: Arc<dyn Fn(Vec<u8>) + Send + Sync>,
    ) -> anyhow::Result<Self> {
        if !is_ffmpeg_available() {
            anyhow::bail!("Không tìm thấy `ffmpeg` trong PATH");
        }
        let dev_name = match device.map(str::trim).filter(|s| !s.is_empty()) {
            Some(d) => d.to_string(),
            None => list_cameras()
                .into_iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("Không phát hiện thiết bị Camera khả dụng"))?,
        };

        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-loglevel", "error"]);
        #[cfg(target_os = "windows")]
        cmd.args([
            "-f",
            "dshow",
            "-rtbufsize",
            "32M",
            "-i",
            &format!("video={dev_name}"),
        ]);
        #[cfg(target_os = "macos")]
        cmd.args([
            "-f",
            "avfoundation",
            "-framerate",
            "30",
            "-i",
            &format!("{dev_name}:none"),
        ]);
        #[cfg(target_os = "linux")]
        cmd.args(["-f", "v4l2", "-i", &dev_name]);

        cmd.args([
            "-vf",
            "fps=15,scale=320:240:force_original_aspect_ratio=decrease,pad=320:240:(ow-iw)/2:(oh-ih)/2,format=yuv420p",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            "-profile:v",
            "baseline",
            "-level",
            "3.0",
            "-b:v",
            "260k",
            "-maxrate",
            "320k",
            "-bufsize",
            "640k",
            "-x264-params",
            "keyint=10:min-keyint=1:bframes=0:ref=1:cabac=0:sliced-threads=0:scenecut=0:aud=1:repeat-headers=1",
            "-f",
            "h264",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }

        let child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("Không khởi chạy được camera `{dev_name}`: {e}"))?;
        Self::start_from_child_with_spawner(child, on_au, |f| {
            thread::Builder::new()
                .name("teza-video-cam".to_string())
                .spawn(f)
        })
    }

    fn start_from_child_with_spawner<F>(
        child: Child,
        on_au: Arc<dyn Fn(Vec<u8>) + Send + Sync>,
        spawn_worker: F,
    ) -> anyhow::Result<Self>
    where
        F: FnOnce(Box<dyn FnOnce() + Send + 'static>) -> std::io::Result<JoinHandle<()>>,
    {
        let child_slot = Arc::new(Mutex::new(Some(child)));
        let mut child_guard = ChildSlotKillGuard::new(child_slot.clone());

        let mut stdout = child_slot
            .lock()
            .ok()
            .and_then(|mut g| g.as_mut().and_then(|c| c.stdout.take()))
            .ok_or_else(|| anyhow::anyhow!("Không mở được stdout của ffmpeg camera"))?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop_w = stop.clone();
        let child_w = child_slot.clone();

        let worker = spawn_worker(Box::new(move || {
            let mut buf = [0u8; 8192];
            let mut acc = Vec::with_capacity(65_536);
            while !stop_w.load(Ordering::Relaxed) {
                match stdout.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        acc.extend_from_slice(&buf[..n]);
                        while let Some(pos) = find_aud_delimiter(&acc, 6) {
                            let raw_chunk: Vec<u8> = acc.drain(..pos).collect();
                            let clean = strip_leading_aud(&raw_chunk);
                            if !clean.is_empty() {
                                on_au(clean);
                            }
                        }
                        if acc.len() > 262_144 {
                            acc.clear();
                        }
                    }
                }
            }
            if let Ok(mut g) = child_w.lock() {
                if let Some(mut c) = g.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
            }
        }))?;

        child_guard.disarm();
        Ok(Self {
            stop,
            child_pid: child_slot,
            worker: Some(worker),
        })
    }

    pub fn stop(mut self) {
        self.shutdown_inner();
    }

    fn shutdown_inner(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(mut g) = self.child_pid.lock() {
            if let Some(mut c) = g.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

impl Drop for FfmpegCameraCapture {
    fn drop(&mut self) {
        self.shutdown_inner();
    }
}

/// Thread-safe raw H.264 Annex-B file writer (prefixes each AU with `H264_AUD` for `ffprobe`/`ffmpeg`).
pub struct H264FileRecorder {
    writer: Mutex<BufWriter<File>>,
}

impl H264FileRecorder {
    pub fn create(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let file = File::create(path.as_ref())?;
        Ok(Self {
            writer: Mutex::new(BufWriter::new(file)),
        })
    }

    pub fn write_au(&self, annexb_au: &[u8]) -> anyhow::Result<()> {
        if annexb_au.is_empty() {
            return Ok(());
        }
        let mut w = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("H264FileRecorder lock poisoned"))?;
        w.write_all(H264_AUD)?;
        w.write_all(annexb_au)?;
        w.flush()?;
        Ok(())
    }
}

/// Smoke-test filter helper for unit tests.
pub fn filter_camera_list(raw: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for name in raw {
        let clean = name.trim();
        if !clean.is_empty() && !is_ignored_camera_device(clean) && !out.iter().any(|x| x == clean)
        {
            out.push(clean.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_ir_and_windows_hello_cameras() {
        let filtered = filter_camera_list(&[
            "Integrated Camera".to_string(),
            "IR Camera (Windows Hello)".to_string(),
            "Intel Depth Camera".to_string(),
            "Logitech Brio".to_string(),
            "Logitech Brio".to_string(),
        ]);
        assert_eq!(filtered, vec!["Integrated Camera", "Logitech Brio"]);
    }

    #[test]
    fn h264_file_recorder_writes_aud_prefixed_annexb() {
        let tmp = std::env::temp_dir().join(format!(
            "teza_video_rec_test_{}_{}.h264",
            std::process::id(),
            std::time::Duration::from_millis(1).as_nanos()
        ));
        {
            let rec = H264FileRecorder::create(&tmp).unwrap();
            rec.write_au(&[0, 0, 0, 1, 0x67, 0x42]).unwrap();
        }
        let bytes = std::fs::read(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(
            bytes,
            vec![0, 0, 0, 1, 0x09, 0xF0, 0, 0, 0, 1, 0x67, 0x42]
        );
    }

    fn spawn_sleeping_dummy_child(pipe_stdout: bool) -> Child {
        #[cfg(target_os = "windows")]
        let mut cmd = {
            use std::os::windows::process::CommandExt;
            let mut c = Command::new("ping");
            c.args(["-n", "30", "127.0.0.1"]);
            c.creation_flags(0x0800_0000);
            c
        };
        #[cfg(not(target_os = "windows"))]
        let mut cmd = {
            let mut c = Command::new("sleep");
            c.arg("30");
            c
        };
        cmd.stdin(Stdio::null())
            .stdout(if pipe_stdout {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::null());
        cmd.spawn().expect("spawn dummy sleeping child")
    }

    #[test]
    fn start_from_child_kills_and_reaps_child_on_missing_stdout_or_spawn_error() {
        let on_au: Arc<dyn Fn(Vec<u8>) + Send + Sync> = Arc::new(|_| {});

        // 1. Missing stdout error kills & reaps child via ChildSlotKillGuard
        let child_no_stdout = spawn_sleeping_dummy_child(false);
        let res = FfmpegCameraCapture::start_from_child_with_spawner(
            child_no_stdout,
            on_au.clone(),
            |f| thread::Builder::new().spawn(f),
        );
        assert!(res.is_err());

        // 2. Worker thread spawn failure kills & reaps child via ChildSlotKillGuard
        let child_spawn_fail = spawn_sleeping_dummy_child(true);
        let started = Instant::now();
        let res2 = FfmpegCameraCapture::start_from_child_with_spawner(
            child_spawn_fail,
            on_au,
            |_f| Err(std::io::Error::other("injected worker spawn failure")),
        );
        assert!(res2.is_err());
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "ChildSlotKillGuard must kill & wait child immediately rather than letting it sleep 30s"
        );
    }

    #[test]
    fn run_command_stderr_with_timeout_captures_output_and_kills_hung_child() {
        // 1. Fast stderr output is captured cleanly
        #[cfg(target_os = "windows")]
        let mut fast = {
            use std::os::windows::process::CommandExt;
            let mut c = Command::new("cmd");
            c.args(["/C", "echo cam-probe-ok 1>&2"]);
            c.creation_flags(0x0800_0000);
            c
        };
        #[cfg(not(target_os = "windows"))]
        let mut fast = {
            let mut c = Command::new("sh");
            c.args(["-c", "echo cam-probe-ok 1>&2"]);
            c
        };
        let out = run_command_stderr_with_timeout(&mut fast, Duration::from_secs(3))
            .expect("fast command stderr");
        assert!(
            String::from_utf8_lossy(&out).contains("cam-probe-ok"),
            "unexpected stderr: {}",
            String::from_utf8_lossy(&out)
        );

        // 2. Hung command is killed promptly once timeout elapses
        #[cfg(target_os = "windows")]
        let mut slow = {
            use std::os::windows::process::CommandExt;
            let mut c = Command::new("ping");
            c.args(["-n", "20", "127.0.0.1"]);
            c.creation_flags(0x0800_0000);
            c
        };
        #[cfg(not(target_os = "windows"))]
        let mut slow = {
            let mut c = Command::new("sleep");
            c.arg("20");
            c
        };
        let started = Instant::now();
        let res = run_command_stderr_with_timeout(&mut slow, Duration::from_millis(150));
        assert!(res.is_none(), "hung command must time out and return None");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "timeout kill must return promptly"
        );
    }
}
