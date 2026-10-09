<script setup lang="ts">
import { computed, ref } from "vue";
import { Copy, Download } from "@lucide/vue";
import {
  Button,
  Disclosure,
  Field,
  FieldDescription,
  FieldError,
  FieldGroup,
  FieldLabel,
  Input,
  SettingsSection,
} from "@ivy/ui";
import { localeText as tr } from "../../../../packages/ui-client/src/runtime";
import iconUrl from "../../chatgpt-plugin/assets/icon.png?url&no-inline";
import logoUrl from "../../chatgpt-plugin/assets/logo.png?url&no-inline";
import { consoleBase } from "./runtime";
import {
  openaiAppId,
  openaiPluginPackage,
  openaiPluginVersion,
} from "./openai-plugin";

const mcpUrl = new URL("mcp", consoleBase).href;
const appId = ref("");
const normalizedId = computed(() => openaiAppId(appId.value));
const invalid = computed(() => !!appId.value.trim() && !normalizedId.value);
const busy = ref(false),
  copied = ref(false),
  error = ref<string | null>(null);

async function copyUrl() {
  error.value = null;
  try {
    await navigator.clipboard.writeText(mcpUrl);
    copied.value = true;
  } catch {
    error.value = tr(
      "Die URL konnte nicht kopiert werden. Markiere und kopiere sie im Feld.",
      "The URL could not be copied. Select and copy it from the field.",
    );
  }
}

