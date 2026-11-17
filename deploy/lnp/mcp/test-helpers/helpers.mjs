// Stand-ins for Google and the opn.onl backend (local HTTP servers, nothing
// leaves this machine), the connector under test, and Claude's side of the
// sign-in.

import crypto from "node:crypto";
import http from "node:http";
import { loadConfig } from "../config.mjs";
import { createApp } from "../app.mjs";

export const ISSUER = "https://links-mcp.test";
export const RESOURCE = `${ISSUER}/mcp`;
export const CLAUDE_CALLBACK = "https://claude.ai/api/mcp/auth_callback";
export const CLAUDE_COM_CALLBACK = "https://claude.com/api/mcp/auth_callback";
export const OAUTH_SECRET = "nRk3vQ8mZp1xLw6yTb4sHd9cJf2gUa7eXo5iYt0q";
export const SIGNIN_SECRET = "Wq7zMn2bVc5xLk8jHg1fDs4aPo9iUy6tRe3wQz0m";

const b64url = (buf) => Buffer.from(buf).toString("base64url");

function listen(handler) {
  return new Promise((resolve) => {
    const server = http.createServer(handler);
    server.listen(0, "127.0.0.1", () => resolve({ server, url: `http://127.0.0.1:${server.address().port}` }));
  });
}

function readBody(req) {
  return new Promise((resolve) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
  });
}

function send(res, status, body) {
  res.writeHead(status, { "Content-Type": "application/json" });
  res.end(JSON.stringify(body));
}

/** A clock the test can move. */
export function clock() {
  let offset = 0;
  const now = () => Date.now() + offset;
  now.advance = (seconds) => {
    offset += seconds * 1000;
  };
  return now;
}

/**
 * Google's token endpoint. A code "g-<person>" redeems to an ID token for that
 * person carrying the nonce the test recorded for the code. The "ID token" is
 * readable JSON: only the backend stand-in ever looks inside, as in production
 * only the backend verifies it.
 */
export async function startGoogle() {
  const google = { forms: [], nonces: new Map(), status: 200, omitIdToken: false };
  const { server, url } = await listen(async (req, res) => {
    if (req.method !== "POST" || req.url !== "/token") return send(res, 404, {});
    const form = Object.fromEntries(new URLSearchParams(await readBody(req)));
    google.forms.push(form);
    if (google.status !== 200) return send(res, google.status, { error: "invalid_grant", error_description: "Bad Request" });
    if (google.omitIdToken) return send(res, 200, { access_token: "at" });
    const person = form.code.replace(/^g-/, "");
    send(res, 200, { access_token: "google-access-token", id_token: idTokenFor(person, google.nonces.get(form.code)) });
  });
  google.server = server;
  google.tokenUrl = `${url}/token`;
  return google;
}

export function idTokenFor(person, nonce) {
  return `IDTOKEN.${b64url(JSON.stringify({ person, nonce }))}`;
}

function verifyAssertion(assertion, secret, nowMs) {
  const [h, p, s] = String(assertion).split(".");
  if (!h || !p || !s) return undefined;
  const expected = b64url(crypto.createHmac("sha256", secret).update(`${h}.${p}`).digest());
  if (expected !== s) return undefined;
  const header = JSON.parse(Buffer.from(h, "base64url"));
  const claims = JSON.parse(Buffer.from(p, "base64url"));
  const nowSec = Math.floor(nowMs / 1000);
  if (header.alg !== "HS256" || claims.aud !== "opn-connector-session") return undefined;
  if (!(claims.exp > nowSec) || claims.exp - claims.iat > 60) return undefined;
  return claims;
}

/**
 * The opn.onl backend, as far as the connector uses it. `people` are the
 * accounts Google sign-in is allowed for; `gone` are user ids the backend no
 * longer renews or honours.
 */
