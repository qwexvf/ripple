export class Dup {
  run(): number {
    return 2;
  }
}

export function callsDup(d: Dup): number {
  return d.run();
}
