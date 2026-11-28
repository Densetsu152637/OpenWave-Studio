import type { SignatureChangeJson, TempoPointJson } from "../ipc/types";

/** Beats are quarter notes everywhere, as in the model. */
const DEFAULT_BPM = 120;

/**
 * The project tempo map: constant and linear-ramp segments, converting beats to seconds and back.
 * Mirrors `ow_model::time::TempoMap` (a ramp is linear in beats, so a segment's time is
 * `60 L / (b1 - b0) * ln(b1 / b0)`).
 */
export class TempoMap {
  readonly #points: readonly TempoPointJson[];
  /** Seconds elapsed at each point's beat. */
  readonly #seconds: readonly number[];

  constructor(points: readonly TempoPointJson[]) {
    this.#points = points.length > 0 ? points : [{ beat: 0, bpm: DEFAULT_BPM, ramp: false }];
    const seconds = [0];
    for (let i = 0; i + 1 < this.#points.length; i++) {
      const a = this.#points[i]!;
      const b = this.#points[i + 1]!;
      const len = b.beat - a.beat;
      seconds.push(seconds[i]! + segmentSeconds(a, b.bpm, len, len));
    }
    this.#seconds = seconds;
  }

  /** The tempo points, in beat order (at least one). */
  get points(): readonly TempoPointJson[] {
    return this.#points;
  }

  #segmentAtBeat(beat: number): number {
    let index = 0;
    while (index + 1 < this.#points.length && this.#points[index + 1]!.beat <= beat) index++;
    return index;
  }

  /** Seconds from beat 0 to `beat`. */
  beatsToSeconds(beat: number): number {
    const first = this.#points[0]!;
    if (beat < 0) return (beat * 60) / first.bpm;
    const index = this.#segmentAtBeat(beat);
    const point = this.#points[index]!;
    const next = this.#points[index + 1];
    const offset = beat - point.beat;
    const elapsed = next
      ? segmentSeconds(point, next.bpm, next.beat - point.beat, offset)
      : (offset * 60) / point.bpm;
    return this.#seconds[index]! + elapsed;
  }

  /** The beat reached `seconds` after beat 0. */
  secondsToBeats(seconds: number): number {
    const first = this.#points[0]!;
    if (seconds < 0) return (seconds * first.bpm) / 60;
    let index = 0;
    while (index + 1 < this.#seconds.length && this.#seconds[index + 1]! <= seconds) index++;
    const point = this.#points[index]!;
    const next = this.#points[index + 1];
    const into = seconds - this.#seconds[index]!;
    const offset = next
      ? segmentBeats(point, next.bpm, next.beat - point.beat, into)
      : (into * point.bpm) / 60;
    return point.beat + offset;
  }

  /** Beat at sample position `samples` of a stream at `sampleRate`. */
  samplesToBeats(samples: number, sampleRate: number): number {
    return this.secondsToBeats(samples / Math.max(1, sampleRate));
  }
}

function segmentSeconds(
  point: TempoPointJson,
  endBpm: number,
  len: number,
  offset: number,
): number {
  const delta = endBpm - point.bpm;
  if (point.ramp && delta !== 0) {
    return ((60 * len) / delta) * Math.log1p((delta * (offset / len)) / point.bpm);
  }
  return (offset * 60) / point.bpm;
}

function segmentBeats(
  point: TempoPointJson,
  endBpm: number,
  len: number,
  seconds: number,
): number {
  const delta = endBpm - point.bpm;
  const beats =
    point.ramp && delta !== 0
      ? ((len * point.bpm) / delta) * Math.expm1((seconds * delta) / (60 * len))
      : (seconds * point.bpm) / 60;
  return Math.min(len, Math.max(0, beats));
}

interface SignatureEntry {
  /** Bar (0-based) the signature starts at. */
  bar: number;
  /** Beat position of that bar's start. */
  beat: number;
  /** Bar length in beats. */
  length: number;
  /** Beats per bar (signature numerator). */
  numerator: number;
}

/** A bar of the timeline: 0-based index, start beat, length in beats. */
export interface Bar {
  index: number;
  start: number;
  length: number;
  /** Signature beats in the bar. */
  beats: number;
}

/** The time-signature map: where bars start. Mirrors `ow_model::time::TimeSignatureMap`. */
export class SignatureMap {
  readonly #entries: readonly SignatureEntry[];

  constructor(changes: readonly SignatureChangeJson[]) {
    const entries: SignatureEntry[] = [];
    const source = changes.length > 0 ? changes : [{ bar: 0, numerator: 4, denominator: 4 }];
    for (const change of source) {
      const previous = entries[entries.length - 1];
      const beat = previous ? previous.beat + (change.bar - previous.bar) * previous.length : 0;
      entries.push({
        bar: change.bar,
        beat,
        length: (4 / change.denominator) * change.numerator,
        numerator: change.numerator,
      });
    }
    this.#entries = entries;
  }

  #entryAtBeat(beat: number): SignatureEntry {
    let index = 0;
    while (index + 1 < this.#entries.length && this.#entries[index + 1]!.beat <= beat) index++;
    return this.#entries[index]!;
  }

  #entryAtBar(bar: number): SignatureEntry {
    let index = 0;
    while (index + 1 < this.#entries.length && this.#entries[index + 1]!.bar <= bar) index++;
    return this.#entries[index]!;
  }

  /** Beat position where `bar` (0-based) starts. */
  barStart(bar: number): number {
    const entry = this.#entryAtBar(bar);
    return entry.beat + (bar - entry.bar) * entry.length;
  }

  /** The bar that contains `beat` (negative beats belong to bar 0). */
  barAt(beat: number): Bar {
    const at = Math.max(0, Number.isFinite(beat) ? beat : 0);
    const entry = this.#entryAtBeat(at);
    // The small slack keeps a beat that is a bar start, up to float noise, in that bar.
    const index = entry.bar + Math.floor((at - entry.beat) / entry.length + 1e-9);
    return { index, start: this.barStart(index), length: entry.length, beats: entry.numerator };
  }

  /** The start of the bar nearest to `beat`; the earlier one on an exact tie. */
  nearestBarStart(beat: number): number {
    const bar = this.barAt(beat);
    const next = this.barStart(bar.index + 1);
    return Math.max(0, beat - bar.start <= next - beat ? bar.start : next);
  }
}
