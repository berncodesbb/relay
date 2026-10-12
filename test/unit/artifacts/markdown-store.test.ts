/**
 * Versioned Markdown artifact storage.
 *
 * Covers save/version/list/latest/inspect/read, atomic writes, overwrite
 * rejection, frontmatter round-tripping of hostile titles, path containment,
 * and visibility to a fresh store instance (restart) on the same directory.
 */

import { mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import {
  ArtifactMarkdownStore,
  ArtifactPathOutsideStoreError,
  ArtifactVersionExistsError,
  InvalidArtifactFileError,
  InvalidArtifactOptionsError,
  InvalidArtifactsDirError,
} from '../../../src/artifacts/markdown-store.js';
import { renderArtifactMarkdown } from '../../../src/artifacts/render.js';
import { InvalidArtifactDraftError } from '../../../src/artifacts/schemas.js';
import {
  clone,
  sampleDecisionLog,
  sampleDraft,
  sampleOpportunityBrief,
  samplePrd,
} from './samples.test.js';

let root: string;
let artifactsDir: string;
let store: ArtifactMarkdownStore;

beforeEach(async () => {
  root = await mkdtemp(path.join(tmpdir(), 'relay-artifacts-'));
  artifactsDir = path.join(root, '.relay', 'artifacts');
  store = new ArtifactMarkdownStore(artifactsDir);
});

afterEach(async () => {
  await rm(root, { recursive: true, force: true });
});

async function files(kind: string): Promise<readonly string[]> {
  return readdir(path.join(artifactsDir, kind)).catch(() => [] as string[]);
}

describe('ArtifactMarkdownStore', () => {
  it('requires an absolute artifacts directory', () => {
    expect(() => new ArtifactMarkdownStore('relative/artifacts')).toThrowError(InvalidArtifactsDirError);
  });

  it('saves version 1 with frontmatter, rendered body, and workspace-relative path', async () => {
    const draft = samplePrd();
    const stored = await store.save('prd', draft);

    expect(stored.version).toBe(1);
    expect(stored.kind).toBe('prd');
    expect(stored.title).toBe('Release gate PRD');
    expect(stored.path).toBe(`.relay/artifacts/prd/${path.basename(stored.path)}`);
    expect(stored.artifactId).toMatch(/^art_/);
    expect(stored.path).toMatch(/\.md$/);

    const read = await store.read(stored.path);
    expect(read.frontmatter.id).toBe(stored.artifactId);
    expect(read.frontmatter.kind).toBe('prd');
    expect(read.frontmatter.version).toBe(1);
    expect(read.frontmatter.title).toBe('Release gate PRD');
    expect(read.frontmatter.status).toBe('draft');
    expect(String(read.frontmatter.createdAt)).toMatch(/^\d{4}-\d{2}-\d{2}T/);
    expect(read.body).toBe(renderArtifactMarkdown('prd', draft));
  });

  it('writes files as <kind>/v<n>-<slug>.md with a YAML frontmatter block', async () => {
    const stored = await store.save('opportunity-brief', sampleOpportunityBrief());
    const absolute = path.join(root, stored.path);
    const content = await readFile(absolute, 'utf8');
    expect(path.basename(absolute)).toBe('v1-faster-release-feedback-loop.md');
    expect(content.startsWith('---\n')).toBe(true);
    expect(content).toContain('\n---\n\n# Faster release feedback loop');
  });

  it('saves v2 as a distinct file and reports it as latest', async () => {
    const v1 = await store.save('prd', samplePrd());
    const revised = clone(samplePrd());
    revised.summary = 'Tighter summary after review.';
    const v2 = await store.save('prd', revised, { artifactId: v1.artifactId, previousVersion: 1 });

    expect(v2.artifactId).toBe(v1.artifactId);
    expect(v2.version).toBe(2);
    expect(v2.path).not.toBe(v1.path);

    const listed = await store.list('prd');
    expect(listed.map((item) => item.version)).toEqual([1, 2]);
    const latest = await store.latest('prd');
    expect(latest?.version).toBe(2);
    expect(latest?.path).toBe(v2.path);

    const body = await store.read(v2.path);
    expect(body.body).toContain('Tighter summary after review.');
  });

  it('sorts versions numerically, not lexicographically', async () => {
    let stored = await store.save('decision-log', sampleDecisionLog());
    for (let version = 2; version <= 11; version += 1) {
      const draft = clone(sampleDecisionLog());
      draft.title = `Decision log revision ${version}`;
      stored = await store.save('decision-log', draft, {
        artifactId: stored.artifactId,
        previousVersion: stored.version,
      });
    }
    const listed = await store.list('decision-log');
    expect(listed.map((item) => item.version)).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
    expect(await store.latest('decision-log')).toEqual(listed.at(-1));
  });

  it('never overwrites an existing version file', async () => {
    const first = await store.save('prd', samplePrd());
    const again = clone(samplePrd());
    await expect(store.save('prd', again)).rejects.toThrowError(ArtifactVersionExistsError);

    // A retitled revision may create its own v2 file, but repeating that
    // version for the same artifact is refused even though the filename
    // differs.
    const v1 = await store.save('trd', sampleDraft('trd'));
    const retitled = clone(sampleDraft('trd')) as { title: string };
    retitled.title = 'A different TRD title';
    const v2 = await store.save('trd', retitled, { artifactId: v1.artifactId, previousVersion: 1 });
    expect(v2.version).toBe(2);
    expect(path.basename(v2.path)).toBe('v2-a-different-trd-title.md');
    await expect(
      store.save('trd', clone(sampleDraft('trd')), { artifactId: v1.artifactId, previousVersion: 1 }),
    ).rejects.toThrowError(ArtifactVersionExistsError);

    // And the original file is untouched.
    const body = await store.read(first.path);
    expect(body.frontmatter.title).toBe('Release gate PRD');
  });

  it('leaves no .tmp files behind after successful saves', async () => {
    const v1 = await store.save('prd', samplePrd());
    await store.save('prd', clone(samplePrd()), { artifactId: v1.artifactId, previousVersion: 1 });
    const names = await readdir(path.join(artifactsDir, 'prd'));
    expect(names.filter((name) => name.includes('.tmp'))).toEqual([]);
  });

  it('round-trips hostile titles through YAML frontmatter', async () => {
    const draft = clone(sampleOpportunityBrief());
    draft.title = 'Ship it: "now" --- really? \\ yes';
    const stored = await store.save('opportunity-brief', draft);
    const read = await store.read(stored.path);
    expect(read.frontmatter.title).toBe(draft.title);

    const delimiterOnly = clone(samplePrd());
    delimiterOnly.title = '---';
    const dashStored = await store.save('prd', delimiterOnly);
    const dashRead = await store.read(dashStored.path);
    expect(dashRead.frontmatter.title).toBe('---');
    expect(dashRead.body.startsWith('# ---')).toBe(true);
  });

  it('round-trips scalar-lookalike titles as strings', async () => {
    for (const title of ['true', '123', 'null', '# hash']) {
      const draft = clone(samplePrd());
      draft.title = title;
      const stored = await store.save('prd', draft);
      const read = await store.read(stored.path);
      expect(read.frontmatter.title, `title ${title}`).toBe(title);
      expect(typeof read.frontmatter.title).toBe('string');
    }
  });

  it('slugs unpronounceable titles to a safe fallback', async () => {
    const draft = clone(samplePrd());
    draft.title = '!!!';
    const stored = await store.save('prd', draft);
    expect(path.basename(stored.path)).toBe('v1-untitled.md');
  });

  it('inspects by artifact id and by path, and returns null for unknown ids', async () => {
    const v1 = await store.save('prd', samplePrd());
    const revised = clone(samplePrd());
    revised.summary = 'Second version.';
    const v2 = await store.save('prd', revised, { artifactId: v1.artifactId, previousVersion: 1 });

    const byId = await store.inspect(v1.artifactId);
    expect(byId?.frontmatter.version).toBe(2);
    expect(byId?.path).toBe(v2.path);
    expect(byId?.body).toContain('Second version.');

    const byPath = await store.inspect(v1.path);
    expect(byPath?.frontmatter.version).toBe(1);

    const byBasename = await store.inspect(path.basename(v1.path));
    expect(byBasename).toBeNull();

    expect(await store.inspect('art_does-not-exist')).toBeNull();
  });

  it('accepts store-relative paths on read', async () => {
    const stored = await store.save('decision-log', sampleDecisionLog());
    const storeRelative = stored.path.replace(/^\.relay\/artifacts\//, '');
    const read = await store.read(storeRelative);
    expect(read.frontmatter.kind).toBe('decision-log');
  });

  it('rejects paths that escape the artifacts directory', async () => {
    await expect(store.read('../../../../etc/hosts')).rejects.toThrowError(ArtifactPathOutsideStoreError);
    await expect(store.read('/etc/hosts')).rejects.toThrowError(ArtifactPathOutsideStoreError);
  });

  it('rejects invalid drafts and bad save options', async () => {
    const bad = clone(samplePrd()) as Record<string, unknown>;
    delete bad['requirements'];
    await expect(store.save('prd', bad)).rejects.toThrowError(InvalidArtifactDraftError);

    const v1 = await store.save('prd', samplePrd());
    await expect(
      store.save('prd', samplePrd(), { artifactId: v1.artifactId, previousVersion: 0 }),
    ).rejects.toThrowError(InvalidArtifactOptionsError);
    await expect(
      store.save('prd', samplePrd(), { artifactId: '../evil' }),
    ).rejects.toThrowError(InvalidArtifactOptionsError);
  });

  it('reports an empty list for kinds with no files', async () => {
    expect(await store.list('trd')).toEqual([]);
    expect(await store.latest('delivery-report')).toBeNull();
  });

  it('flags corrupt artifact files with actionable diagnostics', async () => {
    await store.save('prd', samplePrd());
    const listed = await store.list('prd');
    const absolute = path.join(root, listed[0]!.path);
    await writeFile(absolute, 'no frontmatter here', 'utf8');
    await expect(store.read(listed[0]!.path)).rejects.toThrowError(InvalidArtifactFileError);
  });

  it('sees saved artifacts after a restart with a new store instance', async () => {
    const v1 = await store.save('prd', samplePrd());
    const revised = clone(samplePrd());
    revised.summary = 'Post-restart revision.';
    await store.save('prd', revised, { artifactId: v1.artifactId, previousVersion: 1 });

    const restarted = new ArtifactMarkdownStore(artifactsDir);
    const latest = await restarted.latest('prd');
    expect(latest?.version).toBe(2);
    const read = await restarted.read(latest?.path ?? '');
    expect(read.frontmatter.id).toBe(v1.artifactId);
    expect(read.body).toContain('Post-restart revision.');
    const inspected = await restarted.inspect(v1.artifactId);
    expect(inspected?.frontmatter.version).toBe(2);
  });

  it('keeps separate artifacts of the same kind distinct', async () => {
    const a = clone(samplePrd());
    a.title = 'First PRD';
    const b = clone(samplePrd());
    b.title = 'Second PRD';
    const storedA = await store.save('prd', a);
    const storedB = await store.save('prd', b);
    expect(storedA.artifactId).not.toBe(storedB.artifactId);
    expect(await files('prd')).toHaveLength(2);
    const listed = await store.list('prd');
    expect(listed.map((item) => item.artifactId).sort()).toEqual(
      [storedA.artifactId, storedB.artifactId].sort(),
    );
  });
});
