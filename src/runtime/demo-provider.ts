/**
 * Scripted happy-path demo provider.
 *
 * A FakeProvider script that carries any goal through all seven workflow
 * modes with schema-valid artifact drafts. It demonstrates the PIPELINE, not
 * real model reasoning: every draft is clearly labeled "demo" and references
 * {@link DEMO_GOAL}. No files are written; the demo decision log states that
 * no changes were required and the demo verification reports
 * overallPass=true by construction so the delivery stage is reachable.
 */

import { FakeProvider } from '../providers/fake.js';

export const DEMO_GOAL = 'Demonstrate the Relay workflow end to end (fake provider)';

function briefDraft(): unknown {
  return {
    title: `Demo opportunity brief — ${DEMO_GOAL}`,
    problem:
      `The stated goal is: "${DEMO_GOAL}". This demo brief exists to show the discovery stage producing a schema-valid opportunity brief.`,
    targetUsers: [
      {
        name: 'Relay user',
        description: 'A developer evaluating the workflow pipeline with the deterministic fake provider.',
      },
    ],
    evidence: [
      {
        description: 'Demo run configured with the fake provider; no repository evidence is claimed.',
        source: 'demo',
      },
    ],
    desiredOutcomes: [
      'A complete set of workflow artifacts from discovery through delivery.',
      'A pause before delivery requiring explicit user authorization.',
    ],
    assumptions: ['The demo runs against a disposable demo workspace.'],
    constraints: ['No commits, pushes, or deployments may be performed.'],
    openQuestions: [],
  };
}

function prdDraft(): unknown {
  return {
    title: `Demo PRD — ${DEMO_GOAL}`,
    summary: `Demo requirements derived from the goal "${DEMO_GOAL}" for pipeline demonstration purposes.`,
    requirements: [
      {
        id: 'REQ-1',
        statement: 'The workflow shall produce one artifact per mode, versioned as Markdown.',
        rationale: 'Demonstrates artifact creation and approval checkpoints.',
      },
      {
        id: 'REQ-2',
        statement: 'The workflow shall pause before any delivery action.',
        rationale: 'Demonstrates the delivery hold policy.',
      },
    ],
    exclusions: ['Real model reasoning.', 'Changes to user files.'],
    acceptanceCriteria: [
      {
        id: 'AC-1',
        given: 'the demo workflow has run through verification',
        when: 'artifact storage is inspected',
        then: 'a Markdown artifact exists for every workflow mode',
      },
      {
        id: 'AC-2',
        given: 'the delivery report exists',
        when: 'the workflow attempts to finish',
        then: 'it stops and requires explicit user authorization for delivery actions',
      },
    ],
  };
}

function trdDraft(): unknown {
  return {
    title: `Demo TRD — ${DEMO_GOAL}`,
    summary: `Demo technical design for "${DEMO_GOAL}": the pipeline itself is the architecture under demonstration.`,
    repositoryFindings: [
      {
        area: 'demo',
        finding: 'Repository facts are not inspected by the demo script; this finding is a placeholder.',
      },
    ],
    architecture:
      'Demo architecture: the workflow controller drives seven modes over an append-only event log; providers are replaceable workers.',
    interfaces: [
      { name: 'WorkflowController', description: 'Drives mode steps and applies transition policy (demo description).' },
    ],
    migrations: [],
    risks: [
      {
        risk: 'Demo content may be mistaken for real analysis.',
        mitigation: 'Every demo artifact title and summary is labeled "demo".',
      },
    ],
    rolloutPlan: 'Demo rollout: run the workflow with the fake provider in a disposable workspace.',
    rollbackPlan: 'Delete the demo workspace; nothing outside it is touched.',
  };
}

