// Private SDK 0.3.283 transport. Policy, native RPC and persistence live in Rust.
export const protocolVersion = 1;
export function session(sdk, send) {
  const queries = new Map();
  const active = new Set();
  const callbacks = new Map();
  let nextCallback = 0;
  let stopped = false;
  const emit = value => { if (!stopped) send({ version: protocolVersion, ...value }); };
  const finishCallbacks = turnId => {
    for (const [id, pending] of callbacks) {
      if (pending.turnId !== turnId) continue;
      callbacks.delete(id);
      pending.cleanup();
      pending.resolve({ behavior: 'deny', message: 'turn already finished' });
    }
  };
  const close = entry => {
    if (entry.closed) return;
    entry.closed = true;
    active.delete(entry);
    entry.abort.abort();
    finishCallbacks(entry.turnId);
    entry.query.close();
  };
  const iterable = messages => (async function* () { yield* messages; })();
  function options(params, entry) {
    const value = { ...params.options, abortController: entry.abort };
    // These functions cannot cross JSON. Rust explicitly chooses whether to
    // install the SDK callback, and supplies its complete SDK result later.
    if (params.permissionCallback) {
      value.canUseTool = (toolName, input, context) => new Promise((resolve, reject) => {
        if (entry.closed || context.signal.aborted) {
          resolve({ behavior: 'deny', message: 'turn already finished' });
          return;
        }
        const callbackId = ++nextCallback;
        const cancel = () => {
          const pending = callbacks.get(callbackId);
          if (!pending) return;
          callbacks.delete(callbackId);
          pending.cleanup();
          resolve({ behavior: 'deny', message: 'turn already finished' });
          emit({ type: 'callbackCancelled', turnId: entry.turnId, callbackId });
        };
        const cleanup = () => context.signal.removeEventListener('abort', cancel);
        callbacks.set(callbackId, { resolve, reject, cleanup, turnId: entry.turnId });
        context.signal.addEventListener('abort', cancel, { once: true });
        emit({ type: 'permission', turnId: entry.turnId, callbackId,
          toolName, input, toolUseId: context.toolUseID ?? null });
      });
    }
    return value;
  }
  async function dispatch(method, params) {
    if (method === 'query/start') {
      if (queries.has(params.turnId)) throw new Error('SDK query already exists');
      const entry = { turnId: params.turnId, abort: new AbortController(), query: null, closed: false };
      entry.query = sdk.query({ prompt: iterable(params.messages), options: options(params, entry) });
      queries.set(entry.turnId, entry);
      active.add(entry);
      // Register before consuming: permissions may arrive immediately, and
      // controls must remain readable while the iterator is awaiting input.
      entry.completion = (async () => {
        let failure;
        try {
          for await (const message of entry.query) {
            if (entry.closed || stopped) break;
            emit({ type: 'message', turnId: entry.turnId, message });
          }
        } catch (error) {
          failure = { name: error?.name ?? 'Error', message: String(error?.message ?? error) };
        } finally {
          queries.delete(entry.turnId);
          close(entry);
          emit({ type: 'end', turnId: entry.turnId, ...(failure ? { error: failure } : {}) });
        }
      })();
      return null;
    }
    if (method === 'permission/reply') {
      const pending = callbacks.get(params.callbackId);
      if (pending) {
        callbacks.delete(params.callbackId);
        pending.cleanup();
        if (params.error) {
          const error = new Error(String(params.error.message));
          error.name = params.error.name ?? 'Error';
          pending.reject(error);
        } else pending.resolve(params.result);
      }
      return null;
    }
    if (method === 'query/steer') {
      for (const entry of queries.values()) {
        if (entry.turnId !== params.turnId) continue;
        entry.query.streamInput(iterable(params.messages));
        break;
      }
      return null;
    }
    if (method === 'query/interrupt') {
      const entry = queries.get(params.turnId);
      if (entry) {
        entry.abort.abort();
        await entry.query.interrupt();
      }
      return null;
    }
    if (method === 'query/close') {
      const entry = queries.get(params.turnId);
      if (entry) close(entry);
      return null;
    }
    if (method === 'models' || method === 'usage') {
      const entry = { turnId: null, abort: new AbortController(), query: null, closed: false };
      entry.query = sdk.query({ prompt: iterable([]), options: options(params, entry) });
      active.add(entry);
      const timer = setTimeout(() => entry.abort.abort(), params.timeoutMs);
      try {
        return method === 'models' ? await entry.query.supportedModels()
          : await entry.query.usage_EXPERIMENTAL_MAY_CHANGE_DO_NOT_RELY_ON_THIS_API_YET({ skipBehaviors: true });
      } finally {
        clearTimeout(timer);
        close(entry);
      }
    }
    if (method === 'stop') { await stop(); return null; }
    throw new Error('Unknown SDK bridge method: ' + method);
  }
  async function receive(message) {
    if (stopped) return;
    const id = message.id;
    try {
      if (message.version !== protocolVersion) throw new Error('SDK bridge version mismatch');
      const result = await dispatch(message.method, message.params ?? {});
      // A stop response must be written even after notifications are closed.
      send({ version: protocolVersion, id, result });
    } catch (error) {
      send({ version: protocolVersion, id, error: {
        name: error?.name ?? 'Error', message: String(error?.message ?? error),
      } });
    }
  }
  async function stop() {
    if (stopped) return;
    stopped = true;
    for (const entry of active) close(entry);
    queries.clear();
  }
  return { receive, stop };
}
