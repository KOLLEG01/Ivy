const tones = [
  "bg-sky-500/15 text-sky-700 dark:text-sky-300",
  "bg-violet-500/15 text-violet-700 dark:text-violet-300",
  "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300",
  "bg-amber-500/20 text-amber-800 dark:text-amber-300",
  "bg-rose-500/15 text-rose-700 dark:text-rose-300",
  "bg-teal-500/15 text-teal-700 dark:text-teal-300",
  "bg-indigo-500/15 text-indigo-700 dark:text-indigo-300",
  "bg-orange-500/15 text-orange-700 dark:text-orange-300",
];

/** One host keeps the same tint in navigation, composers and usage indicators. */
export function hostTone(seed: string): string {
  let hash = 0;
  for (const char of seed) hash = (hash * 31 + char.codePointAt(0)!) >>> 0;
  return tones[hash % tones.length]!;
}
export const hostInitial = (label: string) => label.match(/[\p{L}\p{N}]/u)?.[0]?.toLocaleUpperCase() ?? "?";
