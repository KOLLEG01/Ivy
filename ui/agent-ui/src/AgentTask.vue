<script setup lang="ts">
import {
  computed,
  nextTick,
  onBeforeUnmount,
  onMounted,
  ref,
  shallowRef,
  watch,
} from "vue";
import {
  Archive,
  ArrowDown,
  ArrowUp,
  ChevronRight,
  Download,
  FolderOpen,
  LoaderCircle,
  MoreHorizontal,
  Square,
  Target,
  Trash2,
  X,
} from "@lucide/vue";
import {
  ActivityIndicator,
  Button,
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
  ComposerAddMenu,
  ComposerAttachments,
  ComposerQueue,
  ConversationComposer,
  ContentView,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  Input,
  Label,
  OptionSelect,
  RemoteState,
  Textarea,
  useRemote,
  submitOnEnter,
  ToolbarContent,
} from "@ivy/ui";
import type { Agent, BoundTool, Transport, Wire } from "../../../packages/sdk/src/client.js";
import { route, usePage } from "../../../packages/ui-client/src/runtime";
import {
  list,
  modelsFrom,
  nativeRead,
  optionalTool,
  permissionProfileLabel,
  permissionProfilesFrom,
  record,
  supportsFields,
  text,
} from "../../../packages/ui-client/src/native";
import { useNativeAction } from "../../../packages/ui-client/src/native-action";
import NativeActionState from "../../../packages/ui-client/src/NativeActionState.vue";
import NativeInputs from "../../../packages/ui-client/src/NativeInputs.vue";
import TaskProjectDialog from "../../../packages/ui-client/src/TaskProjectDialog.vue";
import MessageImages from "../../../packages/ui-client/src/MessageImages.vue";
import { readNativeImage } from "../../../packages/ui-client/src/native-images";
import {
  isUnmaterializedNativeHistory,
  readNativeOutputPage,
} from "../../../packages/ui-client/src/native-output-page";
import type { NativeOutputPage } from "../../../packages/ui-client/src/native-output-page";
import {
  nativeModesFrom,
  nativeModePayload,
} from "../../../packages/ui-client/src/native-modes";
import { readNativeThreadControl } from "../../../packages/ui-client/src/native-thread-control";
import { useNativeSettings, effectiveNativeSettings } from "../../../packages/ui-client/src/native-settings";
import { useObjectArchive } from "../../../packages/ui-client/src/object-archive";
import {
  messageInput,
  supportsInputType,
  useMessageAttachments,
} from "../../../packages/ui-client/src/message-attachments";
import type { StagedAttachment } from "../../../packages/ui-client/src/message-attachments";
import { base, client, notifications, outputCache, tr } from "./runtime";
import {
  activitySummary,
  conversationItem,
  liveMessages,
  liveSteps,
} from "./conversation";
import type { ConversationItem, ConversationStep } from "./conversation";
const props = defineProps<{ node: string; threadId: string; turnId: string }>();
const state = useRemote((signal) =>
  readNativeThreadControl(client, props.node, props.threadId, signal),
);
const imageCwd = computed(() => text(state.value.value?.thread.cwd));
const imageLoader = computed(() => {
  const node = props.node, cwd = imageCwd.value;
  return cwd ? (source: string, signal: AbortSignal) => readNativeImage(client, node, source, cwd, signal) : null;
});
const taskInventory = useRemote(
  (signal) =>
    client.request(
      "inventory.get",
      {
        resourceRef: {
          serviceNodeId: props.node,
          namespace: "codex",
          kind: "thread",
          nativeId: props.threadId,
        },
      },
      { signal },
    ),
  0,
  ["inventory/codex/thread/" + props.node],
);
const capabilities = useRemote(async () => {
  const [
    send,
    resume,
    interrupt,
    rename,
    archive,
    unarchive,
    remove,
    goalSet,
    goalClear,
    steer,
    project,
  ] = await Promise.all(
    [
      "codex.turn/start",
      "codex.thread/resume",
      "codex.turn/interrupt",
      "codex.thread/name/set",
      "codex.thread/archive",
      "codex.thread/unarchive",
      "codex.thread/delete",
      "codex.thread/goal/set",
      "codex.thread/goal/clear",
      "codex.turn/steer",
      "codex.thread/metadata/update",
    ].map((name) => optionalTool(client, props.node, name)),
  );
  return {
    send,
    resume,
    interrupt,
    rename,
    archive,
    unarchive,
    remove,
    goalSet,
    goalClear,
    steer,
    project,
  };
}, 0, ["services/agent-manager/" + props.node]);
const projectOpen = ref(false);
const settings = useNativeSettings(client, () => props.node, () => text(state.value.value?.thread.cwd));
const { models, permissions, modes } = settings;
const goal = useRemote(async (signal) =>
  (await optionalTool(client, props.node, "codex.thread/goal/get"))
    ? nativeRead(
        client,
        props.node,
        "codex.thread/goal/get",
        { threadId: props.threadId },
        signal,
      )
    : null,
);
const turns = usePage(async (signal, cursor) => {
  try {
    const result = record(
      await nativeRead(
        client,
        props.node,
        "codex.thread/turns/list",
        {
          threadId: props.threadId,
          limit: 30,
          sortDirection: "desc",
          itemsView: "notLoaded",
          ...(cursor ? { cursor } : {}),
        },
        signal,
      ),
    );
    return {
      items: list(result.data),
      nextCursor: text(result.nextCursor) || null,
    };
  } catch (error) {
    if (
      !cursor &&
      !props.turnId &&
      isUnmaterializedNativeHistory(error, props.threadId)
    )
      return { items: [], nextCursor: null };
    throw error;
  }
});
const selectedTurn = computed(() => props.turnId);
const outputScope = () =>
  [props.node, props.threadId, selectedTurn.value] as const;
const cachedOutput = () => {
  const page = outputCache.read(outputScope());
  return page ? [page] : [];
};
const outputPages = shallowRef<NativeOutputPage[]>(cachedOutput());
const outputPreview = ref(outputPages.value.length > 0);
const outputLoading = ref(false);
const outputLoadingEarlier = ref(false);
const outputError = ref<string | null>(null);
let outputController: AbortController | null = null;
let outputSerial = 0,
  outputQueued = false;
