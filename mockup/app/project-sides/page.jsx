'use client';

import { useEffect } from 'react';
import { useData } from '@/components/Data';
import { useT } from '@/components/Prefs';
import PageHead from '@/components/PageHead';

// Keep old bookmarks usable without exposing the retired manual connection flow.
export default function ProjectSidesPage() {
  const data = useData(), t = useT();
  useEffect(() => {
    if (data.nativeConsole && ['ready', 'stale'].includes(data.phase)) {
      // The native service serves static documents, not Next's RSC requests.
      window.location.replace('/console/server-engagements/');
    }
  }, [data.nativeConsole, data.phase]);
  return <PageHead title={t('nav.serverEngagements')} />;
}
