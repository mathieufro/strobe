import { describe, it, expect, afterAll } from 'vitest';

// This test has a slow afterAll that delays vitest finalization,
// simulating the scenario where JSON stdout is never written
// but STROBE_TEST events are already streamed.

let cleanupDelay = 0;

// Check env var to enable slow cleanup (only for specific test scenarios)
if (process.env.STROBE_SLOW_CLEANUP) {
  cleanupDelay = parseInt(process.env.STROBE_SLOW_CLEANUP, 10) || 0;
}

describe('Slow cleanup tests', () => {
  afterAll(async () => {
    if (cleanupDelay > 0) {
      // Simulate a hanging afterAll (e.g., server shutdown)
      await new Promise(resolve => setTimeout(resolve, cleanupDelay));
    }
  });

  it('passes quickly', () => {
    expect(1 + 1).toBe(2);
  });

  it('also passes quickly', () => {
    expect('hello').toContain('ell');
  });
});
