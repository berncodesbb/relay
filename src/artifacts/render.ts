/**
 * Artifact Markdown rendering.
 *
 * Turns a validated draft payload into a deterministic, human-readable
 * Markdown body (same input always produces the identical string). Sections,
 * headings, and tables are fixed per artifact kind so that diffs between
 * versions are meaningful; user/model prose only ever appears as content, and
 * table cells are escaped so values cannot break out of the table. No HTML is
 * emitted.
 */

import { type ArtifactKind } from '../core/artifacts.js';
import { type ArtifactDraftOf, requireArtifactDraft } from './schemas.js';

const NONE = '_(none recorded)_';

/** Heading plus its content, separated by a blank line. */
function section(heading: string, content: string): string {
  return `${heading}\n\n${content}`;
}

/** Markdown bullet list, or a deterministic placeholder when empty. */
function bullets(items: readonly string[]): string {
  if (items.length === 0) return NONE;
  return items.map((item) => `- ${item}`).join('\n');
}

/** Sub-bullets indented under a labeled parent line. */
function labeledList(label: string, items: readonly string[], itemFormatter: (item: string) => string): string {
  if (items.length === 0) return `- **${label}:** none`;
  const rendered = items.map((item) => `  - ${itemFormatter(item)}`).join('\n');
  return `- **${label}:**\n${rendered}`;
}

/** Inline comma-joined values, or "none". */
function inlineList(items: readonly string[]): string {
  return items.length === 0 ? 'none' : items.join(', ');
}

/** Make a value safe for a single table row: escape pipes, flatten newlines. */
function cell(value: string): string {
  return value.replace(/\|/g, '\\|').replace(/[\r\n]+/g, ' ');
}

function table(headers: readonly string[], rows: readonly (readonly string[])[]): string {
  const head = `| ${headers.join(' | ')} |`;
  const separator = `| ${headers.map(() => '---').join(' | ')} |`;
  const body = rows.map((row) => `| ${row.map(cell).join(' | ')} |`).join('\n');
  return [head, separator, body].filter((line) => line.length > 0).join('\n');
}

function renderOpportunityBrief(draft: ArtifactDraftOf<'opportunity-brief'>): string {
  const sections: string[] = [`# ${draft.title}`];
  sections.push(section('## Problem', draft.problem));
  sections.push(
    section(
      '## Target users',
      table(
        ['Name', 'Description'],
        draft.targetUsers.map((user) => [user.name, user.description]),
      ),
    ),
  );
  sections.push(
    section(
      '## Evidence',
      bullets(draft.evidence.map((item) => (item.source ? `${item.description} (source: ${item.source})` : item.description))),
    ),
  );
  sections.push(section('## Desired outcomes', bullets(draft.desiredOutcomes)));
  sections.push(section('## Assumptions', bullets(draft.assumptions)));
  sections.push(section('## Constraints', bullets(draft.constraints)));
  sections.push(section('## Open questions', bullets(draft.openQuestions)));
  return joinSections(sections);
}

function renderPrd(draft: ArtifactDraftOf<'prd'>): string {
  const sections: string[] = [`# ${draft.title}`];
  sections.push(section('## Summary', draft.summary));
  sections.push(
    section(
      '## Requirements',
      draft.requirements
        .map((requirement) =>
          [
            `### ${requirement.id}`,
            requirement.statement,
            `_Rationale:_ ${requirement.rationale}`,
          ].join('\n\n'),
        )
        .join('\n\n'),
    ),
  );
  sections.push(section('## Exclusions', bullets(draft.exclusions)));
  sections.push(
    section(
      '## Acceptance criteria',
      draft.acceptanceCriteria
        .map((criterion) =>
          [
            `### ${criterion.id}`,
            `- **Given:** ${criterion.given}`,
            `- **When:** ${criterion.when}`,
            `- **Then:** ${criterion.then}`,
          ].join('\n'),
        )
        .join('\n\n'),
    ),
  );
  return joinSections(sections);
}

function renderTrd(draft: ArtifactDraftOf<'trd'>): string {
  const sections: string[] = [`# ${draft.title}`];
  sections.push(section('## Summary', draft.summary));
  sections.push(
    section(
      '## Repository findings',
      draft.repositoryFindings.length === 0
        ? NONE
        : table(
            ['Area', 'Finding'],
            draft.repositoryFindings.map((finding) => [finding.area, finding.finding]),
          ),
    ),
  );
  sections.push(section('## Architecture', draft.architecture));
  sections.push(
    section(
      '## Interfaces',
      bullets(draft.interfaces.map((iface) => `**${iface.name}** — ${iface.description}`)),
    ),
  );
  sections.push(
    section(
      '## Migrations',
      bullets(draft.migrations.map((migration) => `${migration.description} (risk: ${migration.risk})`)),
    ),
  );
  sections.push(
    section(
      '## Risks',
      bullets(draft.risks.map((risk) => `${risk.risk} — mitigation: ${risk.mitigation}`)),
    ),
  );
  sections.push(section('## Rollout', draft.rolloutPlan));
  sections.push(section('## Rollback', draft.rollbackPlan));
  return joinSections(sections);
}

