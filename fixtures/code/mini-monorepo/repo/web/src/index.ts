import { fetchStats, reductionPct } from "./api";

async function main(): Promise<void> {
  const stats = await fetchStats("http://127.0.0.1:7410");
  console.log(`net saved: ${stats.netSaved} tokens`);
  console.log(`reduction: ${reductionPct(stats).toFixed(1)}%`);
}

main().catch((error: unknown) => {
  console.error("stats fetch failed", error);
  process.exit(1);
});
