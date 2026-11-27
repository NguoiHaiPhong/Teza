import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  AlertTriangle,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  Clock,
  Cloud,
  Database,
  ExternalLink,
  Eye,
  EyeOff,
  FileText,
  Film,
  History,
  Image,
  KeyRound,
  Loader2,
  Lock,
  LockOpen,
  LogOut,
  Mic,
  RefreshCw,
  Shield,
  Sparkles,
  Trash2,
  X,
} from "lucide-react";
import { bridge } from "../bridge";
import { useI18n } from "../i18n";
import type {
  GDriveCloudItemDto,
  GDriveConfigDto,
  GDriveConfigPatch,
  GDrivePruneResultDto,
  GDriveStatusDto,
  GDriveStorageOverviewDto,
} from "../types";

export interface CloudStoragePanelProps {
  relayEnabled: boolean;
  onOpenNetworkTab: () => void;
  onLockVault?: () => Promise<void>;
}

type ManagerSubTab = "by_thread" | "largest_items" | "prune_age";

type ConfirmDeleteAction =
  | {
      kind: "thread";
      threadId: string;
      threadName: string;
      count: number;
      bytes: number;
    }
  | {
      kind: "items";
      sha256s: string[];
      totalBytes: number;
    }
  | {
      kind: "age";
      days: number;
    };

