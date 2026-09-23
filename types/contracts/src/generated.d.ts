export declare namespace Wire {
    type Json = (null) | (boolean) | (number) | (string) | (Array<Wire.Json>) | ({
        [key: string]: Wire.Json;
    });
    type Identifier = string;
    type ContractVersion = string;
    type Hash = string;
    type ResourceRef = {
        "serviceNodeId": string;
        "namespace": string;
        "kind": string;
        "nativeId": string;
    };
    type Error = {
        "code": number;
        "message": string;
        "data": {
            "code": string;
            "outcome": "not_executed" | "unknown" | "completed";
            "details"?: Wire.Json;
        };
    };
    type Request = {
        "jsonrpc": "2.0";
        "id": (string) | (number);
        "method": string;
        "params": {
            [key: string]: Wire.Json;
        };
    };
    type Success = {
        "jsonrpc": "2.0";
        "id": (string) | (number);
        "result": Wire.Json;
    };
    type Failure = {
        "jsonrpc": "2.0";
        "id": (string) | (number) | (null);
        "error": Wire.Error;
    };
    type Content = ({
        "encoding": "json";
        "value": Wire.Json;
    }) | ({
        "encoding": "text";
        "value": string;
    }) | ({
        "encoding": "base64";
        "value": string;
    });
    type Owner = ({
        "kind": "hive";
    }) | ({
        "kind": "agent";
    }) | ({
        "kind": "service";
        "serviceName": string;
    });
    type RetentionPolicy = {
        "objects": ({
            "mode": "retain" | "owned";
        }) | ({
            "mode": "expire";
            "maximumAgeDays": number;
        });
        "revisions": ({
            "mode": "all" | "current";
        }) | (({
            "mode": "bounded";
            "maximumCount": number;
            "maximumAgeDays"?: number;
        }) | ({
            "mode": "bounded";
            "maximumCount"?: number;
            "maximumAgeDays": number;
        }));
    };
    type RevisionReference = {
        "objectId": Wire.Identifier;
        "revision": number;
    };
    type RevisionReferences = {
        [key: string]: Wire.RevisionReference;
    };
    type DataContract = ({
        "key": string;
        "version": Wire.ContractVersion;
        "owner": Wire.Owner;
        "mediaType": string;
        "retention": Wire.RetentionPolicy;
        "specMarkdown": string;
        "jsonSchema"?: ({
            [key: string]: unknown;
        }) | (boolean);
    }) & (unknown);
    type ToolDefinition = {
        "namespace": string;
        "name": string;
        "interfaceVersion": Wire.ContractVersion;
        "description": string;
        "inputSchema": ({
            [key: string]: unknown;
        }) | (boolean);
        "outputSchema": ({
            [key: string]: unknown;
        }) | (boolean);
        "discovery"?: {
            "mcp"?: {
                "name": string;
                "surface"?: "ivy" | "ivy_dev";
            };
            "group"?: string;
            "summary"?: string;
            "keywords"?: Array<string>;
        };
        "annotations"?: {
            "title"?: string;
            "readOnlyHint"?: boolean;
            "destructiveHint"?: boolean;
            "idempotentHint"?: boolean;
            "openWorldHint"?: boolean;
        };
        "nativeMethod"?: string;
        "nativeSchemaIdentity"?: string;
    };
    type ToolCall = {
        "expectedCallerPrincipalId"?: Wire.Identifier;
        "qualifiedName": string;
        "arguments": Wire.Json;
        "expectedDefinitionHash": Wire.Hash;
        "operationId"?: Wire.Identifier;
        "serviceNodeId"?: string;
        "hostId"?: string;
        "serviceName"?: string;
        "resourceRef"?: Wire.ResourceRef;
    };
    type ObjectWrite = ({
        "mutationId": string;
        "contractVersion": Wire.ContractVersion;
        "content": Wire.Content;
        "references": Wire.RevisionReferences;
        "create": {
            "contractKey": string;
            "parentId": (string) | (null);
            "name": string;
            "ownerObjectId": (Wire.Identifier) | (null);
            "icon"?: (string) | (null);
        };
        "objectId"?: string;
        "expectedRevision"?: number;
    }) | ({
        "mutationId": string;
        "contractVersion": Wire.ContractVersion;
        "content": Wire.Content;
        "references": Wire.RevisionReferences;
        "create"?: {
            "contractKey": string;
            "parentId": (string) | (null);
            "name": string;
            "ownerObjectId": (Wire.Identifier) | (null);
            "icon"?: (string) | (null);
        };
        "objectId": string;
        "expectedRevision": number;
    });
    type DeploymentRecord = ({
        "deploymentId": string;
        "hostId": string;
        "instanceId": string;
        "componentId": string;
        "requestHash": Wire.Hash;
        "previousBuild": (string) | (null);
        "targetBuild": (string) | (null);
        "observedBuild": (string) | (null);
        "phase": "preparing" | "prepared" | "checking" | "draining" | "activating" | "verifying" | "succeeded" | "rolling_back" | "rolled_back" | "failed" | "needs_attention";
        "createdAt": string;
        "updatedAt": string;
        "readiness": {
            "state": "not_run" | "passed" | "failed" | "unknown";
            "message": string;
        };
        "errorCode"?: string;
    }) & (unknown);
    type QueryPredicate = ({
        "op": "contains";
        "field": string;
        "value": string;
    }) | ({
        "op": "eq" | "ne" | "gt" | "gte" | "lt" | "lte";
        "field": string;
        "value": (null) | (boolean) | (number) | (string);
    }) | ({
        "op": "in";
        "field": string;
        "value": Array<(null) | (boolean) | (number) | (string)>;
    }) | ({
        "op": "isNull";
        "field": string;
    }) | ({
        "op": "and" | "or";
        "args": Array<Wire.QueryPredicate>;
    }) | ({
        "op": "not";
        "arg": Wire.QueryPredicate;
    });
    type ObjectQuery = {
        "contractKey": string;
        "contractVersions"?: Array<Wire.ContractVersion>;
        "where"?: Wire.QueryPredicate;
        "select"?: Array<string>;
        "orderBy"?: Array<{
            "field": string;
            "direction": "asc" | "desc";
        }>;
        "includeArchived"?: boolean;
        "limit"?: number;
        "cursor"?: string;
    };
    type ServiceConnect = {
        "serviceNodeId": string;
        "hostId": string;
        "serviceName": string;
        "instanceMode"?: "singleton" | "multiple";
        "version": string;
        "buildId": Wire.Hash;
        "hiveProtocol": 1;
        "nativeVersion"?: string;
    };
    type ContractRequirement = {
        "key": string;
        "readVersions": Array<Wire.ContractVersion>;
        "writeVersions": Array<Wire.ContractVersion>;
        "readScope"?: Wire.ContractReadScope;
    };
    type TopicEventKind = {
        "kind": string;
        "title": string;
        "description": string;
    };
    type TopicDefinition = {
        "topic": string;
        "version": Wire.ContractVersion;
        "title": string;
        "description": string;
        "payloadSchema": ({
            [key: string]: unknown;
        }) | (boolean);
        "eventKinds": Array<Wire.TopicEventKind>;
    };
    type NamespaceDefinition = {
        "namespace": string;
        "description": string;
        "guideMarkdown": string;
        "tools": Array<Wire.ToolDefinition>;
        "discoveryGroups"?: Array<{
            "group": string;
            "description": string;
        }>;
        "notifications"?: Array<{
            "name": string;
            "version": Wire.ContractVersion;
            "description": string;
            "payloadSchema": ({
                [key: string]: unknown;
            }) | (boolean);
        }>;
        "topics": Array<Wire.TopicDefinition>;
        "inventoryKinds": Array<{
            "kind": string;
            "version": Wire.ContractVersion;
            "summarySchema": ({
                [key: string]: unknown;
            }) | (boolean);
            "searchPointers"?: Array<string>;
            "archivedPointer"?: string;
            "recencyPointer"?: string;
        }>;
    };
    type RegistrySync = {
        "discoveryHint"?: string;
        "mcpPrefix"?: string;
        "namespaces": Array<Wire.NamespaceDefinition>;
        "contracts": Array<Wire.DataContract>;
        "requiredContracts": Array<Wire.ContractRequirement>;
    };
    type EventFilter = {
        "topics"?: Array<string>;
        "sources"?: Array<string>;
        "objectIds"?: Array<Wire.Identifier>;
    };
    type EventSubscribe = {
        "name": string;
        "filter": Wire.EventFilter;
        "initialSequence"?: number;
        "limit"?: number;
    };
    type EventAck = {
        "name": string;
        "throughSequence": number;
        "gapThroughSequence"?: number;
    };
    type Diagnostic = {
        "code": string;
        "resource": Wire.Json;
        "severity": "info" | "warning" | "error";
        "source": string;
        "firstObservedAt": string;
        "lastObservedAt": string;
        "status": "current" | "stale" | "unknown" | "resolved";
        "message": string;
    };
    type ServiceHeartbeat = {
        "ready": boolean;
        "diagnostics": Array<Wire.Diagnostic>;
    };
    type InventoryRevisionConflictDetails = {
        "serviceNodeId": Wire.Identifier;
        "namespace": string;
        "kind": string;
        "currentRevision": number;
    };
    type InventoryEntry = {
        "nativeId": string;
        "summary": Wire.Json;
        "observedAt": string;
    };
    type InventorySync = ({
        "namespace": string;
        "kind": string;
        "schemaVersion": Wire.ContractVersion;
        "mode": "snapshot" | "delta";
        "snapshotRevision": number;
        "expectedRevision"?: number;
        "entries": Array<Wire.InventoryEntry>;
        "removedNativeIds"?: Array<string>;
    }) & (unknown);
    type ContractReadScope = {
        "roots": (null) | (Array<string>);
        "history": "current" | "all";
        "includeArchived": boolean;
        "references": Array<{
            "objectId": string;
            "revision": number;
        }>;
    };
}
export declare namespace Operation {
    type ObjectMetadata = {
        "id": Wire.Identifier;
        "parentId": (Wire.Identifier) | (null);
        "ownerObjectId": (Wire.Identifier) | (null);
        "name": string;
        "path": string;
        "position": number;
        "icon": (string) | (null);
        "contractKey": string;
        "currentRevision": number;
        "contractVersion": Wire.ContractVersion;
        "archivedAt": (string) | (null);
        "effectivelyArchived": boolean;
        "createdAt": string;
        "updatedAt": string;
    };
    type RevisionMetadata = {
        "objectId": Wire.Identifier;
        "revision": number;
        "contractVersion": Wire.ContractVersion;
        "contentHash": Wire.Hash;
        "mediaType": string;
        "byteLength": number;
        "createdAt": string;
        "references": Wire.RevisionReferences;
    };
    type ObjectRead = {
        "object": Operation.ObjectMetadata;
        "revision": Operation.RevisionMetadata;
        "content": Wire.Content;
    };
    type ObjectWriteResult = {
        "object": Operation.ObjectMetadata;
        "revision": Operation.RevisionMetadata;
    };
    type QueryItem = {
        "objectId": Wire.Identifier;
        "revision": number;
        "contractVersion": Wire.ContractVersion;
        "values": {
            [key: string]: (null) | (boolean) | (number) | (string);
        };
    };
    type ServiceNode = {
        "serviceNodeId": Wire.Identifier;
        "hostId": Wire.Identifier;
        "serviceName": string;
        "instanceMode": "singleton" | "multiple";
        "principalId": Wire.Identifier;
        "version": string;
        "buildId": Wire.Hash;
        "hiveProtocol": 1;
        "nativeVersion": (string) | (null);
        "connected": boolean;
        "synced": boolean;
        "ready": boolean;
        "stale": boolean;
        "desiredEnabled": boolean;
        "lastContactAt": (string) | (null);
        "lastSuccessfulSyncAt": (string) | (null);
        "lastFailedSyncAt": (string) | (null);
        "lastObservationAt": (string) | (null);
        "diagnostic": ({
            "code": string;
            "severity": "info" | "warning" | "error";
            "message": string;
            "observedAt": string;
        }) | (null);
    };
    type Host = {
        "hostId": Wire.Identifier;
        "serviceNodeIds": Array<Wire.Identifier>;
        "connected": boolean;
    };
    type Provider = {
        "node": Operation.ServiceNode;
        "eligible": boolean;
    };
    type NodeContracts = {
        "provider": Operation.Provider;
        "hasCatalog": boolean;
        "capturedAt": (string) | (null);
        "items": Array<Wire.ContractRequirement>;
        "nextCursor": (string) | (null);
    };
    type ToolBinding = {
        "qualifiedName": string;
        "definition": Wire.ToolDefinition;
        "definitionHash": Wire.Hash;
    };
    type NamespaceSnapshot = {
        "namespace": string;
        "description": string;
        "guideMarkdown": string;
        "provider": Operation.Provider;
        "toolCount": number;
        "topicCount": number;
    };
    type NamespaceSummary = {
        "namespace": string;
        "providers": Array<Operation.Provider>;
    };
    type DiscoveryEntry = {
        "kind": "service" | "group" | "tool" | "provider";
        "mcpName"?: string;
        "name": string;
        "description": string;
        "serviceName": string;
        "namespace"?: string;
        "group"?: string;
        "toolCount"?: number;
        "providerCount"?: number;
        "availableCount"?: number;
        "serviceNodeId"?: Wire.Identifier;
        "hostId"?: Wire.Identifier;
        "available"?: boolean;
        "effect"?: "read" | "write" | "unknown";
    };
    type DiscoveryProvider = {
        "serviceNodeId": Wire.Identifier;
        "hostId": Wire.Identifier;
        "available": boolean;
    };
    type InventoryItem = {
        "resourceRef": Wire.ResourceRef;
        "schemaVersion": Wire.ContractVersion;
        "summary": Wire.Json;
        "observedAt": string;
        "stale": boolean;
        "snapshotRevision": number;
    };
    type Event = {
        "sequence": number;
        "topic": string;
        "topicVersion": Wire.ContractVersion;
        "source": string;
        "occurredAt": string;
        "mutationId": Wire.Identifier;
        "payload": Wire.Json;
    };
    type EventPublication = {
        "sequence": number;
        "topic": string;
        "publishedAt": string;
    };
    type EventGap = {
        "prunedThroughSequence": number;
        "resumeAfterSequence": number;
    };
    type EventBatch = {
        "items": Array<Operation.Event>;
        "throughSequence": number;
        "hasMore": boolean;
        "gap": (Operation.EventGap) | (null);
    };
    type FamilyRetentionStatus = {
        "contractKey": string;
        "policy": Wire.RetentionPolicy;
        "policyHash": Wire.Hash;
        "previewRequired": boolean;
        "objectCount": number;
        "revisionCount": number;
        "byteLength": number;
        "eligibleObjectCount": number;
        "eligibleRevisionCount": number;
        "protectedRevisionCount": number;
    };
    type RetentionStatus = {
        "checkedAt": string;
        "nextPassAt": string;
        "lastSuccessAt": (string) | (null);
        "lastFailure": ({
            "at": string;
            "message": string;
        }) | (null);
        "families": Array<Operation.FamilyRetentionStatus>;
        "events": {
            "count": number;
            "byteLength": number;
            "prunedThroughSequence": number;
        };
        "mutations": {
            "count": number;
            "byteLength": number;
            "expiredBefore": number;
        };
    };
    type UiMetadata = {
        "uiId": string;
        "displayName": string;
        "description": string;
        "iconKey": "ui" | "bot" | "book-open" | "list-checks" | "message-circle" | "phone" | "camera" | "newspaper" | "settings";
    };
    type UiAsset = {
        "path": string;
        "objectId": Wire.Identifier;
        "revision": number;
        "mediaType": string;
        "contentHash": Wire.Hash;
    };
    type ServiceRequirement = {
        "serviceName": string;
        "namespace": string;
        "interfaceVersion": Wire.ContractVersion;
    };
    type NotificationFilter = {
        "namespace": string;
        "name": string;
        "version": Wire.ContractVersion;
        "serviceNodeId"?: Wire.Identifier;
    };
    type UiRequirements = {
        "hiveProtocol": 1;
        "contracts": Array<Wire.ContractRequirement>;
        "services": Array<Operation.ServiceRequirement>;
    };
    type UiRelease = {
        "releaseId": string;
        "entryPath": string;
        "requirements": Operation.UiRequirements;
        "assets": Array<Operation.UiAsset>;
    };
    type UI = {
        "metadata": Operation.UiMetadata;
        "currentReleaseId": (string) | (null);
        "releases": Array<Operation.UiRelease>;
    };
    type UiReleaseSummary = {
        "releaseId": string;
        "entryPath": string;
        "assetCount": number;
    };
    type UiIssue = {
        "code": string;
        "message": string;
        "resource": {
            "serviceNodeId"?: Wire.Identifier;
            "serviceName"?: string;
            "namespace"?: string;
            "qualifiedName"?: string;
            "contractKey"?: string;
            "assetPath"?: string;
        };
    };
    type UiInspection = {
        "uiId": string;
        "currentReleaseId": (string) | (null);
        "requestedReleaseId": (string) | (null);
        "release": (Operation.UiReleaseSummary) | (null);
        "requirements": (Operation.UiRequirements) | (null);
        "checkedAt": string;
        "status": "ready" | "unavailable" | "incompatible" | "invalid" | "missing" | "unselected";
        "issues": Array<Operation.UiIssue>;
        "issuesTruncated": boolean;
    };
    type UiSummary = {
        "metadata": Operation.UiMetadata;
        "currentReleaseId": (string) | (null);
        "releaseCount": number;
        "current": Operation.UiInspection;
    };
    type UiDefinition = {
        "metadata": Operation.UiMetadata;
        "entryPath": string;
        "requirements": Operation.UiRequirements;
        "dataContracts": Array<Wire.DataContract>;
    };
    type UiPointerResult = {
        "uiId": string;
        "releaseId": string;
        "previousReleaseId": (string) | (null);
    };
    type DeploymentSnapshot = {
        "record": Wire.DeploymentRecord;
        "serviceNodeId": Wire.Identifier;
        "reportedAt": string;
        "stale": boolean;
    };
    type HostConfigurationSummary = {
        "hostId": Wire.Identifier;
        "objectId": Wire.Identifier;
        "revision": number;
        "contentHash": Wire.Hash;
        "updatedAt": string;
        "byteLength": number;
    };
    type HostConfiguration = {
        "hostId": Wire.Identifier;
        "objectId": Wire.Identifier;
        "revision": number;
        "contentHash": Wire.Hash;
        "updatedAt": string;
        "byteLength": number;
        "configuration": Host.HostConfig;
    };
    type HostConfigurationEditor = {
        "hostId": Wire.Identifier;
        "objectId": Wire.Identifier;
        "revision": number;
        "contentHash": Wire.Hash;
        "updatedAt": string;
        "byteLength": number;
        "configuration": Wire.Json;
    };
    type PackageUploadAuthorization = {
        "componentId": string;
        "version": Wire.ContractVersion;
        "buildId": Wire.Hash;
        "archiveHash": Wire.Hash;
        "bytes": number;
        "manifest": Wire.Json;
    };
    type PackageCatalogEntry = {
        "componentId": string;
        "version": string;
        "buildId": Wire.Hash;
        "archiveHash": Wire.Hash;
        "bytes": number;
        "manifest": Host.ReleaseManifest;
        "revision": number;
        "publishedAt": string;
        "publisherPrincipalId": Wire.Identifier;
    };
    type PackageCatalog = {
        "schemaVersion": 1;
        "revision": number;
        "packages": Array<Operation.PackageCatalogEntry>;
    };
    type PackageCatalogSnapshot = {
        "objectId": Wire.Identifier;
        "objectRevision": number;
        "catalog": Operation.PackageCatalog;
    };
    type PackageUploadResult = ({
        "alreadyPublished": true;
        "entry": Operation.PackageCatalogEntry;
    }) | ({
        "alreadyPublished": false;
        "uploadId": Wire.Identifier;
        "uploadToken": string;
        "uploadPath": string;
        "expiresAt": string;
    });
    type Status = {
        "callerPrincipalId": Wire.Identifier;
        "version": string;
        "buildId": Wire.Hash;
        "hiveProtocol": 1;
        "runtimeEpoch": Wire.Identifier;
        "ready": boolean;
        "serverTime": string;
        "storageFormat": number;
        "publicBaseUrl": string;
    };
    type SystemStatusParams = {};
    type SystemStatusResult = Operation.Status;
    type SystemInspectStorageParams = {
        "contracts": Array<{
            "key": string;
            "readScope"?: Wire.ContractReadScope;
        }>;
    };
    type SystemInspectStorageResult = Host.StorageInspection;
    type SystemDiagnosticsParams = {
        "limit"?: number;
        "cursor"?: string;
        "status"?: "current" | "stale" | "unknown" | "resolved";
    };
    type SystemDiagnosticsResult = {
        "items": Array<Wire.Diagnostic>;
        "nextCursor": (string) | (null);
    };
    type ContractsListParams = {
        "limit"?: number;
        "cursor"?: string;
        "key"?: string;
        "allVersions"?: boolean;
    };
    type ContractsListResult = {
        "items": Array<Wire.DataContract>;
        "nextCursor": (string) | (null);
    };
    type ContractsGetParams = {
        "key": string;
        "version"?: Wire.ContractVersion;
    };
    type ContractsGetResult = Wire.DataContract;
    type ContractsRegisterParams = {
        "definition": Wire.DataContract;
        "mutationId": Wire.Identifier;
    };
    type ContractsRegisterResult = Wire.DataContract;
    type ContractsValidateParams = {
        "key": string;
        "contractVersion": Wire.ContractVersion;
        "content": Wire.Content;
    };
    type ContractsValidateResult = {
        "valid": true;
        "contentHash": Wire.Hash;
        "byteLength": number;
    };
    type ObjectsStatParams = ({
        "objectId": Wire.Identifier;
        "path"?: string;
    }) | ({
        "objectId"?: Wire.Identifier;
        "path": string;
    });
    type ObjectsStatResult = Operation.ObjectMetadata;
    type ObjectsReadParams = ({
        "objectId": Wire.Identifier;
        "path"?: string;
        "revision"?: number;
    }) | ({
        "objectId"?: Wire.Identifier;
        "path": string;
        "revision"?: number;
    });
    type ObjectsReadResult = Operation.ObjectRead;
    type ObjectsWriteParams = Wire.ObjectWrite;
    type ObjectsWriteResult = Operation.ObjectWriteResult;
    type ObjectsHistoryParams = {
        "objectId": Wire.Identifier;
        "limit"?: number;
        "cursor"?: string;
    };
    type ObjectsHistoryResult = {
        "items": Array<Operation.RevisionMetadata>;
        "nextCursor": (string) | (null);
        "historyComplete": boolean;
    };
    type ObjectsListParams = {
        "parentId": (Wire.Identifier) | (null);
        "includeArchived"?: boolean;
        "limit"?: number;
        "cursor"?: string;
    };
    type ObjectsListResult = {
        "items": Array<Operation.ObjectMetadata>;
        "nextCursor": (string) | (null);
    };
    type ObjectsTreeParams = {
        "rootId": (Wire.Identifier) | (null);
        "depth"?: number;
        "limit"?: number;
        "includeArchived"?: boolean;
    };
    type ObjectsTreeResult = {
        "items": Array<Operation.ObjectMetadata>;
        "truncated": boolean;
    };
    type ObjectsQueryParams = Wire.ObjectQuery;
    type ObjectsQueryResult = {
        "items": Array<Operation.QueryItem>;
        "nextCursor": (string) | (null);
    };
    type ObjectsSearchParams = {
        "text": string;
        "contractKey"?: string;
        "rootId"?: Wire.Identifier;
        "includeArchived"?: boolean;
        "limit"?: number;
        "cursor"?: string;
    };
    type ObjectsSearchResult = {
        "items": Array<Operation.ObjectMetadata>;
        "nextCursor": (string) | (null);
    };
    type RetentionPreviewParams = {};
    type RetentionPreviewResult = Operation.RetentionStatus;
    type RetentionStatusParams = {};
    type RetentionStatusResult = Operation.RetentionStatus;
    type ObjectsMoveParams = {
        "objectId": Wire.Identifier;
        "parentId": (Wire.Identifier) | (null);
        "name": string;
        "icon"?: (string) | (null);
        "mutationId": Wire.Identifier;
    };
    type ObjectsMoveResult = Operation.ObjectMetadata;
    type ObjectsReorderParams = {
        "objectId": Wire.Identifier;
        "beforeObjectId": (Wire.Identifier) | (null);
        "mutationId": Wire.Identifier;
    };
    type ObjectsReorderResult = Operation.ObjectMetadata;
    type ObjectsArchiveParams = {
        "objectId": Wire.Identifier;
        "archived": boolean;
        "mutationId": Wire.Identifier;
    };
    type ObjectsArchiveResult = Operation.ObjectMetadata;
    type ObjectsDeleteParams = {
        "objectId": Wire.Identifier;
        "expectedRevision": number;
        "mutationId": Wire.Identifier;
    };
    type ObjectsDeleteResult = {
        "deleted": boolean;
    };
    type ServiceConnectParams = Wire.ServiceConnect;
    type ServiceConnectResult = {
        "serviceNodeId": Wire.Identifier;
        "generation": number;
    };
    type RegistrySyncParams = Wire.RegistrySync;
    type RegistrySyncResult = {
        "generation": number;
        "syncedAt": string;
    };
    type ServiceHeartbeatParams = Wire.ServiceHeartbeat;
    type ServiceHeartbeatResult = {
        "generation": number;
        "ready": boolean;
        "observedAt": string;
    };
    type HostsListParams = {
        "limit"?: number;
        "cursor"?: string;
    };
    type HostsListResult = {
        "items": Array<Operation.Host>;
        "nextCursor": (string) | (null);
    };
    type HostsReportParams = Host.ManagementSnapshot;
    type HostsReportResult = {
        "sequence": number;
        "reportedAt": string;
    };
    type HostsObservationsParams = {
        "limit"?: number;
        "cursor"?: string;
        "hostId"?: Wire.Identifier;
    };
    type HostsObservationsResult = {
        "items": Array<Host.ManagementObservation>;
        "nextCursor": (string) | (null);
    };
    type HostConfigurationsListParams = {
        "limit"?: number;
        "cursor"?: string;
    };
    type HostConfigurationsListResult = {
        "items": Array<Operation.HostConfigurationSummary>;
        "nextCursor": (string) | (null);
    };
    type HostConfigurationsGetParams = {
        "hostId": Wire.Identifier;
        "revision"?: number;
    };
    type HostConfigurationsGetResult = Operation.HostConfiguration;
    type HostConfigurationsPutParams = {
        "hostId": Wire.Identifier;
        "expectedRevision"?: number;
        "configuration": Host.HostConfig;
        "mutationId": Wire.Identifier;
    };
    type HostConfigurationsPutResult = Operation.HostConfiguration;
    type HostConfigurationsEditParams = {
        "hostId": Wire.Identifier;
        "revision"?: number;
    };
    type HostConfigurationsEditResult = Operation.HostConfigurationEditor;
    type HostConfigurationsSaveParams = {
        "hostId": Wire.Identifier;
        "expectedRevision"?: number;
        "configuration": Wire.Json;
        "mutationId": Wire.Identifier;
    };
    type HostConfigurationsSaveResult = Operation.HostConfigurationEditor;
    type HostConfigurationsHistoryParams = {
        "hostId": Wire.Identifier;
        "limit"?: number;
        "cursor"?: string;
    };
    type HostConfigurationsHistoryResult = {
        "items": Array<Operation.RevisionMetadata>;
        "nextCursor": (string) | (null);
        "historyComplete": boolean;
    };
    type ServiceNodesListParams = {
        "limit"?: number;
        "cursor"?: string;
        "hostId"?: Wire.Identifier;
        "serviceName"?: string;
    };
    type ServiceNodesListResult = {
        "items": Array<Operation.ServiceNode>;
        "nextCursor": (string) | (null);
    };
    type ServiceNodesGetParams = {
        "serviceNodeId": Wire.Identifier;
    };
    type ServiceNodesGetResult = Operation.ServiceNode;
    type ServiceNodesContractsParams = {
        "serviceNodeId": Wire.Identifier;
        "key"?: string;
        "limit"?: number;
        "cursor"?: string;
    };
    type ServiceNodesContractsResult = Operation.NodeContracts;
    type NamespacesListParams = {
        "limit"?: number;
        "cursor"?: string;
    };
    type NamespacesListResult = {
        "items": Array<Operation.NamespaceSummary>;
        "nextCursor": (string) | (null);
    };
    type NamespacesGetParams = {
        "namespace": string;
        "serviceNodeId"?: Wire.Identifier;
        "hostId"?: Wire.Identifier;
        "serviceName"?: string;
        "resourceRef"?: Wire.ResourceRef;
    };
    type NamespacesGetResult = Operation.NamespaceSnapshot;
    type ToolsListParams = {
        "namespace": string;
        "serviceNodeId"?: Wire.Identifier;
        "hostId"?: Wire.Identifier;
        "serviceName"?: string;
        "resourceRef"?: Wire.ResourceRef;
        "namePrefix"?: string;
        "limit"?: number;
        "cursor"?: string;
    };
    type ToolsListResult = {
        "provider": Operation.Provider;
        "guideMarkdown": string;
        "items": Array<Operation.ToolBinding>;
        "nextCursor": (string) | (null);
    };
    type TopicsListParams = {
        "namespace": string;
        "serviceNodeId"?: Wire.Identifier;
        "hostId"?: Wire.Identifier;
        "serviceName"?: string;
        "resourceRef"?: Wire.ResourceRef;
        "topicPrefix"?: string;
        "limit"?: number;
        "cursor"?: string;
    };
    type TopicsListResult = {
        "provider": Operation.Provider;
        "namespace": string;
        "items": Array<Wire.TopicDefinition>;
        "nextCursor": (string) | (null);
    };
    type ToolsCallParams = Wire.ToolCall;
    type ToolsCallResult = Wire.Json;
    type DiscoveryListParams = {
        "serviceName"?: string;
        "group"?: string;
        "query"?: string;
        "serviceNodeId"?: Wire.Identifier;
        "providers"?: boolean;
        "limit"?: number;
        "cursor"?: string;
    };
    type DiscoveryListResult = {
        "items": Array<Operation.DiscoveryEntry>;
        "nextCursor": (string) | (null);
    };
    type DiscoveryInstructionsParams = {};
    type DiscoveryInstructionsResult = {
        "instructions": string;
    };
    type DiscoveryDescribeParams = {
        "serviceName": string;
        "tools": Array<string>;
        "serviceNodeId"?: Wire.Identifier;
    };
    type DiscoveryDescribeResult = {
        "serviceName": string;
        "provider": (Operation.DiscoveryProvider) | (null);
        "guides": Array<{
            "namespace": string;
            "guideMarkdown": string;
        }>;
        "items": Array<Operation.ToolBinding>;
    };
    type DiscoveryCallParams = {
        "serviceName": string;
        "qualifiedName": string;
        "expectedDefinitionHash": Wire.Hash;
        "serviceNodeId"?: Wire.Identifier;
        "arguments": Wire.Json;
        "operationId"?: Wire.Identifier;
        "expectedCallerPrincipalId"?: Wire.Identifier;
    };
    type DiscoveryCallResult = Wire.Json;
    type InventoryListParams = {
        "limit"?: number;
        "cursor"?: string;
        "serviceNodeId"?: Wire.Identifier;
        "hostId"?: Wire.Identifier;
        "serviceName"?: string;
        "resourceRef"?: Wire.ResourceRef;
        "namespace"?: string;
        "kind"?: string;
        "sort"?: "native-id" | "recency-desc";
        "archived"?: boolean;
        "searchTerm"?: string;
    };
    type InventoryListResult = {
        "items": Array<Operation.InventoryItem>;
        "nextCursor": (string) | (null);
    };
    type InventoryGetParams = {
        "resourceRef": Wire.ResourceRef;
    };
    type InventoryGetResult = Operation.InventoryItem;
    type InventorySyncParams = Wire.InventorySync;
    type InventorySyncResult = {
        "snapshotRevision": number;
        "observedAt": string;
    };
    type EventsPublishParams = {
        "topic": string;
        "topicVersion": Wire.ContractVersion;
        "payload": Wire.Json;
        "mutationId": Wire.Identifier;
    };
    type EventsPublishResult = Operation.EventPublication;
    type EventsReadParams = {
        "afterSequence": number;
        "filter": Wire.EventFilter;
        "limit"?: number;
    };
    type EventsReadResult = Operation.EventBatch;
    type EventsHeadParams = {};
    type EventsHeadResult = {
        "throughSequence": number;
    };
    type EventsSubscribeParams = Wire.EventSubscribe;
    type EventsSubscribeResult = Operation.EventBatch;
    type EventsAckParams = Wire.EventAck;
    type EventsAckResult = {
        "acknowledgedSequence": number;
    };
    type EventsUnsubscribeParams = {
        "name": Wire.Identifier;
        "mutationId": Wire.Identifier;
    };
    type EventsUnsubscribeResult = {
        "removed": boolean;
    };
    type NotificationsSubscribeParams = {
        "filters": Array<Operation.NotificationFilter>;
    };
    type NotificationsSubscribeResult = {
        "subscribed": number;
    };
    type PackagesCatalogParams = {
        "after": number;
        "includeUis": boolean;
    };
    type PackagesCatalogResult = Operation.PackageCatalogSnapshot;
    type PackagesAuthorizeUploadParams = Operation.PackageUploadAuthorization;
    type PackagesAuthorizeUploadResult = Operation.PackageUploadResult;
    type UisListParams = {
        "limit"?: number;
        "cursor"?: string;
    };
    type UisListResult = {
        "items": Array<Operation.UI>;
        "nextCursor": (string) | (null);
    };
    type UisGetParams = {
        "uiId": string;
    };
    type UisGetResult = Operation.UI;
    type UisCatalogParams = {
        "limit"?: number;
        "cursor"?: string;
    };
    type UisCatalogResult = {
        "items": Array<Operation.UiSummary>;
        "nextCursor": (string) | (null);
    };
    type UisReleasesParams = {
        "uiId": string;
        "limit"?: number;
        "cursor"?: string;
    };
    type UisReleasesResult = {
        "items": Array<Operation.UiReleaseSummary>;
        "nextCursor": (string) | (null);
    };
    type UisInspectParams = {
        "uiId": string;
        "releaseId"?: string;
    };
    type UisInspectResult = Operation.UiInspection;
    type UisDeployParams = {
        "metadata": Operation.UiMetadata;
        "release": Operation.UiRelease;
        "expectedReleaseId": (string) | (null);
        "mutationId": Wire.Identifier;
    };
    type UisDeployResult = Operation.UiPointerResult;
    type UisRollbackParams = {
        "uiId": string;
        "releaseId": string;
        "expectedReleaseId": string;
        "mutationId": Wire.Identifier;
    };
    type UisRollbackResult = Operation.UiPointerResult;
    type DeploymentsListParams = {
        "limit"?: number;
        "cursor"?: string;
        "hostId"?: Wire.Identifier;
        "instanceId"?: Wire.Identifier;
    };
    type DeploymentsListResult = {
        "items": Array<Operation.DeploymentSnapshot>;
        "nextCursor": (string) | (null);
    };
    type DeploymentsGetParams = {
        "deploymentId": Wire.Identifier;
        "hostId": Wire.Identifier;
    };
    type DeploymentsGetResult = Operation.DeploymentSnapshot;
    type DeploymentsReportParams = {
        "records": Array<Wire.DeploymentRecord>;
    };
    type DeploymentsReportResult = {
        "reportedAt": string;
    };
    type WikiSearchParams = {
        "text": string;
        "rootId"?: Wire.Identifier;
        "includeArchived"?: boolean;
        "limit"?: number;
        "cursor"?: string;
    };
    type WikiSearchResult = Operation.ObjectsSearchResult;
    type WikiListParams = {
        "parentId"?: (Wire.Identifier) | (null);
        "limit"?: number;
        "cursor"?: string;
        "includeArchived"?: boolean;
    };
    type WikiListResult = Operation.ObjectsQueryResult;
    type WikiReadParams = ({
        "objectId": Wire.Identifier;
        "path"?: string;
        "revision"?: number;
    }) | ({
        "objectId"?: Wire.Identifier;
        "path": string;
        "revision"?: number;
    });
    type WikiReadResult = Operation.ObjectRead;
    type WikiCreateParams = {
        "mutationId": Wire.Identifier;
        "title": string;
        "parentId"?: (Wire.Identifier) | (null);
        "markdown": string;
    };
    type WikiCreateResult = Operation.ObjectWriteResult;
    type WikiUpdateParams = {
        "mutationId": Wire.Identifier;
        "objectId": Wire.Identifier;
        "expectedRevision": number;
        "markdown": string;
    };
    type WikiUpdateResult = Operation.ObjectWriteResult;
    type WikiHistoryParams = {
        "objectId": Wire.Identifier;
        "limit"?: number;
        "cursor"?: string;
    };
    type WikiHistoryResult = Operation.ObjectsHistoryResult;
    type WikiMoveParams = {
        "objectId": Wire.Identifier;
        "parentId": (Wire.Identifier) | (null);
        "name": string;
        "icon"?: (string) | (null);
        "mutationId": Wire.Identifier;
    };
    type WikiMoveResult = Operation.ObjectsMoveResult;
    type WikiArchiveParams = {
        "objectId": Wire.Identifier;
        "archived": boolean;
        "mutationId": Wire.Identifier;
    };
    type WikiArchiveResult = Operation.ObjectsArchiveResult;
}
export declare namespace Transport {
    type ProviderCall = {
        "jsonrpc": "2.0";
        "id": Wire.Identifier;
        "method": "provider.invoke";
        "params": {
            "qualifiedName": string;
            "arguments": Wire.Json;
            "definitionHash": Wire.Hash;
            "generation": number;
            "callerPrincipalId": Wire.Identifier;
            "operationId"?: Wire.Identifier;
        };
    };
    type ServiceNotification = {
        "jsonrpc": "2.0";
        "method": "service.notification";
        "params": {
            "namespace": string;
            "name": string;
            "version": Wire.ContractVersion;
            "payload": Wire.Json;
        };
    };
    type ProviderNotification = {
        "jsonrpc": "2.0";
        "method": "notifications.provider";
        "params": {
            "namespace": string;
            "name": string;
            "version": Wire.ContractVersion;
            "payload": Wire.Json;
            "serviceNodeId": Wire.Identifier;
            "generation": number;
        };
    };
    type EventAvailability = {
        "jsonrpc": "2.0";
        "method": "events.available";
        "params": {
            "throughSequence": number;
        };
    };
}
export declare namespace Host {
    type BuildPlan = ({
        "schemaVersion": 1;
        "componentId": string;
        "kind": "hive" | "service" | "service-manager" | "app" | "native";
        "version": string;
        "description": string;
        "connectsToHive": boolean;
        "requirements": {
            "node": string;
            "os"?: Array<"win32" | "linux">;
            "arch"?: Array<"x64" | "arm64">;
            "hiveProtocol": (1) | (null);
            "contracts": Array<{
                "key": string;
                "readVersions": Array<string>;
                "writeVersions": Array<string>;
                "readScope"?: {
                    "roots": (null) | (Array<string>);
                    "history": "current" | "all";
                    "includeArchived": boolean;
                    "references": Array<{
                        "objectId": string;
                        "revision": number;
                    }>;
                };
            }>;
        };
        "entrypoint"?: {
            "executable": string;
            "args": Array<string>;
            "cwd"?: string;
            "timeoutMs": number;
        };
        "prepare": Array<{
            "executable": string;
            "args": Array<string>;
            "cwd"?: string;
            "timeoutMs": number;
        }>;
        "checks": Array<{
            "executable": string;
            "args": Array<string>;
            "cwd"?: string;
            "timeoutMs": number;
        }>;
        "readiness"?: {
            "timeoutMs": number;
            "command": {
                "executable": string;
                "args": Array<string>;
                "cwd"?: string;
                "timeoutMs": number;
            };
        };
        "shutdown"?: {
            "timeoutMs": number;
        };
        "restart"?: {
            "policy": "never" | "on-failure" | "always";
            "minimumDelayMs": number;
            "maximumDelayMs": number;
        };
        "app"?: {
            "appId": string;
            "dist": string;
            "entryPath": string;
        };
        "storage"?: {
            "minReadableFormat": number;
            "maxReadableFormat": number;
            "writeFormat": number;
        };
        "runtimeReset"?: {
            "delete": Array<{
                "area": "host" | "data" | "work" | "logs";
                "path": string;
            }>;
            "preserve": Array<{
                "area": "host" | "data" | "work" | "logs";
                "path": string;
            }>;
            "completionMarker"?: string;
        };
    }) & (unknown) & (unknown) & (unknown);
    type ReleaseManifest = ({
        "schemaVersion": 1;
        "componentId": string;
        "kind": "hive" | "service" | "service-manager" | "app" | "native";
        "version": string;
        "connectsToHive": boolean;
        "requirements": {
            "node": string;
            "os"?: Array<"win32" | "linux">;
            "arch"?: Array<"x64" | "arm64">;
            "hiveProtocol": (1) | (null);
            "contracts": Array<{
                "key": string;
                "readVersions": Array<string>;
                "writeVersions": Array<string>;
                "readScope"?: {
                    "roots": (null) | (Array<string>);
                    "history": "current" | "all";
                    "includeArchived": boolean;
                    "references": Array<{
                        "objectId": string;
                        "revision": number;
                    }>;
                };
            }>;
        };
        "entrypoint"?: {
            "executable": string;
            "args": Array<string>;
            "cwd"?: string;
            "timeoutMs": number;
        };
        "readiness"?: {
            "timeoutMs": number;
            "command": {
                "executable": string;
                "args": Array<string>;
                "cwd"?: string;
                "timeoutMs": number;
            };
        };
        "shutdown"?: {
            "timeoutMs": number;
        };
        "restart"?: {
            "policy": "never" | "on-failure" | "always";
            "minimumDelayMs": number;
            "maximumDelayMs": number;
        };
        "app"?: {
            "appId": string;
            "dist": string;
            "entryPath": string;
        };
        "storage"?: {
            "minReadableFormat": number;
            "maxReadableFormat": number;
            "writeFormat": number;
        };
        "runtimeReset"?: {
            "delete": Array<{
                "area": "host" | "data" | "work" | "logs";
                "path": string;
            }>;
            "preserve": Array<{
                "area": "host" | "data" | "work" | "logs";
                "path": string;
            }>;
            "completionMarker"?: string;
        };
        "buildId": Wire.Hash;
    }) & (unknown) & (unknown) & (unknown);
    type Credential = {
        "principalId": Wire.Identifier;
        "token": string;
    };
    type HiveSettings = {
        "listenHost": string;
        "listenPort": number;
        "credentials": Array<Host.Credential>;
        "packagePublisherPrincipalIds"?: Array<string>;
        "backup": {
            "directory": string;
            "intervalHours": number;
            "retain": number;
        };
        "trustedProxyAddresses"?: Array<string>;
        "debug"?: {
            "validateMessages"?: boolean;
        };
    };
    type InstanceConfig = {
        "schemaVersion": 1;
        "hostId": string;
        "instanceId": string;
        "serviceNodeId": string;
        "componentId": string;
        "publicBaseUrl": string;
        "dataRoot": string;
        "workRoot"?: string;
        "logsRoot"?: string;
        "artifactRoot": string;
        "buildId": Wire.Hash;
        "version": string;
        "credential"?: string;
        "settings": {
            [key: string]: Wire.Json;
        };
    };
    type Instance = ({
        "instanceId": string;
        "serviceNodeId": string;
        "componentId": string;
        "enabled": boolean;
        "engine": "process" | "docker";
        "credential"?: string;
        "secretPaths"?: Array<string>;
        "paths"?: {
            "data": string;
            "work": string;
            "logs": string;
        };
        "settings": {
            [key: string]: Wire.Json;
        };
        "process"?: {
            "allowWindowsBreakaway": boolean;
        };
        "docker"?: {
            "imageRepository": string;
            "ports": Array<{
                "host": string;
                "port": number;
                "containerPort": number;
            }>;
        };
    }) & (unknown);
    type HostConfig = {
        "schemaVersion": 1;
        "hostId": string;
        "developmentMode"?: boolean;
        "ivyRoot"?: string;
        "servicesRoot"?: string;
        "configPath"?: string;
        "runtimeRoot": string;
        "artifactRoot": string;
        "stagingRoot": string;
        "publicBaseUrl": string;
        "executables": {
            "dotnet"?: string;
            "gcc"?: string;
            [key: string]: string;
        };
        "instances": Array<Host.Instance>;
        "deployment"?: {
            "identity": string;
            "hosts": Array<{
                "hostId": string;
                "configPath": string;
                "ssh"?: string;
                "cliPath"?: string;
            }>;
        };
        "configurationUpdates"?: {
            "intervalSeconds": number;
        };
        "packageUpdates"?: {
            "intervalSeconds": number;
        };
        "restoredFrom"?: {
            "backupId": string;
            "manifestHash": Wire.Hash;
            "restoreId": string;
        };
    };
    type LocalRequest = {
        "action": "deploy" | "rollback" | "restart" | "enable" | "disable";
        "operationId": string;
        "instanceId": string;
        "candidateId"?: Wire.Hash;
        "source"?: string;
        "componentId"?: string;
        "targetBuild"?: Wire.Hash;
    };
    type SourceSnapshot = {
        "snapshotId": string;
        "sourceRoot": string;
        "originalRoot": string;
        "manifestPath": string;
        "capturedAt": string;
    };
    type FileEntry = {
        "path": string;
        "hash": Wire.Hash;
        "bytes": number;
        "mode": number;
    };
    type SnapshotManifest = {
        "schemaVersion": 1;
        "snapshot": Host.SourceSnapshot;
    };
    type ArtifactFiles = Array<Host.FileEntry>;
    type JournalEntry = {
        "schemaVersion": 1;
        "request": Host.LocalRequest;
        "record": Wire.DeploymentRecord;
        "candidateId": (Wire.Hash) | (null);
        "previousCandidateId": (Wire.Hash) | (null);
        "configurationPath": string;
        "sourceSnapshot"?: Host.SourceSnapshot;
        "activation": {
            "previousTarget": (Host.RuntimeTarget) | (null);
            "previousConfigurationPath"?: string;
            "previousEnabled": boolean;
            "targetEnabled": boolean;
            "rollbackTargetRevision": (string) | (null);
        };
    };
    type Candidate = {
        "candidateId": Wire.Hash;
        "componentId": string;
        "artifactRoot": string;
        "manifestPath": string;
        "createdAt": string;
        "platform": {
            "os": "win32" | "linux";
            "arch": "x64" | "arm64";
            "node": string;
        };
        "archivePath"?: string;
        "archiveHash"?: Wire.Hash;
        "buildId": Wire.Hash;
        "dockerImage"?: string;
    };
    type PreparationRecord = {
        "schemaVersion": 1;
        "preparationId": string;
        "componentId": string;
        "buildId": Wire.Hash;
        "snapshot": Host.SourceSnapshot;
        "phase": "building" | "publishing" | "verified" | "failed" | "unknown" | "compacted";
        "candidate": (Host.Candidate) | (null);
        "startedAt": string;
        "updatedAt": string;
        "errorCode": (string) | (null);
    };
    type PreparationInventory = {
        "schemaVersion": 1;
        "hostId": string;
        "observedAt": string;
        "availableBytes": number;
        "entries": Array<{
            "preparationId": string;
            "path": string;
            "phase": "building" | "publishing" | "verified" | "failed" | "unknown" | "compacted" | "unrecognized";
            "buildId": (Wire.Hash) | (null);
            "candidateId": (Wire.Hash) | (null);
            "bytes": number;
            "code": (string) | (null);
        }>;
    };
    type PreparationCompaction = {
        "schemaVersion": 1;
        "hostId": string;
        "observedAt": string;
        "entries": Array<{
            "preparationId": string;
            "action": "compacted" | "recovered" | "retained" | "busy";
            "candidateId": (Wire.Hash) | (null);
            "reclaimedBytes": number;
            "code": (string) | (null);
        }>;
    };
    type BackupFile = {
        "path": string;
        "hash": Wire.Hash;
        "bytes": number;
        "mode": number;
    };
    type BackupManifest = {
        "schemaVersion": 1;
        "backupId": string;
        "hostId": string;
        "createdAt": string;
        "completedAt": string;
        "consistency": "offline-owned-state";
        "platform": {
            "os": "win32" | "linux";
            "arch": string;
            "node": string;
        };
        "configurationHash": Wire.Hash;
        "files": Array<Host.BackupFile>;
        "externalFiles": Array<Host.BackupFile>;
        "hiveSnapshots": Array<{
            "instanceId": string;
            "path": string;
            "format": number;
            "pages": number;
        }>;
        "excluded": Array<string>;
        "ephemeralNativePaths"?: Array<{
            "instanceId": string;
            "path": string;
            "kind": "codex-arg0-helper-cache";
        }>;
        "sharedNativeHomes"?: Array<{
            "instanceId": string;
            "path": string;
            "kind": "external-user-state";
        }>;
        "storageAreas"?: Array<{
            "key": string;
            "path": string;
        }>;
    };
    type BackupResult = {
        "schemaVersion": 1;
        "backupId": string;
        "hostId": string;
        "directory": string;
        "manifestHash": Wire.Hash;
        "completedAt": string;
        "files": number;
        "bytes": number;
        "elapsedMs": number;
    };
    type NativeStorageLayout = {
        "schemaVersion": 1;
        "nativeVersion": "0.154.0";
        "database": "state_5.sqlite";
        "upstream": {
            "tag": string;
            "tree": string;
            "migrationLineEndings": "lf" | "crlf";
            "source": string;
        };
        "migrations": Array<{
            "version": number;
            "description": string;
            "checksum": string;
        }>;
        "schema": Array<{
            "type": "table" | "index" | "trigger" | "view";
            "name": string;
            "tableName": string;
            "sql": string;
        }>;
        "pathColumns": Array<{
            "table": string;
            "column": string;
            "scope": "required-rollout" | "rollout-marker" | "optional-owned-file";
        }>;
    };
    type NativeRecoveryReport = {
        "schemaVersion": 1;
        "restoreId": string;
        "backupId": string;
        "manifestHash": Wire.Hash;
        "completedAt": string;
        "instances": Array<{
            "instanceId": string;
            "nativeVersion": string;
            "layoutHash": Wire.Hash;
            "database": "state_5.sqlite";
            "originalFiles": Array<Host.BackupFile>;
            "workingHash": Wire.Hash;
            "logicalDataHash": Wire.Hash;
            "changes": Array<{
                "table": string;
                "column": string;
                "rows": number;
            }>;
        }>;
    };
    type RestoreResult = {
        "schemaVersion": 1;
        "restoreId": string;
        "backupId": string;
        "manifestHash": Wire.Hash;
        "configPath": string;
        "state": "disabled";
        "restoredBytes": number;
        "elapsedMs": number;
    };
    type Health = {
        "schemaVersion": 1;
        "instanceId": string;
        "componentId": string;
        "buildId": Wire.Hash;
        "pid": number;
        "bootId": string;
        "startedAt": string;
        "observedAt": string;
        "ready": boolean;
        "generation": (number) | (null);
        "launchId"?: string;
        "details": string;
    };
    type RuntimeControl = {
        "schemaVersion": 1;
        "instanceId": string;
        "launchId": string;
        "bootId": string;
        "action": "drain" | "shutdown";
        "requestedAt": string;
    };
    type ProcessStopFence = {
        "schemaVersion": 1;
        "owner": "runtime-owner" | "native";
        "ownerId": string;
        "processId": (number) | (null);
        "observedAt": string;
        "code": "outcome_unknown";
    };
    type RuntimeTarget = {
        "schemaVersion": 1;
        "instanceId": string;
        "revision": string;
        "candidateId": Wire.Hash;
        "desired": "running" | "stopped";
        "configPath": string;
        "requestedAt": string;
    };
    type RuntimeObservation = {
        "schemaVersion": 1;
        "instanceId": string;
        "ownerPid": number;
        "ownerBootId": string;
        "observedAt": string;
        "targetRevision": (string) | (null);
        "candidateId": (Wire.Hash) | (null);
        "buildId": (Wire.Hash) | (null);
        "state": "idle" | "starting" | "ready" | "draining" | "stopped" | "exited" | "failed" | "unknown";
        "health": (Host.Health) | (null);
        "restartCount": number;
        "nextRestartAt": (string) | (null);
        "code": (string) | (null);
        "message": string;
    };
    type ExecutorStatus = {
        "schemaVersion": 1;
        "hostId": string;
        "pid": number;
        "bootId": string;
        "buildId": (Wire.Hash) | (null);
        "observedAt": string;
        "state": "reconciling" | "ready" | "stopping" | "failed";
        "activeDeploymentId": (string) | (null);
        "code": (string) | (null);
        "configurationRevision"?: number;
        "configurationHash"?: Wire.Hash;
    };
    type LinuxResourceBudget = {
        "memoryHighBytes": number;
        "memoryMaxBytes": number;
        "tasksMax": number;
    };
    type LinuxResourceScope = {
        "slice": string;
        "budget": Host.LinuxResourceBudget;
    };
    type BootstrapPlan = {
        "schemaVersion": 1;
        "hostId": string;
        "os": "win32" | "linux";
        "installationId": string;
        "candidateId": Wire.Hash;
        "artifactRoot": string;
        "configPath": string;
        "configHash": Wire.Hash;
        "runtimeRoot": string;
        "nodeExecutable": string;
        "processes": Array<{
            "instanceId": string;
            "componentId": string;
            "name": string;
            "user"?: string;
            "candidateId"?: Wire.Hash;
            "artifactRoot"?: string;
            "entrypoint"?: {
                "executable": string;
                "args": Array<string>;
                "cwd"?: string;
                "timeoutMs": number;
            };
            "configPath"?: string;
            "restart"?: {
                "policy": "never" | "on-failure" | "always";
                "minimumDelayMs": number;
                "maximumDelayMs": number;
            };
            "allowWindowsBreakaway"?: boolean;
        }>;
        "linuxResources"?: Host.LinuxResourceScope;
    };
    type BootstrapSnapshot = {
        "schemaVersion": 1;
        "hostId": string;
        "installationId": string;
        "configPath": string;
        "configurationHash": Wire.Hash;
        "observedAt": string;
        "owners": Array<{
            "instanceId": string;
            "componentId": string;
            "name": string;
            "candidateId": Wire.Hash;
            "definitionHash": Wire.Hash;
            "enabled": boolean;
            "running": boolean;
        }>;
    };
    type BootstrapMaintenanceRequest = {
        "schemaVersion": 1;
        "operationId": string;
        "previous": Host.BootstrapSnapshot;
        "next": Host.BootstrapPlan;
    };
    type BootstrapMaintenanceRecord = {
        "schemaVersion": 1;
        "operationId": string;
        "requestHash": Wire.Hash;
        "request": Host.BootstrapMaintenanceRequest;
        "createdAt": string;
        "updatedAt": string;
        "phase": "checking" | "updating" | "succeeded" | "needs_attention";
        "errorCode": (string) | (null);
        "definitions": Array<{
            "instanceId": string;
            "name": string;
            "previousHash": Wire.Hash;
            "desiredHash": Wire.Hash;
            "applied": boolean;
        }>;
    };
    type ContainerState = {
        "containerId": string;
        "name": string;
        "imageId": Wire.Hash;
        "hostId": string;
        "instanceId": string;
        "candidateId": Wire.Hash;
        "launchId": string;
        "state": "created" | "running" | "paused" | "restarting" | "exited" | "dead" | "removing";
        "exitCode": number;
        "startedAt": string;
        "cgroupParent"?: string;
    };
    type StorageInspection = {
        "schemaVersion": 1;
        "exists": boolean;
        "format": number;
        "contracts": Array<{
            "key": string;
            "versions": Array<string>;
        }>;
    };
    type ManagerSettings = {
        "hostConfigPath": string;
    };
    type ManagedInstance = {
        "instanceId": string;
        "serviceNodeId": string;
        "componentId": string;
        "engine": "process" | "docker";
        "desiredEnabled": boolean;
        "installedBuild": (Wire.Hash) | (null);
        "installedCandidateId": (Wire.Hash) | (null);
        "observedBuild": (Wire.Hash) | (null);
        "observedState": ("idle" | "starting" | "ready" | "draining" | "stopped" | "exited" | "failed" | "unknown") | (null);
        "observedAt": (string) | (null);
        "code": (string) | (null);
        "message": string;
    };
    type HostStatus = {
        "schemaVersion": 1;
        "hostId": string;
        "observedAt": string;
        "executor": (Host.ExecutorStatus) | (null);
        "instances": Array<Host.ManagedInstance>;
        "unfinished": Array<Wire.DeploymentRecord>;
    };
    type ManagementSnapshot = {
        "sequence": number;
        "status": Host.HostStatus;
    };
    type ManagementObservation = {
        "snapshot": Host.ManagementSnapshot;
        "serviceNodeId": string;
        "reportedAt": string;
        "stale": boolean;
    };
    type ManagerStatusInput = {};
    type ManagerDeploymentInput = {
        "deploymentId": string;
    };
    type ManagerDeployInput = {
        "instanceId": string;
        "operationId": string;
        "candidateId"?: Wire.Hash;
        "source"?: string;
    };
    type ManagerRollbackInput = {
        "instanceId": string;
        "operationId": string;
        "targetBuild": Wire.Hash;
    };
    type ManagerLifecycleInput = {
        "instanceId": string;
        "operationId": string;
    };
    type CliOutput = {
        "schemaVersion": 1;
        "ok": boolean;
        "code": string;
        "deploymentId"?: string;
        "data": Wire.Json;
    };
    type HostConfigInput = {
        "schemaVersion": 1;
        "hostId": string;
        "developmentMode"?: boolean;
        "ivyRoot"?: string;
        "servicesRoot"?: string;
        "configPath"?: string;
        "runtimeRoot"?: string;
        "artifactRoot"?: string;
        "stagingRoot"?: string;
        "publicBaseUrl": string;
        "executables": {
            "dotnet"?: string;
            "gcc"?: string;
            [key: string]: string;
        };
        "instances": Array<Host.Instance>;
        "deployment"?: {
            "identity": string;
            "hosts": Array<{
                "hostId": string;
                "configPath": string;
                "ssh"?: string;
                "cliPath"?: string;
            }>;
        };
        "configurationUpdates"?: {
            "intervalSeconds": number;
        };
        "packageUpdates"?: {
            "intervalSeconds": number;
        };
        "restoredFrom"?: {
            "backupId": string;
            "manifestHash": Wire.Hash;
            "restoreId": string;
        };
    };
}
export declare namespace Agent {
    type Catalog = {
        "schemaVersion": 1;
        "provider": "codex";
        "version": string;
        "experimental": true;
        "sourceHash": Wire.Hash;
        "nativeExecutableHash": Wire.Hash;
        "clientRequests": Array<{
            "method": string;
            "paramsRequired": boolean;
            "inputSchema": (boolean) | ({
                [key: string]: Wire.Json;
            });
            "outputSchema": (boolean) | ({
                [key: string]: Wire.Json;
            });
            "responseType": string;
            "inputSource": "native-json" | "native-typescript-and-rust";
            "outputSource": "native-json" | "native-typescript-and-rust";
        }>;
        "serverRequests": Array<{
            "method": string;
            "paramsRequired": boolean;
            "inputSchema": (boolean) | ({
                [key: string]: Wire.Json;
            });
            "outputSchema": (boolean) | ({
                [key: string]: Wire.Json;
            });
            "responseType": string;
            "inputSource": "native-json" | "native-typescript-and-rust";
            "outputSource": "native-json" | "native-typescript-and-rust";
        }>;
        "serverNotifications": Array<{
            "method": string;
            "paramsRequired": boolean;
            "inputSchema": (boolean) | ({
                [key: string]: Wire.Json;
            });
            "paramsAbsent": boolean;
            "inputSource": "native-json" | "native-typescript-and-json-types";
        }>;
        "clientNotifications": Array<{
            "method": string;
            "paramsRequired": boolean;
            "inputSchema": (boolean) | ({
                [key: string]: Wire.Json;
            });
            "paramsAbsent": boolean;
            "inputSource": "native-json" | "native-typescript-and-json-types";
        }>;
        "derivedLegacyTypes": Array<string>;
    };
    type ParametersRef = {
        "schemaVersion": 1;
        "nativeVersion": string;
        "nativeExecutableHash": Wire.Hash;
        "catalogHash": Wire.Hash;
        "method": string;
        "reservedFields": Array<string>;
        "params": Wire.Json;
    };
    type PlanDefinitions = {
        "threadStart": Wire.Hash;
        "threadResume": Wire.Hash;
        "turnStart": Wire.Hash;
        "turnInterrupt": Wire.Hash;
    };
    type PlanLocation = {
        "serviceNodeId": string;
        "hostId": string;
        "kind": "existing" | "normal" | "internal";
        "cwd": string;
        "projectId": string;
    };
    type Plan = {
        "nativeVersion": string;
        "catalogSourceHash": Wire.Hash;
        "definitions": Agent.PlanDefinitions;
        "location"?: Agent.PlanLocation;
        "threadStart": Agent.ParametersRef;
        "threadResume": Agent.ParametersRef;
        "turnStart": Agent.ParametersRef;
    };
    type PlanDraft = {
        "nativeVersion": string;
        "catalogSourceHash": Wire.Hash;
        "definitions": Agent.PlanDefinitions;
        "location"?: Agent.PlanLocation;
        "threadStart": {
            [key: string]: Wire.Json;
        };
        "threadResume": {
            [key: string]: Wire.Json;
        };
        "turnStart": {
            [key: string]: Wire.Json;
        };
    };
    type InvocationDraft = {
        "nativeVersion": string;
        "catalogSourceHash": Wire.Hash;
        "method": "thread/start" | "thread/resume" | "turn/start" | "turn/interrupt";
        "params": {
            [key: string]: Wire.Json;
        };
    };
    type WindowsShell = {
        "executable": string;
        "executableHash": Wire.Hash;
    };
    type WindowsShellStatus = {
        "executable": string;
        "executableHash": Wire.Hash;
        "version": string;
        "packageIdentity": "unpackaged";
    };
    type InstructionsDocument = {
        "schemaVersion": 1;
        "hostId": (string) | (null);
        "enabled": (boolean) | (null);
        "includeLocal": boolean;
        "text": string;
    };
    type InstructionsStatus = {
        "state": "disabled" | "pending" | "applied" | "conflict";
        "home": (string) | (null);
        "desiredVersion": (Wire.Hash) | (null);
        "appliedVersion": (Wire.Hash) | (null);
        "observedAt": string;
        "code": (string) | (null);
        "bytes": number;
    };
    type ManagedOutputStatus = {
        "state": "disabled" | "pending" | "applied" | "conflict";
        "target": (string) | (null);
        "desiredVersion": (Wire.Hash) | (null);
        "appliedVersion": (Wire.Hash) | (null);
        "observedAt": string;
        "code": (string) | (null);
        "bytes": number;
    };
    type McpServer = {
        "name": string;
        "url": string;
        "enabled": boolean;
        "authentication": "none" | "agent-manager";
        "startupTimeoutSeconds": number;
        "toolTimeoutSeconds": number;
    };
    type McpDocument = {
        "schemaVersion": 1;
        "hostId": (string) | (null);
        "enabled": (boolean) | (null);
        "servers": Array<Agent.McpServer>;
    };
    type SkillFile = {
        "path": string;
        "content": string;
    };
    type SkillsDocument = {
        "schemaVersion": 1;
        "hostId": (string) | (null);
        "enabled": (boolean) | (null);
        "files": Array<Agent.SkillFile>;
    };
    type EnvironmentDefaults = {
        "instructions": Agent.InstructionsDocument;
        "mcp": Agent.McpDocument;
        "skills": Agent.SkillsDocument;
    };
    type EnvironmentDefaultsInput = {};
    type EnvironmentStatus = {
        "mcp": Agent.ManagedOutputStatus;
        "skills": Agent.ManagedOutputStatus;
    };
    type ExecutionCapability = {
        "key": string;
        "label": string;
    };
    type CapabilityProfile = {
        "serviceNodeId": Wire.Identifier;
        "hostId": Wire.Identifier;
        "revision": number;
        "capabilities": Array<Agent.ExecutionCapability>;
        "updatedAt": string;
        "operationId": (Wire.Identifier) | (null);
    };
    type CapabilityProfileInput = {};
    type ConfigureCapabilitiesInput = {
        "operationId": Wire.Identifier;
        "expectedRevision": number;
        "capabilities": Array<Agent.ExecutionCapability>;
    };
    type Settings = {
        "nativeExecutable": string;
        "nativeVersion": string;
        "nativeExecutableHash": Wire.Hash;
        "nativeHome"?: string;
        "codexHome"?: string;
        "skillsRoot"?: string;
        "patcherEnabled"?: boolean;
        "projectRoot"?: string;
        "internalProjectRoot"?: string;
        "appServer"?: ({
            "mode": "owned-stdio";
        }) | ({
            "mode": "external-proxy";
            "socketPath"?: string;
            "expectedCodexHome"?: string;
        });
        "windowsShell"?: Agent.WindowsShell;
        "capabilities"?: Array<Agent.ExecutionCapability>;
        "projects": Array<{
            "id": string;
            "name": string;
            "path": string;
        }>;
        "limits": {
            "maxOperations": number;
            "maxJournalBytes": number;
            "maxPendingInputs": number;
            "maxNotificationBytes": number;
        };
    };
    type RequestId = (string) | (number);
    type NativeError = {
        "code": number;
        "message": string;
        "data"?: Wire.Json;
    };
    type Reply = ({
        "result": Wire.Json;
    }) | ({
        "error": Agent.NativeError;
    });
    type Operation = ({
        "schemaVersion": 1;
        "operationId": Wire.Identifier;
        "callerPrincipalId": Wire.Identifier;
        "serviceNodeId": Wire.Identifier;
        "nativeVersion": string;
        "nativeExecutableHash": Wire.Hash;
        "method": string;
        "params": Wire.Json;
        "requestHash": Wire.Hash;
        "phase": "accepted" | "dispatched" | "succeeded" | "failed" | "outcome_unknown";
        "createdAt": string;
        "updatedAt": string;
        "epoch": (string) | (null);
        "requestId": (Agent.RequestId) | (null);
        "reply": (Agent.Reply) | (null);
        "code": (string) | (null);
    }) & (unknown) & (unknown) & (unknown) & (unknown) & (unknown) & (unknown);
    type InputIdentity = {
        "serviceNodeId": Wire.Identifier;
        "epoch": string;
        "requestId": Agent.RequestId;
    };
    type PendingInput = ({
        "identity": Agent.InputIdentity;
        "method": string;
        "params": Wire.Json;
        "observedAt": string;
        "updatedAt": string;
        "threadId": (string) | (null);
        "turnId": (string) | (null);
        "state": "pending" | "answering" | "answered" | "expired" | "outcome_unknown";
        "answerOperationId": (Wire.Identifier) | (null);
        "answerCallerPrincipalId": (Wire.Identifier) | (null);
        "reply": (Agent.Reply) | (null);
        "code": (string) | (null);
    }) & (unknown) & (unknown);
    type Notification = {
        "sequence": number;
        "serviceNodeId": Wire.Identifier;
        "epoch": string;
        "nativeVersion": string;
        "method": string;
        "params": Wire.Json;
        "observedAt": string;
    };
    type Status = {
        "serviceNodeId": Wire.Identifier;
        "hostId": Wire.Identifier;
        "nativeVersion": string;
        "nativeExecutableHash": Wire.Hash;
        "catalogHash": Wire.Hash;
        "epoch": (string) | (null);
        "pid": (number) | (null);
        "state": "starting" | "initializing" | "ready" | "stopping" | "stopped" | "failed";
        "observedAt": string;
        "code": (string) | (null);
        "initialized": (Wire.Json) | (null);
        "pendingInputs": number;
        "operations": {
            "retained": number;
            "maximum": number;
            "bytes": number;
            "maximumBytes": number;
        };
        "observedMethods": Array<{
            "method": string;
            "lastSucceededAt": (string) | (null);
            "lastFailedAt": (string) | (null);
            "lastCode": (string) | (null);
        }>;
        "windowsShell"?: (Agent.WindowsShellStatus) | (null);
        "instructions"?: Agent.InstructionsStatus;
        "environment"?: Agent.EnvironmentStatus;
        "capabilities"?: Agent.CapabilityProfile;
        "connection"?: {
            "mode": "owned-stdio" | "external-proxy";
            "ownsServer": boolean;
            "ownsHome": boolean;
            "actualHome": string;
            "actualVersion": string;
            "socketPath": (string) | (null);
            "serverPid": (number) | (null);
            "lifecycle": "started" | "alreadyRunning" | "explicit-endpoint" | "owned-stdio";
            "mcpIdentity": "shared-user-home" | "instance-environment";
        };
    };
    type ThreadSummary = {
        "nativeId": string;
        "owner": "this-native-connection" | "historical-unattached";
        "epoch": (string) | (null);
        "preview": string;
        "cwd": (string) | (null);
        "name": (string) | (null);
        "status": Wire.Json;
        "source": Wire.Json;
        "updatedAt": Wire.Json;
        "projectId": (string) | (null);
        "recencyAt": (number) | (null);
        "canAcceptDirectInput": (boolean) | (null);
        "threadSource": (string) | (null);
        "archived": boolean;
        "ephemeral": boolean;
    };
    type ProjectSummary = {
        "nativeId": string;
        "source": "native" | "host-configuration";
        "name": string;
        "paths": Array<string>;
        "kind"?: "existing" | "normal" | "internal" | "task";
        "position"?: number;
        "recencyAt"?: (number) | (null);
    };
    type StatusInput = {};
    type NativeDiscoveryInput = {
        "query"?: string;
        "method"?: string;
    };
    type NativeDiscovery = {
        "nativeVersion": string;
        "catalogHash": Wire.Hash;
        "items": Array<{
            "method": string;
            "description": string;
            "readOnlyHint": boolean;
            "expectedDefinitionHash": Wire.Hash;
            "inputSchema"?: (boolean) | ({
                [key: string]: Wire.Json;
            });
            "outputSchema"?: (boolean) | ({
                [key: string]: Wire.Json;
            });
        }>;
    };
    type FrameLimitsInput = {};
    type FrameLimits = {
        "serviceNodeId": Wire.Identifier;
        "nativeVersion": string;
        "nativeExecutableHash": Wire.Hash;
        "catalogHash": Wire.Hash;
        "epoch": (string) | (null);
        "requestFrameBytes": 6291456;
        "receivedFrameBytes": 25165824;
        "answerFrameBytes": 4194304;
        "managementFrameBytes": 33554432;
    };
    type OperationInput = {
        "operationId": Wire.Identifier;
    };
    type Interaction = {
        "operationId": Wire.Identifier;
        "epoch": string;
        "reply": Agent.Reply;
    };
    type PreventInput = {
        "operationId": Wire.Identifier;
        "nativeVersion": "0.154.0";
        "method": string;
        "params": Wire.Json;
        "expectedDefinitionHash": Wire.Hash;
    };
    type OperationAbsence = {
        "kind": "agent_operation_absent";
        "operationId": Wire.Identifier;
        "serviceNodeId": Wire.Identifier;
        "epoch": (string) | (null);
    };
    type PendingInputQuery = {
        "includeExpired"?: boolean;
        "limit"?: number;
        "identity"?: Agent.InputIdentity;
    };
    type PendingInputPage = {
        "epoch": (string) | (null);
        "items": Array<Agent.PendingInput>;
        "truncated": boolean;
    };
    type InputDefinitionQuery = {
        "identity": Agent.InputIdentity;
    };
    type InputDefinition = {
        "identity": Agent.InputIdentity;
        "method": string;
        "nativeVersion": string;
        "catalogHash": Wire.Hash;
        "responseSchema": (boolean) | ({
            [key: string]: Wire.Json;
        });
    };
    type AnswerInput = {
        "operationId": Wire.Identifier;
        "identity": Agent.InputIdentity;
        "reply": Agent.Reply;
    };
    type NotificationQuery = {
        "afterSequence": number;
        "limit"?: number;
    };
    type NotificationPage = {
        "epoch": (string) | (null);
        "firstAvailableSequence": number;
        "throughSequence": number;
        "gap": boolean;
        "hasMore": boolean;
        "items": Array<Agent.Notification>;
    };
    type ProjectsInput = {};
    type ProjectSelection = ({
        "kind": "existing";
        "cwd": string;
    }) | ({
        "kind": "normal" | "internal";
        "key": string;
        "name": string;
    }) | ({
        "kind": "task";
        "key": string;
        "name": string;
    });
    type ProjectResolveInput = {
        "selection": Agent.ProjectSelection;
        "expectedProjectId"?: string;
    };
    type ProjectLocation = {
        "project": Agent.ProjectSummary;
        "cwd": string;
        "kind": "existing" | "normal" | "internal" | "task";
    };
    type ProjectsResult = {
        "source": "native" | "host-configuration";
        "observedAt": string;
        "projects": Array<Agent.ProjectSummary>;
        "defaults"?: {
            "projectRoot": string;
            "internalProjectRoot": string;
        };
    };
    type WorkspaceRequirement = ({
        "kind": "task_workspace";
    }) | ({
        "kind": "existing_project";
        "projectId": string;
        "path": (string) | (null);
        "useWorktree": boolean;
    }) | ({
        "kind": "repository_path";
        "repositoryUrl": string;
        "folderName": string;
    }) | ({
        "kind": "new_project_path";
        "folderName": string;
    });
    type WorkspaceResolveInput = {
        "taskKey": string;
        "requirement": Agent.WorkspaceRequirement;
        "prepare": boolean;
        "verifyOrigin"?: boolean;
    };
    type RepositoryObservation = {
        "name": string;
        "branch": (string) | (null);
        "commit": (string) | (null);
        "dirty": boolean;
        "originName": (string) | (null);
        "originUrl": (string) | (null);
        "originState": "confirmed" | "local_only" | "no_origin" | "unknown";
        "remoteRef": (string) | (null);
        "observedAt": string;
        "limitation": (string) | (null);
    };
    type WorkspaceResolution = {
        "hostId": Wire.Identifier;
        "serviceNodeId": Wire.Identifier;
        "canonicalCwd": string;
        "taskRoot": string;
        "bootstrapPath": string;
        "intendedPath": string;
        "sourcePath": (string) | (null);
        "project": (Wire.ResourceRef) | (null);
        "useWorktree": boolean;
        "repository": (Agent.RepositoryObservation) | (null);
    };
    type Read01540ThreadReadd0 = {
        "includeTurns"?: boolean;
        "threadId": string;
    };
    type Read01540ThreadTurnsListd0 = {
        "cursor"?: (string) | (null);
        "itemsView"?: (Agent.Read01540ThreadTurnsListd1) | (null);
        "limit"?: (number) | (null);
        "sortDirection"?: (Agent.Read01540ThreadTurnsListd2) | (null);
        "threadId": string;
    };
    type Read01540ThreadTurnsListd1 = ("notLoaded") | ("summary") | ("full");
    type Read01540ThreadTurnsListd2 = "asc" | "desc";
    type Read01540ThreadItemsListd0 = {
        "cursor"?: (string) | (null);
        "limit"?: (number) | (null);
        "sortDirection"?: (Agent.Read01540ThreadItemsListd1) | (null);
        "threadId": string;
        "turnId"?: (string) | (null);
    };
    type Read01540ThreadItemsListd1 = "asc" | "desc";
    type ReadInput = ({
        "nativeVersion": "0.154.0";
        "method": "thread/read";
        "params": Agent.Read01540ThreadReadd0;
    }) | ({
        "nativeVersion": "0.154.0";
        "method": "thread/turns/list";
        "params": Agent.Read01540ThreadTurnsListd0;
    }) | ({
        "nativeVersion": "0.154.0";
        "method": "thread/items/list";
        "params": Agent.Read01540ThreadItemsListd0;
    });
    type ReadObservation = ({
        "schemaVersion": 1;
        "observationId": Wire.Identifier;
        "callerPrincipalId": Wire.Identifier;
        "serviceNodeId": Wire.Identifier;
        "nativeExecutableHash": Wire.Hash;
        "catalogHash": Wire.Hash;
        "epoch": string;
        "requestId": Agent.RequestId;
        "observedAt": string;
        "requestHash": Wire.Hash;
        "nativeVersion": "0.154.0";
        "method": "thread/read";
        "params": Agent.Read01540ThreadReadd0;
        "reply": Agent.Reply;
    }) | ({
        "schemaVersion": 1;
        "observationId": Wire.Identifier;
        "callerPrincipalId": Wire.Identifier;
        "serviceNodeId": Wire.Identifier;
        "nativeExecutableHash": Wire.Hash;
        "catalogHash": Wire.Hash;
        "epoch": string;
        "requestId": Agent.RequestId;
        "observedAt": string;
        "requestHash": Wire.Hash;
        "nativeVersion": "0.154.0";
        "method": "thread/turns/list";
        "params": Agent.Read01540ThreadTurnsListd0;
        "reply": Agent.Reply;
    }) | ({
        "schemaVersion": 1;
        "observationId": Wire.Identifier;
        "callerPrincipalId": Wire.Identifier;
        "serviceNodeId": Wire.Identifier;
        "nativeExecutableHash": Wire.Hash;
        "catalogHash": Wire.Hash;
        "epoch": string;
        "requestId": Agent.RequestId;
        "observedAt": string;
        "requestHash": Wire.Hash;
        "nativeVersion": "0.154.0";
        "method": "thread/items/list";
        "params": Agent.Read01540ThreadItemsListd0;
        "reply": Agent.Reply;
    });
}
export declare namespace TaskBoard {
    type NativePlan = Agent.Plan;
    type NativePlanDraft = Agent.PlanDraft;
    type NativeRequest = {
        "nativeVersion": Wire.Identifier;
        "catalogSourceHash": Wire.Hash;
        "method": "thread/start" | "thread/resume" | "turn/start" | "turn/interrupt";
        "params": Agent.ParametersRef;
    };
    type ObjectPin = {
        "objectId": Wire.Identifier;
        "revision": number;
    };
    type Target = {
        "hostId": Wire.Identifier;
        "serviceNodeId": Wire.Identifier;
    };
    type Capability = {
        "key": string;
        "label": string;
    };
    type ExecutionRequirement = ({
        "kind": "host";
        "hostId": Wire.Identifier;
    }) | ({
        "kind": "capability";
        "capabilityKey": Wire.Identifier;
    });
    type NativeOptions = {
        "model": (string) | (null);
        "reasoningEffort": (string) | (null);
        "serviceTier": ("fast" | "flex") | (null);
    };
    type WorkspaceRequirement = ({
        "kind": "task_workspace";
    }) | ({
        "kind": "existing_project";
        "projectId": Wire.Identifier;
        "path": (string) | (null);
        "useWorktree": boolean;
    }) | ({
        "kind": "repository_path";
        "repositoryUrl": string;
        "folderName": string;
    }) | ({
        "kind": "new_project_path";
        "folderName": string;
    });
    type WorkspaceResolution = {
        "hostId": Wire.Identifier;
        "serviceNodeId": Wire.Identifier;
        "canonicalCwd": string;
        "taskRoot": string;
        "bootstrapPath": string;
        "intendedPath": string;
        "sourcePath": (string) | (null);
        "project": (Wire.ResourceRef) | (null);
        "useWorktree": boolean;
        "repository": ({
            "name": string;
            "branch": (string) | (null);
            "commit": (string) | (null);
            "dirty": boolean;
            "originName": (string) | (null);
            "originUrl": (string) | (null);
            "originState": "confirmed" | "local_only" | "no_origin" | "unknown";
            "remoteRef": (string) | (null);
            "observedAt": string;
            "limitation": (string) | (null);
        }) | (null);
    };
    type Actor = {
        "principalId": Wire.Identifier;
        "serviceNodeId": Wire.Identifier;
        "generation": number;
        "source": "user" | "worker" | "scheduler" | "recovery" | "native";
    };
    type Artifact = {
        "object": TaskBoard.ObjectPin;
        "label": string;
        "mediaType": string;
        "contentHash": Wire.Hash;
        "byteLength"?: number;
    };
    type CodeReference = {
        "kind": "repository" | "commit" | "pull_request" | "issue" | "file";
        "url": string;
        "label": string;
        "commit": (string) | (null);
    };
    type Check = {
        "name": string;
        "status": "passed" | "failed" | "not_run" | "blocked";
        "detail": string;
        "evidence": Array<TaskBoard.Artifact>;
    };
    type RepositoryResult = {
        "hostId": Wire.Identifier;
        "serviceNodeId": Wire.Identifier;
        "project": Wire.ResourceRef;
        "repositoryName": string;
        "branch": (string) | (null);
        "commit": (string) | (null);
        "originName": (string) | (null);
        "originUrl": (string) | (null);
        "originState": "confirmed" | "local_only" | "no_origin" | "unknown";
        "remoteRef": (string) | (null);
        "observedAt": string;
        "limitation": (string) | (null);
    };
    type ResultContent = {
        "summary": string;
        "artifacts": Array<TaskBoard.Artifact>;
        "checks": Array<TaskBoard.Check>;
        "codeReferences": Array<TaskBoard.CodeReference>;
        "repositoryResult": (TaskBoard.RepositoryResult) | (null);
        "limitations": Array<string>;
    };
    type Waiting = {
        "reason": "user" | "time" | "dependency" | "host" | "capability" | "workspace" | "external_outcome" | "publication";
        "detail": string;
        "since": string;
    };
    type ChatTarget = {
        "serviceNodeId": Wire.Identifier;
        "expectedBridge": Chat.ExpectedBridge;
        "channel": Chat.Channel;
    };
    type PhoneTarget = {
        "serviceNodeId": Wire.Identifier;
        "recipientId": Wire.Identifier;
    };
    type TaskFields = {
        "title": string;
        "description": string;
        "acceptanceCriteria": Array<string>;
        "category": (string) | (null);
        "control": "user" | "agent";
        "priority": number;
        "executionRequirement": (TaskBoard.ExecutionRequirement) | (null);
        "nativeOptions"?: TaskBoard.NativeOptions;
        "workspaceRequirement": TaskBoard.WorkspaceRequirement;
        "dependencies": Array<Wire.Identifier>;
        "userContact": "ticket" | "chat" | "phone";
        "nextReviewAt": (string) | (null);
        "dueAt": (string) | (null);
    };
    type TaskAttachment = {
        "attachmentId": Wire.Identifier;
        "object": TaskBoard.ObjectPin;
        "filename": string;
        "mediaType": string;
        "byteLength": number;
        "contentHash": Wire.Hash;
        "uploader": TaskBoard.Actor;
        "createdAt": string;
    };
    type ApprovalRequest = {
        "requestId": Wire.Identifier;
        "type": "approval";
        "title": string;
        "detail": string;
    };
    type ApprovalResponse = {
        "requestId": Wire.Identifier;
        "type": "approval";
        "decision": "approved" | "rejected";
        "detail": string;
    };
    type CommentRequest = (TaskBoard.ApprovalRequest);
    type CommentResponse = (TaskBoard.ApprovalResponse);
    type Comment = {
        "commentId": Wire.Identifier;
        "sequence": number;
        "author": TaskBoard.Actor;
        "authorKind": "user" | "agent" | "system";
        "body": string;
        "requests": Array<TaskBoard.CommentRequest>;
        "responses": Array<TaskBoard.CommentResponse>;
        "delivery": (TaskBoard.ObjectPin) | (null);
        "attachments": Array<TaskBoard.TaskAttachment>;
        "run": (TaskBoard.ObjectPin) | (null);
        "turnId": (Wire.Identifier) | (null);
        "replyTo": (Wire.Identifier) | (null);
        "createdAt": string;
        "operationId": Wire.Identifier;
    };
    type CommentDelivery = {
        "commentId": Wire.Identifier;
        "state": "queued" | "delivering" | "delivered" | "outcome_unknown";
        "operationId": Wire.Identifier;
        "updatedAt": string;
        "code": (Wire.Identifier) | (null);
    };
    type PhoneDeliveryRequest = {
        "operationId": Wire.Identifier;
        "recipientId": Wire.Identifier;
        "route": "voice";
        "voicePrompt": string;
    };
    type Delivery = {
        "schemaVersion": 1;
        "taskId": Wire.Identifier;
        "commentId": Wire.Identifier;
        "publication": TaskBoard.ObjectPin;
        "requestedRoute": "chat" | "phone";
        "activeRoute": "chat" | "phone";
        "state": "pending" | "dispatching" | "confirmed" | "outcome_unknown";
        "chatTarget": (TaskBoard.ChatTarget) | (null);
        "phoneTarget": (TaskBoard.PhoneTarget) | (null);
        "operationId": (Wire.Identifier) | (null);
        "chatRequest": (Chat.NotifyRequest) | (null);
        "phoneRequest": (TaskBoard.PhoneDeliveryRequest) | (null);
        "phoneCallId": (Wire.Identifier) | (null);
        "deliveryEvidence": (TaskBoard.ObjectPin) | (null);
        "code": (Wire.Identifier) | (null);
        "createdAt": string;
        "updatedAt": string;
    };
    type TaskKeySequence = {
        "schemaVersion": 1;
        "nextValue": number;
        "updatedAt": string;
    };
    type TaskKeyAllocation = {
        "schemaVersion": 1;
        "operationId": Wire.Identifier;
        "taskKey": Wire.Identifier;
        "createdAt": string;
    };
    type CommentReadState = {
        "schemaVersion": 1;
        "principalId": Wire.Identifier;
        "taskId": Wire.Identifier;
        "readAgentCommentCount": number;
        "updatedAt": string;
    };
    type TaskFieldsInput = {
        "title": string;
        "description": string;
        "acceptanceCriteria": Array<string>;
        "category": (string) | (null);
        "control": "user" | "agent";
        "priority": number;
        "executionRequirement": (TaskBoard.ExecutionRequirement) | (null);
        "nativeOptions"?: TaskBoard.NativeOptions;
        "workspaceRequirement": TaskBoard.WorkspaceRequirement;
        "dependencies": Array<Wire.Identifier>;
        "userContact"?: "ticket" | "chat" | "phone";
        "nextReviewAt": (string) | (null);
        "dueAt": (string) | (null);
    };
    type ExpectedWorkspace = {
        "principalId": Wire.Identifier;
        "rootObjectId": (Wire.Identifier) | (null);
        "callerPrincipalId": Wire.Identifier;
    };
    type SavePlanRequest = {
        "action": "savePlan";
        "operationId": Wire.Identifier;
        "expectedWorkspace"?: TaskBoard.ExpectedWorkspace;
        "plan": TaskBoard.NativePlan;
        "draftHash": Wire.Hash;
    };
    type SavePlanInput = {
        "action": "savePlan";
        "operationId": Wire.Identifier;
        "expectedWorkspace"?: TaskBoard.ExpectedWorkspace;
        "plan": TaskBoard.NativePlanDraft;
    };
    type CreateRequest = {
        "action": "create";
        "operationId": Wire.Identifier;
        "expectedWorkspace"?: TaskBoard.ExpectedWorkspace;
        "fields": TaskBoard.TaskFieldsInput;
    };
    type EditRequest = {
        "action": "edit";
        "operationId": Wire.Identifier;
        "taskId": Wire.Identifier;
        "expectedRevision": number;
        "expectedWorkspace"?: TaskBoard.ExpectedWorkspace;
        "fields": TaskBoard.TaskFieldsInput;
    };
    type TransitionRequest = {
        "action": "transition";
        "operationId": Wire.Identifier;
        "taskId": Wire.Identifier;
        "expectedRevision": number;
        "expectedWorkspace"?: TaskBoard.ExpectedWorkspace;
        "workflowState": "todo" | "in_progress" | "waiting" | "done" | "cancelled";
        "detail": (string) | (null);
    };
    type StartRequest = {
        "action": "start";
        "operationId": Wire.Identifier;
        "taskId": Wire.Identifier;
        "expectedRevision": number;
        "expectedWorkspace"?: TaskBoard.ExpectedWorkspace;
        "target": TaskBoard.Target;
        "intent": TaskBoard.ObjectPin;
        "workspace": TaskBoard.WorkspaceResolution;
        "commentIds": Array<Wire.Identifier>;
    };
    type DeferRequest = {
        "action": "defer";
        "operationId": Wire.Identifier;
        "taskId": Wire.Identifier;
        "expectedRevision": number;
        "expectedWorkspace"?: TaskBoard.ExpectedWorkspace;
        "reason": "user" | "time" | "dependency" | "host" | "capability" | "workspace";
        "detail": string;
        "nextReviewAt": (string) | (null);
    };
    type ReassignRequest = {
        "action": "reassign";
        "operationId": Wire.Identifier;
        "taskId": Wire.Identifier;
        "expectedRevision": number;
        "expectedWorkspace"?: TaskBoard.ExpectedWorkspace;
        "executionRequirement": (TaskBoard.ExecutionRequirement) | (null);
        "workspaceRequirement": TaskBoard.WorkspaceRequirement;
        "reason": string;
        "previousRun": (TaskBoard.ObjectPin) | (null);
        "acknowledgeUnresolved": boolean;
    };
    type CancelRequest = {
        "action": "cancel";
        "operationId": Wire.Identifier;
        "taskId": Wire.Identifier;
        "expectedRevision": number;
        "expectedWorkspace"?: TaskBoard.ExpectedWorkspace;
        "reason": string;
    };
    type ReviewRequest = {
        "action": "review";
        "operationId": Wire.Identifier;
        "taskId": Wire.Identifier;
        "expectedRevision": number;
        "expectedWorkspace"?: TaskBoard.ExpectedWorkspace;
        "result": TaskBoard.ObjectPin;
        "acceptance": string;
    };
    type ContinueRequest = {
        "action": "continue";
        "operationId": Wire.Identifier;
        "taskId": Wire.Identifier;
        "expectedRevision": number;
        "expectedWorkspace"?: TaskBoard.ExpectedWorkspace;
        "feedback": string;
        "target": TaskBoard.Target;
        "intent": TaskBoard.ObjectPin;
        "workspace": TaskBoard.WorkspaceResolution;
        "commentIds": Array<Wire.Identifier>;
    };
    type SaveResultRequest = {
        "action": "saveResult";
        "operationId": Wire.Identifier;
        "taskId": Wire.Identifier;
        "expectedRevision": number;
        "expectedWorkspace"?: TaskBoard.ExpectedWorkspace;
        "run": (TaskBoard.ObjectPin) | (null);
        "content": TaskBoard.ResultContent;
        "kind": "intermediate" | "final";
    };
    type CommentActionRequest = {
        "action": "comment";
        "operationId": Wire.Identifier;
        "taskId": Wire.Identifier;
        "expectedRevision": number;
        "expectedWorkspace"?: TaskBoard.ExpectedWorkspace;
        "commentId": Wire.Identifier;
        "body": string;
        "requests": Array<TaskBoard.CommentRequest>;
        "responses": Array<TaskBoard.CommentResponse>;
        "attachments": Array<TaskBoard.TaskAttachment>;
        "replyTo": (Wire.Identifier) | (null);
    };
    type UploadAttachmentRequest = {
        "action": "uploadAttachment";
        "operationId": Wire.Identifier;
        "taskId": Wire.Identifier;
        "expectedRevision": number;
        "expectedWorkspace"?: TaskBoard.ExpectedWorkspace;
        "attachmentId": Wire.Identifier;
        "filename": string;
        "mediaType": string;
        "bytesBase64": string;
    };
    type ArchiveRequest = {
        "action": "archive";
        "operationId": Wire.Identifier;
        "taskId": Wire.Identifier;
        "expectedRevision": number;
        "expectedWorkspace"?: TaskBoard.ExpectedWorkspace;
        "archived": true;
    };
    type ArchiveResult = Operation.ObjectMetadata;
    type OperationQuery = {
        "operationId": Wire.Identifier;
    };
    type CategoriesQuery = {};
    type CategoriesResult = {
        "categories": Array<string>;
    };
    type TaskQuery = {
        "task": (Wire.Identifier) | ({
            "taskKey": Wire.Identifier;
        });
    };
    type TaskListQuery = {
        "status"?: "backlog" | "todo" | "in_progress" | "waiting" | "review" | "done" | "cancelled";
        "category"?: string;
        "limit"?: number;
        "cursor"?: string;
    };
    type TaskSummary = {
        "taskId": Wire.Identifier;
        "revision": number;
        "taskKey": Wire.Identifier;
        "title": string;
        "workflowState": "backlog" | "todo" | "in_progress" | "waiting" | "review" | "done" | "cancelled";
        "category": (string) | (null);
    };
    type TaskListResult = {
        "items": Array<TaskBoard.TaskSummary>;
        "nextCursor": (string) | (null);
    };
    type MarkCommentsReadRequest = {
        "operationId": Wire.Identifier;
        "taskId": Wire.Identifier;
        "expectedTaskRevision": number;
    };
    type MarkCommentsReadResult = {
        "task": TaskBoard.ObjectPin;
        "readState": TaskBoard.ObjectPin;
        "unreadAgentComments": 0;
    };
    type ExecutionCondition = {
        "state": "idle" | "queued" | "allocating" | "starting" | "active" | "needs_user" | "deferred" | "blocked_dependency" | "blocked_environment" | "recovering" | "outcome_unknown" | "publishing";
        "detail": string;
    };
    type TaskView = {
        "object": TaskBoard.ObjectPin;
        "task": TaskBoard.Task;
        "executionCondition": TaskBoard.ExecutionCondition;
        "unreadAgentComments": number;
    };
    type ExecutionRequest = (TaskBoard.StartRequest) | (TaskBoard.ContinueRequest);
    type ActionRequest = (TaskBoard.SavePlanRequest) | (TaskBoard.CreateRequest) | (TaskBoard.EditRequest) | (TaskBoard.TransitionRequest) | (TaskBoard.DeferRequest) | (TaskBoard.ReassignRequest) | (TaskBoard.CancelRequest) | (TaskBoard.ReviewRequest) | (TaskBoard.SaveResultRequest) | (TaskBoard.CommentActionRequest) | (TaskBoard.UploadAttachmentRequest);
    type ActionInput = (TaskBoard.SavePlanInput) | (TaskBoard.CreateRequest) | (TaskBoard.EditRequest) | (TaskBoard.TransitionRequest) | (TaskBoard.DeferRequest) | (TaskBoard.ReassignRequest) | (TaskBoard.CancelRequest) | (TaskBoard.ReviewRequest) | (TaskBoard.SaveResultRequest) | (TaskBoard.CommentActionRequest) | (TaskBoard.UploadAttachmentRequest);
    type Claim = {
        "operationId": Wire.Identifier;
        "callerPrincipalId": Wire.Identifier;
        "attempt": number;
        "owner": TaskBoard.Actor;
        "request": TaskBoard.ExecutionRequest;
        "requestHash": Wire.Hash;
        "target": TaskBoard.Target;
        "workspace": TaskBoard.WorkspaceResolution;
        "primaryAtStart": (Wire.ResourceRef) | (null);
        "claimedAt": string;
        "phase": "claimed" | "run_created" | "starting" | "running" | "waiting_input" | "publishing_result" | "cancel_requested" | "outcome_unknown";
        "run": (TaskBoard.ObjectPin) | (null);
    };
    type Task = ({
        "schemaVersion": 1;
        "taskKey": Wire.Identifier;
        "fields": TaskBoard.TaskFields;
        "workflowState": "backlog" | "todo" | "in_progress" | "waiting" | "review" | "done" | "cancelled";
        "waiting": (TaskBoard.Waiting) | (null);
        "publication": (TaskBoard.ObjectPin) | (null);
        "primaryResourceRef": (Wire.ResourceRef) | (null);
        "claim": (TaskBoard.Claim) | (null);
        "attemptCount": number;
        "lastRun": (TaskBoard.ObjectPin) | (null);
        "lastHistory": (TaskBoard.ObjectPin) | (null);
        "latestResult": (TaskBoard.ObjectPin) | (null);
        "acceptedReview": (TaskBoard.ObjectPin) | (null);
        "comments": Array<TaskBoard.Comment>;
        "commentDeliveries": Array<TaskBoard.CommentDelivery>;
        "agentCommentCount": number;
        "workRevision": number;
        "attachments": Array<TaskBoard.TaskAttachment>;
        "createdAt": string;
        "updatedAt": string;
    }) & (unknown) & (unknown) & (unknown) & (unknown) & (unknown) & (unknown);
    type NativeCallOrigin = ({
        "kind": "initial";
    }) | ({
        "kind": "reattach";
        "predecessor": TaskBoard.ObjectPin;
        "preparedEpoch": Wire.Identifier;
    });
    type NativeCall = ({
        "operationId": Wire.Identifier;
        "request": TaskBoard.NativeRequest;
        "expectedDefinitionHash": Wire.Hash;
        "origin"?: TaskBoard.NativeCallOrigin;
        "state": "prepared" | "observed" | "outcome_unknown";
        "preparedAt": string;
        "observedAt": (string) | (null);
        "ownerPhase": ("accepted" | "dispatched" | "succeeded" | "failed" | "outcome_unknown") | (null);
        "epoch": (Wire.Identifier) | (null);
        "evidence": (TaskBoard.Artifact) | (null);
        "code": (Wire.Identifier) | (null);
    }) & (unknown) & (unknown);
    type Run = ({
        "schemaVersion": 1;
        "taskId": Wire.Identifier;
        "attempt": number;
        "operationId": Wire.Identifier;
        "callerPrincipalId": Wire.Identifier;
        "originalRequest": TaskBoard.ExecutionRequest;
        "requestHash": Wire.Hash;
        "target": TaskBoard.Target;
        "workspace": TaskBoard.WorkspaceResolution;
        "plan": TaskBoard.NativePlanDraft;
        "owner": TaskBoard.Actor;
        "phase": "starting" | "running" | "waiting_input" | "publishing_result" | "completed" | "failed" | "cancel_requested" | "cancelled" | "outcome_unknown";
        "primaryResourceRef": (Wire.ResourceRef) | (null);
        "turnId": (Wire.Identifier) | (null);
        "nativeEpoch": (Wire.Identifier) | (null);
        "calls": {
            "thread": (TaskBoard.ObjectPin) | (null);
            "turn": (TaskBoard.ObjectPin) | (null);
            "interrupt": (TaskBoard.ObjectPin) | (null);
        };
        "notificationCursor": number;
        "result": (TaskBoard.ObjectPin) | (null);
        "cancellation": ({
            "operationId": Wire.Identifier;
            "reason": string;
            "requestedAt": string;
            "actor": TaskBoard.Actor;
        }) | (null);
        "externalOutcome": {
            "state": "not_started" | "active" | "succeeded" | "failed" | "cancelled" | "unknown";
            "observedAt": (string) | (null);
            "evidence": (TaskBoard.Artifact) | (null);
            "code": (Wire.Identifier) | (null);
        };
        "createdAt": string;
        "updatedAt": string;
        "finishedAt": (string) | (null);
    }) & (unknown) & (unknown) & (unknown) & (unknown) & (unknown);
    type Result = {
        "schemaVersion": 1;
        "taskId": Wire.Identifier;
        "run": (TaskBoard.ObjectPin) | (null);
        "kind": "intermediate" | "final";
        "content": TaskBoard.ResultContent;
        "createdAt": string;
        "actor": TaskBoard.Actor;
        "operationId": Wire.Identifier;
    };
    type Review = {
        "schemaVersion": 1;
        "taskId": Wire.Identifier;
        "result": TaskBoard.ObjectPin;
        "acceptance": string;
        "createdAt": string;
        "actor": TaskBoard.Actor;
        "operationId": Wire.Identifier;
    };
    type History = {
        "schemaVersion": 1;
        "taskId": Wire.Identifier;
        "operationId": Wire.Identifier;
        "actor": TaskBoard.Actor;
        "createdAt": string;
        "kind": "created" | "edited" | "ready" | "started" | "deferred" | "reassigned" | "cancel_requested" | "cancelled" | "reviewed" | "continued" | "result_saved" | "commented" | "recovered" | "outcome_unknown";
        "detail": string;
        "previousTask": (TaskBoard.ObjectPin) | (null);
        "previousHistory": (TaskBoard.ObjectPin) | (null);
        "fromTarget": (TaskBoard.Target) | (null);
        "toTarget": (TaskBoard.Target) | (null);
        "priorRun": (TaskBoard.ObjectPin) | (null);
        "result": (TaskBoard.ObjectPin) | (null);
        "review": (TaskBoard.ObjectPin) | (null);
    };
    type ActionOutcome = {
        "operationId": Wire.Identifier;
        "task": (TaskBoard.ObjectPin) | (null);
        "plan": (TaskBoard.ObjectPin) | (null);
        "run": (TaskBoard.ObjectPin) | (null);
        "history": (TaskBoard.ObjectPin) | (null);
        "result": (TaskBoard.ObjectPin) | (null);
        "review": (TaskBoard.ObjectPin) | (null);
        "attachment": (TaskBoard.TaskAttachment) | (null);
    };
    type NativeUpdateRequest = {
        "action": "nativeUpdate";
        "operationId": Wire.Identifier;
        "taskId": Wire.Identifier;
        "expectedRevision": number;
        "run": TaskBoard.ObjectPin;
        "change": ({
            "kind": "attachCall";
            "slot": "thread" | "turn" | "interrupt";
            "call": TaskBoard.ObjectPin;
        }) | ({
            "kind": "reattachCall";
            "call": TaskBoard.ObjectPin;
        }) | ({
            "kind": "observeCall";
            "slot": "thread" | "turn" | "interrupt";
            "call": TaskBoard.ObjectPin;
        }) | ({
            "kind": "snapshot";
            "evidence": TaskBoard.Artifact;
            "nativeOperationId": Wire.Identifier;
            "notificationCursor": number;
        }) | ({
            "kind": "readSnapshot";
            "snapshot": TaskBoard.Artifact;
            "notificationCursor": number;
        }) | ({
            "kind": "transcript";
            "evidence": TaskBoard.Artifact;
            "notificationCursor": number;
        }) | ({
            "kind": "unavailable";
            "reason": "host" | "external_outcome";
            "code": Wire.Identifier;
            "detail": string;
        }) | ({
            "kind": "cancelUnstarted";
        }) | ({
            "kind": "retireUnstarted";
        });
    };
    type DurableRequest = (TaskBoard.ActionRequest) | (TaskBoard.SchedulerRequest) | (TaskBoard.NativeUpdateRequest);
    type PreparedWrite = {
        "mutationId": Wire.Identifier;
        "contractKey": "task-board/task" | "task-board/run" | "task-board/result" | "task-board/review" | "task-board/history" | "task-board/delivery" | "task-board/native-plan" | "task-board/native-call";
        "contractVersion": "1.0.0" | "1.1.0";
        "payload": TaskBoard.Artifact;
        "destination": ({
            "create": {
                "parentId": (Wire.Identifier) | (null);
                "name": string;
            };
        }) | ({
            "objectId": Wire.Identifier;
            "expectedRevision": number;
        });
        "outcome": (TaskBoard.ObjectPin) | (null);
    };
    type Operation = ({
        "schemaVersion": 1;
        "operationId": Wire.Identifier;
        "callerPrincipalId": Wire.Identifier;
        "request": TaskBoard.DurableRequest;
        "requestHash": Wire.Hash;
        "actor": TaskBoard.Actor;
        "createdAt": string;
        "updatedAt": string;
        "phase": "accepted" | "preparing" | "publishing" | "succeeded" | "failed" | "needs_attention";
        "writes": Array<TaskBoard.PreparedWrite>;
        "outcome": (TaskBoard.ActionOutcome) | (null);
        "error": ({
            "code": Wire.Identifier;
            "message": string;
            "execution": "not_executed" | "completed" | "unknown";
        }) | (null);
    }) & (unknown) & (unknown) & (unknown) & (unknown);
    type Settings = {
        "principalId": Wire.Identifier;
        "rootObjectId": (Wire.Identifier) | (null);
        "scheduler": {
            "enabled": boolean;
            "intervalMs": number;
            "pageSize": number;
        };
        "chatTarget": (TaskBoard.ChatTarget) | (null);
        "phoneTarget": (TaskBoard.PhoneTarget) | (null);
    };
    type WorkspaceQuery = {};
    type WorkspaceInfo = {
        "serviceNodeId": Wire.Identifier;
        "generation": number;
        "principalId": Wire.Identifier;
        "rootObjectId": (Wire.Identifier) | (null);
        "callerPrincipalId": Wire.Identifier;
        "role": "user" | "worker" | "observer";
        "recovered": boolean;
        "observedAt": string;
        "scheduler": {
            "enabled": boolean;
            "intervalMs": number;
            "pageSize": number;
        };
        "chatTarget": (TaskBoard.ChatTarget) | (null);
        "phoneTarget": (TaskBoard.PhoneTarget) | (null);
    };
    type Coordination = {
        "schemaVersion": 1;
        "principalId": Wire.Identifier;
        "activeOperation": (TaskBoard.ObjectPin) | (null);
        "updatedAt": string;
    };
    type SchedulerRequest = (TaskBoard.StartRequest) | (TaskBoard.ContinueRequest) | (TaskBoard.TransitionRequest) | (TaskBoard.DeferRequest);
    type ScanCursor = {
        "schemaVersion": 1;
        "principalId": Wire.Identifier;
        "serviceNodeId": Wire.Identifier;
        "lane": "operations" | "runs" | "tasks" | "deliveries";
        "queryHash": Wire.Hash;
        "cursor": (string) | (null);
        "updatedAt": string;
    };
    type SchedulerAttempt = ({
        "schemaVersion": 1;
        "principalId": Wire.Identifier;
        "task": TaskBoard.ObjectPin;
        "sequence": number;
        "request": TaskBoard.SchedulerRequest;
        "requestHash": Wire.Hash;
        "previous": (TaskBoard.ObjectPin) | (null);
        "previousOperation": (TaskBoard.ObjectPin) | (null);
        "preparedAt": string;
    }) & (unknown);
    type NativeSignals = {
        "schemaVersion": 1;
        "run": TaskBoard.ObjectPin;
        "serviceNodeId": Wire.Identifier;
        "nativeVersion": Wire.Identifier;
        "epoch": Wire.Identifier;
        "threadId": Wire.Identifier;
        "turnId": Wire.Identifier;
        "observedAt": string;
        "inputsTruncated": boolean;
        "inputs": Array<{
            "identity": Agent.InputIdentity;
            "method": string;
            "turnId": (Wire.Identifier) | (null);
            "state": "pending" | "answering";
            "observedAt": string;
        }>;
        "notificationCursor": number;
        "notificationGap": boolean;
        "notificationsPending": boolean;
        "activity": Array<{
            "epoch": Wire.Identifier;
            "sequence": number;
            "method": string;
            "observedAt": string;
        }>;
    };
    type NativeEvidenceChunk = {
        "object": TaskBoard.ObjectPin;
        "contentHash": Wire.Hash;
        "byteLength": number;
    };
    type NativeEvidence = {
        "schemaVersion": 1;
        "format": "agent-operation/canonical-json";
        "operationId": Wire.Identifier;
        "callerPrincipalId": Wire.Identifier;
        "serviceNodeId": Wire.Identifier;
        "nativeVersion": Wire.Identifier;
        "nativeExecutableHash": Wire.Hash;
        "method": string;
        "requestHash": Wire.Hash;
        "phase": "accepted" | "dispatched" | "succeeded" | "failed" | "outcome_unknown";
        "epoch": (Wire.Identifier) | (null);
        "contentHash": Wire.Hash;
        "byteLength": number;
        "chunks": Array<TaskBoard.NativeEvidenceChunk>;
    };
    type NativeReadEvidence = {
        "schemaVersion": 1;
        "format": "agent-read-observation/canonical-json";
        "observationId": Wire.Identifier;
        "callerPrincipalId": Wire.Identifier;
        "serviceNodeId": Wire.Identifier;
        "nativeVersion": Wire.Identifier;
        "nativeExecutableHash": Wire.Hash;
        "catalogHash": Wire.Hash;
        "method": "thread/read" | "thread/turns/list" | "thread/items/list";
        "requestHash": Wire.Hash;
        "epoch": Wire.Identifier;
        "contentHash": Wire.Hash;
        "byteLength": number;
        "chunks": Array<TaskBoard.NativeEvidenceChunk>;
    };
    type NativeTurnSearch = ({
        "schemaVersion": 1;
        "run": TaskBoard.ObjectPin;
        "nativeVersion": Wire.Identifier;
        "serviceNodeId": Wire.Identifier;
        "epoch": Wire.Identifier;
        "threadId": Wire.Identifier;
        "turnId": Wire.Identifier;
        "phase": "searching" | "finished";
        "nextCursor": (string) | (null);
        "pageCount": number;
        "visitedCursorHashes": Array<Wire.Hash>;
        "createdAt": string;
        "updatedAt": string;
    }) & (unknown) & (unknown);
    type ReadEvidenceRef = {
        "artifact": TaskBoard.Artifact;
        "observationId": Wire.Identifier;
        "epoch": Wire.Identifier;
    };
    type NativeTurnSnapshot = {
        "schemaVersion": 1;
        "run": TaskBoard.ObjectPin;
        "nativeVersion": Wire.Identifier;
        "serviceNodeId": Wire.Identifier;
        "epoch": Wire.Identifier;
        "threadId": Wire.Identifier;
        "turnId": Wire.Identifier;
        "turnCursor": (string) | (null);
        "metadata": TaskBoard.ReadEvidenceRef;
        "turn": TaskBoard.ReadEvidenceRef;
        "status": "inProgress" | "completed" | "failed" | "interrupted";
        "createdAt": string;
    };
    type NativeResultPage = {
        "schemaVersion": 1;
        "runId": Wire.Identifier;
        "collectionId": Wire.Identifier;
        "previous": (TaskBoard.ObjectPin) | (null);
        "index": number;
        "cursor": (string) | (null);
        "nextCursor": (string) | (null);
        "evidence": TaskBoard.ReadEvidenceRef;
        "itemCount": number;
        "nativeBytes": number;
    };
    type NativeTranscript = ({
        "schemaVersion": 1;
        "collectionId": Wire.Identifier;
        "run": TaskBoard.ObjectPin;
        "snapshot": TaskBoard.Artifact;
        "phase": "collecting" | "complete";
        "head": (TaskBoard.ObjectPin) | (null);
        "pageCount": number;
        "nativeBytes": number;
        "nextCursor": (string) | (null);
        "verification": (TaskBoard.ReadEvidenceRef) | (null);
        "verificationCursor": (string) | (null);
        "createdAt": string;
        "updatedAt": string;
    }) & (unknown);
    type NativeFullTurnTranscript = {
        "schemaVersion": 1;
        "collectionId": Wire.Identifier;
        "run": TaskBoard.ObjectPin;
        "snapshot": TaskBoard.Artifact;
        "unsupportedItems": TaskBoard.ReadEvidenceRef;
        "fullTurn": TaskBoard.ReadEvidenceRef;
        "fullTurnCursor": (string) | (null);
        "fullTurnBytes": number;
        "verification": TaskBoard.ReadEvidenceRef;
        "verificationCursor": (string) | (null);
        "createdAt": string;
    };
}
export declare namespace Automation {
    type Date = string;
    type LocalTime = string;
    type Timestamp = string;
    type Definition = {
        "scheduleId": string;
        "principalId": Wire.Identifier;
        "rootObjectId": (Wire.Identifier) | (null);
        "timeZone": string;
        "firstDate": Automation.Date;
        "localTime": Automation.LocalTime;
        "catchUp": "all" | "latest";
        "subscriptionName": string;
        "inputSource": string;
        "inputTopic": string;
        "initialSequence": number;
    };
    type Settings = {
        "definition": Automation.Definition;
        "maximumPeriodsPerTick": number;
        "pollMs": number;
    };
    type Pending = {
        "periodDate": Automation.Date;
        "advanceToDate": Automation.Date;
        "skippedFromDate": (Automation.Date) | (null);
        "skippedThroughDate": (Automation.Date) | (null);
        "snapshot": (Operation.Status) | (null);
    };
    type Checkpoint = {
        "schemaVersion": 1;
        "definition": Automation.Definition;
        "definitionHash": Wire.Hash;
        "nextDate": Automation.Date;
        "pending": (Automation.Pending) | (null);
    };
    type Period = {
        "schemaVersion": 1;
        "definitionHash": Wire.Hash;
        "periodDate": Automation.Date;
        "timeZone": string;
        "localTime": Automation.LocalTime;
        "skippedFromDate": (Automation.Date) | (null);
        "skippedThroughDate": (Automation.Date) | (null);
        "snapshot": Operation.Status;
    };
    type EventReceipt = {
        "schemaVersion": 1;
        "definitionHash": Wire.Hash;
        "sequence": number;
        "topic": string;
        "topicVersion": Wire.ContractVersion;
        "source": string;
        "occurredAt": Automation.Timestamp;
        "sourceMutationId": Wire.Identifier;
        "payloadHash": Wire.Hash;
    };
}
export declare namespace Chat {
    type NativePlan = Agent.Plan;
    type NativePlanDraft = Agent.PlanDraft;
    type NativeRequest = Agent.InvocationDraft;
    type ObjectPin = {
        "objectId": Wire.Identifier;
        "revision": number;
    };
    type Channel = {
        "adapter": "webchat" | "whatsapp";
        "accountId": Wire.Identifier;
        "channelId": Wire.Identifier;
    };
    type ChannelConfiguration = {
        "channel": Chat.Channel;
        "displayName": string;
    };
    type Definition = {
        "workspaceId": Wire.Identifier;
        "principalId": Wire.Identifier;
        "rootObjectId": (Wire.Identifier) | (null);
        "project": Wire.ResourceRef;
        "nativePlan": Chat.NativePlan;
        "channels": Array<Chat.ChannelConfiguration>;
    };
    type Settings = {
        "definition": Chat.Definition;
        "pollMs": number;
        "whatsappConfigPath"?: string;
        "language"?: "en" | "de";
    };
    type ExpectedBridge = {
        "principalId": Wire.Identifier;
        "callerPrincipalId": Wire.Identifier;
        "workspaceId": Wire.Identifier;
        "definitionHash": Wire.Hash;
    };
    type Artifact = {
        "object": Chat.ObjectPin;
        "contentHash": Wire.Hash;
        "byteLength": number;
        "mediaType": string;
        "label": string;
    };
    type Image = {
        "object": Chat.ObjectPin;
        "contentHash": Wire.Hash;
        "byteLength": number;
        "mediaType": "image/png" | "image/jpeg" | "image/webp";
        "label": string;
    };
    type Payload = {
        "text": string;
        "images": Array<Chat.Image>;
        "turnOptions"?: {
            "model"?: (string) | (null);
            "effort"?: ("none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra") | (null);
            "serviceTier"?: ("fast" | "flex") | (null);
        };
    };
    type Error = {
        "code": Wire.Identifier;
        "outcome": "not_executed" | "unknown";
        "detail": string;
    };
    type InputIdentity = {
        "channel": Chat.Channel;
        "senderPrincipalId": Wire.Identifier;
        "messageId": Wire.Identifier;
    };
    type CreateMainRequest = {
        "action": "createMain";
        "operationId": Wire.Identifier;
        "expectedBridge": Chat.ExpectedBridge;
        "expectedMainRevision": (number) | (null);
        "reason": string;
    };
    type SendRequest = {
        "action": "send";
        "operationId": Wire.Identifier;
        "expectedBridge": Chat.ExpectedBridge;
        "channel": Chat.Channel;
        "messageId": Wire.Identifier;
        "expectedBinding": (Chat.ObjectPin) | (null);
        "payload": Chat.Payload;
    };
    type CancelRequest = {
        "action": "cancel";
        "operationId": Wire.Identifier;
        "expectedBridge": Chat.ExpectedBridge;
        "inputId": Wire.Identifier;
        "expectedRevision": number;
        "reason": string;
    };
    type ReceiveRequest = {
        "action": "receive";
        "operationId": Wire.Identifier;
        "expectedBridge": Chat.ExpectedBridge;
        "channel": Chat.Channel;
        "afterSequence": number;
        "limit": number;
    };
    type AcknowledgeRequest = {
        "action": "acknowledge";
        "operationId": Wire.Identifier;
        "expectedBridge": Chat.ExpectedBridge;
        "offerId": Wire.Identifier;
        "replyIds": Array<Wire.Identifier>;
    };
    type OperationQuery = {
        "operationId": Wire.Identifier;
        "expectedBridge": Chat.ExpectedBridge;
    };
    type WorkspaceQuery = {};
    type HistoryQuery = {
        "expectedBridge": Chat.ExpectedBridge;
        "channel": Chat.Channel;
        "afterSequence": number;
        "limit": number;
    };
    type InputQuery = {
        "expectedBridge": Chat.ExpectedBridge;
        "inputId": Wire.Identifier;
    };
    type NotifyRequest = {
        "action": "notify";
        "operationId": Wire.Identifier;
        "expectedBridge": Chat.ExpectedBridge;
        "channel": Chat.Channel;
        "source": Chat.ObjectPin;
        "text": string;
    };
    type NoticeQuery = {
        "expectedBridge": Chat.ExpectedBridge;
        "operationId": Wire.Identifier;
    };
    type ActionRequest = (Chat.CreateMainRequest) | (Chat.SendRequest) | (Chat.CancelRequest) | (Chat.ReceiveRequest) | (Chat.AcknowledgeRequest) | (Chat.NotifyRequest);
    type Binding = {
        "schemaVersion": 1;
        "workspaceId": Wire.Identifier;
        "definitionHash": Wire.Hash;
        "project": Wire.ResourceRef;
        "primary": Wire.ResourceRef;
        "nativePlan": Chat.NativePlan;
        "createdAt": string;
        "operationId": Wire.Identifier;
        "predecessor": (Chat.ObjectPin) | (null);
    };
    type Ticket = {
        "inputId": Wire.Identifier;
        "identity": Chat.InputIdentity;
        "requestHash": Wire.Hash;
        "binding": Chat.ObjectPin;
        "sequence": number;
        "admittedAt": string;
    };
    type Conversation = {
        "schemaVersion": 1;
        "workspaceId": Wire.Identifier;
        "definitionHash": Wire.Hash;
        "createdAt": string;
    };
    type ResultPublication = {
        "inputId": Wire.Identifier;
        "collection": Chat.ObjectPin;
        "result": Chat.ObjectPin;
        "firstSequence": number;
        "partCount": number;
        "publishedParts": number;
        "createdAt": string;
    };
    type Main = {
        "schemaVersion": 1;
        "definition": Chat.Definition;
        "definitionHash": Wire.Hash;
        "conversation": Chat.ObjectPin;
        "binding": (Chat.ObjectPin) | (null);
        "pendingAction": (Chat.ObjectPin) | (null);
        "publication": (Chat.ResultPublication) | (null);
        "queue": Array<Chat.Ticket>;
        "nextSequence": number;
        "updatedAt": string;
    };
    type NativeCall = ({
        "schemaVersion": 1;
        "operationId": Wire.Identifier;
        "serviceNodeId": Wire.Identifier;
        "callerPrincipalId": Wire.Identifier;
        "request": Chat.ObjectPin;
        "nativeVersion": Wire.Identifier;
        "catalogSourceHash": Wire.Hash;
        "method": "thread/start" | "thread/resume" | "turn/start" | "turn/interrupt";
        "predecessor": (Chat.ObjectPin) | (null);
        "preparedEpoch": (Wire.Identifier) | (null);
        "requestHash": Wire.Hash;
        "expectedDefinitionHash": Wire.Hash;
        "createdAt": string;
        "state": "prepared" | "accepted" | "dispatched" | "succeeded" | "failed" | "outcome_unknown";
        "epoch": (Wire.Identifier) | (null);
        "evidence": (Chat.ObjectPin) | (null);
        "code": (Wire.Identifier) | (null);
        "updatedAt": string;
    }) & (unknown) & (unknown) & (unknown) & (unknown) & (unknown);
    type Input = ({
        "schemaVersion": 1;
        "workspaceId": Wire.Identifier;
        "definitionHash": Wire.Hash;
        "identity": Chat.InputIdentity;
        "operationId": Wire.Identifier;
        "requestHash": Wire.Hash;
        "binding": Chat.ObjectPin;
        "sequence": number;
        "payload": Chat.Payload;
        "state": "queued" | "preparing" | "delivering" | "running" | "waiting_input" | "collecting" | "completed" | "failed" | "cancel_requested" | "cancelled" | "outcome_unknown";
        "nativeCalls": {
            "resume": (Chat.ObjectPin) | (null);
            "turn": (Chat.ObjectPin) | (null);
            "interrupt": (Chat.ObjectPin) | (null);
        };
        "turnId": (Wire.Identifier) | (null);
        "epoch": (Wire.Identifier) | (null);
        "result": (Chat.ObjectPin) | (null);
        "error": (Chat.Error) | (null);
        "cancellation": ({
            "operationId": Wire.Identifier;
            "callerPrincipalId": Wire.Identifier;
            "reason": string;
            "requestedAt": string;
        }) | (null);
        "admittedAt": string;
        "updatedAt": string;
        "finishedAt": (string) | (null);
    }) & (unknown) & (unknown) & (unknown) & (unknown) & (unknown) & (unknown) & (unknown) & (unknown);
    type Evidence = {
        "schemaVersion": 1;
        "format": "native-request/canonical-json" | "agent-operation/canonical-json" | "agent-read-observation/canonical-json";
        "contentHash": Wire.Hash;
        "byteLength": number;
        "chunks": Array<Chat.Artifact>;
    };
    type Collection = ({
        "schemaVersion": 1;
        "input": Chat.ObjectPin;
        "binding": Chat.ObjectPin;
        "turnCall": Chat.ObjectPin;
        "turnId": Wire.Identifier;
        "epoch": Wire.Identifier;
        "attempt": number;
        "previousAttempt": (Chat.ObjectPin) | (null);
        "reads": Array<Chat.ObjectPin>;
        "nativeBytes": number;
        "state": "collecting" | "ready" | "complete" | "failed";
        "result": (Chat.ObjectPin) | (null);
        "error": (Chat.Error) | (null);
        "createdAt": string;
        "updatedAt": string;
        "retryAuthorization"?: ({
            "callerPrincipalId": Wire.Identifier;
            "operationId": Wire.Identifier;
            "reason": string;
            "failed": Chat.ObjectPin;
        }) | (null);
    }) & (unknown) & (unknown) & (unknown) & (unknown) & (unknown) & (unknown);
    type RetryCollectionRequest = {
        "inputId": Wire.Identifier;
        "expectedCollection": Chat.ObjectPin;
        "expectedBridge": Chat.ExpectedBridge;
        "operationId": Wire.Identifier;
        "reason": string;
    };
    type Result = {
        "schemaVersion": 1;
        "inputId": Wire.Identifier;
        "binding": Chat.ObjectPin;
        "turnId": Wire.Identifier;
        "nativeState": "completed" | "failed" | "interrupted";
        "evidence": Array<Chat.ObjectPin>;
        "textParts": Array<Chat.Artifact>;
        "createdAt": string;
    };
    type ReplyOrigin = ({
        "kind": "native";
        "inputId": Wire.Identifier;
        "binding": Chat.ObjectPin;
        "result": Chat.ObjectPin;
    }) | ({
        "kind": "notice";
        "operation": Chat.ObjectPin;
        "claim": Chat.ObjectPin;
        "producerPrincipalId": Wire.Identifier;
        "source": Chat.ObjectPin;
    });
    type Reply = ({
        "schemaVersion": 1;
        "workspaceId": Wire.Identifier;
        "definitionHash": Wire.Hash;
        "channel": Chat.Channel;
        "origin": Chat.ReplyOrigin;
        "sequence": number;
        "partIndex": number;
        "text": string;
        "artifacts": Array<Chat.Artifact>;
        "createdAt": string;
        "immutableHash": Wire.Hash;
        "state": "pending" | "outcome_unknown" | "confirmed";
        "firstOfferedAt": (string) | (null);
        "confirmedAt": (string) | (null);
    }) & (unknown) & (unknown) & (unknown);
    type Offer = {
        "schemaVersion": 1;
        "operationId": Wire.Identifier;
        "callerPrincipalId": Wire.Identifier;
        "channel": Chat.Channel;
        "afterSequence": number;
        "throughSequence": number;
        "replies": Array<Chat.ObjectPin>;
        "createdAt": string;
    };
    type Acknowledgement = {
        "schemaVersion": 1;
        "operationId": Wire.Identifier;
        "callerPrincipalId": Wire.Identifier;
        "offer": Chat.ObjectPin;
        "replyIds": Array<Wire.Identifier>;
        "createdAt": string;
    };
    type MainView = {
        "object": Chat.ObjectPin;
        "data": Chat.Main;
    };
    type BindingView = {
        "object": Chat.ObjectPin;
        "data": Chat.Binding;
    };
    type InputView = {
        "object": Chat.ObjectPin;
        "data": Chat.Input;
    };
    type ReplyView = {
        "object": Chat.ObjectPin;
        "data": Chat.Reply;
    };
    type OfferView = {
        "object": Chat.ObjectPin;
        "data": Chat.Offer;
    };
    type AcknowledgementView = {
        "object": Chat.ObjectPin;
        "data": Chat.Acknowledgement;
    };
    type ActionOutcome = ({
        "action": "createMain";
        "operationId": Wire.Identifier;
        "binding": Chat.BindingView;
    }) | ({
        "action": "send";
        "operationId": Wire.Identifier;
        "input": Chat.InputView;
    }) | ({
        "action": "cancel";
        "operationId": Wire.Identifier;
        "input": Chat.InputView;
    }) | ({
        "action": "receive";
        "operationId": Wire.Identifier;
        "offer": Chat.OfferView;
        "replies": Array<Chat.ReplyView>;
    }) | ({
        "action": "acknowledge";
        "operationId": Wire.Identifier;
        "acknowledgement": Chat.AcknowledgementView;
    }) | ({
        "action": "notify";
        "operationId": Wire.Identifier;
        "reply": Chat.ReplyView;
    });
    type Operation = ({
        "schemaVersion": 1;
        "workspaceId": Wire.Identifier;
        "definitionHash": Wire.Hash;
        "callerPrincipalId": Wire.Identifier;
        "operationId": Wire.Identifier;
        "request": Chat.ActionRequest;
        "requestHash": Wire.Hash;
        "phase": "accepted" | "applying" | "succeeded" | "failed" | "outcome_unknown";
        "nativeCall": (Chat.ObjectPin) | (null);
        "outcome": (Chat.ActionOutcome) | (null);
        "error": (Chat.Error) | (null);
        "createdAt": string;
        "updatedAt": string;
    }) & (unknown) & (unknown) & (unknown) & (unknown);
    type WorkspaceInfo = {
        "expectedBridge": Chat.ExpectedBridge;
        "project": Wire.ResourceRef;
        "main": (Chat.MainView) | (null);
        "binding": (Chat.BindingView) | (null);
        "channels": Array<Chat.ChannelConfiguration>;
        "nativeOwnerReady": boolean;
        "observedAt": string;
    };
    type HistoryPage = {
        "entries": Array<(Chat.InputView) | (Chat.ReplyView)>;
        "throughSequence": number;
        "hasMore": boolean;
    };
}
export declare namespace Secretary {
    type Json = Wire.Json;
    type Pin = {
        "objectId": string;
        "revision": number;
    };
    type Scope = {
        "secretaryId": string;
        "rootObjectId": string;
    };
    type Identity = {
        "serviceNodeId": string;
        "hostId": string;
        "principalId": string;
        "scope": Secretary.Scope;
    };
    type Settings = {
        "schemaVersion": 1;
        "identity": Secretary.Identity;
        "pollMs": number;
        "recordsPerTick": number;
    };
    type ContactWindow = {
        "timeZone": string;
        "start": string;
        "end": string;
    };
    type ContactRule = {
        "enabled": boolean;
        "minimumUrgency": "normal" | "high" | "critical";
        "window": (Secretary.ContactWindow) | (null);
    };
    type VoiceRule = {
        "enabled": boolean;
        "minimumUrgency": "critical";
        "window": (Secretary.ContactWindow) | (null);
        "immediateOnly": boolean;
    };
    type GlobalRules = {
        "main": Secretary.ContactRule;
        "voice": Secretary.VoiceRule;
        "researchMaxMinutes": number;
    };
    type RuleOverrides = {
        "main"?: {
            "enabled"?: boolean;
            "minimumUrgency"?: "normal" | "high" | "critical";
            "window"?: (Secretary.ContactWindow) | (null);
        };
        "voice"?: {
            "enabled"?: boolean;
            "minimumUrgency"?: "critical";
            "window"?: (Secretary.ContactWindow) | (null);
            "immediateOnly"?: boolean;
        };
        "researchMaxMinutes"?: number;
    };
    type ExecutionTarget = {
        "serviceNodeId": string;
        "threadCwd": string;
        "model": (string) | (null);
        "effort": "low" | "medium" | "high" | "xhigh";
        "permissions": string;
    };
    type SecretaryConfiguration = {
        "schemaVersion": 1;
        "rules": Secretary.GlobalRules;
        "execution": (Secretary.ExecutionTarget) | (null);
        "updatedAt": string;
    };
    type ScheduleTrigger = {
        "kind": "schedule";
        "timeZone": string;
        "cadence": "interval" | "daily" | "weekly";
        "intervalMinutes": (number) | (null);
        "localTime": (string) | (null);
        "weekdays": Array<number>;
    };
    type EventTrigger = {
        "kind": "event";
        "topic": string;
        "topicVersion": Wire.ContractVersion;
        "sourceServiceNodeId": (string) | (null);
        "eventKind": (string) | (null);
    };
    type AssignmentTrigger = (Secretary.ScheduleTrigger) | (Secretary.EventTrigger);
    type Assignment = {
        "schemaVersion": 1;
        "assignmentId": string;
        "name": string;
        "description": string;
        "enabled": boolean;
        "trigger": Secretary.AssignmentTrigger;
        "prompt": string;
        "rules": Secretary.RuleOverrides;
        "createdAt": string;
        "updatedAt": string;
    };
    type ExecutionTrigger = {
        "kind": "schedule" | "event";
        "key": string;
        "occurredAt": string;
        "payload": Secretary.Json;
    };
    type NativeTarget = {
        "serviceNodeId": string;
        "hostId": string;
        "nativeVersion": string;
        "nativeExecutableHash": Wire.Hash;
        "catalogHash": Wire.Hash;
    };
    type NativeOperations = {
        "threadStart": (string) | (null);
        "turnStart": (string) | (null);
        "archive": (string) | (null);
        "delete": (string) | (null);
    };
    type Execution = {
        "schemaVersion": 1;
        "executionId": Wire.Hash;
        "assignment": Secretary.Pin;
        "assignmentSnapshot": Secretary.Assignment;
        "trigger": Secretary.ExecutionTrigger;
        "effectiveRules": Secretary.GlobalRules;
        "executionTarget": Secretary.ExecutionTarget;
        "phase": "queued" | "starting" | "running" | "archiving" | "archived" | "deleting" | "deleted" | "failed";
        "serviceNodeId": string;
        "nativeTarget": (Secretary.NativeTarget) | (null);
        "threadId": (string) | (null);
        "turnId": (string) | (null);
        "nativeOperations": Secretary.NativeOperations;
        "result": (string) | (null);
        "errorCode": (string) | (null);
        "createdAt": string;
        "startedAt": (string) | (null);
        "completedAt": (string) | (null);
        "archivedAt": (string) | (null);
        "deleteAfter": (string) | (null);
        "deletedAt": (string) | (null);
        "updatedAt": string;
    };
    type Workspace = {
        "scope": Secretary.Scope;
        "configuration": (Secretary.Pin) | (null);
        "observedAt": string;
    };
    type WorkspaceRequest = {};
    type SaveConfigurationRequest = {
        "operationId": string;
        "configuration": Secretary.Pin;
        "value": Secretary.SecretaryConfiguration;
    };
    type SaveAssignmentRequest = {
        "operationId": string;
        "assignment": (Secretary.Pin) | (null);
        "value": Secretary.Assignment;
    };
}
export interface OperationMap {
    "system.status": {
        params: Operation.SystemStatusParams;
        result: Operation.SystemStatusResult;
    };
    "system.inspectStorage": {
        params: Operation.SystemInspectStorageParams;
        result: Operation.SystemInspectStorageResult;
    };
    "system.diagnostics": {
        params: Operation.SystemDiagnosticsParams;
        result: Operation.SystemDiagnosticsResult;
    };
    "contracts.list": {
        params: Operation.ContractsListParams;
        result: Operation.ContractsListResult;
    };
    "contracts.get": {
        params: Operation.ContractsGetParams;
        result: Operation.ContractsGetResult;
    };
    "contracts.register": {
        params: Operation.ContractsRegisterParams;
        result: Operation.ContractsRegisterResult;
    };
    "contracts.validate": {
        params: Operation.ContractsValidateParams;
        result: Operation.ContractsValidateResult;
    };
    "objects.stat": {
        params: Operation.ObjectsStatParams;
        result: Operation.ObjectsStatResult;
    };
    "objects.read": {
        params: Operation.ObjectsReadParams;
        result: Operation.ObjectsReadResult;
    };
    "objects.write": {
        params: Operation.ObjectsWriteParams;
        result: Operation.ObjectsWriteResult;
    };
    "objects.history": {
        params: Operation.ObjectsHistoryParams;
        result: Operation.ObjectsHistoryResult;
    };
    "objects.list": {
        params: Operation.ObjectsListParams;
        result: Operation.ObjectsListResult;
    };
    "objects.tree": {
        params: Operation.ObjectsTreeParams;
        result: Operation.ObjectsTreeResult;
    };
    "objects.query": {
        params: Operation.ObjectsQueryParams;
        result: Operation.ObjectsQueryResult;
    };
    "objects.search": {
        params: Operation.ObjectsSearchParams;
        result: Operation.ObjectsSearchResult;
    };
    "retention.preview": {
        params: Operation.RetentionPreviewParams;
        result: Operation.RetentionPreviewResult;
    };
    "retention.status": {
        params: Operation.RetentionStatusParams;
        result: Operation.RetentionStatusResult;
    };
    "objects.move": {
        params: Operation.ObjectsMoveParams;
        result: Operation.ObjectsMoveResult;
    };
    "objects.reorder": {
        params: Operation.ObjectsReorderParams;
        result: Operation.ObjectsReorderResult;
    };
    "objects.archive": {
        params: Operation.ObjectsArchiveParams;
        result: Operation.ObjectsArchiveResult;
    };
    "objects.delete": {
        params: Operation.ObjectsDeleteParams;
        result: Operation.ObjectsDeleteResult;
    };
    "service.connect": {
        params: Operation.ServiceConnectParams;
        result: Operation.ServiceConnectResult;
    };
    "registry.sync": {
        params: Operation.RegistrySyncParams;
        result: Operation.RegistrySyncResult;
    };
    "service.heartbeat": {
        params: Operation.ServiceHeartbeatParams;
        result: Operation.ServiceHeartbeatResult;
    };
    "hosts.list": {
        params: Operation.HostsListParams;
        result: Operation.HostsListResult;
    };
    "hosts.report": {
        params: Operation.HostsReportParams;
        result: Operation.HostsReportResult;
    };
    "hosts.observations": {
        params: Operation.HostsObservationsParams;
        result: Operation.HostsObservationsResult;
    };
    "hostConfigurations.list": {
        params: Operation.HostConfigurationsListParams;
        result: Operation.HostConfigurationsListResult;
    };
    "hostConfigurations.get": {
        params: Operation.HostConfigurationsGetParams;
        result: Operation.HostConfigurationsGetResult;
    };
    "hostConfigurations.put": {
        params: Operation.HostConfigurationsPutParams;
        result: Operation.HostConfigurationsPutResult;
    };
    "hostConfigurations.edit": {
        params: Operation.HostConfigurationsEditParams;
        result: Operation.HostConfigurationsEditResult;
    };
    "hostConfigurations.save": {
        params: Operation.HostConfigurationsSaveParams;
        result: Operation.HostConfigurationsSaveResult;
    };
    "hostConfigurations.history": {
        params: Operation.HostConfigurationsHistoryParams;
        result: Operation.HostConfigurationsHistoryResult;
    };
    "serviceNodes.list": {
        params: Operation.ServiceNodesListParams;
        result: Operation.ServiceNodesListResult;
    };
    "serviceNodes.get": {
        params: Operation.ServiceNodesGetParams;
        result: Operation.ServiceNodesGetResult;
    };
    "serviceNodes.contracts": {
        params: Operation.ServiceNodesContractsParams;
        result: Operation.ServiceNodesContractsResult;
    };
    "namespaces.list": {
        params: Operation.NamespacesListParams;
        result: Operation.NamespacesListResult;
    };
    "namespaces.get": {
        params: Operation.NamespacesGetParams;
        result: Operation.NamespacesGetResult;
    };
    "tools.list": {
        params: Operation.ToolsListParams;
        result: Operation.ToolsListResult;
    };
    "topics.list": {
        params: Operation.TopicsListParams;
        result: Operation.TopicsListResult;
    };
    "tools.call": {
        params: Operation.ToolsCallParams;
        result: Operation.ToolsCallResult;
    };
    "discovery.list": {
        params: Operation.DiscoveryListParams;
        result: Operation.DiscoveryListResult;
    };
    "discovery.instructions": {
        params: Operation.DiscoveryInstructionsParams;
        result: Operation.DiscoveryInstructionsResult;
    };
    "discovery.describe": {
        params: Operation.DiscoveryDescribeParams;
        result: Operation.DiscoveryDescribeResult;
    };
    "discovery.call": {
        params: Operation.DiscoveryCallParams;
        result: Operation.DiscoveryCallResult;
    };
    "inventory.list": {
        params: Operation.InventoryListParams;
        result: Operation.InventoryListResult;
    };
    "inventory.get": {
        params: Operation.InventoryGetParams;
        result: Operation.InventoryGetResult;
    };
    "inventory.sync": {
        params: Operation.InventorySyncParams;
        result: Operation.InventorySyncResult;
    };
    "events.publish": {
        params: Operation.EventsPublishParams;
        result: Operation.EventsPublishResult;
    };
    "events.read": {
        params: Operation.EventsReadParams;
        result: Operation.EventsReadResult;
    };
    "events.head": {
        params: Operation.EventsHeadParams;
        result: Operation.EventsHeadResult;
    };
    "events.subscribe": {
        params: Operation.EventsSubscribeParams;
        result: Operation.EventsSubscribeResult;
    };
    "events.ack": {
        params: Operation.EventsAckParams;
        result: Operation.EventsAckResult;
    };
    "events.unsubscribe": {
        params: Operation.EventsUnsubscribeParams;
        result: Operation.EventsUnsubscribeResult;
    };
    "notifications.subscribe": {
        params: Operation.NotificationsSubscribeParams;
        result: Operation.NotificationsSubscribeResult;
    };
    "packages.catalog": {
        params: Operation.PackagesCatalogParams;
        result: Operation.PackagesCatalogResult;
    };
    "packages.authorizeUpload": {
        params: Operation.PackagesAuthorizeUploadParams;
        result: Operation.PackagesAuthorizeUploadResult;
    };
    "uis.list": {
        params: Operation.UisListParams;
        result: Operation.UisListResult;
    };
    "uis.get": {
        params: Operation.UisGetParams;
        result: Operation.UisGetResult;
    };
    "uis.catalog": {
        params: Operation.UisCatalogParams;
        result: Operation.UisCatalogResult;
    };
    "uis.releases": {
        params: Operation.UisReleasesParams;
        result: Operation.UisReleasesResult;
    };
    "uis.inspect": {
        params: Operation.UisInspectParams;
        result: Operation.UisInspectResult;
    };
    "uis.deploy": {
        params: Operation.UisDeployParams;
        result: Operation.UisDeployResult;
    };
    "uis.rollback": {
        params: Operation.UisRollbackParams;
        result: Operation.UisRollbackResult;
    };
    "deployments.list": {
        params: Operation.DeploymentsListParams;
        result: Operation.DeploymentsListResult;
    };
    "deployments.get": {
        params: Operation.DeploymentsGetParams;
        result: Operation.DeploymentsGetResult;
    };
    "deployments.report": {
        params: Operation.DeploymentsReportParams;
        result: Operation.DeploymentsReportResult;
    };
    "wiki.search": {
        params: Operation.WikiSearchParams;
        result: Operation.WikiSearchResult;
    };
    "wiki.list": {
        params: Operation.WikiListParams;
        result: Operation.WikiListResult;
    };
    "wiki.read": {
        params: Operation.WikiReadParams;
        result: Operation.WikiReadResult;
    };
    "wiki.create": {
        params: Operation.WikiCreateParams;
        result: Operation.WikiCreateResult;
    };
    "wiki.update": {
        params: Operation.WikiUpdateParams;
        result: Operation.WikiUpdateResult;
    };
    "wiki.history": {
        params: Operation.WikiHistoryParams;
        result: Operation.WikiHistoryResult;
    };
    "wiki.move": {
        params: Operation.WikiMoveParams;
        result: Operation.WikiMoveResult;
    };
    "wiki.archive": {
        params: Operation.WikiArchiveParams;
        result: Operation.WikiArchiveResult;
    };
}
export type OperationName = keyof OperationMap;
export type Params<M extends OperationName> = OperationMap[M]["params"];
export type Result<M extends OperationName> = OperationMap[M]["result"];
