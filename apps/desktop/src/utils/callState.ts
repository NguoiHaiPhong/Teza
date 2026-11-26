import type {
  AudioDeviceInfoDto,
  CallDevicesDto,
  CallEndReason,
  CallPhase,
  CallSnapshotDto,
  ThreadKind,
} from "../types.ts";

export type CallStartDisabledReason =
  | "relay_enabled"
  | "busy"
  | "unsupported_thread"
  | "not_authenticated";

export interface CanStartVoiceCallInput {
  threadKind: ThreadKind | null;
  threadId: string;
  myUid: string;
  relayEnabled: boolean;
  callPhase: CallPhase;
  authed: boolean;
}

export function formatCallDuration(secs: number): string {
  const safe = Number.isFinite(secs) && secs > 0 ? Math.floor(secs) : 0;
  const hours = Math.floor(safe / 3600);
  const mins = Math.floor((safe % 3600) / 60);
  const rem = safe % 60;
  const mm = String(mins).padStart(2, "0");
  const ss = String(rem).padStart(2, "0");
  if (hours > 0) {
    return `${hours}:${mm}:${ss}`;
  }
  return `${mm}:${ss}`;
}

export function canStartVoiceCall(input: CanStartVoiceCallInput): {
  allowed: boolean;
  disabledReason: CallStartDisabledReason | null;
} {
  if (!input.authed) {
    return { allowed: false, disabledReason: "not_authenticated" };
  }
  if (
    input.threadKind !== "direct" ||
    !input.threadId ||
    input.threadId === "cloud-me" ||
    (input.myUid && input.threadId === input.myUid)
  ) {
    return { allowed: false, disabledReason: "unsupported_thread" };
  }
  if (input.relayEnabled) {
    return { allowed: false, disabledReason: "relay_enabled" };
  }
  if (input.callPhase !== "idle") {
    return { allowed: false, disabledReason: "busy" };
  }
  return { allowed: true, disabledReason: null };
}

export type ClassifiedCallEnd =
  | {
      kind:
        | "local_hangup"
        | "local_declined"
        | "local_cancelled"
        | "remote_ended"
        | "remote_cancelled"
        | "answered_on_other_device"
        | "ended_on_other_device"
        | "ring_timeout";
      isSilent: boolean;
    }
  | {
      kind: "remote_rejected" | "error";
      detailMessage: string;
      isSilent: false;
    };

export function classifyCallEndReason(reason: CallEndReason): ClassifiedCallEnd {
  if (typeof reason === "string") {
    switch (reason) {
      case "local_declined":
      case "local_cancelled":
        return { kind: reason, isSilent: true };
      case "local_hangup":
      case "remote_ended":
      case "remote_cancelled":
      case "answered_on_other_device":
      case "ended_on_other_device":
      case "ring_timeout":
        return { kind: reason, isSilent: false };
    }
  }
  if ("remote_rejected" in reason) {
    return {
      kind: "remote_rejected",
      detailMessage: reason.remote_rejected.message || "",
      isSilent: false,
    };
  }
  return {
    kind: "error",
    detailMessage: reason.error || "",
    isSilent: false,
  };
}

export function normalizeLevelPercent(permille: number): number {
  if (!Number.isFinite(permille) || permille <= 0) return 0;
  return Math.max(0, Math.min(100, Math.round(permille / 10)));
}

const VIRTUAL_AUDIO_PATTERNS = [
  "vb-audio",
  "virtual cable",
  "cable input",
  "cable output",
  "cable in ",
  "cable out ",
  "stereo mix",
  "what u hear",
  "wave out mix",
  "loopback",
  "monitor of ",
  "blackhole",
  "soundflower",
  "virtual audio",
];

function isVirtualAudioName(dev: AudioDeviceInfoDto): boolean {
  if (typeof dev.is_virtual === "boolean") {
    return dev.is_virtual;
  }
  const low = dev.name.trim().toLowerCase();
  return VIRTUAL_AUDIO_PATTERNS.some((pat) => low.includes(pat));
}

function isSpdifDigitalOutputName(name: string): boolean {
  const low = name.trim().toLowerCase();
  return (
    low.includes("digital output") ||
    low.includes("s/pdif") ||
    low.includes("spdif") ||
    low.includes("iec958")
  );
}

export interface EffectiveCallDevicesInfo {
  defaultInput: AudioDeviceInfoDto | null;
  defaultOutput: AudioDeviceInfoDto | null;
  effectiveInput: AudioDeviceInfoDto | null;
  effectiveOutput: AudioDeviceInfoDto | null;
  inputIsVirtual: boolean;
  outputIsVirtual: boolean;
  hasPhysicalInput: boolean;
  hasPhysicalOutput: boolean;
}

function pickEffectiveDevice(
  list: AudioDeviceInfoDto[],
  selectedName: string | null,
  avoidSpdifOnAuto: boolean,
): { def: AudioDeviceInfoDto | null; effective: AudioDeviceInfoDto | null } {
  const def = list.find((d) => d.is_default) ?? list[0] ?? null;
  const trimmed = selectedName?.trim().toLowerCase();
  if (trimmed) {
    const matched = list.find((d) => d.name.trim().toLowerCase() === trimmed);
    if (matched) {
      return { def, effective: matched };
    }
  }
  if (def && !isVirtualAudioName(def)) {
    return { def, effective: def };
  }
  const phys = list.find(
    (d) =>
      !isVirtualAudioName(d) &&
      (!avoidSpdifOnAuto || !isSpdifDigitalOutputName(d.name)),
  );
  if (phys) {
    return { def, effective: phys };
  }
  if (avoidSpdifOnAuto) {
    const fallbackPhys = list.find((d) => !isVirtualAudioName(d));
    if (fallbackPhys) {
      return { def, effective: fallbackPhys };
    }
  }
  return { def, effective: def };
}

export function resolveEffectiveCallDevices(
  devices: CallDevicesDto | null,
): EffectiveCallDevicesInfo {
  const inputs = devices?.inputs ?? [];
  const outputs = devices?.outputs ?? [];
  const inPick = pickEffectiveDevice(inputs, devices?.selected_input ?? null, false);
  const outPick = pickEffectiveDevice(outputs, devices?.selected_output ?? null, true);

  return {
    defaultInput: inPick.def,
    defaultOutput: outPick.def,
    effectiveInput: inPick.effective,
    effectiveOutput: outPick.effective,
    inputIsVirtual: inPick.effective ? isVirtualAudioName(inPick.effective) : false,
    outputIsVirtual: outPick.effective ? isVirtualAudioName(outPick.effective) : false,
    hasPhysicalInput: inputs.some((d) => !isVirtualAudioName(d)),
    hasPhysicalOutput: outputs.some((d) => !isVirtualAudioName(d)),
  };
}

export function shouldShowVideoOverlay(
  active: CallSnapshotDto | null,
  minimized: boolean,
): boolean {
  if (!active || minimized) return false;
  return (
    active.call_type === "DirectVideo" &&
    active.phase !== "idle" &&
    active.phase !== "incoming_ringing"
  );
}

export function shouldCaptureLocalVideo(active: CallSnapshotDto | null): boolean {
  if (!active) return false;
  return (
    active.call_type === "DirectVideo" &&
    active.phase === "connected" &&
    active.cam_enabled
  );
}

export function isCameraFailureStatus(status: string): boolean {
  return (
    status === "unavailable" ||
    status === "denied" ||
    status === "unsupported" ||
    status === "error"
  );
}
