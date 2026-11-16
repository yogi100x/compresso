export function formatTable(rows: string[][]): string {
  const widths: number[] = [];
  for (const row of rows) {
    row.forEach((cell, column) => {
      widths[column] = Math.max(widths[column] ?? 0, cell.length);
    });
  }
  return rows
    .map((row) => row.map((cell, column) => cell.padEnd(widths[column])).join("  ").trimEnd())
    .join("\n");
}

export function printError(message: string): void {
  console.error(`error: ${message}`);
}

export function printLine(message: string): void {
  console.log(message);
}
