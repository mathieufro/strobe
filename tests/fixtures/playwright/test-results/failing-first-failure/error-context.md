# Instructions

- Following Playwright test failed.
- Explain why, be concise, respect Playwright best practices.
- Provide a snippet of code with the fix, if possible.

# Test info

- Name: failing.spec.ts >> first failure
- Location: tests/failing.spec.ts:3:5

# Error details

```
Error: expect(received).toBe(expected) // Object.is equality

Expected: 3
Received: 2
```

# Test source

```ts
  1  | import { test, expect } from "@playwright/test";
  2  | 
  3  | test("first failure", () => {
> 4  |   expect(1 + 1).toBe(3);
     |                 ^ Error: expect(received).toBe(expected) // Object.is equality
  5  | });
  6  | 
  7  | test("second failure", () => {
  8  |   expect("a").toBe("b");
  9  | });
  10 | 
  11 | test("would pass if reached", () => {
  12 |   expect(true).toBe(true);
  13 | });
  14 | 
```