from modules import audio

def test_audio_generate_sine():
    buf = audio.generate_sine(440.0)
    assert len(buf) == 512

def test_audio_process_buffer(audio_buffer):
    rms = audio.process_buffer(audio_buffer)
    assert rms > 0.0

def test_audio_apply_effect(audio_buffer):
    audio.apply_effect(audio_buffer, 2.0)
    rms = audio.process_buffer(audio_buffer)
    assert rms > 0.0

def test_audio_intentional_failure():
    """Intentional failure for adapter validation."""
    assert audio.process_buffer([]) == 1.0  # Will fail: returns 0.0
