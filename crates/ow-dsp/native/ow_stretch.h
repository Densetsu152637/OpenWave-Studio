// C ABI over Signalsmith Stretch for ow-dsp (Rust declares these by hand; no bindgen).
// Audio is interleaved; lengths are in frames.
#ifndef OW_STRETCH_H
#define OW_STRETCH_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct ow_stretch ow_stretch;

// Creates an instance with the library's default preset for `sample_rate`.
ow_stretch *ow_stretch_create(int channels, float sample_rate);
void ow_stretch_destroy(ow_stretch *handle);
void ow_stretch_reset(ow_stretch *handle);
size_t ow_stretch_input_latency(const ow_stretch *handle);
size_t ow_stretch_output_latency(const ow_stretch *handle);
void ow_stretch_set_transpose_semitones(ow_stretch *handle, float semitones);
// Feeds pre-roll without producing output.
void ow_stretch_seek(ow_stretch *handle, const float *input, size_t input_frames, double playback_rate);
// Consumes `input_frames`, produces `output_frames`; the ratio is implied by the lengths.
void ow_stretch_process(ow_stretch *handle, const float *input, size_t input_frames, float *output, size_t output_frames);
// Whole-buffer stretch with latency compensation; returns 0 when the output is too short.
int ow_stretch_exact(ow_stretch *handle, const float *input, size_t input_frames, float *output, size_t output_frames);

#ifdef __cplusplus
}
#endif

#endif
