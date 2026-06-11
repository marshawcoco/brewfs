import type { AclEntry, AclUpdateRequest } from './api';
import type { AclEntryRow } from './aclView';

export function formatAclDraft(rows: AclEntryRow[]): string {
  return JSON.stringify(
    rows.map((row) => {
      const id = Number(row.id);
      if (row.id !== '-' && Number.isInteger(id)) {
        return {
          scope: row.scope,
          tag: row.tag,
          id,
          perm: row.perm,
        };
      }
      return {
        scope: row.scope,
        tag: row.tag,
        perm: row.perm,
      };
    }),
    null,
    2,
  );
}

export function parseAclDraft(value: string): AclUpdateRequest {
  let parsed: unknown;
  try {
    parsed = JSON.parse(value);
  } catch (err: unknown) {
    throw new Error(err instanceof Error ? err.message : 'ACL draft is not valid JSON.');
  }

  if (!Array.isArray(parsed)) {
    throw new Error('ACL draft must be a JSON array.');
  }

  return {
    entries: parsed.map(parseEntry),
  };
}

function parseEntry(value: unknown, index: number): AclEntry {
  if (!isRecord(value)) {
    throw new Error(`ACL entry ${index + 1} must be an object.`);
  }

  const scope = requiredString(value, 'scope', index);
  const tag = requiredString(value, 'tag', index);
  const perm = requiredString(value, 'perm', index);
  const id = value.id;

  if (id === undefined || id === null) {
    return { scope, tag, perm };
  }
  if (typeof id !== 'number' || !Number.isInteger(id)) {
    throw new Error(`ACL entry ${index + 1} id must be a number.`);
  }

  return { scope, tag, id, perm };
}

function requiredString(record: Record<string, unknown>, key: keyof AclEntry, index: number): string {
  const value = record[key];
  if (typeof value !== 'string' || value.length === 0) {
    throw new Error(`ACL entry ${index + 1} is missing ${key}.`);
  }
  return value;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
