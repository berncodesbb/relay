/**
 * Core artifact contracts.
 *
 * Artifact kinds and their dependency graph are owned by the core so that the
 * reducer, policies, and workflow controller never depend on storage or
 * Markdown rendering details.
 */

export const ARTIFACT_KINDS = [
  'opportunity-brief',
  'decision-log',
  'prd',
  'trd',
  'execution-plan',
  'verification-report',
  'delivery-report',
] as const;

export type ArtifactKind = (typeof ARTIFACT_KINDS)[number];

export function isArtifactKind(value: unknown): value is ArtifactKind {
  return typeof value === 'string' && (ARTIFACT_KINDS as readonly string[]).includes(value);
}

/**
 * Upstream artifacts that must exist (and normally be approved) before an
 * artifact of a given kind is created. Used for deterministic downstream
 * invalidation: revising an approved upstream artifact marks every transitive
 * dependent stale until it is regenerated and re-approved.
 */
export const ARTIFACT_DEPENDENCIES: Readonly<Record<ArtifactKind, readonly ArtifactKind[]>> = {
  'opportunity-brief': [],
  'decision-log': ['opportunity-brief'],
  prd: ['opportunity-brief'],
  trd: ['prd'],
  'execution-plan': ['prd', 'trd'],
  'verification-report': ['execution-plan'],
  'delivery-report': ['prd', 'trd', 'execution-plan', 'verification-report'],
};

/** Artifact kinds that act as contractual checkpoints requiring explicit approval. */
export const CONTRACTUAL_ARTIFACT_KINDS: readonly ArtifactKind[] = [
  'opportunity-brief',
  'prd',
  'trd',
  'execution-plan',
  'verification-report',
];

export function isContractualArtifactKind(kind: ArtifactKind): boolean {
  return CONTRACTUAL_ARTIFACT_KINDS.includes(kind);
}

/** All kinds transitively downstream of `kind` (excluding itself). */
export function downstreamArtifactKinds(kind: ArtifactKind): ArtifactKind[] {
  const result = new Set<ArtifactKind>();
  const visit = (k: ArtifactKind): void => {
    for (const candidate of ARTIFACT_KINDS) {
      if (result.has(candidate)) continue;
      if (ARTIFACT_DEPENDENCIES[candidate].includes(k)) {
        result.add(candidate);
        visit(candidate);
      }
    }
  };
  visit(kind);
  return [...result];
}
