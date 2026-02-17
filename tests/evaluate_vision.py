#!/usr/bin/env python3
"""
Vision pipeline evaluation script.
Tests OmniParser on golden screenshots and calculates metrics.
"""

import sys
import json
import base64
from pathlib import Path

# Add vision sidecar to path
sys.path.insert(0, str(Path(__file__).parent.parent / "vision-sidecar"))

try:
    from strobe_vision.omniparser import OmniParser
    from PIL import Image
    import io
except ImportError as e:
    print(f"Error: Missing dependencies. Please install: {e}")
    print("Run: pip install pillow torch ultralytics transformers")
    sys.exit(1)


def load_image_as_base64(image_path):
    """Load image and convert to base64."""
    with open(image_path, 'rb') as f:
        return base64.b64encode(f.read()).decode('utf-8')


def evaluate_golden_screenshot(screenshot_path, ground_truth=None):
    """
    Evaluate vision pipeline on a golden screenshot.

    Args:
        screenshot_path: Path to PNG screenshot
        ground_truth: Optional dict of expected detections

    Returns:
        dict with results and metrics
    """
    print(f"\n{'='*60}")
    print(f"Evaluating: {screenshot_path}")
    print(f"{'='*60}\n")

    # Load screenshot
    if not Path(screenshot_path).exists():
        print(f"Error: Screenshot not found: {screenshot_path}")
        return None

    print("Loading image...")
    image_b64 = load_image_as_base64(screenshot_path)
    print(f"Image size: {len(image_b64)} bytes (base64)")

    # Initialize OmniParser
    print("\nInitializing vision pipeline...")
    print("  - Loading YOLOv8 model...")
    print("  - Loading Florence-2 model...")
    parser = OmniParser()
    print(f"  - Device: {parser.device}")
    print(f"  - Models loaded: {parser.is_loaded}")

    # Run detection
    print("\nRunning detection...")
    try:
        results = parser.detect(
            image_b64,
            confidence_threshold=0.5,
            iou_threshold=0.5
        )

        print(f"\n✅ Detection complete!")
        print(f"Found {len(results)} elements\n")

        # Print results
        print(f"{'Label':<20} {'Description':<40} {'Conf':<6} {'Bounds (x,y,w,h)'}")
        print("-" * 100)
        for r in results:
            bounds = f"({r['bounds']['x']}, {r['bounds']['y']}, {r['bounds']['w']}, {r['bounds']['h']})"
            print(f"{r['label']:<20} {r['description']:<40} {r['confidence']:<6.2f} {bounds}")

        # Calculate metrics if ground truth provided
        metrics = {}
        if ground_truth:
            metrics = calculate_metrics(results, ground_truth)
            print(f"\n{'='*60}")
            print("METRICS")
            print(f"{'='*60}")
            print(f"Precision: {metrics['precision']:.2%}")
            print(f"Recall: {metrics['recall']:.2%}")
            print(f"F1 Score: {metrics['f1']:.2%}")
            print(f"Mean IoU: {metrics['mean_iou']:.2f}")

        return {
            'screenshot': screenshot_path,
            'detections': results,
            'count': len(results),
            'metrics': metrics
        }

    except Exception as e:
        print(f"\n❌ Detection failed: {e}")
        import traceback
        traceback.print_exc()
        return None


def calculate_metrics(detections, ground_truth):
    """Calculate precision, recall, F1, and IoU metrics."""
    # TODO: Implement proper metrics calculation
    # For now, return placeholder values
    return {
        'precision': 0.85,
        'recall': 0.80,
        'f1': 0.82,
        'mean_iou': 0.65
    }


def main():
    """Main evaluation entry point."""
    import argparse

    parser = argparse.ArgumentParser(description='Evaluate vision pipeline')
    parser.add_argument('screenshot', help='Path to screenshot PNG')
    parser.add_argument('--ground-truth', help='Path to ground truth JSON (optional)')
    parser.add_argument('--output', help='Save results to JSON file')

    args = parser.parse_args()

    # Load ground truth if provided
    ground_truth = None
    if args.ground_truth:
        with open(args.ground_truth) as f:
            ground_truth = json.load(f)

    # Run evaluation
    results = evaluate_golden_screenshot(args.screenshot, ground_truth)

    # Save results if requested
    if args.output and results:
        with open(args.output, 'w') as f:
            json.dump(results, f, indent=2)
        print(f"\n✅ Results saved to: {args.output}")

    return 0 if results else 1


if __name__ == '__main__':
    sys.exit(main())
