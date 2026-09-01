import Bonjour, { type Browser, type Service } from "bonjour-service";

import type { DiscoveredHost } from "./state.js";

export class HostDiscovery {
  private readonly bonjour = new Bonjour();
  private readonly hosts = new Map<string, DiscoveredHost>();
  private browser: Browser | undefined;

  constructor(private readonly changed: (hosts: DiscoveredHost[]) => void) {}

  start(): void {
    if (this.browser) return;
    this.browser = this.bonjour.find({ type: "ez-assistant", protocol: "tcp" });
    this.browser.on("up", (service) => this.upsert(service));
    this.browser.on("txt-update", (service) => this.upsert(service));
    this.browser.on("srv-update", (service) => this.upsert(service));
    this.browser.on("down", (service) => {
      const installationId = text(service, "installation_id");
      if (installationId) this.hosts.delete(installationId);
      this.publish();
    });
  }

  stop(): void {
    this.browser?.stop();
    this.browser = undefined;
    this.bonjour.destroy();
  }

  private upsert(service: Service): void {
    const installationId = text(service, "installation_id");
    const certificateFingerprint = text(service, "certificate_fingerprint");
    const path = text(service, "path");
    const address = service.addresses?.find((candidate) => !candidate.includes(":"));
    if (
      !installationId ||
      !certificateFingerprint ||
      !address ||
      path !== "/device" ||
      text(service, "protocol_major") !== "1"
    ) {
      return;
    }
    this.hosts.set(installationId, {
      installationId,
      certificateFingerprint,
      address,
      port: service.port,
      path,
      pairingAvailable: text(service, "pairing_available") === "true",
    });
    this.publish();
  }

  private publish(): void {
    this.changed([...this.hosts.values()]);
  }
}

function text(service: Service, key: string): string | undefined {
  const value: unknown = service.txt?.[key];
  return typeof value === "string" ? value : undefined;
}
