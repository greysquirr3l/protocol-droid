#!/usr/bin/env node
// Minimal TS SDK stdio client used by `ts_client_to_pd_server_matrix`.
//
// Spawns the protocol-droid `stdio_server` example as a child
// process, drives a `tools/list` then a `tools/call` for the
// `shout` tool the example advertises, and prints a single JSON
// summary to stdout. The Rust test reads the summary and
// asserts on its fields; this keeps the cross-test's view of
// the TS client the same shape as the rmcp client's: a
// deterministic JSON payload the parent parses, not a stream of
// `console.log` lines that has to be split.
//
// CLI:
//   node client.mjs <protocol-droid-stdio_server-binary>
//
// The first positional argument is the absolute path to the
// `protocol-droid stdio_server` example the matrix spawned. The
// TS client passes that path straight to `StdioClientTransport`
// as the `command`, with no extra args, so the server is invoked
// exactly the way a user would invoke it from a terminal.

import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

const serverPath = process.argv[2];
if (!serverPath) {
  console.error(
    "client.mjs: missing <protocol-droid-stdio_server-binary> argument",
  );
  process.exit(2);
}

const client = new Client(
  {
    name: "ts-sdk-interop-client",
    version: "0.0.0",
  },
  {
    capabilities: {},
  },
);

const transport = new StdioClientTransport({
  command: serverPath,
  args: [],
});

let tools;
let callResult;
let serverIdentity;

try {
  await client.connect(transport);

  // The TS SDK does not expose the negotiated protocol version
  // through a public getter; it is internal to the client. The
  // TS client always offers its `LATEST_PROTOCOL_VERSION`
  // (currently `2025-11-25` at the pinned 1.32.1), and the
  // server's response is the negotiated version. For a matrix
  // test in this direction, the "version" is the TS SDK's
  // offered version — a single value, not a per-row parameter.
  // The cross-test asserts on the server's *identity* (name +
  // version), which is what `getServerVersion()` returns, plus
  // the tools list and the call result.
  serverIdentity = client.getServerVersion();

  tools = await client.listTools();
  callResult = await client.callTool({
    name: "shout",
    arguments: { text: "hello from ts-sdk" },
  });
} finally {
  // `close()` shuts the child down on the polite path; the
  // cross-test's `StdioClient` wrapper kills the child if the
  // parent test is already unwinding, so the two error paths
  // never disagree about who owns the lifecycle.
  try {
    await client.close();
  } catch {
    // best-effort; the child's exit is not the cross-test's claim
  }
}

process.stdout.write(
  JSON.stringify({
    server_identity: serverIdentity,
    tools: tools.tools,
    call_result: callResult,
  }) + "\n",
);