function renderExecutionPlan(draft: ArtifactDraftOf<'execution-plan'>): string {
  const sections: string[] = [`# ${draft.title}`];
  sections.push(section('## Summary', draft.summary));
  const tasks = draft.tasks
    .map((task) =>
      [
        `### Task ${task.id} — ${task.title}`,
        task.description,
        `- **Dependencies:** ${inlineList(task.dependencies)}`,
        labeledList('Affected files', task.affectedFiles, (file) => `\`${file}\``),
        labeledList('Checks', task.checks, (check) => `\`${check}\``),
        labeledList('Integration points', task.integrationPoints, (point) => point),
      ].join('\n\n'),
    )
    .join('\n\n');
  sections.push(section('## Tasks', tasks));
  return joinSections(sections);
}

function renderVerificationReport(draft: ArtifactDraftOf<'verification-report'>): string {
  const sections: string[] = [`# ${draft.title}`];
  sections.push(section('## Summary', draft.summary));
  sections.push(
    section(
      '## Results',
      table(
        ['Criterion', 'Status', 'Evidence'],
        draft.results.map((result) => [
          result.criterionId ? `${result.criterionId} — ${result.criterion}` : result.criterion,
          result.status,
          result.evidence,
        ]),
      ),
    ),
  );
  sections.push(section('## Overall', `**${draft.overallPass ? 'pass' : 'fail'}**`));
  if (draft.notes.length > 0) {
    sections.push(section('## Notes', draft.notes));
  }
  return joinSections(sections);
}

function renderDeliveryReport(draft: ArtifactDraftOf<'delivery-report'>): string {
  const sections: string[] = [`# ${draft.title}`];
  sections.push(section('## Summary', draft.summary));
  const trace = draft.trace
    .map((entry) =>
      [
        `### ${entry.requirementId || 'Requirement'}`,
        entry.requirement,
        labeledList('Design decisions', entry.designDecisions, (decision) => decision),
        labeledList('Changes', entry.changes, (change) => change),
        labeledList('Evidence', entry.evidence, (item) => item),
      ].join('\n\n'),
    )
    .join('\n\n');
  sections.push(section('## Trace', trace));
  sections.push(section('## Next steps', bullets(draft.nextSteps)));
  sections.push(section('## Held actions', bullets(draft.heldActions)));
  return joinSections(sections);
}

function renderDecisionLog(draft: ArtifactDraftOf<'decision-log'>): string {
  const sections: string[] = [`# ${draft.title}`];
  const entries = draft.entries
    .map((entry, index) => {
      const lines = [
        `- **Mode:** ${entry.mode}`,
        `- **Rationale:** ${entry.rationale}`,
      ];
      if (entry.alternatives.length > 0) {
        lines.push(`- **Alternatives:** ${entry.alternatives}`);
      }
      return [`### ${index + 1}. ${entry.decision}`, lines.join('\n')].join('\n\n');
    })
    .join('\n\n');
  sections.push(section('## Entries', entries));
  return joinSections(sections);
}

function joinSections(sections: readonly string[]): string {
  return `${sections.join('\n\n')}\n`;
}

/**
 * Render a draft payload as a deterministic Markdown body. The draft is
 * validated first; invalid drafts throw {@link InvalidArtifactDraftError} with
 * the full issue list.
 */
export function renderArtifactMarkdown(kind: ArtifactKind, draft: unknown): string {
  switch (kind) {
    case 'opportunity-brief':
      return renderOpportunityBrief(requireArtifactDraft('opportunity-brief', draft));
    case 'prd':
      return renderPrd(requireArtifactDraft('prd', draft));
    case 'trd':
      return renderTrd(requireArtifactDraft('trd', draft));
    case 'execution-plan':
      return renderExecutionPlan(requireArtifactDraft('execution-plan', draft));
    case 'verification-report':
      return renderVerificationReport(requireArtifactDraft('verification-report', draft));
    case 'delivery-report':
      return renderDeliveryReport(requireArtifactDraft('delivery-report', draft));
    case 'decision-log':
      return renderDecisionLog(requireArtifactDraft('decision-log', draft));
  }
}
