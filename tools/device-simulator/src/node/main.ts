import path from "node:path";

import { DeviceConnection } from "./connection.js";
import { isSimulatorFault } from "./faults.js";
import { HostDiscovery } from "./discovery.js";
import { SimulatorIdentity } from "./identity.js";
import { isTerminalProfile } from "./profiles.js";
import { SimulatorState } from "./state.js";
import { SimulatorWebBridge } from "./webBridge.js";

const home = resolveArgument("--home") ?? path.resolve(".device-simulator");
const requestedPort = Number.parseInt(resolveArgument("--port") ?? "0", 10);
if (!Number.isInteger(requestedPort) || requestedPort < 0 || requestedPort > 65_535) {
  throw new Error("--port must be between 0 and 65535");
}

const identity = await SimulatorIdentity.open(home);
const state = new SimulatorState("mixed");
const connection = new DeviceConnection(identity, state);
const discovery = new HostDiscovery((hosts) => state.setHosts(hosts));
const bridge = new SimulatorWebBridge(state, {
  async connect(hostInstallationId) {
    const host = state.snapshot().hosts.find(
      (candidate) => candidate.installationId === hostInstallationId,
    );
    if (!host) throw new Error("selected Host is no longer available");
    try {
      const snapshot = state.snapshot();
      await connection.connect(host, snapshot.declaredCapabilities, snapshot.outputPreference);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      state.patch({ phase: "error", lastError: message });
      state.diagnostic("error", message);
      throw error;
    }
  },
  disconnect() {
    connection.disconnect();
  },
  setOutputPreference(preference) {
    connection.setOutputPreference(preference);
  },
  setTerminalProfile(profile) {
    if (!isTerminalProfile(profile)) throw new Error("unknown terminal profile");
    state.setTerminalProfile(profile);
  },
  submitText(text) {
    connection.submitText(text);
  },
  retryLastText() {
    connection.retryLastText();
  },
  submitPcm(pcmS16Le) {
    connection.submitPcm(pcmS16Le);
  },
  retryLastPcm() {
    connection.retryLastPcm();
  },
  cancelPlayback() {
    connection.cancelPlayback();
  },
  injectFault(fault) {
    if (!isSimulatorFault(fault)) throw new Error("unknown simulator fault");
    connection.injectFault(fault);
  },
  async resetDevice() {
    connection.disconnect();
    connection.resetPairingSession();
    await identity.clearCredential();
    state.patch({ phase: "disconnected" });
    state.clearTransient();
    state.diagnostic("identity", "pairing credential cleared");
  },
});

connection.setPlaybackObserver({
  start(playback) {
    bridge.publishPlaybackStart(playback);
  },
  frame(pcmS16Le) {
    bridge.publishPlaybackFrame(pcmS16Le);
  },
  end(playback) {
    bridge.publishPlaybackEnd(playback);
  },
});

discovery.start();
const port = await bridge.listen(requestedPort);
console.log(`EZ Assistant device simulator: http://127.0.0.1:${port}`);
console.log(`Simulator home: ${home}`);

for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.once(signal, () => {
    connection.disconnect();
    discovery.stop();
    process.exit(0);
  });
}

function resolveArgument(name: string): string | undefined {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : undefined;
}
