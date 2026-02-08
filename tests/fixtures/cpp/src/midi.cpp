#include "midi.h"
#include "globals.h"
#include <cstdio>

namespace midi {

bool note_on(uint8_t note, uint8_t velocity) {
    g_counter++;
    printf("[MIDI] NoteOn note=%d vel=%d\n", note, velocity);
    return note < 128 && velocity > 0;
}

bool control_change(uint8_t cc, uint8_t value) {
    g_counter++;
    return cc < 128;
}

std::vector<MidiMessage> generate_sequence(int length) {
    std::vector<MidiMessage> seq;
    seq.reserve(length);
    for (int i = 0; i < length; i++) {
        seq.push_back({0x90, static_cast<uint8_t>(60 + (i % 12)),
                        static_cast<uint8_t>(80 + (i % 40))});
    }
    return seq;
}

} // namespace midi
