// The connector's own OAuth 2.1 sign-in: it is the authorization server Claude
// talks to and the protected resource (/mcp) the tokens are for.
//
//   Claude -> /mcp                      401, pointing at the metadata below
//   Claude -> /.well-known/...          who signs people in (this server)
//   Claude -> /register                 a public client, for Claude's callback only
//   person -> /authorize                checked, then sent to Google
//   Google -> /oauth/google/callback    code -> ID token -> the backend checks
//                                       it and names the opn.onl account
//   person -> Claude's callback         with a one-time code
//   Claude -> /token                    code (+ PKCE verifier) -> tokens;
//                                       later, refresh token -> new tokens
//
// Nothing is stored on disk. Client ids, access tokens and refresh tokens are
// sealed (AES-256-GCM) with keys derived from MCP_OAUTH_SECRET, so they
// survive a restart and nobody else can make or read one. Only what must be
// used once is kept in memory, with an expiry and a size cap: pending Google
// round trips, authorization codes, spent refresh tokens, ended sign-ins. A
// restart drops sign-ins that are in flight and forgets which refresh tokens
// were spent; it does not sign anyone out.
//
// The /token and /register protocol handling is the MCP SDK's; this file is
// the policy behind them and the two browser steps.

import { AsyncLocalStorage } from "node:async_hooks";
import crypto from "node:crypto";
import express from "express";
import { clientRegistrationHandler } from "@modelcontextprotocol/sdk/server/auth/handlers/register.js";
import { tokenHandler } from "@modelcontextprotocol/sdk/server/auth/handlers/token.js";
import { CustomOAuthError, InvalidGrantError, InvalidTargetError, ServerError } from "@modelcontextprotocol/sdk/server/auth/errors.js";

export const ACCESS_TOKEN_SECONDS = 3600;
export const CODE_SECONDS = 60;
export const PENDING_SECONDS = 600;
/** The `aud` of a renewal assertion; the backend accepts no other. */
export const ASSERTION_AUDIENCE = "opn-connector-session";
const ASSERTION_SECONDS = 30;
const UPSTREAM_TIMEOUT_MS = 8000;
const SIGNIN_COOKIE = "__Host-lnp_mcp_signin";
const MAX_TOKEN_CHARS = 4096;

// How many entries each in-memory map may hold. A full map refuses new
// entries (after dropping expired ones) rather than growing.
export const LIMITS = {
  pending: 2000,
  codes: 1000,
  spentCodes: 2000,
  spentRefreshTokens: 20000,
  endedSignIns: 5000,
  rateKeys: 10000,
};

// Requests per window, per client address. /register and /token are called by
// Anthropic's servers, a few addresses shared by everyone, so they are roomy;
// /authorize and the Google callback are one person's browser.
export const RATE_LIMITS = {
  register: { max: 60, windowSeconds: 3600 },
  authorize: { max: 30, windowSeconds: 600 },
  callback: { max: 30, windowSeconds: 600 },
  token: { max: 300, windowSeconds: 600 },
};

// --- small tools ------------------------------------------------------------

const b64url = (buf) => Buffer.from(buf).toString("base64url");
const randomId = () => b64url(crypto.randomBytes(32));
const sha256 = (text) => crypto.createHash("sha256").update(text).digest();

/** Compare two strings without telling, by timing, where they differ. */
export function safeEqual(a, b) {
  if (typeof a !== "string" || typeof b !== "string") return false;
  return crypto.timingSafeEqual(sha256(a), sha256(b));
}

/** A request parameter that was sent exactly once, as text. */
function one(v) {
  return typeof v === "string" ? v : undefined;
}

function escapeHtml(text) {
  return String(text).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
}

/** A map whose entries expire, and which never holds more than `max`. */
export class ExpiringMap {
  constructor(max, now) {
    this.max = max;
    this.now = now;
    this.map = new Map();
  }
  sweep() {
    const now = this.now();
    for (const [key, entry] of this.map) if (entry.expiresAt <= now) this.map.delete(key);
  }
  /** False when the map is full of live entries: the caller must refuse. */
  set(key, value, expiresAt) {
    if (!this.map.has(key) && this.map.size >= this.max) {
      this.sweep();
      if (this.map.size >= this.max) return false;
    }
    this.map.set(key, { value, expiresAt });
    return true;
  }
  get(key) {
    const entry = this.map.get(key);
    if (!entry) return undefined;
    if (entry.expiresAt <= this.now()) {
      this.map.delete(key);
      return undefined;
    }
    return entry.value;
  }
  has(key) {
    return this.get(key) !== undefined;
  }
  /** Read and remove: what makes a code or a state single-use. */
  take(key) {
    const value = this.get(key);
    this.map.delete(key);
    return value;
  }
  get size() {
    return this.map.size;
  }
}

