/-
  Shared sudo runtime for the Lean 4 external backend.

  Lean 4.14, no Mathlib. Generated modules import this file and call the
  helpers below. Semantics follow spec/language.md and backends/haskell/SudoRt.hs.

  Maps / sets: association lists kept sorted by `SOrd` (structural). Iteration
  order is therefore key-sorted, which is one legal choice under sudo's
  unspecified-order rule. Equality is order-insensitive. Observable tests that
  read the first iterated key will see the least key, not insertion order.
-/
namespace SudoRt

-- ---- traps -----------------------------------------------------------------

structure Trap where
  kind : String
  detail : String
  deriving Inhabited, Repr, BEq

instance : ToString Trap where
  toString t :=
    if t.detail.isEmpty then t.kind else s!"{t.kind}: {t.detail}"

def fail {α : Type} (k d : String) : Except Trap α :=
  .error { kind := k, detail := d }

def failK {α : Type} (k : String) : Except Trap α :=
  fail k ""

-- ---- Flow for loop compilation ---------------------------------------------

inductive Flow (σ : Type) (ρ : Type) where
  | cont : σ → Flow σ ρ
  | brk : σ → Flow σ ρ
  | ret : ρ → Flow σ ρ
  deriving Repr

/-- Well-founded fuel loop. Generated modules call this instead of `let rec`
so Lean does not hoist a helper (`foo.go`) that captures do-block locals. -/
def natIter {σ ρ : Type} (fuel : Nat) (step : σ → Except Trap (Flow σ ρ)) (s0 : σ) :
    Except Trap (Flow σ ρ) :=
  let rec go (fuel : Nat) (s : σ) : Except Trap (Flow σ ρ) :=
    match fuel with
    | 0 => fail "StackOverflow" "loop fuel exhausted"
    | fuel + 1 => do
        match ← step s with
        | .ret r => pure (.ret r)
        | .brk s => pure (.brk s)
        | .cont s => go fuel s
  go fuel s0

/-- Start-state first so `σ` is inferred from a known value before the
stepper is elaborated. `natIter fuel step s0` leaves `σ` as a metavariable
while matching the stepper's product patterns (and `.ret r`). -/
def natIterOn {σ ρ : Type} (s0 : σ) (fuel : Nat)
    (step : σ → Except Trap (Flow σ ρ)) :
    Except Trap (Flow σ ρ) :=
  natIter fuel step s0

/-- Drive a fuel loop and dispatch `.ret` / after-loop join. `s0` is first
so the after-function's product pattern on `σ` is fully typed. -/
def runLoopOn {σ ρ α : Type} (s0 : σ) (fuel : Nat)
    (step : σ → Except Trap (Flow σ ρ))
    (after : σ → Except Trap α)
    (onRet : ρ → Except Trap α) : Except Trap α := do
  match ← natIterOn s0 fuel step with
  | .ret r => onRet r
  | .brk s => after s
  | .cont s => after s

-- ---- Result (error first, success second — mirrors Haskell SResult) --------

inductive SResult (ε : Type) (α : Type) where
  | err : ε → SResult ε α
  | ok : α → SResult ε α
  deriving Repr, BEq, Inhabited

-- ---- Eq / Ord used for containers and assert -------------------------------

class SEq (α : Type) where
  beq : α → α → Bool

class SOrd (α : Type) extends SEq α where
  le : α → α → Bool

def cmp {α : Type} [SOrd α] (a b : α) : Ordering :=
  if SEq.beq a b then Ordering.eq
  else if SOrd.le a b then Ordering.lt
  else Ordering.gt

instance : SEq Int where
  beq a b := decide (a = b)

instance : SOrd Int where
  le a b := decide (a ≤ b)

instance : SEq Bool where
  beq a b := decide (a = b)

instance : SOrd Bool where
  le a b :=
    match a, b with
    | false, true => true
    | true, false => false
    | _, _ => true

/-- IEEE equality: NaN ≠ NaN, −0.0 == +0.0. -/
instance : SEq Float where
  beq a b := Float.beq a b

