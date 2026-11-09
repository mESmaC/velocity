module
public import Std
public section

/-!
# ACP client: design

Implements SPEC.md §7 M3 "ACP client (`agent-client-protocol`): spawn per thread,
`session/new` in the worktree with AIRC as an MCP server, updates → transcript, auto-allow
permissions, cancel, queued prompts, `fs`/`terminal` rooted at the worktree but not confined
to it, full tool access with your config loaded and shared MCP servers", which is SPEC.md
§3.2 Agent protocol: ACP. The parts it draws on:
- "Start a thread: Spawn the agent with your own config loaded (Tool access, below), and call
  `session/new` with `cwd` set to the thread's worktree and `mcpServers` set to AIRC plus
  your shared MCP servers."
- "Where an agent offers a bypass mode, `session/set_mode` selects it when the session
  starts."
- "Transcript, tool steps, diffs (F4): `session/update` notifications: message chunks,
  `tool_call` / `tool_call_update` (kind, status, file locations, diff content) and plan
  updates."
- "Velocity advertises the `fs` and `terminal` client capabilities, with relative paths
  resolved against the thread's worktree. They are not confined to it (Tool access)."
- Tool access: "AIRC: always passed (F14)." and "servers listed here are passed to every
  agent of every provider in `session/new`".

Permission answers (allow-always, Rule 1) and queued prompts are the agent runner's
(`proofs/AgentRunner/`); the client calls them unchanged. ACP's own terminal rule is used
for terminal output: when output passes `outputByteLimit`, the client truncates from the
beginning, at a character boundary.

The design has five parts, each a pure function the Rust crate implements:
1. `sessionServers`: the MCP servers passed in `session/new`: AIRC first, then the shared
   servers, keeping the first server of each name, since agents key servers by name.
2. `modeToSelect` and `startRequests`: the requests that start a session.
3. `resolvePath`: where an `fs` or `terminal` path points (Windows paths first).
4. `retainedStart`: how much terminal output to keep under the agent's byte limit.
5. `record` and `transcript`: how `session/update` notifications become transcript entries.

## Cost models and classes of alternatives

Each is stated again in the theorem that uses it (`cost_bound`, `optimal`).
1. Servers. Cost: servers passed. Class: every server list that contains a server of each
   required name (AIRC and every shared server).
2. Session start. Cost: requests sent. Class: every request sequence that sends an
   `initialize`, a `session/new`, and leaves the session in the bypass mode whenever the
   agent offers one, in any order and with any extra requests.
3. Paths. Cost: bytes in the resolved path. Class: every resolver meeting the declarative
   specification `Resolves`. The specification fixes the path, so no resolver writes fewer
   bytes.
4. Terminal output. Cost: bytes dropped from the start of the output. Class: every start
   position that keeps at most the limit and starts on a character boundary.
5. Transcript. Cost: entries shown. Class: every grouping of the updates into entries where
   each entry shows one thing (one message, one tool call or one plan) and entries are shown
   in the order things happened. An update to a tool call shown earlier may change that
   entry in place.
-/

namespace Velocity.AcpClient

open scoped List

/-! ## 1. MCP servers for `session/new` -/

section Servers

variable {Name Server : Type} [DecidableEq Name]

/-- `keepFrom seen names` marks each name that is in neither `seen` nor earlier in `names`. -/
@[expose] def keepFrom : List Name → List Name → List Bool
  | _, [] => []
  | seen, name :: rest => decide (name ∉ seen) :: keepFrom (seen ++ [name]) rest

/-- `keepFirstByName names` marks the first occurrence of each name. -/
@[expose] def keepFirstByName (names : List Name) : List Bool := keepFrom [] names

/-- Keeps the elements whose flag is `true`. -/
@[expose] def selectByKeep {Item : Type} : List Item → List Bool → List Item
  | item :: items, true :: flags => item :: selectByKeep items flags
  | _ :: items, false :: flags => selectByKeep items flags
  | _, _ => []

/-- The servers passed in `session/new`: AIRC first, then the shared servers, keeping the
first server of each name. -/
@[expose] def sessionServers (name : Server → Name) (airc : Server) (shared : List Server) :
    List Server :=
  selectByKeep (airc :: shared) (keepFirstByName ((airc :: shared).map name))

/-- The same selection, written as a recursion over servers. -/
@[expose] def selectFrom (name : Server → Name) : List Name → List Server → List Server
  | _, [] => []
  | seen, server :: rest =>
      if name server ∈ seen then selectFrom name seen rest
      else server :: selectFrom name (seen ++ [name server]) rest

theorem keepFrom_congr {first second : List Name}
    (same : ∀ candidate, candidate ∈ first ↔ candidate ∈ second) (names : List Name) :
    keepFrom first names = keepFrom second names := by
  induction names generalizing first second with
  | nil => rfl
  | cons head tail ih =>
    simp only [keepFrom]
    rw [ih (first := first ++ [head]) (second := second ++ [head]) fun candidate => by
      simp only [List.mem_append, List.mem_singleton, same candidate]]
    simp [same head]

theorem selectByKeep_keepFrom (name : Server → Name) (seen : List Name)
    (servers : List Server) :
    selectByKeep servers (keepFrom seen (servers.map name)) = selectFrom name seen servers := by
  induction servers generalizing seen with
  | nil => simp [selectByKeep, selectFrom]
  | cons server rest ih =>
    by_cases seenServer : name server ∈ seen
    · simp only [List.map_cons, keepFrom, seenServer, not_true_eq_false, decide_false,
        selectByKeep, selectFrom, ↓reduceIte]
      rw [keepFrom_congr (first := seen ++ [name server]) (second := seen) ?_ _, ih]
      intro candidate
      simp only [List.mem_append, List.mem_singleton]
      constructor
      · rintro (h | rfl)
        · exact h
        · exact seenServer
      · exact Or.inl
    · simp only [List.map_cons, keepFrom, seenServer, not_false_eq_true, decide_true,
        selectByKeep, selectFrom, ↓reduceIte, ih]

theorem sessionServers_eq (name : Server → Name) (airc : Server) (shared : List Server) :
    sessionServers name airc shared = selectFrom name [] (airc :: shared) :=
  selectByKeep_keepFrom name [] (airc :: shared)

theorem selectFrom_sublist (name : Server → Name) (seen : List Name) (servers : List Server) :
    selectFrom name seen servers <+ servers := by
  induction servers generalizing seen with
  | nil => exact List.Sublist.slnil
  | cons server rest ih =>
    simp only [selectFrom]
    split
    · exact (ih seen).cons server
    · exact (ih _).cons_cons server

theorem selectFrom_complete (name : Server → Name) (seen : List Name) (servers : List Server) :
    ∀ server ∈ servers, name server ∈ seen ∨
      name server ∈ (selectFrom name seen servers).map name := by
  induction servers generalizing seen with
  | nil => simp
  | cons head rest ih =>
    intro server member
    simp only [selectFrom]
    split
    next seenHead =>
      rcases List.mem_cons.mp member with same | later
      · exact Or.inl (same ▸ seenHead)
      · exact ih seen server later
    next _ =>
      rcases List.mem_cons.mp member with same | later
      · exact Or.inr (by simp [same])
      · rcases ih (seen ++ [name head]) server later with old | kept
        · rcases List.mem_append.mp old with inSeen | isHead
          · exact Or.inl inSeen
          · exact Or.inr (by simp at isHead; simp [isHead])
        · exact Or.inr (List.mem_cons_of_mem _ kept)

theorem selectFrom_fresh (name : Server → Name) (seen : List Name) (servers : List Server) :
    ((selectFrom name seen servers).map name).Nodup ∧
      ∀ server ∈ selectFrom name seen servers, name server ∉ seen := by
  induction servers generalizing seen with
  | nil => simp [selectFrom]
  | cons head rest ih =>
    simp only [selectFrom]
    split
    · exact ih seen
    next unseen =>
      obtain ⟨nodup, fresh⟩ := ih (seen ++ [name head])
      refine ⟨?_, ?_⟩
      · refine List.nodup_cons.mpr ⟨?_, nodup⟩
        intro inRest
        obtain ⟨server, member, sameName⟩ := List.mem_map.mp inRest
        exact fresh server member (by simp [sameName])
      · intro server member
        rcases List.mem_cons.mp member with same | later
        · exact same ▸ unseen
        · exact fun inSeen => fresh server later (List.mem_append_left _ inSeen)

/-- What `session/new` receives: AIRC first; every shared server's name; no name twice;
nothing that wasn't asked for, in the order given. -/
theorem sessionServers_correct (name : Server → Name) (airc : Server) (shared : List Server) :
    (∃ rest, sessionServers name airc shared = airc :: rest) ∧
    (∀ server ∈ airc :: shared, name server ∈ (sessionServers name airc shared).map name) ∧
    ((sessionServers name airc shared).map name).Nodup ∧
    sessionServers name airc shared <+ airc :: shared := by
  rw [sessionServers_eq]
  refine ⟨⟨selectFrom name [name airc] shared, by simp [selectFrom]⟩, fun server member => ?_,
    (selectFrom_fresh _ _ _).1,
    selectFrom_sublist _ _ _⟩
  rcases selectFrom_complete name [] (airc :: shared) server member with none | kept
  · simp at none
  · exact kept

