/**
 * WebCodecs H.264 media utilities for desktop 1-1 video calls:
 * - Chunk-safe base64 encoding/decoding
 * - H.264 Annex-B / AVCC normalization & SPS codec extraction
 * - RemoteH264Renderer (VideoDecoder -> HTMLCanvasElement)
 * - LocalCameraEncoder (getUserMedia -> VideoEncoder -> Annex-B base64 AUs)
 */

export type LocalCameraStatus =
  | "idle"
  | "starting"
  | "active"
  | "off"
  | "unavailable"
  | "denied"
  | "unsupported"
  | "error";

export interface VideoCallMediaCapabilities {
  hasVideoDecoder: boolean;
  hasVideoEncoder: boolean;
  hasGetUserMedia: boolean;
}

export interface CameraDeviceInfo {
  deviceId: string;
  label: string;
}

export type AvcChunkFormat = "annexb" | "avc";

export interface RemoteH264RendererOptions {
  onDimensions?: (width: number, height: number) => void;
  onError?: (err: string) => void;
  onFirstFrame?: () => void;
  onKeyframeNeeded?: (reason: "overflow" | "decode_error") => void;
}

export interface RemoteH264FrameInput {
  timestamp: number;
  is_keyframe: boolean;
  just_synced?: boolean;
  data_b64: string;
}

export interface LocalCameraEncoderOptions {
  targetWidth?: number;
  targetHeight?: number;
  targetFps?: number;
  bitrateBps?: number;
  onEncodedAu: (dataB64: string, isKeyFrame: boolean) => void;
  onStatusChange?: (status: LocalCameraStatus, detail?: string) => void;
  onStreamChange?: (stream: MediaStream | null) => void;
}

const DEFAULT_DECODER_CODEC = "avc1.42E01E";
const ENCODER_CODEC_CANDIDATES = ["avc1.42E01E", "avc1.42001E"] as const;
const MAX_DECODE_QUEUE_SIZE = 6;
const MAX_ENCODE_QUEUE_SIZE = 3;
const KEYFRAME_INTERVAL_FRAMES = 30;
const BASE64_CHUNK_SIZE = 0x2000;
const VIDEO_CLOCK_HZ = 90_000;
const METADATA_WAIT_TIMEOUT_MS = 1000;

interface ParsedAvcDescription {
  isValidAvcC: boolean;
  nalLengthSize: number;
  sps: Uint8Array[];
  pps: Uint8Array[];
}

interface OffscreenVideoLike {
  muted?: boolean;
  playsInline?: boolean;
  autoplay?: boolean;
  srcObject: MediaProvider | null;
  readonly readyState: number;
  readonly videoWidth?: number;
  readonly videoHeight?: number;
  play?: () => Promise<void> | void;
  pause?: () => void;
  addEventListener?: (type: string, listener: () => void) => void;
  removeEventListener?: (type: string, listener: () => void) => void;
}

function formatErrorMessage(err: unknown): string {
  if (err instanceof Error) {
    return err.message || err.name || "Unknown error";
  }
  if (typeof err === "string") {
    return err;
  }
  if (err && typeof err === "object" && "message" in err) {
    const msg = (err as { message?: unknown }).message;
    if (typeof msg === "string" && msg.length > 0) {
      return msg;
    }
  }
  return String(err);
}

function getErrorName(err: unknown): string {
  if (err instanceof Error) {
    return err.name;
  }
  if (err && typeof err === "object" && "name" in err) {
    const name = (err as { name?: unknown }).name;
    if (typeof name === "string") {
      return name;
    }
  }
  return "";
}

function toUint8Array(
  source: AllowSharedBufferSource | null | undefined,
): Uint8Array | null {
  if (!source) return null;
  if (source instanceof Uint8Array) {
    return source;
  }
  if (ArrayBuffer.isView(source)) {
    return new Uint8Array(source.buffer, source.byteOffset, source.byteLength);
  }
  return new Uint8Array(source);
}

function hexByte(value: number): string {
  return (value & 0xff).toString(16).toUpperCase().padStart(2, "0");
}

function startsWithAnnexBStartCode(bytes: Uint8Array): boolean {
  if (
    bytes.byteLength >= 4 &&
    bytes[0] === 0 &&
    bytes[1] === 0 &&
    bytes[2] === 0 &&
    bytes[3] === 1
  ) {
    return true;
  }
  return (
    bytes.byteLength >= 3 &&
    bytes[0] === 0 &&
    bytes[1] === 0 &&
    bytes[2] === 1
  );
}

function splitAnnexBNals(bytes: Uint8Array): Uint8Array[] {
  const nals: Uint8Array[] = [];
  const len = bytes.byteLength;
  let i = 0;
  let nalStart = -1;

  while (i + 2 < len) {
    let scLen = 0;
    if (
      i + 3 < len &&
      bytes[i] === 0 &&
      bytes[i + 1] === 0 &&
      bytes[i + 2] === 0 &&
      bytes[i + 3] === 1
    ) {
      scLen = 4;
    } else if (bytes[i] === 0 && bytes[i + 1] === 0 && bytes[i + 2] === 1) {
      scLen = 3;
    }

    if (scLen > 0) {
      if (nalStart >= 0 && i > nalStart) {
        nals.push(bytes.subarray(nalStart, i));
      }
      nalStart = i + scLen;
      i += scLen;
    } else {
      i += 1;
    }
  }

  if (nalStart >= 0 && nalStart < len) {
    nals.push(bytes.subarray(nalStart, len));
  }

  return nals;
}

