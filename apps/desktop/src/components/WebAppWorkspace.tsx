import { useEffect, useRef } from "react";
import {
  ArrowLeft,
  ArrowRight,
  Bookmark,
  Calendar,
  Code2,
  Compass,
  ExternalLink,
  FileText,
  Folder,
  Globe,
  Home,
  LayoutGrid,
  Mail,
  RefreshCw,
  X,
} from "lucide-react";
import { bridge, isTauri } from "../bridge";
import { useI18n } from "../i18n";
import { extractWebAppDomain, type WebAppItem } from "../utils/webApps";

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
  const openedRef = useRef<string | null>(null);

  useEffect(() => {
    if (!isTauri()) return;
    if (suspended) {
      void bridge.webappHideAll();
      return;
    }

    const hostEl = hostRef.current;
    if (!hostEl) return;

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
      const b = getBounds();
      if (b.width < 40 || b.height < 40) return;
      if (openedRef.current !== app.id) {
        openedRef.current = app.id;
        void bridge.webappOpen(app.id, app.url, b).catch((e) => onError?.(e));
      } else {
        void bridge.webappOpen(app.id, app.url, b).catch(() =>
          bridge.webappSetBounds(app.id, b).catch(() => undefined),
        );
      }
    };

    const rafId = window.requestAnimationFrame(syncOpenOrBounds);
    const ro = new ResizeObserver(() => {
      const b = getBounds();
      if (b.width >= 40 && b.height >= 40 && openedRef.current === app.id) {
        void bridge.webappSetBounds(app.id, b).catch(() => undefined);
      }
    });
    ro.observe(hostEl);

    const onResize = () => {
      const b = getBounds();
      if (b.width >= 40 && b.height >= 40 && openedRef.current === app.id) {
        void bridge.webappSetBounds(app.id, b).catch(() => undefined);
      }
    };
    window.addEventListener("resize", onResize);

    return () => {
      window.cancelAnimationFrame(rafId);
      ro.disconnect();
      window.removeEventListener("resize", onResize);
    };
  }, [app.id, app.url, suspended, onError]);

  useEffect(() => {
    return () => {
      if (isTauri()) {
        void bridge.webappHideAll();
      }
    };
  }, []);

  const domain = extractWebAppDomain(app.url);

  return (
    <div className="relative flex min-w-0 flex-1 flex-col overflow-hidden" style={{ background: "var(--bg-stage)" }}>
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
              if (isTauri()) void bridge.webappHideAll();
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
                onClick={() => void bridge.webappNavigate(app.id, "back")}
                title={t("back")}
                className="hover-surface rounded-lg p-1.5 transition"
                style={{ color: "var(--text-secondary)" }}
              >
                <ArrowLeft className="h-4 w-4" />
              </button>
              <button
                type="button"
                onClick={() => void bridge.webappNavigate(app.id, "forward")}
                title={t("forward")}
                className="hover-surface rounded-lg p-1.5 transition"
                style={{ color: "var(--text-secondary)" }}
              >
                <ArrowRight className="h-4 w-4" />
              </button>
              <button
                type="button"
                onClick={() => void bridge.webappNavigate(app.id, "reload")}
                title={t("reload")}
                className="hover-surface rounded-lg p-1.5 transition"
                style={{ color: "var(--text-secondary)" }}
              >
                <RefreshCw className="h-4 w-4" />
              </button>
              <button
                type="button"
                onClick={() => void bridge.webappNavigate(app.id, "home", app.url)}
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
              if (isTauri()) void bridge.webappHideAll();
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
        {!isTauri() ? (
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
        ) : (
          <div className="text-xs opacity-60" style={{ color: "var(--text-secondary)" }}>
            {t("loadingDomain", { domain })}
          </div>
        )}
      </div>
    </div>
  );
}
