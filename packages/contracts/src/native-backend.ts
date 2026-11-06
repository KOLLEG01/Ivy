import { validateAgent } from './agent-validation.js';
import { requireThat } from './errors.js';
import type { Agent, Wire } from './generated.js';

export function sameNativeBackend(actual: Agent.BackendIdentity | undefined, expected: Agent.BackendIdentity | undefined): boolean {
  return actual === undefined && expected === undefined || !!actual && !!expected &&
    actual.backendId === expected.backendId && actual.provider === expected.provider && actual.accountId === expected.accountId;
}

/** Ivy's selector stays outside unchanged provider parameters. */
export function backendArguments(arguments_: Wire.Json, backend?: Agent.BackendIdentity): Record<string, Wire.Json> {
  requireThat(arguments_ !== null && typeof arguments_ === 'object' && !Array.isArray(arguments_), 'invalid_arguments', 'Backend management arguments must be an object.');
  if (backend) validateAgent('BackendIdentity', backend);
  const supplied = arguments_['backend'];
  if (supplied !== undefined) validateAgent('BackendIdentity', supplied);
  requireThat(supplied === undefined || sameNativeBackend(supplied as Agent.BackendIdentity, backend), 'native_backend_mismatch', 'Management arguments select another backend or account.');
  return backend ? { ...arguments_, backend: { ...backend } } : arguments_;
}
