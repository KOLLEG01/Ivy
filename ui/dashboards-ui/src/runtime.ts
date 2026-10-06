import definition from "../ui.json";
import {
  serviceTools,
  newOperationId,
} from "../../../packages/sdk/src/client.js";
import type { Wire } from "../../../packages/sdk/src/client.js";
import { uiRuntime } from "../../../packages/ui-client/src/runtime";
export const { base, client, live } = uiRuntime(definition.metadata);
export const tr = (de: string, en: string) =>
  navigator.language.startsWith("de") ? de : en;
export async function call<T>(
  node: string,
  name: string,
  args: Record<string, unknown> = {},
  mutation = false,
): Promise<T> {
  return (await serviceTools(client, node, [
    { namespace: "dashboards", interfaceVersion: "1.0.0" },
  ]).call(
    "dashboards." + name,
    args as Wire.Json,
    mutation ? await newOperationId(client) : undefined,
  )) as T;
}
export interface Dashboard {
  title: string;
  html: string;
  refreshSeconds: number;
  metadata?: Record<string, Wire.Json>;
  imageToken?: string | null;
  sources: Record<
    string,
    { objectId: string } | { contractKey: string; limit?: number }
  >;
}
export interface Row {
  id: string;
  revision: number;
  url: string;
  imageUrl?: string | null;
  value: Dashboard;
}
export const starter = `<style>body { font: 24px system-ui; padding: 32px; } h1 { margin-top: 0; }</style>
<h1>My dashboard</h1><pre id="content"></pre>
<script>
dashboard.onUpdate(async ({ data, output }) => {
  document.getElementById('content').textContent = JSON.stringify(data, null, 2);
  // output: { mode: 'interactive' | 'image', width, height, colorMode }
});
</script>`;