const outputNextCursor = computed(
  () => outputPages.value.at(-1)?.nextCursor ?? null,
);
const outputDownload = computed(
  () => outputPages.value.find((page) => page.download)?.download ?? null,
);
const itemId = (entry: unknown) => text(record(record(entry).item).id);
const loadOutput = async (reset: boolean, live = false) => {
  if (outputLoading.value || outputLoadingEarlier.value) {
    // Never drop a reload that arrives while another read is in flight.
    if (reset) outputQueued = true;
    return;
  }
  const cursor = reset ? undefined : (outputNextCursor.value ?? undefined);
  if (!reset && (!cursor || outputPreview.value)) return;
  const scope = outputScope();
  const pageCount = reset && live ? outputPages.value.length : 1;
  const serial = reset ? ++outputSerial : outputSerial;
  if (reset) {
    outputController?.abort();
    outputController = new AbortController();
    outputLoading.value = true;
  } else outputLoadingEarlier.value = true;
  const element = scroller.value;
  const previousHeight = element?.scrollHeight ?? 0;
  const previousTop = element?.scrollTop ?? 0;
  try {
    const page = await readNativeOutputPage(
      client,
      scope[0],
      scope[1],
      scope[2],
      cursor,
      outputController?.signal ?? new AbortController().signal,
    );
    if (serial !== outputSerial) return;
    const pages = [page];
    // Reread the loaded item window with fresh cursors. Unioning old entries retains deletions
    // and reusing their opaque cursors can duplicate or skip messages after an insertion.
    while (
      reset &&
      page.mode === "items" &&
      pages.length < pageCount &&
      pages.at(-1)?.nextCursor
    ) {
      pages.push(
        await readNativeOutputPage(
          client,
          scope[0],
          scope[1],
          scope[2],
          pages.at(-1)!.nextCursor!,
          outputController!.signal,
        ),
      );
      if (serial !== outputSerial) return;
    }
    outputPages.value = reset ? pages : [...outputPages.value, page];
    if (reset) outputCache.write(scope, page);
    outputPreview.value = false;
    outputError.value = null;
    if (!reset && element) {
      await nextTick();
      element.scrollTop = element.scrollHeight - previousHeight + previousTop;
    }
  } catch (cause) {
    if (serial !== outputSerial || outputController?.signal.aborted) return;
    outputCache.invalidate(scope[0], scope[1]);
    if (reset) outputPages.value = [];
    outputPreview.value = false;
    outputError.value =
      cause instanceof Error
        ? cause.message
        : "Saved output could not be loaded.";
  } finally {
    if (serial === outputSerial) {
      outputLoading.value = false;
      outputLoadingEarlier.value = false;
    }
  }
  if (outputQueued && serial === outputSerial) {
    outputQueued = false;
    queueMicrotask(() => void loadOutput(true, true));
  }
  if (
    serial === outputSerial &&
    outputPages.value.at(-1)?.mode === "search" &&
    outputNextCursor.value
  )
    queueMicrotask(() => void loadOutput(false));
};
const refreshOutput = () => loadOutput(true);
const reloadOutput = () => loadOutput(true, true);
const loadEarlierOutput = () => loadOutput(false);
const resetOutputView = (preview: boolean) => {
  outputSerial++;
  outputController?.abort();
  outputQueued = false;
  outputLoading.value = false;
  outputLoadingEarlier.value = false;
  outputError.value = null;
  outputPages.value = preview ? cachedOutput() : [];
  outputPreview.value = outputPages.value.length > 0;
};
watch(
  () => state.value.value?.status.epoch,
  (epoch) => {
    if (epoch && outputCache.observeEpoch(props.node, epoch)) {
      resetOutputView(false);
      void refreshOutput();
    }
  },
);
const downloadTurn = () => {
  const download = outputDownload.value;
  if (!download || outputLoading.value || outputError.value) return;
  const url = URL.createObjectURL(
    new Blob([download.raw], { type: "application/json" }),
  );
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = download.name;
  anchor.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
};
const action = useNativeAction(
  client,
  "ivy:agent-task-action:" + base.href + props.node + ":" + props.threadId,
);
const goalAction = useNativeAction(
  client,
  "ivy:agent-task-goal:" + base.href + props.node + ":" + props.threadId,
);
watch(
  () => action.saved.value,
  (saved) => {
    if (
      saved?.phase === "succeeded" &&
      saved.call.qualifiedName === "codex.thread/resume" &&
      saved.call.serviceNodeId === props.node &&
      record(saved.call.arguments).threadId === props.threadId
    )
      void state.refresh();
  },
  { deep: true, immediate: true },
);
const draftKey =
  "ivy:agent-message:" + base.href + props.node + ":" + props.threadId;
const message = ref(""),
  model = ref(""),
  effort = ref(""),
  mode = ref(""),
  permission = ref("");
const goalDraft = ref(""),
  goalEditing = ref(false),
  goalDirty = ref(false);
const attachments = useMessageAttachments(client, () => props.node);
// Messages sent while a turn runs wait here and follow in order; each can instead steer the running turn.
interface Options { model: string; effort: string; mode: string; permission: string }
interface Queued {
  id: string;
  text: string;
  attachments: StagedAttachment[];
  options: Options;
  operationId?: string | undefined;
  error?: string | undefined;
}
const queueKey = "ivy:agent-queue:" + base.href + props.node + ":" + props.threadId;
const queue = ref<Queued[]>([]);
try {
  const saved = JSON.parse(sessionStorage.getItem(draftKey) ?? "{}");
  message.value = text(saved.message);
  model.value = text(saved.model);
  effort.value = text(saved.effort);
  mode.value = text(saved.mode);
  permission.value = text(saved.permission);
  if (Array.isArray(saved.attachments)) attachments.restore(saved.attachments);
  const queued = JSON.parse(sessionStorage.getItem(queueKey) ?? "[]");
  if (Array.isArray(queued)) queue.value = queued;
} catch {
  action.error.value = "The saved message could not be read.";
}
watch([message, model, effort, mode, permission, attachments.staged], () => {
  try {
    sessionStorage.setItem(
      draftKey,
      JSON.stringify({
        message: message.value,
        model: model.value,
        effort: effort.value,
        mode: mode.value,
        permission: permission.value,
        attachments: attachments.staged.value,
      }),
    );
  } catch {
    action.error.value = "The message draft cannot be retained in this tab.";
  }
});
watch(
  queue,
  (value) => {
    try {
      if (value.length) sessionStorage.setItem(queueKey, JSON.stringify(value));
      else sessionStorage.removeItem(queueKey);
    } catch {
      action.error.value = "Queued messages cannot be retained in this tab. Keep this page open.";
    }
  },
  { deep: true },
);
const goalRecord = computed(() => record(record(goal.value.value).goal));
watch(
  () => goalRecord.value.objective,
  (objective) => {
    if (!goalDirty.value) goalDraft.value = text(objective);
  },
  { immediate: true },
);
const choices = computed(() => modelsFrom(models.value.value));
const permissionChoices = computed(() =>
  permissionProfilesFrom(permissions.value.value).filter(
    (profile) => profile.allowed,
  ),
);
const latestSettings = ref<Record<string, unknown>>({});
const nativeMode = computed(() => text(record(latestSettings.value.collaborationMode ?? state.value.value?.thread.collaborationMode).mode));
const nativeSettings = computed(() => {
  const resumed = action.saved.value?.phase === 'succeeded' && action.saved.value.call.qualifiedName === 'codex.thread/resume' ? record(action.saved.value.result) : {};
  const current = { ...resumed, ...record(state.value.value?.source), ...state.value.value?.thread, ...latestSettings.value };
  current.model ||= resumed.model;
  current.reasoningEffort ||= resumed.reasoningEffort;
  const profile = settings.defaults.value.value?.document[state.value.value?.status.serverType ?? 'codex'];
  if (profile?.mode && (!nativeMode.value || nativeMode.value === 'default') && turns.value.value?.items.length === 0 && !turns.value.value.nextCursor) current.collaborationMode = null;
  return effectiveNativeSettings(models.value.value, settings.config.value.value, current, turns.value.value?.items.length === 0 ? profile : null);
});
const nativeModel = computed(() => nativeSettings.value.model);
const nativeEffort = computed(() => model.value && model.value !== nativeModel.value
  ? choices.value.find(value => value.model === model.value)?.defaultEffort ?? '' : nativeSettings.value.effort);
const modelLabel = computed(() => choices.value.find(value => value.model === nativeModel.value)?.name || nativeModel.value || (models.error.value ? 'Model unavailable' : 'Loading model…'));
const modeLabel = computed(() => modes.loading.value && !modes.value.value ? 'Loading modes…' : modeChoices.value.find(value => value.mode === nativeSettings.value.mode)?.name || (modes.error.value ? 'Modes unavailable' : 'Default'));
const effectiveModel = computed(() => model.value || nativeModel.value);
const modelChoice = computed(() =>
  choices.value.find((value) => value.model === effectiveModel.value),
);
const efforts = computed(() => modelChoice.value?.efforts ?? []);
watch(model, () => {
  if (effort.value && !efforts.value.includes(effort.value)) effort.value = "";
});
const modeChoices = computed(() =>
  nativeModesFrom(capabilities.value.value?.send, modes.value.value),
);
const payloadFor = (options: Options) =>
  nativeModePayload(
    capabilities.value.value?.send,
    modeChoices.value.find((value) => value.mode === options.mode),
    choices.value.find(
      (value) => value.model === (options.model || nativeModel.value),
    ),
    options.effort || (options.model && options.model !== nativeModel.value ? choices.value.find(value => value.model === options.model)?.defaultEffort ?? '' : nativeEffort.value),
  );
