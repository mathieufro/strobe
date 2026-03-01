import { describe, it, expect } from 'vitest';

function add(a, b) { return a + b; }
function sub(a, b) { return a - b; }
function mul(a, b) { return a * b; }

describe('Math operations', () => {
  it('adds two numbers', () => {
    expect(add(2, 3)).toBe(5);
  });

  it('subtracts two numbers', () => {
    expect(sub(10, 4)).toBe(6);
  });

  it('multiplies two numbers', () => {
    expect(mul(3, 7)).toBe(21);
  });
});
