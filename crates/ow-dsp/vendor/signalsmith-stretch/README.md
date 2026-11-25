# Vendored: Signalsmith Stretch

Time-stretch and pitch-shift library by Geraint Luff / Signalsmith Audio, MIT licensed
(`LICENSE.txt`). Header-only C++11.

- `signalsmith-stretch.h`: <https://github.com/Signalsmith-Audio/signalsmith-stretch>
- `signalsmith-linear/` (the FFT and STFT it includes; its own `LICENSE.txt`, MIT):
  <https://github.com/Signalsmith-Audio/linear>

Copied unmodified from the `signalsmith-stretch` crate 0.1.3 on crates.io (Rust wrapper repository
`colinmarc/signalsmith-stretch-rs`, commit `c4d0cbd41351966f7fc0b18b9d83d9e381aa4e26`, which pins
these two submodules; the submodule commits themselves are not recorded in the published crate).
From the linear library only `fft.h`, `linear.h`, `stft.h` and `platform/` are kept. To update,
copy the files from a newer crate release and run `cargo test -p ow-dsp stretch`.

`../../native/` holds our C ABI wrapper; `../../build.rs` compiles it with `cc`.
