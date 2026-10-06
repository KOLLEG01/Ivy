import { canonical, hashJson } from "../../../packages/sdk/src/node.js";
import { requireThat } from "../../../packages/sdk/src/node.js";
import { validateChat } from "../../../packages/sdk/src/node.js";
import { checkedNativeContract } from "../../../packages/sdk/src/node.js";
import type { Agent, Chat, Wire } from "../../../packages/sdk/src/node.js";
import {
  nativeFrameBytes,
  nativeRequestFrameBytes,
} from "../../agent-manager/src/limits.js";
import {
  validateNativeOperation,
  validateNativeRead,
  validateNativePayload,
} from "../../../packages/sdk/src/native-evidence.js";
import { readChatEvidence, saveChatEvidence } from "./evidence-bytes.js";
import { ChatStore } from "./store.js";
import { ChatAdmission } from "./admission.js";
import { ChatOperations } from './operations.js';
import { mainInputOrigin, mainTurnContext } from './main-context.js';
import {
  resolveChatPlan,
  validateChatNativeDraft,
  chatTurnOptions,
} from "./native-plan.js";

const methods = {
  "thread/start": "threadStart",
  "thread/resume": "threadResume",
  "turn/start": "turnStart",
  "turn/interrupt": "turnInterrupt",
} as const;
const same = (a: unknown, b: unknown) => canonical(a) === canonical(b);
export const chatNativeCatalog = checkedNativeContract;

/** Pure plan/identity verification; current owner discovery and Main/queue claims are separate. */
export async function verifyChatNativeRequest(
  store: ChatStore,
  definition: Chat.Definition,
  call: Chat.NativeCall,
  request: Chat.NativeRequest,
): Promise<void> {
  validateChat("Definition", definition);
  validateChat("NativeCall", call);
  validateChatNativeDraft(request);
  const plan = await resolveChatPlan(store, definition.nativePlan),
    source = chatNativeCatalog(request.nativeVersion, {
      sourceHash: request.catalogSourceHash,
    }),
    method = source.definitions.get(request.method)!;
  requireThat(
    request.nativeVersion === plan.nativeVersion &&
      request.catalogSourceHash === plan.catalogSourceHash &&
      request.catalogSourceHash === source.catalog.sourceHash &&
      call.nativeVersion === request.nativeVersion &&
      call.catalogSourceHash === request.catalogSourceHash &&
      call.method === request.method &&
      call.callerPrincipalId === definition.principalId &&
      call.serviceNodeId === definition.project.serviceNodeId &&
      call.requestHash ===
        hashJson({ method: request.method, params: request.params }) &&
      call.expectedDefinitionHash === hashJson(method) &&
      call.expectedDefinitionHash === plan.definitions[methods[request.method]],
    "chat_native_mismatch",
    "The native call must retain its configured caller, owner, catalog, method, definition and exact request.",
  );
  canonical(
    {
      id: "00000000-0000-0000-0000-000000000000",
      method: request.method,
      params: request.params,
    },
    nativeRequestFrameBytes,
  );
  source.contract.validateInput(
    request.method,
    request.params,
    nativeRequestFrameBytes,
  );
  const params = request.params as Record<string, Wire.Json>,
    template = { ...params };
  if (request.method === "thread/resume" || request.method === "turn/start")
    delete template["threadId"];
  if (request.method === "turn/start") {
    delete template["input"];
    delete template["clientUserMessageId"];
  }
  let expectedTemplate =
    request.method === "turn/interrupt" ? {} : plan[methods[request.method]];
  if (request.method === 'thread/resume' && Object.hasOwn(params, 'excludeTurns')) {
    // This native option changes the response projection; retained request bytes stay exact.
    expectedTemplate = { ...(expectedTemplate as Record<string, Wire.Json>), excludeTurns: params['excludeTurns']! };
  }
  if (request.method === "turn/start") {
    const local = store.localInfo(call.request.objectId),
      metadata =
        local ??
        (await store.client.request("objects.stat", {
          objectId: call.request.objectId,
        }));
    const contractKey = "key" in metadata ? metadata.key : metadata.contractKey;
    const objectId = "id" in metadata ? metadata.id : call.request.objectId;
    requireThat(
      contractKey === "chat-bridge/input" ||
        contractKey === "chat-bridge/evidence",
      "chat_scope_mismatch",
      "Turn options require the original input.",
    );
    const parent = await store.read(
      "chat-bridge/input",
      contractKey === "chat-bridge/input" ? objectId : metadata.parentId!,
    );
    const operation = await new ChatOperations(store, new ChatAdmission(definition)).find(parent.value.identity.senderPrincipalId, parent.value.operationId);
    requireThat(operation && ['send', 'notify'].includes(operation.value.request.action), 'chat_identity_conflict', 'Main context requires the original input operation.');
    expectedTemplate = mainTurnContext(chatTurnOptions(
      plan.turnStart,
      parent.value.payload.turnOptions,
    ), mainInputOrigin(parent.value.identity.senderPrincipalId, operation!.value.request as Chat.SendRequest | Chat.NotifyRequest));
  }
  if (request.method !== "turn/interrupt")
    requireThat(
      same(template, expectedTemplate),
      "chat_native_mismatch",
      "Native execution cannot substitute different settings for the retained plan.",
    );
  requireThat(
    !(
      params["permissions"] != null &&
      (params["sandbox"] != null || params["sandboxPolicy"] != null)
    ),
    "chat_native_mismatch",
    "Native execution cannot combine distinct permission models.",
  );
}

