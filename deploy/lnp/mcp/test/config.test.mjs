import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { ConfigError, DEFAULT_REDIRECT_URIS, loadConfig, parseRedirectUris } from "../config.mjs";
import { ISSUER, OAUTH_SECRET, SIGNIN_SECRET, testEnv } from "../test-helpers/helpers.mjs";

const refuses = (env, pattern) => assert.throws(() => loadConfig(env), (e) => e instanceof ConfigError && pattern.test(e.message), JSON.stringify(env));

test("a complete environment loads, with the documented defaults", () => {
  const config = loadConfig(testEnv());
  assert.equal(config.issuer, ISSUER);
  assert.equal(config.resource, `${ISSUER}/mcp`);
  assert.equal(config.sessionSeconds, 168 * 3600);
  assert.deepEqual(config.allowedRedirectUris, ["https://claude.ai/api/mcp/auth_callback", "https://claude.com/api/mcp/auth_callback"]);
  assert.deepEqual(config.allowedRedirectUris, DEFAULT_REDIRECT_URIS);
  assert.equal(config.backendUrl, "http://backend:3000");
  assert.deepEqual(config.hiddenTools, ["delete_link", "shorten_urls_bulk"]);
  assert.equal(config.googleAuthUrl, "https://accounts.google.com/o/oauth2/v2/auth");
  assert.equal(config.googleTokenUrl, "https://oauth2.googleapis.com/token");
});

test("blank settings are unset settings, as compose passes them", () => {
  const config = loadConfig(testEnv({ MCP_SESSION_HOURS: "", MCP_ALLOWED_REDIRECT_URIS: "  ", BACKEND_URL: "" }));
  assert.equal(config.sessionSeconds, 168 * 3600);
  assert.deepEqual(config.allowedRedirectUris, DEFAULT_REDIRECT_URIS);
  refuses(testEnv({ MCP_OAUTH_SECRET: "   " }), /MCP_OAUTH_SECRET is not set/);
});

test("the server secret is required, long enough and not a placeholder", () => {
  refuses(testEnv({ MCP_OAUTH_SECRET: undefined }), /MCP_OAUTH_SECRET is not set/);
  refuses(testEnv({ MCP_OAUTH_SECRET: "" }), /MCP_OAUTH_SECRET is not set/);
  refuses(testEnv({ MCP_OAUTH_SECRET: OAUTH_SECRET.slice(0, 31) }), /at least 32 bytes/);
  for (const placeholder of [
    "changeme-changeme-changeme-changeme-1234",
    "your-secret-goes-here-0123456789abcdefgh",
    "REPLACE-with-openssl-rand-base64-48-xxxx",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "abababababababababababababababababababab",
    "this-is-an-example-secret-0123456789abcd",
  ]) {
    refuses(testEnv({ MCP_OAUTH_SECRET: placeholder }), /placeholder/);
  }
});

test("the secret shared with the backend is required and is a different one", () => {
  refuses(testEnv({ CONNECTOR_SIGNIN_SECRET: undefined }), /CONNECTOR_SIGNIN_SECRET is not set/);
  refuses(testEnv({ CONNECTOR_SIGNIN_SECRET: "short" }), /at least 32 bytes/);
  refuses(testEnv({ CONNECTOR_SIGNIN_SECRET: OAUTH_SECRET }), /two different secrets/);
  assert.equal(loadConfig(testEnv()).signinSecret, SIGNIN_SECRET);
});

test("the public address is required and is a bare https origin", () => {
  refuses(testEnv({ MCP_PUBLIC_URL: undefined }), /MCP_PUBLIC_URL is not set/);
  for (const bad of ["links-mcp.test", "http://links-mcp.test", "https://links-mcp.test/mcp", "https://links-mcp.test/?x=1", "https://links-mcp.test/#f", "https://user@links-mcp.test"]) {
    refuses(testEnv({ MCP_PUBLIC_URL: bad }), /MCP_PUBLIC_URL/);
  }
  assert.equal(loadConfig(testEnv({ MCP_PUBLIC_URL: "https://Links-MCP.test/" })).issuer, "https://links-mcp.test");
});

test("Google's client is required", () => {
  refuses(testEnv({ GOOGLE_CLIENT_ID: "" }), /GOOGLE_CLIENT_ID is not set/);
  refuses(testEnv({ GOOGLE_CLIENT_SECRET: undefined }), /GOOGLE_CLIENT_SECRET is not set/);
});

test("the sign-in length is a number of hours within bounds", () => {
  assert.equal(loadConfig(testEnv({ MCP_SESSION_HOURS: "24" })).sessionSeconds, 86400);
  assert.equal(loadConfig(testEnv({ MCP_SESSION_HOURS: "1.5" })).sessionSeconds, 5400);
  for (const bad of ["0", "0.5", "721", "-1", "abc", "1e3", "24h"]) refuses(testEnv({ MCP_SESSION_HOURS: bad }), /MCP_SESSION_HOURS/);
});

test("the redirect allow-list takes exact https addresses only", () => {
  assert.deepEqual(parseRedirectUris("https://a.example/cb , https://b.example/cb,https://a.example/cb"), ["https://a.example/cb", "https://b.example/cb"]);
  for (const bad of [
    "https://*.claude.ai/api/mcp/auth_callback",
    "https://claude.ai/*",
    "http://claude.ai/api/mcp/auth_callback",
    "http://localhost:3000/callback",
    "https://localhost/callback",
    "https://app.localhost/callback",
    "https://127.0.0.1/callback",
    "https://[::1]/callback",
    "https://user:pw@claude.ai/api/mcp/auth_callback",
    "https://claude.ai/api/mcp/auth_callback#frag",
    "https://CLAUDE.ai/api/mcp/auth_callback",
    "claude.ai/api/mcp/auth_callback",
    "claude://callback",
    ",",
  ]) {
    assert.throws(() => parseRedirectUris(bad), ConfigError, bad);
    refuses(testEnv({ MCP_ALLOWED_REDIRECT_URIS: bad }), /MCP_ALLOWED_REDIRECT_URIS/);
  }
});

test("the process exits instead of listening when a secret is missing", () => {
  const server = fileURLToPath(new URL("../server.mjs", import.meta.url));
  const run = (env) => spawnSync(process.execPath, [server], { env: { PATH: process.env.PATH, SystemRoot: process.env.SystemRoot, PORT: "0", ...env }, encoding: "utf8", timeout: 20000 });

  const missing = run(testEnv({ MCP_OAUTH_SECRET: undefined }));
  assert.equal(missing.status, 1, missing.stderr);
  assert.match(missing.stderr, /cannot start: MCP_OAUTH_SECRET is not set/);

  const placeholder = run(testEnv({ MCP_OAUTH_SECRET: "changeme-changeme-changeme-changeme-1234" }));
  assert.equal(placeholder.status, 1, placeholder.stderr);
  assert.match(placeholder.stderr, /placeholder/);
  // The refused value is not echoed.
  assert.ok(!placeholder.stderr.includes("changeme-changeme"));
});
