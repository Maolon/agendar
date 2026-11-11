import { describe, expect, it } from "vitest";
import { createClient } from "../api/client";
import { createFixtureTransport } from "../dev/fixtureTransport";
import { absentApprovalRevision } from "../state/hash";

/**
 * The dev fixture transport is what the browser preview demos with, so its
 * answers to the delivery commands are tested like any other client surface.
 * It is compiled out of production (`no-fixtures-in-build.test.ts`).
 */
async function fixtureClient() {
  return createClient(createFixtureTransport(), "fixture");
}

describe("dev fixture transport: a project with live work and a stopped step", () => {
  it("serves Meridian as active with one stopped step", async () => {
    const client = await fixtureClient();
    const snapshot = await client.getSnapshot();
    const meridian = snapshot.state.projects["meridian"]!;
    expect(meridian.summary.variance).toBe("active");
    expect(meridian.summary.stopped_steps).toBe(1);
    const steps = Object.values(meridian.features);
    expect(
      steps.filter((step) => step.progress === "doing" && step.liveness === "lost"),
    ).toHaveLength(1);
    expect(
      steps.filter((step) => step.progress === "doing" && step.liveness === "live"),
    ).toHaveLength(1);
    // No other fixture project carries the count.
    expect(
      Object.values(snapshot.state.projects).filter((project) => project.summary.stopped_steps),
    ).toHaveLength(1);
  });
});

describe("dev fixture transport: delivery views", () => {
  it("serves a review need, its demos and a report", async () => {
    const client = await fixtureClient();
    const snapshot = await client.getSnapshot();
    const need = snapshot.needs.find((item) => item.kind === "review");
    expect(need).toBeDefined();
    const delivery = snapshot.state.projects[need!.project]?.deliveries[need!.source];
    expect(delivery).toBeDefined();
    expect(delivery!.criteria.map((entry) => entry.provenance)).toEqual([
      "seen",
      "agent_says",
      "not_proven_yet",
    ]);

    const transcript = await client.readReport({
      project: "bookfinder",
      blob: { host: "h", sha256: "81".repeat(32), ext: "txt", bytes: 1 },
    });
    expect(transcript.text).toContain("$ shelf search");

    const screenshot = await client.readReport({
      project: "bookfinder",
      blob: { host: "h", sha256: "82".repeat(32), ext: "png", bytes: 1 },
    });
    expect(screenshot.media_token).toBe("82".repeat(32));
  });

  it("sends the three-demo delivery back with comments only, never accepts with comments", async () => {
    const client = await fixtureClient();
    const snapshot = await client.getSnapshot();
    const need = snapshot.needs.find((item) => item.question === "Review receipt-free delivery")!;
    const delivery = snapshot.state.projects[need.project]!.deliveries[need.source]!;
    expect(delivery.demos.map((demo) => demo.kind)).toEqual(["diff", "cli_transcript", "screenshot"]);
    const comments = [
      { anchor: { kind: "region" as const, demo: 2, x: 0, y: 0, w: 1000, h: 1000 }, text: "Too dark" },
    ];
    await expect(
      client.reviewDelivery({
        request_id: "01HZZZZZZZZZZZZZZZZZZZZZZ1",
        delivery_id: delivery.id,
        expected_delivery_revision: delivery.revision,
        verdict: "accept",
        comments,
      }),
    ).rejects.toThrow(/only allowed when sending/);
    const sent = await client.reviewDelivery({
      request_id: "01HZZZZZZZZZZZZZZZZZZZZZZ2",
      delivery_id: delivery.id,
      expected_delivery_revision: delivery.revision,
      verdict: "send_back",
      comments,
    });
    expect(sent.event_id).toBeTruthy();
  });

  it("observes a criterion only after the profile was approved", async () => {
    const client = await fixtureClient();
    const snapshot = await client.getSnapshot();
    const need = snapshot.needs.find((item) => item.kind === "review")!;
    const delivery = snapshot.state.projects[need.project]!.deliveries[need.source]!;

    const profile = { argv: ["agendar", "check-probe"], cwd: "repos/bookfinder", timeout_s: 30 };
    const approved = await client.approveCheck({
      request_id: need.id.slice(0, 26).toUpperCase(),
      expected_approval_revision: await absentApprovalRevision(profile),
      profile: { ...profile },
      consent: true,
    });
    const run = await client.runCheck({
      request_id: need.id.slice(0, 26).toUpperCase(),
      project: need.project,
      delivery_id: delivery.id,
      criterion: "alias",
      approval_id: approved.approval_id,
      expected_delivery_revision: delivery.revision,
      expected_approval_revision: approved.approval_id,
    });
    expect(run.result).toBe("passed");

    const after = await client.getSnapshot();
    const updated = after.state.projects[need.project]!.deliveries[delivery.id]!;
    expect(updated.criteria[0]!.provenance).toBe("seen");
    expect(updated.criteria[0]!.observation).toBe(run.observation_id);
  });

  it("refuses a placeholder approval revision", async () => {
    const client = await fixtureClient();
    const snapshot = await client.getSnapshot();
    const need = snapshot.needs.find((item) => item.kind === "review")!;
    // The native command parses this field as a SHA-256; an empty token is not
    // a precondition and must never reach a real run.
    await expect(
      client.approveCheck({
        request_id: need.id.slice(0, 26).toUpperCase(),
        expected_approval_revision: "",
        profile: { argv: ["agendar", "check-probe"], cwd: "repos/bookfinder", timeout_s: 30 },
        consent: true,
      }),
    ).rejects.toThrow(/SHA-256/);
  });

  it("accepts a review with the shown revision and appends a house rule", async () => {
    const client = await fixtureClient();
    const snapshot = await client.getSnapshot();
    const need = snapshot.needs.find((item) => item.kind === "review")!;
    const delivery = snapshot.state.projects[need.project]!.deliveries[need.source]!;

    await client.reviewDelivery({
      request_id: need.id.slice(0, 26).toUpperCase(),
      delivery_id: delivery.id,
      expected_delivery_revision: delivery.revision,
      verdict: "accept",
    });
    await client.setRule({
      request_id: need.id.slice(1, 27).toUpperCase(),
      project: need.project,
      expected_revision: "absent",
      rule: { rule_id: "keep-alias-list", text: "Keep the alias list complete", enabled: true },
    });

    const after = await client.getSnapshot();
    expect(after.needs.some((item) => item.id === need.id)).toBe(false);
    expect(
      after.state.projects[need.project]!.rules.some(
        (rule) => rule.value.rule_id === "keep-alias-list",
      ),
    ).toBe(true);
  });

  it("refuses a stale delivery revision", async () => {
    const client = await fixtureClient();
    const snapshot = await client.getSnapshot();
    const need = snapshot.needs.find((item) => item.kind === "review")!;
    const delivery = snapshot.state.projects[need.project]!.deliveries[need.source]!;
    await expect(
      client.reviewDelivery({
        request_id: need.id.slice(2, 28).toUpperCase(),
        delivery_id: delivery.id,
        expected_delivery_revision: "stale",
        verdict: "accept",
      }),
    ).rejects.toMatchObject({ code: "E_STALE_ANSWER" });
  });
});