function joinNalsAs4ByteAnnexB(nals: Uint8Array[]): Uint8Array {
  let total = 0;
  for (let i = 0; i < nals.length; i += 1) {
    if (nals[i].byteLength > 0) {
      total += 4 + nals[i].byteLength;
    }
  }
  if (total === 0) {
    return new Uint8Array(0);
  }
  const out = new Uint8Array(total);
  let offset = 0;
  for (let i = 0; i < nals.length; i += 1) {
    const nal = nals[i];
    if (nal.byteLength === 0) continue;
    out[offset] = 0;
    out[offset + 1] = 0;
    out[offset + 2] = 0;
    out[offset + 3] = 1;
    out.set(nal, offset + 4);
    offset += 4 + nal.byteLength;
  }
  return out;
}

function parseAvcDescription(
  avcDescription: AllowSharedBufferSource | null | undefined,
): ParsedAvcDescription {
  const result: ParsedAvcDescription = {
    isValidAvcC: false,
    nalLengthSize: 4,
    sps: [],
    pps: [],
  };
  const desc = toUint8Array(avcDescription);
  if (!desc || desc.byteLength === 0) {
    return result;
  }

  // Defensive fallback: if description itself is Annex-B, extract SPS/PPS directly.
  if (startsWithAnnexBStartCode(desc)) {
    const nals = splitAnnexBNals(desc);
    for (const nal of nals) {
      if (nal.byteLength === 0) continue;
      const nalType = nal[0] & 0x1f;
      if (nalType === 7) result.sps.push(nal);
      else if (nalType === 8) result.pps.push(nal);
    }
    return result;
  }

  // ISO/IEC 14496-15 AVCDecoderConfigurationRecord (avcC)
  if (desc.byteLength < 7 || desc[0] !== 1) {
    return result;
  }

  result.isValidAvcC = true;
  result.nalLengthSize = (desc[4] & 0x03) + 1;
  const numSps = desc[5] & 0x1f;
  let offset = 6;

  for (let i = 0; i < numSps; i += 1) {
    if (offset + 2 > desc.byteLength) return result;
    const spsLen = (desc[offset] << 8) | desc[offset + 1];
    offset += 2;
    if (offset + spsLen > desc.byteLength) return result;
    if (spsLen > 0) {
      result.sps.push(desc.subarray(offset, offset + spsLen));
    }
    offset += spsLen;
  }

  if (offset >= desc.byteLength) {
    return result;
  }

  const numPps = desc[offset];
  offset += 1;
  for (let i = 0; i < numPps; i += 1) {
    if (offset + 2 > desc.byteLength) return result;
    const ppsLen = (desc[offset] << 8) | desc[offset + 1];
    offset += 2;
    if (offset + ppsLen > desc.byteLength) return result;
    if (ppsLen > 0) {
      result.pps.push(desc.subarray(offset, offset + ppsLen));
    }
    offset += ppsLen;
  }

  return result;
}

function splitAvccNalsDetailed(
  bytes: Uint8Array,
  nalLengthSize: number,
): { nals: Uint8Array[]; exact: boolean } {
  const size = Math.max(1, Math.min(4, nalLengthSize));
  const nals: Uint8Array[] = [];
  let offset = 0;
  let truncated = false;

  while (offset + size <= bytes.byteLength) {
    let nalLen = 0;
    for (let k = 0; k < size; k += 1) {
      nalLen = nalLen * 256 + bytes[offset + k];
    }
    offset += size;
    if (nalLen === 0) {
      continue;
    }
    if (offset + nalLen > bytes.byteLength) {
      truncated = true;
      break;
    }
    nals.push(bytes.subarray(offset, offset + nalLen));
    offset += nalLen;
  }

  const exact = !truncated && offset === bytes.byteLength && nals.length > 0;
  return { nals, exact };
}

function prependMissingSpsPps(
  nals: Uint8Array[],
  parsedDesc: ParsedAvcDescription,
): Uint8Array[] {
  if (parsedDesc.sps.length === 0 && parsedDesc.pps.length === 0) {
    return nals;
  }
  let hasSps = false;
  let hasPps = false;
  for (let i = 0; i < nals.length; i += 1) {
    const nal = nals[i];
    if (nal.byteLength === 0) continue;
    const nalType = nal[0] & 0x1f;
    if (nalType === 7) hasSps = true;
    else if (nalType === 8) hasPps = true;
  }
  if (hasSps && hasPps) {
    return nals;
  }
  const combined: Uint8Array[] = [];
  if (!hasSps && parsedDesc.sps.length > 0) {
    combined.push(...parsedDesc.sps);
  }
  if (!hasPps && parsedDesc.pps.length > 0) {
    combined.push(...parsedDesc.pps);
  }
  combined.push(...nals);
  return combined;
}

function toEvenDimension(value: number, fallback: number): number {
  const safe = Number.isFinite(value) && value >= 2 ? Math.floor(value) : fallback;
  return Math.max(2, safe & ~1);
}

function extractValidDimensions(
  rawW: unknown,
  rawH: unknown,
): { width: number; height: number } | null {
  if (
    typeof rawW === "number" &&
    typeof rawH === "number" &&
    Number.isFinite(rawW) &&
    Number.isFinite(rawH) &&
    rawW >= 2 &&
    rawH >= 2
  ) {
    return { width: rawW, height: rawH };
  }
  return null;
}

function getTrackDimensions(
  stream: MediaStream,
): { width: number; height: number } | null {
  try {
    const track =
      typeof stream.getVideoTracks === "function"
        ? stream.getVideoTracks()[0]
        : undefined;
    const settings =
      track && typeof track.getSettings === "function"
        ? track.getSettings()
        : undefined;
    return extractValidDimensions(settings?.width, settings?.height);
  } catch {
    return null;
  }
}

