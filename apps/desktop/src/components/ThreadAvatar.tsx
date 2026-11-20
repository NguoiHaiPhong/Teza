import { useEffect, useState } from "react";
import { Bookmark, Users } from "lucide-react";
import type { ThreadKind } from "../types";
import { useResolvedMediaUrl } from "../utils/mediaCache";

interface Props {
  avatar: string;
  title: string;
  kind: ThreadKind;
  size?: "xs" | "sm" | "md" | "lg" | "xl";
}

interface MultiSlot {
  name: string;
  url: string;
}

const PALETTE = ["#3b82f6", "#8b5cf6", "#10b981", "#f59e0b", "#ec4899", "#06b6d4"];

function pickColor(seed: string): string {
  let hash = 0;
  for (let i = 0; i < seed.length; i += 1) {
    hash = (hash * 31 + seed.charCodeAt(i)) | 0;
  }
  return PALETTE[Math.abs(hash) % PALETTE.length];
}

function parseMultiAvatar(raw: string): MultiSlot[] | null {
  if (!raw.startsWith("multi:")) return null;
  const parts = raw.split(";").slice(1);
  const slots = parts
    .map((p) => {
      const idx = p.indexOf("|");
      if (idx < 0) return { name: p.trim(), url: "" };
      return {
        name: p.slice(0, idx).trim(),
        url: p.slice(idx + 1).trim(),
      };
    })
    .filter((s) => s.name || s.url)
    .slice(0, 4);
  return slots.length > 0 ? slots : null;
}

function SingleImg({
  src,
  alt,
  className,
  fallback,
}: {
  src: string;
  alt: string;
  className: string;
  fallback: React.ReactNode;
}) {
  const { src: resolvedSrc, requestFallback } = useResolvedMediaUrl(src);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    setFailed(false);
  }, [src]);

  if (!src || failed || !resolvedSrc) {
    return <>{fallback}</>;
  }

  return (
    <img
      src={resolvedSrc}
      alt={alt}
      loading="lazy"
      decoding="async"
      referrerPolicy="no-referrer"
      onError={() => {
        if (resolvedSrc === src && /^https:\/\//i.test(src)) {
          requestFallback();
        } else {
          setFailed(true);
        }
      }}
      className={className}
    />
  );
}

export function ThreadAvatar({ avatar, title, kind, size = "md" }: Props) {
  const boxCls =
    size === "xl"
      ? "h-20 w-20 text-2xl"
      : size === "lg"
        ? "h-16 w-16 text-xl"
        : size === "sm"
          ? "h-9 w-9 text-xs"
          : size === "xs"
            ? "h-7 w-7 text-[11px]"
            : "h-11 w-11 text-sm";

  const multiSlots = parseMultiAvatar(avatar);
  if (multiSlots) {
    return (
      <div
        className={`${boxCls} grid shrink-0 grid-cols-2 grid-rows-2 gap-0.5 overflow-hidden rounded-full p-0.5`}
        style={{ background: "var(--bg-surface)" }}
      >
        {multiSlots.map((slot, idx) => (
          <div key={`${slot.name}-${idx}`} className="min-h-0 min-w-0 h-full w-full overflow-hidden rounded-full">
            <SingleImg
              src={slot.url}
              alt={slot.name || title}
              className="h-full w-full object-cover"
              fallback={
                <div
                  className="flex h-full w-full items-center justify-center rounded-full text-[9px] font-semibold text-white"
                  style={{ background: pickColor(slot.name || `${title}-${idx}`) }}
                >
                  {(slot.name || "?").slice(0, 1).toUpperCase()}
                </div>
              }
            />
          </div>
        ))}
      </div>
    );
  }

  const fallback = (
    <div
      className={`${boxCls} flex shrink-0 items-center justify-center rounded-full font-semibold text-white`}
      style={{
        background:
          kind === "cloud" ? "var(--accent)" : pickColor(title || "L"),
      }}
    >
      {kind === "cloud" ? (
        <Bookmark className={size === "lg" ? "h-7 w-7" : "h-5 w-5"} />
      ) : kind === "group" && (!title || title.startsWith("Nhóm ")) ? (
        <Users className={size === "lg" ? "h-7 w-7" : "h-5 w-5"} />
      ) : (
        (title || "?").slice(0, 1).toUpperCase()
      )}
    </div>
  );

  if (!avatar) {
    return fallback;
  }

  return (
    <SingleImg
      src={avatar}
      alt={title}
      className={`${boxCls} shrink-0 rounded-full object-cover`}
      fallback={fallback}
    />
  );
}
