/*
 * Shared SQL lexer and highlighter.
 *
 * The IntelliJ edition colours SQL in two places with the same token set:
 * `ui/highlight/SqlLexer.kt` produces the tokens and
 * `ui/highlight/SqlHighlightColors.kt` maps each to a TextAttributesKey that
 * resolves through the IDE colour scheme. That scheme indirection is the point
 * of the design: the plugin owns the *meaning* of a token, never a colour value,
 * so a theme switch or a user with a custom scheme gets consistent results.
 *
 * A web page has no scheme to look up, so the same meaning is bound to the
 * host's semantic CSS variables instead (`--openidea-accent` for keywords,
 * `--openidea-link` for table names, …). This keeps the original's contract —
 * token classes are chosen by the lexer, colours are chosen by the environment —
 * and the log page's "no literal colours" rule stays true.
 *
 * Token classes match the original's `MP_SQL.*` keys one for one:
 *
 *   sqlv-kw     KEYWORD      keyword            (KEYWORD)
 *   sqlv-tbl    TABLE        table name         (STATIC_FIELD — italic)
 *   sqlv-id     IDENTIFIER   identifier         (IDENTIFIER)
 *   sqlv-str    STRING       quoted literal     (STRING)
 *   sqlv-num    NUMBER       numeric literal    (NUMBER)
 *   sqlv-com    COMMENT      `--` and block     (LINE_COMMENT — italic)
 *   sqlv-op     OPERATOR     operators          (OPERATION_SIGN)
 *   sqlv-paren  PAREN        parentheses        (PARENTHESES)
 *
 * Everything is pure: `tokenize(text)` returns `{ cls, text }` pieces and
 * `into(element, text)` writes them as `<span class="sqlv-…">` nodes built with
 * `textContent`, so no SQL text can ever become markup. The schema name is
 * `sqlv` rather than `pv` because both the preview page and the log panel use
 * it, and a future page may mount either one twice.
 */

/** The original's keyword set (`SqlLexer.kt`, KEYWORDS). Order is irrelevant. */
const KEYWORDS = new Set(
  (
    'select from where and or not null is in between like exists group by order ' +
    'asc desc having limit offset distinct all join left right inner outer full ' +
    'cross on as using union insert into values update set delete table create ' +
    'drop alter add column primary key foreign references constraint default ' +
    'index case when then else end count sum max min avg coalesce nullif cast ' +
    'convert'
  ).split(' '),
);

/** Words after which the next identifier is a table name (`SqlLexer` heuristics). */
const TABLE_LEADERS = new Set([
  'from', 'join', 'inner', 'left', 'right', 'full', 'cross', 'into',
  'update', 'table',
]);

/** Multi-character operators, longest first, exactly the original's list. */
const MULTI_OPS = [
  '<=>', '->>', '!=', '<>', '<=', '>=', '||', '::', ':=', '->', '&&', '<<',
  '>>', '!~', '~*', '@>', '<@', '=>',
];

/** Single-character operators that are not parentheses. `,` and `;` are
 *  punctuation but the original colours them as operator signs too. */
const SINGLE_OPS = new Set([
  '+', '-', '*', '/', '%', '=', '<', '>', '!', '&', '|', '^', '~', '?', ':',
  ',', ';',
]);

/** Token classes; also the CSS class suffixes. */
export const CLASSES = {
  keyword: 'kw',
  table: 'tbl',
  identifier: 'id',
  string: 'str',
  number: 'num',
  comment: 'com',
  operator: 'op',
  paren: 'paren',
};

function isWordStart(ch) {
  return (ch >= 'a' && ch <= 'z') || (ch >= 'A' && ch <= 'Z') || ch === '_' || ch === '$';
}

function isWordPart(ch) {
  return isWordStart(ch) || (ch >= '0' && ch <= '9');
}

function isDigit(ch) {
  return ch >= '0' && ch <= '9';
}

function isSpace(ch) {
  return ch === ' ' || ch === '\t' || ch === '\r' || ch === '\n' || ch === '\f';
}

/**
 * Split `text` into `{ cls, text }` pieces. `cls` is `null` for whitespace so
 * callers can append it verbatim without a span. Tokens never overlap and their
 * concatenation is exactly `text`.
 */
