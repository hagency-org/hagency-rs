'use client';

import PageHead from '@/components/PageHead';
import NativePlugins from '@/components/NativePlugins';
import { useData } from '@/components/Data';
import { useT } from '@/components/Prefs';

export default function PluginsPage() {
  const data = useData();
  const t = useT();
  return data.nativeConsole ? <NativePlugins /> : <PageHead title={t('pl.title')} sub={t('pl.sub')} />;
}
