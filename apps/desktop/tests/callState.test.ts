import test from "node:test";
import assert from "node:assert/strict";
import {
  canStartVoiceCall,
  classifyCallEndReason,
  formatCallDuration,
  isCameraFailureStatus,
  normalizeLevelPercent,
  resolveEffectiveCallDevices,
  shouldCaptureLocalVideo,
  shouldShowVideoOverlay,
} from "../src/utils/callState.ts";
import type { CallSnapshotDto } from "../src/types.ts";

test("formatCallDuration formats mm:ss and h:mm:ss cleanly", () => {
  assert.equal(formatCallDuration(0), "00:00");
  assert.equal(formatCallDuration(-5), "00:00");
  assert.equal(formatCallDuration(9.8), "00:09");
  assert.equal(formatCallDuration(134), "02:14");
  assert.equal(formatCallDuration(3849), "1:04:09");
});

test("canStartVoiceCall enforces direct-only, auth, idle phase, and strict relay exclusion", () => {
  assert.deepEqual(
    canStartVoiceCall({
      threadKind: "direct",
      threadId: "2000",
      myUid: "1000",
      relayEnabled: false,
      callPhase: "idle",
      authed: true,
    }),
    { allowed: true, disabledReason: null },
  );

  // Relay enabled strictly disables calling
  assert.deepEqual(
    canStartVoiceCall({
      threadKind: "direct",
      threadId: "2000",
      myUid: "1000",
      relayEnabled: true,
      callPhase: "idle",
      authed: true,
    }),
    { allowed: false, disabledReason: "relay_enabled" },
  );

  // Busy call disables initiating another call
  assert.deepEqual(
    canStartVoiceCall({
      threadKind: "direct",
      threadId: "2000",
      myUid: "1000",
      relayEnabled: false,
      callPhase: "connected",
      authed: true,
    }),
    { allowed: false, disabledReason: "busy" },
  );

  // Group, cloud, or self threads are rejected
  assert.deepEqual(
    canStartVoiceCall({
      threadKind: "group",
      threadId: "g-1",
      myUid: "1000",
      relayEnabled: false,
      callPhase: "idle",
      authed: true,
    }),
    { allowed: false, disabledReason: "unsupported_thread" },
  );
  assert.deepEqual(
    canStartVoiceCall({
      threadKind: "cloud",
      threadId: "cloud-me",
      myUid: "1000",
      relayEnabled: false,
      callPhase: "idle",
      authed: true,
    }),
    { allowed: false, disabledReason: "unsupported_thread" },
  );
  assert.deepEqual(
    canStartVoiceCall({
      threadKind: "direct",
      threadId: "1000",
      myUid: "1000",
      relayEnabled: false,
      callPhase: "idle",
      authed: true,
    }),
    { allowed: false, disabledReason: "unsupported_thread" },
  );

  assert.deepEqual(
    canStartVoiceCall({
      threadKind: "direct",
      threadId: "2000",
      myUid: "1000",
      relayEnabled: false,
      callPhase: "idle",
      authed: false,
    }),
    { allowed: false, disabledReason: "not_authenticated" },
  );
});

test("classifyCallEndReason maps unit and tagged Rust serde variants", () => {
  assert.deepEqual(classifyCallEndReason("local_declined"), {
    kind: "local_declined",
    isSilent: true,
  });
  assert.deepEqual(classifyCallEndReason("local_cancelled"), {
    kind: "local_cancelled",
    isSilent: true,
  });
  assert.deepEqual(classifyCallEndReason("answered_on_other_device"), {
    kind: "answered_on_other_device",
    isSilent: false,
  });
  assert.deepEqual(
    classifyCallEndReason({ remote_rejected: { code: 3, message: "Bận" } }),
    {
      kind: "remote_rejected",
      detailMessage: "Bận",
      isSilent: false,
    },
  );
  assert.deepEqual(classifyCallEndReason({ error: "UDP relay error" }), {
    kind: "error",
    detailMessage: "UDP relay error",
    isSilent: false,
  });
});

test("normalizeLevelPercent clamps permille meters to 0..100", () => {
  assert.equal(normalizeLevelPercent(-10), 0);
  assert.equal(normalizeLevelPercent(456), 46);
  assert.equal(normalizeLevelPercent(1500), 100);
});

test("resolveEffectiveCallDevices falls back to S/PDIF physical output when OS default is virtual", () => {
  const info = resolveEffectiveCallDevices({
    inputs: [
      { name: "CABLE Output (VB-Audio Virtual Cable)", is_default: true, is_virtual: true },
      { name: "Stereo Mix (Realtek(R) Audio)", is_default: false, is_virtual: true },
    ],
    outputs: [
      { name: "CABLE Input (VB-Audio Virtual Cable)", is_default: true, is_virtual: true },
      { name: "Realtek Digital Output (Realtek(R) Audio)", is_default: false, is_virtual: false },
    ],
    selected_input: null,
    selected_output: null,
  });

  assert.equal(info.hasPhysicalInput, false);
  assert.equal(info.hasPhysicalOutput, true);
  assert.equal(info.inputIsVirtual, true);
  assert.equal(info.outputIsVirtual, false);
  assert.equal(info.effectiveInput?.name, "CABLE Output (VB-Audio Virtual Cable)");
  assert.equal(info.effectiveOutput?.name, "Realtek Digital Output (Realtek(R) Audio)");
});

