// The connector's sign-in, end to end, against local stand-ins for Google and
// the backend. Each test starts its own connector.

import assert from "node:assert/strict";
import crypto from "node:crypto";
import { test } from "node:test";
import { ACCESS_TOKEN_SECONDS, CODE_SECONDS, ExpiringMap, PENDING_SECONDS, RateLimiter, Sealer, signAssertion } from "../oauth.mjs";
import {
  CLAUDE_CALLBACK,
  CLAUDE_COM_CALLBACK,
  ISSUER,
  OAUTH_SECRET,
  RESOURCE,
  SIGNIN_SECRET,
  authorize,
  capturingLogs,
  clock,
  cookieFrom,
  getCode,
  googleCallback,
  mcpCall,
  pkce,
  redeem,
  refresh,
  register,
  signIn,
  startBackend,
  startConnector,
  startGoogle,
} from "../test-helpers/helpers.mjs";

/** Start a connector for one test and close it afterwards. */
async function withConnector(t, options) {
  const ctx = await startConnector(options);
  t.after(() => ctx.close());
  return ctx;
}

/** Who the backend saw a /links call from. */
function linksCallers(ctx) {
  return ctx.backend.calls("/links").map((r) => ctx.backend.sessions.get(r.headers.authorization.replace("Bearer ", "")));
}

/** Change one character near the end (the very last may carry unused bits). */
const flipLast = (text) => {
  const i = text.length - 4;
  return text.slice(0, i) + (text[i] === "A" ? "B" : "A") + text.slice(i + 1);
};

// --- discovery ----------------------------------------------------------------

test("an unauthenticated /mcp call is a 401 that points at the resource metadata", async (t) => {
  const ctx = await withConnector(t);
  const res = await mcpCall(ctx, undefined);
  assert.equal(res.status, 401);
  assert.equal(res.headers.get("www-authenticate"), `Bearer resource_metadata="${ISSUER}/.well-known/oauth-protected-resource/mcp"`);
  assert.equal(res.headers.get("cache-control"), "no-store");
  assert.equal(ctx.backend.requests.length, 0, "nothing reaches the backend without a token");
});

test("both metadata documents name this server, whatever Host the request carries", async (t) => {
  const ctx = await withConnector(t);
  for (const path of ["/.well-known/oauth-protected-resource/mcp", "/.well-known/oauth-protected-resource"]) {
    const res = await ctx.get(path, { headers: { Host: "evil.example", "X-Forwarded-Host": "evil.example" } });
    assert.equal(res.status, 200);
    assert.deepEqual(await res.json(), {
      resource: RESOURCE,
      authorization_servers: [ISSUER],
      bearer_methods_supported: ["header"],
      resource_name: "LNP Links",
    });
  }
  const res = await ctx.get("/.well-known/oauth-authorization-server", { headers: { Host: "evil.example" } });
  assert.equal(res.status, 200);
  assert.deepEqual(await res.json(), {
    issuer: ISSUER,
    authorization_endpoint: `${ISSUER}/authorize`,
    token_endpoint: `${ISSUER}/token`,
    registration_endpoint: `${ISSUER}/register`,
    response_types_supported: ["code"],
    grant_types_supported: ["authorization_code", "refresh_token"],
    code_challenge_methods_supported: ["S256"],
    token_endpoint_auth_methods_supported: ["none"],
    authorization_response_iss_parameter_supported: true,
  });
  assert.equal((await ctx.postJson("/.well-known/oauth-authorization-server", {})).status, 405);
});

// --- registration ---------------------------------------------------------------

test("registration accepts Claude's two callbacks and issues a public client", async (t) => {
  const ctx = await withConnector(t);
  for (const uris of [[CLAUDE_CALLBACK], [CLAUDE_COM_CALLBACK], [CLAUDE_CALLBACK, CLAUDE_COM_CALLBACK]]) {
    const { res, body } = await register(ctx, uris);
    assert.equal(res.status, 201, JSON.stringify(body));
    assert.equal(res.headers.get("cache-control"), "no-store");
    assert.deepEqual(body.redirect_uris, uris);
    assert.equal(body.token_endpoint_auth_method, "none");
    assert.match(body.client_id, /^lnpc1\.[A-Za-z0-9_-]+$/);
    assert.equal(body.client_secret, undefined);
  }
  // A client that asks to be confidential is still a public one, with no secret.
  const { res, body } = await register(ctx, [CLAUDE_CALLBACK], { token_endpoint_auth_method: "client_secret_post" });
  assert.equal(res.status, 201);
  assert.equal(body.token_endpoint_auth_method, "none");
  assert.equal(body.client_secret, undefined);
  assert.equal(body.client_secret_expires_at, undefined);
});

test("registration refuses every other redirect address", async (t) => {
  const ctx = await withConnector(t);
  const refused = [
    ["https://*.claude.ai/api/mcp/auth_callback"],
    ["https://claude.ai/*"],
    ["http://claude.ai/api/mcp/auth_callback"],
    ["http://localhost:3118/callback"],
    ["http://127.0.0.1:3118/callback"],
    ["https://localhost/api/mcp/auth_callback"],
    ["https://claude.ai/api/mcp/auth_callback/extra"],
    ["https://claude.ai/api/mcp/auth_callback/"],
    ["https://claude.ai/api/mcp/auth_callback?x=1"],
    ["https://claude.ai/api/mcp/auth_callback#frag"],
    ["https://claude.ai/api/mcp/../mcp/auth_callback"],
    ["https://user@claude.ai/api/mcp/auth_callback"],
    ["https://claude.ai@evil.example/api/mcp/auth_callback"],
    ["https://claude.ai.evil.example/api/mcp/auth_callback"],
    ["https://evil-claude.ai/api/mcp/auth_callback"],
    ["https://CLAUDE.AI/api/mcp/auth_callback"],
    ["https://claude.ai:8443/api/mcp/auth_callback"],
    ["https://evil.example/cb"],
    // One good address does not carry a bad one in.
    [CLAUDE_CALLBACK, "https://evil.example/cb"],
    [],
  ];
  for (const uris of refused) {
    const { res, body } = await register(ctx, uris);
    assert.equal(res.status, 400, `${JSON.stringify(uris)} -> ${JSON.stringify(body)}`);
    assert.equal(body.client_id, undefined, JSON.stringify(uris));
  }
  const { res, body } = await register(ctx, ["https://evil.example/cb"]);
  assert.equal(res.status, 400);
  assert.equal(body.error, "invalid_redirect_uri");
  // Not a list at all.
  assert.equal((await ctx.postJson("/register", { redirect_uris: CLAUDE_CALLBACK })).status, 400);
  assert.equal((await ctx.postJson("/register", {})).status, 400);
});

