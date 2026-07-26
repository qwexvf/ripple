import { CurrentPlayerViaFragmentDocument } from "@/generated/graphql";

export function FragmentPage(): string {
  return String(CurrentPlayerViaFragmentDocument);
}
