import { useEffect, useRef, useState } from "react";
import {
  AlertTriangle,
  Archive,
  ArrowDownToLine,
  Bookmark,
  Bot,
  CalendarClock,
  FolderKanban,
  Languages,
  Lock,
  LogOut,
  Moon,
  Settings,
  Sun,
  UserCheck,
  UserPlus,
  X,
} from "lucide-react";
import { useI18n } from "../i18n";
import type { AccountProfile, AppUpdateInfo, ConnectionStatus, ThemeMode } from "../types";
import { isImeComposingEvent } from "../utils/searchNormalize";
import type { SettingsTab } from "./SettingsModal";
import { ThreadAvatar } from "./ThreadAvatar";

const FOCUSABLE_SELECTOR =
  'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])';

function getVisibleFocusables(root: HTMLElement | null): HTMLElement[] {
  if (!root) return [];
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
    (el) => el.getClientRects().length > 0,
  );
}

export interface NavDrawerProps {
  open: boolean;
  isLocked?: boolean;
  onClose: () => void;
  myProfile: AccountProfile | null;
  connStatus: ConnectionStatus;
  theme: ThemeMode;
  appVersion: string;
  availableUpdate: AppUpdateInfo | null;
  archivedCount: number;
  hasPasscode: boolean;
  pendingFriendRequestsCount?: number;
  aiEnabled?: boolean;
  pendingSchedulesCount?: number;
  attentionScheduleCount?: number;
  onOpenMyProfile: () => void;
  onOpenSavedMessages: () => void;
  onOpenAiChat?: () => void;
  onOpenScheduledMessages?: () => void;
  onOpenArchived: () => void;
  onOpenFindUser: () => void;
  onOpenFriendRequests?: () => void;
  onOpenFolders: () => void;
  onLockApp: () => void;
  onOpenSettings: (tab?: SettingsTab) => void;
  onCycleTheme: () => void;
  onOpenUpdateModal: () => void;
  onLogout: () => void;
}

