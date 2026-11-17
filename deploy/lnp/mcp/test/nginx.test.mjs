// The backend's connector routes (/auth/connector/google and, above all,
// /auth/connector/session, which trusts a shared secret rather than a person)
// must not be reachable through any public host. This reads the three nginx
// files and picks the location nginx would pick for each path, by nginx's own
// rules; ci.yml replays the same paths against a running nginx.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const read = (path) => readFileSync(new URL(`../../../../${path}`, import.meta.url), "utf8").replace(/\r\n/g, "\n");
const STOCK = read("frontend/nginx.conf");
const ADMIN = read("deploy/lnp/nginx/admin.conf");
const PUBLIC = read("deploy/lnp/nginx/public.conf");

// Comments go, and a regex quantifier's braces ({1,255}) are set aside so the
// only braces left are blocks.
const prepared = (text) => text.replace(/^\s*#.*$/gm, "").replace(/\{(\d+)(,\d*)?\}/g, "<<$1$2>>");
const quantifiers = (pattern) => pattern.replace(/<<(\d+)(,\d*)?>>/g, "{$1$2}");

/** `text` with everything inside nested blocks blanked out. */
function topLevel(text) {
  let out = "";
  let depth = 0;
  for (const ch of text) {
    if (ch === "}") depth -= 1;
    out += depth > 0 ? " " : ch;
    if (ch === "{") depth += 1;
  }
  return out;
}

/** The `location` blocks directly inside `text`: `{ modifier, pattern, body }`. */
function locations(text) {
  const found = [];
  const start = /(^|[\s;{}])location\s+(?:(=|\^~|~\*|~)\s+)?("[^"]*"|\S+)\s*\{/g;
  // Only this level's locations are seen.
  const depthZeroText = topLevel(text);
  let match;
  while ((match = start.exec(depthZeroText))) {
    const open = match.index + match[0].length;
    let level = 1;
    let end = open;
    while (level > 0) {
      if (text[end] === "{") level += 1;
      if (text[end] === "}") level -= 1;
      end += 1;
    }
    found.push({ modifier: match[2] ?? "", pattern: quantifiers(match[3].replace(/^"|"$/g, "")), body: text.slice(open, end - 1) });
  }
  return found;
}

/** The location nginx serves `path` from, descending into nested locations. */
function pick(text, path) {
  const all = locations(text);
  const exact = all.find((l) => l.modifier === "=" && l.pattern === path);
  let chosen = exact;
  if (!chosen) {
    const prefixes = all.filter((l) => (l.modifier === "" || l.modifier === "^~") && path.startsWith(l.pattern));
    const longest = prefixes.sort((a, b) => b.pattern.length - a.pattern.length)[0];
    const regex = longest?.modifier === "^~" ? undefined : all.find((l) => (l.modifier === "~" || l.modifier === "~*") && new RegExp(l.pattern, l.modifier === "~*" ? "i" : "").test(path));
    chosen = regex ?? longest;
  }
  if (!chosen) return undefined;
  return pick(chosen.body, path) ?? chosen;
}

/** The stock server block's contents, with a deployment's file where the include is. */
function serverWith(siteConf) {
  const stock = prepared(STOCK);
  const include = "include /etc/nginx/site.d/*.conf;";
  assert.ok(stock.includes(include), "the stock config includes site.d");
  const merged = stock.replace(include, prepared(siteConf));
  const server = /\bserver\s*\{/.exec(merged);
  return merged.slice(server.index + server[0].length);
}

const CONNECTOR_PATHS = ["/auth/connector/google", "/auth/connector/session", "/auth/connector", "/auth/connector/session/"];
/** Whether the chosen location itself (not a location nested in it) proxies. */
const reaches = (location) => /\bproxy_pass\b/.test(topLevel(location.body));
const own = (location) => topLevel(location.body);

test("the simulator picks locations the way nginx does", () => {
  const stock = serverWith("");
  assert.ok(reaches(pick(stock, "/abc123")), "a short code is proxied");
  assert.ok(reaches(pick(stock, "/abc123/verify")), "its password check is proxied");
  assert.ok(!reaches(pick(stock, "/dashboard")), "a page is not");
  const admin = serverWith(ADMIN);
  assert.ok(reaches(pick(admin, "/api/auth/google")), "the dashboard's sign-in is proxied");
  assert.ok(reaches(pick(admin, "/api/links")));
  assert.match(own(pick(admin, "/api/auth/login")), /return 403/);
  const pub = serverWith(PUBLIC);
  assert.ok(reaches(pick(pub, "/api/abc123")), "a short link's API call is proxied");
  assert.ok(reaches(pick(pub, "/api/abc123/view/file")));
  assert.match(own(pick(pub, "/api/links")), /return 404/);
  assert.match(own(pick(pub, "/api/abc123/unknown")), /return 404/, "the end of the /api/ block");
  assert.ok(!reaches(pick(pub, "/api/abc123/unknown")));
});

test("the dashboard host refuses the connector's backend routes", () => {
  const admin = serverWith(ADMIN);
  for (const path of CONNECTOR_PATHS) {
    const api = pick(admin, `/api${path}`);
    assert.match(own(api), /^\s*return 403;\s*$/, `/api${path}`);
    assert.ok(!reaches(pick(admin, path)), `${path} is not proxied either`);
  }
});

test("the public host refuses the connector's backend routes", () => {
  const pub = serverWith(PUBLIC);
  for (const path of CONNECTOR_PATHS) {
    for (const full of [`/api${path}`, path]) {
      const location = pick(pub, full);
      assert.ok(!reaches(location), full);
      assert.match(own(location), /return 404;/, full);
    }
  }
});

test("the stock image does not proxy the connector's backend routes", () => {
  const stock = serverWith("");
  for (const path of CONNECTOR_PATHS) {
    for (const full of [path, `/api${path}`]) assert.ok(!reaches(pick(stock, full)), full);
  }
});
