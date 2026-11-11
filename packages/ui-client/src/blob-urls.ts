/** Own verified Blob URLs by their encoded byte size; callers may pin the open preview. */
export class BlobUrls {
  private readonly entries = new Map<string, { url: string; bytes: number }>();
  private bytes = 0;
  private pinned: string | null = null;
  constructor(private readonly removed: (key: string) => void, private readonly maximumBytes = 24 * 1024 * 1024, private readonly maximumEntries = 64) {}
  get(key: string): string | null {
    const entry = this.entries.get(key);
    if (!entry) return null;
    this.entries.delete(key); this.entries.set(key, entry);
    return entry.url;
  }
  pin(key: string | null) { this.pinned = key; }
  private remove(key: string) {
    const entry = this.entries.get(key);
    if (!entry) return;
    this.entries.delete(key); this.bytes -= entry.bytes;
    URL.revokeObjectURL(entry.url); this.removed(key);
  }
  put(key: string, file: Blob): string {
    if (file.size > this.maximumBytes) throw new Error('The image exceeds the retained image budget.');
    let required = this.bytes - (this.entries.get(key)?.bytes ?? 0) + file.size;
    let count = this.entries.size + (this.entries.has(key) ? 0 : 1);
    const evicted: string[] = [];
    for (const [candidate, entry] of this.entries) {
      if (required <= this.maximumBytes && count <= this.maximumEntries) break;
      if (candidate === key || candidate === this.pinned) continue;
      evicted.push(candidate); required -= entry.bytes; count--;
    }
    if (required > this.maximumBytes || count > this.maximumEntries) throw new Error('Close the open image before loading another.');
    // Admission is checked before changing an existing image or evicting its neighbours.
    const url = URL.createObjectURL(file);
    for (const candidate of evicted) this.remove(candidate);
    this.remove(key);
    this.entries.set(key, { url, bytes: file.size }); this.bytes += file.size;
    return url;
  }
  clear() { for (const key of this.entries.keys()) this.remove(key); this.pinned = null; }
}
