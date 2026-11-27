import test from "node:test";
import assert from "node:assert/strict";
import {
  LocalCameraEncoder,
  RemoteH264Renderer,
  decodeBase64ToBytes,
  encodeBytesToBase64,
  extractAvcCodecFromAnnexB,
  getVideoCallMediaCapabilities,
  listBrowserCameras,
  normalizeWebCodecsChunkToAnnexB,
  type LocalCameraStatus,
  type RemoteKeyframeReason,
} from "../src/utils/videoCallMedia.ts";

// Valid Constrained Baseline SPS (avc1.42C01E), PPS, IDR, and P-slice NAL units
const SAMPLE_SPS_BASELINE = new Uint8Array([
  0x67, 0x42, 0xc0, 0x1e, 0xda, 0x05, 0x07, 0xec, 0x04, 0x40,
]);
const SAMPLE_SPS_HIGH = new Uint8Array([
  0x67, 0x64, 0x00, 0x1f, 0xac, 0xd9, 0x40, 0x50,
]);
const SAMPLE_PPS = new Uint8Array([0x68, 0xce, 0x0f, 0x2c, 0x80]);
const SAMPLE_IDR = new Uint8Array([0x65, 0x88, 0x84, 0x5c, 0x46, 0x28]);
const SAMPLE_PSLICE = new Uint8Array([0x41, 0x9a, 0x21, 0x70, 0x09, 0x6c]);

function concatBytes(...parts: Uint8Array[]): Uint8Array {
  const total = parts.reduce((acc, p) => acc + p.byteLength, 0);
  const out = new Uint8Array(total);
  let offset = 0;
  for (const p of parts) {
    out.set(p, offset);
    offset += p.byteLength;
  }
  return out;
}

function annexB4(...nals: Uint8Array[]): Uint8Array {
  const parts: Uint8Array[] = [];
  for (const nal of nals) {
    parts.push(new Uint8Array([0x00, 0x00, 0x00, 0x01]), nal);
  }
  return concatBytes(...parts);
}

function buildAvcC(
  sps: Uint8Array,
  pps: Uint8Array,
  lengthSizeMinusOne = 3,
): Uint8Array {
  return concatBytes(
    new Uint8Array([
      1, // configurationVersion
      sps[1] ?? 0x42, // AVCProfileIndication
      sps[2] ?? 0xc0, // profile_compatibility
      sps[3] ?? 0x1e, // AVCLevelIndication
      0xfc | (lengthSizeMinusOne & 0x03),
      0xe1, // 1 SPS
      (sps.byteLength >> 8) & 0xff,
      sps.byteLength & 0xff,
    ]),
    sps,
    new Uint8Array([
      1, // 1 PPS
      (pps.byteLength >> 8) & 0xff,
      pps.byteLength & 0xff,
    ]),
    pps,
  );
}

function buildAvccPayload(nals: Uint8Array[], lengthSize = 4): Uint8Array {
  const parts: Uint8Array[] = [];
  for (const nal of nals) {
    const lenBuf = new Uint8Array(lengthSize);
    let remaining = nal.byteLength;
    for (let i = lengthSize - 1; i >= 0; i -= 1) {
      lenBuf[i] = remaining & 0xff;
      remaining = Math.floor(remaining / 256);
    }
    parts.push(lenBuf, nal);
  }
  return concatBytes(...parts);
}

interface SavedGlobalProp {
  key: string;
  existed: boolean;
  descriptor: PropertyDescriptor | undefined;
}

