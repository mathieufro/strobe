#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <signal.h>
#include <sys/wait.h>

// Global variables (for watch variable testing)
static int g_crash_count = 0;
static float g_temperature = 98.6f;
static const char* g_app_state = "running";

// ========== Crash Scenarios ==========

// Has interesting locals for DWARF resolution testing
void crash_null_deref(void) {
    int local_counter = 42;
    float local_ratio = 3.14159f;
    char local_buffer[64];
    strcpy(local_buffer, "about to crash");
    int* ptr = NULL;

    // These locals should be visible in crash frame:
    // local_counter=42, local_ratio=3.14159, local_buffer="about to crash"
    printf("[CRASH] About to dereference NULL (counter=%d, ratio=%.2f)\n",
           local_counter, local_ratio);
    fflush(stdout);

    g_crash_count++;
    *ptr = local_counter;  // SIGSEGV
}

void crash_abort(void) {
    int error_code = -1;
    const char* reason = "intentional abort for testing";

    printf("[CRASH] About to abort (error_code=%d, reason=%s)\n",
           error_code, reason);
    fflush(stdout);

    g_crash_count++;
    abort();  // SIGABRT
}

static int recurse_depth = 0;

void crash_stack_overflow(int depth) {
    char frame_padding[4096];  // Eat stack space
    memset(frame_padding, depth & 0xFF, sizeof(frame_padding));
    recurse_depth = depth;

    if (depth % 100 == 0) {
        printf("[CRASH] Recursion depth: %d\n", depth);
        fflush(stdout);
    }

    crash_stack_overflow(depth + 1);  // Eventually SIGSEGV (stack overflow)
}

// ========== Fork/Exec Scenarios ==========

void do_child_work(int child_id, int iterations) {
    printf("[CHILD %d] PID=%d started, doing %d iterations\n",
           child_id, getpid(), iterations);

    for (int i = 0; i < iterations; i++) {
        // Simulate work with varied durations
        volatile double result = 0;
        for (int j = 0; j < (child_id + 1) * 10000; j++) {
            result += j * 0.001;
        }

        if (i % 10 == 0) {
            printf("[CHILD %d] iteration %d/%d (result=%.2f)\n",
                   child_id, i, iterations, result);
        }
    }

    printf("[CHILD %d] PID=%d finished\n", child_id, getpid());
}

void fork_workers(int num_workers) {
    printf("[PARENT] PID=%d forking %d workers\n", getpid(), num_workers);

    pid_t children[16];
    int n = num_workers < 16 ? num_workers : 16;

    for (int i = 0; i < n; i++) {
        pid_t pid = fork();
        if (pid == 0) {
            // Child process
            do_child_work(i, 50);
            _exit(0);
        } else if (pid > 0) {
            children[i] = pid;
            printf("[PARENT] Forked child %d with PID %d\n", i, pid);
        } else {
            perror("fork");
        }
    }

    // Wait for all children
    for (int i = 0; i < n; i++) {
        int status;
        waitpid(children[i], &status, 0);
        printf("[PARENT] Child %d (PID %d) exited with status %d\n",
               i, children[i], WEXITSTATUS(status));
    }
}

void fork_exec(void) {
    printf("[PARENT] PID=%d forking + exec\n", getpid());

    pid_t pid = fork();
    if (pid == 0) {
        // Child: exec a simple command
        execlp("echo", "echo", "Hello from child process!", NULL);
        perror("exec failed");
        _exit(1);
    } else if (pid > 0) {
        int status;
        waitpid(pid, &status, 0);
        printf("[PARENT] Exec child (PID %d) exited with status %d\n",
               pid, WEXITSTATUS(status));
    }
}

// ========== Slow Functions (for duration query testing) ==========

void fast_function(void) {
    // ~0 ns — just a counter increment
    volatile int x = 0;
    for (int i = 0; i < 100; i++) x += i;
}

void medium_function(void) {
    // ~1-5ms
    volatile double result = 0;
    for (int i = 0; i < 100000; i++) {
        result += i * 0.001;
    }
    printf("[TIMING] medium_function result=%.2f\n", result);
}

void slow_function(void) {
    // ~50ms
    usleep(50000);
    printf("[TIMING] slow_function done\n");
}

void very_slow_function(void) {
    // ~500ms
    usleep(500000);
    printf("[TIMING] very_slow_function done\n");
}

void run_slow_functions(void) {
    printf("[TIMING] Running functions with varied durations...\n");

    for (int round = 0; round < 5; round++) {
        fast_function();
        fast_function();
        fast_function();
        medium_function();
        slow_function();
        if (round == 2) very_slow_function();
    }

    printf("[TIMING] Done\n");
}

// ========== Main ==========

int main(int argc, char* argv[]) {
    const char* mode = (argc > 1) ? argv[1] : "mixed";

    printf("[STRESS TEST 1C] PID=%d mode=%s\n", getpid(), mode);

    if (strcmp(mode, "crash-null") == 0) {
        crash_null_deref();
    } else if (strcmp(mode, "crash-abort") == 0) {
        crash_abort();
    } else if (strcmp(mode, "crash-stack") == 0) {
        crash_stack_overflow(0);
    } else if (strcmp(mode, "fork-workers") == 0) {
        fork_workers(3);
    } else if (strcmp(mode, "fork-exec") == 0) {
        fork_exec();
    } else if (strcmp(mode, "slow-functions") == 0) {
        run_slow_functions();
    } else if (strcmp(mode, "mixed") == 0) {
        // Non-crashing scenarios first
        run_slow_functions();
        fork_workers(2);
        fork_exec();
        // Crash last (terminates process)
        crash_null_deref();
    } else {
        fprintf(stderr, "Unknown mode: %s\n", mode);
        fprintf(stderr, "Usage: %s [crash-null|crash-abort|crash-stack|fork-workers|fork-exec|slow-functions|mixed]\n", argv[0]);
        return 1;
    }

    return 0;
}
