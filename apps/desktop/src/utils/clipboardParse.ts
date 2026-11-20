export interface ClipboardFileEntryLike {
  name: string;
  type: string;
}

export interface ClipboardItemLike<T extends ClipboardFileEntryLike = ClipboardFileEntryLike> {
  kind: string;
  getAsFile: () => T | null;
}

export interface ClipboardDataLike<T extends ClipboardFileEntryLike = ClipboardFileEntryLike> {
  items?: ArrayLike<ClipboardItemLike<T>> | null;
  files?: ArrayLike<T> | null;
  getData: (format: string) => string;
}

export function extFromMime(mime: string): string {
  switch (mime.toLowerCase()) {
    case "image/jpeg":
    case "image/jpg":
      return "jpg";
    case "image/webp":
      return "webp";
    case "image/gif":
      return "gif";
    case "image/bmp":
      return "bmp";
    default:
      return "png";
  }
}

export function extractAttachmentFromClipboard<T extends ClipboardFileEntryLike>(
  cd: ClipboardDataLike<T> | null | undefined,
): T | null {
  if (!cd) return null;
  const fromItems: T[] = [];
  if (cd.items) {
    for (const item of Array.from(cd.items)) {
      if (item.kind === "file") {
        const f = item.getAsFile();
        if (f) fromItems.push(f);
      }
    }
  }
  const candidates = fromItems.length > 0 ? fromItems : Array.from(cd.files ?? []);
  if (candidates.length === 0) return null;

  const plainText = cd.getData("text/plain");
  const uriList = cd.getData("text/uri-list");
  // Word/Excel/browsers often place both text AND an auto-rendered snapshot image on the clipboard;
  // prefer pasting text unless the clipboard actually holds non-image files or no text.
  if (
    plainText.trim().length > 0 &&
    !uriList &&
    candidates.every(
      (f) =>
        f.type.startsWith("image/") &&
        (!f.name || /^image\.(png|jpe?g|bmp|webp|gif)$/i.test(f.name)),
    )
  ) {
    return null;
  }
  return candidates[0];
}
