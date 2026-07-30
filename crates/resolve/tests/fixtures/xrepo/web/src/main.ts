import { send } from "@org/api-client";

export function run(): string {
  return send("hi");
}
