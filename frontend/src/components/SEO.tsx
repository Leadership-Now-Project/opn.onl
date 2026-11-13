import { Helmet } from 'react-helmet-async';
import { version as APP_VERSION } from '../../package.json';
import { SEO_TAG_ATTRIBUTE } from '../utils/prerenderedHead';
import { normalizeSiteUrl } from '../utils/siteUrl';
import { IS_LNP, SITE_NAME } from '../config/theme';

interface SEOProps {
  title?: string;
  description?: string;
  keywords?: string;
  image?: string;
  url?: string;
  type?: 'website' | 'article';
  /** App and link pages: `noindex`, and no canonical unless `url` is given. */
  noIndex?: boolean;
  schemaType?: 'WebSite' | 'WebApplication' | 'SoftwareApplication' | 'Organization' | 'FAQPage';
  faqItems?: { question: string; answer: string }[];
  breadcrumbs?: { name: string; url: string }[];
}

const BASE_URL = normalizeSiteUrl(import.meta.env.VITE_FRONTEND_URL);
const DEFAULT_IMAGE = `${BASE_URL}/og-image.png`;
const GITHUB_URL = 'https://github.com/ysalitrynskyi/opn.onl';

// What a share preview (a chat app's card, a social post) says in the LNP
// build. Previewers do not run the page's code: they read the tags of the
// page as served, which for a short link's pages is the app's shell, so these
// defaults are what most LNP previews show. The image is the theme's own
// og-image.png (themes/lnp/public).
const DEFAULT_DESCRIPTION = 'Create short, memorable links with branded QR codes, smart device & geo routing, one-time links and first-party analytics. Self-hostable, privacy-focused URL shortener built with Rust and React.';
const LNP_DESCRIPTION = 'Short links from the Leadership Now Project.';
const BRAND_NAME = IS_LNP ? 'Leadership Now Project' : 'opn.onl';

// Marks every tag below so main.tsx can drop the copies a prerendered page was
// shipped with (see utils/prerenderedHead.ts). Any head tag a prerendered page
// needs belongs here, not in index.html: a static copy stays next to this one.
const HEAD_TAG = { [SEO_TAG_ATTRIBUTE]: '' };

