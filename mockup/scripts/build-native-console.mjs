/* Build tooling only. Deploy the manifest directory with the Rust executable.
 * A retained font cache can replay actual downloaded fonts without network. */
import { cp, mkdir, mkdtemp, readFile, readdir, realpath, rm, stat, symlink, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const source = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
const outputAt = args.indexOf('--output');
const cacheAt = args.indexOf('--font-cache');
if (outputAt < 0 || !args[outputAt + 1]) throw new Error('Usage: build-native-console.mjs --output <new directory> [--font-cache <existing .next directory>]');
const output = resolve(args[outputAt + 1]);
await mkdir(output, { mode: 0o700 }); // Refuse existing output, retaining failed artifacts.
const work = await mkdtemp(join(dirname(output), 'native-console-build-'));
const staged = join(work, 'mockup');
await mkdir(join(staged, 'app'), { recursive: true, mode: 0o700 });
for (const name of ['components', 'lib', 'package.json', 'jsconfig.json', 'next.config.mjs']) await cp(join(source, name), join(staged, name), { recursive: true });
/* Each route's WHOLE directory, not its page.jsx alone: pages import
 * siblings (engagements/NativeVerdict.jsx, project-sides/register-side.jsx,
 * project-sides/registration-control.jsx, invites/page.jsx) and a
 * page.jsx-only stage broke the canonical build with Module not found.
 *
 * This is the set of pages the NATIVE console serves, and it is not the whole
 * app tree: `config`, `capability`, `projects` and `workforce` are retained-only
 * pages whose render reads a data provenance the native provider never
 * publishes, so staging them fails the static export outright ("Cannot read
 * properties of undefined"), and the native rail links none of them (it
 * renders them as disabled rows). Dynamic segments are excluded too:
 * agents/[name] would emit one document per mock agent. `onboard` is a bare
 * redirect(), which a static export cannot honour. The served-binary test
 * (tests/console/rail.rs) parses Rail.jsx and GETs every native href, so a
 * rail page can never go missing from this list again (board #88). */
const ROUTES = ['server-engagements', 'usage', 'resources', 'alerts', 'engagements', 'accounts', 'agents', 'project-sides', 'approvals', 'tasks', 'project-board', 'task-graphs', 'invites', 'setup'];
for (const route of ROUTES) await cp(join(source, 'app', route), join(staged, 'app', route), { recursive: true });
await rm(join(staged, 'app', 'agents', '[name]'), { recursive: true, force: true });
/* app/page.jsx IS 我的资源 — the front door the rail's root row marks current. */
for (const name of ['page.jsx', 'layout.jsx', 'globals.css']) await cp(join(source, 'app', name), join(staged, 'app', name));
/*
 * ADR-145 build-time constants, staged inside the mkdtemp tree before
 * `next build` — no repo path is generated and nothing enters
 * manifest.json. The version is parsed from the root Cargo.toml's
 * [workspace.package] (the single source the versioned-release ADR
 * fixes); the schema head defaults to the migration registry's current
 * value and may be overridden by --schema-head when it moves.
 */
const toml = await readFile(join(source, '..', 'Cargo.toml'), 'utf8');
const packageAt = toml.indexOf('[workspace.package]');
const version = packageAt >= 0 ? /^version\s*=\s*"([^"]+)"\s*$/m.exec(toml.slice(packageAt))?.[1] : undefined;
if (!version) throw new Error('Workspace version missing from [workspace.package]');
const headAt = args.indexOf('--schema-head');
const schemaHead = headAt >= 0 ? Number(args[headAt + 1]) : 25;
if (!Number.isSafeInteger(schemaHead) || schemaHead < 1) throw new Error('Invalid --schema-head');
await writeFile(
  join(staged, 'lib', 'native-api.js'),
  `\n/* Appended by build-native-console.mjs (ADR-145): build-time constants.\n * The globalThis mirror exists for the bundle test: minifiers keep property\n * names and string literals while they may mangle the module-scoped binding. */\nHAGENCY_NATIVE_VERSION = ${JSON.stringify(version)};\nHAGENCY_NATIVE_SCHEMA_HEAD = ${schemaHead};\nglobalThis.__hagencyNativeVersion = ${JSON.stringify(version)};\nglobalThis.__hagencySchemaHead = ${schemaHead};\n`,
  { flag: 'a' },
);
// mock-data.js imports ../../native/hagency-core/role-capacity.json; mirror that path beside the staged tree.
await mkdir(join(work, 'native', 'hagency-core'), { recursive: true, mode: 0o700 });
await cp(join(source, '..', 'native', 'hagency-core', 'role-capacity.json'), join(work, 'native', 'hagency-core', 'role-capacity.json'));
await symlink(await realpath(join(source, 'node_modules')), join(staged, 'node_modules'), 'dir');
const env = Object.fromEntries(['PATH', 'HOME', 'TMPDIR', 'LANG'].filter((k) => process.env[k]).map((k) => [k, process.env[k]]));
Object.assign(env, { NEXT_TELEMETRY_DISABLED: '1', NEXT_PUBLIC_HAGENCY_NATIVE_CONSOLE: '1' });
if (cacheAt >= 0) {
  const cache = resolve(args[cacheAt + 1]);
  const families = { Roboto: [], 'Noto Sans SC': [] };
  // Turbopack emits CSS in chunks; webpack emits it in css.
  const styles = [];
  for (const directory of ['chunks', 'css']) {
    const folder = join(cache, 'static', directory);
    for (const file of await readdir(folder).catch(e => { if (e.code === 'ENOENT') return []; throw e; })) {
      if (file.endsWith('.css')) styles.push(join(folder, file));
    }
  }
  for (const cssPath of styles) {
    const css = await readFile(cssPath, 'utf8');
    for (const match of css.matchAll(/@font-face\{[^}]+\}/g)) {
      const family = /font-family:([^;]+);/.exec(match[0])?.[1]?.replaceAll('"', '').replaceAll("'", '').trim();
      if (!families[family] || !match[0].includes('src:url(')) continue;
      const block = match[0].replace(/src:url\(([^)]+)\)/, (_, path) => {
        const asset = path.replaceAll('"', '').replaceAll("'", '');
        const publicAt = asset.indexOf('/_next/');
        const local = publicAt >= 0 ? resolve(cache, asset.slice(publicAt + 7)) : resolve(dirname(cssPath), asset);
        return `src: url(${local})`;
      }).replaceAll(';', ';\n');
      families[family].push(block);
    }
  }
  if (Object.values(families).some((rows) => rows.length === 0)) throw new Error('Font cache lacks the retained layout fonts');
  const replay = join(work, 'retained-fonts.cjs');
  await writeFile(replay, `const fonts=${JSON.stringify(Object.fromEntries(Object.entries(families).map(([k, v]) => [k, v.join('\n')])))}; module.exports=new Proxy({}, {get(_, url) { if(typeof url!=='string') return undefined; const family=new URL(url).searchParams.get('family')?.split(':')[0]; return fonts[family]; }});`, { mode: 0o600 });
  env.NEXT_FONT_GOOGLE_MOCKED_RESPONSES = replay;
  console.log('Replaying the retained layout’s actual local font bytes; no font network requests.');
}
console.log(`Native build staging: ${work}`);
const next = join(staged, 'node_modules', 'next', 'dist', 'bin', 'next');
const child = spawn(process.execPath, [next, 'build', '--webpack'], { cwd: staged, env, stdio: 'inherit' });
const code = await new Promise((done, reject) => { child.once('error', reject); child.once('exit', done); });
if (code !== 0) throw new Error(`Next build failed (${code}); staging retained at ${work}`);
const exported = join(staged, 'out');
const files = [];
async function walk(dir, relative) {
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const path = `${relative}/${entry.name}`;
    if (entry.isDirectory()) await walk(join(dir, entry.name), path);
    else if (entry.isFile()) files.push(path);
    else throw new Error('Build contains a link or non-file');
  }
}
await walk(join(exported, '_next', 'static'), '_next/static');
/* The page documents, DERIVED from the export: the front door plus every
 * `<route>/index.html`. A page built but missing here is absent from the
 * bundle and 404s live (board #88); the framework's own error documents are
 * not pages the rail reaches. */
