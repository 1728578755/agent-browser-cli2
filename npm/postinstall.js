const fs = require("node:fs");
const path = require("node:path");

const bin = path.resolve(__dirname, "bin", "atb.js");
try {
  fs.chmodSync(bin, 0o755);
} catch (_) {
  // Windows 和部分包管理器环境不需要 chmod。
}
