#include "audio.h"
#include "globals.h"
#include <cmath>

namespace audio {

float process_buffer(AudioBuffer* buf) {
    float sum_sq = 0.0f;
    for (uint32_t i = 0; i < buf->size; i++) {
        sum_sq += buf->samples[i] * buf->samples[i];
    }
    g_counter++;
    return sqrtf(sum_sq / buf->size);
}

AudioBuffer generate_sine(float freq) {
    AudioBuffer buf;
    buf.sample_rate = 44100;
    buf.size = 512;
    for (uint32_t i = 0; i < buf.size; i++) {
        buf.samples[i] = sinf(2.0f * 3.14159f * freq * i / buf.sample_rate);
    }
    return buf;
}

void apply_effect(AudioBuffer* buf, float gain) {
    for (uint32_t i = 0; i < buf->size; i++) {
        buf->samples[i] *= gain;
    }
}

} // namespace audio
