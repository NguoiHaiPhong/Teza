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

use crate::framing::{find_aud_delimiter, strip_leading_aud, H264_AUD};

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
        cmd.status().map(|s| s.success()).unwrap_or(false)
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
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let Ok(out) = cmd.output() else {
            return Vec::new();
        };
        let stderr = String::from_utf8_lossy(&out.stderr);
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
        if let Ok(out) = cmd.output() {
            let stderr = String::from_utf8_lossy(&out.stderr);
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

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("Không khởi chạy được camera `{dev_name}`: {e}"))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Không mở được stdout của ffmpeg camera"))?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop_w = stop.clone();
        let child_slot = Arc::new(Mutex::new(Some(child)));
        let child_w = child_slot.clone();

        let worker = thread::Builder::new()
            .name("teza-video-cam".to_string())
            .spawn(move || {
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
            })?;

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
}
