/-
  sudo Lean 4 runtime for the external protocol-4 backend.

  Toolchain: Lean 4.14.x (no Mathlib). Generated programs import this
  module, or the emitter inlines it into `{entry}_test.lean` so
  `lean --run` does not need Lake.

  Maps and Sets are sorted association arrays. Iteration is key-ascending
  under the structural order in `SOrd`. sudo leaves Map/Set order
  unspecified (language.md §12); this backend picks a deterministic
  representation so TAP / KAT output is stable. Observable tests must
  not depend on iteration order.

  Every trapping operation lives in `SudoM` (`EStateM Trap Unit`), which
  is total. Mid-body traps preserve `let mut` state so `expect_trap`
  sees assignments that ran before the fault.
-/
set_option linter.unusedVariables false

namespace SudoRt

structure Trap where
  kind : String
  detail : String := ""
  deriving Inhabited, Repr, BEq

abbrev SudoM := EStateM Trap Unit

@[inline] def trap {α} (kind : String) (detail : String := "") : SudoM α :=
  throw { kind, detail }

@[inline] def trapK {α} (kind : String) : SudoM α :=
  trap kind ""

def expectTrap (kind : String) (line : Nat) (body : SudoM Unit) : SudoM Unit :=
  tryCatch
    (do
      body
      trap "AssertFailed" s!"line {line}: expected {kind}")
    (fun t =>
      if t.kind == kind then pure () else throw t)

-- ---- i64 arithmetic -------------------------------------------------------

def i64Min : Int := -9223372036854775808
def i64Max : Int := 9223372036854775807

def chk (n : Int) : SudoM Int :=
  if n < i64Min || n > i64Max then trapK "Overflow" else pure n

def addI (a b : Int) : SudoM Int := chk (a + b)
def subI (a b : Int) : SudoM Int := chk (a - b)
def mulI (a b : Int) : SudoM Int := chk (a * b)
def negI (a : Int) : SudoM Int := chk (-a)

def absI (a : Int) : SudoM Int :=
  if a == i64Min then trapK "Overflow"
  else if a < 0 then pure (-a)
  else pure a

/-- Floor division (sign of the divisor), matching sudo / Python `//`. -/
def divI (a b : Int) : SudoM Int := do
  if b == 0 then trapK "DivByZero"
  else if a == i64Min && b == (-1) then trapK "Overflow"
  else
    let q := a / b
    let r := a % b
    if r == 0 then chk q
    else if (a < 0 && b < 0) || (0 ≤ a && 0 ≤ b) then chk q
    else chk (q - 1)

/-- Floor modulo (sign of the divisor), matching sudo / Python `%`. -/
def modI (a b : Int) : SudoM Int := do
  if b == 0 then trapK "DivByZero"
  else
    let r := a % b
    if r == 0 then pure r
    else if (a < 0 && b < 0) || (0 ≤ a && 0 ≤ b) then pure r
    else pure (r + b)

def minI (a b : Int) : Int := if a <= b then a else b
def maxI (a b : Int) : Int := if a >= b then a else b

-- ---- floats --------------------------------------------------------------

def fdiv (a b : Float) : Float :=
  if b == 0.0 then
    if a == 0.0 || a.isNaN then 0.0 / 0.0
    else
      let sa : Float := if a == 0.0 && (1.0 / a) < 0.0 || a < 0.0 then -1.0 else 1.0
      let sb : Float := if b == 0.0 && (1.0 / b) < 0.0 || b < 0.0 then -1.0 else 1.0
      sa * sb * (1.0 / 0.0)
  else a / b

def fsign (x : Float) : Float :=
  if x == 0.0 then
    if (1.0 / x) < 0.0 then -1.0 else 1.0
  else if x < 0.0 then -1.0
  else 1.0

def fmin (a b : Float) : Float :=
  if a.isNaN || b.isNaN then 0.0 / 0.0
  else if a == b then
    if fsign a < fsign b then a else b
  else if a < b then a else b

def fmax (a b : Float) : Float :=
  if a.isNaN || b.isNaN then 0.0 / 0.0
  else if a == b then
    if fsign a > fsign b then a else b
  else if a > b then a else b

def ffloor (x : Float) : Float :=
  if x.isNaN || x.isInf then x
  else
    let t := x.floor
    if x == 0.0 && (1.0 / x) < 0.0 then -0.0 else t

