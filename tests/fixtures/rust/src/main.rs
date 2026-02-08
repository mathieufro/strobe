use std::sync::atomic::Ordering;
use strobe_test_fixture::*;

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "basic".to_string());

    match mode.as_str() {
        "basic" => {
            println!("Running basic mode");
            let buf = audio::generate_sine(440.0);
            let rms = audio::process_buffer(&buf);
            println!("Audio RMS: {}", rms);
            midi::note_on(60, 100);
            midi::control_change(1, 64);
            engine::update_state();
            engine::print_stats();
            println!("Done");
        }
        "threads" => {
            println!("[THREADS] Starting multi-threaded mode");

            let handles: Vec<_> = (0..2)
                .map(|i| {
                    std::thread::Builder::new()
                        .name(format!("audio-{}", i))
                        .spawn(move || {
                            for _ in 0..100 {
                                let buf = audio::generate_sine(440.0);
                                audio::process_buffer(&buf);
                                G_BUFFER_COUNT.fetch_add(1, Ordering::Relaxed);
                                std::thread::sleep(std::time::Duration::from_millis(10));
                            }
                        })
                        .unwrap()
                })
                .collect();

            let midi_handle = std::thread::Builder::new()
                .name("midi-processor".to_string())
                .spawn(|| {
                    for note in 0..100u8 {
                        midi::note_on(note % 128, 100);
                        G_NOTE_COUNT.fetch_add(1, Ordering::Relaxed);
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                })
                .unwrap();

            for h in handles {
                h.join().unwrap();
            }
            midi_handle.join().unwrap();
            engine::print_stats();
            println!("[THREADS] Done");
        }
        "globals" => {
            println!("[GLOBALS] Starting");
            for i in 0..50u64 {
                G_BUFFER_COUNT.store(i, Ordering::Relaxed);
                G_NOTE_COUNT.store(i * 2, Ordering::Relaxed);
                G_TEMPO.store(120_000 + i * 100, Ordering::Relaxed);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            println!("[GLOBALS] Done");
        }
        _ => {
            eprintln!("Unknown mode: {}", mode);
            std::process::exit(1);
        }
    }
}
