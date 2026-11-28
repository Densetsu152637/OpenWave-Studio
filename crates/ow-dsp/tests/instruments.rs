//! Reference patches for the two built-in synthesizers: pitch, spectra, filter and modulation
//! behavior, determinism, independence from block size, state, parameter smoothing and realtime
//! safety. Run with `cargo test -p ow-dsp instruments`.

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use ow_dsp::instruments::fm::{self, FmSynth, OpField, op_param};
use ow_dsp::instruments::synth::{self as sub, Synth};
use ow_dsp::{Device, Event, ParamBank, ParamDescriptor, StateError, find};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

const RATE: f32 = 48_000.0;

fn render(dev: &mut dyn Device, events: &[Event], frames: usize) -> (Vec<f32>, Vec<f32>) {
    let mut left = vec![0.5; frames];
    let mut right = vec![0.5; frames];
    dev.process(&mut [&mut left, &mut right], events);
    (left, right)
}

fn set(dev: &mut dyn Device, params: &[(u32, f64)]) {
    for (id, value) in params {
        dev.set_param(*id, *value);
    }
}

/// A flat subtractive patch: sine, filter open, no resonance, instant envelope, 0 dB.
fn flat_synth(waveform: f64) -> Synth {
    let mut synth = Synth::new();
    synth.prepare(RATE, 4096);
    set(
        &mut synth,
        &[
            (sub::PARAM_WAVEFORM, waveform),
            (sub::PARAM_CUTOFF, 20_000.0),
            (sub::PARAM_RESONANCE, 0.0),
            (sub::PARAM_ATTACK, 0.001),
            (sub::PARAM_DECAY, 0.001),
            (sub::PARAM_SUSTAIN, 1.0),
            (sub::PARAM_RELEASE, 0.05),
            (sub::PARAM_GAIN, 0.0),
        ],
    );
    synth
}

/// A flat FM patch: operator 1 alone, a sine at the note, full sustain, 0 dB.
fn flat_fm() -> FmSynth {
    let mut synth = FmSynth::new();
    synth.prepare(RATE, 4096);
    set(&mut synth, &[(fm::PARAM_GAIN, 0.0)]);
    for op in 0..4 {
        set(
            &mut synth,
            &[
                (op_param(op, OpField::Attack), 0.001),
                (op_param(op, OpField::Decay), 0.001),
                (op_param(op, OpField::Sustain), 1.0),
                (op_param(op, OpField::Release), 0.05),
                (op_param(op, OpField::VelocitySensitivity), 0.0),
                (
                    op_param(op, OpField::Level),
                    if op == 0 { 1.0 } else { 0.0 },
                ),
            ],
        );
    }
    synth
}

/// `signal` cut into whole windows of `size` frames.
fn blocks(signal: &[f32], size: usize) -> std::slice::ChunksExact<'_, f32> {
    signal.chunks_exact(size)
}

fn hz(key: u8) -> f64 {
    440.0 * 2.0_f64.powf((f64::from(key) - 69.0) / 12.0)
}

/// Magnitude spectrum of a Hann-windowed `signal` (a power-of-two length).
fn spectrum(signal: &[f32]) -> Vec<f64> {
    let n = signal.len();
    assert!(n.is_power_of_two());
    let mut re: Vec<f64> = signal
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let w = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos();
            f64::from(*s) * w
        })
        .collect();
    let mut im = vec![0.0; n];
    let bits = n.trailing_zeros();
    for i in 0..n {
        let j = (i as u32).reverse_bits() >> (32 - bits);
        if (j as usize) > i {
            re.swap(i, j as usize);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -std::f64::consts::TAU / len as f64;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (wr, wi) = ((angle * k as f64).cos(), (angle * k as f64).sin());
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * wr - im[b] * wi, re[b] * wi + im[b] * wr);
                (re[b], im[b]) = (re[a] - tr, im[a] - ti);
                (re[a], im[a]) = (re[a] + tr, im[a] + ti);
            }
        }
        len *= 2;
    }
    re.iter()
        .zip(&im)
        .take(n / 2)
        .map(|(r, i)| (r * r + i * i).sqrt() / n as f64)
        .collect()
}

const FFT: usize = 32_768;

/// Spectrum of the steady part of a render (after `skip` frames).
fn steady(signal: &[f32], skip: usize) -> Vec<f64> {
    spectrum(&signal[skip..skip + FFT])
}

/// The largest magnitude within 1 percent of `freq`.
fn at(spec: &[f64], freq: f64) -> f64 {
    let bin = |f: f64| f * FFT as f64 / f64::from(RATE);
    let (lo, hi) = ((bin(freq * 0.99)) as usize, (bin(freq * 1.01)) as usize + 1);
    spec[lo..=hi].iter().fold(0.0_f64, |m, v| m.max(*v))
}

fn peak_hz(spec: &[f64]) -> f64 {
    let (index, _) = spec
        .iter()
        .enumerate()
        .skip(2)
        .max_by(|a, b| a.1.total_cmp(b.1))
        .unwrap();
    index as f64 * f64::from(RATE) / FFT as f64
}

fn rms(signal: &[f32]) -> f64 {
    (signal.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / signal.len() as f64).sqrt()
}

fn peak(signal: &[f32]) -> f32 {
    signal.iter().fold(0.0, |m, s| m.max(s.abs()))
}

fn db(ratio: f64) -> f64 {
    20.0 * ratio.log10()
}

fn cents(measured: f64, expected: f64) -> f64 {
    1200.0 * (measured / expected).log2()
}

/// Frequency from positive-going zero crossings.
fn measure_hz(signal: &[f32]) -> f64 {
    let mut crossings = Vec::new();
    for (i, pair) in signal.windows(2).enumerate() {
        if pair[0] <= 0.0 && pair[1] > 0.0 {
            crossings.push(i as f64 + f64::from(-pair[0]) / f64::from(pair[1] - pair[0]));
        }
    }
    assert!(crossings.len() > 4, "too few cycles");
    (crossings.len() - 1) as f64 * f64::from(RATE) / (crossings.last().unwrap() - crossings[0])
}

fn note(frames: usize, key: u8) -> (Vec<Event>, usize) {
    (vec![Event::note_on(0, key, 127)], frames)
}

