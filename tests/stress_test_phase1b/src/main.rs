use clap::Parser;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicBool, AtomicI32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

// ============ Global State (for watch variable testing) ============

static G_SAMPLE_RATE: AtomicU64 = AtomicU64::new(44100);
static G_BUFFER_SIZE: AtomicU64 = AtomicU64::new(512);
static G_TEMPO: AtomicU64 = AtomicU64::new(120_000); // millibeat
static G_MIDI_NOTE_ON_COUNT: AtomicU64 = AtomicU64::new(0);
static G_AUDIO_BUFFER_COUNT: AtomicU64 = AtomicU64::new(0);
static G_PARAMETER_UPDATES: AtomicU64 = AtomicU64::new(0);
static G_EFFECT_CHAIN_DEPTH: AtomicI32 = AtomicI32::new(0);

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "10")]
    duration: u64,

    #[arg(long, default_value = "4")]
    audio_threads: usize,

    #[arg(long, default_value = "true")]
    enable_midi: bool,

    #[arg(long, default_value = "true")]
    enable_automation: bool,
}

// ============ Audio DSP Module ============

mod audio {
    use super::*;

    #[repr(C)]
    #[derive(Debug)]
    pub struct AudioBuffer {
        pub samples: [f32; 512],
        pub sample_rate: u64,
        pub channels: usize,
        pub timestamp: u64,
    }

    #[repr(C)]
    #[derive(Debug)]
    pub struct EffectParams {
        pub gain: f32,
        pub pan: f32,
        pub wet_dry: f32,
        pub frequency: f32,
        pub resonance: f32,
    }

    #[repr(C)]
    #[derive(Debug)]
    pub struct EffectChain {
        pub level: i32,
        pub params: Box<EffectParams>,
        pub next: Option<Box<EffectChain>>,
    }

    pub fn create_effect_chain(depth: i32) -> EffectChain {
        G_EFFECT_CHAIN_DEPTH.store(depth, Ordering::Relaxed);

        if depth <= 0 {
            return EffectChain {
                level: 0,
                params: Box::new(EffectParams {
                    gain: 1.0,
                    pan: 0.0,
                    wet_dry: 0.5,
                    frequency: 1000.0,
                    resonance: 0.7,
                }),
                next: None,
            };
        }

        EffectChain {
            level: depth,
            params: Box::new(EffectParams {
                gain: 0.8 + (depth as f32 * 0.1),
                pan: (depth as f32 * 0.1).sin(),
                wet_dry: 0.5,
                frequency: 1000.0 + (depth as f32 * 100.0),
                resonance: 0.7,
            }),
            next: Some(Box::new(create_effect_chain(depth - 1))),
        }
    }

    pub fn process_audio_buffer(buffer: &mut AudioBuffer, chain: &EffectChain) -> f32 {
        let mut sum = 0.0f32;

        // Simulate DSP processing
        for i in 0..buffer.samples.len() {
            let sample = buffer.samples[i];
            let processed = apply_effect_chain(sample, chain);
            buffer.samples[i] = processed;
            sum += processed.abs();
        }

        G_AUDIO_BUFFER_COUNT.fetch_add(1, Ordering::Relaxed);
        sum / buffer.samples.len() as f32
    }

    fn apply_effect_chain(mut sample: f32, chain: &EffectChain) -> f32 {
        // Apply current effect
        sample *= chain.params.gain;
        sample = sample * (1.0 - chain.params.pan.abs()) +
                 sample * chain.params.pan * chain.params.wet_dry;

        // Recurse through chain
        if let Some(ref next) = chain.next {
            sample = apply_effect_chain(sample, next);
        }

        sample
    }

    pub fn generate_sine_buffer(frequency: f32, phase: &mut f32) -> AudioBuffer {
        let sample_rate = G_SAMPLE_RATE.load(Ordering::Relaxed);
        let mut buffer = AudioBuffer {
            samples: [0.0; 512],
            sample_rate,
            channels: 2,
            timestamp: G_AUDIO_BUFFER_COUNT.load(Ordering::Relaxed),
        };

        for i in 0..buffer.samples.len() {
            buffer.samples[i] = (*phase).sin() * 0.5;
            *phase += 2.0 * std::f32::consts::PI * frequency / sample_rate as f32;
            if *phase > 2.0 * std::f32::consts::PI {
                *phase -= 2.0 * std::f32::consts::PI;
            }
        }

        buffer
    }
}

