import type { Metadata } from "next";
import type { ReactNode } from "react";

// Placeholder root layout — the Foundation agent creates only the minimum
// needed for `next build` to succeed. Plan 08's Structure agents replace
// this with the real console shell (per Atomic Design: this file stays a
// thin "page" concern, delegating rendering to organisms/modules).
export const metadata: Metadata = {
  title: "Moira Console",
  description: "Moira admin console workspace scaffold.",
};

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
