export interface Stats {
  observedTokens: number;
  appliedTokens: number;
  netSaved: number;
}

export async function fetchStats(baseUrl: string): Promise<Stats> {
  const res = await fetch(`${baseUrl}/v1/stats`);
  if (!res.ok) {
    throw new Error(`stats request failed: ${res.status}`);
  }
  return (await res.json()) as Stats;
}

export function reductionPct(stats: Stats): number {
  if (stats.observedTokens === 0) {
    return 0;
  }
  return (stats.netSaved / stats.observedTokens) * 100;
}