/-- Structural order for float containers. NaN sorts after every finite/Inf;
−0.0 and +0.0 compare equal (they already `SEq`). Not used as a map-key
policy — sudo leaves map/set iteration order unspecified; we only need a
total order so `SSet`/`SMap` stay sorted. -/
instance : SOrd Float where
  le a b :=
    if a.isNaN then b.isNaN
    else if b.isNaN then true
    else a ≤ b

instance : SEq Unit where
  beq _ _ := true

instance : SOrd Unit where
  le _ _ := true

instance [SEq α] : SEq (Option α) where
  beq
    | none, none => true
    | some a, some b => SEq.beq a b
    | _, _ => false

instance [SOrd α] : SOrd (Option α) where
  le
    | none, none => true
    | none, some _ => true
    | some _, none => false
    | some a, some b => SOrd.le a b

instance [SEq ε] [SEq α] : SEq (SResult ε α) where
  beq
    | .ok a, .ok b => SEq.beq a b
    | .err a, .err b => SEq.beq a b
    | _, _ => false

instance [SOrd ε] [SOrd α] : SOrd (SResult ε α) where
  le
    | .err a, .err b => SOrd.le a b
    | .err _, .ok _ => true
    | .ok _, .err _ => false
    | .ok a, .ok b => SOrd.le a b

instance [SEq α] [SEq β] : SEq (α × β) where
  beq a b := SEq.beq a.1 b.1 && SEq.beq a.2 b.2

instance [SOrd α] [SOrd β] : SOrd (α × β) where
  le a b :=
    if SEq.beq a.1 b.1 then SOrd.le a.2 b.2 else SOrd.le a.1 b.1

instance [SEq α] [SEq β] [SEq γ] : SEq (α × β × γ) where
  beq a b := SEq.beq a.1 b.1 && SEq.beq a.2.1 b.2.1 && SEq.beq a.2.2 b.2.2

instance [SOrd α] [SOrd β] [SOrd γ] : SOrd (α × β × γ) where
  le a b :=
    if !(SEq.beq a.1 b.1) then SOrd.le a.1 b.1
    else if !(SEq.beq a.2.1 b.2.1) then SOrd.le a.2.1 b.2.1
    else SOrd.le a.2.2 b.2.2

instance [SEq α] [SEq β] [SEq γ] [SEq δ] : SEq (α × β × γ × δ) where
  beq a b :=
    SEq.beq a.1 b.1 && SEq.beq a.2.1 b.2.1 && SEq.beq a.2.2.1 b.2.2.1 && SEq.beq a.2.2.2 b.2.2.2

