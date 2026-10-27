process.env.NO_BROWSER = '1';
process.env.INITIAL_AGENT_MODE = 'agent';
await import('./node_modules/@agentclientprotocol/codex-acp/dist/index.js');
