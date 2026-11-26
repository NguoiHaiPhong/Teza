import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ArrowLeft,
  ArrowRight,
  Bookmark,
  Calendar,
  Check,
  Code2,
  Compass,
  ExternalLink,
  Eye,
  EyeOff,
  FileText,
  Folder,
  Globe,
  Home,
  KeyRound,
  LayoutGrid,
  Lock,
  LogIn,
  Mail,
  Pencil,
  Plus,
  RefreshCw,
  ShieldCheck,
  Sparkles,
  Trash2,
  User,
  X,
} from "lucide-react";
import { bridge, isTauri } from "../bridge";
import { useI18n } from "../i18n";
import type { WebAppAccountDto } from "../types";
import { extractWebAppDomain, type WebAppItem } from "../utils/webApps";
import {
  buildWebAppAccountInput,
  extractWebAppOrigin,
  isWebAppOverlaySuspended,
  sortWebAppAccountsForDisplay,
} from "../utils/webappAccounts";

export const WEBAPP_ICON_OPTIONS: { id: string; label: string; icon: typeof Globe }[] = [
  { id: "globe", label: "Web", icon: Globe },
  { id: "calendar", label: "Lịch", icon: Calendar },
  { id: "folder", label: "Tài liệu", icon: Folder },
  { id: "mail", label: "Thư", icon: Mail },
  { id: "file", label: "Ghi chú", icon: FileText },
  { id: "code", label: "Dev", icon: Code2 },
  { id: "bookmark", label: "Dấu trang", icon: Bookmark },
  { id: "compass", label: "Khám phá", icon: Compass },
];

export function renderWebAppIcon(iconId: string, className = "h-5 w-5") {
  const found = WEBAPP_ICON_OPTIONS.find((x) => x.id === iconId);
  const Icon = found ? found.icon : Globe;
  return <Icon className={className} />;
}

interface PendingFillRequest {
  appId: string;
  accountId: string;
  submit: boolean;
}

interface Props {
  app: WebAppItem;
  suspended: boolean;
  allEmbeddedApps?: WebAppItem[];
  onSelectApp?: (item: WebAppItem) => void;
  onBackToHub: () => void;
  onCloseToChat: () => void;
  onOpenExternal: (url: string) => void;
  onError?: (err: unknown) => void;
}