test("a registration needs no storage: it survives a restart, and only with the same secret", async (t) => {
  const google = await startGoogle();
  const backend = await startBackend();
  const first = await withConnector(t, { google, backend });
  const { body } = await register(first);
  const { challenge } = pkce();

  const restarted = await startConnector({ google, backend });
  t.after(() => restarted.close());
  assert.equal((await authorize(restarted, { clientId: body.client_id, challenge })).status, 302);

  const otherSecret = await startConnector({ google, backend, env: { MCP_OAUTH_SECRET: "Zx9cVb2nMq5wEr8tYu1iOp4aSd7fGh0jKl3zXc6v" } });
  t.after(() => otherSecret.close());
  const refused = await authorize(otherSecret, { clientId: body.client_id, challenge });
  assert.equal(refused.status, 400);
  assert.equal(refused.headers.get("location"), null);
});

test("a callback taken off the allow-list stops working for clients that registered it", async (t) => {
  const google = await startGoogle();
  const backend = await startBackend();
  const both = await withConnector(t, { google, backend });
  const { body } = await register(both, [CLAUDE_COM_CALLBACK]);

  const narrowed = await startConnector({ google, backend, env: { MCP_ALLOWED_REDIRECT_URIS: CLAUDE_CALLBACK } });
  t.after(() => narrowed.close());
  const res = await authorize(narrowed, { clientId: body.client_id, challenge: pkce().challenge, redirectUri: CLAUDE_COM_CALLBACK });
  assert.equal(res.status, 400);
  assert.equal(res.headers.get("location"), null);
  assert.equal((await register(narrowed, [CLAUDE_COM_CALLBACK])).res.status, 400);
});

// --- /authorize -----------------------------------------------------------------

test("/authorize shows a page, and redirects nowhere, for an unknown client or redirect address", async (t) => {
  const ctx = await withConnector(t);
  const { body } = await register(ctx);
  const { challenge } = pkce();
  const sealer = new Sealer(OAUTH_SECRET);
  const cases = {
    "no client": { clientId: undefined, challenge },
    "made-up client": { clientId: "lnpc1.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", challenge },
    "tampered client": { clientId: flipLast(body.client_id), challenge },
    "an access token as client id": { clientId: sealer.seal("access", { r: [CLAUDE_CALLBACK] }), challenge },
    "a client sealed for an address that is not allowed": { clientId: sealer.seal("client", { r: ["https://evil.example/cb"], t: 1 }), challenge, redirectUri: "https://evil.example/cb" },
    "unregistered redirect": { clientId: body.client_id, challenge, redirectUri: "https://evil.example/cb" },
    "allowed but not registered by this client": { clientId: body.client_id, challenge, redirectUri: CLAUDE_COM_CALLBACK },
    "look-alike redirect": { clientId: body.client_id, challenge, redirectUri: `${CLAUDE_CALLBACK}/` },
  };
  for (const [what, params] of Object.entries(cases)) {
    const res = await authorize(ctx, params);
    assert.equal(res.status, 400, what);
    assert.equal(res.headers.get("location"), null, what);
    assert.match(res.headers.get("content-type"), /^text\/html/, what);
    assert.equal(res.headers.get("set-cookie"), null, what);
    const html = await res.text();
    assert.ok(!html.includes("evil.example"), `${what}: the page repeats the address`);
  }
  // The same parameter twice is not a parameter.
  const twice = await ctx.get(`/authorize?client_id=${encodeURIComponent(body.client_id)}&client_id=${encodeURIComponent(body.client_id)}&redirect_uri=${encodeURIComponent(CLAUDE_CALLBACK)}`);
  assert.equal(twice.status, 400);
  assert.equal(ctx.oauth.internals.pending.size, 0, "nothing was kept for a refused request");
  assert.equal((await ctx.postForm("/authorize", { client_id: body.client_id })).status, 405);
});

test("/authorize sends other mistakes back to Claude's callback as errors, never on to Google", async (t) => {
  const ctx = await withConnector(t);
  const { body } = await register(ctx);
  const { challenge } = pkce();
  const cases = [
    ["no PKCE challenge", { challenge: undefined }, "invalid_request"],
    ["plain PKCE", { challenge, extra: { code_challenge_method: "plain" } }, "invalid_request"],
    ["no challenge method", { challenge, extra: { code_challenge_method: undefined } }, "invalid_request"],
    ["a challenge that is not a SHA-256", { challenge: "short" }, "invalid_request"],
    ["no state", { challenge, state: "" }, "invalid_request"],
    ["response_type=token", { challenge, extra: { response_type: "token" } }, "unsupported_response_type"],
    ["another resource", { challenge, extra: { resource: "https://other.example/mcp" } }, "invalid_target"],
    ["this host, another path", { challenge, extra: { resource: `${ISSUER}/other` } }, "invalid_target"],
    ["the bare host", { challenge, extra: { resource: ISSUER } }, "invalid_target"],
    ["a look-alike host", { challenge, extra: { resource: "https://links-mcp.test.evil.example/mcp" } }, "invalid_target"],
  ];
  for (const [what, params, error] of cases) {
    const res = await authorize(ctx, { clientId: body.client_id, ...params });
    assert.equal(res.status, 302, what);
    const location = new URL(res.headers.get("location"));
    assert.equal(`${location.origin}${location.pathname}`, CLAUDE_CALLBACK, what);
    assert.equal(location.searchParams.get("error"), error, what);
    assert.equal(location.searchParams.get("code"), null, what);
    assert.equal(location.searchParams.get("iss"), ISSUER, what);
    if (params.state !== "") assert.equal(location.searchParams.get("state"), "claude-state-1", what);
  }
  assert.equal(ctx.oauth.internals.pending.size, 0);
});

test("/authorize sends a good request to Google with a fresh state and nonce", async (t) => {
  const ctx = await withConnector(t);
  const { body } = await register(ctx);
  const first = await authorize(ctx, { clientId: body.client_id, challenge: pkce().challenge });
  assert.equal(first.status, 302);
  assert.equal(first.headers.get("cache-control"), "no-store");
  const google = new URL(first.headers.get("location"));
  assert.equal(`${google.origin}${google.pathname}`, "https://accounts.google.com/o/oauth2/v2/auth");
  assert.deepEqual(Object.fromEntries(google.searchParams), {
    client_id: "client-1.apps.googleusercontent.com",
    redirect_uri: `${ISSUER}/oauth/google/callback`,
    response_type: "code",
    scope: "openid email",
    state: google.searchParams.get("state"),
    nonce: google.searchParams.get("nonce"),
    prompt: "select_account",
  });
  assert.match(google.searchParams.get("state"), /^[A-Za-z0-9_-]{43}$/);
  assert.match(google.searchParams.get("nonce"), /^[A-Za-z0-9_-]{43}$/);
  assert.notEqual(google.searchParams.get("state"), "claude-state-1");
  assert.match(first.headers.get("set-cookie"), /^__Host-lnp_mcp_signin=[A-Za-z0-9_-]{43}; Path=\/; Max-Age=600; Secure; HttpOnly; SameSite=Lax$/);

  const second = new URL((await authorize(ctx, { clientId: body.client_id, challenge: pkce().challenge })).headers.get("location"));
  assert.notEqual(second.searchParams.get("state"), google.searchParams.get("state"));
  assert.notEqual(second.searchParams.get("nonce"), google.searchParams.get("nonce"));

  // A trailing slash or capitals in the resource are the same resource; no resource is fine too.
  for (const resource of [`${RESOURCE}/`, "HTTPS://LINKS-MCP.TEST/mcp", undefined]) {
    const res = await authorize(ctx, { clientId: body.client_id, challenge: pkce().challenge, extra: { resource } });
    assert.equal(new URL(res.headers.get("location")).hostname, "accounts.google.com", String(resource));
  }
});

