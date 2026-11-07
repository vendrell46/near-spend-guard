// Amounts as an invoice writes them and as the guard stores them. Strings and
// BigInt only: a float can't hold 128 bits, and rounding here would change
// what gets paid.

import { printable } from "./display.ts";

export const NEAR_DECIMALS = 24;

const U128_MAX = (1n << 128n) - 1n;

function checkDecimals(decimals: number): void {
  if (!Number.isInteger(decimals) || decimals < 0 || decimals > 255) {
    throw new Error(`a token's decimals are 0 to 255, got ${decimals}`);
  }
}

/** Whole tokens to the token's smallest unit: "5.25" with 6 decimals is
 * "5250000". Refuses what it would have to round, since 0.0000001 of a token
 * with 6 decimals is a mistake, not zero. */
export function parseAmount(value: string, decimals: number): string {
  checkDecimals(decimals);
  const match = /^(0|[1-9]\d*)(?:\.(\d+))?$/.exec(value);
  if (!match) {
    throw new Error(`amount must be a decimal number such as "5.25", got ${printable(value)}`);
  }
  const fraction = (match[2] ?? "").replace(/0+$/, "");
  if (fraction.length > decimals) {
    throw new Error(`amount has more decimals than the token's ${decimals}: ${value}`);
  }
  const base = BigInt(match[1] + fraction.padEnd(decimals, "0"));
  if (base === 0n) {
    throw new Error("amount must be above 0");
  }
  if (base > U128_MAX) {
    throw new Error(`amount does not fit in 16 bytes: ${value}`);
  }
  return base.toString();
}

/** The smallest unit to whole tokens: "5250000" with 6 decimals is "5.25". */
export function formatAmount(base: string, decimals: number): string {
  checkDecimals(decimals);
  if (!/^(?:0|[1-9]\d*)$/.test(base)) {
    throw new Error(`amount must be a decimal string, got ${printable(base)}`);
  }
  const digits = base.padStart(decimals + 1, "0");
  const point = digits.length - decimals;
  const fraction = digits.slice(point).replace(/0+$/, "");
  return fraction === "" ? digits.slice(0, point) : `${digits.slice(0, point)}.${fraction}`;
}
