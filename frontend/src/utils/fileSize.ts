const UNITS = ['KB', 'MB', 'GB', 'TB'];

/**
 * A byte count as people say it: "0 bytes", "1 byte", "830 bytes", "12.4 MB",
 * "500 MB". Binary units (1 MB = 1,048,576 bytes), which is what the size
 * limit is set in, with one decimal below 100 and none from there up.
 */
export function formatBytes(bytes: number): string {
    if (!Number.isFinite(bytes) || bytes < 0) return '';
    if (bytes < 1024) return `${bytes} ${bytes === 1 ? 'byte' : 'bytes'}`;
    let value = bytes / 1024;
    let unit = 0;
    while (value >= 1024 && unit < UNITS.length - 1) {
        value /= 1024;
        unit += 1;
    }
    // Rounded first, so 99.96 reads "100", not "100.0".
    const rounded = Math.round(value * 10) / 10;
    const text = rounded >= 100 ? String(Math.round(value)) : String(rounded);
    return `${text} ${UNITS[unit]}`;
}