// --- the whole flow ---------------------------------------------------------------

test("the whole flow: 401, register, authorize, Google, callback, token, then tools run as that person", async (t) => {
  const ctx = await withConnector(t);
  assert.equal((await mcpCall(ctx, undefined)).status, 401);

  const grant = await getCode(ctx, "alice", { state: "state from Claude & co" });
  assert.equal(`${grant.location.origin}${grant.location.pathname}`, CLAUDE_CALLBACK);
  assert.equal(grant.location.searchParams.get("state"), "state from Claude & co");
  assert.equal(grant.location.searchParams.get("iss"), ISSUER);
  assert.match(grant.code, /^[A-Za-z0-9_-]{43}$/);

  // Google was asked the standard way, with this server's own callback.
  assert.deepEqual(ctx.google.forms, [
    {
      code: "g-alice",
      client_id: "client-1.apps.googleusercontent.com",
      client_secret: "google-client-secret-value",
      redirect_uri: `${ISSUER}/oauth/google/callback`,
      grant_type: "authorization_code",
    },
  ]);
  // The backend got the ID token and the nonce, and the protocol it expects.
  const [signInCall] = ctx.backend.calls("/auth/connector/google");
  assert.equal(signInCall.headers["x-forwarded-proto"], "https");
  assert.deepEqual(Object.keys(JSON.parse(signInCall.body)).sort(), ["id_token", "nonce"]);

  const res = await redeem(ctx, grant);
  assert.equal(res.status, 200);
  assert.equal(res.headers.get("cache-control"), "no-store");
  const tokens = await res.json();
  assert.equal(tokens.token_type, "Bearer");
  assert.equal(tokens.expires_in, ACCESS_TOKEN_SECONDS);
  assert.match(tokens.access_token, /^lnpa1\./);
  assert.match(tokens.refresh_token, /^lnpr1\./);
  // The backend session is inside the token, where Claude cannot read it.
  for (const session of ctx.backend.sessions.keys()) {
    assert.ok(!tokens.access_token.includes(session) && !tokens.refresh_token.includes(session));
    assert.ok(!Buffer.from(tokens.access_token.split(".")[1], "base64url").toString("latin1").includes(session));
  }

  const call = await mcpCall(ctx, tokens.access_token, { headers: { "CF-Connecting-IP": "203.0.113.9" } });
  assert.equal(call.status, 200);
  const result = await call.json();
  assert.equal(result.result.isError, undefined, JSON.stringify(result));
  assert.match(result.result.content[0].text, /"owner": ?"alice"/);
  assert.deepEqual(linksCallers(ctx), ["alice"]);
  const [linksCall] = ctx.backend.calls("/links");
  assert.equal(linksCall.headers["cf-connecting-ip"], "203.0.113.9");
  assert.equal(linksCall.headers["x-forwarded-proto"], "https");

  // The tools staff don't get are still hidden.
  const list = await (await mcpCall(ctx, tokens.access_token, { method: "tools/list", params: {} })).json();
  const names = list.result.tools.map((tool) => tool.name);
  assert.ok(names.includes("shorten_url"));
  assert.ok(!names.includes("delete_link") && !names.includes("shorten_urls_bulk"));
});

test("each person's token runs as that person, never as another", async (t) => {
  const ctx = await withConnector(t);
  const alice = await signIn(ctx, "alice");
  const bob = await signIn(ctx, "bob");
  await mcpCall(ctx, bob.tokens.access_token);
  await mcpCall(ctx, alice.tokens.access_token);
  await mcpCall(ctx, bob.tokens.access_token);
  assert.deepEqual(linksCallers(ctx), ["bob", "alice", "bob"]);

  // After a refresh too, and Bob's client cannot refresh Alice's sign-in.
  const stolen = await refresh(ctx, { clientId: bob.clientId, refreshToken: alice.tokens.refresh_token });
  assert.equal(stolen.status, 400);
  assert.equal((await stolen.json()).error, "invalid_grant");
  const renewed = await (await refresh(ctx, { clientId: alice.clientId, refreshToken: alice.tokens.refresh_token })).json();
  await mcpCall(ctx, renewed.access_token);
  assert.deepEqual(linksCallers(ctx).slice(-1), ["alice"]);
});

test("/mcp accepts POST only, and nothing is logged about who signed in", async (t) => {
  const ctx = await withConnector(t);
  let tokens;
  let grant;
  const logs = await capturingLogs(async () => {
    grant = await getCode(ctx, "alice");
    tokens = await (await redeem(ctx, grant)).json();
    await mcpCall(ctx, tokens.access_token);
    const renewed = await (await refresh(ctx, { clientId: grant.clientId, refreshToken: tokens.refresh_token })).json();
    // Refusals log a line each; they must not carry anything either.
    ctx.backend.gone.add(11);
    assert.equal((await mcpCall(ctx, renewed.access_token)).status, 401);
    await refresh(ctx, { clientId: grant.clientId, refreshToken: renewed.refresh_token });
    ctx.backend.gone.delete(11);
    await redeem(ctx, grant);
    await getCode(ctx, "mallory").catch(() => {});
    await mcpCall(ctx, flipLast(tokens.access_token));
  });
  const secrets = [tokens.access_token, tokens.refresh_token, grant.code, grant.verifier, grant.clientId, "IDTOKEN", "alice", "mallory", "SESSION-", OAUTH_SECRET, SIGNIN_SECRET, "google-client-secret-value", "g-alice"];
  assert.match(logs, /session refused by backend: 401/);
  for (const secret of secrets) assert.ok(!logs.includes(secret), `the log contains ${secret.slice(0, 12)}…: ${logs}`);

  const get = await ctx.get("/mcp", { headers: { Authorization: `Bearer ${tokens.access_token}` } });
  assert.equal(get.status, 405);
  assert.equal(get.headers.get("allow"), "POST");
  assert.equal((await ctx.get("/health")).status, 200);
  assert.equal((await ctx.get("/nothing-here")).status, 404);
});

// --- /token: the authorization code -------------------------------------------------

