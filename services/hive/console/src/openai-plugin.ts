import { strToU8, zipSync } from "fflate";

export const openaiPluginVersion = "1.0.2";

export function openaiAppId(input: string): string | null {
  const value = input.trim();
  const direct = /^(?:plugin_)?(asdk_app_[a-f0-9]{32})$/i.exec(value);
  if (direct) return direct[1]!.toLowerCase();
  try {
    const url = new URL(value);
    if (
      url.protocol !== "https:" ||
      url.hostname !== "chatgpt.com" ||
      !url.pathname.startsWith("/plugins")
    )
      return null;
    return (
      /(?:^|\/)(?:plugin_)?(asdk_app_[a-f0-9]{32})(?=$|[/?#])/i
        .exec(url.pathname + url.hash)?.[1]
        ?.toLowerCase() ?? null
    );
  } catch {
    return null;
  }
}

export function openaiPluginPackage(
  input: string,
  assets: { icon: Uint8Array; logo: Uint8Array },
): Uint8Array<ArrayBuffer> {
  const appId = openaiAppId(input);
  if (!appId) throw new Error("Invalid OpenAI app ID.");
  const name = "dev-" + appId.slice("asdk_app_".length);
  const description = "Ivy Hive workspace and host tools.";
  const json = (value: unknown) =>
    strToU8(JSON.stringify(value, null, 2) + "\n");
  return new Uint8Array(
    zipSync({
      ".app.json": json({ apps: { [name]: { id: appId } } }),
      ".codex-plugin/plugin.json": json({
        name,
        version: openaiPluginVersion,
        description,
        apps: "./.app.json",
        author: { name: "Ivy" },
        interface: {
          displayName: "Ivy",
          shortDescription: description,
          longDescription: description,
          developerName: "Ivy",
          category: "Productivity",
          capabilities: [],
          brandColor: "#1F2937",
          composerIcon: "./assets/icon.png",
          logo: "./assets/logo.png",
        },
      }),
      "assets/icon.png": assets.icon,
      "assets/logo.png": assets.logo,
    }),
  );
}
