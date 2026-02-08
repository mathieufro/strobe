pub mod audio;
pub mod engine;
pub mod midi;

use std::sync::atomic::AtomicU64;

pub static G_SAMPLE_RATE: AtomicU64 = AtomicU64::new(44100);
pub static G_TEMPO: AtomicU64 = AtomicU64::new(120_000);
pub static G_BUFFER_COUNT: AtomicU64 = AtomicU64::new(0);
pub static G_NOTE_COUNT: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_process() {
        let buf = audio::generate_sine(440.0);
        let rms = audio::process_buffer(&buf);
        assert!(rms > 0.0);
    }

    #[test]
    fn test_midi_note_on() {
        assert!(midi::note_on(60, 100));
    }

    #[test]
    fn test_engine_update() {
        engine::update_state();
    }

    #[test]
    #[ignore]
    fn test_ignored_for_now() {
        todo!("not implemented yet");
    }

    #[test]
    fn test_intentional_failure() {
        assert_eq!(1, 2, "intentional failure for adapter testing");
    }
}
