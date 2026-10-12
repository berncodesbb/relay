/**
 * Task context packet construction.
 *
 * Each workflow step receives a compact, bounded context packet — never the
 * full historical conversation. The packet combines the goal, the current mode
 * contract, upstream artifact contents, and a few redacted repository facts
 * gathered directly through the tool registry (read capability is permitted in
 * every mode). Verification gets a deliberately independent packet: approved
 * acceptance criteria, plan checks, the resulting diff, and decision-log test
 * evidence — never the implementation conversation.
 */

import { type ArtifactKind } from '../core/artifacts.js';
import { MODE_CONTRACTS, type WorkflowMode } from '../core/modes.js';
import type { ProjectState } from '../core/project-state.js';
import type { ArtifactMarkdownStore } from '../artifacts/markdown-store.js';
import type { ToolRegistry } from '../tools/registry.js';

export type ModeContextInput = {
  readonly state: ProjectState;
  readonly goal: string;
  readonly workspaceRoot: string;
  readonly artifacts: ArtifactMarkdownStore;
  readonly tools: ToolRegistry;
  readonly pendingUserNote?: string | undefined;
};

/** Hard ceiling for the assembled packet; the head is kept. */
const PACKET_MAX_CHARS = 12_000;
/** Per-artifact content ceiling. */
const ARTIFACT_MAX_CHARS = 4_000;
/** Repository instruction head. */
const INSTRUCTIONS_MAX_CHARS = 2_000;
/** Diff ceiling for verification. */
const DIFF_MAX_CHARS = 6_000;
/** Ceiling for the combined repo-facts block. */
const REPO_FACTS_MAX_CHARS = 2_500;

const TRUNCATION_MARK = '… [truncated]';

function head(text: string, maxChars: number): string {
  const clean = text.trim();
  if (clean.length <= maxChars) return clean;
  return `${clean.slice(0, Math.max(0, maxChars - TRUNCATION_MARK.length))}${TRUNCATION_MARK}`;
}

/** Execute a read tool directly through the registry; failures degrade to notes. */
async function runTool(
  tools: ToolRegistry,
  workspaceRoot: string,
  name: string,
  args: Record<string, unknown>,
): Promise<string> {
  if (!tools.has(name)) return `(${name} unavailable: not registered)`;
  try {
    const tool = tools.get(name);
    const parsed = tool.argsSchema.safeParse(args);
    if (!parsed.success) {
      return `(${name} unavailable: invalid arguments)`;
    }
    const controller = new AbortController();
    const result = await tool.execute(parsed.data, {
      workspaceRoot,
      signal: controller.signal,
      log: () => undefined,
    });
    if (!result.ok) return `(${name} unavailable: ${head(result.output, 300)})`;
    return result.output;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return `(${name} unavailable: ${head(message, 300)})`;
  }
}

type ArtifactExcerpt = {
  readonly kind: ArtifactKind;
  readonly version: number;
  readonly status: string;
  readonly body: string;
};

async function readArtifactExcerpt(
  input: ModeContextInput,
  kind: ArtifactKind,
): Promise<ArtifactExcerpt | null> {
  const artifactId = input.state.artifactsByKind[kind];
  if (artifactId === undefined) return null;
  const artifact = input.state.artifacts[artifactId];
  if (artifact === undefined) return null;
  try {
    const file = artifact.path !== null ? await input.artifacts.read(artifact.path) : null;
    if (file === null) return null;
    return {
      kind,
      version: artifact.currentVersion,
      status: artifact.status,
      body: head(file.body, ARTIFACT_MAX_CHARS),
    };
  } catch {
    return null;
  }
}

async function artifactSection(
  input: ModeContextInput,
  kinds: readonly ArtifactKind[],
): Promise<string> {
  const parts: string[] = [];
  for (const kind of kinds) {
    const excerpt = await readArtifactExcerpt(input, kind);
    if (excerpt === null) {
      parts.push(`### ${kind}\n(not available yet)`);
      continue;
    }
    parts.push(
      [
        `### ${excerpt.kind} (v${excerpt.version}, ${excerpt.status})`,
        excerpt.body,
      ].join('\n\n'),
    );
  }
  return parts.join('\n\n');
}