export async function startBackend({ now = Date.now } = {}) {
  const backend = {
    people: { alice: { user_id: 11, token_version: 0 }, bob: { user_id: 22, token_version: 3 } },
    gone: new Set(),
    sessions: new Map(), // session token -> person
    requests: [],
    googleStatus: undefined, // force an answer to /auth/connector/google
    sessionStatus: undefined, // force an answer to /auth/connector/session
    sessionAs: undefined, // answer every renewal with this person's session
    down: false,
    counter: 0,
  };
  const newSession = (person) => {
    const token = `SESSION-${person}-${++backend.counter}-${b64url(crypto.randomBytes(6))}`;
    backend.sessions.set(token, person);
    return { token, ...backend.people[person] };
  };
  const { server, url } = await listen(async (req, res) => {
    const body = req.method === "POST" ? await readBody(req) : "";
    const path = req.url.split("?")[0];
    backend.requests.push({ method: req.method, path, url: req.url, headers: req.headers, body });
    if (backend.down) return req.socket.destroy();
    const bearer = /^Bearer (.+)$/.exec(req.headers.authorization ?? "")?.[1];
    const person = backend.sessions.get(bearer);
    const live = person && !backend.gone.has(backend.people[person].user_id);

    if (req.method === "POST" && path === "/auth/connector/google") {
      if (backend.googleStatus) return send(res, backend.googleStatus, { error: "refused" });
      const json = JSON.parse(body);
      const match = /^IDTOKEN\.(.+)$/.exec(json.id_token ?? "");
      if (!match) return send(res, 401, { error: "Google sign-in failed" });
      const claims = JSON.parse(Buffer.from(match[1], "base64url"));
      if (!json.nonce || claims.nonce !== json.nonce) return send(res, 401, { error: "Google sign-in failed" });
      if (!backend.people[claims.person]) return send(res, 403, { error: "This Google account is not allowed" });
      return send(res, 200, newSession(claims.person));
    }
    if (req.method === "POST" && path === "/auth/connector/session") {
      if (backend.sessionStatus) return send(res, backend.sessionStatus, { error: "refused" });
      if (backend.sessionAs) return send(res, 200, newSession(backend.sessionAs));
      const claims = verifyAssertion(JSON.parse(body).assertion, SIGNIN_SECRET, now());
      if (!claims) return send(res, 401, { error: "Invalid assertion" });
      const who = Object.keys(backend.people).find((name) => String(backend.people[name].user_id) === claims.sub);
      if (!who || backend.gone.has(backend.people[who].user_id) || backend.people[who].token_version !== claims.tv) {
        return send(res, 401, { error: "This sign-in can no longer be renewed" });
      }
      return send(res, 200, newSession(who));
    }
    if (req.method === "GET" && path === "/auth/me") {
      return live ? send(res, 200, { email: `${person}@leadershipnowproject.com` }) : send(res, 401, { error: "Unauthorized" });
    }
    if (req.method === "GET" && path === "/links") {
      return live ? send(res, 200, { links: [], owner: person }) : send(res, 401, { error: "Unauthorized" });
    }
    send(res, 404, { error: "Not found" });
  });
  backend.server = server;
  backend.url = url;
  backend.calls = (path) => backend.requests.filter((r) => r.path === path);
  return backend;
}

export function testEnv(overrides = {}) {
  return {
    MCP_PUBLIC_URL: ISSUER,
    MCP_OAUTH_SECRET: OAUTH_SECRET,
    CONNECTOR_SIGNIN_SECRET: SIGNIN_SECRET,
    GOOGLE_CLIENT_ID: "client-1.apps.googleusercontent.com",
    GOOGLE_CLIENT_SECRET: "google-client-secret-value",
    ...overrides,
  };
}

/** The connector, between a Google and a backend stand-in. */
export async function startConnector({ env = {}, options = {}, google, backend, now } = {}) {
  now ??= clock();
  google ??= await startGoogle();
  backend ??= await startBackend({ now });
  const config = { ...loadConfig(testEnv({ BACKEND_URL: backend.url, ...env })), googleTokenUrl: google.tokenUrl };
  const app = createApp(config, { now, ...options });
  const { server, url } = await new Promise((resolve) => {
    const server = app.listen(0, "127.0.0.1", () => resolve({ server, url: `http://127.0.0.1:${server.address().port}` }));
  });
  const ctx = {
    app,
    config,
    url,
    now,
    google,
    backend,
    oauth: app.locals.oauth,
    servers: [server, google.server, backend.server],
    async close() {
      for (const s of ctx.servers) {
        s.closeAllConnections?.();
        await new Promise((resolve) => s.close(resolve));
      }
    },
    get: (path, init = {}) => fetch(`${url}${path}`, { redirect: "manual", ...init }),
    postJson: (path, body, headers = {}) =>
      fetch(`${url}${path}`, { method: "POST", redirect: "manual", headers: { "Content-Type": "application/json", ...headers }, body: JSON.stringify(body) }),
    postForm: (path, form, headers = {}) =>
      fetch(`${url}${path}`, {
        method: "POST",
        redirect: "manual",
        headers: { "Content-Type": "application/x-www-form-urlencoded", ...headers },
        body: new URLSearchParams(form),
      }),
  };
  return ctx;
}

