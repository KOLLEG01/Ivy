<script setup lang="ts">
import { computed, ref } from "vue";
import { Disclosure,
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  JsonTree,
  Input,
  PageHeader,
  RemoteState,
  StatusBadge,
  useRemote,
} from "@ivy/ui";
import {
  allMcpTools,
  mcpRequest,
  protocolVersion,
  type JsonObject,
} from "../mcp-client";

const surface = useRemote(async (signal) => {
  const [discovery, listed] = await Promise.all([
    mcpRequest("server/discover", {}, signal),
    allMcpTools(signal),
  ]);
  return {
    discovery,
    pages: listed.pages,
    tools: listed.items as JsonObject[],
  };
}, 0, ["services"]);
const filter = ref("");
const serviceName = (tool: JsonObject): string => {
  const metadata = tool._meta;
  const owner = metadata && typeof metadata === "object" && !Array.isArray(metadata)
    ? (metadata as JsonObject)["ivy/serviceName"] : undefined;
  return typeof owner === "string" ? owner : "other";
};
const services = computed(() => {
  const groups = new Map<string, JsonObject[]>();
  for (const tool of surface.value.value?.tools ?? []) {
    const name = serviceName(tool);
    const tools = groups.get(name) ?? [];
    tools.push(tool);
    groups.set(name, tools);
  }
  return [...groups].sort(([a], [b]) => a.localeCompare(b));
});
const filteredServices = computed(() =>
  services.value
    .map(
      ([name, tools]) =>
        [
          name,
          tools.filter((tool) =>
            [name, tool.name, tool.description]
              .join(" ")
              .toLowerCase()
              .includes(filter.value.toLowerCase()),
          ),
        ] as const,
    )
    .filter(([, tools]) => tools.length),
);
const refresh = () => void surface.refresh();
</script>

<template>
  <PageHeader
    title="MCP Tools"
    description="All published tools with complete schemas and service guides, grouped by their owner."
    :loading="surface.loading.value"
  />
  <RemoteState
    :loading="surface.loading.value"
    :error="surface.error.value"
    :has-data="!!surface.value.value"
    @retry="surface.refresh"
  />
  <template v-if="surface.value.value">
    <div class="mb-6 grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
      <Card
        ><CardHeader
          ><CardTitle class="text-sm">Protocol request</CardTitle></CardHeader
        ><CardContent class="font-mono text-xs">{{
          protocolVersion
        }}</CardContent></Card
      >
      <Card
        ><CardHeader
          ><CardTitle class="text-sm">Published tools</CardTitle></CardHeader
        ><CardContent class="text-2xl font-semibold">{{
          surface.value.value.tools.length
        }}</CardContent></Card
      >
      <Card
        ><CardHeader><CardTitle class="text-sm">Services</CardTitle></CardHeader
        ><CardContent class="text-2xl font-semibold">{{
          services.length
        }}</CardContent></Card
      >
    </div>

    <section class="mb-8 space-y-3" aria-labelledby="direct-heading">
      <h2 id="direct-heading" class="text-lg font-semibold">Direct tools</h2>
      <Input
        v-model="filter"
        aria-label="Filter tools"
        placeholder="Filter tools or services…"
      />
      <p v-if="!filteredServices.length" class="text-sm text-muted-foreground">
        No matching tools.
      </p>
      <section
        v-for="[name, tools] in filteredServices"
        :key="name"
        :data-service="name"
        class="space-y-2"
      >
        <h3 class="flex items-center gap-2 font-semibold">
          {{ name }} <StatusBadge :label="String(tools.length)" />
        </h3>
        <Disclosure
          v-for="tool in tools"
          :key="String(tool.name)"
          :data-tool="String(tool.name)"
         
         variant="card" trigger-class="min-w-0 break-all font-mono"><template #trigger>
            {{ tool.name }}
          </template>
          <p class="my-3 whitespace-pre-wrap text-sm text-muted-foreground">
            {{ tool.description }}
          </p>
          <JsonTree :value="tool" :label="String(tool.name)" :open-depth="1" />
        </Disclosure>
      </section>
    </section>
    <section class="mb-8 space-y-3" aria-labelledby="protocol-heading">
      <h2 id="protocol-heading" class="text-lg font-semibold">
        Protocol surface
      </h2>
      <p class="text-sm text-muted-foreground">
        This is the raw result data returned by <code>server/discover</code> and
        every paginated <code>tools/list</code> request, including cache hints,
        server metadata, security schemes, annotations and schemas.
      </p>
      <Disclosure variant="card" default-open title="server/discover">
        <JsonTree
          class="mt-3"
          :value="surface.value.value.discovery"
          label="result"
          :open-depth="2"
        />
      </Disclosure>
      <Disclosure variant="card" trigger-class="flex flex-wrap items-center gap-2"><template #trigger>
          tools/list pages
          <StatusBadge
            :label="`${surface.value.value.pages.length} page${surface.value.value.pages.length === 1 ? '' : 's'}`"
          />
        </template>
        <JsonTree
          class="mt-3"
          :value="surface.value.value.pages"
          label="pages"
          :open-depth="1"
        />
      </Disclosure>
    </section>
  </template>
</template>