async function repositoryFacts(input: ModeContextInput): Promise<string> {
  const instructions = await runTool(input.tools, input.workspaceRoot, 'repo-instructions', {});
  const status = await runTool(input.tools, input.workspaceRoot, 'git-status', {});
  const listing = await runTool(input.tools, input.workspaceRoot, 'list-dir', { path: '.', depth: 2 });
  const block = [
    '### Repository instructions (head)',
    head(instructions, INSTRUCTIONS_MAX_CHARS),
    '### Git status',
    head(status, 800),
    '### Top-level layout',
    head(listing, 1200),
  ].join('\n\n');
  return head(block, REPO_FACTS_MAX_CHARS);
}

function headerBlock(input: ModeContextInput, mode: WorkflowMode): string {
  const contract = MODE_CONTRACTS[mode];
  return [
    `# Relay task context — ${mode} (${contract.title})`,
    `Goal: ${head(input.goal, 500)}`,
    `Mode contract: ${contract.description}`,
    `Completion evidence: ${contract.completionEvidence}`,
    `Autonomy: ${input.state.autonomy}`,
  ].join('\n');
}

function noteBlock(note: string): string {
  return `## User note\n${head(note, 1000)}`;
}

const OUTPUT_CONTRACT_LINE =
  'Respond ONLY with a JSON object matching the provided schema for this mode\'s artifact draft.';

/** Kinds whose contents the given mode consumes as upstream inputs. */
function upstreamKinds(mode: WorkflowMode): readonly ArtifactKind[] {
  return MODE_CONTRACTS[mode].requiredInputs;
}

/**
 * Build the bounded context packet for the state's current mode. Verification
 * and delivery assemble specialized packets; every other mode gets the
 * contract header, upstream artifacts, and repository facts.
 */
export async function buildModeContext(input: ModeContextInput): Promise<string> {
  const mode = input.state.currentMode;
  const sections: string[] = [headerBlock(input, mode)];
  const note = input.pendingUserNote ?? null;
  if (note !== null && note.trim().length > 0) sections.push(noteBlock(note.trim()));

  if (mode === 'verification') {
    // Independent review packet: approved criteria, plan checks, diff, and
    // decision-log test evidence only. No prior conversation, no narrative.
    sections.push('## Approved acceptance criteria (from the PRD)');
    const criteria = await readArtifactExcerpt(input, 'prd');
    sections.push(criteria === null ? '(PRD not available)' : criteria.body);
    sections.push('## Execution plan (tasks and checks)');
    const plan = await readArtifactExcerpt(input, 'execution-plan');
    sections.push(plan === null ? '(execution plan not available)' : plan.body);
    sections.push('## Implementation decision log (test evidence)');
    const log = await readArtifactExcerpt(input, 'decision-log');
    sections.push(log === null ? '(no decision log recorded)' : log.body);
    const diff = await runTool(input.tools, input.workspaceRoot, 'git-diff', {});
    sections.push(`## Resulting diff\n${head(diff, DIFF_MAX_CHARS)}`);
    sections.push(
      [
        '## Review stance',
        'You are verifying work you did not write. Judge each acceptance criterion strictly against the diff and test evidence above. Report overallPass=false when any criterion is not satisfied.',
      ].join('\n'),
    );
  } else if (mode === 'delivery') {
    sections.push('## Delivery trace ingredients');
    sections.push('### Goal');
    sections.push(head(input.goal, 1000));
    const ingredients = await artifactSection(input, [
      'prd',
      'trd',
      'execution-plan',
      'verification-report',
    ]);
    sections.push(ingredients);
    const status = await runTool(input.tools, input.workspaceRoot, 'git-status', {});
    sections.push(`### Changed files (git status)\n${head(status, 1000)}`);
  } else {
    if (upstreamKinds(mode).length > 0) {
      const upstream = await artifactSection(input, upstreamKinds(mode));
      if (upstream.length > 0) sections.push(`## Upstream inputs\n${upstream}`);
    }
    sections.push(`## Repository facts\n${await repositoryFacts(input)}`);
  }

  sections.push(OUTPUT_CONTRACT_LINE);
  const packet = sections.join('\n\n');
  if (packet.length <= PACKET_MAX_CHARS) return packet;
  return `${packet.slice(0, PACKET_MAX_CHARS - TRUNCATION_MARK.length)}${TRUNCATION_MARK}`;
}
