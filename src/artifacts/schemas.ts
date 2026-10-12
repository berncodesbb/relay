/**
 * Model-facing artifact draft schemas.
 *
 * Every artifact Relay persists starts life as a draft proposed by a model and
 * is therefore untrusted. These Zod schemas are the single validation gate
 * before any artifact is rendered or written: fields are present, bounded in
 * length, arrays are capped, identifiers are constrained to a safe uppercase
 * alphabet so they can never inject structure into Markdown headings, and
 * execution-plan file references must be workspace-relative.
 */

import { z } from 'zod';
import { type ArtifactKind, isArtifactKind } from '../core/artifacts.js';

/** Shared bounds. Titles are single-line so they stay renderable as headings. */
const titleSchema = z
  .string()
  .min(1)
  .max(200)
  .refine((value) => !/[\r\n]/.test(value), { message: 'title must be a single line' });

/** Requirement/criterion/task identifiers: uppercase, digits, dashes only. */
const identifierSchema = z
  .string()
  .min(1)
  .max(100)
  .regex(/^[A-Z0-9-]+$/, {
    message: 'identifier must contain only uppercase letters, digits, and dashes (e.g. "REQ-1")',
  });

/** Free-form bounded prose used where the kind has no more specific bound. */
const proseSchema = z.string().min(1).max(1000);

/** Identifier references (dependencies between tasks) reuse the safe alphabet. */
const identifierReferenceSchema = z
  .string()
  .min(1)
  .max(100)
  .regex(/^[A-Z0-9-]+$/, {
    message: 'dependency must be a task identifier (uppercase letters, digits, dashes)',
  });

/**
 * Workspace-relative file path: not absolute, no `..` segments, no `~`
 * expansion, no drive-letter prefixes. Forward or backward slashes are both
 * rejected as segment separators when they introduce traversal.
 */
const workspaceRelativePathSchema = z.string().min(1).max(500).superRefine((value, ctx) => {
  if (value.startsWith('/') || value.startsWith('\\')) {
    ctx.addIssue({
      code: 'custom',
      message: `affected file "${value}" must be workspace-relative, not absolute`,
    });
    return;
  }
  if (/^[a-zA-Z]:/.test(value)) {
    ctx.addIssue({
      code: 'custom',
      message: `affected file "${value}" looks like an absolute path with a drive letter`,
    });
    return;
  }
  if (value.startsWith('~')) {
    ctx.addIssue({
      code: 'custom',
      message: `affected file "${value}" must not reference the home directory`,
    });
    return;
  }
  const segments = value.split(/[\\/]/);
  if (segments.includes('..')) {
    ctx.addIssue({
      code: 'custom',
      message: `affected file "${value}" must not contain ".." segments`,
    });
  }
});

export const opportunityBriefSchema = z.object({
  title: titleSchema,
  problem: z.string().min(1).max(4000),
  targetUsers: z
    .array(z.object({ name: z.string().min(1).max(120), description: z.string().min(1).max(1000) }))
    .min(1)
    .max(12),
  evidence: z
    .array(z.object({ description: z.string().min(1).max(1000), source: z.string().max(500) }))
    .max(20),
  desiredOutcomes: z.array(z.string().min(1).max(500)).min(1).max(12),
  assumptions: z.array(proseSchema).max(20),
  constraints: z.array(proseSchema).max(20),
  openQuestions: z.array(proseSchema).max(20),
});

export const prdSchema = z.object({
  title: titleSchema,
  summary: z.string().min(1).max(4000),
  requirements: z
    .array(
      z.object({
        id: identifierSchema,
        statement: z.string().min(1).max(1000),
        rationale: z.string().min(1).max(1000),
      }),
    )
    .min(1)
    .max(40),
  exclusions: z.array(z.string().min(1).max(500)).max(30),
  acceptanceCriteria: z
    .array(
      z.object({
        id: identifierSchema,
        given: z.string().min(1).max(500),
        when: z.string().min(1).max(500),
        then: z.string().min(1).max(500),
      }),
    )
    .min(1)
    .max(40),
});

export const trdSchema = z.object({
  title: titleSchema,
  summary: z.string().min(1).max(4000),
  repositoryFindings: z
    .array(z.object({ area: z.string().min(1).max(200), finding: z.string().min(1).max(1000) }))
    .max(30),
  architecture: z.string().min(1).max(8000),
  interfaces: z
    .array(z.object({ name: z.string().min(1).max(200), description: z.string().min(1).max(1000) }))
    .max(30),
  migrations: z
    .array(z.object({ description: z.string().min(1).max(1000), risk: z.string().min(1).max(500) }))
    .max(20),
  risks: z
    .array(z.object({ risk: z.string().min(1).max(500), mitigation: z.string().min(1).max(1000) }))
    .max(20),
  rolloutPlan: z.string().min(1).max(4000),
  rollbackPlan: z.string().min(1).max(2000),
});

export const executionPlanSchema = z.object({
  title: titleSchema,
  summary: z.string().min(1).max(4000),
  tasks: z
    .array(
      z.object({
        id: identifierSchema,
        title: z.string().min(1).max(200),
        description: z.string().min(1).max(2000),
        dependencies: z.array(identifierReferenceSchema).max(10),
        affectedFiles: z.array(workspaceRelativePathSchema).max(50),
        checks: z.array(z.string().min(1).max(300)).max(10),
        integrationPoints: z.array(z.string().min(1).max(300)).max(10),
      }),
    )
    .min(1)
    .max(50),
});

export const verificationReportSchema = z.object({
  title: titleSchema,
  summary: z.string().min(1).max(4000),
  results: z
    .array(
      z.object({
        criterionId: z.string().max(100),
        criterion: z.string().min(1).max(1000),
        status: z.enum(['pass', 'fail', 'unevaluated']),
        evidence: z.string().min(1).max(2000),
      }),
    )
    .min(1)
    .max(60),
  overallPass: z.boolean(),
  notes: z.string().max(4000),
});

