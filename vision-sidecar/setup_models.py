#!/usr/bin/env python3
"""Download OmniParser v2.0 vision models for Strobe UI observation.

Downloads fine-tuned models from microsoft/OmniParser-v2.0 on HuggingFace:
  - icon_detect/model.pt: Fine-tuned YOLOv8 for UI icon detection (~39MB)
  - icon_caption/: Fine-tuned Florence-2-base for icon captioning (~1GB)
"""

import os
import sys
from pathlib import Path


def models_dir():
    """Get models directory."""
    return Path.home() / ".strobe" / "models"


def check_dependencies():
    """Verify required packages are installed."""
    missing = []
    try:
        import torch
    except ImportError:
        missing.append("torch")
    try:
        import ultralytics
    except ImportError:
        missing.append("ultralytics")
    try:
        import transformers
    except ImportError:
        missing.append("transformers")
    try:
        from huggingface_hub import hf_hub_download
    except ImportError:
        missing.append("huggingface_hub")

    if missing:
        print(f"ERROR: Missing packages: {', '.join(missing)}", file=sys.stderr)
        print("Install with: pip install torch ultralytics transformers huggingface_hub pillow", file=sys.stderr)
        sys.exit(1)


def download_icon_detect():
    """Download OmniParser v2.0 fine-tuned YOLO icon detection model."""
    from huggingface_hub import hf_hub_download

    detect_dir = models_dir() / "icon_detect"
    detect_dir.mkdir(parents=True, exist_ok=True)

    model_path = detect_dir / "model.pt"
    if model_path.exists():
        size_mb = model_path.stat().st_size / 1024 / 1024
        if size_mb > 20:  # Fine-tuned model is ~39MB, generic is ~6MB
            print(f"  YOLO icon_detect already downloaded: {model_path} ({size_mb:.1f} MB)")
            return
        else:
            print(f"  Found generic YOLO model ({size_mb:.1f} MB), replacing with fine-tuned...")

    for fname in ["model.pt", "model.yaml", "train_args.yaml"]:
        print(f"  Downloading icon_detect/{fname}...")
        hf_hub_download(
            repo_id="microsoft/OmniParser-v2.0",
            filename=f"icon_detect/{fname}",
            local_dir=str(models_dir()),
        )

    size_mb = model_path.stat().st_size / 1024 / 1024
    print(f"  YOLO icon_detect downloaded: {model_path} ({size_mb:.1f} MB)")


def download_icon_caption():
    """Download OmniParser v2.0 fine-tuned Florence-2 caption model."""
    from huggingface_hub import hf_hub_download

    caption_dir = models_dir() / "icon_caption"
    caption_dir.mkdir(parents=True, exist_ok=True)

    safetensors_path = caption_dir / "model.safetensors"
    if safetensors_path.exists():
        size_mb = safetensors_path.stat().st_size / 1024 / 1024
        if size_mb > 500:  # Fine-tuned is ~1GB
            print(f"  Florence-2 icon_caption already downloaded: {caption_dir} ({size_mb:.0f} MB)")
            return

    for fname in ["config.json", "generation_config.json", "model.safetensors"]:
        print(f"  Downloading icon_caption/{fname}...")
        hf_hub_download(
            repo_id="microsoft/OmniParser-v2.0",
            filename=f"icon_caption/{fname}",
            local_dir=str(models_dir()),
        )

    size_mb = safetensors_path.stat().st_size / 1024 / 1024
    print(f"  Florence-2 icon_caption downloaded: {caption_dir} ({size_mb:.0f} MB)")


def download_florence2_processor():
    """Ensure Florence-2 base processor (tokenizer) is cached.

    The OmniParser fine-tuned model only ships weights, not the processor.
    The processor comes from the base microsoft/Florence-2-base model.

    SECURITY NOTE: trust_remote_code=True executes arbitrary Python from the
    HuggingFace model repo. We pin to a specific revision to reduce supply chain
    risk. Update FLORENCE2_REVISION only after auditing the new repo contents.
    """
    from transformers import AutoProcessor

    # Pin to known-good revision to mitigate trust_remote_code supply chain risk.
    # Florence-2-base requires trust_remote_code for its custom processor.
    FLORENCE2_REVISION = "refs/pr/6"

    print("  Caching Florence-2-base processor (tokenizer)...")
    print("  WARNING: trust_remote_code=True is required for Florence-2 processor.")
    print(f"  Pinned to revision: {FLORENCE2_REVISION}")
    try:
        AutoProcessor.from_pretrained(
            "microsoft/Florence-2-base",
            trust_remote_code=True,
            revision=FLORENCE2_REVISION,
        )
        print("  Florence-2-base processor cached.")
    except Exception as e:
        print(f"  WARNING: Failed to cache processor: {e}", file=sys.stderr)
        print("  The processor will be downloaded on first use.", file=sys.stderr)


def setup_flash_attn_stub():
    """Create flash_attn stub for macOS (no CUDA).

    Florence-2 imports flash_attn but it's CUDA-only.
    A stub module prevents ImportError on macOS/CPU.
    """
    import torch
    if torch.cuda.is_available():
        return  # Real flash_attn can be installed on CUDA

    try:
        import flash_attn
        return  # Already available
    except ImportError:
        pass

    # Find site-packages in current env
    import site
    site_packages = site.getsitepackages()
    if not site_packages:
        return

    stub_dir = Path(site_packages[0]) / "flash_attn"
    if stub_dir.exists():
        print("  flash_attn stub already exists.")
        return

    print("  Creating flash_attn stub (macOS/CPU, no CUDA)...")
    stub_dir.mkdir(parents=True, exist_ok=True)
    (stub_dir / "__init__.py").write_text(
        '"""Stub for flash_attn on non-CUDA platforms."""\n'
    )
    print("  flash_attn stub created.")


def main():
    print("Strobe Vision - OmniParser v2.0 Model Setup")
    print("=" * 55)

    mdir = models_dir()
    print(f"Models directory: {mdir}\n")

    check_dependencies()

    print("1. YOLO icon detection model (~39 MB)")
    download_icon_detect()

    print("\n2. Florence-2 icon caption model (~1 GB)")
    download_icon_caption()

    print("\n3. Florence-2 base processor (tokenizer)")
    download_florence2_processor()

    print("\n4. flash_attn compatibility")
    setup_flash_attn_stub()

    print("\n" + "=" * 55)
    print("All OmniParser v2.0 models ready!")
    print(f"\nModels installed at: {mdir}")


if __name__ == "__main__":
    main()
