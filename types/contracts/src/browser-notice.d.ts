/** Shared envelope for a service-owned *.browser-notification event. */
export interface BrowserNotice {
    title: string;
    body: string;
    tag: string;
    target: {
        uiId: string;
        fragment: string;
    };
}
export declare const browserNoticeSchema: {
    type: string;
    additionalProperties: boolean;
    properties: {
        title: {
            type: string;
            minLength: number;
            maxLength: number;
        };
        body: {
            type: string;
            maxLength: number;
        };
        tag: {
            type: string;
            minLength: number;
            maxLength: number;
        };
        target: {
            type: string;
            additionalProperties: boolean;
            properties: {
                uiId: {
                    type: string;
                    pattern: string;
                };
                fragment: {
                    type: string;
                    pattern: string;
                    maxLength: number;
                };
            };
            required: string[];
        };
    };
    required: string[];
};
export declare function browserNoticeTopic(namespace: string): {
    topic: string;
    version: string;
    title: string;
    description: string;
    payloadSchema: {
        type: string;
        additionalProperties: boolean;
        properties: {
            title: {
                type: string;
                minLength: number;
                maxLength: number;
            };
            body: {
                type: string;
                maxLength: number;
            };
            tag: {
                type: string;
                minLength: number;
                maxLength: number;
            };
            target: {
                type: string;
                additionalProperties: boolean;
                properties: {
                    uiId: {
                        type: string;
                        pattern: string;
                    };
                    fragment: {
                        type: string;
                        pattern: string;
                        maxLength: number;
                    };
                };
                required: string[];
            };
        };
        required: string[];
    };
    eventKinds: never[];
};
