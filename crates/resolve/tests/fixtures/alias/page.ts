import { original as renamed } from "./util";
import * as helpers from "./util";

export function run(): string {
  return renamed("x") + helpers.other("y");
}
