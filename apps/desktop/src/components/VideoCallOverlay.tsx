import { useEffect, useRef, useState } from "react";
import {
  Activity,
  AlertTriangle,
  Camera,
  Headphones,
  Mic,
  MicOff,
  Minimize2,
  PhoneOff,
  RefreshCw,
  SlidersHorizontal,
  Video,
  VideoOff,
} from "lucide-react";
import { useI18n } from "../i18n";
import type { CallDevicesDto, CallSnapshotDto, CallTelemetryDto } from "../types";
import {
  formatCallDuration,
  isCameraFailureStatus,
  resolveEffectiveCallDevices,
} from "../utils/callState";
import type { CameraDeviceInfo, LocalCameraStatus } from "../utils/videoCallMedia";
import { ThreadAvatar } from "./ThreadAvatar";

interface Props {
  active: CallSnapshotDto;
  telemetry: CallTelemetryDto | null;
  devices: CallDevicesDto | null;
  devicesLoading: boolean;
  cameraDevices: CameraDeviceInfo[];
  selectedCameraId: string | null;
  localStream: MediaStream | null;
  localCameraStatus: LocalCameraStatus;
  localCameraError: string | null;
  hasRemoteVideoFrame: boolean;
  remoteVideoDims: { width: number; height: number } | null;
  busy: boolean;
  hangupBusy?: boolean;
  onRemoteCanvasChange: (canvas: HTMLCanvasElement | null) => void;
  onToggleMute: (nextMuted: boolean) => void;
  onToggleCamera: (nextEnabled: boolean) => void;
  onHangup: () => void;
  onMinimize: () => void;
  onSelectThread: (partnerId: string) => void;
  onRefreshDevices: () => void;
  onRefreshCameras: () => void;
  onSelectDevices: (input: string | null, output: string | null) => void;
  onSelectCamera: (deviceId: string | null) => void;
}