def fceil (x : Float) : Float :=
  if x.isNaN || x.isInf then x
  else
    let t := x.ceil
    if t == 0.0 && x < 0.0 then -0.0 else t

/-- Ties away from zero (language.md §4.3), not bankers' rounding. -/
def fround (x : Float) : Float :=
  if x.isNaN || x.isInf then x
  else if x == 0.0 then x
  else
    let t := if x >= 0.0 then x.floor else x.ceil
    let d := Float.abs (x - t)
    if d < 0.5 then
      if t == 0.0 && x < 0.0 then -0.0 else t
    else t + fsign x

def fsqrt (x : Float) : Float :=
  if x.isNaN || x < 0.0 then 0.0 / 0.0 else x.sqrt

def fabs (x : Float) : Float :=
  if x.isNaN then x
  else if x == 0.0 then 0.0
  else Float.abs x

def floatOfInt (n : Int) : Float := Float.ofInt n

def floatTruncToInt (x : Float) : Int :=
  if x < 0.0 then
    -Int.ofNat ((-x).toUInt64.toNat)
  else
    Int.ofNat (x.toUInt64.toNat)

def intOfFloat (x : Float) : SudoM Int := do
  if x.isNaN || x.isInf then
    trap "InvalidConvert" "NaN or infinity to int"
  else
    let minF : Float := -9223372036854775808.0
    let maxP1 : Float := 9223372036854775808.0
    if x < minF || x >= maxP1 then
      trap "InvalidConvert" "float out of int range"
    else
      pure (floatTruncToInt x)

def feq (a b : Float) : Bool :=
  !(a.isNaN || b.isNaN) && a == b

-- ---- structural classes --------------------------------------------------

class SEq (α : Type) where
  eq : α → α → Bool

class SOrd (α : Type) where
  cmp : α → α → Ordering

class Canon (α : Type) where
  canon : α → String

export SEq (eq)
export SOrd (cmp)
export Canon (canon)

instance : SEq Int where
  eq a b := a == b

instance : SOrd Int where
  cmp a b := compare a b

instance : Canon Int where
  canon n := toString n

instance : SEq Bool where
  eq a b := a == b

instance : SOrd Bool where
  cmp a b :=
    if a == b then .eq else if a == false then .lt else .gt

instance : Canon Bool where
  canon b := if b then "true" else "false"

def floatCanon (x : Float) : String :=
  let inner :=
    if x.isNaN then "NaN"
    else if x.isInf then if x > 0.0 then "Inf" else "-Inf"
    else toString x
  "{\"f\": \"" ++ inner ++ "\"}"

instance : SEq Float where
  eq := feq

instance : Canon Float where
  canon := floatCanon

instance : SEq Unit where
  eq _ _ := true

instance : SOrd Unit where
  cmp _ _ := .eq

instance : Canon Unit where
  canon _ := "[]"

instance [SEq α] [SEq β] : SEq (α × β) where
  eq a b := SEq.eq a.1 b.1 && SEq.eq a.2 b.2

instance [SOrd α] [SOrd β] : SOrd (α × β) where
  cmp a b :=
    match SOrd.cmp a.1 b.1 with
    | .eq => SOrd.cmp a.2 b.2
    | o => o

instance [Canon α] [Canon β] : Canon (α × β) where
  canon p := "[" ++ Canon.canon p.1 ++ ", " ++ Canon.canon p.2 ++ "]"

instance [SEq α] : SEq (Option α) where
  eq
    | none, none => true
    | some a, some b => SEq.eq a b
    | _, _ => false

instance [SOrd α] : SOrd (Option α) where
  cmp
    | none, none => .eq
    | none, some _ => .lt
    | some _, none => .gt
    | some a, some b => SOrd.cmp a b

instance [Canon α] : Canon (Option α) where
  canon
    | none => "{\"e\": \"Option.None\"}"
    | some v => "{\"e\": \"Option.Some\", \"v\": [" ++ Canon.canon v ++ "]}"

instance [SEq ε] [SEq α] : SEq (Except ε α) where
  eq
    | .ok a, .ok b => SEq.eq a b
    | .error a, .error b => SEq.eq a b
    | _, _ => false

instance [Canon ε] [Canon α] : Canon (Except ε α) where
  canon
    | .ok v => "{\"e\": \"Result.Ok\", \"v\": [" ++ Canon.canon v ++ "]}"
    | .error e => "{\"e\": \"Result.Err\", \"v\": [" ++ Canon.canon e ++ "]}"

