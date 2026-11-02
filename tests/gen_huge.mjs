// 生成一个超大页面用于 read/scan 性能验收（默认 120000 个元素）。
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const rows = Number(process.argv[2] || 20000);
const cells = 6;
const parts = [
  "<!doctype html><html><head><meta charset=\"utf-8\"><title>Huge Page</title></head><body>",
  "<h1>超大页面</h1>",
  "<table>",
];
for (let i = 0; i < rows; i += 1) {
  parts.push("<tr>");
  for (let c = 0; c < cells; c += 1) {
    parts.push(`<td class="c${c}"><span>row ${i} cell ${c}</span></td>`);
  }
  parts.push("</tr>");
}
parts.push("</table>");
for (let i = 0; i < 500; i += 1) {
  parts.push(`<a href="https://example.com/${i}">link ${i}</a>`);
}
parts.push("</body></html>");
const file = path.join(os.tmpdir(), "abc-huge.html");
fs.writeFileSync(file, parts.join(""));
const stat = fs.statSync(file);
console.log(JSON.stringify({ file, bytes: stat.size, elements: rows * (cells * 2 + 1) + 500 + 3 }));
