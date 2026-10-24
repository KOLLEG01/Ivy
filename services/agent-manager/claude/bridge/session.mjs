// Private vendor-SDK transport. Native protocol, policy and persistence live in Rust.
const version = 1;
const denied = { behavior: 'deny', message: 'turn already finished' };
const iterable = async function* (messages) { yield* messages; };
const failure = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error) });

export function session(sdk, send) {
  const queries = new Map(), callbacks = new Map(), discoveries = new Set();
  let nextCallback = 0, stopped = false;
  const emit = value => send({ version, ...value });
  function cancelCallbacks(turnId) {
    for (const [id, callback] of callbacks) if (callback.turnId === turnId) {
      callbacks.delete(id); callback.finish(denied);
      void emit({ type: 'callbackCancelled', turnId, callbackId: id });
    }
  }
  function close(entry, error) {
    if (entry.closed) return;
    entry.closed = true;
    queries.delete(entry.turnId);
    cancelCallbacks(entry.turnId);
    entry.controller.abort();
    entry.query.close();
    void emit({ type: 'end', turnId: entry.turnId, ...(error ? { error: failure(error) } : {}) });
  }
  function permission(entry, toolName, input, options) {
    if (entry.closed || options.signal?.aborted) return Promise.resolve(denied);
    return new Promise((resolve, reject) => {
      const callbackId = ++nextCallback;
      const abort = () => {
        if (!callbacks.delete(callbackId)) return;
        finish(denied);
        void emit({ type: 'callbackCancelled', turnId: entry.turnId, callbackId });
      };
      const finish = (value, error) => {
        options.signal?.removeEventListener('abort', abort);
        if (error) reject(Object.assign(new Error(error.message), { name: error.name }));
        else resolve(value);
      };
      callbacks.set(callbackId, { turnId: entry.turnId, finish });
      options.signal?.addEventListener('abort', abort, { once: true });
      void emit({ type: 'permission', turnId: entry.turnId, callbackId,
        toolName, input, toolUseId: options.toolUseID });
    });
  }
  async function discover(method, params) {
    const controller = new AbortController();
    const query = sdk.query({ prompt: iterable([]), options: { ...params.options, abortController: controller } });
    const entry = { controller, query };
    discoveries.add(entry);
    let timer;
    try {
      const cancelled = new Promise((_, reject) => {
        controller.signal.addEventListener('abort', () => reject(new Error('SDK discovery cancelled')), { once: true });
        timer = setTimeout(() => controller.abort(), params.timeoutMs);
      });
      return await Promise.race([cancelled, method === 'models' ? query.supportedModels()
        : query.usage_EXPERIMENTAL_MAY_CHANGE_DO_NOT_RELY_ON_THIS_API_YET({ skipBehaviors: true })]);
    } finally {
      clearTimeout(timer); discoveries.delete(entry); controller.abort(); query.close();
    }
  }
  async function dispatch(method, params) {
    if (stopped) throw new Error('SDK bridge stopped');
    if (method === 'query/start') {
      if (queries.has(params.turnId)) throw new Error('SDK query already exists');
      const controller = new AbortController();
      const entry = { turnId: params.turnId, controller, closed: false };
      const options = { ...params.options, abortController: controller };
      if (params.permissionCallback) options.canUseTool = (name, input, options) => permission(entry, name, input, options);
      entry.query = sdk.query({ prompt: iterable(params.messages), options });
      queries.set(entry.turnId, entry);
      void (async () => {
        try {
          for await (const message of entry.query) {
            if (entry.closed) break;
            await emit({ type: 'message', turnId: entry.turnId, message });
          }
          close(entry);
        } catch (error) { close(entry, error); }
      })();
      return null;
    }
    if (method === 'permission/reply') {
      const callback = callbacks.get(params.callbackId);
      if (callback) {
        callbacks.delete(params.callbackId); callback.finish(params.result, params.error);
      }
      return null;
    }
    if (method === 'models' || method === 'usage') return discover(method, params);
    const entry = queries.get(params.turnId);
    if (method === 'query/close') { if (entry) close(entry); return null; }
    if (!entry) throw new Error('SDK query does not exist');
    if (method === 'query/steer') { await entry.query.streamInput(iterable(params.messages)); return null; }
    if (method === 'query/interrupt') {
      cancelCallbacks(entry.turnId);
      await entry.query.interrupt();
      return null;
    }
    throw new Error('Unknown SDK bridge method');
  }
  return {
    async receive(frame) {
      try {
        if (frame.version !== version) throw new Error('SDK bridge version mismatch');
        const result = await dispatch(frame.method, frame.params ?? {});
        await emit({ id: frame.id, result });
      } catch (error) { await emit({ id: frame.id, error: failure(error) }); }
    },
    async stop() {
      if (stopped) return;
      stopped = true;
      for (const entry of [...queries.values()]) close(entry);
      for (const entry of discoveries) entry.controller.abort();
    },
  };
}
