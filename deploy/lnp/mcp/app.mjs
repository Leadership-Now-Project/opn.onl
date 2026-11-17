// LNP Links remote MCP server: opn-mcp's tools over Streamable HTTP, so staff
// add one URL in Claude (Connectors) instead of installing anything.
//
// The server signs people in itself (oauth.mjs): Claude is sent through
// Google, the backend checks the Google ID token and names the person's
// opn.onl account, and Claude gets an access token that carries a session for
// that account. Every /mcp request must bring such a token; tools then run as
// that person, so links land in their own account.
//
// Stateless: a fresh MCP server per request, no sessions to keep.

import express from "express";
import { StreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/streamableHttp.js";
import { OpnClient } from "opn-mcp/dist/client.js";
import { createServer } from "opn-mcp/dist/server.js";
import { createOAuth, proxyHeaders } from "./oauth.mjs";

const MAX_BODY_BYTES = 1_000_000;
const BACKEND_TIMEOUT_MS = 8000;

function jsonRpcError(res, status, message) {
  res.status(status).json({ jsonrpc: "2.0", error: { code: -32000, message }, id: null });
}

export function createApp(config, options = {}) {
  class BackendClient extends OpnClient {
    constructor(token, extraHeaders) {
      super({ baseUrl: config.backendUrl, apiKey: token });
      this.extraHeaders = extraHeaders;
    }
    // opn-mcp sends these on every call, JSON and binary alike.
    authHeader() {
      return { ...super.authHeader(), ...this.extraHeaders };
    }
  }

  function buildServer(client) {
    const server = createServer(() => client);
    for (const name of config.hiddenTools) {
      const tool = server._registeredTools[name];
      if (!tool) throw new Error(`HIDDEN_TOOLS names an unknown tool: ${name}`);
      tool.remove();
    }
    return server;
  }
  // Fail at boot, not on the first call, if HIDDEN_TOOLS is wrong.
  buildServer(new BackendClient("boot-check", {}));

  const oauth = createOAuth(config, options);
  const app = express();
  app.disable("x-powered-by");

  app.get("/health", (_req, res) => res.type("text/plain").send("ok"));
  app.use(oauth.router);

  app.all("/mcp", express.json({ limit: MAX_BODY_BYTES }), async (req, res) => {
    // Stateless: no server-to-client stream (GET) and no session to end (DELETE).
    if (req.method !== "POST") {
      res.set("Allow", "POST");
      return jsonRpcError(res, 405, "Method not allowed");
    }
    const person = oauth.authenticate(req, res);
    if (!person) return jsonRpcError(res, 401, "Sign in to LNP Links");

    // The session inside the token is at most an hour old, but the person may
    // have been removed since: ask the backend before running anything.
    const headers = proxyHeaders(req);
    let status;
    try {
      const me = await fetch(`${config.backendUrl}/auth/me`, {
        headers: { ...headers, Authorization: `Bearer ${person.backendToken}` },
        signal: AbortSignal.timeout(BACKEND_TIMEOUT_MS),
      });
      status = me.status;
    } catch {
      return jsonRpcError(res, 502, "LNP Links is not reachable right now");
    }
    if (status === 401 || status === 403) {
      console.error(`session refused by backend: ${status}`);
      oauth.endSignIn(person.sid, person.sessionEnd);
      oauth.challenge(res, "The sign-in has ended");
      return jsonRpcError(res, 401, "Sign in to LNP Links");
    }
    if (status !== 200) return jsonRpcError(res, 502, "LNP Links is not reachable right now");

    const server = buildServer(new BackendClient(person.backendToken, headers));
    const transport = new StreamableHTTPServerTransport({
      sessionIdGenerator: undefined,
      enableJsonResponse: true,
    });
    res.on("close", () => {
      transport.close();
      server.close();
    });
    await server.connect(transport);
    await transport.handleRequest(req, res, req.body);
  });

  app.use((_req, res) => jsonRpcError(res, 404, "Not found"));
  // Express's own handler would print the stack to the caller.
  // eslint-disable-next-line no-unused-vars
  app.use((err, _req, res, _next) => {
    const status = Number.isInteger(err?.status) && err.status >= 400 && err.status < 500 ? err.status : 500;
    if (status === 500) console.error(err);
    if (!res.headersSent) jsonRpcError(res, status, status === 500 ? "Internal error" : "Bad request");
  });

  app.locals.oauth = oauth;
  return app;
}