// ----- subtractive synth ---------------------------------------------------------------------

#[test]
fn instruments_subtractive_triangle_has_the_right_pitch_and_level() {
    let mut synth = flat_synth(3.0);
    let (events, frames) = note(48_000, 57);
    let (out, _) = render(&mut synth, &events, frames);
    let error = cents(measure_hz(&out[4800..]), 220.0);
    assert!(error.abs() < 1.0, "{error} cents");
    let p = peak(&out[4800..]);
    assert!((0.95..=1.05).contains(&p), "triangle peak {p}");
    // A triangle has only odd harmonics, falling as 1/n^2.
    let spec = steady(&out, 8000);
    let h1 = at(&spec, 220.0);
    let h3 = at(&spec, 660.0);
    assert!(
        (db(h3 / h1) + 19.1).abs() < 1.5,
        "third harmonic {} dB",
        db(h3 / h1)
    );
    assert!(at(&spec, 440.0) < h1 * 0.01, "no even harmonics");
}

#[test]
fn instruments_subtractive_oscillator_two_tuning_and_sub_place_spectral_peaks() {
    // Oscillator 1 and 2 are sines, 2 an octave up and 7 semitones above that.
    let mut synth = flat_synth(2.0);
    set(
        &mut synth,
        &[
            (sub::PARAM_OSC1_LEVEL, 0.5),
            (sub::PARAM_OSC2_WAVEFORM, 2.0),
            (sub::PARAM_OSC2_LEVEL, 0.5),
            (sub::PARAM_OSC2_OCTAVE, 3.0),
            (sub::PARAM_OSC2_SEMITONE, 7.0),
        ],
    );
    let (events, frames) = note(60_000, 57);
    let (out, _) = render(&mut synth, &events, frames);
    let spec = steady(&out, 8000);
    let (f1, f2) = (220.0, 220.0 * 2.0 * 2.0_f64.powf(7.0 / 12.0));
    let (a1, a2) = (at(&spec, f1), at(&spec, f2));
    assert!((a1 / a2 - 1.0).abs() < 0.1, "equal levels: {a1} {a2}");
    assert!(at(&spec, 330.0) < a1 * 0.01, "nothing between the peaks");

    // The sub alone: a sine an octave below, then two octaves, then a square (odd harmonics).
    for (octave, expected) in [(0.0, 110.0), (1.0, 55.0)] {
        let mut synth = flat_synth(2.0);
        set(
            &mut synth,
            &[
                (sub::PARAM_OSC1_LEVEL, 0.0),
                (sub::PARAM_SUB_LEVEL, 1.0),
                (sub::PARAM_SUB_OCTAVE, octave),
            ],
        );
        let (out, _) = render(&mut synth, &events, frames);
        assert!((peak_hz(&steady(&out, 8000)) - expected).abs() < 1.0);
    }
    let mut synth = flat_synth(2.0);
    set(
        &mut synth,
        &[
            (sub::PARAM_OSC1_LEVEL, 0.0),
            (sub::PARAM_SUB_LEVEL, 1.0),
            (sub::PARAM_SUB_WAVEFORM, 1.0),
        ],
    );
    let (out, _) = render(&mut synth, &events, frames);
    let spec = steady(&out, 8000);
    assert!(
        at(&spec, 330.0) > at(&spec, 110.0) * 0.25,
        "square has a third harmonic"
    );
    assert!(at(&spec, 220.0) < at(&spec, 110.0) * 0.02, "and no second");
}

#[test]
fn instruments_subtractive_filter_types_cut_where_they_should() {
    let level = |filter_type: f64, cutoff: f64, hz_probe: f64| {
        let mut synth = flat_synth(0.0);
        set(
            &mut synth,
            &[
                (sub::PARAM_FILTER_TYPE, filter_type),
                (sub::PARAM_CUTOFF, cutoff),
            ],
        );
        let (events, frames) = note(60_000, 45); // 110 Hz saw
        let (out, _) = render(&mut synth, &events, frames);
        at(&steady(&out, 8000), hz_probe)
    };
    let open = level(0.0, 20_000.0, 2200.0);
    // Low-pass at 500 Hz: the 20th harmonic (2200 Hz) falls at least 24 dB (2 octaves, 12 dB/oct).
    assert!(db(level(0.0, 500.0, 2200.0) / open) < -24.0);
    assert!(db(level(0.0, 500.0, 110.0) / level(0.0, 20_000.0, 110.0)) > -1.5);
    // High-pass at 2 kHz: the fundamental falls, the 20th harmonic stays.
    assert!(db(level(1.0, 2000.0, 110.0) / level(0.0, 20_000.0, 110.0)) < -30.0);
    assert!(db(level(1.0, 2000.0, 2200.0) / open) > -3.0);
    // Band-pass at 2.2 kHz: both far ends fall.
    assert!(db(level(2.0, 2200.0, 110.0) / level(0.0, 20_000.0, 110.0)) < -20.0);
    assert!(db(level(2.0, 2200.0, 2200.0) / open) > -6.0);
}

#[test]
fn instruments_subtractive_filter_envelope_and_key_tracking_move_the_cutoff() {
    // A positive envelope amount opens the filter, then it closes as the envelope decays.
    let mut synth = flat_synth(0.0);
    set(
        &mut synth,
        &[
            (sub::PARAM_CUTOFF, 200.0),
            (sub::PARAM_FILTER_ENV_AMOUNT, 60.0),
            (sub::PARAM_FILTER_ATTACK, 0.001),
            (sub::PARAM_FILTER_DECAY, 0.15),
            (sub::PARAM_FILTER_SUSTAIN, 0.0),
        ],
    );
    let (events, frames) = note(48_000, 45);
    let (out, _) = render(&mut synth, &events, frames);
    let bright = |from: usize| {
        let window = &out[from..from + 2048];
        // Energy above the 4th harmonic, as a share of the whole: a crude spectral centroid.
        let spec = spectrum(window);
        let high: f64 = spec[(1000.0 * 2048.0 / 48_000.0) as usize..].iter().sum();
        let all: f64 = spec.iter().sum();
        high / all
    };
    assert!(bright(480) > 0.04, "early {}", bright(480));
    assert!(bright(36_000) < 0.02, "late {}", bright(36_000));
    assert!(bright(480) > bright(36_000) * 3.0);

    // Key tracking at 1 keeps the filter's place against the notes: an octave up passes the
    // same share of its harmonics. Without it the higher note is darker.
    let share = |track: f64, key: u8| {
        let mut synth = flat_synth(0.0);
        set(
            &mut synth,
            &[(sub::PARAM_CUTOFF, 400.0), (sub::PARAM_KEY_TRACK, track)],
        );
        let (events, frames) = note(24_000, key);
        let (out, _) = render(&mut synth, &events, frames);
        let mut open = flat_synth(0.0);
        let (full, _) = render(&mut open, &events, frames);
        rms(&out[4000..]) / rms(&full[4000..])
    };
    assert!(
        (share(1.0, 45) - share(1.0, 69)).abs() < 0.03,
        "tracked: {} {}",
        share(1.0, 45),
        share(1.0, 69)
    );
    assert!(share(0.0, 45) - share(0.0, 69) > 0.1, "untracked");
}

