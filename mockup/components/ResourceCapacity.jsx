'use client';
import { useT } from '@/components/Prefs';
import Meter from '@/components/Meter';

// Allocation availability is not metered token consumption. Missing or stale
// observations never render a full bar, and zero capacity is a real value.
export default function ResourceCapacity({ remaining, total, retained, stale = false }) {
  const t = useT();
  const known = !stale && Number.isSafeInteger(remaining) && remaining >= 0 && Number.isSafeInteger(total) && total >= 0;
  const number = n => Number.isSafeInteger(n) ? n.toLocaleString() : t('nu.unknown');
  return <div className="resource-capacity" data-resource-capacity>
    <b>{known ? number(remaining) : t('nu.unknown')} / {number(total)} <span>{t('rc.tokensAvailable')}</span></b>
    {known && total > 0 && <Meter pct={remaining / total * 100} ok over={retained == null ? null : Math.max(0, retained - total)} label={t('rc.overdrawn', { n: number(retained - total) })} />}
    {stale ? <small>{t('rc.stale')}</small> : known && retained != null ? <small>{t('rc.retained', { n: number(retained) })}</small> : null}
  </div>;
}
