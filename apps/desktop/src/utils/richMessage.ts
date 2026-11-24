import type { CallCardPayload, MessageDto, PollDto, ReminderPayload } from "../types.ts";

export const REMINDER_PREFIX = "teza-reminder:";
export const POLL_PREFIX = "teza-poll:";
export const CALL_PREFIX = "teza-call:";

export function isReminderThumbUrl(url?: string | null): boolean {
  if (!url) return false;
  return /\/reminder[_-]/i.test(url);
}

export function parseVietnameseScheduleTime(text: string, fallbackTsMs: number): number {
  const cleaned = (text || "").trim();
  if (!cleaned) return 0;
  const fallbackDate = new Date(
    fallbackTsMs > 1e12 ? fallbackTsMs : fallbackTsMs > 0 ? fallbackTsMs * 1000 : Date.now(),
  );
  const fallbackYear = fallbackDate.getFullYear();

  const buildLocalTs = (year: number, month: number, day: number, hour: number, minute: number) => {
    if (month < 1 || month > 12 || day < 1 || day > 31 || hour > 23 || minute > 59) return 0;
    const d = new Date(year, month - 1, day, hour, minute, 0, 0);
    return Number.isFinite(d.getTime()) ? d.getTime() : 0;
  };

  // E.g. "Thứ Bảy, 21 tháng 11 lúc 15:00" or "21 tháng 11 năm 2026 15:00"
  const viLong = cleaned.match(
    /(\d{1,2})\s+tháng\s+(\d{1,2})(?:[,\s/.-]*(?:năm\s*)?(\d{4}))?(?:[^\d]+|\s+lúc\s+)(\d{1,2}):(\d{2})/i,
  );
  if (viLong) {
    const day = Number.parseInt(viLong[1], 10);
    const month = Number.parseInt(viLong[2], 10);
    const year = viLong[3] ? Number.parseInt(viLong[3], 10) : fallbackYear;
    const hour = Number.parseInt(viLong[4], 10);
    const minute = Number.parseInt(viLong[5], 10);
    return buildLocalTs(year, month, day, hour, minute);
  }

  // E.g. "21/11/2026 15:00" or "15:00 21/11/2026"
  const dmyTime = cleaned.match(/(\d{1,2})\/(\d{1,2})(?:\/(\d{4}))?\s+(\d{1,2}):(\d{2})/);
  if (dmyTime) {
    const day = Number.parseInt(dmyTime[1], 10);
    const month = Number.parseInt(dmyTime[2], 10);
    const year = dmyTime[3] ? Number.parseInt(dmyTime[3], 10) : fallbackYear;
    const hour = Number.parseInt(dmyTime[4], 10);
    const minute = Number.parseInt(dmyTime[5], 10);
    return buildLocalTs(year, month, day, hour, minute);
  }

  const timeDmy = cleaned.match(/(\d{1,2}):(\d{2})\s+(\d{1,2})\/(\d{1,2})(?:\/(\d{4}))?/);
  if (timeDmy) {
    const hour = Number.parseInt(timeDmy[1], 10);
    const minute = Number.parseInt(timeDmy[2], 10);
    const day = Number.parseInt(timeDmy[3], 10);
    const month = Number.parseInt(timeDmy[4], 10);
    const year = timeDmy[5] ? Number.parseInt(timeDmy[5], 10) : fallbackYear;
    return buildLocalTs(year, month, day, hour, minute);
  }

  return 0;
}

export function isReminderMessage(
  msg: Pick<MessageDto, "msg_type" | "attachment_name" | "attachment_url" | "text">,
): boolean {
  const mt = (msg.msg_type || "").toLowerCase();
  if (mt.includes("reminder")) return true;
  if (msg.attachment_name?.startsWith(REMINDER_PREFIX)) return true;
  if (isReminderThumbUrl(msg.attachment_url)) return true;
  if ((msg.attachment_name || "").trim().startsWith("⏰")) return true;
  const trimmed = (msg.text || "").trim();
  return trimmed.startsWith("[Nhắc hẹn]") || trimmed.startsWith("Nhắc hẹn:");
}

