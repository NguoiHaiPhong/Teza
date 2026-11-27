import { useEffect, useMemo, useState } from "react";
import {
  ArrowDown,
  ArrowUp,
  Ban,
  Bookmark,
  Briefcase,
  Check,
  Flame,
  Folder,
  GripVertical,
  Heart,
  Loader2,
  Plus,
  RefreshCw,
  Search,
  Shield,
  Star,
  Tag,
  Trash2,
  User,
  Users,
  X,
} from "lucide-react";
import { useI18n } from "../i18n";
import type { ChatFolderDto, ThreadDto, ThreadKind } from "../types";
import { filterThreadsBySearchQuery, isImeComposingEvent } from "../utils/searchNormalize";
import { moveArrayItem } from "../utils/threadState";
import { ThreadAvatar } from "./ThreadAvatar";

export const FOLDER_ICON_OPTIONS: { id: string; label: string; icon: typeof Folder }[] = [
  { id: "folder", label: "Thư mục", icon: Folder },
  { id: "briefcase", label: "Công việc", icon: Briefcase },
  { id: "star", label: "Quan trọng", icon: Star },
  { id: "heart", label: "Gia đình", icon: Heart },
  { id: "users", label: "Nhóm", icon: Users },
  { id: "user", label: "Cá nhân", icon: User },
  { id: "tag", label: "Thẻ nhãn", icon: Tag },
  { id: "bookmark", label: "Lưu trữ", icon: Bookmark },
  { id: "flame", label: "Ưu tiên", icon: Flame },
  { id: "shield", label: "Nội bộ", icon: Shield },
];

export function renderFolderIcon(iconId: string, className = "h-5 w-5") {
  const found = FOLDER_ICON_OPTIONS.find((item) => item.id === iconId);
  const Icon = found ? found.icon : Folder;
  return <Icon className={className} />;
}

interface Props {
  open: boolean;
  isLocked?: boolean;
  folders: ChatFolderDto[];
  threads: ThreadDto[];
  initialEditingFolder?: ChatFolderDto | null;
  onClose: () => void;
  onSaveFolder: (folder: ChatFolderDto) => Promise<void>;
  onDeleteFolder: (folderId: string) => Promise<void>;
  onReorderFolders?: (orderedIds: string[]) => Promise<void>;
  onSyncRemoteLabels: () => Promise<number>;
}

function emptyDraft(position: number): ChatFolderDto {
  return {
    id: `fld-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`,
    name: "",
    icon: "folder",
    color: "",
    thread_ids: [],
    excluded_thread_ids: [],
    include_kinds: [],
    only_unread: false,
    exclude_muted: false,
    position,
    remote_label_id: null,
  };
}

