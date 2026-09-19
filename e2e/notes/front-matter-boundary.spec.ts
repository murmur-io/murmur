import { test, expect } from "@playwright/test";
import { splitNoteDocument, joinNoteDocument } from "../../src/app/shared/note-document/front-matter";

const corpus = [
  "plain body\n", "", "---\nunterminated: yes\nbody",
  "\uFEFF---\r\n# comment\r\ntags: [idea]\r\nnested:\r\n  key: 'value'\r\n---\r\n\r\n\r\nBody\r\n",
  "---\nempty:\nquoted: \"a:b\"\nblock: |\n  hello\naliases:\n - prior\n---\n \n\nBody\n",
  "---\na: 1\n---", "---\r\na: 1\r\n---",
];
for (const [index, markdown] of corpus.entries()) {
  test(`opaque front matter corpus ${index} round trips unchanged`, () => {
    const doc = splitNoteDocument(markdown);
    expect(joinNoteDocument(doc.frontMatterPrefix, doc.body)).toBe(markdown);
  });
}
for (const eol of ["\n", "\r\n"]) {
  test(`appending body after EOF YAML fence preserves delimiter ${JSON.stringify(eol)}`, () => {
    const prefix = `---${eol}key: value${eol}---`;
    const doc = splitNoteDocument(prefix);
    expect(doc.body).toBe("");
    expect(joinNoteDocument(doc.frontMatterPrefix, "New body")).toBe(`${prefix}${eol}New body`);
  });
}
