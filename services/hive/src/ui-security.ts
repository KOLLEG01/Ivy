/** Published *.sandbox.html assets run scripts but never inherit the Hive origin. Other UI assets may be framed only by Ivy itself. */
export function uiContentPolicy(assetPath = ""): string {
  return assetPath.endsWith(".sandbox.html")
    ? "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; connect-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'"
    : "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data: blob:; font-src 'self'; base-uri 'none'; object-src 'none'; form-action *; worker-src 'self'; manifest-src 'self'" +
        (assetPath ? "; frame-ancestors 'self'; frame-src 'self'" : "; frame-ancestors 'none'");
}
