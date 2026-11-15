import parts from "./document-template.json" with { type: "json" };

// Shared by the browser viewer and image renderer; contains no credentials.
export function dashboardDocument(html: string, channel: string): string {
  const token = JSON.stringify(channel).replaceAll("<", "\\u003c");
  return parts[0] + token + parts[1] + html + parts[2];
}