function resolveEncoderDimensions(
  stream: MediaStream,
  targetWidth: number,
  targetHeight: number,
  videoEl?: OffscreenVideoLike | null,
): { width: number; height: number } {
  const maxW = toEvenDimension(targetWidth, 640);
  const maxH = toEvenDimension(targetHeight, 480);
  const sourceDims =
    getTrackDimensions(stream) ??
    (videoEl
      ? extractValidDimensions(videoEl.videoWidth, videoEl.videoHeight)
      : null);
  if (sourceDims) {
    const { width: rawW, height: rawH } = sourceDims;
    if (rawW <= maxW && rawH <= maxH) {
      return {
        width: toEvenDimension(rawW, maxW),
        height: toEvenDimension(rawH, maxH),
      };
    }
    const scale = Math.min(maxW / rawW, maxH / rawH);
    return {
      width: toEvenDimension(rawW * scale, maxW),
      height: toEvenDimension(rawH * scale, maxH),
    };
  }
  return { width: maxW, height: maxH };
}

function nowMs(): number {
  if (typeof globalThis.performance?.now === "function") {
    return globalThis.performance.now();
  }
  return Date.now();
}

export function getVideoCallMediaCapabilities(): VideoCallMediaCapabilities {
  const g = globalThis as Record<string, unknown>;
  const nav = g.navigator as
    | { mediaDevices?: { getUserMedia?: unknown } }
    | undefined;
  return {
    hasVideoDecoder: typeof g.VideoDecoder === "function",
    hasVideoEncoder: typeof g.VideoEncoder === "function",
    hasGetUserMedia: typeof nav?.mediaDevices?.getUserMedia === "function",
  };
}

export function decodeBase64ToBytes(b64: string): Uint8Array {
  if (!b64) {
    return new Uint8Array(0);
  }
  const clean = /\s/.test(b64) ? b64.replace(/\s+/g, "") : b64;
  if (clean.length === 0) {
    return new Uint8Array(0);
  }
  const binary = atob(clean);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    out[i] = binary.charCodeAt(i) & 0xff;
  }
  return out;
}

export function encodeBytesToBase64(bytes: Uint8Array): string {
  if (bytes.byteLength === 0) {
    return "";
  }
  const parts: string[] = [];
  for (let i = 0; i < bytes.byteLength; i += BASE64_CHUNK_SIZE) {
    const slice = bytes.subarray(
      i,
      Math.min(i + BASE64_CHUNK_SIZE, bytes.byteLength),
    );
    parts.push(String.fromCharCode(...slice));
  }
  return btoa(parts.join(""));
}

/**
 * Extract an RFC 6381 AVC codec string (e.g. "avc1.42C01E" or "avc1.64001F")
 * from the first SPS NAL unit present in an H.264 Annex-B byte stream.
 */
export function extractAvcCodecFromAnnexB(annexb: Uint8Array): string | null {
  if (annexb.byteLength < 7) {
    return null;
  }
  const nals = splitAnnexBNals(annexb);
  for (let i = 0; i < nals.length; i += 1) {
    const nal = nals[i];
    if (nal.byteLength >= 4 && (nal[0] & 0x1f) === 7) {
      return `avc1.${hexByte(nal[1])}${hexByte(nal[2])}${hexByte(nal[3])}`;
    }
  }
  return null;
}

/**
 * Normalize an H.264 WebCodecs EncodedVideoChunk payload into a 4-byte start-code
 * Annex-B Access Unit. Converts length-prefixed AVCC buffers using `avcDescription`
 * and prepends SPS/PPS from `avcC` on keyframes when missing.
 *
 * When `formatHint === "avc"`, chunks are always parsed as length-prefixed AVCC
 * so 4-byte lengths 256..511 (`0x00 00 01 xx`) or 1 (`0x00 00 00 01`) are never
 * mistaken for Annex-B start codes. When `formatHint` is omitted and a valid
 * `avcC` description is provided, exact AVCC framing takes precedence over
 * Annex-B start-code sniffing.
 */
export function normalizeWebCodecsChunkToAnnexB(
  chunkBytes: Uint8Array,
  isKeyFrame: boolean,
  avcDescription?: AllowSharedBufferSource | null,
  formatHint?: AvcChunkFormat,
): Uint8Array {
  if (chunkBytes.byteLength === 0) {
    return new Uint8Array(0);
  }

  const parsedDesc = parseAvcDescription(avcDescription);
  let nals: Uint8Array[];

  if (formatHint === "avc") {
    nals = splitAvccNalsDetailed(chunkBytes, parsedDesc.nalLengthSize).nals;
  } else if (formatHint === "annexb") {
    if (startsWithAnnexBStartCode(chunkBytes)) {
      nals = splitAnnexBNals(chunkBytes);
    } else {
      nals = splitAvccNalsDetailed(chunkBytes, parsedDesc.nalLengthSize).nals;
    }
  } else {
    const avcc = parsedDesc.isValidAvcC
      ? splitAvccNalsDetailed(chunkBytes, parsedDesc.nalLengthSize)
      : null;
    if (avcc && avcc.exact) {
      nals = avcc.nals;
    } else if (startsWithAnnexBStartCode(chunkBytes)) {
      nals = splitAnnexBNals(chunkBytes);
    } else {
      nals = avcc
        ? avcc.nals
        : splitAvccNalsDetailed(chunkBytes, parsedDesc.nalLengthSize).nals;
    }
  }

  const finalNals = isKeyFrame ? prependMissingSpsPps(nals, parsedDesc) : nals;
  return joinNalsAs4ByteAnnexB(finalNals);
}

export async function listBrowserCameras(): Promise<CameraDeviceInfo[]> {
  const g = globalThis as Record<string, unknown>;
  const nav = g.navigator as
    | { mediaDevices?: { enumerateDevices?: () => Promise<MediaDeviceInfo[]> } }
    | undefined;
  if (typeof nav?.mediaDevices?.enumerateDevices !== "function") {
    return [];
  }
  try {
    const devices = await nav.mediaDevices.enumerateDevices();
    const out: CameraDeviceInfo[] = [];
    const seenIds = new Set<string>();
    for (const dev of devices) {
      if (!dev || dev.kind !== "videoinput") continue;
      const id = typeof dev.deviceId === "string" ? dev.deviceId : "";
      if (!id || seenIds.has(id)) continue;
      seenIds.add(id);
      const rawLabel = typeof dev.label === "string" ? dev.label.trim() : "";
      out.push({
        deviceId: id,
        label: rawLabel || `Camera ${out.length + 1}`,
      });
    }
    return out;
  } catch {
    return [];
  }
}

