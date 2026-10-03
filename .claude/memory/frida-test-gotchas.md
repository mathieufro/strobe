---
name: frida-test-gotchas
description: "Pitfalls when writing integration tests that attach Frida sessions"
metadata:
  type: reference
---

- Integration tests that use Frida must run sequentially inside a single `#[tokio::test]`; parallel Frida sessions SIGSEGV.
- Use `project_root = fixture.parent()`, not the grandparent: JsResolver scans the whole directory tree, and a broad root makes same-named functions in different fixtures ambiguous.
- ESM hook temp files: store the actual path returned by `generate_esm_hook_script` and clean up that path (a format-string guess for cleanup was wrong before).
