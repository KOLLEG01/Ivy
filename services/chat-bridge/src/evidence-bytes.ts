import { canonical, digest } from "../../../packages/sdk/src/node.js";
import { IvyError, requireThat } from "../../../packages/sdk/src/node.js";
import { validateAgent } from "../../../packages/sdk/src/node.js";
import type { Chat } from "../../../packages/sdk/src/node.js";
import { ChatStore, mutation } from "./store.js";
import { validateChatNativeDraft } from "./native-plan.js";

export const chunkBytes = 1024 * 1024,
  maximumEvidenceBytes = 16 * chunkBytes;
type Format = Chat.Evidence["format"];
function validateBody(format: Format, value: unknown): void {
  if (format === "native-request/canonical-json")
    validateChatNativeDraft(value);
  else if (format === "agent-operation/canonical-json")
    validateAgent("Operation", value);
  else if (format === "agent-read-observation/canonical-json")
    validateAgent("ReadObservation", value);
  else
    throw new IvyError(
      "chat_evidence_mismatch",
      "Unknown native evidence format.",
    );
}
async function scope(store: ChatStore, parentId: string): Promise<void> {
  try {
    await store.read("chat-bridge/operation", parentId);
    return;
  } catch (error) {
    if (!(error instanceof IvyError) || error.code !== "chat_scope_mismatch")
      throw error;
  }
  await store.read("chat-bridge/input", parentId);
}

/** Immutable byte/shape mechanics only. Native dispatch/result authority additionally requires
 * the owning call's exact caller, node, version, method, request and native-schema verification. */
export async function saveChatEvidence(
  store: ChatStore,
  parentId: string,
  format: Format,
  original: unknown,
): Promise<Chat.ObjectPin> {
  const encoded = canonical(original, maximumEvidenceBytes),
    value: unknown = JSON.parse(encoded);
  validateBody(format, value);
  const bytes = Buffer.from(encoded),
    contentHash = digest(bytes),
    chunks: Chat.Artifact[] = [];
  await scope(store, parentId);
  for (
    let offset = 0, index = 0;
    offset < bytes.length;
    offset += chunkBytes, index++
  ) {
    const part = bytes.subarray(
      offset,
      Math.min(bytes.length, offset + chunkBytes),
    );
    const name = "Chat evidence " + contentHash.slice(7) + " part " + index;
    const pin = store.saveLocalBlob(
      "chat-bridge/evidence-chunk",
      name,
      parentId,
      part,
    );
    chunks.push({
      object: pin,
      contentHash: digest(part),
      byteLength: part.length,
      mediaType: "application/octet-stream",
      label: "Native evidence part " + (index + 1),
    });
  }
  const manifest: Chat.Evidence = {
    schemaVersion: 1,
    format,
    byteLength: bytes.length,
    contentHash,
    chunks,
  };
  return store.write(
    "chat-bridge/evidence",
    manifest,
    mutation(parentId, format + ":" + contentHash + ":manifest"),
    {
      create: {
        parentId,
        name:
          "Chat evidence manifest " +
          format.split("/")[0] +
          " " +
          contentHash.slice(7),
      },
    },
  );
}

export async function readChatEvidence(
  store: ChatStore,
  parentId: string,
  pin: Chat.ObjectPin,
  format: Format,
): Promise<unknown> {
  await scope(store, parentId);
  const manifest = (await store.read("chat-bridge/evidence", pin, parentId))
    .value;
  requireThat(
    manifest.format === format &&
      manifest.chunks.length === Math.ceil(manifest.byteLength / chunkBytes) &&
      new Set(
        manifest.chunks.map(
          (chunk) => chunk.object.objectId + ":" + chunk.object.revision,
        ),
      ).size === manifest.chunks.length,
    "chat_evidence_mismatch",
    "Evidence must retain its exact format and distinct ordered chunk pins.",
  );
  const parts: Buffer[] = [];
  for (let index = 0; index < manifest.chunks.length; index++) {
    const chunk = manifest.chunks[index]!;
    requireThat(
      chunk.mediaType === "application/octet-stream",
      "chat_evidence_mismatch",
      "An evidence chunk has another media type.",
    );
    const bytes = store.localBlob(
      "chat-bridge/evidence-chunk",
      chunk.object,
      parentId,
    );
    requireThat(
      bytes.length ===
        Math.min(chunkBytes, manifest.byteLength - index * chunkBytes) &&
        bytes.length === chunk.byteLength &&
        digest(bytes) === chunk.contentHash,
      "chat_evidence_mismatch",
      "Evidence bytes differ from their exact ordered length and hash.",
    );
    parts.push(bytes);
  }
  const body = Buffer.concat(parts);
  requireThat(
    body.length === manifest.byteLength &&
      digest(body) === manifest.contentHash,
    "chat_evidence_mismatch",
    "The complete evidence body has another hash or length.",
  );
  let decoded: string, value: unknown;
  try {
    decoded = new TextDecoder("utf-8", { fatal: true }).decode(body);
    value = JSON.parse(decoded);
  } catch {
    throw new IvyError(
      "chat_evidence_mismatch",
      "The evidence body is not valid UTF-8 JSON.",
    );
  }
  requireThat(
    canonical(value, maximumEvidenceBytes) === decoded,
    "chat_evidence_mismatch",
    "Evidence must retain canonical JSON bytes.",
  );
  validateBody(format, value);
  return value;
}
