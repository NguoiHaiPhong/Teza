import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type {
  MouseEvent as ReactMouseEvent,
  PointerEvent as ReactPointerEvent,
} from "react";
import {
  Activity,
  AlertTriangle,
  Camera,
  Eye,
  EyeOff,
  GripHorizontal,
  Headphones,
  Maximize2,
  Mic,
  MicOff,
  Minimize2,
  Minus,
  PhoneOff,
  PictureInPicture2,
  RefreshCw,
  SlidersHorizontal,
  Video,
  VideoOff,
} from "lucide-react";
import { useI18n, type UiLanguage } from "../i18n";
import type { CallDevicesDto, CallSnapshotDto, CallTelemetryDto } from "../types";
import {
  formatCallDuration,
  isCameraFailureStatus,
  resolveEffectiveCallDevices,
} from "../utils/callState";
import type { CameraDeviceInfo, LocalCameraStatus } from "../utils/videoCallMedia";
import {
  applyPipDrag,
  applyPipPreset,
  applyPipResize,
  clampPipRect,
  computeDefaultPipRect,
  detectActivePipPreset,
  getNextPipPreset,
  type PipPresetKey,
  type PipRect,
  type ResizeHandle,
} from "../utils/videoPipLayout";
import { ThreadAvatar } from "./ThreadAvatar";

export type VideoCallViewMode = "expanded" | "floating";