def arrGet! {α} [Inhabited α] (a : Array α) (i : Nat) : α :=
  a.getD i default

def arrInsert {α} (xs : Array α) (i : Nat) (v : α) : Array α :=
  xs.extract 0 i ++ #[v] ++ xs.extract i xs.size

def arrErase {α} (xs : Array α) (i : Nat) : Array α :=
  xs.extract 0 i ++ xs.extract (i + 1) xs.size

def arrayEq {α} [SEq α] [Inhabited α] (a b : Array α) : Bool :=
  if a.size != b.size then false
  else
    let rec go (fuel i : Nat) : Bool :=
      match fuel with
      | 0 => true
      | n + 1 =>
        if SEq.eq (arrGet! a i) (arrGet! b i) then go n (i + 1) else false
    go a.size 0

def arrayCmp {α} [SOrd α] [Inhabited α] (a b : Array α) : Ordering :=
  let rec go (fuel i : Nat) : Ordering :=
    match fuel with
    | 0 => compare a.size b.size
    | n + 1 =>
      if i < a.size && i < b.size then
        match SOrd.cmp (arrGet! a i) (arrGet! b i) with
        | .eq => go n (i + 1)
        | o => o
      else compare a.size b.size
  go (Nat.min a.size b.size) 0

def arrayCanon {α} [Canon α] (a : Array α) : String :=
  "[" ++ String.intercalate ", " (a.toList.map Canon.canon) ++ "]"

instance [SEq α] [Inhabited α] : SEq (Array α) where
  eq := arrayEq

instance [SOrd α] [Inhabited α] : SOrd (Array α) where
  cmp := arrayCmp

instance [Canon α] : Canon (Array α) where
  canon := arrayCanon

-- ---- lists ---------------------------------------------------------------

def listGet {α} [Inhabited α] (xs : Array α) (i : Int) : SudoM α :=
  if i < 0 || i >= Int.ofNat xs.size then
    trap "OutOfBounds" s!"index {i} length {xs.size}"
  else
    pure (arrGet! xs i.toNat)

def listSet {α} (xs : Array α) (i : Int) (v : α) : SudoM (Array α) :=
  if i < 0 || i >= Int.ofNat xs.size then
    trap "OutOfBounds" s!"index {i} length {xs.size}"
  else
    pure (xs.setD i.toNat v)

def listAppend {α} (xs : Array α) (v : α) : Array α :=
  xs.push v

def listPop {α} [Inhabited α] (xs : Array α) : SudoM (α × Array α) :=
  if xs.size == 0 then trapK "OutOfBounds"
  else
    let v := arrGet! xs (xs.size - 1)
    pure (v, xs.pop)

def listInsert {α} (xs : Array α) (i : Int) (v : α) : SudoM (Array α) :=
  if i < 0 || i > Int.ofNat xs.size then
    trap "OutOfBounds" s!"insert {i} length {xs.size}"
  else
    pure (arrInsert xs i.toNat v)

def listRemoveAt {α} [Inhabited α] (xs : Array α) (i : Int) : SudoM (α × Array α) :=
  if i < 0 || i >= Int.ofNat xs.size then
    trap "OutOfBounds" s!"index {i} length {xs.size}"
  else
    let v := arrGet! xs i.toNat
    pure (v, arrErase xs i.toNat)

def listSwap {α} (xs : Array α) (i j : Int) : SudoM (Array α) := do
  if i < 0 || i >= Int.ofNat xs.size || j < 0 || j >= Int.ofNat xs.size then
    trap "OutOfBounds" s!"swap {i} {j} length {xs.size}"
  else
    pure (xs.swap! i.toNat j.toNat)

def filled {α} (n : Int) (v : α) : SudoM (Array α) :=
  if n < 0 then trap "InvalidArg" s!"filled({n})"
  else pure (Array.mkArray n.toNat v)

def listConcat {α} (a b : Array α) : Array α :=
  a ++ b

/-- Int / float `a.sort()`: stable; floats put NaN last and `-0.0` before `0.0`. -/
def sortInts (xs : Array Int) : Array Int :=
  xs.toList.mergeSort (· ≤ ·) |>.toArray

def floatSortKey (x : Float) : (Nat × Float × Float) :=
  if x.isNaN then (2, 0.0, 0.0)
  else (1, x, fsign x)

