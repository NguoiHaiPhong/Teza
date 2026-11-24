import { useEffect, useRef, useState } from "react";
import {
  AlertTriangle,
  ArrowDownToLine,
  AtSign,
  Bell,
  Bookmark,
  CheckCircle2,
  ExternalLink,
  Eye,
  FolderKanban,
  Globe,
  Headphones,
  Keyboard,
  Languages,
  Loader2,
  Lock,
  LogOut,
  MessageSquare,
  Mic,
  Monitor,
  Network,
  Palette,
  Phone,
  Plus,
  RefreshCw,
  Send,
  SlidersHorizontal,
  Sparkles,
  Trash2,
  User,
  Users,
  Volume2,
  X,
  Zap,
} from "lucide-react";
import { bridge } from "../bridge";
import { UI_LANGUAGE_OPTIONS, useI18n, type TranslationKey } from "../i18n";
import { useEmojiStyle } from "../utils/emojiStyleContext";
import { RetroEmoticon } from "./RetroEmoticon";
import type {
  AccountProfile,
  AppUpdateInfo,
  BuiltInRailFolderId,
  CallAudioTestResultDto,
  CallDevicesDto,
  ConnectionStatus,
  NotificationPrefs,
  NotifyBadgeMode,
  NotifyPosition,
  NotifyPreviewMode,
  PasscodeConfig,
  RailDisplayMode,
  RelayConfigSummary,
  RelayMode,
  ThemeMode,
  TranslateConfigSummary,
} from "../types";
import { resolveEffectiveCallDevices } from "../utils/callState";
import { hashPasscode, validatePasscodeCandidate, verifyPasscode } from "../utils/passcode";
import {
  extractWebAppDomain,
  normalizeWebUrl,
  type WebAppItem,
  type WebAppOpenMode,
  type WebAppsConfig,
} from "../utils/webApps";
import { ThreadAvatar } from "./ThreadAvatar";
import { WEBAPP_ICON_OPTIONS, renderWebAppIcon } from "./WebAppWorkspace";

export type FontSizeScale = "sm" | "md" | "lg";
export type DensityMode = "comfortable" | "compact";
export type SettingsTab =
  | "account"
  | "notifications"
  | "appearance"
  | "folders"
  | "webapps"
  | "calls"
  | "translate"
  | "network"
  | "system";

interface Props {
  open: boolean;
  initialTab?: SettingsTab;
  myProfile: AccountProfile | null;
  connStatus: ConnectionStatus;
  onOpenMyProfile: () => void;
  onLogout: () => void;
  theme: ThemeMode;
  onThemeChange: (t: ThemeMode) => void;
  fontSize: FontSizeScale;
  onFontSizeChange: (f: FontSizeScale) => void;
  density: DensityMode;
  onDensityChange: (d: DensityMode) => void;
  fxEnabled: boolean;
  onFxChange: (v: boolean) => void;
  sendOnEnter: boolean;
  onSendOnEnterChange: (v: boolean) => void;
  notifyPrefs: NotificationPrefs;
  onNotifyPrefsChange: (next: NotificationPrefs) => void;
  onTestNotification: () => void;
  onTestSound: () => void;
  closeToTray: boolean;
  onCloseToTrayChange: (v: boolean) => void;
  customFoldersCount: number;
  hiddenBuiltInFolders: BuiltInRailFolderId[];
  onToggleBuiltInFolder: (id: BuiltInRailFolderId) => void;
  railDisplayMode: RailDisplayMode;
  onRailDisplayModeChange: (m: RailDisplayMode) => void;
  onOpenFoldersModal: () => void;
  webAppsCfg: WebAppsConfig;
  onWebAppsCfgChange: (next: WebAppsConfig) => void;
  onOpenWebApp?: (app: WebAppItem) => void;
  passcodeCfg: PasscodeConfig;
  onUpdatePasscode: (next: PasscodeConfig) => void;
  onLockNow: () => void;
  translateCfg: TranslateConfigSummary | null;
  onOpenTranslateModal: () => void;
  relayCfg: RelayConfigSummary | null;
  onRelaySaved: (cfg: RelayConfigSummary) => void;
  focusRelay?: boolean;
  callDevices: CallDevicesDto | null;
  callDevicesLoading: boolean;
  onRefreshCallDevices: () => void;
  onSelectCallDevices: (input: string | null, output: string | null) => Promise<void> | void;
  appVersion: string;
  availableUpdate: AppUpdateInfo | null;
  onOpenUpdateModal: () => void;
  onClose: () => void;
}

const THEMES: { id: ThemeMode; labelKey: TranslationKey; dot: string }[] = [
  { id: "triforce", labelKey: "themeObsidianGold", dot: "#cfa13a" },
  { id: "night-blue", labelKey: "themeNightBlue", dot: "#4c82b8" },
  { id: "dark", labelKey: "themeDarkViolet", dot: "#7c68cf" },
  { id: "light", labelKey: "themeLight", dot: "#2e6da4" },
];

const NAV_TABS: { id: SettingsTab; labelKey: TranslationKey; icon: typeof Bell }[] = [
  { id: "notifications", labelKey: "tabNotifications", icon: Bell },
  { id: "appearance", labelKey: "tabAppearance", icon: Palette },
  { id: "folders", labelKey: "tabFolders", icon: FolderKanban },
  { id: "webapps", labelKey: "tabWebApps", icon: Globe },
  { id: "calls", labelKey: "tabCalls", icon: Phone },
  { id: "translate", labelKey: "tabTranslate", icon: Languages },
  { id: "network", labelKey: "tabNetwork", icon: Network },
  { id: "account", labelKey: "tabAccount", icon: User },
  { id: "system", labelKey: "tabSystem", icon: SlidersHorizontal },
];

const POSITION_CORNERS: { id: NotifyPosition; labelKey: TranslationKey }[] = [
  { id: "top-left", labelKey: "posTopLeft" },
  { id: "top-right", labelKey: "posTopRight" },
  { id: "bottom-left", labelKey: "posBottomLeft" },
  { id: "bottom-right", labelKey: "posBottomRight" },
];

function ToggleRow({
  icon,
  label,
  desc,
  checked,
  onChange,
  action,
}: {
  icon: React.ReactNode;
  label: string;
  desc?: string;
  checked: boolean;
  onChange: (v: boolean) => void;
  action?: React.ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-3 rounded-xl px-3 py-2.5 transition hover:bg-black/10">
      <label className="flex min-w-0 flex-1 cursor-pointer items-center gap-2.5">
        <span className="shrink-0" style={{ color: "var(--accent)" }}>
          {icon}
        </span>
        <div className="min-w-0 flex-1">
          <div className="text-[13.5px] font-medium">{label}</div>
          {desc && (
            <div className="text-xs" style={{ color: "var(--text-secondary)" }}>
              {desc}
            </div>
          )}
        </div>
        <input
          type="checkbox"
          checked={checked}
          onChange={(e) => onChange(e.target.checked)}
          className="h-4 w-4 shrink-0 cursor-pointer accent-[var(--accent)]"
        />
      </label>
      {action}
    </div>
  );
}

