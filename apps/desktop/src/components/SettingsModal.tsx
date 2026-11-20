import { useEffect, useRef, useState } from "react";
import {
  ArrowDownToLine,
  Bell,
  CheckCircle2,
  Languages,
  Loader2,
  Network,
  Palette,
  RefreshCw,
  Sparkles,
  Volume2,
  X,
  Zap,
} from "lucide-react";
import { bridge } from "../bridge";
import type {
  AppUpdateInfo,
  RelayConfigSummary,
  RelayMode,
  ThemeMode,
  TranslateConfigSummary,
} from "../types";

export type FontSizeScale = "sm" | "md" | "lg";
export type DensityMode = "comfortable" | "compact";

interface Props {
  open: boolean;
  theme: ThemeMode;
  onThemeChange: (t: ThemeMode) => void;
  fontSize: FontSizeScale;
  onFontSizeChange: (f: FontSizeScale) => void;
  density: DensityMode;
  onDensityChange: (d: DensityMode) => void;
  fxEnabled: boolean;
  onFxChange: (v: boolean) => void;
  buzzEnabled: boolean;
  onBuzzChange: (v: boolean) => void;
  desktopNotifyEnabled: boolean;
  onDesktopNotifyChange: (v: boolean) => void;
  soundEnabled: boolean;
  onSoundChange: (v: boolean) => void;
  closeToTray: boolean;
  onCloseToTrayChange: (v: boolean) => void;
  translateCfg: TranslateConfigSummary | null;
  onOpenTranslateModal: () => void;
  relayCfg: RelayConfigSummary | null;
  onRelaySaved: (cfg: RelayConfigSummary) => void;
  focusRelay?: boolean;
  appVersion: string;
  availableUpdate: AppUpdateInfo | null;
  onOpenUpdateModal: () => void;
  onClose: () => void;
}

const THEMES: { id: ThemeMode; label: string; dot: string }[] = [
  { id: "triforce", label: "Obsidian Vàng Kim", dot: "#cfa13a" },
  { id: "night-blue", label: "Night Blue", dot: "#4c82b8" },
  { id: "dark", label: "Dark Tím", dot: "#7c68cf" },
  { id: "light", label: "Light Sáng", dot: "#2e6da4" },
];

function ToggleRow({
  icon,
  label,
  desc,
  checked,
  onChange,
}: {
  icon: React.ReactNode;
  label: string;
  desc?: string;
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <label className="flex cursor-pointer items-center justify-between gap-3 rounded-xl px-3 py-2.5 transition hover:bg-black/10">
      <div className="flex min-w-0 items-center gap-2.5">
        <span className="shrink-0" style={{ color: "var(--accent)" }}>
          {icon}
        </span>
        <div className="min-w-0">
          <div className="text-[14px] font-medium">{label}</div>
          {desc && (
            <div className="text-xs" style={{ color: "var(--text-secondary)" }}>
              {desc}
            </div>
          )}
        </div>
      </div>
      <input
        type="checkbox"
        checked={checked}
        onChange={(e) => onChange(e.target.checked)}
        className="h-4 w-4 shrink-0 cursor-pointer accent-[var(--accent)]"
      />
    </label>
  );
}

