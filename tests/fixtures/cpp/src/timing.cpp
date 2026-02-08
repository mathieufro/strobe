#include "timing.h"
#include <cstdio>
#include <unistd.h>

namespace timing {

void fast() {
    volatile int x = 0;
    for (int i = 0; i < 100; i++) x += i;
}

void medium() {
    volatile double result = 0;
    for (int i = 0; i < 100000; i++) {
        result += i * 0.001;
    }
}

void slow() {
    usleep(50000); // 50ms
}

void very_slow() {
    usleep(500000); // 500ms
}

} // namespace timing
