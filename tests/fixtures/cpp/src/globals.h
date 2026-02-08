#pragma once
#include <cstdint>

struct Point {
    int32_t x, y;
    double value;
};

extern uint32_t g_counter;
extern double g_tempo;
extern int64_t g_sample_rate;
extern Point* g_point_ptr;