test("a code needs the PKCE verifier it was issued for", async (t) => {
  const ctx = await withConnector(t);
  const wrong = [
    ["another verifier", { verifier: pkce().verifier }],
    // Refused by the protocol layer before the code is looked at.
    ["no verifier", { verifier: undefined }, "kept"],
    ["an empty verifier", { verifier: "" }],
    ["the challenge itself", null],
    ["a verifier that is too short", { verifier: "abc" }],
  ];
  for (const [what, change, kept] of wrong) {
    const grant = await getCode(ctx, "alice");
    const challenge = crypto.createHash("sha256").update(grant.verifier).digest("base64url");
    const res = await redeem(ctx, { ...grant, ...(change ?? { verifier: challenge }) });
    assert.equal(res.status, 400, what);
    const body = await res.json();
    assert.ok(["invalid_grant", "invalid_request"].includes(body.error), `${what}: ${body.error}`);
    assert.equal(body.access_token, undefined, what);
    const late = await redeem(ctx, grant);
    if (kept) {
      assert.equal(late.status, 200, what);
      continue;
    }
    // A failed attempt uses the code up: the right verifier is too late.
    assert.equal(late.status, 400, what);
    assert.equal((await late.json()).error, "invalid_grant", what);
  }
});

test("a code is bound to its client, its redirect address and this resource", async (t) => {
  const ctx = await withConnector(t);
  const other = (await register(ctx)).body.client_id;
  const cases = [
    ["another client", (g) => ({ ...g, clientId: other }), "invalid_grant"],
    ["another redirect address", (g) => ({ ...g, redirectUri: CLAUDE_COM_CALLBACK }), "invalid_grant"],
    ["another resource", (g) => ({ ...g, resource: "https://other.example/mcp" }), "invalid_target"],
    ["a made-up code", (g) => ({ ...g, code: crypto.randomBytes(32).toString("base64url") }), "invalid_grant"],
  ];
  for (const [what, change, error] of cases) {
    const grant = await getCode(ctx, "alice");
    const res = await redeem(ctx, change(grant));
    assert.equal(res.status, 400, what);
    assert.equal((await res.json()).error, error, what);
  }
  // An unknown client is refused before any code is looked at.
  const grant = await getCode(ctx, "alice");
  const unknown = await redeem(ctx, { ...grant, clientId: flipLast(grant.clientId) });
  assert.equal(unknown.status, 400);
  assert.equal((await unknown.json()).error, "invalid_client");
  // OAuth 2.1 clients send no redirect_uri here; PKCE binds the code.
  const without = await redeem(ctx, { ...grant, redirectUri: undefined, resource: undefined });
  assert.equal(without.status, 200);
});

test("a code works once, and a second use ends the sign-in it produced", async (t) => {
  const ctx = await withConnector(t);
  const grant = await getCode(ctx, "alice");
  const tokens = await (await redeem(ctx, grant)).json();
  assert.equal((await mcpCall(ctx, tokens.access_token)).status, 200);

  const again = await redeem(ctx, grant);
  assert.equal(again.status, 400);
  assert.equal((await again.json()).error, "invalid_grant");

  // Whoever redeemed first may not be the person: those tokens are dead now.
  const call = await mcpCall(ctx, tokens.access_token);
  assert.equal(call.status, 401);
  assert.match(call.headers.get("www-authenticate"), /^Bearer error="invalid_token", error_description="[^"]+", resource_metadata="https:\/\/links-mcp\.test\/\.well-known\/oauth-protected-resource\/mcp"$/);
  const refreshed = await refresh(ctx, { clientId: grant.clientId, refreshToken: tokens.refresh_token });
  assert.equal((await refreshed.json()).error, "invalid_grant");
});

test("a code expires after a minute", async (t) => {
  const ctx = await withConnector(t);
  const fresh = await getCode(ctx, "alice");
  ctx.now.advance(CODE_SECONDS - 1);
  assert.equal((await redeem(ctx, fresh)).status, 200);

  const stale = await getCode(ctx, "alice");
  ctx.now.advance(CODE_SECONDS + 1);
  const res = await redeem(ctx, stale);
  assert.equal(res.status, 400);
  assert.equal((await res.json()).error, "invalid_grant");
  assert.ok(CODE_SECONDS <= 60);
});

// --- /token: refresh ------------------------------------------------------------------

test("a refresh rotates the token, renews the backend session and keeps the person", async (t) => {
  const ctx = await withConnector(t);
  const { clientId, tokens } = await signIn(ctx, "bob");
  const sessionsBefore = ctx.backend.sessions.size;

  const res = await refresh(ctx, { clientId, refreshToken: tokens.refresh_token, headers: { "CF-Connecting-IP": "160.79.104.10" } });
  assert.equal(res.status, 200);
  assert.equal(res.headers.get("cache-control"), "no-store");
  const renewed = await res.json();
  assert.notEqual(renewed.refresh_token, tokens.refresh_token);
  assert.notEqual(renewed.access_token, tokens.access_token);
  assert.equal(renewed.expires_in, ACCESS_TOKEN_SECONDS);

  // The backend was asked, with a short-lived assertion for Bob at the token
  // version seen at sign-in, and made a new session.
  const [renewal] = ctx.backend.calls("/auth/connector/session");
  assert.equal(renewal.headers["cf-connecting-ip"], "160.79.104.10");
  assert.equal(renewal.headers["x-forwarded-proto"], "https");
  const claims = JSON.parse(Buffer.from(JSON.parse(renewal.body).assertion.split(".")[1], "base64url"));
  assert.equal(claims.sub, "22");
  assert.equal(claims.tv, 3);
  assert.equal(claims.aud, "opn-connector-session");
  assert.ok(claims.exp - claims.iat <= 60 && claims.exp - claims.iat > 0);
  assert.equal(ctx.backend.sessions.size, sessionsBefore + 1);

  await mcpCall(ctx, renewed.access_token);
  const [linksCall] = ctx.backend.calls("/links");
  const used = linksCall.headers.authorization.replace("Bearer ", "");
  assert.equal(ctx.backend.sessions.get(used), "bob");
  assert.equal([...ctx.backend.sessions.keys()].indexOf(used), sessionsBefore, "the new access token carries the renewed session");

  // The chain goes on.
  const next = await refresh(ctx, { clientId, refreshToken: renewed.refresh_token });
  assert.equal(next.status, 200);
});

test("using a rotated refresh token again ends the whole sign-in", async (t) => {
  const ctx = await withConnector(t);
  const { clientId, tokens } = await signIn(ctx, "alice");
  const renewed = await (await refresh(ctx, { clientId, refreshToken: tokens.refresh_token })).json();
  assert.equal((await mcpCall(ctx, renewed.access_token)).status, 200);

  const replay = await refresh(ctx, { clientId, refreshToken: tokens.refresh_token });
  assert.equal(replay.status, 400);
  const body = await replay.json();
  assert.equal(body.error, "invalid_grant");
  assert.equal(body.access_token, undefined);

  // The tokens the first use produced die with it.
  assert.equal((await mcpCall(ctx, renewed.access_token)).status, 401);
  assert.equal((await mcpCall(ctx, tokens.access_token)).status, 401);
  const dead = await refresh(ctx, { clientId, refreshToken: renewed.refresh_token });
  assert.equal((await dead.json()).error, "invalid_grant");

  // Somebody else's sign-in is untouched.
  const bob = await signIn(ctx, "bob");
  assert.equal((await mcpCall(ctx, bob.tokens.access_token)).status, 200);
});

