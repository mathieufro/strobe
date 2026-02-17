"""Audio processing module."""
import math
import functools

def generate_sine(frequency: float, size: int = 512, sample_rate: int = 44100) -> list:
    """Generate a sine wave buffer."""
    return [math.sin(2 * math.pi * frequency * i / sample_rate) for i in range(size)]

def process_buffer(buf: list) -> float:
    """Calculate RMS of buffer."""
    if not buf:
        return 0.0
    sum_sq = sum(x * x for x in buf)
    return math.sqrt(sum_sq / len(buf))

def apply_effect(buf: list, gain: float) -> None:
    """Apply gain effect in-place."""
    for i in range(len(buf)):
        buf[i] *= gain

def timing_decorator(func):
    """Simple timing decorator for testing."""
    @functools.wraps(func)
    def wrapper(*args, **kwargs):
        return func(*args, **kwargs)
    return wrapper

@timing_decorator
def decorated_process(freq: float) -> float:
    """Decorated function for testing dynamic resolution."""
    buf = generate_sine(freq)
    return process_buffer(buf)
