"""JSON protocol types for daemon <-> sidecar communication."""

from dataclasses import dataclass, field, asdict
from typing import Optional
import json


@dataclass
class DetectRequest:
    id: str
    type: str  # "detect"
    image: str  # base64 PNG
    confidence_threshold: float = 0.3
    iou_threshold: float = 0.5

    @classmethod
    def from_json(cls, data: dict) -> "DetectRequest":
        opts = data.get("options", {})
        return cls(
            id=data["id"],
            type=data["type"],
            image=data["image"],
            confidence_threshold=opts.get("confidence_threshold", 0.3),
            iou_threshold=opts.get("iou_threshold", 0.5),
        )


@dataclass
class DetectedElement:
    label: str
    description: str
    confidence: float
    bounds: dict  # {"x": int, "y": int, "w": int, "h": int}


@dataclass
class DetectResponse:
    id: str
    type: str = "result"
    elements: list = field(default_factory=list)
    latency_ms: int = 0

    def to_json(self) -> str:
        return json.dumps(asdict(self))


@dataclass
class ErrorResponse:
    id: str
    type: str = "error"
    message: str = ""

    def to_json(self) -> str:
        return json.dumps(asdict(self))


@dataclass
class PongResponse:
    id: str
    type: str = "pong"
    models_loaded: bool = False
    device: str = "cpu"

    def to_json(self) -> str:
        return json.dumps(asdict(self))