test("two uses of one refresh token at the same moment do not both succeed", async (t) => {
  const ctx = await withConnector(t);
  const { clientId, tokens } = await signIn(ctx, "alice");
  const [a, b] = await Promise.all([refresh(ctx, { clientId, refreshToken: tokens.refresh_token }), refresh(ctx, { clientId, refreshToken: tokens.refresh_token })]);
  assert.deepEqual([a.status, b.status].sort(), [200, 400]);
  const winner = await (a.status === 200 ? a : b).json();
  assert.equal((await mcpCall(ctx, winner.access_token)).status, 401, "the sign-in ended");
});

test("someone the backend no longer renews gets no new token and loses the old ones", async (t) => {
  const ctx = await withConnector(t);
  const { clientId, tokens } = await signIn(ctx, "alice");
  ctx.backend.gone.add(11);

  // Tools stop at once: the backend refuses the session inside the token.
  const call = await mcpCall(ctx, tokens.access_token);
  assert.equal(call.status, 401);
  assert.match(call.headers.get("www-authenticate"), /error="invalid_token"/);
  assert.equal(ctx.backend.calls("/links").length, 0);

  const res = await refresh(ctx, { clientId, refreshToken: tokens.refresh_token });
  assert.equal(res.status, 400);
  const body = await res.json();
  assert.equal(body.error, "invalid_grant");
  assert.equal(body.access_token, undefined);
  assert.equal(body.refresh_token, undefined);

  // Even if the account came back, this sign-in is over: only Google sign-in starts another.
  ctx.backend.gone.delete(11);
  assert.equal((await mcpCall(ctx, tokens.access_token)).status, 401);
  assert.equal((await refresh(ctx, { clientId, refreshToken: tokens.refresh_token })).status, 400);
});

test("a refresh is refused when the backend refuses the renewal, whatever the token says", async (t) => {
  const ctx = await withConnector(t);
  for (const [who, change] of [
    ["alice", () => ctx.backend.gone.add(11)],
    ["bob", () => (ctx.backend.people.bob.token_version += 1)],
  ]) {
    const { clientId, tokens } = await signIn(ctx, who);
    change();
    const before = ctx.backend.calls("/auth/connector/session").length;
    const res = await refresh(ctx, { clientId, refreshToken: tokens.refresh_token });
    assert.equal(res.status, 400, who);
    assert.equal((await res.json()).error, "invalid_grant", who);
    assert.equal(ctx.backend.calls("/auth/connector/session").length, before + 1, "the backend was asked");
  }
});

test("a backend that is down does not burn the refresh token or end the sign-in", async (t) => {
  const ctx = await withConnector(t);
  const { clientId, tokens } = await signIn(ctx, "alice");
  for (const breakIt of [() => (ctx.backend.down = true), () => (ctx.backend.sessionStatus = 500), () => (ctx.backend.sessionStatus = 429), () => (ctx.backend.sessionStatus = 404)]) {
    breakIt();
    const res = await capturingLogs(async () => {
      const failed = await refresh(ctx, { clientId, refreshToken: tokens.refresh_token });
      assert.equal(failed.status, 500);
      const body = await failed.json();
      assert.equal(body.error, "server_error");
      assert.equal(body.access_token, undefined);
    });
    assert.match(res, /session renewal/);
    ctx.backend.down = false;
    ctx.backend.sessionStatus = undefined;
  }
  const ok = await refresh(ctx, { clientId, refreshToken: tokens.refresh_token });
  assert.equal(ok.status, 200);
});

test("a backend answer for someone else is not turned into a token", async (t) => {
  const ctx = await withConnector(t);
  const { clientId, tokens } = await signIn(ctx, "alice");
  // A backend that answers every renewal with Bob's session.
  ctx.backend.sessionAs = "bob";
  const logs = await capturingLogs(async () => {
    const res = await refresh(ctx, { clientId, refreshToken: tokens.refresh_token });
    assert.equal(res.status, 500);
    assert.equal((await res.json()).access_token, undefined);
  });
  assert.match(logs, /unexpected answer/);
  assert.equal(ctx.backend.calls("/auth/connector/session").length, 1);
});

test("the sign-in ends a fixed time after the Google sign-in, however often it is refreshed", async (t) => {
  const ctx = await withConnector(t, { env: { MCP_SESSION_HOURS: "3" } });
  let { clientId, tokens } = await signIn(ctx, "alice");

  // Refresh every 50 minutes: 2 h 30 in, everything still works.
  for (let i = 0; i < 3; i++) {
    ctx.now.advance(50 * 60);
    const res = await refresh(ctx, { clientId, refreshToken: tokens.refresh_token });
    assert.equal(res.status, 200, `refresh ${i}`);
    tokens = await res.json();
  }
  // The last access token is cut to the 30 minutes that are left.
  assert.ok(tokens.expires_in <= 30 * 60 && tokens.expires_in >= 30 * 60 - 2, String(tokens.expires_in));
  ctx.now.advance(29 * 60);
  assert.equal((await mcpCall(ctx, tokens.access_token)).status, 200);

  ctx.now.advance(2 * 60);
  assert.equal((await mcpCall(ctx, tokens.access_token)).status, 401);
  const before = ctx.backend.calls("/auth/connector/session").length;
  const res = await refresh(ctx, { clientId, refreshToken: tokens.refresh_token });
  assert.equal(res.status, 400);
  assert.equal((await res.json()).error, "invalid_grant");
  assert.equal(ctx.backend.calls("/auth/connector/session").length, before, "the backend is not even asked");
});

test("an access token lasts an hour", async (t) => {
  const ctx = await withConnector(t);
  const { tokens } = await signIn(ctx, "alice");
  ctx.now.advance(ACCESS_TOKEN_SECONDS - 5);
  assert.equal((await mcpCall(ctx, tokens.access_token)).status, 200);
  ctx.now.advance(10);
  const res = await mcpCall(ctx, tokens.access_token);
  assert.equal(res.status, 401);
  assert.match(res.headers.get("www-authenticate"), /error="invalid_token"/);
  assert.ok(ACCESS_TOKEN_SECONDS <= 3600);
});

// --- tokens that are not ours ---------------------------------------------------------

