import { test, expect } from 'vitest';

test('first failure', () => {
  expect(1 + 1).toBe(3);
});

test('second failure', () => {
  expect('a').toBe('b');
});
