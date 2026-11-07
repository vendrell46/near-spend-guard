import assert from "node:assert/strict";
import { test } from "node:test";

import { formatAmount, parseAmount } from "../src/amount.ts";
import { isPlainAscii, plain, printable } from "../src/display.ts";

test("whole tokens to base units", () => {
  assert.equal(parseAmount("5", 6), "5000000");
  assert.equal(parseAmount("5.25", 6), "5250000");
  assert.equal(parseAmount("0.000001", 6), "1");
  assert.equal(parseAmount("7", 0), "7");
  assert.equal(parseAmount("1", 24), "1000000000000000000000000");
  assert.equal(parseAmount("0.1", 24), "100000000000000000000000");
});

test("trailing zeros aren't precision", () => {
  assert.equal(parseAmount("5.2500000000", 6), "5250000");
  assert.equal(parseAmount("7.000", 0), "7");
});

test("nothing is rounded", () => {
  assert.throws(() => parseAmount("0.0000001", 6), /more decimals/);
  assert.throws(() => parseAmount("1.5", 0), /more decimals/);
  assert.throws(() => parseAmount("5.0000001", 6), /more decimals/);
});

test("only plain decimals are read", () => {
  for (const value of ["", ".5", "5.", "05", "+5", "-5", "1e3", "0x10", "5,25", " 5", "5 ", "٥"]) {
    assert.throws(() => parseAmount(value, 6), /decimal number/, value);
  }
});

test("zero is not a payment", () => {
  assert.throws(() => parseAmount("0", 6), /above 0/);
  assert.throws(() => parseAmount("0.000000", 6), /above 0/);
});

test("amounts stop at u128", () => {
  const max = ((1n << 128n) - 1n).toString();
  assert.equal(parseAmount(max, 0), max);
  assert.throws(() => parseAmount((1n << 128n).toString(), 0), /16 bytes/);
  assert.throws(() => parseAmount(max, 1), /16 bytes/);
});

test("base units to whole tokens", () => {
  assert.equal(formatAmount("5250000", 6), "5.25");
  assert.equal(formatAmount("5000000", 6), "5");
  assert.equal(formatAmount("1", 6), "0.000001");
  assert.equal(formatAmount("0", 6), "0");
  assert.equal(formatAmount("7", 0), "7");
  assert.equal(formatAmount("1000000000000000000000000", 24), "1");
});

test("format and parse invert each other", () => {
  for (const base of ["1", "10", "999999", "1000000", "1000001", "340282366920938463463374607431768211455"]) {
    assert.equal(parseAmount(formatAmount(base, 6), 6), base);
  }
});

test("a token's decimals are checked both ways", () => {
  for (const decimals of [-1, 1.5, 256, Number.NaN]) {
    assert.throws(() => parseAmount("1", decimals), /decimals/);
    assert.throws(() => formatAmount("1", decimals), /decimals/);
  }
  assert.equal(formatAmount("1", 255).length, 257);
});

test("only the guard's own spelling of a base amount is formatted", () => {
  for (const base of ["", "007", "+7", "7.0", "1e3"]) {
    assert.throws(() => formatAmount(base, 6), /decimal string/, base);
  }
});

test("printable text hides nothing and parses back", () => {
  const nasty = 'pay\x1b[2K\r"\\\u202egnp.exe\u200b\ud800\x7f\x9b';
  const out = printable(nasty);
  assert.match(out, /^[\x20-\x7e]*$/);
  assert.equal(JSON.parse(out), nasty);
  assert.equal(printable("plain text"), '"plain text"');
});

test("plain text is the same without its quotes", () => {
  assert.equal(plain("a\x1b[2K"), "a\\u001b[2K");
  assert.equal(plain("TST"), "TST");
});

test("plain ASCII is told apart", () => {
  assert.equal(isPlainAscii("order 42: ~ok"), true);
  assert.equal(isPlainAscii(""), true);
  for (const value of ["é", "\n", "\x7f", "\u202e", "a\u200bb"]) {
    assert.equal(isPlainAscii(value), false, value);
  }
});