function withMockedGlobals(
  overrides: Record<string, unknown>,
  fn: () => Promise<void> | void,
): Promise<void> {
  const saved: SavedGlobalProp[] = [];
  const g = globalThis as Record<string, unknown>;
  for (const [key, value] of Object.entries(overrides)) {
    const existed = Object.prototype.hasOwnProperty.call(g, key);
    const descriptor = Object.getOwnPropertyDescriptor(g, key);
    saved.push({ key, existed, descriptor });
    if (value === undefined) {
      Object.defineProperty(g, key, {
        value: undefined,
        configurable: true,
        writable: true,
        enumerable: true,
      });
    } else {
      Object.defineProperty(g, key, {
        value,
        configurable: true,
        writable: true,
        enumerable: true,
      });
    }
  }

  const restore = () => {
    for (let i = saved.length - 1; i >= 0; i -= 1) {
      const { key, existed, descriptor } = saved[i];
      if (existed && descriptor) {
        Object.defineProperty(g, key, descriptor);
      } else {
        delete g[key];
      }
    }
  };

  return Promise.resolve()
    .then(() => fn())
    .finally(restore);
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

// ---------------------------------------------------------------------------
// 1. Base64 encode/decode tests
// ---------------------------------------------------------------------------

test("base64 roundtrips empty, all 256 byte values, unaligned lengths, and 256 KiB buffers without stack overflow", () => {
  assert.equal(encodeBytesToBase64(new Uint8Array(0)), "");
  assert.deepEqual(decodeBase64ToBytes(""), new Uint8Array(0));
  assert.deepEqual(decodeBase64ToBytes("   \r\n "), new Uint8Array(0));

  const allBytes = new Uint8Array(256);
  for (let i = 0; i < 256; i += 1) {
    allBytes[i] = i;
  }
  const allB64 = encodeBytesToBase64(allBytes);
  assert.deepEqual(decodeBase64ToBytes(allB64), allBytes);

  // Unaligned lengths (1, 2, 3, 5, 7)
  for (const len of [1, 2, 3, 5, 7]) {
    const sample = allBytes.subarray(10, 10 + len);
    const encoded = encodeBytesToBase64(sample);
    assert.deepEqual(decodeBase64ToBytes(` ${encoded}\n`), sample);
  }

  // Large buffer (256 KiB) exceeds single-call String.fromCharCode spread stack limit
  const large = new Uint8Array(256 * 1024 + 13);
  for (let i = 0; i < large.byteLength; i += 1) {
    large[i] = (i * 31 + 17) & 0xff;
  }
  const largeB64 = encodeBytesToBase64(large);
  const decodedLarge = decodeBase64ToBytes(largeB64);
  assert.equal(decodedLarge.byteLength, large.byteLength);
  assert.deepEqual(decodedLarge, large);
});

// ---------------------------------------------------------------------------
// 2. SPS codec extraction & Annex-B / AVCC normalization tests
// ---------------------------------------------------------------------------

test("extractAvcCodecFromAnnexB parses 4-byte and 3-byte Annex-B SPS profiles accurately", () => {
  const baselineAu = annexB4(SAMPLE_SPS_BASELINE, SAMPLE_PPS, SAMPLE_IDR);
  assert.equal(extractAvcCodecFromAnnexB(baselineAu), "avc1.42C01E");

  const highAu3Byte = concatBytes(
    new Uint8Array([0x00, 0x00, 0x01]),
    new Uint8Array([0x09, 0xf0]), // AUD before SPS
    new Uint8Array([0x00, 0x00, 0x01]),
    SAMPLE_SPS_HIGH,
    new Uint8Array([0x00, 0x00, 0x01]),
    SAMPLE_PPS,
  );
  assert.equal(extractAvcCodecFromAnnexB(highAu3Byte), "avc1.64001F");

  // Delta frame with no SPS returns null
  assert.equal(extractAvcCodecFromAnnexB(annexB4(SAMPLE_PSLICE)), null);

  // Truncated SPS (< 4 bytes) returns null
  const truncatedSps = annexB4(new Uint8Array([0x67, 0x42, 0xc0]));
  assert.equal(extractAvcCodecFromAnnexB(truncatedSps), null);
});

test("normalizeWebCodecsChunkToAnnexB normalizes 3-byte Annex-B, converts 4-byte/2-byte AVCC, and prepends avcC SPS/PPS on keyframes", () => {
  // Mixed 3-byte and 4-byte Annex-B normalized to canonical 4-byte start codes
  const mixedAnnexB = concatBytes(
    new Uint8Array([0x00, 0x00, 0x01]),
    SAMPLE_SPS_BASELINE,
    new Uint8Array([0x00, 0x00, 0x00, 0x01]),
    SAMPLE_PPS,
    new Uint8Array([0x00, 0x00, 0x01]),
    SAMPLE_IDR,
  );
  const expectedKey = annexB4(SAMPLE_SPS_BASELINE, SAMPLE_PPS, SAMPLE_IDR);
  assert.deepEqual(
    normalizeWebCodecsChunkToAnnexB(mixedAnnexB, true),
    expectedKey,
  );

  // 4-byte AVCC delta frame (no SPS/PPS prepended)
  const avccDelta = buildAvccPayload([SAMPLE_PSLICE], 4);
  const avcC = buildAvcC(SAMPLE_SPS_BASELINE, SAMPLE_PPS, 3);
  assert.deepEqual(
    normalizeWebCodecsChunkToAnnexB(avccDelta, false, avcC),
    annexB4(SAMPLE_PSLICE),
  );

  // 4-byte AVCC keyframe missing SPS/PPS -> prepended from avcC (ArrayBuffer input)
  const avccIdrOnly = buildAvccPayload([SAMPLE_IDR], 4);
  assert.deepEqual(
    normalizeWebCodecsChunkToAnnexB(avccIdrOnly, true, avcC.buffer),
    expectedKey,
  );

  // 4-byte AVCC keyframe that ALREADY contains SPS/PPS -> no duplicate SPS/PPS
  const avccWithSpsPps = buildAvccPayload(
    [SAMPLE_SPS_BASELINE, SAMPLE_PPS, SAMPLE_IDR],
    4,
  );
  assert.deepEqual(
    normalizeWebCodecsChunkToAnnexB(avccWithSpsPps, true, avcC),
    expectedKey,
  );

  // 2-byte AVCC (lengthSizeMinusOne = 1)
  const avcC2 = buildAvcC(SAMPLE_SPS_HIGH, SAMPLE_PPS, 1);
  const avcc2Idr = buildAvccPayload([SAMPLE_IDR], 2);
  assert.deepEqual(
    normalizeWebCodecsChunkToAnnexB(avcc2Idr, true, avcC2),
    annexB4(SAMPLE_SPS_HIGH, SAMPLE_PPS, SAMPLE_IDR),
  );
});

test("normalizeWebCodecsChunkToAnnexB does not misdetect 261-byte (0x00 00 01 05), 300-byte, or 1-byte AVCC chunks as Annex-B", () => {
  const avcC = buildAvcC(SAMPLE_SPS_BASELINE, SAMPLE_PPS, 3);

  // 261-byte P-slice NAL -> 4-byte AVCC length prefix is [0x00, 0x00, 0x01, 0x05]
  // Previously matched 3-byte Annex-B start code [0x00, 0x00, 0x01] and corrupted the NAL header to 0x05 (false IDR)!
  const pSlice261 = new Uint8Array(261);
  pSlice261[0] = 0x41; // non-IDR slice
  for (let i = 1; i < pSlice261.byteLength; i += 1) {
    pSlice261[i] = (i * 13 + 7) & 0xff;
  }
  const avcc261 = buildAvccPayload([pSlice261], 4);
  assert.deepEqual(avcc261.subarray(0, 4), new Uint8Array([0x00, 0x00, 0x01, 0x05]));
  const expected261 = annexB4(pSlice261);

  // With explicit formatHint = "avc" (with and without avcC description)
  const norm261Hinted = normalizeWebCodecsChunkToAnnexB(avcc261, false, avcC, "avc");
  assert.deepEqual(norm261Hinted, expected261);
  assert.equal(norm261Hinted[4], 0x41);
  assert.deepEqual(
    normalizeWebCodecsChunkToAnnexB(avcc261, false, null, "avc"),
    expected261,
  );

  // Without formatHint, valid avcC description prefers exact AVCC framing
  assert.deepEqual(
    normalizeWebCodecsChunkToAnnexB(avcc261, false, avcC),
    expected261,
  );

  // 300-byte P-slice NAL -> 4-byte AVCC length prefix is [0x00, 0x00, 0x01, 0x2c]
  const pSlice300 = new Uint8Array(300);
  pSlice300[0] = 0x41;
  for (let i = 1; i < pSlice300.byteLength; i += 1) {
    pSlice300[i] = (i * 29 + 3) & 0xff;
  }
  const avcc300 = buildAvccPayload([pSlice300], 4);
  assert.deepEqual(avcc300.subarray(0, 4), new Uint8Array([0x00, 0x00, 0x01, 0x2c]));
  const expected300 = annexB4(pSlice300);
  assert.deepEqual(
    normalizeWebCodecsChunkToAnnexB(avcc300, false, avcC, "avc"),
    expected300,
  );
  assert.deepEqual(
    normalizeWebCodecsChunkToAnnexB(avcc300, false, avcC),
    expected300,
  );

  // Multi-NAL AVCC starting with 1-byte NAL (prefix [0x00, 0x00, 0x00, 0x01]) + 261-byte P-slice
  const aud1Byte = new Uint8Array([0x09]);
  const avccMulti = buildAvccPayload([aud1Byte, pSlice261], 4);
  assert.deepEqual(avccMulti.subarray(0, 4), new Uint8Array([0x00, 0x00, 0x00, 0x01]));
  const expectedMulti = annexB4(aud1Byte, pSlice261);
  assert.deepEqual(
    normalizeWebCodecsChunkToAnnexB(avccMulti, false, avcC, "avc"),
    expectedMulti,
  );
  assert.deepEqual(
    normalizeWebCodecsChunkToAnnexB(avccMulti, false, avcC),
    expectedMulti,
  );

  // 261-byte keyframe IDR in AVCC prepends SPS/PPS cleanly
  const idr261 = new Uint8Array(261);
  idr261[0] = 0x65;
  idr261.fill(0xab, 1);
  assert.deepEqual(
    normalizeWebCodecsChunkToAnnexB(buildAvccPayload([idr261], 4), true, avcC, "avc"),
    annexB4(SAMPLE_SPS_BASELINE, SAMPLE_PPS, idr261),
  );

  // Genuine Annex-B chunk passed alongside an avcC description still parses as Annex-B
  const genuineAnnexB = annexB4(SAMPLE_IDR);
  assert.deepEqual(
    normalizeWebCodecsChunkToAnnexB(genuineAnnexB, true, avcC),
    annexB4(SAMPLE_SPS_BASELINE, SAMPLE_PPS, SAMPLE_IDR),
  );
});

// ---------------------------------------------------------------------------
// 3. Capabilities & listBrowserCameras
// ---------------------------------------------------------------------------

test("getVideoCallMediaCapabilities and listBrowserCameras handle presence, filtering, and fallback", async () => {
  await withMockedGlobals(
    {
      VideoDecoder: undefined,
      VideoEncoder: undefined,
      navigator: undefined,
    },
    async () => {
      assert.deepEqual(getVideoCallMediaCapabilities(), {
        hasVideoDecoder: false,
        hasVideoEncoder: false,
        hasGetUserMedia: false,
      });
      assert.deepEqual(await listBrowserCameras(), []);
    },
  );

  await withMockedGlobals(
    {
      VideoDecoder: class {},
      VideoEncoder: class {},
      navigator: {
        mediaDevices: {
          getUserMedia: async () => ({}),
          enumerateDevices: async () => [
            { kind: "audioinput", deviceId: "mic-1", label: "USB Mic" },
            { kind: "videoinput", deviceId: "", label: "" },
            { kind: "videoinput", deviceId: "cam-1", label: "HD Pro Webcam" },
            { kind: "videoinput", deviceId: "cam-1", label: "Duplicate" },
            { kind: "videoinput", deviceId: "cam-2", label: "   " },
          ],
        },
      },
    },
    async () => {
      assert.deepEqual(getVideoCallMediaCapabilities(), {
        hasVideoDecoder: true,
        hasVideoEncoder: true,
        hasGetUserMedia: true,
      });
      assert.deepEqual(await listBrowserCameras(), [
        { deviceId: "cam-1", label: "HD Pro Webcam" },
        { deviceId: "cam-2", label: "Camera 2" },
      ]);
    },
  );
});

// ---------------------------------------------------------------------------
// 4. RemoteH264Renderer tests
// ---------------------------------------------------------------------------

interface MockDecodedChunkRecord {
  type: "key" | "delta";
  timestamp: number;
  data: Uint8Array;
}

class MockEncodedVideoChunk {
  readonly type: "key" | "delta";
  readonly timestamp: number;
  readonly byteLength: number;
  readonly data: Uint8Array;

  constructor(init: { type: "key" | "delta"; timestamp: number; data: AllowSharedBufferSource }) {
    this.type = init.type;
    this.timestamp = init.timestamp;
    const raw =
      init.data instanceof Uint8Array
        ? init.data
        : ArrayBuffer.isView(init.data)
          ? new Uint8Array(init.data.buffer, init.data.byteOffset, init.data.byteLength)
          : new Uint8Array(init.data);
    this.data = new Uint8Array(raw);
    this.byteLength = this.data.byteLength;
  }

  copyTo(dest: BufferSource): void {
    const out =
      dest instanceof Uint8Array
        ? dest
        : ArrayBuffer.isView(dest)
          ? new Uint8Array(dest.buffer, dest.byteOffset, dest.byteLength)
          : new Uint8Array(dest);
    out.set(this.data);
  }
}

class MockVideoFrame {
  displayWidth: number;
  displayHeight: number;
  codedWidth: number;
  codedHeight: number;
  timestamp: number;
  closed = false;
  closeCount = 0;

  constructor(
    _source?: unknown,
    init?: { timestamp?: number; displayWidth?: number; displayHeight?: number },
  ) {
    this.displayWidth = init?.displayWidth ?? 640;
    this.displayHeight = init?.displayHeight ?? 480;
    this.codedWidth = this.displayWidth;
    this.codedHeight = this.displayHeight;
    this.timestamp = init?.timestamp ?? 0;
  }

  close(): void {
    this.closed = true;
    this.closeCount += 1;
  }
}

test("RemoteH264Renderer gates keyframes, configures SPS codec dynamically, always closes VideoFrame, guards overflow, and recovers from errors", async () => {
  const createdDecoders: MockDecoder[] = [];
  const emittedFrames: MockVideoFrame[] = [];
  let nextFrameWidth = 640;
  let nextFrameHeight = 360;
  let throwOnNextDecode = false;

  class MockDecoder {
    state: CodecState = "unconfigured";
    decodeQueueSize = 0;
    configuredCodecs: VideoDecoderConfig[] = [];
    decodedChunks: MockDecodedChunkRecord[] = [];
    resetCount = 0;
    closeCount = 0;
    readonly init: VideoDecoderInit;

    constructor(init: VideoDecoderInit) {
      this.init = init;
      createdDecoders.push(this);
    }

    configure(config: VideoDecoderConfig): void {
      if (this.state === "closed") {
        throw new Error("Cannot configure closed decoder");
      }
      this.configuredCodecs.push({ ...config });
      this.state = "configured";
    }

    decode(chunk: MockEncodedVideoChunk): void {
      if (this.state !== "configured") {
        throw new Error("Decoder not configured");
      }
      if (throwOnNextDecode) {
        throwOnNextDecode = false;
        this.state = "closed";
        throw new Error("Simulated decode failure");
      }
      this.decodedChunks.push({
        type: chunk.type,
        timestamp: chunk.timestamp,
        data: chunk.data,
      });
      const vf = new MockVideoFrame(null, {
        timestamp: chunk.timestamp,
        displayWidth: nextFrameWidth,
        displayHeight: nextFrameHeight,
      });
      emittedFrames.push(vf);
      this.init.output(vf as unknown as VideoFrame);
    }

    reset(): void {
      this.resetCount += 1;
      this.decodeQueueSize = 0;
      if (this.state !== "closed") {
        this.state = "unconfigured";
      }
    }

    close(): void {
      this.closeCount += 1;
      this.state = "closed";
    }

    triggerAsyncError(msg: string): void {
      this.state = "closed";
      this.init.error(new Error(msg) as unknown as DOMException);
    }
  }

  let drawCalls = 0;
  let clearCalls = 0;
  let throwOnDraw = false;
  const mockCanvas = {
    width: 0,
    height: 0,
    getContext: (kind: string) =>
      kind === "2d"
        ? {
            drawImage: () => {
              drawCalls += 1;
              if (throwOnDraw) {
                throwOnDraw = false;
                throw new Error("Simulated drawImage error");
              }
            },
            clearRect: () => {
              clearCalls += 1;
            },
          }
        : null,
  };

  await withMockedGlobals(
    {
      VideoDecoder: MockDecoder,
      EncodedVideoChunk: MockEncodedVideoChunk,
    },
    () => {
      const dims: Array<[number, number]> = [];
      const errors: string[] = [];
      const keyframeReasons: RemoteKeyframeReason[] = [];
      let firstFrameCount = 0;

      const renderer = new RemoteH264Renderer({
        onDimensions: (w, h) => dims.push([w, h]),
        onError: (err) => errors.push(err),
        onFirstFrame: () => {
          firstFrameCount += 1;
        },
        onKeyframeNeeded: (reason) => keyframeReasons.push(reason),
      });
      renderer.attachCanvas(mockCanvas as unknown as HTMLCanvasElement);

      const deltaB64 = encodeBytesToBase64(annexB4(SAMPLE_PSLICE));
      const keyHighB64 = encodeBytesToBase64(
        annexB4(SAMPLE_SPS_HIGH, SAMPLE_PPS, SAMPLE_IDR),
      );
      const keyBaseB64 = encodeBytesToBase64(
        annexB4(SAMPLE_SPS_BASELINE, SAMPLE_PPS, SAMPLE_IDR),
      );

      // 1. Delta frames before first keyframe are gated/dropped
      renderer.pushFrame({ timestamp: 90000, is_keyframe: false, data_b64: deltaB64 });
      assert.equal(createdDecoders.length, 0);
      assert.equal(drawCalls, 0);
      assert.equal(firstFrameCount, 0);

      // 2. First keyframe configures exact High Profile SPS codec and draws
      renderer.pushFrame({
        timestamp: 96000,
        is_keyframe: true,
        just_synced: true,
        data_b64: keyHighB64,
      });
      assert.equal(createdDecoders.length, 1);
      const dec1 = createdDecoders[0];
      assert.equal(dec1.configuredCodecs.length, 1);
      assert.equal(dec1.configuredCodecs[0].codec, "avc1.64001F");
      assert.equal(dec1.configuredCodecs[0].optimizeForLatency, true);
      assert.equal(dec1.decodedChunks.length, 1);
      assert.equal(dec1.decodedChunks[0].type, "key");
      assert.equal(mockCanvas.width, 640);
      assert.equal(mockCanvas.height, 360);
      assert.deepEqual(dims, [[640, 360]]);
      assert.equal(firstFrameCount, 1);
      assert.equal(drawCalls, 1);
      assert.equal(emittedFrames[0].closed, true);

      // 3. Subsequent delta frame decodes and does not re-fire onFirstFrame;
      //    even if drawImage throws, VideoFrame.close() MUST run in finally.
      throwOnDraw = true;
      renderer.pushFrame({ timestamp: 102000, is_keyframe: false, data_b64: deltaB64 });
      assert.equal(dec1.decodedChunks.length, 2);
      assert.equal(dec1.decodedChunks[1].type, "delta");
      assert.equal(firstFrameCount, 1);
      assert.equal(emittedFrames[1].closed, true);
      assert.equal(errors.length, 1);

      // 4. Keyframe with a new SPS profile reconfigures decoder dynamically
      nextFrameWidth = 480;
      nextFrameHeight = 640;
      renderer.pushFrame({ timestamp: 108000, is_keyframe: true, data_b64: keyBaseB64 });
      assert.equal(dec1.configuredCodecs.length, 2);
      assert.equal(dec1.configuredCodecs[1].codec, "avc1.42C01E");
      assert.deepEqual(dims, [
        [640, 360],
        [480, 640],
      ]);
      assert.equal(emittedFrames[2].closed, true);

      // 5. Queue overflow (> 6) drops delta frame, resets decoder, requests keyframe, and gates until next keyframe
      dec1.decodeQueueSize = 7;
      renderer.pushFrame({ timestamp: 114000, is_keyframe: false, data_b64: deltaB64 });
      assert.equal(dec1.resetCount, 1);
      assert.equal(dec1.decodedChunks.length, 3); // delta was dropped!
      assert.deepEqual(keyframeReasons, ["overflow"]);

      // Next delta is also gated while awaiting keyframe
      renderer.pushFrame({ timestamp: 120000, is_keyframe: false, data_b64: deltaB64 });
      assert.equal(dec1.decodedChunks.length, 3);

      // Keyframe resumes decoding after overflow reset
      renderer.pushFrame({ timestamp: 126000, is_keyframe: true, data_b64: keyBaseB64 });
      assert.equal(dec1.decodedChunks.length, 4);

      // 6. Async decoder error recovery: discards closed decoder, requests keyframe, drops delta, recreates on next keyframe
      dec1.triggerAsyncError("Hardware decoder lost");
      assert.equal(errors.length, 2);
      assert.deepEqual(keyframeReasons, ["overflow", "decode_error"]);
      renderer.pushFrame({ timestamp: 132000, is_keyframe: false, data_b64: deltaB64 });
      assert.equal(createdDecoders.length, 1); // still gated

      renderer.pushFrame({ timestamp: 138000, is_keyframe: true, data_b64: keyHighB64 });
      assert.equal(createdDecoders.length, 2);
      const dec2 = createdDecoders[1];
      assert.equal(dec2.configuredCodecs[0].codec, "avc1.64001F");
      assert.equal(dec2.decodedChunks.length, 1);

      // 6b. Synchronous decode() throw also triggers onKeyframeNeeded("decode_error")
      throwOnNextDecode = true;
      renderer.pushFrame({ timestamp: 141000, is_keyframe: false, data_b64: deltaB64 });
      assert.equal(errors.length, 3);
      assert.deepEqual(keyframeReasons, ["overflow", "decode_error", "decode_error"]);
      renderer.pushFrame({ timestamp: 142000, is_keyframe: true, data_b64: keyHighB64 });
      assert.equal(createdDecoders.length, 3);
      const dec3Recovered = createdDecoders[2];

      // 7. reset() closes decoder, clears canvas, and resets onFirstFrame gate
      renderer.reset();
      assert.equal(dec3Recovered.closeCount, 1);
      assert.equal(clearCalls, 1);
      renderer.pushFrame({ timestamp: 144000, is_keyframe: false, data_b64: deltaB64 });
      assert.equal(createdDecoders.length, 3); // gated after reset

      renderer.pushFrame({ timestamp: 150000, is_keyframe: true, data_b64: keyBaseB64 });
      assert.equal(createdDecoders.length, 4);
      assert.equal(firstFrameCount, 2);

      // 8. destroy() closes decoder and closes any stale late VideoFrame without drawing
      const dec4 = createdDecoders[3];
      renderer.destroy();
      assert.equal(dec4.closeCount, 1);
      const staleFrame = new MockVideoFrame();
      const drawsBefore = drawCalls;
      dec4.init.output(staleFrame as unknown as VideoFrame);
      assert.equal(staleFrame.closed, true);
      assert.equal(drawCalls, drawsBefore);

      // Every VideoFrame ever emitted was closed exactly once
      for (const f of emittedFrames) {
        assert.equal(f.closeCount, 1);
      }
    },
  );
});

// ---------------------------------------------------------------------------
// 5. LocalCameraEncoder tests
// ---------------------------------------------------------------------------

test("LocalCameraEncoder reports unsupported, unavailable, denied, and error statuses cleanly", async () => {
  // Missing capabilities -> "unsupported"
  await withMockedGlobals(
    {
      VideoEncoder: undefined,
      navigator: undefined,
    },
    async () => {
      const statuses: LocalCameraStatus[] = [];
      const enc = new LocalCameraEncoder({
        onEncodedAu: () => undefined,
        onStatusChange: (s) => statuses.push(s),
      });
      const res = await enc.start();
      assert.equal(res, "unsupported");
      assert.deepEqual(statuses, ["unsupported"]);
      enc.destroy();
    },
  );

  // Error mappings for getUserMedia
  const errorCases: Array<{
    errName: string;
    devices: Array<{ kind: string; deviceId: string; label: string }>;
    expected: LocalCameraStatus;
  }> = [
    { errName: "NotFoundError", devices: [], expected: "unavailable" },
    { errName: "DevicesNotFoundError", devices: [], expected: "unavailable" },
    { errName: "OverconstrainedError", devices: [], expected: "unavailable" },
    {
      errName: "OverconstrainedError",
      devices: [{ kind: "videoinput", deviceId: "cam-1", label: "Cam" }],
      expected: "error",
    },
    { errName: "NotAllowedError", devices: [], expected: "denied" },
    { errName: "PermissionDeniedError", devices: [], expected: "denied" },
    { errName: "NotReadableError", devices: [], expected: "error" },
  ];

  for (const tc of errorCases) {
    await withMockedGlobals(
      {
        VideoEncoder: class {},
        navigator: {
          mediaDevices: {
            getUserMedia: async () => {
              const err = new Error(`Mock ${tc.errName}`);
              err.name = tc.errName;
              throw err;
            },
            enumerateDevices: async () => tc.devices,
          },
        },
      },
      async () => {
        const statuses: LocalCameraStatus[] = [];
        const enc = new LocalCameraEncoder({
          onEncodedAu: () => undefined,
          onStatusChange: (s) => statuses.push(s),
        });
        const res = await enc.start();
        assert.equal(res, tc.expected, `Expected ${tc.errName} -> ${tc.expected}`);
        assert.deepEqual(statuses, ["starting", tc.expected]);
        enc.destroy();
      },
    );
  }
});

test("LocalCameraEncoder negotiates H.264 codec, handles PLI requestKeyframe, normalizes AVCC+avcC, closes VideoFrames, and cleanly stops tracks", async () => {
  const createdEncoders: MockVideoEncoder[] = [];
  const createdFrames: MockVideoFrame[] = [];
  const avcC = buildAvcC(SAMPLE_SPS_BASELINE, SAMPLE_PPS, 3);
  let capturedConstraints: MediaStreamConstraints | null = null;
  let trackStoppedCount = 0;
  let videoElRef: {
    srcObject: unknown;
    readyState: number;
    muted: boolean;
    playsInline: boolean;
    autoplay: boolean;
    play: () => Promise<void>;
    pause: () => void;
    paused: boolean;
  } | null = null;

  class MockVideoEncoder {
    state: CodecState = "unconfigured";
    encodeQueueSize = 0;
    configuredConfig: VideoEncoderConfig | null = null;
    encodeCalls: Array<{ keyFrame: boolean; timestamp: number }> = [];
    closeCount = 0;
    readonly init: VideoEncoderInit;

    static async isConfigSupported(cfg: VideoEncoderConfig): Promise<VideoEncoderSupport> {
      // Reject avc1.42E01E to verify fallback to avc1.42001E
      return {
        config: cfg,
        supported: cfg.codec === "avc1.42001E" && cfg.avc?.format === "annexb",
      };
    }

    constructor(init: VideoEncoderInit) {
      this.init = init;
      createdEncoders.push(this);
    }

    configure(cfg: VideoEncoderConfig): void {
      this.configuredConfig = { ...cfg };
      this.state = "configured";
    }

    encode(frame: MockVideoFrame, options?: VideoEncoderEncodeOptions): void {
      const isKey = Boolean(options?.keyFrame);
      this.encodeCalls.push({ keyFrame: isKey, timestamp: frame.timestamp });
      // Emit AVCC payload; provide decoderConfig.description ONLY on the very first keyframe
      // to verify avcDescription caching for subsequent PLI keyframes!
      const payload = isKey
        ? buildAvccPayload([SAMPLE_IDR], 4)
        : buildAvccPayload([SAMPLE_PSLICE], 4);
      const chunk = new MockEncodedVideoChunk({
        type: isKey ? "key" : "delta",
        timestamp: frame.timestamp,
        data: payload,
      });
      const metadata: EncodedVideoChunkMetadata | undefined =
        this.encodeCalls.length === 1
          ? { decoderConfig: { codec: "avc1.42001E", description: avcC } }
          : undefined;
      this.init.output(chunk as unknown as EncodedVideoChunk, metadata);
    }

    close(): void {
      this.closeCount += 1;
      this.state = "closed";
    }
  }

  class MockCaptureVideoFrame extends MockVideoFrame {
    constructor(source: unknown, init?: { timestamp?: number }) {
      super(source, init);
      createdFrames.push(this);
    }
  }

  const fakeTrack = {
    kind: "video",
    getSettings: () => ({ width: 1280, height: 720 }),
    stop: () => {
      trackStoppedCount += 1;
    },
  };
  const fakeStream = {
    getTracks: () => [fakeTrack],
    getVideoTracks: () => [fakeTrack],
  };

  await withMockedGlobals(
    {
      VideoEncoder: MockVideoEncoder,
      VideoFrame: MockCaptureVideoFrame,
      document: {
        createElement: (tag: string) => {
          assert.equal(tag, "video");
          videoElRef = {
            srcObject: null,
            readyState: 4,
            muted: false,
            playsInline: false,
            autoplay: false,
            paused: false,
            play: async () => undefined,
            pause: () => {
              if (videoElRef) videoElRef.paused = true;
            },
          };
          return videoElRef;
        },
      },
      navigator: {
        mediaDevices: {
          getUserMedia: async (constraints: MediaStreamConstraints) => {
            capturedConstraints = constraints;
            return fakeStream;
          },
        },
      },
    },
    async () => {
      const encodedAus: Array<{ bytes: Uint8Array; isKeyFrame: boolean }> = [];
      const statuses: LocalCameraStatus[] = [];
      const streams: Array<MediaStream | null> = [];

      const encoder = new LocalCameraEncoder({
        targetWidth: 640,
        targetHeight: 480,
        targetFps: 50, // 20ms interval for fast deterministic testing
        bitrateBps: 450_000,
        onEncodedAu: (b64, isKeyFrame) => {
          encodedAus.push({ bytes: decodeBase64ToBytes(b64), isKeyFrame });
        },
        onStatusChange: (status) => statuses.push(status),
        onStreamChange: (stream) => streams.push(stream),
      });

      const startStatus = await encoder.start("cam-usb-1");
      assert.equal(startStatus, "active");
      assert.deepEqual(statuses, ["starting", "active"]);
      assert.equal(streams.length, 1);
      assert.equal(streams[0], fakeStream as unknown as MediaStream);

      // Verify exact constraints (including exact deviceId & audio: false)
      assert.equal(capturedConstraints?.audio, false);
      const vConstraints = capturedConstraints?.video as MediaTrackConstraints;
      assert.deepEqual(vConstraints.deviceId, { exact: "cam-usb-1" });

      // Verify codec fallback + aspect-preserving even dimensions (1280x720 scaled into 640x480 -> 640x360)
      assert.equal(createdEncoders.length, 1);
      const mockEnc = createdEncoders[0];
      assert.equal(mockEnc.configuredConfig?.codec, "avc1.42001E");
      assert.equal(mockEnc.configuredConfig?.width, 640);
      assert.equal(mockEnc.configuredConfig?.height, 360);
      assert.equal(mockEnc.configuredConfig?.bitrate, 450_000);
      assert.equal(mockEnc.configuredConfig?.latencyMode, "realtime");
      assert.equal(mockEnc.configuredConfig?.bitrateMode, "variable");

      // Wait for at least 2 frames (frame 0 = keyframe, frame 1 = delta)
      const deadline1 = Date.now() + 1000;
      while (encodedAus.length < 2 && Date.now() < deadline1) {
        await delay(10);
      }
      assert.ok(encodedAus.length >= 2, `Expected >=2 frames, got ${encodedAus.length}`);
      assert.equal(encodedAus[0].isKeyFrame, true);
      assert.deepEqual(
        encodedAus[0].bytes,
        annexB4(SAMPLE_SPS_BASELINE, SAMPLE_PPS, SAMPLE_IDR),
      );
      assert.equal(encodedAus[1].isKeyFrame, false);
      assert.deepEqual(encodedAus[1].bytes, annexB4(SAMPLE_PSLICE));

      // Trigger PLI requestKeyframe() and verify the next encoded frame is a keyframe with cached SPS+PPS!
      const countBeforePli = encodedAus.length;
      encoder.requestKeyframe();
      const deadline2 = Date.now() + 1000;
      while (encodedAus.length <= countBeforePli && Date.now() < deadline2) {
        await delay(10);
      }
      const pliFrame = encodedAus[countBeforePli];
      assert.ok(pliFrame, "Expected PLI keyframe to be emitted");
      assert.equal(pliFrame.isKeyFrame, true);
      assert.deepEqual(
        pliFrame.bytes,
        annexB4(SAMPLE_SPS_BASELINE, SAMPLE_PPS, SAMPLE_IDR),
      );

      // Backpressure guard: encodeQueueSize > 3 skips encoding
      mockEnc.encodeQueueSize = 4;
      const callsWhileBackpressured = mockEnc.encodeCalls.length;
      await delay(50);
      assert.equal(mockEnc.encodeCalls.length, callsWhileBackpressured);
      mockEnc.encodeQueueSize = 0;

      // readyState < 2 guard skips encoding until video element has frame data
      if (videoElRef) {
        videoElRef.readyState = 1;
      }
      const callsWhileNotReady = mockEnc.encodeCalls.length;
      await delay(50);
      assert.equal(mockEnc.encodeCalls.length, callsWhileNotReady);
      if (videoElRef) {
        videoElRef.readyState = 4;
      }

      // Stop cleanly: stops tracks, cancels timer, clears video.srcObject, closes encoder, emits stream(null) & "off"
      encoder.stop();
      assert.equal(trackStoppedCount, 1);
      assert.equal(videoElRef?.srcObject, null);
      assert.equal(videoElRef?.paused, true);
      assert.equal(mockEnc.closeCount, 1);
      assert.equal(streams[streams.length - 1], null);
      assert.equal(statuses[statuses.length - 1], "off");

      // Every created VideoFrame must have been closed in finally
      assert.ok(createdFrames.length >= 3);
      for (const vf of createdFrames) {
        assert.equal(vf.closeCount, 1);
      }

      // No post-stop callbacks (neither timer ticks nor late VideoEncoder output/error callbacks)
      const auCountAfterStop = encodedAus.length;
      const statusCountAfterStop = statuses.length;
      await delay(50);
      mockEnc.init.output(
        new MockEncodedVideoChunk({
          type: "key",
          timestamp: 999999,
          data: buildAvccPayload([SAMPLE_IDR], 4),
        }) as unknown as EncodedVideoChunk,
      );
      mockEnc.init.error(new Error("late error") as unknown as DOMException);
      assert.equal(encodedAus.length, auCountAfterStop);
      assert.equal(statuses.length, statusCountAfterStop);

      encoder.destroy();
    },
  );
});

test("LocalCameraEncoder stops late-resolving MediaStream tracks if stop() is called while getUserMedia is pending", async () => {
  let resolveGum: ((stream: unknown) => void) | null = null;
  let stoppedLateTrack = 0;

  await withMockedGlobals(
    {
      VideoEncoder: class {},
      navigator: {
        mediaDevices: {
          getUserMedia: () =>
            new Promise((resolve) => {
              resolveGum = resolve;
            }),
        },
      },
    },
    async () => {
      const streamEvents: Array<MediaStream | null> = [];
      const enc = new LocalCameraEncoder({
        onEncodedAu: () => undefined,
        onStreamChange: (s) => streamEvents.push(s),
      });

      const startPromise = enc.start();
      await delay(5);
      enc.stop();

      const gumResolver = resolveGum as ((stream: unknown) => void) | null;
      assert.ok(gumResolver, "getUserMedia should have been invoked");
      if (!gumResolver) {
        throw new Error("Expected getUserMedia resolver");
      }
      gumResolver({
        getTracks: () => [
          {
            stop: () => {
              stoppedLateTrack += 1;
            },
          },
        ],
      });

      await startPromise;
      assert.equal(stoppedLateTrack, 1);
      assert.deepEqual(streamEvents, []);
      enc.destroy();
    },
  );
});

test("LocalCameraEncoder in AVCC fallback mode preserves 261-byte (0x00 00 01 05) and 300-byte frames without corruption", async () => {
  const avcC = buildAvcC(SAMPLE_SPS_BASELINE, SAMPLE_PPS, 3);
  const pSlice261 = new Uint8Array(261);
  pSlice261[0] = 0x41;
  for (let i = 1; i < pSlice261.byteLength; i += 1) pSlice261[i] = (i * 7 + 1) & 0xff;
  const pSlice300 = new Uint8Array(300);
  pSlice300[0] = 0x41;
  for (let i = 1; i < pSlice300.byteLength; i += 1) pSlice300[i] = (i * 11 + 5) & 0xff;

  let encodeIdx = 0;

  class MockAvccOnlyVideoEncoder {
    state: CodecState = "unconfigured";
    encodeQueueSize = 0;
    readonly init: VideoEncoderInit;

    static async isConfigSupported(cfg: VideoEncoderConfig): Promise<VideoEncoderSupport> {
      // Reject annexb configs so LocalCameraEncoder falls back to AVCC mode
      return {
        config: cfg,
        supported: !cfg.avc && cfg.codec === "avc1.42E01E",
      };
    }

    constructor(init: VideoEncoderInit) {
      this.init = init;
    }

    configure(): void {
      this.state = "configured";
    }

    encode(frame: MockVideoFrame, options?: VideoEncoderEncodeOptions): void {
      const idx = encodeIdx++;
      const isKey = Boolean(options?.keyFrame) || idx === 0;
      const payload =
        idx === 0
          ? buildAvccPayload([SAMPLE_IDR], 4)
          : idx === 1
            ? buildAvccPayload([pSlice261], 4)
            : buildAvccPayload([pSlice300], 4);
      const chunk = new MockEncodedVideoChunk({
        type: isKey ? "key" : "delta",
        timestamp: frame.timestamp,
        data: payload,
      });
      // Intentionally omit decoderConfig.description on delta frames to ensure
      // LocalCameraEncoder's stored avcFormat = "avc" handles 261B/300B chunks even without per-chunk metadata
      const metadata: EncodedVideoChunkMetadata | undefined =
        idx === 0
          ? { decoderConfig: { codec: "avc1.42E01E", description: avcC } }
          : undefined;
      this.init.output(chunk as unknown as EncodedVideoChunk, metadata);
    }

    close(): void {
      this.state = "closed";
    }
  }

  const fakeTrack = {
    kind: "video",
    readyState: "live",
    getSettings: () => ({ width: 640, height: 480 }),
    stop: () => undefined,
  };
  const fakeStream = {
    getTracks: () => [fakeTrack],
    getVideoTracks: () => [fakeTrack],
  };

  await withMockedGlobals(
    {
      VideoEncoder: MockAvccOnlyVideoEncoder,
      VideoFrame: MockVideoFrame,
      navigator: {
        mediaDevices: {
          getUserMedia: async () => fakeStream,
        },
      },
    },
    async () => {
      const encodedAus: Array<{ bytes: Uint8Array; isKeyFrame: boolean }> = [];
      const enc = new LocalCameraEncoder({
        targetFps: 50,
        onEncodedAu: (b64, isKeyFrame) => {
          encodedAus.push({ bytes: decodeBase64ToBytes(b64), isKeyFrame });
        },
      });
      const status = await enc.start();
      assert.equal(status, "active");

      const deadline = Date.now() + 1000;
      while (encodedAus.length < 3 && Date.now() < deadline) {
        await delay(10);
      }
      enc.stop();
      enc.destroy();

      assert.ok(encodedAus.length >= 3, `Expected >=3 encoded AUs, got ${encodedAus.length}`);
      assert.deepEqual(
        encodedAus[0].bytes,
        annexB4(SAMPLE_SPS_BASELINE, SAMPLE_PPS, SAMPLE_IDR),
      );
      assert.deepEqual(encodedAus[1].bytes, annexB4(pSlice261));
      assert.deepEqual(encodedAus[2].bytes, annexB4(pSlice300));
    },
  );
});

test("LocalCameraEncoder stops cleanly and reports unavailable when video track fires ended or becomes non-live", async () => {
  for (const mode of ["event", "readyState"] as const) {
    let encoderCloseCount = 0;
    let addedEndedListener: (() => void) | null = null;
    let removedEndedListener = false;

    class MockEncoder {
      state: CodecState = "unconfigured";
      encodeQueueSize = 0;
      static async isConfigSupported(cfg: VideoEncoderConfig): Promise<VideoEncoderSupport> {
        return { config: cfg, supported: true };
      }
      configure(): void {
        this.state = "configured";
      }
      encode(): void {
        // no-op
      }
      close(): void {
        encoderCloseCount += 1;
        this.state = "closed";
      }
    }

    const fakeTrack = {
      kind: "video",
      readyState: "live" as "live" | "ended",
      getSettings: () => ({ width: 640, height: 480 }),
      addEventListener: (ev: string, fn: () => void) => {
        if (ev === "ended") addedEndedListener = fn;
      },
      removeEventListener: (ev: string, fn: () => void) => {
        if (ev === "ended" && fn === addedEndedListener) {
          removedEndedListener = true;
        }
      },
      stop: () => undefined,
    };
    const fakeStream = {
      getTracks: () => [fakeTrack],
      getVideoTracks: () => [fakeTrack],
    };

    await withMockedGlobals(
      {
        VideoEncoder: MockEncoder,
        VideoFrame: MockVideoFrame,
        navigator: {
          mediaDevices: {
            getUserMedia: async () => fakeStream,
          },
        },
      },
      async () => {
        const statuses: LocalCameraStatus[] = [];
        const streams: Array<MediaStream | null> = [];
        const enc = new LocalCameraEncoder({
          targetFps: 50,
          onEncodedAu: () => undefined,
          onStatusChange: (s) => statuses.push(s),
          onStreamChange: (s) => streams.push(s),
        });

        assert.equal(await enc.start(), "active");
        assert.ok(addedEndedListener, "ended listener should be attached");

        if (mode === "event") {
          fakeTrack.readyState = "ended";
          const listener = addedEndedListener as (() => void) | null;
          if (!listener) throw new Error("Missing ended listener");
          listener();
        } else {
          fakeTrack.readyState = "ended";
          const deadline = Date.now() + 1000;
          while (statuses[statuses.length - 1] !== "unavailable" && Date.now() < deadline) {
            await delay(10);
          }
        }

        assert.equal(statuses[statuses.length - 1], "unavailable", `mode=${mode}`);
        assert.equal(streams[streams.length - 1], null, `mode=${mode}`);
        assert.equal(encoderCloseCount, 1, `mode=${mode}`);
        assert.equal(removedEndedListener, true, `mode=${mode}`);
        enc.destroy();
      },
    );
  }
});

test("RemoteH264Renderer.pushFrame skips base64 decoding for dropped delta frames while waitingForKeyframe or overflowing", async () => {
  const realAtob = globalThis.atob;
  let atobCalls = 0;
  const createdDecoders: Array<{
    state: CodecState;
    decodeQueueSize: number;
    decodedCount: number;
    resetCount: number;
  }> = [];

  class SpyDecoder {
    state: CodecState = "unconfigured";
    decodeQueueSize = 0;
    decodedCount = 0;
    resetCount = 0;
    readonly init: VideoDecoderInit;

    constructor(init: VideoDecoderInit) {
      this.init = init;
      createdDecoders.push(this);
    }

    configure(): void {
      this.state = "configured";
    }

    decode(): void {
      this.decodedCount += 1;
    }

    reset(): void {
      this.resetCount += 1;
      this.decodeQueueSize = 0;
      this.state = "unconfigured";
    }

    close(): void {
      this.state = "closed";
    }
  }

  await withMockedGlobals(
    {
      VideoDecoder: SpyDecoder,
      EncodedVideoChunk: MockEncodedVideoChunk,
      atob: (input: string) => {
        atobCalls += 1;
        return realAtob(input);
      },
    },
    () => {
      const keyframeReasons: RemoteKeyframeReason[] = [];
      const errors: string[] = [];
      const renderer = new RemoteH264Renderer({
        onError: (err) => errors.push(err),
        onKeyframeNeeded: (reason) => keyframeReasons.push(reason),
      });

      const deltaB64 = encodeBytesToBase64(annexB4(SAMPLE_PSLICE));
      const keyB64 = encodeBytesToBase64(
        annexB4(SAMPLE_SPS_BASELINE, SAMPLE_PPS, SAMPLE_IDR),
      );

      // 1. Delta frames while waitingForKeyframe MUST skip base64 decoding entirely
      renderer.pushFrame({ timestamp: 3000, is_keyframe: false, data_b64: deltaB64 });
      renderer.pushFrame({ timestamp: 6000, is_keyframe: false, data_b64: "!!!invalid-base64!!!" });
      assert.equal(atobCalls, 0);
      assert.equal(errors.length, 0);
      assert.equal(createdDecoders.length, 0);

      // 2. Keyframe decodes base64 once and unlocks delta decoding
      renderer.pushFrame({ timestamp: 9000, is_keyframe: true, data_b64: keyB64 });
      assert.equal(atobCalls, 1);
      assert.equal(createdDecoders.length, 1);
      assert.equal(createdDecoders[0].decodedCount, 1);

      // 3. Normal delta after sync decodes base64
      renderer.pushFrame({ timestamp: 12000, is_keyframe: false, data_b64: deltaB64 });
      assert.equal(atobCalls, 2);
      assert.equal(createdDecoders[0].decodedCount, 2);

      // 4. Queue overflow drop on delta MUST reset decoder & request keyframe BEFORE base64 decoding
      createdDecoders[0].decodeQueueSize = 7;
      renderer.pushFrame({ timestamp: 15000, is_keyframe: false, data_b64: deltaB64 });
      assert.equal(atobCalls, 2);
      assert.equal(createdDecoders[0].resetCount, 1);
      assert.deepEqual(keyframeReasons, ["overflow"]);

      // 5. Subsequent delta while re-gated also skips base64 decoding
      renderer.pushFrame({ timestamp: 18000, is_keyframe: false, data_b64: deltaB64 });
      assert.equal(atobCalls, 2);

      renderer.destroy();
    },
  );
});

test("LocalCameraEncoder waits for video metadata and falls back to videoEl dimensions when track.getSettings omits size", async () => {
  let configuredConfig: VideoEncoderConfig | null = null;
  let removedMetaListeners = 0;

  class MockAspectEncoder {
    state: CodecState = "unconfigured";
    encodeQueueSize = 0;

    static async isConfigSupported(cfg: VideoEncoderConfig): Promise<VideoEncoderSupport> {
      return { config: cfg, supported: true };
    }

    configure(cfg: VideoEncoderConfig): void {
      configuredConfig = { ...cfg };
      this.state = "configured";
    }

    encode(): void {
      // no-op
    }

    close(): void {
      this.state = "closed";
    }
  }

  const fakeTrack = {
    kind: "video",
    readyState: "live",
    // Intentionally omit width & height to test fallback to videoEl.videoWidth/videoHeight
    getSettings: () => ({}),
    stop: () => undefined,
  };
  const fakeStream = {
    getTracks: () => [fakeTrack],
    getVideoTracks: () => [fakeTrack],
  };

  await withMockedGlobals(
    {
      VideoEncoder: MockAspectEncoder,
      VideoFrame: MockVideoFrame,
      document: {
        createElement: (tag: string) => {
          assert.equal(tag, "video");
          const listeners = new Map<string, Set<() => void>>();
          const el = {
            srcObject: null as unknown,
            readyState: 0,
            videoWidth: 0,
            videoHeight: 0,
            muted: false,
            playsInline: false,
            autoplay: false,
            addEventListener: (ev: string, fn: () => void) => {
              const set = listeners.get(ev) ?? new Set();
              set.add(fn);
              listeners.set(ev, set);
            },
            removeEventListener: (ev: string, fn: () => void) => {
              const set = listeners.get(ev);
              if (set?.delete(fn)) {
                removedMetaListeners += 1;
              }
            },
            play: async () => {
              // Simulate async loadedmetadata delivering 16:9 1280x720 intrinsic dimensions
              setTimeout(() => {
                el.videoWidth = 1280;
                el.videoHeight = 720;
                el.readyState = 4;
                for (const fn of listeners.get("loadedmetadata") ?? []) {
                  fn();
                }
              }, 10);
            },
            pause: () => undefined,
          };
          return el;
        },
      },
      navigator: {
        mediaDevices: {
          getUserMedia: async () => fakeStream,
        },
      },
    },
    async () => {
      const enc = new LocalCameraEncoder({
        targetWidth: 640,
        targetHeight: 480,
        targetFps: 15,
        onEncodedAu: () => undefined,
      });

      const status = await enc.start();
      assert.equal(status, "active");
      const cfg = configuredConfig as VideoEncoderConfig | null;
      assert.ok(cfg, "VideoEncoder should have been configured");
      // 1280x720 scaled into 640x480 must preserve 16:9 aspect ratio -> 640x360 (not 640x480!)
      assert.equal(cfg.width, 640);
      assert.equal(cfg.height, 360);
      assert.ok(removedMetaListeners >= 1, "metadata listeners must be cleaned up");

      enc.stop();
      enc.destroy();
    },
  );
});

test("RemoteH264Renderer caches last decoded frame and repaints immediately on canvas reattach", async () => {
  class FakeOffscreenCanvas {
    width: number;
    height: number;
    drawCount = 0;
    clearCount = 0;
    lastSource: unknown = null;

    constructor(width: number, height: number) {
      this.width = width;
      this.height = height;
    }

    getContext(kind: string) {
      if (kind !== "2d") return null;
      return {
        drawImage: (source: unknown) => {
          this.drawCount += 1;
          this.lastSource = source;
        },
        clearRect: () => {
          this.clearCount += 1;
        },
      };
    }
  }

  class ReattachMockDecoder {
    state: CodecState = "unconfigured";
    decodeQueueSize = 0;
    readonly init: VideoDecoderInit;

    constructor(init: VideoDecoderInit) {
      this.init = init;
    }

    configure(): void {
      this.state = "configured";
    }

    decode(chunk: MockEncodedVideoChunk): void {
      const vf = new MockVideoFrame(null, {
        timestamp: chunk.timestamp,
        displayWidth: 800,
        displayHeight: 450,
      });
      this.init.output(vf as unknown as VideoFrame);
    }

    reset(): void {
      this.state = "unconfigured";
    }

    close(): void {
      this.state = "closed";
    }
  }

  const makeMockCanvas = () => {
    const draws: Array<{ source: unknown; w: number; h: number }> = [];
    const canvas = {
      width: 0,
      height: 0,
      getContext: (kind: string) =>
        kind === "2d"
          ? {
              drawImage: (source: unknown, _x: number, _y: number, w: number, h: number) => {
                draws.push({ source, w, h });
              },
              clearRect: () => undefined,
            }
          : null,
    };
    return { canvas, draws };
  };

  await withMockedGlobals(
    {
      VideoDecoder: ReattachMockDecoder,
      EncodedVideoChunk: MockEncodedVideoChunk,
      OffscreenCanvas: FakeOffscreenCanvas,
    },
    () => {
      const keyframeReasons: RemoteKeyframeReason[] = [];
      const renderer = new RemoteH264Renderer({
        onKeyframeNeeded: (reason) => keyframeReasons.push(reason),
      });

      const first = makeMockCanvas();
      renderer.attachCanvas(first.canvas as unknown as HTMLCanvasElement);
      // Initial attach before any frame must not trigger keyframe request
      assert.deepEqual(keyframeReasons, []);

      const keyB64 = encodeBytesToBase64(
        annexB4(SAMPLE_SPS_BASELINE, SAMPLE_PPS, SAMPLE_IDR),
      );
      renderer.pushFrame({ timestamp: 90000, is_keyframe: true, data_b64: keyB64 });
      assert.equal(first.draws.length, 1);

      // Detach canvas (e.g. remount) and attach a fresh canvas -> immediate cached repaint + PLI request
      renderer.attachCanvas(null);
      const second = makeMockCanvas();
      renderer.attachCanvas(second.canvas as unknown as HTMLCanvasElement);
      assert.equal(second.canvas.width, 800);
      assert.equal(second.canvas.height, 450);
      assert.equal(second.draws.length, 1);
      assert.ok(second.draws[0].source instanceof FakeOffscreenCanvas);
      assert.deepEqual(keyframeReasons, ["reattach"]);

      // After reset(), cached frame is cleared so reattaching does not repaint stale frame
      renderer.reset();
      renderer.attachCanvas(null);
      const third = makeMockCanvas();
      renderer.attachCanvas(third.canvas as unknown as HTMLCanvasElement);
      assert.equal(third.draws.length, 0);
      assert.deepEqual(keyframeReasons, ["reattach"]);

      renderer.destroy();
    },
  );
});
