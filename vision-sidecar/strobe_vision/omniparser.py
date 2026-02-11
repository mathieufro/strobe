"""OmniParser v2 wrapper (YOLOv8 + Florence-2)."""

import base64
import io
import time
from PIL import Image
from .models import models_dir, select_device
from .protocol import DetectedElement


class OmniParser:
    def __init__(self):
        self.device = select_device()
        self.yolo_model = None
        self.caption_model = None
        self.caption_processor = None
        self._loaded = False

    def load(self):
        """Load models into device memory."""
        if self._loaded:
            return

        import sys
        mdir = models_dir()

        # Load YOLO detection model
        from ultralytics import YOLO
        yolo_path = f"{mdir}/icon_detect/best.pt"
        self.yolo_model = YOLO(yolo_path)
        print(f"Loaded YOLO from {yolo_path}", file=sys.stderr)

        # Load Florence-2 caption model
        from transformers import AutoModelForCausalLM, AutoProcessor
        caption_path = f"{mdir}/icon_caption"
        self.caption_processor = AutoProcessor.from_pretrained(
            caption_path, trust_remote_code=True
        )
        self.caption_model = AutoModelForCausalLM.from_pretrained(
            caption_path, trust_remote_code=True
        )
        if self.device != "cpu":
            self.caption_model = self.caption_model.to(self.device)
        print(f"Loaded Florence-2 from {caption_path} on {self.device}", file=sys.stderr)

        self._loaded = True

    def detect(
        self, image_b64: str, confidence_threshold: float = 0.3, iou_threshold: float = 0.5
    ) -> list[DetectedElement]:
        """Detect UI elements in a base64-encoded PNG image."""
        self.load()

        # Decode image
        img_bytes = base64.b64decode(image_b64)
        image = Image.open(io.BytesIO(img_bytes)).convert("RGB")

        # Run YOLO detection
        results = self.yolo_model(
            image, conf=confidence_threshold, iou=iou_threshold, verbose=False
        )

        elements = []
        if results and len(results) > 0:
            boxes = results[0].boxes
            for box in boxes:
                x1, y1, x2, y2 = box.xyxy[0].tolist()
                conf = float(box.conf[0])
                cls_id = int(box.cls[0])

                # Crop for captioning
                crop = image.crop((int(x1), int(y1), int(x2), int(y2)))
                label, description = self._caption_crop(crop)

                elements.append(DetectedElement(
                    label=label or f"element_{cls_id}",
                    description=description or "",
                    confidence=round(conf, 3),
                    bounds={
                        "x": int(x1),
                        "y": int(y1),
                        "w": int(x2 - x1),
                        "h": int(y2 - y1),
                    },
                ))

        return elements

    def _caption_crop(self, crop: Image.Image) -> tuple[str, str]:
        """Use Florence-2 to caption a cropped UI element."""
        try:
            import torch
            prompt = "<CAPTION>"
            inputs = self.caption_processor(
                text=prompt, images=crop, return_tensors="pt"
            )
            if self.device != "cpu":
                inputs = {k: v.to(self.device) if hasattr(v, 'to') else v for k, v in inputs.items()}

            with torch.no_grad():
                generated = self.caption_model.generate(
                    **inputs, max_length=50, num_beams=3
                )
            caption = self.caption_processor.batch_decode(
                generated, skip_special_tokens=True
            )[0].strip()

            # Extract label (first word) and description (full caption)
            parts = caption.split()
            label = parts[0].lower() if parts else "element"
            return label, caption
        except Exception as e:
            import sys
            print(f"Caption error: {e}", file=sys.stderr)
            return "element", ""

    @property
    def is_loaded(self) -> bool:
        return self._loaded
