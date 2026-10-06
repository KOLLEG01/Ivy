import { downloadContentFromMessage, normalizeMessageContent } from "baileys";
import type { WAMessage } from "baileys";
import {
  mkdir,
  readFile,
  writeFile,
  readdir,
  stat,
  rm,
} from "node:fs/promises";
import { join, basename, resolve, dirname } from "node:path";
import { voiceWav } from "./audio.js";
import { digest } from "../../../../packages/sdk/src/node.js";
import { requireThat } from "../../../../packages/sdk/src/node.js";
import type { Chat } from "../../../../packages/sdk/src/node.js";
import type { ChatBridge } from "../bridge.js";
import type { WhatsAppConfig } from "./config.js";
import { decode } from "./auth.js";
import type { Inbox, WhatsAppJournal } from "./journal.js";
import { scopedOperationId } from "../../../../packages/sdk/src/client.js";
import { translator } from "./messages.js";
import type { Translator } from "./messages.js";
export function messageText(message: WAMessage): string {
  const content = normalizeMessageContent(message.message);
  return (
    content?.conversation ??
    content?.extendedTextMessage?.text ??
    content?.imageMessage?.caption ??
    content?.documentMessage?.caption ??
    ""
  );
}
export function commandText(message: WAMessage): string {
  const content = normalizeMessageContent(message.message);
  return content?.conversation ?? content?.extendedTextMessage?.text ?? "";
}
export async function pruneMedia(
  dataRoot: string,
  journal: WhatsAppJournal,
): Promise<void> {
  const root = resolve(dataRoot, "whatsapp-media");
  let entries;
  try {
    entries = await readdir(root, { withFileTypes: true });
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return;
    throw error;
  }
  for (const entry of entries) {
    if (
      !entry.isDirectory() ||
      entry.isSymbolicLink() ||
      !/^[a-f0-9]{64}$/.test(entry.name)
    )
      continue;
    const path = resolve(root, entry.name);
    requireThat(
      dirname(path) === root,
      "whatsapp_media_invalid",
      "Media cleanup must stay inside transport storage.",
    );
    if (
      Date.now() - (await stat(path)).mtimeMs < 7 * 86400_000 ||
      journal.db
        .prepare("SELECT 1 FROM inbox WHERE id=? AND state='pending'")
        .get(entry.name)
    )
      continue;
    await rm(path, { recursive: true, force: true });
    journal.set("local-input:" + entry.name, null);
  }
}
export async function mediaPayload(
  row: Inbox,
  bridge: ChatBridge,
  journal: WhatsAppJournal,
  config: WhatsAppConfig,
  dataRoot: string,
  sameHost: boolean,
  t: Translator = translator(),
): Promise<Chat.Payload> {
  const saved = journal.get<Chat.Payload>("payload:" + row.id);
  if (saved) return saved;
  const message = decode<WAMessage>(row.message),
    content = normalizeMessageContent(message.message);
  let text = messageText(message);
  const images: Chat.Image[] = [];
  const media =
    content?.imageMessage ?? content?.audioMessage ?? content?.documentMessage;
  if (media) {
    const maximum = content?.imageMessage
      ? sameHost
        ? 4 * 1024 * 1024
        : 2097152
      : 32 * 1024 * 1024;
    const audio = !!content?.audioMessage,
      declaredLength = Number(media.fileLength);
    // Protobuf toJSON emits byte fields as base64 strings before BufferJSON runs.
    const checksum = media.fileSha256
      ? "sha256:" +
        (typeof media.fileSha256 === "string"
          ? Buffer.from(media.fileSha256, "base64")
          : Buffer.from(media.fileSha256)
        ).toString("hex")
      : null;
    requireThat(
      (audio || (Number.isSafeInteger(declaredLength) && declaredLength > 0)) &&
        (!Number.isFinite(declaredLength) || declaredLength <= maximum),
      "whatsapp_media_limit",
      t("media.declared_limit", { mib: maximum / 1024 / 1024 }),
    );
    const directory = join(dataRoot, "whatsapp-media", row.id);
    await mkdir(directory, { recursive: true, mode: 0o700 });
    const original = join(directory, "original");
    let bytes: Buffer;
    try {
      bytes = await readFile(original);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
      // Select the admitted original explicitly: downloadMediaMessage can select
      // thumbnailDirectPath when a media message has directPath but no url.
      const stream = await downloadContentFromMessage(
        media,
        content?.imageMessage ? "image" : audio ? "audio" : "document",
        { options: { signal: AbortSignal.timeout(60000) } },
      );
      const parts: Buffer[] = [];
      let length = 0;
      try {
        for await (const chunk of stream) {
          const part = Buffer.from(chunk);
          length += part.length;
          requireThat(
            length <= maximum,
            "whatsapp_media_limit",
            t("media.too_large"),
          );
          parts.push(part);
        }
      } finally {
        stream.destroy();
      }
      bytes = Buffer.concat(parts);
      requireThat(bytes.length > 0, "whatsapp_media_invalid", t("media.empty"));
      if (!audio)
        requireThat(
          bytes.length === declaredLength &&
            (!checksum || digest(bytes) === checksum),
          "whatsapp_media_invalid",
          t("media.checksum"),
        );
      await writeFile(original, bytes, { flag: "wx", mode: 0o600 });
    }
    requireThat(
      bytes.length > 0 && bytes.length <= maximum,
      "whatsapp_media_limit",
      t("media.limit"),
    );
    // WhatsApp voice metadata is not stable across message variants. Successful Ogg/Opus
    // decoding and the decoded duration below are the authoritative audio validation.
    if (!audio)
      requireThat(
        bytes.length === declaredLength &&
          (!checksum || digest(bytes) === checksum),
        "whatsapp_media_invalid",
        t("media.checksum"),
      );
    if (content?.imageMessage) {
      const type = content.imageMessage.mimetype;
      requireThat(
        type === "image/png" || type === "image/jpeg" || type === "image/webp",
        "whatsapp_media_unsupported",
        t("media.image_types"),
      );
      if (sameHost) {
        journal.set("local-input:" + row.id, [
          {
            type: "image",
            url: "data:" + type + ";base64," + bytes.toString("base64"),
          },
        ]);
        text = text || t("media.image_caption");
      } else {
        const main = await bridge.main.queue.main();
        const request = {
          mutationId: await scopedOperationId(bridge.main.store.client, [
            "whatsapp-image",
            row.id,
          ]),
          contractVersion: "1.0.0",
          references: {},
          create: {
            contractKey:
              "chat-bridge/image-" +
              (type === "image/jpeg"
                ? "jpeg"
                : type === "image/png"
                  ? "png"
                  : "webp"),
            parentId: bridge.main.store.rootObjectId,
            ownerObjectId: main.value.conversation.objectId,
            name: "WhatsApp image " + row.id,
          },
          content: {
            encoding: "base64" as const,
            value: bytes.toString("base64"),
          },
        };
        const result = await bridge.main.store.client.request(
          "objects.write",
          request,
        );
        images.push({
          object: {
            objectId: result.object.id,
            revision: result.revision.revision,
          },
          contentHash: digest(bytes),
          byteLength: bytes.length,
          mediaType: type,
          label: "WhatsApp image",
        });
      }
    } else if (content?.audioMessage) {
      requireThat(
        config.transcription,
        "whatsapp_transcription_unconfigured",
        t("media.transcription_missing"),
      );
      const cached = journal.get<string>("transcript:" + row.id);
      if (cached !== null)
        text = t("media.transcription_prefix", { transcript: cached });
      else {
        const settings = config.transcription;
        const declaredSeconds = Number(content.audioMessage.seconds);
        requireThat(
          !Number.isFinite(declaredSeconds) || declaredSeconds <= 600,
          "whatsapp_media_limit",
          t("media.voice_declared_limit"),
        );
        const wavBytes = await voiceWav(bytes, t);
        requireThat(
          wavBytes.length <= 600 * 32000 + 4096,
          "whatsapp_media_limit",
          t("media.voice_actual_limit"),
        );
        const body = new FormData();
        body.set(
          "file",
          new Blob([new Uint8Array(wavBytes)], { type: "audio/wav" }),
          "audio.wav",
        );
        body.set("model", settings.model);
        body.set("response_format", "json");
        if (settings.language) body.set("language", settings.language);
        const url = new URL(
          settings.baseUrl.replace(/\/$/, "") + "/audio/transcriptions",
        );
        requireThat(
          url.protocol === "https:",
          "invalid_arguments",
          t("media.transcription_https"),
        );
        const response = await fetch(url, {
          method: "POST",
          headers: { Authorization: "Bearer " + settings.apiKey },
          body,
          signal: AbortSignal.timeout(180000),
          redirect: "error",
        });
        requireThat(
          response.ok,
          "whatsapp_transcription_failed",
          t("media.transcription_failed"),
        );
        const result = (await response.json()) as { text?: unknown };
        const transcript =
          typeof result.text === "string" ? result.text.trim() : "";
        requireThat(
          !!transcript && [...transcript].length <= 65000,
          "whatsapp_transcription_failed",
          t("media.transcription_invalid"),
        );
        journal.set("transcript:" + row.id, transcript);
        text = t("media.transcription_prefix", { transcript });
      }
    } else {
      requireThat(
        sameHost,
        "whatsapp_attachment_owner_mismatch",
        t("media.document_host"),
      );
      const name =
        basename(content!.documentMessage!.fileName ?? "attachment")
          .replace(/[^\p{L}\p{N}._ -]/gu, "_")
          .slice(0, 120) || "attachment";
      const path = join(directory, "file-" + name);
      try {
        await writeFile(path, bytes, { flag: "wx", mode: 0o600 });
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
        requireThat(
          digest(await readFile(path)) === digest(bytes),
          "whatsapp_media_invalid",
          t("media.original_changed"),
        );
      }
      journal.set("local-input:" + row.id, [{ type: "mention", name, path }]);
      text = text || t("media.attachment", { name });
    }
  } else
    requireThat(
      !content?.videoMessage && !content?.stickerMessage && !!text,
      "whatsapp_media_unsupported",
      t("media.supported"),
    );
  requireThat(
    [...text].length <= 65536,
    "whatsapp_media_limit",
    t("media.message_limit"),
  );
  const payload = { text, images };
  journal.set("payload:" + row.id, payload);
  return payload;
}