export function isPollMessage(msg: Pick<MessageDto, "msg_type" | "attachment_name" | "text">): boolean {
  const mt = (msg.msg_type || "").toLowerCase();
  if (mt.includes("poll")) return true;
  if (msg.attachment_name?.startsWith(POLL_PREFIX)) return true;
  const trimmed = (msg.text || "").trim();
  return trimmed.startsWith("[Bình chọn]") || trimmed.startsWith("Bình chọn:");
}

export function isCallMessage(msg: Pick<MessageDto, "msg_type" | "attachment_name">): boolean {
  const mt = (msg.msg_type || "").trim().toLowerCase();
  if (
    mt === "chat.call" ||
    mt === "chat.voip" ||
    mt === "call" ||
    mt === "voip" ||
    mt.startsWith("call.")
  ) {
    return true;
  }
  return Boolean(msg.attachment_name?.startsWith(CALL_PREFIX));
}

export function parseCallPayload(
  msg: Pick<MessageDto, "msg_type" | "attachment_name" | "text" | "is_outgoing">,
): CallCardPayload | null {
  if (!isCallMessage(msg)) return null;
  if (msg.attachment_name?.startsWith(CALL_PREFIX)) {
    try {
      const raw = JSON.parse(msg.attachment_name.slice(CALL_PREFIX.length)) as Partial<CallCardPayload>;
      const callType = raw.callType === "video" ? "video" : "voice";
      const durationSecs =
        typeof raw.durationSecs === "number" && Number.isFinite(raw.durationSecs) && raw.durationSecs > 0
          ? Math.floor(raw.durationSecs)
          : 0;
      const validOutcome =
        raw.outcome === "completed" ||
        raw.outcome === "missed" ||
        raw.outcome === "cancelled" ||
        raw.outcome === "declined"
          ? raw.outcome
          : durationSecs > 0
            ? "completed"
            : msg.is_outgoing
              ? "cancelled"
              : "missed";
      return {
        callType,
        outcome: validOutcome,
        durationSecs,
      };
    } catch {
      // fall through to text/type fallback
    }
  }

  const mt = (msg.msg_type || "").toLowerCase();
  const txt = (msg.text || "").trim();
  const lowTxt = txt.toLowerCase();
  const callType: "voice" | "video" =
    mt.includes("video") || lowTxt.includes("video") ? "video" : "voice";
  const mmss = txt.match(/(\d+):(\d{2})/);
  const durationSecs = mmss
    ? Number.parseInt(mmss[1], 10) * 60 + Number.parseInt(mmss[2], 10)
    : 0;

  let outcome: CallCardPayload["outcome"];
  if (durationSecs > 0) {
    outcome = "completed";
  } else if (/nhỡ|bỏ lỡ|miss|no_answer|không trả lời/i.test(lowTxt)) {
    outcome = "missed";
  } else if (/hủy|cancel/i.test(lowTxt)) {
    outcome = "cancelled";
  } else if (/từ chối|bận|reject|declin|busy/i.test(lowTxt)) {
    outcome = "declined";
  } else {
    outcome = msg.is_outgoing ? "cancelled" : "missed";
  }

  return {
    callType,
    outcome,
    durationSecs,
  };
}

export function parseReminderPayload(
  msg: Pick<MessageDto, "msg_type" | "attachment_name" | "attachment_url" | "text" | "ts">,
): ReminderPayload | null {
  if (!isReminderMessage(msg)) return null;
  if (msg.attachment_name?.startsWith(REMINDER_PREFIX)) {
    try {
      const raw = JSON.parse(msg.attachment_name.slice(REMINDER_PREFIX.length)) as Partial<ReminderPayload>;
      const cleanTitle = (raw.title || "").replace(/^⏰\s*/, "").trim() || "Nhắc hẹn";
      const scheduleText =
        typeof raw.scheduleText === "string" && raw.scheduleText.trim()
          ? raw.scheduleText.trim()
          : undefined;
      let startTime = typeof raw.startTime === "number" && raw.startTime > 0 ? raw.startTime : 0;
      if (startTime <= 0 && scheduleText) {
        startTime = parseVietnameseScheduleTime(scheduleText, msg.ts);
      }
      if (startTime <= 0 && !scheduleText) {
        startTime = msg.ts;
      }
      const payload: ReminderPayload = {
        title: cleanTitle,
        startTime,
        repeat: typeof raw.repeat === "number" ? raw.repeat : 0,
        reminderId: typeof raw.reminderId === "string" ? raw.reminderId : "",
      };
      if (scheduleText) {
        payload.scheduleText = scheduleText;
      }
      return payload;
    } catch {
      // fall through to text/legacy summary
    }
  }
  const rawName = (msg.attachment_name || "").replace(/^⏰\s*/, "").trim();
  const rawText = (msg.text || "")
    .replace(/^\[Nhắc hẹn\]\s*/i, "")
    .replace(/^Nhắc hẹn:\s*/i, "")
    .replace(/^⏰\s*/, "")
    .trim();
  if (rawName) {
    const parsedTime = parseVietnameseScheduleTime(rawText, msg.ts);
    const payload: ReminderPayload = {
      title: rawName,
      startTime: parsedTime > 0 ? parsedTime : msg.ts,
      repeat: 0,
      reminderId: "",
    };
    if (rawText && rawText !== rawName) {
      payload.scheduleText = rawText;
    }
    return payload;
  }
  return {
    title: rawText || "Nhắc hẹn",
    startTime: msg.ts,
    repeat: 0,
    reminderId: "",
  };
}

