import pytest
from modules import engine

def test_engine_counter():
    engine.g_counter = 42
    assert engine.g_counter == 42

def test_engine_tempo():
    engine.g_tempo = 140.0
    assert engine.g_tempo == 140.0

@pytest.mark.skip(reason="Skipped for adapter validation")
def test_engine_skipped():
    pass
