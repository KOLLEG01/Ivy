import test from "node:test";
import assert from "node:assert/strict";
import { effectiveNativeSettings } from "../packages/ui-client/src/native-effective-settings.js";

const models = {
  data: [
    {
      id: "server-model",
      model: "server-model",
      displayName: "Server model",
      isDefault: true,
      defaultReasoningEffort: "high",
      supportedReasoningEfforts: [
        { reasoningEffort: "low" },
        { reasoningEffort: "high" },
      ],
    },
    {
      id: "other-model",
      model: "other-model",
      displayName: "Other model",
      isDefault: false,
      defaultReasoningEffort: "low",
      supportedReasoningEfforts: [{ reasoningEffort: "low" }],
    },
    {
      id: "small-model",
      model: "small-model",
      displayName: "Small model",
      isDefault: false,
      defaultReasoningEffort: "none",
      supportedReasoningEfforts: [],
    },
  ],
};
const config = {
  config: {
    model: "server-model",
    model_reasoning_effort: "high",
    sandbox_mode: "workspace-write",
  },
};
test("effective composer defaults follow native configuration, server profiles and current thread settings", () => {
  assert.deepEqual(effectiveNativeSettings(models, config, null), {
    model: "server-model",
    effort: "high",
    mode: "default",
    permission: ":workspace",
  });
  const profile = {
    model: "other-model",
    effort: null,
    mode: "plan",
    permission: ":read-only",
  };
  assert.deepEqual(effectiveNativeSettings(models, config, null, profile), {
    model: "other-model",
    effort: "low",
    mode: "plan",
    permission: ":read-only",
  });
  assert.deepEqual(
    effectiveNativeSettings(
      models,
      config,
      {
        model: "server-model",
        effort: "low",
        collaborationMode: { mode: "default" },
        activePermissionProfile: { id: ":danger-full-access" },
      },
      profile,
    ),
    {
      model: "server-model",
      effort: "low",
      mode: "default",
      permission: ":danger-full-access",
    },
  );
  assert.equal(
    effectiveNativeSettings(models, config, { model: "small-model" }).effort,
    "",
  );
  assert.equal(
    effectiveNativeSettings(models, null, null).model,
    "server-model",
  );
  assert.equal(effectiveNativeSettings(null, null, null).model, "");
});
