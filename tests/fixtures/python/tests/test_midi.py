from modules import midi

def test_midi_note_on():
    assert midi.note_on(60, 100) is True

def test_midi_control_change():
    assert midi.control_change(1, 64) is True

def test_midi_generate_sequence():
    seq = midi.generate_sequence(8)
    assert len(seq) == 8
