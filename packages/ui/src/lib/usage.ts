/** One used share: more fill always means more used. `used` null is unknown, never 0%. */
export interface UsageMeter { id: string; caption: string; label: string; used: number | null; detail?: string | undefined }
/** A captioned group, e.g. one account's limit windows; system meters carry their own captions. */
export interface UsageMeterGroup { id: string; caption?: string; meters: UsageMeter[] }
export type UsagePillLevel = "full" | "meters" | "mark";
export const usageTone = (used: number | null) =>
  used === null ? "" : used >= 90 ? "bg-destructive" : used >= 75 ? "bg-amber-500" : "bg-foreground/60";
export const meterText = (meter: UsageMeter) =>
  meter.label + ": " + (meter.used === null ? (meter.detail ?? "unknown") : meter.used + "% used" + (meter.detail ? " · " + meter.detail : ""));