export class RemoteH264Renderer {
  private readonly options: RemoteH264RendererOptions;
  private canvas: HTMLCanvasElement | null = null;
  private ctx: CanvasRenderingContext2D | null = null;
  private decoder: VideoDecoder | null = null;
  private decoderGen = 0;
  private currentCodec: string | null = null;
  private waitingForKeyframe = true;
  private hasEmittedFirstFrame = false;
  private lastWidth = 0;
  private lastHeight = 0;
  private destroyed = false;
  private reportedUnsupported = false;
  private lastRtpTimestamp: number | null = null;
  private unwrappedRtpTicks = 0;

  constructor(options?: RemoteH264RendererOptions) {
    this.options = options ?? {};
  }

  attachCanvas(canvas: HTMLCanvasElement | null): void {
    if (this.destroyed) {
      this.canvas = null;
      this.ctx = null;
      return;
    }
    if (this.canvas === canvas) {
      return;
    }
    this.canvas = canvas;
    this.ctx = canvas ? canvas.getContext("2d") : null;
  }

  pushFrame(frame: RemoteH264FrameInput): void {
    if (this.destroyed) {
      return;
    }

    const g = globalThis as Record<string, unknown>;
    const VideoDecoderCtor = g.VideoDecoder as typeof VideoDecoder | undefined;
    const EncodedVideoChunkCtor = g.EncodedVideoChunk as
      | typeof EncodedVideoChunk
      | undefined;

    if (
      typeof VideoDecoderCtor !== "function" ||
      typeof EncodedVideoChunkCtor !== "function"
    ) {
      if (!this.reportedUnsupported) {
        this.reportedUnsupported = true;
        this.options.onError?.("WebCodecs VideoDecoder is not supported");
      }
      return;
    }

    if (!frame.data_b64) {
      return;
    }

    // Resync point from transport: discard stale decoder references and require keyframe.
    if (frame.just_synced) {
      this.waitingForKeyframe = true;
      this.resetDecoderForResync();
    }

    // Backpressure guard: prevent latency buildup when decode queue overflows.
    if (
      this.decoder &&
      this.decoder.state === "configured" &&
      this.decoder.decodeQueueSize > MAX_DECODE_QUEUE_SIZE
    ) {
      this.waitingForKeyframe = true;
      this.resetDecoderForResync();
      if (!frame.is_keyframe) {
        this.options.onKeyframeNeeded?.("overflow");
        return;
      }
    }

    // Gate until a keyframe arrives BEFORE base64 decoding so dropped delta frames
    // never waste main-thread CPU or throw VideoDecoder DataError.
    if (this.waitingForKeyframe && !frame.is_keyframe) {
      return;
    }

    if (
      !frame.is_keyframe &&
      (!this.decoder || this.decoder.state !== "configured")
    ) {
      this.waitingForKeyframe = true;
      return;
    }

    let bytes: Uint8Array;
    try {
      bytes = decodeBase64ToBytes(frame.data_b64);
    } catch (err) {
      this.options.onError?.(`Invalid video frame base64: ${formatErrorMessage(err)}`);
      return;
    }
    if (bytes.byteLength === 0) {
      return;
    }

    const detectedCodec = extractAvcCodecFromAnnexB(bytes);
    const targetCodec =
      detectedCodec ?? this.currentCodec ?? DEFAULT_DECODER_CODEC;

    try {
      if (!this.decoder || this.decoder.state === "closed") {
        this.createDecoder(VideoDecoderCtor);
      }

      const decoder = this.decoder;
      if (!decoder) {
        return;
      }

      const needsConfigure =
        decoder.state === "unconfigured" ||
        (frame.is_keyframe &&
          detectedCodec !== null &&
          detectedCodec !== this.currentCodec);

      if (needsConfigure) {
        if (!frame.is_keyframe) {
          this.waitingForKeyframe = true;
          return;
        }
        decoder.configure({
          codec: targetCodec,
          optimizeForLatency: true,
        });
        this.currentCodec = targetCodec;
      }

      const chunk = new EncodedVideoChunkCtor({
        type: frame.is_keyframe ? "key" : "delta",
        timestamp: this.toMicrosecondTimestamp(frame.timestamp),
        data: bytes,
      });

      decoder.decode(chunk);
      if (frame.is_keyframe) {
        this.waitingForKeyframe = false;
      }
    } catch (err) {
      this.handleDecoderFailure(formatErrorMessage(err));
    }
  }

  reset(): void {
    this.decoderGen += 1;
    this.waitingForKeyframe = true;
    this.hasEmittedFirstFrame = false;
    this.lastWidth = 0;
    this.lastHeight = 0;
    this.currentCodec = null;
    this.lastRtpTimestamp = null;
    this.unwrappedRtpTicks = 0;
    this.closeDecoder();
    if (this.canvas && this.ctx && typeof this.ctx.clearRect === "function") {
      try {
        this.ctx.clearRect(0, 0, this.canvas.width, this.canvas.height);
      } catch {
        // Ignore canvas clear failures
      }
    }
  }

  destroy(): void {
    this.destroyed = true;
    this.reset();
    this.canvas = null;
    this.ctx = null;
  }

