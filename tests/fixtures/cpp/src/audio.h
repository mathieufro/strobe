#pragma once
#include <cstdint>

namespace audio {

struct AudioBuffer {
    float samples[512];
    uint32_t sample_rate;
    uint32_t size;
};

float process_buffer(AudioBuffer* buf);
AudioBuffer generate_sine(float freq);
void apply_effect(AudioBuffer* buf, float gain);

} // namespace audio
