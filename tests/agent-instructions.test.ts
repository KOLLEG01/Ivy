import assert from 'node:assert/strict';
import { test } from 'node:test';
import { hiveMcpInstructions, hiveMcpAgentSummary } from '../instructions/hive-mcp.js';

test('always-loaded Ivy summary shares the wiki and usage paragraphs with full instructions', () => {
  assert.deepEqual(hiveMcpAgentSummary.split('\n\n').slice(1), hiveMcpInstructions.split('\n\n').slice(1, 4));
  assert.doesNotMatch(hiveMcpAgentSummary, /Tool workflows:|Use a service's specialized tools/);
});