  private createDecoder(VideoDecoderCtor: typeof VideoDecoder): void {
    this.closeDecoder();
    const gen = ++this.decoderGen;
    this.currentCodec = null;
    this.decoder = new VideoDecoderCtor({
      output: (videoFrame: VideoFrame) => {
        this.handleDecodedFrame(videoFrame, gen);
      },
      error: (err: DOMException) => {
        if (this.destroyed || gen !== this.decoderGen) {
          return;
        }
        this.handleDecoderFailure(formatErrorMessage(err));
      },
    });
  }

  private handleDecodedFrame(videoFrame: VideoFrame, gen: number): void {
    try {
      if (this.destroyed || gen !== this.decoderGen) {
        return;
      }
      const width = videoFrame.displayWidth || videoFrame.codedWidth || 0;
      const height = videoFrame.displayHeight || videoFrame.codedHeight || 0;
      if (width <= 0 || height <= 0) {
        return;
      }

      if (width !== this.lastWidth || height !== this.lastHeight) {
        this.lastWidth = width;
        this.lastHeight = height;
        this.options.onDimensions?.(width, height);
      }

      if (this.canvas && this.ctx) {
        if (this.canvas.width !== width) {
          this.canvas.width = width;
        }
        if (this.canvas.height !== height) {
          this.canvas.height = height;
        }
        this.ctx.drawImage(videoFrame, 0, 0, width, height);
      }

      if (!this.hasEmittedFirstFrame) {
        this.hasEmittedFirstFrame = true;
        this.options.onFirstFrame?.();
      }
    } catch (err) {
      this.options.onError?.(formatErrorMessage(err));
    } finally {
      try {
        videoFrame.close();
      } catch {
        // Ignore already-closed frames
      }
    }
  }

  private resetDecoderForResync(): void {
    this.currentCodec = null;
    if (!this.decoder) {
      return;
    }
    if (this.decoder.state === "closed") {
      this.decoder = null;
      return;
    }
    try {
      this.decoder.reset();
    } catch {
      this.closeDecoder();
    }
  }

  private handleDecoderFailure(message: string): void {
    this.waitingForKeyframe = true;
    this.currentCodec = null;
    this.decoderGen += 1;
    this.closeDecoder();
    this.options.onError?.(message);
    if (!this.destroyed) {
      this.options.onKeyframeNeeded?.("decode_error");
    }
  }

  private closeDecoder(): void {
    const dec = this.decoder;
    this.decoder = null;
    if (!dec) return;
    try {
      if (dec.state !== "closed") {
        dec.close();
      }
    } catch {
      // Ignore decoder close errors
    }
  }

  private toMicrosecondTimestamp(rtpTimestamp: number): number {
    const u32 = Number.isFinite(rtpTimestamp) ? rtpTimestamp >>> 0 : 0;
    if (this.lastRtpTimestamp === null) {
      this.lastRtpTimestamp = u32;
      this.unwrappedRtpTicks = 0;
      return 0;
    }
    const delta = (u32 - this.lastRtpTimestamp) | 0;
    this.lastRtpTimestamp = u32;
    this.unwrappedRtpTicks = Math.max(0, this.unwrappedRtpTicks + delta);
    return Math.round((this.unwrappedRtpTicks * 1_000_000) / VIDEO_CLOCK_HZ);
  }
}

export class LocalCameraEncoder {
  private readonly options: LocalCameraEncoderOptions;
  private readonly targetWidth: number;
  private readonly targetHeight: number;
  private readonly targetFps: number;
  private readonly bitrateBps: number;

  private status: LocalCameraStatus = "idle";
  private destroyed = false;
  private active = false;
  private sessionGen = 0;
  private stream: MediaStream | null = null;
  private videoTrack: MediaStreamTrack | null = null;
  private videoTrackEndedHandler: (() => void) | null = null;
  private emittedStream = false;
  private videoEl: OffscreenVideoLike | null = null;
  private cancelPendingMetadataWait: (() => void) | null = null;
  private encoder: VideoEncoder | null = null;
  private avcFormat: AvcChunkFormat = "avc";
  private pumpTimer: ReturnType<typeof setInterval> | null = null;
  private frameCount = 0;
  private forceNextKeyFrame = true;
  private startClockMs = 0;
  private lastFrameTimestampUs = -1;
  private lastAvcDescription: Uint8Array | null = null;

  constructor(options: LocalCameraEncoderOptions) {
    this.options = options;
    this.targetWidth = toEvenDimension(options.targetWidth ?? 640, 640);
    this.targetHeight = toEvenDimension(options.targetHeight ?? 480, 480);
    const fps = options.targetFps ?? 15;
    this.targetFps = Number.isFinite(fps) && fps > 0 ? Math.min(60, fps) : 15;
    const bitrate = options.bitrateBps ?? 500_000;
    this.bitrateBps =
      Number.isFinite(bitrate) && bitrate > 0 ? Math.round(bitrate) : 500_000;
  }

