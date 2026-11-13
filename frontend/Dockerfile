# Shared production nginx configuration. CI targets this stage directly for a
# cheap smoke test without running the Chromium-based frontend build.
FROM nginx:alpine AS csp-config

COPY nginx.conf /etc/nginx/conf.d/default.conf
# Per-deployment locations are mounted here; empty in the stock image.
RUN mkdir -p /etc/nginx/site.d

# Bake the API origin into the CSP connect-src. The frontend fetches the API
# cross-origin (VITE_API_URL, e.g. https://l.opn.onl), so the document CSP must
# allow that origin - otherwise connect-src 'self' blocks every API call and the
# app loads no data. Derived from the same build arg the JS bundle uses, so the
# CSP and the baked-in API URL cannot drift. Also allow the WebSocket origin.
#
# RUN is `sh -c` without -e, and a bare `! grep` is exempt from `set -e`, so the
# guards are explicit `if ...; exit 1` and the build fails if the placeholder is
# missing before the substitution (nothing would be replaced and the CSP would
# silently lack the API origin) or still present after it.
ARG VITE_API_URL
RUN set -e; \
    CONF=/etc/nginx/conf.d/default.conf; \
    if ! grep -q '__API_CONNECT_SRC__' "$CONF"; then \
        echo "ERROR: __API_CONNECT_SRC__ placeholder not found in nginx.conf; the CSP would ship without the API origin"; \
        exit 1; \
    fi; \
    if [ -n "$VITE_API_URL" ]; then \
        WS_ORIGIN=$(printf '%s' "$VITE_API_URL" | sed -e 's#^https://#wss://#' -e 's#^http://#ws://#'); \
        CONNECT_SRC="$VITE_API_URL $WS_ORIGIN"; \
    else \
        CONNECT_SRC=""; \
    fi; \
    sed -i "s#__API_CONNECT_SRC__#${CONNECT_SRC}#g" "$CONF"; \
    if grep -q '__API_CONNECT_SRC__' "$CONF"; then \
        echo "ERROR: __API_CONNECT_SRC__ placeholder still present after substitution"; \
        exit 1; \
    fi; \
    echo "CSP connect-src extra sources: '${CONNECT_SRC}'"

# Google sign-in on the login page (src/pages/Login.tsx) needs Google Identity
# Services in the CSP: its script, the endpoints that script calls, its frames
# and its stylesheet. Only a deployment that signs people in with Google sets
# CSP_GOOGLE_SIGN_IN=true; without it every placeholder becomes nothing and the
# policy is exactly the stock one. Each placeholder sits directly before its
# `;` in nginx.conf, so the leading space belongs to the value. Same guards as
# above: fail if a placeholder is missing before or left over after.
ARG CSP_GOOGLE_SIGN_IN
RUN set -e; \
    CONF=/etc/nginx/conf.d/default.conf; \
    for NAME in SCRIPT STYLE CONNECT FRAME; do \
        if ! grep -q "__CSP_GOOGLE_${NAME}__" "$CONF"; then \
            echo "ERROR: __CSP_GOOGLE_${NAME}__ placeholder not found in nginx.conf"; \
            exit 1; \
        fi; \
    done; \
    if [ "$CSP_GOOGLE_SIGN_IN" = "true" ]; then \
        G_SCRIPT=" https://accounts.google.com/gsi/client"; \
        G_STYLE=" https://accounts.google.com/gsi/style"; \
        G_CONNECT=" https://accounts.google.com/gsi/"; \
        G_FRAME=" frame-src https://accounts.google.com/gsi/__CSP_DRIVE_FRAME__;"; \
    elif [ -z "$CSP_GOOGLE_SIGN_IN" ] || [ "$CSP_GOOGLE_SIGN_IN" = "false" ]; then \
        G_SCRIPT=""; G_STYLE=""; G_CONNECT=""; G_FRAME=""; \
    else \
        echo "ERROR: CSP_GOOGLE_SIGN_IN must be true, false or unset, not '$CSP_GOOGLE_SIGN_IN'"; \
        exit 1; \
    fi; \
    sed -i \
        -e "s#__CSP_GOOGLE_SCRIPT__#${G_SCRIPT}#g" \
        -e "s#__CSP_GOOGLE_STYLE__#${G_STYLE}#g" \
        -e "s#__CSP_GOOGLE_CONNECT__#${G_CONNECT}#g" \
        -e "s#__CSP_GOOGLE_FRAME__#${G_FRAME}#g" \
        "$CONF"; \
    if grep -q '__CSP_GOOGLE_' "$CONF"; then \
        echo "ERROR: a __CSP_GOOGLE_ placeholder is still present after substitution"; \
        exit 1; \
    fi; \
    echo "CSP Google sign-in sources: '${G_SCRIPT}'"

