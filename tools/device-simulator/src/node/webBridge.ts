import { randomBytes } from "node:crypto";
import { createReadStream } from "node:fs";
import { readFile, stat } from "node:fs/promises";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { WebSocketServer, type RawData, type WebSocket } from "ws";

import { base64Url, isRecord } from "./protocol.js";
import type { SimulatorFault } from "./faults.js";
import type { TerminalProfile } from "./profiles.js";
import { SimulatorState, type SimulatorSnapshot } from "./state.js";

const MAX_BRIDGE_CONTROL_BYTES = 64 * 1024;
const MAX_BRIDGE_PCM_BYTES = 60 * 16_000 * 2;

export interface BridgeActions {
  connect(hostInstallationId: string): Promise<void>;
  disconnect(): void;
  setOutputPreference(preference: "text" | "audio" | "text_and_audio"): void;
  setTerminalProfile(profile: TerminalProfile): void;
  submitText(text: string): void;
  retryLastText(): void;
  submitPcm(pcmS16Le: Uint8Array): void;
  retryLastPcm(): void;
  cancelPlayback(): void;
  injectFault(fault: SimulatorFault): void;
  resetDevice(): Promise<void>;
}

export class SimulatorWebBridge {
  private readonly token = base64Url(randomBytes(24));
  private readonly clients = new Set<WebSocket>();

  constructor(
    private readonly state: SimulatorState,
    private readonly actions: BridgeActions,
  ) {}

  async listen(port = 0): Promise<number> {
    const webRoot = path.resolve(
      path.dirname(fileURLToPath(import.meta.url)),
      process.env.NODE_ENV === "production" ? "../web" : "../../dist/web",
    );
    const server = createServer((request, response) => {
      void this.serveFile(webRoot, request, response);
    });
    const sockets = new WebSocketServer({ noServer: true, maxPayload: MAX_BRIDGE_PCM_BYTES });
    server.on("upgrade", (request, socket, head) => {
      const authority = request.headers.host;
      const origin = request.headers.origin;
      const url = new URL(request.url ?? "/", `http://${authority ?? "invalid"}`);
      const address = server.address();
      const boundPort = address && typeof address === "object" ? address.port : port;
      const expectedOrigin = `http://127.0.0.1:${boundPort}`;
      if (
        url.pathname !== "/bridge" ||
        url.searchParams.get("token") !== this.token ||
        authority !== expectedOrigin.slice("http://".length) ||
        origin !== expectedOrigin
      ) {
        socket.destroy();
        return;
      }
      sockets.handleUpgrade(request, socket, head, (webSocket) => {
        sockets.emit("connection", webSocket, request);
      });
    });
    sockets.on("connection", (socket) => {
      this.clients.add(socket);
      socket.send(JSON.stringify({ type: "snapshot", payload: this.state.snapshot() }));
      socket.on("message", (data, isBinary) => {
        if (isBinary) {
          void this.handlePcmAction(socket, data);
        } else if (rawByteLength(data) <= MAX_BRIDGE_CONTROL_BYTES) {
          void this.handleAction(socket, data.toString("utf8"));
        } else {
          this.sendActionError(socket, "control action exceeds 64 KiB");
        }
      });
      socket.on("close", () => this.clients.delete(socket));
    });
    this.state.on("changed", (snapshot: SimulatorSnapshot) => this.broadcast(snapshot));
    await new Promise<void>((resolve, reject) => {
      server.once("error", reject);
      server.listen(port, "127.0.0.1", resolve);
    });
    const address = server.address();
    if (!address || typeof address === "string") throw new Error("H5 bridge did not bind TCP");
    return address.port;
  }

  publishPlaybackStart(playback: {
    outputId: string;
    runId: string;
    streamId: number;
    text: string;
    sampleCount: number;
  }): void {
    this.broadcastControl({ type: "playback_start", payload: playback });
  }

  publishPlaybackFrame(pcmS16Le: Uint8Array): void {
    const frame = Buffer.from(pcmS16Le);
    for (const client of this.clients) {
      if (client.readyState === client.OPEN) client.send(frame);
    }
  }

  publishPlaybackEnd(playback: {
    outputId: string;
    streamId: number;
    reason: string;
  }): void {
    this.broadcastControl({ type: "playback_end", payload: playback });
  }

  private async handlePcmAction(socket: WebSocket, data: RawData): Promise<void> {
    try {
      const pcm = rawBytes(data);
      if (
        pcm.byteLength === 0 ||
        pcm.byteLength > MAX_BRIDGE_PCM_BYTES ||
        pcm.byteLength % 640 !== 0
      ) {
        throw new Error("recording must contain complete PCM frames and be at most 60 seconds");
      }
      this.actions.submitPcm(pcm);
    } catch (error) {
      this.sendActionError(socket, error instanceof Error ? error.message : String(error));
    }
  }

