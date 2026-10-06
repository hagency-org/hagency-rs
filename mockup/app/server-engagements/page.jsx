'use client';
import { useCallback, useState } from 'react';
import NativeAssociation from '@/components/NativeAssociation';
import PageHead from '@/components/PageHead';
import NativeStatusStrip from '@/components/NativeStatusStrip';
import NativeServerEngagements from '@/components/NativeServerEngagements';
import { useT } from '@/components/Prefs';
export default function ServerEngagementsPage() {
  const t = useT();
  const [version, setVersion] = useState(0);
  const connected = useCallback(() => setVersion(v => v + 1), []);
  return <><PageHead title={t('nav.serverEngagements')} sub={t('se.sub')}><NativeStatusStrip /></PageHead><NativeAssociation onConnected={connected} /><NativeServerEngagements key={version} /></>;
}