// ============ MIDI Module ============

mod midi {
    use super::*;

    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct MidiMessage {
        pub status: u8,
        pub data1: u8,
        pub data2: u8,
        pub timestamp: u64,
    }

    pub fn process_note_on(note: u8, velocity: u8) -> bool {
        G_MIDI_NOTE_ON_COUNT.fetch_add(1, Ordering::Relaxed);

        let tempo = G_TEMPO.load(Ordering::Relaxed);
        let should_trigger = (note as u64 * velocity as u64) % tempo < 1000;

        if should_trigger {
            println!("[MIDI] Note ON: {} vel: {} at tempo {}", note, velocity, tempo / 1000);
        }

        should_trigger
    }

    pub fn process_control_change(cc: u8, _value: u8) {
        G_PARAMETER_UPDATES.fetch_add(1, Ordering::Relaxed);

        match cc {
            7 => { /* Volume */ }
            10 => { /* Pan */ }
            74 => { /* Filter cutoff */ }
            _ => {}
        }
    }

    pub fn generate_midi_sequence(pattern: u8) -> Vec<MidiMessage> {
        let mut messages = Vec::new();
        let base_time = G_AUDIO_BUFFER_COUNT.load(Ordering::Relaxed);

        // Generate note sequence based on pattern
        for i in 0..16 {
            if (pattern >> (i % 8)) & 1 == 1 {
                messages.push(MidiMessage {
                    status: 0x90, // Note on
                    data1: 60 + i,
                    data2: 64 + (i * 4),
                    timestamp: base_time + (i as u64 * 100),
                });
            }
        }

        messages
    }
}

// ============ Engine Module ============

mod engine {
    use super::*;

    pub struct Engine {
        pub running: Arc<AtomicBool>,
        pub start_time: Instant,
    }

    impl Engine {
        pub fn new() -> Self {
            Engine {
                running: Arc::new(AtomicBool::new(true)),
                start_time: Instant::now(),
            }
        }

        pub fn update_global_state(&self) {
            let elapsed = self.start_time.elapsed().as_millis() as u64;

            // Modulate tempo over time
            let tempo = 100_000 + ((elapsed / 100) % 80_000);
            G_TEMPO.store(tempo, Ordering::Relaxed);

            // Occasionally change buffer size
            if elapsed % 5000 < 100 {
                let sizes = [128, 256, 512, 1024];
                let idx = ((elapsed / 5000) % 4) as usize;
                G_BUFFER_SIZE.store(sizes[idx], Ordering::Relaxed);
            }
        }

        pub fn print_statistics(&self) {
            let elapsed = self.start_time.elapsed().as_secs_f64();
            let buffers = G_AUDIO_BUFFER_COUNT.load(Ordering::Relaxed);
            let midi_notes = G_MIDI_NOTE_ON_COUNT.load(Ordering::Relaxed);
            let params = G_PARAMETER_UPDATES.load(Ordering::Relaxed);

            println!("\n[ENGINE STATS]");
            println!("  Elapsed: {:.2}s", elapsed);
            println!("  Audio buffers: {} ({:.0} buffers/sec)", buffers, buffers as f64 / elapsed);
            println!("  MIDI notes: {} ({:.0} notes/sec)", midi_notes, midi_notes as f64 / elapsed);
            println!("  Parameter updates: {} ({:.0} updates/sec)", params, params as f64 / elapsed);
            println!("  Sample rate: {}", G_SAMPLE_RATE.load(Ordering::Relaxed));
            println!("  Buffer size: {}", G_BUFFER_SIZE.load(Ordering::Relaxed));
            println!("  Tempo: {} BPM", G_TEMPO.load(Ordering::Relaxed) / 1000);
            println!();
        }
    }
}

// ============ Main ============

