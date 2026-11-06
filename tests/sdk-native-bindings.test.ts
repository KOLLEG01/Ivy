import test from "node:test";
import assert from "node:assert/strict";
import {
  nativeBackendTools,
  nativeServiceTools,
} from "../packages/sdk/src/client.js";
import type { RpcClient } from "../packages/sdk/src/client.js";

test("native binding caches separate legacy, backend and account identities", async () => {
  const hashes = ["a", "b", "c"].map((value) => "sha256:" + value.repeat(64));
  let discoveries = 0;
  const client: RpcClient = {
    async request(method, params) {
      assert.equal(method, "tools.list", "No invocation is authorized by discovery.");
      assert.deepEqual(params, {
        namespace: "codex",
        serviceNodeId: "native-agent",
        namePrefix: "config/read",
      });
      return {
        provider: { node: { serviceNodeId: "native-agent" } },
        items: [{
          qualifiedName: "codex.config/read",
          definitionHash: hashes[discoveries++],
          catalogHash: "sha256:" + "d".repeat(64),
          definition: {
            namespace: "codex",
            name: "config/read",
            interfaceVersion: "0.163.0",
            inputSchema: {},
            outputSchema: {},
          },
        }],
        nextCursor: null,
      } as never;
    },
  };
  const legacy = nativeServiceTools(client, "native-agent");
  await legacy.binding("codex.config/read", hashes[0]!);
  for (const [index, accountId] of ["default", "other"].entries()) {
    const backend = { backendId: "codex", provider: "codex", accountId } as const;
    const scoped = nativeBackendTools(client, "native-agent", backend);
    await scoped.binding("codex.config/read", hashes[index + 1]!);
    assert.notEqual(scoped, legacy);
    assert.equal(nativeBackendTools(client, "native-agent", { ...backend }), scoped);
    await assert.rejects(scoped.binding("codex.config/read", hashes[0]!), {
      code: "tool_definition_changed",
    });
  }
  await legacy.binding("codex.config/read", hashes[0]!);
  assert.equal(discoveries, 3);
});