/** Fixed-window request counter per key, in a bounded map. */
export class RateLimiter {
  constructor(now, maxKeys = LIMITS.rateKeys) {
    this.windows = new ExpiringMap(maxKeys, now);
    this.now = now;
  }
  /** Seconds to wait, or 0 when the request may go ahead. */
  check(bucket, address, { max, windowSeconds }) {
    let key = `${bucket}:${address}`;
    let window = this.windows.get(key);
    if (!window) {
      window = { count: 0, resetAt: this.now() + windowSeconds * 1000 };
      // No room for another address: the strangers share one counter.
      if (!this.windows.set(key, window, window.resetAt)) {
        key = `${bucket}:overflow`;
        window = this.windows.get(key) ?? window;
        this.windows.map.set(key, { value: window, expiresAt: window.resetAt });
      }
    }
    window.count += 1;
    return window.count > max ? Math.max(1, Math.ceil((window.resetAt - this.now()) / 1000)) : 0;
  }
}

/** The caller's address: Cloudflare's header in production, else the socket's. */
export function clientAddress(req) {
  const header = req.headers["cf-connecting-ip"];
  if (typeof header === "string" && header.length > 0 && header.length <= 64) return header;
  return req.socket?.remoteAddress ?? "unknown";
}

// The backend sits behind TLS-terminating proxies: without X-Forwarded-Proto it
// redirects to https, and without the caller's address everyone would share
// this container's rate-limit bucket.
export function proxyHeaders(req) {
  const headers = { "X-Forwarded-Proto": "https" };
  const ip = req.headers["cf-connecting-ip"];
  if (typeof ip === "string" && ip) headers["CF-Connecting-IP"] = ip;
  return headers;
}

// --- sealed values ------------------------------------------------------------

// One key per kind of value, and the kind is authenticated too, so a refresh
// token is never accepted as an access token or a client id. The trailing
// number is the format's version.
const KINDS = { client: "lnpc1", access: "lnpa1", refresh: "lnpr1" };

export class Sealer {
  constructor(secret) {
    this.keys = {};
    for (const [kind, prefix] of Object.entries(KINDS)) {
      this.keys[kind] = Buffer.from(crypto.hkdfSync("sha256", secret, "lnp-links-mcp", prefix, 32));
    }
  }
  seal(kind, payload) {
    const prefix = KINDS[kind];
    const iv = crypto.randomBytes(12);
    const cipher = crypto.createCipheriv("aes-256-gcm", this.keys[kind], iv);
    cipher.setAAD(Buffer.from(prefix));
    const body = Buffer.concat([cipher.update(JSON.stringify(payload), "utf8"), cipher.final()]);
    return `${prefix}.${b64url(Buffer.concat([iv, body, cipher.getAuthTag()]))}`;
  }
  /** The payload, or undefined for anything this server did not seal as `kind`. */
  unseal(kind, text) {
    const prefix = KINDS[kind];
    if (typeof text !== "string" || text.length > MAX_TOKEN_CHARS || !text.startsWith(`${prefix}.`)) return undefined;
    const encoded = text.slice(prefix.length + 1);
    if (!/^[A-Za-z0-9_-]+$/.test(encoded)) return undefined;
    const raw = Buffer.from(encoded, "base64url");
    if (raw.length < 12 + 16 + 2) return undefined;
    try {
      const decipher = crypto.createDecipheriv("aes-256-gcm", this.keys[kind], raw.subarray(0, 12));
      decipher.setAAD(Buffer.from(prefix));
      decipher.setAuthTag(raw.subarray(raw.length - 16));
      const body = Buffer.concat([decipher.update(raw.subarray(12, raw.length - 16)), decipher.final()]);
      const payload = JSON.parse(body.toString("utf8"));
      return payload && typeof payload === "object" ? payload : undefined;
    } catch {
      return undefined;
    }
  }
}