#[test]
fn instruments_subtractive_lfo_modulates_pitch_cutoff_and_amplitude() {
    let windows = |signal: &[f32], size: usize| -> Vec<(f64, f64)> {
        let sounding = signal.chunks_exact(size).filter(|w| rms(w) > 0.05);
        sounding.map(|w| (measure_hz(w), rms(w))).collect()
    };
    let rms_windows =
        |signal: &[f32], size: usize| -> Vec<f64> { signal.chunks_exact(size).map(rms).collect() };
    // Vibrato: a 5 Hz sine LFO at one semitone swings the pitch from -1 to +1 semitone.
    let mut synth = flat_synth(2.0);
    set(
        &mut synth,
        &[
            (sub::PARAM_LFO_RATE, 5.0),
            (sub::PARAM_LFO_PITCH, 1.0),
            (sub::PARAM_LFO_SHAPE, 0.0),
        ],
    );
    let (events, frames) = note(48_000, 69);
    let (out, _) = render(&mut synth, &events, frames);
    let freqs: Vec<f64> = windows(&out, 960).iter().map(|w| w.0).collect();
    let (lo, hi) = freqs
        .iter()
        .fold((f64::MAX, f64::MIN), |(a, b), f| (a.min(*f), b.max(*f)));
    assert!(
        cents(hi, 440.0) > 70.0 && cents(hi, 440.0) < 105.0,
        "{}",
        cents(hi, 440.0)
    );
    assert!(
        cents(lo, 440.0) < -70.0 && cents(lo, 440.0) > -105.0,
        "{}",
        cents(lo, 440.0)
    );

    // Tremolo: full depth takes the level to zero twice... once per cycle, at 4 Hz.
    let mut synth = flat_synth(2.0);
    set(
        &mut synth,
        &[
            (sub::PARAM_LFO_RATE, 4.0),
            (sub::PARAM_LFO_AMP, 1.0),
            (sub::PARAM_LFO_SHAPE, 1.0),
        ],
    );
    let (out, _) = render(&mut synth, &events, frames);
    let levels = rms_windows(&out, 480);
    let (min, max) = levels
        .iter()
        .fold((f64::MAX, f64::MIN), |(a, b), l| (a.min(*l), b.max(*l)));
    assert!(min < 0.1 * max, "tremolo depth: {min} {max}");
    assert!(max > 0.65, "full level at the top of the cycle: {max}");

    // Cutoff: a square LFO at 24 semitones alternates a dark and a bright note.
    let mut synth = flat_synth(0.0);
    set(
        &mut synth,
        &[
            (sub::PARAM_CUTOFF, 400.0),
            (sub::PARAM_LFO_RATE, 2.0),
            (sub::PARAM_LFO_CUTOFF, 24.0),
            (sub::PARAM_LFO_SHAPE, 2.0),
        ],
    );
    let (out, _) = render(&mut synth, &events, frames);
    let levels: Vec<f64> = blocks(&out, 2400).map(rms).collect();
    let (min, max) = levels
        .iter()
        .skip(2)
        .fold((f64::MAX, f64::MIN), |(a, b), l| (a.min(*l), b.max(*l)));
    assert!(max > min * 1.8, "cutoff swing: {min} {max}");
}

#[test]
fn instruments_subtractive_unison_detunes_and_spreads_across_the_stereo_field() {
    let mut synth = flat_synth(2.0);
    set(
        &mut synth,
        &[
            (sub::PARAM_UNISON_VOICES, 2.0), // three copies
            (sub::PARAM_UNISON_DETUNE, 50.0),
            (sub::PARAM_UNISON_SPREAD, 1.0),
        ],
    );
    let (events, frames) = note(60_000, 57);
    let (left, right) = render(&mut synth, &events, frames);
    let (sl, sr) = (steady(&left, 8000), steady(&right, 8000));
    let (down, up) = (
        220.0 * 2.0_f64.powf(-0.5 / 12.0),
        220.0 * 2.0_f64.powf(0.5 / 12.0),
    );
    for spec in [&sl, &sr] {
        assert!(at(spec, 220.0) > 0.0);
    }
    // The flat copy is centered; the lower copy leans left and the upper one right.
    assert!(
        (at(&sl, 220.0) / at(&sr, 220.0) - 1.0).abs() < 0.05,
        "{} {}",
        at(&sl, 220.0),
        at(&sr, 220.0)
    );
    assert!(
        at(&sl, down) > at(&sr, down) * 5.0,
        "{} {}",
        at(&sl, down),
        at(&sr, down)
    );
    assert!(at(&sr, up) > at(&sl, up) * 5.0);
    // Three equal copies, each at about 1/sqrt(3) of a unit sine, in the pan law.
    assert!(at(&sl, down) > at(&sl, 220.0) * 0.8);
    // Spread 0 puts everything in the middle.
    set(&mut synth, &[(sub::PARAM_UNISON_SPREAD, 0.0)]);
    synth.reset();
    let (left, right) = render(&mut synth, &events, frames);
    assert_eq!(left, right);
    // Unison is level-normalised: seven copies are not seven times louder.
    set(&mut synth, &[(sub::PARAM_UNISON_VOICES, 6.0)]);
    synth.reset();
    let (seven, _) = render(&mut synth, &events, frames);
    assert!(rms(&seven[8000..]) < rms(&left[8000..]) * 1.5);
}

