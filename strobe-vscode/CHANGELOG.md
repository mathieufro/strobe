# Changelog

## 0.1.0 — Initial Release

### Features
- **Launch & Trace:** Right-click any function to trace it at runtime with Strobe
- **Debug Adapter Protocol:** Full DAP support — breakpoints, stepping, variable inspection, stack traces
- **Memory Inspector:** Read/write arbitrary memory addresses and DWARF variables
- **Live Watch Viewer:** Monitor variables with auto-polling, scoped to specific functions
- **Test Explorer:** Discover and run Rust (Cargo) and Go tests with Strobe instrumentation
- **Retained Sessions:** Stop sessions with retention for post-mortem analysis
- **Inline Decorations:** See call counts, average duration, and return values inline in your code
- **Settings UI:** Configure event limits, polling intervals, and serialization depth
- **Keyboard Shortcuts:** Quick access to launch, trace, breakpoints, and memory inspector

### Supported Languages
- Rust (native DWARF, `::` patterns, Cargo test discovery)
- C/C++ (native DWARF, `::` patterns)
- Swift (native DWARF, `.` patterns)
- Go (native DWARF, `.` patterns, `go test` discovery)
