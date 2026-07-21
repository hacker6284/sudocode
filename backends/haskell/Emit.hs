{-# LANGUAGE BangPatterns #-}
{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE ScopedTypeVariables #-}
-- sudo → Haskell external backend emitter (protocol v1).
-- Reads one emit request JSON from stdin; writes one response JSON to stdout.
module Main where

import Control.Exception (evaluate)
import Control.Monad (when, foldM)
import Data.Char (isAlphaNum, isAsciiLower, isAsciiUpper, isDigit, toLower, toUpper)
import Data.Int (Int64)
import Data.List (find, intercalate, isPrefixOf, nub, partition)
import Data.Maybe (fromMaybe, isJust, mapMaybe)
import qualified Data.Set as Set
import System.IO (hGetContents, hPutStr, hSetEncoding, stdin, stdout, stderr, hPutStrLn, utf8)
import System.Exit (exitSuccess)
import SudoJson

-- ===========================================================================
-- IR types
-- ===========================================================================

data Ty
  = TInt | TFloat | TBool
  | TList Ty | TSet Ty | TMap Ty Ty
  | TOption Ty | TResult Ty Ty
  | TTuple [Ty]
  | TFunc [Ty] (Maybe Ty)
  | TRecord String | TEnum String
  deriving (Eq, Show)

data Place
  = PVar String
  | PIndex Place Ty IrExpr
  | PField Place Ty String
  deriving (Eq, Show)

data IrExpr = IrExpr { eTy :: Ty, eKind :: IrExprKind }
  deriving (Eq, Show)

data IrExprKind
  = EInt Int64
  | EFloat Double
  | EBool Bool
  | EText [Int64]
  | ELocal String
  | EConst String
  | EFuncRef String
  | EList [IrExpr]
  | ETuple [IrExpr]
  | ECallFunc String [IrExpr]
  | ECallValue IrExpr [IrExpr]
  | ENewRecord String [IrExpr]
  | ENewVariant String String [IrExpr]
  | EBuiltin Builtin [IrExpr]
  | EMutBuiltin Builtin Place Ty [IrExpr]
  | EGetField IrExpr String
  | EIndex IrExpr IrExpr
  | EUnary UnaryOp IrExpr
  | EBinary BinaryOp IrExpr IrExpr
  deriving (Eq, Show)

data UnaryOp = UNeg | UNot deriving (Eq, Show)
data BinaryOp
  = BAdd | BSub | BMul | BDiv | BMod
  | BLt | BLe | BGt | BGe | BEq | BNe | BAnd | BOr
  deriving (Eq, Show)

data Builtin
  = AbsInt | AbsFloat | MinInt | MaxInt | MinFloat | MaxFloat
  | FloatOfInt | IntOfFloat | Floor | Ceil | Round | Sqrt
  | Filled | NewMap | NewSet
  | ListLength | ListAppend | ListPop | ListInsert | ListRemoveAt | ListSwap | ListSort
  | MapSize | MapGet | MapHas | MapDelete | MapKeys | MapValues
  | SetSize | SetAdd | SetHas | SetRemove | SetItems
  | OptIsSome | OptIsNone | OptUnwrap | OptGetOr
  | ResIsOk | ResIsErr | ResUnwrap | ResGetOr
  deriving (Eq, Show)

data IrPattern
  = PatInt Int64
  | PatBool Bool
  | PatWildcard
  | PatVariant String String [String]
  deriving (Eq, Show)

data IrMatchArm = IrMatchArm IrPattern [IrStmt]
  deriving (Eq, Show)

data IrStmt
  = SAssign Place IrExpr Bool
  | STupleAssign [String] [Bool] IrExpr
  | SExpr IrExpr
  | SIf [(IrExpr, [IrStmt])] (Maybe [IrStmt])
  | SWhile IrExpr [IrStmt]
  | SForRange String IrExpr IrExpr Bool [IrStmt]
  | SForIn [String] IrExpr [IrStmt]
  | SMatch IrExpr [IrMatchArm]
  | SReturn (Maybe IrExpr)
  | SAssert IrExpr Int
  | SSkip
  | SBreak
  | SContinue
  | SExpectTrap String [IrStmt] Int
  deriving (Eq, Show)

data IrParam = IrParam { pName :: String, pInout :: Bool, pTy :: Ty }
  deriving (Eq, Show)

data IrFunc = IrFunc
  { fName :: String
  , fExport :: Bool
  , fParams :: [IrParam]
  , fRet :: Maybe Ty
  , fBody :: [IrStmt]
  } deriving (Eq, Show)

data IrTest = IrTest { tName :: String, tBody :: [IrStmt] }
  deriving (Eq, Show)

data IrRecord = IrRecord { rName :: String, rFields :: [(String, Ty)] }
  deriving (Eq, Show)

data IrEnum = IrEnum { enName :: String, enVariants :: [(String, [(String, Ty)])] }
  deriving (Eq, Show)

data IrConst = IrConst { cName :: String, cTy :: Ty, cValue :: IrExpr }
  deriving (Eq, Show)

data IrModule = IrModule
  { mName :: String
  , mImports :: [String]
  , mRecords :: [IrRecord]
  , mEnums :: [IrEnum]
  , mConsts :: [IrConst]
  , mFuncs :: [IrFunc]
  , mTests :: [IrTest]
  } deriving (Eq, Show)

data EmitReq = EmitReq
  { rqEntry :: String
  , rqWithTests :: Bool
  , rqModules :: [IrModule]
  } deriving (Eq, Show)

-- ===========================================================================
-- Strict IR decode
-- ===========================================================================

type Dec a = Either String a

decodeRequest :: Value -> Dec EmitReq
decodeRequest v = do
  expectKeys ["protocol", "cmd", "entry", "with_tests", "modules"] v
  proto <- objGet "protocol" v >>= \case
    VNum n | n == "1" -> Right (1 :: Int)
    VNum n -> Left ("unsupported protocol: " ++ n)
    _ -> Left "protocol must be number 1"
  cmd <- objGet "cmd" v >>= asStr
  when (cmd /= "emit") (Left ("unknown cmd: " ++ cmd))
  entry <- objGet "entry" v >>= asStr
  withT <- objGet "with_tests" v >>= asBool
  modsV <- objGet "modules" v >>= asArr
  mods <- mapM decodeModule modsV
  when (null mods) (Left "modules must be non-empty")
  let lastName = mName (last mods)
  when (lastName /= entry) (Left ("entry " ++ show entry ++ " != last module " ++ show lastName))
  pure (EmitReq entry withT mods)

decodeModule :: Value -> Dec IrModule
decodeModule v = do
  expectKeys ["name", "imports", "records", "enums", "consts", "funcs", "tests"] v
  name <- objGet "name" v >>= asStr
  imports <- objGet "imports" v >>= asArr >>= mapM asStr
  records <- objGet "records" v >>= asArr >>= mapM decodeRecord
  enums <- objGet "enums" v >>= asArr >>= mapM decodeEnum
  consts <- objGet "consts" v >>= asArr >>= mapM decodeConst
  funcs <- objGet "funcs" v >>= asArr >>= mapM decodeFunc
  tests <- objGet "tests" v >>= asArr >>= mapM decodeTest
  pure (IrModule name imports records enums consts funcs tests)

decodeRecord :: Value -> Dec IrRecord
decodeRecord v = do
  expectKeys ["name", "fields"] v
  name <- objGet "name" v >>= asStr
  fields <- objGet "fields" v >>= asArr >>= mapM decodeFieldPair
  pure (IrRecord name fields)

decodeFieldPair :: Value -> Dec (String, Ty)
decodeFieldPair (VArr [a, b]) = do
  n <- asStr a
  t <- decodeTy b
  pure (n, t)
decodeFieldPair _ = Left "field must be [name, ty]"

decodeEnum :: Value -> Dec IrEnum
decodeEnum v = do
  expectKeys ["name", "variants"] v
  name <- objGet "name" v >>= asStr
  variants <- objGet "variants" v >>= asArr >>= mapM decodeVariant
  pure (IrEnum name variants)

decodeVariant :: Value -> Dec (String, [(String, Ty)])
decodeVariant v = do
  expectKeys ["name", "fields"] v
  name <- objGet "name" v >>= asStr
  fields <- objGet "fields" v >>= asArr >>= mapM decodeFieldPair
  pure (name, fields)

decodeConst :: Value -> Dec IrConst
decodeConst v = do
  expectKeys ["name", "ty", "value"] v
  name <- objGet "name" v >>= asStr
  ty <- objGet "ty" v >>= decodeTy
  val <- objGet "value" v >>= decodeExpr
  pure (IrConst name ty val)

decodeFunc :: Value -> Dec IrFunc
decodeFunc v = do
  expectKeys ["name", "export", "params", "ret", "ret_boundary", "body"] v
  name <- objGet "name" v >>= asStr
  exp <- objGet "export" v >>= asBool
  params <- objGet "params" v >>= asArr >>= mapM decodeParam
  ret <- objGet "ret" v >>= decodeMaybeTy
  _rb <- objGet "ret_boundary" v  -- ignored this session
  body <- objGet "body" v >>= asArr >>= mapM decodeStmt
  pure (IrFunc name exp params ret body)

decodeParam :: Value -> Dec IrParam
decodeParam v = do
  expectKeys ["name", "inout", "ty", "boundary"] v
  name <- objGet "name" v >>= asStr
  io <- objGet "inout" v >>= asBool
  ty <- objGet "ty" v >>= decodeTy
  _b <- objGet "boundary" v
  pure (IrParam name io ty)

decodeTest :: Value -> Dec IrTest
decodeTest v = do
  expectKeys ["name", "body"] v
  name <- objGet "name" v >>= asStr
  body <- objGet "body" v >>= asArr >>= mapM decodeStmt
  pure (IrTest name body)

decodeMaybeTy :: Value -> Dec (Maybe Ty)
decodeMaybeTy VNull = Right Nothing
decodeMaybeTy v = Just <$> decodeTy v

decodeTy :: Value -> Dec Ty
decodeTy v = do
  (tag, payload) <- extTag v
  case (tag, payload) of
    ("Int", Nothing) -> Right TInt
    ("Float", Nothing) -> Right TFloat
    ("Bool", Nothing) -> Right TBool
    ("List", Just p) -> TList <$> decodeTy p
    ("Set", Just p) -> TSet <$> decodeTy p
    ("Map", Just (VArr [k, val])) -> TMap <$> decodeTy k <*> decodeTy val
    ("Map", _) -> Left "Map expects [k,v]"
    ("Option_", Just p) -> TOption <$> decodeTy p
    ("Result_", Just (VArr [t, e])) -> TResult <$> decodeTy t <*> decodeTy e
    ("Result_", _) -> Left "Result_ expects [t,e]"
    ("Tuple", Just (VArr xs)) -> TTuple <$> mapM decodeTy xs
    ("Tuple", _) -> Left "Tuple expects array"
    ("Func", Just p) -> do
      expectKeys ["params", "ret"] p
      ps <- objGet "params" p >>= asArr >>= mapM decodeTy
      r <- objGet "ret" p >>= decodeMaybeTy
      pure (TFunc ps r)
    ("Record", Just p) -> TRecord <$> asStr p
    ("Enum", Just p) -> TEnum <$> asStr p
    _ -> Left ("unknown Ty tag: " ++ tag)

decodeExpr :: Value -> Dec IrExpr
decodeExpr v = do
  expectKeys ["ty", "kind"] v
  ty <- objGet "ty" v >>= decodeTy
  kind <- objGet "kind" v >>= decodeExprKind
  pure (IrExpr ty kind)

decodeExprKind :: Value -> Dec IrExprKind
decodeExprKind v = do
  (tag, payload) <- extTag v
  case (tag, payload) of
    ("Int", Just p) -> EInt <$> asI64 p
    ("Float", Just p) -> EFloat <$> asDouble p
    ("Bool", Just p) -> EBool <$> asBool p
    ("Text", Just (VArr xs)) -> EText <$> mapM asI64Num xs
    ("Text", _) -> Left "Text expects array of numbers"
    ("Local", Just p) -> ELocal <$> asStr p
    ("Const", Just p) -> EConst <$> asStr p
    ("FuncRef", Just p) -> EFuncRef <$> asStr p
    ("List", Just (VArr xs)) -> EList <$> mapM decodeExpr xs
    ("List", _) -> Left "List expects array"
    ("Tuple", Just (VArr xs)) -> ETuple <$> mapM decodeExpr xs
    ("Tuple", _) -> Left "Tuple expects array"
    ("CallFunc", Just p) -> do
      expectKeys ["name", "args"] p
      n <- objGet "name" p >>= asStr
      args <- objGet "args" p >>= asArr >>= mapM decodeExpr
      pure (ECallFunc n args)
    ("CallValue", Just p) -> do
      expectKeys ["callee", "args"] p
      c <- objGet "callee" p >>= decodeExpr
      args <- objGet "args" p >>= asArr >>= mapM decodeExpr
      pure (ECallValue c args)
    ("NewRecord", Just p) -> do
      expectKeys ["name", "args"] p
      n <- objGet "name" p >>= asStr
      args <- objGet "args" p >>= asArr >>= mapM decodeExpr
      pure (ENewRecord n args)
    ("NewVariant", Just p) -> do
      expectKeys ["enum_name", "variant", "args"] p
      en <- objGet "enum_name" p >>= asStr
      vn <- objGet "variant" p >>= asStr
      args <- objGet "args" p >>= asArr >>= mapM decodeExpr
      pure (ENewVariant en vn args)
    ("Builtin", Just p) -> do
      expectKeys ["builtin", "args"] p
      b <- objGet "builtin" p >>= decodeBuiltin
      args <- objGet "args" p >>= asArr >>= mapM decodeExpr
      pure (EBuiltin b args)
    ("MutBuiltin", Just p) -> do
      expectKeys ["builtin", "recv", "recv_ty", "args"] p
      b <- objGet "builtin" p >>= decodeBuiltin
      recv <- objGet "recv" p >>= decodePlace
      rty <- objGet "recv_ty" p >>= decodeTy
      args <- objGet "args" p >>= asArr >>= mapM decodeExpr
      pure (EMutBuiltin b recv rty args)
    ("GetField", Just p) -> do
      expectKeys ["recv", "name"] p
      r <- objGet "recv" p >>= decodeExpr
      n <- objGet "name" p >>= asStr
      pure (EGetField r n)
    ("Index", Just p) -> do
      expectKeys ["recv", "index"] p
      r <- objGet "recv" p >>= decodeExpr
      i <- objGet "index" p >>= decodeExpr
      pure (EIndex r i)
    ("Unary", Just p) -> do
      expectKeys ["op", "operand"] p
      op <- objGet "op" p >>= decodeUnary
      o <- objGet "operand" p >>= decodeExpr
      pure (EUnary op o)
    ("Binary", Just p) -> do
      expectKeys ["op", "lhs", "rhs"] p
      op <- objGet "op" p >>= decodeBinary
      l <- objGet "lhs" p >>= decodeExpr
      r <- objGet "rhs" p >>= decodeExpr
      pure (EBinary op l r)
    _ -> Left ("unknown IrExprKind: " ++ tag)

decodeUnary :: Value -> Dec UnaryOp
decodeUnary v = asStr v >>= \case
  "Neg" -> Right UNeg
  "Not" -> Right UNot
  s -> Left ("unknown UnaryOp: " ++ s)

decodeBinary :: Value -> Dec BinaryOp
decodeBinary v = asStr v >>= \case
  "Add" -> Right BAdd
  "Sub" -> Right BSub
  "Mul" -> Right BMul
  "Div" -> Right BDiv
  "Mod" -> Right BMod
  "Lt" -> Right BLt
  "Le" -> Right BLe
  "Gt" -> Right BGt
  "Ge" -> Right BGe
  "Eq" -> Right BEq
  "Ne" -> Right BNe
  "And" -> Right BAnd
  "Or" -> Right BOr
  s -> Left ("unknown BinaryOp: " ++ s)

decodeBuiltin :: Value -> Dec Builtin
decodeBuiltin v = asStr v >>= \case
  "AbsInt" -> Right AbsInt
  "AbsFloat" -> Right AbsFloat
  "MinInt" -> Right MinInt
  "MaxInt" -> Right MaxInt
  "MinFloat" -> Right MinFloat
  "MaxFloat" -> Right MaxFloat
  "FloatOfInt" -> Right FloatOfInt
  "IntOfFloat" -> Right IntOfFloat
  "Floor" -> Right Floor
  "Ceil" -> Right Ceil
  "Round" -> Right Round
  "Sqrt" -> Right Sqrt
  "Filled" -> Right Filled
  "NewMap" -> Right NewMap
  "NewSet" -> Right NewSet
  "ListLength" -> Right ListLength
  "ListAppend" -> Right ListAppend
  "ListPop" -> Right ListPop
  "ListInsert" -> Right ListInsert
  "ListRemoveAt" -> Right ListRemoveAt
  "ListSwap" -> Right ListSwap
  "ListSort" -> Right ListSort
  "MapSize" -> Right MapSize
  "MapGet" -> Right MapGet
  "MapHas" -> Right MapHas
  "MapDelete" -> Right MapDelete
  "MapKeys" -> Right MapKeys
  "MapValues" -> Right MapValues
  "SetSize" -> Right SetSize
  "SetAdd" -> Right SetAdd
  "SetHas" -> Right SetHas
  "SetRemove" -> Right SetRemove
  "SetItems" -> Right SetItems
  "OptIsSome" -> Right OptIsSome
  "OptIsNone" -> Right OptIsNone
  "OptUnwrap" -> Right OptUnwrap
  "OptGetOr" -> Right OptGetOr
  "ResIsOk" -> Right ResIsOk
  "ResIsErr" -> Right ResIsErr
  "ResUnwrap" -> Right ResUnwrap
  "ResGetOr" -> Right ResGetOr
  s -> Left ("unknown Builtin: " ++ s)

decodePlace :: Value -> Dec Place
decodePlace v = do
  (tag, payload) <- extTag v
  case (tag, payload) of
    ("Var", Just p) -> PVar <$> asStr p
    ("Index", Just p) -> do
      expectKeys ["base", "base_ty", "index"] p
      b <- objGet "base" p >>= decodePlace
      bt <- objGet "base_ty" p >>= decodeTy
      i <- objGet "index" p >>= decodeExpr
      pure (PIndex b bt i)
    ("Field", Just p) -> do
      expectKeys ["base", "base_ty", "name"] p
      b <- objGet "base" p >>= decodePlace
      bt <- objGet "base_ty" p >>= decodeTy
      n <- objGet "name" p >>= asStr
      pure (PField b bt n)
    _ -> Left ("unknown Place: " ++ tag)

decodeStmt :: Value -> Dec IrStmt
decodeStmt v = do
  (tag, payload) <- extTag v
  case (tag, payload) of
    ("Skip", Nothing) -> Right SSkip
    ("Break", Nothing) -> Right SBreak
    ("Continue", Nothing) -> Right SContinue
    ("Assign", Just p) -> do
      expectKeys ["target", "value", "declares"] p
      t <- objGet "target" p >>= decodePlace
      val <- objGet "value" p >>= decodeExpr
      d <- objGet "declares" p >>= asBool
      pure (SAssign t val d)
    ("TupleAssign", Just p) -> do
      expectKeys ["targets", "declares", "value"] p
      ts <- objGet "targets" p >>= asArr >>= mapM asStr
      ds <- objGet "declares" p >>= asArr >>= mapM asBool
      val <- objGet "value" p >>= decodeExpr
      pure (STupleAssign ts ds val)
    ("Expr", Just p) -> SExpr <$> decodeExpr p
    ("If", Just p) -> do
      expectKeys ["arms", "else_block"] p
      armsV <- objGet "arms" p >>= asArr
      arms <- mapM decodeIfArm armsV
      elseB <- objGet "else_block" p >>= \case
        VNull -> Right Nothing
        VArr xs -> Just <$> mapM decodeStmt xs
        _ -> Left "else_block must be null or array"
      pure (SIf arms elseB)
    ("While", Just p) -> do
      expectKeys ["cond", "body"] p
      c <- objGet "cond" p >>= decodeExpr
      b <- objGet "body" p >>= asArr >>= mapM decodeStmt
      pure (SWhile c b)
    ("ForRange", Just p) -> do
      expectKeys ["var", "from", "to", "down", "body"] p
      var <- objGet "var" p >>= asStr
      fr <- objGet "from" p >>= decodeExpr
      to <- objGet "to" p >>= decodeExpr
      down <- objGet "down" p >>= asBool
      b <- objGet "body" p >>= asArr >>= mapM decodeStmt
      pure (SForRange var fr to down b)
    ("ForIn", Just p) -> do
      expectKeys ["vars", "iter", "body"] p
      vs <- objGet "vars" p >>= asArr >>= mapM asStr
      it <- objGet "iter" p >>= decodeExpr
      b <- objGet "body" p >>= asArr >>= mapM decodeStmt
      pure (SForIn vs it b)
    ("Match", Just p) -> do
      expectKeys ["scrutinee", "arms"] p
      sc <- objGet "scrutinee" p >>= decodeExpr
      arms <- objGet "arms" p >>= asArr >>= mapM decodeMatchArm
      pure (SMatch sc arms)
    ("Return", Just VNull) -> Right (SReturn Nothing)
    ("Return", Just p) -> SReturn . Just <$> decodeExpr p
    ("Return", Nothing) -> Right (SReturn Nothing)
    -- serde encodes Return(None) as {"Return": null}
    ("Assert", Just p) -> do
      expectKeys ["cond", "line"] p
      c <- objGet "cond" p >>= decodeExpr
      line <- objGet "line" p >>= asLine
      pure (SAssert c line)
    ("ExpectTrap", Just p) -> do
      expectKeys ["kind", "body", "line"] p
      k <- objGet "kind" p >>= asStr
      b <- objGet "body" p >>= asArr >>= mapM decodeStmt
      line <- objGet "line" p >>= asLine
      pure (SExpectTrap k b line)
    _ -> Left ("unknown IrStmt: " ++ tag ++ " payload=" ++ show (isJust payload))

decodeIfArm :: Value -> Dec (IrExpr, [IrStmt])
decodeIfArm (VArr [c, VArr body]) = do
  cond <- decodeExpr c
  stmts <- mapM decodeStmt body
  pure (cond, stmts)
decodeIfArm _ = Left "If arm must be [cond, body[]]"

decodeMatchArm :: Value -> Dec IrMatchArm
decodeMatchArm v = do
  expectKeys ["pattern", "body"] v
  pat <- objGet "pattern" v >>= decodePattern
  body <- objGet "body" v >>= asArr >>= mapM decodeStmt
  pure (IrMatchArm pat body)

decodePattern :: Value -> Dec IrPattern
decodePattern v = do
  (tag, payload) <- extTag v
  case (tag, payload) of
    ("Int", Just p) -> PatInt <$> asI64 p
    ("Bool", Just p) -> PatBool <$> asBool p
    ("Wildcard", Nothing) -> Right PatWildcard
    ("Variant", Just p) -> do
      expectKeys ["enum_name", "variant", "binders"] p
      en <- objGet "enum_name" p >>= asStr
      vn <- objGet "variant" p >>= asStr
      bs <- objGet "binders" p >>= asArr >>= mapM asStr
      pure (PatVariant en vn bs)
    _ -> Left ("unknown IrPattern: " ++ tag)

asLine :: Value -> Dec Int
asLine (VNum n) = case reads n of
  [(i, "")] -> Right i
  _ -> Left ("bad line number: " ++ n)
asLine _ = Left "line must be number"

-- ===========================================================================
-- Naming
-- ===========================================================================

hsReserved :: Set.Set String
hsReserved = Set.fromList
  [ "case", "class", "data", "default", "deriving", "do", "else", "foreign"
  , "if", "import", "in", "infix", "infixl", "infixr", "instance", "let"
  , "module", "newtype", "of", "then", "type", "where", "_"
  -- Common Prelude names that would clash under unqualified import of the
  -- entry module into *_test.hs (and with Prelude in every generated module).
  , "take", "drop", "map", "filter", "foldr", "foldl", "length", "head"
  , "tail", "init", "last", "null", "elem", "notElem", "lookup", "zip"
  , "unzip", "repeat", "replicate", "cycle", "iterate", "sum", "product"
  , "maximum", "minimum", "reverse", "concat", "and", "or", "any", "all"
  , "id", "const", "flip", "curry", "uncurry", "error", "undefined"
  , "show", "read", "print", "putStr", "getLine", "return", "pure"
  , "seq", "id", "pi", "exp", "log", "sqrt", "sin", "cos", "tan"
  , "compare", "min", "max", "succ", "pred", "toEnum", "fromEnum"
  , "enumFrom", "otherwise", "maybe", "either", "fst", "snd"
  , "not", "div", "mod", "quot", "rem", "gcd", "lcm", "abs", "signum"
  , "negate", "recip", "trunc", "round", "ceiling", "floor"
  ]

mangleValue :: String -> String
mangleValue n =
  let base = case n of
        c : _ | isAsciiUpper c -> "v_" ++ n
        _ -> n
      base' = if base `Set.member` hsReserved then base ++ "_" else base
  in base'

mangleType :: String -> String
mangleType n =
  let base = case n of
        c : cs | isAsciiLower c -> "T_" ++ (toUpper c : cs)
        [] -> "T"
        _ -> n
      base' = if map toLower base `Set.member` hsReserved
                 || base `Set.member` hsReserved
              then base ++ "_"
              else base
  in base'

mangleModule :: String -> String
mangleModule = mangleType  -- module names must start uppercase

mangleField :: String -> String -> String
mangleField recName field =
  let rn = mangleType recName
      prefix = case rn of
        c : cs -> toLower c : cs
        [] -> "r"
      raw = prefix ++ "_" ++ field
  in if raw `Set.member` hsReserved then raw ++ "_" else raw

mangleVariant :: String -> String -> String
mangleVariant en vn = mangleType en ++ "_" ++ mangleType vn

-- Cross-module "mod.func" → ("Mod", "func")
splitQual :: String -> (Maybe String, String)
splitQual s = case break (== '.') s of
  (a, '.' : b) | not (null a) && not (null b) && '.' `notElem` b ->
    -- only first dot; monomorphized names may have __ but not more dots usually
    (Just a, b)
  _ ->
    -- handle module.func with possible extra? take first dot only always if present
    case break (== '.') s of
      (a, '.' : b) | not (null a) && not (null b) -> (Just a, b)
      _ -> (Nothing, s)

-- test_fn_names — faithful port of sudoc_ir::names
sanitizeTest :: String -> String
sanitizeTest name =
  let go [] _ acc = reverse acc
      go (c : cs) prevUs acc
        | c >= 'A' && c <= 'Z' = go cs False (toLower c : acc)
        | c >= 'a' && c <= 'z' = go cs False (c : acc)
        | c >= '0' && c <= '9' = go cs False (c : acc)
        | not prevUs && not (null acc) = go cs True ('_' : acc)
        | otherwise = go cs prevUs acc
      stripped = dropWhileEnd (== '_') (go name False [])
  in if null stripped then "t" else stripped

dropWhileEnd :: (a -> Bool) -> [a] -> [a]
dropWhileEnd p = reverse . dropWhile p . reverse

testNamesFor :: [IrTest] -> [String]
testNamesFor tests =
  snd $ foldl (\(used, acc) t ->
    let san = sanitizeTest (tName t)
        (name, used') = pick san used 2
    in (used', acc ++ [name])
  ) (Set.empty, []) tests
  where
    pick san used _n =
      let cand0 = "test_" ++ san
      in if not (Set.member cand0 used)
         then (cand0, Set.insert cand0 used)
         else pickN san used 2
    pickN san used n =
      let cand = "test_" ++ san ++ "_" ++ show n
      in if not (Set.member cand used)
         then (cand, Set.insert cand used)
         else pickN san used (n + 1)

-- ===========================================================================
-- Emit context
-- ===========================================================================

data Ctx = Ctx
  { ctxMod :: IrModule
  , ctxAll :: [IrModule]
  , ctxFRetParts :: [String]   -- haskell type strings for FRet components
  , ctxFRetNames :: [String]   -- how to build FRet at return (expr strings)
  , ctxInouts :: [String]      -- inout param names (sudo names)
  , ctxMode :: Mode
  , ctxLoopVars :: [String]    -- threaded vars for current loop (sudo names)
  , ctxFresh :: Int
  }

data Mode = ExprMode | LoopMode deriving (Eq, Show)

lookupFunc :: Ctx -> String -> Maybe IrFunc
lookupFunc ctx name =
  let (mq, fn) = splitQual name
  in case mq of
    Nothing -> find (\f -> fName f == fn) (mFuncs (ctxMod ctx))
    Just mn -> do
      m <- find (\m -> mName m == mn) (ctxAll ctx)
      find (\f -> fName f == fn) (mFuncs m)

lookupRecord :: Ctx -> String -> Maybe IrRecord
lookupRecord ctx n =
  find (\r -> rName r == n) (mRecords (ctxMod ctx))
  `orElse` (listToMaybe $ mapMaybe (\m -> find (\r -> rName r == n) (mRecords m)) (ctxAll ctx))

orElse :: Maybe a -> Maybe a -> Maybe a
orElse (Just x) _ = Just x
orElse Nothing y = y

listToMaybe :: [a] -> Maybe a
listToMaybe [] = Nothing
listToMaybe (x : _) = Just x

-- ===========================================================================
-- Type rendering
-- ===========================================================================

renderTy :: Ty -> String
renderTy = \case
  TInt -> "Int64"
  TFloat -> "Double"
  TBool -> "Bool"
  TList t -> "[" ++ renderTy t ++ "]"
  TSet t -> "(S.Set " ++ renderTy t ++ ")"
  TMap k v -> "(M.Map " ++ renderTy k ++ " " ++ renderTy v ++ ")"
  TOption t -> "(Rt.SOption " ++ renderTy t ++ ")"
  TResult t e -> "(Rt.SResult " ++ renderTy e ++ " " ++ renderTy t ++ ")"
  TTuple [] -> "()"
  TTuple [t] -> renderTy t  -- shouldn't happen; 1-tuples collapse
  TTuple ts -> "(" ++ intercalate ", " (map renderTy ts) ++ ")"
  TFunc ps ret ->
    -- Always parenthesize function types so they don't flatten when used as
    -- a parameter type: `[Int64] -> (Int64 -> Int64 -> Bool) -> ...`
    let args = map renderTy ps
        r = maybe "()" renderTy ret
    in "(" ++ intercalate " -> " (args ++ [r]) ++ ")"
  TRecord n -> mangleType n
  TEnum n -> mangleType n

-- FRet shape for a function
fretParts :: IrFunc -> [Ty]
fretParts f =
  let ret = case fRet f of
        Just t -> [t]
        Nothing -> []
      ios = [pTy p | p <- fParams f, pInout p]
  in ret ++ ios

fretTyStr :: IrFunc -> String
fretTyStr f =
  case fretParts f of
    [] -> "()"
    [t] -> renderTy t
    ts -> "(" ++ intercalate ", " (map renderTy ts) ++ ")"

-- ===========================================================================
-- Pretty-print helpers (layout-rule Haskell; depth-aware indentation)
-- ===========================================================================

-- Spaces per indentation level. Every backend in this repo uses a fixed step;
-- 2 keeps nested case/let chains readable without eating the whole margin.
indN :: Int
indN = 2

indentAll :: Int -> String -> String
indentAll n s =
  let pad = replicate n ' '
  in intercalate "\n" [ pad ++ l | l <- lines s ]

-- True when a one-line body should still be broken onto the next line
-- (starts a nested layout construct).
isBlockStart :: String -> Bool
isBlockStart s =
  "case " `isPrefixOf` s
  || "if " `isPrefixOf` s
  || "let " `isPrefixOf` s
  || "do " `isPrefixOf` s
  || "do\n" `isPrefixOf` s

-- Layout-rule single-alternative case used for forced (non-recursive) binds.
-- `let !x = e` black-holes when e mentions x; case does not (see friction log).
bangBind :: String -> String -> String -> String
bangBind pat expr rest =
  let hdr = "case " ++ expr ++ " of"
      armPrefix = "!" ++ pat ++ " ->"
      restLs = lines rest
  in case restLs of
       [one]
         | length one <= 72 && not (isBlockStart one) ->
             hdr ++ "\n" ++ replicate indN ' ' ++ armPrefix ++ " " ++ one
       _ ->
             hdr ++ "\n"
             ++ replicate indN ' ' ++ armPrefix ++ "\n"
             ++ indentAll (2 * indN) rest

bangBind_ :: String -> String -> String
bangBind_ expr rest = bangBind "_" expr rest

-- Multi-arm layout-rule case. Scrutinee is parenthesized only when multi-line.
caseOf :: String -> [(String, String)] -> String
caseOf scrut arms =
  let scrut' =
        if '\n' `elem` scrut
        then "(\n" ++ indentAll indN scrut ++ "\n)"
        else scrut
      renderArm (pat, body) =
        let bodyLs = lines body
        in case bodyLs of
             [one]
               | length one <= 72 && not (isBlockStart one) ->
                   [replicate indN ' ' ++ pat ++ " -> " ++ one]
             _ ->
                   [replicate indN ' ' ++ pat ++ " ->"]
                   ++ [indentAll (2 * indN) body]
  in "case " ++ scrut' ++ " of\n" ++ intercalate "\n" (concatMap renderArm arms)

-- Layout if/then/else with bodies nested under then/else.
layoutIf :: String -> String -> String -> String
layoutIf c t e =
  "if " ++ c ++ "\n"
  ++ replicate indN ' ' ++ "then\n"
  ++ indentAll (2 * indN) t ++ "\n"
  ++ replicate indN ' ' ++ "else\n"
  ++ indentAll (2 * indN) e

-- `let lhs = rhs in inE` with layout-aligned let/in.
prettyLet :: String -> String -> String -> String
prettyLet lhs rhs inE =
  "let " ++ lhs ++ " =\n"
  ++ indentAll (indN + 4) rhs ++ "\n"
  ++ "in\n"
  ++ indentAll indN inE

-- ===========================================================================
-- Body IR + readability peepholes (session 4)
-- ===========================================================================
-- Forced-bind / control-flow tree for statement bodies. Leaf expressions stay
-- pre-rendered strings from emitExpr. Peepholes rewrite this tree to a fixed
-- point before pretty-printing — never string-search the final text.

-- Pattern for a single-arm bang force-bind (`case e of !pat -> body`).
data FPat
  = FpVar String
  | FpWild
  | FpTup [FPat]
  deriving (Eq, Show)

-- Statement-body IR.
data Hs
  = HExpr String
  | HForce FPat Hs Hs              -- case scrut of { !pat -> body }
  | HCase Hs [(String, Hs)]        -- multi-arm case (pats pre-rendered)
  | HIf String Hs Hs               -- if cond then t else e
  | HLet String Hs Hs              -- let lhs = rhs in body
  deriving (Eq, Show)

-- Render FPat for bangBind (which prefixes a single outer `!`).
-- Tuple components get their own bangs so fields are forced at the same point
-- as the old nested `case _io0 of !items -> case _ret of !p ->` hops.
-- Resulting source looks like `!(!p, !items)` which GHC accepts with BangPatterns.
renderFPat :: FPat -> String
renderFPat FpWild = "_"
renderFPat (FpVar v) = v
renderFPat (FpTup ps) =
  "(" ++ intercalate ", " (map bangField ps) ++ ")"
  where
    bangField FpWild = "_"
    bangField (FpVar v) = "!" ++ v
    bangField p@(FpTup _) = "!" ++ renderFPat p

isIdentChar :: Char -> Bool
isIdentChar c = isAlphaNum c || c == '_' || c == '\''

-- Whole-identifier substitution in a rendered fragment.
substId :: String -> String -> String -> String
substId old new str
  | null old = str
  | otherwise = go True str
  where
    go _ [] = []
    go atBound s
      | atBound
      , old `isPrefixOf` s
      , let rest = drop (length old) s
      , null rest || not (isIdentChar (head rest))
      = new ++ go newEndsBound rest
      | otherwise =
          let c = head s
          in c : go (not (isIdentChar c)) (tail s)
      where
        newEndsBound = null new || not (isIdentChar (last new))

substFPat :: String -> String -> FPat -> FPat
substFPat old new = go
  where
    go FpWild = FpWild
    go (FpVar v) = FpVar (if v == old then new else v)
    go (FpTup ps) = FpTup (map go ps)

-- Does this pattern bind `name`? (shadowing boundary for subst)
patBinds :: String -> FPat -> Bool
patBinds name FpWild = False
patBinds name (FpVar v) = v == name
patBinds name (FpTup ps) = any (patBinds name) ps

-- Multi-arm case patterns are free-form strings (`Rt.Cont (i, items)`); treat
-- a crude whole-id match as "binds" to avoid subst under a shadowing arm.
strPatBinds :: String -> String -> Bool
strPatBinds name pat =
  -- true if `name` appears as a whole identifier in pat
  substId name "#BOUND#" pat /= pat

substHs :: String -> String -> Hs -> Hs
substHs old new = go
  where
    go (HExpr s) = HExpr (substId old new s)
    go (HForce pat scrut body)
      | patBinds old pat =
          -- Scrutinee is outside the binder; body is shadowed — leave both
          -- the pattern and the body alone (do not rename the binder).
          HForce pat (go scrut) body
      | otherwise =
          HForce (substFPat old new pat) (go scrut) (go body)
    go (HCase scrut arms) =
      HCase (go scrut)
        [ if strPatBinds old p
          then (p, b)  -- arm binder shadows: leave pat + body untouched
          else (substId old new p, go b)
        | (p, b) <- arms
        ]
    go (HIf c t e) = HIf (substId old new c) (go t) (go e)
    go (HLet lhs rhs body) =
      -- `let go items i = RHS in BODY`: params (and the recursive fn name)
      -- shadow inside RHS only. BODY is outside the param scope, so it still
      -- gets the outer substitution. Never rename binders on the lhs.
      let ws = words lhs
          fn = case ws of
            (f : _) -> f
            [] -> ""
          params = drop 1 ws
          rhsShadowed = old == fn || old `elem` params
      in HLet lhs
           (if rhsShadowed then rhs else go rhs)
           (go body)

-- Bare variable reference (parameter or already-bound name). Not an app/lit.
isBareVar :: String -> Bool
isBareVar s =
  not (null s)
  && all isIdentChar s
  && (let c = head s in c == '_' || isAsciiLower c || isAsciiUpper c)

-- True when `name` occurs free (referenced, not only bound) in the tree.
-- Used to refuse peep 1/2 when the intermediate name is still live — those
-- are real copies (`b = a` then mutate `b`, still read `a`), not renames.
freeInHs :: String -> Hs -> Bool
freeInHs name = go
  where
    idIn = strPatBinds name  -- whole-identifier occurrence in a string
    go (HExpr s) = idIn s
    go (HForce pat scrut body) =
      go scrut || (not (patBinds name pat) && go body)
    go (HCase scrut arms) =
      go scrut || or [ not (strPatBinds name p) && go b | (p, b) <- arms ]
    go (HIf c t e) = idIn c || go t || go e
    go (HLet lhs rhs body) =
      let ws = words lhs
          fn = case ws of
            (f : _) -> f
            [] -> ""
          params = drop 1 ws
          rhsShadowed = name == fn || name `elem` params
      in (not rhsShadowed && go rhs) || go body

-- Rename a temp component inside a tuple pattern (or return Nothing).
renameTupComp :: [FPat] -> String -> String -> Maybe [FPat]
renameTupComp ps old new =
  if any match ps
  then Just (map ren ps)
  else Nothing
  where
    match (FpVar v) = v == old
    match _ = False
    ren (FpVar v) | v == old = FpVar new
    ren p = p

-- Peel hop-of-hop rebinds (peephole 1). Returns rewritten (pat, body).
-- Only when the intermediate temp is dead in the residual body — otherwise
-- the hop is a live copy, not a rename.
collapseHops :: FPat -> Hs -> (FPat, Hs)
collapseHops pat body = case (pat, body) of
  -- case E of !x -> case x of !y -> B  ==>  case E of !y -> B
  (FpVar x, HForce pat' (HExpr y) body')
    | isBareVar y && x == y
    , not (freeInHs x body')
    -> collapseHops pat' body'
  -- case E of !(…temps…) -> case temp of !real -> B  ==> rename temp→real in pat
  (FpTup ps, HForce (FpVar new) (HExpr old) body')
    | isBareVar old
    , not (freeInHs old body')
    , Just ps' <- renameTupComp ps old new
    -> collapseHops (FpTup ps') body'
  _ -> (pat, body)

isTailIdentity :: FPat -> Hs -> Bool
isTailIdentity (FpVar x) (HExpr y) = x == y
isTailIdentity _ _ = False

-- One peephole pass. Outer simplifyHs iterates to a fixed point.
--
-- Order matters: collapse hop-of-hop rebinds (peep 1) on the *unpeeled*
-- body *before* recursively peeping it. Otherwise peep 2 treats temps like
-- `_io0`/`_ret` as bare vars and substitutes them into the body, destroying
-- the hop shape peep 1 needs to rename the outer pattern to the real names
-- (`items`, `p`, …).
peepHs :: Hs -> Hs
peepHs (HExpr s) = HExpr s
peepHs (HForce pat scrut body) =
  let scrut' = peepHs scrut
      -- Peephole 1 first (top-down absorb of rebind hops).
      (pat1, body1) = collapseHops pat body
      -- Then simplify the residual body (and collapse anything peeping exposes).
      body' = peepHs body1
      (pat2, body2) = collapseHops pat1 body'
  in case (pat2, scrut') of
       -- Peephole 2: never case a bare variable — but only for pure renames.
       -- If `v` is still free in the body, this is a copy (`b = a` with both
       -- live); substituting would merge identities and break later rebinds.
       (FpWild, HExpr v)
         | isBareVar v -> body2
       (FpVar x, HExpr v)
         | isBareVar v && x == v -> body2
         | isBareVar v && not (freeInHs v body2) ->
             peepHs (substHs x v body2)
       -- Peephole 3: tail identity  case E of !x -> x  ==>  E
       _
         | isTailIdentity pat2 body2 -> scrut'
       _ -> HForce pat2 scrut' body2
peepHs (HCase scrut arms) =
  HCase (peepHs scrut) [(p, peepHs b) | (p, b) <- arms]
peepHs (HIf c t e) = HIf c (peepHs t) (peepHs e)
peepHs (HLet lhs rhs body) = HLet lhs (peepHs rhs) (peepHs body)

simplifyHs :: Hs -> Hs
simplifyHs h =
  let h' = peepHs h
  in if h' == h then h else simplifyHs h'

-- Force-bind smart constructor (un-simplified; simplifyHs runs later).
hForce :: FPat -> Hs -> Hs -> Hs
hForce = HForce

hForceE :: FPat -> String -> Hs -> Hs
hForceE pat e body = HForce pat (HExpr e) body

hForceE_ :: String -> Hs -> Hs
hForceE_ e body = hForceE FpWild e body

-- Pretty-print Hs back to layout-rule Haskell.
-- Parenthesize multi-line force-bind scrutinees only: bangBind does not.
-- HCase goes through caseOf, which already parenthesizes multi-line scrutinees
-- — do not double-wrap.
renderForceScrut :: Hs -> String
renderForceScrut (HExpr s) = s
renderForceScrut other =
  let r = renderHs other
  in if '\n' `elem` r
     then "(\n" ++ indentAll indN r ++ "\n)"
     else r

renderHs :: Hs -> String
renderHs (HExpr s) = s
renderHs (HForce pat scrut body) =
  bangBind (renderFPat pat) (renderForceScrut scrut) (renderHs body)
renderHs (HCase scrut arms) =
  caseOf (renderHs scrut) [(p, renderHs b) | (p, b) <- arms]
renderHs (HIf c t e) =
  layoutIf c (renderHs t) (renderHs e)
renderHs (HLet lhs rhs body) =
  prettyLet lhs (renderHs rhs) (renderHs body)

-- Compile then simplify then render (entry for function/test bodies).
emitBody :: Ctx -> [IrStmt] -> String
emitBody ctx stmts = renderHs (simplifyHs (compileBlock ctx stmts))

-- ===========================================================================
-- Expression emission
-- ===========================================================================

-- Atomic / already-delimited forms that need no extra parens as function args.
isAtomicExpr :: IrExpr -> Bool
isAtomicExpr e = case eKind e of
  EInt _ -> True
  EFloat _ -> True
  EBool _ -> True
  EText _ -> True
  ELocal _ -> True
  EConst _ -> True
  EFuncRef _ -> True
  EList _ -> True
  ETuple _ -> True
  _ -> False

-- Infix / boolean forms that need parens when used as a function argument
-- or as an operand of another infix at equal-or-looser precedence.
isInfixy :: IrExpr -> Bool
isInfixy e = case eKind e of
  EBinary {} -> True
  _ -> False

-- Peephole 4 helpers: bare numerals where a monomorphic context pins Int64;
-- keep `(n :: Int64)` only when the site is polymorphic / unpinned.
emitIntBare :: Int64 -> String
emitIntBare n = show n

emitIntAnn :: Int64 -> String
emitIntAnn n = "(" ++ show n ++ " :: Int64)"

emitExpr :: Ctx -> IrExpr -> String
emitExpr ctx e = case eKind e of
  -- Default: annotated. Monomorphic call/arg sites use emitArgBareInt.
  EInt n -> emitIntAnn n
  EFloat f
    | isNaN f -> "(0/0 :: Double)"
    | isInfinite f && f > 0 -> "(1/0 :: Double)"
    | isInfinite f -> "((-1)/0 :: Double)"
    | otherwise -> "(" ++ show f ++ " :: Double)"
  EBool True -> "True"
  EBool False -> "False"
  -- Text codepoints are [Int64]; bare digits need a pin (list ascription).
  EText cps ->
    "([" ++ intercalate ", " (map show cps) ++ "] :: [Int64])"
  ELocal n -> mangleValue n
  EConst n ->
    let (mq, cn) = splitQual n
    in case mq of
      Just m -> mangleModule m ++ "." ++ mangleValue cn
      Nothing -> mangleValue cn
  EFuncRef n ->
    let (mq, fn) = splitQual n
    in case mq of
      Just m -> mangleModule m ++ "." ++ mangleValue fn
      Nothing -> mangleValue fn
  EList xs ->
    let elems = map (emitListElem ctx) xs
        body = "[" ++ intercalate ", " elems ++ "]"
    in case eTy e of
         -- Pin literal / underdetermined list elems (assertEq, binds, etc.).
         TList TInt -> "(" ++ body ++ " :: [Int64])"
         TList TFloat -> "(" ++ body ++ " :: [Double])"
         _ -> body
  ETuple [] -> "()"
  ETuple xs -> "(" ++ intercalate ", " (map (emitExpr ctx) xs) ++ ")"
  ECallFunc name args ->
    emitCallName name ++ concatMap (\a -> " " ++ emitArg ctx a) args
  ECallValue cal args ->
    emitArg ctx cal ++ concatMap (\a -> " " ++ emitArg ctx a) args
  ENewRecord name args ->
    mangleType name ++ concatMap (\a -> " " ++ emitArg ctx a) args
  ENewVariant en vn args
    | en == "Option" && vn == "Some" ->
        "Rt.SSome " ++ emitArg ctx (head args)
    | en == "Option" && vn == "None" -> "Rt.SNone"
    | en == "Result" && vn == "Ok" ->
        "Rt.SOk " ++ emitArg ctx (head args)
    | en == "Result" && vn == "Err" ->
        "Rt.SErr " ++ emitArg ctx (head args)
    | otherwise ->
        mangleVariant en vn ++ concatMap (\a -> " " ++ emitArg ctx a) args
  EBuiltin b args -> emitBuiltin ctx b args
  EMutBuiltin {} ->
    "error \"internal: MutBuiltin reached emitExpr; hoist failed\""
  EGetField recv name ->
    case eTy recv of
      TRecord rn -> mangleField rn name ++ " " ++ emitArg ctx recv
      _ -> mangleField "?" name ++ " " ++ emitArg ctx recv
  EIndex recv idx ->
    case eTy recv of
      -- Map keys are polymorphic — pin int literals (peep 4 keeps ann here).
      TMap _ _ -> "Rt.mapGet " ++ emitArg ctx recv ++ " " ++ emitAssertArg ctx idx
      _ -> "Rt.at " ++ emitArg ctx recv ++ " " ++ emitArg ctx idx
  EUnary op o -> case op of
    UNeg ->
      if eTy o == TInt
      then "Rt.negI " ++ emitArg ctx o
      else "negate " ++ emitArg ctx o
    UNot -> "not " ++ emitArg ctx o
  EBinary op l r -> emitBinary ctx op l r

-- List elements: bare ints inside an ascribed [Int64] list; else normal.
emitListElem :: Ctx -> IrExpr -> String
emitListElem ctx e = case eKind e of
  EInt n -> emitIntBare n
  _ -> emitExpr ctx e

-- Function-argument position: monomorphic callee signatures pin Int64, so
-- integer literals drop their annotation here (peephole 4). Non-atoms still
-- get parentheses. Polymorphic sites (e.g. sudoAssertEq) use emitAssertArg.
emitArg :: Ctx -> IrExpr -> String
emitArg ctx e = case eKind e of
  EInt n -> emitIntBare n
  _ | isAtomicExpr e -> emitExpr ctx e
  _ -> "(" ++ emitExpr ctx e ++ ")"

-- Polymorphic assertion operands: keep Int64 annotations on bare literals.
emitAssertArg :: Ctx -> IrExpr -> String
emitAssertArg ctx e
  | isAtomicExpr e = emitExpr ctx e
  | otherwise = "(" ++ emitExpr ctx e ++ ")"

-- Infix operand: parenthesize nested infix (and nothing else — app binds tighter).
emitOperand :: Ctx -> IrExpr -> String
emitOperand ctx e
  | isInfixy e = "(" ++ emitExpr ctx e ++ ")"
  | otherwise = emitExpr ctx e

emitCallName :: String -> String
emitCallName name =
  let (mq, fn) = splitQual name
  in case mq of
    Just m -> mangleModule m ++ "." ++ mangleValue fn
    Nothing -> mangleValue fn

paren :: String -> String
paren s = "(" ++ s ++ ")"

-- Parenthesize a rendered string only when it is not already delimited and
-- contains spaces (i.e. looks like an application or infix form).
parenIfApp :: String -> String
parenIfApp s
  | null s = s
  | head s == '(' || head s == '[' = s
  | ' ' `notElem` s = s
  | otherwise = "(" ++ s ++ ")"

emitBinary :: Ctx -> BinaryOp -> IrExpr -> IrExpr -> String
emitBinary ctx op l r =
  let lt = eTy l
      le = emitOperand ctx l
      re = emitOperand ctx r
      la = emitArg ctx l
      ra = emitArg ctx r
  in case op of
    BAdd | lt == TInt -> "Rt.chkAdd " ++ la ++ " " ++ ra
    BAdd | isListTy lt -> le ++ " ++ " ++ re
    BAdd -> le ++ " + " ++ re  -- float
    BSub | lt == TInt -> "Rt.chkSub " ++ la ++ " " ++ ra
    BSub -> le ++ " - " ++ re
    BMul | lt == TInt -> "Rt.chkMul " ++ la ++ " " ++ ra
    BMul -> le ++ " * " ++ re
    BDiv | lt == TInt -> "Rt.divI " ++ la ++ " " ++ ra
    BDiv -> "Rt.fdiv " ++ la ++ " " ++ ra
    BMod -> "Rt.modI " ++ la ++ " " ++ ra
    BLt -> le ++ " < " ++ re
    BLe -> le ++ " <= " ++ re
    BGt -> le ++ " > " ++ re
    BGe -> le ++ " >= " ++ re
    BEq -> le ++ " == " ++ re
    BNe -> le ++ " /= " ++ re
    BAnd -> le ++ " && " ++ re
    BOr -> le ++ " || " ++ re

isListTy :: Ty -> Bool
isListTy (TList _) = True
isListTy _ = False

emitBuiltin :: Ctx -> Builtin -> [IrExpr] -> String
emitBuiltin ctx b args =
  let -- Monomorphic Int64 / container positions: bare int literals (peep 4).
      a i = emitArg ctx (args !! i)
      -- Polymorphic value positions: keep Int64 ann via emitExpr.
      p i = emitAssertArg ctx (args !! i)
  in case b of
    AbsInt -> "Rt.absI " ++ a 0
    AbsFloat -> "abs " ++ a 0
    MinInt -> "Rt.minI " ++ a 0 ++ " " ++ a 1
    MaxInt -> "Rt.maxI " ++ a 0 ++ " " ++ a 1
    MinFloat -> "Rt.fmin " ++ a 0 ++ " " ++ a 1
    MaxFloat -> "Rt.fmax " ++ a 0 ++ " " ++ a 1
    FloatOfInt -> "Rt.floatOfInt " ++ a 0
    IntOfFloat -> "Rt.intOfFloat " ++ a 0
    Floor -> "Rt.floorF " ++ a 0
    Ceil -> "Rt.ceilF " ++ a 0
    Round -> "Rt.roundHalfAway " ++ a 0
    Sqrt -> "Rt.sqrtF " ++ a 0
    -- filledL :: Int64 -> a -> [a] — value is polymorphic.
    Filled -> "Rt.filledL " ++ a 0 ++ " " ++ p 1
    NewMap -> "Rt.mapNew"
    NewSet -> "Rt.setNew"
    ListLength -> "Rt.listLen " ++ a 0
    MapSize -> "Rt.mapSize " ++ a 0
    -- key may be Int64-pinned by map type; value paths use p when needed.
    MapGet -> "Rt.mapGetOpt " ++ a 0 ++ " " ++ p 1
    MapHas -> "Rt.mapHas " ++ a 0 ++ " " ++ p 1
    MapKeys -> "Rt.mapKeysL " ++ a 0
    MapValues -> "Rt.mapValuesL " ++ a 0
    SetSize -> "Rt.setSize " ++ a 0
    SetHas -> "Rt.setHas " ++ a 0 ++ " " ++ p 1
    SetItems -> "Rt.setItemsL " ++ a 0
    OptIsSome -> "Rt.optIsSome " ++ a 0
    OptIsNone -> "Rt.optIsNone " ++ a 0
    OptUnwrap -> "Rt.optUnwrap " ++ a 0
    OptGetOr -> "Rt.optGetOr " ++ a 0 ++ " " ++ p 1
    ResIsOk -> "Rt.resIsOk " ++ a 0
    ResIsErr -> "Rt.resIsErr " ++ a 0
    ResUnwrap -> "Rt.resUnwrap " ++ a 0
    ResGetOr -> "Rt.resGetOr " ++ a 0 ++ " " ++ p 1
    -- Mutating builtins shouldn't appear as pure Builtin
    ListAppend -> error "ListAppend is MutBuiltin"
    ListPop -> error "ListPop is MutBuiltin"
    ListInsert -> error "ListInsert is MutBuiltin"
    ListRemoveAt -> error "ListRemoveAt is MutBuiltin"
    ListSwap -> error "ListSwap is MutBuiltin"
    ListSort -> error "ListSort is MutBuiltin"
    MapDelete -> error "MapDelete is MutBuiltin"
    SetAdd -> error "SetAdd is MutBuiltin"
    SetRemove -> error "SetRemove is MutBuiltin"

-- ===========================================================================
-- Place helpers
-- ===========================================================================

placeRoot :: Place -> String
placeRoot (PVar n) = n
placeRoot (PIndex b _ _) = placeRoot b
placeRoot (PField b _ _) = placeRoot b

-- Read place as expression
emitPlaceGet :: Ctx -> Place -> String
emitPlaceGet ctx = \case
  PVar n -> mangleValue n
  PIndex b bt i ->
    let baseA = parenIfApp (emitPlaceGet ctx b)
        -- List indices are monomorphic Int64; map keys are polymorphic.
        idxA = case bt of
          TMap _ _ -> emitAssertArg ctx i
          _ -> emitArg ctx i
    in case bt of
      TMap _ _ -> "Rt.mapGet " ++ baseA ++ " " ++ idxA
      TList _ -> "Rt.at " ++ baseA ++ " " ++ idxA
      _ -> "Rt.at " ++ baseA ++ " " ++ idxA
  PField b (TRecord rn) n ->
    mangleField rn n ++ " " ++ parenIfApp (emitPlaceGet ctx b)
  PField b _ n ->
    "/*field*/ " ++ n ++ " " ++ parenIfApp (emitPlaceGet ctx b)

-- Rebuild root after setting place to `valExpr`
emitPlaceSet :: Ctx -> Place -> String -> String
emitPlaceSet ctx place valExpr = go place valExpr
  where
    go (PVar _) v = v
    go (PIndex b bt i) v =
      let baseE = emitPlaceGet ctx b
          idxA = case bt of
            TMap _ _ -> emitAssertArg ctx i
            _ -> emitArg ctx i
          vA = parenIfApp v
          newBase = case bt of
            TMap _ _ -> "Rt.mapPut " ++ parenIfApp baseE ++ " " ++ idxA ++ " " ++ vA
            _ -> "Rt.putL " ++ parenIfApp baseE ++ " " ++ idxA ++ " " ++ vA
      in go b newBase
    go (PField b (TRecord rn) n) v =
      let baseE = emitPlaceGet ctx b
          fld = mangleField rn n
          newBase = parenIfApp baseE ++ " { " ++ fld ++ " = " ++ v ++ " }"
      in go b newBase
    go (PField b _ n) v =
      let baseE = emitPlaceGet ctx b
          newBase = parenIfApp baseE ++ " { " ++ n ++ " = " ++ v ++ " }"
      in go b newBase

-- ===========================================================================
-- Statement compilation
-- ===========================================================================

-- Collect mutated outer vars for a loop body
collectThreaded :: [IrStmt] -> Set.Set String -> [String]
collectThreaded body declaredInLoop =
  nub (goStmts body)
  where
    goStmts = concatMap goStmt
    goStmt = \case
      SAssign (PVar n) _ False | n `Set.notMember` declaredInLoop -> [n]
      SAssign p _ False ->
        let r = placeRoot p
        in if r `Set.notMember` declaredInLoop then [r] else []
      SAssign (PVar n) _ True -> []  -- declares
      SAssign {} -> []
      STupleAssign ts ds _ ->
        [ t | (t, d) <- zip ts ds, not d, t `Set.notMember` declaredInLoop ]
      SExpr e -> inoutTargets e
      SIf arms elseB ->
        concatMap (goStmts . snd) arms ++ maybe [] goStmts elseB
      SWhile _ b -> goStmts b
      SForRange _ _ _ _ b -> goStmts b
      SForIn _ _ b -> goStmts b
      SMatch _ arms -> concatMap (\(IrMatchArm _ b) -> goStmts b) arms
      _ -> []
    inoutTargets (IrExpr _ (ECallFunc name args)) = []  -- handled at stmt with lookup
    inoutTargets _ = []

-- Better: walk statements for assigns with declares:false and inout writebacks
collectThreadedFull :: Ctx -> [IrStmt] -> [String]
collectThreadedFull ctx body =
  let declared = Set.fromList (collectDeclared body)
  in nub (go body declared)
  where
    go stmts decl = concatMap (\s -> goOne s decl) stmts
    goOne s decl = case s of
      SAssign p v False ->
        let r = placeRoot p
            fromVal = mutRoots decl v ++ inoutFromExpr decl v
        in (if r `Set.member` decl then [] else [r]) ++ fromVal
      SAssign p v True ->
        -- declares may still wrap MutBuiltin value (e.g. x = xs.pop())
        mutRoots decl v ++ inoutFromExpr decl v
      STupleAssign ts ds v ->
        [ t | (t, d) <- zip ts ds, not d, t `Set.notMember` decl ]
        ++ mutRoots decl v ++ inoutFromExpr decl v
      SExpr e -> mutRoots decl e ++ inoutFromExpr decl e
      SIf arms elseB ->
        concatMap (\(_, b) -> go b decl) arms ++ maybe [] (\b -> go b decl) elseB
      SWhile _ b -> go b decl
      SForRange _ _ _ _ b -> go b decl
      SForIn _ _ b -> go b decl
      SMatch _ arms -> concatMap (\(IrMatchArm _ b) -> go b decl) arms
      SExpectTrap _ b _ -> go b decl
      _ -> []
    mutRoots dcl (IrExpr _ (EMutBuiltin _ recv _ _)) =
      let r = placeRoot recv
      in if r `Set.member` dcl then [] else [r]
    mutRoots _ _ = []
    inoutFromExpr dcl (IrExpr _ (ECallFunc name args)) =
      case lookupFunc ctx name of
        Just f ->
          let paramArgs = zip (fParams f) args
          in [ placeRootOfExpr a
             | (p, a) <- paramArgs, pInout p
             , placeRootOfExpr a `Set.notMember` dcl
             , placeRootOfExpr a /= "?"
             ]
        Nothing -> []
    inoutFromExpr _ _ = []

-- Collect names first-declared in a block (for excluding from thread list)
collectDeclared :: [IrStmt] -> [String]
collectDeclared = concatMap $ \case
  SAssign (PVar n) _ True -> [n]
  STupleAssign ts ds _ -> [ t | (t, d) <- zip ts ds, d ]
  SIf arms elseB ->
    concatMap (collectDeclared . snd) arms ++ maybe [] collectDeclared elseB
  SWhile _ b -> collectDeclared b
  SForRange v _ _ _ b -> v : collectDeclared b
  SForIn vs _ b -> vs ++ collectDeclared b
  SMatch _ arms -> concatMap (\(IrMatchArm p b) -> patBinders p ++ collectDeclared b) arms
  SExpectTrap _ b _ -> collectDeclared b
  _ -> []

patBinders :: IrPattern -> [String]
patBinders (PatVariant _ _ bs) = bs
patBinders _ = []

-- Also collect inout writeback targets from call statements
collectInoutWritebacks :: Ctx -> [IrStmt] -> [String]
collectInoutWritebacks ctx = concatMap go
  where
    go = \case
      SAssign _ v _ -> fromExpr v
      SExpr e -> fromExpr e
      SIf arms elseB -> concatMap (goList . snd) arms ++ maybe [] goList elseB
      SWhile _ b -> goList b
      SForRange _ _ _ _ b -> goList b
      SForIn _ _ b -> goList b
      SMatch _ arms -> concatMap (\(IrMatchArm _ b) -> goList b) arms
      SExpectTrap _ b _ -> goList b
      _ -> []
    goList = concatMap go
    fromExpr (IrExpr _ (ECallFunc name args)) =
      case lookupFunc ctx name of
        Just f ->
          let ios = [p | p <- fParams f, pInout p]
              -- pair with args of same index among all params
              paramArgs = zip (fParams f) args
          in mapMaybe (\(p, a) ->
                if pInout p then Just (placeRootOfExpr a) else Nothing) paramArgs
        Nothing -> []
    fromExpr _ = []

placeRootOfExpr :: IrExpr -> String
placeRootOfExpr (IrExpr _ (ELocal n)) = n
placeRootOfExpr (IrExpr _ (EGetField r _)) = placeRootOfExpr r
placeRootOfExpr (IrExpr _ (EIndex r _)) = placeRootOfExpr r
placeRootOfExpr _ = "?"

loopThreadedVars :: Ctx -> [IrStmt] -> [String]
loopThreadedVars ctx body =
  let declared = Set.fromList (collectDeclared body)
      assigns = collectThreadedFull ctx body
      ios = collectInoutWritebacks ctx body
      allv = nub (assigns ++ ios)
  in filter (`Set.notMember` declared) allv

-- Build vars tuple expression from current bindings
varsTupleExpr :: [String] -> String
varsTupleExpr [] = "()"
varsTupleExpr [v] = mangleValue v
varsTupleExpr vs = "(" ++ intercalate ", " (map mangleValue vs) ++ ")"

varsTuplePat :: [String] -> String
varsTuplePat [] = "()"
varsTuplePat [v] = mangleValue v
varsTuplePat vs = "(" ++ intercalate ", " (map mangleValue vs) ++ ")"

-- FRet construction from optional return expr
buildFRet :: Ctx -> Maybe IrExpr -> String
buildFRet ctx mret =
  let retParts = case mret of
        Just e -> [emitExpr ctx e]
        Nothing ->
          -- void: only inouts, or ()
          []
      ioParts = map mangleValue (ctxInouts ctx)
      parts = case (mret, ctxInouts ctx) of
        (Just e, []) -> [emitExpr ctx e]
        (Nothing, []) -> ["()"]
        (Nothing, [io]) -> [mangleValue io]
        (Just e, ios) -> emitExpr ctx e : map mangleValue ios
        (Nothing, ios) -> map mangleValue ios
  in case parts of
    [p] -> p
    ps -> "(" ++ intercalate ", " ps ++ ")"

-- Hoist MutBuiltin (and nested) out of expressions into forced binds.
-- Returns (wrapper, pure-expr, updated-ctx). Wrapper is Hs -> Hs.
type Hoist = (Hs -> Hs, IrExpr, Ctx)

freshName :: Ctx -> String -> (String, Ctx)
freshName ctx prefix =
  let n = ctxFresh ctx
  in (prefix ++ show n, ctx { ctxFresh = n + 1 })

-- MutBuiltin arg emission: Int64 index positions are monomorphic (bare ok);
-- element/key/value positions are polymorphic and need Int64 pins (peep 4).
emitMutArg :: Ctx -> Builtin -> Int -> IrExpr -> String
emitMutArg ctx b i arg = case b of
  ListSwap -> emitArg ctx arg          -- both indices Int64
  ListRemoveAt -> emitArg ctx arg      -- index Int64
  ListInsert | i == 0 -> emitArg ctx arg  -- index
  ListInsert -> emitAssertArg ctx arg     -- value
  ListAppend -> emitAssertArg ctx arg
  SetAdd -> emitAssertArg ctx arg
  SetRemove -> emitAssertArg ctx arg
  MapDelete -> emitAssertArg ctx arg
  _ -> emitAssertArg ctx arg

hoistExpr :: Ctx -> IrExpr -> Hoist
hoistExpr ctx e = case eKind e of
  EMutBuiltin b recv rty args ->
    let (argsW, args', ctx1) = hoistMany ctx args
        (tmp, ctx2) = freshName ctx1 "_hm"
        root = placeRoot recv
        recvE = emitPlaceGet ctx2 recv
        a i = emitMutArg ctx2 b i (args' !! i)
        (call, hasVal) = mutCall b recvE rty a
        setRoot = emitPlaceSet ctx2 recv "_newRecv"
        bindPat =
          if hasVal
          then FpTup [FpVar "_newRecv", FpVar tmp]
          else FpTup [FpVar "_newRecv", FpWild]
        wrap cont =
          argsW $
          hForceE bindPat call $
          hForceE (FpVar (mangleValue root)) setRoot $
          cont
        e'' = if hasVal
              then IrExpr (eTy e) (ELocal tmp)  -- mangleValue on _hm0 stays _hm0
              else IrExpr (eTy e) (EBool True)  -- unit-ish; shouldn't be read
    in (wrap, e'', ctx2)
  EList xs ->
    let (w, xs', c) = hoistMany ctx xs
    in (w, e { eKind = EList xs' }, c)
  ETuple xs ->
    let (w, xs', c) = hoistMany ctx xs
    in (w, e { eKind = ETuple xs' }, c)
  ECallFunc name args ->
    let (w, args', c) = hoistMany ctx args
    in (w, e { eKind = ECallFunc name args' }, c)
  ECallValue cal args ->
    let (w1, cal', c1) = hoistExpr ctx cal
        (w2, args', c2) = hoistMany c1 args
    in (\k -> w1 (w2 k), e { eKind = ECallValue cal' args' }, c2)
  ENewRecord name args ->
    let (w, args', c) = hoistMany ctx args
    in (w, e { eKind = ENewRecord name args' }, c)
  ENewVariant en vn args ->
    let (w, args', c) = hoistMany ctx args
    in (w, e { eKind = ENewVariant en vn args' }, c)
  EBuiltin b args ->
    let (w, args', c) = hoistMany ctx args
    in (w, e { eKind = EBuiltin b args' }, c)
  EGetField recv name ->
    let (w, recv', c) = hoistExpr ctx recv
    in (w, e { eKind = EGetField recv' name }, c)
  EIndex recv idx ->
    let (w1, recv', c1) = hoistExpr ctx recv
        (w2, idx', c2) = hoistExpr c1 idx
    in (\k -> w1 (w2 k), e { eKind = EIndex recv' idx' }, c2)
  EUnary op o ->
    let (w, o', c) = hoistExpr ctx o
    in (w, e { eKind = EUnary op o' }, c)
  EBinary op l r ->
    let (w1, l', c1) = hoistExpr ctx l
        (w2, r', c2) = hoistExpr c1 r
    in (\k -> w1 (w2 k), e { eKind = EBinary op l' r' }, c2)
  _ -> (id, e, ctx)

hoistMany :: Ctx -> [IrExpr] -> (Hs -> Hs, [IrExpr], Ctx)
hoistMany ctx [] = (id, [], ctx)
hoistMany ctx (x : xs) =
  let (w1, x', c1) = hoistExpr ctx x
      (w2, xs', c2) = hoistMany c1 xs
  in (\k -> w1 (w2 k), x' : xs', c2)

mutCall :: Builtin -> String -> Ty -> (Int -> String) -> (String, Bool)
mutCall b recvE rty a =
  let recvA = parenIfApp recvE
  in case b of
    ListAppend -> ("Rt.appendL " ++ recvA ++ " " ++ a 0, False)
    ListPop -> ("Rt.popL " ++ recvA, True)
    ListInsert -> ("Rt.insertL " ++ recvA ++ " " ++ a 0 ++ " " ++ a 1, False)
    ListRemoveAt -> ("Rt.removeAtL " ++ recvA ++ " " ++ a 0, True)
    ListSwap -> ("Rt.swapL " ++ recvA ++ " " ++ a 0 ++ " " ++ a 1, False)
    ListSort ->
      case rty of
        TList TFloat -> ("Rt.sortFloatsL " ++ recvA, False)
        _ -> ("Rt.sortL " ++ recvA, False)
    MapDelete -> ("Rt.mapDelete " ++ recvA ++ " " ++ a 0, True)
    SetAdd -> ("Rt.setAdd " ++ recvA ++ " " ++ a 0, True)
    SetRemove -> ("Rt.setRemove " ++ recvA ++ " " ++ a 0, True)
    _ -> ("error \"not mut\"", False)

-- Compile statement list → body IR (relative indent 0 when rendered).
compileBlock :: Ctx -> [IrStmt] -> Hs
compileBlock ctx [] =
  case ctxMode ctx of
    ExprMode -> HExpr (buildFRet ctx Nothing)
    LoopMode -> HExpr ("Rt.Cont " ++ varsTupleExpr (ctxLoopVars ctx))
compileBlock ctx (s : rest) = compileStmt ctx s rest

compileStmt :: Ctx -> IrStmt -> [IrStmt] -> Hs
compileStmt ctx s rest = case s of
  SSkip -> compileBlock ctx rest
  SBreak ->
    case ctxMode ctx of
      LoopMode -> HExpr ("Rt.Brk " ++ varsTupleExpr (ctxLoopVars ctx))
      ExprMode -> HExpr "error \"break outside loop\""
  SContinue ->
    case ctxMode ctx of
      LoopMode -> HExpr ("Rt.Cont " ++ varsTupleExpr (ctxLoopVars ctx))
      ExprMode -> HExpr "error \"continue outside loop\""
  SReturn mret ->
    let fr = buildFRet ctx mret
    in case ctxMode ctx of
      ExprMode -> HExpr fr
      LoopMode -> HExpr ("Rt.Ret " ++ parenIfApp fr)

  SAssert cond line ->
    let (w, cond', ctx') = hoistExpr ctx cond
        -- sudoAssertEq is polymorphic (Eq a, Canon a) — use emitExpr so bare
        -- int literals keep their Int64 pin (peephole 4 keeps ann here).
        body = case eKind cond' of
          EBinary BEq l r ->
            "Rt.sudoAssertEq " ++ emitAssertArg ctx' l ++ " "
              ++ emitAssertArg ctx' r ++ " " ++ show line
          _ ->
            "Rt.sudoAssert " ++ emitAssertArg ctx' cond' ++ " " ++ show line
        cont = compileBlock ctx' rest
    in w (hForceE_ body cont)

  SAssign target value declares ->
    case eKind value of
      ECallFunc name args | isInoutCall ctx name ->
        emitInoutCall ctx (Just target) name args rest
      EMutBuiltin b recv rty args ->
        emitMutBuiltin ctx (Just target) b recv rty args rest
      _ ->
        let (w, value', ctx') = hoistExpr ctx value
            v = emitExpr ctx' value'
            root = placeRoot target
            setE = emitPlaceSet ctx' target v
            cont = compileBlock ctx' rest
        in w (hForceE (FpVar (mangleValue root)) setE cont)

  STupleAssign targets _ value ->
    let (w, value', ctx') = hoistExpr ctx value
        v = emitExpr ctx' value'
        pat = FpTup (map (FpVar . mangleValue) targets)
        cont = compileBlock ctx' rest
    in w (hForceE pat v cont)

  SExpr e ->
    case eKind e of
      ECallFunc name args | isInoutCall ctx name ->
        emitInoutCall ctx Nothing name args rest
      EMutBuiltin b recv rty args ->
        emitMutBuiltin ctx Nothing b recv rty args rest
      _ ->
        let (w, e', ctx') = hoistExpr ctx e
            cont = compileBlock ctx' rest
        in w (hForceE_ (emitExpr ctx' e') cont)

  SIf arms elseB ->
    let cont = compileBlock ctx rest
        -- splice rest into each arm so fallthrough continues after the if
        compileArm (c, b) = (emitExpr ctx c, compileBlock ctx (b ++ rest))
        compiledArms = map compileArm arms
        elseCode = case elseB of
          Just b -> compileBlock ctx (b ++ rest)
          Nothing -> cont
        chain = foldr (\(c, a) els -> HIf c a els) elseCode compiledArms
    in chain

  SMatch scrut arms ->
    let sc = HExpr (emitExpr ctx scrut)
        armPair (IrMatchArm pat body) =
          (emitPat ctx pat, compileBlock ctx (body ++ rest))
    in HCase sc (map armPair arms)

  SWhile cond body ->
    emitWhile ctx cond body rest

  SForRange var from to down body ->
    emitForRange ctx var from to down body rest

  SForIn vars iter body ->
    emitForIn ctx vars iter body rest

  SExpectTrap kind body line ->
    -- Tests route through compileTestIO; pure compileBlock path is a hard error.
    HExpr (emitExpectTrapPure ctx kind body line)

isInoutCall :: Ctx -> String -> Bool
isInoutCall ctx name =
  case lookupFunc ctx name of
    Just f -> any pInout (fParams f)
    Nothing -> False

emitPat :: Ctx -> IrPattern -> String
emitPat ctx = \case
  PatInt n -> show n
  PatBool True -> "True"
  PatBool False -> "False"
  PatWildcard -> "_"
  PatVariant en vn binders
    | en == "Option" && vn == "Some" ->
        "Rt.SSome " ++ maybe "_" mangleValue (listToMaybe binders)
    | en == "Option" && vn == "None" -> "Rt.SNone"
    | en == "Result" && vn == "Ok" ->
        "Rt.SOk " ++ maybe "_" mangleValue (listToMaybe binders)
    | en == "Result" && vn == "Err" ->
        "Rt.SErr " ++ maybe "_" mangleValue (listToMaybe binders)
    | null binders -> mangleVariant en vn
    | otherwise ->
        mangleVariant en vn ++ concatMap (\b -> " " ++ mangleValue b) binders

-- Rebuild a place-like expr with a new value at the leaf (field/index path).
rebuildFromExpr :: Ctx -> IrExpr -> String -> String
rebuildFromExpr _ctx e newV = go e newV
  where
    go expr v = case eKind expr of
      ELocal _ -> v
      EGetField recv name ->
        case eTy recv of
          TRecord rn ->
            let fld = mangleField rn name
                inner = case eKind recv of
                  ELocal n -> mangleValue n ++ " { " ++ fld ++ " = " ++ v ++ " }"
                  _ -> parenIfApp (emitExpr _ctx recv) ++ " { " ++ fld ++ " = " ++ v ++ " }"
            in go recv inner
      EIndex recv idx ->
        case eTy recv of
          TMap _ _ ->
            let inner = "Rt.mapPut " ++ emitArg _ctx recv ++ " "
                        ++ emitAssertArg _ctx idx ++ " " ++ parenIfApp v
            in go recv inner
          _ ->
            let inner = "Rt.putL " ++ emitArg _ctx recv ++ " "
                        ++ emitArg _ctx idx ++ " " ++ parenIfApp v
            in go recv inner
      _ -> v

writebackOne :: Ctx -> Int -> IrExpr -> Hs -> Hs
writebackOne ctx i a cont =
  let newV = "_io" ++ show i
      root = placeRootOfExpr a
      rebuilt = rebuildFromExpr ctx a newV
  in hForceE (FpVar (mangleValue root)) rebuilt cont

emitInoutCall :: Ctx -> Maybe Place -> String -> [IrExpr] -> [IrStmt] -> Hs
emitInoutCall ctx mtarget name args rest =
  case lookupFunc ctx name of
    Nothing -> HExpr ("error \"unknown func " ++ name ++ "\"")
    Just f ->
      let ios = [p | p <- fParams f, pInout p]
          hasRet = isJust (fRet f)
          nio = length ios
          call = emitCallName name ++ concatMap (\a -> " " ++ emitArg ctx a) args
          retPat = case (hasRet, nio) of
            (False, 0) -> FpTup []  -- unit; rendered specially below
            (False, 1) -> FpVar "_io0"
            (True, 0) -> FpVar "_ret"
            (True, n) -> FpTup (FpVar "_ret" : [FpVar ("_io" ++ show i) | i <- [0 .. n - 1]])
            (False, n) -> FpTup [FpVar ("_io" ++ show i) | i <- [0 .. n - 1]]
          -- Unit return with no inouts: force `()` pattern.
          retPatFinal = case (hasRet, nio) of
            (False, 0) -> FpWild  -- case call of !_ -> cont; call is ()
            _ -> retPat
          paramArgs = zip (fParams f) args
          ioArgs = [a | (p, a) <- paramArgs, pInout p]
          cont0 = compileBlock ctx rest
          cont1 = case mtarget of
            Just t | hasRet ->
              let root = placeRoot t
                  setE = emitPlaceSet ctx t "_ret"
              in hForceE (FpVar (mangleValue root)) setE cont0
            _ -> cont0
          cont2 = foldr (\(i, a) c -> writebackOne ctx i a c)
                        cont1
                        (zip [0 ..] ioArgs)
      in hForceE retPatFinal call cont2

emitMutBuiltin :: Ctx -> Maybe Place -> Builtin -> Place -> Ty -> [IrExpr] -> [IrStmt] -> Hs
emitMutBuiltin ctx mtarget b recv rty args rest =
  let (aw, args', ctx') = hoistMany ctx args
      recvE = emitPlaceGet ctx' recv
      a i = emitMutArg ctx' b i (args' !! i)
      root = placeRoot recv
      (call, hasVal) = mutCall b recvE rty a
      bindPat =
        if hasVal
        then FpTup [FpVar "_newRecv", FpVar "_mutRes"]
        else FpTup [FpVar "_newRecv", FpWild]
      setRoot = emitPlaceSet ctx' recv "_newRecv"
      cont0 = compileBlock ctx' rest
      cont1 = case mtarget of
        Just t | hasVal ->
          let r = placeRoot t
              se = emitPlaceSet ctx' t "_mutRes"
          in hForceE (FpVar (mangleValue r)) se cont0
        _ -> cont0
      cont2 = hForceE (FpVar (mangleValue root)) setRoot cont1
  in aw (hForceE bindPat call cont2)

-- Distinct recursive helper names for nested loops: go, go2, go3, ...
loopGoName :: Ctx -> (String, Ctx)
loopGoName ctx =
  let n = ctxFresh ctx
      name = if n == 0 then "go" else "go" ++ show (n + 1)
  in (name, ctx { ctxFresh = n + 1 })

-- Shared Flow match arms after a loop body runs once.
-- (Outer invoke uses retProp separately; inside go, Ret always re-wraps.)
flowArms :: String -> Hs -> [(String, Hs)]
flowArms contPat contBody =
  [ ("Rt.Cont " ++ contPat, contBody)
  , ("Rt.Brk s", HExpr "Rt.Brk s")
  , ("Rt.Ret r", HExpr "Rt.Ret r")
  ]

-- Match on go result: Brk continues after the loop; Ret propagates.
loopInvoke :: String -> String -> Hs -> String -> Hs
loopInvoke goCall contPat after retProp =
  HCase (HExpr goCall)
    [ ("Rt.Brk " ++ contPat, after)
    , ("Rt.Ret r", HExpr retProp)
    ]

emitWhile :: Ctx -> IrExpr -> [IrStmt] -> [IrStmt] -> Hs
emitWhile ctx cond body rest =
  let (go, ctxG) = loopGoName ctx
      vars = loopThreadedVars ctxG body
      ctxL = ctxG { ctxMode = LoopMode, ctxLoopVars = vars }
      bodyE = compileBlock ctxL body
      condE = emitExpr ctx cond
      goParams = unwords (map mangleValue vars)
      goArgs = unwords (map mangleValue vars)
      contPat = varsTuplePat vars
      after = compileBlock ctx rest
      retProp = case ctxMode ctx of
        ExprMode -> "r"
        LoopMode -> "Rt.Ret r"
      contCall =
        HExpr (go ++ (if null vars then "" else " " ++ unwords (map mangleValue vars)))
      goBody =
        HIf ("not (" ++ condE ++ ")")
          (HExpr ("Rt.Brk " ++ varsTupleExpr vars))
          (HCase bodyE (flowArms contPat contCall))
      lhs = go ++ (if null vars then "" else " " ++ goParams)
      goCall = go ++ (if null vars then "" else " " ++ goArgs)
  in HLet lhs goBody (loopInvoke goCall contPat after retProp)

rebindVars :: [String] -> String
rebindVars _ =
  -- Pattern-match on Rt.Brk already binds the threaded vars; do NOT
  -- emit `let !v = v` (that is a classic black-hole / <<loop>>).
  ""

emitForRange :: Ctx -> String -> IrExpr -> IrExpr -> Bool -> [IrStmt] -> [IrStmt] -> Hs
emitForRange ctx var from to down body rest =
  let (go, ctxG) = loopGoName ctx
      vars = loopThreadedVars ctxG body
      ctxL = ctxG { ctxMode = LoopMode, ctxLoopVars = vars }
      bodyE = compileBlock ctxL body
      fromE = emitExpr ctx from
      toE = emitExpr ctx to
      i = mangleValue var
      contPat = varsTuplePat vars
      after = compileBlock ctx rest
      retProp = case ctxMode ctx of
        ExprMode -> "r"
        LoopMode -> "Rt.Ret r"
      cmpEnd = if down then " < " else " > "
      step = if down then "(" ++ i ++ " - 1)" else "(" ++ i ++ " + 1)"
      goParams = unwords (i : map mangleValue vars)
      stepCall =
        HExpr (go ++ " " ++ step
          ++ (if null vars then "" else " " ++ unwords (map mangleValue vars)))
      contBody =
        HIf (i ++ " == _toV")
          (HExpr ("Rt.Brk " ++ varsTupleExpr vars))
          stepCall
      goBody =
        HIf (i ++ cmpEnd ++ "_toV")
          (HExpr ("Rt.Brk " ++ varsTupleExpr vars))
          (HCase bodyE (flowArms contPat contBody))
      goCall =
        go ++ " _fromV"
        ++ (if null vars then "" else " " ++ unwords (map mangleValue vars))
  in hForceE (FpVar "_fromV") fromE $
     hForceE (FpVar "_toV") toE $
     HLet (go ++ " " ++ goParams) goBody
       (loopInvoke goCall contPat after retProp)

emitForIn :: Ctx -> [String] -> IrExpr -> [IrStmt] -> [IrStmt] -> Hs
emitForIn ctx vars iter body rest =
  let (go, ctxG) = loopGoName ctx
      threaded = loopThreadedVars ctxG body
      ctxL = ctxG { ctxMode = LoopMode, ctxLoopVars = threaded }
      bodyE = compileBlock ctxL body
      iterE = emitExpr ctx iter
      snap = case eTy iter of
        TSet _ -> "Rt.setItemsL " ++ parenIfApp iterE
        TMap _ _ -> "M.toAscList " ++ parenIfApp iterE
        _ -> iterE
      contPat = varsTuplePat threaded
      after = compileBlock ctx rest
      retProp = case ctxMode ctx of
        ExprMode -> "r"
        LoopMode -> "Rt.Ret r"
      binders = case vars of
        [v] -> mangleValue v
        [k, v] -> "(" ++ mangleValue k ++ ", " ++ mangleValue v ++ ")"
        vs -> "(" ++ intercalate ", " (map mangleValue vs) ++ ")"
      goParams = unwords ("_remaining" : map mangleValue threaded)
      contCall =
        HExpr (go ++ " _xs"
          ++ (if null threaded then "" else " " ++ unwords (map mangleValue threaded)))
      goBody =
        HCase (HExpr "_remaining")
          [ ("[]", HExpr ("Rt.Brk " ++ varsTupleExpr threaded))
          , ( "(" ++ binders ++ " : _xs)"
            , HCase bodyE (flowArms contPat contCall)
            )
          ]
      goCall =
        go ++ " _items"
        ++ (if null threaded then "" else " " ++ unwords (map mangleValue threaded))
  in hForceE (FpVar "_items") snap $
     HLet (go ++ " " ++ goParams) goBody
       (loopInvoke goCall contPat after retProp)

emitExpectTrapPure :: Ctx -> String -> [IrStmt] -> Int -> String
emitExpectTrapPure ctx kind body line =
  -- This is only valid in tests compiled as IO; pure path shouldn't hit this
  "error \"expect_trap outside IO test\""

-- Wrap a (possibly multi-line) pure expression for evaluate / forceUnit.
wrapParenExpr :: String -> String
wrapParenExpr e
  | '\n' `elem` e = "(\n" ++ indentAll indN e ++ "\n)"
  | otherwise = "(" ++ e ++ ")"

-- Compile test body as IO ()
compileTestIO :: Ctx -> [IrStmt] -> String
compileTestIO ctx stmts =
  case findExpectTrap stmts of
    Nothing ->
      let pureE = emitBody ctx { ctxMode = ExprMode, ctxInouts = [], ctxLoopVars = [] } stmts
      in "Rt.forceUnit " ++ wrapParenExpr pureE
    Just _ ->
      compileTestIOStmts ctx stmts

findExpectTrap :: [IrStmt] -> Maybe IrStmt
findExpectTrap = find $ \case SExpectTrap {} -> True; _ -> False

-- Layout-rule expect_trap IO block.
-- Multi-line expressions must NOT appear as continuation lines of `_r <- …`
-- at the same indent as the do-statement (layout would treat the closing
-- parens as a new statement → parse error). Bind the pure body with `let`
-- first so the try/evaluate line stays single-line.
emitExpectTrapIO :: String -> String -> Int -> String -> String
emitExpectTrapIO kind bodyE line prefixE =
  let -- Lazy lets (no bang): traps must fire inside `evaluate`, not at bind.
      -- Fresh names → no recursive black-hole risk.
      -- RHS indent must exceed the binding-name column (`let _expectBody`
      -- starts the name at col 4 of the let line); otherwise layout treats
      -- the next line as a sibling declaration → parse error on `case`.
      letRhsInd = 8
      prefixLet =
        if null prefixE || prefixE == "()"
        then []
        else lines $
          "let _prefix =\n"
          ++ indentAll letRhsInd prefixE
      bodyLet =
        lines $
          "let _expectBody =\n"
          ++ indentAll letRhsInd bodyE
      tryBind =
        "_r <- Control.Exception.try (Control.Exception.evaluate _expectBody)"
        ++ " :: IO (Either Rt.SudoTrap ())"
      match =
        caseOf "_r"
          [ ( "Left (Rt.SudoTrap _k _) | _k == " ++ show kind
            , "return ()"
            )
          , ( "Left (Rt.SudoTrap _k _d)"
            , "Rt.trap \"AssertFailed\" "
              ++ "(\"line " ++ show line ++ ": expected trap "
              ++ kind ++ ", got \" ++ _k)"
            )
          , ( "Right _"
            , "Rt.trap \"AssertFailed\" "
              ++ "\"line " ++ show line
              ++ ": expected trap " ++ kind ++ ", but nothing trapped\""
            )
          ]
      -- Force pure prefix for side effects before the trap body.
      prefixForce =
        if null prefixLet
        then []
        else ["case _prefix of !_ -> return ()"]
      bodyLines =
        prefixLet ++ prefixForce ++ bodyLet ++ [tryBind] ++ lines match
  in "do\n" ++ intercalate "\n" [ replicate indN ' ' ++ l | l <- bodyLines ]

compileTestIOStmts :: Ctx -> [IrStmt] -> String
compileTestIOStmts ctx [] = "return ()"
compileTestIOStmts ctx (s : rest) = case s of
  SExpectTrap kind body line ->
    let bodyE = emitBody ctx { ctxMode = ExprMode, ctxInouts = [], ctxLoopVars = [] } body
    in emitExpectTrapIO kind bodyE line ""
  _other ->
    let purePrefix = takeWhile (not . isExpect) (s : rest)
        mtrap = dropWhile (not . isExpect) (s : rest)
    in case mtrap of
      (SExpectTrap kind body line : _) ->
        let prefixE = if null purePrefix
                      then "()"
                      else emitBody ctx { ctxMode = ExprMode, ctxInouts = [], ctxLoopVars = [] }
                             purePrefix
            bodyE = emitBody ctx { ctxMode = ExprMode, ctxInouts = [], ctxLoopVars = [] } body
        in emitExpectTrapIO kind bodyE line prefixE
      _ ->
        let pureE = emitBody ctx { ctxMode = ExprMode, ctxInouts = [], ctxLoopVars = [] } (s : rest)
        in "Rt.forceUnit " ++ wrapParenExpr pureE
  where
    isExpect (SExpectTrap {}) = True
    isExpect _ = False

-- ===========================================================================
-- Module emission
-- ===========================================================================

emitModule :: [IrModule] -> IrModule -> String
emitModule allMods m =
  let modName = mangleModule (mName m)
      imports = mImports m
      hdr =
        [ "{-# LANGUAGE BangPatterns #-}"
        , "{-# LANGUAGE RecordWildCards #-}"
        , "-- Generated by sudoc haskell backend from " ++ mName m ++ ".sudo"
        , "module " ++ modName ++ " where"
        , ""
        , "import Data.Int (Int64)"
        , "import qualified Data.Map.Strict as M"
        , "import qualified Data.Set as S"
        , "import qualified SudoRt as Rt"
        ]
          ++ [ "import qualified " ++ mangleModule i | i <- imports ]
          ++ [""]
      records = concatMap emitRecord (mRecords m)
      enums = concatMap emitEnum (mEnums m)
      consts = concatMap (emitConst m allMods) (mConsts m)
      funcs = concatMap (emitFunc m allMods) (mFuncs m)
  in unlines (hdr ++ records ++ enums ++ consts ++ funcs)

emitRecord :: IrRecord -> [String]
emitRecord (IrRecord name fields) =
  let tn = mangleType name
      flds = [mangleField name fn ++ " :: " ++ renderTy ty | (fn, ty) <- fields]
      -- Multi-field records get one field per line; empty/nullary stay compact.
      dataLines = case flds of
        [] -> ["data " ++ tn ++ " = " ++ tn ++ " deriving (Eq, Ord, Show)"]
        [one]
          | length one < 60 ->
              [ "data " ++ tn ++ " = " ++ tn ++ " { " ++ one ++ " } deriving (Eq, Ord, Show)" ]
        _ ->
              [ "data " ++ tn ++ " = " ++ tn ++ " {" ]
              ++ [ replicate indN ' ' ++ f ++ comma
                 | (f, comma) <- zip flds (replicate (length flds - 1) "," ++ [""])
                 ]
              ++ [ "  } deriving (Eq, Ord, Show)" ]
      fieldCanons =
        [ "Rt.canon (" ++ mangleField name fn ++ " r)" | (fn, _) <- fields ]
      canonBody =
        if null fields
        then "Rt.canonRecord " ++ show name ++ " []"
        else "Rt.canonRecord " ++ show name ++ " ["
             ++ intercalate ", " fieldCanons ++ "]"
  in dataLines
     ++ [ "instance Rt.Canon " ++ tn ++ " where"
        , replicate indN ' ' ++ "canon r = " ++ canonBody
        , ""
        ]

emitEnum :: IrEnum -> [String]
emitEnum (IrEnum name variants) =
  let tn = mangleType name
      cons = [ emitCon name vn fields | (vn, fields) <- variants ]
      arms = map (emitCanonArm name) variants
      -- Multi-constructor enums: one constructor per line.
      dataLines = case cons of
        [] -> ["data " ++ tn ++ " deriving (Eq, Ord, Show)"]
        [c]
          | length c < 70 ->
              [ "data " ++ tn ++ " = " ++ c ++ " deriving (Eq, Ord, Show)" ]
        (c0 : cs) ->
              [ "data " ++ tn ]
              ++ [ replicate indN ' ' ++ "= " ++ c0 ]
              ++ [ replicate indN ' ' ++ "| " ++ c | c <- cs ]
              ++ [ replicate indN ' ' ++ "deriving (Eq, Ord, Show)" ]
  in dataLines
     ++ [ "instance Rt.Canon " ++ tn ++ " where" ]
     ++ arms
     ++ [""]
  where
    emitCon en vn fields =
      let cn = mangleVariant en vn
          tys = map (renderTy . snd) fields
      in if null tys then cn else cn ++ " " ++ unwords tys
    emitCanonArm en (vn, fields) =
      let cn = mangleVariant en vn
          binders = [ "_c" ++ show i | i <- [0 .. length fields - 1] ]
          pat = if null fields then cn else cn ++ " " ++ unwords binders
          canons = [ "Rt.canon " ++ b | b <- binders ]
          rhs =
            if null fields
            then "Rt.canonEnum " ++ show en ++ " " ++ show vn ++ " []"
            else "Rt.canonEnum " ++ show en ++ " " ++ show vn
                 ++ " [" ++ intercalate ", " canons ++ "]"
      in replicate indN ' ' ++ "canon (" ++ pat ++ ") = " ++ rhs

emitConst :: IrModule -> [IrModule] -> IrConst -> [String]
emitConst m allMods (IrConst name ty val) =
  let ctx = baseCtx m allMods
  in [ mangleValue name ++ " :: " ++ renderTy ty
     , mangleValue name ++ " = " ++ emitExpr ctx val
     , ""
     ]

baseCtx :: IrModule -> [IrModule] -> Ctx
baseCtx m allMods = Ctx m allMods [] [] [] ExprMode [] 0

-- Function / test definition: signature on its own line; body layout-indented.
emitDefLines :: String -> String -> String -> [String]
emitDefLines sig lhs bodyE =
  let bodyLs = lines bodyE
      defLines = case bodyLs of
        [one] | length one <= 72 && not (isBlockStart one) ->
          [lhs ++ " = " ++ one]
        _ ->
          (lhs ++ " =") : map (replicate indN ' ' ++) bodyLs
  in sig : defLines ++ [""]

emitFunc :: IrModule -> [IrModule] -> IrFunc -> [String]
emitFunc m allMods f =
  let fn = mangleValue (fName f)
      params = fParams f
      paramTys = map (renderTy . pTy) params
      retTy = fretTyStr f
      sig = if null params
            then fn ++ " :: " ++ retTy
            else fn ++ " :: " ++ intercalate " -> " (paramTys ++ [retTy])
      args = unwords (map (mangleValue . pName) params)
      inouts = [pName p | p <- params, pInout p]
      ctx = (baseCtx m allMods) { ctxInouts = inouts, ctxMode = ExprMode }
      bodyE = emitBody ctx (fBody f)
      lhs = if null params then fn else fn ++ " " ++ args
  in emitDefLines sig lhs bodyE

emitTestFile :: [IrModule] -> IrModule -> String
emitTestFile allMods m =
  let modName = mangleModule (mName m)
      imports = mImports m
      names = testNamesFor (mTests m)
      tests = zip names (mTests m)
      ctx0 = baseCtx m allMods
      testFns = concatMap (emitTestFn ctx0) tests
      -- TAP names must be the test_* function names (test_fn_names), not the
      -- human-readable IrTest.name — lockstep matches outcomes by that key.
      listEntries =
        [ "(" ++ show fn ++ ", " ++ fn ++ ")"
        | (fn, t) <- tests
        ]
  in unlines $
    [ "{-# LANGUAGE BangPatterns #-}"
    , "module Main where"
    , ""
    , "import " ++ modName
    , "import qualified SudoRt as Rt"
    , "import qualified Control.Exception"
    , "import Data.Int (Int64)"
    , "import qualified Data.Map.Strict as M"
    , "import qualified Data.Set as S"
    ]
    ++ [ "import qualified " ++ mangleModule i | i <- imports ]
    ++ [""]
    ++ testFns
    ++ [ "main :: IO ()"
       , "main = Rt.runTests"
       , "  [ " ++ intercalate "\n  , " listEntries
       , "  ]"
       ]

emitTestFn :: Ctx -> (String, IrTest) -> [String]
emitTestFn ctx (fn, t) =
  let body = compileTestIO ctx (tBody t)
  in emitDefLines (fn ++ " :: IO ()") fn body

-- ===========================================================================
-- Main
-- ===========================================================================

main :: IO ()
main = do
  hSetEncoding stdin utf8
  hSetEncoding stdout utf8
  hSetEncoding stderr utf8
  input <- hGetContents stdin
  _ <- evaluate (length input)
  runtimeSrc <- readFile "SudoRt.hs"
  _ <- evaluate (length runtimeSrc)
  case parseValue input of
    Left err -> respondError ("JSON parse error: " ++ err)
    Right val -> case decodeRequest val of
      Left err -> respondError ("IR decode error: " ++ err)
      Right req -> case emitAll runtimeSrc req of
        Left err -> respondError err
        Right files -> respondOk files

respondError :: String -> IO ()
respondError msg = do
  hPutStr stdout (renderValue (VObj [("error", VStr msg)]))
  hPutStr stdout "\n"
  exitSuccess

respondOk :: [(String, String)] -> IO ()
respondOk files = do
  let fileVals =
        [ VObj [("path", VStr p), ("contents", VStr c)]
        | (p, c) <- files
        ]
  hPutStr stdout (renderValue (VObj [("files", VArr fileVals)]))
  hPutStr stdout "\n"
  exitSuccess

emitAll :: String -> EmitReq -> Either String [(String, String)]
emitAll runtimeSrc req =
  let mods = rqModules req
      modFiles =
        [ (mangleModule (mName m) ++ ".hs", emitModule mods m)
        | m <- mods
        ]
      testFiles =
        if rqWithTests req
        then
          let entry = last mods
              tname = mName entry ++ "_test.hs"
          in [(tname, emitTestFile mods entry)]
        else []
  in Right $ ("SudoRt.hs", runtimeSrc) : modFiles ++ testFiles
