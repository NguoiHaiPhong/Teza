import { useId } from "react";
import { useEmojiStyle } from "../utils/emojiStyleContext";
import { findRetroEmoticonByEmoji, type RetroEmoticonId } from "../utils/retroEmoticons";

interface RetroEmoticonProps {
  id: RetroEmoticonId;
  size?: number;
  animated?: boolean;
  className?: string;
  title?: string;
}

type SphereMood = "gold" | "angry" | "devil" | "sick" | "blush";

function getMood(id: RetroEmoticonId): SphereMood {
  if (id === "angry") return "angry";
  if (id === "devil") return "devil";
  if (id === "sick") return "sick";
  if (id === "blush") return "blush";
  return "gold";
}

function getWrapperAnim(id: RetroEmoticonId, animated: boolean): string {
  if (!animated) return "";
  switch (id) {
    case "rofl":
      return "re-anim-rofl";
    case "laugh":
    case "big_grin":
    case "party":
    case "clap":
      return "re-anim-bounce";
    case "angry":
    case "sweat":
    case "sick":
      return "re-anim-shake";
    case "heart":
    case "love":
    case "kiss":
    case "fire":
    case "star":
      return "re-anim-pulse";
    case "hug":
    case "thumbs_up":
    case "pray":
    case "rose":
      return "re-anim-nod";
    default:
      return "";
  }
}

