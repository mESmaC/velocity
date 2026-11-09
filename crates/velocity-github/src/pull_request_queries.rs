pub(crate) const FIELDS: &str = "id number title body url state isDraft author { login }
    headRefOid headRefName baseRefName additions deletions changedFiles createdAt updatedAt
    closedAt mergedAt reviewDecision mergeStateStatus headRepository { nameWithOwner }";

pub(crate) const COMMENT_FIELDS: &str = "id body url author { login } createdAt updatedAt";

pub(crate) const CHECKS: &str = "query($owner:String!,$name:String!,$number:Int!,$cursor:String) {
  repository(owner:$owner,name:$name) { pullRequest(number:$number) {
    commits(last:1) { nodes { commit { statusCheckRollup { contexts(first:100,after:$cursor) {
      nodes { __typename
        ... on CheckRun { name status conclusion detailsUrl }
        ... on StatusContext { context state targetUrl }
      } pageInfo { hasNextPage endCursor }
    } } } } }
  } }
}";

pub(crate) const THREADS: &str = "id path line startLine isResolved isOutdated";
pub(crate) const REVIEWS: &str = "id body state author { login } submittedAt commit { oid }";
