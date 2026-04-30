import { describe, expect, test } from "bun:test";
import { RPC_ERROR } from "../src/types/protocol";

describe("JSON-RPC error codes", () => {
  test("match the JSON-RPC 2.0 spec", () => {
    expect(RPC_ERROR.PARSE).toBe(-32700);
    expect(RPC_ERROR.METHOD_NOT_FOUND).toBe(-32601);
    expect(RPC_ERROR.INTERNAL).toBe(-32603);
  });
});
