import {
  canonical
} from "./chunk-XW75TK4D.js";

// packages/contracts/src/canonical.ts
import { createHash } from "node:crypto";
var digest = (bytes) => "sha256:" + createHash("sha256").update(bytes).digest("hex");
var hashJson = (value) => digest(canonical(value));

export {
  digest,
  hashJson
};
//# sourceMappingURL=chunk-LGOCXC6T.js.map
