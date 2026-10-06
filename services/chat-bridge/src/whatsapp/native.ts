import type { Agent, Chat, Wire } from "../../../../packages/sdk/src/node.js";
import { canonical, IvyError, requireThat, readNativeTurnContents, verifyNativeTurnContents } from "../../../../packages/sdk/src/node.js";
import { checkedNativeContract } from "../../../../packages/sdk/src/node.js";
import { NativeOwner } from "../../../../packages/sdk/src/native-owner.js";
import type { NativeOperationCall } from "../../../../packages/sdk/src/native-owner.js";
import type { ChatBridge } from "../bridge.js";
import { identity, WhatsAppJournal } from "./journal.js";
import { resolveChatPlan, chatTurnOptions } from "../native-plan.js";
import { admittedNativeInput } from "../input-media.js";
import type { Document } from "../store.js";
import { hashJson } from "../../../../packages/sdk/src/node.js";
import { randomUUID } from "node:crypto";
import { scopedOperationId } from "../../../../packages/sdk/src/client.js";
import { nativeThreadState } from "../../../../packages/sdk/src/native-observations.js";
import { translator } from "./messages.js";
import type { Translator } from "./messages.js";
import { mainTurnContext } from '../main-context.js';
interface SavedCall {
  call: NativeOperationCall;
  attempted: boolean;
  result?: Wire.Json;
}
export class WhatsAppNative {
  readonly owner: NativeOwner;
  private current: Promise<{
    main: Document<"chat-bridge/main">;
    binding: Document<"chat-bridge/binding">;
  }> | null = null;
  private version = -1;
  private busy = false;
  lastDispatchMs: number | null = null;
  private busyRevision = 0;
  private authRetryAt = 0;
  private prepared: { id: string; work: Promise<void> } | null = null;
  private host: Promise<string> | null = null;
  private readonly completedTurns = new Map<string, { epoch: string; stamp: string; turn: Record<string, Wire.Json>; bytes: number }>();
  sameHost(hostId: string): Promise<boolean> {
    this.host ??= this.owner
      .status()
      .then((s) => s.hostId)
      .catch((error) => {
        this.host = null;
        throw error;
      });
    return this.host.then((id) => id === hostId);
  }
  async prepare(): Promise<void> {
    const { binding } = await this.bound(),
      id = binding.pin.objectId;
    if (this.prepared?.id !== id) {
      const work = (async () => {
        const busyRevision = this.busyRevision;
        const plan = await resolveChatPlan(
          this.bridge.main.store,
          this.bridge.main.admission.definition.nativePlan,
        );
        const bound = await this.owner.binding("thread/resume");
        const response = await this.owner.interact({
          operationId: "wa-resume-" + randomUUID(),
          method: "thread/resume",
          definitionHash: bound.definitionHash,
          params: {
            ...plan.threadResume,
            threadId: binding.value.primary.nativeId,
            excludeTurns: true,
          },
        });
        if (
          "error" in response.reply &&
          response.reply.error.code === -32600 &&
          response.reply.error.message ===
            `no rollout found for thread id ${binding.value.primary.nativeId}`
        ) {
          // An empty Main may still be loaded. Its binding alone cannot prove that
          // the native owner retained it after a process restart.
          const observed = await this.owner.read('thread/read', {
            threadId: binding.value.primary.nativeId, includeTurns: false,
          }, undefined, true);
          if ('error' in observed.reply && observed.reply.error.code === -32600 &&
            observed.reply.error.message === `thread not loaded: ${binding.value.primary.nativeId}`)
            throw new IvyError('chat_main_unavailable', this.t('main.unavailable'));
          requireThat('result' in observed.reply, 'native_read_failed', 'Main availability could not be observed.');
          const state = nativeThreadState(checkedNativeContract(binding.value.nativePlan.nativeVersion).contract,
            observed.reply.result);
          if (busyRevision === this.busyRevision) this.busy = !state.canStartTurn;
          return;
        }
        requireThat(
          "result" in response.reply,
          "native_read_failed",
          "Main could not be attached to native notifications.",
        );
        const state = nativeThreadState(
          checkedNativeContract(binding.value.nativePlan.nativeVersion)
            .contract,
          response.reply.result,
        );
        if (busyRevision === this.busyRevision) this.busy = !state.canStartTurn;
      })();
      this.prepared = { id, work };
      void work.catch(() => {
        if (this.prepared?.work === work) this.prepared = null;
      });
    }
    return this.prepared!.work;
  }
  invalidate(connectionChanged = false): void {
    this.current = null;
    if (connectionChanged) {
      this.prepared = null;
      this.host = null;
      this.busy = false;
    }
  }
  setBusy(busy: boolean): void {
    this.busy = busy;
    this.busyRevision++;
  }
  private async requireAccount(): Promise<void> {
    requireThat(Date.now() >= this.authRetryAt, 'chat_auth_required', this.t('message.auth_required'));
    const binding = await this.owner.binding('account/read');
    const response = await this.owner.interact({ operationId: 'wa-account-' + randomUUID(), method: 'account/read',
      definitionHash: binding.definitionHash, params: { refreshToken: false } });
    const account = 'result' in response.reply ? response.reply.result as { requiresOpenaiAuth?: boolean; account?: unknown } | null : null;
    requireThat(account && typeof account.requiresOpenaiAuth === 'boolean', 'native_read_failed', 'Native account readiness could not be read.');
    if (account.requiresOpenaiAuth && !account.account) {
      this.authRetryAt = Date.now() + 30_000;
      throw new IvyError('chat_auth_required', this.t('message.auth_required'));
    }
  }
  bound() {
    if (this.version !== this.bridge.main.store.changeVersion) {
      this.current = null;
      this.version = this.bridge.main.store.changeVersion;
    }
    if (!this.current) {
      const pending = (async () => {
        const main = await this.bridge.main.find();
        requireThat(
          main?.value.binding,
          "chat_main_missing",
          this.t("main.first_message"),
        );
        return {
          main,
          binding: await this.bridge.main.store.read(
            "chat-bridge/binding",
            main.value.binding,
          ),
        };
      })();
      this.current = pending;
      void pending.catch(() => {
        if (this.current === pending) this.current = null;
      });
    }
    return this.current;
  }
  constructor(
    readonly bridge: ChatBridge,
    readonly journal: WhatsAppJournal,
    readonly t: Translator = translator(),
  ) {
    const d = bridge.main.admission.definition,
      c = checkedNativeContract(d.nativePlan.nativeVersion, {
        sourceHash: d.nativePlan.catalogSourceHash,
      });
    this.owner = new NativeOwner(bridge.main.store.client, d.principalId, {
      serviceNodeId: d.project.serviceNodeId,
      nativeVersion: c.catalog.version,
      nativeExecutableHash: c.catalog.nativeExecutableHash,
      catalogHash: c.catalogHash,
    });
  }
  /** Transport-owned intent and AgentManager receipt suffice for a direct Main turn. */
  async send(
    sender: string,
    request: Chat.SendRequest,
  ): Promise<Wire.Json | null> {
    const original = structuredClone(request),
      main = this.bridge.main;
    main.admission.send(sender, original);
    requireThat(
      original.expectedBinding,
      "chat_main_missing",
      "Direct WhatsApp delivery needs the automatically selected Main binding.",
    );
    const expectedBinding = original.expectedBinding;
    const key = "direct-request:" + original.messageId,
      hash = hashJson({ sender, request: original }),
      prior = this.journal.get<string>(key);
    requireThat(
      !prior || prior === hash,
      "whatsui_identity_conflict",
      "The admitted direct input cannot change.",
    );
    if (!prior) this.journal.set(key, hash);
    const { binding } = await this.bound();
    if (binding.pin.objectId !== expectedBinding.objectId && this.journal.get('recover-main:' + original.messageId)) {
      await this.recoverMain(sender, original);
      return null;
    }
    requireThat(
      binding.pin.objectId === expectedBinding.objectId &&
        binding.pin.revision === expectedBinding.revision,
      "chat_binding_mismatch",
      "Main changed before input delivery.",
    );
    const threadId = binding.value.primary.nativeId;
    try { return await this.command(
      "input:" + original.messageId,
      "turn/start",
      async () => {
        const plan = await resolveChatPlan(
          main.store,
          main.admission.definition.nativePlan,
        );
        const local = this.journal.get<Wire.Json[]>(
          "local-input:" + original.messageId,
        );
        const input = original.payload.images.length
          ? (
              await admittedNativeInput(
                main.admission,
                main.store.client,
                sender,
                original,
              )
            ).input
          : [
              ...(original.payload.text
                ? [{ type: "text", text: original.payload.text }]
                : []),
              ...(local ?? []),
            ];
        return {
          ...mainTurnContext(chatTurnOptions(plan.turnStart, original.payload.turnOptions), 'user message from WhatsApp'),
          threadId,
          input,
          clientUserMessageId: "wa-" + identity(original.messageId),
        };
      },
      async () => {
        await this.prepare();
        await this.requireAccount();
        const { main: current } = await this.bound();
        requireThat(
          current?.value.binding?.objectId === expectedBinding.objectId &&
            current.value.binding.revision === expectedBinding.revision,
          "chat_binding_mismatch",
          "Main changed before the original input was delivered.",
        );
        requireThat(
          !current.value.pendingAction && !current.value.queue.length,
          "native_read_pending",
          "Earlier Main work is still pending.",
        );
        requireThat(
          !this.busy,
          "native_read_pending",
          "The native Main has an active turn.",
        );
      },
    ); } catch (error) {
      if (IvyError.from(error).code !== 'chat_main_unavailable') throw error;
      await this.recoverMain(sender, original);
      return null;
    }
  }
  private async recoverMain(sender: string, original: Chat.SendRequest): Promise<void> {
    const id = original.messageId, key = 'interaction:input:' + id;
    const attempted = this.journal.get<SavedCall>(key);
    const legacy = this.journal.get<{attempted: boolean}>('native:input:' + id);
    requireThat(!attempted?.attempted && !legacy?.attempted && attempted?.result === undefined,
      'interaction_expired', this.t('message.expired'));
    const main = this.bridge.main;
    let recovery = this.journal.get<Chat.CreateMainRequest>('recover-main:' + id);
    if (!recovery) {
      const current = await main.find();
      requireThat(current && current.value.binding && original.expectedBinding,
        'native_read_pending', 'Main recovery requires its original binding.');
      requireThat(current.value.binding.objectId === original.expectedBinding.objectId &&
        current.value.binding.revision === original.expectedBinding.revision &&
        !current.value.pendingAction && current.value.queue.length === 0,
        'native_read_pending', 'Main changed before its missing native session could be recovered.');
      recovery = {action:'createMain', operationId:await scopedOperationId(main.store.client,
        ['whatsapp-recover-main', id, original.expectedBinding.objectId]),
        expectedBridge:main.admission.expected(sender), expectedMainRevision:current.pin.revision,
        reason:'Automatically recover Main after its native session was confirmed missing.'};
      this.journal.set('recover-main:' + id, recovery);
      await this.journal.flush();
    }
    const result = await main.create(sender, recovery);
    if (result.phase !== 'succeeded') {
      requireThat(result.phase !== 'failed', result.error?.code ?? 'chat_operation_failed',
        result.error?.detail ?? 'Main recovery failed.');
      throw new IvyError('native_read_pending', 'The original Main recovery is still pending.');
    }
    requireThat(result.outcome?.action === 'createMain', 'chat_native_mismatch', 'Main recovery needs its binding receipt.');
    const binding = result.outcome.binding.object;
    this.journal.transaction(() => {
      this.journal.rebindRequest(id, original, {...original, expectedBinding:binding});
      this.journal.set(key, null);
      this.journal.set('direct-request:' + id, null);
      this.journal.set('listener-new:' + binding.objectId, true);
      this.journal.set('recover-main:' + id, null);
    });
    await this.journal.flush();
    this.invalidate(true);
  }
  async read(
    method: Agent.ReadObservation["method"],
    params: Wire.Json,
  ): Promise<Record<string, Wire.Json>> {
    const observed = await this.owner.read(method, params, undefined, true);
    return this.readResult(observed, method, params);
  }
  /** One metadata turn at a time; contents use item pages when the owner supports them. */
  async turnPage(threadId: string, cursor: string | null): Promise<Record<string, Wire.Json>> {
    const params = { threadId, cursor, limit: 1, sortDirection: 'desc', itemsView: 'notLoaded' };
    const snapshot = await this.owner.read('thread/turns/list', params, undefined, true);
    const page = await this.readResult(snapshot, 'thread/turns/list', params), data = page['data'];
    requireThat(Array.isArray(data) && data.length <= 1, 'native_read_failed', 'History recovery requires its one-turn metadata page.');
    if (!data.length) return page;
    const turn = data[0] as Record<string, Wire.Json>;
    requireThat(typeof turn['id'] === 'string', 'native_read_failed', 'History recovery requires the original turn identity.');
    const { items: _items, itemsView: _view, ...metadata } = turn;
    const key = canonical([threadId, turn['id']]), stamp = canonical(metadata), saved = this.completedTurns.get(key);
    if (saved?.epoch === snapshot.epoch && saved.stamp === stamp) return { ...page, data: [structuredClone(saved.turn)] };
    this.completedTurns.delete(key);
    const contents = await readNativeTurnContents(this.owner, snapshot, threadId, turn['id']);
    const verified = await verifyNativeTurnContents(this.owner, contents, threadId, turn['id']);
    const complete = { ...verified.turn, itemsView: 'full', items: verified.items.map(item =>
      ['userMessage', 'steeringUserMessage', 'agentMessage'].includes(String(item['type'])) ? item : { id: item['id']!, type: item['type']! }) };
    if (['completed', 'failed', 'interrupted'].includes(String(turn['status'])) && typeof turn['completedAt'] === 'number' && Number.isFinite(turn['completedAt'])) {
      const bytes = Buffer.byteLength(canonical(complete));
      if (bytes <= 32 * 1024 * 1024) {
        this.completedTurns.delete(key);
        while (this.completedTurns.size >= 16 || [...this.completedTurns.values()].reduce((sum, value) => sum + value.bytes, bytes) > 32 * 1024 * 1024)
          this.completedTurns.delete(this.completedTurns.keys().next().value!);
        this.completedTurns.set(key, { epoch: snapshot.epoch, stamp, turn: structuredClone(complete), bytes });
      }
    }
    return { ...page, data: [complete] };
  }
  private async readResult(observed: Agent.ReadObservation, method: Agent.ReadObservation['method'], params: Wire.Json): Promise<Record<string, Wire.Json>> {
    if (
      method === "thread/turns/list" &&
      "error" in observed.reply &&
      params &&
      typeof params === "object" &&
      !Array.isArray(params) &&
      typeof params["threadId"] === "string" &&
      observed.reply.error.code === -32600 &&
      observed.reply.error.message ===
        `thread ${params["threadId"]} is not materialized yet; thread/turns/list is unavailable before first user message`
    ) {
      const { binding } = await this.bound();
      requireThat(
        binding.value.primary.nativeId === params["threadId"],
        "chat_native_mismatch",
        "An empty-history observation must target the bound Main.",
      );
      // Native explicitly confirms that no first user message exists; there is no history to replay.
      return { data: [], nextCursor: null };
    }
    if (
      "error" in observed.reply &&
      observed.reply.error.code === -32600 &&
      observed.reply.error.message.startsWith("thread not loaded: ")
    ) {
      const { binding } = await this.bound();
      const threadId = binding.value.primary.nativeId;
      requireThat(
        params &&
          typeof params === "object" &&
          !Array.isArray(params) &&
          params["threadId"] === threadId &&
          observed.reply.error.message === `thread not loaded: ${threadId}`,
        "chat_native_mismatch",
        "Resume must target the bound Main.",
      );
      this.prepared = null;
      await this.prepare();
      requireThat(
        false,
        "native_read_pending",
        "The bound Main is being resumed; retry its original read.",
      );
    }
    requireThat(
      "result" in observed.reply,
      "native_read_failed",
      "Native read failed.",
    );
    return observed.reply.result as Record<string, Wire.Json>;
  }
  /** Only outstanding mutation intent is transport state. Native owns the conversation. */
  async command(
    key: string,
    method: string,
    params: () => Promise<Wire.Json>,
    guard: () => Promise<void> = async () => {},
  ): Promise<Wire.Json | null> {
    const readOnly = ["model/list", "thread/list"].includes(method),
      journalKey = "interaction:" + key;
    // Honor outstanding effects in the reference transport journal before using the new path.
    const retained = this.journal.get<{
      call: NativeOperationCall;
      attempted: boolean;
    }>("native:" + key);
    if (retained?.attempted) {
      const previous = await this.owner.operation(retained.call);
      requireThat(
        !("kind" in previous) && previous.phase !== "outcome_unknown",
        "interaction_expired",
        this.t("native.previous_unclear"),
      );
      if (previous.phase === "accepted" || previous.phase === "dispatched")
        return null;
      requireThat(
        previous.reply && "result" in previous.reply,
        "whatsapp_native_command_failed",
        this.t("native.previous_failed"),
      );
      return previous.reply.result;
    }
    let saved = readOnly ? null : this.journal.get<SavedCall>(journalKey);
    if (!saved) {
      const binding = await this.owner.binding(method);
      const callParams = await params();
      saved = {
        call: {
          operationId: "wa-" + identity(key, method, callParams),
          method,
          params: callParams,
          definitionHash: binding.definitionHash,
        },
        attempted: false,
      };
      if (!readOnly) this.journal.set(journalKey, saved);
    }
    requireThat(
      saved.call.method === method,
      "whatsui_identity_conflict",
      "Command recovery cannot change the native method.",
    );
    if (saved.result !== undefined) return saved.result;
    let response: Agent.Interaction;
    const busyRevision = this.busyRevision,
      started = performance.now(),
      recovering = saved.attempted;
    try {
      response = saved.attempted
        ? await this.owner.interaction(saved.call.operationId)
        : await this.owner.interact(saved.call, async () => {
            await guard();
            saved!.attempted = true;
            if (!readOnly) {
              this.journal.set(journalKey, saved);
              await this.journal.flush();
            }
          });
    } catch (error) {
      if (
        error instanceof IvyError &&
        error.code === "interaction_expired" &&
        method === "turn/start"
      ) {
        const original = saved.call.params as Record<string, Wire.Json>;
        let cursor: string | null = null;
        // Bounded native recovery using the original client message identity; no local history replica.
        for (let page = 0; page < 64; page++) {
          const history = await this.turnPage(original['threadId'] as string, cursor);
          for (const turn of history["data"] as Array<
            Record<string, Wire.Json>
          >) {
            const items = turn["items"] as Array<Record<string, Wire.Json>>;
            if (
              items.some(
                (item) =>
                  item["type"] === "userMessage" &&
                  item["clientId"] === original["clientUserMessageId"],
              )
            )
              return { turn };
          }
          cursor = history["nextCursor"] as string | null;
          if (!cursor) break;
        }
      }
      if (
        !recovering &&
        error instanceof IvyError &&
        error.outcome === "not_executed"
      ) {
        saved.attempted = false;
        if (!readOnly) {
          this.journal.set(journalKey, saved);
          await this.journal.flush();
        }
      }
      throw error;
    }
    this.lastDispatchMs = Math.round(performance.now() - started);
    if ("error" in response.reply) {
      // Native rejected the request before creating a turn; retry only concrete busy/unloaded states.
      if (
        method === "turn/start" &&
        response.reply.error.code === -32600 &&
        (response.reply.error.message ===
          `thread not loaded: ${(saved.call.params as Record<string, Wire.Json>)["threadId"]}` ||
          /turn.*(?:in progress|already active)|(?:active|in progress).*turn/i.test(
            response.reply.error.message,
          ))
      ) {
        saved.attempted = false;
        saved.call.operationId = "wa-retry-" + randomUUID();
        this.journal.set(journalKey, saved);
        this.prepared = null;
        this.invalidate();
        throw new IvyError("native_read_pending", response.reply.error.message);
      }
      throw new IvyError(
        "whatsapp_native_command_failed",
        response.reply.error.message,
        "completed",
      );
    }
    saved.result = response.reply.result;
    if (!readOnly) this.journal.set(journalKey, saved);
    if (method === "turn/start" && busyRevision === this.busyRevision)
      this.busy = true;
    return response.reply.result;
  }
}
