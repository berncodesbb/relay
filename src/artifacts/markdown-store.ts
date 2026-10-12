/**
 * Versioned Markdown artifact storage under `.relay/artifacts/`.
 *
 * Each artifact version is an immutable file `<kind>/v<n>-<slug>.md` with YAML
 * frontmatter (id, kind, version, title, status, timestamps) followed by the
 * rendered body. Writes are atomic (temp file + rename), existing version
 * files are never overwritten, and all reads go through the filesystem so a
 * fresh store instance on the same directory sees everything a previous one
 * wrote.
 *
 * The store is constructed with the absolute artifacts directory. Paths it
 * returns and accepts are workspace-relative (`.relay/artifacts/...`) for the
 * standard `<workspace>/.relay/artifacts` layout; paths relative to the
 * artifacts directory itself are also accepted on input as a fallback.
 */

import { randomUUID } from 'node:crypto';
import { mkdir, readFile, readdir, rename, stat, unlink, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { parse as parseYaml, stringify as stringifyYaml } from 'yaml';
import { ARTIFACT_KINDS, type ArtifactKind, isArtifactKind } from '../core/artifacts.js';
import { newId, nowIso } from '../core/ids.js';
import { renderArtifactMarkdown } from './render.js';
import {
  InvalidArtifactDraftError,
  parseArtifactDraft,
  UnknownArtifactKindError,
} from './schemas.js';

export type StoredArtifact = {
  readonly artifactId: string;
  readonly kind: ArtifactKind;
  readonly title: string;
  readonly version: number;
  /** Path relative to the workspace root. */
  readonly path: string;
};

export type SaveArtifactOptions = {
  /** Existing artifact id when revising; a fresh `art_...` id otherwise. */
  readonly artifactId?: string;
  /** Version being replaced; the saved version is `previousVersion + 1`. */
  readonly previousVersion?: number;
};

export type ArtifactFile = {
  readonly frontmatter: Record<string, unknown>;
  readonly body: string;
  readonly path: string;
};

export class InvalidArtifactsDirError extends Error {
  constructor(
    readonly artifactsDir: string,
    readonly detail: string,
  ) {
    super(`Invalid artifacts directory "${artifactsDir}": ${detail}`);
    this.name = 'InvalidArtifactsDirError';
  }
}

export class InvalidArtifactOptionsError extends Error {
  constructor(readonly detail: string) {
    super(`Invalid artifact save options: ${detail}`);
    this.name = 'InvalidArtifactOptionsError';
  }
}

/** Raised when the computed version file already exists; it is never overwritten. */
export class ArtifactVersionExistsError extends Error {
  constructor(readonly path: string) {
    super(`Artifact version file already exists and will not be overwritten: ${path}`);
    this.name = 'ArtifactVersionExistsError';
  }
}

export class ArtifactPathOutsideStoreError extends Error {
  constructor(readonly path: string) {
    super(`Artifact path "${path}" resolves outside the artifacts directory`);
    this.name = 'ArtifactPathOutsideStoreError';
  }
}

export class InvalidArtifactFileError extends Error {
  constructor(
    readonly path: string,
    readonly detail: string,
  ) {
    super(`Invalid artifact file "${path}": ${detail}`);
    this.name = 'InvalidArtifactFileError';
  }
}

/** Only files we wrote are considered; they always carry a version prefix. */
const VERSION_FILE_PATTERN = /^v(\d+)-.*\.md$/;

/** Kebab-case slug for filenames: lowercase, dashed, no leading/trailing dashes. */
function slugify(title: string): string {
  const slug = title
    .normalize('NFKD')
    .replace(/\p{M}/gu, '')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 60)
    .replace(/-+$/g, '');
  return slug.length > 0 ? slug : 'untitled';
}

export class ArtifactMarkdownStore {
  private readonly artifactsDir: string;
  /** Assumed workspace root for the standard `<root>/.relay/artifacts` layout. */
  private readonly workspaceRoot: string;

  constructor(artifactsDir: string) {
    if (typeof artifactsDir !== 'string' || artifactsDir.length === 0 || !path.isAbsolute(artifactsDir)) {
      throw new InvalidArtifactsDirError(String(artifactsDir), 'an absolute directory path is required');
    }
    this.artifactsDir = path.resolve(artifactsDir);
    this.workspaceRoot = path.resolve(this.artifactsDir, '..', '..');
  }

