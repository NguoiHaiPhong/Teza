import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { loadEmojiStyle, saveEmojiStyle, type EmojiStyle } from "./retroEmoticons";

interface EmojiStyleContextValue {
  emojiStyle: EmojiStyle;
  setEmojiStyle: (style: EmojiStyle) => void;
}

const EmojiStyleContext = createContext<EmojiStyleContextValue | null>(null);

export function EmojiStyleProvider({ children }: { children: ReactNode }) {
  const [emojiStyle, setEmojiStyleState] = useState<EmojiStyle>(() => loadEmojiStyle());

  const setEmojiStyle = useCallback((next: EmojiStyle) => {
    saveEmojiStyle(next);
    setEmojiStyleState(next);
  }, []);

  useEffect(() => {
    const onStorage = (ev: StorageEvent) => {
      if (ev.key === "teza.emojiStyle") {
        setEmojiStyleState(ev.newValue === "system" ? "system" : "retro");
      }
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, []);

  const value = useMemo(() => ({ emojiStyle, setEmojiStyle }), [emojiStyle, setEmojiStyle]);

  return <EmojiStyleContext.Provider value={value}>{children}</EmojiStyleContext.Provider>;
}

export function useEmojiStyle(): EmojiStyleContextValue {
  const ctx = useContext(EmojiStyleContext);
  if (!ctx) {
    return {
      emojiStyle: "retro",
      setEmojiStyle: saveEmojiStyle,
    };
  }
  return ctx;
}
