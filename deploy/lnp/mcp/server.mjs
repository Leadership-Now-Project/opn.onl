// LNP Links remote MCP server: opn-mcp's tools over Streamable HTTP, so staff
// add one URL in Claude (Connectors) instead of installing anything.
//
// Cloudflare Access sits in front of this hostname with Managed OAuth: Claude
// signs the person in with Google through Access, and every request that
// reaches us carries Access's signed identity in Cf-Access-Jwt-Assertion. We
// never check it ourselves; the backend's /auth/cf-access does (keys, aud,
// email domain) and answers with a session for that person, exactly as the
// dashboard's login does. Tools then run as that person, so links land in
// their own account.
//
// Stateless: a fresh MCP server per request, no sessions to keep.

import http from "node:http";
import { StreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/streamableHttp.js";
import { OpnClient } from "opn-mcp/dist/client.js";
import { createServer } from "opn-mcp/dist/server.js";

const PORT = Number(process.env.PORT || 3000);
const BACKEND_URL = (process.env.BACKEND_URL || "http://backend:3000").replace(/\/+$/, "");
// Tools staff don't get. Unknown names stop the server at boot, so a newer
// opn-mcp that renames one fails loudly instead of quietly exposing it.
const HIDDEN_TOOLS = (process.env.HIDDEN_TOOLS ?? "delete_link,shorten_urls_bulk")
  .split(",")
  .map((s) => s.trim())
  .filter(Boolean);
const MAX_BODY_BYTES = 1_000_000;

// The backend sits behind TLS-terminating proxies: without X-Forwarded-Proto it
// redirects to https, and without the caller's address every staff member
// would share this container's rate-limit bucket.
function proxyHeaders(req) {
  const headers = { "X-Forwarded-Proto": "https" };
  const ip = req.headers["cf-connecting-ip"];
  if (typeof ip === "string" && ip) headers["CF-Connecting-IP"] = ip;
  return headers;
}

class BackendClient extends OpnClient {
  constructor(token, extraHeaders) {
    super({ baseUrl: BACKEND_URL, apiKey: token });
    this.extraHeaders = extraHeaders;
  }
  // opn-mcp sends these on every call, JSON and binary alike.
  authHeader() {
    return { ...super.authHeader(), ...this.extraHeaders };
  }
}

function buildServer(client) {
  const server = createServer(() => client);
  for (const name of HIDDEN_TOOLS) {
    const tool = server._registeredTools[name];
    if (!tool) throw new Error(`HIDDEN_TOOLS names an unknown tool: ${name}`);
    tool.remove();
  }
  return server;
}

/** Trade the Access assertion for an opn.onl session token. */
async function signIn(assertion, headers) {
  const res = await fetch(`${BACKEND_URL}/auth/cf-access`, {
    method: "POST",
    headers: { ...headers, "Cf-Access-Jwt-Assertion": assertion },
  });
  if (!res.ok) return { status: res.status };
  const { token } = await res.json();
  return { token };
}

function readJson(req) {
  return new Promise((resolve, reject) => {
    let size = 0;
    const chunks = [];
    req.on("data", (c) => {
      size += c.length;
      if (size > MAX_BODY_BYTES) {
        reject(Object.assign(new Error("body too large"), { status: 413 }));
        req.destroy();
      } else chunks.push(c);
    });
    req.on("end", () => {
      try {
        resolve(JSON.parse(Buffer.concat(chunks).toString("utf8")));
      } catch {
        reject(Object.assign(new Error("invalid JSON"), { status: 400 }));
      }
    });
    req.on("error", reject);
  });
}

function jsonRpcError(res, status, message) {
  res.writeHead(status, { "Content-Type": "application/json" });
  res.end(JSON.stringify({ jsonrpc: "2.0", error: { code: -32000, message }, id: null }));
}

async function handle(req, res) {
  const path = new URL(req.url, "http://x").pathname;
  if (path === "/health") {
    res.writeHead(200, { "Content-Type": "text/plain" });
    return res.end("ok");
  }
  if (path !== "/mcp") return jsonRpcError(res, 404, "Not found");
  // Stateless: no server-to-client stream (GET) and no session to end (DELETE).
  if (req.method !== "POST") {
    res.setHeader("Allow", "POST");
    return jsonRpcError(res, 405, "Method not allowed");
  }

  const assertion = req.headers["cf-access-jwt-assertion"];
  if (typeof assertion !== "string" || !assertion) {
    return jsonRpcError(res, 401, "Not signed in through Cloudflare Access");
  }
  const headers = proxyHeaders(req);
  const { token, status } = await signIn(assertion, headers);
  if (!token) {
    console.error(`sign-in refused by backend: ${status}`);
    return jsonRpcError(res, status === 403 ? 403 : 401, "Sign-in refused");
  }

  const body = await readJson(req);
  const server = buildServer(new BackendClient(token, headers));
  const transport = new StreamableHTTPServerTransport({
    sessionIdGenerator: undefined,
    enableJsonResponse: true,
  });
  res.on("close", () => {
    transport.close();
    server.close();
  });
  await server.connect(transport);
  await transport.handleRequest(req, res, body);
}

// Fail at boot, not on the first call, if HIDDEN_TOOLS is wrong.
buildServer(new BackendClient("boot-check", {}));

http
  .createServer((req, res) => {
    handle(req, res).catch((e) => {
      console.error(e);
      if (!res.headersSent) jsonRpcError(res, e.status || 500, e.status ? e.message : "Internal error");
    });
  })
  .listen(PORT, () => {
    console.error(`LNP Links MCP on :${PORT}, backend ${BACKEND_URL}, hidden: ${HIDDEN_TOOLS.join(", ") || "none"}`);
  });