# File sharing (the dashboard's Files tab) uploads straight from the browser to
# the storage bucket, so the page's connect-src must name the bucket's origin,
# e.g. https://my-bucket.s3.us-east-2.amazonaws.com. Only a dashboard image for
# a deployment with file sharing sets CSP_FILES_UPLOAD_ORIGIN; without it the
# placeholder becomes nothing and the policy is exactly the stock one. The
# value is written into a header, so anything but a bare https origin is
# refused: a space or a semicolon in it would add sources or whole directives.
ARG CSP_FILES_UPLOAD_ORIGIN
RUN set -e; \
    CONF=/etc/nginx/conf.d/default.conf; \
    if ! grep -q '__CSP_FILES_CONNECT__' "$CONF"; then \
        echo "ERROR: __CSP_FILES_CONNECT__ placeholder not found in nginx.conf"; \
        exit 1; \
    fi; \
    if [ -z "$CSP_FILES_UPLOAD_ORIGIN" ]; then \
        F_CONNECT=""; \
    elif printf '%s' "$CSP_FILES_UPLOAD_ORIGIN" | grep -Eq '^https://[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?(:[0-9]{1,5})?$'; then \
        F_CONNECT=" $CSP_FILES_UPLOAD_ORIGIN"; \
    else \
        echo "ERROR: CSP_FILES_UPLOAD_ORIGIN must be an https origin such as https://bucket.s3.us-east-2.amazonaws.com, not '$CSP_FILES_UPLOAD_ORIGIN'"; \
        exit 1; \
    fi; \
    sed -i "s#__CSP_FILES_CONNECT__#${F_CONNECT}#g" "$CONF"; \
    if grep -q '__CSP_FILES_' "$CONF"; then \
        echo "ERROR: a __CSP_FILES_ placeholder is still present after substitution"; \
        exit 1; \
    fi; \
    echo "CSP file upload origin: '${F_CONNECT}'"

# "Google Drive Files" on the dashboard's Files tab (src/utils/googleDrive.ts)
# opens Google's file picker and asks the Drive API about the file picked:
# the API loader script and the picker module it fetches (apis.google.com),
# the picker's frame (docs.google.com/picker) and the Drive API itself
# (www.googleapis.com). Only a dashboard image for a deployment with the picker
# sets CSP_GOOGLE_DRIVE_PICKER=true; without it every placeholder becomes
# nothing and the policy is exactly what it was. The picker is used with the
# Google sign-in above, whose frame-src directive it adds its frame to, so
# turning it on without CSP_GOOGLE_SIGN_IN=true does not build.
ARG CSP_GOOGLE_DRIVE_PICKER
RUN set -e; \
    CONF=/etc/nginx/conf.d/default.conf; \
    for NAME in SCRIPT CONNECT; do \
        if ! grep -q "__CSP_DRIVE_${NAME}__" "$CONF"; then \
            echo "ERROR: __CSP_DRIVE_${NAME}__ placeholder not found in nginx.conf"; \
            exit 1; \
        fi; \
    done; \
    if [ "$CSP_GOOGLE_DRIVE_PICKER" = "true" ]; then \
        if ! grep -q '__CSP_DRIVE_FRAME__' "$CONF"; then \
            echo "ERROR: CSP_GOOGLE_DRIVE_PICKER=true needs CSP_GOOGLE_SIGN_IN=true"; \
            exit 1; \
        fi; \
        D_SCRIPT=" https://apis.google.com"; \
        D_CONNECT=" https://www.googleapis.com"; \
        D_FRAME=" https://docs.google.com"; \
    elif [ -z "$CSP_GOOGLE_DRIVE_PICKER" ] || [ "$CSP_GOOGLE_DRIVE_PICKER" = "false" ]; then \
        D_SCRIPT=""; D_CONNECT=""; D_FRAME=""; \
    else \
        echo "ERROR: CSP_GOOGLE_DRIVE_PICKER must be true, false or unset, not '$CSP_GOOGLE_DRIVE_PICKER'"; \
        exit 1; \
    fi; \
    sed -i \
        -e "s#__CSP_DRIVE_SCRIPT__#${D_SCRIPT}#g" \
        -e "s#__CSP_DRIVE_CONNECT__#${D_CONNECT}#g" \
        -e "s#__CSP_DRIVE_FRAME__#${D_FRAME}#g" \
        "$CONF"; \
    if grep -q '__CSP_DRIVE_' "$CONF"; then \
        echo "ERROR: a __CSP_DRIVE_ placeholder is still present after substitution"; \
        exit 1; \
    fi; \
    echo "CSP Google Drive picker sources: '${D_SCRIPT}'"

