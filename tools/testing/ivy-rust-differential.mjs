import assert from 'node:assert/strict';

// Only observational fields may vary. No wildcard or recursive key removal: a same-named
// field in an operation receipt remains exact unless its full pointer was reviewed.
const observational = new Set(['observedAt', 'updatedAt', 'createdAt', 'pid', 'connectionGeneration']);
export function projectObservation(value, volatilePointers = []) {
  const copy = structuredClone(value);
  for (const pointer of volatilePointers) {
    assert.match(pointer, /^\/(?:[^~]|~[01])*$/);
    const parts = pointer.slice(1).split('/').map(x => x.replaceAll('~1','/').replaceAll('~0','~'));
    const leaf = parts.pop();
    assert.ok(observational.has(leaf), `Identity/outcome field cannot be normalized: ${pointer}`);
    let parent = copy;
    for (const part of parts) { assert.ok(parent && Object.hasOwn(parent,part), `Missing ${pointer}`); parent = parent[part]; }
    assert.ok(parent && Object.hasOwn(parent,leaf), `Missing ${pointer}`);
    parent[leaf] = '<volatile observation>';
  }
  return copy;
}

export function compareObservation(reference, candidate, volatilePointers = []) {
  assert.deepStrictEqual(projectObservation(candidate, volatilePointers), projectObservation(reference, volatilePointers));
}

export function qualifyPreparedCandidate(candidate, runtime) {
  // A universal archive does not make the candidate record platform-independent.
  for (const key of ['os', 'arch', 'node']) assert.equal(candidate.platform[key], runtime[key], `Candidate ${key} mismatch`);
  assert.equal(candidate.candidateId, candidate.buildId);
}

export async function runDifferential({ steps, reference, candidate, record }) {
  assert.ok(steps.length > 0);
  const names = new Set();
  const results = [];
  for (const step of steps) {
    assert.match(step.id, /^[a-z0-9][a-z0-9-]{0,90}$/);
    assert.ok(!names.has(step.id), 'Duplicate step identity'); names.add(step.id);
    assert.ok(step.method && step.params && step.expected, 'Each step needs a request and independent expected result');
    // Reject unsupported normalization before any dispatch.
    projectObservation(step.expected, step.volatilePointers);
    const observations = {};
    for (const [lane, client] of [['reference',reference],['candidate',candidate]]) {
      await record(`${step.id}-${lane}-request`, { method: step.method, params: step.params });
      let observed;
      try {
        observed = { result: await client.request(step.method, structuredClone(step.params)) };
      } catch (error) {
        // Transport failures never count as equivalent successes or authorize a fresh replay.
        if (!error.code || !['not_executed','completed'].includes(error.outcome)) {
          await record(`${step.id}-${lane}-unknown`, { outcome: 'unknown', code: error.code ?? 'transport_error' });
          throw new Error(`Unresolved ${step.id} in ${lane}; reconcile its original request`, { cause: error });
        }
        observed = { error: { code: error.code, outcome: error.outcome, message: error.message,
          details: error.details ?? null, httpStatus: error.httpStatus ?? null, rpcCode: error.toWire?.().code ?? null } };
      }
      await record(`${step.id}-${lane}-result`, observed);
      compareObservation(step.expected, observed, step.volatilePointers);
      observations[lane] = observed;
    }
    compareObservation(observations.reference, observations.candidate, step.volatilePointers);
    results.push({ id: step.id, passed: true });
  }
  return results;
}
