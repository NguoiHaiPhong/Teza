import { useEffect } from "react";
import {
  Archive,
  ArrowDownToLine,
  Bookmark,
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
import { ThreadAvatar } from "./ThreadAvatar";

export interface NavDrawerProps {
  open: boolean;
  onClose: () => void;
  myProfile: AccountProfile | null;
  connStatus: ConnectionStatus;
  theme: ThemeMode;
  appVersion: string;
  availableUpdate: AppUpdateInfo | null;
  archivedCount: number;
  hasPasscode: boolean;
  pendingFriendRequestsCount?: number;
  onOpenMyProfile: () => void;
  onOpenSavedMessages: () => void;
  onOpenArchived: () => void;
  onOpenFindUser: () => void;
  onOpenFriendRequests?: () => void;
  onOpenFolders: () => void;
  onLockApp: () => void;
  onOpenSettings: (
    tab?:
      | "account"
      | "notifications"
      | "appearance"
      | "folders"
      | "webapps"
      | "calls"
      | "translate"
      | "network"
      | "system",
  ) => void;
  onCycleTheme: () => void;
  onOpenUpdateModal: () => void;
  onLogout: () => void;
}

export function NavDrawer({
  open,
  onClose,
  myProfile,
  connStatus,
  theme,
  appVersion,
  availableUpdate,
  archivedCount,
  hasPasscode,
  pendingFriendRequestsCount = 0,
  onOpenMyProfile,
  onOpenSavedMessages,
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
  useEffect(() => {
    if (!open) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, onClose]);

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
        className="flex h-full w-72 max-w-[85vw] flex-col shadow-2xl"
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

          <button
            type="button"
            onClick={() => {
              onClose();
              onLogout();
            }}
            className="hover-menu flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left font-medium transition"
            style={{ color: "var(--danger-text)" }}
          >
            <LogOut className="h-4 w-4 shrink-0" />
            <span>{t("logoutMenu")}</span>
          </button>
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
