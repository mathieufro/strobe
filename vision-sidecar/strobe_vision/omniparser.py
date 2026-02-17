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
        import torch
        mdir = models_dir()

        # Load OmniParser v2.0 fine-tuned YOLO icon detection model
        from ultralytics import YOLO
        yolo_path = f"{mdir}/icon_detect/model.pt"
        self.yolo_model = YOLO(yolo_path)
        print(f"Loaded YOLO from {yolo_path}", file=sys.stderr)

        # Load Florence-2 caption model (OmniParser v2.0 fine-tuned)
        # trust_remote_code=True required: Florence-2 uses custom model architecture.
        # Processor comes from base Florence-2, model weights are fine-tuned.
        from transformers import AutoModelForCausalLM, AutoProcessor
        caption_path = f"{mdir}/icon_caption"

        # Processor from base Florence-2 (fine-tuned weights don't include tokenizer)
        self.caption_processor = AutoProcessor.from_pretrained(
            "microsoft/Florence-2-base", trust_remote_code=True
        )

        # Model from fine-tuned OmniParser weights
        if self.device == "cpu":
            self.caption_model = AutoModelForCausalLM.from_pretrained(
                caption_path, torch_dtype=torch.float32, trust_remote_code=True
            )
        else:
            self.caption_model = AutoModelForCausalLM.from_pretrained(
                caption_path, torch_dtype=torch.float16, trust_remote_code=True
            ).to(self.device)
        print(f"Loaded Florence-2 from {caption_path} on {self.device}", file=sys.stderr)

        self._loaded = True

    def detect(
        self, image_b64: str, confidence_threshold: float = 0.01, iou_threshold: float = 0.1
    ) -> list[DetectedElement]:
        """Detect UI elements in a base64-encoded PNG image.

        Default thresholds match OmniParser v2 reference: conf=0.01, iou=0.1.
        """
        self.load()

        # SEC-3: Validate base64 size to prevent memory exhaustion
        MAX_IMAGE_SIZE = 50 * 1024 * 1024  # 50MB base64 limit
        if len(image_b64) > MAX_IMAGE_SIZE:
            raise ValueError(f"Image too large: {len(image_b64)} bytes exceeds 50MB limit")

        # Decode image
        img_bytes = base64.b64decode(image_b64)
        image = Image.open(io.BytesIO(img_bytes)).convert("RGB")

        # SEC-3: Validate image dimensions (4K limit)
        MAX_PIXELS = 3840 * 2160
        if image.width * image.height > MAX_PIXELS:
            raise ValueError(f"Image too large: {image.width}x{image.height} exceeds 4K limit")

        # Run YOLO detection
        results = self.yolo_model(
            image, conf=confidence_threshold, iou=iou_threshold, verbose=False
        )

        elements = []
        if results and len(results) > 0:
            boxes = results[0].boxes
            w, h = image.size

            # Filter overlapping boxes: keep smaller box when IoU > threshold
            filtered = self._remove_overlap(boxes, iou_threshold)

            for box_data in filtered:
                x1, y1, x2, y2 = box_data['xyxy']
                conf = box_data['conf']

                # Crop and resize to 64x64 for captioning (matches OmniParser reference)
                crop = image.crop((int(x1), int(y1), int(x2), int(y2)))
                crop = crop.resize((64, 64))
                label, description = self._caption_crop(crop)

                elements.append(DetectedElement(
                    label=label or "icon",
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

    def _remove_overlap(self, boxes, iou_threshold: float) -> list[dict]:
        """Remove overlapping boxes, keeping the smaller one (OmniParser strategy)."""
        box_list = []
        for box in boxes:
            x1, y1, x2, y2 = box.xyxy[0].tolist()
            conf = float(box.conf[0])
            area = (x2 - x1) * (y2 - y1)
            box_list.append({'xyxy': (x1, y1, x2, y2), 'conf': conf, 'area': area})

        filtered = []
        for i, b1 in enumerate(box_list):
            is_valid = True
            for j, b2 in enumerate(box_list):
                if i != j and self._compute_iou(b1['xyxy'], b2['xyxy']) > iou_threshold:
                    if b1['area'] > b2['area']:
                        is_valid = False
                        break
            if is_valid:
                filtered.append(b1)
        return filtered

    @staticmethod
    def _compute_iou(box1, box2) -> float:
        """Compute IoU with OmniParser's extended metric (max of IoU, ratio1, ratio2)."""
        x1 = max(box1[0], box2[0])
        y1 = max(box1[1], box2[1])
        x2 = min(box1[2], box2[2])
        y2 = min(box1[3], box2[3])
        intersection = max(0, x2 - x1) * max(0, y2 - y1)

        area1 = (box1[2] - box1[0]) * (box1[3] - box1[1])
        area2 = (box2[2] - box2[0]) * (box2[3] - box2[1])
        union = area1 + area2 - intersection + 1e-6

        iou = intersection / union
        ratio1 = intersection / (area1 + 1e-6)
        ratio2 = intersection / (area2 + 1e-6)
        return max(iou, ratio1, ratio2)

    def _caption_crop(self, crop: Image.Image) -> tuple[str, str]:
        """Use Florence-2 to caption a cropped UI element."""
        try:
            import torch
            prompt = "<CAPTION>"

            if self.device != "cpu":
                inputs = self.caption_processor(
                    images=crop, text=prompt, return_tensors="pt", do_resize=False
                ).to(device=self.device, dtype=torch.float16)
            else:
                inputs = self.caption_processor(
                    images=crop, text=prompt, return_tensors="pt"
                ).to(device=self.device)

            with torch.no_grad():
                generated = self.caption_model.generate(
                    input_ids=inputs["input_ids"],
                    pixel_values=inputs["pixel_values"],
                    max_new_tokens=20, num_beams=1, do_sample=False
                )
            caption = self.caption_processor.batch_decode(
                generated, skip_special_tokens=True
            )[0].strip()

            # Extract label (first word) and description (full caption)
            parts = caption.split()
            label = parts[0].lower() if parts else "icon"
            return label, caption
        except Exception as e:
            import sys
            print(f"Caption error: {e}", file=sys.stderr)
            return "icon", ""

    @property
    def is_loaded(self) -> bool:
        return self._loaded