export interface VideoCallOverlayProps {
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
  minimized?: boolean;
  viewMode?: VideoCallViewMode;
  onViewModeChange?: (mode: VideoCallViewMode) => void;
  onCollapseToBar?: () => void;
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

type PointerInteraction =
  | {
      kind: "drag";
      pointerId: number;
      startX: number;
      startY: number;
      startRect: PipRect;
    }
  | {
      kind: "resize";
      handle: ResizeHandle;
      pointerId: number;
      startX: number;
      startY: number;
      startRect: PipRect;
    };

interface VideoPipCopy {
  floatBtn: string;
  floatBtnTooltip: string;
  collapseToBarTooltip: string;
  systemPipBtn: string;
  systemPipActiveBtn: string;
  systemPipBanner: string;
  systemPipFallbackNotice: string;
  dragHeaderTooltip: string;
  expandedHeaderDoubleClickHint: string;
  cycleSizeTooltip: string;
  presetTooltip: Record<PipPresetKey, string>;
  hideSelfViewTooltip: string;
  showSelfViewTooltip: string;
  resizeHandleTooltip: string;
}

function getVideoPipCopy(lang: UiLanguage): VideoPipCopy {
  switch (lang) {
    case "en":
      return {
        floatBtn: "Float",
        floatBtnTooltip: "Float video frame so you can chat in Teza at the same time",
        collapseToBarTooltip: "Collapse to call bar",
        systemPipBtn: "Pop-out (PiP)",
        systemPipActiveBtn: "Exit PiP",
        systemPipBanner: "Showing in Picture-in-Picture",
        systemPipFallbackNotice: "System PiP unavailable — switched to floating frame in Teza",
        dragHeaderTooltip: "Drag to move · Double-click to expand",
        expandedHeaderDoubleClickHint: "Double-click to switch to floating frame",
        cycleSizeTooltip: "Switch frame size (S / M / L)",
        presetTooltip: {
          sm: "Small frame (S)",
          md: "Medium frame (M)",
          lg: "Large frame (L)",
        },
        hideSelfViewTooltip: "Hide self-view",
        showSelfViewTooltip: "Show self-view",
        resizeHandleTooltip: "Drag to resize",
      };
    case "ko":
      return {
        floatBtn: "플로팅",
        floatBtnTooltip: "채팅과 동시에 통화할 수 있도록 플로팅 창으로 전환",
        collapseToBarTooltip: "통화 바로 접기",
        systemPipBtn: "항상 위 창 (PiP)",
        systemPipActiveBtn: "PiP 종료",
        systemPipBanner: "항상 위 창(PiP)으로 표시 중",
        systemPipFallbackNotice: "시스템 PiP를 사용할 수 없어 Teza 플로팅 창으로 전환했습니다",
        dragHeaderTooltip: "드래그하여 이동 · 더블클릭하여 확대",
        expandedHeaderDoubleClickHint: "더블클릭하여 플로팅 창으로 전환",
        cycleSizeTooltip: "프레임 크기 전환 (S / M / L)",
        presetTooltip: {
          sm: "작게 (S)",
          md: "보통 (M)",
          lg: "크게 (L)",
        },
        hideSelfViewTooltip: "내 화면 숨기기",
        showSelfViewTooltip: "내 화면 표시",
        resizeHandleTooltip: "드래그하여 크기 조절",
      };
    default:
      return {
        floatBtn: "Khung nổi",
        floatBtnTooltip: "Thu gọn thành khung nổi để vừa gọi video vừa chat trong Teza",
        collapseToBarTooltip: "Thu gọn về thanh gọi",
        systemPipBtn: "Cửa sổ nổi (PiP)",
        systemPipActiveBtn: "Tắt cửa sổ nổi",
        systemPipBanner: "Đang hiển thị cửa sổ nổi",
        systemPipFallbackNotice:
          "Cửa sổ nổi hệ thống không khả dụng — đã chuyển sang khung nổi trong Teza",
        dragHeaderTooltip: "Kéo để di chuyển · Nhấp đúp để phóng to",
        expandedHeaderDoubleClickHint: "Nhấp đúp để thu gọn thành khung nổi",
        cycleSizeTooltip: "Đổi kích thước khung (S / M / L)",
        presetTooltip: {
          sm: "Cỡ nhỏ (S)",
          md: "Cỡ vừa (M)",
          lg: "Cỡ lớn (L)",
        },
        hideSelfViewTooltip: "Ẩn camera của bạn",
        showSelfViewTooltip: "Hiện camera của bạn",
        resizeHandleTooltip: "Kéo để thay đổi kích thước",
      };
  }
}

const RESIZE_HANDLES: readonly { handle: ResizeHandle; className: string }[] = [
  { handle: "n", className: "cursor-ns-resize -top-1.5 left-3 right-3 h-3 z-30" },
  { handle: "s", className: "cursor-ns-resize -bottom-1.5 left-3 right-3 h-3 z-30" },
  { handle: "e", className: "cursor-ew-resize -right-1.5 top-3 bottom-3 w-3 z-30" },
  { handle: "w", className: "cursor-ew-resize -left-1.5 top-3 bottom-3 w-3 z-30" },
  { handle: "nw", className: "cursor-nwse-resize -left-1.5 -top-1.5 h-4 w-4 z-40" },
  { handle: "ne", className: "cursor-nesw-resize -right-1.5 -top-1.5 h-4 w-4 z-40" },
  { handle: "sw", className: "cursor-nesw-resize -bottom-1.5 -left-1.5 h-4 w-4 z-40" },
  { handle: "se", className: "cursor-nwse-resize -bottom-1.5 -right-1.5 h-4 w-4 z-40" },
];

const PIP_PRESET_KEYS: readonly PipPresetKey[] = ["sm", "md", "lg"];

const PIP_PRESET_LABELS: Record<PipPresetKey, string> = {
  sm: "S",
  md: "M",
  lg: "L",
};

const DEFAULT_PIP_CANVAS_W = 640;
const DEFAULT_PIP_CANVAS_H = 360;

function readViewportSize(): { width: number; height: number } {
  if (typeof window === "undefined") {
    return { width: 1280, height: 800 };
  }
  return {
    width: window.innerWidth || 1280,
    height: window.innerHeight || 800,
  };
}

function paintPipPlaceholderFrame(
  canvas: HTMLCanvasElement,
  partnerName: string,
  subtitle: string,
): void {
  try {
    if (canvas.width < 16 || canvas.height < 16) {
      canvas.width = DEFAULT_PIP_CANVAS_W;
      canvas.height = DEFAULT_PIP_CANVAS_H;
    }
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const w = canvas.width;
    const h = canvas.height;

    const grad = ctx.createLinearGradient(0, 0, 0, h);
    grad.addColorStop(0, "#0f172a");
    grad.addColorStop(1, "#05080f");
    ctx.fillStyle = grad;
    ctx.fillRect(0, 0, w, h);

    // Subtle center badge with partner initial
    const radius = Math.max(26, Math.min(w, h) * 0.14);
    const cx = w / 2;
    const cy = h * 0.42;

    ctx.beginPath();
    ctx.arc(cx, cy, radius, 0, Math.PI * 2);
    ctx.fillStyle = "rgba(59, 130, 246, 0.22)";
    ctx.fill();
    ctx.lineWidth = 2;
    ctx.strokeStyle = "rgba(96, 165, 250, 0.65)";
    ctx.stroke();

    const initial = (partnerName.trim()[0] || "T").toUpperCase();
    ctx.fillStyle = "#f8fafc";
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.font = `700 ${Math.round(radius * 0.9)}px system-ui, -apple-system, Segoe UI, sans-serif`;
    ctx.fillText(initial, cx, cy + 1);

    const nameSize = Math.max(15, Math.round(h * 0.06));
    ctx.font = `600 ${nameSize}px system-ui, -apple-system, Segoe UI, sans-serif`;
    ctx.fillStyle = "#f8fafc";
    const clippedName = partnerName.length > 28 ? `${partnerName.slice(0, 27)}…` : partnerName;
    ctx.fillText(clippedName, cx, cy + radius + nameSize * 1.15);

    const subSize = Math.max(12, Math.round(h * 0.042));
    ctx.font = `500 ${subSize}px system-ui, -apple-system, Segoe UI, sans-serif`;
    ctx.fillStyle = "#94a3b8";
    const clippedSub = subtitle.length > 42 ? `${subtitle.slice(0, 41)}…` : subtitle;
    ctx.fillText(clippedSub, cx, cy + radius + nameSize * 1.15 + subSize * 1.45);
  } catch {
    // Ignore 2D context drawing errors
  }
}

function isInteractiveTarget(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false;
  return Boolean(target.closest("button, a, input, select, textarea, [data-no-drag='true']"));
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
  minimized = false,
  viewMode,
  onViewModeChange,
  onCollapseToBar,
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
}: VideoCallOverlayProps) {
  const { t, lang } = useI18n();
  const copy = useMemo(() => getVideoPipCopy(lang), [lang]);

  const [internalViewMode, setInternalViewMode] = useState<VideoCallViewMode>(
    viewMode ?? "expanded",
  );
  const currentViewMode: VideoCallViewMode = viewMode ?? internalViewMode;
  const isExpanded = currentViewMode === "expanded";

  const [pipRect, setPipRect] = useState<PipRect>(() => {
    const vp = readViewportSize();
    return computeDefaultPipRect(vp.width, vp.height);
  });
  const [isDragging, setIsDragging] = useState(false);
  const [activeResizeHandle, setActiveResizeHandle] = useState<ResizeHandle | null>(null);
  const [localPipUserVisible, setLocalPipUserVisible] = useState(true);

  const [devicesOpen, setDevicesOpen] = useState(false);
  const [localElapsed, setLocalElapsed] = useState(0);

  const [systemPipActive, setSystemPipActive] = useState(false);
  const [systemPipNotice, setSystemPipNotice] = useState<string | null>(null);

  const remoteCanvasRef = useRef<HTMLCanvasElement | null>(null);
  const localVideoRef = useRef<HTMLVideoElement | null>(null);
  const pipBridgeVideoRef = useRef<HTMLVideoElement | null>(null);
  const pipStreamRef = useRef<MediaStream | null>(null);
  const pipBusyRef = useRef(false);
  const pipNoticeTimerRef = useRef<number | null>(null);
  const interactionRef = useRef<PointerInteraction | null>(null);

  const changeViewMode = useCallback(
    (next: VideoCallViewMode) => {
      if (viewMode === undefined) {
        setInternalViewMode(next);
      }
      onViewModeChange?.(next);
    },
    [viewMode, onViewModeChange],
  );

  const handleCollapseToBar = useCallback(() => {
    if (onCollapseToBar) {
      onCollapseToBar();
    } else {
      onMinimize();
    }
  }, [onCollapseToBar, onMinimize]);

  const showTemporaryPipNotice = useCallback((message: string) => {
    setSystemPipNotice(message);
    if (pipNoticeTimerRef.current !== null) {
      window.clearTimeout(pipNoticeTimerRef.current);
    }
    pipNoticeTimerRef.current = window.setTimeout(() => {
      setSystemPipNotice(null);
      pipNoticeTimerRef.current = null;
    }, 4500);
  }, []);

  const stopPipStream = useCallback(() => {
    const stream = pipStreamRef.current;
    if (stream) {
      for (const track of stream.getTracks()) {
        try {
          track.stop();
        } catch {
          // Ignore track stop failures
        }
      }
      pipStreamRef.current = null;
    }
    const bridge = pipBridgeVideoRef.current;
    if (bridge && bridge.srcObject) {
      bridge.srcObject = null;
    }
  }, []);

  const matchedTelemetry =
    telemetry && telemetry.call_id === active.call_id ? telemetry : null;

  const isConnected = active.phase === "connected";
  const isCancellingStage = active.phase === "dialing" || active.phase === "ringing_outgoing";
  const effectiveMuted = matchedTelemetry?.mic_muted ?? active.mic_muted;
  const effectiveCamEnabled = active.cam_enabled;
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
  const cameraHasIssue = isCameraFailureStatus(localCameraStatus);
  const cameraStarting = localCameraStatus === "starting";
  const effectiveHangupBusy = hangupBusy ?? busy;

  // Attach remote canvas once for the lifetime of this overlay (never remounts across viewMode!)
  useEffect(() => {
    const canvas = remoteCanvasRef.current;
    if (canvas && canvas.width === 300 && canvas.height === 150) {
      canvas.width = DEFAULT_PIP_CANVAS_W;
      canvas.height = DEFAULT_PIP_CANVAS_H;
    }
    onRemoteCanvasChange(canvas);
    return () => {
      onRemoteCanvasChange(null);
    };
  }, [onRemoteCanvasChange]);

  // Bind local camera MediaStream without remounting the <video> element
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

  // Call duration ticker
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

  // Close devices popover when minimized to call bar
  useEffect(() => {
    if (minimized) {
      setDevicesOpen(false);
    }
  }, [minimized]);

  // Auto-reclamp floating PiP rectangle when browser/window resizes
  useEffect(() => {
    const onWindowResize = () => {
      const vp = readViewportSize();
      setPipRect((prev) => clampPipRect(prev, vp.width, vp.height));
    };
    window.addEventListener("resize", onWindowResize);
    return () => window.removeEventListener("resize", onWindowResize);
  }, []);

  // Re-clamp when switching into floating mode or restoring from minimized so frame is guaranteed visible
  useEffect(() => {
    if (!isExpanded && !minimized) {
      const vp = readViewportSize();
      setPipRect((prev) => clampPipRect(prev, vp.width, vp.height));
    }
  }, [isExpanded, minimized]);

  // Escape key: close devices popover; or exit expanded -> floating.
  // Never steal Escape from the underlying chat UI while in floating or minimized mode!
  useEffect(() => {
    if (minimized) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.key !== "Escape") return;
      if (devicesOpen) {
        e.preventDefault();
        e.stopPropagation();
        setDevicesOpen(false);
        return;
      }
      if (isExpanded) {
        e.preventDefault();
        e.stopPropagation();
        changeViewMode("floating");
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [minimized, devicesOpen, isExpanded, changeViewMode]);

  // Listen to System Picture-in-Picture lifecycle events on the hidden bridge video
  useEffect(() => {
    const bridge = pipBridgeVideoRef.current;
    if (!bridge) return;
    const onEnterPip = () => {
      setSystemPipActive(true);
      setSystemPipNotice(null);
    };
    const onLeavePip = () => {
      setSystemPipActive(false);
      stopPipStream();
    };
    bridge.addEventListener("enterpictureinpicture", onEnterPip);
    bridge.addEventListener("leavepictureinpicture", onLeavePip);
    return () => {
      bridge.removeEventListener("enterpictureinpicture", onEnterPip);
      bridge.removeEventListener("leavepictureinpicture", onLeavePip);
    };
  }, [stopPipStream]);

  // Clean up System PiP when the call ends or returns to idle
  useEffect(() => {
    const phase = active.phase as string;
    if (phase !== "idle" && phase !== "ended") return;
    stopPipStream();
    setSystemPipActive(false);
    if (
      typeof document !== "undefined" &&
      pipBridgeVideoRef.current &&
      document.pictureInPictureElement === pipBridgeVideoRef.current
    ) {
      void document.exitPictureInPicture().catch(() => undefined);
    }
  }, [active.phase, stopPipStream]);

  // Clean up System PiP and timers on unmount
  useEffect(() => {
    return () => {
      if (pipNoticeTimerRef.current !== null) {
        window.clearTimeout(pipNoticeTimerRef.current);
        pipNoticeTimerRef.current = null;
      }
      const bridge = pipBridgeVideoRef.current;
      stopPipStream();
      if (
        typeof document !== "undefined" &&
        bridge &&
        document.pictureInPictureElement === bridge
      ) {
        void document.exitPictureInPicture().catch(() => undefined);
      }
    };
  }, [stopPipStream]);

  const effectiveDuration = Math.max(
    active.duration_secs,
    matchedTelemetry?.duration_secs ?? 0,
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

  const videoTxFrames = matchedTelemetry?.video_tx_frames ?? 0;
  const videoRxFrames = matchedTelemetry?.video_rx_frames ?? 0;
  const videoDropped = matchedTelemetry?.video_rx_dropped_incomplete ?? 0;
  const partnerDisplayName = active.partner_name || active.partner_id;

  const placeholderSubtitle = !isConnected
    ? phaseText
    : !active.peer_cam_enabled
      ? t("callVideoPeerCameraOff")
      : t("callVideoWaitingRemote");

  // Keep System PiP canvas updated with a clean placeholder whenever remote video is absent
  useEffect(() => {
    if (!systemPipActive || showRemoteVideo) return;
    const canvas = remoteCanvasRef.current;
    if (!canvas) return;
    paintPipPlaceholderFrame(canvas, partnerDisplayName, placeholderSubtitle);
  }, [systemPipActive, showRemoteVideo, partnerDisplayName, placeholderSubtitle]);

  const handleToggleSystemPip = useCallback(async () => {
    if (pipBusyRef.current) return;
    pipBusyRef.current = true;
    try {
      const bridge = pipBridgeVideoRef.current;
      const canvas = remoteCanvasRef.current;

      if (
        typeof document !== "undefined" &&
        bridge &&
        document.pictureInPictureElement === bridge
      ) {
        await document.exitPictureInPicture();
        stopPipStream();
        setSystemPipActive(false);
        return;
      }

      const supportsPip =
        typeof document !== "undefined" &&
        Boolean(document.pictureInPictureEnabled) &&
        Boolean(bridge && typeof bridge.requestPictureInPicture === "function") &&
        Boolean(canvas && typeof canvas.captureStream === "function");

      if (!supportsPip || !bridge || !canvas) {
        changeViewMode("floating");
        showTemporaryPipNotice(copy.systemPipFallbackNotice);
        return;
      }

      if (canvas.width < 16 || canvas.height < 16) {
        canvas.width = DEFAULT_PIP_CANVAS_W;
        canvas.height = DEFAULT_PIP_CANVAS_H;
      }

      stopPipStream();
      const stream = canvas.captureStream(30);
      pipStreamRef.current = stream;
      bridge.srcObject = stream;

      // Ensure at least one freshly committed frame is emitted on the captured stream
      // so WebView2 / Chromium resolves loadedmetadata + play() immediately.
      if (!showRemoteVideo) {
        paintPipPlaceholderFrame(canvas, partnerDisplayName, placeholderSubtitle);
      } else {
        try {
          const ctx = canvas.getContext("2d");
          if (ctx) {
            ctx.drawImage(canvas, 0, 0);
          }
        } catch {
          // Ignore self-blit errors; live decoded frames continue streaming
        }
      }

      await bridge.play();
      if (bridge.readyState < 1) {
        await new Promise<void>((resolve) => {
          let settled = false;
          const finish = () => {
            if (settled) return;
            settled = true;
            bridge.removeEventListener("loadedmetadata", finish);
            bridge.removeEventListener("error", finish);
            window.clearTimeout(timer);
            resolve();
          };
          const timer = window.setTimeout(finish, 800);
          bridge.addEventListener("loadedmetadata", finish, { once: true });
          bridge.addEventListener("error", finish, { once: true });
        });
      }
      await bridge.requestPictureInPicture();
    } catch {
      stopPipStream();
      setSystemPipActive(false);
      changeViewMode("floating");
      showTemporaryPipNotice(copy.systemPipFallbackNotice);
    } finally {
      pipBusyRef.current = false;
    }
  }, [
    changeViewMode,
    copy.systemPipFallbackNotice,
    partnerDisplayName,
    placeholderSubtitle,
    showRemoteVideo,
    showTemporaryPipNotice,
    stopPipStream,
  ]);

  // Header drag handlers (active in floating mode)
  const handleHeaderPointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (isExpanded || e.button !== 0 || isInteractiveTarget(e.target)) return;
    e.preventDefault();
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      // Ignore pointer capture errors
    }
    interactionRef.current = {
      kind: "drag",
      pointerId: e.pointerId,
      startX: e.clientX,
      startY: e.clientY,
      startRect: pipRect,
    };
    setIsDragging(true);
  };

  const handleHeaderPointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    const activeInteraction = interactionRef.current;
    if (
      !activeInteraction ||
      activeInteraction.kind !== "drag" ||
      activeInteraction.pointerId !== e.pointerId
    ) {
      return;
    }
    const vp = readViewportSize();
    const dx = e.clientX - activeInteraction.startX;
    const dy = e.clientY - activeInteraction.startY;
    setPipRect(applyPipDrag(activeInteraction.startRect, dx, dy, vp.width, vp.height));
  };

  const finishHeaderDrag = (e: ReactPointerEvent<HTMLDivElement>) => {
    const activeInteraction = interactionRef.current;
    if (
      !activeInteraction ||
      activeInteraction.kind !== "drag" ||
      activeInteraction.pointerId !== e.pointerId
    ) {
      return;
    }
    try {
      if (e.currentTarget.hasPointerCapture(e.pointerId)) {
        e.currentTarget.releasePointerCapture(e.pointerId);
      }
    } catch {
      // Ignore release errors
    }
    interactionRef.current = null;
    setIsDragging(false);
  };

  const handleHeaderDoubleClick = (e: ReactMouseEvent<HTMLDivElement>) => {
    if (isInteractiveTarget(e.target)) return;
    changeViewMode(isExpanded ? "floating" : "expanded");
  };

  // Edge/corner resize handlers (active in floating mode)
  const handleResizePointerDown =
    (handle: ResizeHandle) => (e: ReactPointerEvent<HTMLDivElement>) => {
      if (isExpanded || e.button !== 0) return;
      e.preventDefault();
      e.stopPropagation();
      try {
        e.currentTarget.setPointerCapture(e.pointerId);
      } catch {
        // Ignore pointer capture errors
      }
      interactionRef.current = {
        kind: "resize",
        handle,
        pointerId: e.pointerId,
        startX: e.clientX,
        startY: e.clientY,
        startRect: pipRect,
      };
      setActiveResizeHandle(handle);
    };

  const handleResizePointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    const activeInteraction = interactionRef.current;
    if (
      !activeInteraction ||
      activeInteraction.kind !== "resize" ||
      activeInteraction.pointerId !== e.pointerId
    ) {
      return;
    }
    const vp = readViewportSize();
    const dx = e.clientX - activeInteraction.startX;
    const dy = e.clientY - activeInteraction.startY;
    setPipRect(
      applyPipResize(
        activeInteraction.startRect,
        activeInteraction.handle,
        dx,
        dy,
        vp.width,
        vp.height,
      ),
    );
  };

  const finishResize = (e: ReactPointerEvent<HTMLDivElement>) => {
    const activeInteraction = interactionRef.current;
    if (
      !activeInteraction ||
      activeInteraction.kind !== "resize" ||
      activeInteraction.pointerId !== e.pointerId
    ) {
      return;
    }
    try {
      if (e.currentTarget.hasPointerCapture(e.pointerId)) {
        e.currentTarget.releasePointerCapture(e.pointerId);
      }
    } catch {
      // Ignore release errors
    }
    interactionRef.current = null;
    setActiveResizeHandle(null);
  };

  const handleApplyPreset = (preset: PipPresetKey) => {
    const vp = readViewportSize();
    setPipRect((prev) => applyPipPreset(prev, preset, vp.width, vp.height));
  };

  const handleCyclePreset = () => {
    const vp = readViewportSize();
    setPipRect((prev) => applyPipPreset(prev, getNextPipPreset(prev), vp.width, vp.height));
  };

  // Layout responsiveness inside floating mode
  const isCompactFloating = !isExpanded && pipRect.width < 340;
  const showSegmentedPresets = !isExpanded && pipRect.width >= 370;
  const activePreset = detectActivePipPreset(pipRect);
  const canFitLocalPip = isExpanded || (pipRect.width >= 300 && pipRect.height >= 200);
  const showLocalSelfViewCard = isExpanded || (canFitLocalPip && localPipUserVisible);

  // Proportional local self-view dimensions when floating
  const floatingLocalWidth = Math.round(Math.max(84, Math.min(148, pipRect.width * 0.27)));
  const floatingLocalHeight = Math.round(floatingLocalWidth * 0.68);

  // Smart positioning for devices popover in floating mode so it never clips offscreen
  const viewportSnapshot = readViewportSize();
  const floatingPopoverAbove = pipRect.y >= 320;
  const floatingPopoverAlignRight = pipRect.x + pipRect.width >= viewportSnapshot.width / 2;

  const renderDevicesPopover = (placementClassName: string) => (
    <div
      data-no-drag="true"
      onPointerDown={(e) => e.stopPropagation()}
      className={`${placementClassName} z-50 w-80 max-w-[calc(100vw-2rem)] space-y-2.5 rounded-xl p-3 text-left text-xs shadow-2xl`}
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

      {isConnected && matchedTelemetry && (
        <div
          className="space-y-1 rounded-lg px-2.5 py-2 text-[11px]"
          style={{
            background: usingVirtualAudio ? "rgba(245,158,11,0.12)" : "var(--bg-surface)",
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
            Audio TX: {matchedTelemetry.tx_packets} · RX: {matchedTelemetry.rx_packets}
            {matchedTelemetry.tx_errors > 0 ? ` · TX err: ${matchedTelemetry.tx_errors}` : ""}
          </div>
          <div className="tabular-nums">
            Video TX: {videoTxFrames} · RX: {videoRxFrames}
            {videoDropped > 0 ? ` · drop: ${videoDropped}` : ""}
            {remoteVideoDims && showRemoteVideo
              ? ` · ${remoteVideoDims.width}×${remoteVideoDims.height}`
              : ""}
          </div>
          {usingVirtualAudio && <div>{t("callVirtualAudioWarningShort")}</div>}
          {effectiveDuration >= 4 && matchedTelemetry.rx_packets === 0 && (
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
  );

  return (
    <>
      {/* Hidden bridge video for Always-on-Top OS System Picture-in-Picture */}
      <video
        ref={pipBridgeVideoRef}
        muted
        playsInline
        aria-hidden="true"
        className="pointer-events-none fixed -left-[9999px] h-1 w-1 opacity-0"
      />

      {/* Unified container — never remounts <canvas> or <video> across expanded <-> floating <-> minimized */}
      <div
        role={minimized ? undefined : isExpanded ? "dialog" : "region"}
        aria-modal={minimized ? undefined : isExpanded ? "true" : undefined}
        aria-hidden={minimized ? "true" : undefined}
        aria-label={t("callVideoTitle")}
        inert={minimized || undefined}
        className={
          minimized
            ? "pointer-events-none fixed -left-[9999px] top-0 h-[360px] w-[640px] overflow-hidden opacity-0 select-none"
            : isExpanded
              ? "fixed inset-0 z-[75] flex items-center justify-center bg-black/75 p-2 backdrop-blur-sm sm:p-4"
              : "fixed z-[75] select-none"
        }
        style={
          minimized || isExpanded
            ? undefined
            : {
                left: `${pipRect.x}px`,
                top: `${pipRect.y}px`,
                width: `${pipRect.width}px`,
                height: `${pipRect.height}px`,
              }
        }
      >
        <div
          className={
            isExpanded
              ? "relative flex h-full max-h-[860px] w-full max-w-5xl flex-col overflow-hidden rounded-2xl shadow-2xl"
              : "relative flex h-full w-full flex-col overflow-hidden rounded-2xl shadow-[0_18px_48px_rgba(0,0,0,0.58)]"
          }
          style={{
            background: "#070b12",
            border: isExpanded
              ? "1px solid var(--border-subtle)"
              : activeResizeHandle || isDragging
                ? "1px solid var(--accent)"
                : "1px solid rgba(255, 255, 255, 0.2)",
            color: "#f8fafc",
          }}
        >
          {/* Top info & actions bar (draggable header in floating mode; double-click toggles expanded <-> floating) */}
          <div
            onPointerDown={handleHeaderPointerDown}
            onPointerMove={handleHeaderPointerMove}
            onPointerUp={finishHeaderDrag}
            onPointerCancel={finishHeaderDrag}
            onLostPointerCapture={finishHeaderDrag}
            onDoubleClick={handleHeaderDoubleClick}
            title={isExpanded ? copy.expandedHeaderDoubleClickHint : copy.dragHeaderTooltip}
            className={`pointer-events-auto absolute inset-x-0 top-0 z-20 flex items-center justify-between gap-2 ${
              isExpanded
                ? "flex-wrap px-4 py-3"
                : `touch-none px-2.5 py-1.5 ${isDragging ? "cursor-grabbing" : "cursor-grab"}`
            }`}
            style={{
              background: isExpanded
                ? "linear-gradient(180deg, rgba(7, 11, 18, 0.88) 0%, rgba(7, 11, 18, 0.45) 70%, rgba(7, 11, 18, 0) 100%)"
                : "linear-gradient(180deg, rgba(7, 11, 18, 0.92) 0%, rgba(7, 11, 18, 0.52) 78%, rgba(7, 11, 18, 0) 100%)",
            }}
          >
            <div className="flex min-w-0 flex-1 items-center gap-2 sm:gap-2.5">
              {!isExpanded && (
                <GripHorizontal
                  aria-hidden="true"
                  className="h-3.5 w-3.5 shrink-0 text-slate-400/80"
                />
              )}

              {(isExpanded || pipRect.width >= 320) && (
                <button
                  type="button"
                  data-no-drag="true"
                  onClick={() => {
                    onSelectThread(active.partner_id);
                    if (isExpanded) {
                      changeViewMode("floating");
                    }
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
                    size={isExpanded ? "sm" : "xs"}
                  />
                </button>
              )}

              <div className="min-w-0">
                <div className="flex flex-wrap items-center gap-1.5">
                  <span
                    className={`truncate font-semibold text-white ${
                      isExpanded ? "text-sm" : "text-xs"
                    }`}
                  >
                    {partnerDisplayName}
                  </span>
                  {isExpanded && active.peer_mic_muted && (
                    <span className="inline-flex shrink-0 items-center gap-1 rounded-full bg-white/10 px-2 py-0.5 text-[10.5px] font-medium text-slate-200 backdrop-blur-xs">
                      <MicOff className="h-3 w-3" />
                      <span>{t("callPeerMutedBadge")}</span>
                    </span>
                  )}
                  {isExpanded && isConnected && !active.peer_cam_enabled && (
                    <span className="inline-flex shrink-0 items-center gap-1 rounded-full bg-white/10 px-2 py-0.5 text-[10.5px] font-medium text-slate-200 backdrop-blur-xs">
                      <VideoOff className="h-3 w-3" />
                      <span>{t("callVideoPeerCamOffBadge")}</span>
                    </span>
                  )}
                  {isExpanded && missingMic && (
                    <span className="inline-flex shrink-0 items-center gap-1 rounded-full bg-amber-500/20 px-2 py-0.5 text-[10.5px] font-medium text-amber-300">
                      <AlertTriangle className="h-3 w-3" />
                      <span>{t("callNoMicBadge")}</span>
                    </span>
                  )}
                  {isExpanded && usingVirtualAudio && (
                    <button
                      type="button"
                      data-no-drag="true"
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
                  className={`truncate font-medium tabular-nums ${
                    isExpanded ? "text-xs" : "text-[10.5px]"
                  }`}
                  style={{ color: "var(--accent)" }}
                >
                  {isExpanded
                    ? phaseText
                    : isConnected
                      ? formatCallDuration(effectiveDuration)
                      : phaseText}
                </div>
              </div>
            </div>

            <div data-no-drag="true" className="flex shrink-0 items-center gap-1 sm:gap-1.5">
              {isExpanded && isConnected && matchedTelemetry && (
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

              {!isExpanded &&
                (showSegmentedPresets ? (
                  <div className="inline-flex items-center rounded-lg bg-white/10 p-0.5 backdrop-blur-xs">
                    {PIP_PRESET_KEYS.map((key) => {
                      const isCurrent = activePreset === key;
                      return (
                        <button
                          key={key}
                          type="button"
                          onClick={() => handleApplyPreset(key)}
                          title={copy.presetTooltip[key]}
                          className="rounded-md px-1.5 py-0.5 text-[10px] font-semibold transition"
                          style={{
                            background: isCurrent ? "var(--accent)" : "transparent",
                            color: isCurrent ? "#ffffff" : "#cbd5e1",
                          }}
                        >
                          {PIP_PRESET_LABELS[key]}
                        </button>
                      );
                    })}
                  </div>
                ) : (
                  <button
                    type="button"
                    onClick={handleCyclePreset}
                    title={copy.cycleSizeTooltip}
                    className="inline-flex h-6 min-w-6 items-center justify-center rounded-lg bg-white/10 px-1.5 text-[10px] font-semibold text-white backdrop-blur-xs transition hover:bg-white/20"
                  >
                    {PIP_PRESET_LABELS[activePreset ?? "md"]}
                  </button>
                ))}

              {isExpanded ? (
                <button
                  type="button"
                  onClick={() => changeViewMode("floating")}
                  title={copy.floatBtnTooltip}
                  className="inline-flex items-center gap-1.5 rounded-xl bg-white/10 px-3 py-1.5 text-xs font-semibold text-white backdrop-blur-xs transition hover:bg-white/20"
                >
                  <Minimize2 className="h-3.5 w-3.5" />
                  <span className="hidden sm:inline">{copy.floatBtn}</span>
                </button>
              ) : (
                <button
                  type="button"
                  onClick={() => changeViewMode("expanded")}
                  title={t("callVideoExpandBtn")}
                  className="inline-flex h-6 w-6 items-center justify-center rounded-lg bg-white/10 text-white backdrop-blur-xs transition hover:bg-white/20"
                >
                  <Maximize2 className="h-3.5 w-3.5" />
                </button>
              )}

              <button
                type="button"
                onClick={handleCollapseToBar}
                title={copy.collapseToBarTooltip}
                className={
                  isExpanded
                    ? "inline-flex items-center gap-1.5 rounded-xl bg-white/10 px-2.5 py-1.5 text-xs font-semibold text-white backdrop-blur-xs transition hover:bg-white/20"
                    : "inline-flex h-6 w-6 items-center justify-center rounded-lg bg-white/10 text-white backdrop-blur-xs transition hover:bg-white/20"
                }
              >
                <Minus className="h-3.5 w-3.5" />
                {isExpanded && <span className="hidden md:inline">{t("callVideoMinimizeBtn")}</span>}
              </button>
            </div>
          </div>

          {/* Main video stage — NEVER unmounts <canvas ref={remoteCanvasRef}> or <video ref={localVideoRef}> */}
          <div className="relative flex min-h-0 flex-1 items-center justify-center overflow-hidden bg-[#05080f]">
            <canvas
              ref={remoteCanvasRef}
              className={`h-full w-full object-contain transition-opacity duration-200 ${
                showRemoteVideo ? "opacity-100" : "opacity-0"
              }`}
            />

            {!showRemoteVideo && (
              <div
                className={`absolute inset-0 flex flex-col items-center justify-center text-center ${
                  isExpanded ? "gap-4 px-6" : "gap-2 px-3"
                }`}
              >
                <div
                  aria-hidden="true"
                  className={`pointer-events-none absolute rounded-full opacity-15 blur-3xl ${
                    isExpanded ? "h-64 w-64" : "h-32 w-32"
                  }`}
                  style={{ background: "var(--accent)" }}
                />
                <div
                  className={`relative flex items-center justify-center ${
                    isExpanded ? "h-24 w-24" : "h-12 w-12"
                  }`}
                >
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
                      size={isExpanded ? "lg" : "sm"}
                    />
                  </div>
                </div>
                <div className="relative space-y-1">
                  <h3
                    className={`truncate font-bold text-white ${
                      isExpanded ? "text-lg" : "max-w-[200px] text-xs"
                    }`}
                  >
                    {partnerDisplayName}
                  </h3>
                  {!isConnected ? (
                    <p className={isExpanded ? "text-sm text-slate-300" : "text-[11px] text-slate-300"}>
                      {phaseText}
                    </p>
                  ) : !active.peer_cam_enabled ? (
                    <div
                      className={`inline-flex items-center gap-1.5 rounded-full bg-white/10 font-medium text-slate-200 ${
                        isExpanded ? "px-3.5 py-1.5 text-xs" : "px-2.5 py-1 text-[10.5px]"
                      }`}
                    >
                      <VideoOff className="h-3.5 w-3.5 shrink-0 text-slate-300" />
                      <span className="truncate">{t("callVideoPeerCameraOff")}</span>
                    </div>
                  ) : (
                    <div
                      className={`inline-flex items-center gap-1.5 rounded-full bg-white/10 font-medium text-slate-200 ${
                        isExpanded ? "px-3.5 py-1.5 text-xs" : "px-2.5 py-1 text-[10.5px]"
                      }`}
                    >
                      <span
                        className="h-2 w-2 shrink-0 animate-pulse rounded-full"
                        style={{ background: "var(--accent)" }}
                      />
                      <span className="truncate">{t("callVideoWaitingRemote")}</span>
                    </div>
                  )}
                </div>
              </div>
            )}

            {/* System Picture-in-Picture active indicator */}
            {systemPipActive && (
              <div
                className={`pointer-events-none absolute left-1/2 z-20 -translate-x-1/2 rounded-full border border-emerald-400/35 bg-emerald-950/85 px-3 py-1 text-emerald-200 shadow-lg backdrop-blur-md ${
                  isExpanded ? "top-16 text-xs" : "top-10 text-[10.5px]"
                }`}
              >
                <span className="inline-flex items-center gap-1.5 font-medium whitespace-nowrap">
                  <PictureInPicture2 className="h-3.5 w-3.5 shrink-0 text-emerald-400" />
                  <span>{copy.systemPipBanner}</span>
                </span>
              </div>
            )}

            {/* Non-blocking System PiP fallback notice */}
            {systemPipNotice && (
              <div
                role="status"
                className={`pointer-events-none absolute left-1/2 z-20 max-w-[90%] -translate-x-1/2 rounded-xl border border-amber-400/35 bg-slate-950/92 px-3 py-1.5 text-center text-amber-200 shadow-xl backdrop-blur-md ${
                  isExpanded ? "bottom-20 text-xs" : "bottom-12 text-[10.5px]"
                }`}
              >
                {systemPipNotice}
              </div>
            )}

            {/* Live dimensions badge while resizing floating PiP */}
            {!isExpanded && activeResizeHandle && (
              <div className="pointer-events-none absolute left-1/2 top-1/2 z-20 -translate-x-1/2 -translate-y-1/2 rounded-lg border border-white/15 bg-black/75 px-2.5 py-1 text-[11px] font-semibold text-white tabular-nums backdrop-blur-xs">
                {pipRect.width} × {pipRect.height}
              </div>
            )}

            {/* Local self-view PiP card — ALWAYS mounted so <video ref={localVideoRef}> never unmounts */}
            <div
              title={localCameraError ?? undefined}
              className={`group absolute z-20 overflow-hidden rounded-xl border border-white/15 bg-[#0f172a]/95 shadow-2xl transition-opacity duration-150 ${
                isExpanded
                  ? "bottom-24 right-3 h-28 w-40 sm:bottom-24 sm:right-5 sm:h-36 sm:w-52"
                  : "bottom-12 right-2"
              } ${showLocalSelfViewCard ? "opacity-100" : "pointer-events-none invisible opacity-0"}`}
              style={
                isExpanded
                  ? undefined
                  : {
                      width: `${floatingLocalWidth}px`,
                      height: `${floatingLocalHeight}px`,
                    }
              }
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
                <div className="absolute inset-0 flex flex-col items-center justify-center gap-1 px-2 text-center">
                  {cameraStarting ? (
                    <RefreshCw
                      className={`${isExpanded ? "h-5 w-5" : "h-3.5 w-3.5"} animate-spin`}
                      style={{ color: "var(--accent)" }}
                    />
                  ) : cameraPendingConnect ? (
                    <Video
                      className={`${isExpanded ? "h-5 w-5" : "h-3.5 w-3.5"} animate-pulse text-slate-300`}
                    />
                  ) : cameraHasIssue ? (
                    <AlertTriangle
                      className={`${isExpanded ? "h-5 w-5" : "h-3.5 w-3.5"} text-amber-400`}
                    />
                  ) : (
                    <VideoOff
                      className={`${isExpanded ? "h-5 w-5" : "h-3.5 w-3.5"} text-slate-400`}
                    />
                  )}
                  <span
                    className={`font-medium leading-tight ${
                      isExpanded ? "text-[11px]" : "text-[9.5px]"
                    } ${cameraHasIssue ? "text-amber-300" : "text-slate-300"}`}
                  >
                    {localPipStatusText}
                  </span>
                </div>
              )}
              <div className="pointer-events-none absolute bottom-1 left-1 inline-flex items-center gap-1 rounded-md bg-black/60 px-1.5 py-0.5 text-[9.5px] font-medium text-white backdrop-blur-xs">
                <span>{t("callVideoYouLabel")}</span>
                {effectiveMuted && <MicOff className="h-2.5 w-2.5 text-rose-400" />}
              </div>
              {!isExpanded && (
                <button
                  type="button"
                  data-no-drag="true"
                  onClick={() => setLocalPipUserVisible(false)}
                  title={copy.hideSelfViewTooltip}
                  className="absolute right-1 top-1 inline-flex h-5 w-5 items-center justify-center rounded-md bg-black/65 text-slate-200 opacity-0 transition hover:bg-black/85 group-hover:opacity-100"
                >
                  <EyeOff className="h-3 w-3" />
                </button>
              )}
            </div>

            {/* Restore self-view pill when user hid it in floating mode */}
            {!isExpanded && canFitLocalPip && !localPipUserVisible && (
              <button
                type="button"
                data-no-drag="true"
                onClick={() => setLocalPipUserVisible(true)}
                title={copy.showSelfViewTooltip}
                className="absolute bottom-12 right-2 z-20 inline-flex items-center gap-1 rounded-lg border border-white/15 bg-black/65 px-2 py-1 text-[10px] font-medium text-slate-200 backdrop-blur-xs transition hover:bg-black/85"
              >
                <Eye className="h-3 w-3" />
                <span>{t("callVideoYouLabel")}</span>
              </button>
            )}

            {/* Subtle visual resize corner grip inside bottom-right of floating card */}
            {!isExpanded && (
              <svg
                aria-hidden="true"
                viewBox="0 0 12 12"
                className="pointer-events-none absolute bottom-1 right-1 z-20 h-3 w-3 text-white/45"
              >
                <path
                  d="M11 3L3 11M11 7L7 11"
                  stroke="currentColor"
                  strokeWidth="1.5"
                  strokeLinecap="round"
                />
              </svg>
            )}
          </div>

          {/* Bottom floating control bar */}
          <div
            className={`pointer-events-none absolute inset-x-0 z-20 flex justify-center ${
              isExpanded ? "bottom-4 px-4" : "bottom-2 px-2"
            }`}
          >
            <div
              data-no-drag="true"
              className={`pointer-events-auto relative flex items-center rounded-2xl border border-white/15 bg-[#0f172a]/90 shadow-2xl backdrop-blur-md ${
                isExpanded
                  ? "gap-2 px-3.5 py-2 sm:gap-2.5"
                  : isCompactFloating
                    ? "gap-1.5 px-2 py-1"
                    : "gap-2 px-2.5 py-1.5"
              }`}
            >
              {/* Mute / Unmute */}
              <button
                type="button"
                disabled={busy || missingMic}
                onClick={() => onToggleMute(!effectiveMuted)}
                title={effectiveMuted ? t("callUnmuteBtn") : t("callMuteBtn")}
                className={`inline-flex items-center justify-center rounded-xl text-xs font-semibold transition disabled:opacity-45 ${
                  isExpanded
                    ? "gap-1.5 px-3 py-2"
                    : isCompactFloating
                      ? "h-7 w-7"
                      : "h-8 w-8"
                }`}
                style={{
                  background: effectiveMuted ? "var(--danger-bg)" : "rgba(255, 255, 255, 0.1)",
                  color: effectiveMuted ? "var(--danger-text)" : "#f8fafc",
                }}
              >
                {effectiveMuted ? (
                  <MicOff className={isExpanded ? "h-4 w-4" : "h-3.5 w-3.5"} />
                ) : (
                  <Mic className={isExpanded ? "h-4 w-4" : "h-3.5 w-3.5"} />
                )}
                {isExpanded && (
                  <span className="hidden sm:inline">
                    {effectiveMuted ? t("callUnmuteBtn") : t("callMuteBtn")}
                  </span>
                )}
              </button>

              {/* Camera On / Off */}
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
                className={`relative inline-flex items-center justify-center rounded-xl text-xs font-semibold transition disabled:opacity-45 ${
                  isExpanded
                    ? "gap-1.5 px-3 py-2"
                    : isCompactFloating
                      ? "h-7 w-7"
                      : "h-8 w-8"
                }`}
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
                  <Video className={isExpanded ? "h-4 w-4" : "h-3.5 w-3.5"} />
                ) : (
                  <VideoOff className={isExpanded ? "h-4 w-4" : "h-3.5 w-3.5"} />
                )}
                {isExpanded && (
                  <span className="hidden sm:inline">
                    {effectiveCamEnabled
                      ? t("callVideoCameraOffBtn")
                      : cameraHasIssue
                        ? t("callVideoCameraRetryBtn")
                        : t("callVideoCameraOnBtn")}
                  </span>
                )}
                {cameraHasIssue && (
                  <span
                    aria-hidden="true"
                    className="absolute -right-1 -top-1 h-2.5 w-2.5 rounded-full bg-amber-400 ring-2 ring-[#0f172a]"
                  />
                )}
              </button>

              {/* Always-on-Top System Picture-in-Picture (works across all Windows apps) */}
              <button
                type="button"
                onClick={() => void handleToggleSystemPip()}
                title={systemPipActive ? copy.systemPipActiveBtn : copy.systemPipBtn}
                aria-pressed={systemPipActive}
                className={`inline-flex items-center justify-center rounded-xl text-xs font-semibold transition ${
                  isExpanded
                    ? "gap-1.5 px-3 py-2"
                    : isCompactFloating
                      ? "h-7 w-7"
                      : "h-8 w-8"
                }`}
                style={{
                  background: systemPipActive
                    ? "var(--accent-soft)"
                    : "rgba(255, 255, 255, 0.1)",
                  color: systemPipActive ? "var(--accent)" : "#f8fafc",
                }}
              >
                <PictureInPicture2 className={isExpanded ? "h-4 w-4" : "h-3.5 w-3.5"} />
                {isExpanded && (
                  <span className="hidden md:inline">
                    {systemPipActive ? copy.systemPipActiveBtn : copy.systemPipBtn}
                  </span>
                )}
              </button>

              {/* Devices popover toggle */}
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
                className={`inline-flex items-center justify-center rounded-xl text-xs font-semibold transition ${
                  isExpanded
                    ? "gap-1.5 px-3 py-2"
                    : isCompactFloating
                      ? "h-7 w-7"
                      : "h-8 w-8"
                }`}
                style={{
                  background: devicesOpen ? "var(--accent-soft)" : "rgba(255, 255, 255, 0.1)",
                  color: devicesOpen ? "var(--accent)" : "#e2e8f0",
                }}
              >
                <SlidersHorizontal className={isExpanded ? "h-4 w-4" : "h-3.5 w-3.5"} />
              </button>

              {/* Hangup / Cancel */}
              <button
                type="button"
                disabled={effectiveHangupBusy}
                onClick={onHangup}
                title={isCancellingStage ? t("callCancelBtn") : t("callHangupBtn")}
                className={`inline-flex items-center justify-center rounded-xl bg-rose-600 text-xs font-semibold text-white shadow-lg transition hover:bg-rose-500 disabled:opacity-50 ${
                  isExpanded
                    ? "gap-1.5 px-4 py-2"
                    : isCompactFloating
                      ? "h-7 px-2"
                      : "h-8 px-2.5"
                }`}
              >
                <PhoneOff className={isExpanded ? "h-4 w-4" : "h-3.5 w-3.5"} />
                {isExpanded && (
                  <span>{isCancellingStage ? t("callCancelBtn") : t("callHangupBtn")}</span>
                )}
              </button>

              {/* Devices popover inside bottom bar in expanded mode */}
              {isExpanded &&
                devicesOpen &&
                renderDevicesPopover("absolute bottom-full right-0 mb-3")}
            </div>
          </div>
        </div>

        {/* Devices popover outside inner card's overflow-hidden in floating mode so it never clips */}
        {!isExpanded &&
          devicesOpen &&
          renderDevicesPopover(
            `absolute ${floatingPopoverAbove ? "bottom-full mb-2" : "top-full mt-2"} ${
              floatingPopoverAlignRight ? "right-0" : "left-0"
            }`,
          )}

        {/* 8 interactive edge & corner resize handles (floating mode only) */}
        {!isExpanded &&
          RESIZE_HANDLES.map(({ handle, className }) => (
            <div
              key={handle}
              role="presentation"
              data-resize-handle={handle}
              title={handle === "se" ? copy.resizeHandleTooltip : undefined}
              onPointerDown={handleResizePointerDown(handle)}
              onPointerMove={handleResizePointerMove}
              onPointerUp={finishResize}
              onPointerCancel={finishResize}
              onLostPointerCapture={finishResize}
              className={`absolute touch-none ${className}`}
            />
          ))}
      </div>
    </>
  );
}
