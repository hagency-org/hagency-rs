'use client';

import { useEffect } from 'react';
import { useRouter } from 'next/navigation';
import { useData } from '@/components/Data';
import { useT } from '@/components/Prefs';
import PageHead from '@/components/PageHead';

// Keep old bookmarks usable without exposing the retired manual connection flow.
export default function ProjectSidesPage() {
  const data = useData(), t = useT(), router = useRouter();
  useEffect(() => {
    if (data.nativeConsole && ['ready', 'stale'].includes(data.phase)) {
      router.replace('/server-engagements/');
    }
  }, [data.nativeConsole, data.phase, router]);
  return <PageHead title={t('nav.serverEngagements')} />;
}
