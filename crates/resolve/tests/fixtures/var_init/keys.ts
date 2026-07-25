export function collect(): string[] {
  return ["a"];
}

export function report(): number {
  // the call sits inside a variable initialiser; the caller is `report`
  const keys = new Set(collect());
  return keys.size;
}