export function NavDrawer({
  open,
  isLocked = false,
  onClose,
  myProfile,
  connStatus,
  theme,
  appVersion,
  availableUpdate,
  archivedCount,
  hasPasscode,
  pendingFriendRequestsCount = 0,
  aiEnabled = false,
  pendingSchedulesCount = 0,
  attentionScheduleCount = 0,
  onOpenMyProfile,
  onOpenSavedMessages,
  onOpenAiChat,
  onOpenScheduledMessages,
  onOpenArchived,
  onOpenFindUser,
  onOpenFriendRequests,
  onOpenFolders,
  onLockApp,
  onOpenSettings,
  onCycleTheme,
  onOpenUpdateModal,
  onLogout,
}: NavDrawerProps) {
  const { t } = useI18n();
  const [confirmLogout, setConfirmLogout] = useState(false);
  const drawerRef = useRef<HTMLElement | null>(null);
  const prevFocusRef = useRef<HTMLElement | null>(null);
  const logoutConfirmRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!open) {
      setConfirmLogout(false);
      return;
    }
    prevFocusRef.current =
      document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const focusTimer = window.setTimeout(() => {
      const root = drawerRef.current;
      if (!root) return;
      const focusables = getVisibleFocusables(root);
      (focusables[0] ?? root).focus();
    }, 20);
    return () => {
      window.clearTimeout(focusTimer);
      const prev = prevFocusRef.current;
      prevFocusRef.current = null;
      if (prev && prev.isConnected) {
        prev.focus();
      }
    };
  }, [open]);

  useEffect(() => {
    if (confirmLogout) {
      logoutConfirmRef.current?.scrollIntoView({ behavior: "smooth", block: "nearest" });
    }
  }, [confirmLogout]);

  useEffect(() => {
    if (!open || isLocked) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (isLocked) return;
      if (e.key === "Tab") {
        const root = drawerRef.current;
        if (!root) return;
        const focusables = getVisibleFocusables(root);
        if (focusables.length === 0) {
          e.preventDefault();
          root.focus();
          return;
        }
        const first = focusables[0];
        const last = focusables[focusables.length - 1];
        const active = document.activeElement as HTMLElement | null;
        if (!active || !root.contains(active)) {
          e.preventDefault();
          (e.shiftKey ? last : first).focus();
          return;
        }
        if (e.shiftKey && active === first) {
          e.preventDefault();
          last.focus();
        } else if (!e.shiftKey && active === last) {
          e.preventDefault();
          first.focus();
        }
        return;
      }

      if (isImeComposingEvent(e)) return;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        if (confirmLogout) {
          setConfirmLogout(false);
        } else {
          onClose();
        }
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, isLocked, confirmLogout, onClose]);

  if (!open) return null;

  const themeLabels: Record<ThemeMode, string> = {
    triforce: t("themeObsidianGold"),
    "night-blue": t("themeNightBlue"),
    dark: t("themeDarkViolet"),
    light: t("themeLight"),
  };

  const statusText =
    connStatus === "connected"
      ? t("statusOnline")
      : connStatus === "connecting"
        ? t("statusConnecting")
        : t("statusReconnecting");
  const statusColor =
    connStatus === "connected"
      ? "#10b981"
      : connStatus === "connecting"
        ? "#f59e0b"
        : "#ef4444";

  return (
    <div
      className="fixed inset-0 z-50 flex bg-black/50 backdrop-blur-[1px]"
      onClick={onClose}
    >
      <aside
        ref={drawerRef}
        role="dialog"
        aria-modal="true"
        aria-label={t("menuMainSettings")}
        tabIndex={-1}
        className="flex h-full w-72 max-w-[85vw] flex-col shadow-2xl outline-none"
        style={{
          background: "var(--bg-sidebar)",
          borderRight: "1px solid var(--border-subtle)",
          color: "var(--text-primary)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        {/* User Profile Header */}
        <div
          className="relative px-4 pb-4 pt-5"
          style={{
            background: "linear-gradient(135deg, var(--accent-soft), transparent)",
            borderBottom: "1px solid var(--border-subtle)",
          }}
        >
          <button
            type="button"
            onClick={onClose}
            title={t("close")}
            className="hover-menu absolute right-3 top-3 rounded-lg p-1.5"
            style={{ color: "var(--text-secondary)" }}
          >
            <X className="h-4 w-4" />
          </button>

          <button
            type="button"
            onClick={() => {
              onOpenMyProfile();
              onClose();
            }}
            className="flex w-full items-center gap-3 text-left transition hover:opacity-90"
            title={t("myProfileMenu")}
          >
            <ThreadAvatar
              avatar={myProfile?.avatar || ""}
              title={myProfile?.display_name || "Teza"}
              kind="direct"
              size="lg"
            />
            <div className="min-w-0 flex-1 pr-4">
              <div className="truncate text-[15px] font-bold">
                {myProfile?.display_name || "Tài khoản Teza"}
              </div>
              {myProfile?.uid && (
                <div className="truncate text-xs" style={{ color: "var(--text-secondary)" }}>
                  UID: {myProfile.uid}
                </div>
              )}
              <div className="mt-1 flex items-center gap-1.5 text-[11.5px] font-medium">
                <span
                  className="h-2 w-2 shrink-0 rounded-full"
                  style={{ background: statusColor }}
                />
                <span style={{ color: statusColor }}>{statusText}</span>
              </div>
            </div>
          </button>
        </div>

        {/* Navigation Items */}
        <nav className="flex-1 space-y-1 overflow-y-auto px-2.5 py-3 text-[13.5px]">
          <button
            type="button"
            onClick={() => {
              onOpenSavedMessages();
              onClose();
            }}
            className="hover-menu flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left font-medium transition"
          >
            <Bookmark className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
            <span>{t("savedMessages")}</span>
          </button>

          {onOpenAiChat && (
            <button
              type="button"
              onClick={() => {
                onOpenAiChat();
                onClose();
              }}
              className="hover-menu flex w-full items-center justify-between gap-2 rounded-xl px-3 py-2.5 text-left font-medium transition"
            >
              <span className="flex items-center gap-3">
                <Bot className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
                <span>{t("navAiAssistantMenu")}</span>
              </span>
              {aiEnabled ? (
                <span
                  className="h-2 w-2 shrink-0 rounded-full bg-emerald-400"
                  title="AI"
                />
              ) : (
                <span
                  className="rounded-lg px-2 py-0.5 text-[10.5px]"
                  style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
                >
                  {t("navAiOptionalBadge")}
                </span>
              )}
            </button>
          )}

          {onOpenScheduledMessages && (
            <button
              type="button"
              onClick={() => {
                onOpenScheduledMessages();
                onClose();
              }}
              className="hover-menu flex w-full items-center justify-between gap-2 rounded-xl px-3 py-2.5 text-left font-medium transition"
            >
              <span className="flex items-center gap-3">
                <CalendarClock className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
                <span>{t("navScheduledMessagesMenu")}</span>
              </span>
              {(attentionScheduleCount > 0 || pendingSchedulesCount > 0) && (
                <span className="flex shrink-0 items-center gap-1">
                  {attentionScheduleCount > 0 && (
                    <span
                      title={t("navSchedulesAttentionTitle", { count: attentionScheduleCount })}
                      className="inline-flex items-center gap-1 rounded-full bg-amber-500/20 px-2 py-0.5 text-[11px] font-bold text-amber-400"
                      style={{ border: "1px solid rgba(245, 158, 11, 0.45)" }}
                    >
                      <AlertTriangle className="h-3 w-3 shrink-0" />
                      <span>{attentionScheduleCount > 99 ? "99+" : attentionScheduleCount}</span>
                    </span>
                  )}
                  {pendingSchedulesCount > 0 && (
                    <span
                      className="rounded-full px-2 py-0.5 text-[11px] font-bold"
                      style={{ background: "var(--accent)", color: "var(--text-on-accent)" }}
                    >
                      {pendingSchedulesCount > 99 ? "99+" : pendingSchedulesCount}
                    </span>
                  )}
                </span>
              )}
            </button>
          )}

          <button
            type="button"
            onClick={() => {
              onOpenFindUser();
              onClose();
            }}
            className="hover-menu flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left font-medium transition"
          >
            <UserPlus className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
            <span>{t("findNewFriendMenu")}</span>
          </button>

          <button
            type="button"
            onClick={() => {
              onOpenFriendRequests?.();
              onClose();
            }}
            className="hover-menu flex w-full items-center justify-between gap-2 rounded-xl px-3 py-2.5 text-left font-medium transition"
          >
            <span className="flex items-center gap-3">
              <UserCheck className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
              <span>{t("friendRequestsMenu")}</span>
            </span>
            {pendingFriendRequestsCount > 0 && (
              <span
                className="rounded-full px-2 py-0.5 text-[11px] font-bold"
                style={{ background: "var(--accent)", color: "var(--text-on-accent)" }}
              >
                {pendingFriendRequestsCount > 99 ? "99+" : pendingFriendRequestsCount}
              </span>
            )}
          </button>

          <button
            type="button"
            onClick={() => {
              onOpenFolders();
              onClose();
            }}
            className="hover-menu flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left font-medium transition"
          >
            <FolderKanban className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
            <span>{t("manageFoldersMenu")}</span>
          </button>

          <button
            type="button"
            onClick={() => {
              onOpenArchived();
              onClose();
            }}
            className="hover-menu flex w-full items-center justify-between gap-2 rounded-xl px-3 py-2.5 text-left font-medium transition"
          >
            <span className="flex items-center gap-3">
              <Archive className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
              <span>{t("archivedChatsMenu")}</span>
            </span>
            {archivedCount > 0 && (
              <span
                className="rounded-full px-2 py-0.5 text-[11px] font-semibold"
                style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
              >
                {archivedCount}
              </span>
            )}
          </button>

          {hasPasscode && (
            <button
              type="button"
              onClick={() => {
                onLockApp();
                onClose();
              }}
              className="hover-menu flex w-full items-center justify-between gap-2 rounded-xl px-3 py-2.5 text-left font-medium transition"
            >
              <span className="flex items-center gap-3">
                <Lock className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
                <span>{t("lockApp")}</span>
              </span>
              <span className="text-[11px] opacity-65">Ctrl+L</span>
            </button>
          )}

          <button
            type="button"
            onClick={() => {
              onOpenSettings("notifications");
              onClose();
            }}
            className="hover-menu flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left font-medium transition"
          >
            <Settings className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
            <span>{t("settingsMenu")}</span>
          </button>

          <button
            type="button"
            onClick={() => {
              onOpenSettings("translate");
              onClose();
            }}
            className="hover-menu flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left font-medium transition"
          >
            <Languages className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
            <span>{t("translateMenu")}</span>
          </button>

          <button
            type="button"
            onClick={onCycleTheme}
            className="hover-menu flex w-full items-center justify-between gap-2 rounded-xl px-3 py-2.5 text-left font-medium transition"
          >
            <span className="flex items-center gap-3">
              {theme === "light" ? (
                <Sun className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
              ) : (
                <Moon className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
              )}
              <span>{t("themeModeMenu")}</span>
            </span>
            <span
              className="rounded-lg px-2 py-0.5 text-[11px]"
              style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
            >
              {themeLabels[theme]}
            </span>
          </button>

          <button
            type="button"
            onClick={() => {
              onOpenUpdateModal();
              onClose();
            }}
            className="hover-menu flex w-full items-center justify-between gap-2 rounded-xl px-3 py-2.5 text-left font-medium transition"
          >
            <span className="flex items-center gap-3">
              <ArrowDownToLine className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
              <span>{t("checkUpdatesMenu")}</span>
            </span>
            {availableUpdate && (
              <span className="rounded-full bg-emerald-500/20 px-2 py-0.5 text-[10.5px] font-bold text-emerald-400">
                v{availableUpdate.version}
              </span>
            )}
          </button>

          <div className="my-2 h-px" style={{ background: "var(--border-subtle)" }} />

          {!confirmLogout ? (
            <button
              type="button"
              onClick={() => setConfirmLogout(true)}
              className="hover-menu flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left font-medium transition"
              style={{ color: "var(--danger-text)" }}
            >
              <LogOut className="h-4 w-4 shrink-0" />
              <span>{t("logoutMenu")}</span>
            </button>
          ) : (
            <div
              ref={logoutConfirmRef}
              className="space-y-2 rounded-xl p-3 text-xs"
              style={{
                background: "var(--danger-bg)",
                border: "1px solid rgba(239, 68, 68, 0.35)",
              }}
            >
              <div className="font-semibold" style={{ color: "var(--danger-text)" }}>
                {t("lockScreenConfirmLogout")}
              </div>
              <div className="flex items-center justify-end gap-2">
                <button
                  type="button"
                  onClick={() => setConfirmLogout(false)}
                  className="hover-menu rounded-lg px-2.5 py-1.5 font-medium"
                  style={{
                    background: "var(--bg-sidebar)",
                    color: "var(--text-primary)",
                  }}
                >
                  {t("cancel")}
                </button>
                <button
                  type="button"
                  onClick={() => {
                    setConfirmLogout(false);
                    onClose();
                    onLogout();
                  }}
                  className="inline-flex items-center gap-1 rounded-lg bg-rose-600 px-2.5 py-1.5 font-semibold text-white transition hover:bg-rose-500"
                >
                  <LogOut className="h-3.5 w-3.5" />
                  <span>{t("logoutMenu")}</span>
                </button>
              </div>
            </div>
          )}
        </nav>

        {/* Version Footer */}
        <div
          className="px-4 py-2.5 text-xs"
          style={{
            borderTop: "1px solid var(--border-subtle)",
            color: "var(--text-secondary)",
          }}
        >
          Teza v{appVersion || "0.6.1"}
        </div>
      </aside>
    </div>
  );
}
