/* global AudioWorkletProcessor, registerProcessor, sampleRate */

class PcmPlaybackProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const processorOptions = options.processorOptions ?? {};
    this.initialBufferSamples = positiveInteger(
      processorOptions.initialBufferSamples,
      Math.round(sampleRate * 0.12),
    );
    this.recoveryBufferSamples = positiveInteger(
      processorOptions.recoveryBufferSamples,
      Math.round(sampleRate * 0.08),
    );
    this.maxBufferSamples = positiveInteger(
      processorOptions.maxBufferSamples,
      Math.round(sampleRate * 90),
    );
    this.fadeSamples = positiveInteger(
      processorOptions.fadeSamples,
      Math.round(sampleRate * 0.005),
    );
    this.items = [];
    this.head = 0;
    this.itemOffset = 0;
    this.queuedSamples = 0;
    this.markerCount = 0;
    this.playing = false;
    this.bufferThreshold = this.initialBufferSamples;
    this.underflowReported = false;
    this.overflowReported = false;
    this.fadeInRemaining = 0;
    this.closed = false;
    this.port.onmessage = (message) => this.handleMessage(message.data);
  }

  handleMessage(message) {
    if (!message || typeof message.type !== "string") return;
    if (message.type === "samples" && message.samples instanceof Float32Array) {
      this.enqueueSamples(message.samples);
      return;
    }
    if (message.type === "silence" && Number.isInteger(message.sampleCount)) {
      this.enqueueSilence(message.sampleCount);
      return;
    }
    if (message.type === "marker" && validMarker(message.marker)) {
      this.items.push({ type: "marker", marker: message.marker });
      this.markerCount += 1;
      return;
    }
    if (message.type === "segment_start") {
      this.items.push({ type: "segment_start" });
      return;
    }
    if (message.type === "clear") {
      this.clear();
      return;
    }
    if (message.type === "close") {
      this.clear();
      this.closed = true;
      this.port.close();
    }
  }

  enqueueSamples(samples) {
    if (samples.length === 0) return;
    if (!this.reserve(samples.length)) return;
    this.items.push({ type: "samples", samples });
    this.queuedSamples += samples.length;
  }

  enqueueSilence(sampleCount) {
    if (sampleCount <= 0 || !this.reserve(sampleCount)) return;
    this.items.push({ type: "silence", sampleCount });
    this.queuedSamples += sampleCount;
  }

  reserve(sampleCount) {
    if (this.queuedSamples + sampleCount <= this.maxBufferSamples) return true;
    if (!this.overflowReported) {
      this.overflowReported = true;
      this.port.postMessage({ type: "overflow" });
    }
    return false;
  }

  clear() {
    this.items.length = 0;
    this.head = 0;
    this.itemOffset = 0;
    this.queuedSamples = 0;
    this.markerCount = 0;
    this.playing = false;
    this.bufferThreshold = this.initialBufferSamples;
    this.underflowReported = false;
    this.overflowReported = false;
    this.fadeInRemaining = 0;
  }

  process(_inputs, outputs) {
    if (this.closed) return false;
    const output = outputs[0]?.[0];
    if (!output) return true;
    output.fill(0);

    this.drainControlItems();
    if (!this.playing) {
      if (this.queuedSamples < this.bufferThreshold && this.markerCount === 0) return true;
      this.playing = true;
      this.underflowReported = false;
    }

    let samplesToMarker = this.samplesUntilNextMarker();
    for (let index = 0; index < output.length; index += 1) {
      const drainedMarker = this.drainControlItems();
      const sample = this.takeSample();
      if (sample !== undefined) {
        let gain = 1;
        if (this.fadeInRemaining > 0) {
          gain = Math.min(gain, (this.fadeSamples - this.fadeInRemaining + 1) / this.fadeSamples);
          this.fadeInRemaining -= 1;
        }
        if (samplesToMarker !== undefined && samplesToMarker <= this.fadeSamples) {
          gain = Math.min(gain, samplesToMarker / this.fadeSamples);
        }
        output[index] = sample * gain;
        if (samplesToMarker !== undefined) samplesToMarker -= 1;
        continue;
      }
      const drainedAtEnd = this.drainControlItems() || drainedMarker;
      this.playing = false;
      this.bufferThreshold = drainedAtEnd
        ? this.initialBufferSamples
        : this.recoveryBufferSamples;
      if (!drainedAtEnd && !this.underflowReported) {
        this.underflowReported = true;
        this.port.postMessage({ type: "underflow" });
      }
      break;
    }
    this.compactQueue();
    return true;
  }

  drainControlItems() {
    let drained = false;
    while (
      this.items[this.head]?.type === "marker"
      || this.items[this.head]?.type === "segment_start"
    ) {
      const item = this.items[this.head];
      this.head += 1;
      this.itemOffset = 0;
      if (item.type === "segment_start") {
        this.fadeInRemaining = this.fadeSamples;
        continue;
      }
      this.markerCount -= 1;
      this.port.postMessage({ type: "marker_drained", marker: item.marker });
      drained = true;
    }
    return drained;
  }

  samplesUntilNextMarker() {
    let samples = -this.itemOffset;
    for (let index = this.head; index < this.items.length; index += 1) {
      const item = this.items[index];
      if (item.type === "marker") return Math.max(0, samples);
      if (item.type === "samples") samples += item.samples.length;
      if (item.type === "silence") samples += item.sampleCount;
    }
    return undefined;
  }

  takeSample() {
    const item = this.items[this.head];
    if (!item || item.type === "marker") return undefined;
    let sample = 0;
    let length = 0;
    if (item.type === "samples") {
      sample = item.samples[this.itemOffset] ?? 0;
      length = item.samples.length;
    } else {
      length = item.sampleCount;
    }
    this.itemOffset += 1;
    this.queuedSamples -= 1;
    if (this.itemOffset >= length) {
      this.head += 1;
      this.itemOffset = 0;
    }
    return sample;
  }

  compactQueue() {
    if (this.head === 0) return;
    if (this.head >= this.items.length) {
      this.items.length = 0;
      this.head = 0;
      return;
    }
    if (this.head >= 256) {
      this.items.splice(0, this.head);
      this.head = 0;
    }
  }
}

function positiveInteger(value, fallback) {
  return Number.isInteger(value) && value > 0 ? value : fallback;
}

function validMarker(value) {
  return value
    && typeof value.outputId === "string"
    && Number.isInteger(value.streamId);
}

registerProcessor("ez-assistant-pcm-playback", PcmPlaybackProcessor);
