# Vision Models Setup

The vision pipeline requires two models:
1. **YOLOv8** for UI element detection (~25MB)
2. **Florence-2** for element captioning (~1.5GB)

## Quick Setup

```bash
cd vision-sidecar
python3 setup_models.py
```

This will download models to `~/.strobe/models/`:
- `~/.strobe/models/icon_detect/` — YOLOv8 model
- `~/.strobe/models/icon_caption/` — Florence-2 model

## Manual Download

If automated download fails, manually download from HuggingFace:

### YOLOv8 (Icon Detection)
```bash
mkdir -p ~/.strobe/models/icon_detect
cd ~/.strobe/models/icon_detect

# Download YOLOv8n (nano) model - fastest, adequate accuracy
wget https://github.com/ultralytics/assets/releases/download/v0.0.0/yolov8n.pt
```

### Florence-2 (Icon Captioning)
```bash
mkdir -p ~/.strobe/models/icon_caption

# Download Florence-2 base model
python3 << EOF
from transformers import AutoProcessor, AutoModelForCausalLM

model_id = "microsoft/Florence-2-base"
save_dir = "~/.strobe/models/icon_caption"

# Download model and processor
processor = AutoProcessor.from_pretrained(model_id)
model = AutoModelForCausalLM.from_pretrained(model_id)

# Save locally
processor.save_pretrained(save_dir)
model.save_pretrained(save_dir)
print(f"Saved to {save_dir}")
EOF
```

## Device Requirements

- **CPU**: Works on all systems (slow, ~1-2s per frame)
- **Apple Silicon (M1/M2/M3)**: Requires PyTorch with MPS support (~200-500ms per frame)
- **NVIDIA GPU**: Requires CUDA-enabled PyTorch (~100-300ms per frame)

## Verify Installation

```bash
cd vision-sidecar
python3 -c "from strobe_vision.models import models_dir; print(models_dir())"
```

Should output: `/Users/<you>/.strobe/models` (or similar)

Check models exist:
```bash
ls -lh ~/.strobe/models/icon_detect/
ls -lh ~/.strobe/models/icon_caption/
```

## Troubleshooting

### "No such file or directory: icon_detect"
Models not downloaded. Run `python3 setup_models.py`.

### "OutOfMemoryError"
Florence-2 is large (~1.5GB). Reduce batch size or use CPU:
```bash
export STROBE_VISION_DEVICE=cpu
```

### ImportError: transformers
```bash
pip install transformers torch ultralytics pillow
```

## Storage Space

Total disk usage: ~1.6GB
- YOLOv8: 25MB
- Florence-2: 1.5GB
- PyTorch dependencies: varies by platform
