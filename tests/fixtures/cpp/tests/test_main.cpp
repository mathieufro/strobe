#include <catch2/catch_test_macros.hpp>
#include "audio.h"
#include "midi.h"
#include "timing.h"

TEST_CASE("Audio buffer processing", "[unit][audio]") {
    auto buf = audio::generate_sine(440.0f);
    float rms = audio::process_buffer(&buf);
    REQUIRE(rms > 0.0f);
}

TEST_CASE("Audio apply effect", "[unit][audio]") {
    auto buf = audio::generate_sine(440.0f);
    audio::apply_effect(&buf, 2.0f);
    float rms = audio::process_buffer(&buf);
    REQUIRE(rms > 0.0f);
}

TEST_CASE("Audio generate sine", "[unit][audio]") {
    auto buf = audio::generate_sine(440.0f);
    REQUIRE(buf.size == 512);
    REQUIRE(buf.sample_rate == 44100);
}

TEST_CASE("MIDI note on", "[unit][midi]") {
    REQUIRE(midi::note_on(60, 100));
}

TEST_CASE("MIDI control change", "[unit][midi]") {
    REQUIRE(midi::control_change(1, 64));
}

TEST_CASE("MIDI sequence generation", "[unit][midi]") {
    auto seq = midi::generate_sequence(8);
    REQUIRE(seq.size() == 8);
}

TEST_CASE("Timing fast function", "[integration][timing]") {
    timing::fast();
    REQUIRE(true);
}

TEST_CASE("Timing medium function", "[integration][timing]") {
    timing::medium();
    REQUIRE(true);
}

TEST_CASE("Timing slow function", "[integration][timing]") {
    timing::slow();
    REQUIRE(true);
}

// Intentionally failing test (for adapter validation)
TEST_CASE("Intentional failure", "[unit][expected-fail]") {
    REQUIRE(1 == 2);
}

// Intentionally stuck test (for stuck detector validation)
TEST_CASE("Stuck test - infinite loop", "[.][stuck]") {
    volatile bool done = false;
    while (!done) { }
}
