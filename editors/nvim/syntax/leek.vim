" Vim/Neovim syntax file for Leekscript.
"
" This is the lexical layer: comments, strings, numbers, keywords, builtin
" functions/constants/types, and operators. The leek-lsp server layers
" context-aware semantic tokens on top (locals vs. functions vs. classes vs.
" fields), so identifiers are refined by the LSP while this file colors
" everything the LSP intentionally leaves alone — most importantly comments,
" which the server never emits as semantic tokens.
"
" Mirrors editors/vscode/syntaxes/leek.tmLanguage.json; keep the two in sync.
"
" NOTE on ordering: for overlapping `:syntax match`/`:syntax region` items that
" start at the same position, Vim gives the item defined LAST the higher
" priority (`:help :syn-priority`). So the broad, low-priority matches
" (operators, call sites) are defined FIRST, and the lexical regions that must
" win — comments and strings — are defined AFTER them. `:syntax keyword` items
" always outrank matches/regions, so their order is irrelevant.

if exists("b:current_syntax")
  finish
endif

let s:cpo_save = &cpo
set cpo&vim

syntax case match

" --- Operators & punctuation (low priority — defined first) -----------------
syntax match leekOperator "[-+*/%=<>!&|^~?:@\\]"

" --- Declarations & call sites (matches — keep below comments/strings) ------
" `:syntax keyword` items (keywords/builtins/types below) outrank these, so a
" keyword followed by `(` keeps its own color.
" Any identifier immediately followed by `(` is a call site...
syntax match leekFunctionCall "\<\h\w*\>\ze\s*("
" ...but a name in a declaration / `new` position is defined later so it wins
" over the generic call match (e.g. `new Robot()` colors Robot as a class).
syntax match leekFunctionDef  "\%(\<function\s\+\)\@<=\h\w*"
syntax match leekClassDef     "\%(\<class\s\+\)\@<=\h\w*"
syntax match leekClassDef     "\%(\<extends\s\+\)\@<=\h\w*"
syntax match leekClassDef     "\%(\<new\s\+\)\@<=\h\w*"
" `contains` is a reserved argument name for :syn keyword, so match it here.
syntax match leekBuiltinFunc  "\<contains\>"

" --- Comments (must win over operators — defined after them) ----------------
syntax keyword leekTodo contained TODO FIXME XXX NOTE HACK BUG
" Pragma tags inside line comments: //@version //@strict //@experimental
syntax match leekPragma contained "@\%(version\|strict\|experimental\)\>"
syntax region leekLineComment start="//" end="$" keepend contains=leekTodo,leekPragma,@Spell
syntax region leekBlockComment start="/\*" end="\*/" contains=leekTodo,@Spell

" --- Strings ----------------------------------------------------------------
syntax match leekEscape contained "\\[nrt\\\"'0]"
syntax match leekEscape contained "\\u{[0-9A-Fa-f]\+}"
syntax region leekString start=+"+ skip=+\\.+ end=+"+ contains=leekEscape,@Spell
syntax region leekString start=+'+ skip=+\\.+ end=+'+ contains=leekEscape

" --- Numbers ----------------------------------------------------------------
syntax match leekNumber "\<0[xX][0-9A-Fa-f][0-9A-Fa-f_]*\>"
syntax match leekNumber "\<0[bB][01][01_]*\>"
syntax match leekNumber "\<[0-9][0-9_]*\>"
syntax match leekFloat  "\<[0-9][0-9_]*\.[0-9_]\+\%([eE][-+]\=[0-9_]\+\)\=\>"
syntax match leekFloat  "\<[0-9][0-9_]*[eE][-+]\=[0-9_]\+\>"
syntax match leekFloat  "∞"
syntax match leekFloat  "π"

" --- Keywords (keyword items always outrank matches/regions) ----------------
syntax keyword leekConditional if else switch case default
syntax keyword leekRepeat      while do for foreach in
syntax keyword leekStatement   break continue return
syntax keyword leekStorage     function class extends constructor include var global
syntax keyword leekModifier    static private public protected final
syntax keyword leekOperatorWord and or not xor is instanceof new
syntax keyword leekBoolean      true false
syntax keyword leekConstant     null
syntax keyword leekThis         this super

