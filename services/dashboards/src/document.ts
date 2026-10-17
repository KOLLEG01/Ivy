// Shared by the browser viewer and image renderer; contains no credentials.
import template from "./document-template.json" with { type: "json" };

export function dashboardDocument(html: string, channel: string): string {
  const token = JSON.stringify(channel).replaceAll("<", "\\u003c");
  return template[0]! + token + template[1]! + html + template[2]!;
}