async function download() {
  if (!normalizedId.value || busy.value) return;
  const id = normalizedId.value;
  busy.value = true;
  error.value = null;
  try {
    const [icon, logo] = await Promise.all(
      [iconUrl, logoUrl].map(async (url) => {
        const response = await fetch(url);
        if (!response.ok) throw new Error("Plugin asset unavailable.");
        return new Uint8Array(await response.arrayBuffer());
      }),
    );
    const bytes = openaiPluginPackage(id, { icon: icon!, logo: logo! });
    const url = URL.createObjectURL(
      new Blob([bytes], { type: "application/zip" }),
    );
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = `ivy-openai-plugin-${openaiPluginVersion}.zip`;
    anchor.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  } catch {
    error.value = tr(
      "Das Plugin-Paket konnte nicht erstellt werden. Bitte versuche es erneut.",
      "The plugin package could not be created. Please try again.",
    );
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <SettingsSection
    class="mb-6"
    :title="tr('OpenAI anbinden', 'Connect OpenAI')"
    :description="
      tr(
        'Verbinde diesen Hive mit ChatGPT und Codex.',
        'Connect this Hive to ChatGPT and Codex.',
      )
    "
  >
    <form class="space-y-5" @submit.prevent="download">
      <FieldGroup>
        <Field>
          <FieldLabel for="openai-mcp-url">MCP-URL</FieldLabel>
          <div class="flex flex-col gap-2 sm:flex-row">
            <Input
              id="openai-mcp-url"
              :model-value="mcpUrl"
              readonly
              class="min-w-0 font-mono text-xs"
            />
            <Button type="button" variant="outline" @click="copyUrl">
              <Copy aria-hidden="true" />
              <span aria-live="polite">{{
                copied
                  ? tr("Kopiert", "Copied")
                  : tr("URL kopieren", "Copy URL")
              }}</span>
            </Button>
          </div>
        </Field>
        <Field :data-invalid="invalid || undefined">
          <FieldLabel for="openai-app-id">OpenAI App-ID</FieldLabel>
          <Input
            id="openai-app-id"
            v-model="appId"
            required
            autocomplete="off"
            autocapitalize="none"
            :spellcheck="false"
            :disabled="busy"
            :aria-invalid="invalid"
            :aria-describedby="
              invalid
                ? 'openai-app-id-help openai-app-id-error'
                : 'openai-app-id-help'
            "
            placeholder="asdk_app_…"
            class="font-mono text-xs"
          />
          <FieldDescription id="openai-app-id-help">
            {{
              tr(
                "Füge die App-ID oder die Adresse der ChatGPT-Plugin-Detailseite ein. Die ID wird von OpenAI beim Anlegen der Verbindung vergeben.",
                "Paste the app ID or the ChatGPT plugin detail URL. OpenAI assigns the ID when you create the connection.",
              )
            }}
          </FieldDescription>
          <FieldError v-if="invalid" id="openai-app-id-error">
            {{
              tr(
                "Erwartet wird asdk_app_ oder plugin_asdk_app_ mit anschließend 32 Hex-Zeichen, oder die zugehörige ChatGPT-Plugin-URL.",
                "Enter asdk_app_ or plugin_asdk_app_ followed by 32 hexadecimal characters, or the corresponding ChatGPT plugin URL.",
              )
            }}
          </FieldError>
        </Field>
      </FieldGroup>
      <div class="flex flex-wrap items-center gap-3">
        <Button type="submit" :disabled="!normalizedId || busy">
          <Download aria-hidden="true" />
          {{
            busy
              ? tr("Paket wird erstellt…", "Creating package…")
              : tr("Plugin-Paket herunterladen", "Download plugin package")
          }}
        </Button>
        <span class="text-xs text-muted-foreground"
          >{{ tr("Paketversion", "Package version") }}
          {{ openaiPluginVersion }} · ZIP</span
        >
      </div>
      <FieldError v-if="error">{{ error }}</FieldError>
      <Disclosure default-open :title="tr('Kurzanleitung', 'Setup guide')">
        <ol class="list-decimal space-y-3 pl-5 text-sm">
          <li>
            {{ tr("Öffne", "Open") }}
            <a
              href="https://chatgpt.com/plugins"
              target="_blank"
              rel="noopener noreferrer"
              class="underline underline-offset-4"
              >ChatGPT → Plugins</a
            >.
            {{
              tr(
                "Wähle + → Add custom MCP server (je nach Oberfläche Create app). Falls die Option fehlt, aktiviere den Developer Mode in den ChatGPT-Einstellungen.",
                "Choose + → Add custom MCP server (Create app in some interfaces). If the option is missing, enable Developer mode in ChatGPT settings.",
              )
            }}
          </li>
          <li>
            {{
              tr(
                "Verwende den Namen Ivy, die MCP-URL oben und OAuth als Anmeldung. Erstelle die Verbindung als Plugin und melde dich im Ivy-Dialog mit deinem Hive-Zugangsschlüssel an.",
                "Use the name Ivy, the MCP URL above, and OAuth authentication. Create the connection as a plugin, then sign in through the Ivy dialog with your Hive credential.",
              )
            }}
          </li>
          <li>
            {{
              tr(
                "Öffne die Detailseite des Ivy-Plugins. Kopiere die Adresse oder die darin enthaltene ID plugin_asdk_app_… und füge sie oben ein.",
                "Open the Ivy plugin detail page. Copy its URL or the plugin_asdk_app_… ID within it and paste it above.",
              )
            }}
          </li>
          <li>
            {{
              tr(
                "Lade das ZIP herunter. Wähle beim selben Ivy-Plugin Upload new version und lade das ZIP unverändert hoch.",
                "Download the ZIP. On the same Ivy plugin, choose Upload new version and upload the ZIP unchanged.",
              )
            }}
          </li>
          <li>
            {{
              tr(
                "Aktiviere Ivy und starte einen neuen Chat. Wähle @Ivy und frage zum Test: Welche Dienste sind in meinem Hive registriert?",
                "Enable Ivy and start a new chat. Select @Ivy and test it by asking: Which services are registered in my Hive?",
              )
            }}
          </li>
        </ol>
        <p class="mt-3 text-sm text-muted-foreground">
          <a
            href="https://developers.openai.com/plugins/build/plugins#create-and-test-a-plugin-locally-with-an-mcp-server"
            target="_blank"
            rel="noopener noreferrer"
            class="underline underline-offset-4"
            >{{
              tr("Offizielle OpenAI-Anleitung", "Official OpenAI setup guide")
            }}</a
          >
        </p>
      </Disclosure>
    </form>
  </SettingsSection>
</template>
