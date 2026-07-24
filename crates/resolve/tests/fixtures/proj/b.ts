import { helper } from "./a";

export function run(): number {
  return helper(21);
}

function boot() {
  run();
}
