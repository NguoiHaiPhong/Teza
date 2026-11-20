import type { MessageDto, ThreadDto } from "../types.ts";

export function dayKey(ts: number): string {
  const d = new Date(ts > 1e12 ? ts : ts * 1000);
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
}

export function formatDayLabel(ts: number): string {
  const d = new Date(ts > 1e12 ? ts : ts * 1000);
  const today = new Date();
  const yest = new Date(Date.now() - 86_400_000);
  if (dayKey(d.getTime()) === dayKey(today.getTime())) return "Hôm nay";
  if (dayKey(d.getTime()) === dayKey(yest.getTime())) return "Hôm qua";
  return d.toLocaleDateString("vi-VN");
}

export function formatThreadTime(ts: number): string {
  if (!ts) return "";
  const ms = ts > 1e12 ? ts : ts * 1000;
  const d = new Date(ms);
  const now = new Date();
  if (dayKey(ms) === dayKey(now.getTime())) {
    return d.toLocaleTimeString("vi-VN", { hour: "2-digit", minute: "2-digit" });
  }
  const yest = new Date(Date.now() - 86_400_000);
  if (dayKey(ms) === dayKey(yest.getTime())) {
    return "Hôm qua";
  }
  return d.toLocaleDateString("vi-VN", { day: "2-digit", month: "2-digit" });
}

export function upsertMsg(list: MessageDto[], incoming: MessageDto): MessageDto[] {
  const idx = list.findIndex((m) => m.msg_id === incoming.msg_id);
  if (idx >= 0) {
    const next = [...list];
    next[idx] = incoming;
    return next;
  }
  return [...list, incoming].sort((a, b) => a.ts - b.ts);
}

export function compareThreads(a: ThreadDto, b: ThreadDto): number {
  const aUnread = a.unread_count > 0 ? 1 : 0;
  const bUnread = b.unread_count > 0 ? 1 : 0;
  if (bUnread !== aUnread) return bUnread - aUnread;
  if (b.last_ts !== a.last_ts) return b.last_ts - a.last_ts;
  return (b.is_pinned ? 1 : 0) - (a.is_pinned ? 1 : 0);
}

export function bumpThreadList(list: ThreadDto[], msg: MessageDto, incUnread: boolean): ThreadDto[] {
  const rawPreview = (
    msg.text || (msg.attachment_name ? `[Tệp: ${msg.attachment_name}]` : `[${msg.msg_type}]`)
  )
    .replace(/\s+/g, " ")
    .trim();
  const idx = list.findIndex((t) => t.id === msg.thread_id);
  if (idx < 0) return list;
  const target = list[idx];
  let preview = rawPreview;
  if (rawPreview) {
    if (msg.is_outgoing) {
      preview = rawPreview.startsWith("Bạn:") ? rawPreview : `Bạn: ${rawPreview}`;
    } else if (
      target.kind === "group" &&
      msg.sender_name &&
      msg.sender_name !== "Bạn" &&
      !/^\d{6,}$/.test(msg.sender_name.trim())
    ) {
      preview = `${msg.sender_name}: ${rawPreview}`;
    }
  }
  const next = [...list];
  next[idx] = {
    ...target,
    last_preview: msg.ts >= target.last_ts ? preview : target.last_preview,
    last_ts: Math.max(target.last_ts, msg.ts),
    unread_count: msg.is_outgoing
      ? 0
      : incUnread
        ? target.unread_count + 1
        : target.unread_count,
  };
  return next.sort(compareThreads);
}
