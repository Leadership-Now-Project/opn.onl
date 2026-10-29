# opn.onl at Leadership Now Project

This fork of [ysalitrynskyi/opn.onl](https://github.com/ysalitrynskyi/opn.onl) runs LNP's link shortener:

| Hostname | Container | Access |
|---|---|---|
| `ldr.now` | `frontend-public` (127.0.0.1:3101) | Public: short-link redirects, password and interstitial pages only |
| `links.leadershipnowproject.com` | `frontend-admin` (127.0.0.1:3102) | Cloudflare Access, LNP staff Google accounts |
| `links-mcp.leadershipnowproject.com` | `mcp` (127.0.0.1:3104) | Cloudflare Access with Managed OAuth: the Claude connector |

Each hostname proxies its own `/api/` to the single backend, so browser calls are always same-origin. On `ldr.now` only the redirect, verify and preview routes pass; the rest of `/api/` is 404 and dashboard pages redirect to the admin host.

## Changes from upstream

Kept small so they carry across upstream releases:

- `backend/src/handlers/cf_access.rs` (new): `POST /auth/cf-access` signs in the person Cloudflare Access authenticated, from the `Cf-Access-Jwt-Assertion` header. Off unless `CF_ACCESS_TEAM_DOMAIN` and `CF_ACCESS_AUD` are set. Registered in `handlers/mod.rs`, `lib.rs` and `openapi.rs`; `FIRST_ACCOUNT_ADMIN_LOCK` in `auth.rs` is `pub(crate)` so both sign-up paths share it.
- `frontend/src/pages/Login.tsx`: a build with `VITE_CF_ACCESS=true` calls that endpoint on load. `frontend/src/config/api.ts` names the endpoint.
- `frontend/nginx.conf` and `frontend/Dockerfile`: an `include /etc/nginx/site.d/*.conf;` point (empty in the stock image) and the `VITE_CF_ACCESS` build arg.
- `frontend/themes/lnp.js` and `frontend/themes/lnp/public/` (new): the Leadership Now Project look (navy, grey, Plus Jakarta Sans, square corners, LNP logo and icons), applied only when the frontend is built with `VITE_THEME=lnp` (the dashboard image). `tailwind.config.js` merges it in; `src/index.css` takes its body colours and fonts from the config; `Logo.tsx` shows the LNP logo; the Dockerfile copies the theme's icons over the stock ones. `src/designTokens.lnp.test.ts` checks its colours for WCAG AA contrast.
- `deploy/lnp/` and `.github/workflows/lnp-images.yml`: deployment only. `deploy/lnp/mcp/` is the Claude connector (below); it uses the published `opn-mcp` package unchanged.

The first account to sign in becomes the instance admin.

## Building images

CI (`ci.yml`) runs on every pull request. When it passes, tag the commit and push the tag:

    git tag lnp-1.9.0-1 && git push origin lnp-1.9.0-1

`lnp-images.yml` then publishes `opn-backend`, `opn-frontend-public`, `opn-frontend-admin` and `opn-mcp-remote` at that tag to `ghcr.io/leadership-now-project`.

## Running

On the server, in a copy of this folder:

    cp .env.example .env && chmod 600 .env   # fill in secrets and OPN_TAG
    docker compose pull && docker compose up -d

After changing a file in `nginx/`, restart that container (`docker compose restart frontend-public` or `frontend-admin`). `nginx -s reload` inside the container does not pick up the change, because the file is bind-mounted and replaced, not edited in place.

The host's reverse proxy sends `ldr.now` to 127.0.0.1:3101 and `links.leadershipnowproject.com` to 127.0.0.1:3102. It must pass `Host`, `X-Forwarded-Proto: https` and Cloudflare's headers through unchanged (`FORCE_HTTPS` redirects anything that does not arrive as https).

## Claude connector

`mcp/server.mjs` serves [opn-mcp](https://github.com/ysalitrynskyi/opn-mcp)'s tools over Streamable HTTP at `https://links-mcp.leadershipnowproject.com/mcp`, so staff add it in Claude under Connectors instead of installing anything.

- Cloudflare Access protects the hostname with **Managed OAuth** on (Additional settings), so Claude can run the sign-in itself. Allowed redirect URIs: Claude's callback (`https://claude.ai/*`, `https://claude.com/*`).
- Each request reaches the server with Access's `Cf-Access-Jwt-Assertion`. The server passes it to the backend's `/auth/cf-access`, which checks it and returns a session for that person. Tools then act as them, so links land in their own account. **Add the connector's Access AUD tag to `CF_ACCESS_AUD`** (comma-separated) or every sign-in is refused.
- `delete_link` and `shorten_urls_bulk` are hidden (`HIDDEN_TOOLS`); staff delete in the dashboard.
- `OPN_MCP_TAG` in `.env` pins its image separately from `OPN_TAG`.

## Updating from upstream

    git fetch upstream --tags
    git rebase <new upstream tag>     # on the lnp branch
    # resolve, push, let CI pass, tag lnp-<version>-1

## Licence

AGPL-3.0, as upstream. This public fork is the source offer for the modified service.