const currentOptions = computed<Options>(() => ({
  model: model.value,
  effort: effort.value,
  mode: mode.value || (modeChoices.value.some(value => value.mode === nativeSettings.value.mode) && turns.value.value?.items.length === 0 && (!nativeMode.value || nativeMode.value === 'default') ? nativeSettings.value.mode : ''),
  permission: permission.value,
}));
const modePayload = computed(() => payloadFor(currentOptions.value));
const resumedThread = computed(() => {
  const saved = action.saved.value;
  return saved?.phase === "succeeded" &&
    saved.call.qualifiedName === "codex.thread/resume" &&
    record(saved.call.arguments).threadId === props.threadId
    ? record(record(saved.result).thread)
    : null;
});
const attached = computed(
  () =>
    !state.error.value &&
    (state.value.value?.attached === true || !!resumedThread.value),
);
const activeTurn = computed(() =>
  turns.value.value?.items.map(record).find((t) => t.status === "inProgress"),
);
const threadStatus = computed(() =>
  text(
    record(resumedThread.value?.status ?? state.value.value?.thread.status)
      .type,
    "unknown",
  ),
);
const archivedOverride = ref<boolean | null>(null);
const archived = computed(
  () =>
    archivedOverride.value ??
    record(taskInventory.value.value?.summary).archived === true,
);
watch(
  () => record(taskInventory.value.value?.summary).archived,
  (value) => {
    if (typeof value === "boolean") archivedOverride.value = value;
  },
);
const taskName = ref(""),
  taskNameDirty = ref(false);
watch(
  () => state.value.value?.thread.name,
  (value) => {
    if (!taskNameDirty.value) taskName.value = text(value);
  },
  { immediate: true },
);
// A background refresh keeps the last owner-confirmed state. The send path performs
// its own immediate preflight read before dispatching, so refresh animation need not
// disable the draft or button.
const knownIdle = computed(
  () => attached.value && ["idle", "systemError"].includes(threadStatus.value),
);
const selectedError = computed(() => {
  const turn = turns.value.value?.items
    .map(record)
    .find((turn) => !selectedTurn.value || turn.id === selectedTurn.value);
  return turn?.status === "failed"
    ? text(record(turn.error).message).slice(0, 4096)
    : "";
});
const optionsValid = (options: Options) =>
  !settings.config.loading.value && !settings.config.error.value &&
  (!options.model || choices.value.some((v) => v.model === options.model)) &&
  (!options.effort ||
    (
      choices.value.find(
        (v) => v.model === (options.model || nativeModel.value),
      )?.efforts ?? []
    ).includes(options.effort)) &&
  (!options.permission ||
    permissionChoices.value.some((profile) => profile.id === options.permission)) &&
  (!options.mode ||
    (!modes.loading.value && !modes.error.value && payloadFor(options) !== null)) &&
  supportsFields(capabilities.value.value?.send, [
    "threadId",
    "input",
    ...(options.mode
      ? ["collaborationMode"]
      : [
          ...(options.model ? ["model"] : []),
          ...(options.effort ? ["effort"] : []),
        ]),
    ...(options.permission ? ["permissions"] : []),
  ]);
const imagesSupported = (binding: BoundTool | undefined) =>
  supportsInputType(binding, "localImage");
const working = computed(
  () => !!activeTurn.value || threadStatus.value === "active",
);
const submitting = ref(false);
const sending = computed(() => submitting.value || action.pending.value && ['codex.turn/start', 'codex.turn/steer', 'codex.thread/resume'].includes(action.saved.value?.call.qualifiedName ?? ''));
const canResume = computed(
  () =>
    !state.error.value &&
    !!state.value.value &&
    !attached.value &&
    threadStatus.value !== "active" &&
    !action.locked.value &&
    supportsFields(capabilities.value.value?.resume, ["threadId"]),
);
// A draft is complete once its files are on the host and its settings are valid; it is queued when sent.
const canSubmit = computed(
  () =>
    (!!message.value.trim() || attachments.staged.value.length > 0) &&
    !attachments.uploading.value &&
    !attachments.failed.value &&
    (!attachments.staged.value.some((value) => value.image) ||
      imagesSupported(capabilities.value.value?.send)) &&
    optionsValid(currentOptions.value) &&
    !action.locked.value &&
    !submitting.value &&
    (knownIdle.value || canResume.value || working.value),
);
const canInterrupt = computed(
  () =>
    attached.value &&
    !!activeTurn.value &&
    !action.locked.value &&
    !(
      action.saved.value?.label === "Request interruption" &&
      action.saved.value.phase === "succeeded" &&
      record(action.saved.value.call.arguments).turnId === activeTurn.value.id
    ) &&
    supportsFields(capabilities.value.value?.interrupt, ["threadId", "turnId"]),
);
const refresh = () => {
  void state.refresh();
  void taskInventory.refresh();
  void turns.refresh();
  void permissions.refresh();
  void goal.refresh();
  void reloadOutput();
};
// Native events arrive in bursts while a turn runs. Coalesce them into one bounded reread of the
// conversation so Hive's per-session request slots are not exhausted by superseded reads.
let snapshotTimer: ReturnType<typeof setInterval> | undefined,
  eventRefreshTimer: ReturnType<typeof setTimeout> | undefined,
  eventRefreshTask = false;
