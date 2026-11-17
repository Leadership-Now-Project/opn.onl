# opn.onl at Leadership Now Project

This fork of [ysalitrynskyi/opn.onl](https://github.com/ysalitrynskyi/opn.onl) runs LNP's link shortener:

| Hostname | Container | Access |
|---|---|---|
| `ldr.now` | `frontend-public` (127.0.0.1:3101) | Public: short-link redirects, the password and interstitial pages, and the Secure View viewer |
| `links.leadershipnowproject.com` | `frontend-admin` (127.0.0.1:3102) | Sign in with Google on the login page; `leadershipnowproject.com` and `.org` accounts only |
| `links-mcp.leadershipnowproject.com` | `mcp` (127.0.0.1:3104) | The Claude connector. It signs people in itself (OAuth 2.1, then Google); `leadershipnowproject.com` and `.org` accounts only |

Each hostname proxies its own `/api/` to the single backend, so browser calls are always same-origin. On `ldr.now` only the redirect, verify and preview routes and the Secure View viewer's two calls pass; the rest of `/api/` is 404 and dashboard pages redirect to the admin host.

## Changes from upstream

Kept small so they carry across upstream releases:

- `backend/src/handlers/google_auth.rs` (new): `POST /auth/google` signs in a Google account from an allowed email domain. The login page sends the one-time code from Google's popup; the backend redeems it and checks the ID token. Off unless `GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET` and `GOOGLE_ALLOWED_DOMAINS` are set. `GET /auth/settings` then carries `google_client_id`.
- `backend/src/handlers/cf_access.rs` (new): `POST /auth/cf-access` signs in the person Cloudflare Access authenticated, from the `Cf-Access-Jwt-Assertion` header. Off unless `CF_ACCESS_TEAM_DOMAIN` and `CF_ACCESS_AUD` are set. This deployment no longer sets them, so it is off here.
- `backend/src/handlers/connector_auth.rs` (new): the Claude connector's sign-in (below, "Claude connector"). `POST /auth/connector/google` takes the Google ID token the connector received and checks it with the same function as `/auth/google`; `POST /auth/connector/session` renews that session for the connector, against an assertion signed with `CONNECTOR_SIGNIN_SECRET`, while the account is still live and unchanged. Off unless Google sign-in is configured.
- `backend/src/handlers/verified_sign_in.rs` (new): what all of those do once the address is verified (find the account or create it, first account is admin), so they cannot drift. All are registered in `handlers/mod.rs`, `lib.rs` and `openapi.rs`; `FIRST_ACCOUNT_ADMIN_LOCK` in `auth.rs` is `pub(crate)` so every sign-up path shares it.
- `backend/src/handlers/default_org.rs` (new): the shared organization (below, "Shared links"). `verified_sign_in.rs` calls it after every sign-in; `links.rs`, `folders.rs` and `tags.rs` ask it which organization stands in for a member's personal scope when a request names none; and `GET /auth/settings` carries `default_org`. `GET /links` and `GET /links/export` take `mine=true`, and an organization's list names each link's creator (`created_by`) and says whether the caller may delete it (`can_delete`). Off unless `DEFAULT_ORG_NAME` and `DEFAULT_ORG_SLUG` are set.
- `frontend/src/pages/Login.tsx`: when the settings carry `google_client_id`, the page shows "Sign in with Google" in place of the password form. `frontend/src/config/api.ts` names the endpoint.
- `frontend/src/pages/Dashboard.tsx` and `src/components/dashboard/LinkViewToggle.tsx` (new): when the settings carry `default_org` and the account is a member, the dashboard works in that organization and offers "My Links" / "All LNP Links".
- `frontend/nginx.conf` and `frontend/Dockerfile`: an `include /etc/nginx/site.d/*.conf;` point (empty in the stock image), and the `CSP_GOOGLE_SIGN_IN` build arg, which adds Google's sign-in sources to the Content-Security-Policy. Only the dashboard image sets it; without it the policy is the stock one.
- `frontend/themes/lnp.js` and `frontend/themes/lnp/public/` (new): the Leadership Now Project look (navy, grey, Plus Jakarta Sans, square corners, LNP logo and icons), applied only when the frontend is built with `VITE_THEME=lnp` (the dashboard image). `tailwind.config.js` merges it in; `src/index.css` takes its body colours and fonts from the config; `Logo.tsx` shows the LNP logo; the Dockerfile copies the theme's icons over the stock ones. `src/designTokens.lnp.test.ts` checks its colours for WCAG AA contrast.
- File sharing (below, "File sharing"): `backend/src/utils/files/` (the `FileStore` trait, its S3 and in-memory implementations, how a file is served, the sweep), `backend/src/handlers/files.rs` (`/files/uploads`, `/files/uploads/{id}/complete`, `/files/usage`), the `link_files` table, and `links.rs`, where `POST /links` and completing an upload share `insert_link` and `GET /{code}` signs a file link's address. A file link is a link whose `original_url` is the marker `opn-file:<id>`; the API shows it as an empty URL plus a `file` object. Frontend: `CreateFilesTab.tsx`, `AliasField.tsx` (the alias control, taken out of `CreateLinkForm.tsx` so both tabs use it), and the file row in `LinkRow.tsx`. `frontend/Dockerfile` takes `CSP_FILES_UPLOAD_ORIGIN`. Off unless `FILES_S3_BUCKET` is set; the `/files` routes answer 404 then.
- Secure View (below, "Secure View"): `backend/src/utils/secure_view.rs` (the ticket and view token, the protected copy of a PDF, where the viewer's bytes come from), `backend/src/handlers/secure_view.rs` (`PUT /links/{id}/secure-view`, `POST /{code}/view`, `GET /{code}/view/file`), two columns on `link_files`, `click_events.viewer_email`, and `get`/`put` on the `FileStore` trait. `GET /{code}` sends a Secure View link to the viewer instead of signing an address. Frontend: `src/pages/SecureView.tsx` and `src/components/viewer/` (the viewer, with PDF.js), `SecureViewSwitch.tsx`, and `VITE_PUBLIC_LINKS_ONLY` in `Layout.tsx`. New dependencies: `lopdf` (backend) and `pdfjs-dist` (frontend). Does nothing unless file sharing is on.
- Google Drive links (below, "Google Drive picker"): `frontend/src/utils/googleDrive.ts` (the Drive token, Google's picker, the two Drive calls), `DriveFilePanel.tsx` (the "Google Drive Files" button and its confirmation panel) and the Drive row in `LinkRow.tsx`. Backend: `utils/drive.rs`, the `link_drive_files` table, an optional `drive_file` on `POST /links`, `drive` on every link answer, and `drive` in `GET /auth/settings`. A Drive link is an ordinary URL link to the file's address in Drive. `frontend/Dockerfile` takes `CSP_GOOGLE_DRIVE_PICKER`. Off unless `GOOGLE_PICKER_API_KEY` and `GOOGLE_PICKER_APP_ID` are set.
- Media links (below, "Media pages"): `backend/src/utils/media.rs` (reading a YouTube or Vimeo video out of an address, the player's address, the ticket, the embed check), `backend/src/handlers/media.rs` (`POST /{code}/media`), the `link_media` table, an optional `media` on `POST /links`, `media_cta` on `PUT /links/{id}`, and `media` on every link answer. `GET /{code}` sends a media link to the page instead of the video's host. A media link is an ordinary URL link to the video's own page. Frontend: `CreateMediaTab.tsx`, `MediaCtaFields.tsx`, `src/pages/MediaPage.tsx`, `src/utils/media.ts`, a `media` mode of `DriveFilePanel.tsx`, and the badge in `LinkRow.tsx`. `frontend/Dockerfile` takes `CSP_MEDIA_EMBEDS`. A link to a YouTube or Vimeo video, or into Google Drive, is no longer kept in the Redis redirect cache, media link or not.
- `deploy/lnp/` and `.github/workflows/lnp-images.yml`: deployment only. `deploy/lnp/mcp/` is the Claude connector (below); it uses the published `opn-mcp` package unchanged, and the MCP SDK's own handlers for the OAuth token and registration endpoints.

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

On this host `nginx/admin.conf` refuses every other way to get a session (password sign-in, registration, password reset, passkeys, `/auth/cf-access`, and the connector's `/auth/connector/…`).

Moving from Cloudflare Access to Google sign-in, in this order:

1. Deploy the new backend and dashboard images, the new `nginx/admin.conf` and the `GOOGLE_*` settings together, while the Access application still protects the hostname. `admin.conf` goes with the new dashboard image: an older image signs in through `/api/auth/cf-access`, which the new file refuses.
2. Through Access, open the dashboard and sign in with Google once with a `.com` account and once with a `.org` account. Check each sees its existing links.
3. Only then remove the Access application from `links.leadershipnowproject.com`. (The connector's Access application has since gone too: see "Claude connector".)

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

## Secure View

A switch on an uploaded file's link, "Secure View Share", off by default. It is on the Files tab when uploading and in the link's edit dialog afterwards. With it on, `https://ldr.now/<code>` does not hand over the file: it opens a viewer that asks the visitor for an email address and then shows the document. With it off the link behaves as in "File sharing" above. It needs file sharing to be on, and it works for the files a browser can show: PDF, PNG, JPEG, GIF and WebP. For anything else the switch is greyed out.

These are deterrents, not locks. A visitor can still photograph or screenshot the screen, and someone technical can still get the bytes their own browser was sent. What the viewer does:

- no download, print or open button; right-click, text selection, drag, copy, Ctrl/Cmd+P and Ctrl/Cmd+S do nothing, and printing the page prints nothing;
- the visitor's email and the date and time (UTC) are drawn across every page, in the page itself and again on top of it;
- the document blurs when the window is not the one in front;
- a PDF is drawn page by page with no text in the page, so there is nothing to select or search.

What the server guarantees:

- The file is reached only after every check the link has (deleted, expired, scheduled, password, click limit) and an email. The checks run when the link is opened, again when the email is given, and again each time the viewer fetches the file (all but the click limit, which that visit has already been counted against).
- No address of the bucket is ever sent for a Secure View link. The viewer gets the bytes from the app server, which reads them from the bucket.
- Opening the link gives the visitor a ticket that works for 15 minutes, once; giving an email trades it for a token that fetches the file for 5 minutes. Neither is ever in an address sent to a server: the ticket travels in the part of the address after `#`, which browsers keep to themselves, and the token in a request header. Changing or removing the link's password ends both at once, and so does switching Secure View off: switching it on again does not bring them back.
- The click is counted, and a one-time or click-limited link is used up, when the email is given: not by opening the link, and not by a chat app drawing a preview.
- A visitor may type any address. It is not verified.

**Who sees the emails.** The link's creator, and the owners and admins of the organization (and an instance admin), in the link's analytics under "Recent clicks". Other members see the views without the addresses. The addresses are erased with the rest of a click's visitor data. For every link, that is after the retention window (13 months by default). For a personal link (one that belongs to no organization) it is also when the creator's account is deleted. A link in an organization belongs to the organization, not to the person who made it, and at LNP that is every staff link ("Shared links" above): deleting that person's account does not erase its addresses, which stay until the retention window.

**The protected copy.** When Secure View is switched on for a PDF of at most 50 MB (`SECURE_VIEW_PROTECT_MAX_BYTES`), the server makes a second copy of it marked "no printing, no copying, no changes" (AES-256, opens without a password) and the viewer is given that one. This is best effort: a PDF that is larger, damaged, not quite to the standard or already encrypted gets no copy and is shown from the original, with a line in the backend log (`Secure View: file … could not be given a protected copy`, followed by the reason). Making the copy never inflates the PDF's page contents, and the step works within a budget (`SECURE_VIEW_PROTECT_BUDGET_BYTES`, 256 MB by default) and refuses a PDF with more than 20,000 pages or 1,000,000 objects. That does not bound everything: the PDF library unpacks a file's object streams while it reads it, and a small file built for the purpose (measured: 33 KB) can make that step take gigabytes. Only someone who can upload and switch Secure View on can cause it. The backend container therefore has a memory ceiling in `docker-compose.yml` (`mem_limit: 1g`): such a file ends the backend, which restarts in a few seconds, instead of starving the host. The copy is not retried at start, so it does not loop. The copy is made in the background a moment after switching on. It is deleted with the original (link deleted or expired, account or organization deleted) and when Secure View is switched off.

Nothing to set up beyond file sharing. Three things to know when deploying it:

- **Images.** `lnp-images.yml` builds the public image with `VITE_THEME=lnp` and `VITE_PUBLIC_LINKS_ONLY=true`, so the viewer, the password page and the interstitial carry the LNP look and a header with the logo alone. Deploy the backend, both frontend images and `nginx/public.conf` together: the viewer's two calls (`/api/<code>/view`, `/api/<code>/view/file`) are allowed by the new `public.conf`, and an older one answers them 404.
- **The file passes through the app server and Cloudflare** to the visitor (an ordinary file link does not). `public.conf` passes it straight through with a 5 minute timeout. Nothing in the app caches it (`Cache-Control: no-store`).
- **The bucket policy is unchanged**: the backend already has `s3:GetObject` and `s3:PutObject`. No CORS rule is needed, because the browser never talks to the bucket.

Check it with one PDF: switch Secure View on, open the link in a private window, give an address, see the watermark, and find the address under the link's analytics.

## Google Drive picker

On the **Files** tab, "Google Drive Files" opens Google's own file picker. The person picks one file and gets a short link (`https://ldr.now/<code>`) that redirects to the file where it lives in Drive: nothing is copied, so the link always opens the current version. Off until `GOOGLE_PICKER_API_KEY` and `GOOGLE_PICKER_APP_ID` are both set (and Google sign-in is on): the button then says it is not set up, and nothing else changes.

How it works:

- Everything that talks to Google runs in the person's browser. The app server never receives a Google token; it stores the link and which Drive file it was made for (`link_drive_files`).
- The first click asks the person, in Google's window, to let LNP Links see the files they pick with it (the `drive.file` scope, and no other). It cannot list or read the rest of their Drive. Google remembers the answer, so later clicks go straight to the picker.
- After picking, the panel shows who can open the file ("Anyone with the link can view", "Only people at <domain> with the link", "Restricted"). If outsiders would be locked out it says so and offers a tick box, unticked, "Change to: anyone with the link can view". Sharing is changed only when that box is ticked and Create is clicked, and the link is not made if Google refuses the change (a Workspace sharing policy, a shared-drive rule). Left unticked, the link is still made and the panel says recipients may have to request access.
- The link is an ordinary one: it can be edited, cloned, tagged, given an expiry or a password. Changing its destination makes it a plain URL link. Deleting it does nothing to the file in Drive.

Google Cloud, project `lnp-links` (the same project as Google sign-in):

1. **APIs and services, Library**: enable **Google Picker API** and **Google Drive API**.
2. **Credentials, Create credentials, API key**. Restrict it: under *Application restrictions* choose **Websites** and add `https://links.leadershipnowproject.com/*`; under *API restrictions* choose **Restrict key** and tick **Google Picker API** only. This is a browser key, visible to anyone who signs in to the dashboard; the two restrictions are what make that safe. Put it in `.env` as `GOOGLE_PICKER_API_KEY`.
3. `GOOGLE_PICKER_APP_ID` is the project **number** (Cloud console, project picker or IAM and admin, Settings), not its name or id.
4. The OAuth consent screen stays **Internal**, and the OAuth client is the one sign-in already uses: its authorised JavaScript origin `https://links.leadershipnowproject.com` covers the picker. The only scope asked for is `https://www.googleapis.com/auth/drive.file`, which Google classes as non-sensitive.

The dashboard's Content-Security-Policy must allow the picker: `lnp-images.yml` builds the dashboard image with `CSP_GOOGLE_DRIVE_PICKER=true`, which adds `https://apis.google.com` to `script-src`, `https://www.googleapis.com` to `connect-src` and `https://docs.google.com` to `frame-src`. An image built before that change shows the button and the browser blocks the picker.

Turning it on: deploy an image tag built from this version, add the two settings to `.env`, `docker compose up -d`. Check with a file only you can open (the panel warns, the box is unticked, and the link made without ticking it asks an outsider to request access) and with one that anyone with the link can view.

To turn it off, blank either setting. Links already made keep working: they are ordinary links.

## Media pages

On the **Media** tab, a YouTube or Vimeo address, or a video picked in Google Drive ("Google Drive video"), becomes a short link that opens a page of its own instead of sending the visitor to the video's host: the LNP logo, the link's title, the video in its host's own player, and an optional button (the sharer's label and `https` link). The video is not copied anywhere.

- **Which video.** The server reads the video out of the address (YouTube: `watch?v=`, `youtu.be/`, `/shorts/`, `/live/`, `/embed/`; Vimeo: `vimeo.com/<id>`, `vimeo.com/<id>/<hash>` for an unlisted video, `player.vimeo.com/video/<id>`) and builds the player's address itself. The page can only ever frame `www.youtube-nocookie.com`, `player.vimeo.com` or `drive.google.com`.
- **A video that cannot be embedded is refused when the link is created.** The server asks the host's public oEmbed address (`https://www.youtube.com/oembed`, `https://vimeo.com/api/oembed.json`; no other address is ever fetched) and refuses the link when the host says the video is private, missing or not embeddable. If the host cannot be reached, the link is made and the dashboard says it could not be checked. `MEDIA_EMBED_CHECK=false` turns the check off. The backend container needs outbound HTTPS to those two hosts for it.
- **A Drive video** plays in Google's own player and only for people who can open the file, so it gets the same sharing check and the same unticked "Change to: anyone with the link can view" box as "Google Drive picker" above. It needs the picker to be set up. Only a video file can be picked.
- **Password, expiry, start date, click limit and one-time links work as for any link**, and opening the link counts one click. Opening it gives the visitor a ticket in the part of the address after `#`, good for an hour, which the page trades for the video (so a reload works without counting again). A password-protected link gives no ticket, and so no video, without the password. Changing or removing the password ends the tickets already given.
- **Not offered:** Secure View, smart routing rules, and editing the destination (changing a media link's destination through the API makes it a plain link). The button can be changed in the link's edit dialog.
- **Statistics** count opens of the link. Whether the video was played is not known to the app.

Three things to deploy together, from an image tag built from this version:

- **The public image** is built by `lnp-images.yml` with `CSP_MEDIA_EMBEDS=true`, which adds `frame-src 'self' https://www.youtube-nocookie.com https://player.vimeo.com https://drive.google.com` to its Content-Security-Policy. With an older public image the page opens and the browser blocks the player.
- **`nginx/public.conf`** allows the page's one call, `/api/<code>/media`. An older one answers it 404 and the page says the video could not be loaded.
- **The backend**, which adds one table (`link_media`) at start. Nothing is rewritten.

Before deploying, check that no existing link has the code `m` or `media`: both are now reserved for new links. A link that already has one keeps redirecting.

Check it with one YouTube video that is public, one whose owner turned embedding off (refused, with the reason), and one Drive video only you can open (the panel warns, the box is unticked).

## Claude connector

`mcp/server.mjs` serves [opn-mcp](https://github.com/ysalitrynskyi/opn-mcp)'s tools over Streamable HTTP at `https://links-mcp.leadershipnowproject.com/mcp`, so staff add it in Claude under Connectors instead of installing anything.

- `delete_link` and `shorten_urls_bulk` are hidden (`HIDDEN_TOOLS`); staff delete in the dashboard.
- `OPN_MCP_TAG` in `.env` pins its image separately from `OPN_TAG`.
- Nothing in front of the hostname signs anyone in (no Cloudflare Access). The host's reverse proxy sends it to 127.0.0.1:3104 and must pass Cloudflare's `CF-Connecting-IP` through; the rate limits count by it.
- **The connector must only be reachable through Cloudflare.** It believes `CF-Connecting-IP` as it arrives, and passes it on to the backend. Here that is safe because the server's security group allows ports 80 and 443 from Cloudflare's address ranges only, so nobody can reach the connector directly and write the header themselves. A deployment without that rule needs its own arrangement (a proxy in front that removes or overwrites the header), or anyone can pick the address they are rate limited as.

### How sign-in works

The server is its own OAuth 2.1 authorization server (`mcp/oauth.mjs`), which is what Claude's connectors expect of a remote MCP server:

1. Claude calls `/mcp` without a token and gets `401` with `WWW-Authenticate: Bearer resource_metadata="…/.well-known/oauth-protected-resource/mcp"`. That document and `/.well-known/oauth-authorization-server` tell Claude that this server signs people in.
2. Claude registers itself at `/register` (a public client, no secret). Only Claude's callback addresses are accepted: `https://claude.ai/api/mcp/auth_callback` and `https://claude.com/api/mcp/auth_callback` (`MCP_ALLOWED_REDIRECT_URIS`). Anything else, loopback addresses included, is refused, so Claude Code's own direct connection (which returns to `http://localhost:<port>`) does not work; connectors added in Claude do.
3. Claude sends the person's browser to `/authorize` (PKCE `S256` required). The server checks the request and shows a page of its own, "Connect Claude to LNP Links?", before anything goes to Google (below, "The confirmation page"). Only "Continue with Google" on that page (`POST /authorize/continue`) sends the person on to Google with the dashboard's Google client, and Google sends them back to `/oauth/google/callback`.
4. The server redeems Google's code and hands the ID token to the backend (`POST /auth/connector/google`, inside the compose network). The backend checks it exactly as the dashboard's sign-in does (Google's signature, this client, verified email, the two domains, a Workspace account) and answers with a session for that person's account, creating the account on first sign-in. Someone it refuses sees a page saying the account is not allowed, and Claude gets no code.
5. The person returns to Claude with a one-time code (60 seconds), which Claude trades at `/token` for an access token (1 hour) and a refresh token.
6. Every hour or so Claude refreshes. Each refresh asks the backend for a new session for that person (`POST /auth/connector/session`), which it gives only while the account is live, enabled, on an allowed domain and has not had its sessions ended. So someone who has left is cut off at the next refresh at the latest, and at once for tools: every `/mcp` call first checks the session with the backend.
7. `MCP_SESSION_HOURS` (default 168, seven days) after the Google sign-in the refresh token stops working and Claude asks the person to connect again. A refresh token works once; using an old one again ends that sign-in (but see "A request sent twice").

Nothing is stored: client ids and tokens are sealed with `MCP_OAUTH_SECRET`, so a restart signs nobody out. A restart does drop sign-ins that are half-way through (the person clicks Connect again) and forgets which refresh tokens were already used.

### The confirmation page

Someone outside LNP can start a connection in their own Claude account, copy the `/authorize` link Claude opens, and send it to a member of staff. Without a page of our own, that person would see only Google's familiar account chooser, and by choosing their account could connect the outsider's Claude to their LNP Links. So `/authorize` never redirects to Google. It shows:

> **Connect Claude to LNP Links?**
>
> Claude is asking to use LNP Links as you: to create and change short links, and to see their statistics.
>
> Continue only if you yourself just clicked Connect for LNP Links in Claude.
>
> If someone sent you this link, close this page.
>
> [Continue with Google] [Cancel]

- The page shows nothing from the request, loads nothing from anywhere else, and cannot be put in a frame (`frame-ancestors 'none'`, `X-Frame-Options: DENY`), so the button cannot be clicked for someone.
- The two buttons are one form that posts to `/authorize/continue`. It carries the checked request sealed in a token (ten minutes, one use) that only works together with the cookie set when the page was shown, so only the browser that saw the page can press the button, and a form on another site cannot (the cookie is `SameSite=Lax`). A `GET` never reaches Google.
- Cancel sends the person back to Claude with `error=access_denied`.
- Nothing but a rate counter is kept in memory for a link that was merely opened.

### A request sent twice

Codes and refresh tokens work once. Using one a second time normally ends the sign-in it belongs to, because a second holder may be a thief. A client that sends the same request twice (a retry, or two at the same moment) used to sign the person out that way. So:

- An authorization code presented again within 10 seconds by the same client with the same, correct PKCE verifier is refused (`invalid_grant`) and nothing else happens. With another or no verifier, from another client, or later, it ends the sign-in as before.
- A refresh token presented again within 10 seconds of being rotated is refused (`invalid_grant`) and nothing else happens. Later, it ends the sign-in as before.

The second request never gets tokens either way. What is given up: a stolen refresh token (or a stolen code together with its PKCE verifier) that is used within 10 seconds of the real client's own use is refused without ending the sign-in, so in that window whoever came first keeps what they got and the theft goes unnoticed. Outside it, nothing has changed.

### Limits

Requests are counted per caller: an IPv4 address, or the /64 of an IPv6 address (as the backend does), from `CF-Connecting-IP`. Callers with no readable address share one counter with a tenth of the budget. Per 10 minutes: `/authorize` 30, its button 30, the Google callback 30, `/token` 300 for requests that carry a code or refresh token of this server's making and 60 for anything else, `/mcp` without a valid token 300 (calls with one are not counted); `/register` 60 an hour.

Nothing in memory grows without bound, and nobody can be locked out by other callers: the table of counters (10,000) and the lists of sign-ins in flight (2,000 each) drop their oldest entry when full. A flood can therefore make someone who is half-way through signing in click Connect again, and no more. Junk sent to `/token`, from any address, never uses up the budget real refreshes have. The lists that security rests on do not forget: a full list of used refresh tokens (20,000) refuses the refresh and leaves its token good.

### Settings (`.env`)

| Setting | Meaning |
|---|---|
| `MCP_OAUTH_SECRET` | Required. Seals client ids and tokens. `openssl rand -base64 48`. The server does not start without it, or with one shorter than 32 bytes or a sample value. |
| `CONNECTOR_SIGNIN_SECRET` | Required. Shared by the connector and the backend only: the connector signs each session renewal with it. `openssl rand -base64 48`; not the same value as `MCP_OAUTH_SECRET` or `JWT_SECRET`. Whoever knows it and can reach the backend directly can get a session as any staff account, so it stays in `.env` and the route stays off the internet (below). |
| `GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET` | The dashboard's Google client; the connector gets the same values. |
| `MCP_PUBLIC_URL` | Optional; default `https://links-mcp.leadershipnowproject.com`. The address the server calls itself in everything it hands Claude. Never taken from a request. |
| `MCP_SESSION_HOURS` | Optional; default 168. How long after the Google sign-in a person must sign in again (1 to 720). |
| `MCP_ALLOWED_REDIRECT_URIS` | Optional; default Claude's two callbacks. Comma-separated exact `https` addresses; no wildcards, no loopback. |

Google Cloud, project `lnp-links`: on the dashboard's OAuth client (Credentials), add the **authorised redirect URI** `https://links-mcp.leadershipnowproject.com/oauth/google/callback`. Nothing else changes there.

`nginx/admin.conf` and `nginx/public.conf` refuse `/api/auth/connector/…`, so the two sign-in routes are reachable only from the connector, over the compose network. CI replays both files against a running nginx (`ci.yml`, "Connector sign-in routes are not reachable from the public hosts").

### Deploying this change

1. In Google Cloud, add the redirect URI above.
2. In `.env`: add `MCP_OAUTH_SECRET` and `CONNECTOR_SIGNIN_SECRET` (two different values), set the new image tag, and delete `CF_ACCESS_TEAM_DOMAIN` and `CF_ACCESS_AUD`.
3. Copy the new `docker-compose.yml` and `nginx/` to the server, then `docker compose pull && docker compose up -d` (this restarts the backend, both frontends and the connector).
4. Check `curl -si -X POST https://links-mcp.leadershipnowproject.com/mcp` answers `401` with the `WWW-Authenticate` header, and `curl -s -o /dev/null -w '%{http_code}' -X POST https://links.leadershipnowproject.com/api/auth/connector/session` answers `403`.
5. In Claude, each person opens Settings, Connectors, LNP Links and connects once more (the old connection was made through Cloudflare Access and no longer works). If Claude does not offer to reconnect, remove the connector and add `https://links-mcp.leadershipnowproject.com/mcp` again.
6. Remove the connector's Access application in Cloudflare Zero Trust if it is still there.

To rotate `MCP_OAUTH_SECRET`: change it in `.env` and `docker compose up -d mcp`. Every client id and token stops working at once and each person connects again, as in step 5. Rotating `CONNECTOR_SIGNIN_SECRET` needs `docker compose up -d backend mcp` and signs nobody out.

If someone cannot connect, `docker compose logs mcp` and the backend log say why (`sign-in refused by backend: 403`, `Connector Google sign-in refused: …`), without the address or any token.

## Updating from upstream

    git fetch upstream --tags
    git rebase <new upstream tag>     # on the lnp branch
    # resolve, push, let CI pass, tag lnp-<version>-1

## Licence

AGPL-3.0, as upstream. This public fork is the source offer for the modified service.
