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
    } else {
        fprintf(stderr, "Unknown mode: %s\n", mode);
        fprintf(stderr, "Usage: %s [hello|crash-null|crash-abort|crash-stack|fork-workers|fork-exec|slow-functions|threads|globals]\n", argv[0]);
        return 1;
    }

    return 0;
}
