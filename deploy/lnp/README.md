# opn.onl at Leadership Now Project

This fork of [ysalitrynskyi/opn.onl](https://github.com/ysalitrynskyi/opn.onl) runs LNP's link shortener:

| Hostname | Container | Access |
|---|---|---|
| `ldr.now` | `frontend-public` (127.0.0.1:3101) | Public: short-link redirects, password and interstitial pages only |
| `links.leadershipnowproject.com` | `frontend-admin` (127.0.0.1:3102) | Sign in with Google on the login page; `leadershipnowproject.com` and `.org` accounts only |
| `links-mcp.leadershipnowproject.com` | `mcp` (127.0.0.1:3104) | Cloudflare Access with Managed OAuth: the Claude connector |

Each hostname proxies its own `/api/` to the single backend, so browser calls are always same-origin. On `ldr.now` only the redirect, verify and preview routes pass; the rest of `/api/` is 404 and dashboard pages redirect to the admin host.

## Changes from upstream

Kept small so they carry across upstream releases:

- `backend/src/handlers/google_auth.rs` (new): `POST /auth/google` signs in a Google account from an allowed email domain. The login page sends the one-time code from Google's popup; the backend redeems it and checks the ID token. Off unless `GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET` and `GOOGLE_ALLOWED_DOMAINS` are set. `GET /auth/settings` then carries `google_client_id`.
- `backend/src/handlers/cf_access.rs` (new): `POST /auth/cf-access` signs in the person Cloudflare Access authenticated, from the `Cf-Access-Jwt-Assertion` header. Off unless `CF_ACCESS_TEAM_DOMAIN` and `CF_ACCESS_AUD` are set. Only the Claude connector uses it now.
- `backend/src/handlers/verified_sign_in.rs` (new): what both of those do once the address is verified (find the account or create it, first account is admin), so they cannot drift. All three are registered in `handlers/mod.rs`, `lib.rs` and `openapi.rs`; `FIRST_ACCOUNT_ADMIN_LOCK` in `auth.rs` is `pub(crate)` so every sign-up path shares it.
- `backend/src/handlers/default_org.rs` (new): the shared organization (below, "Shared links"). `verified_sign_in.rs` calls it after every sign-in; `links.rs`, `folders.rs` and `tags.rs` ask it which organization stands in for a member's personal scope when a request names none; and `GET /auth/settings` carries `default_org`. `GET /links` and `GET /links/export` take `mine=true`, and an organization's list names each link's creator (`created_by`) and says whether the caller may delete it (`can_delete`). Off unless `DEFAULT_ORG_NAME` and `DEFAULT_ORG_SLUG` are set.
- `frontend/src/pages/Login.tsx`: when the settings carry `google_client_id`, the page shows "Sign in with Google" in place of the password form. `frontend/src/config/api.ts` names the endpoint.
- `frontend/src/pages/Dashboard.tsx` and `src/components/dashboard/LinkViewToggle.tsx` (new): when the settings carry `default_org` and the account is a member, the dashboard works in that organization and offers "My Links" / "All LNP Links".
- `frontend/nginx.conf` and `frontend/Dockerfile`: an `include /etc/nginx/site.d/*.conf;` point (empty in the stock image), and the `CSP_GOOGLE_SIGN_IN` build arg, which adds Google's sign-in sources to the Content-Security-Policy. Only the dashboard image sets it; without it the policy is the stock one.
- `frontend/themes/lnp.js` and `frontend/themes/lnp/public/` (new): the Leadership Now Project look (navy, grey, Plus Jakarta Sans, square corners, LNP logo and icons), applied only when the frontend is built with `VITE_THEME=lnp` (the dashboard image). `tailwind.config.js` merges it in; `src/index.css` takes its body colours and fonts from the config; `Logo.tsx` shows the LNP logo; the Dockerfile copies the theme's icons over the stock ones. `src/designTokens.lnp.test.ts` checks its colours for WCAG AA contrast.
- File sharing (below, "File sharing"): `backend/src/utils/files/` (the `FileStore` trait, its S3 and in-memory implementations, how a file is served, the sweep), `backend/src/handlers/files.rs` (`/files/uploads`, `/files/uploads/{id}/complete`, `/files/usage`), the `link_files` table, and `links.rs`, where `POST /links` and completing an upload share `insert_link` and `GET /{code}` signs a file link's address. A file link is a link whose `original_url` is the marker `opn-file:<id>`; the API shows it as an empty URL plus a `file` object. Frontend: `CreateFilesTab.tsx`, `AliasField.tsx` (the alias control, taken out of `CreateLinkForm.tsx` so both tabs use it), and the file row in `LinkRow.tsx`. `frontend/Dockerfile` takes `CSP_FILES_UPLOAD_ORIGIN`. Off unless `FILES_S3_BUCKET` is set; the `/files` routes answer 404 then.
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

## Google sign-in

The dashboard's login page has one button, "Sign in with Google". The backend accepts an account only if Google says its email is verified, the email is at `leadershipnowproject.com` or `leadershipnowproject.org`, and the account is a Google Workspace account of one of those two domains (the `hd` claim, which may name either; `GOOGLE_REQUIRE_HD=false` drops this last check). Someone who already has an account keeps it, with their links: accounts are matched by email.

Google Cloud, project `lnp-links`:

- OAuth consent screen: user type **Internal**.
- Credentials: an OAuth 2.0 client ID of type **Web application**, with the authorised JavaScript origin `https://links.leadershipnowproject.com`. No redirect URI is needed (the popup flow uses `postmessage`).
- Put the client ID and secret in `.env` as `GOOGLE_CLIENT_ID` and `GOOGLE_CLIENT_SECRET`.

On this host `nginx/admin.conf` refuses every other way to get a session (password sign-in, registration, password reset, passkeys, `/auth/cf-access`).

Moving from Cloudflare Access to Google sign-in, in this order:

1. Deploy the new backend and dashboard images, the new `nginx/admin.conf` and the `GOOGLE_*` settings together, while the Access application still protects the hostname. `admin.conf` goes with the new dashboard image: an older image signs in through `/api/auth/cf-access`, which the new file refuses.
2. Through Access, open the dashboard and sign in with Google once with a `.com` account and once with a `.org` account. Check each sees its existing links.
3. Only then remove the Access application from `links.leadershipnowproject.com`. Leave the connector's Access application (`links-mcp.…`) alone, and take the removed application's AUD tag out of `CF_ACCESS_AUD` so only the connector's is left.

If a staff account is refused in step 2, the backend log says why (`Google sign-in refused: …`), without the address.

## Shared links

All staff links live in one organization, so nobody's links are out of reach when they are away or leave. `.env`:

```
DEFAULT_ORG_NAME=Leadership Now Project
DEFAULT_ORG_SLUG=lnp
DEFAULT_ORG_ADMINS=jay.shapiro@leadershipnowproject.com,marie@leadershipnowproject.com,dan.mathis@leadershipnowproject.com
```

- **Joining.** Each account joins the organization the next time it signs in (dashboard or Claude connector), as an editor. There is no manual step. The organization itself is created at the first sign-in after the setting is deployed; its owner is the first address in `DEFAULT_ORG_ADMINS` that already has an account.
- **Admins.** An address in `DEFAULT_ORG_ADMINS` becomes an admin at its next sign-in. The list only promotes: taking an address off it does not demote that person. Change a role by hand under Teams.
- **Existing links.** At that same sign-in the person's personal links, folders and tags move into the organization, once. A link in the trash is not moved. Until a colleague has signed in again, their older links are not in "All LNP Links" yet.
- **New links** are created in the organization, from the dashboard and from the Claude connector alike. The same goes for new folders and tags, and the connector lists the organization's folders and tags.
- **The dashboard** opens on "My Links" (the links you created) and switches to "All LNP Links", which shows who created each one. The choice is remembered per browser.
- **Who can do what** is the organization rule: every member sees and edits every link; deleting is for the link's creator and for admins. Folders and tags are shared: everyone sees and can use and rename all of them, and two people's folders of the same name both appear.
- Links that expire: the reminder email is for personal links only, so links in the organization get none (this deployment has no SMTP anyway).

Do not change `DEFAULT_ORG_SLUG` afterwards: the slug is how the organization is found, and a new slug makes a new, empty one. To turn the feature off, blank the name or the slug; the organization and its links stay as they are, and people then see them under Teams.

## File sharing

From the dashboard's **Files** tab, staff upload a file of any type and share it as a short link (`https://ldr.now/<code>`). Off until `FILES_S3_BUCKET` is set: the tab then says file sharing is not set up, and nothing else changes.

How it works:

- The browser uploads straight to the bucket with a signed URL that works for 15 minutes, so a large file never passes through the app server or Cloudflare's upload limit. The largest file is `FILES_MAX_BYTES` (default 500 MB).
- Opening the short link runs every check any link has (deleted, expired, scheduled, password, click limit), counts the click, then redirects to a signed download address that works for 5 minutes. Nothing else ever shows that address: not the link list, the CSV export, the preview page, or the cache.
- PDF, PNG, JPEG, GIF and WebP open in the browser, and only when the file's name and its declared type agree. Everything else downloads, including HTML, SVG, XML and scripts, whatever type the uploader's browser gave it.
- A file link may have no expiry (the default). The file is deleted from the bucket when its link is deleted or expires. Deleting removes it at once; expiry is picked up by a sweep that runs when the backend starts and then daily, which also retries any delete the bucket refused and removes uploads that were started and never completed (after 24 hours).
- Each person sees the storage their own uploads use, in the Files tab.
- A file link cannot be cloned or pointed at a URL, and has no routing rules. An admin who restores a deleted file link gets a link whose file is gone; it answers "This file is no longer available".

### The bucket

A new bucket, used for nothing else (not the backup bucket). The name must not contain dots: uploads go to `https://<bucket>.s3.<region>.amazonaws.com`, and a dot breaks the TLS certificate match.

1. **Private.** Leave all four **Block Public Access** settings on, and add no bucket policy that grants anyone read access. Files reach visitors only through the short link.
2. **CORS**, so the dashboard page may send the upload (Permissions, Cross-origin resource sharing):

    ```json
    [
      {
        "AllowedOrigins": ["https://links.leadershipnowproject.com"],
        "AllowedMethods": ["PUT"],
        "AllowedHeaders": ["content-type"],
        "ExposeHeaders": [],
        "MaxAgeSeconds": 3000
      }
    ]
    ```

    No rule for `GET`: a visitor's browser is redirected to the file, which is a plain navigation and needs no CORS.

3. **Lifecycle rule**, whole bucket: *Delete expired object delete markers or incomplete multipart uploads*, abort incomplete multipart uploads after 1 day. Uploads are single `PUT`s today; the rule is there so a later, multipart uploader cannot leave invisible parts behind.

    ```json
    {
      "Rules": [
        {
          "ID": "abort-incomplete-multipart-uploads",
          "Status": "Enabled",
          "Filter": {},
          "AbortIncompleteMultipartUpload": { "DaysAfterInitiation": 1 }
        }
      ]
    }
    ```

    Do not add a rule that expires objects by age: a file link with no expiry would lose its file.

4. **IAM policy** for the credentials the backend uses, and nothing more:

    ```json
    {
      "Version": "2012-10-17",
      "Statement": [
        {
          "Sid": "FileSharingObjects",
          "Effect": "Allow",
          "Action": ["s3:PutObject", "s3:GetObject", "s3:DeleteObject"],
          "Resource": "arn:aws:s3:::BUCKET_NAME/*"
        }
      ]
    }
    ```

    `arn:aws:s3:::BUCKET_NAME` (the bucket) and `arn:aws:s3:::BUCKET_NAME/*` (the objects in it) are different resources. These three actions are object actions, so the resource must end in `/*`; with the bare bucket ARN every upload and download is refused with 403. The policy deliberately has no `s3:ListBucket`. One consequence: asking S3 about an object that is not there answers 403, not 404, so completing an upload whose file never arrived reports "File storage is unavailable" instead of "The file has not been uploaded yet". Either way no link is made.

### Settings

`.env` (see `.env.example`): `FILES_S3_BUCKET`, `FILES_S3_REGION` (default `us-east-2`), and either an IAM user's `FILES_S3_ACCESS_KEY` and `FILES_S3_SECRET_KEY`, or both blank to use the server's own AWS credentials (an instance role with the policy above; the instance's metadata hop limit must be 2 for a container to reach it). `FILES_S3_ENDPOINT` stays blank for AWS.

The dashboard's Content-Security-Policy must allow the upload. Set the repository variable `LNP_FILES_UPLOAD_ORIGIN` to `https://<bucket>.s3.<region>.amazonaws.com` (Settings, Secrets and variables, Actions, Variables) **before** building the image tag that turns the feature on. `lnp-images.yml` passes it to the dashboard image as the `CSP_FILES_UPLOAD_ORIGIN` build arg. Without it the Files tab starts an upload and the browser blocks it.

Turning it on, in this order: create the bucket (steps 1 to 4), set the repository variable, build and deploy an image tag, then add the `FILES_*` settings to `.env` and `docker compose up -d`. Check with one small PDF and one `.html` file: the PDF opens in the browser, the HTML file downloads.

To turn it off, blank `FILES_S3_BUCKET`. Existing file links then answer "This file is no longer available" and their files stay in the bucket until it is set again.

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
