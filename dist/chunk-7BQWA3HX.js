import {
  canonical
} from "./chunk-T2KPXKB3.js";

// packages/contracts/src/canonical.ts
import { createHash } from "node:crypto";
var digest = (bytes) => "sha256:" + createHash("sha256").update(bytes).digest("hex");
var hashJson = (value) => digest(canonical(value));

export {
  digest,
  hashJson
};
//# sourceMappingURL=chunk-7BQWA3HX.js.map