export const deliveryReportSchema = z.object({
  title: titleSchema,
  summary: z.string().min(1).max(4000),
  trace: z
    .array(
      z.object({
        requirementId: z.string().max(100),
        requirement: z.string().min(1).max(1000),
        designDecisions: z.array(z.string().min(1).max(1000)).max(10),
        changes: z.array(z.string().min(1).max(1000)).max(30),
        evidence: z.array(z.string().min(1).max(1000)).max(20),
      }),
    )
    .min(1)
    .max(60),
  nextSteps: z.array(z.string().min(1).max(500)).max(20),
  /** Actions intentionally NOT performed (commit, push, deploy, publish...). */
  heldActions: z.array(z.string().min(1).max(300)).min(1).max(20),
});

export const decisionLogSchema = z.object({
  title: titleSchema,
  entries: z
    .array(
      z.object({
        decision: z.string().min(1).max(500),
        rationale: z.string().min(1).max(2000),
        alternatives: z.string().max(1000),
        mode: z.string().min(1).max(40),
      }),
    )
    .min(1)
    .max(100),
});

/** Draft payload schema per artifact kind. */
export const ARTIFACT_DRAFT_SCHEMAS = {
  'opportunity-brief': opportunityBriefSchema,
  'decision-log': decisionLogSchema,
  prd: prdSchema,
  trd: trdSchema,
  'execution-plan': executionPlanSchema,
  'verification-report': verificationReportSchema,
  'delivery-report': deliveryReportSchema,
} as const;

export type OpportunityBriefDraft = z.infer<typeof opportunityBriefSchema>;
export type PrdDraft = z.infer<typeof prdSchema>;
export type TrdDraft = z.infer<typeof trdSchema>;
export type ExecutionPlanDraft = z.infer<typeof executionPlanSchema>;
export type VerificationReportDraft = z.infer<typeof verificationReportSchema>;
export type DeliveryReportDraft = z.infer<typeof deliveryReportSchema>;
export type DecisionLogDraft = z.infer<typeof decisionLogSchema>;

/** The draft payload type for a given artifact kind. */
export type ArtifactDraftOf<K extends ArtifactKind> = z.infer<(typeof ARTIFACT_DRAFT_SCHEMAS)[K]>;

export class UnknownArtifactKindError extends Error {
  constructor(
    readonly kind: string,
    readonly detail: string,
  ) {
    super(`Unknown artifact kind "${kind}": ${detail}`);
    this.name = 'UnknownArtifactKindError';
  }
}

/** Thrown when a draft fails schema validation; `issues` are human-readable. */
export class InvalidArtifactDraftError extends Error {
  constructor(
    readonly kind: ArtifactKind,
    readonly issues: readonly string[],
  ) {
    super(`Invalid ${kind} draft: ${issues.join('; ')}`);
    this.name = 'InvalidArtifactDraftError';
  }
}

/** Resolve the draft schema for `kind`. Throws for unknown kinds. */
export function artifactDraftSchema(kind: ArtifactKind): z.ZodType<ArtifactDraftOf<ArtifactKind>> {
  if (!isArtifactKind(kind)) {
    throw new UnknownArtifactKindError(
      String(kind),
      'expected one of: ' + (Object.keys(ARTIFACT_DRAFT_SCHEMAS) as ArtifactKind[]).join(', '),
    );
  }
  return ARTIFACT_DRAFT_SCHEMAS[kind] as unknown as z.ZodType<ArtifactDraftOf<ArtifactKind>>;
}

export type ParseArtifactDraftSuccess<K extends ArtifactKind> = {
  readonly ok: true;
  readonly kind: K;
  readonly draft: ArtifactDraftOf<K>;
};

export type ParseArtifactDraftFailure = {
  readonly ok: false;
  readonly kind: ArtifactKind;
  readonly issues: readonly string[];
};

export type ParseArtifactDraftResult<K extends ArtifactKind> =
  | ParseArtifactDraftSuccess<K>
  | ParseArtifactDraftFailure;

/** Format Zod issues as `path: message` strings with a root fallback. */
export function formatZodIssues(error: z.ZodError): readonly string[] {
  return error.issues.map((issue) => `${issue.path.join('.') || '(root)'}: ${issue.message}`);
}

/**
 * Validate an untrusted draft payload against the schema for `kind` without
 * throwing. Failures surface as human-readable issue strings.
 */
export function parseArtifactDraft<K extends ArtifactKind>(
  kind: K,
  value: unknown,
): ParseArtifactDraftResult<K> {
  if (!isArtifactKind(kind)) {
    throw new UnknownArtifactKindError(
      String(kind),
      'expected one of: ' + (Object.keys(ARTIFACT_DRAFT_SCHEMAS) as ArtifactKind[]).join(', '),
    );
  }
  const schema = ARTIFACT_DRAFT_SCHEMAS[kind];
  const parsed = schema.safeParse(value);
  if (!parsed.success) {
    return { ok: false, kind, issues: formatZodIssues(parsed.error) };
  }
  return { ok: true, kind, draft: parsed.data as ArtifactDraftOf<K> };
}

/** Validate a draft and throw {@link InvalidArtifactDraftError} on failure. */
export function requireArtifactDraft<K extends ArtifactKind>(
  kind: K,
  value: unknown,
): ArtifactDraftOf<K> {
  const result = parseArtifactDraft(kind, value);
  if (!result.ok) {
    throw new InvalidArtifactDraftError(kind, [...result.issues]);
  }
  return result.draft;
}
