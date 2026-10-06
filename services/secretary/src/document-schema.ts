import { SchemaValidators } from '../../../packages/sdk/src/node.js';
import schema from '../../../specs/schemas/secretary-document.schema.json' with { type: 'json' };

export interface DocumentText {
  schemaVersion: 1; format: 'pdf' | 'docx' | 'html'; representation: 'document_text';
  sections: { label: string; text: string }[];
  limitations: ('visual_layout_not_interpreted' | 'embedded_media_not_interpreted' | 'external_resources_not_loaded'
    | 'pdf_annotations_and_forms_not_interpreted' | 'empty_pdf_pages')[];
}
const validators = new SchemaValidators();
export function validateDocument(value: unknown): asserts value is DocumentText {
  validators.validate({ ...schema, $ref: '#/$defs/DocumentText' }, value);
}
export const documentFormats = new Map<string, DocumentText['format']>([
  ['application/pdf', 'pdf'], ['application/vnd.openxmlformats-officedocument.wordprocessingml.document', 'docx'], ['text/html', 'html'],
]);