async function documents(dir, relative) {
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    if (entry.isDirectory()) {
      /* Only page directories are documents: the chunk tree is walked
       * separately, and the framework's error documents are not rail pages. */
      if (relative === '' && (entry.name === '404' || entry.name === '_not-found' || entry.name === '_next')) continue;
      await documents(join(dir, entry.name), relative ? `${relative}/${entry.name}` : entry.name);
    } else if (entry.isFile() && entry.name === 'index.html') {
      files.push(relative ? `${relative}/index.html` : 'index.html');
    }
  }
}
await documents(exported, '');
const mime = (path) => path === 'index.html' || path.endsWith('/index.html') ? 'text/html; charset=utf-8'
  : path.endsWith('.js') ? 'text/javascript; charset=utf-8' : path.endsWith('.css') ? 'text/css; charset=utf-8'
    : path.endsWith('.woff2') ? 'font/woff2' : path.endsWith('.woff') ? 'font/woff' : null;
let total = 0; let largest = 0; const assets = [];
for (const path of files.sort()) {
  const type = mime(path);
  if (!type) throw new Error(`Unexpected exported asset type: ${path}`);
  const bytes = await readFile(join(exported, path));
  total += bytes.length; largest = Math.max(largest, bytes.length);
  assets.push({ path, size: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex'), mime: type });
}
console.log(JSON.stringify({ count: assets.length, total_bytes: total, largest_bytes: largest }));
if (assets.length > 512 || total > 32 * 1024 * 1024 || largest > 4 * 1024 * 1024) throw new Error('Native asset limits exceeded; no required chunk was excluded');
const manifest = JSON.stringify({ version: 1, assets });
if (Buffer.byteLength(manifest) > 128 * 1024) throw new Error('Native manifest limit exceeded');
for (const asset of assets) {
  await mkdir(dirname(join(output, asset.path)), { recursive: true, mode: 0o700 });
  await writeFile(join(output, asset.path), await readFile(join(exported, asset.path)), { flag: 'wx', mode: 0o600 });
  if ((await stat(join(output, asset.path))).size !== asset.size) throw new Error('Asset changed while packaging');
}
await writeFile(join(output, 'manifest.json'), manifest, { flag: 'wx', mode: 0o600 });
console.log(`Native console assets: ${output}`);
