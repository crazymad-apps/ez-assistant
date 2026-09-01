import { readFileSync } from "node:fs";
import { createPublicKey, verify } from "node:crypto";
import { describe, expect, it } from "vitest";

import {
  authenticationTranscript,
  pairingAssociatedData,
  startPartyA,
} from "../src/node/crypto.js";
import { decodeEnvelope, encodeEnvelope } from "../src/node/protocol.js";

const fixture = JSON.parse(readFileSync(new URL(
  "../../../docs/resources/device-protocol-v1/fixtures/crypto-v1.json",
  import.meta.url,
), "utf8"));
const envelopeFixture = JSON.parse(readFileSync(new URL(
  "../../../docs/resources/device-protocol-v1/fixtures/envelope-v1.json",
  import.meta.url,
), "utf8"));

describe("shared device protocol cryptography", () => {
  it("matches the Rust RFC 9382 SPAKE2 fixture", () => {
    const pairing = fixture.pairing;
    const associatedData = pairingAssociatedData(
      pairing.pairing_request_id,
      pairing.installation_id,
      pairing.certificate_fingerprint,
      pairing.device_nonce,
      pairing.host_nonce,
      pairing.capabilities,
    );
    expect(associatedData.toString("base64url")).toBe(pairing.associated_data);
    const party = startPartyA(
      pairing.pairing_code,
      pairing.pairing_request_id,
      pairing.installation_id,
      BigInt(`0x${pairing.device_scalar_hex}`),
    );
    expect(party.share.toString("base64url")).toBe(pairing.device_share);
    const keys = party.finish(Buffer.from(pairing.host_share, "base64url"), associatedData);
    keys.verifyHostConfirmation(Buffer.from(pairing.host_confirmation, "base64url"));
    expect(keys.confirmationMac.toString("base64url")).toBe(pairing.device_confirmation);
    expect(keys.sessionKey.toString("base64url")).toBe(pairing.session_key);
  });

  it("matches the shared authentication transcript", () => {
    const authentication = fixture.authentication;
    const transcript = authenticationTranscript(
      authentication.connection_id,
      authentication.host_nonce,
      authentication.device_id,
      authentication.device_nonce,
      fixture.pairing.capabilities,
      authentication.output_preference,
    );
    expect(transcript.toString("base64url")).toBe(authentication.transcript);
    const publicKey = createPublicKey({
      key: Buffer.concat([
        Buffer.from("302a300506032b6570032100", "hex"),
        Buffer.from(authentication.public_key, "base64url"),
      ]),
      format: "der",
      type: "spki",
    });
    expect(verify(
      null,
      transcript,
      publicKey,
      Buffer.from(authentication.signature, "base64url"),
    )).toBe(true);
  });

  it("matches the shared strict envelope and stable error codes", () => {
    const encoded = encodeEnvelope(envelopeFixture.valid_envelope);
    const decoded = decodeEnvelope(encoded);
    const payload = decoded.payload as { code: string; recoverable: boolean };
    expect(decoded).toEqual(envelopeFixture.valid_envelope);
    expect(envelopeFixture.error_codes).toContainEqual({
      code: payload.code,
      recoverable: payload.recoverable,
    });
    expect(envelopeFixture.error_codes).toContainEqual({
      code: "pairing_failed",
      recoverable: true,
    });
    expect(envelopeFixture.error_codes).toContainEqual({
      code: "device_revoked",
      recoverable: false,
    });
  });
});
