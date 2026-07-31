import type { NextConfig } from "next";

// Minimal config for the workspace scaffold. Feature-specific settings
// (headers, redirects, auth rewrites, etc.) are added by plan 08.
const nextConfig: NextConfig = {
  // Produces `.next/standalone` — a minimal, self-contained server bundle
  // copied into the Docker runtime stage instead of the full node_modules tree.
  output: "standalone",
  reactStrictMode: true,

  // `pg` selects its own connection implementation at require time and will
  // pick up the optional native binding `pg-native` if it is present. Bundling
  // it resolves that choice at build time and drops the driver's own
  // `connection-string` parsing edge cases; keeping it external means the
  // console runs the same driver code a plain `node` process would. `output:
  // "standalone"` still traces it into the runtime image.
  serverExternalPackages: ["pg"],
};

export default nextConfig;
