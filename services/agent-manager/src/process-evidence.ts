import { join } from "node:path";
import { atomicJson, jsonFile } from "../../../packages/host-runtime/src/config.js";
import { requireThat } from "../../../packages/sdk/src/node.js";

export interface OwnedProcessStop {
  epoch: string;
  observedAt: string;
}

const evidencePath = (dataRoot: string) => join(dataRoot, "owned-process-stops.json");

export async function ownedProcessStops(dataRoot: string): Promise<OwnedProcessStop[]> {
  let saved: unknown;
  try {
    saved = await jsonFile<unknown>(evidencePath(dataRoot), 64 * 1024);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return [];
    throw error;
  }
  requireThat(Array.isArray(saved) && saved.length <= 256 && saved.every(value =>
    value && typeof value === "object" && !Array.isArray(value) &&
    Object.keys(value).sort().join(",") === "epoch,observedAt" &&
    typeof value.epoch === "string" && value.epoch.length > 0 && value.epoch.length <= 256 &&
    typeof value.observedAt === "string" && Number.isFinite(Date.parse(value.observedAt))),
  "native_stop_evidence_invalid", "Stored native process stop evidence is invalid.");
  return saved as OwnedProcessStop[];
}

/** A Linux exit signal is insufficient while another process remains in the owned group. */
export async function ownedProcessTreeStopped(pid: number | undefined): Promise<boolean> {
  if (!pid) return false;
  if (process.platform === "win32") return true; // The Job launcher owns a kill-on-close Job Object.
  for (let attempt = 0; attempt < 10; attempt++) {
    try {
      process.kill(-pid, 0);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ESRCH") return true;
      return false;
    }
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  return false;
}

export async function retainOwnedProcessStop(dataRoot: string, epoch: string): Promise<void> {
  const now = new Date().toISOString();
  const previous = await ownedProcessStops(dataRoot);
  const retained = previous.filter(value => value.epoch !== epoch &&
    Date.parse(value.observedAt) > Date.now() - 14 * 24 * 60 * 60 * 1000);
  await atomicJson(evidencePath(dataRoot), [...retained, { epoch, observedAt: now }].slice(-256));
}
