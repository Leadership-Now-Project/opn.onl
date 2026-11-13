// Build-time theme switches (VITE_THEME). The default build is opn.onl as
// upstream ships it; VITE_THEME=lnp is the Leadership Now Project dashboard
// (colours and fonts in themes/lnp.js).

export const IS_LNP = import.meta.env.VITE_THEME === 'lnp';

/**
 * A build that serves only what a visitor of a short link sees (the password
 * page, the interstitial, the Secure View viewer), on a host where nobody
 * signs in and the dashboard lives elsewhere. The header is then the logo and
 * nothing more: no sign-in or sign-up links (they would lead nowhere), no
 * account menu, and no request for the sign-in settings.
 */
export const PUBLIC_LINKS_ONLY = import.meta.env.VITE_PUBLIC_LINKS_ONLY === 'true';

/** Name shown after the page name in the browser tab. */
export const SITE_NAME = IS_LNP ? 'LNP Links' : 'opn.onl';

/** Where this deployment's source is offered, as AGPL-3.0 section 13 asks. */
export const SOURCE_URL = IS_LNP
    ? 'https://github.com/Leadership-Now-Project/opn.onl'
    : 'https://github.com/ysalitrynskyi/opn.onl';
