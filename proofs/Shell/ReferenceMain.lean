module

import Lean.Data.Json
import Shell.Appearance

public def main : IO Unit := do
  let style := Velocity.Shell.Appearance.referenceStyle .close .hover
  IO.println <| Lean.Json.compress <| Lean.Json.mkObj [
    ("background", Lean.toJson style.background),
    ("color", Lean.toJson style.color),
    ("width", Lean.toJson style.width)]