# Build stage - use full Node image for Puppeteer/Chromium support
FROM node:22-slim AS builder

# Don't let puppeteer download its own browsers (Chrome for Testing has no
# linux/arm64 builds); we use Playwright's Chromium below instead. Puppeteer
# 22+ reads PUPPETEER_SKIP_DOWNLOAD and ignores PUPPETEER_SKIP_CHROMIUM_DOWNLOAD.
ENV PUPPETEER_SKIP_DOWNLOAD=true

WORKDIR /app

# Copy package files
COPY package*.json ./

# Install dependencies
RUN npm ci --legacy-peer-deps

# Chromium for the prerender step. Debian's `chromium` package (150+) crashes
# with SIGTRAP under Docker on both amd64 and arm64, which surfaced as
# puppeteer "Failed to launch the browser process" with an empty stderr.
# Playwright's Chromium is version-pinned through package-lock.json, ships
# proper linux/amd64 AND linux/arm64 builds, and --with-deps installs the
# required system libraries.
RUN npx playwright install chromium --with-deps \
    && rm -rf /var/lib/apt/lists/*

# Copy source code
COPY . .

# Set build-time environment variables (API URLs are baked in at build time)
ARG VITE_API_URL
ARG VITE_FRONTEND_URL
# "lnp" for the Leadership Now Project theme (themes/lnp.js, themes/lnp/public).
ARG VITE_THEME
# "true" for a host that serves only what a short link's visitor sees (the
# password page, the interstitial, the Secure View viewer): the header is then
# the logo alone, with no sign-in links and no account menu.
ARG VITE_PUBLIC_LINKS_ONLY

ENV VITE_API_URL=$VITE_API_URL
ENV VITE_FRONTEND_URL=$VITE_FRONTEND_URL
ENV VITE_THEME=$VITE_THEME
ENV VITE_PUBLIC_LINKS_ONLY=$VITE_PUBLIC_LINKS_ONLY
# A theme's own logo and icons replace the stock ones in public/.
RUN if [ -n "$VITE_THEME" ] && [ -d "themes/$VITE_THEME/public" ]; then cp -r "themes/$VITE_THEME/public/." public/; fi

# Build the application with prerendering. The Playwright Chromium path
# contains a version segment, so resolve it at build time; puppeteer picks it
# up via PUPPETEER_EXECUTABLE_PATH.
RUN PUPPETEER_EXECUTABLE_PATH="$(find /root/.cache/ms-playwright -name chrome -type f | head -1)" \
    npm run build

# Production stage with the already-validated nginx configuration
FROM csp-config

# Install curl for health checks
RUN apk add --no-cache curl

# Copy docker entrypoint script
COPY docker-entrypoint.sh /docker-entrypoint.sh
RUN chmod +x /docker-entrypoint.sh

# Copy built assets from builder
COPY --from=builder /app/dist /usr/share/nginx/html

# Defense-in-depth guard (the build itself already runs scripts/verify-prerender.mjs).
# If dist/ ever ships without a real index.html, nginx silently serves the base
# image's stock "Welcome to nginx!" page at "/" — a green build with a broken
# home page, exactly what took production down once. Fail the image build loudly
# instead: the SPA mount point must be present in the served index.html.
RUN grep -q 'id="root"' /usr/share/nginx/html/index.html \
    || { echo "ERROR: /usr/share/nginx/html/index.html is not the SPA shell (prerender dropped the root route)"; exit 1; }

# Create non-root user for nginx
RUN chown -R nginx:nginx /usr/share/nginx/html && \
    chown -R nginx:nginx /var/cache/nginx && \
    chown -R nginx:nginx /var/log/nginx && \
    touch /var/run/nginx.pid && \
    chown -R nginx:nginx /var/run/nginx.pid

# Expose port
EXPOSE 80

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=15s --retries=3 \
  CMD curl -fsS http://localhost:80/health || exit 1

# Use entrypoint for runtime env injection
ENTRYPOINT ["/docker-entrypoint.sh"]
CMD ["nginx", "-g", "daemon off;"]
