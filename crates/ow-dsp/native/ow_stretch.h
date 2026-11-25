// C ABI over Signalsmith Stretch for ow-dsp (Rust declares these by hand; no bindgen).
//
// Contract:
// * Audio is interleaved: a buffer of `frames` frames holds `channels * frames` floats, where
//   `channels` is what the handle was created with.
// * Frame counts must fit in `int` (the library counts in `int`); the Rust side checks this.
// * A handle is not thread-safe: use it from one thread at a time (moving it between threads
//   between calls is fine).
// * No C++ exception crosses this boundary. Every function catches everything: `create` returns
//   NULL, the int-returning functions return 0 on failure (1 on success) and leave the output
//   unspecified, the void functions do nothing on failure.
#ifndef OW_STRETCH_H
#define OW_STRETCH_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct ow_stretch ow_stretch;

// Creates an instance with the library's default preset for `sample_rate`; NULL on failure.
ow_stretch *ow_stretch_create(int channels, float sample_rate);
void ow_stretch_destroy(ow_stretch *handle);
void ow_stretch_reset(ow_stretch *handle);
size_t ow_stretch_input_latency(const ow_stretch *handle);
size_t ow_stretch_output_latency(const ow_stretch *handle);
void ow_stretch_set_transpose_semitones(ow_stretch *handle, float semitones);
// Feeds pre-roll without producing output.
int ow_stretch_seek(ow_stretch *handle, const float *input, size_t input_frames, double playback_rate);
// Consumes `input_frames`, produces `output_frames`; the ratio is implied by the lengths.
int ow_stretch_process(ow_stretch *handle, const float *input, size_t input_frames, float *output, size_t output_frames);
// Whole-buffer stretch with latency compensation; 0 also when the output is too short.
int ow_stretch_exact(ow_stretch *handle, const float *input, size_t input_frames, float *output, size_t output_frames);

#ifdef __cplusplus
}
#endif

#endif
