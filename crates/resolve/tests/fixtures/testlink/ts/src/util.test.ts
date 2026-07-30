import { getPath } from "./util";

export function fixture(): string {
  return "http://x/y?z=1";
}

export function runs() {
  getPath(fixture());
}
