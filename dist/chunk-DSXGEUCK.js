import {
  canonical
} from "./chunk-GPVLKTRL.js";

// packages/contracts/src/canonical.ts
import { createHash } from "node:crypto";
var digest = (bytes) => "sha256:" + createHash("sha256").update(bytes).digest("hex");
var hashJson = (value) => digest(canonical(value));

export {
  digest,
  hashJson
};
//# sourceMappingURL=chunk-DSXGEUCK.js.map