const scheduleRefresh = (task: boolean) => {
  eventRefreshTask ||= task;
  if (eventRefreshTimer) return;
  eventRefreshTimer = setTimeout(() => {
    eventRefreshTimer = undefined;
    if (state.loading.value || goal.loading.value || turns.loading.value) {
      scheduleRefresh(eventRefreshTask);
      return;
    }
    if (eventRefreshTask) {
      void state.refresh();
      void goal.refresh();
    }
    eventRefreshTask = false;
    void turns.refresh();
    void reloadOutput();
  }, 600);
};
const refreshForEvents = (methods: string[]) => {
  if (
    methods.some((method) =>
      ["thread/deleted", "thread/reverted", "thread/compacted"].includes(method),
    )
  ) {
    resetOutputView(false);
    activity.value = [];
  }
  if (
    methods.some(
      (method) =>
        [
          "thread/started",
          "thread/status/changed",
          "thread/archived",
          "thread/deleted",
          "thread/unarchived",
          "thread/closed",
          "thread/reverted",
          "thread/name/updated",
          "thread/goal/updated",
          "thread/goal/cleared",
          "thread/project/updated",
          "thread/settings/updated",
          "thread/compacted",
          "turn/started",
          "turn/completed",
        ].includes(method),
    )
  )
    scheduleRefresh(true);
  else if (
    methods.some((method) =>
      ["item/completed", "serverRequest/resolved"].includes(method),
    )
  )
    scheduleRefresh(false);
};
const resume = async () => {
  if (!canResume.value || !capabilities.value.value?.resume) return;
  await action.start(
    "Attach historical task",
    capabilities.value.value.resume,
    { threadId: props.threadId, excludeTurns: true },
  );
  if (action.saved.value?.phase === "succeeded") await state.refresh();
  refresh();
};
const setQueued = (id: string, change: Partial<Queued>) => {
  queue.value = queue.value.map((item) =>
    item.id === id ? { ...item, ...change } : item,
  );
};
// Links the message being sent to the retained action, whose outcome decides whether it leaves the queue.
let linking: string | null = null;
watch(
  () => action.saved.value?.operationId,
  (id) => {
    if (linking && id) {
      setQueued(linking, { operationId: id });
      linking = null;
    }
  },
  { flush: "sync" },
);
watch(
  () => [action.saved.value?.operationId, action.saved.value?.phase] as const,
  ([id, phase]) => {
    const item = queue.value.find((value) => value.operationId && value.operationId === id);
    if (!item) return;
    if (phase === "succeeded") {
      queue.value = queue.value.filter((value) => value.id !== item.id);
      refresh();
    } else if (phase === "failed")
      setQueued(item.id, {
        operationId: undefined,
        error: action.saved.value?.detail || "The message was not sent.",
      });
  },
  { immediate: true },
);
const startLinked = async (
  item: Queued,
  label: string,
  binding: BoundTool | undefined,
  args: Wire.Json,
) => {
  if (!binding) return;
  linking = item.id;
  setQueued(item.id, { error: undefined });
  await action.start(label, binding, args);
  linking = null;
  const current = queue.value.find((value) => value.id === item.id);
  if (current && !current.operationId && action.error.value)
    setQueued(item.id, { error: action.error.value });
};
const argsFor = (item: Pick<Queued, "text" | "attachments" | "options">): Wire.Json => ({
  threadId: props.threadId,
  input: messageInput(item.text, item.attachments),
  ...(item.options.mode
    ? { collaborationMode: payloadFor(item.options)! }
    : {
        ...(item.options.model ? { model: item.options.model } : {}),
        ...(item.options.effort ? { effort: item.options.effort } : {}),
      }),
  ...(item.options.permission ? { permissions: item.options.permission } : {}),
});
let dispatching = false;
const dispatch = async () => {
  const item = queue.value[0],
    binding = capabilities.value.value?.send;
  if (
    dispatching ||
    !item ||
    item.error ||
    item.operationId ||
    working.value ||
    action.locked.value ||
    !binding
  )
    return;
  dispatching = true;
  try {
    if (!attached.value) {
      if (!canResume.value) return;
      await resume();
      if (!attached.value) return;
    }
    // Re-read immediately before sending without adopting another definition hash.
    await state.refresh();
    if (!knownIdle.value || working.value || queue.value[0]?.id !== item.id) return;
    if (!optionsValid(item.options)) {
      setQueued(item.id, {
        error: "Its model, reasoning or safety choice is no longer available. Edit it to send it.",
      });
      return;
    }
    await startLinked(item, "Send message", binding, argsFor(item));
  } finally {
    dispatching = false;
  }
};
watch(
  [
    () => queue.value[0]?.id,
    () => queue.value[0]?.error,
    working,
    knownIdle,
    () => action.locked.value,
    () => capabilities.value.value?.send,
  ],
  () => void dispatch(),
);
// An idle task receives the draft directly; it stays in the composer until the send is confirmed.
const draft = () => ({
  text: message.value,
  attachments: attachments.staged.value,
  options: { ...currentOptions.value },
});
const sendDraft = async () => {
  if (submitting.value) return;
  submitting.value = true;
  try {
  // The definition and draft checked at submission are sent as they were; a changed provider
  // definition is refused by Hive instead of being adopted silently.
  const binding = capabilities.value.value?.send,
    args = argsFor(draft());
  if (!binding) return;
  if (!attached.value) await resume();
  await state.refresh();
  if (!knownIdle.value) return;
  await action.start("Send message", binding, args);
  refresh();
  } finally {
    submitting.value = false;
  }
};
watch(
  () => action.saved.value?.phase,
  (phase) => {
    const saved = action.saved.value;
    if (
      phase !== "succeeded" ||
      saved?.call.qualifiedName !== "codex.turn/start" ||
      queue.value.some((item) => item.operationId === saved.operationId)
    )
      return;
    if (
      JSON.stringify(record(saved.call.arguments).input) ===
      JSON.stringify(messageInput(message.value, attachments.staged.value))
    ) {
      message.value = "";
      attachments.clear();
    }
  },
);
const submit = () => {
  if (!canSubmit.value) return;
  if (selectedTurn.value) window.location.hash = route('task', { node: props.node, id: props.threadId });
  if (!working.value && !queue.value.length) {
    void sendDraft();
    return;
  }
  queue.value = [
    ...queue.value,
    {
      id: crypto.randomUUID(),
      text: message.value,
      attachments: attachments.staged.value,
      options: { ...currentOptions.value },
    },
  ];
  message.value = "";
  attachments.clear();
  void dispatch();
};
const canSteer = computed(
  () =>
    working.value &&
    !!activeTurn.value &&
    !action.locked.value &&
    supportsFields(capabilities.value.value?.steer, [
      "threadId",
      "expectedTurnId",
      "input",
    ]),
);
const steer = async (id: string) => {
  const item = queue.value.find((value) => value.id === id),
    binding = capabilities.value.value?.steer;
  if (!item || item.operationId || !canSteer.value || !binding || !activeTurn.value) return;
  if (item.attachments.some((value) => value.image) && !imagesSupported(binding)) {
    setQueued(id, { error: "Images cannot steer a running turn on this host." });
    return;
  }
  await startLinked(item, "Steer task", binding, {
    threadId: props.threadId,
    expectedTurnId: text(activeTurn.value.id),
    input: messageInput(item.text, item.attachments),
  });
};
const editQueued = (id: string) => {
  const item = queue.value.find((value) => value.id === id);
  if (!item || item.operationId) return;
  queue.value = queue.value.filter((value) => value.id !== id);
  message.value = [item.text, message.value].filter((value) => value.trim()).join("\n\n");
  attachments.restore(item.attachments);
};
const removeQueued = (id: string) => {
  queue.value = queue.value.filter((value) => value.id !== id || !!value.operationId);
};
const retryQueued = (id: string) => {
  setQueued(id, { error: undefined });
  void dispatch();
};
const queueView = computed(() =>
  queue.value.map((item) => ({
    id: item.id,
    text: item.text,
    attachments: item.attachments.length,
    sending: !!item.operationId,
    ...(item.error ? { error: item.error } : {}),
  })),
);
const onMessageKeydown = (event: KeyboardEvent) =>
  submitOnEnter(event, () => void submit());
const saveGoal = async () => {
  const binding = capabilities.value.value?.goalSet,
    objective = goalDraft.value.trim();
  if (
    !binding ||
    !objective ||
    goalAction.locked.value ||
    !supportsFields(binding, ["threadId", "objective", "status"])
  )
    return;
  await goalAction.start("Set task goal", binding, {
    threadId: props.threadId,
    objective,
    status: "active",
  });
  if (goalAction.saved.value?.phase === "succeeded") {
    goalDirty.value = false;
    goalEditing.value = false;
    await goal.refresh();
  }
};
const clearGoal = async () => {
  const binding = capabilities.value.value?.goalClear;
  if (
    !binding ||
    goalAction.locked.value ||
    !supportsFields(binding, ["threadId"])
  )
    return;
  await goalAction.start("Clear task goal", binding, {
    threadId: props.threadId,
  });
  if (goalAction.saved.value?.phase === "succeeded") {
    goalDraft.value = "";
    goalDirty.value = false;
    goalEditing.value = false;
    await goal.refresh();
  }
};
const interrupt = async () => {
  if (
    !canInterrupt.value ||
    !capabilities.value.value?.interrupt ||
    !activeTurn.value
  )
    return;
  await action.start(
    "Request interruption",
    capabilities.value.value.interrupt,
    { threadId: props.threadId, turnId: activeTurn.value.id! },
  );
  refresh();
};
const rename = async () => {
  const binding = capabilities.value.value?.rename,
    name = taskName.value.trim();
  if (!binding || !name || action.locked.value) return;
  await action.start("Rename task", binding, {
    threadId: props.threadId,
    name,
  });
  if (action.saved.value?.phase === "succeeded") taskNameDirty.value = false;
  refresh();
};
// A Codex task that works on a TaskBoard ticket is archived through its ticket, which also archives
// every Codex task of that ticket; the board then never shows finished work that was archived here.
const ticketArchive = useObjectArchive(
  client,
  "ivy:agent-ticket-archive:" + base.href + props.node + ":" + props.threadId,
);
const owningTicket = async () =>
  (
    await client.request("objects.query", {
      contractKey: "task-board/task",
      where: {
        op: "and",
        args: [
          { op: "eq", field: "data:/primaryResourceRef/serviceNodeId", value: props.node },
          { op: "eq", field: "data:/primaryResourceRef/nativeId", value: props.threadId },
        ],
      },
      includeArchived: true,
      limit: 1,
    })
  ).items[0]?.objectId ?? null;
const setArchived = async () => {
  const wasArchived = archived.value,
    binding = wasArchived
      ? capabilities.value.value?.unarchive
      : capabilities.value.value?.archive;
  if (action.locked.value || ticketArchive.busy.value) return;
  ticketArchive.clearError();
  const ticket = await owningTicket().catch(() => null);
  if (ticket) {
    if (await ticketArchive.run(ticket, !wasArchived))
      archivedOverride.value = !wasArchived;
    refresh();
    return;
  }
  if (!binding) return;
  await action.start(wasArchived ? "Restore task" : "Archive task", binding, {
    threadId: props.threadId,
  });
  if (action.saved.value?.phase === "succeeded")
    archivedOverride.value = !wasArchived;
  refresh();
};
// Deleting removes the native conversation; a TaskBoard ticket keeps its task and is archived instead.
const deleteOpen = ref(false),
  deleteError = ref("");
const deleteTask = async () => {
  const binding = capabilities.value.value?.remove;
  if (!binding || action.locked.value) return;
  deleteError.value = "";
  if (await owningTicket().catch(() => null)) {
    deleteError.value = tr(
      "Dieser Task gehört zu einem TaskBoard-Ticket. Archiviere ihn stattdessen.",
      "This task belongs to a TaskBoard ticket. Archive it instead.",
    );
    return;
  }
  await action.start("Delete task", binding, { threadId: props.threadId });
  if (action.saved.value?.phase === "succeeded") {
    deleteOpen.value = false;
    window.location.hash = route("host", { node: props.node });
  } else
    deleteError.value =
      action.error.value ||
      text(action.saved.value?.detail) ||
      tr("Der Task konnte nicht gelöscht werden.", "The task could not be deleted.");
};
watch(
  () => props.turnId,
  () => {
    // Changing a linked turn must cancel the previous view even while its read is pending.
    resetOutputView(true);
    void refreshOutput();
  },
  { flush: "sync" },
);
const activity = shallowRef<Agent.Notification[]>([]),
  gap = ref(false),
  partialLive = ref(false);