def sortFloats (xs : Array Float) : Array Float :=
  xs.toList.mergeSort (fun a b =>
    let ka := floatSortKey a
    let kb := floatSortKey b
    ka.1 < kb.1 || (ka.1 == kb.1 && (ka.2.1 < kb.2.1 || (ka.2.1 == kb.2.1 && ka.2.2 ≤ kb.2.2)))
  ) |>.toArray

-- ---- maps / sets (sorted association arrays) -----------------------------

structure SMap (κ ν : Type) where
  pairs : Array (κ × ν)
  deriving Inhabited

structure SSet (α : Type) where
  items : Array α
  deriving Inhabited

def mapFind {κ ν} [SOrd κ] [Inhabited κ] [Inhabited ν] (m : SMap κ ν) (k : κ) : Option (Nat × ν) :=
  let rec go (fuel i : Nat) : Option (Nat × ν) :=
    match fuel with
    | 0 => none
    | n + 1 =>
      let (kk, vv) := arrGet! m.pairs i
      match SOrd.cmp kk k with
      | .eq => some (i, vv)
      | .gt => none
      | .lt => go n (i + 1)
  go m.pairs.size 0

def mapGet {κ ν} [SOrd κ] [Inhabited κ] [Inhabited ν] (m : SMap κ ν) (k : κ) : SudoM ν :=
  match mapFind m k with
  | some (_, v) => pure v
  | none => trapK "KeyMissing"

def mapGetOpt {κ ν} [SOrd κ] [Inhabited κ] [Inhabited ν] (m : SMap κ ν) (k : κ) : Option ν :=
  (mapFind m k).map (·.2)

def mapHas {κ ν} [SOrd κ] [Inhabited κ] [Inhabited ν] (m : SMap κ ν) (k : κ) : Bool :=
  (mapFind m k).isSome

def mapInsert {κ ν} [SOrd κ] [Inhabited κ] [Inhabited ν] (m : SMap κ ν) (k : κ) (v : ν) : SMap κ ν :=
  match mapFind m k with
  | some (i, _) => { m with pairs := m.pairs.setD i (k, v) }
  | none =>
    let rec go (fuel i : Nat) : SMap κ ν :=
      match fuel with
      | 0 => { pairs := m.pairs.push (k, v) }
      | n + 1 =>
        let (kk, _) := arrGet! m.pairs i
        if SOrd.cmp k kk == .lt then { pairs := arrInsert m.pairs i (k, v) }
        else go n (i + 1)
    go m.pairs.size 0

def mapDelete {κ ν} [SOrd κ] [Inhabited κ] [Inhabited ν] (m : SMap κ ν) (k : κ) : Bool × SMap κ ν :=
  match mapFind m k with
  | some (i, _) => (true, { pairs := arrErase m.pairs i })
  | none => (false, m)

def mapKeys {κ ν} (m : SMap κ ν) : Array κ :=
  m.pairs.map (·.1)

def mapValues {κ ν} (m : SMap κ ν) : Array ν :=
  m.pairs.map (·.2)

def mapSize {κ ν} (m : SMap κ ν) : Int :=
  Int.ofNat m.pairs.size

def mapEq {κ ν} [SEq κ] [SEq ν] [Inhabited κ] [Inhabited ν] (a b : SMap κ ν) : Bool :=
  arrayEq a.pairs b.pairs

def mapCanon {κ ν} [Canon κ] [Canon ν] (m : SMap κ ν) : String :=
  let pairs := m.pairs.toList.map fun (k, v) =>
    "[" ++ Canon.canon k ++ ", " ++ Canon.canon v ++ "]"
  "{\"m\": [" ++ String.intercalate ", " pairs ++ "]}"

instance [SEq κ] [SEq ν] [Inhabited κ] [Inhabited ν] : SEq (SMap κ ν) where
  eq := mapEq

instance [Canon κ] [Canon ν] : Canon (SMap κ ν) where
  canon := mapCanon

def setFind {α} [SOrd α] [Inhabited α] (s : SSet α) (x : α) : Option Nat :=
  let rec go (fuel i : Nat) : Option Nat :=
    match fuel with
    | 0 => none
    | n + 1 =>
      match SOrd.cmp (arrGet! s.items i) x with
      | .eq => some i
      | .gt => none
      | .lt => go n (i + 1)
  go s.items.size 0

def setHas {α} [SOrd α] [Inhabited α] (s : SSet α) (x : α) : Bool :=
  (setFind s x).isSome

