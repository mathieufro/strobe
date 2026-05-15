import { test, expect } from "@playwright/test";

test("first failure", () => {
  expect(1 + 1).toBe(3);
});

test("second failure", () => {
  expect("a").toBe("b");
});

test("would pass if reached", () => {
  expect(true).toBe(true);
});