export function VideoCallOverlay({
  active,
  telemetry,
  devices,
  devicesLoading,
  cameraDevices,
  selectedCameraId,
  localStream,
  localCameraStatus,
  localCameraError,
  hasRemoteVideoFrame,
  remoteVideoDims,
  busy,
  hangupBusy,
  onRemoteCanvasChange,
  onToggleMute,
  onToggleCamera,
  onHangup,
  onMinimize,
  onSelectThread,
  onRefreshDevices,
  onRefreshCameras,
  onSelectDevices,
  onSelectCamera,
}: Props) {
  const { t } = useI18n();
  const [devicesOpen, setDevicesOpen] = useState(false);
  const [localElapsed, setLocalElapsed] = useState(0);
  const remoteCanvasRef = useRef<HTMLCanvasElement | null>(null);
  const localVideoRef = useRef<HTMLVideoElement | null>(null);

  const isConnected = active.phase === "connected";
  const isCancellingStage = active.phase === "dialing" || active.phase === "ringing_outgoing";
  const effectiveMuted = telemetry?.mic_muted ?? active.mic_muted;
  const effectiveCamEnabled = active.cam_enabled;
  const remoteSpeaking = Boolean(isConnected && telemetry?.remote_speaking);
  const missingMic = Boolean(isConnected && telemetry && !telemetry.has_input_device);
  const usingVirtualAudio = Boolean(
    isConnected &&
      telemetry &&
      (telemetry.input_is_virtual || telemetry.output_is_virtual),
  );
  const effectiveDevices = resolveEffectiveCallDevices(devices);
  const cameraHasIssue = isCameraFailureStatus(localCameraStatus);
  const cameraStarting = localCameraStatus === "starting";
  const effectiveHangupBusy = hangupBusy ?? busy;

  useEffect(() => {
    onRemoteCanvasChange(remoteCanvasRef.current);
    return () => {
      onRemoteCanvasChange(null);
    };
  }, [onRemoteCanvasChange]);

  useEffect(() => {
    const el = localVideoRef.current;
    if (!el) return;
    if (localStream) {
      el.srcObject = localStream;
      void el.play().catch(() => undefined);
    } else {
      el.srcObject = null;
    }
    return () => {
      el.srcObject = null;
    };
  }, [localStream]);

  useEffect(() => {
    if (!isConnected) {
      setLocalElapsed(0);
      return;
    }
    const baseSecs = Math.max(active.duration_secs, telemetry?.duration_secs ?? 0);
    setLocalElapsed(baseSecs);
    const startedAt = Date.now() - baseSecs * 1000;
    const timer = window.setInterval(() => {
      setLocalElapsed(Math.max(0, Math.floor((Date.now() - startedAt) / 1000)));
    }, 1000);
    return () => window.clearInterval(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isConnected, active.call_id]);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.key !== "Escape") return;
      e.preventDefault();
      if (devicesOpen) {
        setDevicesOpen(false);
      } else {
        onMinimize();
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [devicesOpen, onMinimize]);

  const effectiveDuration = Math.max(
    active.duration_secs,
    telemetry?.duration_secs ?? 0,
    localElapsed,
  );

  const phaseText =
    active.phase === "dialing"
      ? t("callPhaseDialing")
      : active.phase === "ringing_outgoing"
        ? t("callPhaseRingingOutgoing")
        : active.phase === "connecting"
          ? t("callPhaseVideoConnecting")
          : `${t("callPhaseConnected")} · ${formatCallDuration(effectiveDuration)}`;

  const showRemoteVideo = isConnected && active.peer_cam_enabled && hasRemoteVideoFrame;
  const showLocalPreview = Boolean(
    effectiveCamEnabled && localStream && localCameraStatus === "active",
  );
  const isPreConnectPhase =
    active.phase === "dialing" ||
    active.phase === "ringing_outgoing" ||
    active.phase === "connecting";
  const cameraPendingConnect = Boolean(
    isPreConnectPhase && effectiveCamEnabled && localCameraStatus === "idle",
  );

  const localPipStatusText =
    localCameraStatus === "starting"
      ? t("callVideoCameraStarting")
      : cameraPendingConnect
        ? t("callPhaseVideoConnecting")
        : localCameraStatus === "unavailable"
          ? t("callVideoCameraUnavailableShort")
          : localCameraStatus === "denied"
            ? t("callVideoCameraDeniedShort")
            : localCameraStatus === "unsupported"
              ? t("callVideoCameraUnsupportedShort")
              : localCameraStatus === "error"
                ? t("callVideoCameraErrorShort")
                : t("callVideoCameraOffShort");

  const videoTxFrames = telemetry?.video_tx_frames ?? 0;
  const videoRxFrames = telemetry?.video_rx_frames ?? 0;
  const videoDropped = telemetry?.video_rx_dropped_incomplete ?? 0;
  const partnerDisplayName = active.partner_name || active.partner_id;

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label={t("callVideoTitle")}
      className="fixed inset-0 z-[75] flex items-center justify-center bg-black/75 p-2 backdrop-blur-sm sm:p-4"
    >
      <div
        className="relative flex h-full max-h-[860px] w-full max-w-5xl flex-col overflow-hidden rounded-2xl shadow-2xl"
        style={{
          background: "#070b12",
          border: "1px solid var(--border-subtle)",
          color: "#f8fafc",
        }}
      >
        {/* Top info & actions bar */}
        <div
          className="pointer-events-auto absolute inset-x-0 top-0 z-20 flex flex-wrap items-center justify-between gap-3 px-4 py-3"
          style={{
            background:
              "linear-gradient(180deg, rgba(7, 11, 18, 0.88) 0%, rgba(7, 11, 18, 0.45) 70%, rgba(7, 11, 18, 0) 100%)",
          }}
        >
          <div className="flex min-w-0 flex-1 items-center gap-3">
            <button
              type="button"
              onClick={() => {
                onSelectThread(active.partner_id);
                onMinimize();
              }}
              className={`shrink-0 rounded-full transition hover:opacity-85 ${
                remoteSpeaking ? "ring-2 ring-emerald-400" : ""
              }`}
              title={partnerDisplayName}
            >
              <ThreadAvatar
                avatar={active.partner_avatar}
                title={partnerDisplayName}
                kind="direct"
                size="sm"
              />
            </button>
            <div className="min-w-0">
              <div className="flex flex-wrap items-center gap-1.5">
                <span className="truncate text-sm font-semibold text-white">
                  {partnerDisplayName}
                </span>
                {active.peer_mic_muted && (
                  <span className="inline-flex shrink-0 items-center gap-1 rounded-full bg-white/10 px-2 py-0.5 text-[10.5px] font-medium text-slate-200 backdrop-blur-xs">
                    <MicOff className="h-3 w-3" />
                    <span>{t("callPeerMutedBadge")}</span>
                  </span>
                )}
                {isConnected && !active.peer_cam_enabled && (
                  <span className="inline-flex shrink-0 items-center gap-1 rounded-full bg-white/10 px-2 py-0.5 text-[10.5px] font-medium text-slate-200 backdrop-blur-xs">
                    <VideoOff className="h-3 w-3" />
                    <span>{t("callVideoPeerCamOffBadge")}</span>
                  </span>
                )}
                {missingMic && (
                  <span className="inline-flex shrink-0 items-center gap-1 rounded-full bg-amber-500/20 px-2 py-0.5 text-[10.5px] font-medium text-amber-300">
                    <AlertTriangle className="h-3 w-3" />
                    <span>{t("callNoMicBadge")}</span>
                  </span>
                )}
                {usingVirtualAudio && (
                  <button
                    type="button"
                    onClick={() => {
                      setDevicesOpen(true);
                      onRefreshDevices();
                      onRefreshCameras();
                    }}
                    title={t("callVirtualAudioWarningShort")}
                    className="inline-flex shrink-0 items-center gap-1 rounded-full bg-amber-500/20 px-2 py-0.5 text-[10.5px] font-medium text-amber-300 transition hover:bg-amber-500/30"
                  >
                    <AlertTriangle className="h-3 w-3" />
                    <span>{t("callVirtualAudioBadge")}</span>
                  </button>
                )}
              </div>
              <div
                className="text-xs font-medium tabular-nums"
                style={{ color: "var(--accent)" }}
              >
                {phaseText}
              </div>
            </div>
          </div>

          <div className="flex shrink-0 items-center gap-2">
            {isConnected && telemetry && (
              <div
                title={t("callVideoStatsTooltip", {
                  tx: videoTxFrames,
                  rx: videoRxFrames,
                })}
                className="hidden items-center gap-1.5 rounded-full bg-white/10 px-2.5 py-1 text-[11px] text-slate-200/90 tabular-nums backdrop-blur-xs sm:inline-flex"
              >
                <Activity className="h-3 w-3 text-emerald-400" />
                {remoteVideoDims && showRemoteVideo && (
                  <span>
                    {remoteVideoDims.width}×{remoteVideoDims.height} ·
                  </span>
                )}
                <span>
                  TX {videoTxFrames} · RX {videoRxFrames}
                </span>
              </div>
            )}

            <button
              type="button"
              onClick={onMinimize}
              title={t("callVideoMinimizeBtn")}
              className="inline-flex items-center gap-1.5 rounded-xl bg-white/10 px-3 py-1.5 text-xs font-semibold text-white backdrop-blur-xs transition hover:bg-white/20"
            >
              <Minimize2 className="h-3.5 w-3.5" />
              <span className="hidden sm:inline">{t("callVideoMinimizeBtn")}</span>
            </button>
          </div>
        </div>

        {/* Main video stage */}
        <div className="relative flex min-h-0 flex-1 items-center justify-center overflow-hidden bg-[#05080f]">
          <canvas
            ref={remoteCanvasRef}
            className={`h-full w-full object-contain transition-opacity duration-200 ${
              showRemoteVideo ? "opacity-100" : "opacity-0"
            }`}
          />

          {!showRemoteVideo && (
            <div className="absolute inset-0 flex flex-col items-center justify-center gap-4 px-6 text-center">
              <div
                aria-hidden="true"
                className="pointer-events-none absolute h-64 w-64 rounded-full opacity-15 blur-3xl"
                style={{ background: "var(--accent)" }}
              />
              <div className="relative flex h-24 w-24 items-center justify-center">
                {!isConnected && (
                  <span
                    aria-hidden="true"
                    className="absolute inset-0 animate-ping rounded-full opacity-20"
                    style={{ background: "var(--accent)" }}
                  />
                )}
                <div
                  className={`relative rounded-full ring-2 ring-offset-2 ring-offset-[#05080f] ${
                    remoteSpeaking ? "ring-emerald-400" : "ring-[var(--accent)]"
                  }`}
                >
                  <ThreadAvatar
                    avatar={active.partner_avatar}
                    title={partnerDisplayName}
                    kind="direct"
                    size="lg"
                  />
                </div>
              </div>
              <div className="relative space-y-1.5">
                <h3 className="text-lg font-bold text-white">{partnerDisplayName}</h3>
                {!isConnected ? (
                  <p className="text-sm text-slate-300">{phaseText}</p>
                ) : !active.peer_cam_enabled ? (
                  <div className="inline-flex items-center gap-2 rounded-full bg-white/10 px-3.5 py-1.5 text-xs font-medium text-slate-200">
                    <VideoOff className="h-3.5 w-3.5 text-slate-300" />
                    <span>{t("callVideoPeerCameraOff")}</span>
                  </div>
                ) : (
                  <div className="inline-flex items-center gap-2 rounded-full bg-white/10 px-3.5 py-1.5 text-xs font-medium text-slate-200">
                    <span
                      className="h-2 w-2 animate-pulse rounded-full"
                      style={{ background: "var(--accent)" }}
                    />
                    <span>{t("callVideoWaitingRemote")}</span>
                  </div>
                )}
              </div>
            </div>
          )}

          {/* Local PiP self-view card */}
          <div
            title={localCameraError ?? undefined}
            className="absolute bottom-24 right-3 z-20 h-28 w-40 overflow-hidden rounded-xl border border-white/15 bg-[#0f172a]/95 shadow-2xl sm:bottom-24 sm:right-5 sm:h-36 sm:w-52"
          >
            <video
              ref={localVideoRef}
              autoPlay
              muted
              playsInline
              className={`h-full w-full scale-x-[-1] object-cover transition-opacity duration-200 ${
                showLocalPreview ? "opacity-100" : "opacity-0"
              }`}
            />
            {!showLocalPreview && (
              <div className="absolute inset-0 flex flex-col items-center justify-center gap-1.5 px-3 text-center">
                {cameraStarting ? (
                  <RefreshCw
                    className="h-5 w-5 animate-spin"
                    style={{ color: "var(--accent)" }}
                  />
                ) : cameraPendingConnect ? (
                  <Video className="h-5 w-5 animate-pulse text-slate-300" />
                ) : cameraHasIssue ? (
                  <AlertTriangle className="h-5 w-5 text-amber-400" />
                ) : (
                  <VideoOff className="h-5 w-5 text-slate-400" />
                )}
                <span
                  className={`text-[11px] font-medium leading-tight ${
                    cameraHasIssue ? "text-amber-300" : "text-slate-300"
                  }`}
                >
                  {localPipStatusText}
                </span>
              </div>
            )}
            <div className="pointer-events-none absolute bottom-1.5 left-1.5 inline-flex items-center gap-1 rounded-md bg-black/60 px-1.5 py-0.5 text-[10px] font-medium text-white backdrop-blur-xs">
              <span>{t("callVideoYouLabel")}</span>
              {effectiveMuted && <MicOff className="h-2.5 w-2.5 text-rose-400" />}
            </div>
          </div>
        </div>

        {/* Bottom floating control bar */}
        <div className="pointer-events-none absolute inset-x-0 bottom-4 z-20 flex justify-center px-4">
          <div className="pointer-events-auto relative flex items-center gap-2 rounded-2xl border border-white/15 bg-[#0f172a]/90 px-3.5 py-2 shadow-2xl backdrop-blur-md sm:gap-2.5">
            <button
              type="button"
              disabled={busy || missingMic}
              onClick={() => onToggleMute(!effectiveMuted)}
              title={effectiveMuted ? t("callUnmuteBtn") : t("callMuteBtn")}
              className="inline-flex items-center gap-1.5 rounded-xl px-3 py-2 text-xs font-semibold transition disabled:opacity-45"
              style={{
                background: effectiveMuted ? "var(--danger-bg)" : "rgba(255, 255, 255, 0.1)",
                color: effectiveMuted ? "var(--danger-text)" : "#f8fafc",
              }}
            >
              {effectiveMuted ? <MicOff className="h-4 w-4" /> : <Mic className="h-4 w-4" />}
              <span className="hidden sm:inline">
                {effectiveMuted ? t("callUnmuteBtn") : t("callMuteBtn")}
              </span>
            </button>

            <button
              type="button"
              disabled={busy || cameraStarting}
              onClick={() => onToggleCamera(!effectiveCamEnabled)}
              title={
                effectiveCamEnabled
                  ? t("callVideoCameraOffBtn")
                  : cameraHasIssue
                    ? t("callVideoCameraRetryBtn")
                    : t("callVideoCameraOnBtn")
              }
              className="relative inline-flex items-center gap-1.5 rounded-xl px-3 py-2 text-xs font-semibold transition disabled:opacity-45"
              style={{
                background: !effectiveCamEnabled
                  ? cameraHasIssue
                    ? "rgba(245, 158, 11, 0.2)"
                    : "var(--danger-bg)"
                  : "rgba(255, 255, 255, 0.1)",
                color: !effectiveCamEnabled
                  ? cameraHasIssue
                    ? "#fbbf24"
                    : "var(--danger-text)"
                  : "#f8fafc",
              }}
            >
              {effectiveCamEnabled ? (
                <Video className="h-4 w-4" />
              ) : (
                <VideoOff className="h-4 w-4" />
              )}
              <span className="hidden sm:inline">
                {effectiveCamEnabled
                  ? t("callVideoCameraOffBtn")
                  : cameraHasIssue
                    ? t("callVideoCameraRetryBtn")
                    : t("callVideoCameraOnBtn")}
              </span>
              {cameraHasIssue && (
                <span
                  aria-hidden="true"
                  className="absolute -right-1 -top-1 h-2.5 w-2.5 rounded-full bg-amber-400 ring-2 ring-[#0f172a]"
                />
              )}
            </button>

            <button
              type="button"
              onClick={() => {
                const next = !devicesOpen;
                setDevicesOpen(next);
                if (next) {
                  onRefreshDevices();
                  onRefreshCameras();
                }
              }}
              title={t("callVideoDevicesBtn")}
              className="inline-flex items-center gap-1.5 rounded-xl px-3 py-2 text-xs font-semibold transition"
              style={{
                background: devicesOpen ? "var(--accent-soft)" : "rgba(255, 255, 255, 0.1)",
                color: devicesOpen ? "var(--accent)" : "#e2e8f0",
              }}
            >
              <SlidersHorizontal className="h-4 w-4" />
            </button>

            <button
              type="button"
              disabled={effectiveHangupBusy}
              onClick={onHangup}
              className="inline-flex items-center gap-1.5 rounded-xl bg-rose-600 px-4 py-2 text-xs font-semibold text-white shadow-lg transition hover:bg-rose-500 disabled:opacity-50"
            >
              <PhoneOff className="h-4 w-4" />
              <span>{isCancellingStage ? t("callCancelBtn") : t("callHangupBtn")}</span>
            </button>

            {/* Camera & audio devices popover */}
            {devicesOpen && (
              <div
                className="absolute bottom-full right-0 z-30 mb-3 w-80 max-w-[calc(100vw-2rem)] space-y-2.5 rounded-xl p-3 text-left text-xs shadow-2xl"
                style={{
                  background: "var(--bg-sidebar)",
                  border: "1px solid var(--border-subtle)",
                  color: "var(--text-primary)",
                }}
              >
                <div className="flex items-center justify-between">
                  <span className="font-semibold" style={{ color: "var(--text-secondary)" }}>
                    {t("callVideoDevicesBtn")}
                  </span>
                  <button
                    type="button"
                    disabled={devicesLoading}
                    onClick={() => {
                      onRefreshDevices();
                      onRefreshCameras();
                    }}
                    className="hover-menu inline-flex items-center gap-1 rounded-md px-1.5 py-0.5 text-[11px]"
                    style={{ color: "var(--accent)" }}
                  >
                    <RefreshCw className={`h-3 w-3 ${devicesLoading ? "animate-spin" : ""}`} />
                    <span>{t("callRefreshDevicesBtn")}</span>
                  </button>
                </div>

                {isConnected && telemetry && (
                  <div
                    className="space-y-1 rounded-lg px-2.5 py-2 text-[11px]"
                    style={{
                      background: usingVirtualAudio
                        ? "rgba(245,158,11,0.12)"
                        : "var(--bg-surface)",
                      border: usingVirtualAudio
                        ? "1px solid rgba(245,158,11,0.35)"
                        : "1px solid var(--border-subtle)",
                      color: usingVirtualAudio ? "#fbbf24" : "var(--text-secondary)",
                    }}
                  >
                    <div
                      className="break-words font-medium"
                      style={{ color: "var(--text-primary)" }}
                    >
                      {t("callActiveDevicesLabel", {
                        mic: telemetry.active_input_device || "-",
                        speaker: telemetry.active_output_device || "-",
                      })}
                    </div>
                    <div className="tabular-nums">
                      Audio TX: {telemetry.tx_packets} · RX: {telemetry.rx_packets}
                      {telemetry.tx_errors > 0 ? ` · TX err: ${telemetry.tx_errors}` : ""}
                    </div>
                    <div className="tabular-nums">
                      Video TX: {videoTxFrames} · RX: {videoRxFrames}
                      {videoDropped > 0 ? ` · drop: ${videoDropped}` : ""}
                      {remoteVideoDims && showRemoteVideo
                        ? ` · ${remoteVideoDims.width}×${remoteVideoDims.height}`
                        : ""}
                    </div>
                    {usingVirtualAudio && <div>{t("callVirtualAudioWarningShort")}</div>}
                    {effectiveDuration >= 4 && telemetry.rx_packets === 0 && (
                      <div style={{ color: "#fbbf24" }}>{t("callNoRemotePacketsHint")}</div>
                    )}
                  </div>
                )}

                <label className="block space-y-1">
                  <span
                    className="flex items-center gap-1 text-[11px]"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    <Camera className="h-3 w-3" />
                    {t("callVideoCameraDeviceLabel")}
                  </span>
                  <select
                    value={selectedCameraId ?? ""}
                    onChange={(e) => onSelectCamera(e.target.value || null)}
                    className="w-full rounded-lg px-2 py-1 text-xs outline-none"
                    style={{
                      background: "var(--bg-surface)",
                      border: "1px solid var(--border-subtle)",
                      color: "var(--text-primary)",
                    }}
                  >
                    <option value="">
                      {cameraDevices.length > 0
                        ? t("callVideoDefaultCamera")
                        : t("callVideoNoCameraDetected")}
                    </option>
                    {cameraDevices.map((cam) => (
                      <option key={cam.deviceId} value={cam.deviceId}>
                        {cam.label || cam.deviceId}
                      </option>
                    ))}
                  </select>
                </label>

                <label className="block space-y-1">
                  <span
                    className="flex items-center gap-1 text-[11px]"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    <Mic className="h-3 w-3" />
                    {t("callInputDeviceLabel")}
                  </span>
                  <select
                    value={devices?.selected_input ?? ""}
                    onChange={(e) =>
                      onSelectDevices(e.target.value || null, devices?.selected_output ?? null)
                    }
                    className="w-full rounded-lg px-2 py-1 text-xs outline-none"
                    style={{
                      background: "var(--bg-surface)",
                      border: "1px solid var(--border-subtle)",
                      color: "var(--text-primary)",
                    }}
                  >
                    <option value="">
                      {effectiveDevices.defaultInput
                        ? `${t("callOsDefaultDevice")} — ${effectiveDevices.defaultInput.name}${effectiveDevices.defaultInput.is_virtual ? ` [${t("callVirtualDeviceTag")}]` : ""}`
                        : t("callOsDefaultDevice")}
                    </option>
                    {(devices?.inputs ?? []).map((dev) => (
                      <option key={dev.name} value={dev.name}>
                        {dev.name}
                        {dev.is_virtual ? ` [${t("callVirtualDeviceTag")}]` : ""}
                      </option>
                    ))}
                  </select>
                </label>

                <label className="block space-y-1">
                  <span
                    className="flex items-center gap-1 text-[11px]"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    <Headphones className="h-3 w-3" />
                    {t("callOutputDeviceLabel")}
                  </span>
                  <select
                    value={devices?.selected_output ?? ""}
                    onChange={(e) =>
                      onSelectDevices(devices?.selected_input ?? null, e.target.value || null)
                    }
                    className="w-full rounded-lg px-2 py-1 text-xs outline-none"
                    style={{
                      background: "var(--bg-surface)",
                      border: "1px solid var(--border-subtle)",
                      color: "var(--text-primary)",
                    }}
                  >
                    <option value="">
                      {effectiveDevices.defaultOutput
                        ? `${t("callOsDefaultDevice")} — ${effectiveDevices.defaultOutput.name}${effectiveDevices.defaultOutput.is_virtual ? ` [${t("callVirtualDeviceTag")}]` : ""}`
                        : t("callOsDefaultDevice")}
                    </option>
                    {(devices?.outputs ?? []).map((dev) => (
                      <option key={dev.name} value={dev.name}>
                        {dev.name}
                        {dev.is_virtual ? ` [${t("callVirtualDeviceTag")}]` : ""}
                      </option>
                    ))}
                  </select>
                </label>
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
