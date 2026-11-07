// Text that came from the chain or from the agent, on its way to a terminal
// or a model. `msg` can hold anything, and only `msg` can, so it never reaches
// a screen as it is.

/** JSON-quoted, with everything but printable ASCII escaped: control
 * characters, right-to-left overrides and zero-width characters can't move
 * the cursor or hide part of the string. Still parses back to the original. */
export function printable(value: string): string {
  return JSON.stringify(value).replace(
    /[^\x20-\x7e]/g,
    (c) => `\\u${c.charCodeAt(0).toString(16).padStart(4, "0")}`,
  );
}

/** The same without the quotes, for text inside a sentence: a token's symbol,
 * an error the RPC wrote. */
export function plain(value: string): string {
  return printable(value).slice(1, -1);
}

/** Whether `printable` had to hide anything from a human reader. */
export function isPlainAscii(value: string): boolean {
  return /^[\x20-\x7e]*$/.test(value);
}
