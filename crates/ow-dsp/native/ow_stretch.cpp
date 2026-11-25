#include "ow_stretch.h"

#include <signalsmith-stretch.h>

namespace {
// Channel-major indexing into an interleaved buffer.
struct Interleaved {
	float *data;
	int channels;

	struct Channel {
		float *data;
		int channel;
		int stride;
		float &operator[](size_t frame) const { return data[frame * stride + channel]; }
	};
	Channel operator[](size_t channel) const { return Channel{data, int(channel), channels}; }
};
} // namespace

struct ow_stretch {
	signalsmith::stretch::SignalsmithStretch<float> instance;
	int channels;
};

extern "C" {

ow_stretch *ow_stretch_create(int channels, float sample_rate) {
	auto handle = new ow_stretch;
	handle->channels = channels;
	handle->instance.presetDefault(channels, sample_rate);
	return handle;
}

void ow_stretch_destroy(ow_stretch *handle) { delete handle; }

void ow_stretch_reset(ow_stretch *handle) { handle->instance.reset(); }

size_t ow_stretch_input_latency(const ow_stretch *handle) {
	return size_t(handle->instance.inputLatency());
}

size_t ow_stretch_output_latency(const ow_stretch *handle) {
	return size_t(handle->instance.outputLatency());
}

void ow_stretch_set_transpose_semitones(ow_stretch *handle, float semitones) {
	handle->instance.setTransposeSemitones(semitones, 0);
}

void ow_stretch_seek(ow_stretch *handle, const float *input, size_t input_frames, double playback_rate) {
	Interleaved in{const_cast<float *>(input), handle->channels};
	handle->instance.seek(in, int(input_frames), playback_rate);
}

void ow_stretch_process(ow_stretch *handle, const float *input, size_t input_frames, float *output, size_t output_frames) {
	Interleaved in{const_cast<float *>(input), handle->channels};
	Interleaved out{output, handle->channels};
	handle->instance.process(in, int(input_frames), out, int(output_frames));
}

int ow_stretch_exact(ow_stretch *handle, const float *input, size_t input_frames, float *output, size_t output_frames) {
	Interleaved in{const_cast<float *>(input), handle->channels};
	Interleaved out{output, handle->channels};
	return handle->instance.exact(in, int(input_frames), out, int(output_frames)) ? 1 : 0;
}

} // extern "C"
