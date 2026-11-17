// The connector's sign-in, end to end, against local stand-ins for Google and
// the backend. Each test starts its own connector.

import assert from "node:assert/strict";
import crypto from "node:crypto";
import { test } from "node:test";
import {
  ACCESS_TOKEN_SECONDS,
  CODE_RETRY_SECONDS,
  CODE_SECONDS,
  CONFIRM_TEXT,
  CONFIRM_TITLE,
  ExpiringMap,
  LIMITS,
  PENDING_SECONDS,
  RATE_LIMITS,
  REFRESH_RETRY_SECONDS,
  RateLimiter,
  Sealer,
  escapeHtml,
  rateKey,
  signAssertion,
} from "../oauth.mjs";
import {
  CLAUDE_CALLBACK,
  CLAUDE_COM_CALLBACK,
  ISSUER,
  OAUTH_SECRET,
  RESOURCE,
  SIGNIN_SECRET,
  authorize,
  authorizePage,
  capturingLogs,
  clock,
  confirmationFrom,
  cookieFrom,
  getCode,
  googleCallback,
  mcpCall,
  pkce,
  pressButton,
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
  assert.equal(res.headers.get("pragma"), "no-cache");
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

  // Later than a client's own retry would come.
  ctx.now.advance(CODE_RETRY_SECONDS + 1);
  const again = await redeem(ctx, grant);
  assert.equal(again.status, 400);
  assert.equal(again.headers.get("pragma"), "no-cache");
  assert.equal((await again.json()).error, "invalid_grant");

  // Whoever redeemed first may not be the person: those tokens are dead now.
  const call = await mcpCall(ctx, tokens.access_token);
  assert.equal(call.status, 401);
  assert.match(call.headers.get("www-authenticate"), /^Bearer error="invalid_token", error_description="[^"]+", resource_metadata="https:\/\/links-mcp\.test\/\.well-known\/oauth-protected-resource\/mcp"$/);
  const refreshed = await refresh(ctx, { clientId: grant.clientId, refreshToken: tokens.refresh_token });
  assert.equal((await refreshed.json()).error, "invalid_grant");
});

