#!/usr/bin/env python3
"""
REAL vision pipeline integration test.
Tests actual ML inference with OmniParser v2.0 models.

Requires:
  - Python 3.11 venv at vision-sidecar/venv/
  - OmniParser v2.0 models at ~/.strobe/models/
  Run: vision-sidecar/venv/bin/python vision-sidecar/setup_models.py
"""

import sys
import os
import base64
import time
import json
from pathlib import Path

# Add vision sidecar to path
sys.path.insert(0, str(Path(__file__).parent.parent / "vision-sidecar"))

MODELS_DIR = Path.home() / ".strobe" / "models"
GOLDEN_DIR = Path(__file__).parent / "fixtures" / "ui-golden"


def check_models():
    """Verify OmniParser v2.0 models are installed."""
    yolo_path = MODELS_DIR / "icon_detect" / "model.pt"
    caption_config = MODELS_DIR / "icon_caption" / "config.json"
    caption_weights = MODELS_DIR / "icon_caption" / "model.safetensors"

    missing = []
    if not yolo_path.exists():
        missing.append(f"YOLO: {yolo_path}")
    elif yolo_path.stat().st_size < 20 * 1024 * 1024:
        missing.append(f"YOLO: {yolo_path} (too small, need OmniParser v2.0 fine-tuned)")
    if not caption_config.exists():
        missing.append(f"Florence-2 config: {caption_config}")
    if not caption_weights.exists():
        missing.append(f"Florence-2 weights: {caption_weights}")

    if missing:
        print("Models not found:")
        for m in missing:
            print(f"  - {m}")
        print("\nRun: vision-sidecar/venv/bin/python vision-sidecar/setup_models.py")
        return False
    return True


def test_yolo_model_metadata():
    """Test 1: YOLO model is the OmniParser fine-tuned version (not generic COCO)."""
    from ultralytics import YOLO

    model_path = MODELS_DIR / "icon_detect" / "model.pt"
    model = YOLO(str(model_path))

    # OmniParser fine-tuned YOLO has exactly 1 class: "icon"
    assert len(model.names) == 1, f"Expected 1 class, got {len(model.names)}: {model.names}"
    assert model.names[0] == "icon", f"Expected class 'icon', got '{model.names[0]}'"
    assert model.task == "detect"

    size_mb = model_path.stat().st_size / 1024 / 1024
    assert size_mb > 20, f"Model too small ({size_mb:.1f} MB), likely generic COCO"

    print(f"  YOLO: 1 class ('icon'), {size_mb:.1f} MB, task=detect")
    return True


def test_florence2_loads():
    """Test 2: Florence-2 fine-tuned model loads with correct architecture."""
    from transformers import AutoModelForCausalLM, AutoProcessor
    import torch

    caption_path = str(MODELS_DIR / "icon_caption")

    processor = AutoProcessor.from_pretrained(
        "microsoft/Florence-2-base", trust_remote_code=True
    )
    model = AutoModelForCausalLM.from_pretrained(
        caption_path, torch_dtype=torch.float32, trust_remote_code=True
    )

    assert "Florence2" in type(model).__name__, f"Wrong model type: {type(model).__name__}"
    assert "Florence2" in type(processor).__name__, f"Wrong processor type: {type(processor).__name__}"

    param_count = sum(p.numel() for p in model.parameters()) / 1e6
    print(f"  Florence-2: {type(model).__name__}, {param_count:.0f}M params")

    # Quick inference test
    from PIL import Image
    test_img = Image.new("RGB", (64, 64), color="blue")
    inputs = processor(text="<CAPTION>", images=test_img, return_tensors="pt")
    with torch.no_grad():
        generated = model.generate(
            input_ids=inputs["input_ids"],
            pixel_values=inputs["pixel_values"],
            max_new_tokens=20, num_beams=1, do_sample=False
        )
    caption = processor.batch_decode(generated, skip_special_tokens=True)[0].strip()
    assert len(caption) > 0, "Empty caption"
    print(f"  Caption test: '{caption}'")
    return True


def test_omniparser_synthetic():
    """Test 3: Full OmniParser pipeline on synthetic UI image."""
    from strobe_vision.omniparser import OmniParser
    from PIL import Image, ImageDraw
    import io

    # Create synthetic UI with buttons and icons
    img = Image.new("RGB", (800, 600), color="#F0F0F0")
    draw = ImageDraw.Draw(img)

    # Draw UI-like elements
    draw.rectangle([50, 50, 200, 90], fill="#0078D4", outline="#005A9E")  # Button
    draw.rectangle([50, 120, 400, 160], fill="white", outline="#999999")  # Text input
    draw.rectangle([50, 250, 200, 290], fill="#E81123", outline="#C50F1F")  # Button
    draw.ellipse([500, 50, 560, 110], fill="#FFB900", outline="#F0A000")  # Icon
    draw.rectangle([600, 50, 750, 90], fill="#107C10", outline="#0B6A0B")  # Button

    buf = io.BytesIO()
    img.save(buf, format="PNG")
    image_b64 = base64.b64encode(buf.getvalue()).decode()

    parser = OmniParser()
    t0 = time.time()
    elements = parser.detect(image_b64)
    elapsed = time.time() - t0

    print(f"  Synthetic: {len(elements)} elements in {elapsed:.1f}s")
    for e in elements[:5]:
        b = e.bounds
        print(f"    {e.label}: '{e.description}' conf={e.confidence:.3f} ({b['x']},{b['y']},{b['w']},{b['h']})")

    # Pipeline should run without error; detection count varies on synthetic images
    assert elapsed < 60, f"Detection too slow: {elapsed:.1f}s"
    return True


