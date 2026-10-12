/**
 * Artifact draft schema boundaries.
 *
 * Model output is untrusted: every field, cap, identifier alphabet, and path
 * rule must reject bad input with actionable issues before anything is
 * rendered or written.
 */

import { describe, expect, it } from 'vitest';
import {
  ARTIFACT_DRAFT_SCHEMAS,
  artifactDraftSchema,
  formatZodIssues,
  parseArtifactDraft,
  UnknownArtifactKindError,
} from '../../../src/artifacts/schemas.js';
import { ARTIFACT_KINDS } from '../../../src/core/artifacts.js';
import {
  clone,
  sampleDeliveryReport,
  sampleDecisionLog,
  sampleDraft,
  sampleExecutionPlan,
  sampleOpportunityBrief,
  samplePrd,
  sampleTrd,
  sampleVerificationReport,
} from './samples.test.js';

describe('artifact draft schemas', () => {
  it('accepts a valid sample for every kind', () => {
    for (const kind of ARTIFACT_KINDS) {
      const result = parseArtifactDraft(kind, sampleDraft(kind));
      expect(result.ok, `${kind} should parse`).toBe(true);
    }
  });

  it('exposes one schema per kind in the lookup', () => {
    expect(Object.keys(ARTIFACT_DRAFT_SCHEMAS).sort()).toEqual([...ARTIFACT_KINDS].sort());
  });

  it('rejects missing required fields', () => {
    const brief = clone(sampleOpportunityBrief());
    delete (brief as Record<string, unknown>)['problem'];
    const result = parseArtifactDraft('opportunity-brief', brief);
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.issues.some((issue) => issue.includes('problem'))).toBe(true);
    }
  });

  it('rejects over-cap arrays', () => {
    const brief = clone(sampleOpportunityBrief());
    brief.targetUsers = Array.from({ length: 13 }, () => ({
      name: 'User',
      description: 'Description.',
    }));
    const result = parseArtifactDraft('opportunity-brief', brief);
    expect(result.ok).toBe(false);

    const plan = clone(sampleExecutionPlan());
    plan.tasks = Array.from({ length: 51 }, (_, index) => ({
      id: `T-${index + 1}`,
      title: 'Task',
      description: 'Description.',
      dependencies: [],
      affectedFiles: [],
      checks: [],
      integrationPoints: [],
    }));
    expect(parseArtifactDraft('execution-plan', plan).ok).toBe(false);
  });

  it('rejects empty arrays where at least one entry is required', () => {
    const report = clone(sampleVerificationReport());
    report.results = [];
    expect(parseArtifactDraft('verification-report', report).ok).toBe(false);

    const delivery = clone(sampleDeliveryReport());
    delivery.heldActions = [];
    expect(parseArtifactDraft('delivery-report', delivery).ok).toBe(false);
  });

  it('rejects absolute paths, traversal, and home references in affectedFiles', () => {
    for (const bad of ['/etc/passwd', '../outside.ts', 'src/../../escape.ts', 'C:\\repo\\file.ts', '~/notes.txt']) {
      const plan = clone(sampleExecutionPlan());
      plan.tasks[0]!.affectedFiles = [bad];
      const result = parseArtifactDraft('execution-plan', plan);
      expect(result.ok, `${bad} should be rejected`).toBe(false);
      if (!result.ok) {
        expect(result.issues.join(' ')).toContain(bad);
      }
    }
  });

  it('accepts workspace-relative affected files', () => {
    const plan = clone(sampleExecutionPlan());
    plan.tasks[0]!.affectedFiles = ['src/cli.tsx', 'test/unit/cli/cli.test.ts', 'docs/architecture.md'];
    expect(parseArtifactDraft('execution-plan', plan).ok).toBe(true);
  });

  it('rejects bad status enums', () => {
    const report = clone(sampleVerificationReport());
    report.results[0]!.status = 'skipped';
    const result = parseArtifactDraft('verification-report', report);
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.issues.join(' ')).toContain('status');
    }
  });

  it('rejects wrong types', () => {
    const brief = clone(sampleOpportunityBrief());
    (brief as Record<string, unknown>)['title'] = 42;
    expect(parseArtifactDraft('opportunity-brief', brief).ok).toBe(false);

    const report = clone(sampleVerificationReport());
    (report as Record<string, unknown>)['overallPass'] = 'yes';
    expect(parseArtifactDraft('verification-report', report).ok).toBe(false);

    const prd = clone(samplePrd());
    (prd as Record<string, unknown>)['requirements'] = 'REQ-1';
    expect(parseArtifactDraft('prd', prd).ok).toBe(false);

    const trd = clone(sampleTrd());
    (trd as Record<string, unknown>)['rolloutPlan'] = { steps: [] };
    expect(parseArtifactDraft('trd', trd).ok).toBe(false);
  });

  it('rejects identifiers outside the safe alphabet', () => {
    const prd = clone(samplePrd());
    prd.requirements[0]!.id = 'req 1';
    expect(parseArtifactDraft('prd', prd).ok).toBe(false);

    const lower = clone(samplePrd());
    lower.requirements[0]!.id = 'req-1';
    expect(parseArtifactDraft('prd', lower).ok).toBe(false);

    const plan = clone(sampleExecutionPlan());
    plan.tasks[1]!.dependencies = ['t-1'];
    expect(parseArtifactDraft('execution-plan', plan).ok).toBe(false);
  });

  it('rejects multi-line titles', () => {
    const brief = clone(sampleOpportunityBrief());
    brief.title = 'Line one\nLine two';
    expect(parseArtifactDraft('opportunity-brief', brief).ok).toBe(false);
  });

  it('allows explicitly optional empty strings and zero-length lists', () => {
    const brief = clone(sampleOpportunityBrief());
    brief.evidence = [];
    brief.assumptions = [];
    brief.openQuestions = [];
    expect(parseArtifactDraft('opportunity-brief', brief).ok).toBe(true);

    const report = clone(sampleVerificationReport());
    report.results[0]!.criterionId = '';
    report.notes = '';
    expect(parseArtifactDraft('verification-report', report).ok).toBe(true);

    const delivery = clone(sampleDeliveryReport());
    delivery.trace[0]!.requirementId = '';
    delivery.trace[0]!.designDecisions = [];
    expect(parseArtifactDraft('delivery-report', delivery).ok).toBe(true);

    const log = clone(sampleDecisionLog());
    log.entries[0]!.alternatives = '';
    expect(parseArtifactDraft('decision-log', log).ok).toBe(true);
  });

  it('strips unknown keys instead of trusting them', () => {
    const brief = clone(sampleOpportunityBrief()) as Record<string, unknown>;
    brief.sneaky = '<script>alert(1)</script>';
    const result = parseArtifactDraft('opportunity-brief', brief);
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(JSON.stringify(result.draft)).not.toContain('sneaky');
    }
  });

  it('formats issues as "path: message" with path segments joined by dots', () => {
    const brief = clone(sampleOpportunityBrief());
    brief.targetUsers[0]!.name = '';
    const result = parseArtifactDraft('opportunity-brief', brief);
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.issues.join(' | ')).toContain('targetUsers.0.name:');
    }
  });

  it('artifactDraftSchema returns the per-kind schema and throws for unknown kinds', () => {
    expect(artifactDraftSchema('prd')).toBe(ARTIFACT_DRAFT_SCHEMAS.prd);
    expect(() => artifactDraftSchema('wishlist' as never)).toThrowError(UnknownArtifactKindError);
    expect(() => parseArtifactDraft('wishlist' as never, {})).toThrowError(/Unknown artifact kind/);
  });

  it('formatZodIssues falls back to (root) for top-level problems', () => {
    const result = parseArtifactDraft('decision-log', 'not an object');
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.issues.length).toBeGreaterThan(0);
      expect(result.issues.join(' ')).toContain('(root):');
    }
    expect(typeof formatZodIssues).toBe('function');
  });
});
