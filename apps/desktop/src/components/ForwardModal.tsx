import { useEffect, useMemo, useRef, useState } from "react";
import {
  FileText,
  Forward,
  Image as ImageIcon,
  Loader2,
  Mic,
  Search,
  Smile,
  Video,
  X,
} from "lucide-react";
import { useI18n, type TranslationKey, type UiLanguage } from "../i18n";
import type { MessageDto, ThreadDto } from "../types";
import { isExternalLinkMessageType } from "../utils/mediaCache";
import { CALL_PREFIX, POLL_PREFIX, REMINDER_PREFIX } from "../utils/richMessage";
import { filterThreadsBySearchQuery, isImeComposingEvent } from "../utils/searchNormalize";
import { getDisplayBodyText } from "../utils/threadState";
import { ThreadAvatar } from "./ThreadAvatar";

interface Props {
  messages: MessageDto[];
  threads: ThreadDto[];
  isLocked?: boolean;
  onClose: () => void;
  onForwardTo: (targetThreadId: string, messages: MessageDto[]) => Promise<void>;
}

type ForwardMediaKind = "image" | "gif" | "video" | "voice" | "file" | "sticker" | "text";

interface ForwardMessagePreview {
  kind: ForwardMediaKind;
  label: string;
  detail: string;
}

function stripMediaPlaceholder(text: string): string {
  const trimmed = text.trim();
  if (
    trimmed === "[Hình ảnh]" ||
    trimmed === "[GIF]" ||
    trimmed === "[Video]" ||
    trimmed === "[Tin nhắn thoại]" ||
    trimmed === "[Tập tin]" ||
    trimmed === "[Nhãn dán]"
  ) {
    return "";
  }
  return trimmed;
}

