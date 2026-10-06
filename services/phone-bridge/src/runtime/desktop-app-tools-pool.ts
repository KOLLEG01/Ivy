import { canonical, IvyError } from "../../../../packages/sdk/src/node.js";
import { CodexAppTools } from "../../../../packages/sdk/src/codex-app-tools.js";
import type { AppToolsSettings } from "../../../../packages/sdk/src/codex-app-tools.js";

/** Keep the Desktop tool catalogue and its MCP connection alive between calls.
 * A lease's close only releases call ownership; the flow closes the process. */
export class PhoneAppToolsPool {
  private readonly ports = new Map<string, { port: CodexAppTools; inFlight: number; retired: boolean; closing?: Promise<void> }>();
  private readonly all = new Set<{ port: CodexAppTools; inFlight: number; retired: boolean; closing?: Promise<void> }>();
  private closed = false;
  constructor(private readonly create: (settings: AppToolsSettings) => CodexAppTools = settings => new CodexAppTools(settings)) {}

  private retire(key: string, entry: { port: CodexAppTools; inFlight: number; retired: boolean; closing?: Promise<void> }): void {
    if (this.ports.get(key) === entry) this.ports.delete(key);
    entry.retired = true;
    if (entry.inFlight === 0 && !entry.closing)
      entry.closing = entry.port.close().catch(() => undefined).finally(() => this.all.delete(entry));
  }

  open(settings: AppToolsSettings): CodexAppTools {
    if (this.closed) throw new Error("Phone App Tools pool is closed.");
    const key = canonical(settings);
    let entry = this.ports.get(key);
    if (entry && (entry.port.isClosed || entry.port.isCurrentInstallation === false)) {
      this.retire(key, entry);
      entry = undefined;
    }
    if (!entry) {
      entry = { port: this.create(settings), inFlight: 0, retired: false };
      this.ports.set(key, entry);
      this.all.add(entry);
    }
    const selected = entry;
    const finish = () => {
      selected.inFlight--;
      if (selected.retired) this.retire(key, selected);
    };
    const failed = (error: unknown): never => {
      if (IvyError.from(error).code !== 'app_tools_voice_not_ready') {
        this.retire(key, selected);
      }
      throw error;
    };
    return new Proxy(selected.port, {
      get: (target, property) => {
        if (property === "close") return async () => undefined;
        const value = Reflect.get(target, property, target) as unknown;
        if (typeof value !== 'function') return value;
        return (...args: unknown[]) => {
          if (this.closed || selected.retired)
            return Promise.reject(new IvyError('app_tools_closed', 'This Phone App Tools connection was retired.'));
          selected.inFlight++;
          try {
            const result = Reflect.apply(value, target, args) as unknown;
            if (result instanceof Promise)
              return result.then(value => { finish(); return value; }, error => {
                try { failed(error); } finally { finish(); }
              });
            finish();
            return result;
          } catch (error) {
            try { failed(error); } finally { finish(); }
          }
        };
      },
    });
  }

  async prewarm(settings: AppToolsSettings): Promise<void> {
    const port = this.open(settings);
    await port.verifyActor();
  }

  async close(): Promise<void> {
    if (this.closed) return;
    this.closed = true;
    const entries = [...this.all];
    this.ports.clear();
    await Promise.all(entries.map(entry => {
      entry.retired = true;
      entry.closing ??= entry.port.close().catch(() => undefined).finally(() => this.all.delete(entry));
      return entry.closing;
    }));
  }
}
