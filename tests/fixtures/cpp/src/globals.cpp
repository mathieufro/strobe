#include "globals.h"

uint32_t g_counter = 0;
double g_tempo = 120.0;
int64_t g_sample_rate = 44100;
static Point g_point_storage = {10, 20, 99.9};
Point* g_point_ptr = &g_point_storage;
