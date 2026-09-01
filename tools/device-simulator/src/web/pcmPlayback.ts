const PLAYBACK_SAMPLE_RATE = 16_000;
const INTER_SEGMENT_GAP_SECONDS = 0.16;
const WORKLET_PROCESSOR_NAME = "ez-assistant-pcm-playback";

export interface PlaybackStart {
  outputId: string;
  runId: string;
  streamId: number;
  text: string;
  sampleCount: number;
}

export interface PlaybackEnd {
  outputId: string;
  streamId: number;
  reason: string;
}

export interface PlaybackStatus {
  enabled: boolean;
  active: boolean;
  text: string;
}

export interface PlaybackMarker {
  outputId: string;
  streamId: number;
}

export type PcmPlaybackSinkEvent =
  | { type: "marker_drained"; marker: PlaybackMarker }
  | { type: "underflow" }
  | { type: "overflow" }
  | { type: "processor_error" };

export interface PcmPlaybackSink {
  append(samples: Float32Array): void;
  appendSilence(sampleCount: number): void;
  beginSegment(): void;
  markSegment(marker: PlaybackMarker): void;
  clear(): void;
  close(): void;
}

export interface BrowserPcmPlaybackOptions {
  createContext?: () => AudioContext;
  createSink?: (
    context: AudioContext,
    onEvent: (event: PcmPlaybackSinkEvent) => void,
  ) => Promise<PcmPlaybackSink>;
}

interface ActivePlayback {
  playback: PlaybackStart;
  resampler: StreamingLinearResampler;
  receivedSamples: number;
}

/** H5 只播放 Node 已完成正式协议校验的 PCM，不参与 TTS 或设备协议解析。 */
export class BrowserPcmPlayback {
  private context: AudioContext | undefined;
  private sink: PcmPlaybackSink | undefined;
  private active: ActivePlayback | undefined;
  private readonly pendingMarkers = new Set<string>();
  private hasPreviousCompletedSegment = false;
  private readonly createContext: () => AudioContext;
  private readonly createSink: (
    context: AudioContext,
    onEvent: (event: PcmPlaybackSinkEvent) => void,
  ) => Promise<PcmPlaybackSink>;

  constructor(
    private readonly onStatus: (status: PlaybackStatus) => void,
    options: BrowserPcmPlaybackOptions = {},
  ) {
    this.createContext = options.createContext
      ?? (() => new AudioContext({ latencyHint: "interactive" }));
    this.createSink = options.createSink ?? createAudioWorkletSink;
    this.emit("播报未启用");
  }

  get isEnabled(): boolean {
    return this.context !== undefined && this.sink !== undefined;
  }

  get isActive(): boolean {
    return this.active !== undefined || this.pendingMarkers.size > 0;
  }

  async enable(): Promise<void> {
    const context = this.context ?? this.createContext();
    this.context = context;
    try {
      this.sink ??= await this.createSink(context, (event) => this.handleSinkEvent(event));
      if (context.state === "suspended") await context.resume();
    } catch (error) {
      this.sink?.close();
      this.sink = undefined;
      this.context = undefined;
      if (context.state !== "closed") await context.close();
      throw error;
    }
    this.emit(this.isActive ? "播报已启用，正在接收音频" : "播报已启用");
  }

  start(playback: PlaybackStart): void {
    if (this.active) {
      this.failCurrentSegment("playback_overlap");
      return;
    }
    const context = this.context;
    const sink = this.sink;
    if (context && sink && this.hasPreviousCompletedSegment) {
      sink.appendSilence(Math.round(context.sampleRate * INTER_SEGMENT_GAP_SECONDS));
    }
    sink?.beginSegment();
    this.active = {
      playback,
      resampler: new StreamingLinearResampler(PLAYBACK_SAMPLE_RATE, context?.sampleRate ?? 48_000),
      receivedSamples: 0,
    };
    this.emit(sink ? "正在播报" : "收到播报，请先启用播放");
  }

  frame(pcmS16Le: Uint8Array): void {
    const sink = this.sink;
    const active = this.active;
    if (!sink || !active) return;
    let samples: Float32Array;
    try {
      samples = pcm16ToFloat32(pcmS16Le);
    } catch {
      this.failCurrentSegment("playback_frame_invalid");
      return;
    }
    active.receivedSamples += samples.length;
    const resampled = active.resampler.process(samples);
    if (resampled.length > 0) sink.append(resampled);
  }

