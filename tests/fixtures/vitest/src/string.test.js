import { describe, it, expect } from 'vitest';

function upper(s) { return s.toUpperCase(); }
function lower(s) { return s.toLowerCase(); }
function reverse(s) { return s.split('').reverse().join(''); }

describe('String operations', () => {
  it('converts to uppercase', () => {
    expect(upper('hello')).toBe('HELLO');
  });

  it('converts to lowercase', () => {
    expect(lower('WORLD')).toBe('world');
  });

  it('reverses a string', () => {
    expect(reverse('abc')).toBe('cba');
  });
});