fn main() {
    let args = Args::parse();

    println!("[AUDIO DSP STRESS TEST] Running for {} seconds", args.duration);
    println!("  Audio threads: {}", args.audio_threads);
    println!("  MIDI enabled: {}", args.enable_midi);
    println!("  Automation enabled: {}", args.enable_automation);
    println!();

    let engine = Arc::new(engine::Engine::new());
    let mut handles = vec![];

    // Audio processing threads (HOT PATH - should trigger auto-sampling)
    for thread_id in 0..args.audio_threads {
        let engine = Arc::clone(&engine);
        let duration = args.duration;

        let handle = thread::Builder::new()
            .name(format!("audio-{}", thread_id))
            .spawn(move || {
                let mut phase = 0.0f32;
                let base_freq = 440.0 * (1.0 + thread_id as f32 * 0.1);
                let effect_chain = audio::create_effect_chain(5);
                let mut buffer_count = 0u64;

                while engine.running.load(Ordering::Relaxed) &&
                      engine.start_time.elapsed().as_secs() < duration {

                    // Generate audio buffer
                    let mut buffer = audio::generate_sine_buffer(base_freq, &mut phase);

                    // Process through effect chain (deep call stack)
                    let _rms = audio::process_audio_buffer(&mut buffer, &effect_chain);

                    buffer_count += 1;

                    // Simulate real-time audio constraint (512 samples @ 44.1kHz ≈ 11.6ms)
                    if buffer_count % 10 == 0 {
                        thread::sleep(Duration::from_micros(100));
                    }
                }

                buffer_count
            })
            .unwrap();

        handles.push((format!("audio-{}", thread_id), handle));
    }

    // MIDI processing thread
    if args.enable_midi {
        let engine = Arc::clone(&engine);
        let duration = args.duration;

        let handle = thread::Builder::new()
            .name("midi-processor".to_string())
            .spawn(move || {
                let mut message_count = 0u64;
                let patterns = [0b10101010, 0b11001100, 0b11110000, 0b10011001];
                let mut pattern_idx = 0;

                while engine.running.load(Ordering::Relaxed) &&
                      engine.start_time.elapsed().as_secs() < duration {

                    // Generate MIDI sequence
                    let messages = midi::generate_midi_sequence(patterns[pattern_idx]);
                    pattern_idx = (pattern_idx + 1) % patterns.len();

                    // Process each message
                    for msg in messages {
                        if msg.status == 0x90 {
                            midi::process_note_on(msg.data1, msg.data2);
                            message_count += 1;
                        }
                    }

                    // MIDI events come in bursts
                    thread::sleep(Duration::from_millis(50));
                }

                message_count
            })
            .unwrap();

        handles.push(("midi-processor".to_string(), handle));
    }

    // Parameter automation thread
    if args.enable_automation {
        let engine = Arc::clone(&engine);
        let duration = args.duration;

        let handle = thread::Builder::new()
            .name("automation".to_string())
            .spawn(move || {
                let mut automation_count = 0u64;

                while engine.running.load(Ordering::Relaxed) &&
                      engine.start_time.elapsed().as_secs() < duration {

                    // Automate various parameters
                    let elapsed_ms = engine.start_time.elapsed().as_millis() as u8;

                    midi::process_control_change(7, elapsed_ms % 128);  // Volume
                    midi::process_control_change(10, 127_u8.saturating_sub(elapsed_ms)); // Pan
                    midi::process_control_change(74, (elapsed_ms / 2) % 128); // Filter

                    automation_count += 3;

                    thread::sleep(Duration::from_millis(10));
                }

                automation_count
            })
            .unwrap();

        handles.push(("automation".to_string(), handle));
    }

    // Statistics thread
    let engine_stats = Arc::clone(&engine);
    let duration = args.duration;
    let stats_handle = thread::Builder::new()
        .name("stats".to_string())
        .spawn(move || {
            let mut iterations = 0u64;
            while engine_stats.running.load(Ordering::Relaxed) &&
                  engine_stats.start_time.elapsed().as_secs() < duration {

                engine_stats.update_global_state();
                engine_stats.print_statistics();
                iterations += 1;

                thread::sleep(Duration::from_secs(2));
            }
            iterations
        })
        .unwrap();

    handles.push(("stats".to_string(), stats_handle));

    // Wait for completion
    thread::sleep(Duration::from_secs(args.duration));
    engine.running.store(false, Ordering::Relaxed);

    // Join all threads
    println!("\n[SHUTDOWN] Joining threads...\n");
    for (name, handle) in handles {
        match handle.join() {
            Ok(count) => {
                if name.starts_with("audio") || name == "midi-processor" || name == "automation" {
                    println!("[SHUTDOWN] {}: {} operations", name, count);
                }
            }
            Err(_) => eprintln!("[SHUTDOWN] {} panicked", name),
        }
    }

    // Final statistics
    engine.print_statistics();
}