test("resolveEffectiveCallDevices flags virtual-only PC when no physical output exists", () => {
  const info = resolveEffectiveCallDevices({
    inputs: [
      { name: "CABLE Output (VB-Audio Virtual Cable)", is_default: true, is_virtual: true },
    ],
    outputs: [
      { name: "CABLE Input (VB-Audio Virtual Cable)", is_default: true, is_virtual: true },
      { name: "Stereo Mix (Realtek(R) Audio)", is_default: false, is_virtual: true },
    ],
    selected_input: null,
    selected_output: null,
  });

  assert.equal(info.hasPhysicalInput, false);
  assert.equal(info.hasPhysicalOutput, false);
  assert.equal(info.inputIsVirtual, true);
  assert.equal(info.outputIsVirtual, true);
  assert.equal(info.effectiveInput?.name, "CABLE Output (VB-Audio Virtual Cable)");
  assert.equal(info.effectiveOutput?.name, "CABLE Input (VB-Audio Virtual Cable)");
});

test("resolveEffectiveCallDevices prefers analog physical headset over S/PDIF when OS default is VB-Audio", () => {
  const info = resolveEffectiveCallDevices({
    inputs: [
      { name: "CABLE Output (VB-Audio Virtual Cable)", is_default: true, is_virtual: true },
      { name: "Headset Microphone (USB Audio)", is_default: false, is_virtual: false },
    ],
    outputs: [
      { name: "CABLE Input (VB-Audio Virtual Cable)", is_default: true, is_virtual: true },
      { name: "Realtek Digital Output (Realtek(R) Audio)", is_default: false, is_virtual: false },
      { name: "Headset Earphone (USB Audio)", is_default: false, is_virtual: false },
    ],
    selected_input: null,
    selected_output: null,
  });

  assert.equal(info.hasPhysicalInput, true);
  assert.equal(info.hasPhysicalOutput, true);
  assert.equal(info.inputIsVirtual, false);
  assert.equal(info.outputIsVirtual, false);
  assert.equal(info.effectiveInput?.name, "Headset Microphone (USB Audio)");
  assert.equal(info.effectiveOutput?.name, "Headset Earphone (USB Audio)");
});

test("resolveEffectiveCallDevices preserves explicit user selection of virtual device", () => {
  const info = resolveEffectiveCallDevices({
    inputs: [
      { name: "CABLE Output (VB-Audio Virtual Cable)", is_default: true, is_virtual: true },
      { name: "Headset Microphone (USB Audio)", is_default: false, is_virtual: false },
    ],
    outputs: [
      { name: "CABLE Input (VB-Audio Virtual Cable)", is_default: true, is_virtual: true },
      { name: "Headset Earphone (USB Audio)", is_default: false, is_virtual: false },
    ],
    selected_input: "CABLE Output (VB-Audio Virtual Cable)",
    selected_output: "CABLE Input (VB-Audio Virtual Cable)",
  });

  assert.equal(info.inputIsVirtual, true);
  assert.equal(info.outputIsVirtual, true);
  assert.equal(info.effectiveInput?.name, "CABLE Output (VB-Audio Virtual Cable)");
  assert.equal(info.effectiveOutput?.name, "CABLE Input (VB-Audio Virtual Cable)");
});

test("video call overlay and local capture guards behave cleanly across phases", () => {
  const baseSnap: CallSnapshotDto = {
    call_id: 42,
    partner_id: "2000",
    partner_name: "Alice",
    partner_avatar: "",
    call_type: "DirectVideo",
    is_caller: true,
    phase: "dialing",
    mic_muted: false,
    peer_mic_muted: false,
    cam_enabled: true,
    peer_cam_enabled: true,
    duration_secs: 0,
  };

  assert.equal(shouldShowVideoOverlay(null, false), false);
  assert.equal(shouldShowVideoOverlay(baseSnap, false), true);
  assert.equal(shouldShowVideoOverlay(baseSnap, true), false);
  assert.equal(
    shouldShowVideoOverlay({ ...baseSnap, phase: "incoming_ringing" }, false),
    false,
  );
  assert.equal(
    shouldShowVideoOverlay({ ...baseSnap, call_type: "DirectVoice", phase: "connected" }, false),
    false,
  );

  assert.equal(shouldCaptureLocalVideo(baseSnap), false);
  assert.equal(shouldCaptureLocalVideo({ ...baseSnap, phase: "connected" }), true);
  assert.equal(
    shouldCaptureLocalVideo({ ...baseSnap, phase: "connected", cam_enabled: false }),
    false,
  );
  assert.equal(
    shouldCaptureLocalVideo({ ...baseSnap, call_type: "DirectVoice", phase: "connected" }),
    false,
  );

  assert.equal(isCameraFailureStatus("unavailable"), true);
  assert.equal(isCameraFailureStatus("denied"), true);
  assert.equal(isCameraFailureStatus("unsupported"), true);
  assert.equal(isCameraFailureStatus("error"), true);
  assert.equal(isCameraFailureStatus("active"), false);
  assert.equal(isCameraFailureStatus("off"), false);
});
