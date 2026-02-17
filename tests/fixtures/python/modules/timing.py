"""Timing functions with varied durations."""
import time

def fast():
    time.sleep(0.001)  # 1ms

def medium():
    time.sleep(0.05)   # 50ms

def slow():
    time.sleep(0.1)    # 100ms

def very_slow():
    time.sleep(0.3)    # 300ms