export function FolderEditorModal({
  open,
  isLocked = false,
  folders,
  threads,
  initialEditingFolder = null,
  onClose,
  onSaveFolder,
  onDeleteFolder,
  onReorderFolders,
  onSyncRemoteLabels,
}: Props) {
  const { t } = useI18n();
  const [editing, setEditing] = useState<ChatFolderDto>(() => emptyDraft(1));
  const [threadSearch, setThreadSearch] = useState("");
  const [threadListTab, setThreadListTab] = useState<"include" | "exclude">("include");
  const [dragFolderIdx, setDragFolderIdx] = useState<number | null>(null);
  const [saving, setSaving] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const [statusMsg, setStatusMsg] = useState<string | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setThreadSearch("");
    setThreadListTab("include");
    setDragFolderIdx(null);
    setStatusMsg(null);
    setErrorMsg(null);
    if (initialEditingFolder) {
      setEditing({
        ...initialEditingFolder,
        excluded_thread_ids: initialEditingFolder.excluded_thread_ids ?? [],
      });
    } else {
      const nextPos = folders.reduce((m, f) => Math.max(m, f.position), 0) + 1;
      setEditing(emptyDraft(nextPos));
    }
  }, [open, initialEditingFolder, folders.length]);

  useEffect(() => {
    if (!open || isLocked) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (isImeComposingEvent(e) || e.defaultPrevented) return;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        if (!saving) onClose();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, isLocked, saving, onClose]);

  const filteredThreads = useMemo(
    () => filterThreadsBySearchQuery(threads, threadSearch, 120),
    [threads, threadSearch],
  );

  if (!open) return null;

  const isExisting = folders.some((f) => f.id === editing.id);

  const toggleThreadId = (tid: string) => {
    setEditing((prev) => {
      const exists = prev.thread_ids.includes(tid);
      return {
        ...prev,
        thread_ids: exists
          ? prev.thread_ids.filter((x) => x !== tid)
          : [...prev.thread_ids, tid],
        excluded_thread_ids: exists
          ? (prev.excluded_thread_ids ?? [])
          : (prev.excluded_thread_ids ?? []).filter((x) => x !== tid),
      };
    });
  };

  const toggleExcludedThreadId = (tid: string) => {
    setEditing((prev) => {
      const curr = prev.excluded_thread_ids ?? [];
      const exists = curr.includes(tid);
      return {
        ...prev,
        excluded_thread_ids: exists ? curr.filter((x) => x !== tid) : [...curr, tid],
        thread_ids: exists ? prev.thread_ids : prev.thread_ids.filter((x) => x !== tid),
      };
    });
  };

  const handleMoveFolder = async (fromIdx: number, toIdx: number) => {
    if (!onReorderFolders || fromIdx === toIdx) return;
    const reordered = moveArrayItem(folders, fromIdx, toIdx);
    if (reordered === folders) return;
    setErrorMsg(null);
    try {
      await onReorderFolders(reordered.map((f) => f.id));
    } catch (err) {
      setErrorMsg(String(err));
    }
  };

  const toggleKind = (kind: ThreadKind) => {
    setEditing((prev) => {
      const exists = prev.include_kinds.includes(kind);
      return {
        ...prev,
        include_kinds: exists
          ? prev.include_kinds.filter((k) => k !== kind)
          : [...prev.include_kinds, kind],
      };
    });
  };

  const handleSave = async () => {
    const trimmed = editing.name.trim();
    if (!trimmed) {
      setErrorMsg("Vui lòng nhập tên thư mục");
      return;
    }
    setSaving(true);
    setErrorMsg(null);
    setStatusMsg(null);
    try {
      await onSaveFolder({ ...editing, name: trimmed });
      setStatusMsg(`Đã lưu thư mục "${trimmed}"`);
      if (!isExisting) {
        const nextPos = folders.reduce((m, f) => Math.max(m, f.position), editing.position) + 1;
        setEditing(emptyDraft(nextPos));
      }
    } catch (err) {
      setErrorMsg(String(err));
    } finally {
      setSaving(false);
    }
  };

  const handleSyncLabels = async () => {
    setSyncing(true);
    setErrorMsg(null);
    setStatusMsg(null);
    try {
      const count = await onSyncRemoteLabels();
      setStatusMsg(
        count > 0
          ? `Đã đồng bộ ${count} thư mục phân loại từ tài khoản`
          : "Tài khoản chưa có thẻ phân loại nào từ máy chủ",
      );
    } catch (err) {
      setErrorMsg(String(err));
    } finally {
      setSyncing(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/55 p-4 backdrop-blur-xs"
      onClick={onClose}
    >
      <div
        className="flex max-h-[88vh] w-full max-w-3xl flex-col overflow-hidden rounded-2xl shadow-2xl"
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
          <div className="flex items-center gap-2 font-semibold">
            <Folder className="h-5 w-5" style={{ color: "var(--accent)" }} />
            <span className="text-[16px]">{t("folderModalTitle")}</span>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="hover-menu rounded-lg p-1.5"
            title={t("close")}
          >
            <X className="h-4 w-4" style={{ color: "var(--text-secondary)" }} />
          </button>
        </div>

        <div className="flex min-h-0 flex-1 flex-col md:flex-row">
          {/* Left: Folder list + create/sync */}
          <div
            className="flex w-full shrink-0 flex-col gap-2 p-3.5 md:w-64"
            style={{ borderRight: "1px solid var(--border-subtle)" }}
          >
            <div className="flex gap-1.5">
              <button
                type="button"
                onClick={() => {
                  const nextPos = folders.reduce((m, f) => Math.max(m, f.position), 0) + 1;
                  setEditing(emptyDraft(nextPos));
                  setErrorMsg(null);
                  setStatusMsg(null);
                }}
                className="tf-btn flex flex-1 items-center justify-center gap-1.5 rounded-xl px-3 py-2 text-xs font-semibold"
              >
                <Plus className="h-3.5 w-3.5" />
                <span>{t("createFolderBtn")}</span>
              </button>
              <button
                type="button"
                disabled={syncing}
                onClick={() => void handleSyncLabels()}
                title={t("syncTagsTitle")}
                className="hover-menu flex items-center justify-center gap-1 rounded-xl px-2.5 py-2 text-xs font-medium disabled:opacity-50"
                style={{
                  background: "var(--bg-surface)",
                  border: "1px solid var(--border-subtle)",
                }}
              >
                {syncing ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <RefreshCw className="h-3.5 w-3.5" />
                )}
                <span>{t("syncTagsBtn")}</span>
              </button>
            </div>

            <div className="max-h-48 flex-1 space-y-1 overflow-y-auto md:max-h-none">
              {folders.length === 0 ? (
                <div className="py-6 text-center text-xs" style={{ color: "var(--text-secondary)" }}>
                  {t("noCustomFoldersHint")}
                </div>
              ) : (
                folders.map((f, idx) => {
                  const active = f.id === editing.id;
                  return (
                    <div
                      key={f.id}
                      draggable={Boolean(onReorderFolders)}
                      onDragStart={(e) => {
                        setDragFolderIdx(idx);
                        e.dataTransfer.effectAllowed = "move";
                        e.dataTransfer.setData("text/plain", f.id);
                      }}
                      onDragOver={(e) => {
                        if (dragFolderIdx === null) return;
                        e.preventDefault();
                        e.dataTransfer.dropEffect = "move";
                      }}
                      onDrop={(e) => {
                        e.preventDefault();
                        if (dragFolderIdx !== null && dragFolderIdx !== idx) {
                          void handleMoveFolder(dragFolderIdx, idx);
                        }
                        setDragFolderIdx(null);
                      }}
                      onDragEnd={() => setDragFolderIdx(null)}
                      className="flex items-center justify-between gap-1 rounded-xl px-2 py-2 transition"
                      style={{
                        background: active ? "var(--bg-active)" : "var(--bg-surface)",
                        color: active ? "var(--text-on-active)" : "var(--text-primary)",
                        opacity: dragFolderIdx === idx ? 0.55 : 1,
                      }}
                    >
                      {onReorderFolders && (
                        <span
                          className="cursor-grab opacity-55 hover:opacity-100"
                          title="Kéo thả để đổi thứ tự"
                        >
                          <GripVertical className="h-3.5 w-3.5" />
                        </span>
                      )}
                      <button
                        type="button"
                        onClick={() => {
                          setEditing({
                            ...f,
                            excluded_thread_ids: f.excluded_thread_ids ?? [],
                          });
                          setErrorMsg(null);
                          setStatusMsg(null);
                        }}
                        className="flex min-w-0 flex-1 items-center gap-2 text-left text-[13.5px] font-medium"
                      >
                        <span className="shrink-0" style={{ color: active ? "inherit" : "var(--accent)" }}>
                          {renderFolderIcon(f.icon, "h-4 w-4")}
                        </span>
                        <span className="truncate">{f.name}</span>
                        <span className="shrink-0 text-[11px] opacity-70">
                          ({f.thread_ids.length})
                        </span>
                      </button>
                      {onReorderFolders && folders.length > 1 && (
                        <div className="flex shrink-0 items-center">
                          <button
                            type="button"
                            disabled={idx === 0}
                            onClick={() => void handleMoveFolder(idx, idx - 1)}
                            title={t("moveUpTitle")}
                            className="rounded p-0.5 opacity-65 transition hover:opacity-100 disabled:opacity-25"
                          >
                            <ArrowUp className="h-3 w-3" />
                          </button>
                          <button
                            type="button"
                            disabled={idx === folders.length - 1}
                            onClick={() => void handleMoveFolder(idx, idx + 1)}
                            title={t("moveDownTitle")}
                            className="rounded p-0.5 opacity-65 transition hover:opacity-100 disabled:opacity-25"
                          >
                            <ArrowDown className="h-3 w-3" />
                          </button>
                        </div>
                      )}
                      <button
                        type="button"
                        onClick={() => {
                          void onDeleteFolder(f.id).then(() => {
                            if (editing.id === f.id) {
                              const nextPos = folders.reduce((m, x) => Math.max(m, x.position), 0) + 1;
                              setEditing(emptyDraft(nextPos));
                            }
                          });
                        }}
                        title={t("deleteFolder")}
                        className="rounded-lg p-1 opacity-70 transition hover:text-rose-400 hover:opacity-100"
                      >
                        <Trash2 className="h-3.5 w-3.5" />
                      </button>
                    </div>
                  );
                })
              )}
            </div>
          </div>

          {/* Right: Folder form */}
          <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto p-4">
            {errorMsg && (
              <div className="rounded-xl bg-rose-500/15 px-3 py-2 text-xs text-rose-300">
                {errorMsg}
              </div>
            )}
            {statusMsg && (
              <div className="rounded-xl bg-emerald-500/15 px-3 py-2 text-xs text-emerald-300">
                {statusMsg}
              </div>
            )}

            <div className="grid grid-cols-1 gap-3 sm:grid-cols-[1fr_auto]">
              <div>
                <label className="mb-1 block text-xs font-medium" style={{ color: "var(--text-secondary)" }}>
                  {t("folderNameLabel")}
                </label>
                <input
                  value={editing.name}
                  onChange={(e) => setEditing((prev) => ({ ...prev, name: e.target.value }))}
                  placeholder={t("folderNamePlaceholder")}
                  className="w-full rounded-xl px-3 py-2 text-[14px] outline-none"
                  style={{
                    background: "var(--bg-surface)",
                    border: "1px solid var(--border-subtle)",
                  }}
                />
              </div>
              <div>
                <label className="mb-1 block text-xs font-medium" style={{ color: "var(--text-secondary)" }}>
                  {t("folderIconLabel")}
                </label>
                <div className="flex flex-wrap gap-1">
                  {FOLDER_ICON_OPTIONS.map((opt) => {
                    const Icon = opt.icon;
                    const active = editing.icon === opt.id;
                    return (
                      <button
                        key={opt.id}
                        type="button"
                        title={opt.label}
                        onClick={() => setEditing((prev) => ({ ...prev, icon: opt.id }))}
                        className="rounded-lg p-2 transition"
                        style={{
                          background: active ? "var(--accent)" : "var(--bg-surface)",
                          color: active ? "var(--text-on-accent)" : "var(--text-secondary)",
                        }}
                      >
                        <Icon className="h-4 w-4" />
                      </button>
                    );
                  })}
                </div>
              </div>
            </div>

            <div>
              <div className="mb-1.5 text-xs font-medium" style={{ color: "var(--text-secondary)" }}>
                {t("folderAutoRulesLabel")}
              </div>
              <div className="flex flex-wrap gap-2 text-xs">
                {(
                  [
                    { label: t("ruleIncludeDirect"), active: editing.include_kinds.includes("direct"), toggle: () => toggleKind("direct") },
                    { label: t("ruleIncludeGroup"), active: editing.include_kinds.includes("group"), toggle: () => toggleKind("group") },
                    { label: t("ruleOnlyUnread"), active: editing.only_unread, toggle: () => setEditing((p) => ({ ...p, only_unread: !p.only_unread })) },
                    { label: t("ruleExcludeMuted"), active: editing.exclude_muted, toggle: () => setEditing((p) => ({ ...p, exclude_muted: !p.exclude_muted })) },
                  ] as const
                ).map((rule) => (
                  <button
                    key={rule.label}
                    type="button"
                    onClick={rule.toggle}
                    className="flex items-center gap-1.5 rounded-full px-3 py-1.5 font-medium transition"
                    style={{
                      background: rule.active ? "var(--accent-soft)" : "var(--bg-surface)",
                      border: rule.active ? "1px solid var(--accent)" : "1px solid var(--border-subtle)",
                      color: rule.active ? "var(--accent)" : "var(--text-secondary)",
                    }}
                  >
                    {rule.active && <Check className="h-3 w-3" />}
                    <span>{rule.label}</span>
                  </button>
                ))}
              </div>
            </div>

            <div className="flex min-h-0 flex-1 flex-col">
              <div className="mb-1.5 flex flex-wrap items-center justify-between gap-2">
                <div
                  className="flex items-center gap-1 rounded-xl p-0.5"
                  style={{ background: "var(--bg-surface)" }}
                >
                  <button
                    type="button"
                    onClick={() => setThreadListTab("include")}
                    className="rounded-lg px-2.5 py-1 text-xs font-medium transition"
                    style={{
                      background: threadListTab === "include" ? "var(--bg-active)" : "transparent",
                      color:
                        threadListTab === "include"
                          ? "var(--text-on-active)"
                          : "var(--text-secondary)",
                    }}
                  >
                    {t("folderTabInclude", { count: editing.thread_ids.length })}
                  </button>
                  <button
                    type="button"
                    onClick={() => setThreadListTab("exclude")}
                    className="rounded-lg px-2.5 py-1 text-xs font-medium transition"
                    style={{
                      background:
                        threadListTab === "exclude" ? "rgba(244,63,94,0.22)" : "transparent",
                      color: threadListTab === "exclude" ? "#fda4af" : "var(--text-secondary)",
                    }}
                  >
                    {t("folderTabExclude", { count: (editing.excluded_thread_ids ?? []).length })}
                  </button>
                </div>
                <div
                  className="flex w-52 items-center gap-1.5 rounded-lg px-2.5 py-1"
                  style={{ background: "var(--bg-surface)" }}
                >
                  <Search className="h-3.5 w-3.5 shrink-0" style={{ color: "var(--text-secondary)" }} />
                  <input
                    value={threadSearch}
                    onChange={(e) => setThreadSearch(e.target.value)}
                    placeholder={t("filterThreadsPlaceholder")}
                    className="w-full bg-transparent text-xs outline-none"
                  />
                </div>
              </div>

              <div
                className="max-h-56 flex-1 space-y-1 overflow-y-auto rounded-xl p-2"
                style={{ background: "var(--bg-stage)", border: "1px solid var(--border-subtle)" }}
              >
                {filteredThreads.map((thread) => {
                  const excluded = (editing.excluded_thread_ids ?? []).includes(thread.id);
                  const picked =
                    threadListTab === "include" ? editing.thread_ids.includes(thread.id) : excluded;
                  return (
                    <button
                      key={thread.id}
                      type="button"
                      onClick={() =>
                        threadListTab === "include"
                          ? toggleThreadId(thread.id)
                          : toggleExcludedThreadId(thread.id)
                      }
                      className="flex w-full items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-left transition"
                      style={{
                        background: picked
                          ? threadListTab === "exclude"
                            ? "rgba(244,63,94,0.16)"
                            : "var(--accent-soft)"
                          : "transparent",
                      }}
                    >
                      <span
                        className="flex h-4 w-4 shrink-0 items-center justify-center rounded border"
                        style={{
                          borderColor: picked
                            ? threadListTab === "exclude"
                              ? "#fb7185"
                              : "var(--accent)"
                            : "var(--border-subtle)",
                          background: picked
                            ? threadListTab === "exclude"
                              ? "#e11d48"
                              : "var(--accent)"
                            : "transparent",
                          color: "var(--text-on-accent)",
                        }}
                      >
                        {picked &&
                          (threadListTab === "exclude" ? (
                            <Ban className="h-3 w-3" />
                          ) : (
                            <Check className="h-3 w-3" />
                          ))}
                      </span>
                      <ThreadAvatar avatar={thread.avatar} title={thread.title} kind={thread.kind} size="sm" />
                      <div className="min-w-0 flex-1">
                        <div className="truncate text-[13.5px] font-medium">{thread.title}</div>
                      </div>
                      <span className="shrink-0 text-[11px]" style={{ color: "var(--text-secondary)" }}>
                        {thread.kind === "group"
                          ? t("threadKindGroupShort")
                          : thread.kind === "cloud"
                            ? t("folderCloud")
                            : t("threadKindDirectShort")}
                      </span>
                    </button>
                  );
                })}
              </div>
            </div>

            <div className="flex items-center justify-end gap-2 pt-1">
              <button
                type="button"
                onClick={onClose}
                className="hover-menu rounded-xl px-4 py-2 text-[13.5px] font-medium"
                style={{ background: "var(--bg-surface)" }}
              >
                {t("close")}
              </button>
              <button
                type="button"
                disabled={saving}
                onClick={() => void handleSave()}
                className="tf-btn flex items-center gap-1.5 rounded-xl px-4 py-2 text-[13.5px] font-semibold disabled:opacity-50"
              >
                {saving && <Loader2 className="h-4 w-4 animate-spin" />}
                <span>{isExisting ? t("updateFolderBtn") : t("saveNewFolderBtn")}</span>
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