test("tampered, mistyped and foreign tokens are refused", async (t) => {
  const google = await startGoogle();
  const now = clock();
  const backend = await startBackend({ now });
  const ctx = await withConnector(t, { google, backend, now });
  const { clientId, tokens } = await signIn(ctx, "alice");
  const body = tokens.access_token.split(".")[1];
  const middle = Math.floor(body.length / 2);
  const refusedAccess = {
    "last character changed": flipLast(tokens.access_token),
    "middle character changed": `lnpa1.${body.slice(0, middle)}${body[middle] === "A" ? "B" : "A"}${body.slice(middle + 1)}`,
    truncated: tokens.access_token.slice(0, -8),
    "a refresh token": tokens.refresh_token,
    "a refresh token relabelled": tokens.refresh_token.replace("lnpr1.", "lnpa1."),
    "a client id": clientId,
    "the backend's own session": [...backend.sessions.keys()][0],
    "sealed with another secret": new Sealer("Zx9cVb2nMq5wEr8tYu1iOp4aSd7fGh0jKl3zXc6v").seal("access", { s: "s", u: 11, a: RESOURCE, e: now() + 60000, x: now() + 60000, b: "x" }),
    "another version prefix": tokens.access_token.replace("lnpa1.", "lnpa2."),
    empty: "",
    garbage: "not-a-token",
    huge: `lnpa1.${"A".repeat(10000)}`,
  };
  for (const [what, token] of Object.entries(refusedAccess)) {
    const res = await mcpCall(ctx, undefined, { headers: { Authorization: `Bearer ${token}` } });
    assert.equal(res.status, 401, what);
    assert.match(res.headers.get("www-authenticate"), /resource_metadata=/, what);
  }
  for (const header of [tokens.access_token, `Basic ${tokens.access_token}`, `Bearer  ${tokens.access_token}`, `Bearer ${tokens.access_token} extra`]) {
    const res = await mcpCall(ctx, undefined, { headers: { Authorization: header } });
    assert.equal(res.status, 401, header.slice(0, 12));
  }
  assert.equal(backend.calls("/links").length, 0);

  for (const [what, token] of Object.entries({
    tampered: flipLast(tokens.refresh_token),
    "an access token": tokens.access_token,
    "an access token relabelled": tokens.access_token.replace("lnpa1.", "lnpr1."),
    garbage: "not-a-token",
  })) {
    const res = await refresh(ctx, { clientId, refreshToken: token });
    assert.equal(res.status, 400, what);
    assert.equal((await res.json()).error, "invalid_grant", what);
  }
  // The real ones still work: nothing above ended the sign-in.
  assert.equal((await mcpCall(ctx, tokens.access_token)).status, 200);
});

test("a token issued for another resource is refused, even under the same secret", async (t) => {
  const google = await startGoogle();
  const now = clock();
  const backend = await startBackend({ now });
  const ctx = await withConnector(t, { google, backend, now });
  // Another deployment that shares the secret, e.g. a staging copy.
  const other = await startConnector({ google, backend, now, env: { MCP_PUBLIC_URL: "https://other-mcp.test" } });
  t.after(() => other.close());
  const otherClient = (await other.postJson("/register", { redirect_uris: [CLAUDE_CALLBACK] }).then((r) => r.json())).client_id;
  const authorized = await authorize(other, { clientId: otherClient, challenge: pkce().challenge, extra: { resource: "https://other-mcp.test/mcp" } });
  assert.equal(authorized.status, 302);

  const foreign = other.oauth.internals.mint({ sid: "sid-1", userId: 11, clientTag: "c", sessionEnd: now() + 3600_000, tokenVersion: 0 }, [...backend.sessions.keys()][0] ?? "SESSION");
  assert.equal((await mcpCall(other, foreign.access_token)).status, 401, "control: no such session at the backend");
  const signedIn = await signIn(ctx, "alice");
  const session = [...backend.sessions.keys()][0];
  // For the very client that asks here, so only the audience tells it apart.
  const forOther = other.oauth.internals.mint({ sid: "sid-2", userId: 11, clientTag: ctx.oauth.internals.clientTag(signedIn.clientId), sessionEnd: now() + 3600_000, tokenVersion: 0 }, session);
  // It is a perfectly good token where it was issued...
  assert.equal((await mcpCall(other, forOther.access_token)).status, 200);
  // ...and not here.
  const res = await mcpCall(ctx, forOther.access_token);
  assert.equal(res.status, 401);
  const refreshed = await refresh(ctx, { clientId: signedIn.clientId, refreshToken: forOther.refresh_token });
  assert.equal((await refreshed.json()).error, "invalid_grant");
  // And this server issues nothing for another resource.
  const wrongTarget = await refresh(ctx, { clientId: signedIn.clientId, refreshToken: signedIn.tokens.refresh_token, resource: "https://other-mcp.test/mcp" });
  assert.equal((await wrongTarget.json()).error, "invalid_target");
});

test("a token in the query string is not a token", async (t) => {
  const ctx = await withConnector(t);
  const { tokens } = await signIn(ctx, "alice");
  for (const path of [`/mcp?access_token=${tokens.access_token}`, `/mcp?token=${tokens.access_token}`, `/mcp?authorization=Bearer%20${tokens.access_token}`]) {
    const res = await mcpCall(ctx, undefined, { path });
    assert.equal(res.status, 401, path.slice(0, 20));
    assert.equal(res.headers.get("www-authenticate"), `Bearer resource_metadata="${ISSUER}/.well-known/oauth-protected-resource/mcp"`);
  }
  // Nor in the body of the call.
  const res = await fetch(`${ctx.url}/mcp`, {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "application/json, text/event-stream" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list", params: {}, access_token: tokens.access_token }),
  });
  assert.equal(res.status, 401);
  assert.equal(ctx.backend.calls("/auth/me").length, 0);
});

// --- the Google round trip ------------------------------------------------------------

test("a Google state works once, in the browser that started the sign-in, for ten minutes", async (t) => {
  const ctx = await withConnector(t);
  const clientId = (await register(ctx)).body.client_id;
  const start = () => authorize(ctx, { clientId, challenge: pkce().challenge });
  const refusedPage = async (res, what) => {
    assert.equal(res.status, 400, what);
    assert.equal(res.headers.get("location"), null, what);
    assert.match(await res.text(), /expired/, what);
  };

  // Used twice.
  let authorized = await start();
  assert.equal((await googleCallback(ctx, authorized, "alice")).status, 302);
  await refusedPage(await googleCallback(ctx, authorized, "alice"), "state reuse");

  // A state this server never issued, and none at all.
  authorized = await start();
  await refusedPage(await googleCallback(ctx, authorized, "alice", { state: crypto.randomBytes(32).toString("base64url") }), "unknown state");
  await refusedPage(await ctx.get("/oauth/google/callback?code=g-alice"), "no state");
  await refusedPage(await ctx.get("/oauth/google/callback?code=g-alice&state=a&state=b"), "two states");

  // Another browser: no cookie, or the cookie of another sign-in.
  authorized = await start();
  await refusedPage(await googleCallback(ctx, authorized, "alice", { cookie: null }), "no cookie");
  authorized = await start();
  const otherBrowser = cookieFrom(await start());
  await refusedPage(await googleCallback(ctx, authorized, "alice", { cookie: otherBrowser }), "another browser's cookie");
  // A refused attempt used the state up: the right browser is too late.
  await refusedPage(await googleCallback(ctx, authorized, "alice"), "state burned by the refused attempt");

  // Too slow.
  authorized = await start();
  ctx.now.advance(PENDING_SECONDS + 1);
  await refusedPage(await googleCallback(ctx, authorized, "alice"), "expired");

  // None of this reached Google's token endpoint more than the one good time, or made a code.
  assert.equal(ctx.google.forms.length, 1);
  assert.equal(ctx.oauth.internals.codes.size, 1);

  // Two tabs of one browser may both finish.
  const first = await start();
  const second = await authorize(ctx, { clientId, challenge: pkce().challenge, cookie: cookieFrom(first) });
  assert.equal(cookieFrom(second), cookieFrom(first));
  assert.equal((await googleCallback(ctx, second, "bob")).status, 302);
  assert.equal((await googleCallback(ctx, first, "alice")).status, 302);
});

