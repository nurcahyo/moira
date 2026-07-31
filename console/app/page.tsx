// The home route. Copy comes from the console catalog, never from a literal
// here — `tests/unit/lib/no-hardcoded-copy.test.tsx` scans JSX text positions
// for exactly that.
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import styles from "./page.module.css";

export default function HomePage() {
  return (
    <main className={styles.main}>
      <h1 className={styles.title}>{t(CONSOLE_MESSAGE_KEYS.page_home_title)}</h1>
      <p className={styles.body}>{t(CONSOLE_MESSAGE_KEYS.page_home_body)}</p>
    </main>
  );
}
