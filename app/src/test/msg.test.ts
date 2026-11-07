import { afterEach, describe, expect, it } from "vitest";
import { render } from "@testing-library/react";
import { createElement } from "react";
import { I18nProvider } from "../i18n";
import { needReason, renderMsg, type Msg } from "../i18n/msg";

function inLanguage(language: string): void {
  render(createElement(I18nProvider, { preference: language, children: null }));
}

afterEach(() => inLanguage("en"));

describe("core messages", () => {
  const burst: Msg = {
    key: "act.burst_in",
    args: {
      actor: { t: "msg", v: { key: "actor.you" } },
      milestone: { t: "text", v: "MVP" },
      parts: {
        t: "list",
        v: [
          { key: "act.part.finished", args: { n: { t: "count", v: 2 } } },
          { key: "act.part.started", args: { n: { t: "count", v: 1 } } },
        ],
      },
    },
  };

  it("renders English the way the core writes it", () => {
    inLanguage("en");
    expect(renderMsg(burst, "fallback")).toBe("You finished 2 steps, started 1 step in MVP");
    expect(
      needReason({
        reason: "x",
        reason_msg: { key: "need.reason.agents", args: { n: { t: "count", v: 1 }, age: { t: "minutes", v: 38 } } },
      }),
    ).toBe("1 agent waiting · 38m");
  });

  it("renders the chosen language, with plurals, nested messages and lists", () => {
    inLanguage("zh-Hans");
    expect(renderMsg(burst, "fallback")).toBe("你在 MVP 中完成了 2 个步骤，开始了 1 个步骤");
    expect(renderMsg({ key: "need.reason.review", args: { age: { t: "minutes", v: 90 } } }, "x")).toBe(
      "待审核 · 1小时",
    );
    expect(
      renderMsg(
        { key: "act.started", args: { actor: { t: "text", v: "Claude Code" }, step: { t: "text", v: "接线" } } },
        "x",
      ),
    ).toBe("Claude Code 开始了接线");
    inLanguage("zh-Hant");
    expect(renderMsg({ key: "brief.late", args: { title: { t: "text", v: "Wire" } } }, "x")).toBe("Wire 已延誤");
  });

  it("falls back to the core's text for an unknown key or a missing message", () => {
    inLanguage("zh-Hans");
    expect(renderMsg({ key: "not.a.key" }, "Plain English")).toBe("Plain English");
    expect(renderMsg(undefined, "Plain English")).toBe("Plain English");
  });
});