test("the nonce sent to Google must come back in the ID token", async (t) => {
  const ctx = await withConnector(t);
  const clientId = (await register(ctx)).body.client_id;
  const authorized = await authorize(ctx, { clientId, challenge: pkce().challenge });
  const state = new URL(authorized.headers.get("location")).searchParams.get("state");
  // Google answers with an ID token made for some other request.
  ctx.google.nonces.set("g-alice", "the-nonce-of-another-sign-in");
  const res = await ctx.get(`/oauth/google/callback?code=g-alice&state=${state}`, { headers: { Cookie: cookieFrom(authorized) } });
  assert.equal(res.status, 401);
  assert.equal(res.headers.get("location"), null);
  assert.equal(ctx.oauth.internals.codes.size, 0);
  const [call] = ctx.backend.calls("/auth/connector/google");
  assert.equal(JSON.parse(call.body).nonce, new URL(authorized.headers.get("location")).searchParams.get("nonce"));
});

test("someone the backend refuses gets a page and no code", async (t) => {
  const ctx = await withConnector(t);
  const clientId = (await register(ctx)).body.client_id;
  for (const [what, arrange, status] of [
    ["an account that is not LNP's", () => {}, 403],
    ["a disabled account", () => (ctx.backend.googleStatus = 403), 403],
    ["a token the backend cannot verify", () => (ctx.backend.googleStatus = 401), 401],
    ["a backend that is rate limiting", () => (ctx.backend.googleStatus = 429), 429],
    ["a backend that fails", () => (ctx.backend.googleStatus = 500), 502],
    ["a backend with sign-in switched off", () => (ctx.backend.googleStatus = 404), 502],
    ["a backend that is down", () => (ctx.backend.down = true), 502],
  ]) {
    ctx.backend.googleStatus = undefined;
    ctx.backend.down = false;
    arrange();
    const authorized = await authorize(ctx, { clientId, challenge: pkce().challenge, state: "st-9" });
    const res = await googleCallback(ctx, authorized, what.startsWith("an account that is not") ? "mallory" : "alice");
    assert.equal(res.status, status, what);
    assert.equal(res.headers.get("location"), null, what);
    assert.equal(res.headers.get("cache-control"), "no-store", what);
    assert.equal(res.headers.get("referrer-policy"), "no-referrer", what);
    const html = await res.text();
    // The only way on is back to Claude, with an error and no code.
    const [, href] = /<a href="([^"]+)"/.exec(html);
    const link = new URL(href.replaceAll("&amp;", "&"));
    assert.equal(`${link.origin}${link.pathname}`, CLAUDE_CALLBACK, what);
    assert.equal(link.searchParams.get("error"), "access_denied", what);
    assert.equal(link.searchParams.get("state"), "st-9", what);
    assert.equal(link.searchParams.get("code"), null, what);
    assert.equal(ctx.oauth.internals.codes.size, 0, what);
  }
});

test("a Google step that fails or is cancelled makes no code", async (t) => {
  const ctx = await withConnector(t);
  const clientId = (await register(ctx)).body.client_id;
  const start = () => authorize(ctx, { clientId, challenge: pkce().challenge });

  // Cancelled on Google's page: straight back to Claude as access_denied.
  let authorized = await start();
  let state = new URL(authorized.headers.get("location")).searchParams.get("state");
  let res = await ctx.get(`/oauth/google/callback?error=access_denied&state=${state}`, { headers: { Cookie: cookieFrom(authorized) } });
  assert.equal(res.status, 302);
  const back = new URL(res.headers.get("location"));
  assert.equal(`${back.origin}${back.pathname}`, CLAUDE_CALLBACK);
  assert.equal(back.searchParams.get("error"), "access_denied");
  assert.equal(back.searchParams.get("code"), null);
  // An error with a code attached is still an error.
  authorized = await start();
  state = new URL(authorized.headers.get("location")).searchParams.get("state");
  res = await ctx.get(`/oauth/google/callback?error=x&code=g-alice&state=${state}`, { headers: { Cookie: cookieFrom(authorized) } });
  assert.equal(new URL(res.headers.get("location")).searchParams.get("code"), null);

  for (const breakGoogle of [() => (ctx.google.status = 400), () => (ctx.google.omitIdToken = true)]) {
    ctx.google.status = 200;
    ctx.google.omitIdToken = false;
    breakGoogle();
    const logs = await capturingLogs(async () => {
      res = await googleCallback(ctx, await start(), "alice");
    });
    assert.equal(res.status, 502);
    assert.equal(res.headers.get("location"), null);
    assert.match(logs, /google code exchange/);
  }
  assert.equal(ctx.backend.calls("/auth/connector/google").length, 0);
  assert.equal(ctx.oauth.internals.codes.size, 0);
});

// --- limits ---------------------------------------------------------------------------

test("an expiring map never holds more than its cap", () => {
  const now = clock();
  const map = new ExpiringMap(3, now);
  for (const key of ["a", "b", "c"]) assert.equal(map.set(key, key, now() + 1000), true);
  assert.equal(map.set("d", "d", now() + 1000), false, "full of live entries");
  assert.equal(map.size, 3);
  assert.equal(map.set("a", "again", now() + 1000), true, "an existing key is not a new entry");
  now.advance(2);
  assert.equal(map.get("a"), undefined, "expired");
  // "b" and "c" have expired too but are still held: the next entry that
  // does not fit sweeps them out instead of being refused.
  assert.equal(map.set("d", "d", now() + 1000), true);
  assert.equal(map.set("e", "e", now() + 1000), true, "expired entries make room");
  assert.equal(map.size, 2);
  assert.equal(map.set("f", "f", now() + 1000), true);
  assert.equal(map.set("g", "g", now() + 1000), false, "full again");
  assert.equal(map.size, 3);
  assert.equal(map.take("d"), "d");
  assert.equal(map.take("d"), undefined);
});

test("pending sign-ins are capped, and expire", async (t) => {
  const limits = { pending: 3, codes: 2, spentCodes: 10, spentRefreshTokens: 10, endedSignIns: 10, rateKeys: 100 };
  const ctx = await withConnector(t, { options: { limits } });
  const clientId = (await register(ctx)).body.client_id;
  const start = (n) => authorize(ctx, { clientId, challenge: pkce().challenge, state: `s${n}` });
  const kept = [];
  for (let n = 0; n < 3; n++) {
    const res = await start(n);
    assert.equal(new URL(res.headers.get("location")).hostname, "accounts.google.com");
    kept.push(res);
  }
  for (let n = 3; n < 8; n++) {
    const res = await start(n);
    const location = new URL(res.headers.get("location"));
    assert.equal(location.hostname, "claude.ai", "refused, not queued");
    assert.equal(location.searchParams.get("error"), "temporarily_unavailable");
  }
  assert.equal(ctx.oauth.internals.pending.size, 3);

  // Codes are capped too: the third finished sign-in gets a page, not a code.
  assert.equal((await googleCallback(ctx, kept[0], "alice")).status, 302);
  assert.equal((await googleCallback(ctx, kept[1], "bob")).status, 302);
  assert.equal((await googleCallback(ctx, kept[2], "alice")).status, 503);
  assert.equal(ctx.oauth.internals.codes.size, 2);

  ctx.now.advance(PENDING_SECONDS + 1);
  assert.equal(new URL((await start(9)).headers.get("location")).hostname, "accounts.google.com");
  assert.equal(ctx.oauth.internals.pending.size, 1);
});

