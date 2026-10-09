import catalog from "../../../specs/schemas/operations.json" with { type: "json" };

/** Routed writes require their own operation identity; subscriptions use their owning socket. */
export function isReadRequest(
  method: string,
  params: Record<string, unknown>,
): boolean {
  if (method === "notifications.subscribe") return false;
  if (method === "tools.call" || method === "discovery.call")
    return params["operationId"] === undefined;
  const definition = (
    catalog.operations as Record<string, { access: string; mutation: boolean }>
  )[method];
  return !!definition && definition.access === "client" && !definition.mutation;
}
