export type RecordedAudioSample = Readonly<{
  wav_bytes: Uint8Array;
  base64: string;
  label: string;
}>;

const TARGET_SAMPLE_RATE = 16000;

export function encodePcm16MonoWav(samples: Float32Array, sampleRate = TARGET_SAMPLE_RATE): Uint8Array {
  const pcmLen = samples.length * 2;
  const buffer = new ArrayBuffer(44 + pcmLen);
  const view = new DataView(buffer);

  writeAscii(view, 0, "RIFF");
  view.setUint32(4, 36 + pcmLen, true);
  writeAscii(view, 8, "WAVE");
  writeAscii(view, 12, "fmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  writeAscii(view, 36, "data");
  view.setUint32(40, pcmLen, true);

  let offset = 44;
  for (let index = 0; index < samples.length; index += 1) {
    const clamped = Math.max(-1, Math.min(1, samples[index] ?? 0));
    view.setInt16(offset, clamped < 0 ? clamped * 0x8000 : clamped * 0x7fff, true);
    offset += 2;
  }
  return new Uint8Array(buffer);
}

export function downsampleTo16kMono(input: Float32Array, sourceRate: number): Float32Array {
  if (sourceRate === TARGET_SAMPLE_RATE) return input;
  const ratio = sourceRate / TARGET_SAMPLE_RATE;
  const outLength = Math.max(1, Math.round(input.length / ratio));
  const out = new Float32Array(outLength);
  for (let index = 0; index < outLength; index += 1) {
    const start = Math.floor(index * ratio);
    const end = Math.min(input.length, Math.floor((index + 1) * ratio));
    let sum = 0;
    let count = 0;
    for (let cursor = start; cursor < end; cursor += 1) {
      sum += input[cursor] ?? 0;
      count += 1;
    }
    out[index] = count > 0 ? sum / count : (input[start] ?? 0);
  }
  return out;
}

export function synthesizeTestToneSample(fundamentalHz: number): RecordedAudioSample {
  const sampleCount = 24000;
  const samples = new Float32Array(sampleCount);
  for (let index = 0; index < sampleCount; index += 1) {
    const t = index / TARGET_SAMPLE_RATE;
    samples[index] =
      0.45 * Math.sin(2 * Math.PI * fundamentalHz * t) +
      0.25 * Math.sin(2 * Math.PI * fundamentalHz * 3 * t);
  }
  const wav_bytes = encodePcm16MonoWav(samples, TARGET_SAMPLE_RATE);
  return {
    wav_bytes,
    base64: bytesToBase64(wav_bytes),
    label: `合成样本 ${fundamentalHz}Hz`,
  };
}

export function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunkSize = 0x4000;
  for (let offset = 0; offset < bytes.length; offset += chunkSize) {
    const slice = bytes.subarray(offset, offset + chunkSize);
    binary += String.fromCharCode(...slice);
  }
  return btoa(binary);
}

export class BrowserWavRecorder {
  #stream: MediaStream | null = null;
  #context: AudioContext | null = null;
  #processor: ScriptProcessorNode | null = null;
  #source: MediaStreamAudioSourceNode | null = null;
  #chunks: Float32Array[] = [];

  async start(): Promise<void> {
    if (!navigator.mediaDevices?.getUserMedia) {
      throw new Error("当前环境不支持麦克风采集。");
    }
    const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    const AudioCtx = window.AudioContext;
    if (!AudioCtx) {
      stream.getTracks().forEach((track) => track.stop());
      throw new Error("当前浏览器不支持 AudioContext。");
    }
    const context = new AudioCtx();
    const source = context.createMediaStreamSource(stream);
    const processor = context.createScriptProcessor(4096, 1, 1);
    this.#chunks = [];
    processor.onaudioprocess = (event) => {
      const channel = event.inputBuffer.getChannelData(0);
      this.#chunks.push(new Float32Array(channel));
    };
    source.connect(processor);
    processor.connect(context.destination);

    this.#stream = stream;
    this.#context = context;
    this.#source = source;
    this.#processor = processor;
  }

  async stop(): Promise<RecordedAudioSample> {
    const sourceRate = this.#context?.sampleRate ?? TARGET_SAMPLE_RATE;
    this.#processor?.disconnect();
    this.#source?.disconnect();
    this.#stream?.getTracks().forEach((track) => track.stop());
    if (this.#context) {
      await this.#context.close();
    }
    this.#processor = null;
    this.#source = null;
    this.#stream = null;
    this.#context = null;

    const totalLen = this.#chunks.reduce((acc, chunk) => acc + chunk.length, 0);
    const merged = new Float32Array(totalLen);
    let offset = 0;
    for (const chunk of this.#chunks) {
      merged.set(chunk, offset);
      offset += chunk.length;
    }
    this.#chunks = [];

    const pcm16k = downsampleTo16kMono(merged, sourceRate);
    const wav_bytes = encodePcm16MonoWav(pcm16k, TARGET_SAMPLE_RATE);
    return {
      wav_bytes,
      base64: bytesToBase64(wav_bytes),
      label: `麦克风录音 (${(pcm16k.length / TARGET_SAMPLE_RATE).toFixed(1)}s)`,
    };
  }

  dispose(): void {
    this.#processor?.disconnect();
    this.#source?.disconnect();
    this.#stream?.getTracks().forEach((track) => track.stop());
    void this.#context?.close();
    this.#processor = null;
    this.#source = null;
    this.#stream = null;
    this.#context = null;
    this.#chunks = [];
  }
}

function writeAscii(view: DataView, offset: number, text: string): void {
  for (let index = 0; index < text.length; index += 1) {
    view.setUint8(offset + index, text.charCodeAt(index));
  }
}