test("spent refresh tokens are capped: a full list refuses the refresh rather than forgetting", async (t) => {
  const limits = { pending: 10, codes: 10, spentCodes: 10, spentRefreshTokens: 2, endedSignIns: 10, rateKeys: 100 };
  const ctx = await withConnector(t, { options: { limits } });
  let { clientId, tokens } = await signIn(ctx, "alice");
  for (let i = 0; i < 2; i++) tokens = await (await refresh(ctx, { clientId, refreshToken: tokens.refresh_token })).json();
  const res = await refresh(ctx, { clientId, refreshToken: tokens.refresh_token });
  assert.equal(res.status, 500);
  assert.equal(ctx.oauth.internals.spentRefreshTokens.size, 2);
});

test("the open endpoints are rate limited per address, in bounded memory", async (t) => {
  const rateLimits = { register: { max: 3, windowSeconds: 60 }, authorize: { max: 2, windowSeconds: 60 }, callback: { max: 2, windowSeconds: 60 }, token: { max: 2, windowSeconds: 60 } };
  const limits = { pending: 100, codes: 10, spentCodes: 10, spentRefreshTokens: 10, endedSignIns: 10, rateKeys: 50 };
  const ctx = await withConnector(t, { options: { rateLimits, limits } });
  const from = (ip) => ({ "CF-Connecting-IP": ip });
  const reg = (ip) => ctx.postJson("/register", { redirect_uris: [CLAUDE_CALLBACK] }, from(ip));

  for (let i = 0; i < 3; i++) assert.equal((await reg("198.51.100.1")).status, 201);
  const limited = await reg("198.51.100.1");
  assert.equal(limited.status, 429);
  assert.equal((await limited.json()).client_id, undefined);
  assert.ok(Number(limited.headers.get("retry-after")) >= 1);
  const clientId = (await (await reg("198.51.100.2")).json()).client_id;
  assert.ok(clientId, "another address has its own budget");

  const auth = (ip) => ctx.get(`/authorize?client_id=${encodeURIComponent(clientId)}`, { headers: from(ip) });
  await auth("198.51.100.3");
  await auth("198.51.100.3");
  const page = await auth("198.51.100.3");
  assert.equal(page.status, 429);
  assert.match(page.headers.get("content-type"), /^text\/html/);

  const callback = () => ctx.get("/oauth/google/callback?state=x", { headers: from("198.51.100.4") });
  assert.equal((await callback()).status, 400);
  assert.equal((await callback()).status, 400);
  assert.equal((await callback()).status, 429);

  const token = () => ctx.postForm("/token", { grant_type: "refresh_token", client_id: clientId, refresh_token: "x" }, from("198.51.100.5"));
  assert.equal((await token()).status, 400);
  assert.equal((await token()).status, 400);
  assert.equal((await token()).status, 429);

  // The window passes.
  ctx.now.advance(61);
  assert.equal((await reg("198.51.100.1")).status, 201);

  // Any number of addresses: the counters stop at the cap and strangers share one.
  for (let i = 0; i < 120; i++) await reg(`203.0.113.${i}`);
  assert.ok(ctx.oauth.internals.limiter.windows.size <= 51, `${ctx.oauth.internals.limiter.windows.size} counters`);
  const overflow = new RateLimiter(ctx.now, 2);
  const rule = { max: 2, windowSeconds: 60 };
  assert.equal(overflow.check("b", "1", rule), 0);
  assert.equal(overflow.check("b", "2", rule), 0);
  assert.equal(overflow.check("b", "3", rule), 0);
  assert.equal(overflow.check("b", "4", rule), 0);
  assert.ok(overflow.check("b", "5", rule) > 0, "the shared overflow counter fills");
  assert.equal(overflow.check("b", "1", rule), 0, "a known address keeps its own");
});

// --- pieces ---------------------------------------------------------------------------

test("sealed values are bound to their kind, their secret and their bytes", () => {
  const sealer = new Sealer(OAUTH_SECRET);
  const sealed = sealer.seal("access", { u: 1 });
  assert.deepEqual(sealer.unseal("access", sealed), { u: 1 });
  assert.notEqual(sealer.seal("access", { u: 1 }), sealed, "a fresh nonce each time");
  assert.equal(sealer.unseal("refresh", sealed), undefined);
  assert.equal(sealer.unseal("refresh", sealed.replace("lnpa1", "lnpr1")), undefined);
  assert.equal(sealer.unseal("client", sealed.replace("lnpa1", "lnpc1")), undefined);
  assert.equal(new Sealer(SIGNIN_SECRET).unseal("access", sealed), undefined);
  // Each kind has a key of its own, besides being named in what is sealed.
  const keys = Object.values(sealer.keys).map((key) => key.toString("hex"));
  assert.equal(new Set(keys).size, 3);
  for (let i = 6; i < sealed.length; i += 7) {
    const changed = sealed.slice(0, i) + (sealed[i] === "A" ? "B" : "A") + sealed.slice(i + 1);
    // The last character of a base64url string can carry unused bits.
    if (i === sealed.length - 1) continue;
    assert.equal(sealer.unseal("access", changed), undefined, `byte ${i}`);
  }
  for (const junk of [undefined, null, 7, "", "lnpa1.", "lnpa1.!!!!", "lnpa1.AAAA", `lnpa1.${"A".repeat(5000)}`]) assert.equal(sealer.unseal("access", junk), undefined);
});

test("a renewal assertion is an HS256 JWT for the backend, valid half a minute", () => {
  const nowMs = 1_800_000_000_000;
  const assertion = signAssertion(SIGNIN_SECRET, 42, 7, nowMs);
  const [header, payload, signature] = assertion.split(".");
  assert.deepEqual(JSON.parse(Buffer.from(header, "base64url")), { alg: "HS256", typ: "JWT" });
  assert.deepEqual(JSON.parse(Buffer.from(payload, "base64url")), { aud: "opn-connector-session", sub: "42", tv: 7, iat: 1_800_000_000, exp: 1_800_000_030 });
  assert.equal(signature, crypto.createHmac("sha256", SIGNIN_SECRET).update(`${header}.${payload}`).digest("base64url"));
  assert.notEqual(signAssertion(OAUTH_SECRET, 42, 7, nowMs).split(".")[2], signature);
});
