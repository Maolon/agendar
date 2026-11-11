import { describe, expect, it } from "vitest";
import { expectedFields, featureFields, featurePatch, keepEdit } from "../state/agenda";

describe("Keep on an untouched step", () => {
  it("patches the step's own priority, guarded by exactly that field's revision", () => {
    const edit = keepEdit({ priority: 2 });
    expect(featurePatch(edit)).toEqual({ priority: 2 });
    expect(featureFields(edit)).toEqual(["priority"]);
    const revisions = { priority: "p-rev", title: "t-rev" };
    expect(expectedFields(revisions, featureFields(edit))).toEqual({ priority: "p-rev" });
  });
});
