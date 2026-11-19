import { ArrowDownToLine, Bell, Languages, Palette, Sparkles, Volume2, X, Zap } from "lucide-react";
import type { Update } from "@tauri-apps/plugin-updater";
import type { ThemeMode, TranslateConfigSummary } from "../types";

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
  appVersion: string;
  availableUpdate: Update | null;
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
  appVersion,
  availableUpdate,
  onOpenUpdateModal,
  onClose,
}: Props) {
  if (!open) return null;

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
