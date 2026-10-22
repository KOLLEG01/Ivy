import type { Agent } from "../../contracts/src/generated.js";
import type { RpcClient, RequestOptions } from "./client.js";
/** Project identities belong to their host; retained inventory also covers offline hosts. */
export declare function hostProject(client: RpcClient, hostId: string | undefined, projectId: string, options?: RequestOptions): Promise<Agent.ProjectSummary | null>;
