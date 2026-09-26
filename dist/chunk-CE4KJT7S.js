import {
  canonical
} from "./chunk-BO4WKKA7.js";

// packages/contracts/src/canonical.ts
import { createHash } from "node:crypto";
var digest = (bytes) => "sha256:" + createHash("sha256").update(bytes).digest("hex");
var hashJson = (value) => digest(canonical(value));

export {
  digest,
  hashJson
};
//# sourceMappingURL=chunk-CE4KJT7S.js.map
