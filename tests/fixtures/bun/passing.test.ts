import { test, expect } from "bun:test";

test("addition works", () => {
  expect(1 + 1).toBe(2);
});

test("string equality", () => {
  expect("hello").toBe("hello");
});