const PRUNE_AGE_PRESETS = [30, 90, 180, 365] as const;

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  if (bytes < 1024) return `${Math.round(bytes)} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

function normalizeEpochMs(ts: number): number {
  return ts > 0 && ts < 1_000_000_000_000 ? ts * 1000 : ts;
}

function formatDateTime(ts: number, locale: string): string {
  if (!Number.isFinite(ts) || ts <= 0) return "";
  try {
    return new Date(normalizeEpochMs(ts)).toLocaleString(locale, {
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    return new Date(normalizeEpochMs(ts)).toISOString();
  }
}

function formatShortDate(ts: number, locale: string): string {
  if (!Number.isFinite(ts) || ts <= 0) return "";
  try {
    return new Date(normalizeEpochMs(ts)).toLocaleDateString(locale, {
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
    });
  } catch {
    return "";
  }
}

function GoogleLogoIcon({ className = "h-4 w-4" }: { className?: string }) {
  return (
    <svg viewBox="0 0 24 24" className={className} aria-hidden="true">
      <path
        fill="#4285F4"
        d="M23.49 12.27c0-.79-.07-1.54-.2-2.27H12v4.3h6.44a5.5 5.5 0 0 1-2.39 3.61v3h3.87c2.26-2.08 3.57-5.15 3.57-8.64Z"
      />
      <path
        fill="#34A853"
        d="M12 24c3.24 0 5.95-1.07 7.93-2.91l-3.87-3a7.17 7.17 0 0 1-10.67-3.76H1.39v3.1A12 12 0 0 0 12 24Z"
      />
      <path
        fill="#FBBC05"
        d="M5.39 14.33a7.2 7.2 0 0 1 0-4.66V6.57H1.39a12 12 0 0 0 0 10.86l4-3.1Z"
      />
      <path
        fill="#EA4335"
        d="M12 4.75c1.76 0 3.34.61 4.59 1.8l3.44-3.44C17.94 1.19 15.23 0 12 0A12 12 0 0 0 1.39 6.57l4 3.1A7.15 7.15 0 0 1 12 4.75Z"
      />
    </svg>
  );
}

function renderMediaKindIcon(kind: string) {
  const lower = kind.toLowerCase();
  if (lower === "image" || lower === "photo" || lower === "gif" || lower === "sticker") {
    return <Image className="h-3.5 w-3.5 shrink-0" />;
  }
  if (lower === "video") {
    return <Film className="h-3.5 w-3.5 shrink-0" />;
  }
  if (lower === "voice" || lower === "audio") {
    return <Mic className="h-3.5 w-3.5 shrink-0" />;
  }
  return <FileText className="h-3.5 w-3.5 shrink-0" />;
}

function CloudToggleRow({
  icon,
  label,
  checked,
  disabled,
  onChange,
}: {
  icon: React.ReactNode;
  label: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (next: boolean) => void;
}) {
  return (
    <div className="flex items-center justify-between gap-3 rounded-xl px-3 py-2 transition hover:bg-black/10">
      <label
        className={`flex min-w-0 flex-1 items-center gap-2.5 ${
          disabled ? "cursor-not-allowed opacity-60" : "cursor-pointer"
        }`}
      >
        <span className="shrink-0" style={{ color: "var(--accent)" }}>
          {icon}
        </span>
        <span className="min-w-0 flex-1 text-[13px] font-medium">{label}</span>
        <input
          type="checkbox"
          checked={checked}
          disabled={disabled}
          onChange={(e) => onChange(e.target.checked)}
          className="h-4 w-4 shrink-0 cursor-pointer accent-[var(--accent)] disabled:cursor-not-allowed"
        />
      </label>
    </div>
  );
}

export function CloudStoragePanel({
  relayEnabled,
  onOpenNetworkTab,
  onLockVault,
}: CloudStoragePanelProps) {
  const { locale, t } = useI18n();

  const [config, setConfig] = useState<GDriveConfigDto | null>(null);
  const [status, setStatus] = useState<GDriveStatusDto | null>(null);
  const [overview, setOverview] = useState<GDriveStorageOverviewDto | null>(null);

  const [loadingInit, setLoadingInit] = useState(false);
  const [loadingOverview, setLoadingOverview] = useState(false);
  const [savingOAuth, setSavingOAuth] = useState(false);
  const [savingToggle, setSavingToggle] = useState(false);
  const [connecting, setConnecting] = useState(false);
  const [cancellingConnect, setCancellingConnect] = useState(false);
  const [disconnecting, setDisconnecting] = useState(false);
  const [confirmDisconnect, setConfirmDisconnect] = useState(false);
  const [lockingVault, setLockingVault] = useState(false);
  const [unlocking, setUnlocking] = useState(false);
  const [syncingNow, setSyncingNow] = useState(false);
  const [pruning, setPruning] = useState(false);

  const [oauthDrawerOpen, setOauthDrawerOpen] = useState(false);
  const [oauthClientId, setOauthClientId] = useState("");
  const [oauthClientSecret, setOauthClientSecret] = useState("");
  const [oauthBrokerUrl, setOauthBrokerUrl] = useState("");

  const [passphrase, setPassphrase] = useState("");
  const [passphraseConfirm, setPassphraseConfirm] = useState("");
  const [showPassphrase, setShowPassphrase] = useState(false);
  const [passphraseError, setPassphraseError] = useState<string | null>(null);

  const [managerTab, setManagerTab] = useState<ManagerSubTab>("by_thread");
  const [selectedShas, setSelectedShas] = useState<Set<string>>(() => new Set());
  const [pruneDays, setPruneDays] = useState<number>(90);
  const [confirmAction, setConfirmAction] = useState<ConfirmDeleteAction | null>(null);

  const [feedback, setFeedback] = useState<{
    tone: "ok" | "warn" | "error";
    text: string;
  } | null>(null);

  const mountedRef = useRef(true);
  const relayEnabledRef = useRef(relayEnabled);
  relayEnabledRef.current = relayEnabled;
  const initSeqRef = useRef(0);
  const overviewSeqRef = useRef(0);
  const oauthDrawerTouchedRef = useRef(false);
  const prevSyncingRef = useRef(false);
  const confirmBoxRef = useRef<HTMLDivElement | null>(null);
  const confirmDisconnectRef = useRef<HTMLDivElement | null>(null);

  const isRelayBlocked = relayEnabled || Boolean(status?.relay_blocked);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    if (confirmAction) {
      confirmBoxRef.current?.scrollIntoView({ behavior: "smooth", block: "nearest" });
    }
  }, [confirmAction]);

  useEffect(() => {
    if (confirmDisconnect) {
      confirmDisconnectRef.current?.scrollIntoView({ behavior: "smooth", block: "nearest" });
    }
  }, [confirmDisconnect]);

  useEffect(() => {
    if (!status?.connected) {
      setConfirmDisconnect(false);
    }
  }, [status?.connected]);

  const loadOverview = useCallback(async () => {
    if (relayEnabledRef.current) return;
    const seq = ++overviewSeqRef.current;
    setLoadingOverview(true);
    try {
      const data = await bridge.gdriveGetStorageOverview(50);
      if (!mountedRef.current || relayEnabledRef.current || seq !== overviewSeqRef.current) return;
      setOverview(data);
      const validShas = new Set(data.largest_items.map((it) => it.sha256));
      setSelectedShas((prev) => {
        if (prev.size === 0) return prev;
        const next = new Set<string>();
        for (const sha of prev) {
          if (validShas.has(sha)) next.add(sha);
        }
        return next;
      });
    } catch (err) {
      if (!mountedRef.current || relayEnabledRef.current || seq !== overviewSeqRef.current) return;
      setFeedback({
        tone: "error",
        text: err instanceof Error ? err.message : String(err),
      });
    } finally {
      if (mountedRef.current && seq === overviewSeqRef.current) {
        setLoadingOverview(false);
      }
    }
  }, []);

  // Initial load & strict Relay-exclusion guard: NEVER fire Drive IPC calls while relayEnabled is true
  useEffect(() => {
    if (relayEnabled) {
      initSeqRef.current += 1;
      overviewSeqRef.current += 1;
      setLoadingInit(false);
      setLoadingOverview(false);
      setStatus(null);
      setOverview(null);
      setSelectedShas(new Set());
      setConfirmAction(null);
      setConfirmDisconnect(false);
      setPassphrase("");
      setPassphraseConfirm("");
      setPassphraseError(null);
      setFeedback(null);
      return;
    }

    let cancelled = false;
    const seq = ++initSeqRef.current;
    setLoadingInit(true);
    setFeedback(null);

    void Promise.all([bridge.gdriveGetConfig(), bridge.gdriveGetStatus()])
      .then(([cfg, st]) => {
        if (cancelled || !mountedRef.current || seq !== initSeqRef.current) return;
        setConfig(cfg);
        setStatus(st);
        setOauthClientId(cfg.client_id);
        setOauthClientSecret("");
        setOauthBrokerUrl(cfg.token_broker_url ?? "");
        if (!oauthDrawerTouchedRef.current) {
          setOauthDrawerOpen(!st.oauth_configured);
        }
        prevSyncingRef.current = st.syncing;
      })
      .catch((err) => {
        if (cancelled || !mountedRef.current || seq !== initSeqRef.current) return;
        setFeedback({
          tone: "error",
          text: err instanceof Error ? err.message : String(err),
        });
      })
      .finally(() => {
        if (!cancelled && mountedRef.current && seq === initSeqRef.current) {
          setLoadingInit(false);
        }
      });

    return () => {
      cancelled = true;
    };
  }, [relayEnabled]);

  // Subscribe to real-time sync progress events (only when Relay is disabled)
  useEffect(() => {
    if (relayEnabled) return;
    let active = true;
    let unlistenFn: (() => void) | null = null;

    void bridge
      .onGDriveSyncStatus((next) => {
        if (!active || !mountedRef.current) return;
        const wasSyncing = prevSyncingRef.current;
        prevSyncingRef.current = next.syncing;
        setStatus(next);
        setConfig((prev) =>
          prev
            ? {
                ...prev,
                sync_enabled: next.sync_enabled,
                sync_media: next.sync_media,
                sync_history: next.sync_history,
              }
            : prev,
        );
        if (
          wasSyncing &&
          !next.syncing &&
          !next.last_error &&
          next.connected &&
          next.vault_unlocked &&
          !next.relay_blocked
        ) {
          void loadOverview();
        }
      })
      .then((unlisten) => {
        if (!active) {
          unlisten();
        } else {
          unlistenFn = unlisten;
        }
      })
      .catch(() => {
        // Ignore event subscription errors in non-Tauri preview
      });

    return () => {
      active = false;
      if (unlistenFn) {
        unlistenFn();
      }
    };
  }, [relayEnabled, loadOverview]);

  // Auto-load Cloud Storage overview once connected & unlocked (and never when Relay is blocked)
  const canManageCloud =
    !isRelayBlocked && Boolean(status?.connected) && Boolean(status?.vault_unlocked);

  useEffect(() => {
    if (!canManageCloud) {
      setOverview(null);
      setSelectedShas(new Set());
      setConfirmAction(null);
      return;
    }
    void loadOverview();
  }, [canManageCloud, loadOverview]);

  const sortedThreads = useMemo(() => {
    if (!overview) return [];
    return [...overview.by_thread].sort((a, b) => b.total_bytes - a.total_bytes);
  }, [overview]);

  const largestItems = useMemo(() => {
    if (!overview) return [];
    return [...overview.largest_items].sort((a, b) => b.size_bytes - a.size_bytes);
  }, [overview]);

  const getMediaKindLabel = (kind: string): string => {
    const lower = kind.toLowerCase();
    if (lower === "image" || lower === "photo" || lower === "gif" || lower === "sticker") {
      return t("storageBreakdownImages");
    }
    if (lower === "video") {
      return t("storageBreakdownVideos");
    }
    if (lower === "voice" || lower === "audio") {
      return t("storageBreakdownVoice");
    }
    return t("storageBreakdownFiles");
  };

  const getPhaseLabel = (phase: string): string => {
    switch (phase) {
      case "idle":
        return t("gdrivePhaseIdle");
      case "connecting":
        return t("gdrivePhaseConnecting");
      case "uploading_media":
        return t("gdrivePhaseUploadingMedia");
      case "syncing_history":
        return t("gdrivePhaseSyncingHistory");
      case "rehydrating":
        return t("gdrivePhaseRehydrating");
      case "pruning":
        return t("gdrivePhasePruning");
      case "paused_relay":
        return t("gdrivePhasePausedRelay");
      case "error":
        return t("gdrivePhaseError");
      default:
        return phase || t("gdrivePhaseIdle");
    }
  };

  const handleSaveOAuth = async (e: React.FormEvent) => {
    e.preventDefault();
    if (isRelayBlocked || savingOAuth) return;
    setSavingOAuth(true);
    setFeedback(null);
    try {
      const patch: GDriveConfigPatch = {
        clientId: oauthClientId.trim(),
        tokenBrokerUrl: oauthBrokerUrl.trim(),
      };
      const trimmedSecret = oauthClientSecret.trim();
      if (trimmedSecret.length > 0) {
        patch.clientSecret = trimmedSecret;
      }
      const savedCfg = await bridge.gdriveSaveConfig(patch);
      if (!mountedRef.current) return;
      setConfig(savedCfg);
      setOauthClientId(savedCfg.client_id);
      setOauthClientSecret("");
      setOauthBrokerUrl(savedCfg.token_broker_url ?? "");
      const nextStatus = await bridge.gdriveGetStatus();
      if (!mountedRef.current) return;
      setStatus(nextStatus);
      if (nextStatus.oauth_configured) {
        setOauthDrawerOpen(false);
      }
      setFeedback({ tone: "ok", text: t("gdriveOAuthSavedNotice") });
    } catch (err) {
      if (!mountedRef.current) return;
      setFeedback({
        tone: "error",
        text: err instanceof Error ? err.message : String(err),
      });
    } finally {
      if (mountedRef.current) setSavingOAuth(false);
    }
  };

  const handleResetOAuthDefault = async () => {
    if (isRelayBlocked || savingOAuth) return;
    setSavingOAuth(true);
    setFeedback(null);
    try {
      const savedCfg = await bridge.gdriveSaveConfig({ resetCustomOAuth: true });
      if (!mountedRef.current) return;
      setConfig(savedCfg);
      setOauthClientId(savedCfg.client_id);
      setOauthClientSecret("");
      setOauthBrokerUrl(savedCfg.token_broker_url ?? "");
      const nextStatus = await bridge.gdriveGetStatus();
      if (!mountedRef.current) return;
      setStatus(nextStatus);
      if (nextStatus.oauth_configured) {
        setOauthDrawerOpen(false);
      }
      setFeedback({ tone: "ok", text: t("gdriveResetOAuthNotice") });
    } catch (err) {
      if (!mountedRef.current) return;
      setFeedback({
        tone: "error",
        text: err instanceof Error ? err.message : String(err),
      });
    } finally {
      if (mountedRef.current) setSavingOAuth(false);
    }
  };

  const handleConnect = async () => {
    if (isRelayBlocked || connecting) return;
    setConnecting(true);
    setFeedback(null);
    try {
      const nextStatus = await bridge.gdriveConnectStart();
      if (!mountedRef.current) return;
      setStatus(nextStatus);
      setPassphraseError(null);
    } catch (err) {
      if (!mountedRef.current) return;
      const msg = err instanceof Error ? err.message : String(err);
      if (!msg.toLowerCase().includes("cancelled") && !msg.toLowerCase().includes("canceled")) {
        setFeedback({
          tone: "error",
          text: msg,
        });
      }
    } finally {
      if (mountedRef.current) setConnecting(false);
    }
  };

  const handleCancelConnect = async () => {
    if (cancellingConnect) return;
    setCancellingConnect(true);
    try {
      const nextStatus = await bridge.gdriveConnectCancel();
      if (!mountedRef.current) return;
      setStatus(nextStatus);
      setConnecting(false);
    } catch {
      // ignore cancel errors
    } finally {
      if (mountedRef.current) setCancellingConnect(false);
    }
  };

  const handleDisconnect = async () => {
    if (isRelayBlocked || disconnecting || pruning || lockingVault || syncingNow || status?.syncing) {
      return;
    }
    setDisconnecting(true);
    setFeedback(null);
    setConfirmAction(null);
    setConfirmDisconnect(false);
    try {
      const nextStatus = await bridge.gdriveDisconnect();
      if (!mountedRef.current) return;
      setStatus(nextStatus);
      setOverview(null);
      setSelectedShas(new Set());
      setPassphrase("");
      setPassphraseConfirm("");
      setPassphraseError(null);
    } catch (err) {
      if (!mountedRef.current) return;
      setFeedback({
        tone: "error",
        text: err instanceof Error ? err.message : String(err),
      });
    } finally {
      if (mountedRef.current) setDisconnecting(false);
    }
  };

  const handleLockVault = async () => {
    if (
      !onLockVault ||
      isRelayBlocked ||
      lockingVault ||
      disconnecting ||
      pruning ||
      syncingNow ||
      status?.syncing
    ) {
      return;
    }
    setLockingVault(true);
    setFeedback(null);
    setConfirmAction(null);
    setConfirmDisconnect(false);
    try {
      await onLockVault();
      if (!mountedRef.current) return;
      const nextStatus = await bridge.gdriveGetStatus();
      if (!mountedRef.current) return;
      setStatus(nextStatus);
      if (!nextStatus.vault_unlocked) {
        setOverview(null);
        setSelectedShas(new Set());
        setFeedback({ tone: "ok", text: t("gdriveVaultLockedNotice") });
      }
    } catch (err) {
      if (!mountedRef.current) return;
      setFeedback({
        tone: "error",
        text: err instanceof Error ? err.message : String(err),
      });
    } finally {
      if (mountedRef.current) setLockingVault(false);
    }
  };

  const handleUnlockVault = async (e: React.FormEvent) => {
    e.preventDefault();
    if (isRelayBlocked || unlocking || !status?.connected) return;
    setPassphraseError(null);
    setFeedback(null);

    if (passphrase.trim().length < 8) {
      setPassphraseError(t("gdrivePassphraseMinError"));
      return;
    }
    if (!status.has_remote_manifest && passphrase !== passphraseConfirm) {
      setPassphraseError(t("gdrivePassphraseMismatchError"));
      return;
    }

    setUnlocking(true);
    try {
      const nextStatus = await bridge.gdriveUnlockVault(passphrase);
      if (!mountedRef.current) return;
      setStatus(nextStatus);
      setPassphrase("");
      setPassphraseConfirm("");
      setPassphraseError(null);
    } catch (err) {
      if (!mountedRef.current) return;
      setPassphraseError(err instanceof Error ? err.message : String(err));
    } finally {
      if (mountedRef.current) setUnlocking(false);
    }
  };

  const handleToggleSyncSetting = async (patch: GDriveConfigPatch) => {
    if (isRelayBlocked || savingToggle) return;
    setSavingToggle(true);
    setFeedback(null);
    try {
      const savedCfg = await bridge.gdriveSaveConfig(patch);
      if (!mountedRef.current) return;
      setConfig(savedCfg);
      setStatus((prev) =>
        prev
          ? {
              ...prev,
              sync_enabled: savedCfg.sync_enabled,
              sync_media: savedCfg.sync_media,
              sync_history: savedCfg.sync_history,
            }
          : prev,
      );
    } catch (err) {
      if (!mountedRef.current) return;
      setFeedback({
        tone: "error",
        text: err instanceof Error ? err.message : String(err),
      });
    } finally {
      if (mountedRef.current) setSavingToggle(false);
    }
  };

  const handleSyncNow = async () => {
    if (isRelayBlocked || syncingNow || status?.syncing) return;
    setSyncingNow(true);
    setFeedback(null);
    try {
      const nextStatus = await bridge.gdriveSyncNow();
      if (!mountedRef.current) return;
      setStatus(nextStatus);
      if (!nextStatus.relay_blocked && nextStatus.connected && nextStatus.vault_unlocked) {
        await loadOverview();
      }
    } catch (err) {
      if (!mountedRef.current) return;
      setFeedback({
        tone: "error",
        text: err instanceof Error ? err.message : String(err),
      });
    } finally {
      if (mountedRef.current) setSyncingNow(false);
    }
  };

  const validRootFolderId = useMemo(() => {
    const raw = status?.root_folder_id?.trim() ?? "";
    return /^[A-Za-z0-9_-]+$/.test(raw) ? raw : null;
  }, [status?.root_folder_id]);

  const handleOpenVaultFolder = async () => {
    if (!validRootFolderId) return;
    setFeedback(null);
    try {
      await bridge.openExternalUrl(
        `https://drive.google.com/drive/folders/${encodeURIComponent(validRootFolderId)}`,
      );
    } catch (err) {
      if (!mountedRef.current) return;
      setFeedback({
        tone: "error",
        text: err instanceof Error ? err.message : String(err),
      });
    }
  };

  const applyPruneResult = async (res: GDrivePruneResultDto) => {
    if (!mountedRef.current) return;
    if (res.failed_count > 0) {
      setFeedback({
        tone: "warn",
        text: t("gdrivePrunePartialFailed", {
          deleted: res.deleted_count,
          size: formatBytes(res.freed_bytes),
          failed: res.failed_count,
        }),
      });
    } else {
      setFeedback({
        tone: "ok",
        text: t("gdrivePruneSuccess", {
          count: res.deleted_count,
          size: formatBytes(res.freed_bytes),
        }),
      });
    }
    setConfirmAction(null);
    setSelectedShas(new Set());
    try {
      const [nextStatus] = await Promise.all([bridge.gdriveGetStatus(), loadOverview()]);
      if (mountedRef.current) {
        setStatus(nextStatus);
      }
    } catch {
      // loadOverview handles its own error feedback
    }
  };

  const handleExecuteConfirmDelete = async () => {
    if (isRelayBlocked || pruning || !confirmAction) return;
    setPruning(true);
    setFeedback(null);
    try {
      let res: GDrivePruneResultDto;
      if (confirmAction.kind === "thread") {
        res = await bridge.gdriveDeleteCloudByThread(confirmAction.threadId);
      } else if (confirmAction.kind === "items") {
        res = await bridge.gdriveDeleteCloudItems(confirmAction.sha256s);
      } else {
        res = await bridge.gdrivePruneCloudOlderThan(confirmAction.days);
      }
      await applyPruneResult(res);
    } catch (err) {
      if (!mountedRef.current) return;
      setFeedback({
        tone: "error",
        text: err instanceof Error ? err.message : String(err),
      });
      setConfirmAction(null);
    } finally {
      if (mountedRef.current) setPruning(false);
    }
  };

  const toggleItemSelection = (sha256: string) => {
    setSelectedShas((prev) => {
      const next = new Set(prev);
      if (next.has(sha256)) {
        next.delete(sha256);
      } else {
        next.add(sha256);
      }
      return next;
    });
  };

  const handleSelectAllItems = () => {
    setSelectedShas(new Set(largestItems.map((it) => it.sha256)));
  };

  const handleClearSelection = () => {
    setSelectedShas(new Set());
  };

  const selectedBytes = useMemo(() => {
    if (selectedShas.size === 0) return 0;
    let sum = 0;
    for (const item of largestItems) {
      if (selectedShas.has(item.sha256)) {
        sum += item.size_bytes;
      }
    }
    return sum;
  }, [largestItems, selectedShas]);

  // 1. Strict Relay Exclusion State
  if (isRelayBlocked) {
    return (
      <div
        className="flex flex-wrap items-center justify-between gap-3 rounded-xl px-3.5 py-3 text-xs"
        style={{
          background: "var(--danger-bg)",
          color: "var(--danger-text)",
        }}
      >
        <div className="flex min-w-0 flex-1 items-start gap-2">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
          <span className="font-medium leading-relaxed">{t("storageCloudRelayBlocked")}</span>
        </div>
        <button
          type="button"
          onClick={onOpenNetworkTab}
          className="shrink-0 rounded-lg px-2.5 py-1 font-semibold underline hover:opacity-85"
        >
          {t("callOpenNetworkSettings")}
        </button>
      </div>
    );
  }

  if (loadingInit && !status && !config) {
    return (
      <div
        className="flex items-center justify-center gap-2 rounded-xl p-5 text-xs"
        style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
      >
        <Loader2 className="h-4 w-4 animate-spin" style={{ color: "var(--accent)" }} />
        <span>{t("storageLoading")}</span>
      </div>
    );
  }

  const oauthConfigured = Boolean(
    status?.oauth_configured || config?.has_builtin_oauth || config?.client_id?.trim(),
  );
  const usingBuiltinOAuth = Boolean(config?.has_builtin_oauth && !config?.client_id?.trim());
  const usingBroker =
    config?.oauth_mode === "builtin_broker" || config?.oauth_mode === "custom_broker";
  const hasCustomOAuthOverride = Boolean(
    config?.client_id?.trim() || config?.token_broker_url?.trim() || config?.has_client_secret,
  );
  const isWaitingBrowser = connecting || status?.phase === "connecting";
  const isBusySync = syncingNow || Boolean(status?.syncing);
  const progressPct =
    status && status.progress_total > 0
      ? Math.min(100, Math.round((status.progress_done / status.progress_total) * 100))
      : null;

  const quotaLimit = overview?.quota?.drive_limit_bytes ?? null;
  const quotaUsed = overview?.quota?.drive_usage_bytes ?? 0;
  const quotaPct =
    quotaLimit !== null && quotaLimit > 0
      ? Math.min(100, Math.round((quotaUsed / quotaLimit) * 100))
      : null;

  const renderConfirmBox = () => {
    if (!confirmAction) return null;
    let targetSummary = "";
    if (confirmAction.kind === "thread") {
      targetSummary = `${confirmAction.threadName} · ${t("storageObjectCount", { count: confirmAction.count })} (${formatBytes(confirmAction.bytes)})`;
    } else if (confirmAction.kind === "items") {
      targetSummary = `${t("storageObjectCount", { count: confirmAction.sha256s.length })} (${formatBytes(confirmAction.totalBytes)})`;
    } else {
      targetSummary = t("gdrivePruneDaysOption", { days: confirmAction.days });
    }

    return (
      <div
        ref={confirmBoxRef}
        className="space-y-2.5 rounded-xl p-3 text-xs"
        style={{
          background: "var(--danger-bg)",
          border: "1px solid var(--danger-text)",
          color: "var(--danger-text)",
        }}
      >
        <div className="flex items-start gap-2">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
          <div className="min-w-0 flex-1 space-y-1">
            <div className="font-semibold">{t("gdriveConfirmDeleteTitle")}</div>
            <div className="font-medium opacity-95">{targetSummary}</div>
            <div className="leading-relaxed opacity-90">{t("gdriveConfirmDeleteDesc")}</div>
          </div>
        </div>
        <div className="flex flex-wrap items-center justify-end gap-2">
          <button
            type="button"
            disabled={pruning}
            onClick={() => setConfirmAction(null)}
            className="rounded-lg px-3 py-1.5 text-xs font-medium disabled:opacity-50"
            style={{
              background: "var(--bg-sidebar)",
              border: "1px solid var(--border-subtle)",
              color: "var(--text-primary)",
            }}
          >
            {t("cancel")}
          </button>
          <button
            type="button"
            disabled={pruning}
            onClick={() => void handleExecuteConfirmDelete()}
            className="inline-flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-xs font-semibold transition hover:opacity-90 disabled:opacity-50"
            style={{
              background: "#dc2626",
              color: "#ffffff",
            }}
          >
            {pruning ? (
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
            ) : (
              <Trash2 className="h-3.5 w-3.5" />
            )}
            <span>
              {pruning
                ? t("gdriveDeletingBtn")
                : confirmAction.kind === "age"
                  ? t("gdrivePruneNowBtn")
                  : t("delete")}
            </span>
          </button>
        </div>
      </div>
    );
  };

  return (
    <div className="space-y-3">
      {/* Feedback banner */}
      {feedback && (
        <div
          className="flex items-start justify-between gap-2 rounded-xl px-3 py-2 text-xs"
          style={{
            background:
              feedback.tone === "ok"
                ? "rgba(16,185,129,0.12)"
                : feedback.tone === "warn"
                  ? "rgba(245,158,11,0.14)"
                  : "var(--danger-bg)",
            color:
              feedback.tone === "ok"
                ? "#34d399"
                : feedback.tone === "warn"
                  ? "#fbbf24"
                  : "var(--danger-text)",
          }}
        >
          <div className="flex min-w-0 flex-1 items-start gap-2">
            {feedback.tone === "ok" ? (
              <CheckCircle2 className="mt-0.5 h-3.5 w-3.5 shrink-0" />
            ) : (
              <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
            )}
            <span className="leading-relaxed">{feedback.text}</span>
          </div>
          <button
            type="button"
            onClick={() => setFeedback(null)}
            className="shrink-0 rounded p-0.5 opacity-75 hover:opacity-100"
            aria-label={t("close")}
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      )}

      {/* 2. Connection & BYO OAuth Card */}
      <div
        className="space-y-3 rounded-xl p-3.5"
        style={{
          background: "var(--bg-surface)",
          border: "1px solid var(--border-subtle)",
        }}
      >
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="flex min-w-0 flex-1 items-start gap-2.5">
            <Cloud className="mt-0.5 h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
            <div className="min-w-0 flex-1 space-y-1">
              {status?.connected ? (
                <>
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="truncate text-[13.5px] font-semibold">
                      {t("gdriveConnectedAs", {
                        email: status.account_email || status.account_name || "Google Drive",
                      })}
                    </span>
                    <span
                      className="inline-flex shrink-0 items-center gap-1 rounded-full px-2 py-0.5 text-[10.5px] font-semibold"
                      style={
                        status.vault_unlocked
                          ? { background: "rgba(16,185,129,0.14)", color: "#34d399" }
                          : { background: "rgba(245,158,11,0.15)", color: "#fbbf24" }
                      }
                    >
                      {status.vault_unlocked ? (
                        <LockOpen className="h-3 w-3" />
                      ) : (
                        <Lock className="h-3 w-3" />
                      )}
                      {status.vault_unlocked
                        ? t("gdriveVaultUnlockedBadge")
                        : t("gdriveVaultLockedBadge")}
                    </span>
                  </div>
                  {status.account_name &&
                    status.account_email &&
                    status.account_name !== status.account_email && (
                      <div className="text-[11.5px]" style={{ color: "var(--text-secondary)" }}>
                        {status.account_name}
                      </div>
                    )}
                </>
              ) : (
                <>
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="text-[13px] font-semibold">
                      {oauthConfigured ? t("gdriveOAuthConfiguredHint") : t("gdriveOAuthMissingHint")}
                    </span>
                    {usingBuiltinOAuth && (
                      <span
                        className="shrink-0 rounded-full px-2 py-0.5 text-[10.5px] font-semibold"
                        style={{
                          background: "var(--accent-soft)",
                          color: "var(--accent)",
                        }}
                      >
                        {t("gdriveOAuthBuiltinBadge")}
                      </span>
                    )}
                    {usingBroker && (
                      <span
                        className="shrink-0 rounded-full px-2 py-0.5 text-[10.5px] font-semibold"
                        style={{
                          background: "rgba(16,185,129,0.14)",
                          color: "#34d399",
                        }}
                      >
                        {t("gdriveOAuthBrokerBadge")}
                      </span>
                    )}
                  </div>
                  {oauthConfigured && (
                    <div className="text-[11.5px] leading-relaxed" style={{ color: "var(--text-secondary)" }}>
                      {t("gdriveOneClickReadyHint")}
                    </div>
                  )}
                </>
              )}
            </div>
          </div>

          <div className="flex shrink-0 flex-wrap items-center gap-2">
            {status?.connected ? (
              <>
                {validRootFolderId && (
                  <button
                    type="button"
                    onClick={() => void handleOpenVaultFolder()}
                    className="hover-menu inline-flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-xs font-semibold transition"
                    style={{
                      background: "var(--bg-sidebar)",
                      border: "1px solid var(--border-subtle)",
                      color: "var(--text-primary)",
                    }}
                  >
                    <ExternalLink className="h-3.5 w-3.5 shrink-0" />
                    <span>{t("gdriveOpenVaultFolderBtn")}</span>
                  </button>
                )}
                {status.vault_unlocked && onLockVault && (
                  <button
                    type="button"
                    disabled={lockingVault || isBusySync || pruning || disconnecting}
                    onClick={() => void handleLockVault()}
                    title={t("gdriveLockVaultTitle")}
                    className="hover-menu inline-flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-xs font-semibold transition disabled:cursor-not-allowed disabled:opacity-50"
                    style={{
                      background: "var(--bg-sidebar)",
                      border: "1px solid var(--border-subtle)",
                      color: "var(--text-primary)",
                    }}
                  >
                    {lockingVault ? (
                      <Loader2 className="h-3.5 w-3.5 animate-spin" />
                    ) : (
                      <Lock className="h-3.5 w-3.5" />
                    )}
                    <span>{t("gdriveLockVaultBtn")}</span>
                  </button>
                )}
                <button
                  type="button"
                  disabled={disconnecting || pruning || isBusySync || lockingVault}
                  onClick={() => setConfirmDisconnect((prev) => !prev)}
                  title={isBusySync ? t("gdriveDisconnectBusyHint") : undefined}
                  className="inline-flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-xs font-semibold transition hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-50"
                  style={{
                    background: "var(--danger-bg)",
                    color: "var(--danger-text)",
                  }}
                >
                  {disconnecting ? (
                    <Loader2 className="h-3.5 w-3.5 animate-spin" />
                  ) : (
                    <LogOut className="h-3.5 w-3.5" />
                  )}
                  <span>{t("gdriveDisconnectBtn")}</span>
                </button>
              </>
            ) : isWaitingBrowser ? (
              <button
                type="button"
                disabled={cancellingConnect}
                onClick={() => void handleCancelConnect()}
                className="inline-flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-xs font-semibold transition hover:opacity-90 disabled:opacity-50"
                style={{
                  background: "var(--bg-sidebar)",
                  border: "1px solid var(--border-subtle)",
                  color: "var(--text-primary)",
                }}
              >
                {cancellingConnect ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <X className="h-3.5 w-3.5" />
                )}
                <span>{t("gdriveCancelSignInBtn")}</span>
              </button>
            ) : (
              <button
                type="button"
                disabled={!oauthConfigured || savingOAuth}
                onClick={() => void handleConnect()}
                className="inline-flex items-center gap-2 rounded-lg px-3.5 py-2 text-xs font-semibold shadow-sm transition hover:brightness-95 disabled:cursor-not-allowed disabled:opacity-50"
                style={{
                  background: "#ffffff",
                  color: "#1f2937",
                  border: "1px solid #d1d5db",
                }}
              >
                <GoogleLogoIcon className="h-4 w-4 shrink-0" />
                <span>{t("gdriveSignInWithGoogleBtn")}</span>
              </button>
            )}
          </div>
        </div>

        {/* Inline confirmation dialog before disconnecting Google Drive */}
        {status?.connected && confirmDisconnect && (
          <div
            ref={confirmDisconnectRef}
            role="alertdialog"
            aria-label={t("gdriveDisconnectConfirmTitle")}
            className="space-y-2.5 rounded-xl p-3 text-xs"
            style={{
              background: "var(--danger-bg)",
              border: "1px solid var(--danger-text)",
              color: "var(--danger-text)",
            }}
          >
            <div className="flex items-start gap-2">
              <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
              <div className="min-w-0 flex-1 space-y-1">
                <div className="font-semibold">{t("gdriveDisconnectConfirmTitle")}</div>
                <div className="leading-relaxed opacity-90">{t("gdriveDisconnectConfirmDesc")}</div>
              </div>
            </div>
            <div className="flex flex-wrap items-center justify-end gap-2">
              <button
                type="button"
                disabled={disconnecting}
                onClick={() => setConfirmDisconnect(false)}
                className="rounded-lg px-3 py-1.5 text-xs font-medium disabled:opacity-50"
                style={{
                  background: "var(--bg-sidebar)",
                  border: "1px solid var(--border-subtle)",
                  color: "var(--text-primary)",
                }}
              >
                {t("cancel")}
              </button>
              <button
                type="button"
                disabled={disconnecting || pruning || isBusySync || lockingVault}
                onClick={() => void handleDisconnect()}
                className="inline-flex items-center gap-1.5 rounded-lg bg-rose-600 px-3 py-1.5 text-xs font-semibold text-white transition hover:bg-rose-500 disabled:opacity-50"
              >
                {disconnecting ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <LogOut className="h-3.5 w-3.5" />
                )}
                <span>{t("gdriveDisconnectBtn")}</span>
              </button>
            </div>
          </div>
        )}

        {/* Active waiting-for-browser sign-in guidance */}
        {!status?.connected && isWaitingBrowser && (
          <div
            className="flex items-start gap-2.5 rounded-lg px-3 py-2.5 text-xs"
            style={{
              background: "var(--accent-soft)",
              color: "var(--text-primary)",
            }}
          >
            <Loader2
              className="mt-0.5 h-4 w-4 shrink-0 animate-spin"
              style={{ color: "var(--accent)" }}
            />
            <div className="min-w-0 flex-1 space-y-0.5">
              <div className="font-semibold">{t("gdriveConnectingBtn")}</div>
              <div className="leading-relaxed" style={{ color: "var(--text-secondary)" }}>
                {t("gdriveWaitingBrowserHint")}
              </div>
            </div>
          </div>
        )}

        {/* Scope, backup location & E2EE privacy note */}
        <div
          className="space-y-1.5 rounded-lg px-2.5 py-2 text-[11.5px] leading-relaxed"
          style={{
            background: "var(--bg-sidebar)",
            color: "var(--text-secondary)",
          }}
        >
          <div className="flex items-start gap-2">
            <Shield className="mt-0.5 h-3.5 w-3.5 shrink-0" style={{ color: "var(--accent)" }} />
            <span>{t("gdriveScopeNote")}</span>
          </div>
          <div className="pl-[22px] text-[11px] opacity-90">{t("gdriveBackupLocationNote")}</div>
        </div>

        {/* Collapsible Developer / Advanced BYO OAuth drawer */}
        <div className="border-t pt-2.5" style={{ borderColor: "var(--border-subtle)" }}>
          <button
            type="button"
            onClick={() => {
              oauthDrawerTouchedRef.current = true;
              setOauthDrawerOpen((prev) => !prev);
            }}
            className="flex w-full items-center justify-between gap-2 rounded-lg px-1.5 py-1 text-left text-xs font-medium transition hover:bg-black/10"
            style={{ color: "var(--text-secondary)" }}
          >
            <span className="flex items-center gap-1.5">
              {oauthDrawerOpen ? (
                <ChevronDown className="h-3.5 w-3.5 shrink-0" />
              ) : (
                <ChevronRight className="h-3.5 w-3.5 shrink-0" />
              )}
              <span>{t("gdriveOAuthToggle")}</span>
            </span>
            {config?.has_builtin_oauth && (
              <span
                className="shrink-0 rounded-full px-2 py-0.5 text-[10px] font-semibold"
                style={{
                  background: "var(--accent-soft)",
                  color: "var(--accent)",
                }}
              >
                {t("gdriveOAuthBuiltinBadge")}
              </span>
            )}
          </button>

          {oauthDrawerOpen && (
            <form onSubmit={(e) => void handleSaveOAuth(e)} className="mt-2.5 space-y-2.5">
              <div>
                <label
                  className="mb-1 block text-xs font-medium"
                  style={{ color: "var(--text-secondary)" }}
                >
                  {t("gdriveClientIdLabel")}
                </label>
                <input
                  type="text"
                  value={oauthClientId}
                  onChange={(e) => setOauthClientId(e.target.value)}
                  placeholder={t("gdriveClientIdPlaceholder")}
                  autoComplete="off"
                  spellCheck={false}
                  className="w-full rounded-lg px-2.5 py-1.5 font-mono text-xs outline-none"
                  style={{
                    background: "var(--bg-sidebar)",
                    border: "1px solid var(--border-subtle)",
                    color: "var(--text-primary)",
                  }}
                />
              </div>

              <div>
                <label
                  className="mb-1 block text-xs font-medium"
                  style={{ color: "var(--text-secondary)" }}
                >
                  {t("gdriveTokenBrokerLabel")}
                </label>
                <input
                  type="url"
                  value={oauthBrokerUrl}
                  onChange={(e) => setOauthBrokerUrl(e.target.value)}
                  placeholder={t("gdriveTokenBrokerPlaceholder")}
                  autoComplete="off"
                  spellCheck={false}
                  className="w-full rounded-lg px-2.5 py-1.5 font-mono text-xs outline-none"
                  style={{
                    background: "var(--bg-sidebar)",
                    border: "1px solid var(--border-subtle)",
                    color: "var(--text-primary)",
                  }}
                />
                <p className="mt-1 text-[11px] leading-relaxed" style={{ color: "var(--text-secondary)" }}>
                  {t("gdriveTokenBrokerHint")}
                </p>
              </div>

              {!oauthBrokerUrl.trim() && (
                <div>
                  <label
                    className="mb-1 block text-xs font-medium"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    {t("gdriveClientSecretLabel")}
                  </label>
                  <input
                    type="password"
                    value={oauthClientSecret}
                    onChange={(e) => setOauthClientSecret(e.target.value)}
                    placeholder={
                      config?.has_client_secret
                        ? t("gdriveClientSecretSavedPlaceholder")
                        : t("gdriveClientSecretPlaceholder")
                    }
                    autoComplete="new-password"
                    className="w-full rounded-lg px-2.5 py-1.5 font-mono text-xs outline-none"
                    style={{
                      background: "var(--bg-sidebar)",
                      border: "1px solid var(--border-subtle)",
                      color: "var(--text-primary)",
                    }}
                  />
                </div>
              )}

              <div className="flex flex-wrap items-center justify-end gap-2">
                {config?.has_builtin_oauth && hasCustomOAuthOverride && (
                  <button
                    type="button"
                    disabled={savingOAuth}
                    onClick={() => void handleResetOAuthDefault()}
                    className="rounded-lg px-3 py-1.5 text-xs font-medium transition hover:opacity-85 disabled:opacity-50"
                    style={{
                      background: "var(--bg-sidebar)",
                      border: "1px solid var(--border-subtle)",
                      color: "var(--text-primary)",
                    }}
                  >
                    {t("gdriveResetOAuthDefault")}
                  </button>
                )}
                <button
                  type="submit"
                  disabled={savingOAuth}
                  className="tf-btn inline-flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-xs font-semibold disabled:opacity-50"
                >
                  {savingOAuth && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
                  <span>{t("gdriveSaveOAuth")}</span>
                </button>
              </div>
            </form>
          )}
        </div>
      </div>

      {/* 3. E2EE Sync Passphrase Setup / Unlock Card */}
      {status?.connected && !status.vault_unlocked && (
        <form
          onSubmit={(e) => void handleUnlockVault(e)}
          className="space-y-3 rounded-xl p-3.5"
          style={{
            background: "var(--bg-surface)",
            border: "1px solid var(--accent)",
          }}
        >
          <div className="flex items-start gap-2.5">
            <KeyRound className="mt-0.5 h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
            <div className="min-w-0 flex-1 space-y-1">
              <div className="text-[13.5px] font-semibold">{t("gdrivePassphraseTitle")}</div>
              <p className="text-xs leading-relaxed" style={{ color: "var(--text-secondary)" }}>
                {status.has_remote_manifest
                  ? t("gdrivePassphraseUnlockDesc")
                  : t("gdrivePassphraseCreateDesc")}
              </p>
            </div>
          </div>

          <div className="space-y-2">
            <div className="relative">
              <input
                type={showPassphrase ? "text" : "password"}
                value={passphrase}
                onChange={(e) => {
                  setPassphrase(e.target.value);
                  if (passphraseError) setPassphraseError(null);
                }}
                placeholder={t("gdrivePassphrasePlaceholder")}
                autoComplete={status.has_remote_manifest ? "current-password" : "new-password"}
                disabled={unlocking}
                className="w-full rounded-lg py-1.5 pr-9 pl-2.5 text-[13px] outline-none disabled:opacity-50"
                style={{
                  background: "var(--bg-sidebar)",
                  border: "1px solid var(--border-subtle)",
                  color: "var(--text-primary)",
                }}
              />
              <button
                type="button"
                tabIndex={-1}
                onClick={() => setShowPassphrase((v) => !v)}
                className="absolute top-1/2 right-2 -translate-y-1/2 rounded p-1 opacity-75 hover:opacity-100"
                style={{ color: "var(--text-secondary)" }}
                aria-label={showPassphrase ? "Hide" : "Show"}
              >
                {showPassphrase ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
              </button>
            </div>

            {!status.has_remote_manifest && (
              <input
                type={showPassphrase ? "text" : "password"}
                value={passphraseConfirm}
                onChange={(e) => {
                  setPassphraseConfirm(e.target.value);
                  if (passphraseError) setPassphraseError(null);
                }}
                placeholder={t("gdrivePassphraseConfirmPlaceholder")}
                autoComplete="new-password"
                disabled={unlocking}
                className="w-full rounded-lg px-2.5 py-1.5 text-[13px] outline-none disabled:opacity-50"
                style={{
                  background: "var(--bg-sidebar)",
                  border: "1px solid var(--border-subtle)",
                  color: "var(--text-primary)",
                }}
              />
            )}
          </div>

          {passphraseError && (
            <div
              className="flex items-start gap-2 rounded-lg px-2.5 py-2 text-xs"
              style={{
                background: "var(--danger-bg)",
                color: "var(--danger-text)",
              }}
            >
              <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
              <span>{passphraseError}</span>
            </div>
          )}

          <div className="flex justify-end">
            <button
              type="submit"
              disabled={unlocking || passphrase.length === 0}
              className="tf-btn inline-flex items-center gap-1.5 rounded-lg px-3.5 py-1.5 text-xs font-semibold disabled:opacity-50"
            >
              {unlocking ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <LockOpen className="h-3.5 w-3.5" />
              )}
              <span>
                {unlocking
                  ? t("gdriveUnlockingBtn")
                  : status.has_remote_manifest
                    ? t("gdriveUnlockBtn")
                    : t("gdriveCreateVaultBtn")}
              </span>
            </button>
          </div>
        </form>
      )}

      {/* 4. Sync Controls & Live Progress */}
      {canManageCloud && status && (
        <div
          className="space-y-3 rounded-xl p-3.5"
          style={{
            background: "var(--bg-surface)",
            border: "1px solid var(--border-subtle)",
          }}
        >
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div className="min-w-0 space-y-0.5">
              <div className="flex flex-wrap items-center gap-2">
                <span
                  className="inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-[11px] font-semibold"
                  style={
                    status.phase === "error"
                      ? { background: "var(--danger-bg)", color: "var(--danger-text)" }
                      : { background: "var(--accent-soft)", color: "var(--accent)" }
                  }
                >
                  {isBusySync && <Loader2 className="h-3 w-3 animate-spin" />}
                  <span>{getPhaseLabel(status.phase)}</span>
                </span>
                <span className="text-[11.5px]" style={{ color: "var(--text-secondary)" }}>
                  {status.last_sync_ts
                    ? t("gdriveLastSync", { time: formatDateTime(status.last_sync_ts, locale) })
                    : t("gdriveNeverSynced")}
                </span>
              </div>
              {status.pending_media_count > 0 && (
                <div className="text-[11.5px]" style={{ color: "var(--text-secondary)" }}>
                  {t("gdrivePendingSummary", {
                    count: status.pending_media_count,
                    size: formatBytes(status.pending_media_bytes),
                  })}
                </div>
              )}
            </div>

            <button
              type="button"
              disabled={isBusySync || savingToggle || pruning}
              onClick={() => void handleSyncNow()}
              className="tf-btn inline-flex shrink-0 items-center gap-1.5 rounded-lg px-3 py-1.5 text-xs font-semibold disabled:opacity-50"
            >
              {isBusySync ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <RefreshCw className="h-3.5 w-3.5" />
              )}
              <span>{isBusySync ? t("gdriveSyncingBtn") : t("gdriveSyncNowBtn")}</span>
            </button>
          </div>

          {isBusySync && progressPct !== null && (
            <div className="space-y-1">
              <div
                className="flex items-center justify-between text-[11px]"
                style={{ color: "var(--text-secondary)" }}
              >
                <span>
                  {status.progress_done} / {status.progress_total}
                </span>
                <span>{progressPct}%</span>
              </div>
              <div
                className="h-1.5 w-full overflow-hidden rounded-full"
                style={{ background: "var(--bg-sidebar)" }}
              >
                <div
                  className="h-full rounded-full transition-all"
                  style={{ width: `${progressPct}%`, background: "var(--accent)" }}
                />
              </div>
            </div>
          )}

          {status.last_error && (
            <div
              className="flex items-start gap-2 rounded-lg px-2.5 py-2 text-xs"
              style={{
                background: "var(--danger-bg)",
                color: "var(--danger-text)",
              }}
            >
              <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
              <span className="leading-relaxed">{status.last_error}</span>
            </div>
          )}

          <div
            className="space-y-0.5 border-t pt-2"
            style={{ borderColor: "var(--border-subtle)" }}
          >
            <CloudToggleRow
              icon={<RefreshCw className="h-4 w-4" />}
              label={t("gdriveSyncEnabledLabel")}
              checked={config?.sync_enabled ?? status.sync_enabled}
              disabled={savingToggle}
              onChange={(syncEnabled) => void handleToggleSyncSetting({ syncEnabled })}
            />
            <CloudToggleRow
              icon={<Image className="h-4 w-4" />}
              label={t("gdriveSyncMediaLabel")}
              checked={config?.sync_media ?? status.sync_media}
              disabled={savingToggle}
              onChange={(syncMedia) => void handleToggleSyncSetting({ syncMedia })}
            />
            <CloudToggleRow
              icon={<History className="h-4 w-4" />}
              label={t("gdriveSyncHistoryLabel")}
              checked={config?.sync_history ?? status.sync_history}
              disabled={savingToggle}
              onChange={(syncHistory) => void handleToggleSyncSetting({ syncHistory })}
            />
          </div>
        </div>
      )}

      {/* 5. Cloud Storage Manager (No Artificial Cloud Cap + Full Management) */}
      {canManageCloud && (
        <div
          className="space-y-3 rounded-xl p-3.5"
          style={{
            background: "var(--bg-surface)",
            border: "1px solid var(--border-subtle)",
          }}
        >
          {/* Header + Refresh */}
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div className="flex items-center gap-2 text-[13px] font-semibold">
              <Database className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
              <span>{t("gdriveManagerTitle")}</span>
            </div>
            <button
              type="button"
              disabled={loadingOverview || pruning}
              onClick={() => void loadOverview()}
              title={t("gdriveRefreshOverview")}
              className="hover-menu inline-flex items-center gap-1.5 rounded-lg px-2.5 py-1 text-xs font-medium disabled:opacity-50"
              style={{
                background: "var(--bg-sidebar)",
                border: "1px solid var(--border-subtle)",
                color: "var(--text-primary)",
              }}
            >
              <RefreshCw className={`h-3.5 w-3.5 ${loadingOverview ? "animate-spin" : ""}`} />
              <span>{t("gdriveRefreshOverview")}</span>
            </button>
          </div>

          {loadingOverview && !overview ? (
            <div
              className="flex items-center justify-center gap-2 rounded-lg p-5 text-xs"
              style={{ background: "var(--bg-sidebar)", color: "var(--text-secondary)" }}
            >
              <Loader2 className="h-4 w-4 animate-spin" style={{ color: "var(--accent)" }} />
              <span>{t("gdriveLoadingOverview")}</span>
            </div>
          ) : (
            <>
              {/* Real Google Drive Quota Bar */}
              {overview?.quota && (
                <div
                  className="space-y-1.5 rounded-lg p-2.5"
                  style={{
                    background: "var(--bg-sidebar)",
                    border: "1px solid var(--border-subtle)",
                  }}
                >
                  <div className="flex flex-wrap items-center justify-between gap-2 text-xs">
                    <span className="font-medium">
                      {t("gdriveDriveQuotaLabel", {
                        used: formatBytes(quotaUsed),
                        limit:
                          quotaLimit !== null && quotaLimit > 0
                            ? formatBytes(quotaLimit)
                            : t("gdriveDriveUnlimited"),
                      })}
                    </span>
                    {quotaPct !== null && (
                      <span className="font-semibold" style={{ color: "var(--text-secondary)" }}>
                        {quotaPct}%
                      </span>
                    )}
                  </div>
                  {quotaPct !== null && (
                    <div
                      className="h-1.5 w-full overflow-hidden rounded-full"
                      style={{ background: "var(--bg-surface)" }}
                    >
                      <div
                        className="h-full rounded-full transition-all"
                        style={{
                          width: `${quotaPct}%`,
                          background:
                            quotaPct >= 95
                              ? "#ef4444"
                              : quotaPct >= 85
                                ? "#f59e0b"
                                : "var(--accent)",
                        }}
                      />
                    </div>
                  )}
                </div>
              )}

              {/* No artificial cloud cap note */}
              <div
                className="flex items-start gap-2 rounded-lg px-2.5 py-2 text-[11.5px] leading-relaxed"
                style={{
                  background: "var(--bg-sidebar)",
                  color: "var(--text-secondary)",
                }}
              >
                <Sparkles
                  className="mt-0.5 h-3.5 w-3.5 shrink-0"
                  style={{ color: "var(--accent)" }}
                />
                <span>{t("gdriveNoCloudCapNote")}</span>
              </div>

              {/* Teza Cloud Usage Breakdown */}
              <div className="space-y-2">
                <div className="flex flex-wrap items-center justify-between gap-2 text-xs">
                  <span className="font-semibold">
                    {t("gdriveTezaCloudTotal", {
                      size: formatBytes(overview?.teza_cloud_total_bytes ?? 0),
                      count: overview?.teza_cloud_object_count ?? 0,
                    })}
                  </span>
                </div>

                <div className="grid grid-cols-2 gap-1.5 sm:grid-cols-4">
                  {(
                    [
                      {
                        id: "images",
                        label: t("storageBreakdownImages"),
                        bytes: overview?.images_bytes ?? 0,
                        icon: <Image className="h-3.5 w-3.5" />,
                      },
                      {
                        id: "videos",
                        label: t("storageBreakdownVideos"),
                        bytes: overview?.videos_bytes ?? 0,
                        icon: <Film className="h-3.5 w-3.5" />,
                      },
                      {
                        id: "voice",
                        label: t("storageBreakdownVoice"),
                        bytes: overview?.voice_bytes ?? 0,
                        icon: <Mic className="h-3.5 w-3.5" />,
                      },
                      {
                        id: "files",
                        label: t("storageBreakdownFiles"),
                        bytes: overview?.files_bytes ?? 0,
                        icon: <FileText className="h-3.5 w-3.5" />,
                      },
                    ] as const
                  ).map((pill) => (
                    <div
                      key={pill.id}
                      className="rounded-lg px-2.5 py-2"
                      style={{
                        background: "var(--bg-sidebar)",
                        border: "1px solid var(--border-subtle)",
                      }}
                    >
                      <div
                        className="flex items-center gap-1 text-[11px]"
                        style={{ color: "var(--text-secondary)" }}
                      >
                        <span style={{ color: "var(--accent)" }}>{pill.icon}</span>
                        <span className="truncate">{pill.label}</span>
                      </div>
                      <div className="mt-0.5 truncate text-xs font-semibold">
                        {formatBytes(pill.bytes)}
                      </div>
                    </div>
                  ))}
                </div>

                <div
                  className="flex items-center gap-2 rounded-lg px-2.5 py-1.5 text-[11.5px]"
                  style={{
                    background: "var(--bg-sidebar)",
                    border: "1px solid var(--border-subtle)",
                    color: "var(--text-secondary)",
                  }}
                >
                  <History className="h-3.5 w-3.5 shrink-0" style={{ color: "var(--accent)" }} />
                  <span className="truncate font-medium">
                    {t("gdriveHistoryBackupLabel", {
                      count: overview?.history_file_count ?? 0,
                      size: formatBytes(overview?.history_bytes ?? 0),
                    })}
                  </span>
                </div>
              </div>

              {/* 3 Management Sub-Tabs */}
              <div
                className="grid grid-cols-3 gap-1 rounded-xl p-1"
                style={{ background: "var(--bg-sidebar)" }}
              >
                {(
                  [
                    { id: "by_thread", label: t("gdriveTabByThread") },
                    { id: "largest_items", label: t("gdriveTabLargestFiles") },
                    { id: "prune_age", label: t("gdriveTabPruneByAge") },
                  ] as const
                ).map((tab) => {
                  const active = managerTab === tab.id;
                  return (
                    <button
                      key={tab.id}
                      type="button"
                      onClick={() => {
                        setManagerTab(tab.id);
                        setConfirmAction(null);
                      }}
                      className="truncate rounded-lg px-2 py-1.5 text-xs font-semibold transition"
                      style={{
                        background: active ? "var(--accent)" : "transparent",
                        color: active ? "var(--text-on-accent)" : "var(--text-secondary)",
                      }}
                    >
                      {tab.label}
                    </button>
                  );
                })}
              </div>

              {/* Inline Confirmation Box (when active) */}
              {renderConfirmBox()}

              {/* Sub-Tab 1: By Conversation */}
              {managerTab === "by_thread" && (
                <div className="space-y-1.5">
                  {sortedThreads.length === 0 ? (
                    <div
                      className="rounded-lg px-3 py-5 text-center text-xs leading-relaxed"
                      style={{ background: "var(--bg-sidebar)", color: "var(--text-secondary)" }}
                    >
                      {(overview?.history_file_count ?? 0) > 0
                        ? t("gdriveNoMediaItemsWithHistory", {
                            count: overview?.history_file_count ?? 0,
                          })
                        : t("gdriveNoCloudItems")}
                    </div>
                  ) : (
                    <div className="max-h-60 space-y-1.5 overflow-y-auto pr-0.5">
                      {sortedThreads.map((row) => (
                        <div
                          key={row.thread_id}
                          className="flex items-center justify-between gap-3 rounded-lg px-3 py-2"
                          style={{
                            background: "var(--bg-sidebar)",
                            border: "1px solid var(--border-subtle)",
                          }}
                        >
                          <div className="min-w-0 flex-1">
                            <div
                              className="truncate text-xs font-semibold"
                              title={row.thread_name || row.thread_id}
                            >
                              {row.thread_name || row.thread_id}
                            </div>
                            <div
                              className="mt-0.5 flex flex-wrap items-center gap-2 text-[11px]"
                              style={{ color: "var(--text-secondary)" }}
                            >
                              <span>{t("storageObjectCount", { count: row.object_count })}</span>
                              <span>·</span>
                              <span
                                className="font-semibold"
                                style={{ color: "var(--text-primary)" }}
                              >
                                {formatBytes(row.total_bytes)}
                              </span>
                            </div>
                          </div>
                          <button
                            type="button"
                            disabled={pruning}
                            onClick={() =>
                              setConfirmAction({
                                kind: "thread",
                                threadId: row.thread_id,
                                threadName: row.thread_name || row.thread_id,
                                count: row.object_count,
                                bytes: row.total_bytes,
                              })
                            }
                            className="inline-flex shrink-0 items-center gap-1 rounded-lg px-2.5 py-1.5 text-[11.5px] font-semibold transition hover:opacity-90 disabled:opacity-50"
                            style={{
                              background: "var(--danger-bg)",
                              color: "var(--danger-text)",
                            }}
                          >
                            <Trash2 className="h-3.5 w-3.5" />
                            <span>{t("gdriveDeleteThreadCloud")}</span>
                          </button>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              )}

              {/* Sub-Tab 2: Largest Files */}
              {managerTab === "largest_items" && (
                <div className="space-y-2">
                  {largestItems.length === 0 ? (
                    <div
                      className="rounded-lg px-3 py-5 text-center text-xs leading-relaxed"
                      style={{ background: "var(--bg-sidebar)", color: "var(--text-secondary)" }}
                    >
                      {(overview?.history_file_count ?? 0) > 0
                        ? t("gdriveNoMediaItemsWithHistory", {
                            count: overview?.history_file_count ?? 0,
                          })
                        : t("gdriveNoCloudItems")}
                    </div>
                  ) : (
                    <>
                      <div className="flex flex-wrap items-center justify-between gap-2">
                        <div className="flex items-center gap-1.5">
                          <button
                            type="button"
                            disabled={pruning || selectedShas.size === largestItems.length}
                            onClick={handleSelectAllItems}
                            className="hover-menu rounded-lg px-2.5 py-1 text-[11.5px] font-medium disabled:opacity-50"
                            style={{
                              background: "var(--bg-sidebar)",
                              border: "1px solid var(--border-subtle)",
                            }}
                          >
                            {t("gdriveSelectAll")}
                          </button>
                          <button
                            type="button"
                            disabled={pruning || selectedShas.size === 0}
                            onClick={handleClearSelection}
                            className="hover-menu rounded-lg px-2.5 py-1 text-[11.5px] font-medium disabled:opacity-50"
                            style={{
                              background: "var(--bg-sidebar)",
                              border: "1px solid var(--border-subtle)",
                              color: "var(--text-secondary)",
                            }}
                          >
                            {t("gdriveClearSelection")}
                          </button>
                        </div>

                        <button
                          type="button"
                          disabled={pruning || selectedShas.size === 0}
                          onClick={() =>
                            setConfirmAction({
                              kind: "items",
                              sha256s: Array.from(selectedShas),
                              totalBytes: selectedBytes,
                            })
                          }
                          className="inline-flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-[11.5px] font-semibold transition hover:opacity-90 disabled:opacity-50"
                          style={{
                            background: "var(--danger-bg)",
                            color: "var(--danger-text)",
                          }}
                        >
                          <Trash2 className="h-3.5 w-3.5" />
                          <span>{t("gdriveDeleteSelected", { count: selectedShas.size })}</span>
                        </button>
                      </div>

                      <div className="max-h-64 space-y-1.5 overflow-y-auto pr-0.5">
                        {largestItems.map((item: GDriveCloudItemDto) => {
                          const checked = selectedShas.has(item.sha256);
                          const displayName =
                            item.original_name?.trim() ||
                            `${item.kind || "file"}-${item.sha256.slice(0, 8)}`;
                          const isLocalAndCloud = item.local_state > 0;
                          return (
                            <label
                              key={item.sha256}
                              className="flex cursor-pointer items-center gap-2.5 rounded-lg px-2.5 py-2 transition hover:bg-black/10"
                              style={{
                                background: checked ? "var(--accent-soft)" : "var(--bg-sidebar)",
                                border: checked
                                  ? "1px solid var(--accent)"
                                  : "1px solid var(--border-subtle)",
                              }}
                            >
                              <input
                                type="checkbox"
                                checked={checked}
                                disabled={pruning}
                                onChange={() => toggleItemSelection(item.sha256)}
                                className="h-4 w-4 shrink-0 cursor-pointer accent-[var(--accent)]"
                              />
                              <span style={{ color: "var(--accent)" }}>
                                {renderMediaKindIcon(item.kind)}
                              </span>
                              <div className="min-w-0 flex-1">
                                <div className="truncate text-xs font-medium" title={displayName}>
                                  {displayName}
                                </div>
                                <div
                                  className="mt-0.5 flex flex-wrap items-center gap-1.5 text-[11px]"
                                  style={{ color: "var(--text-secondary)" }}
                                >
                                  <span>{getMediaKindLabel(item.kind)}</span>
                                  {(item.thread_name || item.thread_id) && (
                                    <>
                                      <span>·</span>
                                      <span className="max-w-[160px] truncate">
                                        {item.thread_name || item.thread_id}
                                      </span>
                                    </>
                                  )}
                                  {item.cloud_synced_ts > 0 && (
                                    <>
                                      <span>·</span>
                                      <span>{formatShortDate(item.cloud_synced_ts, locale)}</span>
                                    </>
                                  )}
                                </div>
                              </div>
                              <div className="flex shrink-0 flex-col items-end gap-1">
                                <span className="text-xs font-semibold">
                                  {formatBytes(item.size_bytes)}
                                </span>
                                <span
                                  className="rounded-full px-2 py-0.5 text-[10px] font-semibold"
                                  style={
                                    isLocalAndCloud
                                      ? {
                                          background: "rgba(16,185,129,0.14)",
                                          color: "#34d399",
                                        }
                                      : {
                                          background: "rgba(245,158,11,0.15)",
                                          color: "#fbbf24",
                                        }
                                  }
                                >
                                  {isLocalAndCloud
                                    ? t("gdriveLocalAndCloudBadge")
                                    : t("gdriveCloudOnlyBadge")}
                                </span>
                              </div>
                            </label>
                          );
                        })}
                      </div>
                    </>
                  )}
                </div>
              )}

              {/* Sub-Tab 3: Prune by Age */}
              {managerTab === "prune_age" && (
                <div
                  className="space-y-3 rounded-lg p-3"
                  style={{
                    background: "var(--bg-sidebar)",
                    border: "1px solid var(--border-subtle)",
                  }}
                >
                  <div
                    className="flex items-center gap-1.5 text-xs font-medium"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    <Clock className="h-3.5 w-3.5 shrink-0" style={{ color: "var(--accent)" }} />
                    <span>{t("gdrivePruneOlderThanLabel")}</span>
                  </div>

                  <div className="grid grid-cols-2 gap-1.5 sm:grid-cols-4">
                    {PRUNE_AGE_PRESETS.map((days) => {
                      const active = pruneDays === days;
                      return (
                        <button
                          key={days}
                          type="button"
                          disabled={pruning}
                          onClick={() => {
                            setPruneDays(days);
                            setConfirmAction(null);
                          }}
                          className="rounded-lg px-2.5 py-2 text-xs font-semibold transition disabled:opacity-50"
                          style={{
                            background: active ? "var(--accent)" : "var(--bg-surface)",
                            color: active ? "var(--text-on-accent)" : "var(--text-secondary)",
                            border: active
                              ? "1px solid var(--accent)"
                              : "1px solid var(--border-subtle)",
                          }}
                        >
                          {t("gdrivePruneDaysOption", { days })}
                        </button>
                      );
                    })}
                  </div>

                  <div className="flex justify-end">
                    <button
                      type="button"
                      disabled={pruning}
                      onClick={() => setConfirmAction({ kind: "age", days: pruneDays })}
                      className="inline-flex items-center gap-1.5 rounded-lg px-3.5 py-1.5 text-xs font-semibold transition hover:opacity-90 disabled:opacity-50"
                      style={{
                        background: "var(--danger-bg)",
                        color: "var(--danger-text)",
                      }}
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                      <span>{t("gdrivePruneNowBtn")}</span>
                    </button>
                  </div>
                </div>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}

export default CloudStoragePanel;
