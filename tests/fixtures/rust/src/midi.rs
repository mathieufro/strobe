pub fn note_on(note: u8, velocity: u8) -> bool {
    note < 128 && velocity > 0
}

pub fn control_change(cc: u8, value: u8) -> bool {
    let _ = value;
    cc < 128
}

pub fn generate_sequence(length: usize) -> Vec<(u8, u8)> {
    (0..length)
        .map(|i| (60 + (i as u8 % 12), 80 + (i as u8 % 40)))
        .collect()
}
