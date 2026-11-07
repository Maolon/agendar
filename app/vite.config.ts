import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri's dev server contract: fixed port, no remote host.
export default defineConfig(({ command }) => {
  // `pnpm dev` defaults to the fixture transport so it runs in a plain browser.
  // The client still refuses fixtures when the Tauri runtime is present, so the
  // native shell always uses real state. Set VITE_AGENDAR_FIXTURES=0 to disable
  // the fixture path entirely. Production builds never receive this env var, so
  // `import.meta.env.VITE_AGENDAR_FIXTURES === "1"` is statically false and the
  // fixture module is tree-shaken out of every release bundle.
  if (command === "serve" && process.env.VITE_AGENDAR_FIXTURES === undefined) {
    process.env.VITE_AGENDAR_FIXTURES = "1";
  }
  return {
    plugins: [react()],
    clearScreen: false,
    server: {
      port: 1420,
      strictPort: true,
      host: "localhost",
      hmr: { port: 1420 },
    },
    build: {
      target: "es2022",
      sourcemap: false,
    },
  };
});
