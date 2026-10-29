// Leadership Now Project theme, chosen at build time with VITE_THEME=lnp
// (tailwind.config.js merges it over the default theme). Taken from
// leadershipnowproject.org, 29 Oct 2026: navy #1E3A65, cool grey #ECEFF4,
// near-black #1F1F1F, Plus Jakarta Sans throughout, square corners. LNP gold
// (#FFB638) arrives through the logo; it can't carry the white text the
// dashboard puts on its main colour.
//
// One token per line, in the same form as tailwind.config.js:
// src/designTokens.lnp.test.ts checks these for WCAG AA contrast.

export default {
  colors: {
    // primary-600 is the main colour (buttons, links): LNP navy #1E3A65.
    primary: {
      50:  'oklch(0.972 0.010 259 / <alpha-value>)',
      100: 'oklch(0.940 0.020 259 / <alpha-value>)',
      200: 'oklch(0.885 0.035 259 / <alpha-value>)',
      300: 'oklch(0.800 0.055 259 / <alpha-value>)',
      400: 'oklch(0.650 0.085 259 / <alpha-value>)',
      500: 'oklch(0.470 0.095 259 / <alpha-value>)',
      600: 'oklch(0.350 0.083 259 / <alpha-value>)',
      700: 'oklch(0.295 0.070 259 / <alpha-value>)',
      800: 'oklch(0.250 0.058 259 / <alpha-value>)',
      900: 'oklch(0.215 0.048 259 / <alpha-value>)',
      950: 'oklch(0.170 0.036 259 / <alpha-value>)',
    },
    accent: {
      DEFAULT: 'oklch(0.350 0.083 259 / <alpha-value>)',
      soft: 'oklch(0.940 0.020 259 / <alpha-value>)',
    },
    ink:   'oklch(0.239 0.000 0 / <alpha-value>)', // #1F1F1F
    muted: 'oklch(0.450 0.000 0 / <alpha-value>)',
    faint: 'oklch(0.500 0.000 0 / <alpha-value>)',
    line:  'oklch(0.905 0.007 261 / <alpha-value>)',
    line2: 'oklch(0.845 0.009 261 / <alpha-value>)',
    paper: 'oklch(0.951 0.007 261 / <alpha-value>)', // #ECEFF4
    surface: 'oklch(1.000 0.000 0 / <alpha-value>)',
  },
  fontFamily: {
    display: ['"Plus Jakarta Sans Variable"', '"Plus Jakarta Sans"', 'system-ui', 'sans-serif'],
    sans: ['"Plus Jakarta Sans Variable"', '"Plus Jakarta Sans"', 'system-ui', 'sans-serif'],
  },
  // The website uses square corners; keep a hint of rounding on large panels
  // and leave `full` (pills, avatars) as it is.
  borderRadius: {
    sm: '0px',
    DEFAULT: '2px',
    md: '2px',
    lg: '3px',
    xl: '4px',
    '2xl': '4px',
    '3xl': '6px',
    '4xl': '8px',
  },
};
