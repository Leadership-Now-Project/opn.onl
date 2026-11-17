// Entry point of the LNP Links connector: read the settings, refuse to start
// on a bad one, listen. The server itself is app.mjs; its sign-in is oauth.mjs.

import { ConfigError, loadConfig } from "./config.mjs";
import { createApp } from "./app.mjs";

let config;
try {
  config = loadConfig(process.env);
} catch (e) {
  if (!(e instanceof ConfigError)) throw e;
  console.error(`LNP Links MCP cannot start: ${e.message}`);
  process.exit(1);
}

createApp(config).listen(config.port, () => {
  console.error(
    `LNP Links MCP on :${config.port} as ${config.issuer}, backend ${config.backendUrl}, ` +
      `sign-ins last ${config.sessionSeconds / 3600} h, hidden: ${config.hiddenTools.join(", ") || "none"}`,
  );
});