/** The assertion the backend renews a session for: HS256, valid half a minute. */
export function signAssertion(secret, userId, tokenVersion, nowMs) {
  const iat = Math.floor(nowMs / 1000);
  const part = (obj) => b64url(JSON.stringify(obj));
  const signed = `${part({ alg: "HS256", typ: "JWT" })}.${part({
    aud: ASSERTION_AUDIENCE,
    sub: String(userId),
    tv: tokenVersion,
    iat,
    exp: iat + ASSERTION_SECONDS,
  })}`;
  return `${signed}.${b64url(crypto.createHmac("sha256", secret).update(signed).digest())}`;
}

// --- pages ----------------------------------------------------------------------

function sendPage(res, status, title, message, link) {
  res
    .status(status)
    .set({
      "Content-Type": "text/html; charset=utf-8",
      "Cache-Control": "no-store",
      "Content-Security-Policy": "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'",
      "X-Frame-Options": "DENY",
      "X-Content-Type-Options": "nosniff",
      "Referrer-Policy": "no-referrer",
    })
    .send(
      `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">` +
        `<meta name="robots" content="noindex"><title>${escapeHtml(title)}</title>` +
        `<style>body{font:16px/1.5 system-ui,sans-serif;max-width:32rem;margin:4rem auto;padding:0 1rem;color:#1f2937}h1{font-size:1.25rem}</style></head>` +
        `<body><h1>${escapeHtml(title)}</h1><p>${escapeHtml(message)}</p>` +
        (link ? `<p><a href="${escapeHtml(link.href)}" rel="noreferrer">${escapeHtml(link.text)}</a></p>` : "") +
        `</body></html>`,
    );
}

function redirect(res, url) {
  res.status(302).set({ Location: url, "Cache-Control": "no-store", "Referrer-Policy": "no-referrer" }).end();
}

// --- the authorization server -----------------------------------------------------

