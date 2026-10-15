#!/usr/bin/env node
// A vanilla-Node ACP v1.25.0 reference agent, used by
// the ACP stdio interop test in tests/acp_stdio_interop.rs.
//
// Why a hand-rolled agent instead of @agentclientprotocol/sdk
// ---------------------------------------------------------
// The official JS SDK is the canonical reference
// implementation, but the SDK's sample agent is
// distributed as part of a multi-MB npm monorepo
// with a TypeScript build step, peer deps, and a
// separate `npm --prefix src/samples install`.
// That is the right shape for a production
// deployment; it is too heavy for a CI-friendly
// integration test, which is what this fixture
// exists for.
//
// The interop question is "do protocol-droid's
// typed AcpStdioPeer and a hand-rolled vanilla-JS
// agent agree on the wire?" -- the answer is the
// same whether the second agent is the SDK's
// sample or a 100-line Node script, because the
// wire format is published.
//
// What this implements
// ---------------------------------------------------------
//   - newline-delimited JSON-RPC 2.0 on stdin/stdout
//   - initialize (returns a hand-built
//     InitializeResponse with all capability flags
//     off and one auth method)
//
// What this does NOT implement
// ---------------------------------------------------------
//   - session/new and the rest of the session
//     lifecycle (a future T29 increment adds the
//     session types; this fixture only needs to
//     exercise the `initialize` round-trip the
//     typed code already supports)
//   - terminal/*, fs/*, prompt/*, mcp/* (the
//     capability flags stay off on the response,
//     so the consumer is testing the all-off
//     wire form)
//
// How to run
// ---------------------------------------------------------
//   node acp_reference_agent.mjs
//
// The agent reads newline-delimited JSON-RPC
// envelopes from stdin and writes newline-
// delimited JSON-RPC replies to stdout. There
// is no TCP listener, no port to discover, no
// startup line to wait for: the test sends a
// request and reads a reply.

const AGENT_INFO = {
  name: 'acp-reference-agent',
  title: 'A vanilla-Node ACP v1.25.0 agent for interop tests.',
  version: '1.25.0',
};

// The single auth method the reference agent
// advertises. The shape is the discriminated-
// union the ACP schema names: every entry has
// a `type` field whose value is the variant
// tag (`"terminal"` or `"agent"`), and the
// rest of the payload matches the variant's
// `$ref` (`AuthMethodTerminal` for the
// terminal variant: `id`, `name`, optional
// `description`, optional `args`, optional
// `env`). The consumer matches on `type` to
// decode the variant.
const AUTH_METHODS = [
  {
    type: 'terminal',
    id: 'dev-terminal',
    name: 'In-process dev terminal',
    description: 'A no-op terminal auth method the reference agent advertises.',
  },
];

// A static handshake response: the agent
// speaks protocolVersion 1, advertises all-off
// capabilities, and the one auth method above.
// The `meta` field is reserved for protocol-
// neutral extension data and is empty here.
function makeInitializeResponse(id) {
  return {
    jsonrpc: '2.0',
    id,
    result: {
      protocolVersion: 1,
      agentCapabilities: {},
      authMethods: AUTH_METHODS,
      agentInfo: AGENT_INFO,
    },
  };
}

// A JSON-RPC error envelope for the cases the
// reference agent handles. The peer turns the
// `code` + `message` into a typed
// `AcpPeerError::JsonRpc` variant.
function makeJsonRpcError(id, code, message) {
  return {
    jsonrpc: '2.0',
    id: id ?? null,
    error: { code, message },
  };
}

let stdinBuffer = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (chunk) => {
  stdinBuffer += chunk;
  let newlineIndex;
  while ((newlineIndex = stdinBuffer.indexOf('\n')) !== -1) {
    const line = stdinBuffer.slice(0, newlineIndex);
    stdinBuffer = stdinBuffer.slice(newlineIndex + 1);
    if (line.trim().length === 0) {
      continue;
    }
    handleLine(line);
  }
});

// A single line from the consumer: parse,
// route by `method`, write a reply. Malformed
// JSON is a JSON-RPC -32700 (Parse error) with
// `id: null` (we cannot correlate without a
// parsed id).
function handleLine(line) {
  let request;
  try {
    request = JSON.parse(line);
  } catch (_error) {
    process.stdout.write(
      JSON.stringify(makeJsonRpcError(null, -32700, 'Parse error')) + '\n',
    );
    return;
  }
  const id = request.id;
  if (request.jsonrpc !== '2.0') {
    process.stdout.write(
      JSON.stringify(makeJsonRpcError(id, -32600, 'Invalid Request')) + '\n',
    );
    return;
  }
  switch (request.method) {
    case 'initialize':
      process.stdout.write(JSON.stringify(makeInitializeResponse(id)) + '\n');
      return;
    default:
      process.stdout.write(
        JSON.stringify(makeJsonRpcError(id, -32601, 'Method not found')) + '\n',
      );
      return;
  }
}