// --- Claude's side ----------------------------------------------------------

export function pkce() {
  const verifier = b64url(crypto.randomBytes(32));
  return { verifier, challenge: b64url(crypto.createHash("sha256").update(verifier).digest()) };
}

export async function register(ctx, redirectUris = [CLAUDE_CALLBACK], extra = {}) {
  const res = await ctx.postJson("/register", { client_name: "Claude", redirect_uris: redirectUris, token_endpoint_auth_method: "none", ...extra });
  return { res, body: await res.json() };
}

export function authorizeUrl(params) {
  return `/authorize?${new URLSearchParams(params)}`;
}

/** /authorize as Claude sends the person there. Returns the response. */
export async function authorize(ctx, { clientId, challenge, state = "claude-state-1", redirectUri = CLAUDE_CALLBACK, extra = {}, cookie } = {}) {
  const params = {
    response_type: "code",
    client_id: clientId,
    redirect_uri: redirectUri,
    code_challenge: challenge,
    code_challenge_method: "S256",
    state,
    resource: RESOURCE,
    ...extra,
  };
  for (const key of Object.keys(params)) if (params[key] === undefined) delete params[key];
  return ctx.get(authorizeUrl(params), cookie ? { headers: { Cookie: cookie } } : {});
}

export function cookieFrom(res) {
  const header = res.headers.get("set-cookie");
  return header ? header.split(";")[0] : undefined;
}

/**
 * The person at Google choosing `person`, then Google sending the browser
 * back to the connector. Returns the connector's response.
 */
export async function googleCallback(ctx, authorizeResponse, person, { cookie = cookieFrom(authorizeResponse), state } = {}) {
  const toGoogle = new URL(authorizeResponse.headers.get("location"));
  const googleState = state ?? toGoogle.searchParams.get("state");
  const code = `g-${person}`;
  ctx.google.nonces.set(code, toGoogle.searchParams.get("nonce"));
  return ctx.get(`/oauth/google/callback?${new URLSearchParams({ code, state: googleState })}`, cookie ? { headers: { Cookie: cookie } } : {});
}

/** Everything up to Claude holding an authorization code. */
export async function getCode(ctx, person = "alice", { clientId, state = "claude-state-1" } = {}) {
  clientId ??= (await register(ctx)).body.client_id;
  const { verifier, challenge } = pkce();
  const authorized = await authorize(ctx, { clientId, challenge, state });
  if (authorized.status !== 302) throw new Error(`/authorize answered ${authorized.status}`);
  const back = await googleCallback(ctx, authorized, person);
  if (back.status !== 302) throw new Error(`callback answered ${back.status}: ${await back.text()}`);
  const location = new URL(back.headers.get("location"));
  return { clientId, verifier, code: location.searchParams.get("code"), location };
}

export function redeem(ctx, { clientId, code, verifier, redirectUri = CLAUDE_CALLBACK, resource = RESOURCE, headers }) {
  const form = { grant_type: "authorization_code", client_id: clientId, code, code_verifier: verifier, redirect_uri: redirectUri, resource };
  for (const key of Object.keys(form)) if (form[key] === undefined) delete form[key];
  return ctx.postForm("/token", form, headers);
}

export function refresh(ctx, { clientId, refreshToken, resource = RESOURCE, headers }) {
  const form = { grant_type: "refresh_token", client_id: clientId, refresh_token: refreshToken, resource };
  if (resource === undefined) delete form.resource;
  return ctx.postForm("/token", form, headers);
}

/** A whole sign-in: `{ clientId, tokens }`. */
export async function signIn(ctx, person = "alice") {
  const grant = await getCode(ctx, person);
  const res = await redeem(ctx, grant);
  if (res.status !== 200) throw new Error(`/token answered ${res.status}: ${await res.text()}`);
  return { clientId: grant.clientId, tokens: await res.json() };
}

export function mcpCall(ctx, accessToken, { method = "tools/call", params = { name: "list_links", arguments: {} }, headers = {}, path = "/mcp" } = {}) {
  return fetch(`${ctx.url}${path}`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Accept: "application/json, text/event-stream",
      ...(accessToken ? { Authorization: `Bearer ${accessToken}` } : {}),
      ...headers,
    },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
  });
}

/** Run `fn` and return everything it made the server write to stderr. */
export async function capturingLogs(fn) {
  const original = console.error;
  const lines = [];
  console.error = (...args) => lines.push(args.map(String).join(" "));
  try {
    await fn();
  } finally {
    console.error = original;
  }
  return lines.join("\n");
}
