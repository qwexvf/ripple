import { graphql } from "./gql";

const CurrentPlayerQuery = graphql(`
  query CurrentPlayer {
    currentPlayer {
      id
    }
  }
`);

export function PlayerPage() {
  useQuery({ query: CurrentPlayerQuery });
}
