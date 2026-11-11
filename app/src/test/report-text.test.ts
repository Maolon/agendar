import { describe, expect, it } from "vitest";
import { htmlToText } from "../state/reportText";

describe("htmlToText", () => {
  it("puts a blank line around headings and paragraphs", () => {
    const html = "<body><h1>Plan</h1><p>First   paragraph\n spread out.</p><h2>Next</h2><p>Second.</p></body>";
    expect(htmlToText(html)).toBe("Plan\n\nFirst paragraph spread out.\n\nNext\n\nSecond.");
  });

  it("bullets list items, including nested lists", () => {
    const html = "<ul><li>One</li><li>Two<ul><li>Two a</li></ul></li></ul><ol><li>Three</li></ol>";
    expect(htmlToText(html)).toBe("• One\n• Two\n• Two a\n• Three");
  });

  it("separates table cells with tabs, one row per line", () => {
    const html = "<table><thead><tr><th>Name</th><th>Score</th></tr></thead><tbody><tr><td>A</td><td>1</td></tr><tr><td>B</td><td>2</td></tr></tbody></table>";
    expect(htmlToText(html)).toBe("Name\tScore\nA\t1\nB\t2");
  });

  it("drops scripts, styles and other non-text content", () => {
    const html =
      "<p>Kept</p><script>alert('x')</script><style>p{}</style><noscript>no js</noscript>" +
      "<template><p>tpl</p></template><svg><text>chart</text></svg><p>Also kept</p>";
    const text = htmlToText(html);
    expect(text).toBe("Kept\n\nAlso kept");
    expect(text).not.toContain("alert");
  });

  it("decodes entities and keeps preformatted whitespace", () => {
    const html = "<p>Fish &amp; chips &lt;3 &eacute;t&eacute; &#x4E2D;</p><pre>a  b\n  c</pre>";
    expect(htmlToText(html)).toBe("Fish & chips <3 été 中\n\na  b\n  c");
  });

  it("leads with the title when the body has no h1, collapses blank runs and handles br/hr", () => {
    const html =
      "<html><head><title>Weekly audit</title></head><body><p>One<br>Two</p><div></div><div></div><hr><p>Three</p></body></html>";
    expect(htmlToText(html)).toBe("Weekly audit\n\nOne\nTwo\n\n———\n\nThree");
    expect(htmlToText("<title>T</title><h1>Own heading</h1>")).toBe("Own heading");
  });
});
