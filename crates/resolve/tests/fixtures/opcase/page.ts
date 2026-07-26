import { CurrentPlayerDocument } from "@/generated/graphql";

export function Page(): string {
  return String(CurrentPlayerDocument);
}
