/* global AudioWorkletProcessor, registerProcessor */

class PcmRecorderProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const requestedMaximum = options.processorOptions?.maximumSamples;
    this.maximumSamples = Number.isInteger(requestedMaximum) && requestedMaximum > 0
      ? requestedMaximum
      : Number.MAX_SAFE_INTEGER;
    this.recordedSamples = 0;
    this.maximumNotified = false;
    this.closed = false;
    this.port.onmessage = (message) => {
      if (message.data?.type === "close") {
        this.closed = true;
        this.port.close();
      }
    };
  }

  process(inputs, outputs) {
    const output = outputs[0]?.[0];
    output?.fill(0);
    if (this.closed) return false;
    const input = inputs[0]?.[0];
    if (!input || input.length === 0 || this.recordedSamples >= this.maximumSamples) return true;
    const remaining = this.maximumSamples - this.recordedSamples;
    const samples = input.slice(0, Math.min(input.length, remaining));
    this.recordedSamples += samples.length;
    this.port.postMessage({ type: "samples", samples }, [samples.buffer]);
    if (this.recordedSamples >= this.maximumSamples && !this.maximumNotified) {
      this.maximumNotified = true;
      this.port.postMessage({ type: "maximum" });
    }
    return true;
  }
}

registerProcessor("ez-assistant-pcm-recorder", PcmRecorderProcessor);
