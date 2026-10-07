import type { Wire } from "../../../../packages/sdk/src/node.js";
import { requireThat } from "../../../../packages/sdk/src/node.js";
import { SchemaValidators } from "../../../../packages/sdk/src/node.js";
import type { Schema } from "../../../../packages/sdk/src/node.js";
import { phoneSchema } from "./native.js";
import admission from "../../../../specs/schemas/phone-admission.schema.json" with { type: "json" };
import journal from "../../../../specs/schemas/phone-journal.schema.json" with { type: "json" };
import service from "../../../../specs/schemas/phone-service.schema.json" with { type: "json" };
import archive from "../../../../specs/schemas/phone-voice-archive.schema.json" with { type: "json" };
import appTools from "../../../../specs/schemas/codex-app-tools.schema.json" with { type: "json" };

const definitions = {
  ...(phoneSchema("Status")["$defs"] as Record<string, Schema>),
  ...appTools.$defs,
  ...archive.$defs,
  ...journal.$defs,
  ...admission.$defs,
  ...service.$defs,
};
const validators = new SchemaValidators();
const preparedSchemas = new Map<string, Record<string, unknown>>();
function freeze(value: unknown): unknown {
  if (value && typeof value === "object" && !Object.isFrozen(value)) {
    for (const child of Object.values(value as Record<string, unknown>))
      freeze(child);
    Object.freeze(value);
  }
  return value;
}
export function phoneServiceSchema(name: string): Record<string, unknown> {
  requireThat(
    Object.hasOwn(definitions, name),
    "internal_error",
    "Unknown Phone service contract.",
  );
  const prepared = preparedSchemas.get(name);
  if (prepared) return prepared;
  // Keep only reachable existing contracts so each public tool has a small self-contained schema.
  const selected: Record<string, unknown> = {},
    source = definitions as Record<string, Schema>;
  const visit = (value: unknown): void => {
    if (Array.isArray(value)) {
      for (const item of value) visit(item);
      return;
    }
    if (!value || typeof value !== "object") return;
    for (const [key, item] of Object.entries(value)) {
      if (key === "$ref") {
        requireThat(
          typeof item === "string" && item.startsWith("#/$defs/"),
          "internal_error",
          "Phone contracts require local references.",
        );
        const target = item.slice(8);
        requireThat(
          Object.hasOwn(source, target),
          "internal_error",
          "Phone contract reference is missing.",
        );
        if (!Object.hasOwn(selected, target)) {
          selected[target] = source[target];
          visit(source[target]);
        }
      } else visit(item);
    }
  };
  const root = source[name];
  visit(root);
  const result = freeze(
    structuredClone({ ...(root as Record<string, unknown>), $defs: selected }),
  ) as Record<string, unknown>;
  preparedSchemas.set(name, result);
  return result;
}
export function validatePhoneService(name: string, value: unknown): void {
  validators.validate(phoneServiceSchema(name), value, 256 * 1024);
}
export const phoneTools = [
  {
    name: "voiceInputs",
    input: "PhoneCallQuery",
    output: "PhoneVoiceInputs",
    readOnly: true,
    description:
      "Read up to four pending native approvals or questions for the original Voice task, including their exact reply schemas. Nothing is approved automatically.",
  },
  {
    name: "answerVoiceInput",
    input: "PhoneVoiceInputAnswer",
    output: "PhoneVoiceInputSubmitted",
    readOnly: false,
    description:
      "Submit one reply to a current native Voice task input using its exact id and reply schema. A consumed or ended input cannot be sent again; submitted does not confirm the requested task action completed.",
  },
  {
    name: "probeLoopback",
    input: "PhoneStatusQuery",
    output: "PhoneLoopbackProbe",
    readOnly: true,
    description:
      "Measure five seconds of Windows loopback activity while idle (administrator only). Play a test tone in another app; returns peak/sample counts without storing audio. New calls interrupt the probe.",
  },
  {
    name: "audioSetup",
    input: "PhoneStatusQuery",
    output: "PhoneAudioSetup",
    readOnly: true,
    description:
      "Check audio device availability and mute settings for Windows routes (administrator only). Readiness covers device checks, not a live audio test; does not change devices.",
  },
  {
    name: "codecTest",
    input: "PhoneStatusQuery",
    output: "PhoneCodecTestResult",
    readOnly: true,
    description:
      "Test installed codecs and resamplers while idle (administrator only). Uses synthetic audio without opening devices or calls; results are cached until the native process restarts.",
  },
  {
    name: "inventory",
    input: "PhoneStatusQuery",
    output: "PhoneInventory",
    readOnly: true,
    description:
      "List Windows audio devices and supported call codecs for configuration (administrator only). Does not open audio streams or change defaults.",
  },
  {
    name: "reconnect",
    input: "PhoneReconnect",
    output: "SipRegistrationObservation",
    readOnly: false,
    description:
      "Reconnect the configured SIP registration when no calls are active (administrator only). Keep operationId after uncertain replies.",
  },
  {
    name: "calls",
    input: "PhoneStatusQuery",
    output: "PhoneCalls",
    readOnly: true,
    description:
      "List current calls visible to this caller; use phone.call for details.",
  },
  {
    name: "history",
    input: "PhoneHistoryQuery",
    output: "PhoneHistory",
    readOnly: true,
    description:
      "Find current and completed calls by history. Returns this caller’s calls; administrators may inspect all calls.",
  },
  {
    name: "logs",
    input: "PhoneLogsQuery",
    output: "PhoneLogs",
    readOnly: true,
    description:
      "Diagnose a call using retained audio, codec and transport logs. Logs can expire; original command receipts remain available.",
  },
  {
    name: "screen",
    input: "PhoneScreen",
    output: "PhoneCall",
    readOnly: false,
    description:
      "Call a configured recipient with an automated announcement: 1 accepts, 2 declines. Include an audible automated-call disclosure. Read phone.call for the decision; bridgeScreening connects an accepted call to Windows audio.",
  },
  {
    name: "bridgeScreening",
    input: "PhoneCallQuery",
    output: "PhoneCall",
    readOnly: false,
    description:
      "Connect an accepted screening call to Windows audio in the original call, without redialing or starting Voice.",
  },
  {
    name: "status",
    input: "PhoneStatusQuery",
    output: "PhoneServiceStatus",
    readOnly: true,
    description:
      "Check phone readiness, configured recipient IDs and this caller’s active or incoming call.",
  },
  {
    name: "request",
    input: "PhoneRequest",
    output: "PhoneCall",
    readOnly: false,
    description:
      "Call a configured recipient. Choose route=voice and voicePrompt for a voice model conversation; the default route uses Windows audio. Authorized direct destinations require Windows. Success means admitted; check phone.call for connection.",
  },
  {
    name: "voiceCall",
    input: "PhoneVoiceCall",
    output: "PhoneCall",
    readOnly: false,
    description:
      "Call a configured recipient through the voice agent. initialPrompt is required. If that recipient already has a Voice call, send the prompt to its current task and return that call instead of dialing again. Reusing operationId never sends twice. Otherwise deliver the prompt after connection; Sol/high is the default task selection. Use phone_bridge_status to check connection and the retained operation outcome.",
  },
  {
    name: "selectVoice",
    input: "PhoneSelectVoice",
    output: "PhoneOperationResult",
    readOnly: false,
    description:
      "Change model and reasoningEffort for subsequent turns in the active Voice task. Requires callId and a new operationId. Supports gpt-6-luna, gpt-6-sol and gpt-6-astra; Luna excludes ultra. Keeps the phone call and task running. Reusing operationId returns the original outcome.",
  },
  {
    name: "restartVoice",
    input: "PhoneRestartVoice",
    output: "PhoneOperationResult",
    readOnly: false,
    description:
      "Create a new Voice task with the current model and reasoning, transfer the live Voice session and send the original greeting. Keeps the phone call connected. Requires callId and a new operationId; reuse it after lost replies to read the original outcome without another restart.",
  },
  {
    name: "accept",
    input: "PhoneAccept",
    output: "PhoneCall",
    readOnly: false,
    description:
      "Accept the incoming call returned by phone.status for this caller.",
  },
  {
    name: "call",
    input: "PhoneCallQuery",
    output: "PhoneCallView",
    readOnly: true,
    description:
      "Read an original call’s admission, connection, voice prompt and current state.",
  },
  {
    name: "operation",
    input: "PhoneOperationQuery",
    output: "PhoneOperationResult",
    readOnly: true,
    description:
      "Read an original command outcome without repeating effects; null means that command has no retained intent.",
  },
  {
    name: "hangup",
    input: "PhoneCallQuery",
    output: "PhoneCallView",
    readOnly: false,
    description:
      "End the original call or screening and release its audio resources. Uncertain cleanup remains blocked for recovery.",
  },
] as const;
export type PhoneTool = (typeof phoneTools)[number]["name"];
function phoneMcp(
  name: PhoneTool,
): { name: string; surface: "ivy" | "ivy_dev" } | null {
  if (name === "request") return null;
  if (["status", "call", "operation"].includes(name))
    return { name: "phone_bridge_status", surface: "ivy" };
  if (name === "voiceCall")
    return { name: "phone_bridge_call", surface: "ivy" };
  if (name === "hangup") return { name: "phone_bridge_hangup", surface: "ivy" };
  if (name === "selectVoice")
    return { name: "phone_bridge_select_voice", surface: "ivy" };
  if (name === "restartVoice")
    return { name: "phone_bridge_restart_voice", surface: "ivy" };
  if (name === "voiceInputs")
    return { name: "phone_bridge_voice_inputs", surface: "ivy" };
  if (name === "answerVoiceInput")
    return { name: "phone_bridge_answer_voice_input", surface: "ivy" };
  if (
    ["probeLoopback", "audioSetup", "codecTest", "inventory", "logs"].includes(
      name,
    )
  )
    return { name: "phone_bridge_diagnostics", surface: "ivy_dev" };
  if (name === "reconnect")
    return { name: "phone_bridge_reconnect", surface: "ivy_dev" };
  return {
    name:
      "phone_bridge_" +
      name.replace(/[A-Z]/g, (letter) => "_" + letter.toLowerCase()),
    surface: "ivy_dev",
  };
}
export function phoneRegistry(): Wire.RegistrySync {
  return {
    discoveryHint:
      "PhoneBridge lets agents call configured recipients through a voice agent. Use phone_bridge_status, phone_bridge_call with initialPrompt, phone_bridge_select_voice, phone_bridge_restart_voice and phone_bridge_hangup. Native approvals and questions are available through phone_bridge_voice_inputs and phone_bridge_answer_voice_input.",
    namespaces: [
      {
        namespace: "phone",
        description:
          "Phone calls with a voice model, Windows audio or automated announcements",
        guideMarkdown:
          "Use phone_bridge_status for readiness, configured recipient IDs, original call state, current Voice model selection and original operation outcomes. phone_bridge_call requires recipientId, operationId and a nonempty initialPrompt. PhoneBridge creates a local task with Sol/high by default and sends the prompt after connection. During the call, *1<M><R># selects model 1 Luna, 2 Sol or 3 Astra and reasoning 1 low, 2 medium, 3 high, 4 xhigh, 5 max or 6 ultra (not Luna). Selection leaves the current task running; *0# creates a new task with the selected model and reasoning, then starts Voice there. Use phone_bridge_hangup to end the call. " +
          "phone_bridge_select_voice changes model and reasoningEffort in the current task; phone_bridge_restart_voice creates and transfers to a new task with that selection. Both require callId and operationId, keep the phone connection, and return the retained operation. Use a new operationId only for a new action; read a lost reply via phone_bridge_status with callId and operationId. " +
          "If the recipient already has an incoming or outgoing Voice call, phone_bridge_call forwards initialPrompt unchanged to its current task and returns the existing callId. It waits for setup or a task switch and retains the send under the request operationId, so retries cannot duplicate it. " +
          "The internal phone.request method also supports the Windows audio route and authorized direct destinations. " +
          "phone.screen plays an announcement and collects 1/2; phone.bridgeScreening connects an accepted call to Windows audio. " +
          "Keep operationId and callId after lost replies. Admission does not prove connection: inspect the original call or operation through phone_bridge_status. Development operators can use ivy_dev diagnostics and reconnect tools for recovery.",
        tools: phoneTools.map((tool) => {
          const mcp = phoneMcp(tool.name);
          return {
            namespace: "phone",
            name: tool.name,
            ...(mcp ? { discovery: { mcp } } : {}),
            interfaceVersion: "1.0.0",
            description: tool.description,
            inputSchema: phoneServiceSchema(tool.input),
            outputSchema: phoneServiceSchema(tool.output),
            annotations: { readOnlyHint: tool.readOnly, idempotentHint: true },
          };
        }),
        topics: [],
        inventoryKinds: [],
      },
    ],
    contracts: [],
    requiredContracts: [],
  };
}
