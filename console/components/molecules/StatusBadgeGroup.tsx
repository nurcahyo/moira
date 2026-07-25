import { Badge, type BadgeTone } from "../atoms/Badge";
import styles from "./StatusBadgeGroup.module.css";

export interface StatusBadgeItem {
  /** Stable React list key. Not rendered. */
  key: string;
  label: string;
  tone?: BadgeTone;
}

export interface StatusBadgeGroupProps {
  items: StatusBadgeItem[];
  /** Accessible name for the group, e.g. "Provider status". */
  "aria-label": string;
}

/**
 * Molecule composed from the Badge atom: renders a labeled group of
 * status badges as a semantic list. Feature-agnostic and presentational —
 * the caller supplies the items and their tones; renders nothing (rather
 * than an empty list landmark) when there are none.
 */
export function StatusBadgeGroup({
  items,
  "aria-label": ariaLabel,
}: StatusBadgeGroupProps) {
  if (items.length === 0) {
    return null;
  }

  return (
    <ul className={styles.list} aria-label={ariaLabel}>
      {items.map((item) => (
        <li key={item.key} className={styles.item}>
          <Badge tone={item.tone}>{item.label}</Badge>
        </li>
      ))}
    </ul>
  );
}
