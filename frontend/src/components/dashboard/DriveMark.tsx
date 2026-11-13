// Drawn here, not loaded: showing the mark asks Google for nothing.
export default function DriveMark({ size = 22, label }: { size?: number; label?: string }) {
    return (
        <svg
            viewBox="0 0 24 22"
            width={size}
            height={Math.round(size * 22 / 24)}
            className="shrink-0"
            {...(label ? { role: 'img', 'aria-label': label } : { 'aria-hidden': true })}
        >
            <path d="M8 1h8l7.5 13h-8z" fill="#FBBC04" />
            <path d="M8 1 .5 14l4 7L12 8z" fill="#34A853" />
            <path d="M8 14h15.5l-4 7h-15z" fill="#4285F4" />
        </svg>
    );
}