export function SettingsModal({
  open,
  initialTab = "notifications",
  myProfile,
  connStatus,
  onOpenMyProfile,
  onLogout,
  theme,
  onThemeChange,
  fontSize,
  onFontSizeChange,
  density,
  onDensityChange,
  fxEnabled,
  onFxChange,
  sendOnEnter,
  onSendOnEnterChange,
  notifyPrefs,
  onNotifyPrefsChange,
  onTestNotification,
  onTestSound,
  closeToTray,
  onCloseToTrayChange,
  customFoldersCount,
  hiddenBuiltInFolders,
  onToggleBuiltInFolder,
  railDisplayMode,
  onRailDisplayModeChange,
  onOpenFoldersModal,
  webAppsCfg,
  onWebAppsCfgChange,
  onOpenWebApp,
  passcodeCfg,
  onUpdatePasscode,
  onLockNow,
  translateCfg,
  onOpenTranslateModal,
  relayCfg,
  onRelaySaved,
  focusRelay = false,
  callDevices,
  callDevicesLoading,
  onRefreshCallDevices,
  onSelectCallDevices,
  appVersion,
  availableUpdate,
  onOpenUpdateModal,
  onClose,
}: Props) {
  const { lang, setLang, translateCode, t } = useI18n();
  const { emojiStyle, setEmojiStyle } = useEmojiStyle();
  const [activeTab, setActiveTab] = useState<SettingsTab>(initialTab);
  const [callDevicesSaved, setCallDevicesSaved] = useState(false);
  const [callAudioTesting, setCallAudioTesting] = useState(false);
  const [callAudioTestResult, setCallAudioTestResult] = useState<{
    ok: boolean;
    warn?: boolean;
    text: string;
  } | null>(null);
  const effectiveCallDevices = resolveEffectiveCallDevices(callDevices);
  const [relayEnabled, setRelayEnabled] = useState(false);
  const [relayMode, setRelayMode] = useState<RelayMode>("socks5");
  const [relayHost, setRelayHost] = useState("");
  const [relayPort, setRelayPort] = useState("1088");
  const [relayUser, setRelayUser] = useState("");
  const [relayPassword, setRelayPassword] = useState("");
  const [clearRelayPw, setClearRelayPw] = useState(false);
  const [routeMedia, setRouteMedia] = useState(true);
  const [relayTesting, setRelayTesting] = useState(false);
  const [relaySaving, setRelaySaving] = useState(false);
  const [relayFeedback, setRelayFeedback] = useState<{ ok: boolean; text: string } | null>(null);
  const relaySectionRef = useRef<HTMLDivElement | null>(null);
  const [currentPin, setCurrentPin] = useState("");
  const [newPin, setNewPin] = useState("");
  const [confirmPin, setConfirmPin] = useState("");
  const [pinBusy, setPinBusy] = useState(false);
  const [pinFeedback, setPinFeedback] = useState<{ ok: boolean; text: string } | null>(null);
  const [waEditingId, setWaEditingId] = useState<string | null>(null);
  const [waName, setWaName] = useState("");
  const [waUrl, setWaUrl] = useState("");
  const [waIcon, setWaIcon] = useState("globe");
  const [waMode, setWaMode] = useState<WebAppOpenMode>("embedded");
  const [waError, setWaError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setActiveTab(focusRelay ? "network" : initialTab);
    setCallDevicesSaved(false);
    setCallAudioTesting(false);
    setCallAudioTestResult(null);
    setRelayEnabled(relayCfg?.enabled ?? false);
    setRelayMode(relayCfg?.mode ?? "socks5");
    setRelayHost(relayCfg?.host ?? "");
    setRelayPort(String(relayCfg?.port || 1088));
    setRelayUser(relayCfg?.username ?? "");
    setRelayPassword("");
    setClearRelayPw(false);
    setRouteMedia(relayCfg?.route_webview_media ?? true);
    setRelayFeedback(null);
    setCurrentPin("");
    setNewPin("");
    setConfirmPin("");
    setPinFeedback(null);
    setWaEditingId(null);
    setWaName("");
    setWaUrl("");
    setWaIcon("globe");
    setWaMode("embedded");
    setWaError(null);
    if (focusRelay) {
      setTimeout(() => {
        relaySectionRef.current?.scrollIntoView({ behavior: "smooth", block: "nearest" });
      }, 40);
    }
  }, [open, relayCfg, focusRelay, initialTab]);

  useEffect(() => {
    if (open && activeTab === "calls") {
      onRefreshCallDevices();
    }
  }, [open, activeTab, onRefreshCallDevices]);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, onClose]);

  if (!open) return null;

  const patchNotify = (patch: Partial<NotificationPrefs>) => {
    onNotifyPrefsChange({ ...notifyPrefs, ...patch });
  };

  const buildRelayPayload = () => {
    const parsedPort = Number(relayPort.trim());
    const safePort = Number.isInteger(parsedPort) && parsedPort >= 1 && parsedPort <= 65535 ? parsedPort : 0;
    return {
      enabled: relayEnabled,
      mode: relayMode,
      host: relayHost.trim(),
      port: safePort,
      username: relayUser.trim(),
      password: relayPassword || undefined,
      clear_password: clearRelayPw,
      route_webview_media: routeMedia,
    };
  };

  const handleTestRelay = async () => {
    setRelayTesting(true);
    setRelayFeedback(null);
    try {
      const payload = { ...buildRelayPayload(), enabled: true };
      const res = await bridge.testRelayConnection(payload);
      setRelayFeedback({
        ok: true,
        text: t("relayTestSuccess", { host: payload.host, port: payload.port, ms: res.latency_ms }),
      });
    } catch (err) {
      setRelayFeedback({ ok: false, text: String(err) });
    } finally {
      setRelayTesting(false);
    }
  };

  const handleSaveRelay = async () => {
    setRelaySaving(true);
    setRelayFeedback(null);
    try {
      const saved = await bridge.saveRelayConfig(buildRelayPayload());
      onRelaySaved(saved);
      setRelayPassword("");
      setClearRelayPw(false);
      setRelayFeedback({
        ok: true,
        text: saved.enabled
          ? t("relaySaveOnMsg", { mode: saved.mode.toUpperCase(), host: saved.host, port: saved.port })
          : t("relaySaveOffMsg"),
      });
    } catch (err) {
      setRelayFeedback({ ok: false, text: String(err) });
    } finally {
      setRelaySaving(false);
    }
  };

  const activeTitleKey = NAV_TABS.find((tab) => tab.id === activeTab)?.labelKey;
  const activeTitle = activeTitleKey ? t(activeTitleKey) : t("settingsTeza");

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/55 p-4 backdrop-blur-xs"
      onClick={onClose}
    >
      <div
        className="flex h-[82vh] max-h-[680px] w-full max-w-3xl flex-col overflow-hidden rounded-2xl shadow-2xl"
        style={{
          background: "var(--bg-sidebar)",
          border: "1px solid var(--border-subtle)",
          color: "var(--text-primary)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Top Modal Header */}
        <div
          className="flex shrink-0 items-center justify-between px-5 py-3.5"
          style={{ borderBottom: "1px solid var(--border-subtle)" }}
        >
          <div className="flex items-center gap-2.5">
            <SlidersHorizontal className="h-5 w-5" style={{ color: "var(--accent)" }} />
            <h3 className="text-[16px] font-semibold">{t("settingsTeza")}</h3>
            <span className="text-xs" style={{ color: "var(--text-secondary)" }}>
              · {activeTitle}
            </span>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="hover-menu rounded-lg p-1.5"
            title={`${t("close")} (Esc)`}
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        {/* 2-Column Body */}
        <div className="flex min-h-0 flex-1 flex-col md:flex-row">
          {/* Left Category Sidebar */}
          <nav
            className="flex shrink-0 gap-1 overflow-x-auto p-2.5 md:w-52 md:flex-col md:overflow-y-auto"
            style={{
              borderBottom: "1px solid var(--border-subtle)",
              borderRight: "1px solid var(--border-subtle)",
              background: "var(--bg-stage)",
            }}
          >
            {NAV_TABS.map((tab) => {
              const Icon = tab.icon;
              const active = activeTab === tab.id;
              return (
                <button
                  key={tab.id}
                  type="button"
                  onClick={() => setActiveTab(tab.id)}
                  className="flex shrink-0 items-center gap-2.5 rounded-xl px-3 py-2 text-left text-[13px] font-medium transition"
                  style={{
                    background: active ? "var(--accent-soft)" : "transparent",
                    color: active ? "var(--accent)" : "var(--text-secondary)",
                  }}
                >
                  <Icon className="h-4 w-4 shrink-0" />
                  <span className="truncate">{t(tab.labelKey)}</span>
                  {tab.id === "system" && availableUpdate && (
                    <span className="ml-auto h-2 w-2 shrink-0 rounded-full bg-emerald-400" />
                  )}
                </button>
              );
            })}
          </nav>

          {/* Right Panel */}
          <div className="min-h-0 flex-1 space-y-5 overflow-y-auto p-5">
            {activeTab === "notifications" && (
              <>
                <section className="space-y-2">
                  <h4
                    className="text-xs font-semibold uppercase tracking-wider"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    {t("notifyGeneralHeading")}
                  </h4>
                  <div className="rounded-xl p-1" style={{ background: "var(--bg-surface)" }}>
                    <ToggleRow
                      icon={<Bell className="h-4 w-4" />}
                      label={t("notifyEnableLabel")}
                      desc={t("notifyEnableDesc")}
                      checked={notifyPrefs.enabled}
                      onChange={(enabled) => patchNotify({ enabled })}
                    />
                    <ToggleRow
                      icon={<Monitor className="h-4 w-4" />}
                      label={t("notifyCustomPopupLabel")}
                      desc={t("notifyCustomPopupDesc")}
                      checked={notifyPrefs.useCustomPopup}
                      onChange={(useCustomPopup) => patchNotify({ useCustomPopup })}
                    />
                    <ToggleRow
                      icon={<MessageSquare className="h-4 w-4" />}
                      label={t("notifyFocusedLabel")}
                      desc={t("notifyFocusedDesc")}
                      checked={notifyPrefs.notifyWhenFocused}
                      onChange={(notifyWhenFocused) => patchNotify({ notifyWhenFocused })}
                    />
                    <ToggleRow
                      icon={<Volume2 className="h-4 w-4" />}
                      label={t("notifySoundLabel")}
                      desc={t("notifySoundDesc")}
                      checked={notifyPrefs.sound}
                      onChange={(sound) => patchNotify({ sound })}
                      action={
                        <button
                          type="button"
                          onClick={onTestSound}
                          className="hover-menu shrink-0 rounded-lg px-2.5 py-1 text-xs font-semibold"
                          style={{
                            border: "1px solid var(--border-subtle)",
                            color: "var(--accent)",
                          }}
                        >
                          {t("notifyTestSoundBtn")}
                        </button>
                      }
                    />
                    <ToggleRow
                      icon={<Zap className="h-4 w-4" />}
                      label={t("notifyBuzzLabel")}
                      desc={t("notifyBuzzDesc")}
                      checked={notifyPrefs.buzz}
                      onChange={(buzz) => patchNotify({ buzz })}
                    />
                  </div>
                </section>

                <section className="space-y-2">
                  <h4
                    className="text-xs font-semibold uppercase tracking-wider"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    {t("notifyRulesHeading")}
                  </h4>
                  <div className="rounded-xl p-1" style={{ background: "var(--bg-surface)" }}>
                    <ToggleRow
                      icon={<User className="h-4 w-4" />}
                      label={t("notifyDirectLabel")}
                      checked={notifyPrefs.notifyDirect}
                      onChange={(notifyDirect) => patchNotify({ notifyDirect })}
                    />
                    <ToggleRow
                      icon={<Users className="h-4 w-4" />}
                      label={t("notifyGroupLabel")}
                      checked={notifyPrefs.notifyGroup}
                      onChange={(notifyGroup) => patchNotify({ notifyGroup })}
                    />
                    <ToggleRow
                      icon={<AtSign className="h-4 w-4" />}
                      label={t("notifyMentionLabel")}
                      desc={t("notifyMentionDesc")}
                      checked={notifyPrefs.mentionOverride}
                      onChange={(mentionOverride) => patchNotify({ mentionOverride })}
                    />
                  </div>
                </section>

                {/* Position Picker + Duration */}
                <section className="space-y-2.5">
                  <div className="flex items-center justify-between">
                    <h4
                      className="text-xs font-semibold uppercase tracking-wider"
                      style={{ color: "var(--text-secondary)" }}
                    >
                      {t("notifyPosHeading")}
                    </h4>
                    <button
                      type="button"
                      onClick={onTestNotification}
                      className="tf-btn rounded-lg px-3 py-1 text-xs font-semibold"
                    >
                      {t("notifyTestBtn")}
                    </button>
                  </div>

                  <div
                    className="space-y-3 rounded-xl p-3.5"
                    style={{ background: "var(--bg-surface)" }}
                  >
                    {/* 2x2 Monitor Wireframe */}
                    <div
                      className="grid grid-cols-2 gap-2.5 rounded-xl p-3"
                      style={{
                        background: "var(--bg-stage)",
                        border: "1px solid var(--border-subtle)",
                      }}
                    >
                      {POSITION_CORNERS.map((corner) => {
                        const active = notifyPrefs.position === corner.id;
                        return (
                          <button
                            key={corner.id}
                            type="button"
                            onClick={() => patchNotify({ position: corner.id })}
                            className="flex h-14 flex-col justify-between rounded-xl p-2 text-left text-xs font-medium transition"
                            style={{
                              background: active ? "var(--accent-soft)" : "var(--bg-surface)",
                              border: `1.5px solid ${active ? "var(--accent)" : "var(--border-subtle)"}`,
                              color: active ? "var(--accent)" : "var(--text-secondary)",
                            }}
                          >
                            <div
                              className={`flex w-full ${
                                corner.id.includes("right") ? "justify-end" : "justify-start"
                              }`}
                            >
                              <span
                                className="h-2.5 w-8 rounded-full"
                                style={{
                                  background: active ? "var(--accent)" : "var(--text-muted)",
                                  opacity: active ? 1 : 0.45,
                                }}
                              />
                            </div>
                            <span>{t(corner.labelKey)}</span>
                          </button>
                        );
                      })}
                    </div>

                    <div className="flex items-center justify-between gap-3">
                      <span className="text-xs font-medium" style={{ color: "var(--text-secondary)" }}>
                        {t("notifyDurationLabel")}
                      </span>
                      <div
                        className="grid grid-cols-3 gap-1 rounded-xl p-1"
                        style={{ background: "var(--bg-sidebar)" }}
                      >
                        {[3, 5, 8].map((sec) => (
                          <button
                            key={sec}
                            type="button"
                            onClick={() => patchNotify({ durationSec: sec })}
                            className="rounded-lg px-3 py-1 text-xs font-medium transition"
                            style={{
                              background:
                                notifyPrefs.durationSec === sec ? "var(--accent)" : "transparent",
                              color:
                                notifyPrefs.durationSec === sec
                                  ? "var(--text-on-accent)"
                                  : "var(--text-secondary)",
                            }}
                          >
                            {t("secondsUnit", { sec })}
                          </button>
                        ))}
                      </div>
                    </div>
                  </div>
                </section>

                {/* Privacy Preview Mode */}
                <section className="space-y-2">
                  <h4
                    className="flex items-center gap-1.5 text-xs font-semibold uppercase tracking-wider"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    <Eye className="h-3.5 w-3.5" />
                    {t("notifyPrivacyHeading")}
                  </h4>
                  <div
                    className="grid grid-cols-3 gap-1.5 rounded-xl p-1.5"
                    style={{ background: "var(--bg-surface)" }}
                  >
                    {(
                      [
                        { id: "full", label: t("privacyFull") },
                        { id: "sender_only", label: t("privacySenderOnly") },
                        { id: "hidden", label: t("privacyHidden") },
                      ] as { id: NotifyPreviewMode; label: string }[]
                    ).map((opt) => {
                      const active = notifyPrefs.previewMode === opt.id;
                      return (
                        <button
                          key={opt.id}
                          type="button"
                          onClick={() => patchNotify({ previewMode: opt.id })}
                          className="rounded-lg px-2 py-2 text-xs font-medium transition"
                          style={{
                            background: active ? "var(--accent)" : "transparent",
                            color: active ? "var(--text-on-accent)" : "var(--text-secondary)",
                          }}
                        >
                          {opt.label}
                        </button>
                      );
                    })}
                  </div>
                </section>

                {/* Unread Badge Count */}
                <section className="space-y-2">
                  <h4
                    className="text-xs font-semibold uppercase tracking-wider"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    {t("notifyBadgeHeading")}
                  </h4>
                  <div className="space-y-2 rounded-xl p-2" style={{ background: "var(--bg-surface)" }}>
                    <div
                      className="grid grid-cols-2 gap-1 rounded-xl p-1"
                      style={{ background: "var(--bg-sidebar)" }}
                    >
                      {(
                        [
                          { id: "unread_messages", label: t("badgeCountMsgs") },
                          { id: "unread_chats", label: t("badgeCountChats") },
                        ] as { id: NotifyBadgeMode; label: string }[]
                      ).map((opt) => (
                        <button
                          key={opt.id}
                          type="button"
                          onClick={() => patchNotify({ badgeMode: opt.id })}
                          className="rounded-lg py-1.5 text-xs font-medium transition"
                          style={{
                            background:
                              notifyPrefs.badgeMode === opt.id ? "var(--accent)" : "transparent",
                            color:
                              notifyPrefs.badgeMode === opt.id
                                ? "var(--text-on-accent)"
                                : "var(--text-secondary)",
                          }}
                        >
                          {opt.label}
                        </button>
                      ))}
                    </div>
                    <ToggleRow
                      icon={<Bell className="h-4 w-4" />}
                      label={t("badgeIncludeMuted")}
                      checked={notifyPrefs.includeMutedInBadge}
                      onChange={(includeMutedInBadge) => patchNotify({ includeMutedInBadge })}
                    />
                  </div>
                </section>
              </>
            )}

            {activeTab === "appearance" && (
              <>
                <section className="space-y-2.5">
                  <h4
                    className="flex items-center gap-1.5 text-xs font-semibold uppercase tracking-wider"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    <Languages className="h-3.5 w-3.5" />
                    {t("uiLanguageHeading")}
                  </h4>
                  <p className="text-xs" style={{ color: "var(--text-secondary)" }}>
                    {t("uiLanguageDesc")}
                  </p>
                  <div className="grid grid-cols-3 gap-2">
                    {UI_LANGUAGE_OPTIONS.map((opt) => {
                      const active = lang === opt.code;
                      return (
                        <button
                          key={opt.code}
                          type="button"
                          onClick={() => setLang(opt.code)}
                          className="rounded-xl px-3 py-2.5 text-center text-[13.5px] font-semibold transition"
                          style={{
                            background: active ? "var(--accent-soft)" : "var(--bg-surface)",
                            border: `1px solid ${active ? "var(--accent)" : "var(--border-subtle)"}`,
                            color: active ? "var(--accent)" : "var(--text-primary)",
                          }}
                        >
                          {opt.label}
                        </button>
                      );
                    })}
                  </div>
                </section>

                <section className="space-y-3">
                  <h4
                    className="text-xs font-semibold uppercase tracking-wider"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    {t("themeHeading")}
                  </h4>
                  <div className="grid grid-cols-2 gap-2">
                    {THEMES.map((item) => {
                      const active = theme === item.id;
                      return (
                        <button
                          key={item.id}
                          type="button"
                          onClick={() => onThemeChange(item.id)}
                          className="flex items-center gap-2 rounded-xl px-3 py-2.5 text-left text-[13.5px] font-medium transition"
                          style={{
                            background: active ? "var(--accent-soft)" : "var(--bg-surface)",
                            border: `1px solid ${active ? "var(--accent)" : "var(--border-subtle)"}`,
                            color: active ? "var(--accent)" : "var(--text-primary)",
                          }}
                        >
                          <span
                            className="h-3 w-3 shrink-0 rounded-full"
                            style={{ background: item.dot }}
                          />
                          <span className="truncate">{t(item.labelKey)}</span>
                        </button>
                      );
                    })}
                  </div>
                </section>

                <section className="grid grid-cols-2 gap-3">
                  <div>
                    <div
                      className="mb-1.5 text-xs font-medium"
                      style={{ color: "var(--text-secondary)" }}
                    >
                      {t("fontSizeHeading")}
                    </div>
                    <div
                      className="grid grid-cols-3 gap-1 rounded-xl p-1"
                      style={{ background: "var(--bg-surface)" }}
                    >
                      {(
                        [
                          { id: "sm", label: t("fontSmall") },
                          { id: "md", label: t("fontMedium") },
                          { id: "lg", label: t("fontLarge") },
                        ] as const
                      ).map((opt) => (
                        <button
                          key={opt.id}
                          type="button"
                          onClick={() => onFontSizeChange(opt.id)}
                          className="rounded-lg py-1.5 text-xs font-medium transition"
                          style={{
                            background: fontSize === opt.id ? "var(--accent)" : "transparent",
                            color:
                              fontSize === opt.id
                                ? "var(--text-on-accent)"
                                : "var(--text-secondary)",
                          }}
                        >
                          {opt.label}
                        </button>
                      ))}
                    </div>
                  </div>

                  <div>
                    <div
                      className="mb-1.5 text-xs font-medium"
                      style={{ color: "var(--text-secondary)" }}
                    >
                      {t("densityHeading")}
                    </div>
                    <div
                      className="grid grid-cols-2 gap-1 rounded-xl p-1"
                      style={{ background: "var(--bg-surface)" }}
                    >
                      {(
                        [
                          { id: "comfortable", label: t("densityComfortable") },
                          { id: "compact", label: t("densityCompact") },
                        ] as const
                      ).map((opt) => (
                        <button
                          key={opt.id}
                          type="button"
                          onClick={() => onDensityChange(opt.id)}
                          className="rounded-lg py-1.5 text-xs font-medium transition"
                          style={{
                            background: density === opt.id ? "var(--accent)" : "transparent",
                            color:
                              density === opt.id
                                ? "var(--text-on-accent)"
                                : "var(--text-secondary)",
                          }}
                        >
                          {opt.label}
                        </button>
                      ))}
                    </div>
                  </div>
                </section>

                <section className="space-y-2">
                  <h4
                    className="text-xs font-semibold uppercase tracking-wider"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    Hiệu ứng & Soạn thảo
                  </h4>
                  <div className="space-y-2 rounded-xl p-2" style={{ background: "var(--bg-surface)" }}>
                    <ToggleRow
                      icon={<Sparkles className="h-4 w-4" />}
                      label="Hiệu ứng Tiên Sáng"
                      desc="Ánh sáng con trỏ và hạt bụi vàng"
                      checked={fxEnabled}
                      onChange={onFxChange}
                    />
                    <div
                      className="border-t px-2 pt-2.5"
                      style={{ borderColor: "var(--border-subtle)" }}
                    >
                      <div className="mb-1 flex items-center justify-between gap-2 text-xs font-medium">
                        <span>{t("emojiStyleHeading")}</span>
                        <div className="flex items-center gap-1">
                          {(["smile", "rofl", "wink", "love", "devil", "cry"] as const).map(
                            (emoId) => (
                              <RetroEmoticon key={emoId} id={emoId} size={17} />
                            ),
                          )}
                        </div>
                      </div>
                      <div
                        className="mb-2 text-[11.5px]"
                        style={{ color: "var(--text-secondary)" }}
                      >
                        {t("emojiStyleDesc")}
                      </div>
                      <div
                        className="grid grid-cols-2 gap-1 rounded-xl p-1"
                        style={{ background: "var(--bg-sidebar)" }}
                      >
                        <button
                          type="button"
                          onClick={() => setEmojiStyle("retro")}
                          className="rounded-lg px-2 py-1.5 text-xs font-medium transition"
                          style={{
                            background:
                              emojiStyle === "retro" ? "var(--accent)" : "transparent",
                            color:
                              emojiStyle === "retro"
                                ? "var(--text-on-accent)"
                                : "var(--text-secondary)",
                          }}
                        >
                          {t("emojiStyleRetro")}
                        </button>
                        <button
                          type="button"
                          onClick={() => setEmojiStyle("system")}
                          className="rounded-lg px-2 py-1.5 text-xs font-medium transition"
                          style={{
                            background:
                              emojiStyle === "system" ? "var(--accent)" : "transparent",
                            color:
                              emojiStyle === "system"
                                ? "var(--text-on-accent)"
                                : "var(--text-secondary)",
                          }}
                        >
                          {t("emojiStyleSystem")}
                        </button>
                      </div>
                    </div>
                    <div
                      className="border-t px-2 pt-2.5"
                      style={{ borderColor: "var(--border-subtle)" }}
                    >
                      <div className="mb-1.5 flex items-center gap-2 text-xs font-medium">
                        <Send className="h-3.5 w-3.5" style={{ color: "var(--accent)" }} />
                        <span>Phím gửi tin nhắn</span>
                      </div>
                      <div
                        className="grid grid-cols-2 gap-1 rounded-xl p-1"
                        style={{ background: "var(--bg-sidebar)" }}
                      >
                        <button
                          type="button"
                          onClick={() => onSendOnEnterChange(true)}
                          className="rounded-lg px-2 py-1.5 text-xs font-medium transition"
                          style={{
                            background: sendOnEnter ? "var(--accent)" : "transparent",
                            color: sendOnEnter
                              ? "var(--text-on-accent)"
                              : "var(--text-secondary)",
                          }}
                        >
                          Enter để gửi (Shift+Enter xuống dòng)
                        </button>
                        <button
                          type="button"
                          onClick={() => onSendOnEnterChange(false)}
                          className="rounded-lg px-2 py-1.5 text-xs font-medium transition"
                          style={{
                            background: !sendOnEnter ? "var(--accent)" : "transparent",
                            color: !sendOnEnter
                              ? "var(--text-on-accent)"
                              : "var(--text-secondary)",
                          }}
                        >
                          Ctrl+Enter để gửi (Enter xuống dòng)
                        </button>
                      </div>
                    </div>
                  </div>
                </section>
              </>
            )}

            {activeTab === "folders" && (
              <section className="space-y-3">
                <h4
                  className="text-xs font-semibold uppercase tracking-wider"
                  style={{ color: "var(--text-secondary)" }}
                >
                  {t("foldersSectionHeading")}
                </h4>
                <div
                  className="flex items-center justify-between gap-3 rounded-xl p-4"
                  style={{ background: "var(--bg-surface)" }}
                >
                  <div className="flex items-center gap-3">
                    <FolderKanban className="h-5 w-5 shrink-0" style={{ color: "var(--accent)" }} />
                    <div>
                      <div className="text-[14px] font-semibold">
                        {t("customFoldersCountText", { count: customFoldersCount })}
                      </div>
                      <div className="text-xs" style={{ color: "var(--text-secondary)" }}>
                        {t("customFoldersCountDesc")}
                      </div>
                    </div>
                  </div>
                  <button
                    type="button"
                    onClick={() => {
                      onClose();
                      onOpenFoldersModal();
                    }}
                    className="tf-btn shrink-0 rounded-xl px-3.5 py-2 text-xs font-semibold"
                  >
                    {t("manageFoldersBtn")}
                  </button>
                </div>

                <h4
                  className="pt-2 text-xs font-semibold uppercase tracking-wider"
                  style={{ color: "var(--text-secondary)" }}
                >
                  {t("railDisplayHeading")}
                </h4>
                <div
                  className="grid grid-cols-3 gap-2 rounded-xl p-2"
                  style={{ background: "var(--bg-surface)" }}
                >
                  {(
                    [
                      { id: "icon-text", label: t("railModeIconText") },
                      { id: "icon-only", label: t("railModeIconOnly") },
                      { id: "text-only", label: t("railModeTextOnly") },
                    ] as const
                  ).map((opt) => {
                    const active = railDisplayMode === opt.id;
                    return (
                      <button
                        key={opt.id}
                        type="button"
                        onClick={() => onRailDisplayModeChange(opt.id)}
                        className="rounded-lg px-3 py-2 text-xs font-semibold transition"
                        style={{
                          background: active ? "var(--bg-active)" : "var(--bg-sidebar)",
                          color: active ? "var(--text-on-active)" : "var(--text-primary)",
                          border: active
                            ? "1px solid var(--accent)"
                            : "1px solid var(--border-subtle)",
                        }}
                      >
                        {opt.label}
                      </button>
                    );
                  })}
                </div>

                <h4
                  className="pt-2 text-xs font-semibold uppercase tracking-wider"
                  style={{ color: "var(--text-secondary)" }}
                >
                  {t("builtInFoldersHeading")}
                </h4>
                <div className="space-y-1 rounded-xl p-2" style={{ background: "var(--bg-surface)" }}>
                  {(
                    [
                      { id: "unread", label: t("folderUnread"), desc: t("builtInUnreadDesc"), icon: <MessageSquare className="h-4 w-4" /> },
                      { id: "direct", label: t("folderDirect"), desc: t("builtInDirectDesc"), icon: <User className="h-4 w-4" /> },
                      { id: "group", label: t("folderGroup"), desc: t("builtInGroupDesc"), icon: <Users className="h-4 w-4" /> },
                      { id: "cloud", label: t("folderCloud"), desc: t("builtInCloudDesc"), icon: <Bookmark className="h-4 w-4" /> },
                    ] as const
                  ).map((item) => (
                    <ToggleRow
                      key={item.id}
                      icon={item.icon}
                      label={item.label}
                      desc={item.desc}
                      checked={!hiddenBuiltInFolders.includes(item.id)}
                      onChange={() => onToggleBuiltInFolder(item.id)}
                    />
                  ))}
                </div>
              </section>
            )}

            {activeTab === "webapps" && (
              <section className="space-y-3">
                <h4
                  className="text-xs font-semibold uppercase tracking-wider"
                  style={{ color: "var(--text-secondary)" }}
                >
                  {t("webappSectionHeading")}
                </h4>
                <div className="rounded-xl p-2" style={{ background: "var(--bg-surface)" }}>
                  <ToggleRow
                    icon={<Globe className="h-4 w-4" />}
                    label={t("webappEnableLabel")}
                    desc={t("webappEnableDesc")}
                    checked={webAppsCfg.enabled}
                    onChange={(enabled) => onWebAppsCfgChange({ ...webAppsCfg, enabled })}
                  />
                </div>

                <div
                  className="space-y-2.5 rounded-xl p-3.5"
                  style={{ background: "var(--bg-surface)" }}
                >
                  <div className="text-xs font-semibold">
                    {waEditingId ? t("webappEditTitle") : t("webappAddTitle")}
                  </div>
                  <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
                    <input
                      value={waName}
                      onChange={(e) => {
                        setWaName(e.target.value);
                        if (waError) setWaError(null);
                      }}
                      placeholder={t("webappNamePlaceholder")}
                      className="rounded-lg px-3 py-1.5 text-xs outline-none"
                      style={{
                        background: "var(--bg-sidebar)",
                        border: "1px solid var(--border-subtle)",
                      }}
                    />
                    <input
                      value={waUrl}
                      onChange={(e) => {
                        setWaUrl(e.target.value);
                        if (waError) setWaError(null);
                      }}
                      placeholder={t("webappUrlPlaceholder")}
                      className="rounded-lg px-3 py-1.5 text-xs outline-none"
                      style={{
                        background: "var(--bg-sidebar)",
                        border: "1px solid var(--border-subtle)",
                      }}
                    />
                  </div>
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <div className="flex flex-wrap items-center gap-1">
                      {WEBAPP_ICON_OPTIONS.map((opt) => {
                        const Icon = opt.icon;
                        const picked = waIcon === opt.id;
                        return (
                          <button
                            key={opt.id}
                            type="button"
                            title={opt.label}
                            onClick={() => setWaIcon(opt.id)}
                            className="rounded-lg p-1.5 transition"
                            style={{
                              background: picked ? "var(--bg-active)" : "var(--bg-sidebar)",
                              color: picked ? "var(--text-on-active)" : "var(--text-secondary)",
                            }}
                          >
                            <Icon className="h-3.5 w-3.5" />
                          </button>
                        );
                      })}
                    </div>
                    <select
                      value={waMode}
                      onChange={(e) =>
                        setWaMode(e.target.value === "external" ? "external" : "embedded")
                      }
                      className="rounded-lg px-2.5 py-1.5 text-xs outline-none"
                      style={{
                        background: "var(--bg-sidebar)",
                        border: "1px solid var(--border-subtle)",
                      }}
                    >
                      <option value="embedded">{t("webappModeEmbedded")}</option>
                      <option value="external">{t("webappModeExternal")}</option>
                    </select>
                  </div>
                  {waError && (
                    <div className="rounded-lg bg-rose-500/15 px-3 py-1.5 text-xs text-rose-300">
                      {waError}
                    </div>
                  )}
                  <div className="flex items-center gap-2">
                    <button
                      type="button"
                      onClick={() => {
                        const cleanName = waName.trim();
                        const normUrl = normalizeWebUrl(waUrl);
                        if (!cleanName) {
                          setWaError(t("webappErrNoName"));
                          return;
                        }
                        if (!normUrl) {
                          setWaError(t("webappErrBadUrl"));
                          return;
                        }
                        if (waEditingId) {
                          onWebAppsCfgChange({
                            ...webAppsCfg,
                            items: webAppsCfg.items.map((it) =>
                              it.id === waEditingId
                                ? { ...it, name: cleanName, url: normUrl, icon: waIcon, openMode: waMode }
                                : it,
                            ),
                          });
                          setWaEditingId(null);
                        } else {
                          const nextItem: WebAppItem = {
                            id: `wa-${Date.now()}-${Math.random().toString(36).slice(2, 5)}`,
                            name: cleanName,
                            url: normUrl,
                            icon: waIcon,
                            openMode: waMode,
                          };
                          onWebAppsCfgChange({
                            ...webAppsCfg,
                            enabled: true,
                            items: [...webAppsCfg.items, nextItem],
                          });
                        }
                        setWaName("");
                        setWaUrl("");
                        setWaIcon("globe");
                        setWaMode("embedded");
                        setWaError(null);
                      }}
                      className="tf-btn inline-flex items-center gap-1 rounded-lg px-3 py-1.5 text-xs font-semibold"
                    >
                      <Plus className="h-3.5 w-3.5" />
                      <span>{waEditingId ? t("webappBtnUpdate") : t("webappBtnAdd")}</span>
                    </button>
                    {waEditingId && (
                      <button
                        type="button"
                        onClick={() => {
                          setWaEditingId(null);
                          setWaName("");
                          setWaUrl("");
                          setWaIcon("globe");
                          setWaMode("embedded");
                          setWaError(null);
                        }}
                        className="rounded-lg px-3 py-1.5 text-xs font-medium opacity-80 hover:opacity-100"
                        style={{ background: "var(--bg-sidebar)" }}
                      >
                        {t("cancel")}
                      </button>
                    )}
                  </div>
                </div>

                <div className="space-y-1.5">
                  {webAppsCfg.items.length === 0 ? (
                    <div
                      className="rounded-xl p-4 text-center text-xs"
                      style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
                    >
                      {t("webappEmpty")}
                    </div>
                  ) : (
                    webAppsCfg.items.map((item) => (
                      <div
                        key={item.id}
                        className="flex items-center justify-between gap-2 rounded-xl px-3 py-2"
                        style={{ background: "var(--bg-surface)" }}
                      >
                        <div className="flex min-w-0 items-center gap-2.5">
                          <span style={{ color: "var(--accent)" }}>
                            {renderWebAppIcon(item.icon, "h-4 w-4")}
                          </span>
                          <div className="min-w-0">
                            <div className="truncate text-[13px] font-semibold">{item.name}</div>
                            <div
                              className="truncate text-[11px]"
                              style={{ color: "var(--text-secondary)" }}
                            >
                              {extractWebAppDomain(item.url)} ·{" "}
                              {item.openMode === "external"
                                ? t("webappModeExternalShort")
                                : t("webappModeEmbeddedShort")}
                            </div>
                          </div>
                        </div>
                        <div className="flex shrink-0 items-center gap-1">
                          {onOpenWebApp && (
                            <button
                              type="button"
                              onClick={() => {
                                onClose();
                                onOpenWebApp(item);
                              }}
                              title={t("webappOpenNowTitle")}
                              className="hover-menu rounded-lg px-2 py-1 text-[11px] font-medium"
                              style={{ background: "var(--bg-sidebar)", color: "var(--accent)" }}
                            >
                              <ExternalLink className="h-3.5 w-3.5" />
                            </button>
                          )}
                          <button
                            type="button"
                            onClick={() => {
                              setWaEditingId(item.id);
                              setWaName(item.name);
                              setWaUrl(item.url);
                              setWaIcon(item.icon);
                              setWaMode(item.openMode);
                              setWaError(null);
                            }}
                            className="hover-menu rounded-lg px-2 py-1 text-[11px] font-medium"
                            style={{ background: "var(--bg-sidebar)" }}
                          >
                            {t("edit")}
                          </button>
                          <button
                            type="button"
                            onClick={() => {
                              onWebAppsCfgChange({
                                ...webAppsCfg,
                                items: webAppsCfg.items.filter((x) => x.id !== item.id),
                              });
                              if (waEditingId === item.id) {
                                setWaEditingId(null);
                                setWaName("");
                                setWaUrl("");
                              }
                            }}
                            title={t("delete")}
                            className="hover-menu rounded-lg p-1.5 text-rose-400"
                          >
                            <Trash2 className="h-3.5 w-3.5" />
                          </button>
                        </div>
                      </div>
                    ))
                  )}
                </div>
              </section>
            )}

            {activeTab === "calls" && (
              <section className="space-y-4">
                <h4
                  className="flex items-center gap-1.5 text-xs font-semibold uppercase tracking-wider"
                  style={{ color: "var(--text-secondary)" }}
                >
                  <Phone className="h-3.5 w-3.5" />
                  {t("callAudioSettingsHeading")}
                </h4>

                {relayCfg?.enabled ? (
                  <div
                    className="flex flex-wrap items-center justify-between gap-3 rounded-xl px-3.5 py-3 text-xs"
                    style={{
                      background: "var(--danger-bg)",
                      color: "var(--danger-text)",
                    }}
                  >
                    <span className="font-medium">{t("callRelayUnsupportedBanner")}</span>
                    <button
                      type="button"
                      onClick={() => setActiveTab("network")}
                      className="rounded-lg px-2.5 py-1 font-semibold underline hover:opacity-85"
                    >
                      {t("callOpenNetworkSettings")}
                    </button>
                  </div>
                ) : (
                  <div
                    className="flex items-center gap-2 rounded-xl px-3.5 py-2.5 text-xs"
                    style={{
                      background: "rgba(16,185,129,0.12)",
                      color: "#34d399",
                    }}
                  >
                    <CheckCircle2 className="h-4 w-4 shrink-0" />
                    <span>{t("callDirectStatusReady")}</span>
                  </div>
                )}

                <div
                  className="space-y-3 rounded-xl p-3.5"
                  style={{ background: "var(--bg-surface)" }}
                >
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <span className="text-[13.5px] font-semibold">
                      {t("callAudioDevicesBtn")}
                    </span>
                    <div className="flex items-center gap-2">
                      <button
                        type="button"
                        disabled={callAudioTesting || callDevicesLoading}
                        onClick={() => {
                          setCallAudioTesting(true);
                          setCallAudioTestResult(null);
                          void bridge
                            .callTestAudio()
                            .then((res: CallAudioTestResultDto) => {
                              const deviceLabel =
                                res.output_device || t("callOsDefaultDevice");
                              setCallAudioTestResult({
                                ok: true,
                                warn: res.output_is_virtual,
                                text: res.output_is_virtual
                                  ? t("callTestAudioVirtualResult", {
                                      device: deviceLabel,
                                    })
                                  : t("callTestAudioOkResult", {
                                      device: deviceLabel,
                                    }),
                              });
                            })
                            .catch((err) => {
                              setCallAudioTestResult({
                                ok: false,
                                text:
                                  err instanceof Error ? err.message : String(err),
                              });
                            })
                            .finally(() => setCallAudioTesting(false));
                        }}
                        className="hover-menu inline-flex items-center gap-1.5 rounded-lg px-2.5 py-1 text-xs font-medium disabled:opacity-50"
                        style={{
                          border: "1px solid var(--border-subtle)",
                          color: "var(--text-primary)",
                        }}
                      >
                        {callAudioTesting ? (
                          <Loader2 className="h-3.5 w-3.5 animate-spin" />
                        ) : (
                          <Volume2 className="h-3.5 w-3.5" />
                        )}
                        <span>
                          {callAudioTesting
                            ? t("callTestingAudioLabel")
                            : t("callTestAudioBtn")}
                        </span>
                      </button>
                      <button
                        type="button"
                        disabled={callDevicesLoading}
                        onClick={onRefreshCallDevices}
                        className="hover-menu inline-flex items-center gap-1.5 rounded-lg px-2.5 py-1 text-xs font-medium disabled:opacity-50"
                        style={{
                          border: "1px solid var(--border-subtle)",
                          color: "var(--accent)",
                        }}
                      >
                        <RefreshCw
                          className={`h-3.5 w-3.5 ${callDevicesLoading ? "animate-spin" : ""}`}
                        />
                        <span>{t("callRefreshDevicesBtn")}</span>
                      </button>
                    </div>
                  </div>

                  {callDevices && !effectiveCallDevices.hasPhysicalInput && (
                    <div
                      className="flex items-start gap-2 rounded-lg px-2.5 py-2 text-xs"
                      style={{
                        background: "rgba(245,158,11,0.12)",
                        border: "1px solid rgba(245,158,11,0.3)",
                        color: "#fbbf24",
                      }}
                    >
                      <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                      <span>{t("callNoPhysicalMicWarning")}</span>
                    </div>
                  )}

                  {callDevices &&
                    (effectiveCallDevices.inputIsVirtual ||
                      effectiveCallDevices.outputIsVirtual) && (
                      <div
                        className="flex items-start gap-2 rounded-lg px-2.5 py-2 text-xs"
                        style={{
                          background: "rgba(245,158,11,0.12)",
                          border: "1px solid rgba(245,158,11,0.3)",
                          color: "#fbbf24",
                        }}
                      >
                        <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                        <span>
                          {t("callVirtualAudioWarningBanner", {
                            input:
                              effectiveCallDevices.effectiveInput?.name ||
                              t("callOsDefaultDevice"),
                            output:
                              effectiveCallDevices.effectiveOutput?.name ||
                              t("callOsDefaultDevice"),
                          })}
                        </span>
                      </div>
                    )}

                  <label className="block space-y-1">
                    <span
                      className="flex items-center gap-1.5 text-xs font-medium"
                      style={{ color: "var(--text-secondary)" }}
                    >
                      <Mic className="h-3.5 w-3.5" />
                      {t("callInputDeviceLabel")}
                    </span>
                    <select
                      value={callDevices?.selected_input ?? ""}
                      onChange={(e) => {
                        setCallDevicesSaved(false);
                        setCallAudioTestResult(null);
                        void Promise.resolve(
                          onSelectCallDevices(
                            e.target.value || null,
                            callDevices?.selected_output ?? null,
                          ),
                        ).then(() => setCallDevicesSaved(true));
                      }}
                      className="w-full rounded-lg px-2.5 py-1.5 text-[13.5px] outline-none"
                      style={{
                        background: "var(--bg-sidebar)",
                        border: "1px solid var(--border-subtle)",
                        color: "var(--text-primary)",
                      }}
                    >
                      <option value="">
                        {effectiveCallDevices.defaultInput
                          ? `${t("callOsDefaultDevice")} — ${effectiveCallDevices.defaultInput.name}${effectiveCallDevices.defaultInput.is_virtual ? ` [${t("callVirtualDeviceTag")}]` : ""}`
                          : t("callOsDefaultDevice")}
                      </option>
                      {(callDevices?.inputs ?? []).map((dev) => (
                        <option key={dev.name} value={dev.name}>
                          {dev.name}
                          {dev.is_default ? ` (${t("callOsDefaultDevice")})` : ""}
                          {dev.is_virtual ? ` [${t("callVirtualDeviceTag")}]` : ""}
                        </option>
                      ))}
                    </select>
                  </label>

                  <label className="block space-y-1">
                    <span
                      className="flex items-center gap-1.5 text-xs font-medium"
                      style={{ color: "var(--text-secondary)" }}
                    >
                      <Headphones className="h-3.5 w-3.5" />
                      {t("callOutputDeviceLabel")}
                    </span>
                    <select
                      value={callDevices?.selected_output ?? ""}
                      onChange={(e) => {
                        setCallDevicesSaved(false);
                        setCallAudioTestResult(null);
                        void Promise.resolve(
                          onSelectCallDevices(
                            callDevices?.selected_input ?? null,
                            e.target.value || null,
                          ),
                        ).then(() => setCallDevicesSaved(true));
                      }}
                      className="w-full rounded-lg px-2.5 py-1.5 text-[13.5px] outline-none"
                      style={{
                        background: "var(--bg-sidebar)",
                        border: "1px solid var(--border-subtle)",
                        color: "var(--text-primary)",
                      }}
                    >
                      <option value="">
                        {effectiveCallDevices.defaultOutput
                          ? `${t("callOsDefaultDevice")} — ${effectiveCallDevices.defaultOutput.name}${effectiveCallDevices.defaultOutput.is_virtual ? ` [${t("callVirtualDeviceTag")}]` : ""}`
                          : t("callOsDefaultDevice")}
                      </option>
                      {(callDevices?.outputs ?? []).map((dev) => (
                        <option key={dev.name} value={dev.name}>
                          {dev.name}
                          {dev.is_default ? ` (${t("callOsDefaultDevice")})` : ""}
                          {dev.is_virtual ? ` [${t("callVirtualDeviceTag")}]` : ""}
                        </option>
                      ))}
                    </select>
                  </label>

                  {callAudioTestResult && (
                    <div
                      className="flex items-start gap-1.5 rounded-lg px-2.5 py-1.5 text-xs"
                      style={
                        !callAudioTestResult.ok
                          ? {
                              background: "var(--danger-bg)",
                              color: "var(--danger-text)",
                            }
                          : callAudioTestResult.warn
                            ? {
                                background: "rgba(245,158,11,0.12)",
                                color: "#fbbf24",
                              }
                            : {
                                background: "rgba(16,185,129,0.12)",
                                color: "#34d399",
                              }
                      }
                    >
                      {!callAudioTestResult.ok || callAudioTestResult.warn ? (
                        <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                      ) : (
                        <CheckCircle2 className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                      )}
                      <span>{callAudioTestResult.text}</span>
                    </div>
                  )}

                  {callDevicesSaved && (
                    <div
                      className="flex items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-xs"
                      style={{
                        background: "rgba(16,185,129,0.12)",
                        color: "#34d399",
                      }}
                    >
                      <CheckCircle2 className="h-3.5 w-3.5 shrink-0" />
                      <span>{t("callDevicesSavedNotice")}</span>
                    </div>
                  )}
                </div>
              </section>
            )}

            {activeTab === "translate" && (
              <section className="space-y-3">
                <h4
                  className="text-xs font-semibold uppercase tracking-wider"
                  style={{ color: "var(--text-secondary)" }}
                >
                  {t("translateConfigHeading")}
                </h4>
                <div
                  className="flex items-center justify-between gap-3 rounded-xl p-4"
                  style={{ background: "var(--bg-surface)" }}
                >
                  <div className="flex items-center gap-3">
                    <Languages className="h-5 w-5 shrink-0" style={{ color: "var(--accent)" }} />
                    <div>
                      <div className="text-[14px] font-semibold">{t("translateConfigTitle")}</div>
                      <div className="text-xs" style={{ color: "var(--text-secondary)" }}>
                        {t("translateConfigSummary", {
                          provider: (translateCfg?.provider || "deepl").toUpperCase(),
                          lang: translateCfg?.target_lang || "VI",
                          uiLang: translateCode,
                        })}
                      </div>
                      <div className="mt-1.5 inline-flex items-center rounded-full px-2 py-0.5 text-[11px] font-semibold" style={{ background: "var(--accent-soft)", color: "var(--accent)" }}>
                        {translateCfg?.detect_mode === "local_smart_skip"
                          ? t("translateModeBadgeLocal")
                          : t("translateModeBadgeApi")}
                      </div>
                    </div>
                  </div>
                  <button
                    type="button"
                    onClick={() => {
                      onClose();
                      onOpenTranslateModal();
                    }}
                    className="tf-btn shrink-0 rounded-xl px-3.5 py-2 text-xs font-semibold"
                  >
                    {t("translateApiBtn")}
                  </button>
                </div>
              </section>
            )}

            {activeTab === "network" && (
              <div ref={relaySectionRef} className="space-y-2.5">
                <h4
                  className="text-xs font-semibold uppercase tracking-wider"
                  style={{ color: "var(--text-secondary)" }}
                >
                  {t("networkHeading")}
                </h4>
                <div className="space-y-3 rounded-xl p-3" style={{ background: "var(--bg-surface)" }}>
                  <ToggleRow
                    icon={<Network className="h-4 w-4" />}
                    label={t("relayEnableLabel")}
                    desc={t("relayEnableDesc")}
                    checked={relayEnabled}
                    onChange={(v) => {
                      setRelayEnabled(v);
                      setRelayFeedback(null);
                    }}
                  />

                  <div className="space-y-2.5 border-t pt-2.5" style={{ borderColor: "var(--border-subtle)" }}>
                    <div>
                      <div className="mb-1 text-xs font-medium" style={{ color: "var(--text-secondary)" }}>
                        {t("relayProtocolLabel")}
                      </div>
                      <div
                        className="grid grid-cols-2 gap-1 rounded-xl p-1"
                        style={{ background: "var(--bg-sidebar)" }}
                      >
                        {(
                          [
                            { id: "socks5", label: t("relaySocks5Label") },
                            { id: "http", label: t("relayHttpLabel") },
                          ] as const
                        ).map((opt) => (
                          <button
                            key={opt.id}
                            type="button"
                            onClick={() => setRelayMode(opt.id)}
                            className="rounded-lg py-1.5 text-xs font-medium transition"
                            style={{
                              background: relayMode === opt.id ? "var(--accent)" : "transparent",
                              color:
                                relayMode === opt.id
                                  ? "var(--text-on-accent)"
                                  : "var(--text-secondary)",
                            }}
                          >
                            {opt.label}
                          </button>
                        ))}
                      </div>
                    </div>

                    <div className="grid grid-cols-3 gap-2">
                      <div className="col-span-2">
                        <label className="mb-1 block text-xs font-medium" style={{ color: "var(--text-secondary)" }}>
                          {t("relayHostLabel")}
                        </label>
                        <input
                          value={relayHost}
                          onChange={(e) => setRelayHost(e.target.value)}
                          placeholder="10.7.12.233"
                          className="w-full rounded-lg px-2.5 py-1.5 text-[13.5px] outline-none"
                          style={{
                            background: "var(--bg-sidebar)",
                            border: "1px solid var(--border-subtle)",
                          }}
                        />
                      </div>
                      <div>
                        <label className="mb-1 block text-xs font-medium" style={{ color: "var(--text-secondary)" }}>
                          {t("relayPortLabel")}
                        </label>
                        <input
                          type="number"
                          min={1}
                          max={65535}
                          value={relayPort}
                          onChange={(e) => setRelayPort(e.target.value)}
                          placeholder="1088"
                          className="w-full rounded-lg px-2.5 py-1.5 text-[13.5px] outline-none"
                          style={{
                            background: "var(--bg-sidebar)",
                            border: "1px solid var(--border-subtle)",
                          }}
                        />
                      </div>
                    </div>

                    <div className="grid grid-cols-2 gap-2">
                      <div>
                        <label className="mb-1 block text-xs font-medium" style={{ color: "var(--text-secondary)" }}>
                          {t("relayUserLabel")}
                        </label>
                        <input
                          value={relayUser}
                          onChange={(e) => setRelayUser(e.target.value)}
                          placeholder="teza"
                          className="w-full rounded-lg px-2.5 py-1.5 text-[13.5px] outline-none"
                          style={{
                            background: "var(--bg-sidebar)",
                            border: "1px solid var(--border-subtle)",
                          }}
                        />
                      </div>
                      <div>
                        <div className="mb-1 flex items-center justify-between">
                          <label className="text-xs font-medium" style={{ color: "var(--text-secondary)" }}>
                            {t("relayPasswordLabel")}
                          </label>
                          {relayCfg?.has_password && !clearRelayPw && (
                            <button
                              type="button"
                              onClick={() => {
                                setClearRelayPw(true);
                                setRelayPassword("");
                              }}
                              className="text-[11px] underline hover:opacity-80"
                              style={{ color: "var(--danger-text)" }}
                            >
                              {t("relayClearPwBtn")}
                            </button>
                          )}
                        </div>
                        <input
                          type="password"
                          value={relayPassword}
                          onChange={(e) => {
                            setRelayPassword(e.target.value);
                            if (e.target.value) setClearRelayPw(false);
                          }}
                          placeholder={
                            relayCfg?.has_password && !clearRelayPw
                              ? t("relayPwSavedPlaceholder")
                              : t("relayPwPlaceholder")
                          }
                          className="w-full rounded-lg px-2.5 py-1.5 text-[13.5px] outline-none"
                          style={{
                            background: "var(--bg-sidebar)",
                            border: "1px solid var(--border-subtle)",
                          }}
                        />
                      </div>
                    </div>

                    <label className="flex cursor-pointer items-center justify-between gap-2 pt-0.5 text-[13px]">
                      <span style={{ color: "var(--text-secondary)" }}>
                        {t("relayRouteMediaLabel")}
                      </span>
                      <input
                        type="checkbox"
                        checked={routeMedia}
                        onChange={(e) => setRouteMedia(e.target.checked)}
                        className="h-4 w-4 cursor-pointer accent-[var(--accent)]"
                      />
                    </label>

                    {relayFeedback && (
                      <div
                        className="flex items-start gap-2 rounded-lg px-3 py-2 text-xs"
                        style={{
                          background: relayFeedback.ok ? "rgba(16,185,129,0.12)" : "var(--danger-bg)",
                          color: relayFeedback.ok ? "#34d399" : "var(--danger-text)",
                        }}
                      >
                        {relayFeedback.ok && <CheckCircle2 className="mt-0.5 h-3.5 w-3.5 shrink-0" />}
                        <span>{relayFeedback.text}</span>
                      </div>
                    )}

                    <div className="flex items-center justify-end gap-2 pt-1">
                      <button
                        type="button"
                        disabled={relayTesting || relaySaving || !relayHost.trim()}
                        onClick={() => void handleTestRelay()}
                        className="flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-xs font-medium disabled:opacity-50"
                        style={{
                          background: "var(--bg-sidebar)",
                          border: "1px solid var(--border-subtle)",
                        }}
                      >
                        {relayTesting ? (
                          <Loader2 className="h-3.5 w-3.5 animate-spin" />
                        ) : (
                          <RefreshCw className="h-3.5 w-3.5" />
                        )}
                        <span>{t("relayTestBtn")}</span>
                      </button>
                      <button
                        type="button"
                        disabled={relaySaving || relayTesting}
                        onClick={() => void handleSaveRelay()}
                        className="tf-btn flex items-center gap-1.5 rounded-lg px-3.5 py-1.5 text-xs font-semibold disabled:opacity-50"
                      >
                        {relaySaving && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
                        <span>{t("relaySaveBtn")}</span>
                      </button>
                    </div>
                  </div>
                </div>
              </div>
            )}

            {activeTab === "account" && (
              <section className="space-y-3">
                <h4
                  className="text-xs font-semibold uppercase tracking-wider"
                  style={{ color: "var(--text-secondary)" }}
                >
                  {t("accountHeading")}
                </h4>
                <div
                  className="flex items-center justify-between gap-3 rounded-xl p-4"
                  style={{ background: "var(--bg-surface)" }}
                >
                  <div className="flex min-w-0 items-center gap-3">
                    <ThreadAvatar
                      avatar={myProfile?.avatar || ""}
                      title={myProfile?.display_name || "Teza"}
                      kind="direct"
                      size="lg"
                    />
                    <div className="min-w-0">
                      <div className="truncate text-[15px] font-bold">
                        {myProfile?.display_name || t("notSignedIn")}
                      </div>
                      {myProfile?.uid && (
                        <div className="truncate text-xs" style={{ color: "var(--text-secondary)" }}>
                          UID: {myProfile.uid}
                        </div>
                      )}
                      <div className="mt-1 text-xs font-medium" style={{
                        color:
                          connStatus === "connected"
                            ? "#10b981"
                            : connStatus === "connecting"
                              ? "#f59e0b"
                              : "#ef4444",
                      }}>
                        {connStatus === "connected"
                          ? t("onlineDot")
                          : connStatus === "connecting"
                            ? t("connectingDot")
                            : t("reconnectingDot")}
                      </div>
                    </div>
                  </div>
                  {myProfile?.uid && (
                    <button
                      type="button"
                      onClick={() => {
                        onClose();
                        onOpenMyProfile();
                      }}
                      className="hover-menu shrink-0 rounded-xl px-3 py-1.5 text-xs font-semibold"
                      style={{
                        border: "1px solid var(--border-subtle)",
                        color: "var(--accent)",
                      }}
                    >
                      {t("viewProfileBtn")}
                    </button>
                  )}
                </div>

                <button
                  type="button"
                  onClick={() => {
                    onClose();
                    onLogout();
                  }}
                  className="flex w-full items-center justify-center gap-2 rounded-xl py-2.5 text-[13.5px] font-semibold transition hover:opacity-90"
                  style={{
                    background: "var(--danger-bg)",
                    color: "var(--danger-text)",
                  }}
                >
                  <LogOut className="h-4 w-4" />
                  <span>{t("logoutAccountBtn")}</span>
                </button>
              </section>
            )}

            {activeTab === "system" && (
              <>
                <section className="space-y-2">
                  <div className="flex items-center justify-between">
                    <h4
                      className="flex items-center gap-1.5 text-xs font-semibold uppercase tracking-wider"
                      style={{ color: "var(--text-secondary)" }}
                    >
                      <Lock className="h-3.5 w-3.5" />
                      {t("passcodeHeading")}
                    </h4>
                    <span
                      className="rounded-full px-2.5 py-0.5 text-[11px] font-semibold"
                      style={{
                        background: passcodeCfg.enabled
                          ? "rgba(16,185,129,0.16)"
                          : "var(--bg-surface)",
                        color: passcodeCfg.enabled ? "#34d399" : "var(--text-secondary)",
                      }}
                    >
                      {passcodeCfg.enabled ? t("passcodeOn") : t("passcodeOff")}
                    </span>
                  </div>
                  <div
                    className="space-y-3 rounded-xl p-3.5"
                    style={{ background: "var(--bg-surface)" }}
                  >
                    <p className="text-xs leading-relaxed" style={{ color: "var(--text-secondary)" }}>
                      {t("passcodeDesc")}
                    </p>

                    {!passcodeCfg.enabled ? (
                      <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
                        <input
                          type="password"
                          value={newPin}
                          onChange={(e) => setNewPin(e.target.value)}
                          placeholder={t("passcodeNewPlaceholder")}
                          autoComplete="off"
                          className="rounded-lg px-3 py-1.5 text-xs outline-none"
                          style={{
                            background: "var(--bg-sidebar)",
                            border: "1px solid var(--border-subtle)",
                          }}
                        />
                        <input
                          type="password"
                          value={confirmPin}
                          onChange={(e) => setConfirmPin(e.target.value)}
                          placeholder={t("passcodeConfirmPlaceholder")}
                          autoComplete="off"
                          className="rounded-lg px-3 py-1.5 text-xs outline-none"
                          style={{
                            background: "var(--bg-sidebar)",
                            border: "1px solid var(--border-subtle)",
                          }}
                        />
                        <div className="sm:col-span-2">
                          <button
                            type="button"
                            disabled={pinBusy}
                            onClick={() => {
                              const err = validatePasscodeCandidate(newPin, confirmPin);
                              if (err) {
                                setPinFeedback({ ok: false, text: err });
                                return;
                              }
                              setPinBusy(true);
                              setPinFeedback(null);
                              void hashPasscode(newPin)
                                .then(({ salt, hash }) => {
                                  onUpdatePasscode({
                                    ...passcodeCfg,
                                    enabled: true,
                                    salt,
                                    hash,
                                  });
                                  setNewPin("");
                                  setConfirmPin("");
                                  setPinFeedback({
                                    ok: true,
                                    text: t("passcodeEnabledMsg"),
                                  });
                                })
                                .catch((e) => setPinFeedback({ ok: false, text: String(e) }))
                                .finally(() => setPinBusy(false));
                            }}
                            className="tf-btn rounded-lg px-3.5 py-1.5 text-xs font-semibold disabled:opacity-50"
                          >
                            {t("passcodeEnableBtn")}
                          </button>
                        </div>
                      </div>
                    ) : (
                      <div className="space-y-2.5">
                        <div className="flex flex-wrap items-center justify-between gap-2">
                          <div className="flex items-center gap-2 text-xs">
                            <span style={{ color: "var(--text-secondary)" }}>
                              {t("passcodeAutoLockLabel")}
                            </span>
                            <select
                              value={passcodeCfg.autoLockMinutes}
                              onChange={(e) =>
                                onUpdatePasscode({
                                  ...passcodeCfg,
                                  autoLockMinutes: Number(e.target.value) || 0,
                                })
                              }
                              className="rounded-lg px-2.5 py-1 text-xs outline-none"
                              style={{
                                background: "var(--bg-sidebar)",
                                border: "1px solid var(--border-subtle)",
                              }}
                            >
                              <option value={0}>{t("passcodeAutoNever")}</option>
                              <option value={1}>{t("passcodeAuto1m")}</option>
                              <option value={5}>{t("passcodeAuto5m")}</option>
                              <option value={15}>{t("passcodeAuto15m")}</option>
                              <option value={60}>{t("passcodeAuto60m")}</option>
                            </select>
                          </div>
                          <button
                            type="button"
                            onClick={() => {
                              onClose();
                              onLockNow();
                            }}
                            className="inline-flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-xs font-semibold transition"
                            style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
                          >
                            <Lock className="h-3.5 w-3.5" />
                            <span>{t("passcodeLockNowBtn")}</span>
                          </button>
                        </div>

                        <div
                          className="grid grid-cols-1 gap-2 border-t pt-2.5 sm:grid-cols-3"
                          style={{ borderColor: "var(--border-subtle)" }}
                        >
                          <input
                            type="password"
                            value={currentPin}
                            onChange={(e) => setCurrentPin(e.target.value)}
                            placeholder={t("passcodeCurrentPlaceholder")}
                            autoComplete="off"
                            className="rounded-lg px-3 py-1.5 text-xs outline-none"
                            style={{
                              background: "var(--bg-sidebar)",
                              border: "1px solid var(--border-subtle)",
                            }}
                          />
                          <input
                            type="password"
                            value={newPin}
                            onChange={(e) => setNewPin(e.target.value)}
                            placeholder={t("passcodeChangePlaceholder")}
                            autoComplete="off"
                            className="rounded-lg px-3 py-1.5 text-xs outline-none"
                            style={{
                              background: "var(--bg-sidebar)",
                              border: "1px solid var(--border-subtle)",
                            }}
                          />
                          <input
                            type="password"
                            value={confirmPin}
                            onChange={(e) => setConfirmPin(e.target.value)}
                            placeholder={t("passcodeConfirmNewPlaceholder")}
                            autoComplete="off"
                            className="rounded-lg px-3 py-1.5 text-xs outline-none"
                            style={{
                              background: "var(--bg-sidebar)",
                              border: "1px solid var(--border-subtle)",
                            }}
                          />
                        </div>
                        <div className="flex flex-wrap items-center gap-2">
                          <button
                            type="button"
                            disabled={pinBusy || !currentPin || !newPin}
                            onClick={() => {
                              const err = validatePasscodeCandidate(newPin, confirmPin);
                              if (err) {
                                setPinFeedback({ ok: false, text: err });
                                return;
                              }
                              setPinBusy(true);
                              setPinFeedback(null);
                              void verifyPasscode(currentPin, passcodeCfg)
                                .then(async (ok) => {
                                  if (!ok) {
                                    setPinFeedback({
                                      ok: false,
                                      text: t("passcodeWrongCurrent"),
                                    });
                                    return;
                                  }
                                  const { salt, hash } = await hashPasscode(newPin);
                                  onUpdatePasscode({ ...passcodeCfg, enabled: true, salt, hash });
                                  setCurrentPin("");
                                  setNewPin("");
                                  setConfirmPin("");
                                  setPinFeedback({ ok: true, text: t("passcodeChangedMsg") });
                                })
                                .catch((e) => setPinFeedback({ ok: false, text: String(e) }))
                                .finally(() => setPinBusy(false));
                            }}
                            className="tf-btn rounded-lg px-3 py-1.5 text-xs font-semibold disabled:opacity-50"
                          >
                            {t("passcodeChangeBtn")}
                          </button>
                          <button
                            type="button"
                            disabled={pinBusy || !currentPin}
                            onClick={() => {
                              setPinBusy(true);
                              setPinFeedback(null);
                              void verifyPasscode(currentPin, passcodeCfg)
                                .then((ok) => {
                                  if (!ok) {
                                    setPinFeedback({
                                      ok: false,
                                      text: t("passcodeWrongCurrent"),
                                    });
                                    return;
                                  }
                                  onUpdatePasscode({
                                    ...passcodeCfg,
                                    enabled: false,
                                    salt: "",
                                    hash: "",
                                  });
                                  setCurrentPin("");
                                  setNewPin("");
                                  setConfirmPin("");
                                  setPinFeedback({ ok: true, text: t("passcodeDisabledMsg") });
                                })
                                .finally(() => setPinBusy(false));
                            }}
                            className="rounded-lg bg-rose-500/15 px-3 py-1.5 text-xs font-medium text-rose-300 hover:bg-rose-500/25 disabled:opacity-50"
                          >
                            {t("passcodeDisableBtn")}
                          </button>
                        </div>
                      </div>
                    )}

                    {pinFeedback && (
                      <div
                        className={`rounded-lg px-3 py-1.5 text-xs ${
                          pinFeedback.ok
                            ? "bg-emerald-500/15 text-emerald-300"
                            : "bg-rose-500/15 text-rose-300"
                        }`}
                      >
                        {pinFeedback.text}
                      </div>
                    )}
                  </div>
                </section>

                <section className="space-y-2">
                  <h4
                    className="text-xs font-semibold uppercase tracking-wider"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    {t("systemTrayHeading")}
                  </h4>
                  <div className="space-y-2 rounded-xl p-2" style={{ background: "var(--bg-surface)" }}>
                    <ToggleRow
                      icon={<ArrowDownToLine className="h-4 w-4" />}
                      label={t("closeToTrayLabel")}
                      desc={t("closeToTrayDesc")}
                      checked={closeToTray}
                      onChange={onCloseToTrayChange}
                    />
                    <div
                      className="flex items-center justify-between border-t px-3 py-2.5"
                      style={{ borderColor: "var(--border-subtle)" }}
                    >
                      <div>
                        <div className="text-[13.5px] font-medium">
                          {t("appVersionTitle", { version: appVersion || "0.4.2" })}
                        </div>
                        <div className="text-xs" style={{ color: "var(--text-secondary)" }}>
                          {availableUpdate
                            ? t("appUpdateReady", { version: availableUpdate.version })
                            : t("appUpdateCheckDesc")}
                        </div>
                      </div>
                      <button
                        type="button"
                        onClick={() => {
                          onClose();
                          onOpenUpdateModal();
                        }}
                        className="tf-btn rounded-lg px-3 py-1.5 text-xs font-semibold"
                      >
                        {availableUpdate ? t("updateNowBtn") : t("checkBtn")}
                      </button>
                    </div>
                  </div>
                </section>

                <section className="space-y-2">
                  <h4
                    className="flex items-center gap-1.5 text-xs font-semibold uppercase tracking-wider"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    <Keyboard className="h-3.5 w-3.5" />
                    {t("shortcutsHeading")}
                  </h4>
                  <div
                    className="space-y-1.5 rounded-xl p-3 text-xs"
                    style={{ background: "var(--bg-surface)" }}
                  >
                    {[
                      { key: "Ctrl + K", desc: t("shortcutGlobalSearch") },
                      { key: "Ctrl + F", desc: t("shortcutInChatSearch") },
                      { key: "Ctrl + Shift + S", desc: t("shortcutScreenshot") },
                      { key: "Ctrl + 1..9", desc: t("shortcutSwitchFolder") },
                      { key: "Ctrl + L", desc: t("shortcutLockApp") },
                      {
                        key: sendOnEnter ? "Shift + Enter" : "Enter",
                        desc: t("shortcutNewline"),
                      },
                      { key: "Double-click", desc: t("shortcutQuickReply") },
                      { key: "Esc", desc: t("shortcutEsc") },
                    ].map((item) => (
                      <div key={item.key} className="flex items-center justify-between gap-2 py-0.5">
                        <span style={{ color: "var(--text-secondary)" }}>{item.desc}</span>
                        <kbd
                          className="rounded-md px-2 py-0.5 font-mono text-[11px] font-semibold"
                          style={{
                            background: "var(--bg-sidebar)",
                            border: "1px solid var(--border-subtle)",
                            color: "var(--accent)",
                          }}
                        >
                          {item.key}
                        </kbd>
                      </div>
                    ))}
                  </div>
                </section>
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
