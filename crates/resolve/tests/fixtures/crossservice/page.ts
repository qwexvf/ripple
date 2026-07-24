import { CurrentPlayerDocument, FollowPlayerDocument, DuplicatedDocument } from "./generated";

export function PlayerPage() {
  useQuery({ query: CurrentPlayerDocument });
  useMutation({ mutation: FollowPlayerDocument });
  useQuery({ query: DuplicatedDocument });
}