/-- Cost model: the number of servers passed. Class of alternatives: every server list that
has a server of each required name (AIRC's and every shared server's). No such list is
shorter. -/
theorem sessionServers_optimal (name : Server → Name) (airc : Server) (shared : List Server)
    (alternative : List Server)
    (hasEveryName : ∀ server ∈ airc :: shared, name server ∈ alternative.map name) :
    (sessionServers name airc shared).length ≤ alternative.length := by
  obtain ⟨_, _, nodup, sublist⟩ := sessionServers_correct name airc shared
  have subset : (sessionServers name airc shared).map name ⊆ alternative.map name := by
    intro candidate member
    obtain ⟨server, kept, rfl⟩ := List.mem_map.mp member
    exact hasEveryName server (sublist.subset kept)
  have := nodup.length_le_of_subset subset
  simpa using this

end Servers

/-! ## 2. Session start -/

/-- The client's requests that start a session. -/
inductive StartRequest (Path Server : Type) where
  /-- `initialize`, advertising the client capabilities `fs.readTextFile`,
  `fs.writeTextFile` and `terminal`. -/
  | initialization (readTextFile writeTextFile terminal : Bool)
  /-- `session/new`. -/
  | newSession (cwd : Path) (mcpServers : List Server)
  /-- `session/set_mode`, by the mode's index in `availableModes`. -/
  | setMode (mode : Nat)

namespace StartRequest

variable {Path Server : Type}

@[expose] def isInitialize : StartRequest Path Server → Bool
  | .initialization .. => true
  | _ => false

@[expose] def isNewSession : StartRequest Path Server → Bool
  | .newSession .. => true
  | _ => false

@[expose] def isSetMode : StartRequest Path Server → Bool
  | .setMode .. => true
  | _ => false

end StartRequest

section Start

variable {Path Server : Type}

/-- The mode to select with `session/set_mode`: the bypass mode the agent offers (chosen by
`AgentRunner.chooseMode`), unless the session already starts in it. -/
@[expose] def modeToSelect (bypass current : Option Nat) : Option Nat :=
  match bypass with
  | none => none
  | some mode => if current = some mode then none else some mode

/-- Velocity's requests: `initialize` with `fs` and `terminal`, `session/new` in the worktree
with the session's servers, then `session/set_mode` only where it changes the mode. The
bypass mode is read from the `session/new` response, so `set_mode` comes last. -/
@[expose] def startRequests (worktree : Path) (servers : List Server)
    (bypass current : Option Nat) : List (StartRequest Path Server) :=
  [.initialization true true true, .newSession worktree servers] ++
    match modeToSelect bypass current with
    | some mode => [.setMode mode]
    | none => []

/-- The session's mode after a sequence of requests: the last mode set, or the mode the
session started in. -/
@[expose] def modeAfter (current : Option Nat) : List (StartRequest Path Server) → Option Nat
  | [] => current
  | .setMode mode :: rest => modeAfter (some mode) rest
  | _ :: rest => modeAfter current rest

/-- A request sequence that starts a session as §3.2 requires: it initializes, creates a
session, and leaves the session in the bypass mode whenever the agent offers one. It may
send them in any order and send anything else besides. -/
@[expose] def StartsSession (bypass current : Option Nat)
    (requests : List (StartRequest Path Server)) : Prop :=
  (∃ request ∈ requests, request.isInitialize) ∧
  (∃ request ∈ requests, request.isNewSession) ∧
  (∀ mode, bypass = some mode → modeAfter current requests = some mode)

theorem modeAfter_no_setMode (current : Option Nat) (requests : List (StartRequest Path Server))
    (none : ∀ request ∈ requests, request.isSetMode = false) :
    modeAfter current requests = current := by
  induction requests with
  | nil => rfl
  | cons head rest ih =>
    cases head with
    | setMode mode => simp [StartRequest.isSetMode] at none
    | initialization => exact ih fun request member => none request (List.mem_cons_of_mem _ member)
    | newSession => exact ih fun request member => none request (List.mem_cons_of_mem _ member)

theorem length_by_kind (requests : List (StartRequest Path Server)) :
    requests.length = requests.countP (·.isInitialize) + requests.countP (·.isNewSession) +
      requests.countP (·.isSetMode) := by
  induction requests with
  | nil => rfl
  | cons head rest ih =>
    cases head <;>
      simp [StartRequest.isInitialize, StartRequest.isNewSession,
        StartRequest.isSetMode, ih] <;> omega

/-- The start requests do what §3.2 says: `fs` and `terminal` are advertised, `session/new`
runs in the worktree with the session's servers, and the session ends up in the bypass mode
whenever the agent offers one. -/
theorem startRequests_correct (worktree : Path) (servers : List Server)
    (bypass current : Option Nat) :
    (startRequests worktree servers bypass current).head? =
      some (.initialization true true true) ∧
    (startRequests worktree servers bypass current)[1]? = some (.newSession worktree servers) ∧
    StartsSession bypass current (startRequests worktree servers bypass current) := by
  refine ⟨by simp [startRequests], by simp [startRequests],
    ⟨.initialization true true true, by simp [startRequests], rfl⟩,
    ⟨.newSession worktree servers, by simp [startRequests], rfl⟩, fun mode isBypass => ?_⟩
  subst isBypass
  by_cases already : current = some mode
  · simp [startRequests, modeToSelect, already, modeAfter]
  · simp [startRequests, modeToSelect, already, modeAfter]

/-- Cost model: requests sent. At most three; exactly two when the session needs no mode
change. -/
theorem startRequests_cost (worktree : Path) (servers : List Server)
    (bypass current : Option Nat) :
    (startRequests worktree servers bypass current).length =
      2 + (if (modeToSelect bypass current).isSome then 1 else 0) ∧
    (startRequests worktree servers bypass current).length ≤ 3 := by
  simp only [startRequests]
  cases modeToSelect bypass current <;> simp

/-- Cost model: requests sent. Class of alternatives: every sequence satisfying
`StartsSession` (any order, any extra requests). None sends fewer requests. -/
theorem startRequests_optimal (worktree : Path) (servers : List Server)
    (bypass current : Option Nat) (alternative : List (StartRequest Path Server))
    (starts : StartsSession bypass current alternative) :
    (startRequests worktree servers bypass current).length ≤ alternative.length := by
  obtain ⟨⟨initRequest, initializeMember, isInitialize⟩, ⟨newSession, newSessionMember,
    isNewSession⟩, endsInBypass⟩ := starts
  have initializes : 0 < alternative.countP (·.isInitialize) :=
    List.countP_pos_iff.mpr ⟨initRequest, initializeMember, isInitialize⟩
  have creates : 0 < alternative.countP (·.isNewSession) :=
    List.countP_pos_iff.mpr ⟨newSession, newSessionMember, isNewSession⟩
  rw [length_by_kind alternative, (startRequests_cost worktree servers bypass current).1]
  cases bypass with
  | none =>
    simp only [modeToSelect, Option.isSome_none, Bool.false_eq_true, ↓reduceIte]
    omega
  | some mode =>
    by_cases already : current = some mode
    · simp only [modeToSelect, already, ↓reduceIte, Option.isSome_none, Bool.false_eq_true]
      omega
    · have sets : 0 < alternative.countP (·.isSetMode) := by
        refine List.countP_pos_iff.mpr ?_
        refine Classical.byContradiction fun noSet => already ?_
        have noneSet : ∀ request ∈ alternative, request.isSetMode = false := by
          intro request member
          cases isSet : request.isSetMode
          · rfl
          · exact absurd ⟨request, member, isSet⟩ noSet
        rw [← modeAfter_no_setMode current alternative noneSet]
        exact endsInBypass mode rfl
      simp only [modeToSelect, already, ↓reduceIte, Option.isSome_some]
      omega

end Start

/-! ## 3. Paths for `fs` and `terminal` -/

/-- A path as UTF-8 bytes. Separators, drive letters and colons are ASCII, so every byte
test below is exact on UTF-8. -/
abbrev Bytes := List Nat

/-- `\` and `/`: Windows accepts both. -/
@[expose] def isSeparator (byte : Nat) : Bool := byte == 92 || byte == 47

@[expose] def isDriveLetter (byte : Nat) : Bool :=
  (65 ≤ byte && byte ≤ 90) || (97 ≤ byte && byte ≤ 122)

/-- Drive letters compare without case: `c:` is `C:`. -/
@[expose] def foldCase (byte : Nat) : Nat := if 65 ≤ byte && byte ≤ 90 then byte + 32 else byte

/-- `/` → `\`. -/
@[expose] def toBackslash (byte : Nat) : Nat := if byte == 47 then 92 else byte

/-- The shapes of a requested path. -/
inductive Shape where
  | empty
  /-- `C:\…`, `C:/…`, `\\server\share\…`, `\\?\…`, `//…`: used as given. -/
  | absolute
  /-- `\…` or `/…`: from the root of the worktree's drive or share. On a POSIX worktree
  this is an absolute path. -/
  | rootRelative
  /-- `C:…` without a separator: relative to the current folder of drive `C:`. That is the
  worktree when the worktree is on `C:`, and the drive's root otherwise. -/
  | driveRelative (drive : Nat) (rest : Bytes)
  /-- Anything else: relative to the worktree. -/
  | relative
  deriving DecidableEq

@[expose] def shape : Bytes → Shape
  | [] => .empty
  | [first] => if isSeparator first then .rootRelative else .relative
  | first :: second :: rest =>
      if isSeparator first then
        if isSeparator second then .absolute else .rootRelative
      else if isDriveLetter first && second == 58 then
        match rest with
        | [] => .driveRelative first []
        | third :: _ => if isSeparator third then .absolute else .driveRelative first rest
      else .relative

/-- The worktree's drive letter, case folded. -/
@[expose] def driveOf : Bytes → Option Nat
  | first :: second :: _ =>
      if isDriveLetter first && second == 58 then some (foldCase first) else none
  | _ => none

