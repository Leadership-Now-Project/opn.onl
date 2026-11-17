import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

// The checks in designTokens.test.ts, for the Leadership Now Project theme
// (themes/lnp.js), whose tokens replace the default ones when VITE_THEME=lnp.
// Tokens the theme does not set (success, danger, warning) are the defaults,
// already checked there.

type Rgb = [number, number, number];

function oklchToRgb(l: number, c: number, hDeg: number): Rgb {
    const h = (hDeg * Math.PI) / 180;
    const a = c * Math.cos(h);
    const b = c * Math.sin(h);
    const l_ = (l + 0.3963377774 * a + 0.2158037573 * b) ** 3;
    const m_ = (l - 0.1055613458 * a - 0.0638541728 * b) ** 3;
    const s_ = (l - 0.0894841775 * a - 1.291485548 * b) ** 3;
    const linear = [
        4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_,
        -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_,
        -0.0041960863 * l_ - 0.7034186147 * m_ + 1.707614701 * s_,
    ];
    return linear.map((v) => {
        const x = Math.min(1, Math.max(0, v));
        const encoded = x <= 0.0031308 ? 12.92 * x : 1.055 * x ** (1 / 2.4) - 0.055;
        return Math.round(encoded * 255);
    }) as Rgb;
}

function luminance([r, g, b]: Rgb): number {
    const [lr, lg, lb] = [r, g, b].map((v) => {
        const s = v / 255;
        return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
    });
    return 0.2126 * lr + 0.7152 * lg + 0.0722 * lb;
}

function contrast(fg: Rgb, bg: Rgb): number {
    const [hi, lo] = [luminance(fg), luminance(bg)].sort((x, y) => y - x);
    return (hi + 0.05) / (lo + 0.05);
}

const theme = readFileSync(resolve(process.cwd(), 'themes/lnp.js'), 'utf8');

function token(name: string): Rgb {
    const line = theme.split('\n').find((l) => new RegExp(`^\\s*'?${name}'?:\\s*'oklch`).test(l));
    if (!line) throw new Error(`token ${name} not found in themes/lnp.js`);
    const m = line.match(/oklch\(\s*([\d.]+)\s+([\d.]+)\s+([\d.]+)/);
    if (!m) throw new Error(`no oklch() colour for ${name}`);
    return oklchToRgb(Number(m[1]), Number(m[2]), Number(m[3]));
}

const WHITE: Rgb = [255, 255, 255];
const AA = 4.5;

describe('LNP theme token contrast (WCAG AA, 4.5:1 for body text)', () => {
    const grounds = [WHITE, token('surface'), token('paper'), token('50')];

    it.each(['ink', 'muted', 'faint'])('%s text is readable on every neutral ground', (name) => {
        for (const bg of grounds) {
            expect(contrast(token(name), bg)).toBeGreaterThanOrEqual(AA);
        }
    });

    it.each(['600', '700'])('primary-%s reads as link text on every neutral ground', (shade) => {
        for (const bg of grounds) {
            expect(contrast(token(shade), bg)).toBeGreaterThanOrEqual(AA);
        }
    });

    it.each(['600', '700'])('white text is readable on a primary-%s button', (shade) => {
        expect(contrast(WHITE, token(shade))).toBeGreaterThanOrEqual(AA);
    });

    it('primary-600 is LNP navy #1E3A65', () => {
        const [r, g, b] = token('600');
        expect(Math.abs(r - 0x1e)).toBeLessThanOrEqual(2);
        expect(Math.abs(g - 0x3a)).toBeLessThanOrEqual(2);
        expect(Math.abs(b - 0x65)).toBeLessThanOrEqual(2);
    });
});
