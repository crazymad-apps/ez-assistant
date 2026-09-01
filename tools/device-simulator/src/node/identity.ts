import {
  createPrivateKey,
  generateKeyPairSync,
  randomBytes,
  sign,
} from "node:crypto";
import { chmod, mkdir, readFile, rename, writeFile } from "node:fs/promises";
import path from "node:path";

import { base64Url } from "./protocol.js";

export interface PairedCredential {
  status: "pending" | "active";
  deviceId: string;
  hostInstallationId: string;
  hostCertificateFingerprint: string;
  publicKey: string;
  privateKeyPkcs8: string;
}

interface IdentityDocument {
  schemaVersion: 1;
  simulatorInstallationId: string;
  credential?: PairedCredential;
}

export class SimulatorIdentity {
  private constructor(
    readonly home: string,
    private document: IdentityDocument,
  ) {}

  static async open(home: string): Promise<SimulatorIdentity> {
    await mkdir(home, { recursive: true, mode: 0o700 });
    await chmod(home, 0o700);
    const file = path.join(home, "device.json");
    try {
      const parsed: unknown = JSON.parse(await readFile(file, "utf8"));
      if (!isIdentityDocument(parsed)) throw new Error("simulator identity is invalid");
      return new SimulatorIdentity(home, parsed);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
      const identity = new SimulatorIdentity(home, {
        schemaVersion: 1,
        simulatorInstallationId: base64Url(randomBytes(16)),
      });
      await identity.persist();
      return identity;
    }
  }

  get installationId(): string {
    return this.document.simulatorInstallationId;
  }

  get credential(): PairedCredential | undefined {
    return this.document.credential;
  }

  generatePendingCredential(
    deviceId: string,
    hostInstallationId: string,
    hostCertificateFingerprint: string,
  ): PairedCredential {
    const { privateKey, publicKey } = generateKeyPairSync("ed25519");
    const publicDer = publicKey.export({ format: "der", type: "spki" });
    const privateDer = privateKey.export({ format: "der", type: "pkcs8" });
    return {
      status: "pending",
      deviceId,
      hostInstallationId,
      hostCertificateFingerprint,
      publicKey: base64Url(publicDer.subarray(publicDer.length - 32)),
      privateKeyPkcs8: privateDer.toString("base64"),
    };
  }

  sign(credential: PairedCredential, transcript: Uint8Array): Buffer {
    const key = createPrivateKey({
      key: Buffer.from(credential.privateKeyPkcs8, "base64"),
      format: "der",
      type: "pkcs8",
    });
    return sign(null, transcript, key);
  }

  async savePending(credential: PairedCredential): Promise<void> {
    this.document.credential = { ...credential, status: "pending" };
    await this.persist();
  }

  async activate(): Promise<void> {
    if (!this.document.credential) throw new Error("pending credential is missing");
    this.document.credential.status = "active";
    await this.persist();
  }

  async clearCredential(): Promise<void> {
    delete this.document.credential;
    await this.persist();
  }

  private async persist(): Promise<void> {
    const target = path.join(this.home, "device.json");
    const temporary = path.join(this.home, `.device-${base64Url(randomBytes(8))}.tmp`);
    await writeFile(temporary, `${JSON.stringify(this.document, null, 2)}\n`, {
      mode: 0o600,
      flag: "wx",
    });
    await rename(temporary, target);
    await chmod(target, 0o600);
  }
}

function isIdentityDocument(value: unknown): value is IdentityDocument {
  if (typeof value !== "object" || value === null) return false;
  const candidate = value as Partial<IdentityDocument>;
  return (
    candidate.schemaVersion === 1 &&
    typeof candidate.simulatorInstallationId === "string" &&
    candidate.simulatorInstallationId.length > 0
  );
}
