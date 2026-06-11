import { describe, expect, it } from 'vitest';
import { formatAclDraft, parseAclDraft } from './aclDraft';
import type { AclEntryRow } from './aclView';

const rows: AclEntryRow[] = [
  { scope: 'access', tag: 'user_obj', id: '-', perm: 'rwx' },
  { scope: 'access', tag: 'user', id: '1001', perm: 'rw-' },
];

describe('aclDraft', () => {
  it('formats ACL rows as editable JSON entries', () => {
    expect(formatAclDraft(rows)).toBe(
      JSON.stringify(
        [
          { scope: 'access', tag: 'user_obj', perm: 'rwx' },
          { scope: 'access', tag: 'user', id: 1001, perm: 'rw-' },
        ],
        null,
        2,
      ),
    );
  });

  it('parses ACL entry arrays into update requests', () => {
    expect(parseAclDraft('[{"scope":"access","tag":"group","id":1002,"perm":"r-x"}]')).toEqual({
      entries: [{ scope: 'access', tag: 'group', id: 1002, perm: 'r-x' }],
    });
  });

  it('rejects malformed ACL drafts', () => {
    expect(() => parseAclDraft('{"entries":[]}')).toThrow('ACL draft must be a JSON array.');
    expect(() => parseAclDraft('[{"scope":"access","tag":"user_obj"}]')).toThrow(
      'ACL entry 1 is missing perm.',
    );
    expect(() => parseAclDraft('[{"scope":"access","tag":"user","id":"1001","perm":"rwx"}]')).toThrow(
      'ACL entry 1 id must be a number.',
    );
  });
});
