const TARGET_SAMPLE_RATE = 16_000;
const SAMPLES_PER_FRAME = 320;
const MAX_RECORDING_SECONDS = 60;
const WORKLET_PROCESSOR_NAME = "ez-assistant-pcm-recorder";

export interface RecordingResult {
  pcm: Uint8Array;
  durationMs: number;
}

type RecorderWorkletEvent =
  | { type: "samples"; samples: Float32Array }
  | { type: "maximum" }
  | { type: "processor_error" };

/** Demo 页面只负责采集和格式转换；识别仍由 Host SpeechService 完成。 */
export class BrowserPcmRecorder {
  private context: AudioContext | undefined;
  private stream: MediaStream | undefined;
  private source: MediaStreamAudioSourceNode | undefined;
  private processor: AudioWorkletNode | undefined;
  private mutedOutput: GainNode | undefined;
  private chunks: Float32Array[] = [];
  private sampleCount = 0;
  private inputSampleRate = 0;
  private onMaximumDuration: (() => void) | undefined;
  private maximumNotified = false;
  private processorError: Error | undefined;

  get isRecording(): boolean {
    return this.context !== undefined;
  }

  async start(onMaximumDuration: () => void): Promise<void> {
    if (this.isRecording) throw new Error("已经在录音");
    if (!navigator.mediaDevices?.getUserMedia) throw new Error("当前浏览器不支持麦克风采集");

    const stream = await navigator.mediaDevices.getUserMedia({
      audio: {
        channelCount: 1,
        echoCancellation: true,
        noiseSuppression: true,
        autoGainControl: true,
      },
      video: false,
    });
    let context: AudioContext | undefined;
    try {
      const activeContext = new AudioContext({ latencyHint: "interactive" });
      context = activeContext;
      if (!activeContext.audioWorklet) throw new Error("当前浏览器不支持 AudioWorklet 录音");
      await activeContext.audioWorklet.addModule(
        new URL("./pcmRecorder.worklet.js", import.meta.url),
      );
      const source = activeContext.createMediaStreamSource(stream);
      const processor = new AudioWorkletNode(activeContext, WORKLET_PROCESSOR_NAME, {
        numberOfInputs: 1,
        numberOfOutputs: 1,
        outputChannelCount: [1],
        processorOptions: {
          maximumSamples: Math.floor(activeContext.sampleRate * MAX_RECORDING_SECONDS),
        },
      });
      const mutedOutput = activeContext.createGain();
      mutedOutput.gain.value = 0;
      processor.port.addEventListener("message", (message: MessageEvent<unknown>) => {
        const event = parseRecorderEvent(message.data);
        if (!event) return;
        if (event.type === "samples") {
          this.chunks.push(event.samples);
          this.sampleCount += event.samples.length;
        } else if (event.type === "maximum") {
          this.notifyMaximumDuration();
        } else {
          this.processorError = new Error("录音处理器异常退出");
          this.notifyMaximumDuration();
        }
      });
      processor.addEventListener("processorerror", () => {
        this.processorError = new Error("录音处理器异常退出");
        this.notifyMaximumDuration();
      });
      processor.port.start();

      this.context = activeContext;
      this.stream = stream;
      this.source = source;
      this.processor = processor;
      this.mutedOutput = mutedOutput;
      this.chunks = [];
      this.sampleCount = 0;
      this.inputSampleRate = activeContext.sampleRate;
      this.onMaximumDuration = onMaximumDuration;
      this.maximumNotified = false;
      this.processorError = undefined;
      source.connect(processor);
      processor.connect(mutedOutput);
      mutedOutput.connect(activeContext.destination);
      await activeContext.resume();
    } catch (error) {
      for (const track of stream.getTracks()) track.stop();
      if (context && context.state !== "closed") await context.close();
      throw error;
    }
  }

  async stop(): Promise<RecordingResult> {
    if (!this.context) throw new Error("当前没有录音");
    const samples = joinChunks(this.chunks, this.sampleCount);
    const inputSampleRate = this.inputSampleRate;
    const processorError = this.processorError;
    const durationMs = Math.min(
      MAX_RECORDING_SECONDS * 1_000,
      samples.length * 1_000 / inputSampleRate,
    );
    await this.release();
    if (processorError) throw processorError;
    if (samples.length < Math.ceil(inputSampleRate / 10)) {
      throw new Error("录音时间过短，请至少说话 0.1 秒");
    }
    return {
      pcm: encodeProtocolPcm(samples, inputSampleRate),
      durationMs,
    };
  }

  async cancel(): Promise<void> {
    if (this.context) await this.release();
  }

  private notifyMaximumDuration(): void {
    if (this.maximumNotified) return;
    this.maximumNotified = true;
    this.onMaximumDuration?.();
  }

  private async release(): Promise<void> {
    this.source?.disconnect();
    this.processor?.port.postMessage({ type: "close" });
    this.processor?.port.close();
    this.processor?.disconnect();
    this.mutedOutput?.disconnect();
    for (const track of this.stream?.getTracks() ?? []) track.stop();
    const context = this.context;
    this.context = undefined;
    this.stream = undefined;
    this.source = undefined;
    this.processor = undefined;
    this.mutedOutput = undefined;
    this.chunks = [];
    this.sampleCount = 0;
    this.inputSampleRate = 0;
    this.onMaximumDuration = undefined;
    this.maximumNotified = false;
    this.processorError = undefined;
    if (context && context.state !== "closed") await context.close();
  }
}

/** 转为协议固定的 PCM16 LE/16 kHz/mono，并用静音补齐最后一个 20 ms frame。 */
export function encodeProtocolPcm(samples: Float32Array, inputSampleRate: number): Uint8Array {
  if (!Number.isFinite(inputSampleRate) || inputSampleRate <= 0) {
    throw new Error("麦克风采样率无效");
  }
  const outputSamples = Math.floor(samples.length * TARGET_SAMPLE_RATE / inputSampleRate);
  if (outputSamples === 0) throw new Error("录音中没有音频样本");
  const framedSamples = Math.ceil(outputSamples / SAMPLES_PER_FRAME) * SAMPLES_PER_FRAME;
  const bytes = new Uint8Array(framedSamples * 2);
  const view = new DataView(bytes.buffer);
  const ratio = inputSampleRate / TARGET_SAMPLE_RATE;

  for (let index = 0; index < outputSamples; index += 1) {
    const start = Math.floor(index * ratio);
    const end = Math.max(start + 1, Math.min(samples.length, Math.floor((index + 1) * ratio)));
    let sum = 0;
    for (let sourceIndex = start; sourceIndex < end; sourceIndex += 1) {
      sum += samples[sourceIndex] ?? 0;
    }
    const normalized = Math.max(-1, Math.min(1, sum / (end - start)));
    const signed = normalized < 0
      ? Math.round(normalized * 32_768)
      : Math.round(normalized * 32_767);
    view.setInt16(index * 2, signed, true);
  }
  return bytes;
}

function parseRecorderEvent(value: unknown): RecorderWorkletEvent | undefined {
  if (!isRecord(value) || typeof value.type !== "string") return undefined;
  if (value.type === "maximum" || value.type === "processor_error") return { type: value.type };
  if (value.type === "samples" && value.samples instanceof Float32Array) {
    return { type: "samples", samples: value.samples };
  }
  return undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function joinChunks(chunks: Float32Array[], sampleCount: number): Float32Array {
  const samples = new Float32Array(sampleCount);
  let offset = 0;
  for (const chunk of chunks) {
    samples.set(chunk, offset);
    offset += chunk.length;
  }
  return samples;
}