  async start(deviceId?: string | null): Promise<LocalCameraStatus> {
    if (this.destroyed) {
      return "off";
    }

    // Stop any previous capture cleanly before starting a fresh session.
    this.cleanupResources(true);
    const gen = ++this.sessionGen;

    const caps = getVideoCallMediaCapabilities();
    if (!caps.hasGetUserMedia || !caps.hasVideoEncoder) {
      this.setStatus(
        "unsupported",
        !caps.hasGetUserMedia
          ? "MediaDevices getUserMedia is unavailable"
          : "WebCodecs VideoEncoder is unavailable",
      );
      return "unsupported";
    }

    this.setStatus("starting");

    const trimmedDeviceId =
      typeof deviceId === "string" && deviceId.trim().length > 0
        ? deviceId.trim()
        : null;

    const constraints: MediaStreamConstraints = {
      video: {
        width: { ideal: this.targetWidth },
        height: { ideal: this.targetHeight },
        frameRate: { ideal: this.targetFps, max: Math.max(20, this.targetFps) },
        ...(trimmedDeviceId ? { deviceId: { exact: trimmedDeviceId } } : {}),
      },
      audio: false,
    };

    let stream: MediaStream;
    try {
      stream = await navigator.mediaDevices.getUserMedia(constraints);
    } catch (err) {
      if (this.destroyed || gen !== this.sessionGen) {
        return this.status;
      }
      const mapped = await this.classifyGetUserMediaError(err);
      if (this.destroyed || gen !== this.sessionGen) {
        return this.status;
      }
      this.setStatus(mapped, formatErrorMessage(err));
      return mapped;
    }

    if (this.destroyed || gen !== this.sessionGen) {
      this.stopStreamTracks(stream);
      return this.status;
    }

    this.stream = stream;
    const videoTrack =
      typeof stream.getVideoTracks === "function"
        ? (stream.getVideoTracks()[0] ?? null)
        : null;
    this.attachTrackEndedListener(videoTrack, gen);
    this.emittedStream = true;
    this.options.onStreamChange?.(stream);

    if (
      videoTrack &&
      typeof videoTrack.readyState === "string" &&
      videoTrack.readyState !== "live"
    ) {
      this.handleTrackEnded(gen, "Camera track is no longer live");
      return "unavailable";
    }

    const videoEl = this.createOffscreenVideo(
      stream,
      this.targetWidth,
      this.targetHeight,
    );
    this.videoEl = videoEl;
    await this.playAndWaitForMetadata(
      videoEl,
      getTrackDimensions(stream) !== null,
      gen,
    );

    if (this.destroyed || gen !== this.sessionGen) {
      this.cleanupResources(true);
      return this.status;
    }

    const { width, height } = resolveEncoderDimensions(
      stream,
      this.targetWidth,
      this.targetHeight,
      videoEl,
    );

    const g = globalThis as Record<string, unknown>;
    const VideoEncoderCtor = g.VideoEncoder as typeof VideoEncoder | undefined;
    if (typeof VideoEncoderCtor !== "function") {
      this.cleanupResources(true);
      this.setStatus("unsupported", "WebCodecs VideoEncoder is unavailable");
      return "unsupported";
    }

    const candidateConfigs = this.buildEncoderConfigs(width, height);
    const selectedConfig = await this.selectSupportedEncoderConfig(
      VideoEncoderCtor,
      candidateConfigs,
    );

    if (this.destroyed || gen !== this.sessionGen) {
      this.cleanupResources(true);
      return this.status;
    }

    if (!selectedConfig) {
      this.cleanupResources(true);
      this.setStatus(
        "unsupported",
        "No supported H.264 VideoEncoder configuration found",
      );
      return "unsupported";
    }

    let encoder: VideoEncoder;
    try {
      encoder = new VideoEncoderCtor({
        output: (
          chunk: EncodedVideoChunk,
          metadata?: EncodedVideoChunkMetadata,
        ) => {
          this.handleEncodedChunk(chunk, metadata, gen);
        },
        error: (err: DOMException) => {
          if (this.destroyed || gen !== this.sessionGen || !this.active) {
            return;
          }
          const detail = formatErrorMessage(err);
          this.cleanupResources(true);
          this.setStatus("error", detail);
        },
      });
    } catch (err) {
      this.cleanupResources(true);
      this.setStatus("error", formatErrorMessage(err));
      return "error";
    }

    const configured = this.configureEncoderWithFallback(
      encoder,
      selectedConfig,
      candidateConfigs,
    );
    if (!configured.ok) {
      try {
        if (encoder.state !== "closed") {
          encoder.close();
        }
      } catch {
        // Ignore close error
      }
      this.cleanupResources(true);
      this.setStatus("error", configured.error);
      return "error";
    }

    this.encoder = encoder;
    this.avcFormat = configured.avcFormat;
    if (
      this.videoTrack &&
      typeof this.videoTrack.readyState === "string" &&
      this.videoTrack.readyState !== "live"
    ) {
      this.handleTrackEnded(gen, "Camera track ended");
      return "unavailable";
    }
    this.active = true;
    this.frameCount = 0;
    this.forceNextKeyFrame = true;
    this.startClockMs = nowMs();
    this.lastFrameTimestampUs = -1;
    this.lastAvcDescription = null;

    const intervalMs = Math.max(16, Math.round(1000 / this.targetFps));
    this.pumpTimer = setInterval(() => {
      this.pumpFrame(gen);
    }, intervalMs);

    this.setStatus("active");
    return "active";
  }

  requestKeyframe(): void {
    this.forceNextKeyFrame = true;
  }

  stop(): void {
    this.sessionGen += 1;
    const wasRunning =
      this.active || this.status === "active" || this.status === "starting";
    this.cleanupResources(true);
    if (!this.destroyed && wasRunning) {
      this.setStatus("off");
    }
  }

  destroy(): void {
    this.destroyed = true;
    this.sessionGen += 1;
    this.cleanupResources(true);
  }

  private setStatus(status: LocalCameraStatus, detail?: string): void {
    if (this.destroyed) {
      return;
    }
    this.status = status;
    this.options.onStatusChange?.(status, detail);
  }

  private async classifyGetUserMediaError(
    err: unknown,
  ): Promise<LocalCameraStatus> {
    const name = getErrorName(err);
    if (name === "NotFoundError" || name === "DevicesNotFoundError") {
      return "unavailable";
    }
    if (name === "NotAllowedError" || name === "PermissionDeniedError") {
      return "denied";
    }
    if (name === "OverconstrainedError") {
      const hasAnyVideoInput = await this.hasAnyVideoInputDevice();
      return hasAnyVideoInput ? "error" : "unavailable";
    }
    return "error";
  }