export function SettingsModal({
  open,
  theme,
  onThemeChange,
  fontSize,
  onFontSizeChange,
  density,
  onDensityChange,
  fxEnabled,
  onFxChange,
  buzzEnabled,
  onBuzzChange,
  desktopNotifyEnabled,
  onDesktopNotifyChange,
  soundEnabled,
  onSoundChange,
  closeToTray,
  onCloseToTrayChange,
  translateCfg,
  onOpenTranslateModal,
  relayCfg,
  onRelaySaved,
  focusRelay = false,
  appVersion,
  availableUpdate,
  onOpenUpdateModal,
  onClose,
}: Props) {
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
  const relaySectionRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    if (!open) return;
    setRelayEnabled(relayCfg?.enabled ?? false);
    setRelayMode(relayCfg?.mode ?? "socks5");
    setRelayHost(relayCfg?.host ?? "");
    setRelayPort(String(relayCfg?.port || 1088));
    setRelayUser(relayCfg?.username ?? "");
    setRelayPassword("");
    setClearRelayPw(false);
    setRouteMedia(relayCfg?.route_webview_media ?? true);
    setRelayFeedback(null);
    if (focusRelay) {
      setTimeout(() => {
        relaySectionRef.current?.scrollIntoView({ behavior: "smooth", block: "center" });
      }, 40);
    }
  }, [open, relayCfg, focusRelay]);

  if (!open) return null;

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
        text: `Kết nối thành công tới ${payload.host}:${payload.port} (${res.latency_ms} ms)`,
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
          ? `Đã bật & lưu Relay (${saved.mode.toUpperCase()} · ${saved.host}:${saved.port})`
          : "Đã tắt & lưu cấu hình Relay (kết nối trực tiếp)",
      });
    } catch (err) {
      setRelayFeedback({ ok: false, text: String(err) });
    } finally {
      setRelaySaving(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/55 p-4"
      onClick={onClose}
    >
      <div
        className="flex max-h-[88vh] w-full max-w-lg flex-col overflow-hidden rounded-2xl shadow-2xl"
        style={{
          background: "var(--bg-sidebar)",
          border: "1px solid var(--border-subtle)",
          color: "var(--text-primary)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <div
          className="flex items-center justify-between px-5 py-3.5"
          style={{ borderBottom: "1px solid var(--border-subtle)" }}
        >
          <div className="flex items-center gap-2">
            <Palette className="h-5 w-5" style={{ color: "var(--accent)" }} />
            <h3 className="text-[16px] font-semibold">Cài đặt LiteChat</h3>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="rounded-lg p-1.5 hover:bg-black/10"
            title="Đóng"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="flex-1 space-y-5 overflow-y-auto p-5">
          {/* Giao diện */}
          <section className="space-y-3">
            <h4
              className="text-xs font-semibold uppercase tracking-wider"
              style={{ color: "var(--text-secondary)" }}
            >
              Giao diện & Hiển thị
            </h4>
            <div className="grid grid-cols-2 gap-2">
              {THEMES.map((item) => {
                const active = theme === item.id;
                return (
                  <button
                    key={item.id}
                    type="button"
                    onClick={() => onThemeChange(item.id)}
                    className="flex items-center gap-2 rounded-xl px-3 py-2 text-left text-[13.5px] font-medium transition"
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
                    <span className="truncate">{item.label}</span>
                  </button>
                );
              })}
            </div>

            <div className="grid grid-cols-2 gap-3">
              <div>
                <div className="mb-1 text-xs font-medium" style={{ color: "var(--text-secondary)" }}>
                  Cỡ chữ tin nhắn
                </div>
                <div className="grid grid-cols-3 gap-1 rounded-xl p-1" style={{ background: "var(--bg-surface)" }}>
                  {(
                    [
                      { id: "sm", label: "Nhỏ" },
                      { id: "md", label: "Vừa" },
                      { id: "lg", label: "Lớn" },
                    ] as const
                  ).map((opt) => (
                    <button
                      key={opt.id}
                      type="button"
                      onClick={() => onFontSizeChange(opt.id)}
                      className="rounded-lg py-1 text-xs font-medium transition"
                      style={{
                        background: fontSize === opt.id ? "var(--accent)" : "transparent",
                        color: fontSize === opt.id ? "var(--text-on-accent)" : "var(--text-secondary)",
                      }}
                    >
                      {opt.label}
                    </button>
                  ))}
                </div>
              </div>

              <div>
                <div className="mb-1 text-xs font-medium" style={{ color: "var(--text-secondary)" }}>
                  Mật độ danh sách
                </div>
                <div className="grid grid-cols-2 gap-1 rounded-xl p-1" style={{ background: "var(--bg-surface)" }}>
                  {(
                    [
                      { id: "comfortable", label: "Thoáng" },
                      { id: "compact", label: "Gọn" },
                    ] as const
                  ).map((opt) => (
                    <button
                      key={opt.id}
                      type="button"
                      onClick={() => onDensityChange(opt.id)}
                      className="rounded-lg py-1 text-xs font-medium transition"
                      style={{
                        background: density === opt.id ? "var(--accent)" : "transparent",
                        color: density === opt.id ? "var(--text-on-accent)" : "var(--text-secondary)",
                      }}
                    >
                      {opt.label}
                    </button>
                  ))}
                </div>
              </div>
            </div>

            <div className="rounded-xl p-1" style={{ background: "var(--bg-surface)" }}>
              <ToggleRow
                icon={<Sparkles className="h-4 w-4" />}
                label="Hiệu ứng Tiên Sáng"
                desc="Ánh sáng con trỏ và hạt bụi vàng"
                checked={fxEnabled}
                onChange={onFxChange}
              />
            </div>
          </section>

          {/* Thông báo & Hệ thống */}
          <section className="space-y-2">
            <h4
              className="text-xs font-semibold uppercase tracking-wider"
              style={{ color: "var(--text-secondary)" }}
            >
              Thông báo & Hệ thống
            </h4>
            <div className="rounded-xl p-1" style={{ background: "var(--bg-surface)" }}>
              <ToggleRow
                icon={<Bell className="h-4 w-4" />}
                label="Thông báo Desktop"
                desc="Hiện thông báo khi cửa sổ chạy nền"
                checked={desktopNotifyEnabled}
                onChange={onDesktopNotifyChange}
              />
              <ToggleRow
                icon={<Volume2 className="h-4 w-4" />}
                label="Âm báo tin nhắn mới"
                desc="Phát chuông nhẹ khi có tin nhắn từ hội thoại chưa tắt TB"
                checked={soundEnabled}
                onChange={onSoundChange}
              />
              <ToggleRow
                icon={<Zap className="h-4 w-4" />}
                label="Âm thanh & rung khi nhận BUZZ"
                desc="Hiệu ứng gây chú ý khẩn cấp"
                checked={buzzEnabled}
                onChange={onBuzzChange}
              />
              <ToggleRow
                icon={<ArrowDownToLine className="h-4 w-4" />}
                label="Thu nhỏ xuống khay hệ thống khi bấm đóng"
                desc="Giữ kết nối nhận tin nhắn chạy nền ở System Tray"
                checked={closeToTray}
                onChange={onCloseToTrayChange}
              />
            </div>
          </section>

          {/* Mạng nội bộ / Relay */}
          <section ref={relaySectionRef} className="space-y-2.5">
            <h4
              className="text-xs font-semibold uppercase tracking-wider"
              style={{ color: "var(--text-secondary)" }}
            >
              Mạng nội bộ / Máy chủ Relay
            </h4>
            <div className="space-y-3 rounded-xl p-3" style={{ background: "var(--bg-surface)" }}>
              <ToggleRow
                icon={<Network className="h-4 w-4" />}
                label="Sử dụng mạng Relay nội bộ"
                desc="Đi qua máy trung gian (ví dụ 10.7.12.233) cho máy không có Internet"
                checked={relayEnabled}
                onChange={(v) => {
                  setRelayEnabled(v);
                  setRelayFeedback(null);
                }}
              />

              <div className="space-y-2.5 border-t pt-2.5" style={{ borderColor: "var(--border-subtle)" }}>
                <div>
                  <div className="mb-1 text-xs font-medium" style={{ color: "var(--text-secondary)" }}>
                    Giao thức đường truyền
                  </div>
                  <div
                    className="grid grid-cols-2 gap-1 rounded-xl p-1"
                    style={{ background: "var(--bg-sidebar)" }}
                  >
                    {(
                      [
                        { id: "socks5", label: "SOCKS5h (DNS từ xa)" },
                        { id: "http", label: "HTTP CONNECT" },
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
                      Máy chủ Relay (IP / Hostname)
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
                      Cổng
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
                      Tài khoản (nếu có)
                    </label>
                    <input
                      value={relayUser}
                      onChange={(e) => setRelayUser(e.target.value)}
                      placeholder="litechat"
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
                        Mật khẩu
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
                          Xóa MK
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
                          ? "Đã lưu (để trống giữ nguyên)"
                          : "Mật khẩu Relay"
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
                    Định tuyến tải ảnh, avatar & nhãn dán qua Relay
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
                    <span>Kiểm tra kết nối</span>
                  </button>
                  <button
                    type="button"
                    disabled={relaySaving || relayTesting}
                    onClick={() => void handleSaveRelay()}
                    className="tf-btn flex items-center gap-1.5 rounded-lg px-3.5 py-1.5 text-xs font-semibold disabled:opacity-50"
                  >
                    {relaySaving && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
                    <span>Lưu cấu hình Relay</span>
                  </button>
                </div>
              </div>
            </div>
          </section>

          {/* Dịch thuật & Cập nhật */}
          <section className="space-y-2">
            <h4
              className="text-xs font-semibold uppercase tracking-wider"
              style={{ color: "var(--text-secondary)" }}
            >
              Tiện ích & Phiên bản
            </h4>
            <div className="space-y-2">
              <div
                className="flex items-center justify-between rounded-xl px-3.5 py-2.5"
                style={{ background: "var(--bg-surface)" }}
              >
                <div className="flex items-center gap-2.5">
                  <Languages className="h-4 w-4" style={{ color: "var(--accent)" }} />
                  <div>
                    <div className="text-[14px] font-medium">Dịch thuật tin nhắn</div>
                    <div className="text-xs" style={{ color: "var(--text-secondary)" }}>
                      {(translateCfg?.provider || "deepl").toUpperCase()} · Đích{" "}
                      {translateCfg?.target_lang || "VI"}
                    </div>
                  </div>
                </div>
                <button
                  type="button"
                  onClick={() => {
                    onClose();
                    onOpenTranslateModal();
                  }}
                  className="hover-menu rounded-lg px-3 py-1.5 text-xs font-semibold"
                  style={{
                    border: "1px solid var(--border-subtle)",
                    color: "var(--accent)",
                  }}
                >
                  Cấu hình
                </button>
              </div>

              <div
                className="flex items-center justify-between rounded-xl px-3.5 py-2.5"
                style={{ background: "var(--bg-surface)" }}
              >
                <div className="flex items-center gap-2.5">
                  <ArrowDownToLine className="h-4 w-4" style={{ color: "var(--accent)" }} />
                  <div>
                    <div className="text-[14px] font-medium">
                      Phiên bản v{appVersion || "0.1.0"}
                    </div>
                    <div className="text-xs" style={{ color: "var(--text-secondary)" }}>
                      {availableUpdate
                        ? `Đã có bản mới v${availableUpdate.version}`
                        : "Kiểm tra & cài đặt bản cập nhật mới"}
                    </div>
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
                  {availableUpdate ? "Cập nhật ngay" : "Kiểm tra"}
                </button>
              </div>
            </div>
          </section>
        </div>
      </div>
    </div>
  );
}
