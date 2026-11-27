#!/usr/bin/env node
// 门禁：web/ 下的 JS 必须能通过语法解析。
//
// 为什么需要这道闸：`web/preview.js` 里有一个横跨 130 行的模板字符串
// （TEMPLATE），HTML 注释里一旦出现反引号，字面量就提前闭合，整份模块
// 变成语法错误。宿主把 `web/log-panel.html` 的 `<script type="module">`
// 一挂不上，面板就只剩空壳——没有页签、没有按钮回调、没有正文。这类失败
// 不经过 cargo 的任何一道门：Rust 侧编译与单测全绿，包也打得出，只有真机
// 点开才看得见。`node --check` 一秒就能拦住。
//
// 同样的理由覆盖面板页自身的行内 module 脚本：它是面板的主要逻辑，
// 语法错误一样让整个工具窗失效。
//
// 用法：node scripts/check-web-syntax.mjs [--quiet] [--root <dir>]
//   --root 指到别的目录，供 check-web-syntax.test.sh 用夹具自测。
// 退出码：0 全通过；1 有脚本解析失败。

import { readFileSync, readdirSync, statSync, mkdtempSync, writeFileSync, rmSync, existsSync } from 'node:fs';
import { join, relative, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const ROOT_DEFAULT = join(dirname(fileURLToPath(import.meta.url)), '..');
const rootArg = process.argv.indexOf('--root');
const ROOT = rootArg > -1 && process.argv[rootArg + 1] ? process.argv[rootArg + 1] : ROOT_DEFAULT;
const WEB = join(ROOT, 'web');
const quiet = process.argv.includes('--quiet');

if (!existsSync(WEB)) {
  console.error(`error: no web/ directory under ${ROOT}`);
  process.exit(1);
}

/** 收集 web/ 下所有 .js/.mjs 文件。 */
function walk(dir, out = []) {
  for (const name of readdirSync(dir).sort()) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) walk(full, out);
    else if (/\.(m?js)$/.test(name)) out.push(full);
  }
  return out;
}

/** 从 HTML 里抽出每段行内脚本（跳过外链）。 */
function inlineScripts(html) {
  const out = [];
  const re = /<script([^>]*)>([\s\S]*?)<\/script>/gi;
  let m;
  let i = 0;
  while ((m = re.exec(html))) {
    const attrs = m[1];
    if (/\bsrc\s*=/i.test(attrs)) continue;
    const type = /type\s*=\s*["']([^"']*)["']/i.exec(attrs);
    const kind = type ? type[1].toLowerCase() : '';
    if (kind && !/^(module|text\/javascript|application\/javascript)$/.test(kind)) continue;
    out.push({ index: i++, source: m[2] });
  }
  return out;
}

/**
 * 用 `node --check` 做权威解析（只解析不执行）。
 * 行内脚本落在临时目录的 .mjs 里，因为 --check 只吃文件。
 */
function parseFailure(source, label, tmp) {
  const file = join(tmp, `${label.replace(/[^\w.-]+/g, '_')}.mjs`);
  writeFileSync(file, source);
  try {
    execFileSync(process.execPath, ['--check', file], { stdio: 'pipe' });
    return null;
  } catch (err) {
    const text = `${err.stderr || ''}${err.stdout || ''}`.trim();
    // 去掉临时路径，让报错指向仓库内的文件名。
    const clean = text.split('\n').filter((l) => !l.includes(tmp)).join('\n') || err.message;
    return clean;
  }
}

const tmp = mkdtempSync(join(tmpdir(), 'sqlv-webcheck-'));
const failures = [];
let checked = 0;

try {
  for (const file of walk(WEB)) {
    const rel = relative(ROOT, file).replace(/\\/g, '/');
    checked++;
    const err = parseFailure(readFileSync(file, 'utf8'), rel, tmp);
    if (err) failures.push({ label: rel, message: err });
  }

  for (const name of readdirSync(WEB).sort()) {
    if (!name.endsWith('.html')) continue;
    const rel = relative(ROOT, join(WEB, name)).replace(/\\/g, '/');
    for (const s of inlineScripts(readFileSync(join(WEB, name), 'utf8'))) {
      const label = `${rel}#inline-${s.index}`;
      checked++;
      const err = parseFailure(s.source, label, tmp);
      if (err) failures.push({ label, message: err });
    }
  }
} finally {
  rmSync(tmp, { recursive: true, force: true });
}

if (failures.length) {
  for (const f of failures) console.error(`error: ${f.label}: ${f.message}\n`);
  console.error(`${failures.length} of ${checked} web script(s) failed to parse.`);
  process.exit(1);
}

if (!quiet) console.log(`ok: ${checked} web script(s) parse`);
process.exit(0);
