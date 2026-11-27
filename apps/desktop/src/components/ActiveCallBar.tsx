import { useEffect, useRef, useState } from "react";
import {
  AlertTriangle,
  Headphones,
  Maximize2,
  Mic,
  MicOff,
  PhoneOff,
  RefreshCw,
  Video,
  VideoOff,
  Volume2,
} from "lucide-react";
import { useI18n } from "../i18n";
import type { CallDevicesDto, CallSnapshotDto, CallTelemetryDto } from "../types";
import {
  formatCallDuration,
  isCameraFailureStatus,
  normalizeLevelPercent,
  resolveEffectiveCallDevices,
} from "../utils/callState";
import { isImeComposingEvent } from "../utils/searchNormalize";
import type { LocalCameraStatus } from "../utils/videoCallMedia";
import { ThreadAvatar } from "./ThreadAvatar";

interface Props {
  active: CallSnapshotDto;
  telemetry: CallTelemetryDto | null;
  devices: CallDevicesDto | null;
  devicesLoading: boolean;
  busy: boolean;
  hangupBusy?: boolean;
  inline?: boolean;
  localCameraStatus?: LocalCameraStatus;
  onToggleMute: (nextMuted: boolean) => void;
  onToggleCamera?: (nextEnabled: boolean) => void;
  onExpandVideo?: () => void;
  onHangup: () => void;
  onSelectThread: (partnerId: string) => void;
  onRefreshDevices: () => void;
  onSelectDevices: (input: string | null, output: string | null) => void;
  onDevicesPopoverChange?: (open: boolean) => void;
}

