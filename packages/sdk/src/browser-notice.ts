import { scopedOperationId } from "./client.js";
import type { RpcClient, Wire } from "./client.js";
import type { BrowserNotice } from "../../contracts/src/browser-notice.js";
export { browserNoticeTopic } from "../../contracts/src/browser-notice.js";
export type { BrowserNotice } from "../../contracts/src/browser-notice.js";

/** Register browserNoticeTopic(namespace), then publish through the service's connection. */
export async function publishBrowserNotice(
  client: RpcClient,
  namespace: string,
  notice: BrowserNotice,
): Promise<void> {
  const payload = {
    ...notice,
    title: notice.title.slice(0, 100),
    body: notice.body.slice(0, 300),
  };
  await client.request("events.publish", {
    topic: namespace + ".browser-notification",
    topicVersion: "1.0.0",
    payload: payload as unknown as Wire.Json,
    mutationId: await scopedOperationId(client, [
      namespace,
      "browser-notification",
      notice.tag,
    ]),
  });
}
