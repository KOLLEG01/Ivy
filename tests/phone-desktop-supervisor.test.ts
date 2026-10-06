import test from 'node:test';
import assert from 'node:assert/strict';
import { ensureDesktopRunning } from '../services/phone-bridge/src/main.js';
import type { PhoneDesktopLaunch, PhoneDesktopObservation } from '../services/phone-bridge/src/runtime/native.js';

const application = { appUserModelId: 'Fixture.Package!UI', startIfMissing: true };
const observation = (state: PhoneDesktopObservation['state']): PhoneDesktopObservation => ({ state, identity: null });
const launch = (phase: PhoneDesktopLaunch['phase']): PhoneDesktopLaunch => ({ phase, identity: null, activationPid: phase === 'submitted' ? 123 : null, errorCode: null });

test('PhoneBridge starts an absent Desktop once and leaves ready or starting instances alone', async () => {
  for (const state of ['ready', 'waiting'] as const) {
    let launches = 0;
    const native = { observeDesktop: async () => observation(state), launchDesktop: async () => { launches++; return launch('submitted'); } };
    assert.equal(await ensureDesktopRunning(native, application), state); assert.equal(launches, 0);
  }
  let launches = 0, operationId = '';
  const native = { observeDesktop: async () => observation('absent'), launchDesktop: async (_application: typeof application, id: string) => { launches++; operationId = id; return launch('submitted'); } };
  assert.equal(await ensureDesktopRunning(native, application), 'submitted'); assert.equal(launches, 1); assert.match(operationId, /^[0-9a-f-]{36}$/i);
});

test('PhoneBridge refuses ambiguous Desktop ownership and does not launch when disabled', async () => {
  let launches = 0;
  const ambiguous = { observeDesktop: async () => observation('ambiguous'), launchDesktop: async () => { launches++; return launch('submitted'); } };
  await assert.rejects(ensureDesktopRunning(ambiguous, application), { code: 'phone_desktop_unavailable' }); assert.equal(launches, 0);
  const absent = { observeDesktop: async () => observation('absent'), launchDesktop: async () => { launches++; return launch('submitted'); } };
  assert.equal(await ensureDesktopRunning(absent, { ...application, startIfMissing: false }), 'absent'); assert.equal(launches, 0);
});