export function createOAuth(config, { now = Date.now, limits = LIMITS, rateLimits = RATE_LIMITS } = {}) {
  const sealer = new Sealer(config.oauthSecret);
  const limiter = new RateLimiter(now, limits.rateKeys);
  // Google `state` -> the authorization request waiting for that round trip.
  const pending = new ExpiringMap(limits.pending, now);
  // Authorization code -> the sign-in it stands for.
  const codes = new ExpiringMap(limits.codes, now);
  // A code that was already redeemed -> its sign-in, to end it on a replay.
  const spentCodes = new ExpiringMap(limits.spentCodes, now);
  // Ids of refresh tokens that were rotated away.
  const spentRefreshTokens = new ExpiringMap(limits.spentRefreshTokens, now);
  // Sign-ins ended early (a replay, or the backend said the person is gone).
  const endedSignIns = new ExpiringMap(limits.endedSignIns, now);

  // The headers the backend needs from the request being answered; the SDK's
  // token handler does not hand the request to the provider.
  const requestHeaders = new AsyncLocalStorage();

  const callbackUrl = `${config.issuer}/oauth/google/callback`;
  const resourceMetadataUrl = `${config.issuer}/.well-known/oauth-protected-resource/mcp`;
  const clientTag = (clientId) => b64url(sha256(clientId).subarray(0, 16));

  /** True when `value` names this server's /mcp, a trailing slash aside. */
  function isThisResource(value) {
    let url;
    try {
      url = new URL(value);
    } catch {
      return false;
    }
    if (url.hash || url.search || url.username || url.password) return false;
    return `${url.origin}${url.pathname.replace(/\/$/, "")}` === config.resource;
  }

  function endSignIn(sid, sessionEnd) {
    // Only a signed-in person's own replay or removal adds an entry, so the
    // cap is far away; were it reached, the entry would be dropped and that
    // sign-in would last until its tokens expire.
    endedSignIns.set(sid, true, sessionEnd);
  }

  // --- clients: the client id is the registration, sealed ---------------------

  const clientsStore = {
    getClient(clientId) {
      const payload = sealer.unseal("client", clientId);
      if (!payload || !Array.isArray(payload.r) || payload.r.length === 0) return undefined;
      // A callback taken off the allow-list stops working for clients that
      // registered it earlier.
      if (!payload.r.every((uri) => config.allowedRedirectUris.includes(uri))) return undefined;
      return { client_id: clientId, redirect_uris: payload.r, token_endpoint_auth_method: "none" };
    },
    registerClient(metadata) {
      const uris = [...new Set(metadata.redirect_uris)];
      if (uris.length === 0 || !uris.every((uri) => config.allowedRedirectUris.includes(uri))) {
        throw new CustomOAuthError("invalid_redirect_uri", "Only Claude's callback addresses may be registered");
      }
      const issuedAt = Math.floor(now() / 1000);
      // Always a public client: no secret is issued, whatever was asked for.
      return {
        client_id: sealer.seal("client", { r: uris, t: issuedAt }),
        client_id_issued_at: issuedAt,
        redirect_uris: uris,
        token_endpoint_auth_method: "none",
        grant_types: ["authorization_code", "refresh_token"],
        response_types: ["code"],
        ...(typeof metadata.client_name === "string" ? { client_name: metadata.client_name.slice(0, 200) } : {}),
      };
    },
  };

  // --- tokens -------------------------------------------------------------------

  function mint(signIn, backendToken) {
    const nowMs = now();
    const expiresAt = Math.min(nowMs + ACCESS_TOKEN_SECONDS * 1000, signIn.sessionEnd);
    const shared = { s: signIn.sid, u: signIn.userId, c: signIn.clientTag, a: config.resource, x: signIn.sessionEnd };
    return {
      access_token: sealer.seal("access", { ...shared, e: expiresAt, b: backendToken }),
      token_type: "Bearer",
      expires_in: Math.max(1, Math.floor((expiresAt - nowMs) / 1000)),
      refresh_token: sealer.seal("refresh", { ...shared, j: randomId(), v: signIn.tokenVersion }),
    };
  }

  /** The access token's payload, or undefined when it must not be honoured. */
  function verifyAccessToken(token) {
    const p = sealer.unseal("access", token);
    if (!p || typeof p.b !== "string" || typeof p.s !== "string") return undefined;
    if (p.a !== config.resource) return undefined;
    const nowMs = now();
    if (!(p.e > nowMs) || !(p.x > nowMs)) return undefined;
    if (endedSignIns.has(p.s)) return undefined;
    return { sid: p.s, userId: p.u, sessionEnd: p.x, backendToken: p.b };
  }

  async function backendPost(path, body, headers) {
    return fetch(`${config.backendUrl}${path}`, {
      method: "POST",
      headers: { ...headers, "Content-Type": "application/json" },
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(UPSTREAM_TIMEOUT_MS),
    });
  }

  const provider = {
    clientsStore,
    // PKCE is checked here, in constant time, not by the SDK's helper.
    skipLocalPkceValidation: true,

    async exchangeAuthorizationCode(client, code, codeVerifier, redirectUri, resource) {
      const refused = () => new InvalidGrantError("The authorization code is invalid, expired or already used");
      const grant = codes.take(code);
      if (!grant) {
        // A code presented twice: whoever holds the tokens it produced may
        // not be the person, so that sign-in ends.
        const replayed = spentCodes.get(code);
        if (replayed) endSignIn(replayed.sid, replayed.sessionEnd);
        throw refused();
      }
      spentCodes.set(code, { sid: grant.sid, sessionEnd: grant.sessionEnd }, now() + PENDING_SECONDS * 1000);

      if (!safeEqual(grant.clientId, client.client_id)) throw refused();
      // OAuth 2.1 no longer sends redirect_uri here (PKCE does that job); a
      // client that does send it must send the one the code was issued to.
      if (redirectUri !== undefined && !safeEqual(redirectUri, grant.redirectUri)) throw refused();
      if (typeof codeVerifier !== "string" || !/^[A-Za-z0-9\-._~]{43,128}$/.test(codeVerifier)) throw refused();
      if (!safeEqual(b64url(sha256(codeVerifier)), grant.codeChallenge)) throw refused();
      if (resource !== undefined && !isThisResource(resource.href)) {
        throw new InvalidTargetError("This server issues tokens for its own /mcp only");
      }
      return mint(grant, grant.backendToken);
    },

    async exchangeRefreshToken(client, refreshToken, _scopes, resource) {
      const refused = () => new InvalidGrantError("The refresh token is invalid or expired; sign in again");
      const p = sealer.unseal("refresh", refreshToken);
      if (!p || typeof p.s !== "string" || typeof p.j !== "string") throw refused();
      if (p.a !== config.resource || !safeEqual(p.c, clientTag(client.client_id))) throw refused();
      // The sign-in ends a fixed time after the Google sign-in, however often
      // it was refreshed.
      if (!(p.x > now())) throw refused();
      if (endedSignIns.has(p.s)) throw refused();
      const reuse = () => {
        endSignIn(p.s, p.x);
        return refused();
      };
      if (spentRefreshTokens.has(p.j)) throw reuse();
      if (resource !== undefined && !isThisResource(resource.href)) {
        throw new InvalidTargetError("This server issues tokens for its own /mcp only");
      }

      // The backend decides whether this person may carry on: it makes a new
      // session only while the account is live and unchanged since sign-in.
      let response;
      try {
        response = await backendPost(
          "/auth/connector/session",
          { assertion: signAssertion(config.signinSecret, p.u, p.v, now()) },
          requestHeaders.getStore() ?? { "X-Forwarded-Proto": "https" },
        );
      } catch {
        console.error("session renewal: the backend could not be reached");
        throw new ServerError("The sign-in could not be renewed right now");
      }
      if (response.status === 401 || response.status === 403) {
        console.error(`session renewal refused by backend: ${response.status}`);
        throw reuse();
      }
      if (!response.ok) {
        console.error(`session renewal failed: backend answered ${response.status}`);
        throw new ServerError("The sign-in could not be renewed right now");
      }
      const session = await response.json().catch(() => undefined);
      if (!session || typeof session.token !== "string" || session.user_id !== p.u || !Number.isInteger(session.token_version)) {
        console.error("session renewal failed: unexpected answer from the backend");
        throw new ServerError("The sign-in could not be renewed right now");
      }

      // Checked again: the same token may have been redeemed while the
      // backend was answering. Nothing awaits between this and the mark.
      if (spentRefreshTokens.has(p.j) || endedSignIns.has(p.s)) throw reuse();
      if (!spentRefreshTokens.set(p.j, true, p.x)) throw new ServerError("The sign-in could not be renewed right now");
      return mint({ sid: p.s, userId: p.u, clientTag: p.c, sessionEnd: p.x, tokenVersion: session.token_version }, session.token);
    },
  };

  // --- routes -------------------------------------------------------------------

  const limit = (bucket, onLimited) => (req, res, next) => {
    const wait = limiter.check(bucket, clientAddress(req), rateLimits[bucket]);
    if (wait === 0) return next();
    res.set("Retry-After", String(wait));
    onLimited(res);
  };
  const jsonLimited = (res) => res.status(429).set("Cache-Control", "no-store").json({ error: "too_many_requests", error_description: "Too many requests; try again later" });
  const pageLimited = (res) => sendPage(res, 429, "Too many attempts", "Wait a few minutes, then connect LNP Links in Claude again.");

  const router = express.Router();

  const metadata = (document) => (req, res) => {
    if (req.method !== "GET" && req.method !== "HEAD") return res.status(405).set("Allow", "GET").end();
    res.set("Access-Control-Allow-Origin", "*").json(document);
  };
  const protectedResource = metadata({
    resource: config.resource,
    authorization_servers: [config.issuer],
    bearer_methods_supported: ["header"],
    resource_name: "LNP Links",
  });
  router.all("/.well-known/oauth-protected-resource/mcp", protectedResource);
  router.all("/.well-known/oauth-protected-resource", protectedResource);
  router.all(
    "/.well-known/oauth-authorization-server",
    metadata({
      issuer: config.issuer,
      authorization_endpoint: `${config.issuer}/authorize`,
      token_endpoint: `${config.issuer}/token`,
      registration_endpoint: `${config.issuer}/register`,
      response_types_supported: ["code"],
      grant_types_supported: ["authorization_code", "refresh_token"],
      code_challenge_methods_supported: ["S256"],
      token_endpoint_auth_methods_supported: ["none"],
      authorization_response_iss_parameter_supported: true,
    }),
  );

  router.use("/register", limit("register", jsonLimited), clientRegistrationHandler({ clientsStore, rateLimit: false }));
  router.use(
    "/token",
    limit("token", jsonLimited),
    (req, _res, next) => requestHeaders.run(proxyHeaders(req), next),
    tokenHandler({ provider, rateLimit: false }),
  );

  /** Send the person back to the client with an OAuth error. */
  function redirectError(res, redirectUri, state, error, description) {
    const url = new URL(redirectUri);
    url.searchParams.set("error", error);
    url.searchParams.set("error_description", description);
    if (state) url.searchParams.set("state", state);
    url.searchParams.set("iss", config.issuer);
    return url.href;
  }

  router.all("/authorize", limit("authorize", pageLimited), (req, res) => {
    if (req.method !== "GET") return res.status(405).set("Allow", "GET").end();
    const q = req.query;

    // Until the client and its redirect URI are known to be ours, nothing is
    // redirected anywhere: the person sees a page instead.
    const clientId = one(q.client_id);
    const client = clientId && clientsStore.getClient(clientId);
    if (!client) {
      return sendPage(res, 400, "This sign-in link is not valid", "Claude sent a client this server does not know. Remove LNP Links in Claude's connector settings and add it again.");
    }
    const redirectUri = one(q.redirect_uri) ?? (q.redirect_uri === undefined && client.redirect_uris.length === 1 ? client.redirect_uris[0] : undefined);
    if (!redirectUri || !client.redirect_uris.includes(redirectUri)) {
      return sendPage(res, 400, "This sign-in link is not valid", "The address to return to is not one this server allows.");
    }

    const state = one(q.state);
    const fail = (error, description) => redirect(res, redirectError(res, redirectUri, state, error, description));
    if (one(q.response_type) !== "code") return fail("unsupported_response_type", "response_type must be code");
    if (!state || state.length > 1024) return fail("invalid_request", "state is required");
    const codeChallenge = one(q.code_challenge);
    if (one(q.code_challenge_method) !== "S256" || !codeChallenge || !/^[A-Za-z0-9_-]{43}$/.test(codeChallenge)) {
      return fail("invalid_request", "PKCE with code_challenge_method=S256 is required");
    }
    if (q.resource !== undefined && !(one(q.resource) && isThisResource(q.resource))) {
      return fail("invalid_target", "This server issues tokens for its own /mcp only");
    }

    // The browser that starts a sign-in is the one that must finish it.
    const cookies = parseCookies(req);
    const browser = /^[A-Za-z0-9_-]{43}$/.test(cookies[SIGNIN_COOKIE] ?? "") ? cookies[SIGNIN_COOKIE] : randomId();
    const googleState = randomId();
    const nonce = randomId();
    const expiresAt = now() + PENDING_SECONDS * 1000;
    if (!pending.set(googleState, { clientId, redirectUri, state, codeChallenge, nonce, browser: b64url(sha256(browser)) }, expiresAt)) {
      return fail("temporarily_unavailable", "Too many sign-ins are in progress; try again in a few minutes");
    }

    const google = new URL(config.googleAuthUrl);
    google.searchParams.set("client_id", config.googleClientId);
    google.searchParams.set("redirect_uri", callbackUrl);
    google.searchParams.set("response_type", "code");
    google.searchParams.set("scope", "openid email");
    google.searchParams.set("state", googleState);
    google.searchParams.set("nonce", nonce);
    google.searchParams.set("prompt", "select_account");
    res.append("Set-Cookie", `${SIGNIN_COOKIE}=${browser}; Path=/; Max-Age=${PENDING_SECONDS}; Secure; HttpOnly; SameSite=Lax`);
    redirect(res, google.href);
  });

  router.all("/oauth/google/callback", limit("callback", pageLimited), async (req, res) => {
    if (req.method !== "GET") return res.status(405).set("Allow", "GET").end();
    const q = req.query;
    const stale = () => sendPage(res, 400, "This sign-in has expired", "It was already used, or took longer than ten minutes. Go back to Claude and connect LNP Links again.");

    // Taken, not read: a state works once, whatever happens next.
    const googleState = one(q.state);
    const request = googleState ? pending.take(googleState) : undefined;
    if (!request) return stale();
    const browser = parseCookies(req)[SIGNIN_COOKIE];
    if (typeof browser !== "string" || !safeEqual(b64url(sha256(browser)), request.browser)) return stale();

    const backToClaude = (error, description) => redirectError(res, request.redirectUri, request.state, error, description);
    const failed = (status, title, message) =>
      sendPage(res, status, title, message, { href: backToClaude("access_denied", "Sign-in did not complete"), text: "Return to Claude" });

    if (q.error !== undefined) {
      // The person backed out on Google's page (or Google refused).
      return redirect(res, backToClaude("access_denied", "Google sign-in was cancelled"));
    }
    const googleCode = one(q.code);
    if (!googleCode) return stale();

    let idToken;
    try {
      const response = await fetch(config.googleTokenUrl, {
        method: "POST",
        headers: { "Content-Type": "application/x-www-form-urlencoded" },
        body: new URLSearchParams({
          code: googleCode,
          client_id: config.googleClientId,
          client_secret: config.googleClientSecret,
          redirect_uri: callbackUrl,
          grant_type: "authorization_code",
        }),
        signal: AbortSignal.timeout(UPSTREAM_TIMEOUT_MS),
      });
      const body = await response.json().catch(() => undefined);
      if (!response.ok) {
        // Google's body is {"error": "invalid_grant", ...}; only that short code is logged.
        const reason = typeof body?.error === "string" && /^[a-z_]{1,40}$/.test(body.error) ? body.error : "no error code";
        console.error(`google code exchange refused: ${response.status} ${reason}`);
      } else if (typeof body?.id_token === "string" && body.id_token) {
        idToken = body.id_token;
      } else {
        console.error("google code exchange: no ID token in the answer");
      }
    } catch {
      console.error("google code exchange: Google could not be reached");
    }
    if (!idToken) return failed(502, "Google sign-in did not complete", "Go back to Claude and connect LNP Links again.");

    // One verifier for Google's tokens, the backend's: it checks the token
    // (and that it answers this request's nonce) and names the account.
    let session;
    let status = 502;
    try {
      const response = await backendPost("/auth/connector/google", { id_token: idToken, nonce: request.nonce }, proxyHeaders(req));
      status = response.status;
      if (response.ok) session = await response.json().catch(() => undefined);
    } catch {
      console.error("sign-in: the backend could not be reached");
    }
    if (status === 403) {
      console.error("sign-in refused by backend: 403");
      return failed(403, "This Google account is not allowed", "LNP Links is for Leadership Now Project accounts. Go back to Claude, connect again, and choose your Leadership Now Project Google account.");
    }
    if (status === 429) return failed(429, "Too many attempts", "Wait a minute, then connect LNP Links in Claude again.");
    if (!session || typeof session.token !== "string" || !Number.isInteger(session.user_id) || !Number.isInteger(session.token_version)) {
      console.error(`sign-in failed: backend answered ${status}`);
      return failed(status === 401 ? 401 : 502, "Sign-in did not complete", "Go back to Claude and connect LNP Links again.");
    }

    const code = randomId();
    const nowMs = now();
    const grant = {
      sid: randomId(),
      clientId: request.clientId,
      clientTag: clientTag(request.clientId),
      redirectUri: request.redirectUri,
      codeChallenge: request.codeChallenge,
      userId: session.user_id,
      tokenVersion: session.token_version,
      backendToken: session.token,
      sessionEnd: nowMs + config.sessionSeconds * 1000,
    };
    if (!codes.set(code, grant, nowMs + CODE_SECONDS * 1000)) {
      return failed(503, "Sign-in is busy", "Try again in a minute.");
    }
    const back = new URL(request.redirectUri);
    back.searchParams.set("code", code);
    back.searchParams.set("state", request.state);
    back.searchParams.set("iss", config.issuer);
    redirect(res, back.href);
  });

  /** 401 for /mcp, in the shape that starts (or restarts) Claude's sign-in. */
  function challenge(res, error) {
    const params = error ? `error="invalid_token", error_description="${error}", ` : "";
    res.set("WWW-Authenticate", `Bearer ${params}resource_metadata="${resourceMetadataUrl}"`);
    res.set("Cache-Control", "no-store");
  }

  /**
   * The signed-in person behind a /mcp request, or undefined after answering
   * 401. Only the Authorization header is read: never the query string.
   */
  function authenticate(req, res) {
    const header = req.headers.authorization;
    if (typeof header !== "string" || header === "") {
      challenge(res);
      return undefined;
    }
    const match = /^Bearer ([A-Za-z0-9._~+/=-]+)$/i.exec(header);
    const person = match ? verifyAccessToken(match[1]) : undefined;
    if (!person) {
      challenge(res, "The access token is invalid or expired");
      return undefined;
    }
    return person;
  }

  return {
    router,
    authenticate,
    challenge,
    endSignIn,
    // For tests.
    internals: { clientTag, sealer, pending, codes, spentCodes, spentRefreshTokens, endedSignIns, limiter, mint, verifyAccessToken, clientsStore, provider },
  };
}

function parseCookies(req) {
  const out = {};
  const header = req.headers.cookie;
  if (typeof header !== "string" || header.length > 8192) return out;
  for (const part of header.split(";")) {
    const eq = part.indexOf("=");
    if (eq > 0) out[part.slice(0, eq).trim()] = part.slice(eq + 1).trim();
  }
  return out;
}
