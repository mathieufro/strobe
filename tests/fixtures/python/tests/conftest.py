import pytest

@pytest.fixture
def audio_buffer():
    from modules.audio import generate_sine
    return generate_sine(440.0)
