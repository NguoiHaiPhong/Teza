import { useState } from "react";
import { Headphones, Mic, Phone, PhoneIncoming, PhoneOff, RefreshCw } from "lucide-react";
import { useI18n } from "../i18n";
import type { CallDevicesDto, CallSnapshotDto } from "../types";
import { ThreadAvatar } from "./ThreadAvatar";

interface Props {
  active: CallSnapshotDto;
  devices: CallDevicesDto | null;
  devicesLoading: boolean;
  busy: boolean;
  onAnswer: (callId: number) => void;
  onDecline: (callId: number) => void;
  onRefreshDevices: () => void;
  onSelectDevices: (input: string | null, output: string | null) => void;
}

export function IncomingCallModal({
  active,
  devices,
  devicesLoading,
  busy,
  onAnswer,
  onDecline,
  onRefreshDevices,
  onSelectDevices,
}: Props) {
  const { t } = useI18n();
  const [showDevices, setShowDevices] = useState(false);

  return (
    <div className="fixed inset-0 z-[85] flex items-center justify-center bg-black/55 p-4 backdrop-blur-xs">
      <div
        role="dialog"
        aria-modal="true"
        aria-label={t("callIncomingBadge")}
        className="w-full max-w-sm overflow-hidden rounded-2xl p-6 text-center shadow-2xl"
        style={{
          background: "var(--bg-sidebar)",
          border: "1px solid var(--border-subtle)",
          color: "var(--text-primary)",
        }}
      >
        <div
          className="mx-auto mb-3 inline-flex items-center gap-1.5 rounded-full px-3 py-1 text-xs font-semibold"
          style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
        >
          <PhoneIncoming className="h-3.5 w-3.5 animate-pulse" />
          <span>{t("callIncomingBadge")}</span>
        </div>

        <div className="relative mx-auto my-2 flex h-20 w-20 items-center justify-center">
          <span
            aria-hidden="true"
            className="absolute inset-0 animate-ping rounded-full opacity-25"
            style={{ background: "var(--accent)" }}
          />
          <div className="relative rounded-full ring-2 ring-[var(--accent)] ring-offset-2 ring-offset-transparent">
            <ThreadAvatar
              avatar={active.partner_avatar}
              title={active.partner_name || active.partner_id}
              kind="direct"
              size="lg"
            />
          </div>
        </div>

        <h3 className="mt-3 truncate text-lg font-bold">
          {active.partner_name || active.partner_id}
        </h3>
        <p className="mt-0.5 text-xs" style={{ color: "var(--text-secondary)" }}>
          {t("callIncomingRinging")}
        </p>

        <div className="mt-5 flex items-center justify-center gap-5">
          <button
            type="button"
            disabled={busy}
            onClick={() => onDecline(active.call_id)}
            className="flex min-w-[112px] items-center justify-center gap-2 rounded-full bg-rose-600 px-5 py-2.5 text-sm font-semibold text-white shadow-lg transition hover:bg-rose-500 disabled:opacity-50"
          >
            <PhoneOff className="h-4 w-4" />
            <span>{t("callDeclineBtn")}</span>
          </button>
          <button
            type="button"
            disabled={busy}
            onClick={() => onAnswer(active.call_id)}
            className="flex min-w-[112px] items-center justify-center gap-2 rounded-full bg-emerald-600 px-5 py-2.5 text-sm font-semibold text-white shadow-lg transition hover:bg-emerald-500 disabled:opacity-50"
          >
            <Phone className="h-4 w-4" />
            <span>{t("callAnswerBtn")}</span>
          </button>
        </div>

        <div className="mt-4">
          <button
            type="button"
            onClick={() => {
              const next = !showDevices;
              setShowDevices(next);
              if (next) onRefreshDevices();
            }}
            className="hover-menu inline-flex items-center gap-1.5 rounded-lg px-2.5 py-1 text-xs font-medium transition"
            style={{ color: "var(--text-secondary)" }}
          >
            <Headphones className="h-3.5 w-3.5" />
            <span>{t("callAudioDevicesBtn")}</span>
          </button>

          {showDevices && (
            <div
              className="mt-2.5 space-y-2 rounded-xl p-3 text-left text-xs"
              style={{
                background: "var(--bg-surface)",
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
              <label className="block space-y-1">
                <span className="flex items-center gap-1 text-[11px]" style={{ color: "var(--text-secondary)" }}>
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
                    background: "var(--bg-sidebar)",
                    border: "1px solid var(--border-subtle)",
                    color: "var(--text-primary)",
                  }}
                >
                  <option value="">{t("callOsDefaultDevice")}</option>
                  {(devices?.inputs ?? []).map((dev) => (
                    <option key={dev.name} value={dev.name}>
                      {dev.name}
                    </option>
                  ))}
                </select>
              </label>
              <label className="block space-y-1">
                <span className="flex items-center gap-1 text-[11px]" style={{ color: "var(--text-secondary)" }}>
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
                    background: "var(--bg-sidebar)",
                    border: "1px solid var(--border-subtle)",
                    color: "var(--text-primary)",
                  }}
                >
                  <option value="">{t("callOsDefaultDevice")}</option>
                  {(devices?.outputs ?? []).map((dev) => (
                    <option key={dev.name} value={dev.name}>
                      {dev.name}
                    </option>
                  ))}
                </select>
              </label>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