export function RetroEmoticon({
  id,
  size = 22,
  animated = true,
  className = "",
  title,
}: RetroEmoticonProps) {
  const rawUid = useId();
  const uid = rawUid.replace(/[^a-zA-Z0-9_-]/g, "");
  const gradGold = `re-gold-${uid}`;
  const gradAngry = `re-angry-${uid}`;
  const gradDevil = `re-devil-${uid}`;
  const gradSick = `re-sick-${uid}`;
  const gradGloss = `re-gloss-${uid}`;
  const gradHeart = `re-heart-${uid}`;

  const wrapAnim = getWrapperAnim(id, animated);
  const mood = getMood(id);
  const sphereFill =
    mood === "angry"
      ? `url(#${gradAngry})`
      : mood === "devil"
        ? `url(#${gradDevil})`
        : mood === "sick"
          ? `url(#${gradSick})`
          : `url(#${gradGold})`;
  const sphereStroke =
    mood === "angry"
      ? "#b91c1c"
      : mood === "devil"
        ? "#6b21a8"
        : mood === "sick"
          ? "#3f8f29"
          : "#c77400";

  const renderSphereBase = () => (
    <>
      <circle cx="16" cy="16.5" r="13.2" fill="rgba(0,0,0,0.16)" />
      <circle cx="16" cy="16" r="13.2" fill={sphereFill} stroke={sphereStroke} strokeWidth="1.2" />
      <ellipse cx="13" cy="9.2" rx="7.2" ry="4" fill={`url(#${gradGloss})`} />
    </>
  );

  const renderFace = (): React.ReactNode => {
    switch (id) {
      case "smile":
        return (
          <>
            {renderSphereBase()}
            <ellipse cx="11.5" cy="12.6" rx="1.45" ry="2.2" fill="#2b1900" />
            <ellipse cx="20.5" cy="12.6" rx="1.45" ry="2.2" fill="#2b1900" />
            <circle cx="11" cy="11.7" r="0.5" fill="#fff" />
            <circle cx="20" cy="11.7" r="0.5" fill="#fff" />
            <path
              d="M9.6 17.8 Q16 23.6 22.4 17.8"
              fill="none"
              stroke="#2b1900"
              strokeWidth="1.9"
              strokeLinecap="round"
            />
          </>
        );
      case "big_grin":
        return (
          <>
            {renderSphereBase()}
            <ellipse cx="11.4" cy="11.8" rx="1.5" ry="2.2" fill="#2b1900" />
            <ellipse cx="20.6" cy="11.8" rx="1.5" ry="2.2" fill="#2b1900" />
            <circle cx="10.9" cy="11" r="0.5" fill="#fff" />
            <circle cx="20.1" cy="11" r="0.5" fill="#fff" />
            <path
              d="M8.8 16.5 Q16 17.6 23.2 16.5 Q22.5 24.4 16 24.4 Q9.5 24.4 8.8 16.5 Z"
              fill="#fff"
              stroke="#2b1900"
              strokeWidth="1.35"
              strokeLinejoin="round"
            />
            <path d="M12.2 21.8 Q16 20.3 19.8 21.8 Q18.5 24.2 16 24.2 Q13.5 24.2 12.2 21.8 Z" fill="#ef4444" />
          </>
        );
      case "laugh":
        return (
          <>
            {renderSphereBase()}
            <path d="M9.4 13 Q11.5 10.4 13.6 13" fill="none" stroke="#2b1900" strokeWidth="1.9" strokeLinecap="round" />
            <path d="M18.4 13 Q20.5 10.4 22.6 13" fill="none" stroke="#2b1900" strokeWidth="1.9" strokeLinecap="round" />
            <path
              d="M8.8 16.6 Q16 17.5 23.2 16.6 Q22.4 24.4 16 24.4 Q9.6 24.4 8.8 16.6 Z"
              fill="#b91c1c"
              stroke="#2b1900"
              strokeWidth="1.3"
            />
            <path d="M10.1 17 Q16 17.8 21.9 17 L21.4 19.2 Q16 19.8 10.6 19.2 Z" fill="#fff" />
            <ellipse cx="16" cy="22.4" rx="3.4" ry="1.6" fill="#fb7185" />
          </>
        );
      case "rofl":
        return (
          <>
            {renderSphereBase()}
            <path d="M9 12.2 L13.4 10.6 M9 12.2 L13.4 13.8" fill="none" stroke="#2b1900" strokeWidth="1.75" strokeLinecap="round" />
            <path d="M23 12.2 L18.6 10.6 M23 12.2 L18.6 13.8" fill="none" stroke="#2b1900" strokeWidth="1.75" strokeLinecap="round" />
            <circle cx="8" cy="16.2" r="1.7" fill="#38bdf8" stroke="#0284c7" strokeWidth="0.6" />
            <circle cx="24" cy="16.2" r="1.7" fill="#38bdf8" stroke="#0284c7" strokeWidth="0.6" />
            <path
              d="M8.8 16.8 Q16 17.5 23.2 16.8 Q22.4 24.5 16 24.5 Q9.6 24.5 8.8 16.8 Z"
              fill="#b91c1c"
              stroke="#2b1900"
              strokeWidth="1.3"
            />
            <path d="M10.2 17.1 Q16 17.8 21.8 17.1 L21.3 19.2 Q16 19.7 10.7 19.2 Z" fill="#fff" />
          </>
        );
      case "wink":
        return (
          <>
            {renderSphereBase()}
            <ellipse cx="11.3" cy="12.5" rx="1.45" ry="2.2" fill="#2b1900" />
            <circle cx="10.8" cy="11.6" r="0.5" fill="#fff" />
            <g className={animated ? "re-anim-wink" : ""} style={{ transformOrigin: "20.8px 12.8px" }}>
              <path
                d="M18.2 13.1 Q20.8 10.8 23.4 13.1"
                fill="none"
                stroke="#2b1900"
                strokeWidth="2"
                strokeLinecap="round"
              />
            </g>
            <path
              d="M9.8 17.7 Q16 23.4 22.4 17.4"
              fill="none"
              stroke="#2b1900"
              strokeWidth="1.9"
              strokeLinecap="round"
            />
          </>
        );
      case "blush":
        return (
          <>
            {renderSphereBase()}
            <ellipse cx="9.2" cy="17.2" rx="2.7" ry="1.6" fill="rgba(244,63,94,0.45)" />
            <ellipse cx="22.8" cy="17.2" rx="2.7" ry="1.6" fill="rgba(244,63,94,0.45)" />
            <path d="M9.6 13.4 Q11.5 11.4 13.4 13.4" fill="none" stroke="#2b1900" strokeWidth="1.8" strokeLinecap="round" />
            <path d="M18.6 13.4 Q20.5 11.4 22.4 13.4" fill="none" stroke="#2b1900" strokeWidth="1.8" strokeLinecap="round" />
            <path d="M11.4 19.2 Q16 22.6 20.6 19.2" fill="none" stroke="#2b1900" strokeWidth="1.8" strokeLinecap="round" />
          </>
        );
      case "love":
        return (
          <>
            {renderSphereBase()}
            <path
              d="M11.5 15.2 C11.5 15.2 7.8 12.7 7.8 10.5 C7.8 9.2 8.9 8.4 10 8.4 C10.8 8.4 11.3 8.9 11.5 9.5 C11.7 8.9 12.2 8.4 13 8.4 C14.1 8.4 15.2 9.2 15.2 10.5 C15.2 12.7 11.5 15.2 11.5 15.2 Z"
              fill={`url(#${gradHeart})`}
              stroke="#991b1b"
              strokeWidth="0.7"
            />
            <path
              d="M20.5 15.2 C20.5 15.2 16.8 12.7 16.8 10.5 C16.8 9.2 17.9 8.4 19 8.4 C19.8 8.4 20.3 8.9 20.5 9.5 C20.7 8.9 21.2 8.4 22 8.4 C23.1 8.4 24.2 9.2 24.2 10.5 C24.2 12.7 20.5 15.2 20.5 15.2 Z"
              fill={`url(#${gradHeart})`}
              stroke="#991b1b"
              strokeWidth="0.7"
            />
            <path
              d="M10.4 18.6 Q16 23.6 21.6 18.6"
              fill="none"
              stroke="#2b1900"
              strokeWidth="1.9"
              strokeLinecap="round"
            />
          </>
        );
      case "kiss":
        return (
          <>
            {renderSphereBase()}
            <ellipse cx="11.2" cy="12.5" rx="1.4" ry="2.1" fill="#2b1900" />
            <circle cx="10.7" cy="11.7" r="0.45" fill="#fff" />
            <path d="M18.2 13 Q20.4 11 22.6 13" fill="none" stroke="#2b1900" strokeWidth="1.85" strokeLinecap="round" />
            <path
              d="M15.8 17.4 C17.5 17.4 17.6 19.2 16 19.6 C17.6 20 17.5 21.8 15.6 21.8"
              fill="none"
              stroke="#b91c1c"
              strokeWidth="1.8"
              strokeLinecap="round"
            />
            <path
              d="M25.2 19.6 C25.2 19.6 22.4 17.6 22.4 15.8 C22.4 14.8 23.3 14.2 24.1 14.2 C24.7 14.2 25 14.6 25.2 15.1 C25.4 14.6 25.7 14.2 26.3 14.2 C27.1 14.2 28 14.8 28 15.8 C28 17.6 25.2 19.6 25.2 19.6 Z"
              fill="#f43f5e"
              stroke="#9f1239"
              strokeWidth="0.6"
              className={animated ? "re-anim-float" : ""}
            />
          </>
        );
      case "tongue":
        return (
          <>
            {renderSphereBase()}
            <ellipse cx="11.4" cy="12.2" rx="1.45" ry="2.1" fill="#2b1900" />
            <circle cx="10.9" cy="11.3" r="0.48" fill="#fff" />
            <path d="M18.3 12.8 Q20.5 10.6 22.7 12.8" fill="none" stroke="#2b1900" strokeWidth="1.9" strokeLinecap="round" />
            <path
              d="M9.6 17.4 L22.4 17.4 Q21.5 21.8 16 21.8 Q10.5 21.8 9.6 17.4 Z"
              fill="#7f1d1d"
              stroke="#2b1900"
              strokeWidth="1.35"
            />
            <path
              d="M13 18.8 L19 18.8 L19 22.6 A3 3 0 0 1 13 22.6 Z"
              fill="#fb7185"
              stroke="#9f1239"
              strokeWidth="1"
            />
            <line x1="16" y1="19" x2="16" y2="23.4" stroke="#9f1239" strokeWidth="0.9" strokeLinecap="round" />
          </>
        );
      case "cool":
        return (
          <>
            {renderSphereBase()}
            <rect x="6.6" y="10.2" width="8.6" height="5.5" rx="2" fill="#111827" stroke="#000" strokeWidth="0.9" />
            <rect x="16.8" y="10.2" width="8.6" height="5.5" rx="2" fill="#111827" stroke="#000" strokeWidth="0.9" />
            <line x1="15.2" y1="12" x2="16.8" y2="12" stroke="#000" strokeWidth="1.5" />
            <line x1="5" y1="11.5" x2="6.8" y2="11.8" stroke="#000" strokeWidth="1.4" />
            <line x1="27" y1="11.5" x2="25.2" y2="11.8" stroke="#000" strokeWidth="1.4" />
            <line x1="8.2" y1="11.3" x2="10.8" y2="14.2" stroke="rgba(255,255,255,0.55)" strokeWidth="1.1" strokeLinecap="round" />
            <line x1="18.4" y1="11.3" x2="21" y2="14.2" stroke="rgba(255,255,255,0.55)" strokeWidth="1.1" strokeLinecap="round" />
            <path d="M11 20 Q16.8 23.2 22 18.2" fill="none" stroke="#2b1900" strokeWidth="1.9" strokeLinecap="round" />
          </>
        );
      case "angel":
        return (
          <>
            {renderSphereBase()}
            <ellipse
              cx="16"
              cy="4.4"
              rx="8.4"
              ry="2.2"
              fill="none"
              stroke="#38bdf8"
              strokeWidth="2.1"
              className={animated ? "re-anim-float" : ""}
            />
            <ellipse cx="11.6" cy="13.4" rx="1.4" ry="2" fill="#2b1900" />
            <ellipse cx="20.4" cy="13.4" rx="1.4" ry="2" fill="#2b1900" />
            <path d="M10.6 18.8 Q16 22.8 21.4 18.8" fill="none" stroke="#2b1900" strokeWidth="1.8" strokeLinecap="round" />
          </>
        );
      case "devil":
        return (
          <>
            <path d="M6.5 9.2 L4.4 2.8 L11 6.2 Z" fill="#9333ea" stroke="#581c87" strokeWidth="1" strokeLinejoin="round" />
            <path d="M25.5 9.2 L27.6 2.8 L21 6.2 Z" fill="#9333ea" stroke="#581c87" strokeWidth="1" strokeLinejoin="round" />
            {renderSphereBase()}
            <line x1="8.6" y1="10.2" x2="13.6" y2="12.3" stroke="#1f0937" strokeWidth="1.8" strokeLinecap="round" />
            <line x1="23.4" y1="10.2" x2="18.4" y2="12.3" stroke="#1f0937" strokeWidth="1.8" strokeLinecap="round" />
            <circle cx="11.6" cy="13.4" r="1.45" fill="#fef08a" stroke="#1f0937" strokeWidth="0.9" />
            <circle cx="20.4" cy="13.4" r="1.45" fill="#fef08a" stroke="#1f0937" strokeWidth="0.9" />
            <path d="M9.6 18.2 Q16 24.2 22.4 18.2" fill="none" stroke="#1f0937" strokeWidth="1.9" strokeLinecap="round" />
          </>
        );
      case "sad":
        return (
          <>
            {renderSphereBase()}
            <path d="M9 11 Q11.3 9.4 13.5 10.6" fill="none" stroke="#2b1900" strokeWidth="1.4" strokeLinecap="round" />
            <path d="M23 11 Q20.7 9.4 18.5 10.6" fill="none" stroke="#2b1900" strokeWidth="1.4" strokeLinecap="round" />
            <ellipse cx="11.4" cy="13.5" rx="1.4" ry="2" fill="#2b1900" />
            <ellipse cx="20.6" cy="13.5" rx="1.4" ry="2" fill="#2b1900" />
            <path
              d="M9.5 16 Q8 18.6 9.5 19.6 A1.6 1.6 0 0 0 11 18 Z"
              fill="#38bdf8"
              className={animated ? "re-anim-tear" : ""}
            />
            <path d="M11 22.2 Q16 18 21 22.2" fill="none" stroke="#2b1900" strokeWidth="1.8" strokeLinecap="round" />
          </>
        );
      case "cry":
        return (
          <>
            {renderSphereBase()}
            <rect x="9.5" y="13.5" width="3.2" height="10" rx="1.3" fill="#38bdf8" opacity="0.85" className={animated ? "re-anim-tear" : ""} />
            <rect x="19.3" y="13.5" width="3.2" height="10" rx="1.3" fill="#38bdf8" opacity="0.85" className={animated ? "re-anim-tear" : ""} />
            <path d="M9.2 12.6 Q11.4 14.4 13.6 12.6" fill="none" stroke="#2b1900" strokeWidth="1.8" strokeLinecap="round" />
            <path d="M18.4 12.6 Q20.6 14.4 22.8 12.6" fill="none" stroke="#2b1900" strokeWidth="1.8" strokeLinecap="round" />
            <ellipse cx="16" cy="21" rx="4" ry="2.8" fill="#7f1d1d" stroke="#2b1900" strokeWidth="1.2" />
          </>
        );
      case "angry":
        return (
          <>
            {renderSphereBase()}
            <line x1="8.4" y1="10" x2="14" y2="12.6" stroke="#2b0707" strokeWidth="2.1" strokeLinecap="round" />
            <line x1="23.6" y1="10" x2="18" y2="12.6" stroke="#2b0707" strokeWidth="2.1" strokeLinecap="round" />
            <circle cx="11.6" cy="13.8" r="1.5" fill="#2b0707" />
            <circle cx="20.4" cy="13.8" r="1.5" fill="#2b0707" />
            <path d="M10.4 22 Q16 17.5 21.6 22" fill="none" stroke="#2b0707" strokeWidth="2" strokeLinecap="round" />
          </>
        );
      case "surprised":
        return (
          <>
            {renderSphereBase()}
            <path d="M9.6 9.8 Q11.5 8.2 13.4 9.8" fill="none" stroke="#2b1900" strokeWidth="1.35" strokeLinecap="round" />
            <path d="M18.6 9.8 Q20.5 8.2 22.4 9.8" fill="none" stroke="#2b1900" strokeWidth="1.35" strokeLinecap="round" />
            <ellipse cx="11.5" cy="13.2" rx="1.5" ry="2.3" fill="#2b1900" />
            <ellipse cx="20.5" cy="13.2" rx="1.5" ry="2.3" fill="#2b1900" />
            <ellipse cx="16" cy="20.8" rx="3" ry="3.8" fill="#7f1d1d" stroke="#2b1900" strokeWidth="1.4" />
          </>
        );
      case "confused":
        return (
          <>
            {renderSphereBase()}
            <circle cx="11.4" cy="13.2" r="1.6" fill="#2b1900" />
            <circle cx="20.2" cy="13.8" r="1.35" fill="#2b1900" />
            <path d="M9.4 10 Q11.4 8.6 13.4 10" fill="none" stroke="#2b1900" strokeWidth="1.4" strokeLinecap="round" />
            <path d="M18.6 11.4 L22.2 11.4" fill="none" stroke="#2b1900" strokeWidth="1.4" strokeLinecap="round" />
            <path d="M10.8 20.6 Q13.4 18.8 16 20.5 Q18.6 22.2 21.2 19.6" fill="none" stroke="#2b1900" strokeWidth="1.8" strokeLinecap="round" />
            <text x="24.2" y="10.2" fontSize="9.5" fontWeight="900" fill="#dc2626">
              ?
            </text>
          </>
        );
      case "sweat":
        return (
          <>
            {renderSphereBase()}
            <ellipse cx="11.2" cy="12.8" rx="1.4" ry="2.1" fill="#2b1900" />
            <ellipse cx="19.6" cy="12.8" rx="1.4" ry="2.1" fill="#2b1900" />
            <path d="M10 18.2 Q16 23 21.8 18" fill="none" stroke="#2b1900" strokeWidth="1.8" strokeLinecap="round" />
            <path
              d="M25 6.4 C25 6.4 27.6 10.2 27.6 12 A2.6 2.6 0 0 1 22.4 12 C22.4 10.2 25 6.4 25 6.4 Z"
              fill="#38bdf8"
              stroke="#0369a1"
              strokeWidth="0.9"
            />
          </>
        );
      case "sleepy":
        return (
          <>
            {renderSphereBase()}
            <path d="M9.4 13.8 Q11.4 15.5 13.4 13.8" fill="none" stroke="#2b1900" strokeWidth="1.8" strokeLinecap="round" />
            <path d="M18.6 13.8 Q20.6 15.5 22.6 13.8" fill="none" stroke="#2b1900" strokeWidth="1.8" strokeLinecap="round" />
            <circle cx="16" cy="20.4" r="2.2" fill="#7f1d1d" stroke="#2b1900" strokeWidth="1.2" />
            <text
              x="21.5"
              y="9.5"
              fontSize="7.5"
              fontWeight="800"
              fill="#2563eb"
              className={animated ? "re-anim-float" : ""}
            >
              Z
            </text>
            <text x="25.5" y="6.2" fontSize="5.8" fontWeight="800" fill="#60a5fa">
              z
            </text>
          </>
        );
      case "sick":
        return (
          <>
            {renderSphereBase()}
            <path d="M9.4 12.6 Q11.4 10.6 13.4 12.6" fill="none" stroke="#14532d" strokeWidth="1.7" strokeLinecap="round" />
            <path d="M18.6 12.6 Q20.6 10.6 22.6 12.6" fill="none" stroke="#14532d" strokeWidth="1.7" strokeLinecap="round" />
            <path d="M10 19.6 Q13 17.8 16 19.8 Q19 21.8 22 19.6" fill="none" stroke="#14532d" strokeWidth="1.8" strokeLinecap="round" />
            <ellipse cx="9.2" cy="16.8" rx="1.8" ry="1.2" fill="rgba(22,163,74,0.45)" />
            <ellipse cx="22.8" cy="16.8" rx="1.8" ry="1.2" fill="rgba(22,163,74,0.45)" />
          </>
        );
      case "shh":
        return (
          <>
            {renderSphereBase()}
            <ellipse cx="11.5" cy="12.2" rx="1.4" ry="2.1" fill="#2b1900" />
            <ellipse cx="20.5" cy="12.2" rx="1.4" ry="2.1" fill="#2b1900" />
            <circle cx="16" cy="18.6" r="1.7" fill="#7f1d1d" />
            <rect x="14.3" y="17.8" width="3.4" height="9.2" rx="1.7" fill="#fde047" stroke="#b45309" strokeWidth="1.1" />
          </>
        );
      case "smug":
        return (
          <>
            {renderSphereBase()}
            <path d="M9.2 12.6 L13.8 12.2" fill="none" stroke="#2b1900" strokeWidth="1.6" strokeLinecap="round" />
            <path d="M18.2 12.2 L22.8 12.6" fill="none" stroke="#2b1900" strokeWidth="1.6" strokeLinecap="round" />
            <circle cx="12.4" cy="13.8" r="1.4" fill="#2b1900" />
            <circle cx="21.4" cy="13.8" r="1.4" fill="#2b1900" />
            <path d="M11 20.6 Q17.6 22 22.4 17.8" fill="none" stroke="#2b1900" strokeWidth="1.9" strokeLinecap="round" />
          </>
        );
      case "eyebrow":
        return (
          <>
            {renderSphereBase()}
            <path d="M8.8 10.6 Q11.4 8.2 14 10.2" fill="none" stroke="#2b1900" strokeWidth="1.7" strokeLinecap="round" />
            <line x1="18.2" y1="11.4" x2="23.2" y2="11.4" stroke="#2b1900" strokeWidth="1.7" strokeLinecap="round" />
            <ellipse cx="11.4" cy="13.6" rx="1.45" ry="2" fill="#2b1900" />
            <circle cx="20.6" cy="14" r="1.4" fill="#2b1900" />
            <line x1="11.4" y1="20.4" x2="20.8" y2="19.4" stroke="#2b1900" strokeWidth="1.9" strokeLinecap="round" />
          </>
        );
      case "nerd":
        return (
          <>
            {renderSphereBase()}
            <circle cx="11.2" cy="13" r="4.1" fill="rgba(255,255,255,0.22)" stroke="#1e293b" strokeWidth="1.5" />
            <circle cx="20.8" cy="13" r="4.1" fill="rgba(255,255,255,0.22)" stroke="#1e293b" strokeWidth="1.5" />
            <line x1="15.3" y1="12.8" x2="16.7" y2="12.8" stroke="#1e293b" strokeWidth="1.5" />
            <circle cx="11.2" cy="13" r="1.35" fill="#1e293b" />
            <circle cx="20.8" cy="13" r="1.35" fill="#1e293b" />
            <path d="M10.2 18.8 Q16 22.8 21.8 18.8" fill="none" stroke="#2b1900" strokeWidth="1.8" strokeLinecap="round" />
            <rect x="13.8" y="19.8" width="2" height="2.6" rx="0.4" fill="#fff" stroke="#2b1900" strokeWidth="0.8" />
            <rect x="16.2" y="19.8" width="2" height="2.6" rx="0.4" fill="#fff" stroke="#2b1900" strokeWidth="0.8" />
          </>
        );
      case "hug":
        return (
          <>
            {renderSphereBase()}
            <ellipse cx="11.4" cy="12" rx="1.4" ry="2" fill="#2b1900" />
            <ellipse cx="20.6" cy="12" rx="1.4" ry="2" fill="#2b1900" />
            <path d="M10.4 16.8 Q16 21.4 21.6 16.8" fill="none" stroke="#2b1900" strokeWidth="1.8" strokeLinecap="round" />
            <path
              d="M5.2 21.2 Q8.5 18.5 12.2 21.5 Q9.2 25.4 5.2 24 Z"
              fill="#fde047"
              stroke="#b45309"
              strokeWidth="1.1"
            />
            <path
              d="M26.8 21.2 Q23.5 18.5 19.8 21.5 Q22.8 25.4 26.8 24 Z"
              fill="#fde047"
              stroke="#b45309"
              strokeWidth="1.1"
            />
          </>
        );
      case "party":
        return (
          <>
            {renderSphereBase()}
            <polygon points="10,7.2 16,1.2 22,7.2" fill="#ec4899" stroke="#9d174d" strokeWidth="1" strokeLinejoin="round" />
            <circle cx="16" cy="1.5" r="1.4" fill="#38bdf8" />
            <ellipse cx="11.5" cy="13" rx="1.4" ry="2" fill="#2b1900" />
            <ellipse cx="20.5" cy="13" rx="1.4" ry="2" fill="#2b1900" />
            <path
              d="M9.6 17.5 Q16 23.2 22.4 17.5"
              fill="none"
              stroke="#2b1900"
              strokeWidth="1.9"
              strokeLinecap="round"
            />
            <circle cx="5" cy="10" r="1.1" fill="#38bdf8" />
            <circle cx="27" cy="9.5" r="1.1" fill="#22c55e" />
          </>
        );
      case "clap":
        return (
          <>
            <path d="M7.5 19 L12.5 9.5 Q14.2 7.5 15.8 9.2 L12.2 22 Z" fill="#fde047" stroke="#c77400" strokeWidth="1.2" strokeLinejoin="round" />
            <path d="M24.5 19 L19.5 9.5 Q17.8 7.5 16.2 9.2 L19.8 22 Z" fill="#facc15" stroke="#c77400" strokeWidth="1.2" strokeLinejoin="round" />
            <path d="M8.5 22.5 Q16 27.5 23.5 22.5 L21.2 26.5 Q16 28.5 10.8 26.5 Z" fill="#fde047" stroke="#c77400" strokeWidth="1.2" />
            <line x1="16" y1="3.2" x2="16" y2="6.2" stroke="#f59e0b" strokeWidth="1.6" strokeLinecap="round" />
            <line x1="11.2" y1="4.8" x2="13" y2="7.2" stroke="#f59e0b" strokeWidth="1.5" strokeLinecap="round" />
            <line x1="20.8" y1="4.8" x2="19" y2="7.2" stroke="#f59e0b" strokeWidth="1.5" strokeLinecap="round" />
          </>
        );
      case "thumbs_up":
        return (
          <>
            <rect x="4" y="13.2" width="5.5" height="12.6" rx="1.6" fill="#2563eb" stroke="#1d4ed8" strokeWidth="1" />
            <path
              d="M9.8 13.8 L14.5 5.8 Q16.2 3.8 18 5.2 Q19.2 6.5 17.8 10.8 L24 10.8 Q26.4 10.8 26.4 13.2 Q26.4 14.5 25.4 15.2 Q26.2 16.4 25.2 17.8 Q25.8 19.2 24.4 20.5 Q24.8 22.2 22.8 23.6 L11.5 23.6 L9.8 22 Z"
              fill={`url(#${gradGold})`}
              stroke="#c77400"
              strokeWidth="1.2"
              strokeLinejoin="round"
            />
          </>
        );
      case "thumbs_down":
        return (
          <>
            <rect x="4" y="6.2" width="5.5" height="12.6" rx="1.6" fill="#2563eb" stroke="#1d4ed8" strokeWidth="1" />
            <path
              d="M9.8 18.2 L14.5 26.2 Q16.2 28.2 18 26.8 Q19.2 25.5 17.8 21.2 L24 21.2 Q26.4 21.2 26.4 18.8 Q26.4 17.5 25.4 16.8 Q26.2 15.6 25.2 14.2 Q25.8 12.8 24.4 11.5 Q24.8 9.8 22.8 8.4 L11.5 8.4 L9.8 10 Z"
              fill={`url(#${gradGold})`}
              stroke="#c77400"
              strokeWidth="1.2"
              strokeLinejoin="round"
            />
          </>
        );
      case "heart":
        return (
          <>
            <path
              d="M16 28 C16 28 3.8 20.2 3.8 11.4 C3.8 6.8 7.5 3.6 11.6 3.6 C14 3.6 15.5 4.8 16 6.6 C16.5 4.8 18 3.6 20.4 3.6 C24.5 3.6 28.2 6.8 28.2 11.4 C28.2 20.2 16 28 16 28 Z"
              fill={`url(#${gradHeart})`}
              stroke="#9f1239"
              strokeWidth="1.3"
              strokeLinejoin="round"
            />
            <ellipse cx="10.2" cy="9.2" rx="3" ry="1.8" transform="rotate(-24 10.2 9.2)" fill="rgba(255,255,255,0.55)" />
          </>
        );
      case "broken_heart":
        return (
          <>
            <path
              d="M14.5 27.6 C14.5 27.6 3.6 20 3.6 11.4 C3.6 6.8 7.2 3.8 11.2 3.8 C13.5 3.8 14.8 5 15.3 6.8 L13.4 12.2 L16.2 15.8 L13.8 21 Z"
              fill={`url(#${gradHeart})`}
              stroke="#9f1239"
              strokeWidth="1.2"
              strokeLinejoin="round"
            />
            <path
              d="M17.5 27.6 C17.5 27.6 28.4 20 28.4 11.4 C28.4 6.8 24.8 3.8 20.8 3.8 C18.5 3.8 17.2 5 16.7 6.8 L15.2 11.8 L18.2 15.6 L15.8 21.4 Z"
              fill={`url(#${gradHeart})`}
              stroke="#9f1239"
              strokeWidth="1.2"
              strokeLinejoin="round"
            />
          </>
        );
      case "rose":
        return (
          <>
            <path d="M16 14 L16 29" stroke="#15803d" strokeWidth="2.4" strokeLinecap="round" />
            <path d="M16 22 Q10 20 8.5 16.5 Q13.5 16.5 16 20 Z" fill="#22c55e" stroke="#15803d" strokeWidth="1" />
            <path d="M16 24.5 Q22 22.5 23.5 19 Q18.5 19 16 22.5 Z" fill="#16a34a" stroke="#15803d" strokeWidth="1" />
            <path
              d="M16 16.2 C10.4 16.2 9.4 10.6 10.8 6.8 C12.4 7.8 14.2 5.4 16 4.4 C17.8 5.4 19.6 7.8 21.2 6.8 C22.6 10.6 21.6 16.2 16 16.2 Z"
              fill={`url(#${gradHeart})`}
              stroke="#9f1239"
              strokeWidth="1.2"
            />
            <path d="M13.2 8.2 Q16 11.2 18.8 8.2" fill="none" stroke="#fda4af" strokeWidth="1.1" strokeLinecap="round" />
          </>
        );
      case "coffee":
        return (
          <>
            <path d="M11 8.5 Q9.5 6 11.5 3.8" fill="none" stroke="#cbd5e1" strokeWidth="1.6" strokeLinecap="round" className={animated ? "re-anim-float" : ""} />
            <path d="M16.5 8.5 Q15 6 17 3.8" fill="none" stroke="#cbd5e1" strokeWidth="1.6" strokeLinecap="round" className={animated ? "re-anim-float" : ""} />
            <path d="M22.5 14 H25.2 A2.6 3.2 0 0 1 25.2 21 H22.5" fill="none" stroke="#94a3b8" strokeWidth="2" />
            <path d="M6.5 12.2 H22.5 L20.6 24.4 Q20 26.2 18 26.2 H11 Q9 26.2 8.4 24.4 Z" fill="#f8fafc" stroke="#64748b" strokeWidth="1.3" strokeLinejoin="round" />
            <ellipse cx="14.5" cy="12.5" rx="7.2" ry="2" fill="#78350f" />
            <ellipse cx="14.5" cy="27.2" rx="10" ry="2" fill="#cbd5e1" stroke="#64748b" strokeWidth="1" />
          </>
        );
      case "star":
        return (
          <>
            <polygon
              points="16,2.8 19.8,10.8 28.6,12 22.1,18.1 23.8,26.8 16,22.5 8.2,26.8 9.9,18.1 3.4,12 12.2,10.8"
              fill={`url(#${gradGold})`}
              stroke="#c77400"
              strokeWidth="1.3"
              strokeLinejoin="round"
            />
            <ellipse cx="13.5" cy="11.2" rx="3" ry="1.8" transform="rotate(-25 13.5 11.2)" fill="rgba(255,255,255,0.6)" />
          </>
        );
      case "fire":
        return (
          <>
            <path
              d="M16.4 3 C18.5 8.5 24.8 11.8 24.8 18.8 C24.8 24.4 20.8 28.4 16 28.4 C10.4 28.4 7.2 24.2 7.2 19 C7.2 14.5 9.8 11.2 11.8 9 C11.5 11.8 13 13 13.8 13.2 C13.2 8.8 14.8 5.2 16.4 3 Z"
              fill="#f97316"
              stroke="#c2410c"
              strokeWidth="1.2"
              strokeLinejoin="round"
            />
            <path
              d="M16 14.2 C18.5 17.5 20.5 19.8 20.5 22.8 C20.5 25.5 18.4 27.6 16 27.6 C13.4 27.6 11.5 25.4 11.5 22.6 C11.5 19.5 14 16.8 16 14.2 Z"
              fill="#fde047"
            />
          </>
        );
      case "pray":
        return (
          <>
            <path
              d="M15.6 5.2 L10.2 18.8 Q9.2 21.8 12.2 23.6 L15.6 25.5 Z"
              fill={`url(#${gradGold})`}
              stroke="#c77400"
              strokeWidth="1.2"
              strokeLinejoin="round"
            />
            <path
              d="M16.4 5.2 L21.8 18.8 Q22.8 21.8 19.8 23.6 L16.4 25.5 Z"
              fill="#f59e0b"
              stroke="#c77400"
              strokeWidth="1.2"
              strokeLinejoin="round"
            />
            <path d="M7.5 22 L12.5 24.5 L11 28 L5.5 25.5 Z" fill="#38bdf8" stroke="#0284c7" strokeWidth="1" />
            <path d="M24.5 22 L19.5 24.5 L21 28 L26.5 25.5 Z" fill="#38bdf8" stroke="#0284c7" strokeWidth="1" />
          </>
        );
    }
  };

  return (
    <span
      className={`re-emoticon ${wrapAnim} ${className}`}
      style={{ width: `${size}px`, height: `${size}px` }}
      title={title}
      role="img"
      aria-label={title || id}
    >
      <svg
        width={size}
        height={size}
        viewBox="0 0 32 32"
        xmlns="http://www.w3.org/2000/svg"
        className="overflow-visible"
      >
        <defs>
          <radialGradient id={gradGold} cx="35%" cy="30%" r="72%">
            <stop offset="0%" stopColor="#fff7b2" />
            <stop offset="48%" stopColor="#ffd43b" />
            <stop offset="88%" stopColor="#f59f00" />
            <stop offset="100%" stopColor="#e67700" />
          </radialGradient>
          <radialGradient id={gradAngry} cx="35%" cy="30%" r="72%">
            <stop offset="0%" stopColor="#ffb399" />
            <stop offset="52%" stopColor="#ff6b4a" />
            <stop offset="100%" stopColor="#c92a2a" />
          </radialGradient>
          <radialGradient id={gradDevil} cx="35%" cy="30%" r="72%">
            <stop offset="0%" stopColor="#f3d9fa" />
            <stop offset="55%" stopColor="#ae3ec9" />
            <stop offset="100%" stopColor="#6b21a8" />
          </radialGradient>
          <radialGradient id={gradSick} cx="35%" cy="30%" r="72%">
            <stop offset="0%" stopColor="#d8f5a2" />
            <stop offset="55%" stopColor="#94d82d" />
            <stop offset="100%" stopColor="#4c9a2a" />
          </radialGradient>
          <linearGradient id={gradGloss} x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stopColor="#ffffff" stopOpacity="0.82" />
            <stop offset="100%" stopColor="#ffffff" stopOpacity="0" />
          </linearGradient>
          <radialGradient id={gradHeart} cx="35%" cy="28%" r="75%">
            <stop offset="0%" stopColor="#ff8787" />
            <stop offset="55%" stopColor="#fa5252" />
            <stop offset="100%" stopColor="#c92a2a" />
          </radialGradient>
        </defs>
        {renderFace()}
      </svg>
    </span>
  );
}

export function EmojiGlyph({
  emoji,
  size = 22,
  animated = true,
  className = "",
  title,
}: {
  emoji: string;
  size?: number;
  animated?: boolean;
  className?: string;
  title?: string;
}) {
  const { emojiStyle } = useEmojiStyle();
  const retro = emojiStyle === "retro" ? findRetroEmoticonByEmoji(emoji) : undefined;
  if (retro) {
    return (
      <RetroEmoticon
        id={retro.id}
        size={size}
        animated={animated}
        className={className}
        title={title ?? `${retro.label} (${retro.primaryShortcut})`}
      />
    );
  }
  return (
    <span
      className={className}
      style={{ fontSize: `${Math.round(size * 0.86)}px`, lineHeight: 1 }}
      title={title}
    >
      {emoji}
    </span>
  );
}
