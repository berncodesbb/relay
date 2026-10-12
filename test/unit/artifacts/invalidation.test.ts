/**
 * Deterministic downstream invalidation and kind-to-mode mapping.
 *
 * Expectations are derived from the core dependency graph in
 * `src/core/artifacts.ts`, not from this module's implementation.
 */

import { describe, expect, it } from 'vitest';
import { ARTIFACT_DEPENDENCIES, ARTIFACT_KINDS, type ArtifactKind, downstreamArtifactKinds } from '../../../src/core/artifacts.js';
import { MODE_CONTRACTS, WORKFLOW_MODES } from '../../../src/core/modes.js';
import { kindToMode, planInvalidation } from '../../../src/artifacts/invalidation.js';
import { UnknownArtifactKindError } from '../../../src/artifacts/schemas.js';

const ALL_KINDS: readonly ArtifactKind[] = [...ARTIFACT_KINDS];

describe('planInvalidation', () => {
  it('revising the PRD invalidates everything downstream of it, in canonical order', () => {
    const plan = planInvalidation({ revisedKind: 'prd', existingKinds: ALL_KINDS });
    // Canonical ARTIFACT_KINDS order filtered to downstream(prd) = {trd,
    // execution-plan, verification-report, delivery-report}; decision-log only
    // depends on the opportunity brief and stays valid.
    expect(plan).toEqual(['trd', 'execution-plan', 'verification-report', 'delivery-report']);
    expect(plan).not.toContain('decision-log');
    expect(plan).not.toContain('prd');
  });

  it('revising the opportunity brief invalidates every other kind, including the decision log', () => {
    const plan = planInvalidation({ revisedKind: 'opportunity-brief', existingKinds: ALL_KINDS });
    expect(plan).toEqual([
      'decision-log',
      'prd',
      'trd',
      'execution-plan',
      'verification-report',
      'delivery-report',
    ]);
  });

  it('revising a leaf artifact invalidates nothing', () => {
    expect(planInvalidation({ revisedKind: 'delivery-report', existingKinds: ALL_KINDS })).toEqual([]);
  });

  it('only includes kinds that actually exist', () => {
    expect(
      planInvalidation({ revisedKind: 'trd', existingKinds: ['prd', 'trd', 'execution-plan'] }),
    ).toEqual(['execution-plan']);
    expect(planInvalidation({ revisedKind: 'trd', existingKinds: ['prd'] })).toEqual([]);
  });

  it('ignores unknown entries in existingKinds and dedupes', () => {
    const plan = planInvalidation({
      revisedKind: 'prd',
      existingKinds: ['trd', 'trd', 'bogus' as ArtifactKind, 'delivery-report'],
    });
    expect(plan).toEqual(['trd', 'delivery-report']);
  });

  it('matches the core dependency graph for every revised kind', () => {
    for (const kind of ALL_KINDS) {
      const expected = ARTIFACT_KINDS.filter(
        (candidate) => candidate !== kind && downstreamArtifactKinds(kind).includes(candidate),
      );
      expect(planInvalidation({ revisedKind: kind, existingKinds: ALL_KINDS })).toEqual(expected);
    }
  });

  it('throws for unknown revised kinds', () => {
    expect(() => planInvalidation({ revisedKind: 'wishlist' as ArtifactKind, existingKinds: [] })).toThrowError(
      UnknownArtifactKindError,
    );
  });
});

describe('kindToMode', () => {
  it('maps every kind to the mode that produces it', () => {
    const expected: Record<ArtifactKind, string> = {
      'opportunity-brief': 'discovery',
      'decision-log': 'implementation',
      prd: 'specification',
      trd: 'technical-design',
      'execution-plan': 'planning',
      'verification-report': 'verification',
      'delivery-report': 'delivery',
    };
    for (const kind of ALL_KINDS) {
      expect(kindToMode(kind)).toBe(expected[kind]);
    }
  });

  it('is the exact inverse of MODE_CONTRACTS.producedArtifact', () => {
    for (const mode of WORKFLOW_MODES) {
      expect(kindToMode(MODE_CONTRACTS[mode].producedArtifact)).toBe(mode);
    }
    expect(ARTIFACT_DEPENDENCIES['decision-log']).toContain('opportunity-brief');
  });

  it('throws for unknown kinds', () => {
    expect(() => kindToMode('wishlist' as ArtifactKind)).toThrowError(UnknownArtifactKindError);
  });
});
