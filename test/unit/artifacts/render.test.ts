/**
 * Artifact Markdown rendering.
 *
 * Output must be deterministic (same draft, identical string), carry the key
 * headings humans and tools navigate by, survive hostile characters inside
 * table cells, and contain no HTML.
 */

import { describe, expect, it } from 'vitest';
import { renderArtifactMarkdown } from '../../../src/artifacts/render.js';
import { InvalidArtifactDraftError } from '../../../src/artifacts/schemas.js';
import {
  clone,
  sampleDecisionLog,
  sampleDeliveryReport,
  sampleDraft,
  sampleExecutionPlan,
  sampleOpportunityBrief,
  samplePrd,
  sampleTrd,
  sampleVerificationReport,
} from './samples.test.js';

describe('renderArtifactMarkdown', () => {
  it('is deterministic across calls for every kind', () => {
    for (const kind of [
      'opportunity-brief',
      'decision-log',
      'prd',
      'trd',
      'execution-plan',
      'verification-report',
      'delivery-report',
    ] as const) {
      const draft = sampleDraft(kind);
      expect(renderArtifactMarkdown(kind, draft)).toBe(renderArtifactMarkdown(kind, clone(draft)));
    }
  });

  it('renders the opportunity brief structure', () => {
    const markdown = renderArtifactMarkdown('opportunity-brief', sampleOpportunityBrief());
    expect(markdown).toContain('# Faster release feedback loop');
    expect(markdown).toContain('## Problem');
    expect(markdown).toContain('## Target users');
    expect(markdown).toContain('## Evidence');
    expect(markdown).toContain('## Desired outcomes');
    expect(markdown).toContain('## Assumptions');
    expect(markdown).toContain('## Constraints');
    expect(markdown).toContain('## Open questions');
    expect(markdown).toContain('| Name | Description |');
    expect(markdown).toContain('| Platform engineers | Own the deploy pipeline and its gates. |');
  });

  it('renders requirements with ids and Given/When/Then criteria', () => {
    const markdown = renderArtifactMarkdown('prd', samplePrd());
    expect(markdown).toContain('## Requirements');
    expect(markdown).toContain('### REQ-1');
    expect(markdown).toContain('## Exclusions');
    expect(markdown).toContain('## Acceptance criteria');
    expect(markdown).toContain('### AC-1');
    expect(markdown).toContain('- **Given:** a repository with a passing test suite');
    expect(markdown).toContain('- **When:** a change is proposed');
    expect(markdown).toContain('- **Then:** the gate runs the suite and reports the result before merge');
  });

  it('renders the TRD architecture, rollout, and rollback sections', () => {
    const markdown = renderArtifactMarkdown('trd', sampleTrd());
    expect(markdown).toContain('## Architecture');
    expect(markdown).toContain('## Interfaces');
    expect(markdown).toContain('## Rollout');
    expect(markdown).toContain('## Rollback');
    expect(markdown).toContain('Ship behind a flag, enable for one team, then expand.');
  });

  it('renders plan tasks as "### Task <id> — <title>" with detail bullets', () => {
    const markdown = renderArtifactMarkdown('execution-plan', sampleExecutionPlan());
    expect(markdown).toContain('### Task T-1 — Add gate evaluator');
    expect(markdown).toContain('- **Dependencies:** none');
    expect(markdown).toContain('`src/policies/gate.ts`');
    expect(markdown).toContain('`npm test -- --run test/unit/policies`');
    expect(markdown).toContain('- **Dependencies:** T-1');
  });

  it('renders verification results as a criterion/status/evidence table', () => {
    const markdown = renderArtifactMarkdown('verification-report', sampleVerificationReport());
    expect(markdown).toContain('| Criterion | Status | Evidence |');
    expect(markdown).toContain('| AC-1 — Gate runs the suite before merge | pass |');
    expect(markdown).toContain('| unevaluated |');
    expect(markdown).toContain('## Overall');
    expect(markdown).toContain('**fail**');
    expect(markdown).toContain('## Notes');
  });

  it('renders delivery trace as requirement blocks with held actions', () => {
    const markdown = renderArtifactMarkdown('delivery-report', sampleDeliveryReport());
    expect(markdown).toContain('## Trace');
    expect(markdown).toContain('### REQ-1');
    expect(markdown).toContain('## Next steps');
    expect(markdown).toContain('## Held actions');
    expect(markdown).toContain('- git commit');
  });

  it('renders the decision log entries with mode and rationale', () => {
    const markdown = renderArtifactMarkdown('decision-log', sampleDecisionLog());
    expect(markdown).toContain('## Entries');
    expect(markdown).toContain('### 1. Use node:sqlite for project state');
    expect(markdown).toContain('- **Mode:** implementation');
    expect(markdown).toContain('- **Alternatives:** better-sqlite3 was rejected for its native build step.');
  });

  it('escapes pipes and flattens newlines inside table cells', () => {
    const report = clone(sampleVerificationReport());
    report.results[0]!.criterion = 'Criterion with | pipe';
    report.results[0]!.evidence = 'line one\nline two';
    const markdown = renderArtifactMarkdown('verification-report', report);
    const row = markdown.split('\n').find((line) => line.startsWith('| AC-1'));
    expect(row).toBeDefined();
    expect(row).not.toContain('\n');
    expect(row).toContain('Criterion with \\| pipe');
    expect(row).toContain('line one line two');
  });

  it('uses the criterion text alone when no criterion id is present', () => {
    const report = clone(sampleVerificationReport());
    report.results[0]!.criterionId = '';
    const markdown = renderArtifactMarkdown('verification-report', report);
    expect(markdown).toContain('| Gate runs the suite before merge | pass |');
  });

  it('emits no HTML', () => {
    for (const kind of [
      'opportunity-brief',
      'decision-log',
      'prd',
      'trd',
      'execution-plan',
      'verification-report',
      'delivery-report',
    ] as const) {
      const markdown = renderArtifactMarkdown(kind, sampleDraft(kind));
      expect(markdown, `${kind} must not contain HTML`).not.toMatch(/<[a-zA-Z!/]/);
    }
  });

  it('throws InvalidArtifactDraftError with the issue list for bad drafts', () => {
    const prd = clone(samplePrd()) as Record<string, unknown>;
    delete prd['acceptanceCriteria'];
    prd['title'] = '';
    let error: unknown;
    try {
      renderArtifactMarkdown('prd', prd);
    } catch (caught) {
      error = caught;
    }
    expect(error).toBeInstanceOf(InvalidArtifactDraftError);
    const message = error instanceof Error ? error.message : '';
    expect(message).toContain('Invalid prd draft');
    expect(message).toContain('title');
    expect(message).toContain('acceptanceCriteria');
  });
});
