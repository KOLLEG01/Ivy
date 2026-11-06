// Private vendor-SDK transport. Native protocol, policy and persistence live in Rust.
export const version = 2;
const queryLimit = 16, discoveryLimit = 8, callbackLimit = 32;
const capacity = () => Object.assign(new Error('SDK active work capacity is occupied'), { name: 'native_capacity' });
const denied = { behavior: 'deny', message: 'turn already finished' };
const iterable = async function* (messages) { yield* messages; };
const failure = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error) });

// Credits cover original events through Rust processing. RPC replies bypass
// these windows, so a stalled message consumer cannot hold a control response.
export function eventFlow(write) {
  const data = { active: 0, maximum: 1, waiting: [], limit: queryLimit };
  const control = { active: 0, maximum: 8, waiting: [], limit: 96 };
  const pending = new Map();
  let next = 0, closed = false;
  function take(lane) {
    if (closed) return Promise.resolve(false);
    if (lane.active < lane.maximum) { lane.active++; return Promise.resolve(true); }
    if (lane.waiting.length >= lane.limit) return Promise.reject(capacity());
    return new Promise(resolve => lane.waiting.push(resolve));
  }
  function release(lane) {
    const waiting = closed ? undefined : lane.waiting.shift();
    if (waiting) waiting(true);
    else lane.active--;
  }
  return {
    async send(frame) {
      if (Object.hasOwn(frame, 'id')) { if (!closed) await write(frame); return; }
      const lane = frame.type === 'message' ? data : control;
      if (!await take(lane)) return;
      if (closed) { release(lane); return; }
      if (next === Number.MAX_SAFE_INTEGER) { release(lane); throw capacity(); }
      const sequence = ++next;
      let finish;
      const consumed = new Promise(resolve => { finish = () => { release(lane); resolve(); }; });
      pending.set(sequence, finish);
      try {
        const writing = write({ ...frame, eventSequence: sequence });
        frame = undefined;
        await writing;
        await consumed;
      } catch (error) {
        if (pending.delete(sequence)) finish();
        throw error;
      }
    },
    acknowledge(sequence) {
      if (!Number.isSafeInteger(sequence) || sequence <= 0 || !pending.has(sequence)) {
        throw new Error('Invalid SDK event credit');
      }
      const finish = pending.get(sequence);
      pending.delete(sequence);
      finish();
    },
    stop() {
      closed = true;
      for (const finish of pending.values()) finish();
      pending.clear();
      for (const lane of [data, control]) {
        for (const waiting of lane.waiting.splice(0)) waiting(false);
      }
    },
  };
}

export function session(sdk, send, onTransportError) {
  const queries = new Map(), callbacks = new Map(), discoveries = new Set();
  let nextCallback = 0, stopped = false;
  const emit = async value => {
    try {
      const sending = send({ version, ...value });
      value = undefined;
      await sending;
    }
    catch (error) { onTransportError(error); throw error; }
  };
  function cancelCallbacks(turnId) {
    for (const [id, callback] of callbacks) if (callback.turnId === turnId) {
      callbacks.delete(id); callback.finish(denied);
      void emit({ type: 'callbackCancelled', turnId, callbackId: id }).catch(() => {});
    }
  }
  function close(entry, error) {
    if (entry.closed) return;
    entry.closed = true;
    cancelCallbacks(entry.turnId);
    entry.controller.abort();
    entry.query.close();
    void emit({ type: 'end', turnId: entry.turnId, ...(error ? { error: failure(error) } : {}) })
      .catch(() => {}).finally(() => queries.delete(entry.turnId));
  }
  function permission(entry, toolName, input, options) {
    if (entry.closed || options.signal?.aborted) return Promise.resolve(denied);
    if (callbacks.size >= callbackLimit) return Promise.reject(capacity());
    return new Promise((resolve, reject) => {
      const callbackId = ++nextCallback;
      const abort = () => {
        if (!callbacks.delete(callbackId)) return;
        finish(denied);
        void emit({ type: 'callbackCancelled', turnId: entry.turnId, callbackId }).catch(() => {});
      };
      const finish = (value, error) => {
        options.signal?.removeEventListener('abort', abort);
        if (error) reject(Object.assign(new Error(error.message), { name: error.name }));
        else resolve(value);
      };
      callbacks.set(callbackId, { turnId: entry.turnId, finish });
      options.signal?.addEventListener('abort', abort, { once: true });
      void emit({ type: 'permission', turnId: entry.turnId, callbackId,
        toolName, input, toolUseId: options.toolUseID }).catch(() => {});
    });
  }
  async function discover(method, params) {
    if (discoveries.size >= discoveryLimit) throw capacity();
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
      if (queries.size >= queryLimit) throw capacity();
      const controller = new AbortController();
      const entry = { turnId: params.turnId, controller, closed: false };
      const options = { ...params.options, abortController: controller };
      if (params.permissionCallback) options.canUseTool = (name, input, options) => permission(entry, name, input, options);
      entry.query = sdk.query({ prompt: iterable(params.messages), options });
      queries.set(entry.turnId, entry);
      void (async () => {
        try {
          for await (let message of entry.query) {
            if (entry.closed) break;
            const sending = emit({ type: 'message', turnId: entry.turnId, message });
            message = undefined;
            await sending;
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