let afterSequence = 0,
  activityEpoch: string | null = null,
  timer: ReturnType<typeof setTimeout> | undefined,
  stopped = false,
  polling = false,
  unsubscribe: (() => void) | undefined,
  stopStatus: (() => void) | undefined,
  stopServiceChanges: (() => void) | undefined,
  resyncRequested = false;
let pollFailures = 0;
let buffered: Agent.Notification[] = [];
const activityAbort = new AbortController();
const retainNotification = (entry: Agent.Notification) => {
  if (activityEpoch && entry.epoch !== activityEpoch) {
    gap.value = true;
    activity.value = [];
    afterSequence = 0;
    scheduleRefresh(true);
  }
  activityEpoch = entry.epoch;
  if (entry.sequence <= afterSequence) return;
  if (afterSequence && entry.sequence !== afterSequence + 1) {
    gap.value = true;
    scheduleRefresh(true);
  }
  afterSequence = entry.sequence;
  if (record(entry.params).threadId !== props.threadId) return;
  if (entry.method === 'thread/settings/updated') latestSettings.value = record(record(entry.params).threadSettings);
  if (activity.value.length >= 100) partialLive.value = true;
  activity.value = [...activity.value, entry].slice(-100);
  refreshForEvents([entry.method]);
};
const receiveProviderNotification = (value: Transport.ProviderNotification) => {
  const payload = record(value.params.payload);
  if (
    typeof payload.sequence !== "number" ||
    typeof payload.epoch !== "string" ||
    typeof payload.method !== "string" ||
    typeof payload.observedAt !== "string"
  )
    return;
  if (document.hidden) return;
  if (polling)
    buffered = [...buffered, payload as Agent.Notification].slice(-100);
  else retainNotification(payload as Agent.Notification);
};
const poll = async () => {
  if (stopped) return;
  if (polling) {
    resyncRequested = true;
    return;
  }
  if (document.hidden) return;
  polling = true;
  clearTimeout(timer);
  resyncRequested = false;
  let nextPollMs: number | null = notifications.connected ? null : 1000;
  try {
    if (activityEpoch === null && afterSequence === 0) {
      // A future cursor returns the journal's actual high-water mark with gap=true. Bootstrap a
      // bounded recent tail instead of spending minutes paging through unrelated old activity.
      const head = (await nativeRead(
        client,
        props.node,
        "agent.notifications",
        { afterSequence: Number.MAX_SAFE_INTEGER, limit: 1 },
        activityAbort.signal,
      )) as Agent.NotificationPage;
      afterSequence = Math.max(0, head.throughSequence - 100);
      activityEpoch = head.epoch;
      partialLive.value = afterSequence > 0;
    }
    const page = (await nativeRead(
      client,
      props.node,
      "agent.notifications",
      { afterSequence, limit: 100 },
      activityAbort.signal,
    )) as Agent.NotificationPage;
    if (stopped) return;
    pollFailures = 0;
    if ((activityEpoch && page.epoch !== activityEpoch) || page.gap) {
      gap.value = true;
      scheduleRefresh(true);
    }
    activityEpoch = page.epoch;
    const relevant = page.items.filter(
      (item) =>
        item.sequence > afterSequence &&
        record(item.params).threadId === props.threadId,
    );
    if (activity.value.length + relevant.length > 100) partialLive.value = true;
    activity.value = [...activity.value, ...relevant].slice(-100);
    afterSequence = Math.max(afterSequence, page.throughSequence);
    if (page.hasMore) nextPollMs = 0;
    refreshForEvents(relevant.map((item) => item.method));
  } catch {
    // Saved history remains authoritative; the next bounded poll retries live updates.
    nextPollMs = Math.min(30000, 2000 * 2 ** Math.min(pollFailures++, 4));
  } finally {
    polling = false;
    if (nextPollMs !== 0) {
      if (!stopped) for (const entry of buffered) retainNotification(entry);
      buffered = [];
    }
    if (resyncRequested && !pollFailures) nextPollMs = 0;
    if (!stopped && nextPollMs !== null)
      timer = setTimeout(() => void poll(), nextPollMs);
  }
};
const visibilityChanged = () => {
  if (document.hidden) return;
  clearTimeout(timer);
  scheduleRefresh(true);
  void poll();
};
onMounted(() => {
  void refreshOutput();
  // Journal polling stays fast; full snapshots only retry slowly when needed.
  snapshotTimer = setInterval(() => {
    if (
      !document.hidden &&
      !state.loading.value &&
      !goal.loading.value &&
      !turns.loading.value &&
      !outputLoading.value &&
      !outputLoadingEarlier.value &&
      (!notifications.connected ||
        state.error.value ||
        goal.error.value ||
        turns.error.value ||
        outputError.value)
    )
      scheduleRefresh(true);
  }, 15000);
  unsubscribe = notifications.subscribe(
    {
      namespace: "agent",
      name: "notification",
      version: "1.0.0",
      serviceNodeId: props.node,
    },
    receiveProviderNotification,
  );
  stopStatus = notifications.onStatus((ready) => {
    clearTimeout(timer);
    if (ready) {
      scheduleRefresh(true);
      void poll();
    } else timer = setTimeout(() => void poll(), 1000);
  });
  stopServiceChanges = notifications.subscribeChanges(["services/agent-manager/" + props.node], () => {
    scheduleRefresh(true);
    void poll();
  });
  document.addEventListener("visibilitychange", visibilityChanged);
  void poll();
});
onBeforeUnmount(() => {
  stopped = true;
  outputSerial++;
  outputController?.abort();
  unsubscribe?.();
  stopStatus?.();
  stopServiceChanges?.();
  activityAbort.abort();
  document.removeEventListener("visibilitychange", visibilityChanged);
  clearTimeout(timer);
  clearInterval(snapshotTimer);
  clearTimeout(eventRefreshTimer);
});
const displayItems = computed(() => {
  const pages = outputPages.value.filter((page) => page.mode !== "search");
  let entries: NativeOutputPage["items"];
  if (pages[0]?.mode === "items")
    entries = pages.flatMap((page) => page.items).reverse();
  else {
    const groups: Array<{ turnId: string; items: NativeOutputPage["items"] }> =
      [];
    for (const entry of pages.flatMap((page) => page.items)) {
      const turnId = text(record(entry).turnId);
      const group = groups.at(-1);
      if (group?.turnId === turnId) group.items.push(entry);
      else groups.push({ turnId, items: [entry] });
    }
    entries = groups.reverse().flatMap((group) => group.items);
  }
  return entries
    .map((entry) => ({
      entry,
      presentation: conversationItem(record(entry).item),
    }))
    .filter(({ presentation }) => presentation.kind !== "hidden");
});
const savedIds = computed(
  () =>
    new Set(
      displayItems.value.map(({ entry }) =>
        text(record(record(entry).item).id),
      ),
    ),
);
const liveTurnId = computed(
  () =>
    selectedTurn.value ||
    text(activeTurn.value?.id) ||
    [...activity.value]
      .reverse()
      .map((entry) => text(record(entry.params).turnId))
      .find(Boolean) ||
    "",
);
const live = computed(() =>
  liveTurnId.value
    ? liveMessages(activity.value, savedIds.value, liveTurnId.value)
    : [],
);
type ConversationBlock =
  | { type: "message"; key: string; presentation: ConversationItem }
  | {
      type: "steps";
      key: string;
      steps: ConversationStep[];
      summary: string;
      running: ConversationStep | undefined;
    };