  /**
   * Validate and persist a draft as a new version. Returns the stored
   * descriptor; the file layout is `<artifactsDir>/<kind>/v<n>-<slug>.md`.
   */
  async save(
    kind: ArtifactKind,
    draft: unknown,
    options?: SaveArtifactOptions,
  ): Promise<StoredArtifact> {
    if (!isArtifactKind(kind)) {
      throw new UnknownArtifactKindError(
        String(kind),
        'expected one of: ' + ARTIFACT_KINDS.join(', '),
      );
    }
    const parsed = parseArtifactDraft(kind, draft);
    if (!parsed.ok) {
      throw new InvalidArtifactDraftError(kind, [...parsed.issues]);
    }
    const artifactId = options?.artifactId ?? newId('art');
    if (
      typeof artifactId !== 'string' ||
      artifactId.length === 0 ||
      artifactId.length > 200 ||
      /[\\/]|\.\./.test(artifactId)
    ) {
      throw new InvalidArtifactOptionsError(
        `artifactId must be 1..200 characters without path separators or ".." (got "${artifactId}")`,
      );
    }
    const previousVersion = options?.previousVersion;
    if (previousVersion !== undefined && (!Number.isInteger(previousVersion) || previousVersion < 1)) {
      throw new InvalidArtifactOptionsError(
        `previousVersion must be a positive integer (got ${String(previousVersion)})`,
      );
    }
    const version = previousVersion === undefined ? 1 : previousVersion + 1;

    const kindDir = path.join(this.artifactsDir, kind);
    await mkdir(kindDir, { recursive: true });
    const file = path.join(kindDir, `v${version}-${slugify(parsed.draft.title)}.md`);

    // Version uniqueness is per artifact, not per filename: a retitle must not
    // smuggle in a second copy of an existing version.
    const conflictingVersion = await this.findVersionConflict(kind, artifactId, version);
    if (conflictingVersion !== null) {
      throw new ArtifactVersionExistsError(conflictingVersion);
    }
    if (await this.exists(file)) {
      throw new ArtifactVersionExistsError(this.toStoredPath(file));
    }

    const timestamp = nowIso();
    const frontmatter = {
      id: artifactId,
      kind,
      version,
      title: parsed.draft.title,
      status: 'draft',
      createdAt: timestamp,
      updatedAt: timestamp,
    };
    const body = renderArtifactMarkdown(kind, parsed.draft);
    const content = `---\n${stringifyYaml(frontmatter).trimEnd()}\n---\n\n${body}`;

    const tmp = `${file}.tmp-${randomUUID().slice(0, 8)}`;
    try {
      await writeFile(tmp, content, 'utf8');
      // Re-check immediately before the rename so concurrent savers of the
      // same version cannot silently replace each other's file.
      if (await this.exists(file)) {
        throw new ArtifactVersionExistsError(this.toStoredPath(file));
      }
      await rename(tmp, file);
    } catch (error) {
      await unlink(tmp).catch(() => undefined);
      throw error;
    }
    return {
      artifactId,
      kind,
      title: parsed.draft.title,
      version,
      path: this.toStoredPath(file),
    };
  }

  /** Read a stored artifact file by workspace-relative (or store-relative) path. */
  async read(relativePath: string): Promise<{ frontmatter: Record<string, unknown>; body: string }> {
    const absolute = this.resolveArtifactPath(relativePath);
    if (absolute === null) {
      throw new ArtifactPathOutsideStoreError(relativePath);
    }
    const file = await this.readAt(absolute);
    return { frontmatter: file.frontmatter, body: file.body };
  }

  /** All stored versions of a kind, sorted by version (then date, then path). */
  async list(kind: ArtifactKind): Promise<readonly StoredArtifact[]> {
    if (!isArtifactKind(kind)) {
      throw new UnknownArtifactKindError(String(kind), 'expected one of: ' + ARTIFACT_KINDS.join(', '));
    }
    const kindDir = path.join(this.artifactsDir, kind);
    let entries: readonly string[];
    try {
      entries = await readdir(kindDir);
    } catch {
      return [];
    }
    const stored: StoredArtifact[] = [];
    for (const entry of [...entries].sort()) {
      if (!VERSION_FILE_PATTERN.test(entry)) continue;
      const file = await this.readAt(path.join(kindDir, entry));
      stored.push(this.toStoredArtifact(kind, file.frontmatter, file.path));
    }
    return stored.sort(
      (a, b) => a.version - b.version || stringCompare(a.path, b.path),
    );
  }

  /** Highest-version stored artifact of a kind, or null when none exist. */
  async latest(kind: ArtifactKind): Promise<StoredArtifact | null> {
    const items = await this.list(kind);
    return items.at(-1) ?? null;
  }

  /**
   * Resolve an artifact by workspace-relative path or by artifact id (scanning
   * kind directories for matching frontmatter). Returns null when not found.
   */
  async inspect(artifactIdOrPath: string): Promise<ArtifactFile | null> {
    const absolute = this.resolveArtifactPath(artifactIdOrPath);
    if (absolute !== null && (await this.exists(absolute))) {
      return this.readAt(absolute);
    }
    for (const kind of ARTIFACT_KINDS) {
      const matches = (await this.list(kind)).filter((item) => item.artifactId === artifactIdOrPath);
      const best = matches.at(-1);
      if (best) {
        const target = this.resolveArtifactPath(best.path);
        if (target !== null && (await this.exists(target))) {
          return this.readAt(target);
        }
      }
    }
    return null;
  }

