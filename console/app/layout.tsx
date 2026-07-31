import type { Metadata } from "next";
import type { ReactNode } from "react";

import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";

// THE ROOT LAYOUT, AND IT STAYS HERE.
//
// It is deliberately NOT moved into the `(console)` route group. `app/api/**`
// sits outside every group, and Next requires a root layout for the whole
// segment tree — a tree whose only layout lives inside a group leaves those
// segments with no `<html>`/`<body>` ancestor. `(console)/layout.tsx` is the
// authenticated chrome and nests inside this one; `/login` is a sibling of the
// group and therefore renders with this layout alone. That is the point: the
// sign-in page must not render inside an auth-gated layout, or every redirect
// to it loops.
//
// `generateMetadata()` rather than a static `metadata` object so the title and
// description resolve through the catalog like every other string. A static
// object would be the one place in the console where English is inlined.
export function generateMetadata(): Metadata {
  return {
    title: t(CONSOLE_MESSAGE_KEYS.meta_title),
    description: t(CONSOLE_MESSAGE_KEYS.meta_description),
  };
}

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
