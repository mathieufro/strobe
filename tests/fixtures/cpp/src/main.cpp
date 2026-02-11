#include "globals.h"
#include "audio.h"
#include "midi.h"
#include "crash.h"
#include "timing.h"
#include <cstdio>
#include <cstring>
#include <thread>
#include <unistd.h>
#include <sys/wait.h>

// Globals defined in globals.cpp via fixture_lib

static void do_child_work(int child_id, int iterations) {
    printf("[CHILD %d] PID=%d started, doing %d iterations\n",
           child_id, getpid(), iterations);

    for (int i = 0; i < iterations; i++) {
        volatile double result = 0;
        for (int j = 0; j < (child_id + 1) * 10000; j++) {
            result += j * 0.001;
        }
        if (i % 10 == 0) {
            printf("[CHILD %d] iteration %d/%d\n", child_id, i, iterations);
        }
    }

    printf("[CHILD %d] PID=%d finished\n", child_id, getpid());
}

static void fork_workers(int num_workers) {
    printf("[PARENT] PID=%d forking %d workers\n", getpid(), num_workers);

    pid_t children[16];
    int n = num_workers < 16 ? num_workers : 16;

    for (int i = 0; i < n; i++) {
        pid_t pid = fork();
        if (pid == 0) {
            do_child_work(i, 50);
            _exit(0);
        } else if (pid > 0) {
            children[i] = pid;
            printf("[PARENT] Forked child %d with PID %d\n", i, pid);
        } else {
            perror("fork");
        }
    }

    for (int i = 0; i < n; i++) {
        int status;
        waitpid(children[i], &status, 0);
        printf("[PARENT] Child %d (PID %d) exited with status %d\n",
               i, children[i], WEXITSTATUS(status));
    }
}

static void fork_exec() {
    printf("[PARENT] PID=%d forking + exec\n", getpid());

    pid_t pid = fork();
    if (pid == 0) {
        execlp("echo", "echo", "Hello from child process!", nullptr);
        perror("exec failed");
        _exit(1);
    } else if (pid > 0) {
        int status;
        waitpid(pid, &status, 0);
        printf("[PARENT] Exec child (PID %d) exited with status %d\n",
               pid, WEXITSTATUS(status));
    }
}

int main(int argc, char* argv[]) {
    const char* mode = (argc > 1) ? argv[1] : "hello";

    if (strcmp(mode, "hello") == 0) {
        printf("Hello from strobe_test_target\n");
        fprintf(stderr, "Debug output on stderr\n");
    } else if (strcmp(mode, "crash-null") == 0) {
        printf("[TARGET] PID=%d mode=crash-null\n", getpid());
        fflush(stdout);
        crash::null_deref();
    } else if (strcmp(mode, "crash-abort") == 0) {
        printf("[TARGET] PID=%d mode=crash-abort\n", getpid());
        fflush(stdout);
        crash::abort_signal();
    } else if (strcmp(mode, "crash-stack") == 0) {
        crash::stack_overflow(0);
    } else if (strcmp(mode, "fork-workers") == 0) {
        fork_workers(3);
    } else if (strcmp(mode, "fork-exec") == 0) {
        fork_exec();
    } else if (strcmp(mode, "slow-functions") == 0) {
        printf("[TIMING] Running functions with varied durations...\n");
        for (int round = 0; round < 5; round++) {
            timing::fast();
            timing::fast();
            timing::fast();
            timing::medium();
            timing::slow();
            if (round == 2) timing::very_slow();
        }
        printf("[TIMING] Done\n");
    } else if (strcmp(mode, "threads") == 0) {
        printf("[THREADS] Starting multi-threaded mode\n");

        auto audio_worker = [](int id) {
            for (int i = 0; i < 50; i++) {
                auto buf = audio::generate_sine(440.0f);
                audio::process_buffer(&buf);
                usleep(10000);
            }
        };

        auto midi_worker = []() {
            for (int i = 0; i < 50; i++) {
                midi::note_on(60 + (i % 12), 100);
                usleep(20000);
            }
        };

        std::thread t1(audio_worker, 0);
        std::thread t2(audio_worker, 1);
        std::thread t3(midi_worker);

        t1.join();
        t2.join();
        t3.join();

        printf("[THREADS] Done\n");
    } else if (strcmp(mode, "globals") == 0) {
        printf("[GLOBALS] Starting global variable updates\n");
        for (int i = 0; i < 50; i++) {
            g_counter = i;
            g_tempo = 120.0 + (i % 10);
            g_point_ptr->x = i;
            g_point_ptr->y = i * 2;
            auto buf = audio::generate_sine(440.0f);
            audio::process_buffer(&buf);
            usleep(100000);
        }
        printf("[GLOBALS] Done\n");
    } else if (strcmp(mode, "breakpoint-loop") == 0) {
        // Deterministic loop calling process_buffer N times.
        // Useful for breakpoint, hit count, logpoint, and stepping tests.
        int iterations = 10;
        if (argc > 2) iterations = atoi(argv[2]);
        printf("[BP-LOOP] Running %d iterations\n", iterations);
        for (int i = 0; i < iterations; i++) {
            g_counter = i;
            g_tempo = 120.0 + i;
            auto buf = audio::generate_sine(440.0f);
            float rms = audio::process_buffer(&buf);
            audio::apply_effect(&buf, 0.5f);
            printf("[BP-LOOP] iter=%d counter=%u rms=%.3f tempo=%.1f\n",
                   i, g_counter, rms, g_tempo);
        }
        printf("[BP-LOOP] Done, counter=%u\n", g_counter);
    } else if (strcmp(mode, "step-target") == 0) {
        // Designed for stepping tests.
        // Each function call is on its own source line for clear step targets.
        printf("[STEP] Start\n");
        g_counter = 0;
        auto buf = audio::generate_sine(440.0f);
        float rms = audio::process_buffer(&buf);
        audio::apply_effect(&buf, 0.5f);
        midi::note_on(60, 100);
        midi::control_change(1, 64);
        g_counter = 42;
        printf("[STEP] Done counter=%u rms=%.3f\n", g_counter, rms);
    } else if (strcmp(mode, "write-target") == 0) {
        // For debug_write tests: loops calling process_buffer, exits when g_counter>=999.
        // Uses >= because process_buffer/generate_sine increment g_counter, so the write
        // to 999 might be followed by an increment before the check runs.
        printf("[WRITE] Waiting for g_counter to reach 999\n");
        g_counter = 0;
        for (int i = 0; i < 100; i++) {
            auto buf = audio::generate_sine(440.0f);
            audio::process_buffer(&buf);
            if (g_counter >= 999) {
                printf("[WRITE] g_counter reached 999 (actual=%u) at iteration %d\n", g_counter, i);
                return 0;
            }
            usleep(50000); // 50ms
        }
        printf("[WRITE] Timed out, g_counter=%u\n", g_counter);
    } else {
        fprintf(stderr, "Unknown mode: %s\n", mode);
        fprintf(stderr, "Usage: %s [hello|crash-null|crash-abort|crash-stack|fork-workers|fork-exec|slow-functions|threads|globals|breakpoint-loop|step-target|write-target]\n", argv[0]);
        return 1;
    }

    return 0;
}
