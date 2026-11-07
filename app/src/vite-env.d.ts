/// <reference types="vite/client" />

interface ImportMetaEnv {
  /**
   * Development only: "1" (the `pnpm dev` default) serves the fixture
   * transport in a plain browser; "0" uses the native shell. Not set in builds.
   */
  readonly VITE_AGENDAR_FIXTURES?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