export function tokenize(text) {
  const out = [];
  // `FROM`/`JOIN`/… ask for the next identifier to be a table name; that name
  // may be followed by `AS alias`, and the alias is a table name too. Any other
  // token closes both windows: the original deliberately does not guess beyond
  // one alias.
  let expectTable = false;
  let expectAlias = false;
  let i = 0;

  const push = (cls, end) => {
    if (end <= i) return;
    out.push({ cls, text: text.slice(i, end) });
    i = end;
  };

  while (i < text.length) {
    const ch = text[i];

    if (isSpace(ch)) {
      let j = i + 1;
      while (j < text.length && isSpace(text[j])) j += 1;
      push(null, j);
      continue;
    }

    // `-- line comment` and `/* block comment */`.
    if (ch === '-' && text[i + 1] === '-') {
      const nl = text.indexOf('\n', i);
      push(CLASSES.comment, nl < 0 ? text.length : nl);
      continue;
    }
    if (ch === '/' && text[i + 1] === '*') {
      const close = text.indexOf('*/', i + 2);
      push(CLASSES.comment, close < 0 ? text.length : close + 2);
      continue;
    }

    // Quoted strings: `'…'` with `''` escape, and `"…"` with `""` escape.
    if (ch === "'" || ch === '"') {
      let j = i + 1;
      while (j < text.length) {
        if (text[j] === ch) {
          if (text[j + 1] === ch) { j += 2; continue; }
          j += 1;
          break;
        }
        j += 1;
      }
      push(CLASSES.string, Math.min(j, text.length));
      expectTable = false;
      expectAlias = false;
      continue;
    }

    // Backticks and square brackets quote an identifier, not a string.
    if (ch === '`' || ch === '[') {
      const close = ch === '`' ? '`' : ']';
      const end = text.indexOf(close, i + 1);
      const wasTable = expectTable || expectAlias;
      const isAlias = expectAlias && !expectTable;
      push(wasTable ? CLASSES.table : CLASSES.identifier, end < 0 ? text.length : end + 1);
      expectTable = false;
      expectAlias = !isAlias && wasTable;
      continue;
    }

    if (isDigit(ch) || (ch === '.' && isDigit(text[i + 1] || ''))) {
      let j = i + 1;
      while (j < text.length && (isDigit(text[j]) || text[j] === '.')) j += 1;
      push(CLASSES.number, j);
      expectTable = false;
      expectAlias = false;
      continue;
    }

    if (ch === '(' || ch === ')') {
      push(CLASSES.paren, i + 1);
      expectTable = false;
      expectAlias = false;
      continue;
    }

    // Multi-character operators win by longest match, as in the original.
    let matchedOp = null;
    for (const op of MULTI_OPS) {
      if (text.startsWith(op, i)) { matchedOp = op; break; }
    }
    if (matchedOp) {
      push(CLASSES.operator, i + matchedOp.length);
      expectTable = false;
      expectAlias = false;
      continue;
    }
    if (SINGLE_OPS.has(ch)) {
      push(CLASSES.operator, i + 1);
      expectTable = false;
      expectAlias = false;
      continue;
    }

    if (isWordStart(ch)) {
      let j = i + 1;
      while (j < text.length && isWordPart(text[j])) j += 1;
      const word = text.slice(i, j);
      const lower = word.toLowerCase();

      if (KEYWORDS.has(lower)) {
        push(CLASSES.keyword, j);
        if (TABLE_LEADERS.has(lower)) {
          expectTable = true;
          expectAlias = false;
        } else if (lower === 'as' && expectAlias) {
          // `FROM t AS a`: the alias is still a table reference.
          expectTable = true;
          expectAlias = false;
        } else {
          expectTable = false;
          expectAlias = false;
        }
        continue;
      }

      const cls = expectTable || expectAlias ? CLASSES.table : CLASSES.identifier;
      const isAlias = expectAlias && !expectTable;
      push(cls, j);
      // A table name may be followed by one alias; the alias ends the window.
      expectTable = false;
      expectAlias = !isAlias && cls === CLASSES.table;
      continue;
    }

    // Anything else (a stray byte, an emoji) stays plain text.
    push(null, i + 1);
    expectTable = false;
    expectAlias = false;
  }

  return out;
}

/* Repeated statements are the norm in the log list (a loop of the same SELECT),
   so the last few hundred token lists are kept. Bounded so a long session does
   not grow without limit. */
const CACHE_LIMIT = 512;
const cache = new Map();

function tokenizeCached(text) {
  const hit = cache.get(text);
  if (hit) return hit;
  const tokens = tokenize(text);
  if (cache.size >= CACHE_LIMIT) cache.clear();
  cache.set(text, tokens);
  return tokens;
}

/**
 * Replace `element`'s children with coloured spans for `text`.
 *
 * `limit` guards the obvious cost case: a 20 000-character statement would
 * build thousands of nodes for a two-line summary. Past the limit the text is
 * written as a single plain node — the same text, no colours.
 */
export function into(element, text, limit = 20000) {
  const doc = element.ownerDocument || document;
  element.textContent = '';
  if (!text) return element;
  if (text.length > limit) {
    element.textContent = text;
    return element;
  }
  const fragment = doc.createDocumentFragment();
  for (const token of tokenizeCached(text)) {
    if (!token.cls) {
      fragment.appendChild(doc.createTextNode(token.text));
      continue;
    }
    const span = doc.createElement('span');
    span.className = 'sqlv-' + token.cls;
    span.textContent = token.text;
    fragment.appendChild(span);
  }
  element.appendChild(fragment);
  return element;
}

/** The token classes as CSS, injected once per document. */
export function installStyles(doc = document) {
  if (doc.getElementById('sqlv-highlight-styles')) return;
  const style = doc.createElement('style');
  style.id = 'sqlv-highlight-styles';
  // Colours are the host's semantic variables, mirroring the original's scheme
  // keys; italic on table names and comments is the one stylistic bit the
  // original's `STATIC_FIELD` and `LINE_COMMENT` keys carry.
  style.textContent = [
    '.sqlv-kw{color:var(--openidea-accent);font-weight:600}',
    '.sqlv-tbl{color:var(--openidea-link);font-style:italic}',
    '.sqlv-id{color:var(--openidea-fg)}',
    '.sqlv-str{color:var(--openidea-success)}',
    '.sqlv-num{color:var(--openidea-info)}',
    '.sqlv-com{color:var(--openidea-fg-muted);font-style:italic}',
    '.sqlv-op,.sqlv-paren{color:var(--openidea-fg)}',
  ].join('\n');
  (doc.head || doc.documentElement).appendChild(style);
}

export default { tokenize, into, installStyles, CLASSES };