#[test]
fn instruments_subtractive_noise_is_deterministic_and_broadband() {
    let patch = || {
        let mut synth = flat_synth(2.0);
        set(
            &mut synth,
            &[(sub::PARAM_OSC1_LEVEL, 0.0), (sub::PARAM_NOISE_LEVEL, 0.5)],
        );
        synth
    };
    let (events, frames) = note(48_000, 60);
    let (a, _) = render(&mut patch(), &events, frames);
    let (b, _) = render(&mut patch(), &events, frames);
    assert_eq!(a, b, "same noise on every run");
    // Uniform noise of amplitude 0.5 has an RMS of 0.5 / sqrt(3).
    let expected = 0.5 / 3.0_f64.sqrt();
    let measured = rms(&a[4800..]);
    assert!(
        (measured / expected - 1.0).abs() < 0.12,
        "{measured} vs {expected}"
    );
    let spec = steady(&a, 8000);
    let band = |lo: usize, hi: usize| spec[lo..hi].iter().sum::<f64>() / (hi - lo) as f64;
    let (low, high) = (band(100, 1000), band(8000, 9000));
    assert!(
        (low / high - 1.0).abs() < 0.5,
        "flat spectrum: {low} {high}"
    );
}

// ----- FM synth ------------------------------------------------------------------------------

#[test]
fn instruments_fm_carrier_plays_the_note_and_ratios_scale_it() {
    for key in [36_u8, 57, 69, 84, 96] {
        let mut synth = flat_fm();
        let (out, _) = render(&mut synth, &[Event::note_on(0, key, 127)], 48_000);
        let error = cents(measure_hz(&out[4800..]), hz(key));
        assert!(error.abs() < 1.0, "key {key}: {error} cents");
    }
    // Operator 1 at ratio 3 sounds a twelfth up.
    let mut synth = flat_fm();
    set(&mut synth, &[(op_param(0, OpField::Ratio), 6.0)]);
    let (out, _) = render(&mut synth, &[Event::note_on(0, 57, 127)], 48_000);
    assert!(cents(measure_hz(&out[4800..]), 660.0).abs() < 1.0);
    // Fine detune in cents.
    set(&mut synth, &[(op_param(0, OpField::Fine), 25.0)]);
    synth.reset();
    let (out, _) = render(&mut synth, &[Event::note_on(0, 57, 127)], 48_000);
    assert!((cents(measure_hz(&out[4800..]), 660.0) - 25.0).abs() < 1.0);
}

#[test]
fn instruments_fm_modulator_adds_sidebands_where_the_ratios_say() {
    // Algorithm 1 (4>3>2>1) with only operators 1 and 2 sounding: a 1:2 ratio puts sidebands at
    // the odd multiples of the note (fc + k * 2 fc).
    let spectrum_with = |modulator: f64| {
        let mut synth = flat_fm();
        set(
            &mut synth,
            &[
                (op_param(1, OpField::Ratio), 5.0),
                (op_param(1, OpField::Level), modulator),
            ],
        );
        let (out, _) = render(&mut synth, &[Event::note_on(0, 57, 127)], 60_000);
        steady(&out, 8000)
    };
    let plain = spectrum_with(0.0);
    assert!(at(&plain, 660.0) < at(&plain, 220.0) * 0.001, "a pure sine");
    let fm = spectrum_with(0.3);
    assert!(at(&fm, 660.0) > at(&fm, 220.0) * 0.1, "sideband at 3 f");
    assert!(at(&fm, 1100.0) > at(&fm, 220.0) * 0.02, "sideband at 5 f");
    assert!(
        at(&fm, 440.0) < at(&fm, 220.0) * 0.01,
        "no even partials at 1:2"
    );
    // More modulation, more sideband energy.
    let sidebands = |spec: &[f64]| at(spec, 660.0) + at(spec, 1100.0) + at(spec, 1540.0);
    let (weak, strong) = (spectrum_with(0.05), spectrum_with(0.15));
    assert!(sidebands(&strong) > sidebands(&weak) * 2.0);
}

#[test]
fn instruments_controllers_drive_vibrato_and_sustain() {
    // Mod wheel: no vibrato at rest, a swing of about a semitone at full travel.
    let swing = |wheel: u8| {
        let mut synth = flat_synth(2.0);
        let events = [
            Event::control_change(0, 1, wheel),
            Event::note_on(0, 69, 127),
        ];
        let (out, _) = render(&mut synth, &events, 48_000);
        let freqs: Vec<f64> = blocks(&out, 960).map(measure_hz).collect();
        let (lo, hi) = freqs
            .iter()
            .fold((f64::MAX, f64::MIN), |(a, b), f| (a.min(*f), b.max(*f)));
        cents(hi, lo)
    };
    assert!(swing(0) < 2.0, "{}", swing(0));
    assert!(swing(127) > 100.0 && swing(127) < 230.0, "{}", swing(127));
    assert!(swing(64) > 40.0 && swing(64) < swing(127));
    // The same wheel on the FM synth (half a semitone each way).
    let mut synth = flat_fm();
    let events = [Event::control_change(0, 1, 127), Event::note_on(0, 69, 127)];
    let (out, _) = render(&mut synth, &events, 48_000);
    let freqs: Vec<f64> = blocks(&out, 960).map(measure_hz).collect();
    let (lo, hi) = freqs
        .iter()
        .fold((f64::MAX, f64::MIN), |(a, b), f| (a.min(*f), b.max(*f)));
    assert!(
        cents(hi, lo) > 50.0 && cents(hi, lo) < 115.0,
        "{}",
        cents(hi, lo)
    );

    // Sustain pedal: a note released under the pedal keeps sounding until it comes up.
    for kind in [sub::KIND, fm::KIND] {
        let mut device = find(kind).unwrap().create(RATE, 4096);
        set(&mut *device, &[(sub::PARAM_GAIN, 0.0)]);
        let events = [
            Event::control_change(0, 64, 127),
            Event::note_on(0, 60, 127),
            Event::note_off(2000, 60),
            Event::control_change(24_000, 64, 0),
        ];
        let (out, _) = render(&mut *device, &events, 48_000);
        let level = |from: usize, to: usize| rms(&out[from..to]);
        assert!(level(10_000, 20_000) > 0.01, "{kind} held by the pedal");
        // After the pedal comes up the release (0.3 s by default) fades it out.
        assert!(
            level(44_000, 48_000) < level(10_000, 20_000) * 0.01,
            "{kind} released"
        );
        // Without the pedal the same note-off releases at once.
        let mut device = find(kind).unwrap().create(RATE, 4096);
        let events = [Event::note_on(0, 60, 127), Event::note_off(2000, 60)];
        let (out, _) = render(&mut *device, &events, 48_000);
        assert!(rms(&out[40_000..]) < 1e-6);
    }
}

