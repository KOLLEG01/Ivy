<script setup lang="ts">
import { computed, ref, watch } from "vue";
import {
  Alert,
  AlertDescription,
  AlertTitle,
  Button,
  Checkbox,
  CommentCard,
  ContentView,
  Disclosure,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  Label,
  LazyMarkdownEditor as MarkdownEditor,
  PageControls,
  PropertyItem,
  PropertyList,
  RemoteState,
  SheetClose,
  StatusBadge,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
  ToolbarContent,
  useRemote,
} from "@ivy/ui";
import type { UploadedAttachment } from "@ivy/ui";
import {
  ArrowLeft,
  Bot,
  CircleAlert,
  ExternalLink,
  Maximize2,
  MoreHorizontal,
  Pencil,
  User,
  X,
} from "@lucide/vue";
import { route } from "../../../packages/ui-client/src/runtime";
import {
  base,
  client,
  readDocument,
  statusTone,
  taskRoute,
  taskUrl,
  tr,
} from "./runtime";
import {
  taskContext,
  transitions,
  conditionLabel,
  conditionTone,
  noteworthyCondition,
  priorityLabel,
  stateColors,
  stateIcons,
  stateLabel,
} from "./board";
import type { TaskBoard } from "./runtime";
import { useAction } from "./action";
import type { DraftRequest } from "./action";
import ActionState from "./ActionState.vue";
import TaskEditor from "./TaskEditor.vue";
import { useExecutionDefaults } from "./execution-defaults";
import TaskHistory from "./TaskHistory.vue";
import NativeInput from "../../../packages/ui-client/src/NativeInput.vue";
import TaskAttachment from "./TaskAttachment.vue";
import { attachmentBytes, attachmentSource, attachmentUpload, saveBytes, sourceAttachmentId, useAttachmentImages } from "./attachment-images";
import { attachmentView } from "./attachment-view";
import WikiLinkPicker from "./WikiLinkPicker.vue";
import { useObjectArchive } from "../../../packages/ui-client/src/object-archive";
import ObjectDeleteDialog from "../../../packages/ui-client/src/ObjectDeleteDialog.vue";
import { nativeRead } from "../../../packages/ui-client/src/native";
import {
  callBound,
  discover,
  newOperationId,
} from "../../../packages/sdk/src/client.js";
import { hostProject } from "../../../packages/sdk/src/project-inventory.js";

const props = defineProps<{
  taskId: string;
  workspace: TaskBoard.WorkspaceInfo;
  available: boolean;
  query: URLSearchParams;
  sheet?: boolean;
}>();
const emit = defineEmits<{ changed: []; deleted: [] }>();
const task = useRemote((signal) =>
    readDocument("task", props.taskId, props.workspace.rootObjectId, signal), 15000, ["objects/task-board/task"]);
const executionDefaults = useExecutionDefaults(
  () => props.workspace,
  () => task.value.value?.value,
);
const projectHost = computed(() => {
  const current = task.value.value?.value;
  if (current?.claim) return current.claim.target.hostId;
  const target =
    current?.fields.executionRequirement ??
    executionDefaults.defaults.value?.executionRequirement;
  return target?.kind === "host" ? target.hostId : undefined;
});
const project = useRemote(
  async (signal) => {
    const requirement = task.value.value?.value.fields.workspaceRequirement;
    const hostId = projectHost.value;
    return requirement?.kind === "existing_project"
      ? {
          hostId,
          project: await hostProject(client, hostId, requirement.projectId, {
            signal,
          }),
        }
      : null;
  },
  30000,
  ["inventory"],
);
watch(
  () =>
    JSON.stringify([
      projectHost.value,
      task.value.value?.value.fields.workspaceRequirement,
    ]),
  () => void project.refresh(),
);
const inherited = (
  value: string | null | undefined,
  fallback: string | null | undefined,
  nativeDefault: string,
) => value ?? `${fallback ?? nativeDefault} (default)`;
const taskView = useRemote((signal) =>
    nativeRead(
      client,
      props.workspace.serviceNodeId,
      "task-board.read",
      { task: props.taskId },
      signal,
    ) as Promise<TaskBoard.TaskView>, 15000, ["objects/task-board", "services"]);
const action = useAction(() => props.workspace, "task:" + props.taskId);
const panel = ref(""),
  deleteOpen = ref(false),
  error = ref<string | null>(null),
  moveToTodo = ref(false),
  commentBody = ref("");
const pendingAttachments = ref<TaskBoard.TaskAttachment[]>([]),
  uploading = ref(false),
  attachmentsOpen = ref(false),
  descriptionExpanded = ref(false),
  commentInput = ref<{ insertMarkdown: (markdown: string) => void }>();
