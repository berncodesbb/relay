/**
 * Valid draft samples for every artifact kind.
 *
 * Exported so the schema, render, and store suites share one canonical set of
 * well-formed payloads; tests clone and mutate them to probe rejections.
 */

import { describe, expect, it } from 'vitest';
import { ARTIFACT_KINDS, type ArtifactKind } from '../../../src/core/artifacts.js';
import { parseArtifactDraft } from '../../../src/artifacts/schemas.js';

export function clone<T>(value: T): T {
  return structuredClone(value);
}

export function sampleOpportunityBrief() {
  return {
    title: 'Faster release feedback loop',
    problem: 'Releases fail late because feedback only arrives after a full rollout.',
    targetUsers: [
      { name: 'Platform engineers', description: 'Own the deploy pipeline and its gates.' },
      { name: 'Service owners', description: 'Ship changes to production weekly.' },
    ],
    evidence: [
      { description: '35% of rollbacks trace to failures first seen in production.', source: 'incident review 2026-09' },
      { description: 'Median time from merge to production signal is 40 minutes.', source: '' },
    ],
    desiredOutcomes: ['Halve the rollback rate within two quarters.'],
    assumptions: ['Teams adopt the new gate without a migration period.'],
    constraints: ['No new cloud services.'],
    openQuestions: ['Which repository should host the gate?'],
  };
}

export function samplePrd() {
  return {
    title: 'Release gate PRD',
    summary: 'Specify a pre-merge release gate that surfaces rollout risk before code lands.',
    requirements: [
      {
        id: 'REQ-1',
        statement: 'The gate shall run the repository test suite on every proposed change.',
        rationale: 'Late failures are the dominant rollback cause.',
      },
    ],
    exclusions: ['Mobile client behavior.'],
    acceptanceCriteria: [
      {
        id: 'AC-1',
        given: 'a repository with a passing test suite',
        when: 'a change is proposed',
        then: 'the gate runs the suite and reports the result before merge',
      },
    ],
  };
}

export function sampleTrd() {
  return {
    title: 'Release gate TRD',
    summary: 'Map the release gate onto the existing CLI and event log.',
    repositoryFindings: [{ area: 'src/cli', finding: 'Argument parsing already supports subcommands.' }],
    architecture: 'A gate evaluator subscribes to the append-only event log and blocks ADVANCE outcomes until checks pass.',
    interfaces: [{ name: 'GateEvaluator', description: 'Evaluates configured checks for a change.' }],
    migrations: [{ description: 'Add a checks table.', risk: 'Low; additive migration.' }],
    risks: [{ risk: 'Gate adds latency to every merge.', mitigation: 'Cache results per commit hash.' }],
    rolloutPlan: 'Ship behind a flag, enable for one team, then expand.',
    rollbackPlan: 'Disable the flag; no data migration is required.',
  };
}

export function sampleExecutionPlan() {
  return {
    title: 'Release gate execution plan',
    summary: 'Implement the gate evaluator and wire it into the transition policy.',
    tasks: [
      {
        id: 'T-1',
        title: 'Add gate evaluator',
        description: 'Implement the evaluator with a Zod-validated configuration.',
        dependencies: [],
        affectedFiles: ['src/policies/gate.ts', 'test/unit/policies/gate.test.ts'],
        checks: ['npm test -- --run test/unit/policies'],
        integrationPoints: ['Transition policy CONTINUE/ADVANCE decisions.'],
      },
      {
        id: 'T-2',
        title: 'Wire the evaluator into the CLI',
        description: 'Invoke the evaluator before advancing modes.',
        dependencies: ['T-1'],
        affectedFiles: ['src/cli/commands/run.ts'],
        checks: ['npm run typecheck'],
        integrationPoints: [],
      },
    ],
  };
}

export function sampleVerificationReport() {
  return {
    title: 'Release gate verification report',
    summary: 'Every approved acceptance criterion was evaluated against the diff.',
    results: [
      {
        criterionId: 'AC-1',
        criterion: 'Gate runs the suite before merge',
        status: 'pass',
        evidence: 'test/unit/policies/gate.test.ts covers the blocking path.',
      },
      {
        criterionId: 'AC-2',
        criterion: 'Latency stays under 5s',
        status: 'unevaluated',
        evidence: 'No latency budget was instrumented in this change.',
      },
    ],
    overallPass: false,
    notes: 'One criterion remains unevaluated; latency instrumentation is follow-up work.',
  };
}

export function sampleDeliveryReport() {
  return {
    title: 'Release gate delivery report',
    summary: 'Trace from requirement to change to verification evidence.',
    trace: [
      {
        requirementId: 'REQ-1',
        requirement: 'The gate shall run the test suite on every proposed change.',
        designDecisions: ['Evaluator is deterministic and config-driven.'],
        changes: ['Added src/policies/gate.ts.'],
        evidence: ['Verification report AC-1: pass.'],
      },
    ],
    nextSteps: ['User reviews this report.'],
    heldActions: ['git commit', 'git push', 'pull request creation'],
  };
}

export function sampleDecisionLog() {
  return {
    title: 'Release gate decision log',
    entries: [
      {
        decision: 'Use node:sqlite for project state',
        rationale: 'Avoids a native dependency and ships with Node 22.',
        alternatives: 'better-sqlite3 was rejected for its native build step.',
        mode: 'implementation',
      },
    ],
  };
}

export function sampleDraft(kind: ArtifactKind): unknown {
  switch (kind) {
    case 'opportunity-brief':
      return sampleOpportunityBrief();
    case 'prd':
      return samplePrd();
    case 'trd':
      return sampleTrd();
    case 'execution-plan':
      return sampleExecutionPlan();
    case 'verification-report':
      return sampleVerificationReport();
    case 'delivery-report':
      return sampleDeliveryReport();
    case 'decision-log':
      return sampleDecisionLog();
  }
}

describe('artifact draft samples', () => {
  it('every kind sample parses cleanly', () => {
    for (const kind of ARTIFACT_KINDS) {
      const result = parseArtifactDraft(kind, sampleDraft(kind));
      expect(result.ok, `${kind} sample should parse`).toBe(true);
    }
  });
});
