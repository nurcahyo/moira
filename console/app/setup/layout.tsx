// `/setup`'s layout: a public, pre-claim surface OUTSIDE the `(console)` group.
//
// It must not nest inside the authenticated chrome — the wizard runs before any
// session can exist, and a gated layout would redirect its only audience away.
// The root `app/layout.tsx` supplies `<html>`/`<body>`; this one only frames
// the wizard.

import type { ReactNode } from "react";

import styles from "./layout.module.css";

export default function SetupLayout({ children }: { children: ReactNode }) {
  return <div className={styles.shell}>{children}</div>;
}
