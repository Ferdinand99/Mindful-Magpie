// Copies the shared start page renderer into the extension, so that the extension ships it inside
// its own package (Mozilla's add-on review forbids remote code). The sources stay where they are:
//
//   static/magpie/render.js   -> extension/vendor/render.js
//   static/magpie/render.css  -> extension/vendor/render.css
//   web/kit-tokens.css        -> extension/vendor/tokens.css (the kit tokens render.css needs)
//
// The files in extension/vendor/ are committed. CI runs this script and fails when the result
// differs from what is committed (git diff --exit-code), so a changed renderer cannot reach the
// extension by accident. Run it with `npm run build:extension` in web/.
import { copyFileSync, mkdirSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");

// [source, target] pairs, relative to the repository root.
export const files = [
  ["static/magpie/render.js", "extension/vendor/render.js"],
  ["static/magpie/render.css", "extension/vendor/render.css"],
  ["web/kit-tokens.css", "extension/vendor/tokens.css"],
];

/** Copies the files under `outRoot` (the repository root by default). Returns the target paths. */
export function build(outRoot = root) {
  const written = [];
  for (const [source, target] of files) {
    const to = join(outRoot, target);
    mkdirSync(dirname(to), { recursive: true });
    copyFileSync(join(root, source), to);
    written.push(target);
  }
  return written;
}

/** The targets whose content differs from its source under `outRoot`, or that are missing. */
export function stale(outRoot = root) {
  const out = [];
  for (const [source, target] of files) {
    let current = null;
    try {
      current = readFileSync(join(outRoot, target));
    } catch {
      current = null;
    }
    if (current === null || !current.equals(readFileSync(join(root, source)))) out.push(target);
  }
  return out;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  for (const target of build()) console.log(`wrote ${target}`);
}