const pendingSteps = computed(() =>
  liveTurnId.value
    ? liveSteps(activity.value, savedIds.value, liveTurnId.value)
    : [],
);
// Like Codex Desktop, consecutive tool steps collapse into one summary between messages.
const blocks = computed(() => {
  const result: ConversationBlock[] = [];
  const add = (step: ConversationStep) => {
    const last = result.at(-1);
    if (step.presentation.kind === "message")
      result.push({
        type: "message",
        key: step.id,
        presentation: step.presentation,
      });
    else if (last?.type === "steps") last.steps.push(step);
    else
      result.push({
        type: "steps",
        key: "steps:" + step.id,
        steps: [step],
        summary: "",
        running: undefined,
      });
  };
  displayItems.value.forEach(({ entry, presentation }, index) =>
    add({
      id: itemId(entry) || "saved:" + index,
      presentation,
      running: false,
    }),
  );
  pendingSteps.value.forEach(add);
  for (const block of result)
    if (block.type === "steps") {
      block.summary = activitySummary(block.steps);
      block.running = block.steps.findLast((step) => step.running);
    }
  return result;
});
const requests = ref<HTMLElement>(),
  requestCount = ref(0);
const requestAttention = (count: number) => {
  requestCount.value = count;
  if (count && atBottom.value) void scrollBottom();
};
const showRequests = () => requests.value?.scrollIntoView({ block: "center" });
const scroller = ref<HTMLElement>(),
  taskRoot = ref<HTMLElement>(),
  atBottom = ref(true);
const scrollKey = () =>
  "ivy.agent.scroll:" +
  base.href +
  props.node +
  ":" +
  props.threadId +
  ":" +
  props.turnId;
let restoreTop: number | null = null,
  scrollTimer: ReturnType<typeof setTimeout> | undefined;
