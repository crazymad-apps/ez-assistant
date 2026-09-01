import { EventEmitter } from "node:events";

import type { DeviceCapabilities, OutputPreference } from "./protocol.js";
import type { SimulatorFault } from "./faults.js";
import {
  type TerminalProfile,
  terminalProfileDefinition,
} from "./profiles.js";

export interface DiscoveredHost {
  installationId: string;
  certificateFingerprint: string;
  address: string;
  port: number;
  path: string;
  pairingAvailable: boolean;
}

export type SimulatorPhase =
  | "discovering"
  | "disconnected"
  | "connecting"
  | "pairing"
  | "idle"
  | "listening"
  | "recognizing"
  | "processing"
  | "speaking"
  | "accepted_or_queued"
  | "error";

export interface CurrentInteraction {
  clientInputId: string;
  submittedText: string;
  segmentClientInputIds?: string[];
  inputId?: string;
  runId?: string;
  queueState?: string;
  outputId?: string;
  textOutput?: string;
}

export interface DiagnosticEvent {
  at: string;
  kind: string;
  detail: string;
}

export interface CurrentPlayback {
  outputId: string;
  runId: string;
  streamId: number;
  text: string;
  sampleCount: number;
  receivedBytes: number;
  status: "playing" | "completed" | "cancelled" | "failed";
  reason?: string;
}

export interface SimulatorSnapshot {
  phase: SimulatorPhase;
  hosts: DiscoveredHost[];
  selectedHostInstallationId?: string;
  pairingCode?: string;
  pairedDeviceId?: string;
  outputPreference: OutputPreference;
  terminalProfile: TerminalProfile;
  declaredCapabilities: DeviceCapabilities;
  capabilities: DeviceCapabilities;
  currentInteraction?: CurrentInteraction;
  currentPlayback?: CurrentPlayback;
  armedFault?: SimulatorFault;
  lastError?: string;
  diagnostics: DiagnosticEvent[];
}

const DIAGNOSTIC_CAPACITY = 1_000;

export class SimulatorState extends EventEmitter {
  private value: SimulatorSnapshot;

  constructor(profile: TerminalProfile) {
    super();
    const definition = terminalProfileDefinition(profile);
    this.value = {
      phase: "discovering",
      hosts: [],
      outputPreference: definition.outputPreference,
      terminalProfile: profile,
      declaredCapabilities: definition.capabilities,
      capabilities: { ...definition.capabilities },
      diagnostics: [],
    };
  }

  snapshot(): SimulatorSnapshot {
    return structuredClone(this.value);
  }

  setHosts(hosts: DiscoveredHost[]): void {
    this.value.hosts = hosts.sort((left, right) =>
      left.installationId.localeCompare(right.installationId),
    );
    this.changed();
  }

  patch(patch: Partial<Omit<SimulatorSnapshot, "diagnostics" | "hosts">>): void {
    Object.assign(this.value, patch);
    this.changed();
  }

  setTerminalProfile(profile: TerminalProfile): void {
    if (!isDisconnectedPhase(this.value.phase)) {
      throw new Error("请先断开设备，再切换终端形态");
    }
    const definition = terminalProfileDefinition(profile);
    this.value.terminalProfile = profile;
    this.value.declaredCapabilities = definition.capabilities;
    this.value.capabilities = { ...definition.capabilities };
    this.value.outputPreference = definition.outputPreference;
    this.diagnostic("terminal_profile", profile);
  }

  setArmedFault(fault: SimulatorFault | undefined): void {
    if (fault === undefined) delete this.value.armedFault;
    else this.value.armedFault = fault;
    this.changed();
  }

  clearTransient(): void {
    delete this.value.pairingCode;
    delete this.value.lastError;
    this.changed();
  }

  clearPairedDevice(): void {
    delete this.value.pairedDeviceId;
    this.changed();
  }

  beginTextInput(clientInputId: string, submittedText: string): void {
    this.value.currentInteraction = { clientInputId, submittedText };
    this.value.phase = "processing";
    this.changed();
  }

  beginSpeechInput(clientInputId: string): void {
    const current = this.value.currentInteraction;
    if (
      current?.segmentClientInputIds
      && current.inputId === undefined
    ) {
      if (!current.segmentClientInputIds.includes(clientInputId)) {
        current.segmentClientInputIds.push(clientInputId);
      }
    } else {
      this.value.currentInteraction = {
        clientInputId,
        submittedText: "语音输入",
        segmentClientInputIds: [clientInputId],
      };
    }
    this.value.phase = "listening";
    this.changed();
  }

  diagnostic(kind: string, detail: string): void {
    this.value.diagnostics.push({ at: new Date().toISOString(), kind, detail });
    if (this.value.diagnostics.length > DIAGNOSTIC_CAPACITY) {
      this.value.diagnostics.splice(0, this.value.diagnostics.length - DIAGNOSTIC_CAPACITY);
    }
    this.changed();
  }

  private changed(): void {
    this.emit("changed", this.snapshot());
  }
}

function isDisconnectedPhase(phase: SimulatorPhase): boolean {
  return phase === "discovering" || phase === "disconnected";
}