" --- Types ------------------------------------------------------------------
syntax keyword leekType integer int real number float double boolean bool string void any
syntax keyword leekClassType Array Map Set Object Function Class Interval Number String
syntax keyword leekClassType Boolean Integer Real System JSON Color Value Standard

" --- Builtin constants ------------------------------------------------------
syntax keyword leekBuiltinConst PI INFINITY NAN
syntax keyword leekBuiltinConst TYPE_NULL TYPE_BOOLEAN TYPE_NUMBER TYPE_INTEGER TYPE_REAL
syntax keyword leekBuiltinConst TYPE_STRING TYPE_ARRAY TYPE_OBJECT TYPE_FUNCTION
syntax keyword leekBuiltinConst USE_SUCCESS USE_FAILED USE_CRITICAL USE_INVALID_TARGET
syntax keyword leekBuiltinConst USE_NOT_ENOUGH_TP USE_INVALID_POSITION USE_RESURRECT
syntax keyword leekBuiltinConst SORT_ASC SORT_DESC SORT_RANDOM

" --- Builtin functions ------------------------------------------------------
" (Types/keywords that collide — string, number, real, integer, boolean — are
"  intentionally omitted here so they keep their Type/keyword color.)
syntax keyword leekBuiltinFunc getType typeOf instanceOf char clone isNull length count
syntax keyword leekBuiltinFunc isEmpty debug debugC debugE debugW log max min abs ceil
syntax keyword leekBuiltinFunc floor round sqrt pow cos sin tan acos asin atan atan2 exp
syntax keyword leekBuiltinFunc log10 log2 sign hypot cbrt rand randInt randFloat getSeed
syntax keyword leekBuiltinFunc setSeed push pushAll pop shift unshift insert remove
syntax keyword leekBuiltinFunc removeElement removeKey fill sort reverse shuffle join
syntax keyword leekBuiltinFunc indexOf lastIndexOf search inArray slice splice
syntax keyword leekBuiltinFunc chunk concat flatten reduce reduceRight forEach arrayForeach
syntax keyword leekBuiltinFunc arrayMap arrayFilter arrayReduce arrayReduceRight arrayConcat
syntax keyword leekBuiltinFunc arrayFlatten arrayCount arrayMax arrayMin arrayPartition
syntax keyword leekBuiltinFunc arraySort arrayIter arrayKeyExists arrayKeys arrayValues
syntax keyword leekBuiltinFunc arrayLast arrayFirst arrayChunk arrayFoldLeft arrayFoldRight
syntax keyword leekBuiltinFunc arrayCopy arrayConcatAll subArray arrayReplace arrayIntersect
syntax keyword leekBuiltinFunc arrayDifference arrayUnion arrayDistinct arrayUnique
syntax keyword leekBuiltinFunc arrayRandom arraySome arrayEvery arrayProduct arrayAvg
syntax keyword leekBuiltinFunc arrayAdd arrayGroupBy arraySplit arraySplice entries mapKeys
syntax keyword leekBuiltinFunc mapValues mapContainsKey mapContainsValue mapForEach mapSize
syntax keyword leekBuiltinFunc mapIsEmpty mapPut mapGet mapRemove mapClear mapMerge mapFilter
syntax keyword leekBuiltinFunc mapMap setRemove setContains setSize setClear setUnion
syntax keyword leekBuiltinFunc setIntersection setDifference setForeach setForEach setToArray
syntax keyword leekBuiltinFunc setIsEmpty charAt substring substr replace replaceAll split
syntax keyword leekBuiltinFunc toString startsWith endsWith matches format stringContains
syntax keyword leekBuiltinFunc stringFormat stringIndexOf stringJoin stringLength
syntax keyword leekBuiltinFunc stringReverse stringSplit stringSubstring stringToLowerCase
syntax keyword leekBuiltinFunc stringToUpperCase stringMatches numberAbs numberCeil
syntax keyword leekBuiltinFunc numberFloor numberRound numberSqrt numberPow numberCos
syntax keyword leekBuiltinFunc numberSin numberMax numberMin numberExp numberLog intervalSize
syntax keyword leekBuiltinFunc intervalContains intervalIsEmpty intervalIter intervalMin
syntax keyword leekBuiltinFunc intervalMax intervalAvg intervalForeach intervalForEach
syntax keyword leekBuiltinFunc intervalReduce intervalReduceRight intervalFilter intervalMap
syntax keyword leekBuiltinFunc jsonEncode jsonDecode json_encode json_decode color getColor
syntax keyword leekBuiltinFunc getRed getGreen getBlue colorFromRGB colorToRGB getOperations
syntax keyword leekBuiltinFunc getMaxOperations getUsedRAM getMaxRAM getRemainingOperations
syntax keyword leekBuiltinFunc getRamUsage getAITimestamp getCurrentTime getLife getTotalLife
syntax keyword leekBuiltinFunc getMaxLife getStrength getAgility getWisdom getMP getTP getName
syntax keyword leekBuiltinFunc getLevel getColors getTeamName getTeam getEnemiesCount
syntax keyword leekBuiltinFunc getAlliesCount getLeek getLeeks getAlliedLeeks getEnemyLeeks
syntax keyword leekBuiltinFunc getEntities getEnemy getEnemies getAllies getAbsoluteShield
syntax keyword leekBuiltinFunc getRelativeShield getMagic getResistance getCellDistance
syntax keyword leekBuiltinFunc getCellFromXY getCellX getCellY getCell getDistance getMapType
syntax keyword leekBuiltinFunc getMap getNearestEnemy getNearestAlly getNearestAllyTo
syntax keyword leekBuiltinFunc getNearestEnemyTo getPath getPathLength getOperationsCount
syntax keyword leekBuiltinFunc getInstructionsCount getWeapons getWeapon getChips getCooldown
syntax keyword leekBuiltinFunc moveToward moveTowardCell moveTowardLeek moveAwayFrom
syntax keyword leekBuiltinFunc moveAwayFromCell moveAwayFromLeek useWeapon useWeaponOnCell
syntax keyword leekBuiltinFunc useChip useChipOnCell setWeapon say show mark lineOfSight
syntax keyword leekBuiltinFunc isAlive isDead isAlly isEnemy isOnSameLine isOnSameDiagonal
syntax keyword leekBuiltinFunc isInlineAttack isDiagonal isStanding endTurn skipTurn summon
syntax keyword leekBuiltinFunc getEntity getOperationsHistory getDamageReturn getPower
syntax keyword leekBuiltinFunc getStartTP getStartMP

" --- Highlight links --------------------------------------------------------
highlight default link leekTodo         Todo
highlight default link leekPragma       PreProc
highlight default link leekLineComment  Comment
highlight default link leekBlockComment Comment
highlight default link leekEscape       SpecialChar
highlight default link leekString       String
highlight default link leekNumber       Number
highlight default link leekFloat        Float
highlight default link leekConditional  Conditional
highlight default link leekRepeat       Repeat
highlight default link leekStatement    Statement
highlight default link leekStorage      StorageClass
highlight default link leekModifier     StorageClass
highlight default link leekOperatorWord Keyword
highlight default link leekBoolean      Boolean
highlight default link leekConstant     Constant
highlight default link leekThis         Identifier
highlight default link leekType         Type
highlight default link leekClassType    Type
highlight default link leekBuiltinConst Constant
highlight default link leekBuiltinFunc  Function
highlight default link leekFunctionDef  Function
highlight default link leekClassDef     Structure
highlight default link leekFunctionCall Function
highlight default link leekOperator     Operator

let b:current_syntax = "leek"

let &cpo = s:cpo_save
unlet s:cpo_save