  end(playback: PlaybackEnd): void {
    const active = this.active;
    if (
      !active
      || active.playback.outputId !== playback.outputId
      || active.playback.streamId !== playback.streamId
    ) {
      return;
    }
    this.active = undefined;
    if (playback.reason !== "completed") {
      this.resetAudioTimeline();
      const status = playback.reason === "cancelled" ? "播报已停止" : "播报失败";
      this.emit(status);
      return;
    }
    if (active.receivedSamples !== active.playback.sampleCount) {
      this.failCurrentSegment("playback_sample_count_mismatch");
      return;
    }
    const sink = this.sink;
    if (!sink) {
      this.hasPreviousCompletedSegment = false;
      this.emit("播报已跳过（播放未启用）");
      return;
    }
    const tail = active.resampler.flush();
    if (tail.length > 0) sink.append(tail);
    const marker = { outputId: playback.outputId, streamId: playback.streamId };
    this.pendingMarkers.add(markerKey(marker));
    this.hasPreviousCompletedSegment = true;
    sink.markSegment(marker);
    this.emit("正在播报");
  }

  stopLocal(): void {
    this.active = undefined;
    this.resetAudioTimeline();
    this.emit(this.isEnabled ? "播报已停止" : "播报未启用");
  }

  async close(): Promise<void> {
    this.stopLocal();
    this.sink?.close();
    this.sink = undefined;
    const context = this.context;
    this.context = undefined;
    if (context && context.state !== "closed") await context.close();
  }

  private handleSinkEvent(event: PcmPlaybackSinkEvent): void {
    if (event.type === "marker_drained") {
      this.pendingMarkers.delete(markerKey(event.marker));
      if (!this.active && this.pendingMarkers.size === 0) this.emit("播报完成");
      return;
    }
    if (event.type === "underflow") {
      if (this.isActive) this.emit("播报缓冲恢复中");
      return;
    }
    this.failCurrentSegment(
      event.type === "overflow" ? "playback_buffer_overflow" : "playback_processor_error",
    );
  }

  private failCurrentSegment(code: string): void {
    this.active = undefined;
    this.resetAudioTimeline();
    this.emit(code);
  }

  private resetAudioTimeline(): void {
    this.pendingMarkers.clear();
    this.hasPreviousCompletedSegment = false;
    this.sink?.clear();
  }

  private emit(text: string): void {
    this.onStatus({ enabled: this.isEnabled, active: this.isActive, text });
  }
}

export class StreamingLinearResampler {
  private readonly sourceSampleRate: number;
  private readonly targetSampleRate: number;
  private tail: number | undefined;
  private totalInputSamples = 0;
  private nextOutputIndex = 0;

  constructor(sourceSampleRate: number, targetSampleRate: number) {
    if (sourceSampleRate <= 0 || targetSampleRate <= 0) {
      throw new Error("音频采样率必须为正数");
    }
    this.sourceSampleRate = sourceSampleRate;
    this.targetSampleRate = targetSampleRate;
  }

  process(input: Float32Array): Float32Array {
    if (input.length === 0) return new Float32Array();
    const priorTail = this.tail;
    const hadTail = priorTail !== undefined;
    const source = new Float32Array(input.length + (hadTail ? 1 : 0));
    let offset = 0;
    if (hadTail) {
      source[0] = priorTail;
      offset = 1;
    }
    source.set(input, offset);
    if (source.length === 1) {
      this.totalInputSamples += input.length;
      this.tail = source[0];
      return new Float32Array();
    }

    const baseInputIndex = hadTail ? this.totalInputSamples - 1 : 0;
    this.totalInputSamples += input.length;
    const desiredOutputCount = Math.max(0, Math.ceil(
      ((this.totalInputSamples - 1) * this.targetSampleRate / this.sourceSampleRate) - 1e-10,
    ));
    const output = new Float32Array(desiredOutputCount - this.nextOutputIndex);
    for (let index = 0; this.nextOutputIndex < desiredOutputCount; index += 1) {
      const sourcePosition = this.nextOutputIndex * this.sourceSampleRate / this.targetSampleRate;
      const lowerInputIndex = Math.floor(sourcePosition);
      const lower = lowerInputIndex - baseInputIndex;
      const fraction = sourcePosition - lowerInputIndex;
      const first = source[lower] ?? 0;
      const second = source[lower + 1] ?? first;
      output[index] = first + (second - first) * fraction;
      this.nextOutputIndex += 1;
    }
    this.tail = source.at(-1);
    return output;
  }