  // -- internals ------------------------------------------------------------

  /** Stored path of any saved version of `artifactId` at or above `version`. */
  private async findVersionConflict(
    kind: ArtifactKind,
    artifactId: string,
    version: number,
  ): Promise<string | null> {
    let entries: readonly string[];
    try {
      entries = await readdir(path.join(this.artifactsDir, kind));
    } catch {
      return null;
    }
    for (const entry of entries) {
      if (!VERSION_FILE_PATTERN.test(entry)) continue;
      const file = await this.readAt(path.join(this.artifactsDir, kind, entry)).catch(() => null);
      if (file === null) continue;
      const id = file.frontmatter.id;
      const savedVersion = file.frontmatter.version;
      if (id === artifactId && typeof savedVersion === 'number' && savedVersion >= version) {
        return file.path;
      }
    }
    return null;
  }

  private async exists(absolutePath: string): Promise<boolean> {
    try {
      await stat(absolutePath);
      return true;
    } catch {
      return false;
    }
  }

  private async readAt(absolutePath: string): Promise<ArtifactFile> {
    let content: string;
    try {
      content = await readFile(absolutePath, 'utf8');
    } catch (error) {
      throw new InvalidArtifactFileError(
        this.toStoredPath(absolutePath),
        error instanceof Error ? error.message : String(error),
      );
    }
    return { ...this.splitFrontmatter(content, absolutePath), path: this.toStoredPath(absolutePath) };
  }

  /** Split on the first two `---` lines; the body follows the closing one. */
  private splitFrontmatter(
    content: string,
    absolutePath: string,
  ): { frontmatter: Record<string, unknown>; body: string } {
    const storedPath = this.toStoredPath(absolutePath);
    const lines = content.split('\n');
    if ((lines[0] ?? '').trim() !== '---') {
      throw new InvalidArtifactFileError(storedPath, 'missing opening "---" frontmatter delimiter');
    }
    const closeIndex = lines.findIndex((line, index) => index > 0 && line.trim() === '---');
    if (closeIndex === -1) {
      throw new InvalidArtifactFileError(storedPath, 'missing closing "---" frontmatter delimiter');
    }
    const yamlText = lines.slice(1, closeIndex).join('\n');
    let parsed: unknown;
    try {
      parsed = parseYaml(yamlText);
    } catch (error) {
      throw new InvalidArtifactFileError(
        storedPath,
        `invalid YAML frontmatter: ${error instanceof Error ? error.message : String(error)}`,
      );
    }
    if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
      throw new InvalidArtifactFileError(storedPath, 'frontmatter is not a YAML mapping');
    }
    const body = lines.slice(closeIndex + 1).join('\n').replace(/^\n+/, '');
    return { frontmatter: parsed as Record<string, unknown>, body };
  }

  private toStoredArtifact(
    kind: ArtifactKind,
    frontmatter: Record<string, unknown>,
    storedPath: string,
  ): StoredArtifact {
    const artifactId = frontmatter.id;
    const title = frontmatter.title;
    const version = frontmatter.version;
    if (typeof artifactId !== 'string' || artifactId.length === 0) {
      throw new InvalidArtifactFileError(storedPath, 'frontmatter "id" must be a non-empty string');
    }
    if (typeof title !== 'string') {
      throw new InvalidArtifactFileError(storedPath, 'frontmatter "title" must be a string');
    }
    if (typeof version !== 'number' || !Number.isInteger(version) || version < 1) {
      throw new InvalidArtifactFileError(storedPath, 'frontmatter "version" must be a positive integer');
    }
    return { artifactId, kind, title, version, path: storedPath };
  }

  /**
   * Resolve an input path (absolute, workspace-relative, or store-relative) to
   * an absolute path inside the artifacts directory, or null when it escapes
   * the store.
   */
  private resolveArtifactPath(input: string): string | null {
    const candidates = path.isAbsolute(input)
      ? [path.resolve(input)]
      : [path.resolve(this.workspaceRoot, input), path.resolve(this.artifactsDir, input)];
    for (const candidate of candidates) {
      const relative = path.relative(this.artifactsDir, candidate);
      if (relative !== '' && !relative.startsWith('..') && !path.isAbsolute(relative)) {
        return candidate;
      }
    }
    return null;
  }

  /** Workspace-relative path (posix separators) for the standard layout. */
  private toStoredPath(absolutePath: string): string {
    const relative = path.relative(this.workspaceRoot, absolutePath);
    if (relative.startsWith('..')) {
      return path.relative(this.artifactsDir, absolutePath).split(path.sep).join('/');
    }
    return relative.split(path.sep).join('/');
  }
}

function stringCompare(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}