private def arrayBeq {α : Type} [SEq α] (a b : Array α) : Bool :=
  if a.size != b.size then false
  else
    let rec go (i : Nat) : Bool :=
      if h : i < a.size then
        if h' : i < b.size then
          if SEq.beq (a.get ⟨i, h⟩) (b.get ⟨i, h'⟩) then go (i + 1) else false
        else false
      else true
    go 0

instance [SEq α] : SEq (Array α) where
  beq := arrayBeq

/-- Typeclass-free array equality for mutually recursive generated types. -/
def beqBy {α : Type} (beq : α → α → Bool) (a b : Array α) : Bool :=
  if a.size != b.size then false
  else
    let rec go (i : Nat) : Bool :=
      if h : i < a.size then
        if h' : i < b.size then
          if beq (a.get ⟨i, h⟩) (b.get ⟨i, h'⟩) then go (i + 1) else false
        else false
      else true
    go 0

def leBy {α : Type} (beq le : α → α → Bool) (a b : Array α) : Bool :=
  let rec go (i : Nat) : Bool :=
    if h : i < a.size then
      if h' : i < b.size then
        let x := a.get ⟨i, h⟩
        let y := b.get ⟨i, h'⟩
        if beq x y then go (i + 1) else le x y
      else false
    else true
  go 0

def canonBy {α : Type} (canon : α → String) (xs : Array α) : String :=
  let rec go (i : Nat) (acc : List String) : List String :=
    if h : i < xs.size then
      go (i + 1) (acc ++ [canon (xs.get ⟨i, h⟩)])
    else acc
  "[" ++ String.intercalate ", " (go 0 []) ++ "]"

private def arrayLe {α : Type} [SOrd α] (a b : Array α) : Bool :=
  let rec go (i : Nat) : Bool :=
    if h : i < a.size then
      if h' : i < b.size then
        let x := a.get ⟨i, h⟩
        let y := b.get ⟨i, h'⟩
        if SEq.beq x y then go (i + 1)
        else SOrd.le x y
      else false
    else true
  go 0

instance [SOrd α] : SOrd (Array α) where
  le := arrayLe

-- ---- i64 arithmetic --------------------------------------------------------

def i64Min : Int := -9223372036854775808
def i64Max : Int := 9223372036854775807

def narrowI (n : Int) : Except Trap Int :=
  if n < i64Min || n > i64Max then failK "Overflow" else .ok n

def addI (a b : Int) : Except Trap Int := narrowI (a + b)
def subI (a b : Int) : Except Trap Int := narrowI (a - b)
def mulI (a b : Int) : Except Trap Int := narrowI (a * b)

def negI (a : Int) : Except Trap Int := narrowI (-a)

def absI (a : Int) : Except Trap Int :=
  if a == i64Min then failK "Overflow"
  else if a < 0 then .ok (-a)
  else .ok a

/-- Floor division (Lean `Int.fdiv`). -/
def divI (a b : Int) : Except Trap Int :=
  if b == 0 then failK "DivByZero"
  else if a == i64Min && b == -1 then failK "Overflow"
  else .ok (Int.fdiv a b)

/-- Floor modulo (Lean `Int.fmod`). `minInt mod -1 == 0`. -/
def modI (a b : Int) : Except Trap Int :=
  if b == 0 then failK "DivByZero"
  else .ok (Int.fmod a b)

def minI (a b : Int) : Int := if a ≤ b then a else b
def maxI (a b : Int) : Int := if a ≥ b then a else b

-- ---- floats ----------------------------------------------------------------

def isNegZero (x : Float) : Bool :=
  Float.beq x 0.0 && Float.isInf (1.0 / x) && (1.0 / x) < 0

def fdiv (a b : Float) : Float :=
  if b == 0.0 then
    if a == 0.0 || a.isNaN then (0.0 : Float) / 0.0
    else
      let sa : Float := if isNegZero a || (a < 0.0 && !a.isNaN) then -1.0 else 1.0
      let sb : Float := if isNegZero b || (b < 0.0 && !b.isNaN) then -1.0 else 1.0
      sa * sb * (1.0 / 0.0)
  else a / b

def fmin (a b : Float) : Float :=
  if a.isNaN || b.isNaN then (0.0 : Float) / 0.0
  else if a == b then
    if isNegZero a || a < 0 then a
    else if isNegZero b || b < 0 then b
    else a
  else if a < b then a
  else b

def fmax (a b : Float) : Float :=
  if a.isNaN || b.isNaN then (0.0 : Float) / 0.0
  else if a == b then
    -- max(−0.0, 0.0) == +0.0
    if isNegZero a then b else a
  else if a > b then a
  else b

def zeroSignedLike (x : Float) : Float :=
  if x < 0 || isNegZero x then -0.0 else 0.0

def floorF (x : Float) : Float :=
  if x.isNaN || x.isInf then x
  else
    let r := Float.floor x
    if r == 0 then zeroSignedLike x else r

def ceilF (x : Float) : Float :=
  if x.isNaN || x.isInf then x
  else
    let r := Float.ceil x
    if r == 0 then zeroSignedLike x else r

/-- Ties away from zero. Lean 4.14 `Float.round` already does this. -/
def roundHalfAway (x : Float) : Float :=
  if x.isNaN || x.isInf then x
  else if x == 0 then x
  else Float.round x

def sqrtF (x : Float) : Float :=
  if x.isNaN || x < 0 then (0.0 : Float) / 0.0
  else Float.sqrt x

def absF (x : Float) : Float :=
  if isNegZero x then 0.0
  else Float.abs x

/-- Truncate toward zero; trap on NaN / Inf / out of i64 range. -/
def intOfFloat (x : Float) : Except Trap Int :=
  if x.isNaN || x.isInf then
    fail "InvalidConvert" "NaN or infinity to int"
  else
    let t : Float := if x ≥ 0 then Float.floor x else Float.ceil x
    -- Convert via string of the integer part to avoid Float→Int holes.
    let s := t.toString
    -- `toString` of a finite integer-valued float looks like "-3.000000" or "3"
    let rec digits (cs : List Char) (acc : Int) (neg : Bool) : Option Int :=
      match cs with
      | [] => some (if neg then -acc else acc)
      | c :: rest =>
        if c == '-' then digits rest acc true
        else if c == '.' then some (if neg then -acc else acc)
        else if c.isDigit then
          digits rest (acc * 10 + (c.toNat - '0'.toNat)) neg
        else none
    match digits s.toList 0 false with
    | none => fail "InvalidConvert" "float out of int range"
    | some n =>
      if n < i64Min || n > i64Max then
        fail "InvalidConvert" "float out of int range"
      else .ok n

def floatOfInt (n : Int) : Float := Float.ofInt n

-- ---- lists (Array) ---------------------------------------------------------

def idxCheck (len : Nat) (i : Int) : Except Trap Nat :=
  if i < 0 || i ≥ Int.ofNat len then
    fail "OutOfBounds" s!"index {i} of length {len}"
  else .ok i.toNat

def atL {α : Type} (xs : Array α) (i : Int) : Except Trap α := do
  let j ← idxCheck xs.size i
  if h : j < xs.size then
    pure (xs.get ⟨j, h⟩)
  else
    fail "OutOfBounds" s!"index {i} of length {xs.size}"

def putL {α : Type} (xs : Array α) (i : Int) (v : α) : Except Trap (Array α) := do
  let j ← idxCheck xs.size i
  if h : j < xs.size then
    pure (xs.set ⟨j, h⟩ v)
  else
    fail "OutOfBounds" s!"index {i} of length {xs.size}"

def appendL {α : Type} (xs : Array α) (v : α) : Array α × Unit :=
  (xs.push v, ())

def popL {α : Type} (xs : Array α) : Except Trap (Array α × α) := do
  if xs.size == 0 then
    fail "OutOfBounds" "pop from empty list"
  else
    let j := xs.size - 1
    if h : j < xs.size then
      pure (xs.pop, xs.get ⟨j, h⟩)
    else
      fail "OutOfBounds" "pop from empty list"

def insertL {α : Type} (xs : Array α) (i : Int) (v : α) : Except Trap (Array α × Unit) := do
  -- Force i then v (v is already a value) before the bounds check (§12).
  let i' := i
  let v' := v
  let n := xs.size
  if i' < 0 || i' > Int.ofNat n then
    fail "OutOfBounds" s!"insert at {i'} of length {n}"
  else
    let j := i'.toNat
    let left := xs.extract 0 j
    let right := xs.extract j n
    pure (left.push v' ++ right, ())

def removeAtL {α : Type} (xs : Array α) (i : Int) : Except Trap (Array α × α) := do
  let j ← idxCheck xs.size i
  if h : j < xs.size then
    let v := xs.get ⟨j, h⟩
    let left := xs.extract 0 j
    let right := xs.extract (j + 1) xs.size
    pure (left ++ right, v)
  else
    fail "OutOfBounds" s!"index {i} of length {xs.size}"

def swapL {α : Type} (xs : Array α) (i j : Int) : Except Trap (Array α × Unit) := do
  let i' := i
  let j' := j
  let ii ← idxCheck xs.size i'
  let jj ← idxCheck xs.size j'
  if hi : ii < xs.size then
    if hj : jj < xs.size then
      let vi := xs.get ⟨ii, hi⟩
      let vj := xs.get ⟨jj, hj⟩
      let step1 := xs.set ⟨ii, hi⟩ vj
      if hj2 : jj < step1.size then
        pure (step1.set ⟨jj, hj2⟩ vi, ())
      else
        fail "OutOfBounds" s!"index {j'} of length {xs.size}"
    else
      fail "OutOfBounds" s!"index {j'} of length {xs.size}"
  else
    fail "OutOfBounds" s!"index {i'} of length {xs.size}"

def filledL {α : Type} (n : Int) (v : α) : Except Trap (Array α) :=
  let n' := n
  let v' := v
  if n' < 0 then
    fail "InvalidArg" s!"filled({n'})"
  else
    .ok (Array.mkArray n'.toNat v')

private def floatSortLt (x y : Float) : Bool :=
  let group (z : Float) : Nat := if z.isNaN then 2 else 1
  let gx := group x
  let gy := group y
  if gx != gy then gx < gy
  else if x.isNaN then false
  else if x == 0 && y == 0 then
    -- −0.0 before +0.0
    isNegZero x && !isNegZero y
  else x < y

def sortL {α : Type} [SOrd α] (xs : Array α) : Array α × Unit :=
  (xs.qsort (fun a b => SOrd.le a b && !SEq.beq a b), ())

def sortFloatsL (xs : Array Float) : Array Float × Unit :=
  (xs.qsort floatSortLt, ())

def listLen {α : Type} (xs : Array α) : Int := Int.ofNat xs.size

def concatL {α : Type} (a b : Array α) : Array α := a ++ b

-- ---- maps (sorted association lists) ---------------------------------------

structure SMap (κ : Type) (ν : Type) where
  entries : List (κ × ν)
  deriving Repr, Inhabited

def mapNew {κ ν : Type} : SMap κ ν := ⟨[]⟩

def mapSize {κ ν : Type} (m : SMap κ ν) : Int := Int.ofNat m.entries.length

def mapGet {κ ν : Type} [SEq κ] (m : SMap κ ν) (k : κ) : Except Trap ν :=
  let rec go : List (κ × ν) → Except Trap ν
    | [] => failK "KeyMissing"
    | (k', v) :: rest => if SEq.beq k k' then .ok v else go rest
  go m.entries

def mapGetOpt {κ ν : Type} [SEq κ] (m : SMap κ ν) (k : κ) : Option ν :=
  let rec go : List (κ × ν) → Option ν
    | [] => none
    | (k', v) :: rest => if SEq.beq k k' then some v else go rest
  go m.entries

def mapHas {κ ν : Type} [SEq κ] (m : SMap κ ν) (k : κ) : Bool :=
  (mapGetOpt m k).isSome

def mapPut {κ ν : Type} [SOrd κ] (m : SMap κ ν) (k : κ) (v : ν) : SMap κ ν :=
  let rec go : List (κ × ν) → List (κ × ν)
    | [] => [(k, v)]
    | (k', v') :: rest =>
      match cmp k k' with
      | Ordering.lt => (k, v) :: (k', v') :: rest
      | Ordering.eq => (k, v) :: rest
      | Ordering.gt => (k', v') :: go rest
  ⟨go m.entries⟩

def mapDelete {κ ν : Type} [SEq κ] (m : SMap κ ν) (k : κ) : SMap κ ν × Bool :=
  if !(mapHas m k) then (m, false)
  else
    let rec go : List (κ × ν) → List (κ × ν)
      | [] => []
      | (k', v') :: rest => if SEq.beq k k' then rest else (k', v') :: go rest
    (⟨go m.entries⟩, true)

def mapKeysL {κ ν : Type} (m : SMap κ ν) : Array κ :=
  Array.mk (m.entries.map (·.1))

def mapValuesL {κ ν : Type} (m : SMap κ ν) : Array ν :=
  Array.mk (m.entries.map (·.2))

instance [SEq κ] [SEq ν] : SEq (SMap κ ν) where
  beq a b :=
    if a.entries.length != b.entries.length then false
    else
      -- Order-insensitive: both lists are kept sorted, so pairwise compare.
      let rec go : List (κ × ν) → List (κ × ν) → Bool
        | [], [] => true
        | (k1, v1) :: r1, (k2, v2) :: r2 =>
          SEq.beq k1 k2 && SEq.beq v1 v2 && go r1 r2
        | _, _ => false
      go a.entries b.entries

instance [SOrd κ] [SOrd ν] : SOrd (SMap κ ν) where
  le a b :=
    let rec go : List (κ × ν) → List (κ × ν) → Bool
      | [], [] => true
      | [], _ => true
      | _, [] => false
      | (k1, v1) :: r1, (k2, v2) :: r2 =>
        if !(SEq.beq k1 k2) then SOrd.le k1 k2
        else if !(SEq.beq v1 v2) then SOrd.le v1 v2
        else go r1 r2
    go a.entries b.entries

-- ---- sets (sorted unique lists) --------------------------------------------

structure SSet (α : Type) where
  items : List α
  deriving Repr, Inhabited

def setNew {α : Type} : SSet α := ⟨[]⟩

def setSize {α : Type} (s : SSet α) : Int := Int.ofNat s.items.length

def setHas {α : Type} [SEq α] (s : SSet α) (x : α) : Bool :=
  let rec go : List α → Bool
    | [] => false
    | y :: rest => SEq.beq x y || go rest
  go s.items

def setAdd {α : Type} [SOrd α] (s : SSet α) (x : α) : SSet α × Bool :=
  if setHas s x then (s, false)
  else
    let rec go : List α → List α
      | [] => [x]
      | y :: rest =>
        match cmp x y with
        | Ordering.lt => x :: y :: rest
        | Ordering.eq => y :: rest
        | Ordering.gt => y :: go rest
    (⟨go s.items⟩, true)

def setRemove {α : Type} [SEq α] (s : SSet α) (x : α) : SSet α × Bool :=
  if !(setHas s x) then (s, false)
  else
    let rec go : List α → List α
      | [] => []
      | y :: rest => if SEq.beq x y then rest else y :: go rest
    (⟨go s.items⟩, true)

def setItemsL {α : Type} (s : SSet α) : Array α := Array.mk s.items

instance [SEq α] : SEq (SSet α) where
  beq a b :=
    if a.items.length != b.items.length then false
    else
      let rec go : List α → List α → Bool
        | [], [] => true
        | x :: r1, y :: r2 => SEq.beq x y && go r1 r2
        | _, _ => false
      go a.items b.items

instance [SOrd α] : SOrd (SSet α) where
  le a b :=
    let rec go : List α → List α → Bool
      | [], [] => true
      | [], _ => true
      | _, [] => false
      | x :: r1, y :: r2 =>
        if SEq.beq x y then go r1 r2 else SOrd.le x y
    go a.items b.items

-- ---- Option / Result helpers -----------------------------------------------

def optIsSome {α : Type} : Option α → Bool
  | some _ => true
  | none => false

def optIsNone {α : Type} : Option α → Bool
  | none => true
  | some _ => false

def optUnwrap {α : Type} : Option α → Except Trap α
  | some x => .ok x
  | none => failK "UnwrapFailed"

def optGetOr {α : Type} : Option α → α → α
  | some x, _ => x
  | none, d => d

def resIsOk {ε α : Type} : SResult ε α → Bool
  | .ok _ => true
  | .err _ => false

def resIsErr {ε α : Type} : SResult ε α → Bool
  | .err _ => true
  | .ok _ => false

def resUnwrap {ε α : Type} : SResult ε α → Except Trap α
  | .ok x => .ok x
  | .err _ => failK "UnwrapFailed"

def resGetOr {ε α : Type} : SResult ε α → α → α
  | .ok x, _ => x
  | .err _, d => d

-- ---- Canon diagnostics (lockstep.md §4; diagnostic-only) -------------------

class Canon (α : Type) where
  canon : α → String

instance : Canon Int where
  canon n := toString n

instance : Canon Bool where
  canon
    | true => "true"
    | false => "false"

instance : Canon Float where
  canon x :=
    if x.isNaN then "{\"f\": \"NaN\"}"
    else if x.isInf then
      if x > 0 then "{\"f\": \"Inf\"}" else "{\"f\": \"-Inf\"}"
    else if x == 0 && isNegZero x then "{\"f\": \"-0.0\"}"
    else
      let s0 := x.toString
      let s :=
        if s0.contains '.' || s0.contains 'e' || s0.contains 'E' then s0
        else s0 ++ ".0"
      s!"\{\"f\": \"{s}\"}"

instance : Canon Unit where
  canon _ := "null"

instance [Canon α] : Canon (Array α) where
  canon xs :=
    let rec go (i : Nat) (acc : List String) : List String :=
      if h : i < xs.size then
        go (i + 1) (acc ++ [Canon.canon (xs.get ⟨i, h⟩)])
      else acc
    "[" ++ String.intercalate ", " (go 0 []) ++ "]"

instance [Canon α] [Canon β] : Canon (α × β) where
  canon p := s!"[{Canon.canon p.1}, {Canon.canon p.2}]"

instance [Canon α] [Canon β] [Canon γ] : Canon (α × β × γ) where
  canon p :=
    s!"[{Canon.canon p.1}, {Canon.canon p.2.1}, {Canon.canon p.2.2}]"

instance [Canon α] [Canon β] [Canon γ] [Canon δ] : Canon (α × β × γ × δ) where
  canon p :=
    s!"[{Canon.canon p.1}, {Canon.canon p.2.1}, {Canon.canon p.2.2.1}, {Canon.canon p.2.2.2}]"

instance [Canon α] : Canon (Option α) where
  canon
    | none => "{\"e\": \"Option.None\"}"
    | some v => s!"\{\"e\": \"Option.Some\", \"v\": [{Canon.canon v}]}"

instance [Canon ε] [Canon α] : Canon (SResult ε α) where
  canon
    | .ok v => s!"\{\"e\": \"Result.Ok\", \"v\": [{Canon.canon v}]}"
    | .err e => s!"\{\"e\": \"Result.Err\", \"v\": [{Canon.canon e}]}"

instance [Canon κ] [Canon ν] : Canon (SMap κ ν) where
  canon m :=
    let pairs :=
      m.entries.map fun (k, v) =>
        s!"[{Canon.canon k}, {Canon.canon v}]"
    s!"\{\"m\": [{String.intercalate ", " pairs}]}"

instance [Canon α] : Canon (SSet α) where
  canon s :=
    let body := String.intercalate ", " (s.items.map Canon.canon)
    s!"\{\"s\": [{body}]}"

def canonRecord (name : String) (vs : List String) : String :=
  match vs with
  | [] => s!"\{\"r\": \"{name}\"}"
  | _ => s!"\{\"r\": \"{name}\", \"v\": [{String.intercalate ", " vs}]}"

def canonEnum (en vn : String) (vs : List String) : String :=
  match vs with
  | [] => s!"\{\"e\": \"{en}.{vn}\"}"
  | _ => s!"\{\"e\": \"{en}.{vn}\", \"v\": [{String.intercalate ", " vs}]}"

-- ---- asserts ---------------------------------------------------------------

def sudoAssert (cond : Bool) (line : Nat) : Except Trap Unit :=
  if cond then .ok ()
  else fail "AssertFailed" s!"line {line}"

def sudoAssertEq {α : Type} [SEq α] [Canon α] (l r : α) (line : Nat) : Except Trap Unit :=
  if SEq.beq l r then .ok ()
  else fail "AssertFailed" s!"line {line}: {Canon.canon l} != {Canon.canon r}"

-- ---- test runner (TAP-ish) -------------------------------------------------

def runTests (tests : List (String × (Unit → Except Trap Unit))) : IO UInt32 := do
  let mut n : Nat := 1
  let mut passed : Nat := 0
  for (name, action) in tests do
    match action () with
    | .ok () =>
      IO.println s!"ok {n} - {name}"
      passed := passed + 1
    | .error t =>
      let tag :=
        if t.detail.isEmpty then s!"[{t.kind}]"
        else s!"[{t.kind}: {t.detail}]"
      IO.println s!"not ok {n} - {name} {tag}"
    n := n + 1
  let total := tests.length
  IO.println s!"# {passed}/{total} passed"
  if passed == total then
    return 0
  else
    return 1

end SudoRt
