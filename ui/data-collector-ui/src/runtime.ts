import definition from '../ui.json';
import { uiRuntime } from "../../../packages/ui-client/src/runtime";

export const { base, client, live } = uiRuntime(definition.metadata);
