import {
  useEffect,
  useRef,
  type AudioHTMLAttributes,
  type ImgHTMLAttributes,
  type VideoHTMLAttributes,
} from "react";
import { useResolvedMediaUrl } from "../utils/mediaCache";

function useRelayElement(
  src: string,
  forceRelay: boolean,
  onFallbackFail?: () => void,
) {
  const { src: resolvedSrc, failed, requestFallback } = useResolvedMediaUrl(src, forceRelay);
  const onFallbackFailRef = useRef(onFallbackFail);
  onFallbackFailRef.current = onFallbackFail;

  useEffect(() => {
    if (failed) {
      onFallbackFailRef.current?.();
    }
  }, [failed, src]);

  const triggerErrorFallback = () => {
    if (resolvedSrc === src && /^https:\/\//i.test(src)) {
      requestFallback();
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
  onFallbackFail?: () => void;
}

export function RelayImage({
  src,
  forceRelay = false,
  onFallbackFail,
  onError,
  ...rest
}: Props) {
  const { resolvedSrc, triggerErrorFallback } = useRelayElement(src, forceRelay, onFallbackFail);
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
  onFallbackFail?: () => void;
}

export function RelayAudio({
  src,
  forceRelay = false,
  onFallbackFail,
  onError,
  ...rest
}: AudioProps) {
  const { resolvedSrc, triggerErrorFallback } = useRelayElement(src, forceRelay, onFallbackFail);
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
  onFallbackFail?: () => void;
}

export function RelayVideo({
  src,
  forceRelay = false,
  onFallbackFail,
  onError,
  ...rest
}: VideoProps) {
  const { resolvedSrc, triggerErrorFallback } = useRelayElement(src, forceRelay, onFallbackFail);
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
