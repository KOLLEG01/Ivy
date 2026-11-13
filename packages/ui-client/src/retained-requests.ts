/** Outstanding requests, not a cache. Admission never evicts an unresolved original. */
export class RetainedRequests<T extends { operationId: string }> {
  static readonly maximumCount = 16;
  static readonly maximumBytes = 2 * 1024 * 1024;
  constructor(
    private readonly key: string,
    private readonly storage: Storage = sessionStorage,
  ) {}

  read(): Record<string, T> {
    const raw = this.storage.getItem(this.key);
    if (!raw) return {};
    const records = JSON.parse(raw) as Record<string, T>;
    if (
      !records ||
      Array.isArray(records) ||
      typeof records !== "object" ||
      Object.keys(records).length > RetainedRequests.maximumCount ||
      new TextEncoder().encode(raw).byteLength >
        RetainedRequests.maximumBytes ||
      Object.values(records).some(
        (value) =>
          !value || typeof value.operationId !== "string" || !value.operationId,
      )
    )
      throw new Error(
        "The original requests cannot be read. Restore this tab's storage before sending.",
      );
    return records;
  }

  retain(scope: string, value: T) {
    const records = this.read();
    if (records[scope] && records[scope].operationId !== value.operationId)
      throw new Error(
        "Check the original pending request before making another change.",
      );
    records[scope] = value;
    const raw = JSON.stringify(records);
    if (
      Object.keys(records).length > RetainedRequests.maximumCount ||
      new TextEncoder().encode(raw).byteLength > RetainedRequests.maximumBytes
    )
      throw new Error(
        "This tab's pending requests are full. Confirm an original request before sending another.",
      );
    // Storage failure prevents dispatch, including when browser quota is smaller.
    this.storage.setItem(this.key, raw);
  }

  complete(scope: string, operationId: string) {
    const records = this.read();
    if (records[scope]?.operationId !== operationId)
      throw new Error("The original pending request changed.");
    delete records[scope];
    if (Object.keys(records).length)
      this.storage.setItem(this.key, JSON.stringify(records));
    else this.storage.removeItem(this.key);
  }
}
