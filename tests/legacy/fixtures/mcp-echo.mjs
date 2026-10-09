/** Harmless MCP fixture with two tools, so selection tests can check non-selected tool isolation. */
import { createInterface } from 'node:readline';
const input = createInterface({ input: process.stdin, crlfDelay: Infinity });
input.on('line', line => {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  let result;
  switch (request.method) {
    case 'initialize': result = { protocolVersion: request.params.protocolVersion, capabilities: { tools: {} }, serverInfo: { name: 'rainy-echo-fixture', version: '1' } }; break;
    case 'ping': result = {}; break;
    case 'tools/list': result = { tools: ['echo', 'unselected'].map(name => ({ name, description: 'Echo test text without external effects.', inputSchema: { type: 'object', properties: { text: { type: 'string' } }, required: ['text'] } })) }; break;
    case 'tools/call': result = { content: [{ type: 'text', text: String(request.params.arguments.text) }] }; break;
    default: process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, error: { code: -32601, message: 'Unsupported fixture method' } }) + '\n'); return;
  }
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\n');
});
