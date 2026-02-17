"""Crash scenarios."""
import os
import ctypes

def raise_exception():
    raise RuntimeError("intentional crash for testing")

def abort_signal():
    os.abort()

def null_deref():
    ctypes.string_at(0)

def stack_overflow(depth=0):
    return stack_overflow(depth + 1)