#[test]
fn instruments_fm_additive_algorithm_and_feedback() {
    // Algorithm 7 (additive): four carriers at ratios 1, 2, 3 and 4.
    let mut synth = flat_fm();
    set(&mut synth, &[(fm::PARAM_ALGORITHM, 6.0)]);
    for (op, ratio) in [(0, 3.0), (1, 5.0), (2, 6.0), (3, 7.0)] {
        set(
            &mut synth,
            &[
                (op_param(op, OpField::Ratio), ratio),
                (op_param(op, OpField::Level), 0.25),
            ],
        );
    }
    let (out, _) = render(&mut synth, &[Event::note_on(0, 57, 127)], 60_000);
    let spec = steady(&out, 8000);
    let base = at(&spec, 220.0);
    for harmonic in [2.0, 3.0, 4.0] {
        let level = at(&spec, 220.0 * harmonic);
        assert!(
            (level / base - 1.0).abs() < 0.1,
            "harmonic {harmonic}: {level} vs {base}"
        );
    }
    assert!(at(&spec, 1100.0) < base * 0.01);

    // Feedback on operator 4 turns its sine into a saw-like wave: harmonics appear.
    let harmonic = |feedback: f64| {
        let mut synth = flat_fm();
        set(
            &mut synth,
            &[
                (fm::PARAM_ALGORITHM, 6.0),
                (fm::PARAM_FEEDBACK, feedback),
                (op_param(0, OpField::Level), 0.0),
                (op_param(3, OpField::Level), 1.0),
                (op_param(3, OpField::Ratio), 3.0),
            ],
        );
        let (out, _) = render(&mut synth, &[Event::note_on(0, 57, 127)], 60_000);
        let spec = steady(&out, 8000);
        at(&spec, 440.0) / at(&spec, 220.0)
    };
    assert!(harmonic(0.0) < 0.001 && harmonic(1.0) > 0.2);
}

#[test]
fn instruments_fm_envelope_velocity_and_voices() {
    // Attack and decay shape the carrier like the synth's: quiet start, full level, sustain.
    let mut synth = flat_fm();
    set(
        &mut synth,
        &[
            (op_param(0, OpField::Attack), 0.1),
            (op_param(0, OpField::Decay), 0.05),
            (op_param(0, OpField::Sustain), 0.5),
        ],
    );
    let (out, _) = render(&mut synth, &[Event::note_on(0, 31, 127)], 48_000);
    assert!(peak(&out[..480]) < 0.15);
    assert!(peak(&out[4400..4900]) > 0.85);
    assert!((peak(&out[30_000..]) - 0.5).abs() < 0.08);

    // Velocity sensitivity 1: level follows the velocity.
    let level = |velocity| {
        let mut synth = flat_fm();
        set(
            &mut synth,
            &[(op_param(0, OpField::VelocitySensitivity), 1.0)],
        );
        let (out, _) = render(&mut synth, &[Event::note_on(0, 60, velocity)], 4800);
        peak(&out)
    };
    assert!((level(127) / level(64) - 127.0 / 64.0).abs() < 0.03);

    // Sixteen voices, a seventeenth steals, release ends in exact silence.
    let mut synth = flat_fm();
    let on: Vec<Event> = (0..16).map(|i| Event::note_on(0, 40 + i, 100)).collect();
    let (out, _) = render(&mut synth, &on, 2048);
    assert_eq!(synth.active_voices(), 16);
    assert!(out.iter().all(|s| s.is_finite()));
    render(&mut synth, &[Event::note_on(0, 90, 100)], 512);
    assert_eq!(synth.active_voices(), 16);
    let off: Vec<Event> = (0..16)
        .map(|i| Event::note_off(0, 40 + i))
        .chain([Event::note_off(0, 90)])
        .collect();
    let (out, _) = render(&mut synth, &off, 48_000);
    assert_eq!(synth.active_voices(), 0);
    assert!(out[40_000..].iter().all(|s| *s == 0.0));
}

// ----- both ----------------------------------------------------------------------------------

/// A patch that exercises every modulation path of each synth.
fn busy_patch(kind: &str) -> Box<dyn Device> {
    let info = find(kind).unwrap();
    let mut device = info.create(RATE, 512);
    if kind == sub::KIND {
        set(
            &mut *device,
            &[
                (sub::PARAM_WAVEFORM, 0.0),
                (sub::PARAM_OSC2_LEVEL, 0.7),
                (sub::PARAM_OSC2_WAVEFORM, 3.0),
                (sub::PARAM_OSC2_FINE, 12.0),
                (sub::PARAM_SUB_LEVEL, 0.5),
                (sub::PARAM_NOISE_LEVEL, 0.1),
                (sub::PARAM_CUTOFF, 900.0),
                (sub::PARAM_RESONANCE, 0.5),
                (sub::PARAM_FILTER_ENV_AMOUNT, 36.0),
                (sub::PARAM_KEY_TRACK, 0.5),
                (sub::PARAM_LFO_SHAPE, 4.0),
                (sub::PARAM_LFO_RATE, 7.0),
                (sub::PARAM_LFO_PITCH, 0.3),
                (sub::PARAM_LFO_CUTOFF, 12.0),
                (sub::PARAM_LFO_AMP, 0.3),
                (sub::PARAM_UNISON_VOICES, 4.0),
                (sub::PARAM_UNISON_SPREAD, 0.8),
            ],
        );
    } else {
        set(
            &mut *device,
            &[
                (fm::PARAM_ALGORITHM, 4.0),
                (fm::PARAM_FEEDBACK, 0.4),
                (op_param(1, OpField::Level), 0.6),
                (op_param(2, OpField::Level), 0.5),
                (op_param(3, OpField::Level), 0.7),
                (op_param(3, OpField::Ratio), 9.0),
            ],
        );
    }
    device
}