const terminal = computed(() =>
  ["done", "cancelled"].includes(task.value.value?.value.workflowState ?? ""),
);
const descriptionNeedsCollapse = computed(() => {
  const description = task.value.value?.value.fields.description ?? "";
  return description.length > 700 || description.split("\n").length > 7;
});
const archive = useObjectArchive(
  client,
  "ivy:task-board:archive:" + base.href + ":" + props.taskId,
);
const writable = computed(
  () =>
    props.available &&
    props.workspace.role === "user" &&
    !task.error.value &&
    !task.value.value?.read.object.effectivelyArchived &&
    !task.value.value?.value.publication &&
    !archive.busy.value && !archive.pending.value &&
    !action.locked.value,
);
const canDelete = computed(() =>
  props.available && props.workspace.role === "user" &&
  !!task.value.value && !task.value.value.value.claim &&
  !task.value.value.value.publication && !action.locked.value &&
  !archive.busy.value && !archive.pending.value,
);
// The agent task that carries this ticket's native conversation, when one exists.
const agentTaskLink = computed(() => {
  const ref = task.value.value?.value.primaryResourceRef;
  return ref
    ? new URL(
        "agents/#/task?" +
          new URLSearchParams({ node: ref.serviceNodeId, id: ref.nativeId }),
        base,
      ).href
    : null;
});
const nativeWorkAvailable = computed(() => props.workspace.role === 'user' && props.workspace.scheduler.enabled);
const unresolvedApprovals = computed(() => {
  const comments = task.value.value?.value.comments ?? [],
    answered = new Set(
      comments.flatMap((comment) =>
        comment.responses.map((response) => response.requestId),
      ),
    );
  return comments
    .flatMap((comment) =>
      comment.requests.map((request) => ({ comment, request })),
    )
    .filter((item) => !answered.has(item.request.requestId));
});
const markedRevisions = new Set<number>();
watch(
  () => taskView.value.value,
  async (view) => {
    if (
      !view?.unreadAgentComments ||
      props.workspace.role !== "user" ||
      markedRevisions.has(view.object.revision)
    )
      return;
    markedRevisions.add(view.object.revision);
    try {
      const binding = await discover(client, "task-board.markCommentsRead", {
        serviceNodeId: props.workspace.serviceNodeId,
      });
      const operationId = await newOperationId(client);
      await callBound(
        client,
        binding,
        {
          operationId,
          taskId: props.taskId,
          expectedTaskRevision: view.object.revision,
        },
        operationId,
      );
      void taskView.refresh();
    } catch {
      markedRevisions.delete(view.object.revision);
    }
  },
);
const dependencyPage = ref(0),
  dependencies = useRemote(async (signal) =>
      await Promise.all(
        (task.value.value?.value.fields.dependencies ?? [])
          .slice(dependencyPage.value * 20, dependencyPage.value * 20 + 20)
          .map(async (id) => {
            try {
              const value = await readDocument(
                "task",
                id,
                props.workspace.rootObjectId,
                signal,
              );
              return {
                id,
                title: value.value.fields.title,
                state: value.value.workflowState,
              };
            } catch {
              return { id, title: id, state: "Unavailable" };
            }
          }),
      ), 0, ["objects/task-board/task"]);
type Body<T> = T extends unknown
  ? Omit<T, "taskId" | "expectedRevision">
  : never;
type ExistingAction = Body<
  Exclude<DraftRequest, { action: "create" | "savePlan" }>
>;
const refresh = () => {
  void task.refresh();
  void taskView.refresh();
};
const perform = async (label: string, body: ExistingAction) => {
  if (!writable.value || !task.value.value) return;
  error.value = null;
  try {
    const current = await readDocument(
      "task",
      props.taskId,
      props.workspace.rootObjectId,
    );
    const outcome = await action.start(label, {
      ...body,
      taskId: props.taskId,
      expectedRevision: current.pin.revision,
    } as DraftRequest);
    if (outcome) {
      panel.value = "";
      refresh();
      emit("changed");
    }
  } catch (cause) {
    error.value =
      cause instanceof Error
        ? cause.message
        : "The action could not be prepared.";
  }
};
const transition = (
  workflowState: "backlog" | "todo" | "in_progress" | "waiting" | "done" | "cancelled",
  detail: string | null = null,
) =>
  perform("Move task to " + workflowState, {
    action: "transition",
    workflowState,
    detail,
  });
const submitComment = async () => {
  if (!commentBody.value.trim()) return;
  await perform("Add task comment", {
    action: "comment",
    commentId: crypto.randomUUID(),
    body: commentBody.value.trim(),
    moveToTodo: moveToTodo.value,
    requests: [],
    responses: [],
    attachments: pendingAttachments.value,
    replyTo: null,
  });
  if (!action.error.value) {
    commentBody.value = "";
    moveToTodo.value = false;
    pendingAttachments.value = [];
  }
};
const answerApproval = (
  requestId: string,
  decision: "approved" | "rejected",
  replyTo: string,
) =>
  perform(decision === "approved" ? "Approve request" : "Reject request", {
    action: "comment",
    commentId: crypto.randomUUID(),
    body: decision === "approved" ? "Approved." : "Rejected.",
    requests: [],
    responses: [{ requestId, type: "approval", decision, detail: "" }],
    attachments: [],
    replyTo,
  });
