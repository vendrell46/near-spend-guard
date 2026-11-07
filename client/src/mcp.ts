// A stdio MCP server with one tool, `pay`: newline-delimited JSON-RPC, by hand.
// One tool needs four methods, and the SDK's dependencies would sit in the
// process that holds the agent's key.

import { createInterface } from "node:readline";
import type { Readable, Writable } from "node:stream";

import { plain } from "./display.ts";
import { DEFAULT_EXPIRES_IN_S, MAX_EXPIRES_IN_S, type PayReport } from "./pay.ts";

/** Newest first. A client asking for another gets the newest, as the spec
 * says, and decides whether to go on. */
export const PROTOCOL_VERSIONS = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

export const PAY_TOOL = {
  name: "pay",
  title: "Pay through near-spend-guard",
  description:
    "Ask the spending guard to make one payment. The guard holds the funds and decides: " +
    "the answer is paid, refused, or pending (held for a human, who approves the exact " +
    "payment). This tool can do nothing else, and it cannot approve anything. " +
    "After `unknown` or `uncertain`, do not pay again before checking: the payment may " +
    "have gone through.",
  inputSchema: {
    type: "object",
    properties: {
      token: {
        description: 'What to pay in: "native" for NEAR, or {"ft": "<token contract account>"}.',
        oneOf: [
          { const: "native" },
          {
            type: "object",
            properties: { ft: { type: "string" } },
            required: ["ft"],
            additionalProperties: false,
          },
        ],
      },
      receiver_id: { type: "string", description: "The NEAR account to pay." },
      amount: {
        type: "string",
        description: 'In whole tokens, as the invoice writes it, such as "5.25".',
      },
      msg: {
        type: "string",
        description: "Pays through ft_transfer_call with this message. FT only.",
      },
      expires_in_s: {
        type: "integer",
        minimum: 1,
        maximum: MAX_EXPIRES_IN_S,
        default: DEFAULT_EXPIRES_IN_S,
        description: "How long a human has to approve, if it comes to that.",
      },
    },
    required: ["token", "receiver_id", "amount"],
    additionalProperties: false,
  },
};

type RunPay = (args: unknown) => Promise<PayReport>;
type Response = { result: unknown } | { error: { code: number; message: string } };

const OK_STATUSES = ["paid", "pending", "refused"];

async function callTool(params: { name?: unknown; arguments?: unknown }, run: RunPay): Promise<Response> {
  if (params.name !== PAY_TOOL.name) {
    return { error: { code: -32602, message: `unknown tool: ${String(params.name)}` } };
  }
  try {
    const report = await run(params.arguments ?? {});
    const isError = !OK_STATUSES.includes(report.status);
    return { result: { content: [{ type: "text", text: report.text }], isError } };
  } catch (e) {
    // `pay` throws only before anything is sent.
    const text = `not sent: ${plain((e as Error).message)}`;
    return { result: { content: [{ type: "text", text }], isError: true } };
  }
}

async function answer(method: string, params: unknown, run: RunPay): Promise<Response> {
  switch (method) {
    case "initialize": {
      const asked = (params as { protocolVersion?: unknown } | undefined)?.protocolVersion;
      const protocolVersion = PROTOCOL_VERSIONS.includes(asked as string)
        ? asked
        : PROTOCOL_VERSIONS[0];
      return {
        result: {
          protocolVersion,
          capabilities: { tools: {} },
          serverInfo: { name: "near-spend-guard", version: "0.1.0" },
        },
      };
    }
    case "ping":
      return { result: {} };
    case "tools/list":
      return { result: { tools: [PAY_TOOL] } };
    case "tools/call":
      return callTool((params ?? {}) as { name?: unknown; arguments?: unknown }, run);
    default:
      return { error: { code: -32601, message: `no method ${method}` } };
  }
}

/** Serves until `input` ends. Calls run side by side, each answered when it's
 * done; a client matches them by id. */
export async function serve(input: Readable, output: Writable, run: RunPay): Promise<void> {
  const send = (id: unknown, response: Response): void => {
    output.write(`${JSON.stringify({ jsonrpc: "2.0", id, ...response })}\n`);
  };
  const running = new Set<Promise<void>>();
  for await (const line of createInterface({ input, crlfDelay: Infinity })) {
    if (line.trim() === "") {
      continue;
    }
    let message: { id?: unknown; method?: unknown; params?: unknown } | null;
    try {
      message = JSON.parse(line);
    } catch {
      send(null, { error: { code: -32700, message: "not JSON" } });
      continue;
    }
    if (typeof message?.method !== "string") {
      // Answers to requests we never made, and anything else with no method.
      continue;
    }
    if (message.id === undefined) {
      continue;
    }
    const { id, method, params } = message as { id: unknown; method: string; params: unknown };
    const call = answer(method, params, run).then((response) => {
      send(id, response);
      running.delete(call);
    });
    running.add(call);
  }
  await Promise.all(running);
}
