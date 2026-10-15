#!/usr/bin/env node
// A vanilla-Node A2A v1.0 reference server, used by
// the a2a-js interop test in tests/a2a_js_interop.rs.
//
// Why a hand-rolled server instead of @a2a-js/sdk
// ---------------------------------------------------------
// The official @a2a-js/sdk ships its sample agent as
// part of a multi-MB npm monorepo with a TypeScript
// build step, peer deps, and a separate
// `npm --prefix src/samples install`. That is fine
// for a production deployment; it is too heavy for a
// CI-friendly integration test, which is what this
// fixture exists for.
//
// A "non-protocol-droid" server is the only
// requirement for the interop test. The protocol-
// droid A2aClient speaks JSON-RPC 2.0; the wire
// format is published; a 200-line vanilla-Node HTTP
// server implements the same surface the same way.
// The interop test answers "do protocol-droid's
// typed A2aClient and a hand-rolled vanilla-JS
// server agree on the wire?", which is the question
// the interop test exists to answer.
//
// What this implements
// ---------------------------------------------------------
//  - GET /.well-known/agent-card.json
//  - POST /a2a  (JSON-RPC 2.0)
//  - Methods: SendMessage, GetTask, ListTasks,
//             CancelTask
//  - Unknown methods return JSON-RPC -32601
//  - All other JSON-RPC errors return -32603
//
// What this does NOT implement
// ---------------------------------------------------------
//  - Push notification config CRUD (would need
//    a webhook client to test against)
//  - Streaming (SendStreamingMessage) (would need
//    SSE wiring; covered in-process by the
//    typed_dispatch tests)
//  - Extended agent card (the JSON-RPC
//    GetExtendedAgentCard; the static card is
//    enough for the interop test)
//  - Bearer token auth (the interop test does
//    not exercise auth; the consumer-path tests
//    cover that)
//
// How to run
// ---------------------------------------------------------
//   PORT=41241 node a2a_reference_server.mjs
//
// The server logs the bound port to stderr on the
// "listening" line. The interop test reads that
// line, captures the port, and starts driving
// requests.

import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';

const PORT = Number.parseInt(process.env.PORT ?? '41241', 10);

// In-memory task store. Tasks live for the
// lifetime of the server, which is the lifetime
// of a single test run; tests that need
// isolation between runs get a fresh process.
const tasks = new Map();

function makeTask(text) {
  const id = `t-${randomUUID()}`;
  const now = new Date().toISOString();
  const task = {
    id,
    status: {
      state: 'completed',
      message: {
        role: 'agent',
        parts: [{ text: `reference: ${text}` }],
      },
      timestamp: now,
    },
    artifacts: [],
    history: [],
    metadata: {},
  };
  tasks.set(id, task);
  return task;
}

const AGENT_CARD = {
  name: 'a2a-reference-server',
  description: 'A vanilla-Node A2A v1.0 server for interop tests.',
  version: '1.0.0',
  // A2A v1.0 §4 requires the card to publish
  // a non-empty list of protocol bindings. The
  // Rust client deserialises this into
  // `Vec<AgentInterface>`, where every entry
  // has a `url` (parsed as a `Url`), a
  // `protocolBinding`, and a
  // `protocolVersion`. The JSON-RPC binding
  // is the one this server implements, so
  // that is the only entry.
  supportedInterfaces: [
    {
      url: `http://127.0.0.1:${PORT}/a2a`,
      protocolBinding: 'jsonrpc',
      protocolVersion: '1.0',
    },
  ],
  capabilities: {
    streaming: false,
    pushNotifications: false,
    stateTransitionHistory: false,
  },
  defaultInputModes: ['text/plain'],
  defaultOutputModes: ['text/plain'],
  skills: [],
};

function jsonRpcError(id, code, message) {
  return {
    jsonrpc: '2.0',
    id,
    error: { code, message },
  };
}

function jsonRpcResult(id, result) {
  return { jsonrpc: '2.0', id, result };
}

function dispatchRpc(envelope) {
  const id = envelope.id ?? null;
  if (envelope.jsonrpc !== '2.0') {
    return jsonRpcError(id, -32600, 'Invalid Request');
  }
  const method = envelope.method;
  const params = envelope.params ?? {};
  if (typeof method !== 'string') {
    return jsonRpcError(id, -32600, 'Invalid Request');
  }

  switch (method) {
    case 'SendMessage': {
      const text = params?.message?.parts?.[0]?.text ?? '';
      if (typeof text !== 'string') {
        return jsonRpcError(id, -32602, 'params.message.parts[0].text must be a string');
      }
      return jsonRpcResult(id, makeTask(text));
    }

    case 'GetTask': {
      const id = params?.id;
      if (typeof id !== 'string') {
        return jsonRpcError(id, -32602, 'params.id is required');
      }
      const task = tasks.get(id);
      if (!task) {
        return jsonRpcError(id, -32001, `task ${id} not found`);
      }
      return jsonRpcResult(id, task);
    }

    case 'ListTasks': {
      const all = Array.from(tasks.values());
      return jsonRpcResult(id, {
        tasks: all,
        nextPageToken: '',
        pageSize: all.length || 1,
        totalSize: all.length,
      });
    }

    case 'CancelTask': {
      const id = params?.id;
      if (typeof id !== 'string') {
        return jsonRpcError(id, -32602, 'params.id is required');
      }
      const task = tasks.get(id);
      if (!task) {
        return jsonRpcError(id, -32001, `task ${id} not found`);
      }
      task.status = { ...task.status, state: 'cancelled' };
      return jsonRpcResult(id, task);
    }

    default:
      return jsonRpcError(id, -32601, `Method not found: ${method}`);
  }
}

const server = createServer((req, res) => {
  // Card discovery. A2A v1.0 §4 says the
  // canonical path is
  // `/.well-known/agent-card.json`.
  if (req.method === 'GET' && req.url === '/.well-known/agent-card.json') {
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify(AGENT_CARD));
    return;
  }

  // Health check for the test harness; not
  // part of the A2A spec.
  if (req.method === 'GET' && req.url === '/health') {
    res.writeHead(200, { 'content-type': 'text/plain' });
    res.end('ok');
    return;
  }

  // JSON-RPC endpoint.
  if (req.method === 'POST' && req.url === '/a2a') {
    let body = '';
    req.on('data', (chunk) => {
      body += chunk.toString('utf8');
    });
    req.on('end', () => {
      let envelope;
      try {
        envelope = JSON.parse(body);
      } catch (error) {
        res.writeHead(400, { 'content-type': 'application/json' });
        res.end(
          JSON.stringify({
            jsonrpc: '2.0',
            id: null,
            error: {
              code: -32700,
              message: `Parse error: ${error.message}`,
            },
          }),
        );
        return;
      }
      const reply = dispatchRpc(envelope);
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify(reply));
    });
    return;
  }

  res.writeHead(404, { 'content-type': 'text/plain' });
  res.end('not found');
});

server.listen(PORT, '127.0.0.1', () => {
  // The interop test reads this line and parses
  // the port. Keep the format stable: a single
  // line, the literal text "listening on", then
  // the port, then a newline.
  process.stderr.write(`listening on ${PORT}\n`);
});