fn events_for(frames: u32) -> Vec<Event> {
    let mut events = Vec::new();
    for (i, key) in [48_u8, 55, 60, 64, 67, 72].iter().enumerate() {
        let at = i as u32 * 1700;
        events.push(Event::note_on(at, *key, 60 + 10 * i as u8));
        events.push(Event::note_off(at + 5000 + 13 * i as u32, *key));
    }
    events.retain(|e| e.offset < frames);
    events.sort_by_key(|e| e.offset);
    events
}

#[test]
fn instruments_render_is_deterministic_and_independent_of_block_size() {
    let frames = 24_000;
    for kind in [sub::KIND, fm::KIND] {
        let events = events_for(frames as u32);
        let mut whole = busy_patch(kind);
        let (reference, reference_right) = render(&mut *whole, &events, frames);
        assert!(peak(&reference) > 0.05, "{kind} sounds");
        assert!(
            reference
                .iter()
                .chain(&reference_right)
                .all(|s| s.is_finite())
        );
        // The same patch again: identical bits.
        let (again, _) = render(&mut *busy_patch(kind), &events, frames);
        assert_eq!(reference, again, "{kind} is deterministic");
        // The same events cut into odd-sized blocks.
        for size in [1_usize, 7, 16, 64, 333, 4096] {
            let mut device = busy_patch(kind);
            let (mut left, mut right) = (Vec::new(), Vec::new());
            let mut start = 0;
            while start < frames {
                let end = (start + size).min(frames);
                let block: Vec<Event> = events
                    .iter()
                    .filter(|e| (start..end).contains(&(e.offset as usize)))
                    .map(|e| Event {
                        offset: e.offset - start as u32,
                        kind: e.kind,
                    })
                    .collect();
                let (l, r) = render(&mut *device, &block, end - start);
                left.extend(l);
                right.extend(r);
                start = end;
            }
            assert_eq!(left, reference, "{kind} left, blocks of {size}");
            assert_eq!(right, reference_right, "{kind} right, blocks of {size}");
        }
    }
}

#[test]
fn instruments_parameters_glide_instead_of_stepping() {
    // Before the first block a value applies at once (a project's values never glide in).
    let mut synth = flat_synth(2.0);
    set(&mut synth, &[(sub::PARAM_GAIN, -20.0)]);
    let (out, _) = render(&mut synth, &[Event::note_on(0, 57, 127)], 4800);
    assert!(
        (peak(&out[..960]) - 0.1).abs() < 0.01,
        "{}",
        peak(&out[..960])
    );
    // While playing a change glides: not there after 1 ms, there after 100 ms.
    let mut synth = flat_synth(2.0);
    render(&mut synth, &[Event::note_on(0, 57, 127)], 4800);
    synth.set_param(sub::PARAM_GAIN, -20.0);
    let (out, _) = render(&mut synth, &[], 9600);
    assert!(peak(&out[..220]) > 0.7, "{}", peak(&out[..220]));
    assert!(peak(&out[..4800]) > peak(&out[4800..]) * 2.0);
    assert!((peak(&out[7000..]) - 0.1).abs() < 0.01);
    // No step larger than a small fraction of the change between adjacent control updates.
    let jump = out
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0_f32, f32::max);
    assert!(jump < 0.1, "{jump}");
}

#[test]
fn instruments_state_round_trips_and_rejects_damaged_bytes() {
    for kind in [sub::KIND, fm::KIND] {
        let info = find(kind).unwrap();
        let events = events_for(12_000);
        let mut original = busy_patch(kind);
        let state = original.save_state();
        assert!(!state.is_empty());
        let (expected, _) = render(&mut *original, &events, 12_000);
        // A device built from the defaults plays differently until it loads the state.
        let mut restored = info.create(RATE, 512);
        let (before, _) = render(&mut *restored, &events, 12_000);
        assert_ne!(before, expected);
        let mut restored = info.create(RATE, 512);
        restored.load_state(&state).unwrap();
        let (after, _) = render(&mut *restored, &events, 12_000);
        assert_eq!(after, expected, "{kind}");
        // Damaged and future states are refused and change nothing.
        let mut device = info.create(RATE, 512);
        assert_eq!(
            device.load_state(&state[..state.len() - 3]),
            Err(StateError::Malformed)
        );
        assert_eq!(device.load_state(&[1, 0]), Err(StateError::Malformed));
        let mut future = state.clone();
        future[..4].copy_from_slice(&99_u32.to_le_bytes());
        assert_eq!(
            device.load_state(&future),
            Err(StateError::UnsupportedVersion(99))
        );
        let (unchanged, _) = render(&mut *device, &events, 12_000);
        assert_eq!(unchanged, before);
    }
}

#[test]
fn instruments_descriptors_are_consistent_grouped_and_registered() {
    for kind in [sub::KIND, fm::KIND] {
        let info = find(kind).expect("registered");
        assert!(info.instrument && !info.sidechain);
        let mut ids: Vec<u32> = info.params.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), info.params.len(), "{kind}: ids are unique");
        let mut names: Vec<&str> = info.params.iter().map(|p| p.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), info.params.len(), "{kind}: names are unique");
        for p in info.params {
            assert!(p.min < p.max, "{}", p.name);
            assert!((p.min..=p.max).contains(&p.default), "{}", p.name);
            assert!(!p.group.is_empty(), "{} has a group", p.name);
            assert!(p.smoothing_ms >= 0.0);
            if p.scale == ow_dsp::Scale::Log {
                assert!(p.min > 0.0, "{}", p.name);
            }
            if let ow_dsp::ParamKind::Choice(options) = p.kind {
                assert_eq!(p.max as usize, options.len() - 1, "{}", p.name);
                assert_eq!(p.smoothing_ms, 0.0, "a choice is not smoothed: {}", p.name);
            }
            assert_eq!(p.clamp(f64::NAN), p.default);
        }
        let mut device = info.create(RATE, 256);
        let (out, _) = render(&mut *device, &[Event::note_on(0, 60, 100)], 256);
        assert!(
            out.iter().any(|s| *s != 0.0),
            "{kind} plays at its defaults"
        );
    }
    // The ids of the first synth's eight parameters never move.
    let synth = find(sub::KIND).unwrap();
    for (id, name) in [
        (0, "Waveform"),
        (1, "Cutoff"),
        (2, "Resonance"),
        (3, "Attack"),
        (7, "Gain"),
    ] {
        assert_eq!(synth.param(id).unwrap().name, name);
    }
}

