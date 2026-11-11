/**
 * Plain text from an HTML report, for reading in the app.
 *
 * The HTML is parsed with `DOMParser`, which builds an inert document: no
 * script runs and nothing is fetched. The document is only walked, never
 * inserted into the live DOM; the result is shown as escaped text.
 */

const DROPPED = new Set(["SCRIPT", "STYLE", "NOSCRIPT", "TEMPLATE", "SVG", "HEAD", "IFRAME", "OBJECT"]);
const BLOCKS = new Set([
  "P", "DIV", "SECTION", "ARTICLE", "HEADER", "FOOTER", "MAIN", "NAV", "ASIDE",
  "LI", "TR", "PRE", "BLOCKQUOTE", "TABLE", "UL", "OL", "DL", "DT", "DD",
  "FIGURE", "FIGCAPTION", "DETAILS", "SUMMARY", "THEAD", "TBODY", "TFOOT",
]);
const HEADINGS = new Set(["H1", "H2", "H3", "H4", "H5", "H6"]);

export function htmlToText(html: string): string {
  const doc = new DOMParser().parseFromString(html, "text/html");
  const out: string[] = [];
  const atLineStart = () => {
    const last = out[out.length - 1];
    return last === undefined || last.endsWith("\n");
  };
  const newline = () => {
    if (!atLineStart()) out.push("\n");
  };
  const blankLine = () => {
    newline();
    const joined = out.join("");
    if (joined && !joined.endsWith("\n\n")) out.push("\n");
  };

  function walk(node: Node, pre: boolean): void {
    if (node.nodeType === Node.TEXT_NODE) {
      const text = node.textContent ?? "";
      if (pre) {
        out.push(text);
        return;
      }
      const collapsed = text.replace(/\s+/g, " ");
      if (!collapsed.trim()) {
        if (collapsed && !atLineStart()) out.push(" ");
        return;
      }
      out.push(atLineStart() ? collapsed.replace(/^ /, "") : collapsed);
      return;
    }
    if (node.nodeType !== Node.ELEMENT_NODE) return;
    const element = node as Element;
    const tag = element.tagName.toUpperCase();
    if (DROPPED.has(tag)) return;
    if (tag === "BR") {
      out.push("\n");
      return;
    }
    if (tag === "HR") {
      newline();
      out.push("———\n");
      return;
    }
    const heading = HEADINGS.has(tag);
    const block = heading || BLOCKS.has(tag);
    // Paragraph-like blocks stand apart, as they would on the page.
    const spaced = tag === "P" || tag === "PRE" || tag === "BLOCKQUOTE" || tag === "TABLE";
    if (heading || spaced) blankLine();
    else if (block) newline();
    if (tag === "LI") out.push("• ");
    if ((tag === "TD" || tag === "TH") && element.previousElementSibling) out.push("\t");
    const inPre = pre || tag === "PRE";
    for (const child of [...element.childNodes]) walk(child, inPre);
    if (spaced) blankLine();
    else if (block) newline();
  }

  const body = doc.body;
  if (body) walk(body, false);
  let text = out
    .join("")
    .split("\n")
    .map((line) => line.replace(/[ ]+$/, ""))
    .join("\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
  const title = doc.title.trim();
  if (title && !body?.querySelector("h1")) text = text ? `${title}\n\n${text}` : title;
  return text;
}
