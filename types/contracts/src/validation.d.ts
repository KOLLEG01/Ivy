/** Full contract validation facade retained for public SDK/tooling compatibility. */
export { wireSchema, operationSchema, transportSchema, operations, validateRequest, validateInput, validateOutput, validateUiDefinition, validateShared, validateTransport, } from './core-validation.js';
export { componentSchema, validateComponent } from './component-validation.js';
export { hostSchema, validateHost } from './host-validation.js';
export { agentSchema, validateAgent } from './agent-validation.js';
export { taskManagerSchema, validateTaskManager } from './task-manager-validation.js';
export { automationSchema, validateAutomation } from './automation-validation.js';
export { chatSchema, validateChat } from './chat-validation.js';
export { secretarySchema, validateSecretary } from './secretary-validation.js';