  private async handleAction(socket: WebSocket, text: string): Promise<void> {
    try {
      const value: unknown = JSON.parse(text);
      if (!isRecord(value) || typeof value.type !== "string") throw new Error("invalid action");
      if (value.type === "connect" && typeof value.hostInstallationId === "string") {
        await this.actions.connect(value.hostInstallationId);
      } else if (value.type === "disconnect") {
        this.actions.disconnect();
      } else if (
        value.type === "set_output_preference" &&
        (value.preference === "text" || value.preference === "audio" || value.preference === "text_and_audio")
      ) {
        this.actions.setOutputPreference(value.preference);
      } else if (
        value.type === "set_terminal_profile"
        && (
          value.profile === "voice_only"
          || value.profile === "screen_voice"
          || value.profile === "keyboard_screen"
          || value.profile === "mixed"
        )
      ) {
        this.actions.setTerminalProfile(value.profile);
      } else if (value.type === "reset_device") {
        await this.actions.resetDevice();
      } else if (value.type === "submit_text" && typeof value.text === "string") {
        this.actions.submitText(value.text);
      } else if (value.type === "retry_text") {
        this.actions.retryLastText();
      } else if (value.type === "submit_test_pcm") {
        this.actions.submitPcm(testTonePcm());
      } else if (value.type === "retry_pcm") {
        this.actions.retryLastPcm();
      } else if (value.type === "cancel_playback") {
        this.actions.cancelPlayback();
      } else if (
        value.type === "inject_fault"
        && typeof value.fault === "string"
        && isBridgeFault(value.fault)
      ) {
        this.actions.injectFault(value.fault);
      } else {
        throw new Error("unsupported action");
      }
    } catch (error) {
      this.sendActionError(socket, error instanceof Error ? error.message : String(error));
    }
  }

  private sendActionError(socket: WebSocket, message: string): void {
    socket.send(JSON.stringify({ type: "action_error", message }));
  }

  private broadcast(snapshot: SimulatorSnapshot): void {
    this.broadcastControl({ type: "snapshot", payload: snapshot });
  }

  private broadcastControl(message: unknown): void {
    const encoded = JSON.stringify(message);
    for (const client of this.clients) {
      if (client.readyState === client.OPEN) client.send(encoded);
    }
  }

  private async serveFile(
    webRoot: string,
    request: IncomingMessage,
    response: ServerResponse,
  ): Promise<void> {
    try {
      const url = new URL(request.url ?? "/", "http://127.0.0.1");
      const relative = url.pathname === "/" ? "index.html" : url.pathname.slice(1);
      const resolved = path.resolve(webRoot, relative);
      if (!resolved.startsWith(`${webRoot}${path.sep}`) && resolved !== path.join(webRoot, "index.html")) {
        response.writeHead(404).end();
        return;
      }
      if (relative === "index.html") {
        const html = (await readFile(resolved, "utf8")).replace("__SIMULATOR_TOKEN__", this.token);
        response.writeHead(200, {
          "Content-Type": "text/html; charset=utf-8",
          "Cache-Control": "no-store",
        }).end(html);
        return;
      }
      const metadata = await stat(resolved);
      if (!metadata.isFile()) throw new Error("not a file");
      response.writeHead(200, {
        "Content-Type": contentType(resolved),
        "Cache-Control": "no-store",
      });
      createReadStream(resolved).pipe(response);
    } catch {
      response.writeHead(404).end();
    }
  }
}

function isBridgeFault(value: string): value is SimulatorFault {
  return value === "corrupt_next_auth_signature"
    || value === "next_protocol_major_mismatch"
    || value === "duplicate_next_text_envelope"
    || value === "invalid_next_pcm_sequence"
    || value === "disconnect_after_next_input"
    || value === "ignore_next_ping"
    || value === "pause_read_5s"
    || value === "unsupported_output_preference"
    || value === "duplicate_playback_cancel";
}

function rawBytes(data: RawData): Uint8Array {
  if (Array.isArray(data)) return Uint8Array.from(Buffer.concat(data));
  if (data instanceof ArrayBuffer) return new Uint8Array(data.slice(0));
  return Uint8Array.from(data);
}

function rawByteLength(data: RawData): number {
  return Array.isArray(data)
    ? data.reduce((total, chunk) => total + chunk.byteLength, 0)
    : data.byteLength;
}

function testTonePcm(): Uint8Array {
  const samples = 16_000;
  const pcm = new Uint8Array(samples * 2);
  const view = new DataView(pcm.buffer);
  for (let index = 0; index < samples; index += 1) {
    const sample = Math.round(Math.sin(2 * Math.PI * 440 * index / 16_000) * 2_000);
    view.setInt16(index * 2, sample, true);
  }
  return pcm;
}

function contentType(file: string): string {
  if (file.endsWith(".js")) return "text/javascript; charset=utf-8";
  if (file.endsWith(".css")) return "text/css; charset=utf-8";
  if (file.endsWith(".svg")) return "image/svg+xml";
  return "application/octet-stream";
}
