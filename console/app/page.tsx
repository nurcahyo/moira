// Placeholder home route — exists only so `next build` has an entrypoint.
// Out of scope for this scaffold: any real UI, auth gating, or Moira calls.
// Plan 08 replaces this page with the real console shell/dashboard; this
// stays a thin "page" per Atomic Design, only rendering an atom (Badge)
// to confirm the component layer wires up end to end.
import { Badge } from "@/components/atoms/Badge";
import styles from "./page.module.css";

export default function HomePage() {
  return (
    <main className={styles.main}>
      <h1 className={styles.title}>
        Moira Console <Badge tone="info">Scaffold</Badge>
      </h1>
      <p className={styles.body}>
        This workspace is scaffolded: toolchain, Atomic Design layers, and
        test harnesses are in place. Sign-in, the setup wizard, and every
        other feature arrive in{" "}
        <code>plans/08-nextjs-console-google-oauth.md</code>, once plan 07
        lands Moira&apos;s identity contract.
      </p>
    </main>
  );
}
