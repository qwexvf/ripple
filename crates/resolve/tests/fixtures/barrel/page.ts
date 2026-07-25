import { getFragmentData, reveal } from "./gen";

export function render(): string {
  return getFragmentData("doc") + reveal("doc");
}