  flush(): Float32Array {
    const tail = this.tail;
    if (tail === undefined) return new Float32Array();
    const desiredOutputCount = Math.round(
      this.totalInputSamples * this.targetSampleRate / this.sourceSampleRate,
    );
    const output = new Float32Array(Math.max(0, desiredOutputCount - this.nextOutputIndex));
    output.fill(tail);
    this.tail = undefined;
    this.totalInputSamples = 0;
    this.nextOutputIndex = 0;
    return output;
  }
}

class AudioWorkletPcmSink implements PcmPlaybackSink {
  constructor(private readonly node: AudioWorkletNode) {}

  append(samples: Float32Array): void {
    this.node.port.postMessage({ type: "samples", samples }, [samples.buffer]);
  }

  appendSilence(sampleCount: number): void {
    this.node.port.postMessage({ type: "silence", sampleCount });
  }

  beginSegment(): void {
    this.node.port.postMessage({ type: "segment_start" });
  }

  markSegment(marker: PlaybackMarker): void {
    this.node.port.postMessage({ type: "marker", marker });
  }

  clear(): void {
    this.node.port.postMessage({ type: "clear" });
  }

  close(): void {
    this.node.port.postMessage({ type: "close" });
    this.node.port.close();
    this.node.disconnect();
  }
}

async function createAudioWorkletSink(
  context: AudioContext,
  onEvent: (event: PcmPlaybackSinkEvent) => void,
): Promise<PcmPlaybackSink> {
  if (!context.audioWorklet) throw new Error("当前浏览器不支持 AudioWorklet 播放");
  await context.audioWorklet.addModule(new URL("./pcmPlayback.worklet.js", import.meta.url));
  const node = new AudioWorkletNode(context, WORKLET_PROCESSOR_NAME, {
    numberOfInputs: 0,
    numberOfOutputs: 1,
    outputChannelCount: [1],
    processorOptions: {
      initialBufferSamples: Math.round(context.sampleRate * 0.12),
      recoveryBufferSamples: Math.round(context.sampleRate * 0.08),
      maxBufferSamples: Math.round(context.sampleRate * 90),
      fadeSamples: Math.round(context.sampleRate * 0.005),
    },
  });
  node.port.addEventListener("message", (message: MessageEvent<unknown>) => {
    if (isSinkEvent(message.data)) onEvent(message.data);
  });
  node.addEventListener("processorerror", () => onEvent({ type: "processor_error" }));
  node.port.start();
  node.connect(context.destination);
  return new AudioWorkletPcmSink(node);
}

function isSinkEvent(value: unknown): value is PcmPlaybackSinkEvent {
  if (!isRecord(value) || typeof value.type !== "string") return false;
  if (value.type === "underflow" || value.type === "overflow" || value.type === "processor_error") {
    return true;
  }
  return value.type === "marker_drained"
    && isRecord(value.marker)
    && typeof value.marker.outputId === "string"
    && Number.isInteger(value.marker.streamId);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function markerKey(marker: PlaybackMarker): string {
  return `${marker.outputId}:${marker.streamId}`;
}

export function pcm16ToFloat32(pcmS16Le: Uint8Array): Float32Array {
  if (pcmS16Le.byteLength === 0 || pcmS16Le.byteLength % 2 !== 0) {
    throw new Error("PCM16 播放帧长度无效");
  }
  const view = new DataView(pcmS16Le.buffer, pcmS16Le.byteOffset, pcmS16Le.byteLength);
  const samples = new Float32Array(pcmS16Le.byteLength / 2);
  for (let index = 0; index < samples.length; index += 1) {
    const value = view.getInt16(index * 2, true);
    samples[index] = value < 0 ? value / 32_768 : value / 32_767;
  }
  return samples;
}
