import { ArrowDownToLine, Sparkles, X } from "lucide-react";
import { useI18n } from "../i18n";
import type { AppUpdateInfo } from "../types";
import { formatUpdateVersionTag, normalizeUpdateVersion } from "../utils/updateBanner";

export interface UpdateBannerProps {
  availableUpdate: AppUpdateInfo;
  onOpenUpdateModal: () => void;
  onDismiss: () => void;
  className?: string;
}

export function UpdateBanner({
  availableUpdate,
  onOpenUpdateModal,
  onDismiss,
  className = "",
}: UpdateBannerProps) {
  const { t } = useI18n();
  const normalizedVersion = normalizeUpdateVersion(availableUpdate.version);
  if (!normalizedVersion) return null;

  const versionTag = formatUpdateVersionTag(normalizedVersion);
  const message = t("updateBannerText", { version: normalizedVersion });

  return (
    <div
      role="status"
      aria-live="polite"
      className={`flex shrink-0 items-center justify-between gap-2.5 px-3 py-1.5 ${className}`.trim()}
      style={{
        background: "linear-gradient(90deg, var(--accent-soft), var(--bg-surface))",
        borderBottom: "1px solid var(--border-subtle)",
        color: "var(--text-primary)",
      }}
    >
      <div className="flex min-w-0 flex-1 items-center gap-2">
        <Sparkles
          className="h-3.5 w-3.5 shrink-0"
          style={{ color: "var(--accent)" }}
          aria-hidden="true"
        />
        <span
          className="inline-flex shrink-0 items-center gap-1 rounded-full px-2 py-0.5 font-mono text-[11px] font-bold leading-none"
          style={{
            background: "var(--accent-soft)",
            color: "var(--accent)",
            border: "1px solid var(--border-subtle)",
          }}
        >
          <span
            className="h-1.5 w-1.5 rounded-full bg-emerald-500"
            aria-hidden="true"
          />
          {versionTag}
        </span>
        <span
          className="min-w-0 truncate text-[12.5px] font-medium"
          title={message}
        >
          {message}
        </span>
      </div>

      <div className="flex shrink-0 items-center gap-1.5">
        <button
          type="button"
          onClick={onOpenUpdateModal}
          className="tf-btn inline-flex items-center gap-1.5 rounded-lg px-2.5 py-1 text-[12px] font-semibold leading-tight"
        >
          <ArrowDownToLine className="h-3.5 w-3.5 shrink-0" />
          <span>{t("updateNowBtn")}</span>
        </button>
        <button
          type="button"
          onClick={onDismiss}
          title={t("updateLaterTitle")}
          aria-label={t("updateLaterTitle")}
          className="hover-surface inline-flex items-center justify-center rounded-lg p-1 transition"
          style={{ color: "var(--text-secondary)" }}
        >
          <X className="h-3.5 w-3.5" />
        </button>
      </div>
    </div>
  );
}