def setAdd {α} [SOrd α] [Inhabited α] (s : SSet α) (x : α) : Bool × SSet α :=
  match setFind s x with
  | some _ => (false, s)
  | none =>
    let rec go (fuel i : Nat) : Bool × SSet α :=
      match fuel with
      | 0 => (true, { items := s.items.push x })
      | n + 1 =>
        if SOrd.cmp x (arrGet! s.items i) == .lt then
          (true, { items := arrInsert s.items i x })
        else go n (i + 1)
    go s.items.size 0

def setRemove {α} [SOrd α] [Inhabited α] (s : SSet α) (x : α) : Bool × SSet α :=
  match setFind s x with
  | some i => (true, { items := arrErase s.items i })
  | none => (false, s)

def setSize {α} (s : SSet α) : Int :=
  Int.ofNat s.items.size

def setItems {α} (s : SSet α) : Array α :=
  s.items

def setEq {α} [SEq α] [Inhabited α] (a b : SSet α) : Bool :=
  arrayEq a.items b.items

def setCanon {α} [Canon α] (s : SSet α) : String :=
  "{\"s\": [" ++ String.intercalate ", " (s.items.toList.map Canon.canon) ++ "]}"

instance [SEq α] [Inhabited α] : SEq (SSet α) where
  eq := setEq

instance [Canon α] : Canon (SSet α) where
  canon := setCanon

-- ---- option / result -----------------------------------------------------

def optUnwrap {α} (o : Option α) : SudoM α :=
  match o with
  | some v => pure v
  | none => trapK "UnwrapFailed"

def optGetOr {α} (o : Option α) (d : α) : α :=
  o.getD d

def resUnwrap {ε α} (r : Except ε α) : SudoM α :=
  match r with
  | .ok v => pure v
  | .error _ => trapK "UnwrapFailed"

def resGetOr {ε α} (r : Except ε α) (d : α) : α :=
  match r with
  | .ok v => v
  | .error _ => d

-- ---- loops (total: remaining-iteration Nat) -------------------------------

inductive Step (σ : Type) (r : Type) where
  | cont : σ → Step σ r
  | brk : σ → Step σ r
  | ret : r → Step σ r

def rangeCount (lo hi : Int) (down : Bool) : Nat :=
  if down then
    if lo < hi then 0 else (lo - hi + 1).toNat
  else
    if lo > hi then 0 else (hi - lo + 1).toNat

def forRange {σ r} (lo hi : Int) (down : Bool)
    (body : Int → σ → SudoM (Step σ r)) (init : σ) : SudoM (Step σ r) :=
  let rec go (fuel : Nat) (i : Int) (s : σ) : SudoM (Step σ r) :=
    match fuel with
    | 0 => pure (.cont s)
    | n + 1 => do
      match (← body i s) with
      | .cont s' =>
        let i' := if down then i - 1 else i + 1
        go n i' s'
      | step => pure step
  go (rangeCount lo hi down) lo init

def forInArr {σ r α} [Inhabited α] (xs : Array α)
    (body : α → σ → SudoM (Step σ r)) (init : σ) : SudoM (Step σ r) :=
  let rec go (fuel : Nat) (idx : Nat) (s : σ) : SudoM (Step σ r) :=
    match fuel with
    | 0 => pure (.cont s)
    | n + 1 => do
      match (← body (arrGet! xs idx) s) with
      | .cont s' => go n (idx + 1) s'
      | step => pure step
  go xs.size 0 init

-- ---- asserts + TAP runner ------------------------------------------------

def sudoAssert (cond : Bool) (line : Nat) : SudoM Unit :=
  if cond then pure () else trap "AssertFailed" s!"line {line}"

def sudoAssertEq {α} [SEq α] [Canon α] (l r : α) (line : Nat) : SudoM Unit :=
  if SEq.eq l r then pure ()
  else trap "AssertFailed" s!"line {line}: {Canon.canon l} != {Canon.canon r}"

def runTests (tests : List (String × SudoM Unit)) : IO UInt32 := do
  let mut i : Nat := 0
  let mut fails : Nat := 0
  for (name, act) in tests do
    i := i + 1
    match EStateM.run act () with
    | .ok () _ => IO.println s!"ok {i} - {name}"
    | .error t _ =>
      fails := fails + 1
      let tag :=
        if t.detail.isEmpty then s!"[{t.kind}]"
        else s!"[{t.kind}: {t.detail}]"
      IO.println s!"not ok {i} - {name} {tag}"
  IO.println s!"# {i - fails}/{i} passed"
  pure (if fails > 0 then 1 else 0)

end SudoRt
