/**
 * Fake-provider script driving the FULL Relay workflow against the
 * `test/fixtures/sample-repo` fixture repository.
 *
 * Discovery through planning produce schema-valid drafts grounded in the
 * fixture; implementation actually patches `src/greet.js` via the apply-patch
 * tool and then reports a decision log; verification runs the repo's real
 * check (`node test/run.js`) through the run-command tool and cites it;
 * delivery assembles the trace. Exported as step lists so tests can inject
 * failures (retry exhaustion, invalid drafts) at specific modes.
 */

import { FakeProvider, type FakeStep } from '../../src/providers/fake.js';

export const SAMPLE_GOAL = 'Fix sample-repo so greet("World") returns "Hello, World!"';

/** Unified-diff patch fixing the fixture bug in src/greet.js. */
const GREET_PATCH = [
  '--- a/src/greet.js',
  '+++ b/src/greet.js',
  '@@ -3,3 +3,3 @@',
  ' function greet(name) {',
  "-  return `Hey, ${name}.`;",
  "+  return `Hello, ${name}!`;",
  ' }',
].join('\n');

export function sampleWorkflowSteps(): FakeStep[] {
  return [
    {
      match: { purpose: 'intent' },
      response: {
        structuredOutput: {
          kind: 'advance',
          confidence: 0.95,
          rationale: 'scripted: continue the workflow',
        },
      },
    },
    {
      match: { purpose: 'mode:discovery' },
      response: {
        structuredOutput: {
          title: 'Opportunity brief — fix the sample-repo greeting',
          problem:
            'sample-repo ships greet() which returns "Hey, World." while test/run.js asserts greet("World") === "Hello, World!"; the check fails on the current sources.',
          targetUsers: [
            { name: 'sample-repo maintainer', description: 'Runs npm test and expects the greeting check to pass.' },
          ],
          evidence: [
            { description: 'test/run.js asserts greet("World") === "Hello, World!"', source: 'test/run.js' },
            { description: 'src/greet.js returns `Hey, ${name}.`', source: 'src/greet.js' },
          ],
          desiredOutcomes: ['node test/run.js exits 0.'],
          assumptions: ['Only the greeting string needs to change.'],
          constraints: ['No new dependencies; keep the module interface unchanged.'],
          openQuestions: [],
        },
      },
    },
    {
      match: { purpose: 'mode:specification' },
      response: {
        structuredOutput: {
          title: 'PRD — correct sample-repo greeting',
          summary: 'Specify the greeting behavior so the existing check passes.',
          requirements: [
            {
              id: 'REQ-1',
              statement: 'greet("World") must return exactly "Hello, World!".',
              rationale: 'The repository check asserts this exact string.',
            },
          ],
          exclusions: ['Changing the test.', 'Adding CLI features.'],
          acceptanceCriteria: [
            {
              id: 'AC-1',
              given: 'src/greet.js is patched per the execution plan',
              when: 'node test/run.js runs',
              then: 'it prints "all checks passed" and exits 0',
            },
          ],
        },
      },
    },
    {
      match: { purpose: 'mode:technical-design', maxCall: 1 },
      response: {
        toolCalls: [{ name: 'read-file', args: { path: 'src/greet.js' } }],
        finishReason: 'tool-calls',
      },
    },
    {
      match: { purpose: 'mode:technical-design', minCall: 2 },
      response: {
        structuredOutput: {
          title: 'TRD — one-line fix in the greeting module',
          summary: 'Change the template literal returned by greet() in src/greet.js.',
          repositoryFindings: [
            { area: 'src/greet.js', finding: 'greet() returns `Hey, ${name}.`; single export, no other callers.' },
            { area: 'test/run.js', finding: 'Asserts greet("World") === "Hello, World!" via node:assert.' },
          ],
          architecture: 'Keep the module shape; replace the returned template literal with `Hello, ${name}!`.',
          interfaces: [{ name: 'greet(name)', description: 'Unchanged signature; returns the corrected greeting.' }],
          migrations: [],
          risks: [{ risk: 'Other callers expecting the old string.', mitigation: 'Repository search found no other callers.' }],
          rolloutPlan: 'Apply the patch; run node test/run.js.',
          rollbackPlan: 'Revert the one-line change in src/greet.js.',
        },
      },
    },
    {
      match: { purpose: 'mode:planning' },
      response: {
        structuredOutput: {
          title: 'Execution plan — patch the greeting',
          summary: 'One task: patch src/greet.js and run the repository check.',
          tasks: [
            {
              id: 'TASK-1',
              title: 'Fix the greeting string',
              description: 'Apply a unified diff changing the greet() return value to `Hello, ${name}!`.',
              dependencies: [],
              affectedFiles: ['src/greet.js'],
              checks: ['node test/run.js'],
              integrationPoints: [],
            },
          ],
        },
      },
    },
    {
      match: { purpose: 'mode:implementation', maxCall: 1 },
      response: {
        toolCalls: [{ name: 'apply-patch', args: { patch: GREET_PATCH } }],
        finishReason: 'tool-calls',
      },
    },
    {
      match: { purpose: 'mode:implementation', minCall: 2 },
      response: {
        structuredOutput: {
          title: 'Decision log — greeting fix',
          entries: [
            {
              decision: 'Patched src/greet.js to return `Hello, ${name}!`',
              rationale: 'Matches the acceptance criterion AC-1 and the assertion in test/run.js.',
              alternatives: 'Editing the test instead — rejected: the product behavior is the greeting.',
              mode: 'implementation',
            },
            {
              decision: 'Ran the plan check via run-command',
              rationale: 'DECISION-LOG-EVIDENCE: node test/run.js printed "all checks passed" and exited 0.',
              alternatives: '',
              mode: 'implementation',
            },
          ],
        },
      },
    },
    {
      match: { purpose: 'mode:verification', maxCall: 1 },
      response: {
        toolCalls: [{ name: 'run-command', args: { command: 'node', args: ['test/run.js'] } }],
        finishReason: 'tool-calls',
      },
    },
    {
      match: { purpose: 'mode:verification', minCall: 2 },
      response: {
        structuredOutput: {
          title: 'Verification report — greeting check',
          summary: 'The single approved criterion is satisfied by the diff and a fresh test run.',
          results: [
            {
              criterionId: 'AC-1',
              criterion: 'node test/run.js prints "all checks passed" and exits 0',
              status: 'pass',
              evidence: 'patch applied; see decision log — run-command executed node test/run.js with exit code 0',
            },
          ],
          overallPass: true,
          notes: 'Independent review against the diff and the test run evidence.',
        },
      },
    },
    {
      match: { purpose: 'mode:delivery' },
      response: {
        structuredOutput: {
          title: 'Delivery report — sample-repo greeting fix',
          summary: 'One requirement traced to one change and one passing verification.',
          trace: [
            {
              requirementId: 'REQ-1',
              requirement: 'greet("World") must return exactly "Hello, World!".',
              designDecisions: ['Replace the returned template literal in src/greet.js.'],
              changes: ['src/greet.js: greet() now returns `Hello, ${name}!`'],
              evidence: ['verification report AC-1: node test/run.js exited 0'],
            },
          ],
          nextSteps: ['Review the diff.', 'Commit when satisfied.'],
          heldActions: ['git commit', 'git push', 'pull request creation'],
        },
      },
    },
  ];
}

/**
 * The workflow fake provider for the sample repo: the full scripted pipeline,
 * or a custom step list for failure-injection tests.
 */
export function createWorkflowFakeProvider(steps: readonly FakeStep[] = sampleWorkflowSteps()): FakeProvider {
  return new FakeProvider([...steps]);
}