export default function SEO({
  title = 'opn.onl - Open Source URL Shortener',
  description: givenDescription = DEFAULT_DESCRIPTION,
  keywords = 'url shortener, link shortener, short links, branded qr codes, qr code generator, link in bio, smart routing, conditional routing, one-time links, analytics, open source, privacy, rust, react',
  image = DEFAULT_IMAGE,
  url,
  type = 'website',
  noIndex = false,
  schemaType = 'WebApplication',
  faqItems,
  breadcrumbs,
}: SEOProps) {
  // In the LNP build no page describes itself as opn.onl: the default text is
  // LNP's own, and a page's own text names this site.
  const description = !IS_LNP
    ? givenDescription
    : givenDescription === DEFAULT_DESCRIPTION
      ? LNP_DESCRIPTION
      : givenDescription.replaceAll('opn.onl', SITE_NAME);

  const fullTitle = title === 'opn.onl - Open Source URL Shortener'
    ? (IS_LNP ? SITE_NAME : title)
    : `${title} | ${SITE_NAME}`;

  // Resolve the canonical URL to an absolute one. Pages may pass a route-relative
  // path (e.g. "/features") or an already-absolute URL; a relative path is joined
  // onto BASE_URL so every indexable page canonicalizes to its own route rather
  // than defaulting to the site root. It must match the page's sitemap.xml entry
  // exactly, so the home page gets the trailing slash. A noindex page without a
  // `url` gets no canonical: pointing it at the home page contradicted the noindex.
  const canonicalUrl = url
    ? /^https?:\/\//.test(url)
      ? url
      : `${BASE_URL}${url.startsWith('/') ? url : `/${url}`}`
    : noIndex
      ? null
      : `${BASE_URL}/`;

  const faqEntities = faqItems?.map(item => ({
    '@type': 'Question',
    name: item.question,
    acceptedAnswer: {
      '@type': 'Answer',
      text: item.answer,
    },
  }));

  // Main schema
  const mainSchema = {
    '@context': 'https://schema.org',
    '@type': schemaType,
    name: 'opn.onl',
    description,
    url: canonicalUrl ?? `${BASE_URL}/`,
    // On the FAQ page the main schema is the FAQPage itself, so the questions
    // go here rather than into a second FAQPage block.
    ...(schemaType === 'FAQPage' && faqEntities && { mainEntity: faqEntities }),
    ...(schemaType === 'WebApplication' && {
      applicationCategory: 'UtilityApplication',
      operatingSystem: 'Web',
      browserRequirements: 'Requires JavaScript',
      softwareVersion: APP_VERSION,
      author: {
        '@type': 'Person',
        name: 'Yevhen Salitrynskyi',
        url: 'https://github.com/ysalitrynskyi',
      },
      offers: {
        '@type': 'Offer',
        price: '0',
        priceCurrency: 'USD',
        availability: 'https://schema.org/InStock',
      },
      featureList: [
        'URL shortening with custom aliases',
        'Detailed click analytics',
        'Geographic visitor tracking',
        'Branded QR codes (SVG export)',
        'One-time burn-after-reading links',
        'Optional safe-link interstitial',
        'Smart conditional routing (device / geo / language)',
        'Opt-in link-in-bio profile page',
        'Password protection',
        'Link expiration',
        'Team collaboration',
        'API access',
        'Bulk link creation',
        'CSV export',
      ],
      screenshot: `${BASE_URL}/og-image.png`,
    }),
    ...(schemaType === 'WebSite' && {
      potentialAction: {
        '@type': 'SearchAction',
        target: {
          '@type': 'EntryPoint',
          urlTemplate: `${BASE_URL}/dashboard?search={search_term_string}`,
        },
        'query-input': 'required name=search_term_string',
      },
    }),
  };

  // Organization schema (always included)
  const orgSchema = {
    '@context': 'https://schema.org',
    '@type': 'Organization',
    name: 'opn.onl',
    url: BASE_URL,
    logo: `${BASE_URL}/favicon.png`,
    description: 'Open source URL shortener with analytics',
    sameAs: [
      GITHUB_URL,
    ],
    founder: {
      '@type': 'Person',
      name: 'Yevhen Salitrynskyi',
    },
    foundingDate: '2024',
  };

  // FAQ schema (if faqItems provided and the main schema is not already the FAQPage)
  const faqSchema = faqEntities && schemaType !== 'FAQPage' ? {
    '@context': 'https://schema.org',
    '@type': 'FAQPage',
    mainEntity: faqEntities,
  } : null;

  // Breadcrumb schema (if breadcrumbs provided)
  const breadcrumbSchema = breadcrumbs ? {
    '@context': 'https://schema.org',
    '@type': 'BreadcrumbList',
    itemListElement: breadcrumbs.map((crumb, index) => ({
      '@type': 'ListItem',
      position: index + 1,
      name: crumb.name,
      item: crumb.url,
    })),
  } : null;

  // SoftwareSourceCode schema for open source
  const sourceCodeSchema = {
    '@context': 'https://schema.org',
    '@type': 'SoftwareSourceCode',
    name: 'opn.onl',
    codeRepository: GITHUB_URL,
    programmingLanguage: ['Rust', 'TypeScript', 'React'],
    license: 'https://www.gnu.org/licenses/agpl-3.0.en.html',
    runtimePlatform: 'Docker',
  };

  return (
    <Helmet>
      {/* Basic Meta Tags */}
      <title {...HEAD_TAG}>{fullTitle}</title>
      <meta {...HEAD_TAG} name="description" content={description} />
      <meta {...HEAD_TAG} name="keywords" content={keywords} />
      {!IS_LNP && <meta {...HEAD_TAG} name="author" content="Yevhen Salitrynskyi" />}
      <meta {...HEAD_TAG} name="generator" content="opn.onl" />
      {noIndex && <meta {...HEAD_TAG} name="robots" content="noindex, nofollow" />}
      
      {/* Open Graph / Facebook */}
      <meta {...HEAD_TAG} property="og:type" content={type} />
      {canonicalUrl && <meta {...HEAD_TAG} property="og:url" content={canonicalUrl} />}
      <meta {...HEAD_TAG} property="og:title" content={fullTitle} />
      <meta {...HEAD_TAG} property="og:description" content={description} />
      <meta {...HEAD_TAG} property="og:image" content={image} />
      <meta {...HEAD_TAG} property="og:image:width" content="1200" />
      <meta {...HEAD_TAG} property="og:image:height" content="630" />
      <meta {...HEAD_TAG} property="og:site_name" content={BRAND_NAME} />
      <meta {...HEAD_TAG} property="og:locale" content="en_US" />
      
      {/* Twitter */}
      <meta {...HEAD_TAG} name="twitter:card" content="summary_large_image" />
      {canonicalUrl && <meta {...HEAD_TAG} name="twitter:url" content={canonicalUrl} />}
      <meta {...HEAD_TAG} name="twitter:title" content={fullTitle} />
      <meta {...HEAD_TAG} name="twitter:description" content={description} />
      <meta {...HEAD_TAG} name="twitter:image" content={image} />
      {!IS_LNP && <meta {...HEAD_TAG} name="twitter:creator" content="@ysalitrynskyi" />}
      
      {/* Additional SEO (theme-color is site-wide and lives in index.html) */}
      <meta {...HEAD_TAG} name="application-name" content={IS_LNP ? SITE_NAME : 'opn.onl'} />
      <meta {...HEAD_TAG} name="apple-mobile-web-app-title" content={IS_LNP ? SITE_NAME : 'opn.onl'} />
      <meta {...HEAD_TAG} name="apple-mobile-web-app-capable" content="yes" />
      <meta {...HEAD_TAG} name="mobile-web-app-capable" content="yes" />
      
      {/* Canonical URL */}
      {canonicalUrl && <link {...HEAD_TAG} rel="canonical" href={canonicalUrl} />}
      
      {/* DNS Prefetch for external resources */}
      <link {...HEAD_TAG} rel="dns-prefetch" href="//www.google-analytics.com" />
      
      {/* Schema.org JSON-LD - Multiple schemas */}
      {/* These three describe the opn.onl software and its author, not an
          LNP deployment, so the LNP build leaves them out. */}
      {!IS_LNP && (
        <script type="application/ld+json">
          {JSON.stringify(mainSchema)}
        </script>
      )}
      {!IS_LNP && (
        <script type="application/ld+json">
          {JSON.stringify(orgSchema)}
        </script>
      )}
      {!IS_LNP && (
        <script type="application/ld+json">
          {JSON.stringify(sourceCodeSchema)}
        </script>
      )}
      {faqSchema && (
        <script type="application/ld+json">
          {JSON.stringify(faqSchema)}
        </script>
      )}
      {breadcrumbSchema && (
        <script type="application/ld+json">
          {JSON.stringify(breadcrumbSchema)}
        </script>
      )}
    </Helmet>
  );
}

