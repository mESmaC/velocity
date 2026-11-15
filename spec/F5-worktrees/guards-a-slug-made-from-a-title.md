#### Guards · A slug made from a title (a model title, a Task List item or a SPEC section) is at most 40 characters

<!-- split-spec: {"order":767,"kind":"markdown","indentation":0} -->
- A slug made from a title (a model title, a Task List item or a SPEC section) is at most 40 characters.
  A longer one keeps its first words and ends in `-` and six hex digits of a hash of the whole slug, so two long titles still get different names and the worktree, branch and git admin paths stay well under Windows' 260-character limit (#275).
  Titles themselves are never cut.
