#!/usr/bin/env python3
"""Strobe test fixture — Python equivalent of C++ strobe_test_target."""

import sys
import os

def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "hello"

    if mode == "hello":
        print("Hello from Python fixture")
        print("Debug output on stderr", file=sys.stderr)

    elif mode == "crash-exception":
        print(f"[TARGET] PID={os.getpid()} mode=crash-exception")
        sys.stdout.flush()
        raise RuntimeError("intentional crash for testing")

    elif mode == "crash-abort":
        print(f"[TARGET] PID={os.getpid()} mode=crash-abort")
        sys.stdout.flush()
        os.abort()

    elif mode == "crash-segfault":
        import ctypes
        print(f"[TARGET] PID={os.getpid()} mode=crash-segfault")
        sys.stdout.flush()
        ctypes.string_at(0)  # SIGSEGV

    elif mode == "slow-functions":
        from modules import timing
        print("[TIMING] Running functions with varied durations...")
        for round_num in range(5):
            timing.fast()
            timing.fast()
            timing.fast()
            timing.medium()
            timing.slow()
            if round_num == 2:
                timing.very_slow()
        print("[TIMING] Done")

    elif mode == "threads":
        import threading
        from modules import audio, midi
        print("[THREADS] Starting multi-threaded mode")

        def audio_worker(worker_id):
            for i in range(50):
                buf = audio.generate_sine(440.0)
                audio.process_buffer(buf)
                import time; time.sleep(0.01)

        def midi_worker():
            for i in range(50):
                midi.note_on(60 + (i % 12), 100)
                import time; time.sleep(0.02)

        threads = [
            threading.Thread(target=audio_worker, args=(0,), name="audio-0"),
            threading.Thread(target=audio_worker, args=(1,), name="audio-1"),
            threading.Thread(target=midi_worker, name="midi-processor"),
        ]
        for t in threads: t.start()
        for t in threads: t.join()
        print("[THREADS] Done")

    elif mode == "globals":
        from modules import engine, audio
        print("[GLOBALS] Starting global variable updates")
        for i in range(200):
            engine.g_counter = i
            engine.g_tempo = 120.0 + (i % 10)
            engine.g_point["x"] = float(i)
            engine.g_point["y"] = float(i * 2)
            buf = audio.generate_sine(440.0)
            audio.process_buffer(buf)
            import time; time.sleep(0.1)
        print("[GLOBALS] Done")

    elif mode == "breakpoint-loop":
        from modules import audio, engine
        iterations = int(sys.argv[2]) if len(sys.argv) > 2 else 10
        print(f"[BP-LOOP] Running {iterations} iterations")
        for i in range(iterations):
            engine.g_counter = i
            engine.g_tempo = 120.0 + i
            buf = audio.generate_sine(440.0)
            rms = audio.process_buffer(buf)
            audio.apply_effect(buf, 0.5)
            print(f"[BP-LOOP] iter={i} counter={engine.g_counter} rms={rms:.3f} tempo={engine.g_tempo:.1f}")
        print(f"[BP-LOOP] Done, counter={engine.g_counter}")

    elif mode == "step-target":
        from modules import audio, midi, engine
        print("[STEP] Start")
        engine.g_counter = 0
        buf = audio.generate_sine(440.0)
        rms = audio.process_buffer(buf)
        audio.apply_effect(buf, 0.5)
        midi.note_on(60, 100)
        midi.control_change(1, 64)
        engine.g_counter = 42
        print(f"[STEP] Done counter={engine.g_counter} rms={rms:.3f}")

    elif mode == "write-target":
        from modules import audio, engine
        import time
        print("[WRITE] Waiting for g_counter to reach 999")
        engine.g_counter = 0
        for i in range(100):
            buf = audio.generate_sine(440.0)
            audio.process_buffer(buf)
            if engine.g_counter >= 999:
                print(f"[WRITE] g_counter reached 999 (actual={engine.g_counter}) at iteration {i}")
                return
            time.sleep(0.05)
        print(f"[WRITE] Timed out, g_counter={engine.g_counter}")

    elif mode == "async-demo":
        import asyncio
        from modules import audio

        async def async_process():
            buf = audio.generate_sine(440.0)
            await asyncio.sleep(0.01)
            return audio.process_buffer(buf)

        async def async_main():
            results = await asyncio.gather(
                async_process(),
                async_process(),
                async_process(),
            )
            print(f"[ASYNC] Results: {results}")

        asyncio.run(async_main())

    elif mode == "decorators":
        from modules.audio import decorated_process
        result = decorated_process(440.0)
        print(f"[DECORATORS] Result: {result}")

    else:
        print(f"Unknown mode: {mode}", file=sys.stderr)
        sys.exit(1)

if __name__ == "__main__":
    main()