export async function verifyChatNativeOperation(
  store: ChatStore,
  definition: Chat.Definition,
  call: Chat.NativeCall,
  request: Chat.NativeRequest,
  value: unknown,
): Promise<Agent.Operation> {
  await verifyChatNativeRequest(store, definition, call, request);
  const source = chatNativeCatalog(request.nativeVersion, {
    sourceHash: request.catalogSourceHash,
  });
  validateNativeOperation(
    value,
    {
      operationId: call.operationId,
      callerPrincipalId: call.callerPrincipalId,
      serviceNodeId: call.serviceNodeId,
      nativeVersion: call.nativeVersion,
      nativeExecutableHash: source.catalog.nativeExecutableHash,
      catalogHash: source.catalogHash,
      method: call.method,
      params: request.params as Wire.Json,
    },
    "chat_native_mismatch",
  );
  const operation = value;
  validateNativePayload(source.contract, operation, {
    requestBytes: nativeRequestFrameBytes,
    responseBytes: nativeFrameBytes,
  });
  // Native and ChatBridge may run on different clocks. Keep their original timestamps;
  // exact owner/operation/request identities establish correlation, not wall-clock ordering.
  return operation;
}

export interface ChatReadIdentity {
  serviceNodeId: string;
  callerPrincipalId: string;
  nativeVersion: Chat.NativeRequest["nativeVersion"];
  epoch: string;
  method: string;
  params: Wire.Json;
}
export function verifyChatNativeRead(
  value: unknown,
  expected: ChatReadIdentity,
  catalogSourceHash?: string,
): asserts value is Agent.ReadObservation {
  const source = chatNativeCatalog(
    expected.nativeVersion,
    catalogSourceHash ? { sourceHash: catalogSourceHash } : undefined,
  );
  validateNativeRead(
    value,
    {
      ...expected,
      nativeExecutableHash: source.catalog.nativeExecutableHash,
      catalogHash: source.catalogHash,
    },
    "chat_native_mismatch",
  );
  validateNativePayload(source.contract, value, {
    requestBytes: nativeRequestFrameBytes,
    responseBytes: nativeFrameBytes,
  });
}

