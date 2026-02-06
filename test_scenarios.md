# Strobe Testing Scenarios - ERAE Touch MK2 Firmware

## Test Binary
- **Path**: `/Users/alex/erae_touch_mk2_fw/build/bin/erae_mk2_simulator.app/Contents/MacOS/erae_mk2_simulator`
- **Working Directory**: `/Users/alex/erae_touch_mk2_fw`

---

## Scenario 1: Basic Initialization Check (Easy)
**Goal**: Verify the simulator starts cleanly and identify any initialization warnings

**Features Used**:
- stdout/stderr capture (automatic)
- No tracing patterns

**Expected Findings**:
- Initialization messages
- Any JUCE framework warnings
- Hardware simulation status

**MCP Commands**:
```typescript
// Launch with no tracing - just capture output
debug_launch({
  command: "/Users/alex/erae_touch_mk2_fw/build/bin/erae_mk2_simulator.app/Contents/MacOS/erae_mk2_simulator",
  projectRoot: "/Users/alex/erae_touch_mk2_fw"
})

// Wait for startup, then query stderr
debug_query({
  sessionId: "...",
  eventType: "stderr",
  limit: 100
})

// Query stdout
debug_query({
  sessionId: "...",
  eventType: "stdout",
  limit: 100
})
```

---

## Scenario 2: Clock Initialization Tracing (Medium)
**Goal**: Trace the internal clock initialization sequence and tempo setting

**Features Used**:
- Function tracing with targeted patterns
- Multiple pattern addition during runtime
- Function enter/exit events

**Target Functions**:
- `embodme::InternalClock::*` - All clock methods
- `embodme::TimerClock::*` - Hardware timer methods
- `embodme::EraeMK2::run` - Main loop entry

**Expected Findings**:
- Order of clock initialization
- Default tempo setting
- Main loop start timing

**MCP Commands**:
```typescript
// Launch clean
debug_launch({
  command: "/Users/alex/erae_touch_mk2_fw/build/bin/erae_mk2_simulator.app/Contents/MacOS/erae_mk2_simulator",
  projectRoot: "/Users/alex/erae_touch_mk2_fw"
})

// Add clock tracing patterns
debug_trace({
  sessionId: "...",
  add: [
    "embodme::InternalClock::*",
    "embodme::TimerClock::*",
    "embodme::EraeMK2::run"
  ]
})

// Query function traces
debug_query({
  sessionId: "...",
  eventType: "function_enter",
  function: { contains: "Clock" },
  verbose: true,
  limit: 50
})
```

---

## Scenario 3: Clock Stability Monitoring (Hard)
**Goal**: Monitor clock position and tempo stability during operation, checking for jitter

**Features Used**:
- Watch variables with contextual filtering
- Multiple watches with different contexts
- Pattern matching (`*` wildcard)

**Watch Targets**:
- `mClockPos` during tick processing
- `mPpqnTempo` during tempo changes
- `mIsRunning` during transport control

**Expected Findings**:
- Clock position increment consistency
- Tempo drift or stability
- Transport state transitions

**MCP Commands**:
```typescript
// Launch clean
debug_launch({
  command: "/Users/alex/erae_touch_mk2_fw/build/bin/erae_mk2_simulator.app/Contents/MacOS/erae_mk2_simulator",
  projectRoot: "/Users/alex/erae_touch_mk2_fw"
})

// Add targeted traces with contextual watches
debug_trace({
  sessionId: "...",
  add: [
    "embodme::InternalClock::tickInternalProcess",
    "embodme::InternalClock::setBeatPerMinute",
    "embodme::InternalClock::start",
    "embodme::InternalClock::stop"
  ],
  watches: {
    add: [
      {
        variable: "mClockPos",
        on: ["embodme::InternalClock::tickInternalProcess"]
      },
      {
        variable: "mPpqnTempo",
        on: ["embodme::InternalClock::*"]
      },
      {
        variable: "mIsRunning",
        on: ["embodme::InternalClock::start", "embodme::InternalClock::stop"]
      }
    ]
  }
})

// Query traces with watches
debug_query({
  sessionId: "...",
  eventType: "function_enter",
  function: { contains: "InternalClock" },
  verbose: true,
  limit: 100
})
```

---

## Scenario 4: Touch-to-MIDI Latency Analysis (Advanced)
**Goal**: Measure end-to-end latency from touch detection to MIDI output, identify bottlenecks

**Features Used**:
- Complex multi-function tracing
- Multiple contextual watches
- Deep wildcard patterns (`**`)
- File-based pattern matching
- High event limits

**Watch Targets**:
- `sFrameCounter` in touch interrupt
- `mFingers` during event creation
- MIDI queue state during transmission
- Global timestamp correlation

**Target Functions**:
- `@file:finger_detector` - Touch processing
- `embodme::MidiManager::*` - MIDI routing
- `embodme::FingerDetector::*` - Finger tracking
- `EXTI3_IRQHandler` - Touch interrupt
- `TIM3_IRQHandler` - MIDI transmission interrupt

**Expected Findings**:
- Time between touch detection and MIDI note-on
- Processing stages and their durations
- Queue depths and potential bottlenecks
- Timestamp correlation across subsystems

**MCP Commands**:
```typescript
// Launch clean
debug_launch({
  command: "/Users/alex/erae_touch_mk2_fw/build/bin/erae_mk2_simulator.app/Contents/MacOS/erae_mk2_simulator",
  projectRoot: "/Users/alex/erae_touch_mk2_fw"
})

// Set higher event limit for detailed analysis
debug_trace({
  sessionId: "...",
  eventLimit: 500000,
  add: [
    "@file:finger_detector",
    "embodme::MidiManager::routeMidiEvent",
    "embodme::FingerDetector::process",
    "embodme::FingerDetector::createEvents",
    "EXTI3_IRQHandler",
    "TIM3_IRQHandler"
  ],
  watches: {
    add: [
      {
        variable: "sFrameCounter",
        on: ["EXTI3_IRQHandler"]
      },
      {
        variable: "mFingers",
        on: ["embodme::FingerDetector::createEvents"]
      },
      {
        variable: "timeUs",
        on: ["embodme::EraeMK2::run"]
      }
    ]
  }
})

// Query with narrow focus on latency path
debug_query({
  sessionId: "...",
  eventType: "function_enter",
  sourceFile: { contains: "finger_detector" },
  verbose: true,
  limit: 200
})

// Cross-reference MIDI output timing
debug_query({
  sessionId: "...",
  eventType: "function_enter",
  function: { contains: "MidiManager" },
  verbose: true,
  limit: 200
})
```

---

## MCP Interface Evaluation Criteria

During execution, evaluate:
1. **Clarity**: Are error messages clear?
2. **Workflow**: Is the multi-step process intuitive?
3. **Pattern syntax**: Do wildcards work as documented?
4. **Watch resolution**: Does contextual filtering match correctly?
5. **Query flexibility**: Can we find the data we need?
6. **Performance**: Does the system handle realistic loads?
7. **Documentation**: Are the tool descriptions accurate?

---

## Expected Issues to Surface

1. Pattern matching edge cases
2. Watch variable resolution in complex codebases
3. Query result pagination and filtering
4. Event limit overflow handling
5. Session lifecycle management
