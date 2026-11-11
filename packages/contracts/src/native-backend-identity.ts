import type { Agent } from './generated.js';

export function sameNativeBackend(actual: Agent.BackendIdentity | undefined, expected: Agent.BackendIdentity | undefined): boolean {
  return actual === undefined && expected === undefined || !!actual && !!expected &&
    actual.backendId === expected.backendId && actual.provider === expected.provider && actual.accountId === expected.accountId;
}