  private async hasAnyVideoInputDevice(): Promise<boolean> {
    const g = globalThis as Record<string, unknown>;
    const nav = g.navigator as
      | { mediaDevices?: { enumerateDevices?: () => Promise<MediaDeviceInfo[]> } }
      | undefined;
    if (typeof nav?.mediaDevices?.enumerateDevices !== "function") {
      return false;
    }
    try {
      const devices = await nav.mediaDevices.enumerateDevices();
      return devices.some((d) => d && d.kind === "videoinput");
    } catch {
      return false;
    }
  }

  private async playAndWaitForMetadata(
    videoEl: OffscreenVideoLike,
    hasTrackDims: boolean,
    gen: number,
  ): Promise<void> {
    const hasVideoDims = () =>
      extractValidDimensions(videoEl.videoWidth, videoEl.videoHeight) !== null;

    let metadataReady: Promise<void> | null = null;
    let finishMetadataWait: () => void = () => undefined;

    if (
      !hasVideoDims() &&
      typeof videoEl.addEventListener === "function" &&
      (videoEl.readyState < 1 || !hasTrackDims)
    ) {
      let settled = false;
      let timer: ReturnType<typeof setTimeout> | null = null;
      let resolveMeta: (() => void) | null = null;
      const onMeta = () => finish();
      const finish = () => {
        if (settled) return;
        settled = true;
        if (timer !== null) {
          clearTimeout(timer);
          timer = null;
        }
        if (this.cancelPendingMetadataWait === finish) {
          this.cancelPendingMetadataWait = null;
        }
        try {
          videoEl.removeEventListener?.("loadedmetadata", onMeta);
          videoEl.removeEventListener?.("loadeddata", onMeta);
        } catch {
          // Ignore listener cleanup errors
        }
        resolveMeta?.();
      };
      finishMetadataWait = finish;
      this.cancelPendingMetadataWait = finish;
      metadataReady = new Promise<void>((resolve) => {
        resolveMeta = resolve;
        timer = setTimeout(finish, METADATA_WAIT_TIMEOUT_MS);
        try {
          videoEl.addEventListener?.("loadedmetadata", onMeta);
          videoEl.addEventListener?.("loadeddata", onMeta);
        } catch {
          finish();
        }
      });
    }

    if (typeof videoEl.play === "function") {
      try {
        await Promise.resolve(videoEl.play());
      } catch {
        // Autoplay promise may reject in headless/background tabs; readyState check guards pumping.
      }
    }

    if (metadataReady) {
      if (
        this.destroyed ||
        gen !== this.sessionGen ||
        hasVideoDims() ||
        (hasTrackDims && videoEl.readyState >= 1)
      ) {
        finishMetadataWait();
      }
      await metadataReady;
    }
  }

  private createOffscreenVideo(
    stream: MediaStream,
    width: number,
    height: number,
  ): OffscreenVideoLike {
    const g = globalThis as Record<string, unknown>;
    const doc = g.document as
      | { createElement?: (tag: string) => HTMLVideoElement }
      | undefined;
    if (doc && typeof doc.createElement === "function") {
      const el = doc.createElement("video");
      el.muted = true;
      el.playsInline = true;
      el.autoplay = true;
      el.srcObject = stream;
      return el;
    }
    return {
      muted: true,
      playsInline: true,
      autoplay: true,
      srcObject: stream,
      readyState: 4,
      videoWidth: width,
      videoHeight: height,
      pause: () => undefined,
    };
  }

  private buildEncoderConfigs(
    width: number,
    height: number,
  ): VideoEncoderConfig[] {
    const base = {
      width,
      height,
      bitrate: this.bitrateBps,
      framerate: this.targetFps,
      latencyMode: "realtime" as const,
      bitrateMode: "variable" as const,
    };
    const configs: VideoEncoderConfig[] = [];
    for (const codec of ENCODER_CODEC_CANDIDATES) {
      configs.push({
        ...base,
        codec,
        avc: { format: "annexb" },
      });
    }
    for (const codec of ENCODER_CODEC_CANDIDATES) {
      configs.push({
        ...base,
        codec,
      });
    }
    return configs;
  }

  private async selectSupportedEncoderConfig(
    VideoEncoderCtor: typeof VideoEncoder,
    configs: VideoEncoderConfig[],
  ): Promise<VideoEncoderConfig | null> {
    if (typeof VideoEncoderCtor.isConfigSupported !== "function") {
      return configs[0] ?? null;
    }
    for (const cfg of configs) {
      try {
        const support = await VideoEncoderCtor.isConfigSupported(cfg);
        if (support && support.supported) {
          return cfg;
        }
      } catch {
        // Try next candidate configuration
      }
    }
    return null;
  }

  private configureEncoderWithFallback(
    encoder: VideoEncoder,
    preferred: VideoEncoderConfig,
    allConfigs: VideoEncoderConfig[],
  ):
    | { ok: true; config: VideoEncoderConfig; avcFormat: AvcChunkFormat }
    | { ok: false; error: string } {
    const ordered = [
      preferred,
      ...allConfigs.filter((cfg) => cfg !== preferred),
    ];
    let lastError = "Failed to configure VideoEncoder";
    for (const cfg of ordered) {
      try {
        encoder.configure(cfg);
        const avcFormat: AvcChunkFormat =
          cfg.avc?.format === "annexb" ? "annexb" : "avc";
        return { ok: true, config: cfg, avcFormat };
      } catch (err) {
        lastError = formatErrorMessage(err);
      }
    }
    return { ok: false, error: lastError };
  }

