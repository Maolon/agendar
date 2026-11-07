/**
 * The Agendar mark: the step strip — two finished steps, the amber one that
 * needs you and a planned one, as in the project view. Sources:
 * app/src-tauri/icons/source.
 */

/** The full app icon, for the setup screen. */
export function AppLogo({ size = 64 }: { size?: number }) {
  return (
    <svg className="app-logo" width={size} height={size} viewBox="100 100 824 824" role="img" aria-label="Agendar">
      <defs>
        <linearGradient id="agendar-logo-bg" x1="0" y1="100" x2="0" y2="924" gradientUnits="userSpaceOnUse">
          <stop offset="0" stopColor="#2F2F2D" />
          <stop offset="1" stopColor="#1A1A19" />
        </linearGradient>
      </defs>
      <rect x="100" y="100" width="824" height="824" rx="186" fill="url(#agendar-logo-bg)" />
      <rect x="104" y="104" width="816" height="816" rx="182" fill="none" stroke="#FFF" strokeOpacity="0.1" strokeWidth="8" />
      <rect x="220" y="448" width="128" height="128" rx="28" fill="#A1A19C" />
      <rect x="372" y="448" width="128" height="128" rx="28" fill="#A1A19C" />
      <rect x="524" y="448" width="128" height="128" rx="28" fill="#E8890C" />
      <rect x="687" y="459" width="106" height="106" rx="19" fill="none" stroke="#6A6A66" strokeWidth="22" />
    </svg>
  );
}