function planDraft(): unknown {
  return {
    title: `Demo execution plan — ${DEMO_GOAL}`,
    summary: `Demo task graph for "${DEMO_GOAL}": one task per downstream mode, no user-file changes.`,
    tasks: [
      {
        id: 'TASK-1',
        title: 'Produce demo artifacts',
        description: 'Each mode emits its schema-valid demo draft for approval.',
        dependencies: [],
        affectedFiles: [],
        checks: ['demo check: artifact files exist under .relay/artifacts'],
        integrationPoints: [],
      },
    ],
  };
}

function decisionLogDraft(): unknown {
  return {
    title: `Demo decision log — ${DEMO_GOAL}`,
    entries: [
      {
        decision: 'No changes required (demo)',
        rationale:
          `The demo provider does not modify user files; the goal "${DEMO_GOAL}" is satisfied by the pipeline itself. No commands were run.`,
        alternatives: '',
        mode: 'implementation',
      },
    ],
  };
}

function verificationDraft(): unknown {
  return {
    title: `Demo verification report — ${DEMO_GOAL}`,
    summary: 'Demo verification: criteria are satisfied by construction of the scripted pipeline.',
    results: [
      {
        criterionId: 'AC-1',
        criterion: 'a Markdown artifact exists for every workflow mode',
        status: 'pass',
        evidence: 'demo run — criteria satisfied by construction',
      },
      {
        criterionId: 'AC-2',
        criterion: 'the workflow pauses before delivery actions',
        status: 'pass',
        evidence: 'demo run — criteria satisfied by construction',
      },
    ],
    overallPass: true,
    notes: 'Demo verification; no repository commands were executed.',
  };
}

function deliveryDraft(): unknown {
  return {
    title: `Demo delivery report — ${DEMO_GOAL}`,
    summary: `Demo delivery trace for "${DEMO_GOAL}" linking requirements to evidence; delivery actions are held.`,
    trace: [
      {
        requirementId: 'REQ-1',
        requirement: 'The workflow shall produce one artifact per mode, versioned as Markdown.',
        designDecisions: ['Artifacts are rendered deterministically and stored under .relay/artifacts (demo).'],
        changes: [],
        evidence: ['demo run — artifacts exist for every mode'],
      },
      {
        requirementId: 'REQ-2',
        requirement: 'The workflow shall pause before any delivery action.',
        designDecisions: ['Delivery always ends in a policy pause requiring user authorization (demo).'],
        changes: [],
        evidence: ['demo run — workflow paused with the delivery hold'],
      },
    ],
    nextSteps: ['Review the demo artifacts.', 'Delete the demo workspace when finished.'],
    heldActions: [
      'git commit',
      'git push',
      'pull request creation',
      'deployment',
      'publishing',
    ],
  };
}

/**
 * The scripted happy-path demo provider. Steps are keyed by purpose:
 * `intent` classifies any input as a goal (the controller initializes with the
 * raw text when freeText is absent), and `mode:<mode>` steps return
 * schema-valid drafts for each of the seven modes.
 */
export function createDemoFakeProvider(): FakeProvider {
  return new FakeProvider([
    {
      match: { purpose: 'intent' },
      response: {
        structuredOutput: {
          kind: 'provide-goal',
          confidence: 0.95,
          rationale: 'demo provider: treat the input as the product goal',
        },
      },
    },
    { match: { purpose: 'mode:discovery' }, response: { structuredOutput: briefDraft() } },
    { match: { purpose: 'mode:specification' }, response: { structuredOutput: prdDraft() } },
    { match: { purpose: 'mode:technical-design' }, response: { structuredOutput: trdDraft() } },
    { match: { purpose: 'mode:planning' }, response: { structuredOutput: planDraft() } },
    { match: { purpose: 'mode:implementation' }, response: { structuredOutput: decisionLogDraft() } },
    { match: { purpose: 'mode:verification' }, response: { structuredOutput: verificationDraft() } },
    { match: { purpose: 'mode:delivery' }, response: { structuredOutput: deliveryDraft() } },
  ]);
}
