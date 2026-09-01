import {
  createHash,
  createHmac,
  hkdfSync,
  randomBytes,
  timingSafeEqual,
} from "node:crypto";
import { p256 } from "@noble/curves/nist.js";

import type { DeviceCapabilities, OutputPreference } from "./protocol.js";

const M = p256.Point.fromBytes(
  Buffer.from("02886e2f97ace46e55ba9dd7242579f2993b64e16ef3dcab95afd497333d8fa12f", "hex"),
);
const N = p256.Point.fromBytes(
  Buffer.from("03d8bbd6c639c62937b04d997f38c3770719c629d7014d49a24b4f98baa1292b49", "hex"),
);
const ORDER = p256.Point.Fn.ORDER;

export interface PartyAResult {
  share: Buffer;
  finish(hostShare: Uint8Array, associatedData: Uint8Array): PairingKeys;
}

export interface PairingKeys {
  sessionKey: Buffer;
  confirmationMac: Buffer;
  verifyHostConfirmation(candidate: Uint8Array): void;
  bindingMac(label: Uint8Array, transcript: Uint8Array): Buffer;
}

export function startPartyA(
  pairingCode: string,
  pairingRequestId: string,
  installationId: string,
  privateScalar = randomScalar(),
): PartyAResult {
  if (!/^\d{6}$/.test(pairingCode)) throw new Error("pairing code must contain six digits");
  const w = passwordScalar(pairingCode, pairingRequestId, installationId);
  const sharePoint = p256.Point.BASE.multiply(privateScalar).add(M.multiply(w));
  const share = Buffer.from(sharePoint.toBytes(false));
  return {
    share,
    finish(hostShare: Uint8Array, associatedData: Uint8Array): PairingKeys {
      const hostPoint = p256.Point.fromBytes(hostShare);
      const keyPoint = hostPoint.subtract(N.multiply(w)).multiply(privateScalar);
      const keyBytes = Buffer.from(keyPoint.toBytes(false));
      const wBytes = bigintBytes(w, 32);
      const tt = rfcTranscript([
        Buffer.from(pairingRequestId),
        Buffer.from(installationId),
        share,
        Buffer.from(hostShare),
        keyBytes,
        wBytes,
      ]);
      const schedule = createHash("sha256").update(tt).digest();
      const sessionKey = schedule.subarray(0, 16);
      const authenticationKey = schedule.subarray(16);
      const confirmationKeys = Buffer.from(
        hkdfSync(
          "sha256",
          authenticationKey,
          Buffer.alloc(0),
          Buffer.concat([Buffer.from("ConfirmationKeys"), Buffer.from(associatedData)]),
          32,
        ),
      );
      const deviceConfirmation = hmac(confirmationKeys.subarray(0, 16), tt);
      const hostConfirmation = hmac(confirmationKeys.subarray(16), tt);
      return {
        sessionKey: Buffer.from(sessionKey),
        confirmationMac: deviceConfirmation,
        verifyHostConfirmation(candidate: Uint8Array): void {
          const value = Buffer.from(candidate);
          if (value.length !== hostConfirmation.length || !timingSafeEqual(value, hostConfirmation)) {
            throw new Error("host SPAKE2 confirmation failed");
          }
        },
        bindingMac(label: Uint8Array, transcript: Uint8Array): Buffer {
          return hmac(sessionKey, Buffer.concat([Buffer.from(label), Buffer.from(transcript)]));
        },
      };
    },
  };
}

export function pairingAssociatedData(
  pairingRequestId: string,
  installationId: string,
  certificateFingerprint: string,
  deviceNonce: string,
  hostNonce: string,
  capabilities: DeviceCapabilities,
): Buffer {
  return applicationTranscript([
    Buffer.from("ez-assistant-pairing-v1"),
    Buffer.from(pairingRequestId),
    Buffer.from(installationId),
    Buffer.from(certificateFingerprint),
    Buffer.from(deviceNonce),
    Buffer.from(hostNonce),
    capabilityBytes(capabilities),
  ]);
}

export function pairingBindTranscript(associatedData: Uint8Array, publicKey: Uint8Array): Buffer {
  return applicationTranscript([
    Buffer.from("ez-assistant-bind-v1"),
    Buffer.from(associatedData),
    Buffer.from(publicKey),
  ]);
}

export function pairingCommitTranscript(bindTranscript: Uint8Array, deviceId: string): Buffer {
  return applicationTranscript([
    Buffer.from("ez-assistant-commit-v1"),
    Buffer.from(bindTranscript),
    Buffer.from(deviceId),
  ]);
}

export function authenticationTranscript(
  connectionId: string,
  hostNonce: string,
  deviceId: string,
  deviceNonce: string,
  capabilities: DeviceCapabilities,
  preference: OutputPreference,
): Buffer {
  return applicationTranscript([
    Buffer.from("ez-assistant-auth-v1"),
    Buffer.from([0, 1]),
    Buffer.from([0, 0]),
    Buffer.from(connectionId),
    Buffer.from(hostNonce),
    Buffer.from(deviceId),
    Buffer.from(deviceNonce),
    capabilityBytes(capabilities),
    Buffer.from(preference),
  ]);
}

export function applicationTranscript(parts: readonly Uint8Array[]): Buffer {
  const output: Buffer[] = [];
  for (const part of parts) {
    const length = Buffer.alloc(8);
    length.writeBigUInt64BE(BigInt(part.length));
    output.push(length, Buffer.from(part));
  }
  return Buffer.concat(output);
}

function rfcTranscript(parts: readonly Uint8Array[]): Buffer {
  const output: Buffer[] = [];
  for (const part of parts) {
    const length = Buffer.alloc(8);
    length.writeBigUInt64LE(BigInt(part.length));
    output.push(length, Buffer.from(part));
  }
  return Buffer.concat(output);
}

function passwordScalar(code: string, requestId: string, installationId: string): bigint {
  const input = applicationTranscript([
    Buffer.from("ez-assistant-spake2-password-v1"),
    Buffer.from(code),
    Buffer.from(requestId),
    Buffer.from(installationId),
  ]);
  return bytesBigint(createHash("sha512").update(input).digest()) % ORDER;
}

function randomScalar(): bigint {
  while (true) {
    const candidate = bytesBigint(randomBytes(32));
    if (candidate > 0n && candidate < ORDER) return candidate;
  }
}

function capabilityBytes(capabilities: DeviceCapabilities): Buffer {
  return Buffer.from([
    Number(capabilities.input_text),
    Number(capabilities.input_pcm16_16k_mono),
    Number(capabilities.output_text),
    Number(capabilities.output_pcm16_16k_mono),
    Number(capabilities.playback_cancel),
    Number(capabilities.display_status),
    Number(capabilities.display_transcript),
  ]);
}

function bytesBigint(value: Uint8Array): bigint {
  const hex = Buffer.from(value).toString("hex");
  return BigInt(`0x${hex || "0"}`);
}

function bigintBytes(value: bigint, length: number): Buffer {
  return Buffer.from(value.toString(16).padStart(length * 2, "0"), "hex");
}

function hmac(key: Uint8Array, value: Uint8Array): Buffer {
  return createHmac("sha256", key).update(value).digest();
}
