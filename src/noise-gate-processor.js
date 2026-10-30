class SafeDropNoiseGate extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const initial = options.processorOptions || {};
    this.threshold = Math.max(0, Number(initial.threshold) || 0);
    this.smooth = Math.min(100, Math.max(0, Number(initial.smooth) || 0));
    this.envelope = 0;
    this.gain = 1;
    this.port.onmessage = (event) => {
      const data = event.data || {};
      if (Number.isFinite(data.threshold)) this.threshold = Math.max(0, data.threshold);
      if (Number.isFinite(data.smooth)) this.smooth = Math.min(100, Math.max(0, data.smooth));
    };
  }

  process(inputs, outputs) {
    const input = inputs[0]?.[0];
    const output = outputs[0]?.[0];
    if (!output) return true;
    if (!input) { output.fill(0); return true; }

    let sum = 0;
    for (let i = 0; i < input.length; i++) sum += input[i] * input[i];
    const rms = Math.sqrt(sum / Math.max(1, input.length));
    const envelopeTime = rms > this.envelope ? 0.008 : 0.085;
    const envelopeCoeff = Math.exp(-input.length / (sampleRate * envelopeTime));
    this.envelope = rms + envelopeCoeff * (this.envelope - rms);

    const threshold = this.threshold;
    let target = 1;
    if (threshold > 0) {
      // A soft knee avoids the hard on/off discontinuity that caused clicks
      // and clipped the first consonants of quiet speech.
      const floor = 0.08;
      const kneeStart = threshold * 0.45;
      const kneeEnd = threshold * 1.35;
      const x = Math.max(0, Math.min(1, (this.envelope - kneeStart) / (kneeEnd - kneeStart)));
      const smoothX = x * x * (3 - 2 * x);
      target = floor + (1 - floor) * smoothX;
    }
    const attack = 0.006 + (this.smooth / 100) * 0.018;
    const release = 0.12 + (this.smooth / 100) * 0.28;
    const rampTime = target > this.gain ? attack : release;
    const coefficient = Math.exp(-1 / (sampleRate * rampTime));

    for (let i = 0; i < input.length; i++) {
      this.gain = target + coefficient * (this.gain - target);
      output[i] = input[i] * this.gain;
    }
    return true;
  }
}

registerProcessor("safedrop-noise-gate", SafeDropNoiseGate);