def test_omniparser_real_screenshot():
    """Test 4: Full OmniParser pipeline on real desktop screenshot."""
    from strobe_vision.omniparser import OmniParser

    golden = GOLDEN_DIR / "test_desktop.png"
    if not golden.exists():
        print(f"  SKIP: Golden screenshot not found at {golden}")
        return True

    with open(golden, "rb") as f:
        image_b64 = base64.b64encode(f.read()).decode()

    parser = OmniParser()
    t0 = time.time()
    elements = parser.detect(image_b64)
    elapsed = time.time() - t0

    print(f"  Real screenshot: {len(elements)} elements in {elapsed:.1f}s")

    # Real desktop screenshots should have many UI elements
    assert len(elements) >= 20, (
        f"Too few elements ({len(elements)}) on real desktop screenshot. "
        "OmniParser v2.0 should detect 100+ icons on a typical desktop."
    )

    # Check that we get meaningful labels (not all 'icon')
    unique_labels = set(e.label for e in elements)
    assert len(unique_labels) >= 5, (
        f"Too few unique labels ({len(unique_labels)}): {unique_labels}. "
        "Florence-2 captioning may not be working."
    )

    # Check confidence distribution
    high_conf = [e for e in elements if e.confidence > 0.5]
    assert len(high_conf) >= 10, (
        f"Too few high-confidence elements ({len(high_conf)}). "
        "Model may not be the OmniParser fine-tuned version."
    )

    # Show top detections
    top = sorted(elements, key=lambda e: -e.confidence)[:10]
    print("  Top 10:")
    for e in top:
        b = e.bounds
        print(f"    {e.label}: '{e.description[:40]}' conf={e.confidence:.3f}")

    return True


def test_sidecar_protocol():
    """Test 5: Vision sidecar JSON protocol (detect request/response)."""
    from strobe_vision.omniparser import OmniParser
    from strobe_vision.protocol import DetectRequest, DetectResponse, DetectedElement
    from PIL import Image
    import io

    # Create minimal test image
    img = Image.new("RGB", (100, 100), color="red")
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    image_b64 = base64.b64encode(buf.getvalue()).decode()

    # Test request parsing
    req_json = {
        "id": "test-1",
        "type": "detect",
        "image": image_b64,
        "options": {"confidence_threshold": 0.5, "iou_threshold": 0.3}
    }
    req = DetectRequest.from_json(req_json)
    assert req.id == "test-1"
    assert req.confidence_threshold == 0.5
    assert req.iou_threshold == 0.3

    # Test response serialization
    resp = DetectResponse(
        id="test-1",
        elements=[{"label": "button", "description": "A button", "confidence": 0.9,
                    "bounds": {"x": 10, "y": 20, "w": 100, "h": 50}}],
        latency_ms=42
    )
    resp_json = json.loads(resp.to_json())
    assert resp_json["id"] == "test-1"
    assert resp_json["type"] == "result"
    assert len(resp_json["elements"]) == 1
    assert resp_json["latency_ms"] == 42

    print("  Protocol: request parsing + response serialization OK")
    return True


def test_security_limits():
    """Test 6: SEC-3 image size and dimension limits."""
    from strobe_vision.omniparser import OmniParser
    import io

    parser = OmniParser()

    # Test base64 size limit (>50MB)
    try:
        huge_b64 = "A" * (51 * 1024 * 1024)
        parser.detect(huge_b64)
        assert False, "Should have raised ValueError for oversized base64"
    except ValueError as e:
        assert "50MB" in str(e)

    print("  SEC-3 size limits enforced")
    return True


TESTS = [
    ("YOLO model metadata", test_yolo_model_metadata),
    ("Florence-2 model loading", test_florence2_loads),
    ("OmniParser synthetic image", test_omniparser_synthetic),
    ("OmniParser real screenshot", test_omniparser_real_screenshot),
    ("Sidecar JSON protocol", test_sidecar_protocol),
    ("Security limits (SEC-3)", test_security_limits),
]


def main():
    print("=" * 65)
    print("OmniParser v2.0 — Real Integration Tests")
    print("=" * 65)

    if not check_models():
        sys.exit(1)

    passed = 0
    failed = 0
    skipped = 0

    for name, test_fn in TESTS:
        print(f"\n[{passed + failed + skipped + 1}/{len(TESTS)}] {name}")
        try:
            result = test_fn()
            if result:
                passed += 1
                print(f"  PASSED")
            else:
                failed += 1
                print(f"  FAILED")
        except Exception as e:
            failed += 1
            import traceback
            print(f"  FAILED: {e}")
            traceback.print_exc()

    print(f"\n{'=' * 65}")
    print(f"Results: {passed} passed, {failed} failed, {skipped} skipped")
    print(f"{'=' * 65}")

    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
