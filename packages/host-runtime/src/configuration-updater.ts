import { canonical, hashJson } from "../../contracts/src/canonical.js";
import { IvyError, requireThat } from "../../contracts/src/errors.js";
import type { Host, Operation } from "../../contracts/src/generated.js";
import { HiveClient } from "../../sdk/src/client.js";
import { atomicJson } from "./config.js";
import { checkedHostConfig } from "./host-config.js";
import { HostJournal, requiresBootstrapReplacement } from "./journal.js";
import type { ConfigurationAdoption } from "./journal.js";
import { sameInstallation } from "./accepted-configuration.js";

function instanceInput(
  config: Host.HostConfig,
  instance: Host.Instance,
): string {
  const { enabled: _enabled, ...input } = instance;
  return canonical({ publicBaseUrl: config.publicBaseUrl, instance: input });
}

export interface ConfigurationSyncResult {
  changed: boolean;
  revision: number | null;
  contentHash: string | null;
  bootstrapRequired: string[];
}

export class ConfigurationUpdater {
  private readonly client: HiveClient;
  constructor(
    readonly configPath: string,
    readonly journal: HostJournal,
    readonly credential: string,
  ) {
    this.client = new HiveClient(journal.config.publicBaseUrl, { credential });
  }

  private remote(): Promise<Operation.HostConfiguration> {
    return this.client.request(
      "hostConfigurations.get",
      { hostId: this.journal.config.hostId },
      { timeoutMs: 10_000 },
    );
  }

  private async apply(
    value: ConfigurationAdoption,
  ): Promise<ConfigurationSyncResult> {
    if (hashJson(this.journal.config) !== value.contentHash) {
      await atomicJson(this.configPath, value.configuration);
      this.journal.useConfiguration(value.configuration);
    }
    for (const action of value.actions) this.journal.accept(action);
    this.journal.finishConfigurationAdoption(value);
    return {
      changed: true,
      revision: value.revision,
      contentHash: value.contentHash,
      bootstrapRequired: value.bootstrapRequired,
    };
  }

  async sync(): Promise<ConfigurationSyncResult> {
    const retained = this.journal.configurationAdoption();
    if (retained) return this.apply(retained);
    const remote = await this.remote();
    const previous = this.journal.config;
    requireThat(
      remote.hostId === previous.hostId &&
        remote.configuration.hostId === previous.hostId,
      "target_conflict",
      "Hive returned configuration for another host.",
    );
    requireThat(
      hashJson(remote.configuration) === remote.contentHash,
      "configuration_changed",
      "Hive configuration hash does not match its exact content.",
    );
    const next = await checkedHostConfig(remote.configuration, this.configPath);
    sameInstallation(previous, next);
    if (hashJson(previous) === hashJson(next)) {
      return {
        changed: false,
        revision: remote.revision,
        contentHash: hashJson(next),
        bootstrapRequired: [],
      };
    }
    requireThat(
      this.journal.unfinished().length === 0,
      "service_busy",
      "Host configuration waits for current deployments to finish.",
    );
    const installed = previous.instances.filter((instance) =>
      this.journal.installed(instance.instanceId),
    );
    requireThat(
      installed.every((instance) =>
        next.instances.some((value) => value.instanceId === instance.instanceId) ||
        this.journal.installed(instance.instanceId)?.enabled === false,
      ),
      "restart_required",
      "Installed instances must be disabled before removal from desired configuration.",
    );

    const actions: {
        instanceId: string;
        action: "restart" | "enable" | "disable";
      }[] = [],
      bootstrapRequired: string[] = [];
    for (const instance of next.instances) {
      const before = previous.instances.find(
          (value) => value.instanceId === instance.instanceId,
        ),
        retained = this.journal.installed(instance.instanceId);
      if (!before || !retained) continue;
      const enabledChanged = instance.enabled !== retained.enabled;
      const inputChanged =
        instanceInput(previous, before) !== instanceInput(next, instance);
      if (!enabledChanged && !inputChanged) continue;
      if (requiresBootstrapReplacement(instance)) {
        requireThat(
          !enabledChanged,
          "restart_required",
          "Changing a bootstrap owner enabled state requires explicit OS maintenance.",
        );
        bootstrapRequired.push(instance.instanceId);
        continue;
      }
      actions.push({
        instanceId: instance.instanceId,
        action: enabledChanged
          ? instance.enabled
            ? "enable"
            : "disable"
          : "restart",
      });
    }

    const adoption: ConfigurationAdoption = {
      configuration: next,
      revision: remote.revision,
      contentHash: hashJson(next),
      bootstrapRequired,
      actions: actions.map((action) => ({
        ...action,
        operationId:
          "configuration-" +
          hashJson({
            hostId: next.hostId,
            revision: remote.revision,
            ...action,
          }).slice(7, 47),
      })),
    };
    this.journal.stageConfigurationAdoption(adoption);
    return this.apply(adoption);
  }
}

export function configurationFailure(error: unknown): string {
  return IvyError.from(error).code;
}