const restoreScroll = () => {
  restoreTop = null;
  atBottom.value = true;
  try {
    const saved = JSON.parse(sessionStorage.getItem(scrollKey()) ?? "null");
    if (
      saved &&
      !saved.bottom &&
      Number.isFinite(saved.top) &&
      saved.top >= 0
    ) {
      restoreTop = saved.top;
      atBottom.value = false;
    }
  } catch {
    /* View state is optional. */
  }
};
restoreScroll();
watch(() => props.turnId, restoreScroll);
const retainScroll = () => {
  try {
    sessionStorage.setItem(
      scrollKey(),
      JSON.stringify({
        top: restoreTop ?? scroller.value?.scrollTop ?? 0,
        bottom: atBottom.value,
      }),
    );
  } catch {
    /* View state is optional. */
  }
};
const trackScroll = () => {
  const element = scroller.value;
  if (!element || restoreTop !== null) return;
  if (
    element.scrollTop < 180 &&
    outputNextCursor.value &&
    !outputLoadingEarlier.value
  )
    void loadEarlierOutput();
  atBottom.value =
    element.scrollHeight - element.scrollTop - element.clientHeight < 100;
  clearTimeout(scrollTimer);
  scrollTimer = setTimeout(retainScroll, 150);
};
const scrollBottom = async () => {
  await nextTick();
  scroller.value?.scrollTo({ top: scroller.value.scrollHeight });
  atBottom.value = true;
};
watch(
  [
    () => displayItems.value,
    () => live.value.map((item) => item.text).join(""),
  ],
  async () => {
    if (outputLoadingEarlier.value) return;
    await nextTick();
    if (restoreTop !== null && outputPages.value.length && scroller.value) {
      scroller.value.scrollTop = restoreTop;
      restoreTop = null;
    } else if (atBottom.value) await scrollBottom();
  },
);
const viewportChanged = () => {
  const viewport = window.visualViewport;
  if (taskRoot.value && viewport)
    taskRoot.value.style.setProperty(
      "--conversation-height",
      Math.max(260, viewport.height - 48) + "px",
    );
};
onMounted(() => {
  viewportChanged();
  if (outputPages.value.length) {
    if (restoreTop !== null && scroller.value) {
      scroller.value.scrollTop = restoreTop;
      restoreTop = null;
    } else if (atBottom.value) void scrollBottom();
  }
  window.visualViewport?.addEventListener("resize", viewportChanged);
  window.addEventListener("pagehide", retainScroll);
});
onBeforeUnmount(() => {
  window.visualViewport?.removeEventListener("resize", viewportChanged);
  window.removeEventListener("pagehide", retainScroll);
  clearTimeout(scrollTimer);
  retainScroll();
});
</script>
<template>
  <div ref="taskRoot" class="agent-task">
    <ToolbarContent>
      <span class="text-muted-foreground" aria-hidden="true">/</span>
      <h1 class="min-w-0 truncate text-sm font-medium">
        {{
          text(state.value.value?.thread.name) ||
          text(state.value.value?.thread.preview).slice(0, 100) ||
          "Native task"
        }}
      </h1>
      <ActivityIndicator v-if="working" state="working" />
    </ToolbarContent>
    <ToolbarContent side="end">
      <DropdownMenu>
        <DropdownMenuTrigger as-child>
          <Button variant="ghost" size="icon-sm" aria-label="Task actions"
            ><MoreHorizontal aria-hidden="true"
          /></Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" class="w-72">
          <div class="space-y-2 p-2">
            <Label for="task-name">Task name</Label>
            <div class="flex gap-2">
              <Input
                id="task-name"
                v-model="taskName"
                :disabled="
                  action.locked.value || !capabilities.value.value?.rename
                "
                maxlength="512"
                @input="taskNameDirty = true"
                @keydown.stop
              />
              <Button
                variant="outline"
                :disabled="
                  action.locked.value ||
                  !taskName.trim() ||
                  !taskNameDirty ||
                  !capabilities.value.value?.rename
                "
                @click="rename"
                >Rename</Button
              >
            </div>
          </div>
          <DropdownMenuSeparator />
          <DropdownMenuItem
            v-if="supportsFields(capabilities.value.value?.project, ['threadId', 'projectId'])"
            :disabled="action.locked.value || working || !state.value.value"
            @select="projectOpen = true"
          >
            <FolderOpen aria-hidden="true" />{{ tr('Projekt ändern…', 'Change project…') }}
          </DropdownMenuItem>
          <DropdownMenuItem
            :disabled="
              action.locked.value ||
              !(archived
                ? capabilities.value.value?.unarchive
                : capabilities.value.value?.archive)
            "
            @select="setArchived"
          >
            <Archive aria-hidden="true" />{{ archived ? "Restore" : "Archive" }}
          </DropdownMenuItem>
          <DropdownMenuItem
            v-if="outputDownload"
            aria-label="Download complete turn"
            :disabled="outputLoading || !!outputError"
            @select="downloadTurn"
          >
            <Download aria-hidden="true" />Download turn
          </DropdownMenuItem>
          <template v-if="capabilities.value.value?.remove">
            <DropdownMenuSeparator />
            <DropdownMenuItem
              variant="destructive"
              :disabled="action.locked.value || working"
              @select="deleteError = ''; deleteOpen = true"
            >
              <Trash2 aria-hidden="true" />{{ tr("Löschen…", "Delete…") }}
            </DropdownMenuItem>
          </template>
        </DropdownMenuContent>
      </DropdownMenu>
    </ToolbarContent>
    <Dialog v-model:open="deleteOpen">
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{{ tr("Task löschen?", "Delete task?") }}</DialogTitle>
          <DialogDescription>{{
            tr(
              "Die Unterhaltung wird dauerhaft aus Codex entfernt. Dateien in ihrem Arbeitsverzeichnis bleiben erhalten.",
              "The conversation is permanently removed from Codex. Files in its working directory are kept.",
            )
          }}</DialogDescription>
        </DialogHeader>
        <p v-if="deleteError" role="alert" class="text-sm text-destructive">
          {{ deleteError }}
        </p>
        <DialogFooter>
          <Button variant="outline" :disabled="action.locked.value" @click="deleteOpen = false">{{ tr("Abbrechen", "Cancel") }}</Button>
          <Button variant="destructive" :disabled="action.locked.value" :loading="action.busy.value" @click="deleteTask">{{ tr("Löschen", "Delete") }}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
    <TaskProjectDialog
      v-if="projectOpen && capabilities.value.value?.project"
      v-model:open="projectOpen"
      :client="client"
      :node="node"
      :thread-id="threadId"
      :project-id="text(state.value.value?.thread.projectId)"
      :binding="capabilities.value.value.project"
      :scope="base.href"
      @changed="refresh"
    />
    <div ref="scroller" class="agent-messages" @scroll="trackScroll">
      <div class="agent-message-column ivy-conversation">
        <RemoteState
          :loading="state.loading.value"
          :error="state.error.value"
          :has-data="!!state.value.value"
          @retry="state.refresh"
        />
        <p v-if="attached" class="sr-only">
          Direct input confirmed on this native connection
        </p>
        <p
          v-if="threadStatus === 'systemError'"
          class="rounded-lg bg-destructive/10 px-3 py-2 text-sm text-destructive"
        >
          The previous turn failed. You can send a new message after addressing
          the cause.
        </p>
        <section class="space-y-4" aria-labelledby="results-heading">
          <h2 id="results-heading" class="sr-only">Conversation</h2>
          <p
            v-if="selectedTurn && (turns.value.value?.nextCursor || turns.value.value?.items.some(turn => record(turn).id !== selectedTurn))"
            class="rounded-lg bg-muted px-3 py-2 text-sm text-muted-foreground"
          >
            Showing a linked turn.
            <a :href="route('task', { node, id: threadId })" class="underline"
              >Show the complete conversation</a
            >
          </p>
          <p
            v-if="selectedError"
            role="alert"
            class="whitespace-pre-wrap break-words rounded-lg border border-destructive p-4 text-sm"
          >
            Native turn failed: {{ selectedError }}
          </p>
          <RemoteState
            :loading="outputLoading"
            :error="outputError || turns.error.value"
            :has-data="!!outputPages.length"
            :empty="!displayItems.length && !outputNextCursor"
            empty-title="No saved output yet"
            @retry="refreshOutput"
          />
          <p
            v-if="outputLoadingEarlier"
            class="py-2 text-center text-xs text-muted-foreground"
            role="status"
          >
            Loading earlier messages…
          </p>
          <div
            class="space-y-6"
            tabindex="0"
            role="region"
            aria-label="Saved native output"
          >
            <template v-for="block in blocks" :key="block.key">
              <article
                v-if="block.type === 'message'"
                class="conversation-entry"
                :class="{ 'user-message': block.presentation.label === 'You' }"
              >
                <MessageImages
                  v-if="block.presentation.images?.length"
                  :client="client"
                  :node="node"
                  :images="block.presentation.images"
                />
                <ContentView
                  v-if="block.presentation.text"
                  :text="block.presentation.text"
                  :image-loader="imageLoader"
                  :media-type="
                    block.presentation.markdown ? 'text/markdown' : 'text/plain'
                  "
                />
              </article>
              <Collapsible v-else class="activity-group text-sm">
                <CollapsibleTrigger
                  class="group flex max-w-full min-w-0 items-center gap-1.5 rounded-md py-0.5 text-left text-muted-foreground hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-hidden"
                >
                  <span class="min-w-0 truncate">{{ block.summary }}</span>
                  <ChevronRight
                    class="size-3.5 shrink-0 transition-transform group-data-[state=open]:rotate-90"
                    aria-hidden="true"
                  />
                </CollapsibleTrigger>
                <CollapsibleContent>
                  <ul
                    class="activity-steps mt-1.5 max-h-72 space-y-1 overflow-y-auto"
                    :aria-label="block.summary"
                  >
                    <li
                      v-for="step in block.steps"
                      :key="step.id"
                      class="flex min-w-0 items-baseline gap-2"
                    >
                      <span class="shrink-0 font-medium text-muted-foreground">{{
                        step.presentation.label
                      }}</span>
                      <span
                        v-if="step.presentation.text"
                        class="min-w-0 truncate text-muted-foreground"
                        :title="step.presentation.text"
                        >{{ step.presentation.text }}</span
                      >
                      <LoaderCircle
                        v-if="step.running"
                        class="size-3 shrink-0 animate-spin self-center text-muted-foreground"
                        aria-label="Running"
                      />
                    </li>
                  </ul>
                </CollapsibleContent>
                <p
                  v-if="block.running"
                  class="running-step mt-1 flex min-w-0 items-center gap-2 text-muted-foreground"
                  role="status"
                >
                  <LoaderCircle
                    class="size-3 shrink-0 animate-spin"
                    aria-hidden="true"
                  />
                  <span class="min-w-0 truncate" :title="block.running.presentation.text"
                    >{{ block.running.presentation.label
                    }}{{
                      block.running.presentation.text
                        ? " · " + block.running.presentation.text
                        : ""
                    }}</span
                  >
                </p>
              </Collapsible>
            </template>
            <article
              v-for="entry in live"
              :key="entry.id"
              class="space-y-2"
              aria-label="Live answer"
            >
              <p
                v-if="gap || partialLive"
                class="text-xs text-muted-foreground"
              >
                Partial live answer
              </p>
              <ContentView :text="entry.text" media-type="text/markdown" :image-loader="imageLoader" />
            </article>
          </div>
          <p
            v-if="working"
            role="status"
            class="flex items-center gap-2 py-2 text-sm text-muted-foreground"
          >
            <LoaderCircle class="size-4 animate-spin" aria-hidden="true" />
            Working…
          </p>
        </section>
        <div ref="requests">
          <NativeInputs
            hide-empty
            retain-drafts
            :base="base"
            :client="client"
            :notifications="notifications"
            :node="node"
            :thread-id="threadId"
            @attention="requestAttention"
          />
        </div>
      </div>
    </div>
    <div class="agent-input-area">
      <Button
        v-if="requestCount"
        variant="outline"
        size="sm"
        class="mx-auto mb-2"
        @click="showRequests"
        >{{ requestCount }}
        {{ requestCount === 1 ? "request needs" : "requests need" }} your
        attention</Button
      ><Button
        v-if="!atBottom"
        variant="outline"
        size="sm"
        class="mx-auto mb-2 flex rounded-full"
        @click="scrollBottom"
        ><ArrowDown aria-hidden="true" />Latest messages</Button
      >
      <ComposerQueue
        :items="queueView"
        :can-steer="canSteer"
        :disabled="action.locked.value"
        @steer="steer"
        @retry="retryQueued"
        @edit="editQueued"
        @remove="removeQueued"
      />
      <ConversationComposer
        class="agent-composer space-y-1"
        aria-labelledby="message-heading"
        accept-files
        @files="attachments.add"
      >
        <h2 id="message-heading" class="sr-only">Continue this task</h2>
        <ComposerAttachments
          :items="attachments.items.value"
          @remove="attachments.remove"
        />
        <Label for="message" class="sr-only">Message</Label
        ><Textarea
          id="message"
          v-model="message"
          :maxlength="131072"
          class="max-h-40 min-h-12 resize-none border-0 bg-transparent px-2 py-2 text-base shadow-none focus-visible:ring-0 dark:bg-transparent"
          :placeholder="working ? 'Queue a message for this task…' : 'Message this task…'"
          @keydown="onMessageKeydown"
        />
        <div v-if="goalEditing" class="goal-editor flex items-center gap-2">
          <Target
            class="size-4 shrink-0 text-muted-foreground"
            aria-hidden="true"
          />
          <Label for="task-goal" class="sr-only">Goal</Label>
          <Input
            id="task-goal"
            v-model="goalDraft"
            class="h-9 min-w-0 flex-1 border-0 bg-transparent px-1 shadow-none focus-visible:ring-0 dark:bg-transparent"
            :disabled="goalAction.locked.value"
            placeholder="What outcome should this task keep working toward?"
            @input="goalDirty = true"
            @keydown.enter.prevent="saveGoal"
          />
          <Button
            size="sm"
            :disabled="!goalDraft.trim() || goalAction.locked.value"
            @click="saveGoal"
            >Save goal</Button
          >
          <Button
            v-if="goalRecord.objective"
            variant="ghost"
            size="icon-sm"
            :disabled="goalAction.locked.value"
            aria-label="Clear goal"
            @click="clearGoal"
            ><X aria-hidden="true"
          /></Button>
        </div>
        <p v-if="mode && !effectiveModel" class="px-1 text-sm text-muted-foreground">
          Choose a model to change the working mode.
        </p>
        <p
          v-else-if="mode && !modePayload"
          class="px-1 text-sm text-muted-foreground"
        >
          This working mode, model and reasoning choice is not supported by the
          current native capability.
        </p>
        <p v-if="modes.error.value" class="px-1 text-sm text-muted-foreground">
          Working modes unavailable: {{ modes.error.value }}
        </p>
        <p v-if="models.error.value" class="px-1 text-sm text-muted-foreground">
          Model capabilities unavailable: {{ models.error.value }}
        </p>
        <p v-if="permissions.error.value || settings.config.error.value" class="px-1 text-sm text-muted-foreground">{{ permissions.error.value || settings.config.error.value }}</p>
        <Button v-if="models.error.value || modes.error.value || permissions.error.value || settings.config.error.value" variant="ghost" size="sm" @click="settings.refresh">Reload settings</Button>
        <p
          v-if="
            capabilities.error.value ||
            (capabilities.value.value && !capabilities.value.value.send)
          "
          class="px-1 text-sm text-muted-foreground"
        >
          This provider cannot offer the text input action.
          {{ capabilities.error.value }}
        </p>
        <p
          v-if="
            attachments.staged.value.some((value) => value.image) &&
            capabilities.value.value?.send &&
            !supportsInputType(capabilities.value.value.send, 'localImage')
          "
          class="px-1 text-sm text-muted-foreground"
        >
          This Codex version cannot receive images. Remove them to send the message.
        </p>
        <div
          v-if="state.value.value && !attached && !working"
          class="flex items-center gap-1 px-1 text-xs text-muted-foreground"
        >
          <Button
            variant="link"
            size="sm"
            aria-label="Attach historical task"
            class="h-auto px-0 text-xs text-foreground"
            :disabled="!canResume"
            @click="resume"
          >
            Attach task
          </Button>
          <span>to continue this conversation.</span>
        </div>
        <div class="composer-toolbar flex items-center gap-1">
          <div class="flex min-w-0 flex-1 flex-wrap items-center gap-0.5">
            <ComposerAddMenu files @files="attachments.add">
              <DropdownMenuItem
                :disabled="!capabilities.value.value?.goalSet"
                @select="goalEditing = true"
                ><Target aria-hidden="true" />{{
                  goalRecord.objective ? "Edit goal" : "Set goal"
                }}</DropdownMenuItem
              >
            </ComposerAddMenu>
            <Button
              v-if="goalRecord.objective"
              variant="ghost"
              size="sm"
              class="composer-pill"
              :aria-expanded="goalEditing"
              :title="text(goalRecord.objective)"
              @click="goalEditing = !goalEditing"
              ><Target aria-hidden="true" />Goal</Button
            >
            <Label for="task-model" class="sr-only">Model</Label
            ><OptionSelect id="task-model" v-model="model" :disabled="models.loading.value && !models.value.value">
              <option value="">
                {{ modelLabel }}
              </option>
              <option
                v-for="item in choices.filter((value) => !value.hidden)"
                :key="item.id"
                :value="item.model"
              >
                {{ item.name }}
              </option>
              <optgroup
                v-if="choices.some((value) => value.hidden)"
                label="More models"
              >
                <option
                  v-for="item in choices.filter((value) => value.hidden)"
                  :key="item.id"
                  :value="item.model"
                >
                  {{ item.name }}
                </option>
              </optgroup></OptionSelect
            >
            <Label for="task-mode" class="sr-only">Working mode</Label>
            <OptionSelect id="task-mode" v-model="mode" :disabled="modes.loading.value && !modes.value.value">
              <option value="">{{ modeLabel }}</option>
              <option
                v-if="
                  mode &&
                  !modeChoices.some((choice) => choice.mode === mode)
                "
                :value="mode"
              >
                Saved mode · {{ mode }} (unavailable)
              </option>
              <option
                v-for="choice in modeChoices"
                :key="choice.mode"
                :value="choice.mode"
              >
                {{ choice.name }}
              </option>
            </OptionSelect>
            <Label for="task-effort" class="sr-only">Reasoning effort</Label>
            <OptionSelect
              id="task-effort"
              v-model="effort"
              :disabled="!effectiveModel"
            >
              <option value="">
                {{
                  nativeEffort
                    ? nativeEffort.charAt(0).toUpperCase() + nativeEffort.slice(1)
                    : (efforts.length ? 'Loading effort…' : 'No reasoning effort')
                }}
              </option>
              <option v-for="value in efforts" :key="value" :value="value">
                {{ value.charAt(0).toUpperCase() + value.slice(1) }}
              </option>
            </OptionSelect>
            <Label for="task-permission" class="sr-only">Safety</Label>
            <OptionSelect
              id="task-permission"
              v-model="permission"
              :disabled="permissions.loading.value"
            >
              <option value="">{{ nativeSettings.permission ? permissionProfileLabel(nativeSettings.permission) : 'Loading safety…' }}</option>
              <option
                v-for="profile in permissionChoices"
                :key="profile.id"
                :value="profile.id"
              >
                {{ permissionProfileLabel(profile.id) }}
              </option>
            </OptionSelect>
          </div>
          <Button
            v-if="working"
            variant="outline"
            size="icon"
            class="size-9 shrink-0 rounded-full"
            :disabled="!canInterrupt"
            :loading="action.pending.value && action.saved.value?.call.qualifiedName === 'codex.turn/interrupt'"
            aria-label="Request interruption"
            @click="interrupt"
            ><Square class="size-3.5 fill-current" aria-hidden="true" /></Button
          ><Button
            v-if="!working || message.trim() || attachments.items.value.length || sending"
            size="icon"
            class="size-9 shrink-0 rounded-full"
            :disabled="!canSubmit"
            :loading="sending"
            :aria-label="sending ? 'Send message' : working || queue.length ? 'Queue message' : 'Send message'"
            @click="submit"
            ><ArrowUp aria-hidden="true"
          /></Button>
        </div>
        <NativeActionState
          v-if="
            goalAction.saved.value?.phase !== 'succeeded' ||
            goalAction.error.value
          "
          :action="goalAction.saved.value"
          :busy="goalAction.busy.value"
          :error="goalAction.error.value"
          @reconcile="goalAction.reconcile"
          @retry="goalAction.retry"
        />
        <NativeActionState
          v-if="action.saved.value?.phase !== 'succeeded' || action.error.value"
          :action="action.saved.value"
          :busy="action.busy.value"
          :error="action.error.value"
          @reconcile="
            async () => {
              await action.reconcile();
              refresh();
            }
          "
          @retry="
            async () => {
              await action.retry();
              refresh();
            }
          "
        />
      </ConversationComposer>
    </div>
  </div>
