#!/usr/bin/env node
// A vanilla-Node MCP reference server, used by
// the MCP stdio interop test in
// tests/mcp_stdio_interop.rs.
//
// Why a hand-rolled server instead of @modelcontextprotocol/sdk
// ---------------------------------------------------------
// The official TS SDK is the canonical
// reference implementation, but the SDK
// requires a TypeScript build step and a
// peer-dep tree that is too heavy for a
// CI-friendly integration test. The interop
// question is "do protocol-droid's typed
// `McpStdioPeer` and a hand-rolled vanilla-JS
// MCP server agree on the wire?" -- the
// answer is the same whether the second
// server is the SDK's sample or a
// ~100-line Node script, because the wire
// format is published.
//
// What this implements
// ---------------------------------------------------------
//   - newline-delimited JSON-RPC 2.0 on stdin/stdout
//   - initialize (returns a hand-built
//     InitializeResult with `tools` capability
//     on and one tool `echo`)
//   - tools/list (returns the one
//     `echo` tool)
//   - tools/call (returns the
//     arguments echoed back as text
//     content)
//   - ping (returns `{}`)
//
// What this does NOT implement
// ---------------------------------------------------------
//   - notifications/* (a future
//     increment adds the notification
//     stream consumer; this fixture
//     is enough to drive the typed
//     `initialize` + `tools/list` +
//     `tools/call` round-trip the
//     typed code already supports)
//   - resources/*, prompts/*
//     (the typed client peer does
//     not yet model these methods;
//     a future increment adds them)
//
// How to run
// ---------------------------------------------------------
//   node tests/fixtures/mcp_reference_server.mjs
//
// Stdin/stdout are the JSON-RPC channel.
// The server reads one envelope per
// line, routes by `method`, and writes
// one reply per line. Stderr is the
// server's log channel.

// Read one JSON-RPC envelope per line
// from stdin. Route by `method`.
// Notifications are dropped silently
// (the fixture does not implement
// any).
let buffer = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (chunk) => {
  buffer += chunk;
  let newlineIndex;
  while ((newlineIndex = buffer.indexOf('\n')) !== -1) {
    const line = buffer.slice(0, newlineIndex);
    buffer = buffer.slice(newlineIndex + 1);
    if (line.trim().length > 0) {
      handleLine(line);
    }
  }
});

function handleLine(line) {
  let request;
  try {
    request = JSON.parse(line);
  } catch (_error) {
    return;
  }
  const id = request.id;
  switch (request.method) {
    case 'initialize':
      process.stdout.write(
        JSON.stringify(makeInitializeResponse(id)) + '\n',
      );
      return;
    case 'ping':
      process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id, result: {} }) + '\n');
      return;
    case 'tools/list':
      process.stdout.write(JSON.stringify(makeToolsListResponse(id)) + '\n');
      return;
    case 'tools/call':
      process.stdout.write(
        JSON.stringify(makeCallToolResponse(id, request.params)) + '\n',
      );
      return;
    default:
      process.stdout.write(
        JSON.stringify({
          jsonrpc: '2.0',
          id,
          error: { code: -32601, message: 'Method not found' },
        }) + '\n',
      );
      return;
  }
}

// The MCP `initialize` reply. Returns
// the pinned revision
// `2025-06-18`, the server's name
// and version, and a capabilities
// block that advertises `tools`.
function makeInitializeResponse(id) {
  return {
    jsonrpc: '2.0',
    id,
    result: {
      // The pinned revision this
      // fixture speaks. The
      // client's
      // `InitializeRequest`
      // includes the same value.
      protocolVersion: '2025-06-18',
      capabilities: {
        tools: {
          listChanged: false,
        },
      },
      serverInfo: {
        name: 'mcp-reference-server',
        title: 'MCP Reference Server',
        version: '0.0.1',
      },
    },
  };
}

// The `tools/list` reply. Returns
// one tool, `echo`, that echoes
// the call's arguments back as
// text content.
function makeToolsListResponse(id) {
  return {
    jsonrpc: '2.0',
    id,
    result: {
      tools: [
        {
          name: 'echo',
          description: 'Echoes the call arguments back as text content.',
          inputSchema: {
            type: 'object',
            properties: {
              message: {
                type: 'string',
                description: 'The message to echo back.',
              },
            },
            required: ['message'],
          },
        },
      ],
    },
  };
}

// The `tools/call` reply. For
// `echo`, the arguments are
// echoed back as text content.
function makeCallToolResponse(id, params) {
  if (!params || params.name !== 'echo') {
    process.stdout.write(
      JSON.stringify({
        jsonrpc: '2.0',
        id,
        result: {
          content: [],
          isError: true,
        },
      }) + '\n',
    );
    return;
  }
  const message =
    params.arguments && typeof params.arguments.message === 'string'
      ? params.arguments.message
      : '';
  process.stdout.write(
    JSON.stringify({
      jsonrpc: '2.0',
      id,
      result: {
        content: [
          {
            type: 'text',
            text: `echo: ${message}`,
          },
        ],
        isError: false,
      },
    }) + '\n',
  );
}
