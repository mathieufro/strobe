"""Main sidecar server: reads JSON from stdin, writes JSON to stdout."""

import json
import sys
import time
from .protocol import DetectRequest, DetectResponse, ErrorResponse, PongResponse, DetectedElement
from .omniparser import OmniParser
from .models import select_device


def main():
    parser = OmniParser()
    device = select_device()

    print(f"strobe-vision sidecar starting (device={device})", file=sys.stderr)

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue

        try:
            data = json.loads(line)
        except json.JSONDecodeError as e:
            resp = ErrorResponse(id="unknown", message=f"Invalid JSON: {e}")
            sys.stdout.write(resp.to_json() + "\n")
            sys.stdout.flush()
            continue

        req_id = data.get("id", "unknown")
        req_type = data.get("type", "")

        try:
            if req_type == "ping":
                resp = PongResponse(
                    id=req_id,
                    models_loaded=parser.is_loaded,
                    device=device,
                )
                sys.stdout.write(resp.to_json() + "\n")
                sys.stdout.flush()

            elif req_type == "detect":
                req = DetectRequest.from_json(data)

                start = time.monotonic()
                elements = parser.detect(
                    req.image,
                    confidence_threshold=req.confidence_threshold,
                    iou_threshold=req.iou_threshold,
                )
                elapsed_ms = int((time.monotonic() - start) * 1000)

                resp = DetectResponse(
                    id=req_id,
                    elements=[
                        {
                            "label": e.label,
                            "description": e.description,
                            "confidence": e.confidence,
                            "bounds": e.bounds,
                        }
                        for e in elements
                    ],
                    latency_ms=elapsed_ms,
                )
                sys.stdout.write(resp.to_json() + "\n")
                sys.stdout.flush()

            else:
                resp = ErrorResponse(id=req_id, message=f"Unknown request type: {req_type}")
                sys.stdout.write(resp.to_json() + "\n")
                sys.stdout.flush()

        except Exception as e:
            import traceback
            traceback.print_exc(file=sys.stderr)
            resp = ErrorResponse(id=req_id, message=str(e))
            sys.stdout.write(resp.to_json() + "\n")
            sys.stdout.flush()


if __name__ == "__main__":
    main()
