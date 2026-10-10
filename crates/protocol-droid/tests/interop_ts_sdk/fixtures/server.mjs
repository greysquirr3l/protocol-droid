#!/usr/bin/env node
// Minimal TS SDK stdio server used by `pd_client_to_ts_server_matrix`.
//
// Cross-test contract:
//   * Name: `ts-sdk-stdio-server`
//   * Version: `0.0.0`
//   * Capabilities: `{ tools: {} }` (no `prompts`, `resources`, etc.)
//   * Tools:
//       `add(a, b) -> "a + b"` (the sum as a string in the `text`
//        content slot, the cheapest round-trippable shape that does
//        not depend on the TS SDK's `BaseError` codec or on a
//        schema-rev the matrix does not test)
//
// The cross-test's first action is `initialize`, where the TS SDK
// negotiates the protocol version with the caller. After the
// handshake the server handles `tools/list` and `tools/call` for
// `add` and nothing else — adding more tools would not exercise any
// new wire surface (every tool goes through the same
// `CallToolRequestSchema`), and removing it would make the
// cross-test degenerate.

import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import {
  CallToolRequestSchema,
  ListToolsRequestSchema,
} from "@modelcontextprotocol/sdk/types.js";

const server = new Server(
  {
    name: "ts-sdk-stdio-server",
    version: "0.0.0",
  },
  {
    capabilities: {
      tools: {},
    },
  },
);

server.setRequestHandler(ListToolsRequestSchema, async () => {
  return {
    tools: [
      {
        name: "add",
        description: "Adds two numbers and returns the sum as text.",
        inputSchema: {
          type: "object",
          properties: {
            a: { type: "integer", description: "first addend" },
            b: { type: "integer", description: "second addend" },
          },
          required: ["a", "b"],
        },
      },
    ],
  };
});

server.setRequestHandler(CallToolRequestSchema, async (request) => {
  const { name, arguments: args } = request.params;
  if (name !== "add") {
    // The TS SDK's `McpError` is the wire shape for a tool not
    // being on the server, not a free-form string. The
    // cross-test's negative case is covered in
    // `pd_client_to_ts_server_matrix::unknown_tool` separately,
    // and a `throw new McpError(...)` here would surface the same
    // path the cross-test asserts.
    throw new Error(`Unknown tool: ${name}`);
  }
  const a = args?.a;
  const b = args?.b;
  if (typeof a !== "number" || typeof b !== "number") {
    throw new Error(`add requires numeric 'a' and 'b', got ${JSON.stringify(args)}`);
  }
  return {
    content: [
      {
        type: "text",
        text: String(a + b),
      },
    ],
  };
});

const transport = new StdioServerTransport();
await server.connect(transport);
