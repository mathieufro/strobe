"""Model loading and device selection."""

import os
import sys
import torch


def select_device() -> str:
    """Auto-detect best available device: mps > cuda > cpu."""
    if torch.backends.mps.is_available():
        return "mps"
    elif torch.cuda.is_available():
        return "cuda"
    return "cpu"


def models_dir() -> str:
    """Resolve models directory. Check bundled location first, then ~/.strobe/models/."""
    # Bundled with sidecar package
    pkg_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    bundled = os.path.join(pkg_dir, "models")
    if os.path.isdir(bundled):
        return bundled

    # User home
    home = os.path.join(os.path.expanduser("~"), ".strobe", "models")
    if os.path.isdir(home):
        return home

    print(f"ERROR: No models directory found at {bundled} or {home}", file=sys.stderr)
    sys.exit(1)
