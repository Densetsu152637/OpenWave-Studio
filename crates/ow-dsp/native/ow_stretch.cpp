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

// Every body is wrapped so that no exception (std::bad_alloc from the library's vectors, say)
// unwinds into Rust.
extern "C" {

ow_stretch *ow_stretch_create(int channels, float sample_rate) {
	try {
		auto handle = new ow_stretch;
		handle->channels = channels;
		try {
			handle->instance.presetDefault(channels, sample_rate);
		} catch (...) {
			delete handle;
			return nullptr;
		}
		return handle;
	} catch (...) {
		return nullptr;
	}
}

void ow_stretch_destroy(ow_stretch *handle) {
	try { delete handle; } catch (...) {}
}

void ow_stretch_reset(ow_stretch *handle) {
	try { handle->instance.reset(); } catch (...) {}
}

size_t ow_stretch_input_latency(const ow_stretch *handle) {
	try { return size_t(handle->instance.inputLatency()); } catch (...) { return 0; }
}

size_t ow_stretch_output_latency(const ow_stretch *handle) {
	try { return size_t(handle->instance.outputLatency()); } catch (...) { return 0; }
}

void ow_stretch_set_transpose_semitones(ow_stretch *handle, float semitones) {
	try { handle->instance.setTransposeSemitones(semitones, 0); } catch (...) {}
}

int ow_stretch_seek(ow_stretch *handle, const float *input, size_t input_frames, double playback_rate) {
	try {
		Interleaved in{const_cast<float *>(input), handle->channels};
		handle->instance.seek(in, int(input_frames), playback_rate);
		return 1;
	} catch (...) {
		return 0;
	}
}

int ow_stretch_process(ow_stretch *handle, const float *input, size_t input_frames, float *output, size_t output_frames) {
	try {
		Interleaved in{const_cast<float *>(input), handle->channels};
		Interleaved out{output, handle->channels};
		handle->instance.process(in, int(input_frames), out, int(output_frames));
		return 1;
	} catch (...) {
		return 0;
	}
}

int ow_stretch_exact(ow_stretch *handle, const float *input, size_t input_frames, float *output, size_t output_frames) {
	try {
		Interleaved in{const_cast<float *>(input), handle->channels};
		Interleaved out{output, handle->channels};
		return handle->instance.exact(in, int(input_frames), out, int(output_frames)) ? 1 : 0;
	} catch (...) {
		return 0;
	}
}

} // extern "C"
