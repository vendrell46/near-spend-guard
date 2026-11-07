// The agent's key, and the one file that loads near-api-js. It signs one kind
// of transaction: `request_payment` on the guard, with no deposit.

import { createHash } from "node:crypto";

import {
  Account,
  JsonRpcProvider,
  KeyPairSigner,
  type KeyPairString,
  actions,
  baseEncode,
  teraToGas,
} from "near-api-js";
import { InvalidNonceError } from "near-api-js/rpc-errors";

import { type RequestArgs, type Sender, type Submission, submissionOf, withDeadline } from "./pay.ts";

export const INCLUSION_TIMEOUT_MS = 30_000;

export interface NearSenderOptions {
  guardId: string;
  agentId: string;
  rpcUrl: string;
  secretKey: string;
  timeoutMs?: number;
}

export function nearSender(options: NearSenderOptions): {
  sender: Sender;
  publicKey: () => Promise<string>;
} {
  const { guardId, agentId, rpcUrl, secretKey, timeoutMs = INCLUSION_TIMEOUT_MS } = options;
  const provider = new JsonRpcProvider({ url: rpcUrl });
  const signer = KeyPairSigner.fromSecretKey(secretKey as KeyPairString);
  const fresh = (): Account => new Account(agentId, provider, signer);
  let account = fresh();

  async function send(args: RequestArgs, tgas: number): Promise<Submission> {
    let signed;
    try {
      signed = await withDeadline(
        account.createSignedTransaction({
          receiverId: guardId,
          actions: [actions.functionCall("request_payment", { ...args }, teraToGas(tgas), 0n)],
        }),
        timeoutMs,
      );
    } catch (e) {
      return { kind: "rejected", error: `not sent: ${(e as Error).message}`, tx_hash: null };
    }
    if (signed === "timeout") {
      return { kind: "rejected", error: `not sent: no answer within ${timeoutMs / 1000} s`, tx_hash: null };
    }
    const txHash = baseEncode(createHash("sha256").update(signed.transaction.encode()).digest());
    try {
      const tx = await withDeadline(
        provider.sendTransactionUntil(signed, "EXECUTED_OPTIMISTIC"),
        timeoutMs,
      );
      return tx === "timeout"
        ? { kind: "unknown", why: `no answer within ${timeoutMs / 1000} s`, tx_hash: txHash }
        : submissionOf(tx as Parameters<typeof submissionOf>[0], txHash);
    } catch (e) {
      // The key's nonce moved on without this process. It's read again for
      // the next payment, but this one stays unknown: an RPC retrying a
      // transaction that already ran gets the same error.
      if (e instanceof InvalidNonceError) {
        account = fresh();
      }
      // Sent, or perhaps sent: an RPC error can't prove the opposite.
      return { kind: "unknown", why: (e as Error).message, tx_hash: txHash };
    }
  }

  // One transaction at a time. Nonces are handed out in order, but parallel
  // broadcasts don't arrive in order, and once a higher nonce lands every lower
  // one is dead: a payment lost to the client's own timing (seen in the sandbox
  // with six at once). `send` never rejects, so the queue can't stall.
  let queue: Promise<unknown> = Promise.resolve();
  function requestPayment(args: RequestArgs, tgas: number): Promise<Submission> {
    const turn = queue.then(() => send(args, tgas));
    queue = turn;
    return turn;
  }

  return {
    sender: { requestPayment },
    publicKey: async () => (await signer.getPublicKey()).toString(),
  };
}
