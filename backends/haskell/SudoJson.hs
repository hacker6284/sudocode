{-# LANGUAGE LambdaCase #-}
-- Hand-rolled strict JSON for the sudoc external-backend wire format.
-- Budget: small decoder/printer, not a general JSON library.
module SudoJson
  ( Value(..)
  , parseValue
  , renderValue
  , objGet
  , objKeys
  , asStr
  , asArr
  , asObj
  , asBool
  , asI64
  , asI64Num
  , asDouble
  , asNull
  , expectKeys
  , extTag
  ) where

import Data.Char (chr, digitToInt, isDigit, isHexDigit, isSpace, ord)
import Data.Int (Int64)
import Data.List (sort)
import Data.Maybe (fromMaybe)
import Numeric (showHex)

-- | VNum stores raw decimal text (not a parsed Double).
data Value
  = VNull
  | VBool Bool
  | VNum String
  | VStr String
  | VArr [Value]
  | VObj [(String, Value)]
  deriving (Eq, Show)

-- ---------------------------------------------------------------------------
-- Parser
-- ---------------------------------------------------------------------------

parseValue :: String -> Either String Value
parseValue s = case parse s of
  Right (v, rest) | all isSpace rest -> Right v
  Right (_, rest) -> Left ("trailing garbage: " ++ take 40 rest)
  Left e -> Left e

type P a = String -> Either String (a, String)

parse :: P Value
parse s0 =
  let s = dropWhile isSpace s0
  in case s of
    '{' : _ -> object s
    '[' : _ -> array s
    '"' : _ -> fmap (\(t, r) -> (VStr t, r)) (string s)
    't' : 'r' : 'u' : 'e' : r -> Right (VBool True, r)
    'f' : 'a' : 'l' : 's' : 'e' : r -> Right (VBool False, r)
    'n' : 'u' : 'l' : 'l' : r -> Right (VNull, r)
    c : _ | c == '-' || isDigit c -> number s
    [] -> Left "unexpected end of input"
    c : _ -> Left ("unexpected char: " ++ show c)

object :: P Value
object s0 =
  let s1 = dropWhile isSpace s0
  in case s1 of
    '{' : s2 ->
      let s3 = dropWhile isSpace s2
      in case s3 of
        '}' : s4 -> Right (VObj [], s4)
        _ -> do
          (ps, s5) <- pairs s3
          let s6 = dropWhile isSpace s5
          case s6 of
            '}' : s7 -> Right (VObj ps, s7)
            _ -> Left "expected '}' in object"
    _ -> Left "expected '{'"

pairs :: P [(String, Value)]
pairs s = do
  (k, s1) <- string s
  let s2 = dropWhile isSpace s1
  case s2 of
    ':' : s3 -> do
      let s4 = dropWhile isSpace s3
      (v, s5) <- parse s4
      let s6 = dropWhile isSpace s5
      case s6 of
        ',' : s7 -> do
          let s8 = dropWhile isSpace s7
          (rest, s9) <- pairs s8
          Right ((k, v) : rest, s9)
        _ -> Right ([(k, v)], s6)
    _ -> Left "expected ':' after object key"

array :: P Value
array s0 =
  let s1 = dropWhile isSpace s0
  in case s1 of
    '[' : s2 ->
      let s3 = dropWhile isSpace s2
      in case s3 of
        ']' : s4 -> Right (VArr [], s4)
        _ -> do
          (xs, s5) <- elems s3
          let s6 = dropWhile isSpace s5
          case s6 of
            ']' : s7 -> Right (VArr xs, s7)
            _ -> Left "expected ']' in array"
    _ -> Left "expected '['"

elems :: P [Value]
elems s = do
  (v, s1) <- parse s
  let s2 = dropWhile isSpace s1
  case s2 of
    ',' : s3 -> do
      let s4 = dropWhile isSpace s3
      (rest, s5) <- elems s4
      Right (v : rest, s5)
    _ -> Right ([v], s2)

string :: P String
string s0 = case dropWhile isSpace s0 of
  '"' : s -> go s ""
  _ -> Left "expected string"
  where
    go [] _ = Left "unterminated string"
    go ('"' : r) acc = Right (reverse acc, r)
    go ('\\' : r) acc = case r of
      '"' : r' -> go r' ('"' : acc)
      '\\' : r' -> go r' ('\\' : acc)
      '/' : r' -> go r' ('/' : acc)
      'b' : r' -> go r' ('\b' : acc)
      'f' : r' -> go r' ('\f' : acc)
      'n' : r' -> go r' ('\n' : acc)
      'r' : r' -> go r' ('\r' : acc)
      't' : r' -> go r' ('\t' : acc)
      'u' : a : b : c : d : r'
        | all isHexDigit [a, b, c, d] ->
            let cp = hex4 a b c d
            in if cp >= 0xD800 && cp <= 0xDBFF
               then case r' of
                 '\\' : 'u' : e : f : g : h : r''
                   | all isHexDigit [e, f, g, h] ->
                       let lo = hex4 e f g h
                       in if lo >= 0xDC00 && lo <= 0xDFFF
                          then
                            let full = 0x10000 + ((cp - 0xD800) * 0x400) + (lo - 0xDC00)
                            in go r'' (chr full : acc)
                          else Left "invalid low surrogate"
                 _ -> Left "missing low surrogate"
               else if cp >= 0xDC00 && cp <= 0xDFFF
                    then Left "unexpected low surrogate"
                    else go r' (chr cp : acc)
        | otherwise -> Left "bad \\u escape"
      _ -> Left "bad escape"
    go (c : r) acc = go r (c : acc)

hex4 :: Char -> Char -> Char -> Char -> Int
hex4 a b c d =
  ((digitToInt a * 16 + digitToInt b) * 16 + digitToInt c) * 16 + digitToInt d

number :: P Value
number s0 =
  let s = dropWhile isSpace s0
      (raw, rest) = spanNum s
  in if null raw
     then Left "expected number"
     else Right (VNum raw, rest)
  where
    spanNum xs =
      let (sign, xs1) = case xs of
            '-' : r -> ("-", r)
            _ -> ("", xs)
          (intPart, xs2) = span isDigit xs1
          (fracPart, xs3) = case xs2 of
            '.' : r ->
              let (ds, r') = span isDigit r
              in if null ds then (".", r) else ('.' : ds, r')
            _ -> ("", xs2)
          (expPart, xs4) = case xs3 of
            e : r | e == 'e' || e == 'E' ->
              let (sgn, r1) = case r of
                    '+' : t -> ("+", t)
                    '-' : t -> ("-", t)
                    _ -> ("", r)
                  (ds, r2) = span isDigit r1
              in if null ds then ("", xs3) else (e : sgn ++ ds, r2)
            _ -> ("", xs3)
      in (sign ++ intPart ++ fracPart ++ expPart, xs4)

-- ---------------------------------------------------------------------------
-- Printer
-- ---------------------------------------------------------------------------

renderValue :: Value -> String
renderValue = \case
  VNull -> "null"
  VBool True -> "true"
  VBool False -> "false"
  VNum n -> n
  VStr s -> '"' : esc s ++ "\""
  VArr xs -> '[' : intercalate "," (map renderValue xs) ++ "]"
  VObj kvs -> '{' : intercalate "," [renderValue (VStr k) ++ ":" ++ renderValue v | (k, v) <- kvs] ++ "}"

esc :: String -> String
esc = concatMap escChar
  where
    escChar '"' = "\\\""
    escChar '\\' = "\\\\"
    escChar '\b' = "\\b"
    escChar '\f' = "\\f"
    escChar '\n' = "\\n"
    escChar '\r' = "\\r"
    escChar '\t' = "\\t"
    escChar c
      | ord c < 0x20 = "\\u" ++ pad4 (showHex (ord c) "")
      | otherwise = [c]
    pad4 h = replicate (4 - length h) '0' ++ h

intercalate :: [a] -> [[a]] -> [a]
intercalate _ [] = []
intercalate _ [x] = x
intercalate sep (x : xs) = x ++ sep ++ intercalate sep xs

-- ---------------------------------------------------------------------------
-- Decode helpers
-- ---------------------------------------------------------------------------

objGet :: String -> Value -> Either String Value
objGet k (VObj kvs) = case lookup k kvs of
  Just v -> Right v
  Nothing -> Left ("missing key: " ++ k)
objGet _ _ = Left "expected object"

objKeys :: Value -> Either String [String]
objKeys (VObj kvs) = Right (map fst kvs)
objKeys _ = Left "expected object"

-- | Reject objects with keys outside the allowed set.
expectKeys :: [String] -> Value -> Either String ()
expectKeys allowed (VObj kvs) =
  let extra = filter (`notElem` allowed) (map fst kvs)
  in if null extra then Right () else Left ("unknown fields: " ++ show extra)
expectKeys _ _ = Left "expected object"

asStr :: Value -> Either String String
asStr (VStr s) = Right s
asStr _ = Left "expected string"

asArr :: Value -> Either String [Value]
asArr (VArr xs) = Right xs
asArr _ = Left "expected array"

asObj :: Value -> Either String [(String, Value)]
asObj (VObj kvs) = Right kvs
asObj _ = Left "expected object"

asBool :: Value -> Either String Bool
asBool (VBool b) = Right b
asBool _ = Left "expected bool"

asNull :: Value -> Either String ()
asNull VNull = Right ()
asNull _ = Left "expected null"

-- | i64 as decimal string (wire rule for Int leaves).
asI64 :: Value -> Either String Int64
asI64 (VStr s) = parseI64Str s
asI64 _ = Left "expected i64 decimal string"

parseI64Str :: String -> Either String Int64
parseI64Str s =
  case s of
    [] -> Left "empty i64 string"
    '-' : ds | all isDigit ds && not (null ds) -> check (readInteger ('-' : ds))
    ds | all isDigit ds && not (null ds) -> check (readInteger ds)
    _ -> Left ("invalid i64 string: " ++ s)
  where
    check n
      | n < toInteger (minBound :: Int64) || n > toInteger (maxBound :: Int64) =
          Left ("i64 out of range: " ++ s)
      | otherwise = Right (fromInteger n)

readInteger :: String -> Integer
readInteger = read

-- | Plain JSON number → Int64 (for text scalar arrays).
asI64Num :: Value -> Either String Int64
asI64Num (VNum n) =
  case n of
    [] -> Left "empty number"
    '-' : ds | all isDigit ds && not (null ds) -> asI64 (VStr n)
    ds | all isDigit ds && not (null ds) -> asI64 (VStr n)
    _ -> Left ("expected integer number, got: " ++ n)
asI64Num _ = Left "expected number for i64"

-- | Float: JSON number or "nan"/"inf"/"-inf".
asDouble :: Value -> Either String Double
asDouble (VNum n) = case reads n of
  [(d, "")] -> Right d
  _ -> Left ("bad float number: " ++ n)
asDouble (VStr s) = case s of
  "nan" -> Right (0 / 0)
  "inf" -> Right (1 / 0)
  "-inf" -> Right ((-1) / 0)
  _ -> Left ("unknown float string: " ++ s)
asDouble _ = Left "expected float number or nan/inf/-inf string"

-- | Serde external tagging: bare string = unit, single-key object = payload.
extTag :: Value -> Either String (String, Maybe Value)
extTag (VStr s) = Right (s, Nothing)
extTag (VObj [(k, v)]) = Right (k, Just v)
extTag (VObj kvs) = Left ("expected single-key object for enum, got keys: " ++ show (map fst kvs))
extTag _ = Left "expected string or single-key object for enum tag"
