#!/usr/bin/env python3
"""Download vision models for Strobe UI observation."""

import os
import sys
from pathlib import Path

try:
    from transformers import AutoProcessor, AutoModelForCausalLM
    import torch
except ImportError:
    print("ERROR: Required packages not installed", file=sys.stderr)
    print("Run: pip install transformers torch ultralytics pillow", file=sys.stderr)
    sys.exit(1)


def models_dir():
    """Get models directory (same as strobe_vision.models.models_dir)."""
    home = Path.home()
    return home / ".strobe" / "models"


def download_yolo():
    """Download YOLOv8 model."""
    detect_dir = models_dir() / "icon_detect"
    detect_dir.mkdir(parents=True, exist_ok=True)

    model_path = detect_dir / "yolov8n.pt"
    if model_path.exists():
        print(f"✓ YOLOv8 already downloaded: {model_path}")
        return

    print("Downloading YOLOv8n (~25MB)...")
    try:
        from ultralytics import YOLO
        # This will auto-download the model
        model = YOLO("yolov8n.pt")
        # Move to our models directory
        import shutil
        default_path = Path.home() / ".cache" / "ultralytics" / "yolov8n.pt"
        if default_path.exists():
            shutil.copy(default_path, model_path)
            print(f"✓ YOLOv8 downloaded: {model_path}")
        else:
            print(f"! YOLOv8 downloaded to cache, manually copy to {model_path}")
    except Exception as e:
        print(f"✗ Failed to download YOLOv8: {e}", file=sys.stderr)
        sys.exit(1)


def download_florence():
    """Download Florence-2 model."""
    caption_dir = models_dir() / "icon_caption"
    caption_dir.mkdir(parents=True, exist_ok=True)

    config_path = caption_dir / "config.json"
    if config_path.exists():
        print(f"✓ Florence-2 already downloaded: {caption_dir}")
        return

    print("Downloading Florence-2 base (~1.5GB, this will take a while)...")
    try:
        model_id = "microsoft/Florence-2-base"
        processor = AutoProcessor.from_pretrained(model_id)
        model = AutoModelForCausalLM.from_pretrained(model_id)

        processor.save_pretrained(str(caption_dir))
        model.save_pretrained(str(caption_dir))
        print(f"✓ Florence-2 downloaded: {caption_dir}")
    except Exception as e:
        print(f"✗ Failed to download Florence-2: {e}", file=sys.stderr)
        sys.exit(1)


def main():
    print("Strobe Vision Models Setup")
    print("=" * 50)

    mdir = models_dir()
    print(f"Models directory: {mdir}\n")

    download_yolo()
    print()
    download_florence()
    print()

    print("=" * 50)
    print("✓ All models downloaded successfully!")
    print(f"\nTo enable vision in Strobe, add to ~/.strobe/settings.json:")
    print('  {"vision.enabled": true}')


if __name__ == "__main__":
    main()