  private pumpFrame(gen: number): void {
    if (
      this.destroyed ||
      gen !== this.sessionGen ||
      !this.active ||
      !this.encoder ||
      !this.videoEl
    ) {
      return;
    }
    if (
      this.videoTrack &&
      typeof this.videoTrack.readyState === "string" &&
      this.videoTrack.readyState !== "live"
    ) {
      this.handleTrackEnded(gen, "Camera track is no longer live");
      return;
    }
    if (this.encoder.state !== "configured") {
      return;
    }
    if (this.videoEl.readyState < 2) {
      return;
    }
    if (this.encoder.encodeQueueSize > MAX_ENCODE_QUEUE_SIZE) {
      return;
    }

    const g = globalThis as Record<string, unknown>;
    const VideoFrameCtor = g.VideoFrame as typeof VideoFrame | undefined;
    if (typeof VideoFrameCtor !== "function") {
      return;
    }

    const elapsedUs = Math.max(0, Math.round((nowMs() - this.startClockMs) * 1000));
    const timestamp = Math.max(this.lastFrameTimestampUs + 1, elapsedUs);

    const isKeyFrame =
      this.forceNextKeyFrame ||
      this.frameCount === 0 ||
      this.frameCount % KEYFRAME_INTERVAL_FRAMES === 0;

    let vf: VideoFrame | null = null;
    try {
      vf = new VideoFrameCtor(this.videoEl as CanvasImageSource, { timestamp });
      this.lastFrameTimestampUs = timestamp;
      this.encoder.encode(vf, { keyFrame: isKeyFrame });
      this.forceNextKeyFrame = false;
      this.frameCount += 1;
    } catch (err) {
      const name = getErrorName(err);
      if (this.isEncoderClosed()) {
        const detail = formatErrorMessage(err);
        this.cleanupResources(true);
        this.setStatus("error", detail);
      } else if (name !== "InvalidStateError") {
        this.options.onStatusChange?.(this.status, formatErrorMessage(err));
      }
    } finally {
      if (vf) {
        try {
          vf.close();
        } catch {
          // Ignore already-closed VideoFrame
        }
      }
    }
  }

  private handleEncodedChunk(
    chunk: EncodedVideoChunk,
    metadata: EncodedVideoChunkMetadata | undefined,
    gen: number,
  ): void {
    if (this.destroyed || gen !== this.sessionGen || !this.active) {
      return;
    }

    const rawDesc = metadata?.decoderConfig?.description;
    if (rawDesc) {
      const descBytes = toUint8Array(rawDesc);
      if (descBytes && descBytes.byteLength > 0) {
        this.lastAvcDescription = new Uint8Array(descBytes);
      }
    }

    const rawBytes = new Uint8Array(chunk.byteLength);
    try {
      chunk.copyTo(rawBytes);
    } catch {
      return;
    }

    const isKeyFrame = chunk.type === "key";
    const annexb = normalizeWebCodecsChunkToAnnexB(
      rawBytes,
      isKeyFrame,
      rawDesc ?? this.lastAvcDescription,
      this.avcFormat,
    );
    if (annexb.byteLength === 0) {
      return;
    }

    const b64 = encodeBytesToBase64(annexb);
    if (this.destroyed || gen !== this.sessionGen || !this.active) {
      return;
    }
    this.options.onEncodedAu(b64, isKeyFrame);
  }

  private attachTrackEndedListener(
    track: MediaStreamTrack | null,
    gen: number,
  ): void {
    this.detachTrackEndedListener();
    if (!track) return;
    this.videoTrack = track;
    if (typeof track.addEventListener === "function") {
      const onEnded = () => {
        this.handleTrackEnded(gen, "Camera track ended");
      };
      this.videoTrackEndedHandler = onEnded;
      try {
        track.addEventListener("ended", onEnded);
      } catch {
        this.videoTrackEndedHandler = null;
      }
    }
  }

  private detachTrackEndedListener(): void {
    const track = this.videoTrack;
    const handler = this.videoTrackEndedHandler;
    this.videoTrack = null;
    this.videoTrackEndedHandler = null;
    if (track && handler && typeof track.removeEventListener === "function") {
      try {
        track.removeEventListener("ended", handler);
      } catch {
        // Ignore listener removal errors
      }
    }
  }

  private handleTrackEnded(gen: number, detail: string): void {
    if (this.destroyed || gen !== this.sessionGen) {
      return;
    }
    this.sessionGen += 1;
    this.cleanupResources(true);
    this.setStatus("unavailable", detail);
  }

  private isEncoderClosed(): boolean {
    return this.encoder !== null && this.encoder.state === "closed";
  }

  private stopStreamTracks(stream: MediaStream | null): void {
    if (!stream) return;
    try {
      const tracks =
        typeof stream.getTracks === "function" ? stream.getTracks() : [];
      for (const track of tracks) {
        try {
          track.stop();
        } catch {
          // Ignore track stop errors
        }
      }
    } catch {
      // Ignore
    }
  }

  private cleanupResources(emitStreamNull: boolean): void {
    this.active = false;
    this.forceNextKeyFrame = false;
    this.lastAvcDescription = null;
    this.avcFormat = "avc";

    if (this.pumpTimer !== null) {
      clearInterval(this.pumpTimer);
      this.pumpTimer = null;
    }

    if (this.cancelPendingMetadataWait) {
      const cancel = this.cancelPendingMetadataWait;
      this.cancelPendingMetadataWait = null;
      cancel();
    }

    this.detachTrackEndedListener();

    const prevStream = this.stream;
    this.stream = null;
    this.stopStreamTracks(prevStream);

    if (this.videoEl) {
      try {
        this.videoEl.pause?.();
      } catch {
        // Ignore pause errors
      }
      try {
        this.videoEl.srcObject = null;
      } catch {
        // Ignore srcObject clear errors
      }
      this.videoEl = null;
    }

    const prevEncoder = this.encoder;
    this.encoder = null;
    if (prevEncoder) {
      try {
        if (prevEncoder.state !== "closed") {
          prevEncoder.close();
        }
      } catch {
        // Ignore close errors
      }
    }

    if (emitStreamNull && (this.emittedStream || prevStream !== null)) {
      this.emittedStream = false;
      this.options.onStreamChange?.(null);
    }
  }
}
