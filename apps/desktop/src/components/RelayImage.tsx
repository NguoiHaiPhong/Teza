import {
  useEffect,
  useRef,
  type AudioHTMLAttributes,
  type ImgHTMLAttributes,
  type VideoHTMLAttributes,
} from "react";
import {
  isRemoteMediaUrl,
  normalizeMediaRequestUrl,
  useResolvedMediaUrl,
  type TezaMediaKind,
} from "../utils/mediaCache";

function useRelayElement(
  src: string,
  kind: TezaMediaKind,
  forceRelay: boolean,
  threadId?: string,
  msgId?: string,
  onFallbackFail?: () => void,
) {
  const normalizedSrc = normalizeMediaRequestUrl(src);
  const { src: resolvedSrc, failed, requestFallback } = useResolvedMediaUrl(
    normalizedSrc,
    forceRelay,
    { kind, threadId, msgId },
  );
  const onFallbackFailRef = useRef(onFallbackFail);
  onFallbackFailRef.current = onFallbackFail;

  useEffect(() => {
    if (failed) {
      onFallbackFailRef.current?.();
    }
  }, [failed, normalizedSrc]);

  const triggerErrorFallback = () => {
    if (isRemoteMediaUrl(normalizedSrc) && requestFallback()) {
      return true;
    }
    onFallbackFailRef.current?.();
    return false;
  };

  return { resolvedSrc, triggerErrorFallback };
}

interface Props extends Omit<ImgHTMLAttributes<HTMLImageElement>, "src"> {
  src: string;
  forceRelay?: boolean;
  threadId?: string;
  msgId?: string;
  onFallbackFail?: () => void;
}

export function RelayImage({
  src,
  forceRelay = false,
  threadId,
  msgId,
  onFallbackFail,
  onError,
  ...rest
}: Props) {
  const { resolvedSrc, triggerErrorFallback } = useRelayElement(
    src,
    "image",
    forceRelay,
    threadId,
    msgId,
    onFallbackFail,
  );
  if (!resolvedSrc) return null;

  return (
    <img
      {...rest}
      src={resolvedSrc}
      onError={(e) => {
        if (!triggerErrorFallback()) onError?.(e);
      }}
    />
  );
}

interface AudioProps extends Omit<AudioHTMLAttributes<HTMLAudioElement>, "src"> {
  src: string;
  forceRelay?: boolean;
  threadId?: string;
  msgId?: string;
  onFallbackFail?: () => void;
}

export function RelayAudio({
  src,
  forceRelay = false,
  threadId,
  msgId,
  onFallbackFail,
  onError,
  ...rest
}: AudioProps) {
  const { resolvedSrc, triggerErrorFallback } = useRelayElement(
    src,
    "audio",
    forceRelay,
    threadId,
    msgId,
    onFallbackFail,
  );
  if (!resolvedSrc) return null;

  return (
    <audio
      {...rest}
      src={resolvedSrc}
      onError={(e) => {
        if (!triggerErrorFallback()) onError?.(e);
      }}
    />
  );
}

interface VideoProps extends Omit<VideoHTMLAttributes<HTMLVideoElement>, "src"> {
  src: string;
  forceRelay?: boolean;
  threadId?: string;
  msgId?: string;
  onFallbackFail?: () => void;
}

export function RelayVideo({
  src,
  forceRelay = false,
  threadId,
  msgId,
  onFallbackFail,
  onError,
  ...rest
}: VideoProps) {
  const { resolvedSrc, triggerErrorFallback } = useRelayElement(
    src,
    "video",
    forceRelay,
    threadId,
    msgId,
    onFallbackFail,
  );
  if (!resolvedSrc) return null;

  return (
    <video
      {...rest}
      src={resolvedSrc}
      onError={(e) => {
        if (!triggerErrorFallback()) onError?.(e);
      }}
    />
  );
}
