use crate::{G_BUFFER_COUNT, G_NOTE_COUNT, G_SAMPLE_RATE, G_TEMPO};
use std::sync::atomic::Ordering;

pub fn update_state() {
    G_BUFFER_COUNT.fetch_add(1, Ordering::Relaxed);
    G_NOTE_COUNT.fetch_add(1, Ordering::Relaxed);
}

pub fn print_stats() {
    println!(
        "Stats: sample_rate={} tempo={} buffers={} notes={}",
        G_SAMPLE_RATE.load(Ordering::Relaxed),
        G_TEMPO.load(Ordering::Relaxed),
        G_BUFFER_COUNT.load(Ordering::Relaxed),
        G_NOTE_COUNT.load(Ordering::Relaxed),
    );
}