#[test]
fn instruments_param_bank_smooths_snaps_and_saves() {
    const PARAMS: &[ParamDescriptor] = &[
        ParamDescriptor::linear(4, "Smooth", 0.0, 10.0, 0.0, ow_dsp::Unit::None).smoothed(10.0),
        ParamDescriptor::linear(9, "Step", 0.0, 10.0, 1.0, ow_dsp::Unit::None),
        ParamDescriptor::choice(2, "Pick", &["a", "b", "c"], 0).smoothed(50.0),
    ];
    let mut bank = ParamBank::new(PARAMS);
    bank.prepare(48_000.0);
    // Before the first advance everything applies at once.
    bank.set(4, 8.0);
    assert_eq!(bank.value(4), 8.0);
    bank.advance(16);
    bank.set(4, 0.0);
    bank.set(9, 5.0);
    bank.set(2, 2.0);
    assert_eq!(
        (bank.value(9), bank.value(2)),
        (5.0, 2.0),
        "unsmoothed and choices step"
    );
    assert_eq!(bank.target(4), 0.0);
    assert!(bank.value(4) > 7.0, "the glide starts from the old value");
    // One time constant (10 ms) closes about 63 percent of the distance.
    bank.advance(480);
    let after_tau = bank.value(4);
    assert!(
        (after_tau - 8.0 * (-1.0_f64).exp()).abs() < 0.25,
        "{after_tau}"
    );
    // Two half steps equal one whole step.
    let mut a = bank.clone();
    let mut b = bank.clone();
    a.advance(100);
    b.advance(50);
    b.advance(50);
    assert!((a.value(4) - b.value(4)).abs() < 1e-12);
    bank.advance(48_000);
    assert_eq!(bank.value(4), 0.0, "settles exactly");
    // Out-of-range and unknown values.
    assert!(bank.set(9, 99.0) && bank.target(9) == 10.0);
    assert!(!bank.set(1234, 1.0));
    // A snapshot carries the targets and survives a round trip.
    bank.set(9, 3.5);
    let saved = bank.save_state();
    let mut other = ParamBank::new(PARAMS);
    other.load_state(&saved).unwrap();
    assert_eq!((other.value(9), other.value(2)), (3.5, 2.0));
}

#[test]
fn instruments_are_realtime_safe() {
    for kind in [sub::KIND, fm::KIND] {
        let mut device = busy_patch(kind);
        device.prepare(RATE, 128);
        let info = find(kind).unwrap();
        let mut left = [0.0_f32; 128];
        let mut right = [0.0_f32; 128];
        let key = [0.0_f32; 128];
        for block in 0..1500_u32 {
            let events = [
                Event::note_on(3, (30 + block % 60) as u8, 100),
                Event::control_change(10, 1, (block * 37 % 128) as u8),
                Event::note_on(20, (35 + block % 50) as u8, 80),
                // The pedal is down for three blocks of six: its lift releases held voices.
                Event::control_change(30, 64, if block % 6 < 3 { 127 } else { 0 }),
                Event::control_change(30, 7, 99),
                Event::note_off(64, (30 + (block + 7) % 60) as u8),
                Event::note_off(100, (35 + (block + 3) % 50) as u8),
            ];
            assert_no_alloc(|| {
                // Every parameter moves, including choices, so every code path runs.
                if block % 3 == 0 {
                    for (i, p) in info.params.iter().enumerate() {
                        let t = f64::from((block + i as u32 * 7) % 11) / 10.0;
                        device.set_param(p.id, p.clamp(p.min + t * (p.max - p.min)));
                    }
                }
                if block % 500 == 499 {
                    device.reset();
                }
                if block % 2 == 0 {
                    device.process(&mut [&mut left, &mut right], &events);
                } else {
                    device.process_sidechain(&mut [&mut left, &mut right], &[&key, &key], &events);
                }
            });
            assert!(
                left.iter().chain(&right).all(|s| s.is_finite()),
                "{kind} block {block}"
            );
        }
    }
}

// ----- gate round 1 -------------------------------------------------------------------------

#[test]
fn instruments_unison_does_not_cancel_at_note_on() {
    for waveform in [0.0, 1.0, 2.0, 3.0] {
        let first_50_ms = |voices: f64, detune: f64| {
            let mut synth = flat_synth(waveform);
            set(
                &mut synth,
                &[
                    (sub::PARAM_UNISON_VOICES, voices),
                    (sub::PARAM_UNISON_DETUNE, detune),
                    (sub::PARAM_UNISON_SPREAD, 0.0),
                ],
            );
            let (out, _) = render(&mut synth, &[Event::note_on(0, 57, 127)], 2400);
            rms(&out)
        };
        for detune in [0.0, 20.0] {
            let single = first_50_ms(0.0, detune);
            for voices in 1..=6 {
                let level = db(first_50_ms(f64::from(voices), detune) / single);
                assert!(
                    level.abs() < 6.0,
                    "waveform {waveform}, {} copies, detune {detune}: {level} dB",
                    voices + 1
                );
            }
        }
    }
}

