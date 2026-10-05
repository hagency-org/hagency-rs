'use client';
import PageHead from '@/components/PageHead';
import NativeStatusStrip from '@/components/NativeStatusStrip';
import NativeServerEngagements from '@/components/NativeServerEngagements';
import { useT } from '@/components/Prefs';
export default function ServerEngagementsPage() {
  const t = useT();
  return <><PageHead title={t('nav.serverEngagements')} sub={t('se.sub')}><NativeStatusStrip /></PageHead><NativeServerEngagements /></>;
}
