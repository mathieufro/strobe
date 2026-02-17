"""MIDI processing module."""

def note_on(note: int, velocity: int) -> bool:
    """Process a MIDI note-on event."""
    return 0 <= note <= 127 and 0 <= velocity <= 127

def control_change(cc: int, value: int) -> bool:
    """Process a MIDI control change."""
    return 0 <= cc <= 127 and 0 <= value <= 127

def generate_sequence(length: int) -> list:
    """Generate a sequence of MIDI events."""
    return [{"note": 60 + (i % 12), "velocity": 100} for i in range(length)]
