import { test, expect } from "@playwright/test";

test("addition works", () => {
  expect(1 + 1).toBe(2);
});

test("string equality", () => {
  expect("hi").toBe("hi");
});