test("the same client sending its code again within seconds is refused, and stays signed in", async (t) => {
  const ctx = await withConnector(t);
  const other = (await register(ctx)).body.client_id;
  const stillSignedIn = async (tokens) => (await mcpCall(ctx, tokens.access_token)).status;

  // The client's own retry: same client, same verifier, a moment later.
  let grant = await getCode(ctx, "alice");
  let tokens = await (await redeem(ctx, grant)).json();
  ctx.now.advance(CODE_RETRY_SECONDS - 3);
  const retry = await redeem(ctx, grant);
  assert.equal(retry.status, 400);
  const body = await retry.json();
  assert.equal(body.error, "invalid_grant");
  assert.equal(body.access_token, undefined, "the retry gets no tokens of its own");
  assert.equal(await stillSignedIn(tokens), 200);
  // The same request once the few seconds have passed is a replay.
  ctx.now.advance(4);
  assert.equal((await redeem(ctx, grant)).status, 400);
  assert.equal(await stillSignedIn(tokens), 401);

  // Two at the very same moment: one answer, and the sign-in survives the other.
  grant = await getCode(ctx, "alice");
  const [a, b] = await Promise.all([redeem(ctx, grant), redeem(ctx, grant)]);
  assert.deepEqual([a.status, b.status].sort(), [200, 400]);
  assert.equal(await stillSignedIn(await (a.status === 200 ? a : b).json()), 200);

  // Within the same few seconds, anything but the same client with the right
  // verifier is a replay and ends the sign-in.
  for (const [what, change] of [
    ["another verifier", (g) => ({ ...g, verifier: pkce().verifier })],
    ["the challenge in place of the verifier", (g) => ({ ...g, verifier: crypto.createHash("sha256").update(g.verifier).digest("base64url") })],
    ["another client", (g) => ({ ...g, clientId: other })],
  ]) {
    grant = await getCode(ctx, "alice");
    tokens = await (await redeem(ctx, grant)).json();
    assert.equal(await stillSignedIn(tokens), 200, what);
    assert.equal((await redeem(ctx, change(grant))).status, 400, what);
    assert.equal(await stillSignedIn(tokens), 401, what);
  }
  assert.ok(CODE_RETRY_SECONDS <= 10);
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

  // Later than a client's own retry would come.
  ctx.now.advance(REFRESH_RETRY_SECONDS + 1);
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

test("a refresh sent again within seconds is refused, and the sign-in stays", async (t) => {
  const ctx = await withConnector(t);
  const { clientId, tokens } = await signIn(ctx, "alice");
  const renewed = await (await refresh(ctx, { clientId, refreshToken: tokens.refresh_token })).json();

  ctx.now.advance(REFRESH_RETRY_SECONDS - 3);
  const before = ctx.backend.calls("/auth/connector/session").length;
  const retry = await refresh(ctx, { clientId, refreshToken: tokens.refresh_token });
  assert.equal(retry.status, 400);
  const body = await retry.json();
  assert.equal(body.error, "invalid_grant");
  assert.equal(body.access_token, undefined, "the retry gets no tokens of its own");
  assert.equal(ctx.backend.calls("/auth/connector/session").length, before, "and no session from the backend");
  // What the first request produced still works, and the chain goes on.
  assert.equal((await mcpCall(ctx, renewed.access_token)).status, 200);
  const next = await refresh(ctx, { clientId, refreshToken: renewed.refresh_token });
  assert.equal(next.status, 200);

  // The first token once more, after the few seconds: a replay.
  ctx.now.advance(4);
  assert.equal((await refresh(ctx, { clientId, refreshToken: tokens.refresh_token })).status, 400);
  assert.equal((await mcpCall(ctx, renewed.access_token)).status, 401);
  assert.equal((await mcpCall(ctx, (await next.json()).access_token)).status, 401);
  assert.ok(REFRESH_RETRY_SECONDS <= 10);
});

test("two uses of one refresh token at the same moment do not both succeed", async (t) => {
  const ctx = await withConnector(t);
  const { clientId, tokens } = await signIn(ctx, "alice");
  const [a, b] = await Promise.all([refresh(ctx, { clientId, refreshToken: tokens.refresh_token }), refresh(ctx, { clientId, refreshToken: tokens.refresh_token })]);
  assert.deepEqual([a.status, b.status].sort(), [200, 400]);
  const loser = await (a.status === 200 ? b : a).json();
  assert.equal(loser.error, "invalid_grant");
  assert.equal(loser.access_token, undefined);
  // It is the client's own request, twice: the one that won stays signed in.
  const winner = await (a.status === 200 ? a : b).json();
  assert.equal((await mcpCall(ctx, winner.access_token)).status, 200);
  assert.equal((await refresh(ctx, { clientId, refreshToken: winner.refresh_token })).status, 200);
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

test("a full list of pending sign-ins drops its oldest, and never keeps a newcomer out", async (t) => {
  const ctx = await withConnector(t, { options: { limits: { pending: 3, usedConfirmations: 3, codes: 2 } } });
  const clientId = (await register(ctx)).body.client_id;
  const start = (n) => authorize(ctx, { clientId, challenge: pkce().challenge, state: `s${n}` });
  const kept = [];
  for (let n = 0; n < 8; n++) {
    const res = await start(n);
    assert.equal(new URL(res.headers.get("location")).hostname, "accounts.google.com", `sign-in ${n} is not refused`);
    kept.push(res);
  }
  assert.equal(ctx.oauth.internals.pending.size, 3);
  assert.equal(ctx.oauth.internals.usedConfirmations.size, 3);

  // The five that were pushed out have to start again...
  for (const old of kept.slice(0, 5)) {
    const res = await googleCallback(ctx, old, "alice");
    assert.equal(res.status, 400);
    assert.match(await res.text(), /Go back to Claude and connect LNP Links again/);
  }
  // ...the three newest finish. Codes are capped the other way: one already
  // handed to Claude is never dropped, so the third gets a page, not a code.
  assert.equal((await googleCallback(ctx, kept[5], "alice")).status, 302);
  assert.equal((await googleCallback(ctx, kept[6], "bob")).status, 302);
  assert.equal((await googleCallback(ctx, kept[7], "alice")).status, 503);
  assert.equal(ctx.oauth.internals.codes.size, 2);

  ctx.now.advance(PENDING_SECONDS + 1);
  assert.equal(new URL((await start(9)).headers.get("location")).hostname, "accounts.google.com");
});

test("spent refresh tokens are capped: a full list refuses the refresh rather than forgetting", async (t) => {
  const limits = { spentRefreshTokens: 2 };
  const ctx = await withConnector(t, { options: { limits } });
  let { clientId, tokens } = await signIn(ctx, "alice");
  for (let i = 0; i < 2; i++) tokens = await (await refresh(ctx, { clientId, refreshToken: tokens.refresh_token })).json();
  const res = await refresh(ctx, { clientId, refreshToken: tokens.refresh_token });
  assert.equal(res.status, 500);
  assert.equal(ctx.oauth.internals.spentRefreshTokens.size, 2);
});

test("the open endpoints are rate limited per address, in bounded memory", async (t) => {
  const two = { max: 2, windowSeconds: 60 };
  const rateLimits = { register: { max: 3, windowSeconds: 60 }, authorize: two, confirm: two, callback: two, token: two, tokenJunk: two };
  const limits = { rateKeys: 50 };
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

  // The confirmation button has a budget of its own.
  const press = async (ip) => {
    const page = await authorizePage(ctx, { clientId, challenge: pkce().challenge, headers: from(ip) });
    return pressButton(ctx, { confirmation: confirmationFrom(await page.text()), cookie: cookieFrom(page), headers: from("198.51.100.7") });
  };
  assert.equal((await press("198.51.100.8")).status, 302);
  assert.equal((await press("198.51.100.9")).status, 302);
  const pressed = await press("198.51.100.10");
  assert.equal(pressed.status, 429);
  assert.equal(pressed.headers.get("location"), null);

  // The window passes.
  ctx.now.advance(61);
  assert.equal((await reg("198.51.100.1")).status, 201);

  // Any number of addresses: the counters stop at the cap, and nobody is
  // refused for what strangers did.
  for (let i = 0; i < 120; i++) assert.equal((await reg(`203.0.113.${i}`)).status, 201);
  assert.equal(ctx.oauth.internals.limiter.windows.size, 50);
  const limiter = new RateLimiter(ctx.now, 2);
  const rule = { max: 2, windowSeconds: 60 };
  assert.equal(limiter.check("b", "1", rule), 0);
  assert.equal(limiter.check("b", "1", rule), 0);
  assert.ok(limiter.check("b", "1", rule) > 0, "its own budget is spent");
  for (const stranger of ["2", "3", "4", "5", "6", "7"]) assert.equal(limiter.check("b", stranger, rule), 0, "each stranger has a budget of their own");
  assert.equal(limiter.windows.size, 2);
  // Its counter went to make room: it starts again, it is never refused for it.
  assert.equal(limiter.check("b", "1", rule), 0);
});

// --- the confirmation page ------------------------------------------------------------

const PAGE_HEADERS = {
  "content-security-policy": "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'",
  "x-frame-options": "DENY",
  "x-content-type-options": "nosniff",
  "referrer-policy": "no-referrer",
  "cache-control": "no-store",
};

/** Every `&` in the page starts one of the entities escapeHtml writes. */
const everyAmpersandIsEscaped = (html) => !/&(?!amp;|lt;|gt;|quot;|#39;)/.test(html);

test("/authorize asks before anything goes to Google, and keeps nothing for the asking", async (t) => {
  const ctx = await withConnector(t);
  const clientId = (await register(ctx)).body.client_id;
  const { challenge } = pkce();
  const state = `st"><script>alert(1)</script>&x='y'`;
  const page = await authorizePage(ctx, { clientId, challenge, state });
  assert.equal(page.status, 200);
  assert.equal(page.headers.get("location"), null, "a GET never reaches Google");
  assert.match(page.headers.get("content-type"), /^text\/html/);
  for (const [name, value] of Object.entries(PAGE_HEADERS)) assert.equal(page.headers.get(name), value, name);
  assert.match(page.headers.get("set-cookie"), /^__Host-lnp_mcp_signin=[A-Za-z0-9_-]{43}; Path=\/; Max-Age=600; Secure; HttpOnly; SameSite=Lax$/);

  const html = await page.text();
  // The wording, exactly.
  assert.equal(CONFIRM_TITLE, "Connect Claude to LNP Links?");
  assert.deepEqual(CONFIRM_TEXT, [
    "Claude is asking to use LNP Links as you: to create and change short links, and to see their statistics.",
    "Continue only if you yourself just clicked Connect for LNP Links in Claude.",
    "If someone sent you this link, close this page.",
  ]);
  assert.ok(html.includes(`<h1>${CONFIRM_TITLE}</h1>`));
  for (const line of CONFIRM_TEXT) assert.ok(html.includes(`<p>${line}</p>`), line);
  assert.ok(html.includes(`<form method="post" action="/authorize/continue">`));
  assert.ok(html.includes(`<button class="go" type="submit" name="choice" value="continue">Continue with Google</button>`));
  assert.ok(html.includes(`<button type="submit" name="choice" value="cancel">Cancel</button>`));
  // Nothing from outside, no script, no link, and nothing the request said.
  assert.ok(!/<script|<link|<img|<iframe|<a |src=|href=|https?:/i.test(html), html);
  for (const sent of [state, "script>alert", "alert(1)", clientId, challenge, "claude.ai", "claude-state", RESOURCE]) assert.ok(!html.includes(sent), sent);
  assert.match(confirmationFrom(html), /^lnpq1\.[A-Za-z0-9_-]+$/);

  // Nothing is held for a request nobody has confirmed, however many arrive.
  for (let i = 0; i < 20; i++) await authorizePage(ctx, { clientId, challenge: pkce().challenge });
  assert.equal(ctx.oauth.internals.pending.size, 0);
  assert.equal(ctx.oauth.internals.usedConfirmations.size, 0);
  assert.equal(ctx.oauth.internals.codes.size, 0);
  assert.equal(ctx.oauth.internals.limiter.windows.size, 2, "one counter for /register and one for /authorize, for this address");

  // A browser that already has the cookie keeps it (two tabs).
  const second = await authorizePage(ctx, { clientId, challenge: pkce().challenge, cookie: cookieFrom(page) });
  assert.equal(cookieFrom(second), cookieFrom(page));

  // A state too long to carry is refused, back to Claude.
  const long = await authorizePage(ctx, { clientId, challenge, state: "\u0001".repeat(1000) });
  assert.equal(long.status, 302);
  assert.equal(new URL(long.headers.get("location")).searchParams.get("error"), "invalid_request");
});

test("only the page's button, from the browser the page was shown to, starts the Google round trip", async (t) => {
  const ctx = await withConnector(t);
  const clientId = (await register(ctx)).body.client_id;
  const open = async (params = {}) => {
    const page = await authorizePage(ctx, { clientId, challenge: pkce().challenge, state: "st-7 & co", ...params });
    return { confirmation: confirmationFrom(await page.text()), cookie: cookieFrom(page) };
  };
  const refused = async (res, what) => {
    assert.equal(res.status, 400, what);
    assert.equal(res.headers.get("location"), null, what);
    assert.equal(res.headers.get("set-cookie"), null, what);
    for (const [name, value] of Object.entries(PAGE_HEADERS)) assert.equal(res.headers.get(name), value, `${what}: ${name}`);
    assert.match(await res.text(), /This sign-in has expired/, what);
    assert.equal(ctx.oauth.internals.pending.size, 0, `${what}: nothing was started`);
  };

  // Not a GET, whatever it carries.
  let shown = await open();
  const get = await ctx.get(`/authorize/continue?${new URLSearchParams({ confirmation: shown.confirmation, choice: "continue" })}`, { headers: { Cookie: shown.cookie } });
  assert.equal(get.status, 405);
  assert.equal(get.headers.get("allow"), "POST");
  assert.equal(get.headers.get("location"), null);
  assert.equal(ctx.oauth.internals.pending.size, 0);

  // The token without the cookie (a form on another site: SameSite=Lax keeps
  // the cookie back), with another browser's cookie, and no token at all.
  await refused(await pressButton(ctx, { confirmation: shown.confirmation }), "no cookie");
  const otherBrowser = await open();
  await refused(await pressButton(ctx, { confirmation: shown.confirmation, cookie: otherBrowser.cookie }), "another browser's cookie");
  await refused(await pressButton(ctx, { confirmation: otherBrowser.confirmation, cookie: shown.cookie }), "another browser's token");
  await refused(await ctx.postForm("/authorize/continue", { choice: "continue" }, { Cookie: shown.cookie }), "no token");
  await refused(await ctx.postForm("/authorize/continue", `confirmation=${shown.confirmation}&confirmation=${shown.confirmation}&choice=continue`, { Cookie: shown.cookie }), "the token twice");

  // A token this server did not seal as a confirmation.
  const sealer = new Sealer(OAUTH_SECRET);
  const inside = sealer.unseal("confirm", shown.confirmation);
  assert.deepEqual(Object.keys(inside).sort(), ["a", "b", "c", "e", "i", "k", "r", "s"]);
  for (const [what, forged] of Object.entries({
    tampered: flipLast(shown.confirmation),
    "the same thing sealed as an access token": sealer.seal("access", inside),
    "relabelled as a client id": shown.confirmation.replace("lnpq1.", "lnpc1."),
    "sealed with another secret": new Sealer(SIGNIN_SECRET).seal("confirm", inside),
    "sealed for another resource": sealer.seal("confirm", { ...inside, a: "https://other-mcp.test/mcp" }),
    "sealed for a redirect address the client never registered": sealer.seal("confirm", { ...inside, r: CLAUDE_COM_CALLBACK }),
    "sealed for a client that is not one": sealer.seal("confirm", { ...inside, c: "lnpc1.AAAA" }),
    "expired on its face": sealer.seal("confirm", { ...inside, e: ctx.now() - 1 }),
    garbage: "not-a-token",
  })) {
    await refused(await pressButton(ctx, { confirmation: forged, cookie: shown.cookie }), what);
  }

  // Too slow.
  ctx.now.advance(PENDING_SECONDS + 1);
  await refused(await pressButton(ctx, shown), "expired");

  // The real thing. What the form says besides the token is not believed.
  shown = await open();
  const res = await ctx.postForm(
    "/authorize/continue",
    { confirmation: shown.confirmation, choice: "continue", redirect_uri: "https://evil.example/cb", state: "evil", client_id: "evil", code_challenge: "evil" },
    { Cookie: shown.cookie },
  );
  assert.equal(res.status, 302);
  assert.equal(res.headers.get("cache-control"), "no-store");
  const google = new URL(res.headers.get("location"));
  assert.equal(google.hostname, "accounts.google.com");
  assert.ok(!google.href.includes("evil"));
  assert.equal(cookieFrom(res), shown.cookie, "the cookie is renewed for the round trip, not changed");
  assert.equal(ctx.oauth.internals.pending.size, 1);
  // The button works once.
  const twice = await pressButton(ctx, shown);
  assert.equal(twice.status, 400);
  assert.equal(ctx.oauth.internals.pending.size, 1);
  // And the sign-in it started is the one that was checked at /authorize.
  const back = await googleCallback(ctx, res, "alice");
  assert.equal(back.status, 302);
  const location = new URL(back.headers.get("location"));
  assert.equal(`${location.origin}${location.pathname}`, CLAUDE_CALLBACK);
  assert.equal(location.searchParams.get("state"), "st-7 & co");
});

test("Cancel on the confirmation page goes back to Claude as access_denied, and nothing reaches Google", async (t) => {
  const ctx = await withConnector(t);
  const clientId = (await register(ctx, [CLAUDE_COM_CALLBACK])).body.client_id;
  const page = await authorizePage(ctx, { clientId, challenge: pkce().challenge, state: "st 9&x=<y>", redirectUri: CLAUDE_COM_CALLBACK });
  const shown = { confirmation: confirmationFrom(await page.text()), cookie: cookieFrom(page) };

  // Without the cookie, Cancel does nothing either.
  assert.equal((await pressButton(ctx, { confirmation: shown.confirmation, choice: "cancel" })).status, 400);

  // Anything that is not "continue" is a cancel.
  const res = await pressButton(ctx, { ...shown, choice: "something else" });
  assert.equal(res.status, 302);
  assert.equal(res.headers.get("cache-control"), "no-store");
  const back = new URL(res.headers.get("location"));
  assert.equal(`${back.origin}${back.pathname}`, CLAUDE_COM_CALLBACK);
  assert.deepEqual(Object.fromEntries(back.searchParams), { error: "access_denied", error_description: "The connection was cancelled", state: "st 9&x=<y>", iss: ISSUER });
  assert.equal(ctx.oauth.internals.pending.size, 0);

  // The token is used up: Continue is too late.
  const late = await pressButton(ctx, shown);
  assert.equal(late.status, 400);
  assert.equal(ctx.oauth.internals.pending.size, 0);
  assert.equal(ctx.google.forms.length, 0);
});

// --- /mcp ---------------------------------------------------------------------------------

test("/mcp asks who is calling before it reads the body", async (t) => {
  const ctx = await withConnector(t);
  const { tokens } = await signIn(ctx, "alice");
  const post = (body, token) =>
    fetch(`${ctx.url}/mcp`, {
      method: "POST",
      headers: { "Content-Type": "application/json", Accept: "application/json, text/event-stream", ...(token ? { Authorization: `Bearer ${token}` } : {}) },
      body,
    });
  // A body that is not JSON: without a token nobody looks at it.
  for (const token of [undefined, flipLast(tokens.access_token)]) {
    const res = await post("{ this is not JSON", token);
    assert.equal(res.status, 401);
    assert.match(res.headers.get("www-authenticate"), /resource_metadata=/);
  }
  // With one, it is read and refused as a body.
  assert.equal((await post("{ this is not JSON", tokens.access_token)).status, 400);
  assert.equal((await mcpCall(ctx, tokens.access_token)).status, 200);
});

test("/mcp calls without a valid token are rate limited per network; calls with one are not", async (t) => {
  const ctx = await withConnector(t, { options: { rateLimits: { mcpUnauthenticated: { max: 3, windowSeconds: 60 } } } });
  const { tokens } = await signIn(ctx, "alice");
  const from = (ip) => ({ headers: { "CF-Connecting-IP": ip } });
  // Different addresses in one IPv6 /64 are one caller.
  for (const ip of ["2001:db8:5:6::1", "2001:db8:5:6:ffff::2", "2001:db8:5:6::3"]) assert.equal((await mcpCall(ctx, undefined, from(ip))).status, 401);
  const limited = await mcpCall(ctx, flipLast(tokens.access_token), from("2001:db8:5:6:1:2:3:4"));
  assert.equal(limited.status, 429);
  assert.ok(Number(limited.headers.get("retry-after")) >= 1);
  assert.equal((await limited.json()).error.message, "Too many requests; try again later");
  // The person's own calls from that network go on, and are never counted.
  for (let i = 0; i < 5; i++) assert.equal((await mcpCall(ctx, tokens.access_token, from("2001:db8:5:6::1"))).status, 200);
  // The next /64 has its own budget.
  assert.equal((await mcpCall(ctx, undefined, from("2001:db8:5:7::1"))).status, 401);
  ctx.now.advance(61);
  assert.equal((await mcpCall(ctx, undefined, from("2001:db8:5:6::1"))).status, 401);
});

test("a session the backend refuses, or an account it no longer has, ends the sign-in at the next call", async (t) => {
  for (const status of [401, 403, 404]) {
    const ctx = await withConnector(t);
    const { clientId, tokens } = await signIn(ctx, "alice");
    assert.equal((await mcpCall(ctx, tokens.access_token)).status, 200);

    ctx.backend.meStatus = status;
    const logs = await capturingLogs(async () => {
      const call = await mcpCall(ctx, tokens.access_token);
      assert.equal(call.status, 401, `backend ${status}`);
      assert.match(call.headers.get("www-authenticate"), /error="invalid_token", error_description="The sign-in has ended"/);
    });
    assert.match(logs, new RegExp(`session refused by backend: ${status}`));
    const asked = ctx.backend.calls("/auth/me").length;

    // The backend would say yes again, but this sign-in is over, for calls
    // and for refreshes: it is not even asked.
    ctx.backend.meStatus = undefined;
    assert.equal((await mcpCall(ctx, tokens.access_token)).status, 401, `backend ${status}`);
    assert.equal(ctx.backend.calls("/auth/me").length, asked);
    const refreshed = await refresh(ctx, { clientId, refreshToken: tokens.refresh_token });
    assert.equal((await refreshed.json()).error, "invalid_grant", `backend ${status}`);
    assert.equal(ctx.backend.calls("/auth/connector/session").length, 0);
  }

  // A backend in trouble is not a refusal: the sign-in survives it.
  const ctx = await withConnector(t);
  const { tokens } = await signIn(ctx, "alice");
  for (const status of [500, 429, 503]) {
    ctx.backend.meStatus = status;
    assert.equal((await mcpCall(ctx, tokens.access_token)).status, 502, `backend ${status}`);
  }
  ctx.backend.meStatus = undefined;
  assert.equal((await mcpCall(ctx, tokens.access_token)).status, 200);
});

test("only Cloudflare's address header goes on to the backend, never what a caller adds", async (t) => {
  const ctx = await withConnector(t);
  const added = {
    "CF-Connecting-IP": "203.0.113.9",
    "X-Forwarded-For": "6.6.6.6",
    "X-Real-IP": "6.6.6.6",
    Forwarded: "for=6.6.6.6",
    "True-Client-IP": "6.6.6.6",
    "X-Forwarded-Host": "evil.example",
    "X-Forwarded-Proto": "http",
    "X-Original-Forwarded-For": "6.6.6.6",
    "X-Client-IP": "6.6.6.6",
  };
  const clientId = (await register(ctx)).body.client_id;
  const { verifier, challenge } = pkce();
  const page = await authorizePage(ctx, { clientId, challenge, headers: added });
  const toGoogle = await pressButton(ctx, { confirmation: confirmationFrom(await page.text()), cookie: cookieFrom(page), headers: added });
  const google = new URL(toGoogle.headers.get("location"));
  ctx.google.nonces.set("g-alice", google.searchParams.get("nonce"));
  const back = await ctx.get(`/oauth/google/callback?${new URLSearchParams({ code: "g-alice", state: google.searchParams.get("state") })}`, { headers: { ...added, Cookie: cookieFrom(toGoogle) } });
  const code = new URL(back.headers.get("location")).searchParams.get("code");
  const tokens = await (await redeem(ctx, { clientId, code, verifier, headers: added })).json();
  const renewed = await (await refresh(ctx, { clientId, refreshToken: tokens.refresh_token, headers: added })).json();
  assert.equal((await mcpCall(ctx, renewed.access_token, { headers: added })).status, 200);

  const paths = ["/auth/connector/google", "/auth/connector/session", "/auth/me", "/links"];
  for (const path of paths) {
    const calls = ctx.backend.calls(path);
    assert.ok(calls.length >= 1, path);
    for (const call of calls) {
      assert.equal(call.headers["cf-connecting-ip"], "203.0.113.9", path);
      assert.equal(call.headers["x-forwarded-proto"], "https", path);
      for (const name of ["x-forwarded-for", "x-real-ip", "forwarded", "true-client-ip", "x-forwarded-host", "x-original-forwarded-for", "x-client-ip", "cookie"]) {
        assert.equal(call.headers[name], undefined, `${path}: ${name}`);
      }
      assert.ok(!JSON.stringify(call.headers).includes("6.6.6.6"), path);
    }
  }
});

// --- guards -----------------------------------------------------------------------------

test("an error return from Google is looked at only in the browser that started the sign-in", async (t) => {
  const ctx = await withConnector(t);
  const clientId = (await register(ctx)).body.client_id;
  const authorized = await authorize(ctx, { clientId, challenge: pkce().challenge });
  const state = new URL(authorized.headers.get("location")).searchParams.get("state");
  // Someone else's browser, sent a link that claims Google refused.
  for (const headers of [{}, { Cookie: "__Host-lnp_mcp_signin=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" }]) {
    const fresh = await authorize(ctx, { clientId, challenge: pkce().challenge });
    const freshState = new URL(fresh.headers.get("location")).searchParams.get("state");
    const res = await ctx.get(`/oauth/google/callback?error=access_denied&state=${freshState}`, { headers });
    assert.equal(res.status, 400);
    assert.equal(res.headers.get("location"), null, "nobody is sent to Claude's callback");
    assert.match(await res.text(), /This sign-in has expired/);
  }
  // Control: in the right browser it is the cancel it says it is.
  const res = await ctx.get(`/oauth/google/callback?error=access_denied&state=${state}`, { headers: { Cookie: cookieFrom(authorized) } });
  assert.equal(res.status, 302);
  assert.equal(new URL(res.headers.get("location")).searchParams.get("error"), "access_denied");
});

test("every page carries the headers that keep it out of frames and free of outside content", async (t) => {
  const ctx = await withConnector(t, { options: { rateLimits: { callback: { max: 1, windowSeconds: 60 } } } });
  const clientId = (await register(ctx)).body.client_id;
  const authorized = await authorize(ctx, { clientId, challenge: pkce().challenge, state: "st-1" });
  const pages = {
    "the confirmation page": await authorizePage(ctx, { clientId, challenge: pkce().challenge }),
    "an invalid sign-in link": await authorizePage(ctx, { clientId: "nobody", challenge: pkce().challenge }),
    "an expired confirmation": await pressButton(ctx, { confirmation: "x" }),
    "an account the backend refuses": await googleCallback(ctx, authorized, "mallory"),
    "too many attempts": await ctx.get("/oauth/google/callback?state=x"),
  };
  assert.deepEqual(
    Object.values(pages).map((res) => res.status),
    [200, 400, 400, 403, 429],
  );
  for (const [what, res] of Object.entries(pages)) {
    assert.match(res.headers.get("content-type"), /^text\/html; charset=utf-8/, what);
    for (const [name, value] of Object.entries(PAGE_HEADERS)) assert.equal(res.headers.get(name), value, `${what}: ${name}`);
    const html = await res.text();
    assert.ok(!/<script|<link|<img|<iframe|src=/i.test(html), what);
  }
});

test("whatever a page shows from a request is escaped", async (t) => {
  assert.equal(escapeHtml(`<a href="x" title='y'>&amp;</a>`), "&lt;a href=&quot;x&quot; title=&#39;y&#39;&gt;&amp;amp;&lt;/a&gt;");
  assert.equal(escapeHtml(7), "7");

  const ctx = await withConnector(t);
  const clientId = (await register(ctx)).body.client_id;
  const state = `"><script>alert(1)</script><a href='x' title="`;
  // The only request text any page shows: the state, inside the link back to Claude.
  const authorized = await authorize(ctx, { clientId, challenge: pkce().challenge, state });
  const res = await googleCallback(ctx, authorized, "mallory");
  assert.equal(res.status, 403);
  const html = await res.text();
  assert.ok(!html.includes("<script") && !html.includes(`href='x'`), html);
  assert.ok(everyAmpersandIsEscaped(html), html);
  const [, href] = /<a href="([^"]+)" rel="noreferrer">Return to Claude<\/a>/.exec(html);
  assert.ok(href.includes("&amp;state="), "the link's own ampersands are entities");
  const link = new URL(href.replaceAll("&amp;", "&"));
  assert.equal(`${link.origin}${link.pathname}`, CLAUDE_CALLBACK);
  assert.equal(link.searchParams.get("state"), state);

  // The confirmation page's one variable, the sealed token, is escaped as well.
  const page = await (await authorizePage(ctx, { clientId, challenge: pkce().challenge, state })).text();
  assert.ok(everyAmpersandIsEscaped(page) && !page.includes("<script"));
});

test("a sealed value longer than the cap is refused even when it is genuine", () => {
  const sealer = new Sealer(OAUTH_SECRET);
  for (const kind of ["access", "refresh", "client", "confirm"]) {
    const fits = sealer.seal(kind, { pad: "x".repeat(2900) });
    assert.ok(fits.length <= 4096, String(fits.length));
    assert.deepEqual(sealer.unseal(kind, fits), { pad: "x".repeat(2900) });
    const tooLong = sealer.seal(kind, { pad: "x".repeat(3100) });
    assert.ok(tooLong.length > 4096 && tooLong.length < 4300, String(tooLong.length));
    assert.equal(sealer.unseal(kind, tooLong), undefined, kind);
  }
});

test("the kind's prefix is authenticated with the sealed value, besides the kind having its own key", () => {
  const sealer = new Sealer(OAUTH_SECRET);
  const prefix = "lnpa1";
  const key = sealer.keys.access;
  const sealed = sealer.seal("access", { u: 1 });
  const raw = Buffer.from(sealed.slice(prefix.length + 1), "base64url");
  const open = (aad) => {
    const decipher = crypto.createDecipheriv("aes-256-gcm", key, raw.subarray(0, 12));
    if (aad !== undefined) decipher.setAAD(Buffer.from(aad));
    decipher.setAuthTag(raw.subarray(raw.length - 16));
    return JSON.parse(Buffer.concat([decipher.update(raw.subarray(12, raw.length - 16)), decipher.final()]).toString("utf8"));
  };
  // With the right key, the value opens only together with its own prefix.
  assert.deepEqual(open(prefix), { u: 1 });
  assert.throws(() => open(undefined), "it opens without the prefix");
  assert.throws(() => open("lnpa2"), "it opens under another version's prefix");
  assert.throws(() => open("lnpr1"), "it opens under another kind's prefix");

  // And the other way: the right key and the right label on the outside, but
  // sealed without the prefix or under another, is not this server's.
  const forge = (aad) => {
    const iv = crypto.randomBytes(12);
    const cipher = crypto.createCipheriv("aes-256-gcm", key, iv);
    if (aad !== undefined) cipher.setAAD(Buffer.from(aad));
    const body = Buffer.concat([cipher.update(JSON.stringify({ u: 1 }), "utf8"), cipher.final()]);
    return `${prefix}.${Buffer.concat([iv, body, cipher.getAuthTag()]).toString("base64url")}`;
  };
  assert.deepEqual(sealer.unseal("access", forge(prefix)), { u: 1 }, "control: this is how a value is sealed");
  assert.equal(sealer.unseal("access", forge(undefined)), undefined);
  assert.equal(sealer.unseal("access", forge("lnpa0")), undefined);
});

// --- who a caller is, for the limits ------------------------------------------------------

test("an IPv6 caller is its /64, an IPv4 caller its address, and anything else is one shared unknown", () => {
  const cases = {
    "203.0.113.9": "203.0.113.9",
    "2001:db8:1:2::1": "2001:db8:1:2::",
    "2001:db8:1:2:ffff:ffff:ffff:ffff": "2001:db8:1:2::",
    "2001:DB8:1:2:a:b:c:d": "2001:db8:1:2::",
    "2001:db8:1:3::1": "2001:db8:1:3::",
    "2001:db8::1": "2001:db8:0:0::",
    "2001:0db8:0000:0000:0000:0000:0000:0001": "2001:db8:0:0::",
    "::1": "0:0:0:0::",
    "::": "0:0:0:0::",
    "fe80::1%eth0": "fe80:0:0:0::",
    "64:ff9b::192.0.2.33": "64:ff9b:0:0::",
    // IPv4 seen through IPv6 is the IPv4 caller.
    "::ffff:203.0.113.9": "203.0.113.9",
    "::ffff:cb00:7109": "203.0.113.9",
    "0:0:0:0:0:ffff:203.0.113.9": "203.0.113.9",
    // Not addresses.
    "": "unknown",
    unknown: "unknown",
    "203.0.113.9, 198.51.100.1": "unknown",
    "203.0.113.999": "unknown",
    "2001:db8:1:2::1::": "unknown",
    "example.com": "unknown",
  };
  for (const [address, key] of Object.entries(cases)) assert.equal(rateKey(address), key, address);
  for (const junk of [undefined, null, 7, ["203.0.113.9"]]) assert.equal(rateKey(junk), "unknown");
});

test("callers whose address cannot be read share one small budget", async (t) => {
  const ctx = await withConnector(t);
  const clientId = (await register(ctx)).body.client_id;
  const from = (ip) => authorizePage(ctx, { clientId, challenge: pkce().challenge, headers: { "CF-Connecting-IP": ip } });
  // A tenth of /authorize's 30, between them.
  assert.equal((await from("not-an-address")).status, 200);
  assert.equal((await from("203.0.113.9, 198.51.100.1")).status, 200);
  assert.equal((await from("x".repeat(100))).status, 200);
  assert.equal((await from("something else")).status, 429);
  // A caller with an address is not among them.
  assert.equal((await from("203.0.113.9")).status, 200);
});

test("at the real limits, a flood from one network or from many cannot get a real sign-in or refresh refused", async (t) => {
  const ctx = await withConnector(t);
  const { limiter, pending, usedConfirmations } = ctx.oauth.internals;
  const anthropic = { "CF-Connecting-IP": "160.79.104.10" };
  const junk = (ip) => ctx.postForm("/token", { grant_type: "refresh_token", client_id: clientId, refresh_token: `lnpr1.${crypto.randomBytes(48).toString("base64url")}` }, { "CF-Connecting-IP": ip });
  const hex = (n) => n.toString(16);

  // Staff are signed in, and Claude refreshes for them from Anthropic's address.
  const { clientId, tokens } = await signIn(ctx, "alice");
  let current = await (await refresh(ctx, { clientId, refreshToken: tokens.refresh_token, headers: anthropic })).json();

  // 1. One /64, a different address every time: one caller, one budget.
  let refused = 0;
  for (let i = 0; i < 400; i++) {
    const res = await junk(`2001:db8:1:2:${hex(i)}::${hex(i + 1)}`);
    assert.ok(res.status === 400 || res.status === 429, String(res.status));
    if (res.status === 429) refused += 1;
  }
  assert.equal(refused, 400 - RATE_LIMITS.tokenJunk.max);
  // The same network starting sign-ins: the button stops after its budget,
  // far below what the list of pending sign-ins holds.
  let started = 0;
  for (let i = 0; i < RATE_LIMITS.confirm.max + 10; i++) {
    const from = { "CF-Connecting-IP": `2001:db8:1:2:${hex(i)}::9` };
    const page = await authorizePage(ctx, { clientId, challenge: pkce().challenge, headers: from });
    if (page.status !== 200) continue;
    const res = await pressButton(ctx, { confirmation: confirmationFrom(await page.text()), cookie: cookieFrom(page), headers: from });
    if (res.status === 302) started += 1;
  }
  assert.equal(started, RATE_LIMITS.authorize.max);
  assert.equal(pending.size, RATE_LIMITS.confirm.max);
  assert.ok(pending.size < LIMITS.pending / 10);
  assert.equal(usedConfirmations.size, pending.size + 1, "one more: the sign-in at the top of this test");

  // 2. Many networks, each with a budget of its own: more of them than the
  // table of counters holds. Some over HTTP, the rest straight at the table.
  for (let i = 0; i < 300; i++) assert.equal((await junk(`2001:db8:${hex(0x100 + i)}:1::1`)).status, 400);
  for (let i = 0; i < LIMITS.rateKeys + 2000; i++) {
    assert.equal(limiter.check("tokenJunk", rateKey(`2001:db9:${hex(i >> 16)}:${hex(i & 0xffff)}::1`), RATE_LIMITS.tokenJunk), 0);
  }
  assert.equal(limiter.windows.size, LIMITS.rateKeys, "the table is full, and no larger");

  // 3. Junk from Anthropic's own address (an outsider can make Claude send
  // it): more than a real request's budget of it.
  refused = 0;
  for (let i = 0; i < RATE_LIMITS.token.max + 50; i++) if ((await junk(anthropic["CF-Connecting-IP"])).status === 429) refused += 1;
  assert.equal(refused, RATE_LIMITS.token.max + 50 - RATE_LIMITS.tokenJunk.max);

  // After all of that: the real refresh from Anthropic's address goes through,
  // a new person signs in, and their code is redeemed from Anthropic's address.
  const res = await refresh(ctx, { clientId, refreshToken: current.refresh_token, headers: anthropic });
  assert.equal(res.status, 200);
  current = await res.json();
  assert.equal((await mcpCall(ctx, current.access_token, { headers: anthropic })).status, 200);
  const grant = await getCode(ctx, "bob", { clientId });
  assert.equal((await redeem(ctx, { ...grant, headers: anthropic })).status, 200);
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
  assert.equal(new Set(keys).size, 4);
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