/** Every save and reconstruction verifies native authority in addition to the chunk byte proof. */
export class ChatNativeEvidence {
  readonly definition: Chat.Definition;
  constructor(
    readonly store: ChatStore,
    definition: Chat.Definition,
  ) {
    this.definition = new ChatAdmission(definition).definition;
    requireThat(
      store.rootObjectId === definition.rootObjectId,
      "chat_scope_mismatch",
      "Native evidence must use the configured ChatBridge root.",
    );
  }
  private async scope(parentId: string): Promise<void> {
    const key =
      this.store.localInfo(parentId)?.key === "chat-bridge/operation"
        ? "chat-bridge/operation"
        : "chat-bridge/input";
    const parent = await this.store.read(key, parentId);
    requireThat(
      parent.value.workspaceId === this.definition.workspaceId &&
        parent.value.definitionHash === hashJson(this.definition),
      "chat_scope_mismatch",
      "Native evidence cannot attach to another workspace or configuration under the same Hive root.",
    );
  }
  async saveRequest(
    parentId: string,
    request: Chat.NativeRequest,
  ): Promise<Chat.ObjectPin> {
    validateChatNativeDraft(request);
    const original = structuredClone(request);
    await this.scope(parentId);
    return saveChatEvidence(
      this.store,
      parentId,
      "native-request/canonical-json",
      original,
    );
  }
  private async readRequest(
    parentId: string,
    call: Chat.NativeCall,
  ): Promise<Chat.NativeRequest> {
    await this.scope(parentId);
    return (await readChatEvidence(
      this.store,
      parentId,
      call.request,
      "native-request/canonical-json",
    )) as Chat.NativeRequest;
  }
  async request(
    parentId: string,
    call: Chat.NativeCall,
  ): Promise<Chat.NativeRequest> {
    const value = await this.readRequest(parentId, call);
    await verifyChatNativeRequest(this.store, this.definition, call, value);
    return value;
  }
  async saveOperation(
    parentId: string,
    call: Chat.NativeCall,
    value: unknown,
  ): Promise<Chat.ObjectPin> {
    const request = await this.readRequest(parentId, call);
    await verifyChatNativeOperation(
      this.store,
      this.definition,
      call,
      request,
      value,
    );
    return saveChatEvidence(
      this.store,
      parentId,
      "agent-operation/canonical-json",
      value,
    );
  }
  async operation(
    parentId: string,
    call: Chat.NativeCall,
    pin: Chat.ObjectPin,
  ): Promise<Agent.Operation> {
    const request = await this.readRequest(parentId, call),
      value = await readChatEvidence(
        this.store,
        parentId,
        pin,
        "agent-operation/canonical-json",
      );
    return verifyChatNativeOperation(
      this.store,
      this.definition,
      call,
      request,
      value,
    );
  }
  async saveRead(
    parentId: string,
    expected: ChatReadIdentity,
    value: unknown,
  ): Promise<Chat.ObjectPin> {
    requireThat(
      expected.serviceNodeId === this.definition.project.serviceNodeId &&
        expected.callerPrincipalId === this.definition.principalId &&
        expected.nativeVersion === this.definition.nativePlan.nativeVersion,
      "chat_native_mismatch",
      "The native read must use the configured ChatBridge owner.",
    );
    verifyChatNativeRead(
      value,
      expected,
      this.definition.nativePlan.catalogSourceHash,
    );
    await this.scope(parentId);
    return saveChatEvidence(
      this.store,
      parentId,
      "agent-read-observation/canonical-json",
      value,
    );
  }
  async read(
    parentId: string,
    expected: ChatReadIdentity,
    pin: Chat.ObjectPin,
  ): Promise<Agent.ReadObservation> {
    requireThat(
      expected.serviceNodeId === this.definition.project.serviceNodeId &&
        expected.callerPrincipalId === this.definition.principalId &&
        expected.nativeVersion === this.definition.nativePlan.nativeVersion,
      "chat_native_mismatch",
      "The native read must use the configured ChatBridge owner.",
    );
    await this.scope(parentId);
    const value = await readChatEvidence(
      this.store,
      parentId,
      pin,
      "agent-read-observation/canonical-json",
    );
    verifyChatNativeRead(
      value,
      expected,
      this.definition.nativePlan.catalogSourceHash,
    );
    return value;
  }
}