const images = useAttachmentImages(
  () => props.taskId,
  () => task.value.value?.value.attachments ?? [],
  () => {
    const value = task.value.value?.value;
    return value ? [value.fields.description, ...value.fields.acceptanceCriteria, ...value.comments.map((comment) => comment.body), commentBody.value] : [commentBody.value];
  },
);
// Files added to a comment are uploaded to the Task at once and listed with the comment when it is sent.
const uploadFile = async (file: File): Promise<UploadedAttachment> => {
  if (!writable.value) throw new Error("This task cannot be changed right now.");
  uploading.value = true;
  try {
    const current = await readDocument("task", props.taskId, props.workspace.rootObjectId);
    const attachmentId = crypto.randomUUID();
    const outcome = await action.start(
      "Upload task attachment",
      await attachmentUpload(props.taskId, current.pin.revision, attachmentId, file),
    );
    if (!outcome?.attachment)
      throw new Error(action.error.value ?? action.saved.value?.detail ?? "The attachment could not be uploaded.");
    pendingAttachments.value.push(outcome.attachment);
    const src = attachmentSource(attachmentId),
      image = attachmentView(file.name, file.type) === "image";
    if (image) images.remember(src, file);
    return { src, name: file.name, image };
  } finally {
    uploading.value = false;
    refresh();
  }
};
// Images shown inside the comment text are not listed again below it.
const unembedded = (comment: TaskBoard.Comment) =>
  comment.attachments.filter((attachment) =>
    attachmentView(attachment.filename, attachment.mediaType) !== "image" ||
    !comment.body.includes(attachmentSource(attachment.attachmentId)));
// Attachment links in the ticket text download the file.
const openAttachmentLink = (event: MouseEvent) => {
  const href = event.target instanceof Element ? event.target.closest("a[href]")?.getAttribute("href") : null,
    attachmentId = href ? sourceAttachmentId(href) : null;
  if (!attachmentId) return;
  event.preventDefault();
  const attachment = task.value.value?.value.attachments.find((value) => value.attachmentId === attachmentId);
  if (attachment)
    void attachmentBytes(attachment, props.taskId)
      .then((bytes) => saveBytes(bytes, attachment.filename))
      .catch((cause) => { error.value = cause instanceof Error ? cause.message : "The attachment is unavailable."; });
};
const setArchived = async () => {
  if (!props.available || props.workspace.role !== 'user' || !task.value.value || task.value.value.value.claim ||
      task.value.value.value.publication || action.locked.value) return;
  archive.clearError();
  const result = await archive.run(
    props.taskId,
    !task.value.value.read.object.effectivelyArchived,
  );
  if (result) {
    refresh();
    emit("changed");
  }
};
const retryArchive = async () => {
  const pending = archive.pending.value;
  if (!pending || !props.available || props.workspace.role !== 'user') return;
  archive.clearError();
  if (await archive.run(pending.objectId, pending.archived)) {
    refresh();
    emit("changed");
  }
};
const primaryActions = computed(() => {
  const value = task.value.value?.value;
  if (!value || !writable.value) return [];
  return transitions(taskContext(value))
    .filter(item => item.action === 'done' || item.action === 'todo' && value.workflowState === 'backlog' || item.action === 'in_progress')
    .map(item => ({ label: item.action === 'done' ? tr('Als erledigt markieren', 'Move to Done') : item.label, run: () => void transition(item.action) }));
});
const canBacklog = computed(() => {
  const value = task.value.value?.value;
  return !!value && writable.value && transitions(taskContext(value)).some((item) => item.action === "backlog");
});
const executionTarget = (fields: TaskBoard.TaskFields) => {
  const requirement =
    fields.executionRequirement ??
    executionDefaults.defaults.value?.executionRequirement;
  const label =
    requirement?.kind === "host"
      ? requirement.hostId
      : tr("Automatisch", "Automatic");
  return fields.executionRequirement ? label : label + " (default)";
};
const projectLabel = (value: TaskBoard.WorkspaceRequirement) =>
  value.kind === "task_workspace"
    ? "No project"
    : value.kind === "existing_project"
      ? project.value.value?.hostId === projectHost.value &&
        project.value.value?.project?.nativeId === value.projectId
        ? project.value.value.project.name
        : projectHost.value
          ? tr(
              "Projekt auf " + projectHost.value + " nicht verfügbar",
              "Project unavailable on " + projectHost.value,
            )
          : tr("Projekt nicht verfügbar", "Project unavailable")
      : value.kind === "directory_path"
        ? value.path
        : "New project · " + value.folderName;
