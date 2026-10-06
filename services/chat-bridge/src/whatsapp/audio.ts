import { Worker } from "node:worker_threads";
import { IvyError } from "../../../../packages/sdk/src/node.js";
import { translator } from "./messages.js";
import type { Translator } from "./messages.js";
export async function voiceWav(
  bytes: Buffer,
  t: Translator = translator(),
): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    const worker = new Worker(new URL("./audio-worker.js", import.meta.url), {
      workerData: bytes,
      stdout: true,
      stderr: true,
      resourceLimits: { maxOldGenerationSizeMb: 128 },
    });
    worker.stdout.resume();
    worker.stderr.resume();
    let done = false;
    const finish = (error: Error | null, wav?: Buffer) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      void worker.terminate();
      error ? reject(error) : resolve(wav!);
    };
    const timer = setTimeout(
      () => finish(new IvyError("whatsapp_audio_timeout", t("audio.timeout"))),
      30000,
    );
    worker.once("error", () =>
      finish(new IvyError("whatsapp_audio_invalid", t("audio.invalid"))),
    );
    worker.once("exit", () =>
      finish(new IvyError("whatsapp_audio_invalid", t("audio.incomplete"))),
    );
    worker.once("message", (result: { wav?: Uint8Array }) => {
      if (result.wav) finish(null, Buffer.from(result.wav));
      else finish(new IvyError("whatsapp_audio_invalid", t("audio.too_long")));
    });
  });
}
