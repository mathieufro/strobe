#pragma once
#include <cstdint>
#include <vector>

namespace midi {

struct MidiMessage {
    uint8_t status;
    uint8_t data1;
    uint8_t data2;
};

bool note_on(uint8_t note, uint8_t velocity);
bool control_change(uint8_t cc, uint8_t value);
std::vector<MidiMessage> generate_sequence(int length);

} // namespace midi
