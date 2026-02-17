#include "crash.h"
#include "globals.h"
#include <cstdio>
#include <cstdlib>
#include <cstring>

namespace crash {

void null_deref() {
    int local_counter = 42;
    float local_ratio = 3.14159f;
    char local_buffer[64];
    strcpy(local_buffer, "about to crash");
    int* ptr = nullptr;

    printf("[CRASH] About to dereference NULL (counter=%d, ratio=%.2f)\n",
           local_counter, local_ratio);
    fflush(stdout);

    g_counter++;
    *ptr = local_counter; // SIGSEGV
}

void abort_signal() {
    int error_code = -1;
    const char* reason = "intentional abort for testing";

    printf("[CRASH] About to abort (error_code=%d, reason=%s)\n",
           error_code, reason);
    fflush(stdout);

    g_counter++;
    abort(); // SIGABRT
}

static int recurse_depth = 0;

void stack_overflow(int depth) {
    char frame_padding[4096];
    memset(frame_padding, depth & 0xFF, sizeof(frame_padding));
    recurse_depth = depth;

    if (depth % 100 == 0) {
        printf("[CRASH] Recursion depth: %d\n", depth);
        fflush(stdout);
    }

    stack_overflow(depth + 1);
}

} // namespace crash
