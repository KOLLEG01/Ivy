export interface FieldChange {
  field: string;
  before: string;
  after: string;
  /** Long text shows a line comparison on demand instead of both values inline. */
  long?: boolean;
}
export interface ChangeEntry {
  id: string;
  actor: string;
  at: string;
  summary?: string;
  changes: FieldChange[];
}