export function WebAppWorkspace({
  app,
  suspended,
  allEmbeddedApps = [],
  onSelectApp,
  onBackToHub,
  onCloseToChat,
  onOpenExternal,
  onError,
}: Props) {
  const { t } = useI18n();
  const hostRef = useRef<HTMLDivElement | null>(null);
  const mountedRef = useRef(true);
  const activeAppIdRef = useRef(app.id);
  activeAppIdRef.current = app.id;
  const preservePanelErrorRef = useRef(false);
  const openedRef = useRef<string | null>(null);
  const openingRef = useRef<string | null>(null);
  const loadSeqRef = useRef(0);
  const pendingFillRef = useRef<PendingFillRequest | null>(null);
  const fillTimerRef = useRef<number | null>(null);

  const [openError, setOpenError] = useState<string | null>(null);
  const [retrySeq, setRetrySeq] = useState(0);

  // Saved accounts panel state
  const [accountsOpen, setAccountsOpen] = useState(false);
  const [accounts, setAccounts] = useState<WebAppAccountDto[]>([]);
  const [accountsLoading, setAccountsLoading] = useState(false);
  const [panelError, setPanelError] = useState<string | null>(null);
  const [panelNotice, setPanelNotice] = useState<string | null>(null);

  // Add / Edit account form state
  const [formOpen, setFormOpen] = useState(false);
  const [editingAccount, setEditingAccount] = useState<WebAppAccountDto | null>(null);
  const [label, setLabel] = useState("");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [showPassword, setShowPassword] = useState(false);
  const [clearPassword, setClearPassword] = useState(false);
  const [autoFill, setAutoFill] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);
  const [deletingId, setDeletingId] = useState<string | null>(null);

  const effectiveSuspended = isWebAppOverlaySuspended(suspended, accountsOpen);
  const effectiveSuspendedRef = useRef(effectiveSuspended);
  effectiveSuspendedRef.current = effectiveSuspended;

  const domain = extractWebAppDomain(app.url);
  const appOrigin = useMemo(() => extractWebAppOrigin(app.url), [app.url]);

  const sortedAccounts = useMemo(() => sortWebAppAccountsForDisplay(accounts), [accounts]);

  const resetForm = useCallback(() => {
    setFormOpen(false);
    setEditingAccount(null);
    setLabel("");
    setUsername("");
    setPassword("");
    setShowPassword(false);
    setClearPassword(false);
    setAutoFill(false);
    setFormError(null);
  }, []);

  const openAddForm = useCallback(() => {
    setEditingAccount(null);
    setLabel("");
    setUsername("");
    setPassword("");
    setShowPassword(false);
    setClearPassword(false);
    setAutoFill(accounts.length === 0);
    setFormError(null);
    setPanelError(null);
    setPanelNotice(null);
    setConfirmDeleteId(null);
    setFormOpen(true);
  }, [accounts.length]);

  const openEditForm = useCallback((acc: WebAppAccountDto) => {
    setEditingAccount(acc);
    setLabel(acc.label);
    setUsername(acc.username);
    setPassword("");
    setShowPassword(false);
    setClearPassword(false);
    setAutoFill(acc.auto_fill);
    setFormError(null);
    setPanelError(null);
    setPanelNotice(null);
    setConfirmDeleteId(null);
    setFormOpen(true);
  }, []);

  const closeAccountsPanel = useCallback(() => {
    setAccountsOpen(false);
    setConfirmDeleteId(null);
    preservePanelErrorRef.current = false;
    resetForm();
  }, [resetForm]);

  const refreshAccounts = useCallback(async () => {
    const seq = ++loadSeqRef.current;
    const currentAppId = app.id;
    setAccountsLoading(true);
    try {
      const list = await bridge.listWebAppAccounts(currentAppId);
      if (
        loadSeqRef.current === seq &&
        mountedRef.current &&
        activeAppIdRef.current === currentAppId
      ) {
        setAccounts(list);
        if (preservePanelErrorRef.current) {
          preservePanelErrorRef.current = false;
        } else {
          setPanelError(null);
        }
      }
    } catch (err) {
      if (
        loadSeqRef.current === seq &&
        mountedRef.current &&
        activeAppIdRef.current === currentAppId
      ) {
        preservePanelErrorRef.current = false;
        setPanelError(String(err));
      }
    } finally {
      if (
        loadSeqRef.current === seq &&
        mountedRef.current &&
        activeAppIdRef.current === currentAppId
      ) {
        setAccountsLoading(false);
      }
    }
  }, [app.id]);

  useEffect(() => {
    loadSeqRef.current += 1;
    setAccounts([]);
    setAccountsLoading(false);
    setSaving(false);
    setDeletingId(null);
    setOpenError(null);
    setAccountsOpen(false);
    resetForm();
    setConfirmDeleteId(null);
    preservePanelErrorRef.current = false;
    setPanelError(null);
    setPanelNotice(null);
    pendingFillRef.current = null;
    if (fillTimerRef.current !== null) {
      window.clearTimeout(fillTimerRef.current);
      fillTimerRef.current = null;
    }
  }, [app.id, app.url, resetForm]);

  useEffect(() => {
    void refreshAccounts();
  }, [refreshAccounts, app.url]);

  useEffect(() => {
    if (accountsOpen) {
      void refreshAccounts();
    }
  }, [accountsOpen, refreshAccounts]);

  useEffect(() => {
    if (!accountsOpen || suspended) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.defaultPrevented) return;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        if (formOpen) {
          resetForm();
        } else {
          closeAccountsPanel();
        }
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [accountsOpen, suspended, formOpen, resetForm, closeAccountsPanel]);

  useEffect(() => {
    if (!isTauri()) return;
    if (effectiveSuspended) {
      openedRef.current = null;
      if (fillTimerRef.current !== null) {
        window.clearTimeout(fillTimerRef.current);
        fillTimerRef.current = null;
      }
      void bridge.webappHideAll().catch(() => undefined);
      return;
    }

    const hostEl = hostRef.current;
    if (!hostEl) return;
    const openedAppId = app.id;
    let cancelled = false;
    let attempted = false;

    const getBounds = () => {
      const rect = hostEl.getBoundingClientRect();
      return {
        x: Math.round(rect.left),
        y: Math.round(rect.top),
        width: Math.round(rect.width),
        height: Math.round(rect.height),
      };
    };

    const syncOpenOrBounds = () => {
      if (cancelled) return;
      const b = getBounds();
      if (b.width < 40 || b.height < 40) return;
      if (openingRef.current === openedAppId) return;
      attempted = true;
      openingRef.current = openedAppId;
      void bridge
        .webappOpen(openedAppId, app.url, b)
        .then(() => {
          if (openingRef.current === openedAppId) openingRef.current = null;
          if (
            cancelled ||
            !mountedRef.current ||
            activeAppIdRef.current !== openedAppId ||
            effectiveSuspendedRef.current
          ) {
            if (
              !mountedRef.current ||
              activeAppIdRef.current !== openedAppId ||
              effectiveSuspendedRef.current
            ) {
              void bridge.webappHideAll().catch(() => undefined);
            }
            return;
          }
          openedRef.current = openedAppId;
          setOpenError(null);

          const pending = pendingFillRef.current;
          if (pending && pending.appId === openedAppId) {
            pendingFillRef.current = null;
            if (fillTimerRef.current !== null) {
              window.clearTimeout(fillTimerRef.current);
            }
            fillTimerRef.current = window.setTimeout(() => {
              fillTimerRef.current = null;
              if (
                cancelled ||
                !mountedRef.current ||
                activeAppIdRef.current !== openedAppId ||
                effectiveSuspendedRef.current
              ) {
                return;
              }
              void bridge
                .fillWebAppAccount(pending.appId, pending.accountId, pending.submit)
                .catch((err) => {
                  if (!mountedRef.current || activeAppIdRef.current !== pending.appId) {
                    return;
                  }
                  preservePanelErrorRef.current = true;
                  setPanelNotice(null);
                  setPanelError(String(err));
                  setAccountsOpen(true);
                  onError?.(err);
                });
            }, 120);
          }
        })
        .catch((e) => {
          if (openingRef.current === openedAppId) openingRef.current = null;
          if (openedRef.current === openedAppId) openedRef.current = null;
          if (pendingFillRef.current?.appId === openedAppId) {
            pendingFillRef.current = null;
          }
          if (!cancelled && mountedRef.current && activeAppIdRef.current === openedAppId) {
            setOpenError(String(e));
            onError?.(e);
          }
        });
    };

    const rafId = window.requestAnimationFrame(syncOpenOrBounds);
    const ro = new ResizeObserver(() => {
      if (cancelled) return;
      const b = getBounds();
      if (b.width < 40 || b.height < 40) return;
      if (openedRef.current === openedAppId) {
        void bridge.webappSetBounds(openedAppId, b).catch(() => undefined);
      } else if (!attempted) {
        syncOpenOrBounds();
      }
    });
    ro.observe(hostEl);

    const onResize = () => {
      if (cancelled) return;
      const b = getBounds();
      if (b.width < 40 || b.height < 40) return;
      if (openedRef.current === openedAppId) {
        void bridge.webappSetBounds(openedAppId, b).catch(() => undefined);
      } else if (!attempted) {
        syncOpenOrBounds();
      }
    };
    window.addEventListener("resize", onResize);

    return () => {
      cancelled = true;
      if (openingRef.current === openedAppId) openingRef.current = null;
      if (fillTimerRef.current !== null) {
        window.clearTimeout(fillTimerRef.current);
        fillTimerRef.current = null;
      }
      window.cancelAnimationFrame(rafId);
      ro.disconnect();
      window.removeEventListener("resize", onResize);
    };
  }, [app.id, app.url, effectiveSuspended, onError, retrySeq]);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      if (fillTimerRef.current !== null) {
        window.clearTimeout(fillTimerRef.current);
        fillTimerRef.current = null;
      }
      if (isTauri()) {
        void bridge.webappHideAll().catch(() => undefined);
      }
    };
  }, []);

  const handleSaveAccount = async (e: React.FormEvent) => {
    e.preventDefault();
    if (saving) return;
    if (!username.trim()) {
      setFormError(t("webappAccountsErrUsernameRequired"));
      return;
    }

    const currentAppId = app.id;
    setSaving(true);
    setFormError(null);
    setPanelError(null);
    preservePanelErrorRef.current = false;
    try {
      const input = buildWebAppAccountInput({
        editingAccount,
        appId: currentAppId,
        appUrl: app.url,
        appName: app.name,
        label,
        username,
        password,
        clearPassword,
        autoFill,
      });
      await bridge.saveWebAppAccount(input);
      if (!mountedRef.current || activeAppIdRef.current !== currentAppId) return;
      resetForm();
      setPanelNotice(t("webappAccountsSavedNotice"));
      await refreshAccounts();
    } catch (err) {
      if (!mountedRef.current || activeAppIdRef.current !== currentAppId) return;
      setFormError(String(err));
    } finally {
      if (mountedRef.current && activeAppIdRef.current === currentAppId) {
        setSaving(false);
      }
    }
  };

  const handleDeleteAccount = async (acc: WebAppAccountDto) => {
    if (deletingId) return;
    const currentAppId = app.id;
    setDeletingId(acc.id);
    setPanelError(null);
    preservePanelErrorRef.current = false;
    try {
      await bridge.deleteWebAppAccount(acc.id);
      if (!mountedRef.current || activeAppIdRef.current !== currentAppId) return;
      if (editingAccount?.id === acc.id) {
        resetForm();
      }
      setConfirmDeleteId(null);
      setPanelNotice(t("webappAccountsDeletedNotice"));
      await refreshAccounts();
    } catch (err) {
      if (!mountedRef.current || activeAppIdRef.current !== currentAppId) return;
      setPanelError(String(err));
    } finally {
      if (mountedRef.current && activeAppIdRef.current === currentAppId) {
        setDeletingId(null);
      }
    }
  };

  const handleFillAccount = (acc: WebAppAccountDto, submit: boolean) => {
    if (appOrigin && acc.origin && acc.origin !== appOrigin) return;
    setConfirmDeleteId(null);
    if (!isTauri()) {
      setPanelNotice(t("webappAccountsPreviewFillHint"));
      return;
    }
    pendingFillRef.current = {
      appId: app.id,
      accountId: acc.id,
      submit,
    };
    closeAccountsPanel();
  };

  const handleNavigate = (action: "back" | "forward" | "reload" | "home", url?: string) => {
    if (accountsOpen) {
      closeAccountsPanel();
    }
    void bridge.webappNavigate(app.id, action, url).catch((err) => onError?.(err));
  };

  return (
    <div
      className="relative flex min-w-0 flex-1 flex-col overflow-hidden"
      style={{ background: "var(--bg-stage)" }}
    >
      <header
        className="flex h-11 shrink-0 items-center justify-between gap-3 px-3.5"
        style={{
          background: "var(--bg-sidebar)",
          borderBottom: "1px solid var(--border-subtle)",
        }}
      >
        <div className="flex min-w-0 items-center gap-2">
          <button
            type="button"
            onClick={() => {
              closeAccountsPanel();
              if (isTauri()) void bridge.webappHideAll().catch(() => undefined);
              onBackToHub();
            }}
            title={t("webappBackToHub")}
            className="hover-surface inline-flex shrink-0 items-center gap-1.5 rounded-lg px-2 py-1 text-xs font-medium transition"
            style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
          >
            <LayoutGrid className="h-3.5 w-3.5" style={{ color: "var(--accent)" }} />
            <span className="hidden sm:inline">{t("webappBackToHub")}</span>
          </button>
          <span style={{ color: "var(--accent)" }}>{renderWebAppIcon(app.icon, "h-4 w-4 shrink-0")}</span>
          {allEmbeddedApps.length > 1 && onSelectApp ? (
            <select
              aria-label={t("webappMenuLabel")}
              value={app.id}
              onChange={(e) => {
                const picked = allEmbeddedApps.find((x) => x.id === e.target.value);
                if (picked) onSelectApp(picked);
              }}
              className="max-w-[190px] truncate rounded-lg px-2 py-0.5 text-sm font-semibold outline-none"
              style={{
                background: "var(--bg-surface)",
                border: "1px solid var(--border-subtle)",
                color: "var(--text-primary)",
              }}
            >
              {allEmbeddedApps.map((item) => (
                <option key={item.id} value={item.id}>
                  {item.name}
                </option>
              ))}
            </select>
          ) : (
            <span className="truncate text-sm font-semibold">{app.name}</span>
          )}
          <span
            className="hidden truncate rounded-full px-2 py-0.5 text-[11px] sm:inline-block"
            style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
          >
            {domain}
          </span>
        </div>

        <div className="flex shrink-0 items-center gap-1">
          {isTauri() && (
            <>
              <button
                type="button"
                onClick={() => handleNavigate("back")}
                title={t("back")}
                className="hover-surface rounded-lg p-1.5 transition"
                style={{ color: "var(--text-secondary)" }}
              >
                <ArrowLeft className="h-4 w-4" />
              </button>
              <button
                type="button"
                onClick={() => handleNavigate("forward")}
                title={t("forward")}
                className="hover-surface rounded-lg p-1.5 transition"
                style={{ color: "var(--text-secondary)" }}
              >
                <ArrowRight className="h-4 w-4" />
              </button>
              <button
                type="button"
                onClick={() => handleNavigate("reload")}
                title={t("reload")}
                className="hover-surface rounded-lg p-1.5 transition"
                style={{ color: "var(--text-secondary)" }}
              >
                <RefreshCw className="h-4 w-4" />
              </button>
              <button
                type="button"
                onClick={() => handleNavigate("home", app.url)}
                title={t("webappHomeTitle")}
                className="hover-surface rounded-lg p-1.5 transition"
                style={{ color: "var(--text-secondary)" }}
              >
                <Home className="h-4 w-4" />
              </button>
            </>
          )}
          <button
            type="button"
            onClick={() => {
              preservePanelErrorRef.current = false;
              setPanelNotice(null);
              setPanelError(null);
              setConfirmDeleteId(null);
              if (accountsOpen) {
                closeAccountsPanel();
              } else {
                setAccountsOpen(true);
              }
            }}
            title={t("webappAccountsBtnTitle")}
            aria-pressed={accountsOpen}
            className="hover-surface inline-flex items-center gap-1.5 rounded-lg px-2.5 py-1 text-xs font-medium transition"
            style={{
              background: accountsOpen ? "var(--accent)" : "var(--bg-surface)",
              color: accountsOpen ? "var(--text-on-accent)" : "var(--text-primary)",
              border: accountsOpen
                ? "1px solid var(--accent)"
                : "1px solid var(--border-subtle)",
            }}
          >
            <KeyRound
              className="h-3.5 w-3.5"
              style={{ color: accountsOpen ? "var(--text-on-accent)" : "var(--accent)" }}
            />
            <span className="hidden sm:inline">{t("webappAccountsBtn")}</span>
            {accounts.length > 0 && (
              <span
                className="inline-flex min-w-[18px] items-center justify-center rounded-full px-1.5 py-0.5 text-[10px] font-bold leading-none"
                style={{
                  background: accountsOpen ? "rgba(0, 0, 0, 0.2)" : "var(--accent-soft)",
                  color: accountsOpen ? "var(--text-on-accent)" : "var(--accent)",
                }}
              >
                {accounts.length}
              </span>
            )}
          </button>
          <button
            type="button"
            onClick={() => onOpenExternal(app.url)}
            title={t("openExternalBrowser")}
            className="hover-surface inline-flex items-center gap-1 rounded-lg px-2.5 py-1 text-xs font-medium transition"
            style={{ background: "var(--bg-surface)", color: "var(--accent)" }}
          >
            <ExternalLink className="h-3.5 w-3.5" />
            <span className="hidden md:inline">{t("openExternalBrowser")}</span>
          </button>
          <button
            type="button"
            onClick={() => {
              closeAccountsPanel();
              if (isTauri()) void bridge.webappHideAll().catch(() => undefined);
              onCloseToChat();
            }}
            title={t("returnToChat")}
            className="hover-surface rounded-lg p-1.5 transition"
            style={{ color: "var(--text-secondary)" }}
          >
            <X className="h-4 w-4" />
          </button>
        </div>
      </header>

      <div ref={hostRef} className="relative flex min-h-0 flex-1 items-center justify-center">
        {accountsOpen ? (
          <div
            className="absolute inset-0 z-10 overflow-y-auto p-5"
            style={{ background: "var(--bg-stage)" }}
          >
            <div className="mx-auto flex w-full max-w-2xl flex-col gap-4">
              {/* Panel Header */}
              <div
                className="flex flex-wrap items-center justify-between gap-3 rounded-2xl p-4"
                style={{
                  background: "var(--bg-sidebar)",
                  border: "1px solid var(--border-subtle)",
                }}
              >
                <div className="flex min-w-0 items-center gap-3">
                  <div
                    className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl"
                    style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
                  >
                    <KeyRound className="h-5 w-5" />
                  </div>
                  <div className="min-w-0">
                    <h2 className="truncate text-sm font-bold">{t("webappAccountsPanelTitle")}</h2>
                    <p
                      className="truncate text-xs"
                      style={{ color: "var(--text-secondary)" }}
                    >
                      {t("webappAccountsPanelSubtitle", { domain })}
                    </p>
                  </div>
                </div>

                <div className="flex shrink-0 items-center gap-2">
                  <button
                    type="button"
                    onClick={() => (formOpen && !editingAccount ? resetForm() : openAddForm())}
                    className="tf-btn inline-flex items-center gap-1.5 rounded-xl px-3 py-1.5 text-xs font-semibold"
                  >
                    <Plus className="h-3.5 w-3.5" />
                    <span>{t("webappAccountsAddBtn")}</span>
                  </button>
                  <button
                    type="button"
                    onClick={closeAccountsPanel}
                    title={t("close")}
                    className="hover-surface rounded-xl p-1.5 transition"
                    style={{
                      background: "var(--bg-surface)",
                      color: "var(--text-secondary)",
                    }}
                  >
                    <X className="h-4 w-4" />
                  </button>
                </div>
              </div>

              {panelError && (
                <div
                  className="flex items-center justify-between gap-2 rounded-xl px-3.5 py-2.5 text-xs"
                  style={{ background: "var(--danger-bg)", color: "var(--danger-text)" }}
                >
                  <span className="min-w-0 break-words">{panelError}</span>
                  <button
                    type="button"
                    onClick={() => setPanelError(null)}
                    className="shrink-0 rounded p-0.5 opacity-80 hover:opacity-100"
                  >
                    <X className="h-3.5 w-3.5" />
                  </button>
                </div>
              )}

              {panelNotice && (
                <div
                  className="flex items-center justify-between gap-2 rounded-xl px-3.5 py-2.5 text-xs font-medium"
                  style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
                >
                  <span className="inline-flex min-w-0 items-center gap-1.5">
                    <Check className="h-3.5 w-3.5 shrink-0" />
                    <span className="truncate">{panelNotice}</span>
                  </span>
                  <button
                    type="button"
                    onClick={() => setPanelNotice(null)}
                    className="shrink-0 rounded p-0.5 opacity-80 hover:opacity-100"
                  >
                    <X className="h-3.5 w-3.5" />
                  </button>
                </div>
              )}

              {/* Add / Edit Form */}
              {formOpen && (
                <form
                  onSubmit={(e) => void handleSaveAccount(e)}
                  className="space-y-3.5 rounded-2xl p-4 shadow-md"
                  style={{
                    background: "var(--bg-sidebar)",
                    border: "1px solid var(--border-subtle)",
                  }}
                >
                  <div className="flex items-center justify-between gap-2">
                    <div className="text-sm font-semibold">
                      {editingAccount ? t("webappAccountsEditTitle") : t("webappAccountsAddTitle")}
                    </div>
                    <button
                      type="button"
                      onClick={resetForm}
                      title={t("cancel")}
                      className="hover-menu rounded-lg p-1"
                      style={{ color: "var(--text-secondary)" }}
                    >
                      <X className="h-4 w-4" />
                    </button>
                  </div>

                  <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                    <label className="space-y-1">
                      <span
                        className="block text-[11px] font-medium"
                        style={{ color: "var(--text-secondary)" }}
                      >
                        {t("webappAccountsLabelField")}
                      </span>
                      <input
                        type="text"
                        value={label}
                        onChange={(e) => setLabel(e.target.value)}
                        placeholder={t("webappAccountsLabelPlaceholder")}
                        autoComplete="off"
                        className="w-full rounded-xl px-3 py-2 text-sm outline-none"
                        style={{
                          background: "var(--bg-surface)",
                          border: "1px solid var(--border-subtle)",
                          color: "var(--text-primary)",
                        }}
                      />
                    </label>

                    <label className="space-y-1">
                      <span
                        className="block text-[11px] font-medium"
                        style={{ color: "var(--text-secondary)" }}
                      >
                        {t("webappAccountsUsernameField")} *
                      </span>
                      <input
                        type="text"
                        value={username}
                        onChange={(e) => setUsername(e.target.value)}
                        placeholder={t("webappAccountsUsernamePlaceholder")}
                        autoComplete="off"
                        required
                        className="w-full rounded-xl px-3 py-2 text-sm outline-none"
                        style={{
                          background: "var(--bg-surface)",
                          border: "1px solid var(--border-subtle)",
                          color: "var(--text-primary)",
                        }}
                      />
                    </label>
                  </div>

                  <div className="space-y-1">
                    <div className="flex items-center justify-between gap-2">
                      <label
                        htmlFor="webapp-account-password"
                        className="text-[11px] font-medium"
                        style={{ color: "var(--text-secondary)" }}
                      >
                        {t("webappAccountsPasswordField")}
                      </label>
                      {editingAccount?.has_password && (
                        <button
                          type="button"
                          onClick={() => {
                            setClearPassword((prev) => {
                              const next = !prev;
                              if (next) setPassword("");
                              return next;
                            });
                          }}
                          className="text-[11px] font-medium transition hover:underline"
                          style={{
                            color: clearPassword ? "var(--danger-text)" : "var(--text-secondary)",
                          }}
                        >
                          {clearPassword ? t("cancel") : t("webappAccountsClearPassword")}
                        </button>
                      )}
                    </div>
                    <div
                      className="flex items-center gap-1 rounded-xl px-3"
                      style={{
                        background: "var(--bg-surface)",
                        border: "1px solid var(--border-subtle)",
                        opacity: clearPassword ? 0.65 : 1,
                      }}
                    >
                      <input
                        id="webapp-account-password"
                        type={showPassword ? "text" : "password"}
                        value={password}
                        onChange={(e) => {
                          if (clearPassword) setClearPassword(false);
                          setPassword(e.target.value);
                        }}
                        disabled={clearPassword}
                        placeholder={
                          clearPassword
                            ? t("webappAccountsPasswordWillClear")
                            : editingAccount?.has_password
                              ? t("webappAccountsPasswordKeepPlaceholder")
                              : t("webappAccountsPasswordPlaceholder")
                        }
                        autoComplete="new-password"
                        className="min-w-0 flex-1 bg-transparent py-2 text-sm outline-none"
                        style={{ color: "var(--text-primary)" }}
                      />
                      <button
                        type="button"
                        onClick={() => setShowPassword((v) => !v)}
                        disabled={clearPassword}
                        title={
                          showPassword
                            ? t("webappAccountsHidePassword")
                            : t("webappAccountsShowPassword")
                        }
                        aria-label={
                          showPassword
                            ? t("webappAccountsHidePassword")
                            : t("webappAccountsShowPassword")
                        }
                        className="hover-menu rounded-lg p-1.5 transition"
                        style={{ color: "var(--text-secondary)" }}
                      >
                        {showPassword ? (
                          <EyeOff className="h-4 w-4" />
                        ) : (
                          <Eye className="h-4 w-4" />
                        )}
                      </button>
                    </div>
                  </div>

                  <label
                    className="flex cursor-pointer items-start gap-2.5 rounded-xl p-3 transition"
                    style={{
                      background: "var(--bg-surface)",
                      border: "1px solid var(--border-subtle)",
                    }}
                  >
                    <input
                      type="checkbox"
                      checked={autoFill}
                      onChange={(e) => setAutoFill(e.target.checked)}
                      className="mt-0.5 h-4 w-4 shrink-0 accent-[var(--accent)]"
                    />
                    <span className="min-w-0">
                      <span className="block text-xs font-semibold">
                        {t("webappAccountsAutoFillLabel")}
                      </span>
                      <span
                        className="mt-0.5 block text-[11px] leading-relaxed"
                        style={{ color: "var(--text-secondary)" }}
                      >
                        {t("webappAccountsAutoFillDesc")}
                      </span>
                    </span>
                  </label>

                  {formError && (
                    <div
                      className="rounded-xl px-3 py-2 text-xs"
                      style={{ background: "var(--danger-bg)", color: "var(--danger-text)" }}
                    >
                      {formError}
                    </div>
                  )}

                  <div className="flex justify-end gap-2 pt-1">
                    <button
                      type="button"
                      onClick={resetForm}
                      disabled={saving}
                      className="hover-surface rounded-xl px-3.5 py-1.5 text-xs font-medium"
                      style={{ background: "var(--bg-surface)" }}
                    >
                      {t("cancel")}
                    </button>
                    <button
                      type="submit"
                      disabled={saving}
                      className="tf-btn rounded-xl px-4 py-1.5 text-xs font-semibold disabled:opacity-60"
                    >
                      {t("save")}
                    </button>
                  </div>
                </form>
              )}

              {/* Account List */}
              {accountsLoading && sortedAccounts.length === 0 ? (
                <div
                  className="rounded-2xl py-10 text-center text-xs"
                  style={{
                    background: "var(--bg-sidebar)",
                    border: "1px solid var(--border-subtle)",
                    color: "var(--text-secondary)",
                  }}
                >
                  {t("loading")}
                </div>
              ) : sortedAccounts.length === 0 ? (
                <div
                  className="flex flex-col items-center justify-center rounded-2xl px-6 py-12 text-center"
                  style={{
                    background: "var(--bg-sidebar)",
                    border: "1px dashed var(--border-subtle)",
                  }}
                >
                  <div
                    className="mb-3 flex h-12 w-12 items-center justify-center rounded-2xl"
                    style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
                  >
                    <KeyRound className="h-6 w-6" />
                  </div>
                  <div className="text-sm font-semibold">{t("webappAccountsEmptyTitle")}</div>
                  <p
                    className="mt-1 max-w-sm text-xs leading-relaxed"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    {t("webappAccountsEmptyDesc")}
                  </p>
                  {!formOpen && (
                    <button
                      type="button"
                      onClick={openAddForm}
                      className="tf-btn mt-4 inline-flex items-center gap-1.5 rounded-xl px-4 py-2 text-xs font-semibold"
                    >
                      <Plus className="h-4 w-4" />
                      <span>{t("webappAccountsAddBtn")}</span>
                    </button>
                  )}
                </div>
              ) : (
                <div className="space-y-2.5">
                  {sortedAccounts.map((acc) => {
                    const isDeleting = deletingId === acc.id;
                    const confirmingDelete = confirmDeleteId === acc.id;
                    const originMismatch = Boolean(appOrigin && acc.origin && acc.origin !== appOrigin);
                    return (
                      <div
                        key={acc.id}
                        className="flex flex-col gap-3 rounded-2xl p-3.5 transition sm:flex-row sm:items-center sm:justify-between"
                        style={{
                          background: "var(--bg-sidebar)",
                          border: acc.auto_fill
                            ? "1px solid var(--accent)"
                            : "1px solid var(--border-subtle)",
                        }}
                      >
                        <div className="flex min-w-0 items-start gap-3">
                          <div
                            className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl"
                            style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
                          >
                            <User className="h-4 w-4" />
                          </div>
                          <div className="min-w-0 space-y-1">
                            <div className="flex flex-wrap items-center gap-1.5">
                              <span className="truncate text-sm font-semibold">{acc.label}</span>
                              {acc.auto_fill && (
                                <span
                                  className="inline-flex shrink-0 items-center gap-1 rounded-full px-2 py-0.5 text-[10px] font-bold"
                                  style={{
                                    background: "var(--accent-soft)",
                                    color: "var(--accent)",
                                  }}
                                >
                                  <Sparkles className="h-2.5 w-2.5" />
                                  {t("webappAccountsBadgeAutoFill")}
                                </span>
                              )}
                              {acc.has_password && (
                                <span
                                  className="inline-flex shrink-0 items-center gap-1 rounded-full px-2 py-0.5 text-[10px] font-medium"
                                  style={{
                                    background: "var(--bg-surface)",
                                    color: "var(--text-secondary)",
                                  }}
                                >
                                  <Lock className="h-2.5 w-2.5" />
                                  {t("webappAccountsBadgePassword")}
                                </span>
                              )}
                              {originMismatch && (
                                <span
                                  className="inline-flex shrink-0 items-center rounded-full px-2 py-0.5 text-[10px] font-medium"
                                  style={{
                                    background: "var(--danger-bg)",
                                    color: "var(--danger-text)",
                                  }}
                                >
                                  {t("webappAccountsOriginMismatchBadge", { origin: acc.origin })}
                                </span>
                              )}
                            </div>
                            <div
                              className="truncate text-xs"
                              style={{ color: "var(--text-secondary)" }}
                            >
                              {acc.username}
                            </div>
                          </div>
                        </div>

                        <div className="flex shrink-0 flex-wrap items-center gap-1.5">
                          <button
                            type="button"
                            disabled={originMismatch}
                            onClick={() => handleFillAccount(acc, false)}
                            title={
                              originMismatch
                                ? t("webappAccountsOriginMismatchBadge", { origin: acc.origin })
                                : t("webappAccountsFillTitle")
                            }
                            className="tf-btn inline-flex items-center gap-1.5 rounded-xl px-3 py-1.5 text-xs font-semibold disabled:cursor-not-allowed disabled:opacity-45"
                          >
                            <KeyRound className="h-3.5 w-3.5" />
                            <span>{t("webappAccountsFillBtn")}</span>
                          </button>
                          <button
                            type="button"
                            disabled={originMismatch}
                            onClick={() => handleFillAccount(acc, true)}
                            title={
                              originMismatch
                                ? t("webappAccountsOriginMismatchBadge", { origin: acc.origin })
                                : t("webappAccountsFillSubmitTitle")
                            }
                            className="hover-surface inline-flex items-center gap-1.5 rounded-xl px-2.5 py-1.5 text-xs font-medium transition disabled:cursor-not-allowed disabled:opacity-45"
                            style={{
                              background: "var(--bg-surface)",
                              color: "var(--accent)",
                              border: "1px solid var(--border-subtle)",
                            }}
                          >
                            <LogIn className="h-3.5 w-3.5" />
                            <span>{t("webappAccountsFillSubmitBtn")}</span>
                          </button>
                          <button
                            type="button"
                            onClick={() => openEditForm(acc)}
                            title={t("edit")}
                            className="hover-menu rounded-lg p-1.5 transition"
                            style={{ color: "var(--text-secondary)" }}
                          >
                            <Pencil className="h-3.5 w-3.5" />
                          </button>
                          {confirmingDelete ? (
                            <div className="flex items-center gap-1">
                              <button
                                type="button"
                                disabled={isDeleting}
                                onClick={() => void handleDeleteAccount(acc)}
                                className="rounded-lg px-2 py-1 text-[11px] font-semibold transition disabled:opacity-60"
                                style={{
                                  background: "var(--danger-bg)",
                                  color: "var(--danger-text)",
                                }}
                              >
                                {t("webappAccountsConfirmDelete")}
                              </button>
                              <button
                                type="button"
                                onClick={() => setConfirmDeleteId(null)}
                                className="hover-menu rounded-lg p-1"
                                style={{ color: "var(--text-secondary)" }}
                                title={t("cancel")}
                              >
                                <X className="h-3.5 w-3.5" />
                              </button>
                            </div>
                          ) : (
                            <button
                              type="button"
                              onClick={() => setConfirmDeleteId(acc.id)}
                              title={t("delete")}
                              className="hover-menu rounded-lg p-1.5 text-rose-400 transition"
                            >
                              <Trash2 className="h-3.5 w-3.5" />
                            </button>
                          )}
                        </div>
                      </div>
                    );
                  })}
                </div>
              )}

              {/* DPAPI + Native WebView2 Security Info Note */}
              <div
                className="flex items-start gap-2.5 rounded-2xl px-4 py-3 text-xs leading-relaxed"
                style={{
                  background: "var(--bg-sidebar)",
                  border: "1px solid var(--border-subtle)",
                  color: "var(--text-secondary)",
                }}
              >
                <ShieldCheck
                  className="mt-0.5 h-4 w-4 shrink-0"
                  style={{ color: "var(--accent)" }}
                />
                <p>{t("webappAccountsSecurityNote")}</p>
              </div>
            </div>
          </div>
        ) : !isTauri() ? (
          <div className="max-w-md space-y-3 p-6 text-center">
            <div
              className="mx-auto flex h-12 w-12 items-center justify-center rounded-2xl"
              style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
            >
              {renderWebAppIcon(app.icon, "h-6 w-6")}
            </div>
            <div className="text-base font-semibold">{app.name}</div>
            <p className="text-xs leading-relaxed" style={{ color: "var(--text-secondary)" }}>
              {t("webappPreviewHint")}
            </p>
            <button
              type="button"
              onClick={() => onOpenExternal(app.url)}
              className="tf-btn inline-flex items-center gap-1.5 rounded-xl px-4 py-2 text-xs font-semibold"
            >
              <ExternalLink className="h-4 w-4" />
              <span>{t("openDomain", { domain })}</span>
            </button>
          </div>
        ) : openError ? (
          <div className="max-w-md space-y-3 p-6 text-center">
            <div className="text-xs font-medium text-rose-400">{openError}</div>
            <div className="flex flex-wrap items-center justify-center gap-2">
              <button
                type="button"
                onClick={() => {
                  setOpenError(null);
                  setRetrySeq((n) => n + 1);
                }}
                className="tf-btn inline-flex items-center gap-1.5 rounded-xl px-3.5 py-2 text-xs font-semibold"
              >
                <RefreshCw className="h-3.5 w-3.5" />
                <span>{t("reload")}</span>
              </button>
              <button
                type="button"
                onClick={() => onOpenExternal(app.url)}
                className="hover-surface inline-flex items-center gap-1.5 rounded-xl px-3.5 py-2 text-xs font-medium"
                style={{ background: "var(--bg-surface)", color: "var(--accent)" }}
              >
                <ExternalLink className="h-3.5 w-3.5" />
                <span>{t("openExternalBrowser")}</span>
              </button>
            </div>
          </div>
        ) : (
          <div className="text-xs opacity-60" style={{ color: "var(--text-secondary)" }}>
            {t("loadingDomain", { domain })}
          </div>
        )}
      </div>
    </div>
  );
}