function describeForwardMessagePreview(
  msg: MessageDto,
  lang: UiLanguage,
  t: (key: TranslationKey, vars?: Record<string, string | number>) => string,
): ForwardMessagePreview {
  const mt = (msg.msg_type || "").trim().toLowerCase();
  const url = (msg.attachment_url || "").trim();
  const rawAttName = (msg.attachment_name || "").trim();
  const isInternalRichMeta =
    rawAttName.startsWith(POLL_PREFIX) ||
    rawAttName.startsWith(REMINDER_PREFIX) ||
    rawAttName.startsWith(CALL_PREFIX);
  const attName = isInternalRichMeta ? "" : stripMediaPlaceholder(rawAttName);
  const bodyCaption = stripMediaPlaceholder(getDisplayBodyText(msg));

  const isSticker = mt.includes("sticker") || mt === "5" || mt === "35" || mt === "36";
  if (isSticker) {
    return {
      kind: "sticker",
      label: t("stickerAlt"),
      detail: bodyCaption,
    };
  }

  const isGif = mt.includes("gif") || /\.gif(?:$|[?#])/i.test(url) || /\.gif$/i.test(attName);
  if (isGif) {
    return {
      kind: "gif",
      label: "GIF",
      detail: bodyCaption || attName,
    };
  }

  const isPhoto =
    mt.includes("photo") || mt === "2" || mt.includes("image") || /^data:image\//i.test(url);
  if (isPhoto) {
    return {
      kind: "image",
      label: t("photoAlt"),
      detail: bodyCaption || attName,
    };
  }

  const isVideo =
    mt.includes("video") || mt === "4" || mt === "44" || /^data:video\//i.test(url);
  if (isVideo) {
    const detail =
      attName && bodyCaption && attName.toLowerCase() !== bodyCaption.toLowerCase()
        ? `${attName} — ${bodyCaption}`
        : bodyCaption || attName;
    return {
      kind: "video",
      label: lang === "ko" ? "동영상" : "Video",
      detail,
    };
  }

  const isVoice =
    mt.includes("voice") || mt === "3" || mt === "31" || /^data:audio\//i.test(url);
  if (isVoice) {
    return {
      kind: "voice",
      label: lang === "en" ? "Voice message" : lang === "ko" ? "음성 메시지" : "Tin nhắn thoại",
      detail: bodyCaption || attName,
    };
  }

  const isExplicitText = mt === "chat.text" || mt === "text" || mt === "webchat";
  const isRichCard =
    isInternalRichMeta || mt.includes("poll") || mt.includes("reminder") || mt.includes("call");
  const isFile =
    mt.includes("file") ||
    mt === "19" ||
    mt === "share.file" ||
    (!isExplicitText && !isRichCard && !isExternalLinkMessageType(mt) && Boolean(url || attName));
  if (isFile) {
    const detail =
      attName && bodyCaption && attName.toLowerCase() !== bodyCaption.toLowerCase()
        ? `${attName} — ${bodyCaption}`
        : attName || bodyCaption;
    return {
      kind: "file",
      label: lang === "en" ? "File" : lang === "ko" ? "파일" : "Tập tin",
      detail,
    };
  }

  return {
    kind: "text",
    label: "",
    detail: bodyCaption || attName || (msg.text || "").trim() || t("msgFallbackLabel"),
  };
}

function renderForwardKindIcon(kind: ForwardMediaKind) {
  const cls = "h-3.5 w-3.5 shrink-0";
  const style = { color: "var(--accent)" };
  switch (kind) {
    case "image":
    case "gif":
      return <ImageIcon className={cls} style={style} />;
    case "video":
      return <Video className={cls} style={style} />;
    case "voice":
      return <Mic className={cls} style={style} />;
    case "file":
      return <FileText className={cls} style={style} />;
    case "sticker":
      return <Smile className={cls} style={style} />;
    default:
      return null;
  }
}

export function ForwardModal({ messages, threads, isLocked = false, onClose, onForwardTo }: Props) {
  const { t, lang } = useI18n();
  const [query, setQuery] = useState("");
  const [sendingId, setSendingId] = useState<string | null>(null);
  const sendingRef = useRef(false);

  const active = messages.length > 0;
  const busy = sendingId !== null;

  useEffect(() => {
    if (!active) {
      setQuery("");
      setSendingId(null);
      sendingRef.current = false;
      return;
    }
    if (isLocked) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (isImeComposingEvent(e) || e.defaultPrevented) return;
      if (e.key === "Escape" && !sendingRef.current) {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [active, isLocked, onClose]);

  const filtered = useMemo(
    () => filterThreadsBySearchQuery(threads, query, 40),
    [threads, query],
  );

  if (!active) return null;

  const first = messages[0];
  const preview = describeForwardMessagePreview(first, lang, t);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4 backdrop-blur-xs"
      onClick={() => {
        if (!busy) onClose();
      }}
    >
      <div
        className="flex max-h-[80vh] w-full max-w-md flex-col overflow-hidden rounded-2xl shadow-2xl"
        style={{ background: "var(--bg-sidebar)", border: "1px solid var(--border-subtle)" }}
        onClick={(e) => e.stopPropagation()}
      >
        <div
          className="flex items-center justify-between border-b px-4 py-3"
          style={{ borderColor: "var(--border-subtle)" }}
        >
          <div className="flex items-center gap-2 text-[15px] font-semibold">
            <Forward className="h-4 w-4" style={{ color: "var(--accent)" }} />
            <span>
              {messages.length > 1
                ? t("forwardManyTitle", { count: messages.length })
                : t("forwardOneTitle")}
            </span>
          </div>
          <button
            type="button"
            disabled={busy}
            onClick={() => {
              if (!busy) onClose();
            }}
            className="hover-menu rounded-lg p-1 disabled:cursor-not-allowed disabled:opacity-40"
          >
            <X className="h-4 w-4" style={{ color: "var(--text-secondary)" }} />
          </button>
        </div>

        <div className="px-4 pt-3">
          <div
            className="mb-3 rounded-xl px-3 py-2 text-[13px]"
            style={{ background: "var(--quote-bg)", color: "var(--text-secondary)" }}
          >
            <div className="mb-0.5 font-semibold" style={{ color: "var(--text-primary)" }}>
              {messages.length > 1
                ? t("forwardSelectedPrefix", { count: messages.length })
                : t("forwardContentPrefix")}
            </div>
            <div className="flex items-start gap-1.5">
              {preview.kind !== "text" && (
                <span className="mt-0.5 inline-flex shrink-0 items-center">
                  {renderForwardKindIcon(preview.kind)}
                </span>
              )}
              <span className="line-clamp-2 min-w-0 break-words">
                {preview.label && (
                  <span className="font-medium" style={{ color: "var(--text-primary)" }}>
                    {preview.label}
                  </span>
                )}
                {preview.label && preview.detail ? " · " : ""}
                {preview.detail}
              </span>
            </div>
          </div>

          <div
            className="flex items-center gap-2 rounded-xl px-3 py-2"
            style={{ background: "var(--bg-surface)" }}
          >
            <Search className="h-4 w-4 shrink-0" style={{ color: "var(--text-secondary)" }} />
            <input
              autoFocus
              disabled={busy}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={t("forwardDestPlaceholder")}
              className="w-full bg-transparent text-[15px] outline-none disabled:cursor-not-allowed disabled:opacity-60"
            />
          </div>
        </div>

        <div className="mt-2 flex-1 overflow-y-auto px-2 pb-3" aria-busy={busy}>
          {filtered.length === 0 ? (
            <div className="py-8 text-center text-[13px]" style={{ color: "var(--text-secondary)" }}>
              {t("emptySearch")}
            </div>
          ) : (
            filtered.map((tItem) => {
              const isSendingThis = sendingId === tItem.id;
              return (
                <button
                  key={tItem.id}
                  type="button"
                  disabled={busy}
                  onClick={() => {
                    if (sendingRef.current || busy) return;
                    sendingRef.current = true;
                    setSendingId(tItem.id);
                    void onForwardTo(tItem.id, messages).finally(() => {
                      sendingRef.current = false;
                      setSendingId(null);
                      onClose();
                    });
                  }}
                  className={`hover-surface flex w-full items-center justify-between gap-3 rounded-xl px-3 py-2 text-left transition disabled:cursor-not-allowed ${
                    isSendingThis ? "opacity-100" : "disabled:opacity-45"
                  }`}
                >
                  <div className="flex min-w-0 items-center gap-2.5">
                    <ThreadAvatar avatar={tItem.avatar} title={tItem.title} kind={tItem.kind} size="sm" />
                    <div className="min-w-0">
                      <div className="truncate text-[15px] font-medium">{tItem.title}</div>
                      <div className="text-xs" style={{ color: "var(--text-secondary)" }}>
                        {tItem.kind === "group"
                          ? t("threadKindGroupShort")
                          : tItem.kind === "cloud"
                            ? t("folderCloud")
                            : t("threadKindDirectShort")}
                      </div>
                    </div>
                  </div>
                  <span className="tf-btn inline-flex shrink-0 items-center gap-1.5 rounded-lg px-2.5 py-1 text-[13px] font-semibold">
                    {isSendingThis && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
                    <span>{isSendingThis ? t("sendingStatus") : t("sendBtn")}</span>
                  </span>
                </button>
              );
            })
          )}
        </div>
      </div>
    </div>
  );
}
