import { requireThat } from "../../../packages/contracts/src/errors.js";
import type {
  OperationName,
  Params,
  Result,
} from "../../../packages/contracts/src/generated.js";

type Request = <M extends OperationName>(
  method: M,
  params: Params<M>,
) => Result<M>;

/** Wiki owns page semantics; object storage continues to own CAS and mutation receipts. */
export function wikiHandlers(request: Request, update: (params: Params<"wiki.update">) => Result<"wiki.update">) {
  const page = (selector: Params<"objects.stat">) => {
    const object = request("objects.stat", selector);
    requireThat(
      object.contractKey === "wiki/page",
      "invalid_arguments",
      "Select a Wiki page. Use Hive object tools for other data.",
    );
    return object;
  };
  const parent = (parentId: string | null | undefined) => {
    if (parentId) page({ objectId: parentId });
  };
  return {
    "wiki.search": (params: Params<"wiki.search">) => {
      parent(params.rootId);
      return request("objects.search", { ...params, contractKey: "wiki/page" });
    },
    "wiki.list": ({ parentId, ...params }: Params<"wiki.list">) => {
      parent(parentId);
      return request("objects.query", {
        ...params,
        contractKey: "wiki/page",
        where: parentId
          ? { op: "eq", field: "object.parentId", value: parentId }
          : { op: "isNull", field: "object.parentId" },
        select: ["object.name", "object.path", "object.effectivelyArchived"],
        orderBy: [{ field: "object.name", direction: "asc" }],
      });
    },
    "wiki.read": (params: Params<"wiki.read">) => {
      page(
        params.objectId
          ? { objectId: params.objectId }
          : { path: params.path! },
      );
      return request("objects.read", params);
    },
    "wiki.create": (params: Params<"wiki.create">) => {
      parent(params.parentId);
      return request("objects.write", {
        mutationId: params.mutationId,
        contractVersion: "1.0.0",
        references: {},
        create: {
          contractKey: "wiki/page",
          name: params.title,
          parentId: params.parentId ?? null,
          ownerObjectId: null,
        },
        content: { encoding: "text", value: params.markdown },
      });
    },
    "wiki.update": (params: Params<"wiki.update">) => {
      const current = page({ objectId: params.objectId });
      requireThat(
        current.contractVersion === "1.0.0",
        "contract_version_conflict",
        "This Wiki page version is not writable.",
      );
      return update(params);
    },
    "wiki.history": (params: Params<"wiki.history">) => {
      page({ objectId: params.objectId });
      return request("objects.history", params);
    },
    "wiki.move": (params: Params<"wiki.move">) => {
      page({ objectId: params.objectId });
      parent(params.parentId);
      return request("objects.move", params);
    },
    "wiki.archive": (params: Params<"wiki.archive">) => {
      page({ objectId: params.objectId });
      return request("objects.archive", params);
    },
  };
}