const nativeInput = (comment: TaskBoard.Comment) => task.value.value?.value.comments
  .find(reply => reply.replyTo === comment.commentId && reply.nativeInput)?.nativeInput ?? comment.nativeInput;
const copyId = () => void navigator.clipboard?.writeText(props.taskId);
const copyLink = () => void navigator.clipboard?.writeText(taskUrl(props.workspace.serviceNodeId, props.taskId));
</script>

<template>
  <div :class="sheet ? 'px-5 pb-8 sm:px-8' : 'mx-auto w-full max-w-6xl'">
    <component
      :is="sheet ? 'div' : ToolbarContent"
      :class="
        sheet
          ? 'sticky top-0 z-10 -mx-5 mb-4 flex h-14 items-center gap-2 border-b bg-background px-5 sm:-mx-8 sm:px-8'
          : undefined
      "
    >
      <div class="flex min-w-0 flex-1 items-center gap-2">
        <Button
          v-if="!sheet"
          variant="ghost"
          size="icon-sm"
          as-child
          ><a
            :href="route('tasks', { node: workspace.serviceNodeId })"
            aria-label="Back to board"
            ><ArrowLeft aria-hidden="true" /></a
        ></Button>
        <template v-if="task.value.value">
          <span class="min-w-0 truncate text-sm text-muted-foreground">{{
            task.value.value.value.taskKey
          }}</span>
          <span
            class="inline-flex min-w-0 items-center gap-1.5 text-sm"
            ><component
              :is="
                stateIcons[task.value.value.value.workflowState] ??
                stateIcons.todo
              "
              class="size-4"
              :class="
                stateColors[task.value.value.value.workflowState] ??
                'text-muted-foreground'
              "
              aria-hidden="true"
            /><span class="max-sm:sr-only truncate">{{
              stateLabel(task.value.value.value.workflowState)
            }}</span></span
          >
          <StatusBadge
            v-if="task.value.value.read.object.effectivelyArchived"
            label="Archived"
          />
        </template>
      </div>
      <div class="flex shrink-0 items-center gap-1.5">
        <Button
          v-for="item in primaryActions"
          :key="item.label"
          size="sm"
          @click="item.run"
          >{{ item.label }}</Button
        >
        <Button
          v-if="writable && !terminal && task.value.value"
          variant="ghost"
          size="icon-sm"
          aria-label="Edit task"
          :aria-pressed="panel === 'edit'"
          @click="panel = panel === 'edit' ? '' : 'edit'"
          ><Pencil aria-hidden="true"
        /></Button>
        <DropdownMenu>
          <DropdownMenuTrigger as-child
            ><Button variant="ghost" size="icon-sm" aria-label="More actions"
              ><MoreHorizontal aria-hidden="true" /></Button
          ></DropdownMenuTrigger>
          <DropdownMenuContent align="end" class="w-52">
            <DropdownMenuItem @select="copyId">Copy task ID</DropdownMenuItem>
            <DropdownMenuItem @select="copyLink">Copy task link</DropdownMenuItem>
            <template v-if="task.value.value && (writable || canDelete)">
              <DropdownMenuSeparator />
              <DropdownMenuItem
                v-if="canBacklog"
                @select="transition('backlog')"
                >Move to Backlog</DropdownMenuItem
              >
              <DropdownMenuItem
                v-if="writable && !terminal"
                @select="
                  task.value.value.value.claim
                    ? perform('Request cancellation', {
                        action: 'cancel',
                        reason: 'Cancelled by user.',
                      })
                    : transition('cancelled', 'Cancelled by user.')
                "
                >Cancel task</DropdownMenuItem
              >
              <DropdownMenuItem
                v-if="!task.value.value.value.claim"
                @select="setArchived"
                >{{
                  task.value.value.read.object.effectivelyArchived
                    ? "Restore task"
                    : "Archive task"
                }}</DropdownMenuItem
              >
              <DropdownMenuItem
                v-if="canDelete"
                variant="destructive"
                @select="deleteOpen = true"
                >Delete permanently…</DropdownMenuItem
              >
            </template>
          </DropdownMenuContent>
        </DropdownMenu>
        <Button
          v-if="sheet"
          variant="ghost"
          size="icon-sm"
          as-child
          ><a
            :href="taskRoute(workspace.serviceNodeId, taskId)"
            aria-label="Open full page"
            title="Open full page"
            ><Maximize2 aria-hidden="true" /></a
        ></Button>
        <SheetClose v-if="sheet" as-child
          ><Button variant="ghost" size="icon-sm" aria-label="Close"
            ><X aria-hidden="true" /></Button
        ></SheetClose>
      </div>
    </component>
    <RemoteState
      :loading="task.loading.value"
      :error="task.error.value"
      :has-data="!!task.value.value"
      @retry="task.refresh"
    />
    <template v-if="task.value.value">
      <h1
        class="mb-4 text-xl font-semibold tracking-tight break-words sm:text-2xl"
      >
        <span class="sr-only">{{ task.value.value.value.taskKey }} · </span
        >{{ task.value.value.value.fields.title }}
      </h1>
      <Alert
        v-if="
          taskView.value.value && task.value.value.value.workflowState !== 'review' &&
          noteworthyCondition(taskView.value.value.executionCondition.state)
        "
        class="mb-4"
        :variant="
          conditionTone(taskView.value.value.executionCondition.state) ===
          'bad'
            ? 'destructive'
            : 'default'
        "
        ><CircleAlert aria-hidden="true" /><AlertTitle>{{
          taskView.value.value.executionCondition.state ===
          "blocked_environment"
            ? "Execution unavailable"
            : conditionLabel(taskView.value.value.executionCondition.state)
        }}</AlertTitle
        ><AlertDescription>{{
          taskView.value.value.executionCondition.detail
        }}</AlertDescription></Alert
      >
      <ActionState
        v-if="action.saved.value?.phase !== 'succeeded' || action.error.value"
        :action="action"
        class="mb-4"
        @changed="refresh"
      />
      <p
        v-if="error || archive.error.value"
        role="alert"
        class="mb-4 text-sm text-destructive"
      >
        {{ error || archive.error.value }}
      </p>
      <div v-if="archive.pending.value" class="mb-4 flex flex-wrap items-center gap-3" role="status">
        <span class="text-sm text-muted-foreground">{{ tr("Archivierungsaktion noch nicht bestätigt.", "Archive action awaiting confirmation.") }}</span>
        <Button variant="outline" size="sm" :disabled="archive.busy.value || !available" @click="retryArchive">
          {{ tr("Erneut prüfen", "Check again") }}
        </Button>
      </div>
      <section v-if="panel === 'edit'" aria-label="Edit task">
        <TaskEditor
          :workspace="workspace"
          :available="available"
          :task="task.value.value"
          @saved="
            panel = '';
            refresh();
            emit('changed');
          "
          @cancel="panel = ''"
        />
      </section>
      <div v-else class="task-detail-grid">
        <div class="min-w-0 space-y-8 [grid-area:main]" @click="openAttachmentLink">
          <section class="space-y-2" aria-labelledby="task-description">
            <h2
              id="task-description"
              class="text-sm font-semibold text-muted-foreground"
            >
              Description
            </h2>
            <div
              v-if="task.value.value.value.fields.description"
              :class="
                descriptionNeedsCollapse && !descriptionExpanded
                  ? 'max-h-60 overflow-hidden [mask-image:linear-gradient(to_bottom,black_70%,transparent)]'
                  : ''
              "
            >
              <ContentView
                :text="task.value.value.value.fields.description"
                media-type="text/markdown"
                :trusted-image-urls="images.urls.value"
                :trusted-attachments="images.files.value"
              />
            </div>
            <p v-else class="text-sm text-muted-foreground">
              No description provided.
            </p>
            <Button
              v-if="descriptionNeedsCollapse"
              variant="link"
              size="sm"
              class="px-0"
              @click="descriptionExpanded = !descriptionExpanded"
              >{{ descriptionExpanded ? "Show less" : "Show more" }}</Button
            >
          </section>

          <section
            v-if="task.value.value.value.fields.acceptanceCriteria.length"
            class="space-y-2"
            aria-labelledby="task-acceptance"
          >
            <h2
              id="task-acceptance"
              class="text-sm font-semibold text-muted-foreground"
            >
              Acceptance criteria
            </h2>
            <ul class="list-disc space-y-1 pl-5 text-sm">
              <li
                v-for="criterion in task.value.value.value.fields
                  .acceptanceCriteria"
                :key="criterion"
              >
                <ContentView :text="criterion" media-type="text/markdown" :trusted-image-urls="images.urls.value" :trusted-attachments="images.files.value" />
              </li>
            </ul>
          </section>



        </div>
        <section
          class="min-w-0 space-y-3 [grid-area:activity]"
          aria-labelledby="task-activity"
        >
            <h2 id="task-activity" class="sr-only">Activity</h2>
            <Tabs default-value="comments">
              <TabsList class="max-w-full justify-start overflow-x-auto">
                <TabsTrigger value="comments"
                  >Comments ({{
                    task.value.value.value.comments.filter((comment) => !(comment.authorKind === 'system' && comment.body === 'Ticket edited')).length
                  }})</TabsTrigger
                >
                <TabsTrigger value="history">History</TabsTrigger>
              </TabsList>

              <TabsContent value="comments" class="mt-3 space-y-4">
                <p
                  v-if="!task.value.value.value.comments.length"
                  class="text-sm text-muted-foreground"
                >
                  No comments yet.
                </p>
                <ol v-else class="space-y-3">
                  <li
                    v-for="comment in task.value.value.value.comments"
                    :key="comment.commentId"
                  >
                    <p v-if="comment.authorKind === 'system' && comment.body === 'Ticket edited'"
                      class="px-1 text-xs text-muted-foreground">
                      {{ tr('Ticket bearbeitet', 'Ticket edited') }} · {{ new Date(comment.createdAt).toLocaleString() }}
                    </p>
                    <CommentCard
                      v-else
                      :author="
                        comment.authorKind === 'agent'
                          ? 'Agent'
                          : comment.authorKind === 'user'
                            ? 'You'
                            : comment.authorKind
                      "
                      :meta="new Date(comment.createdAt).toLocaleString()"
                      ><template #icon
                        ><span
                          class="flex size-5 shrink-0 items-center justify-center rounded-full bg-background text-muted-foreground ring-1 ring-border"
                          aria-hidden="true"
                          ><Bot
                            v-if="comment.authorKind === 'agent'"
                            class="size-3" /><User v-else class="size-3" /></span
                      ></template>
                      <Button v-if="comment.sourceTaskId" as-child variant="link" size="sm" class="h-auto px-0">
                        <a :href="taskRoute(workspace.serviceNodeId, comment.sourceTaskId)">From another task</a>
                      </Button>
                      <ContentView
                        v-if="!comment.nativeInput || comment.replyTo || nativeInput(comment)?.state === 'answered' || nativeInput(comment)?.state === 'expired'"
                        :text="comment.body"
                        media-type="text/markdown"
                        :trusted-image-urls="images.urls.value"
                        :trusted-attachments="images.files.value"
                      />
                      <NativeInput v-if="comment.nativeInput && !comment.replyTo && !['answered', 'expired'].includes(nativeInput(comment)!.state)" compact
                        :input="nativeInput(comment)!" :epoch="comment.nativeInput.identity.epoch"
                        :available="writable" :base="base" :client="client" @answered="refresh" />
                      <div
                        v-for="request in comment.requests"
                        :key="request.requestId"
                        class="rounded-lg border p-3 text-sm"
                      >
                        <p class="font-medium">
                          Approval · {{ request.title }}
                        </p>
                        <p class="mt-1 text-muted-foreground">
                          {{ request.detail }}
                        </p>
                        <div
                          v-if="
                            unresolvedApprovals.some(
                              (item) =>
                                item.request.requestId === request.requestId,
                            )
                          "
                          class="mt-3 flex gap-2"
                        >
                          <Button
                            size="sm"
                            :disabled="!writable"
                            @click="
                              answerApproval(
                                request.requestId,
                                'approved',
                                comment.commentId,
                              )
                            "
                            >Approve</Button
                          >
                          <Button
                            size="sm"
                            variant="outline"
                            :disabled="!writable"
                            @click="
                              answerApproval(
                                request.requestId,
                                'rejected',
                                comment.commentId,
                              )
                            "
                            >Reject</Button
                          >
                        </div>
                      </div>
                      <p
                        v-for="response in comment.responses"
                        :key="response.requestId"
                        class="text-sm"
                      >
                        <strong>Approval {{ response.decision }}.</strong>
                        {{ response.detail }}
                      </p>
                      <div
                        v-if="unembedded(comment).length"
                        class="space-y-2"
                      >
                        <TaskAttachment
                          v-for="attachment in unembedded(comment)"
                          :key="attachment.attachmentId"
                          :attachment="attachment"
                          :task-id="taskId"
                        />
                      </div>
                    </CommentCard>
                  </li>
                </ol>
                <p v-if="task.value.value.read.object.effectivelyArchived" role="status" class="rounded-lg bg-muted px-3 py-2 text-sm text-muted-foreground">
                  This task is archived. Restore it from the task actions to comment or change it.
                </p>
                <div v-else class="space-y-2">
                  <MarkdownEditor
                    ref="commentInput"
                    v-model="commentBody"
                    variant="field"
                    label="Comment"
                    placeholder="Add a comment. Agent-controlled work will receive it automatically."
                    :disabled="!writable"
                    :trusted-image-urls="images.urls.value"
                    :trusted-attachments="images.files.value"
                    :upload-file="uploadFile"
                  ><template #actions><WikiLinkPicker :disabled="!writable" @select="commentInput?.insertMarkdown($event)" /></template></MarkdownEditor>
                  <div class="flex flex-wrap items-center gap-2">
                    <Label v-if="task.value.value.value.workflowState === 'done' && writable" class="flex items-center gap-2">
                      <Checkbox v-model="moveToTodo" :disabled="!writable" />
                      {{ tr('Nach Todo verschieben', 'Move to Todo') }}
                    </Label>
                    <span v-if="uploading" role="status" class="text-sm text-muted-foreground">Uploading…</span>
                    <Button
                      size="sm"
                      class="ml-auto"
                      :disabled="!writable || !commentBody.trim() || uploading"
                      @click="submitComment"
                      >Add comment</Button
                    >
                  </div>
                </div>
                <Disclosure
                  v-if="task.value.value.value.attachments.length"
                  v-model:open="attachmentsOpen"
                  :title="'Attachments (' + task.value.value.value.attachments.length + ')'"
                  class="border-t pt-3"
                >
                  <div v-if="attachmentsOpen" class="space-y-2">
                    <TaskAttachment
                      v-for="attachment in task.value.value.value.attachments"
                      :key="attachment.attachmentId"
                      :attachment="attachment"
                      :task-id="taskId"
                    />
                  </div>
                </Disclosure>
              </TabsContent>

              <TabsContent value="history" class="mt-3">
                <TaskHistory :task-id="taskId" :workspace="workspace" />
              </TabsContent>
            </Tabs>
        </section>

        <aside
          class="min-w-0 space-y-6 [grid-area:aside] lg:self-start lg:border-l lg:pl-6"
          aria-label="Task details"
        >
          <section aria-labelledby="task-properties">
            <h2
              id="task-properties"
              class="mb-3 text-sm font-semibold text-muted-foreground"
            >
              Properties
            </h2>
            <PropertyList>
              <PropertyItem label="Status"
                ><StatusBadge
                  :label="stateLabel(task.value.value.value.workflowState)"
                  :tone="statusTone(task.value.value.value.workflowState)"
              /></PropertyItem>
              <PropertyItem label="Worker">
                <Bot
                  v-if="task.value.value.value.fields.control === 'agent'"
                  class="size-4 text-muted-foreground"
                  aria-hidden="true"
                /><User v-else class="size-4 text-muted-foreground" aria-hidden="true" />{{
                  task.value.value.value.fields.control === "agent"
                    ? "Agent"
                    : "Me"
                }}
              </PropertyItem>
              <PropertyItem label="Priority">{{
                priorityLabel(task.value.value.value.fields.priority)
              }}</PropertyItem>
              <PropertyItem label="Category">{{
                task.value.value.value.fields.category ?? "None"
              }}</PropertyItem>
              <PropertyItem
                v-if="task.value.value.value.fields.dueAt"
                label="Due"
                >{{
                  new Date(task.value.value.value.fields.dueAt).toLocaleString()
                }}</PropertyItem
              >
              <PropertyItem
                v-if="task.value.value.value.fields.nextReviewAt"
                label="Next review"
                >{{
                  new Date(
                    task.value.value.value.fields.nextReviewAt,
                  ).toLocaleString()
                }}</PropertyItem
              >
              <PropertyItem label="Updates">{{
                {
                  ticket: "Ticket only",
                  chat: "Ticket and main chat",
                  phone: "Ticket and voice call",
                }[task.value.value.value.fields.userContact] ??
                task.value.value.value.fields.userContact
              }}</PropertyItem>
            </PropertyList>
          </section>
          <section
            v-if="task.value.value.value.fields.control === 'agent' || task.value.value.value.claim || agentTaskLink || nativeWorkAvailable"
            aria-labelledby="task-execution"
          >
            <h2
              id="task-execution"
              class="mb-3 text-sm font-semibold text-muted-foreground"
            >
              Execution
            </h2>
            <PropertyList>
              <PropertyItem label="Host">{{
                executionTarget(task.value.value.value.fields)
              }}</PropertyItem>
              <PropertyItem
                v-if="task.value.value.value.fields.requiredCapabilities?.length"
                label="Dependencies"
                >{{
                  task.value.value.value.fields.requiredCapabilities.join(", ")
                }}</PropertyItem
              >
              <PropertyItem label="Project"
                ><span class="break-all">{{
                  projectLabel(task.value.value.value.fields.workspaceRequirement)
                }}</span></PropertyItem
              >
              <PropertyItem
                v-if="(task.value.value.value.fields.workspaceRequirement.kind === 'existing_project' && !task.value.value.value.fields.workspaceRequirement.useWorktree) || task.value.value.value.fields.workspaceRequirement.kind === 'directory_path'"
                label="Project scheduling">{{ task.value.value.value.fields.allowParallel ? 'Parallel' : 'Sequential' }}</PropertyItem>
              <PropertyItem v-if="task.value.value.value.claim" label="Run"
                >{{ task.value.value.value.claim.phase }} on
                {{ task.value.value.value.claim.target.hostId }}</PropertyItem
              >
              <PropertyItem
                v-if="task.value.value.value.claim"
                label="Directory"
                ><span class="font-mono text-xs break-all">{{
                  task.value.value.value.claim.workspace.canonicalCwd
                }}</span></PropertyItem
              >
              <PropertyItem label="Model">{{
                inherited(
                  task.value.value.value.fields.nativeOptions?.model,
                  executionDefaults.defaults.value?.nativeOptions.model,
                  tr("Host-Standard", "Host default"),
                )
              }}</PropertyItem>
              <PropertyItem label="Reasoning">{{
                inherited(
                  task.value.value.value.fields.nativeOptions?.reasoningEffort,
                  executionDefaults.defaults.value?.nativeOptions
                    .reasoningEffort,
                  tr("Modell-Standard", "Model default"),
                )
              }}</PropertyItem>
              <PropertyItem label="Speed">{{
                inherited(
                  task.value.value.value.fields.nativeOptions?.serviceTier,
                  executionDefaults.defaults.value?.nativeOptions.serviceTier,
                  "standard",
                )
              }}</PropertyItem>
            </PropertyList>
            <Button
              v-if="agentTaskLink"
              variant="outline"
              size="sm"
              class="mt-3 w-full"
              as-child
              ><a :href="agentTaskLink"
                ><ExternalLink aria-hidden="true" />Open agent task</a
              ></Button
            >
            <Button
              v-if="nativeWorkAvailable"
              variant="outline"
              size="sm"
              class="mt-2 h-auto min-h-9 w-full whitespace-normal py-2"
              :disabled="!writable || !!task.value.value.value.claim"
              @click="perform(tr('Neuen Codex-Chat starten', 'Start a new Codex chat'), {
                action: 'newThread', reason: 'Explicitly requested from the TaskBoard ticket.',
              })"
              ><Bot aria-hidden="true" />{{ tr('Neuen Codex-Chat starten', 'Start a new Codex chat') }}</Button
            >
          </section>
          <section
            v-if="task.value.value.value.fields.dependencies.length"
            class="space-y-2"
            aria-labelledby="task-dependencies"
          >
            <h2
              id="task-dependencies"
              class="text-sm font-semibold text-muted-foreground"
            >
              Blockers
            </h2>
            <a
              v-for="item in dependencies.value.value"
              :key="item.id"
              :href="taskRoute(workspace.serviceNodeId, item.id)"
              class="flex items-center justify-between gap-2 rounded-md px-2 py-1.5 text-sm hover:bg-muted"
              ><span class="min-w-0 truncate">{{ item.title }}</span
              ><StatusBadge
                :label="stateLabel(item.state)"
                :tone="statusTone(item.state)"
            /></a>
            <PageControls
              v-if="task.value.value.value.fields.dependencies.length > 20"
              :page="dependencyPage + 1"
              :count="dependencies.value.value?.length ?? 0"
              :has-next="
                (dependencyPage + 1) * 20 <
                task.value.value.value.fields.dependencies.length
              "
              :loading="dependencies.loading.value"
              @previous="
                dependencyPage--;
                dependencies.refresh();
              "
              @next="
                dependencyPage++;
                dependencies.refresh();
              "
            />
          </section>
          <p class="border-t pt-4 text-xs text-muted-foreground">
            Created
            {{ new Date(task.value.value.value.createdAt).toLocaleString() }}
            <br />Updated
            {{ new Date(task.value.value.value.updatedAt).toLocaleString() }}
            <br />Revision {{ task.value.value.pin.revision }} · work
            {{ task.value.value.value.workRevision }}
          </p>
        </aside>
      </div>
    </template>
    <ObjectDeleteDialog v-model:open="deleteOpen" :client="client" :object-id="taskId" :expected-revision="task.value.value?.read.object.currentRevision ?? null" :label="'the ticket “' + (task.value.value?.value.taskKey ?? taskId) + '”'" consequence="This also deletes its attachments and related workflow records." :storage-key="'ivy.task-board.delete:' + base.href + ':' + taskId" @deleted="emit('deleted')" />
  </div>
</template>
<style scoped>
.task-detail-grid {
  display: grid;
  min-width: 0;
  gap: 2rem;
  grid-template-columns: minmax(0, 1fr);
  grid-template-areas: "main" "activity" "aside";
}
@media (min-width: 1024px) {
  .task-detail-grid {
    grid-template-columns: minmax(0, 1fr) 17rem;
    grid-template-areas: "main aside" "activity aside";
    grid-template-rows: auto 1fr;
  }
}
@media (min-width: 1280px) {
  .task-detail-grid {
    grid-template-columns: minmax(0, 1fr) 19rem;
  }
}
</style>