/-- Windows worktrees get `\` separators. -/
@[expose] def windowsStyle (worktree : Bytes) : Bool :=
  (driveOf worktree).isSome || worktree.contains 92

/-- Where a UNC share name ends in `\\server\share\…`. `separators` counts the separators
seen after the leading `\\`; `index` is the position of the next byte. -/
@[expose] def uncRootEnd (separators : Nat) (index : Nat) : Bytes → Nat
  | [] => index
  | byte :: rest =>
      if isSeparator byte then
        if separators == 1 then index else uncRootEnd (separators + 1) (index + 1) rest
      else uncRootEnd separators (index + 1) rest

/-- The length of the worktree's root: `C:` (2), `\\server\share`, or nothing on POSIX. -/
@[expose] def rootLength : Bytes → Nat
  | first :: second :: rest =>
      if isDriveLetter first && second == 58 then 2
      else if isSeparator first && isSeparator second then uncRootEnd 0 2 rest
      else 0
  | _ => 0

@[expose] def normalize (worktree part : Bytes) : Bytes :=
  if windowsStyle worktree then part.map toBackslash else part

@[expose] def needsSeparator (worktree : Bytes) : Bool :=
  match worktree.getLast? with
  | none => false
  | some last => !isSeparator last

@[expose] def separatorFor (worktree : Bytes) : Nat := if windowsStyle worktree then 92 else 47

/-- `worktree`, one separator, then `part`. -/
@[expose] def joinRelative (worktree part : Bytes) : Bytes :=
  worktree ++ (if needsSeparator worktree then [separatorFor worktree] else []) ++
    normalize worktree part

/-- Where a path in an `fs` or `terminal` request points. Relative paths are resolved
against the worktree; absolute paths are used as given, so agents can reach any file on the
machine (§3.2 Tool access: "The working directory, not a jail"). -/
@[expose] def resolvePath (worktree requested : Bytes) : Bytes :=
  match shape requested with
  | .empty => worktree
  | .absolute => requested
  | .rootRelative => worktree.take (rootLength worktree) ++ normalize worktree requested
  | .driveRelative drive rest =>
      if driveOf worktree = some (foldCase drive) then joinRelative worktree rest
      else [drive, 58, 92] ++ rest.map toBackslash
  | .relative => joinRelative worktree requested

/-- The declarative specification of path resolution, case by case. -/
@[expose] def Resolves (worktree requested result : Bytes) : Prop :=
  (shape requested = .empty → result = worktree) ∧
  (shape requested = .absolute → result = requested) ∧
  (shape requested = .rootRelative →
    result = worktree.take (rootLength worktree) ++ normalize worktree requested) ∧
  (∀ drive rest, shape requested = .driveRelative drive rest →
    (driveOf worktree = some (foldCase drive) → result = joinRelative worktree rest) ∧
    (driveOf worktree ≠ some (foldCase drive) →
      result = [drive, 58, 92] ++ rest.map toBackslash)) ∧
  (shape requested = .relative → result = joinRelative worktree requested)

theorem resolvePath_resolves (worktree requested : Bytes) :
    Resolves worktree requested (resolvePath worktree requested) := by
  refine ⟨fun h => by simp only [resolvePath, h], fun h => by simp only [resolvePath, h],
    fun h => by simp only [resolvePath, h], fun drive rest h => ⟨fun same => ?_, fun other => ?_⟩,
    fun h => by simp only [resolvePath, h]⟩
  · simp only [resolvePath, h]
    exact ite_eq_left same
  · simp only [resolvePath, h]
    exact ite_eq_right other

/-- `Resolves` fixes the result: path resolution leaves no choice. -/
theorem resolves_unique (worktree requested first second : Bytes)
    (resolvesFirst : Resolves worktree requested first)
    (resolvesSecond : Resolves worktree requested second) : first = second := by
  obtain ⟨empty₁, absolute₁, root₁, drive₁, relative₁⟩ := resolvesFirst
  obtain ⟨empty₂, absolute₂, root₂, drive₂, relative₂⟩ := resolvesSecond
  cases h : shape requested with
  | empty => rw [empty₁ h, empty₂ h]
  | absolute => rw [absolute₁ h, absolute₂ h]
  | rootRelative => rw [root₁ h, root₂ h]
  | relative => rw [relative₁ h, relative₂ h]
  | driveRelative drive rest =>
    by_cases same : driveOf worktree = some (foldCase drive)
    · rw [(drive₁ drive rest h).1 same, (drive₂ drive rest h).1 same]
    · rw [(drive₁ drive rest h).2 same, (drive₂ drive rest h).2 same]

theorem letter_not_separator (byte : Nat) (letter : isDriveLetter byte = true) :
    isSeparator byte = false := by
  unfold isSeparator
  have notBackslash : (byte == 92) = false := by
    rw [beq_eq_false_iff_ne]
    intro same
    subst same
    simp [isDriveLetter] at letter
  have notSlash : (byte == 47) = false := by
    rw [beq_eq_false_iff_ne]
    intro same
    subst same
    simp [isDriveLetter] at letter
  rw [notBackslash, notSlash]
  rfl

theorem separator_not_letter (byte : Nat) (separator : isSeparator byte = true) :
    isDriveLetter byte = false := by
  cases letter : isDriveLetter byte
  · rfl
  · rw [letter_not_separator byte letter] at separator
    exact absurd separator Bool.false_ne_true

theorem shape_separators (first second : Nat) (rest : Bytes)
    (separatorFirst : isSeparator first = true) (separatorSecond : isSeparator second = true) :
    shape (first :: second :: rest) = .absolute := by
  simp only [shape, separatorFirst, separatorSecond, ↓reduceIte]

theorem shape_drive_separator (drive separator : Nat) (rest : Bytes)
    (letter : isDriveLetter drive = true) (isSep : isSeparator separator = true) :
    shape (drive :: 58 :: separator :: rest) = .absolute := by
  simp only [shape, letter_not_separator drive letter, letter, isSep, Bool.false_eq_true,
    ↓reduceIte, Bool.true_and, beq_self_eq_true]

theorem shape_absolute_cases (path : Bytes) (absolute : shape path = .absolute) :
    (∃ first second rest, path = first :: second :: rest ∧
      isSeparator first = true ∧ isSeparator second = true) ∨
    (∃ drive separator rest, path = drive :: 58 :: separator :: rest ∧
      isDriveLetter drive = true ∧ isSeparator separator = true) := by
  match path, absolute with
  | [], h => simp only [shape, reduceCtorEq] at h
  | [first], h =>
    cases separatorFirst : isSeparator first <;>
      simp only [shape, separatorFirst, Bool.false_eq_true, ↓reduceIte, reduceCtorEq] at h
  | first :: second :: rest, h =>
    cases separatorFirst : isSeparator first
    · cases drive : (isDriveLetter first && second == 58)
      · simp only [shape, separatorFirst, drive, Bool.false_eq_true, ↓reduceIte,
          reduceCtorEq] at h
      · have parts := drive
        simp only [Bool.and_eq_true, beq_iff_eq] at parts
        match rest, h with
        | [], h =>
          simp only [shape, separatorFirst, drive, Bool.false_eq_true, ↓reduceIte,
            reduceCtorEq] at h
        | third :: more, h =>
          cases separatorThird : isSeparator third
          · simp only [shape, separatorFirst, drive, separatorThird, Bool.false_eq_true,
              ↓reduceIte, reduceCtorEq] at h
          · exact Or.inr ⟨first, third, more, by rw [parts.2], parts.1, separatorThird⟩
    · cases separatorSecond : isSeparator second
      · simp only [shape, separatorFirst, separatorSecond, Bool.false_eq_true, ↓reduceIte,
          reduceCtorEq] at h
      · exact Or.inl ⟨first, second, rest, rfl, separatorFirst, separatorSecond⟩

theorem shape_rootRelative_cases (path : Bytes) (rootRelative : shape path = .rootRelative) :
    ∃ separator rest, path = separator :: rest ∧ isSeparator separator = true := by
  match path, rootRelative with
  | [], h => simp only [shape, reduceCtorEq] at h
  | [first], h =>
    cases separatorFirst : isSeparator first
    · simp only [shape, separatorFirst, Bool.false_eq_true, ↓reduceIte, reduceCtorEq] at h
    · exact ⟨first, [], rfl, separatorFirst⟩
  | first :: second :: rest, h =>
    cases separatorFirst : isSeparator first
    · cases drive : (isDriveLetter first && second == 58)
      · simp only [shape, separatorFirst, drive, Bool.false_eq_true, ↓reduceIte,
          reduceCtorEq] at h
      · match rest, h with
        | [], h =>
          simp only [shape, separatorFirst, drive, Bool.false_eq_true, ↓reduceIte,
            reduceCtorEq] at h
        | third :: more, h =>
          cases separatorThird : isSeparator third <;>
            simp only [shape, separatorFirst, drive, separatorThird, Bool.false_eq_true,
              ↓reduceIte, reduceCtorEq] at h
    · exact ⟨first, second :: rest, rfl, separatorFirst⟩

theorem shape_driveRelative_cases (path : Bytes) (drive : Nat) (rest : Bytes)
    (driveRelative : shape path = .driveRelative drive rest) :
    path = drive :: 58 :: rest ∧ isDriveLetter drive = true := by
  match path, driveRelative with
  | [], h => simp only [shape, reduceCtorEq] at h
  | [first], h =>
    cases separatorFirst : isSeparator first <;>
      simp only [shape, separatorFirst, Bool.false_eq_true, ↓reduceIte, reduceCtorEq] at h
  | first :: second :: more, h =>
    cases separatorFirst : isSeparator first
    · cases isDrive : (isDriveLetter first && second == 58)
      · simp only [shape, separatorFirst, isDrive, Bool.false_eq_true, ↓reduceIte,
          reduceCtorEq] at h
      · have parts := isDrive
        simp only [Bool.and_eq_true, beq_iff_eq] at parts
        match more, h with
        | [], h =>
          simp only [shape, separatorFirst, isDrive, Bool.false_eq_true, ↓reduceIte,
            Shape.driveRelative.injEq] at h
          obtain ⟨rfl, rfl⟩ := h
          exact ⟨by rw [parts.2], parts.1⟩
        | third :: tail, h =>
          cases separatorThird : isSeparator third
          · simp only [shape, separatorFirst, isDrive, separatorThird, Bool.false_eq_true,
              ↓reduceIte, Shape.driveRelative.injEq] at h
            obtain ⟨rfl, rfl⟩ := h
            exact ⟨by rw [parts.2], parts.1⟩
          · simp only [shape, separatorFirst, isDrive, separatorThird, Bool.false_eq_true,
              ↓reduceIte, reduceCtorEq] at h
    · cases separatorSecond : isSeparator second <;>
        simp only [shape, separatorFirst, separatorSecond, Bool.false_eq_true, ↓reduceIte,
          reduceCtorEq] at h

theorem shape_append_absolute (path suffix : Bytes) (absolute : shape path = .absolute) :
    shape (path ++ suffix) = .absolute := by
  rcases shape_absolute_cases path absolute with
    ⟨first, second, rest, rfl, separatorFirst, separatorSecond⟩ |
    ⟨drive, separator, rest, rfl, letter, isSep⟩
  · exact shape_separators first second _ separatorFirst separatorSecond
  · exact shape_drive_separator drive separator _ letter isSep

theorem isSeparator_toBackslash (byte : Nat) :
    isSeparator (toBackslash byte) = isSeparator byte := by
  unfold toBackslash
  cases slash : (byte == 47)
  · rfl
  · rw [beq_iff_eq] at slash
    subst slash
    rfl

theorem uncRootEnd_ge (separators index : Nat) (rest : Bytes) :
    index ≤ uncRootEnd separators index rest := by
  induction rest generalizing separators index with
  | nil => exact Nat.le_refl _
  | cons byte more ih =>
    simp only [uncRootEnd]
    split
    · split
      · exact Nat.le_refl _
      · exact Nat.le_of_succ_le (ih _ _)
    · exact Nat.le_of_succ_le (ih _ _)

/-- On an absolute worktree, every resolved path is absolute: a relative path never reaches
the operating system, where it would resolve against Velocity's own folder instead of the
worktree. -/
theorem resolvePath_absolute (worktree requested : Bytes)
    (absoluteWorktree : shape worktree = .absolute) :
    shape (resolvePath worktree requested) = .absolute := by
  cases h : shape requested with
  | empty =>
    simp only [resolvePath, h]
    exact absoluteWorktree
  | absolute =>
    simp only [resolvePath, h]
  | relative =>
    simp only [resolvePath, h, joinRelative, List.append_assoc]
    exact shape_append_absolute _ _ absoluteWorktree
  | driveRelative drive rest =>
    simp only [resolvePath, h]
    split
    · simp only [joinRelative, List.append_assoc]
      exact shape_append_absolute _ _ absoluteWorktree
    · exact shape_drive_separator drive 92 _ (shape_driveRelative_cases requested drive rest h).2
        rfl
  | rootRelative =>
    simp only [resolvePath, h]
    obtain ⟨separator, more, rfl, isSep⟩ := shape_rootRelative_cases requested h
    rcases shape_absolute_cases worktree absoluteWorktree with
      ⟨first, second, rest, rfl, separatorFirst, separatorSecond⟩ |
      ⟨drive, separator', rest, rfl, letter, _⟩
    · have notLetter := separator_not_letter first separatorFirst
      simp only [rootLength, notLetter, Bool.false_and, Bool.false_eq_true, ↓reduceIte,
        separatorFirst, separatorSecond, Bool.and_self]
      obtain ⟨extra, isExtra⟩ : ∃ extra, uncRootEnd 0 2 rest = extra + 1 + 1 :=
        ⟨uncRootEnd 0 2 rest - 2, by have := uncRootEnd_ge 0 2 rest; omega⟩
      rw [isExtra, List.take_succ_cons, List.take_succ_cons, List.cons_append, List.cons_append]
      exact shape_separators first second _ separatorFirst separatorSecond
    · have rootIs : rootLength (drive :: 58 :: separator' :: rest) = 2 := by
        simp only [rootLength, letter, Bool.true_and, beq_self_eq_true, ↓reduceIte]
      have windows : windowsStyle (drive :: 58 :: separator' :: rest) = true := by
        simp only [windowsStyle, driveOf, letter, Bool.true_and, beq_self_eq_true, ↓reduceIte,
          Option.isSome_some, Bool.true_or]
      have takeIs : (drive :: 58 :: separator' :: rest).take 2 = [drive, 58] := rfl
      rw [rootIs, takeIs]
      simp only [normalize, windows, ↓reduceIte, List.map_cons, List.cons_append,
        List.nil_append]
      exact shape_drive_separator drive _ _ letter ((isSeparator_toBackslash separator).trans isSep)

/-- Relative paths are rooted at the worktree: the result starts with the worktree. -/
theorem resolvePath_rooted (worktree requested : Bytes)
    (relative : shape requested = .relative ∨ shape requested = .empty) :
    worktree <+: resolvePath worktree requested := by
  rcases relative with relative | empty
  · simp only [resolvePath, relative, joinRelative, List.append_assoc]
    exact List.prefix_append _ _
  · simp only [resolvePath, empty]
    exact List.prefix_refl worktree

/-- Absolute paths are used as given: an agent's file access is not confined to its
worktree (§3.2 Tool access). -/
theorem resolvePath_not_confined (worktree requested : Bytes)
    (absolute : shape requested = .absolute) : resolvePath worktree requested = requested := by
  simp only [resolvePath, absolute]

theorem normalize_length (worktree part : Bytes) :
    (normalize worktree part).length = part.length := by
  simp only [normalize]
  split
  · exact List.length_map _
  · rfl

/-- Cost model: bytes in the resolved path. At most the worktree, the requested path and
three more bytes. -/
theorem resolvePath_cost (worktree requested : Bytes) :
    (resolvePath worktree requested).length ≤ worktree.length + requested.length + 3 := by
  have joinBound : ∀ part : Bytes,
      (joinRelative worktree part).length ≤ worktree.length + part.length + 1 := by
    intro part
    simp only [joinRelative, List.length_append, normalize_length]
    split <;> simp only [List.length_singleton, List.length_nil] <;> omega
  cases h : shape requested with
  | empty =>
    simp only [resolvePath, h]
    omega
  | absolute =>
    simp only [resolvePath, h]
    omega
  | rootRelative =>
    simp only [resolvePath, h, List.length_append, List.length_take, normalize_length]
    omega
  | relative =>
    simp only [resolvePath, h]
    have := joinBound requested
    omega
  | driveRelative drive rest =>
    obtain ⟨isPath, _⟩ := shape_driveRelative_cases requested drive rest h
    have restShorter : rest.length + 2 = requested.length := by
      rw [isPath]
      rfl
    simp only [resolvePath, h]
    split
    · have := joinBound rest
      omega
    · simp only [List.length_append, List.length_map, List.length_cons, List.length_nil]
      omega

/-- Cost model: bytes in the resolved path. Class of alternatives: every resolver meeting
the specification `Resolves`. Each returns the same path, so none writes fewer bytes. -/
theorem resolvePath_optimal (worktree requested alternative : Bytes)
    (resolves : Resolves worktree requested alternative) :
    (resolvePath worktree requested).length ≤ alternative.length := by
  rw [resolves_unique worktree requested _ _ (resolvePath_resolves worktree requested) resolves]
  exact Nat.le_refl _

/-! ## 4. Terminal output under `outputByteLimit` -/

/-- UTF-8 continuation bytes, `10xxxxxx`: a character never starts on one. -/
@[expose] def isContinuation (byte : Nat) : Bool := 128 ≤ byte && byte < 192

/-- A position where a character starts, or the end of the output. -/
@[expose] def CharBoundary (output : Bytes) (start : Nat) : Prop :=
  start = output.length ∨ ∃ inBounds : start < output.length, isContinuation output[start] = false

/-- The first byte to keep: all of it when it fits, otherwise the first character boundary
from which the rest fits (ACP: "truncate from the beginning of the output", at a character
boundary). -/
@[expose] def retainedStart (output : Bytes) (limit : Nat) : Nat :=
  if output.length ≤ limit then 0
  else
    (output.length - limit) +
      ((output.drop (output.length - limit)).takeWhile isContinuation).length

/-- The positions an implementation may keep from: the rest fits the limit, and it starts a
character (or keeps everything). -/
@[expose] def ValidStart (output : Bytes) (limit start : Nat) : Prop :=
  start ≤ output.length ∧ output.length - start ≤ limit ∧ (start = 0 ∨ CharBoundary output start)

theorem takeWhile_facts (keep : Nat → Bool) : ∀ bytes : Bytes,
    (bytes.takeWhile keep).length ≤ bytes.length ∧
    (∀ position (_ : position < (bytes.takeWhile keep).length),
      ∃ inBounds : position < bytes.length, keep bytes[position] = true) ∧
    (∀ inBounds : (bytes.takeWhile keep).length < bytes.length,
      keep bytes[(bytes.takeWhile keep).length] = false)
  | [] => by simp
  | byte :: rest => by
    cases keepByte : keep byte
    · simp only [List.takeWhile_cons, keepByte, Bool.false_eq_true, ↓reduceIte,
        List.length_nil, List.length_cons]
      refine ⟨Nat.zero_le _, fun position below => absurd below (Nat.not_lt_zero _),
        fun _ => keepByte⟩
    · obtain ⟨bound, inside, stop⟩ := takeWhile_facts keep rest
      simp only [List.takeWhile_cons, keepByte, ↓reduceIte, List.length_cons]
      refine ⟨by omega, fun position below => ?_, fun inBounds => stop (by omega)⟩
      cases position with
      | zero => exact ⟨by simp, keepByte⟩
      | succ position =>
        obtain ⟨inRest, keepAt⟩ := inside position (by omega)
        exact ⟨by simp; omega, keepAt⟩

theorem takeWhile_boundary (output : Bytes) (from_ : Nat) (fromInBounds : from_ ≤ output.length) :
    CharBoundary output (from_ + ((output.drop from_).takeWhile isContinuation).length) ∧
    (∀ position, from_ ≤ position →
      position < from_ + ((output.drop from_).takeWhile isContinuation).length →
      ∃ inBounds : position < output.length, isContinuation output[position] = true) ∧
    from_ + ((output.drop from_).takeWhile isContinuation).length ≤ output.length := by
  obtain ⟨bound, inside, stop⟩ := takeWhile_facts isContinuation (output.drop from_)
  simp only [List.length_drop] at bound
  refine ⟨?_, fun position low high => ?_, by omega⟩
  · by_cases atEnd : from_ + ((output.drop from_).takeWhile isContinuation).length = output.length
    · exact Or.inl atEnd
    · have below : ((output.drop from_).takeWhile isContinuation).length <
          (output.drop from_).length := by simp only [List.length_drop]; omega
      have notCont := stop below
      simp only [List.getElem_drop] at notCont
      exact Or.inr ⟨by omega, notCont⟩
  · obtain ⟨inDrop, isCont⟩ := inside (position - from_) (by omega)
    simp only [List.getElem_drop, Nat.add_sub_cancel' low] at isCont
    exact ⟨by simp only [List.length_drop] at inDrop; omega, isCont⟩

/-- Keeps at most the limit, from a character boundary, and keeps everything that fits. -/
theorem retainedStart_correct (output : Bytes) (limit : Nat) :
    ValidStart output limit (retainedStart output limit) ∧
    (output.length ≤ limit → retainedStart output limit = 0) := by
  refine ⟨?_, fun fits => by simp [retainedStart, fits]⟩
  unfold retainedStart
  split
  · exact ⟨Nat.zero_le _, by omega, Or.inl rfl⟩
  · obtain ⟨boundary, _, bound⟩ := takeWhile_boundary output (output.length - limit) (by omega)
    exact ⟨bound, by omega, Or.inr boundary⟩

/-- Cost model: bytes inspected past the cut, one per skipped continuation byte. At most
the limit. -/
theorem retainedStart_cost (output : Bytes) (limit : Nat) :
    retainedStart output limit - (output.length - limit) ≤ limit := by
  unfold retainedStart
  split
  · omega
  · obtain ⟨_, _, bound⟩ := takeWhile_boundary output (output.length - limit) (by omega)
    omega

/-- Cost model: bytes dropped from the start of the output. Class of alternatives: every
start that keeps at most the limit from a character boundary (`ValidStart`). None drops
fewer bytes. -/
theorem retainedStart_optimal (output : Bytes) (limit start : Nat)
    (valid : ValidStart output limit start) : retainedStart output limit ≤ start := by
  obtain ⟨_, fits, boundaryOrZero⟩ := valid
  unfold retainedStart
  split
  · exact Nat.zero_le _
  next tooLong =>
    obtain ⟨_, inside, _⟩ := takeWhile_boundary output (output.length - limit) (by omega)
    refine Nat.le_of_not_lt fun below => ?_
    rcases boundaryOrZero with zero | boundary
    · omega
    · rcases boundary with atEnd | ⟨inBounds, notCont⟩
      · omega
      · obtain ⟨_, isCont⟩ := inside start (by omega) below
        simp_all

/-! ## 5. Transcript -/

/-- What one transcript entry shows. Message chunks carry an optional `messageId`. -/
inductive EntryClass (Key : Type) where
  | userMessage (message : Option Key)
  | agentMessage (message : Option Key)
  | agentThought (message : Option Key)
  | toolCall (call : Key)
  | plan
  deriving DecidableEq

@[expose] def EntryClass.isToolCall {Key : Type} : EntryClass Key → Bool
  | .toolCall _ => true
  | _ => false

/-- One entry: what it shows and the updates it holds, oldest first. The thread view merges
a tool call's updates and shows a plan's latest version. -/
structure Entry (Key Payload : Type) where
  entryClass : EntryClass Key
  payloads : List Payload

/-- Where `record` put an update. -/
inductive Placement where
  | appended (index : Nat)
  | added (index : Nat)
  deriving DecidableEq

section Transcript

variable {Key Payload : Type} [DecidableEq Key]

/-- The first entry of this class. -/
@[expose] def findClass (entryClass : EntryClass Key) : List (Entry Key Payload) → Option Nat
  | [] => none
  | entry :: rest =>
      if entry.entryClass = entryClass then some 0
      else (findClass entryClass rest).map (· + 1)

/-- The entry an update joins. A tool call update joins its call wherever it is. A message
chunk or plan joins the last entry when that entry shows the same thing; otherwise it
starts a new entry. -/
@[expose] def entryFor (entries : List (Entry Key Payload)) (entryClass : EntryClass Key) :
    Option Nat :=
  if entryClass.isToolCall then findClass entryClass entries
  else
    match entries.getLast? with
    | some last => if last.entryClass = entryClass then some (entries.length - 1) else none
    | none => none

@[expose] def appendAt (payload : Payload) : Nat → List (Entry Key Payload) → List (Entry Key Payload)
  | _, [] => []
  | 0, entry :: rest => { entry with payloads := entry.payloads ++ [payload] } :: rest
  | index + 1, entry :: rest => entry :: appendAt payload index rest

@[expose] def record (entries : List (Entry Key Payload)) (entryClass : EntryClass Key)
    (payload : Payload) : List (Entry Key Payload) × Placement :=
  match entryFor entries entryClass with
  | some index => (appendAt payload index entries, .appended index)
  | none => (entries ++ [⟨entryClass, [payload]⟩], .added entries.length)

/-- The transcript of a session's updates, in the order they arrived. -/
@[expose] def transcript (updates : List (EntryClass Key × Payload)) : List (Entry Key Payload) :=
  updates.foldl (fun entries update => (record entries update.1 update.2).1) []

/-- The updates that can start an entry: everything except an update to a tool call that
is already shown. -/
@[expose] def structural (classes : List (EntryClass Key)) : List (EntryClass Key) :=
  classes.foldl (fun kept entryClass =>
    if entryClass.isToolCall && decide (entryClass ∈ kept) then kept else kept ++ [entryClass]) []

/-- Removes adjacent repeats. -/
@[expose] def collapse {Item : Type} [DecidableEq Item] (items : List Item) : List Item :=
  items.foldl (fun kept item => if kept.getLast? = some item then kept else kept ++ [item]) []

/-- The payloads shown under one class, in entry order. -/
@[expose] def shownUnder (entryClass : EntryClass Key) (entries : List (Entry Key Payload)) :
    List Payload :=
  (entries.filter (·.entryClass = entryClass)).flatMap (·.payloads)

/-- Induction from the end of a list: transcripts grow one update at a time. -/
@[elab_as_elim] theorem snocInduction {Item : Type} {motive : List Item → Prop}
    (nil : motive []) (snoc : ∀ items item, motive items → motive (items ++ [item]))
    (items : List Item) : motive items := by
  rw [← List.reverse_reverse items]
  induction items.reverse with
  | nil => exact nil
  | cons item rest ih =>
    rw [List.reverse_cons]
    exact snoc _ _ ih

theorem collapse_snoc {Item : Type} [DecidableEq Item] (items : List Item) (item : Item) :
    collapse (items ++ [item]) =
      if (collapse items).getLast? = some item then collapse items
      else collapse items ++ [item] := by
  unfold collapse
  rw [List.foldl_append]
  rfl

theorem structural_snoc (classes : List (EntryClass Key)) (entryClass : EntryClass Key) :
    structural (classes ++ [entryClass]) =
      if entryClass.isToolCall && decide (entryClass ∈ structural classes) then
        structural classes
      else structural classes ++ [entryClass] := by
  unfold structural
  rw [List.foldl_append]
  rfl

theorem transcript_snoc (updates : List (EntryClass Key × Payload))
    (update : EntryClass Key × Payload) :
    transcript (updates ++ [update]) = (record (transcript updates) update.1 update.2).1 := by
  unfold transcript
  rw [List.foldl_append]
  rfl

theorem mem_collapse {Item : Type} [DecidableEq Item] (items : List Item) (item : Item) :
    item ∈ collapse items ↔ item ∈ items := by
  induction items using snocInduction with
  | nil => simp [collapse]
  | snoc items last ih =>
    rw [collapse_snoc]
    split
    next lastIs =>
      rw [ih]
      simp only [List.mem_append, List.mem_singleton]
      constructor
      · exact Or.inl
      · rintro (h | rfl)
        · exact h
        · exact ih.mp (List.mem_of_getLast? lastIs)
    next _ => simp [ih]

theorem collapse_sublist {Item : Type} [DecidableEq Item] (items : List Item) :
    collapse items <+ items := by
  induction items using snocInduction with
  | nil => simp [collapse]
  | snoc items last ih =>
    rw [collapse_snoc]
    split
    · exact ih.trans (List.sublist_append_left _ _)
    · exact ih.append (List.Sublist.refl [last])

theorem structural_toolCall_count (classes : List (EntryClass Key)) (entryClass : EntryClass Key)
    (isTool : entryClass.isToolCall = true) : (structural classes).count entryClass ≤ 1 := by
  induction classes using snocInduction with
  | nil => simp [structural]
  | snoc classes last ih =>
    rw [structural_snoc]
    split
    · exact ih
    next notKept =>
      simp only [List.count_append, List.count_singleton]
      by_cases same : last = entryClass
      · subst same
        simp only [isTool, Bool.true_and, decide_eq_true_eq] at notKept
        simp [List.count_eq_zero_of_not_mem notKept]
      · simp only [beq_iff_eq, same, ↓reduceIte, Nat.add_zero]
        exact ih

theorem findClass_spec (entryClass : EntryClass Key) (entries : List (Entry Key Payload)) :
    match findClass entryClass entries with
    | some index => ∃ inBounds : index < entries.length,
        entries[index].entryClass = entryClass
    | none => entryClass ∉ entries.map (·.entryClass) := by
  induction entries with
  | nil => simp [findClass]
  | cons entry rest ih =>
    by_cases same : entry.entryClass = entryClass
    · have value : findClass entryClass (entry :: rest) = some 0 := by
        unfold findClass
        exact ite_eq_left same
      rw [value]
      exact ⟨by simp, same⟩
    · cases found : findClass entryClass rest with
      | none =>
        have value : findClass entryClass (entry :: rest) = none := by
          unfold findClass
          rw [ite_eq_right same, found]
          rfl
        rw [value]
        rw [found] at ih
        simp only [List.map_cons, List.mem_cons, not_or]
        exact ⟨fun h => same h.symm, ih⟩
      | some index =>
        have value : findClass entryClass (entry :: rest) = some (index + 1) := by
          unfold findClass
          rw [ite_eq_right same, found]
          rfl
        rw [value]
        rw [found] at ih
        obtain ⟨inBounds, atIndex⟩ := ih
        exact ⟨by simp; omega, by simpa using atIndex⟩

omit [DecidableEq Key] in
theorem appendAt_classes (payload : Payload) (index : Nat) (entries : List (Entry Key Payload)) :
    (appendAt payload index entries).map (·.entryClass) = entries.map (·.entryClass) := by
  induction entries generalizing index with
  | nil => simp [appendAt]
  | cons entry rest ih =>
    cases index with
    | zero => simp [appendAt]
    | succ index => simp [appendAt, ih]

theorem entryFor_spec (entries : List (Entry Key Payload)) (entryClass : EntryClass Key) :
    match entryFor entries entryClass with
    | some index => ∃ inBounds : index < entries.length, entries[index].entryClass = entryClass
    | none =>
        (entryClass.isToolCall = true → entryClass ∉ entries.map (·.entryClass)) ∧
        (entryClass.isToolCall = false →
          (entries.map (·.entryClass)).getLast? ≠ some entryClass) := by
  cases isTool : entryClass.isToolCall
  · have notTool : ¬ entryClass.isToolCall = true := by simp [isTool]
    have value : entryFor entries entryClass =
        match entries.getLast? with
        | some last => if last.entryClass = entryClass then some (entries.length - 1) else none
        | none => none := by
      unfold entryFor
      exact ite_eq_right notTool
    cases lastIs : entries.getLast? with
    | none =>
      rw [lastIs] at value
      rw [value]
      exact ⟨fun h => absurd h Bool.false_ne_true, fun _ => by rw [List.getLast?_map, lastIs]; simp⟩
    | some last =>
      rw [lastIs] at value
      by_cases same : last.entryClass = entryClass
      · rw [value]
        dsimp only
        rw [ite_eq_left same]
        have nonempty : 0 < entries.length :=
          List.length_pos_iff.mpr (List.ne_nil_of_mem (List.mem_of_getLast? lastIs))
        refine ⟨by omega, ?_⟩
        have atLast := lastIs
        rw [List.getLast?_eq_getElem?, List.getElem?_eq_some_iff] at atLast
        obtain ⟨_, isLast⟩ := atLast
        rw [isLast]
        exact same
      · rw [value]
        dsimp only
        rw [ite_eq_right same]
        exact ⟨fun h => absurd h Bool.false_ne_true,
          fun _ => by rw [List.getLast?_map, lastIs]; simpa using same⟩
  · have value : entryFor entries entryClass = findClass entryClass entries := by
      unfold entryFor
      exact ite_eq_left isTool
    rw [value]
    have spec := findClass_spec entryClass entries
    cases found : findClass entryClass entries with
    | none =>
      rw [found] at spec
      exact ⟨fun _ => spec, fun h => absurd h (by simp)⟩
    | some index =>
      rw [found] at spec
      exact spec

/-- For a message chunk or plan, `entryFor` only ever names the last entry. -/
theorem entryFor_index_last (entries : List (Entry Key Payload)) (entryClass : EntryClass Key)
    (index : Nat) (notTool : entryClass.isToolCall = false)
    (found : entryFor entries entryClass = some index) : index = entries.length - 1 := by
  unfold entryFor at found
  simp only [notTool, Bool.false_eq_true, ↓reduceIte] at found
  split at found
  · split at found
    · exact (Option.some.inj found).symm
    · simp at found
  · simp at found

/-- The entry classes, in order, are the structural updates' classes with adjacent repeats
removed: one entry per run, in the order things happened. -/
theorem transcript_classes (updates : List (EntryClass Key × Payload)) :
    (transcript updates).map (·.entryClass) = collapse (structural (updates.map (·.1))) := by
  induction updates using snocInduction with
  | nil => simp [transcript, structural, collapse]
  | snoc updates update ih =>
    rw [transcript_snoc, List.map_append, List.map_singleton, structural_snoc]
    have spec := entryFor_spec (transcript updates) update.1
    unfold record
    cases found : entryFor (transcript updates) update.1 with
    | some index =>
      rw [found] at spec
      obtain ⟨inBounds, same⟩ := spec
      simp only [appendAt_classes, ih]
      have shown : update.1 ∈ collapse (structural (updates.map (·.1))) := by
        rw [← ih]
        exact List.mem_map.mpr ⟨_, List.getElem_mem inBounds, same⟩
      cases isTool : update.1.isToolCall
      · have isLast := entryFor_index_last (transcript updates) update.1 index isTool found
        have lastIs : (collapse (structural (updates.map (·.1)))).getLast? = some update.1 := by
          rw [← ih, List.getLast?_eq_getElem?, List.length_map, List.getElem?_map, ← isLast,
            List.getElem?_eq_getElem inBounds, Option.map_some, same]
        simp [collapse_snoc, lastIs]
      · simp [(mem_collapse _ _).mp shown]
    | none =>
      rw [found] at spec
      simp only [List.map_append, List.map_singleton, ih]
      cases isTool : update.1.isToolCall
      · have notLast := spec.2 isTool
        rw [ih] at notLast
        simp [collapse_snoc, notLast]
      · have notShown := spec.1 isTool
        rw [ih, mem_collapse] at notShown
        have notLast : (collapse (structural (updates.map (·.1)))).getLast? ≠ some update.1 := by
          intro lastIs
          exact notShown ((mem_collapse _ _).mp (List.mem_of_getLast? lastIs))
        simp [notShown, collapse_snoc, notLast]

theorem getLast?_collapse {Item : Type} [DecidableEq Item] (items : List Item) :
    (collapse items).getLast? = items.getLast? := by
  induction items using snocInduction with
  | nil => simp [collapse]
  | snoc items last ih =>
    rw [collapse_snoc]
    split
    next lastIs => simp [← ih, lastIs]
    next _ => simp

theorem structural_sublist (classes : List (EntryClass Key)) : structural classes <+ classes := by
  induction classes using snocInduction with
  | nil => simp [structural]
  | snoc classes last ih =>
    rw [structural_snoc]
    split
    · exact ih.trans (List.sublist_append_left _ _)
    · exact ih.append (List.Sublist.refl [last])

/-- A tool call is shown in one entry only. -/
theorem transcript_toolCall_unique (updates : List (EntryClass Key × Payload))
    (entryClass : EntryClass Key) (isTool : entryClass.isToolCall = true) :
    ((transcript updates).map (·.entryClass)).count entryClass ≤ 1 := by
  rw [transcript_classes]
  exact Nat.le_trans ((collapse_sublist _).count_le entryClass)
    (structural_toolCall_count _ entryClass isTool)

theorem shownUnder_nil_of_absent (entryClass : EntryClass Key) (entries : List (Entry Key Payload))
    (absent : ∀ entry ∈ entries, entry.entryClass ≠ entryClass) :
    shownUnder entryClass entries = [] := by
  simp only [shownUnder, List.flatMap_eq_nil_iff, List.mem_filter, decide_eq_true_eq, and_imp]
  intro entry member same
  exact absurd same (absent entry member)

theorem shownUnder_cons (entryClass : EntryClass Key) (entry : Entry Key Payload)
    (entries : List (Entry Key Payload)) :
    shownUnder entryClass (entry :: entries) =
      (if entry.entryClass = entryClass then entry.payloads else []) ++
        shownUnder entryClass entries := by
  simp only [shownUnder, List.filter_cons, decide_eq_true_eq]
  split <;> simp

theorem shownUnder_append (entryClass : EntryClass Key) (first second : List (Entry Key Payload)) :
    shownUnder entryClass (first ++ second) =
      shownUnder entryClass first ++ shownUnder entryClass second := by
  simp [shownUnder, List.filter_append, List.flatMap_append]

/-- Appending to the last entry of a class appends to what that class shows. -/
theorem shownUnder_appendAt_same (payload : Payload) (entryClass : EntryClass Key) :
    ∀ (index : Nat) (entries : List (Entry Key Payload)) (inBounds : index < entries.length),
      entries[index].entryClass = entryClass →
      (∀ entry ∈ entries.drop (index + 1), entry.entryClass ≠ entryClass) →
      shownUnder entryClass (appendAt payload index entries) =
        shownUnder entryClass entries ++ [payload]
  | 0, entry :: rest, _, atIndex, noneLater => by
    simp only [List.getElem_cons_zero] at atIndex
    simp only [List.drop_succ_cons, List.drop_zero] at noneLater
    simp [appendAt, shownUnder_cons, atIndex, shownUnder_nil_of_absent _ _ noneLater]
  | index + 1, entry :: rest, inBounds, atIndex, noneLater => by
    simp only [appendAt, shownUnder_cons, List.append_assoc]
    rw [shownUnder_appendAt_same payload entryClass index rest (by simpa using inBounds)
      (by simpa using atIndex) (by simpa using noneLater)]

/-- Appending to an entry of another class changes nothing this class shows. -/
theorem shownUnder_appendAt_other (payload : Payload) (entryClass : EntryClass Key) :
    ∀ (index : Nat) (entries : List (Entry Key Payload)) (inBounds : index < entries.length),
      entries[index].entryClass ≠ entryClass →
      shownUnder entryClass (appendAt payload index entries) = shownUnder entryClass entries
  | 0, entry :: rest, _, atIndex => by
    simp only [List.getElem_cons_zero] at atIndex
    simp [appendAt, shownUnder_cons, atIndex]
  | index + 1, entry :: rest, inBounds, atIndex => by
    simp only [appendAt, shownUnder_cons]
    rw [shownUnder_appendAt_other payload entryClass index rest (by simpa using inBounds)
      (by simpa using atIndex)]

theorem absent_after_of_count (entries : List (Entry Key Payload)) (entryClass : EntryClass Key)
    (index : Nat) (inBounds : index < entries.length)
    (atIndex : entries[index].entryClass = entryClass)
    (once : (entries.map (·.entryClass)).count entryClass ≤ 1) :
    ∀ entry ∈ entries.drop (index + 1), entry.entryClass ≠ entryClass := by
  intro entry later same
  have parts := List.take_append_drop (index + 1) (entries.map (·.entryClass))
  have early : entryClass ∈ (entries.map (·.entryClass)).take (index + 1) := by
    rw [List.mem_iff_getElem]
    exact ⟨index, by simp; omega, by simp [atIndex]⟩
  have lateMember : entryClass ∈ (entries.map (·.entryClass)).drop (index + 1) := by
    rw [← List.map_drop]
    exact List.mem_map.mpr ⟨entry, later, same⟩
  have := List.count_pos_iff.mpr early
  have := List.count_pos_iff.mpr lateMember
  rw [← parts, List.count_append] at once
  omega

/-- Every update is shown, under its own class, in the order it arrived. -/
theorem transcript_content (updates : List (EntryClass Key × Payload))
    (entryClass : EntryClass Key) :
    shownUnder entryClass (transcript updates) =
      (updates.filter (·.1 = entryClass)).map (·.2) := by
  induction updates using snocInduction with
  | nil => simp [transcript, shownUnder]
  | snoc updates update ih =>
    rw [transcript_snoc, List.filter_append, List.map_append, ← ih]
    have spec := entryFor_spec (transcript updates) update.1
    unfold record
    cases found : entryFor (transcript updates) update.1 with
    | some index =>
      rw [found] at spec
      obtain ⟨inBounds, atIndex⟩ := spec
      simp only
      by_cases same : update.1 = entryClass
      · subst same
        have noneLater : ∀ entry ∈ (transcript updates).drop (index + 1),
            entry.entryClass ≠ update.1 := by
          cases isTool : update.1.isToolCall
          · have isLast := entryFor_index_last (transcript updates) update.1 index isTool found
            rw [isLast, Nat.sub_add_cancel (by omega), List.drop_length]
            simp
          · exact absent_after_of_count _ _ index inBounds atIndex
              (transcript_toolCall_unique updates update.1 isTool)
        rw [shownUnder_appendAt_same update.2 update.1 index _ inBounds atIndex noneLater]
        simp
      · rw [shownUnder_appendAt_other _ entryClass index _ inBounds (atIndex ▸ same)]
        simp [same]
    | none =>
      simp only
      rw [shownUnder_append, shownUnder_cons]
      by_cases same : update.1 = entryClass
      · simp [shownUnder, same]
      · simp [shownUnder, same]

/-- Keeps the (class, entry) pairs of structural updates. -/
@[expose] def structuralPairs (pairs : List (EntryClass Key × Nat)) :
    List (EntryClass Key × Nat) :=
  pairs.foldl (fun kept pair =>
    if pair.1.isToolCall && decide (pair.1 ∈ kept.map (·.1)) then kept else kept ++ [pair]) []

theorem structuralPairs_snoc (pairs : List (EntryClass Key × Nat)) (pair : EntryClass Key × Nat) :
    structuralPairs (pairs ++ [pair]) =
      if pair.1.isToolCall && decide (pair.1 ∈ (structuralPairs pairs).map (·.1)) then
        structuralPairs pairs
      else structuralPairs pairs ++ [pair] := by
  unfold structuralPairs
  rw [List.foldl_append]
  rfl

theorem structuralPairs_classes (pairs : List (EntryClass Key × Nat)) :
    (structuralPairs pairs).map (·.1) = structural (pairs.map (·.1)) := by
  induction pairs using snocInduction with
  | nil => simp [structuralPairs, structural]
  | snoc pairs pair ih =>
    rw [structuralPairs_snoc, List.map_append, List.map_singleton, structural_snoc, ← ih]
    split <;> simp

theorem structuralPairs_sublist (pairs : List (EntryClass Key × Nat)) :
    structuralPairs pairs <+ pairs := by
  induction pairs using snocInduction with
  | nil => simp [structuralPairs]
  | snoc pairs pair ih =>
    rw [structuralPairs_snoc]
    split
    · exact ih.trans (List.sublist_append_left _ _)
    · exact ih.append (List.Sublist.refl [pair])

/-- An alternative transcript: `classes[i]` is what its entry `i` shows and `assignment[k]`
is the entry update `k` goes to. It is valid when every entry shows one thing (each update
goes to an entry of its own class) and entries are shown in the order things happened (the
entries of structural updates never go backwards). An update to a tool call already shown
may go to any entry of that call. -/
@[expose] def ValidGrouping (updates : List (EntryClass Key × Payload))
    (classes : List (EntryClass Key)) (assignment : List Nat) : Prop :=
  assignment.length = updates.length ∧
  (∀ pair ∈ (updates.map (·.1)).zip assignment, classes[pair.2]? = some pair.1) ∧
  ((structuralPairs ((updates.map (·.1)).zip assignment)).map (·.2)).Pairwise (· ≤ ·)

/-- Runs of an ordered, homogeneous assignment need distinct, increasing entries. -/
theorem collapse_length_le_last (classes : List (EntryClass Key)) :
    ∀ (pairs : List (EntryClass Key × Nat)) (last : EntryClass Key × Nat),
      (∀ pair ∈ pairs ++ [last], classes[pair.2]? = some pair.1) →
      ((pairs ++ [last]).map (·.2)).Pairwise (· ≤ ·) →
      (collapse ((pairs ++ [last]).map (·.1))).length ≤ last.2 + 1 := by
  intro pairs
  induction pairs using snocInduction with
  | nil => intro last _ _; simp [collapse]
  | snoc pairs previous ih =>
    intro last homogeneous ordered
    have orderedPrefix : ((pairs ++ [previous]).map (·.2)).Pairwise (· ≤ ·) := by
      rw [List.map_append, List.pairwise_append] at ordered
      exact ordered.1
    have previousBelow : previous.2 ≤ last.2 := by
      rw [List.map_append, List.pairwise_append] at ordered
      exact ordered.2.2 previous.2 (by simp) last.2 (by simp)
    have before := ih previous
      (fun pair member => homogeneous pair (List.mem_append_left [last] member)) orderedPrefix
    rw [List.map_append, List.map_singleton, collapse_snoc]
    split
    · omega
    next different =>
      rw [List.length_append, List.length_singleton]
      have differentClass : previous.1 ≠ last.1 := by
        intro same
        apply different
        rw [getLast?_collapse]
        simp [same]
      have differentEntry : previous.2 ≠ last.2 := by
        intro same
        have atPrevious := homogeneous previous (by simp)
        have atLast := homogeneous last (by simp)
        rw [same, atLast] at atPrevious
        exact differentClass (Option.some.inj atPrevious).symm
      omega

theorem grouping_length_le (classes : List (EntryClass Key))
    (pairs : List (EntryClass Key × Nat))
    (homogeneous : ∀ pair ∈ pairs, classes[pair.2]? = some pair.1)
    (ordered : (pairs.map (·.2)).Pairwise (· ≤ ·)) :
    (collapse (pairs.map (·.1))).length ≤ classes.length := by
  induction pairs using snocInduction with
  | nil => simp [collapse]
  | snoc pairs last _ =>
    have := collapse_length_le_last classes pairs last homogeneous ordered
    have inBounds := homogeneous last (by simp)
    have : last.2 < classes.length := by
      rw [List.getElem?_eq_some_iff] at inBounds
      exact inBounds.1
    omega

/-- Cost model: entries shown. Class of alternatives: every valid grouping
(`ValidGrouping`). None shows fewer entries. -/
theorem transcript_optimal (updates : List (EntryClass Key × Payload))
    (classes : List (EntryClass Key)) (assignment : List Nat)
    (valid : ValidGrouping updates classes assignment) :
    (transcript updates).length ≤ classes.length := by
  obtain ⟨sameLength, homogeneous, ordered⟩ := valid
  have firsts : ((updates.map (·.1)).zip assignment).map (·.1) = updates.map (·.1) :=
    List.map_fst_zip (by simp [sameLength])
  have lengthIs : (transcript updates).length =
      (collapse ((structuralPairs ((updates.map (·.1)).zip assignment)).map (·.1))).length := by
    rw [structuralPairs_classes, firsts, ← transcript_classes, List.length_map]
  rw [lengthIs]
  exact grouping_length_le classes _
    (fun pair member => homogeneous pair ((structuralPairs_sublist _).subset member)) ordered

/-- Class comparisons to record one update: a tool call update scans for its call, anything
else looks at the last entry only. -/
@[expose] def recordComparisons (entries : List (Entry Key Payload))
    (entryClass : EntryClass Key) : Nat :=
  if entryClass.isToolCall then
    match findClass entryClass entries with
    | some index => index + 1
    | none => entries.length
  else 1

theorem findClass_lt (entryClass : EntryClass Key) (entries : List (Entry Key Payload))
    (index : Nat) (found : findClass entryClass entries = some index) :
    index < entries.length := by
  have spec := findClass_spec entryClass entries
  rw [found] at spec
  exact spec.1

/-- Cost model: entries kept, and class comparisons per recorded update. A transcript never
has more entries than updates, and recording one update compares each entry at most once. -/
theorem transcript_cost (updates : List (EntryClass Key × Payload))
    (entries : List (Entry Key Payload)) (entryClass : EntryClass Key) :
    (transcript updates).length ≤ updates.length ∧
    recordComparisons entries entryClass ≤ entries.length + 1 := by
  refine ⟨?_, ?_⟩
  · rw [← List.length_map (f := (·.entryClass)), transcript_classes]
    have := (collapse_sublist (structural (updates.map (·.1)))).length_le
    have := (structural_sublist (updates.map (·.1))).length_le
    simp only [List.length_map] at *
    omega
  · unfold recordComparisons
    split
    · split
      case h_1 index found => have := findClass_lt entryClass entries index found; omega
      · omega
    · omega

end Transcript

/-! ## The feature -/

/-- **correct.** SPEC.md §3.2: `session/new` runs in the worktree with AIRC and every
shared MCP server; `fs` and `terminal` are advertised; the bypass mode is selected whenever
offered; relative paths resolve against the worktree, absolute paths are used as given and
the result on an absolute worktree is always absolute; terminal output keeps at most the
limit from a character boundary and everything that fits; every `session/update` is shown,
under its own class and in order, one tool call per entry and one entry per run. -/
theorem correct :
    (∀ (Name Server : Type) [DecidableEq Name] (name : Server → Name) (airc : Server)
        (shared : List Server),
      (∃ rest, sessionServers name airc shared = airc :: rest) ∧
      (∀ server ∈ airc :: shared, name server ∈ (sessionServers name airc shared).map name) ∧
      ((sessionServers name airc shared).map name).Nodup ∧
      sessionServers name airc shared <+ airc :: shared) ∧
    (∀ (Path Server : Type) (worktree : Path) (servers : List Server)
        (bypass current : Option Nat),
      (startRequests worktree servers bypass current).head? =
        some (.initialization true true true) ∧
      (startRequests worktree servers bypass current)[1]? =
        some (.newSession worktree servers) ∧
      StartsSession bypass current (startRequests worktree servers bypass current)) ∧
    (∀ worktree requested : Bytes,
      Resolves worktree requested (resolvePath worktree requested) ∧
      (shape requested = .absolute → resolvePath worktree requested = requested) ∧
      (shape requested = .relative ∨ shape requested = .empty →
        worktree <+: resolvePath worktree requested) ∧
      (shape worktree = .absolute → shape (resolvePath worktree requested) = .absolute)) ∧
    (∀ (output : Bytes) (limit : Nat),
      ValidStart output limit (retainedStart output limit) ∧
      (output.length ≤ limit → retainedStart output limit = 0)) ∧
    (∀ (Key Payload : Type) [DecidableEq Key] (updates : List (EntryClass Key × Payload)),
      (transcript updates).map (·.entryClass) = collapse (structural (updates.map (·.1))) ∧
      (∀ entryClass, shownUnder entryClass (transcript updates) =
        (updates.filter (·.1 = entryClass)).map (·.2)) ∧
      (∀ entryClass, entryClass.isToolCall = true →
        ((transcript updates).map (·.entryClass)).count entryClass ≤ 1)) :=
  ⟨fun _ _ _ name airc shared => sessionServers_correct name airc shared,
   fun _ _ worktree servers bypass current =>
     startRequests_correct worktree servers bypass current,
   fun worktree requested => ⟨resolvePath_resolves worktree requested,
     resolvePath_not_confined worktree requested, resolvePath_rooted worktree requested,
     resolvePath_absolute worktree requested⟩,
   fun output limit => retainedStart_correct output limit,
   fun _ _ _ updates => ⟨transcript_classes updates, transcript_content updates,
     transcript_toolCall_unique updates⟩⟩

/-- **cost_bound.** Cost models: requests sent to start a session (at most 3); bytes in a
resolved path (at most the worktree, the request and 3); bytes inspected past the cut when
truncating terminal output (at most the limit); transcript entries (at most the updates) and
class comparisons per recorded update (at most the entries and 1). -/
theorem cost_bound :
    (∀ (Path Server : Type) (worktree : Path) (servers : List Server)
        (bypass current : Option Nat),
      (startRequests worktree servers bypass current).length ≤ 3) ∧
    (∀ worktree requested : Bytes,
      (resolvePath worktree requested).length ≤ worktree.length + requested.length + 3) ∧
    (∀ (output : Bytes) (limit : Nat),
      retainedStart output limit - (output.length - limit) ≤ limit) ∧
    (∀ (Key Payload : Type) [DecidableEq Key] (updates : List (EntryClass Key × Payload))
        (entries : List (Entry Key Payload)) (entryClass : EntryClass Key),
      (transcript updates).length ≤ updates.length ∧
      recordComparisons entries entryClass ≤ entries.length + 1) :=
  ⟨fun _ _ worktree servers bypass current =>
     (startRequests_cost worktree servers bypass current).2,
   resolvePath_cost, retainedStart_cost,
   fun _ _ _ updates entries entryClass => transcript_cost updates entries entryClass⟩

/-- **optimal.** For each part, its cost model and class of alternatives:
- servers passed, over every list holding a server of each required name;
- requests sent, over every request sequence satisfying `StartsSession`;
- bytes in the resolved path, over every resolver meeting `Resolves`;
- bytes of terminal output dropped, over every `ValidStart`;
- transcript entries, over every `ValidGrouping`.
In each, no alternative costs less. -/
theorem optimal :
    (∀ (Name Server : Type) [DecidableEq Name] (name : Server → Name) (airc : Server)
        (shared alternative : List Server),
      (∀ server ∈ airc :: shared, name server ∈ alternative.map name) →
      (sessionServers name airc shared).length ≤ alternative.length) ∧
    (∀ (Path Server : Type) (worktree : Path) (servers : List Server)
        (bypass current : Option Nat) (alternative : List (StartRequest Path Server)),
      StartsSession bypass current alternative →
      (startRequests worktree servers bypass current).length ≤ alternative.length) ∧
    (∀ worktree requested alternative : Bytes,
      Resolves worktree requested alternative →
      (resolvePath worktree requested).length ≤ alternative.length) ∧
    (∀ (output : Bytes) (limit start : Nat),
      ValidStart output limit start → retainedStart output limit ≤ start) ∧
    (∀ (Key Payload : Type) [DecidableEq Key] (updates : List (EntryClass Key × Payload))
        (classes : List (EntryClass Key)) (assignment : List Nat),
      ValidGrouping updates classes assignment →
      (transcript updates).length ≤ classes.length) :=
  ⟨fun _ _ _ name airc shared alternative =>
     sessionServers_optimal name airc shared alternative,
   fun _ _ worktree servers bypass current alternative =>
     startRequests_optimal worktree servers bypass current alternative,
   resolvePath_optimal, retainedStart_optimal,
   fun _ _ _ updates classes assignment => transcript_optimal updates classes assignment⟩

#print axioms sessionServers_correct
#print axioms sessionServers_optimal
#print axioms startRequests_correct
#print axioms startRequests_cost
#print axioms startRequests_optimal
#print axioms resolvePath_resolves
#print axioms resolves_unique
#print axioms resolvePath_absolute
#print axioms resolvePath_rooted
#print axioms resolvePath_not_confined
#print axioms resolvePath_cost
#print axioms resolvePath_optimal
#print axioms retainedStart_correct
#print axioms retainedStart_cost
#print axioms retainedStart_optimal
#print axioms transcript_classes
#print axioms transcript_content
#print axioms transcript_toolCall_unique
#print axioms transcript_cost
#print axioms transcript_optimal
#print axioms correct
#print axioms cost_bound
#print axioms optimal

end Velocity.AcpClient
