/**
 * Deterministic downstream artifact invalidation.
 *
 * When an approved artifact is revised, every existing artifact that is
 * transitively downstream of it in the core dependency graph is stale until it
 * is regenerated and re-approved. The plan computed here is purely a function
 * of the core graph and the kinds that currently exist, so the reducer and
 * policies can replay it identically.
 */

import { ARTIFACT_KINDS, type ArtifactKind, downstreamArtifactKinds, isArtifactKind } from '../core/artifacts.js';
import { MODE_CONTRACTS, type WorkflowMode, WORKFLOW_MODES } from '../core/modes.js';
import { UnknownArtifactKindError } from './schemas.js';

export type PlanInvalidationInput = {
  /** The kind whose artifact was just revised. */
  readonly revisedKind: ArtifactKind;
  /** Kinds that currently exist in project state (unknown values are ignored). */
  readonly existingKinds: readonly ArtifactKind[];
};

/**
 * Existing kinds that are transitively downstream of `revisedKind`, returned
 * in canonical `ARTIFACT_KINDS` order so invalidation application is stable.
 * Kinds with no transitive dependency on the revised kind (and the revised
 * kind itself) are never included.
 */
export function planInvalidation(input: PlanInvalidationInput): readonly ArtifactKind[] {
  if (!isArtifactKind(input.revisedKind)) {
    throw new UnknownArtifactKindError(
      String(input.revisedKind),
      'expected one of: ' + ARTIFACT_KINDS.join(', '),
    );
  }
  const downstream = new Set<ArtifactKind>(downstreamArtifactKinds(input.revisedKind));
  const existing = new Set<ArtifactKind>(
    input.existingKinds.filter((kind): kind is ArtifactKind => isArtifactKind(kind)),
  );
  return ARTIFACT_KINDS.filter((kind) => existing.has(kind) && downstream.has(kind));
}

/**
 * The mode that produces artifacts of `kind`, derived by inverting
 * `MODE_CONTRACTS` (never hardcoded), so mode/kind mappings cannot drift.
 */
export function kindToMode(kind: ArtifactKind): WorkflowMode {
  for (const mode of WORKFLOW_MODES) {
    if (MODE_CONTRACTS[mode].producedArtifact === kind) {
      return mode;
    }
  }
  throw new UnknownArtifactKindError(
    String(kind),
    'no workflow mode produces this artifact kind',
  );
}
