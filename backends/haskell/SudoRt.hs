{-# LANGUAGE BangPatterns #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE StrictData #-}
-- Shared sudo runtime for the Haskell external backend.
-- Semantics mirror sudoc backend_rs/backend_py runtimes (spec/language.md).
-- StrictData: constructor fields are forced at construction so traps buried
-- in Option/Result/Flow payloads fire under call-by-value (spec §12 / F2).
-- Records/enums need nothing further from container construction-time
-- strictness: StrictData already WHNF-forces every field at construction.
module SudoRt where

import Control.Exception (Exception, SomeException, evaluate, try, throw, fromException, AsyncException(StackOverflow))
import Data.Foldable (toList)
import Data.Int (Int64)
import qualified Data.Map.Strict as M
import qualified Data.Sequence as Sq
import qualified Data.Set as S
import Data.List (intercalate)
import Data.Ord (comparing)
import System.Exit (ExitCode(..), exitWith)
import System.IO (hFlush, stdout)

-- ---- traps -----------------------------------------------------------------

data SudoTrap = SudoTrap { trapKind :: String, trapDetail :: String }
  deriving (Eq)

instance Show SudoTrap where
  show (SudoTrap k d)
    | null d = k
    | otherwise = k ++ ": " ++ d

instance Exception SudoTrap

trap :: String -> String -> a
trap k d = throw (SudoTrap k d)

trapK :: String -> a
trapK k = trap k ""

-- ---- Flow for loop compilation ---------------------------------------------

data Flow r s = Cont s | Brk s | Ret r
  deriving (Eq, Show)

-- ---- Option / Result (qualified as Rt.SOption / Rt.SResult) -----------------

data SOption a = SNone | SSome a
  deriving (Eq, Ord, Show)

-- Error type first (Either-like), success second.
data SResult e a = SErr e | SOk a
  deriving (Eq, Ord, Show)

optIsSome :: SOption a -> Bool
optIsSome (SSome _) = True
optIsSome SNone = False

optIsNone :: SOption a -> Bool
optIsNone SNone = True
optIsNone _ = False

optUnwrap :: SOption a -> a
optUnwrap (SSome x) = x
optUnwrap SNone = trapK "UnwrapFailed"

optGetOr :: SOption a -> a -> a
optGetOr (SSome x) _ = x
optGetOr SNone d = d

resIsOk :: SResult e a -> Bool
resIsOk (SOk _) = True
resIsOk _ = False

resIsErr :: SResult e a -> Bool
resIsErr (SErr _) = True
resIsErr _ = False

resUnwrap :: SResult e a -> a
resUnwrap (SOk x) = x
resUnwrap (SErr _) = trapK "UnwrapFailed"

resGetOr :: SResult e a -> a -> a
resGetOr (SOk x) _ = x
resGetOr (SErr _) d = d

-- ---- i64 arithmetic --------------------------------------------------------

i64Min, i64Max :: Integer
i64Min = toInteger (minBound :: Int64)
i64Max = toInteger (maxBound :: Int64)

narrowI :: Integer -> Int64
narrowI n
  | n < i64Min || n > i64Max = trapK "Overflow"
  | otherwise = fromInteger n

chkAdd :: Int64 -> Int64 -> Int64
chkAdd a b = narrowI (toInteger a + toInteger b)

chkSub :: Int64 -> Int64 -> Int64
chkSub a b = narrowI (toInteger a - toInteger b)

chkMul :: Int64 -> Int64 -> Int64
chkMul a b = narrowI (toInteger a * toInteger b)

negI :: Int64 -> Int64
negI a = narrowI (negate (toInteger a))

absI :: Int64 -> Int64
absI a
  | a == minBound = trapK "Overflow"
  | a < 0 = negate a
  | otherwise = a

-- Floor div/mod: Haskell's div/mod already round toward -∞.
divI :: Int64 -> Int64 -> Int64
divI a b
  | b == 0 = trapK "DivByZero"
  | a == minBound && b == (-1) = trapK "Overflow"
  | otherwise = a `div` b

modI :: Int64 -> Int64 -> Int64
modI a b
  | b == 0 = trapK "DivByZero"
  | otherwise = a `mod` b

minI :: Int64 -> Int64 -> Int64
minI a b = if a <= b then a else b

maxI :: Int64 -> Int64 -> Int64
maxI a b = if a >= b then a else b

-- ---- floats ----------------------------------------------------------------

fdiv :: Double -> Double -> Double
fdiv a b
  | b == 0.0 =
      if a == 0.0 || isNaN a
      then 0 / 0
      else
        let sa = if isNegativeZero a || (a < 0 && not (isNaN a)) then (-1.0) else 1.0
            sb = if isNegativeZero b || (b < 0 && not (isNaN b)) then (-1.0) else 1.0
        in sa * sb * (1 / 0)
  | otherwise = a / b

fmin :: Double -> Double -> Double
fmin a b
  | isNaN a || isNaN b = 0 / 0
  | a == b =
      if isNegativeZero a || (a < 0) then a
      else if isNegativeZero b || (b < 0) then b
      else a
  | a < b = a
  | otherwise = b

fmax :: Double -> Double -> Double
fmax a b
  | isNaN a || isNaN b = 0 / 0
  | a == b =
      -- max(-0.0, 0.0) == +0.0
      if isNegativeZero a || (a < 0 && not (isNegativeZero b) && b == 0) then b else a
  | a > b = a
  | otherwise = b

-- Sign to give a zero-magnitude floor/ceil result: negative iff the
-- input was on the negative side of (or exactly) negative zero.
zeroSignedLike :: Double -> Double
zeroSignedLike x = if x < 0 || isNegativeZero x then -0.0 else 0.0

floorF :: Double -> Double
floorF x
  | isNaN x || isInfinite x = x
  | otherwise =
      let r = fromInteger (floor x) :: Double
      in if r == 0 then zeroSignedLike x else r

ceilF :: Double -> Double
ceilF x
  | isNaN x || isInfinite x = x
  | otherwise =
      let r = fromInteger (ceiling x) :: Double
      in if r == 0 then zeroSignedLike x else r

-- Ties away from zero (not banker's rounding). Truncates first, then
-- compares the fractional distance to 0.5 -- never adds 0.5 before
-- rounding, which would double-round values just below a half
-- boundary (e.g. 0.49999999999999994) up past the tie.
roundHalfAway :: Double -> Double
roundHalfAway x
  | isNaN x || isInfinite x = x
  | x == 0 = x
  | otherwise =
      let t = if x >= 0 then floorF x else ceilF x
          d = abs (x - t)
      in if d < 0.5 then t else t + (if x < 0 then -1 else 1)

sqrtF :: Double -> Double
sqrtF x
  | isNaN x || x < 0 = 0 / 0
  | otherwise = sqrt x

intOfFloat :: Double -> Int64
intOfFloat x
  | isNaN x || isInfinite x = trap "InvalidConvert" "NaN or infinity to int"
  | otherwise =
      let t = truncate x :: Integer
      in if t < i64Min || t > i64Max
         then trap "InvalidConvert" "float out of int range"
         else fromInteger t

floatOfInt :: Int64 -> Double
floatOfInt = fromIntegral

-- ---- lists (Data.Sequence.Seq: O(1) append/cons, O(log n) index/update) ----

idxCheck :: Int -> Int64 -> Int
idxCheck len i
  | i < 0 || toInteger i >= toInteger len =
      trap "OutOfBounds" ("index " ++ show i ++ " of length " ++ show len)
  | otherwise = fromIntegral i

at :: Sq.Seq a -> Int64 -> a
at xs i =
  let j = idxCheck (Sq.length xs) i
  in Sq.index xs j

-- WHNF-force the element at every store so construction-time strictness holds
-- (Stage A). Deep force would re-walk already-strict contents — never do that.
-- Force order: index bounds-check before the value (§12 place-before-RHS).
-- Nested `let` (not a multi-bind `let`) — GHC multi-bind bang patterns do
-- not reliably force left-to-right; nested `let`/`case` does.
putL :: Sq.Seq a -> Int64 -> a -> Sq.Seq a
putL xs i v =
  let !j = idxCheck (Sq.length xs) i
  in let !v' = v
     in Sq.update j v' xs

-- Returns (newList, result). Force the stored element; bang the unit for
-- consistent multi-return tuple strictness (Haskell (,) is lazy).
appendL :: Sq.Seq a -> a -> (Sq.Seq a, ())
appendL xs v =
  let !v' = v
      !xs' = xs Sq.|> v'
  in (xs', ())

popL :: Sq.Seq a -> (Sq.Seq a, a)
popL xs = case Sq.viewr xs of
  Sq.EmptyR -> trap "OutOfBounds" "pop from empty list"
  ys Sq.:> v ->
    let !v' = v
    in (ys, v')

-- Force i then v, in that argument order, BEFORE branching on the bounds
-- check — a trap in v must fire even when i is out of range (§12).
insertL :: Sq.Seq a -> Int64 -> a -> (Sq.Seq a, ())
insertL xs i v =
  let !i' = i
      !v' = v
      n = Sq.length xs
  in if i' < 0 || toInteger i' > toInteger n
     then trap "OutOfBounds" ("insert at " ++ show i' ++ " of length " ++ show n)
     else
       let j = fromIntegral i'
           (left, right) = Sq.splitAt j xs
           !xs' = left Sq.>< (v' Sq.<| right)
       in (xs', ())

removeAtL :: Sq.Seq a -> Int64 -> (Sq.Seq a, a)
removeAtL xs i =
  let j = idxCheck (Sq.length xs) i
      (left, right) = Sq.splitAt j xs
  in case Sq.viewl right of
       v Sq.:< rest ->
         let !v' = v
             !xs' = left Sq.>< rest
         in (xs', v')
       Sq.EmptyL -> trap "OutOfBounds" ("index " ++ show i ++ " of length " ++ show (Sq.length xs))

-- Force i then j, in that argument order, BEFORE either index's bounds
-- check can trap — a trap in j must fire even when i is out of range (§12).
swapL :: Sq.Seq a -> Int64 -> Int64 -> (Sq.Seq a, ())
swapL xs i j =
  let !i' = i
      !j' = j
      n = Sq.length xs
      ii = idxCheck n i'
      jj = idxCheck n j'
      !vi = Sq.index xs ii
      !vj = Sq.index xs jj
      step1 = putL xs (fromIntegral ii) vj
      !step2 = putL step1 (fromIntegral jj) vi
  in (step2, ())

-- Force n then v, in that argument order, BEFORE branching on n — a trap in
-- v must fire even when n < 0 (§12: arguments evaluate before the callee's
-- own precondition check runs). Force v exactly once; do not re-force per
-- slot.
filledL :: Int64 -> a -> Sq.Seq a
filledL n v =
  let !n' = n
      !v' = v
  in if n' < 0
     then trap "InvalidArg" ("filled(" ++ show n' ++ ")")
     else Sq.replicate (fromIntegral n') v'

-- Stable sort. For floats: NaN last, -0.0 before +0.0.
-- Elements are already WHNF by induction (entered via putL/appendL/listOf);
-- only the result tuple is constructed here.
sortL :: Ord a => Sq.Seq a -> (Sq.Seq a, ())
sortL xs =
  let !xs' = Sq.sortBy compare xs
  in (xs', ())

sortFloatsL :: Sq.Seq Double -> (Sq.Seq Double, ())
sortFloatsL xs =
  let !xs' = Sq.sortBy floatCmp xs
  in (xs', ())
  where
    floatCmp x y =
      let kx = floatSortKey x
          ky = floatSortKey y
      in compare kx ky

-- (nan_group, value_for_ord, sign_for_zeros)
-- nan_group 2 last; among reals ordinary <; ±0 by sign (-1 before +1)
floatSortKey :: Double -> (Int, Double, Int)
floatSortKey x
  | isNaN x = (2, 0.0, 0)
  | x == 0.0 =
      let s = if isNegativeZero x then (-1) else 1
      in (1, 0.0, s)
  | x < 0 = (1, x, -1)
  | otherwise = (1, x, 1)

listLen :: Sq.Seq a -> Int64
listLen xs = fromIntegral (Sq.length xs)

-- Build a Seq while WHNF-forcing each element left-to-right. Used by the
-- emitter for list/text literals so traps in unread literal slots fire at
-- construction (Stage A). Forcing happens on the [a] walk *before*
-- Sq.fromList; Sq.fromList alone does not guarantee force order.
listOf :: [a] -> Sq.Seq a
listOf xs = Sq.fromList (forceLtr xs)
  where
    forceLtr [] = []
    forceLtr (x : rest) =
      let !x' = x
      in x' : forceLtr rest

-- ---- maps ------------------------------------------------------------------

type SMap k v = M.Map k v

mapNew :: Ord k => SMap k v
mapNew = M.empty

mapSize :: SMap k v -> Int64
mapSize m = fromIntegral (M.size m)

mapGet :: Ord k => SMap k v -> k -> v
mapGet m k = case M.lookup k m of
  Just v -> v
  Nothing -> trapK "KeyMissing"

mapGetOpt :: Ord k => SMap k v -> k -> SOption v
mapGetOpt m k = case M.lookup k m of
  Just v -> SSome v
  Nothing -> SNone

mapHas :: Ord k => SMap k v -> k -> Bool
mapHas m k = M.member k m

-- Force order: key before value (§12 place-before-RHS). Nested `let` so
-- bangs fire left-to-right (multi-bind `let !k; !v` can force `v` first).
mapPut :: Ord k => SMap k v -> k -> v -> SMap k v
mapPut m k v =
  let !k' = k
  in let !v' = v
     in M.insert k' v' m

mapDelete :: Ord k => SMap k v -> k -> (SMap k v, Bool)
mapDelete m k =
  if M.member k m
  then
    let !m' = M.delete k m
    in (m', True)
  else (m, False)

-- Keys/values already entered via mapPut (element-strict); no re-force needed.
mapKeysL :: SMap k v -> Sq.Seq k
mapKeysL m = Sq.fromList (M.keys m)

mapValuesL :: SMap k v -> Sq.Seq v
mapValuesL m = Sq.fromList (M.elems m)

-- ---- sets ------------------------------------------------------------------

type SSet a = S.Set a

setNew :: Ord a => SSet a
setNew = S.empty

setSize :: SSet a -> Int64
setSize s = fromIntegral (S.size s)

setAdd :: Ord a => SSet a -> a -> (SSet a, Bool)
setAdd s x =
  let !x' = x
  in if S.member x' s
     then (s, False)
     else
       let !s' = S.insert x' s
       in (s', True)

setHas :: Ord a => SSet a -> a -> Bool
setHas s x = S.member x s

setRemove :: Ord a => SSet a -> a -> (SSet a, Bool)
setRemove s x =
  if S.member x s
  then
    let !s' = S.delete x s
    in (s', True)
  else (s, False)

-- Elements already entered via setAdd (element-strict); no re-force needed.
setItemsL :: SSet a -> Sq.Seq a
setItemsL s = Sq.fromList (S.toAscList s)

-- ---- Canon diagnostics -----------------------------------------------------

class Canon a where
  canon :: a -> String

instance Canon Int64 where
  canon = show

instance Canon Bool where
  canon True = "true"
  canon False = "false"

instance Canon Double where
  canon x
    | isNaN x = "{\"f\": \"NaN\"}"
    | isInfinite x = if x > 0 then "{\"f\": \"Inf\"}" else "{\"f\": \"-Inf\"}"
    | x == 0.0 && isNegativeZero x = "{\"f\": \"-0.0\"}"
    | otherwise =
        let s0 = show x
            s = if '.' `elem` s0 || 'e' `elem` s0 || 'E' `elem` s0 then s0 else s0 ++ ".0"
        in "{\"f\": \"" ++ s ++ "\"}"

-- Byte-identical to the former Canon [a]: front-to-back, ", " separator, [ ].
instance Canon a => Canon (Sq.Seq a) where
  canon xs = "[" ++ intercalate ", " (map canon (toList xs)) ++ "]"

instance (Canon a, Canon b) => Canon (a, b) where
  canon (a, b) = "[" ++ canon a ++ ", " ++ canon b ++ "]"

instance (Canon a, Canon b, Canon c) => Canon (a, b, c) where
  canon (a, b, c) = "[" ++ intercalate ", " [canon a, canon b, canon c] ++ "]"

instance (Canon a, Canon b, Canon c, Canon d) => Canon (a, b, c, d) where
  canon (a, b, c, d) = "[" ++ intercalate ", " [canon a, canon b, canon c, canon d] ++ "]"

instance Canon a => Canon (SOption a) where
  canon SNone = "{\"e\": \"Option.None\"}"
  canon (SSome v) = "{\"e\": \"Option.Some\", \"v\": [" ++ canon v ++ "]}"

instance (Canon e, Canon a) => Canon (SResult e a) where
  canon (SOk v) = "{\"e\": \"Result.Ok\", \"v\": [" ++ canon v ++ "]}"
  canon (SErr e) = "{\"e\": \"Result.Err\", \"v\": [" ++ canon e ++ "]}"

instance (Canon k, Canon v) => Canon (M.Map k v) where
  canon m =
    let pairs = [[canon k, canon v] | (k, v) <- M.toAscList m]
        body = intercalate ", " ["[" ++ intercalate ", " p ++ "]" | p <- pairs]
    in "{\"m\": [" ++ body ++ "]}"

instance Canon a => Canon (S.Set a) where
  canon s =
    let body = intercalate ", " (map canon (S.toAscList s))
    in "{\"s\": [" ++ body ++ "]}"

instance Canon () where
  canon () = "null"

-- Helpers for generated record/enum Canon instances (diagnostic format per
-- spec/lockstep.md §4). Keeps intercalate out of every generated module.
canonRecord :: String -> [String] -> String
canonRecord name [] = "{\"r\": \"" ++ name ++ "\"}"
canonRecord name vs =
  "{\"r\": \"" ++ name ++ "\", \"v\": [" ++ intercalate ", " vs ++ "]}"

canonEnum :: String -> String -> [String] -> String
canonEnum en vn [] = "{\"e\": \"" ++ en ++ "." ++ vn ++ "\"}"
canonEnum en vn vs =
  "{\"e\": \"" ++ en ++ "." ++ vn ++ "\", \"v\": [" ++ intercalate ", " vs ++ "]}"

-- ---- asserts ---------------------------------------------------------------

sudoAssert :: Bool -> Int -> ()
sudoAssert cond line
  | cond = ()
  | otherwise = trap "AssertFailed" ("line " ++ show line)

sudoAssertEq :: (Eq a, Canon a) => a -> a -> Int -> ()
sudoAssertEq l r line
  | l == r = ()
  | otherwise = trap "AssertFailed" ("line " ++ show line ++ ": " ++ canon l ++ " != " ++ canon r)

-- ---- test runner -----------------------------------------------------------

runTests :: [(String, IO ())] -> IO ()
runTests tests = do
  results <- mapM runOne (zip [1 ..] tests)
  let passed = length (filter id results)
      total = length tests
  putStrLn ("# " ++ show passed ++ "/" ++ show total ++ " passed")
  hFlush stdout
  if all id results
    then exitWith ExitSuccess
    else exitWith (ExitFailure 1)
  where
    runOne (n, (name, action)) = do
      r <- try action :: IO (Either SomeException ())
      case r of
        Right () -> do
          putStrLn ("ok " ++ show n ++ " - " ++ name)
          hFlush stdout
          return True
        Left e -> do
          let tag = case fromException e of
                Just (SudoTrap k d)
                  | null d -> "[" ++ k ++ "]"
                  | otherwise -> "[" ++ k ++ ": " ++ d ++ "]"
                Nothing ->
                  case fromException e of
                    Just StackOverflow -> "[StackOverflow]"
                    Nothing -> "[Exception: " ++ show e ++ "]"
          putStrLn ("not ok " ++ show n ++ " - " ++ name ++ " " ++ tag)
          hFlush stdout
          return False

-- Force a pure () result into IO for uniform test shape.
forceUnit :: () -> IO ()
forceUnit u = evaluate u >> return ()
