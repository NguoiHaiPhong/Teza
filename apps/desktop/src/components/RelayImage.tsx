import type { ImgHTMLAttributes } from "react";
import { useResolvedMediaUrl } from "../utils/mediaCache";

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
  const { src: resolvedSrc, requestFallback } = useResolvedMediaUrl(src, forceRelay);

  if (!resolvedSrc) {
    return null;
  }

  return (
    <img
      {...rest}
      src={resolvedSrc}
      onError={(e) => {
        if (resolvedSrc === src && /^https:\/\//i.test(src)) {
          requestFallback();
        } else {
          onFallbackFail?.();
          onError?.(e);
        }
      }}
    />
  );
}
