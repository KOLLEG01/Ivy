// Separate test-only entry. Production main.mjs has no fake-SDK selector.
process.argv[2] = '--transport';
await import('./agent-claude-bridge-fake.mjs');
