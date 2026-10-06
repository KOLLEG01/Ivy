type ToolPresentation = {
  title: string;
  description: string;
  destructiveHint?: boolean;
  openWorldHint?: boolean;
};

/** MCP display metadata for known tools, scoped to their owning service. */
const presentations: Record<string, Record<string, ToolPresentation>> = {
  hive: {
    hive_status: {
      title: "Check Hive status",
      description:
        "Read Hive readiness, version, and the current caller identity.",
    },
    ivy_instructions: {
      title: "Read Ivy instructions",
      description:
        "Read the complete MCP instructions or tested workflow examples. Follow nextCursor if this page is partial.",
    },
    ivy_tool_schema: {
      title: "Read Ivy tool schema",
      description:
        "Read a public tool's exact input and output JSON Schemas when a client hides alternatives or references in its tool signature.",
    },
    hive_object_read: {
      title: "Read Hive object",
      description:
        "Read the content, metadata, or revision history of a stored object.",
    },
    hive_object_write: {
      title: "Save Hive object",
      description: "Create or update a stored object with revision checking.",
    },
    hive_object_search: {
      title: "Search Hive objects",
      description:
        "Search stored content by text or structured objects by field.",
    },
    hive_object_list: {
      title: "List Hive objects",
      description: "List objects under a parent or browse their hierarchy.",
    },
    hive_object_move: {
      title: "Move Hive object",
      description: "Move, rename, or reorder a stored object.",
    },
    hive_object_archive: {
      title: "Archive Hive object",
      description: "Archive or restore a stored object.",
    },
    hive_schema_list: {
      title: "List Hive schemas",
      description: "List compact data contracts and versions; read a selected full schema with hive_schema_get.",
    },
    hive_schema_get: {
      title: "Read Hive schema",
      description: "Read a data contract, its schema, and usage guidance.",
    },
    hive_schema_register: {
      title: "Register Hive schema",
      description: "Register a data contract for agent-owned objects.",
      destructiveHint: false,
    },
    wiki_search: {
      title: "Search wiki",
      description: "Search wiki page titles and Markdown content.",
    },
    wiki_list: {
      title: "List wiki pages",
      description: "List wiki pages under a parent page or at the root.",
    },
    wiki_read: {
      title: "Read wiki page",
      description: "Read a wiki page or its revision history.",
    },
    wiki_create: {
      title: "Create wiki page",
      description: "Create a wiki page with a title and Markdown content.",
      destructiveHint: false,
    },
    wiki_update: {
      title: "Update wiki page",
      description: "Update, move, archive, or restore a wiki page.",
    },
    hive_inspect_storage: {
      title: "Inspect Hive storage",
      description: "Check stored schema versions and storage compatibility.",
    },
    hive_diagnostics: {
      title: "Read Hive diagnostics",
      description: "Find current or historical service problems.",
    },
    hive_retention_status: {
      title: "Check data retention",
      description:
        "Read retention policies and preview data eligible for cleanup.",
    },
    hive_schema_validate: {
      title: "Validate object data",
      description: "Check content against a specific schema version.",
    },
    hive_hosts: {
      title: "List hosts",
      description:
        "List connected hosts, their services, and reported configuration.",
    },
    hive_host_configuration_list: {
      title: "List host configurations",
      description: "List saved host settings and their current revisions.",
    },
    hive_host_configuration_read: {
      title: "Read host configuration",
      description:
        "Read host settings with secrets hidden, or their revision history.",
    },
    hive_host_configuration_update: {
      title: "Update host configuration",
      description:
        "Save host settings at the current revision while preserving hidden secrets.",
    },
    hive_service_nodes: {
      title: "List service instances",
      description: "Find registered service instances by host or service name.",
    },
    hive_service_node_get: {
      title: "Read service instance",
      description:
        "Read a service instance's status, identity, or required schemas.",
    },
    hive_resources: {
      title: "Find service resources",
      description:
        "Find resources advertised by services and identify their owners.",
    },
    hive_resource_get: {
      title: "Read service resource",
      description: "Read an advertised resource and when it was last observed.",
    },
    hive_event_topics: {
      title: "Find event triggers",
      description:
        "List registered triggers, event kinds and payload schemas. Omit selectors to browse all providers.",
    },
    hive_event_read: {
      title: "Read service events",
      description: "Read saved events after a sequence number.",
    },
    hive_package_catalog: {
      title: "List packages",
      description: "List available service and application packages.",
    },
    hive_authorize_package_upload: {
      title: "Authorize package upload",
      description:
        "Create a single-use upload token for one package, valid for five minutes.",
      destructiveHint: false,
    },
    hive_ui_get: {
      title: "Read application release",
      description: "Read a published application and its available releases.",
    },
    hive_ui_catalog: {
      title: "List applications",
      description: "List published applications and their availability.",
    },
    hive_ui_inspect: {
      title: "Check application release",
      description:
        "Check an application release and its dependencies before deployment.",
    },
    hive_ui_deploy: {
      title: "Deploy application",
      description: "Publish and activate an application release.",
    },
    hive_ui_rollback: {
      title: "Roll back application",
      description: "Activate a previous application release.",
    },
    hive_deployment_list: {
      title: "List deployments",
      description: "List reported service deployments and their status.",
    },
    hive_deployment_get: {
      title: "Read deployment",
      description:
        "Read a deployment's original operation and outcome on its host.",
    },
  },
  "agent-manager": {
    agent_manager_status: {
      title: "Check Codex host",
      description: "Read the status and work capabilities of a Codex host.",
    },
    agent_manager_read: {
      title: "Read Codex task",
      description:
        "Read current Codex task state or the outcome of an earlier action.",
    },
    agent_manager_inputs: {
      title: "Find Codex requests",
      description:
        "Find pending questions or approvals and read their reply schemas.",
    },
    agent_manager_answer: {
      title: "Answer Codex request",
      description: "Answer a pending Codex question or approval request.",
      openWorldHint: true,
    },
    agent_manager_discover: {
      title: "Discover Codex methods",
      description:
        "Find native Codex methods on a host and read their schemas.",
    },
    agent_manager_invoke: {
      title: "Invoke Codex method",
      description: "Run a native Codex method on its owning host.",
      openWorldHint: true,
    },
    agent_manager_projects: {
      title: "Find Codex projects",
      description:
        "Read available Codex projects or labeled host configuration.",
    },
    agent_manager_resolve_project: {
      title: "Resolve Codex project",
      description:
        "Validate a project path or create a durable project assignment.",
      destructiveHint: false,
    },
    agent_manager_interact: {
      title: "Send Codex interaction",
      description:
        "Run a native Codex method with a briefly cached reply; reconcile uncertain results before retrying.",
      openWorldHint: true,
    },
    agent_manager_interaction: {
      title: "Read Codex interaction",
      description: "Read the cached reply to a recent Codex interaction.",
    },
    agent_manager_notifications: {
      title: "Read Codex events",
      description: "Read saved Codex events and any gaps in their history.",
    },
    agent_manager_configure_capabilities: {
      title: "Configure Codex host",
      description: "Set which kinds of agent work a host accepts.",
    },
  },
  "task-board": {
    task_list: {
      title: "List tasks",
      description: "List TaskBoard tasks or categories with optional filters.",
    },
    task_read: {
      title: "Read task",
      description: "Read a TaskBoard task or the outcome of an earlier action.",
    },
    task_create: {
      title: "Create task",
      description: "Create a TaskBoard task in the backlog. Read task_configuration first and use its current defaults unless the user explicitly requests different settings. Omitted userContact inherits the current board default.",
      destructiveHint: false,
    },
    task_update: {
      title: "Update task",
      description:
        "Update task details, workflow, assignment, or results; may start or cancel agent work. Content edits preserve omitted userContact. Contact, priority and date edits during an active run keep its running assignment intact.",
    },
    task_comment: {
      title: "Comment on task",
      description:
        "Add a comment or question; user comments can start agent work.",
      destructiveHint: false,
    },
    task_review: {
      title: "Review task result",
      description: "Accept a saved final TaskBoard result after review.",
      destructiveHint: false,
    },
    task_upload_attachment: {
      title: "Attach file to task",
      description: "Attach a file to a TaskBoard task.",
      destructiveHint: false,
    },
    task_save_plan: {
      title: "Save task execution plan",
      description: "Save the agent, version, and execution plan for a task.",
    },
  },
  "chat-bridge": {
    chat_bridge_send: {
      title: "Send message to Main",
      description:
        "Queue a message or notification for the Main agent to process.",
      destructiveHint: false,
    },
    chat_bridge_read: {
      title: "Read Main messages",
      description:
        "Read message history or the state of an earlier ChatBridge action.",
    },
    chat_bridge_status: {
      title: "Check Main connection",
      description:
        "Read configured message channels and the shared Main conversation binding.",
    },
    chat_bridge_create_main: {
      title: "Set up Main conversation",
      description:
        "Create or replace the shared Main conversation at its current revision.",
    },
    chat_bridge_cancel: {
      title: "Cancel Main input",
      description: "Request cancellation of a queued or running Main input.",
    },
    chat_bridge_retry_result: {
      title: "Retry Main result retrieval",
      description:
        "Retrieve the original turn result again without starting another turn.",
    },
  },
  "phone-bridge": {
    phone_bridge_status: {
      title: "Check phone status",
      description: "Read availability, configured recipients, and call status.",
    },
    phone_bridge_call: {
      title: "Start phone call",
      description: "Call a configured recipient through a voice agent.",
      openWorldHint: true,
    },
    phone_bridge_hangup: {
      title: "End phone call",
      description: "End a call and release its audio resources.",
    },
    phone_bridge_diagnostics: {
      title: "Diagnose phone service",
      description:
        "Check audio devices, codecs, call logs, and voice-task archival.",
    },
    phone_bridge_reconnect: {
      title: "Reconnect phone service",
      description:
        "Reconnect the configured SIP account while no calls are active.",
    },
    phone_bridge_calls: {
      title: "List active calls",
      description: "List current calls visible to the caller.",
    },
    phone_bridge_history: {
      title: "Read call history",
      description: "Find current and completed calls in the caller's history.",
    },
    phone_bridge_screen: {
      title: "Screen a phone call",
      description:
        "Call a configured recipient with an automated announcement and collect acceptance or refusal.",
      openWorldHint: true,
    },
    phone_bridge_bridge_screening: {
      title: "Connect screened call",
      description:
        "Connect an accepted screening call to Windows audio without redialing.",
      openWorldHint: true,
    },
    phone_bridge_accept: {
      title: "Accept incoming call",
      description: "Answer the incoming call reported by the phone service.",
      openWorldHint: true,
    },
  },
  secretary: {
    secretary_binding: {
      title: "Read Secretary binding",
      description: "Read the current service node, host and expected scope for Secretary calls.",
    },
    secretary_list_assignments: {
      title: "List assignments",
      description: "List Secretary assignments using the binding's expected scope.",
    },
    secretary_history: {
      title: "Read inbox action outcome",
      description: "Read the original result of a Secretary inbox action.",
    },
    secretary_operation_read: {
      title: "Read Secretary operation outcome",
      description: "Read the caller-owned result of a Secretary inbox or assignment action.",
    },
    secretary_create_assignment: {
      title: "Create assignment",
      description: "Schedule agent work or trigger it from service events.",
      destructiveHint: false,
    },
    secretary_update_assignment: {
      title: "Update assignment",
      description:
        "Change an assignment's prompt, schedule, or event trigger at its current revision.",
    },
    secretary_status: {
      title: "Check Secretary status",
      description:
        "Find the assignment workspace and current configuration revision.",
    },
    secretary_configure_execution: {
      title: "Configure assignments",
      description: "Set the agent execution target and user contact rules.",
    },
  },
};

export function toolPresentation(
  serviceName: string,
  name: string,
): (ToolPresentation & { openWorldHint: boolean }) | undefined {
  const service = Object.hasOwn(presentations, serviceName)
    ? presentations[serviceName]
    : undefined;
  const presentation =
    service && Object.hasOwn(service, name) ? service[name] : undefined;
  // Private storage and bounded administration are closed-world. Agent dispatch
  // and calls to external recipients are marked above; unknown tools keep MCP defaults.
  return presentation
    ? { ...presentation, openWorldHint: presentation.openWorldHint ?? false }
    : undefined;
}

export function fallbackToolTitle(name: string): string {
  const words = name.replaceAll("_", " ");
  return words.charAt(0).toUpperCase() + words.slice(1);
}