export function parsePollPayload(
  msg: Pick<MessageDto, "msg_id" | "msg_type" | "attachment_name" | "text">,
): PollDto | null {
  if (!isPollMessage(msg)) return null;
  if (msg.attachment_name?.startsWith(POLL_PREFIX)) {
    try {
      const raw = JSON.parse(msg.attachment_name.slice(POLL_PREFIX.length)) as Partial<PollDto>;
      return {
        poll_id: typeof raw.poll_id === "number" ? raw.poll_id : 0,
        question: (raw.question || "").trim() || "Bình chọn",
        options: Array.isArray(raw.options) ? raw.options : [],
        allow_multi: Boolean(raw.allow_multi),
        allow_add_option: Boolean(raw.allow_add_option),
        is_anonymous: Boolean(raw.is_anonymous),
        expired_time: typeof raw.expired_time === "number" ? raw.expired_time : 0,
        closed: Boolean(raw.closed),
      };
    } catch {
      // fall through
    }
  }
  const cleanQuestion = (msg.text || "")
    .replace(/^\[Bình chọn\]\s*/i, "")
    .replace(/^Bình chọn:\s*/i, "")
    .trim();
  const parsedId = Number.parseInt(msg.msg_id.replace(/\D+/g, ""), 10);
  return {
    poll_id: Number.isFinite(parsedId) ? parsedId : 0,
    question: cleanQuestion || "Bình chọn",
    options: [],
    allow_multi: false,
    allow_add_option: false,
    is_anonymous: false,
    expired_time: 0,
    closed: false,
  };
}

export function formatReminderCountdown(
  startTimeMs: number,
  nowMs: number = Date.now(),
  lang: "vi" | "en" | "ko" = "vi",
): string {
  if (!startTimeMs || startTimeMs <= 0) return "";
  const diff = startTimeMs - nowMs;
  if (diff <= -60_000) {
    return lang === "en" ? "Passed" : lang === "ko" ? "지남" : "Đã qua";
  }
  if (diff < 60_000) {
    return lang === "en" ? "Starting soon" : lang === "ko" ? "곧 시작" : "Sắp diễn ra";
  }
  const totalMins = Math.round(diff / 60_000);
  const days = Math.floor(totalMins / (24 * 60));
  const hours = Math.floor((totalMins % (24 * 60)) / 60);
  const mins = totalMins % 60;
  if (days > 0) {
    if (lang === "en") return `In ${days}d${hours > 0 ? ` ${hours}h` : ""}`;
    if (lang === "ko") return `${days}일${hours > 0 ? ` ${hours}시간` : ""} 후`;
    return `Còn ${days} ngày${hours > 0 ? ` ${hours} giờ` : ""}`;
  }
  if (hours > 0) {
    if (lang === "en") return `In ${hours}h${mins > 0 ? ` ${mins}m` : ""}`;
    if (lang === "ko") return `${hours}시간${mins > 0 ? ` ${mins}분` : ""} 후`;
    return `Còn ${hours} giờ${mins > 0 ? ` ${mins} phút` : ""}`;
  }
  if (lang === "en") return `In ${mins}m`;
  if (lang === "ko") return `${mins}분 후`;
  return `Còn ${mins} phút`;
}