#[test]
fn instruments_same_frame_controllers_apply_in_arrival_order() {
    let tail = |events: &[Event], kind: &str| {
        let mut device = find(kind).unwrap().create(RATE, 4096);
        set(&mut *device, &[(sub::PARAM_GAIN, 0.0)]);
        let (out, _) = render(&mut *device, events, 48_000);
        rms(&out[40_000..])
    };
    for kind in [sub::KIND, fm::KIND] {
        // Pedal down then up at one frame: up wins, the note-off releases.
        let released = [
            Event::note_on(0, 60, 127),
            Event::control_change(1000, 64, 127),
            Event::control_change(1000, 64, 0),
            Event::note_off(2000, 60),
        ];
        assert!(tail(&released, kind) < 1e-6, "{kind}");
        // Up then down: down wins, the note is held by the pedal.
        let held = [
            Event::note_on(0, 60, 127),
            Event::control_change(1000, 64, 0),
            Event::control_change(1000, 64, 127),
            Event::note_off(2000, 60),
        ];
        assert!(tail(&held, kind) > 0.01, "{kind}");
    }
    // Repeated mod wheel at one frame: the last value stands.
    let swing = |events: &[Event]| {
        let mut synth = flat_synth(2.0);
        let (out, _) = render(&mut synth, events, 48_000);
        let freqs: Vec<f64> = blocks(&out, 960).map(measure_hz).collect();
        let (lo, hi) = freqs
            .iter()
            .fold((f64::MAX, f64::MIN), |(a, b), f| (a.min(*f), b.max(*f)));
        cents(hi, lo)
    };
    let off_last = [
        Event::control_change(0, 1, 127),
        Event::control_change(0, 1, 0),
        Event::note_on(0, 69, 127),
    ];
    let on_last = [
        Event::control_change(0, 1, 0),
        Event::control_change(0, 1, 127),
        Event::note_on(0, 69, 127),
    ];
    assert!(swing(&off_last) < 2.0 && swing(&on_last) > 100.0);
}

#[test]
fn instruments_a_pedal_held_voice_is_stolen_like_a_released_one() {
    for kind in [sub::KIND, fm::KIND] {
        let mut device = find(kind).unwrap().create(RATE, 4096);
        set(&mut *device, &[(sub::PARAM_GAIN, 0.0)]);
        if kind == sub::KIND {
            set(&mut *device, &[(sub::PARAM_WAVEFORM, 2.0)]);
        }
        let mut events = vec![Event::note_on(0, 40, 100)];
        events.extend((41..55).map(|key| Event::note_on(0, key, 100)));
        // Key 55 is released under the pedal; keys 40 to 54 stay held.
        events.push(Event::note_on(0, 55, 100));
        events.push(Event::control_change(100, 64, 127));
        events.push(Event::note_off(200, 55));
        // A seventeenth note: it must take the pedal-held voice, not the oldest held one.
        events.push(Event::note_on(300, 90, 100));
        let (out, _) = render(&mut *device, &events, 48_000);
        let spec = steady(&out, 8000);
        // Key 40 (the oldest held note) still sounds; key 55's voice is gone.
        assert!(at(&spec, hz(40)) > at(&spec, hz(55)) * 5.0, "{kind}");
    }
}

#[test]
fn instruments_envelope_tail_flushes_to_exact_zero() {
    use ow_dsp::instruments::common::{EnvCoefs, Envelope};
    let coefs = EnvCoefs::new(0.001, 0.01, 0.5, 0.05, 48_000.0);
    let mut envelope = Envelope::idle();
    envelope.trigger();
    for _ in 0..2000 {
        envelope.tick(&coefs);
    }
    envelope.release();
    for _ in 0..48_000 {
        let level = envelope.tick(&coefs);
        assert!(
            level == 0.0 || level >= 1.0e-5,
            "no sub-threshold level: {level}"
        );
    }
    assert!(envelope.finished() && envelope.level == 0.0);
}

/// The default `builtin.synth` patch, and a resonant square patch with stealing, render what the
/// synth before stage 16 rendered (fixture `tests/fixtures/legacy_synth.txt`, made with the
/// code at 96d0f48: the whole-signal RMS and every 64th left sample), so projects saved before
/// the new synth sound the same.
#[test]
fn instruments_the_default_synth_patch_still_sounds_like_the_original() {
    let fixture = include_str!("fixtures/legacy_synth.txt");
    let signal = |name: &str| -> (f64, Vec<(usize, f32)>) {
        let mut rms_value = 0.0;
        let mut samples = Vec::new();
        for line in fixture.lines() {
            let parts: Vec<&str> = line.split(' ').collect();
            if parts[0] != name {
                continue;
            }
            if parts[1] == "rms" {
                rms_value = parts[2].parse().unwrap();
            } else {
                samples.push((parts[1].parse().unwrap(), parts[2].parse().unwrap()));
            }
        }
        (rms_value, samples)
    };
    let check = |name: &str, synth: &mut Synth, events: &[Event], frames: usize| {
        let (expected_rms, expected) = signal(name);
        let (out, right) = render(synth, events, frames);
        assert_eq!(out, right);
        assert!((rms(&out) / expected_rms - 1.0).abs() < 1e-4, "{name} rms");
        for (i, value) in expected {
            assert!(
                (out[i] - value).abs() < 2e-4,
                "{name} frame {i}: {} vs {value}",
                out[i]
            );
        }
    };
    let mut a = Synth::new();
    a.prepare(RATE, 4096);
    let events_a = [
        Event::note_on(0, 60, 100),
        Event::note_on(4000, 64, 90),
        Event::note_on(8000, 67, 110),
        Event::note_off(14_000, 60),
        Event::note_off(16_000, 64),
        Event::note_off(18_000, 67),
        Event::note_on(20_000, 72, 127),
    ];
    check("A", &mut a, &events_a, 40_000);

    let mut b = Synth::new();
    b.prepare(RATE, 4096);
    set(
        &mut b,
        &[
            (sub::PARAM_WAVEFORM, 1.0),
            (sub::PARAM_CUTOFF, 1500.0),
            (sub::PARAM_RESONANCE, 0.6),
            (sub::PARAM_ATTACK, 0.02),
            (sub::PARAM_DECAY, 0.15),
            (sub::PARAM_SUSTAIN, 0.4),
            (sub::PARAM_RELEASE, 0.25),
            (sub::PARAM_GAIN, -6.0),
        ],
    );
    let mut events_b: Vec<Event> = (0..20_u32)
        .map(|i| Event::note_on(i * 100, 40 + (i * 2) as u8, 100))
        .collect();
    events_b.extend((0..20_u32).map(|i| Event::note_off(5000 + i * 50, 40 + (i * 2) as u8)));
    events_b.sort_by_key(|e| e.offset);
    check("B", &mut b, &events_b, 30_000);
}