</template>

<style scoped>
.agent-task {
  height: calc(var(--conversation-height, calc(100dvh - 3rem)) - var(--ivy-install-banner-height, 0px));
  display: flex;
  min-height: 0;
  flex-direction: column;
}
.agent-messages {
  flex: 1 1 0;
  min-height: 0;
  overflow-y: auto;
  overscroll-behavior: contain;
  padding: 1.5rem 1rem;
  scroll-padding-bottom: 1.5rem;
}
.agent-message-column,
.agent-input-area {
  width: 100%;
  max-width: 48rem;
  margin-inline: auto;
}
.agent-message-column {
  display: flex;
  flex-direction: column;
  gap: 1.5rem;
}
.agent-input-area {
  flex: 0 1 auto;
  display: flex;
  flex-direction: column;
  min-height: 0;
  padding: 0.5rem 1rem max(0.75rem, env(safe-area-inset-bottom));
  max-height: 60%;
}
.agent-composer {
  min-height: 0;
  overflow-y: auto;
}
.composer-pill {
  height: 1.75rem;
  gap: 0.25rem;
  border-radius: 999px;
  padding-inline: 0.625rem;
  color: var(--muted-foreground);
  font-size: 0.75rem;
  font-weight: 400;
}
.composer-pill:hover,
.composer-pill[aria-expanded="true"] {
  background: var(--muted);
  color: var(--foreground);
}
.goal-editor {
  border-radius: 0.75rem;
  background: var(--muted);
  padding: 0.25rem 0.375rem 0.25rem 0.75rem;
}
.user-message {
  width: fit-content;
  max-width: 85%;
  margin-left: auto;
  border-radius: 1.5rem;
  background: var(--muted);
  padding: 0.625rem 1.125rem;
}
.activity-steps {
  border-left: 2px solid var(--border);
  padding-left: 0.75rem;
}
.running-step {
  opacity: 0.75;
}
@media (max-width: 640px) {
  .composer-toolbar :deep([data-slot="select-trigger"]) {
    max-width: 8rem;
  }
}
@media (min-width: 768px) {
  .agent-messages {
    padding: 2rem;
  }
}
</style>
