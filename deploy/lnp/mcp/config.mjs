// Settings of the LNP Links connector, read once at boot. Anything wrong here
// stops the server before it listens: a connector that signs people in with a
// guessable secret or redirects to the wrong place is worse than one that is
// down.

export class ConfigError extends Error {}

export const DEFAULT_REDIRECT_URIS = [
  // Claude's hosted apps (web, desktop, mobile) all come back through this
  // one; the second is where Anthropic has said it may move.
  "https://claude.ai/api/mcp/auth_callback",
  "https://claude.com/api/mcp/auth_callback",
];
const DEFAULT_SESSION_HOURS = 168;
const MAX_SESSION_HOURS = 720;
const MIN_SECRET_BYTES = 32;
// Sample values from documentation are public; so is anything that says what
// it is instead of being random.
const PLACEHOLDER_WORDS = ["changeme", "change-me", "change_me", "placeholder", "example", "your-secret", "yoursecret", "replace", "secret-here"];

/** Compose passes an option left alone as an empty string: blank is unset. */
function value(env, name) {
  const raw = env[name];
  if (typeof raw !== "string") return undefined;
  const trimmed = raw.trim();
  return trimmed === "" ? undefined : trimmed;
}

function required(env, name, hint) {
  const v = value(env, name);
  if (v === undefined) throw new ConfigError(`${name} is not set${hint ? ` (${hint})` : ""}`);
  return v;
}

function secret(env, name) {
  const v = required(env, name, "openssl rand -base64 48");
  if (Buffer.byteLength(v, "utf8") < MIN_SECRET_BYTES) {
    throw new ConfigError(`${name} must be at least ${MIN_SECRET_BYTES} bytes (openssl rand -base64 48)`);
  }
  const lower = v.toLowerCase();
  if (PLACEHOLDER_WORDS.some((word) => lower.includes(word)) || new Set(v).size < 10) {
    throw new ConfigError(`${name} looks like a placeholder; generate one with openssl rand -base64 48`);
  }
  return v;
}

const LOOPBACK = /^(localhost|.*\.localhost|127(\.\d{1,3}){3}|\[::1\]|0\.0\.0\.0|\[::\])$/i;

/** An https origin with nothing after it, in the spelling `new URL` gives. */
function publicUrl(env) {
  const raw = required(env, "MCP_PUBLIC_URL", "the address people add in Claude, without /mcp");
  let url;
  try {
    url = new URL(raw);
  } catch {
    throw new ConfigError("MCP_PUBLIC_URL is not a URL");
  }
  if (url.protocol !== "https:" || url.username || url.password || url.search || url.hash || (url.pathname !== "/" && url.pathname !== "")) {
    throw new ConfigError("MCP_PUBLIC_URL must be https://<host> with no path, query or fragment");
  }
  return url.origin;
}

/**
 * Exact redirect URIs a client may register. Each must be a plain https
 * address: no wildcard, no userinfo, no fragment, not this machine.
 */
export function parseRedirectUris(raw) {
  const uris = raw
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
  if (uris.length === 0) throw new ConfigError("MCP_ALLOWED_REDIRECT_URIS lists no address");
  for (const uri of uris) {
    let url;
    try {
      url = new URL(uri);
    } catch {
      throw new ConfigError(`MCP_ALLOWED_REDIRECT_URIS: not a URL: ${uri}`);
    }
    if (uri.includes("*")) throw new ConfigError(`MCP_ALLOWED_REDIRECT_URIS: wildcards are not supported: ${uri}`);
    if (url.protocol !== "https:") throw new ConfigError(`MCP_ALLOWED_REDIRECT_URIS: must be https: ${uri}`);
    if (url.username || url.password) throw new ConfigError(`MCP_ALLOWED_REDIRECT_URIS: must not carry userinfo: ${uri}`);
    if (url.hash || uri.includes("#")) throw new ConfigError(`MCP_ALLOWED_REDIRECT_URIS: must not carry a fragment: ${uri}`);
    if (LOOPBACK.test(url.hostname)) throw new ConfigError(`MCP_ALLOWED_REDIRECT_URIS: loopback addresses are not supported: ${uri}`);
    // Compared as text later, so the text must be the one spelling a URL has.
    if (url.href !== uri) throw new ConfigError(`MCP_ALLOWED_REDIRECT_URIS: write it as ${url.href}`);
  }
  return [...new Set(uris)];
}

function sessionHours(env) {
  const raw = value(env, "MCP_SESSION_HOURS");
  if (raw === undefined) return DEFAULT_SESSION_HOURS;
  const hours = Number(raw);
  if (!/^\d+(\.\d+)?$/.test(raw) || !(hours >= 1 && hours <= MAX_SESSION_HOURS)) {
    throw new ConfigError(`MCP_SESSION_HOURS must be a number from 1 to ${MAX_SESSION_HOURS}`);
  }
  return hours;
}

export function loadConfig(env = process.env) {
  const issuer = publicUrl(env);
  const oauthSecret = secret(env, "MCP_OAUTH_SECRET");
  const signinSecret = secret(env, "CONNECTOR_SIGNIN_SECRET");
  if (oauthSecret === signinSecret) {
    throw new ConfigError("MCP_OAUTH_SECRET and CONNECTOR_SIGNIN_SECRET must be two different secrets");
  }
  const port = Number(value(env, "PORT") ?? 3000);
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new ConfigError("PORT is not a port number");

  return {
    port,
    backendUrl: (value(env, "BACKEND_URL") ?? "http://backend:3000").replace(/\/+$/, ""),
    // Tools staff don't get. Unknown names stop the server at boot, so a newer
    // opn-mcp that renames one fails loudly instead of quietly exposing it.
    hiddenTools: (env.HIDDEN_TOOLS ?? "delete_link,shorten_urls_bulk")
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean),
    // Never taken from a request's Host header.
    issuer,
    // What people enter in Claude, and what every token is issued for.
    resource: `${issuer}/mcp`,
    oauthSecret,
    signinSecret,
    sessionSeconds: Math.round(sessionHours(env) * 3600),
    allowedRedirectUris: parseRedirectUris(value(env, "MCP_ALLOWED_REDIRECT_URIS") ?? DEFAULT_REDIRECT_URIS.join(",")),
    googleClientId: required(env, "GOOGLE_CLIENT_ID", "the dashboard's Google OAuth client"),
    googleClientSecret: required(env, "GOOGLE_CLIENT_SECRET", "the dashboard's Google OAuth client"),
    googleAuthUrl: "https://accounts.google.com/o/oauth2/v2/auth",
    googleTokenUrl: "https://oauth2.googleapis.com/token",
  };
}