export function ActiveCallBar({
  active,
  telemetry,
  devices,
  devicesLoading,
  busy,
  hangupBusy,
  inline = false,
  localCameraStatus = "idle",
  onToggleMute,
  onToggleCamera,
  onExpandVideo,
  onHangup,
  onSelectThread,
  onRefreshDevices,
  onSelectDevices,
  onDevicesPopoverChange,
}: Props) {
  const { t } = useI18n();
  const [devicesOpen, setDevicesOpen] = useState(false);
  const [localElapsed, setLocalElapsed] = useState(0);
  const onDevicesPopoverChangeRef = useRef(onDevicesPopoverChange);
  onDevicesPopoverChangeRef.current = onDevicesPopoverChange;
  const effectiveHangupBusy = hangupBusy ?? busy;
  const matchedTelemetry =
    telemetry && telemetry.call_id === active.call_id ? telemetry : null;

  const isVideoCall = active.call_type === "DirectVideo";
  const isConnected = active.phase === "connected";
  const isCancellingStage = active.phase === "dialing" || active.phase === "ringing_outgoing";
  const effectiveMuted = matchedTelemetry?.mic_muted ?? active.mic_muted;
  const effectiveCamEnabled = active.cam_enabled;
  const cameraHasIssue = isCameraFailureStatus(localCameraStatus);
  const cameraStarting = localCameraStatus === "starting";

  useEffect(() => {
    if (!isConnected) {
      setLocalElapsed(0);
      return;
    }
    const baseSecs = Math.max(active.duration_secs, matchedTelemetry?.duration_secs ?? 0);
    setLocalElapsed(baseSecs);
    const startedAt = Date.now() - baseSecs * 1000;
    const timer = window.setInterval(() => {
      setLocalElapsed(Math.max(0, Math.floor((Date.now() - startedAt) / 1000)));
    }, 1000);
    return () => window.clearInterval(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isConnected, active.call_id]);

  const effectiveDuration = Math.max(
    active.duration_secs,
    matchedTelemetry?.duration_secs ?? 0,
    localElapsed,
  );
  const micPct = effectiveMuted
    ? 0
    : normalizeLevelPercent(matchedTelemetry?.mic_level_permille ?? 0);
  const remotePct = normalizeLevelPercent(matchedTelemetry?.remote_level_permille ?? 0);
  const remoteSpeaking = Boolean(isConnected && matchedTelemetry?.remote_speaking);
  const missingMic = Boolean(
    isConnected && matchedTelemetry && !matchedTelemetry.has_input_device,
  );
  const usingVirtualAudio = Boolean(
    isConnected &&
      matchedTelemetry &&
      (matchedTelemetry.input_is_virtual || matchedTelemetry.output_is_virtual),
  );
  const effectiveDevices = resolveEffectiveCallDevices(devices);

  useEffect(() => {
    onDevicesPopoverChangeRef.current?.(devicesOpen);
  }, [devicesOpen]);

  useEffect(() => {
    return () => {
      onDevicesPopoverChangeRef.current?.(false);
    };
  }, []);

  useEffect(() => {
    if (!devicesOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || isImeComposingEvent(e) || e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      setDevicesOpen(false);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [devicesOpen]);

  const phaseText =
    active.phase === "dialing"
      ? t("callPhaseDialing")
      : active.phase === "ringing_outgoing"
        ? t("callPhaseRingingOutgoing")
        : active.phase === "connecting"
          ? isVideoCall
            ? t("callPhaseVideoConnecting")
            : t("callPhaseConnecting")
          : `${t("callPhaseConnected")} · ${formatCallDuration(effectiveDuration)}`;

  return (
    <div
      className={
        inline
          ? "flex w-full shrink-0 justify-center px-3 py-1.5"
          : "pointer-events-none fixed inset-x-0 top-3 z-[70] flex justify-center px-4"
      }
      style={
        inline
          ? {
              background: "var(--bg-sidebar)",
              borderBottom: "1px solid var(--border-subtle)",
            }
          : undefined
      }
    >
      <div
        className={`pointer-events-auto relative flex w-full flex-wrap items-center justify-between gap-3 ${
          inline
            ? "max-w-4xl rounded-xl px-3 py-1.5 shadow-sm"
            : "max-w-2xl rounded-2xl px-3.5 py-2 shadow-2xl"
        }`}
        style={{
          background: inline ? "var(--bg-surface)" : "var(--bg-sidebar)",
          border: "1px solid var(--border-subtle)",
          color: "var(--text-primary)",
        }}
      >
        <div className="flex min-w-0 flex-1 items-center gap-2.5 text-left">
          <button
            type="button"
            onClick={() => onSelectThread(active.partner_id)}
            className={`shrink-0 rounded-full transition hover:opacity-85 ${
              remoteSpeaking ? "ring-2 ring-emerald-400" : ""
            }`}
          >
            <ThreadAvatar
              avatar={active.partner_avatar}
              title={active.partner_name || active.partner_id}
              kind="direct"
              size="sm"
            />
          </button>
          <div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-center gap-1.5">
              <button
                type="button"
                onClick={() => onSelectThread(active.partner_id)}
                className="truncate text-left text-[13.5px] font-semibold transition hover:opacity-85"
              >
                {active.partner_name || active.partner_id}
              </button>
              {isVideoCall && (
                <span
                  className="inline-flex shrink-0 items-center gap-1 rounded-full px-2 py-0.5 text-[10.5px] font-semibold"
                  style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
                >
                  <Video className="h-3 w-3" />
                  <span>{t("callVideoBadge")}</span>
                </span>
              )}
              {active.peer_mic_muted && (
                <span
                  className="inline-flex shrink-0 items-center gap-1 rounded-full px-2 py-0.5 text-[10.5px] font-medium"
                  style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
                >
                  <MicOff className="h-3 w-3" />
                  <span>{t("callPeerMutedBadge")}</span>
                </span>
              )}
              {isVideoCall && isConnected && !active.peer_cam_enabled && (
                <span
                  className="inline-flex shrink-0 items-center gap-1 rounded-full px-2 py-0.5 text-[10.5px] font-medium"
                  style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
                >
                  <VideoOff className="h-3 w-3" />
                  <span>{t("callVideoPeerCamOffBadge")}</span>
                </span>
              )}
              {missingMic && (
                <span
                  className="inline-flex shrink-0 items-center gap-1 rounded-full bg-amber-500/15 px-2 py-0.5 text-[10.5px] font-medium text-amber-400"
                >
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
                  }}
                  title={t("callVirtualAudioWarningShort")}
                  className="inline-flex shrink-0 items-center gap-1 rounded-full bg-amber-500/15 px-2 py-0.5 text-[10.5px] font-medium text-amber-400 transition hover:bg-amber-500/25"
                >
                  <AlertTriangle className="h-3 w-3" />
                  <span>{t("callVirtualAudioBadge")}</span>
                </button>
              )}
            </div>
            <button
              type="button"
              onClick={() => onSelectThread(active.partner_id)}
              className="block text-left text-xs tabular-nums transition hover:opacity-85"
              style={{ color: "var(--accent)" }}
            >
              {phaseText}
            </button>
          </div>
        </div>

        {isConnected && (
          <div className="hidden items-center gap-3 text-[11px] sm:flex">
            <div className="flex items-center gap-1.5" title={t("callMicLevelLabel")}>
              <Mic className="h-3 w-3" style={{ color: "var(--text-secondary)" }} />
              <div
                className="h-1.5 w-12 overflow-hidden rounded-full"
                style={{ background: "var(--bg-surface)" }}
              >
                <div
                  className="h-full rounded-full transition-all duration-150"
                  style={{
                    width: `${micPct}%`,
                    background: effectiveMuted ? "var(--text-secondary)" : "var(--accent)",
                  }}
                />
              </div>
            </div>
            <div className="flex items-center gap-1.5" title={t("callRemoteLevelLabel")}>
              <Volume2
                className={`h-3 w-3 ${remoteSpeaking ? "text-emerald-400" : ""}`}
                style={remoteSpeaking ? undefined : { color: "var(--text-secondary)" }}
              />
              <div
                className="h-1.5 w-12 overflow-hidden rounded-full"
                style={{ background: "var(--bg-surface)" }}
              >
                <div
                  className="h-full rounded-full bg-emerald-400 transition-all duration-150"
                  style={{ width: `${remotePct}%` }}
                />
              </div>
            </div>
          </div>
        )}

        <div className="flex shrink-0 items-center gap-1.5">
          <button
            type="button"
            disabled={busy || missingMic}
            onClick={() => onToggleMute(!effectiveMuted)}
            title={effectiveMuted ? t("callUnmuteBtn") : t("callMuteBtn")}
            className="hover-menu inline-flex items-center gap-1.5 rounded-xl px-2.5 py-1.5 text-xs font-semibold transition disabled:opacity-45"
            style={{
              background: effectiveMuted ? "var(--danger-bg)" : "var(--bg-surface)",
              color: effectiveMuted ? "var(--danger-text)" : "var(--text-primary)",
            }}
          >
            {effectiveMuted ? <MicOff className="h-3.5 w-3.5" /> : <Mic className="h-3.5 w-3.5" />}
            <span className="hidden sm:inline">
              {effectiveMuted ? t("callUnmuteBtn") : t("callMuteBtn")}
            </span>
          </button>

          {isVideoCall && onToggleCamera && (
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
              className="hover-menu relative inline-flex items-center gap-1.5 rounded-xl px-2.5 py-1.5 text-xs font-semibold transition disabled:opacity-45"
              style={{
                background: !effectiveCamEnabled
                  ? cameraHasIssue
                    ? "rgba(245, 158, 11, 0.16)"
                    : "var(--danger-bg)"
                  : "var(--bg-surface)",
                color: !effectiveCamEnabled
                  ? cameraHasIssue
                    ? "#f59e0b"
                    : "var(--danger-text)"
                  : "var(--text-primary)",
              }}
            >
              {effectiveCamEnabled ? (
                <Video className="h-3.5 w-3.5" />
              ) : (
                <VideoOff className="h-3.5 w-3.5" />
              )}
              <span className="hidden md:inline">
                {effectiveCamEnabled ? t("callVideoCameraOffBtn") : t("callVideoCameraOnBtn")}
              </span>
              {cameraHasIssue && (
                <span
                  aria-hidden="true"
                  className="absolute -right-0.5 -top-0.5 h-2 w-2 rounded-full bg-amber-400"
                />
              )}
            </button>
          )}

          {isVideoCall && onExpandVideo && (
            <button
              type="button"
              onClick={onExpandVideo}
              title={t("callVideoExpandBtn")}
              className="hover-menu inline-flex items-center gap-1.5 rounded-xl px-2.5 py-1.5 text-xs font-semibold transition"
              style={{
                background: "var(--accent-soft)",
                color: "var(--accent)",
              }}
            >
              <Maximize2 className="h-3.5 w-3.5" />
              <span className="hidden sm:inline">{t("callVideoExpandBtn")}</span>
            </button>
          )}

          <button
            type="button"
            onClick={() => {
              const next = !devicesOpen;
              setDevicesOpen(next);
              if (next) onRefreshDevices();
            }}
            title={t("callAudioDevicesBtn")}
            className="hover-menu rounded-xl p-2 transition"
            style={{
              background: devicesOpen ? "var(--accent-soft)" : "var(--bg-surface)",
              color: devicesOpen ? "var(--accent)" : "var(--text-secondary)",
            }}
          >
            <Headphones className="h-3.5 w-3.5" />
          </button>

          <button
            type="button"
            disabled={effectiveHangupBusy}
            onClick={onHangup}
            className="inline-flex items-center gap-1.5 rounded-xl bg-rose-600 px-3 py-1.5 text-xs font-semibold text-white transition hover:bg-rose-500 disabled:opacity-50"
          >
            <PhoneOff className="h-3.5 w-3.5" />
            <span>{isCancellingStage ? t("callCancelBtn") : t("callHangupBtn")}</span>
          </button>
        </div>

        {devicesOpen && (
          <div
            className="absolute right-3 top-full z-[72] mt-2 w-80 space-y-2.5 rounded-xl p-3 text-xs shadow-2xl"
            style={{
              background: "var(--bg-sidebar)",
              border: "1px solid var(--border-subtle)",
            }}
          >
            <div className="flex items-center justify-between">
              <span className="font-semibold" style={{ color: "var(--text-secondary)" }}>
                {t("callAudioSettingsHeading")}
              </span>
              <button
                type="button"
                disabled={devicesLoading}
                onClick={onRefreshDevices}
                className="hover-menu inline-flex items-center gap-1 rounded-md px-1.5 py-0.5 text-[11px]"
                style={{ color: "var(--accent)" }}
              >
                <RefreshCw className={`h-3 w-3 ${devicesLoading ? "animate-spin" : ""}`} />
                <span>{t("callRefreshDevicesBtn")}</span>
              </button>
            </div>
            {isConnected && matchedTelemetry && (
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
                <div className="break-words font-medium" style={{ color: "var(--text-primary)" }}>
                  {t("callActiveDevicesLabel", {
                    mic: matchedTelemetry.active_input_device || "-",
                    speaker: matchedTelemetry.active_output_device || "-",
                  })}
                </div>
                <div className="tabular-nums">
                  TX: {matchedTelemetry.tx_packets} · RX: {matchedTelemetry.rx_packets}
                  {matchedTelemetry.tx_errors > 0 ? ` · TX err: ${matchedTelemetry.tx_errors}` : ""}
                </div>
                {isVideoCall &&
                  (matchedTelemetry.video_tx_frames !== undefined ||
                    matchedTelemetry.video_rx_frames !== undefined) && (
                    <div className="tabular-nums">
                      Video TX: {matchedTelemetry.video_tx_frames ?? 0} · RX:{" "}
                      {matchedTelemetry.video_rx_frames ?? 0}
                    </div>
                  )}
                {usingVirtualAudio && <div>{t("callVirtualAudioWarningShort")}</div>}
                {effectiveDuration >= 4 && matchedTelemetry.rx_packets === 0 && (
                  <div style={{ color: "#fbbf24" }}>{t("callNoRemotePacketsHint")}</div>
                )}
              </div>
            )}
            <label className="block space-y-1">
              <span className="text-[11px]" style={{ color: "var(--text-secondary)" }}>
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
              <span className="text-[11px]" style={{ color: "var(--text-secondary)" }}>
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
  );
}
