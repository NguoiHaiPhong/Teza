import { useMemo, useState } from "react";
import {
  ExternalLink,
  Globe,
  LayoutGrid,
  MonitorSmartphone,
  Pencil,
  Plus,
  Search,
  Trash2,
  X,
} from "lucide-react";
import { useI18n } from "../i18n";
import {
  extractWebAppDomain,
  normalizeWebUrl,
  type WebAppItem,
  type WebAppOpenMode,
  type WebAppsConfig,
} from "../utils/webApps";
import { WEBAPP_ICON_OPTIONS, renderWebAppIcon } from "./WebAppWorkspace";

interface Props {
  config: WebAppsConfig;
  lastActiveId?: string | null;
  onChangeConfig: (next: WebAppsConfig) => void;
  onLaunch: (item: WebAppItem) => void;
  onOpenExternal: (url: string) => void;
  onCloseToChat: () => void;
}

export function WebAppHub({
  config,
  lastActiveId = null,
  onChangeConfig,
  onLaunch,
  onOpenExternal,
  onCloseToChat,
}: Props) {
  const { t } = useI18n();
  const [query, setQuery] = useState("");
  const [formOpen, setFormOpen] = useState(false);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [icon, setIcon] = useState("globe");
  const [openMode, setOpenMode] = useState<WebAppOpenMode>("embedded");
  const [formError, setFormError] = useState<string | null>(null);

  const resetForm = () => {
    setEditingId(null);
    setName("");
    setUrl("");
    setIcon("globe");
    setOpenMode("embedded");
    setFormError(null);
    setFormOpen(false);
  };

  const openAddForm = () => {
    setEditingId(null);
    setName("");
    setUrl("");
    setIcon("globe");
    setOpenMode("embedded");
    setFormError(null);
    setFormOpen(true);
  };

  const openEditForm = (item: WebAppItem) => {
    setEditingId(item.id);
    setName(item.name);
    setUrl(item.url);
    setIcon(item.icon || "globe");
    setOpenMode(item.openMode);
    setFormError(null);
    setFormOpen(true);
  };

  const handleSubmitForm = (e: React.FormEvent) => {
    e.preventDefault();
    const cleanName = name.trim();
    const normalizedUrl = normalizeWebUrl(url);
    if (!cleanName) {
      setFormError(t("webappErrNoName"));
      return;
    }
    if (!normalizedUrl) {
      setFormError(t("webappErrBadUrl"));
      return;
    }

    if (editingId) {
      const nextItems = config.items.map((x) =>
        x.id === editingId
          ? { ...x, name: cleanName, url: normalizedUrl, icon, openMode }
          : x,
      );
      onChangeConfig({ ...config, items: nextItems });
    } else {
      const nextItem: WebAppItem = {
        id: `wa-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`,
        name: cleanName,
        url: normalizedUrl,
        icon,
        openMode,
      };
      onChangeConfig({ ...config, enabled: true, items: [...config.items, nextItem] });
    }
    resetForm();
  };

  const handleDelete = (item: WebAppItem) => {
    onChangeConfig({
      ...config,
      items: config.items.filter((x) => x.id !== item.id),
    });
    if (editingId === item.id) {
      resetForm();
    }
  };

  const filteredItems = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return config.items;
    return config.items.filter(
      (item) =>
        item.name.toLowerCase().includes(q) ||
        item.url.toLowerCase().includes(q) ||
        extractWebAppDomain(item.url).toLowerCase().includes(q),
    );
  }, [config.items, query]);

  return (
    <div
      className="relative z-10 flex min-w-0 flex-1 flex-col overflow-hidden"
      style={{ background: "var(--bg-stage)" }}
    >
      <header
        className="flex shrink-0 flex-wrap items-center justify-between gap-3 px-5 py-3.5"
        style={{
          background: "var(--bg-sidebar)",
          borderBottom: "1px solid var(--border-subtle)",
        }}
      >
        <div className="flex min-w-0 items-center gap-3">
          <div
            className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl"
            style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
          >
            <LayoutGrid className="h-5 w-5" />
          </div>
          <div className="min-w-0">
            <h1 className="truncate text-[16px] font-bold">{t("webappHubTitle")}</h1>
            <p className="truncate text-xs" style={{ color: "var(--text-secondary)" }}>
              {config.items.length > 0
                ? t("webappCountSubtitle", { count: config.items.length })
                : t("webappHubSubtitle")}
            </p>
          </div>
        </div>

        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={() => (formOpen && !editingId ? resetForm() : openAddForm())}
            className="tf-btn flex items-center gap-1.5 rounded-xl px-3.5 py-2 text-xs font-semibold"
          >
            <Plus className="h-4 w-4" />
            <span>{t("webappAddTitle")}</span>
          </button>
          <button
            type="button"
            onClick={onCloseToChat}
            title={t("returnToChat")}
            className="hover-surface rounded-xl p-2 transition"
            style={{ color: "var(--text-secondary)", background: "var(--bg-surface)" }}
          >
            <X className="h-4 w-4" />
          </button>
        </div>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto p-5">
        <div className="mx-auto max-w-5xl space-y-5">
          {formOpen && (
            <form
              onSubmit={handleSubmitForm}
              className="space-y-3 rounded-2xl p-4 shadow-lg"
              style={{
                background: "var(--bg-sidebar)",
                border: "1px solid var(--border-subtle)",
              }}
            >
              <div className="flex items-center justify-between">
                <div className="text-sm font-semibold">
                  {editingId ? t("webappEditTitle") : t("webappAddTitle")}
                </div>
                <button
                  type="button"
                  onClick={resetForm}
                  className="hover-menu rounded-lg p-1"
                  style={{ color: "var(--text-secondary)" }}
                >
                  <X className="h-4 w-4" />
                </button>
              </div>

              <div className="grid grid-cols-1 gap-2.5 sm:grid-cols-2">
                <input
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  placeholder={t("webappNamePlaceholder")}
                  className="rounded-xl px-3 py-2 text-sm outline-none"
                  style={{
                    background: "var(--bg-surface)",
                    border: "1px solid var(--border-subtle)",
                  }}
                />
                <input
                  value={url}
                  onChange={(e) => setUrl(e.target.value)}
                  placeholder={t("webappUrlPlaceholder")}
                  className="rounded-xl px-3 py-2 text-sm outline-none"
                  style={{
                    background: "var(--bg-surface)",
                    border: "1px solid var(--border-subtle)",
                  }}
                />
              </div>

              <div className="flex flex-wrap items-center justify-between gap-3">
                <div className="flex flex-wrap gap-1">
                  {WEBAPP_ICON_OPTIONS.map((opt) => {
                    const active = icon === opt.id;
                    return (
                      <button
                        key={opt.id}
                        type="button"
                        title={opt.label}
                        onClick={() => setIcon(opt.id)}
                        className="rounded-lg p-2 transition"
                        style={{
                          background: active ? "var(--accent)" : "var(--bg-surface)",
                          color: active ? "var(--text-on-accent)" : "var(--text-secondary)",
                        }}
                      >
                        <opt.icon className="h-4 w-4" />
                      </button>
                    );
                  })}
                </div>

                <div
                  className="flex items-center gap-1 rounded-xl p-1 text-xs font-medium"
                  style={{ background: "var(--bg-surface)" }}
                >
                  {(
                    [
                      { id: "embedded", label: t("webappModeEmbeddedShort") },
                      { id: "external", label: t("webappModeExternalShort") },
                    ] as const
                  ).map((modeOpt) => {
                    const active = openMode === modeOpt.id;
                    return (
                      <button
                        key={modeOpt.id}
                        type="button"
                        onClick={() => setOpenMode(modeOpt.id)}
                        className="rounded-lg px-2.5 py-1 transition"
                        style={{
                          background: active ? "var(--accent)" : "transparent",
                          color: active ? "var(--text-on-accent)" : "var(--text-secondary)",
                        }}
                      >
                        {modeOpt.label}
                      </button>
                    );
                  })}
                </div>
              </div>

              {formError && (
                <div
                  className="rounded-xl px-3 py-2 text-xs"
                  style={{ background: "var(--danger-bg)", color: "var(--danger-text)" }}
                >
                  {formError}
                </div>
              )}

              <div className="flex justify-end gap-2">
                <button
                  type="button"
                  onClick={resetForm}
                  className="hover-surface rounded-xl px-3.5 py-1.5 text-xs font-medium"
                  style={{ background: "var(--bg-surface)" }}
                >
                  {t("cancel")}
                </button>
                <button
                  type="submit"
                  className="tf-btn rounded-xl px-4 py-1.5 text-xs font-semibold"
                >
                  {editingId ? t("webappBtnUpdate") : t("webappBtnAdd")}
                </button>
              </div>
            </form>
          )}

          {config.items.length > 0 && (
            <div
              className="flex items-center gap-2.5 rounded-xl px-3.5 py-2"
              style={{
                background: "var(--bg-sidebar)",
                border: "1px solid var(--border-subtle)",
              }}
            >
              <Search className="h-4 w-4 shrink-0" style={{ color: "var(--text-secondary)" }} />
              <input
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder={t("webappSearchPlaceholder")}
                className="min-w-0 flex-1 bg-transparent text-sm outline-none"
              />
              {query && (
                <button
                  type="button"
                  onClick={() => setQuery("")}
                  className="hover-menu rounded p-1"
                  style={{ color: "var(--text-secondary)" }}
                >
                  <X className="h-3.5 w-3.5" />
                </button>
              )}
            </div>
          )}

          {config.items.length === 0 ? (
            <div
              className="flex flex-col items-center justify-center rounded-2xl px-6 py-16 text-center"
              style={{
                background: "var(--bg-sidebar)",
                border: "1px dashed var(--border-subtle)",
              }}
            >
              <div
                className="mb-3 flex h-14 w-14 items-center justify-center rounded-2xl"
                style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
              >
                <Globe className="h-7 w-7" />
              </div>
              <div className="text-base font-semibold">{t("webappHubTitle")}</div>
              <p className="mt-1 max-w-md text-xs leading-relaxed" style={{ color: "var(--text-secondary)" }}>
                {t("webappHubSubtitle")}
              </p>
              <button
                type="button"
                onClick={openAddForm}
                className="tf-btn mt-4 inline-flex items-center gap-1.5 rounded-xl px-4 py-2 text-xs font-semibold"
              >
                <Plus className="h-4 w-4" />
                <span>{t("webappAddFirstBtn")}</span>
              </button>
            </div>
          ) : filteredItems.length === 0 ? (
            <div
              className="rounded-2xl py-12 text-center text-sm"
              style={{
                background: "var(--bg-sidebar)",
                color: "var(--text-secondary)",
              }}
            >
              {t("webappNoMatch")}
            </div>
          ) : (
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
              {filteredItems.map((item) => {
                const domain = extractWebAppDomain(item.url);
                const isLastActive = lastActiveId === item.id;
                return (
                  <div
                    key={item.id}
                    role="button"
                    tabIndex={0}
                    onClick={() => onLaunch(item)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        onLaunch(item);
                      }
                    }}
                    className="group relative flex cursor-pointer flex-col justify-between gap-3 rounded-2xl p-4 text-left transition hover:-translate-y-0.5"
                    style={{
                      background: "var(--bg-sidebar)",
                      border: isLastActive
                        ? "1px solid var(--accent)"
                        : "1px solid var(--border-subtle)",
                    }}
                  >
                    <div className="flex items-start justify-between gap-2">
                      <div className="flex min-w-0 items-center gap-3">
                        <div
                          className="flex h-11 w-11 shrink-0 items-center justify-center rounded-xl"
                          style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
                        >
                          {renderWebAppIcon(item.icon, "h-5 w-5")}
                        </div>
                        <div className="min-w-0">
                          <div className="flex items-center gap-1.5">
                            <span className="truncate text-[14.5px] font-semibold">{item.name}</span>
                            {isLastActive && (
                              <span
                                className="shrink-0 rounded-full px-2 py-0.5 text-[10px] font-bold"
                                style={{
                                  background: "var(--accent-soft)",
                                  color: "var(--accent)",
                                }}
                              >
                                {t("webappRunningBadge")}
                              </span>
                            )}
                          </div>
                          <div className="truncate text-xs" style={{ color: "var(--text-secondary)" }}>
                            {domain}
                          </div>
                        </div>
                      </div>

                      <div
                        className="flex shrink-0 items-center gap-0.5 opacity-75 transition group-hover:opacity-100"
                        onClick={(e) => e.stopPropagation()}
                        onKeyDown={(e) => e.stopPropagation()}
                      >
                        <button
                          type="button"
                          onClick={() => onOpenExternal(item.url)}
                          title={t("openExternalBrowser")}
                          className="hover-menu rounded-lg p-1.5"
                          style={{ color: "var(--text-secondary)" }}
                        >
                          <ExternalLink className="h-3.5 w-3.5" />
                        </button>
                        <button
                          type="button"
                          onClick={() => openEditForm(item)}
                          title={t("edit")}
                          className="hover-menu rounded-lg p-1.5"
                          style={{ color: "var(--text-secondary)" }}
                        >
                          <Pencil className="h-3.5 w-3.5" />
                        </button>
                        <button
                          type="button"
                          onClick={() => handleDelete(item)}
                          title={t("delete")}
                          className="hover-menu rounded-lg p-1.5 text-rose-400"
                        >
                          <Trash2 className="h-3.5 w-3.5" />
                        </button>
                      </div>
                    </div>

                    <div className="flex items-center justify-between gap-2 border-t pt-2.5 text-xs" style={{ borderColor: "var(--border-subtle)" }}>
                      <span
                        className="inline-flex items-center gap-1 font-medium"
                        style={{ color: "var(--text-secondary)" }}
                      >
                        {item.openMode === "external" ? (
                          <ExternalLink className="h-3.5 w-3.5" />
                        ) : (
                          <MonitorSmartphone className="h-3.5 w-3.5" style={{ color: "var(--accent)" }} />
                        )}
                        <span>
                          {item.openMode === "external"
                            ? t("webappModeExternalShort")
                            : t("webappModeEmbeddedShort")}
                        </span>
                      </span>
                      <span className="font-semibold" style={{ color: "var(--accent)" }}>
                        {item.openMode === "external" ? t("openExternalBrowser") : t("webappOpenEmbedded")} →
                      </span>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
