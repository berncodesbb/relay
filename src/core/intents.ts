/**
 * Intent contracts.
 *
 * Natural-language input is classified by a model into an {@link IntentProposal}
 * which is validated here before the deterministic controller applies it.
 * Low-confidence or ambiguous consequential intents must surface a focused
 * question instead of acting. Explicit slash commands bypass classification.
 */

import { z } from 'zod';
import { ARTIFACT_KINDS, type ArtifactKind } from './artifacts.js';
import { WORKFLOW_MODES, type AutonomyBound, type WorkflowMode } from './modes.js';

export const INTENT_KINDS = [
  'provide-goal',
  'advance',
  'revise-artifact',
  'change-mode',
  'set-autonomy',
  'answer-question',
  'request-status',
  'request-explanation',
  'inspect-artifact',
  'pause',
  'unknown',
] as const;

export type IntentKind = (typeof INTENT_KINDS)[number];

/** Intents that change workflow direction or touch approved artifacts. */
export const CONSEQUENTIAL_INTENT_KINDS: readonly IntentKind[] = [
  'revise-artifact',
  'change-mode',
  'set-autonomy',
];

const modeSchema = z.enum(WORKFLOW_MODES);
const artifactKindSchema = z.enum(ARTIFACT_KINDS);
const autonomySchema = z.string().refine(
  (value) =>
    value === 'off' || value === 'next-checkpoint' || value === 'workflow'
    || (value.startsWith('until:') && WORKFLOW_MODES.includes(value.slice('until:'.length) as WorkflowMode)),
  { message: 'autonomy must be off, next-checkpoint, workflow, or until:<mode>' },
);

/**
 * Schema applied to model-produced intent classifications. Everything the
 * controller reads from a model passes through this validation first.
 */
export const intentProposalSchema = z.object({
  kind: z.enum(INTENT_KINDS),
  confidence: z.number().min(0).max(1),
  rationale: z.string().min(1).max(2000),
  mode: modeSchema.nullish(),
  autonomy: autonomySchema.nullish(),
  artifactKind: artifactKindSchema.nullish(),
  /** Focused question to ask when the intent is ambiguous. */
  question: z.string().max(1000).nullish(),
  /** Cleaned-up restatement of user input to carry forward. */
  freeText: z.string().max(20000).nullish(),
});

export type IntentProposal = z.infer<typeof intentProposalSchema>;

export type IntentSource = 'natural-language' | 'command';

export type AppliedIntent = {
  readonly kind: IntentKind;
  readonly source: IntentSource;
  readonly confidence: number;
  readonly mode: WorkflowMode | null;
  readonly autonomy: AutonomyBound | null;
  readonly artifactKind: ArtifactKind | null;
  readonly overridden: boolean;
};

/** Confidence below this threshold requires a focused question before acting. */
export const INTENT_CONFIDENCE_FLOOR = 0.6;

export function isIntentAmbiguous(proposal: IntentProposal): boolean {
  if (proposal.kind === 'unknown') return true;
  if (proposal.confidence < INTENT_CONFIDENCE_FLOOR) return true;
  const consequential = CONSEQUENTIAL_INTENT_KINDS.includes(proposal.kind);
  if (consequential && proposal.confidence < 0.8) return true;
  return false;
}

/** Slash commands that map directly onto intents without model classification. */
export const SLASH_COMMANDS = [
  '/status',
  '/why',
  '/inspect',
  '/mode',
  '/auto',
  '/pause',
  '/resume',
  '/approve',
  '/revise',
] as const;

export type SlashCommand = (typeof SLASH_COMMANDS)[number];

export type ParsedCommand =
  | { readonly command: 'status' }
  | { readonly command: 'why' }
  | { readonly command: 'inspect'; readonly target: string }
  | { readonly command: 'mode'; readonly mode: WorkflowMode }
  | { readonly command: 'auto'; readonly bound: AutonomyBound }
  | { readonly command: 'pause'; readonly reason: string }
  | { readonly command: 'resume' }
  | { readonly command: 'approve'; readonly artifactId?: string | undefined }
  | { readonly command: 'revise'; readonly artifactKind: ArtifactKind; readonly reason: string };

export function parseSlashCommand(input: string): ParsedCommand | null {
  const trimmed = input.trim();
  if (!trimmed.startsWith('/')) return null;
  const [rawCommand, ...rest] = trimmed.split(/\s+/);
  const arg = rest.join(' ');
  switch (rawCommand) {
    case '/status':
      return arg ? null : { command: 'status' };
    case '/why':
      return arg ? null : { command: 'why' };
    case '/inspect':
      return arg ? { command: 'inspect', target: arg } : null;
    case '/mode': {
      if (!arg) return null;
      const mode = arg.trim();
      return WORKFLOW_MODES.includes(mode as WorkflowMode)
        ? { command: 'mode', mode: mode as WorkflowMode }
        : null;
    }
    case '/auto': {
      if (!arg) return { command: 'auto', bound: 'workflow' };
      const bound = arg.trim();
      if (
        bound === 'off' || bound === 'next-checkpoint' || bound === 'workflow'
        || (bound.startsWith('until:') && WORKFLOW_MODES.includes(bound.slice('until:'.length) as WorkflowMode))
      ) {
        return { command: 'auto', bound: bound as AutonomyBound };
      }
      return null;
    }
    case '/pause':
      return { command: 'pause', reason: arg || 'requested by user' };
    case '/resume':
      return arg ? null : { command: 'resume' };
    case '/approve':
      return { command: 'approve', artifactId: arg || undefined };
    case '/revise': {
      const [kindArg, ...reasonParts] = rest;
      if (!kindArg || !(ARTIFACT_KINDS as readonly string[]).includes(kindArg)) return null;
      return {
        command: 'revise',
        artifactKind: kindArg as ArtifactKind,
        reason: reasonParts.join(' ') || 'user requested revision',
      };
    }
    default:
      return null;
  }
}
