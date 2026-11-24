# Expansion design: lex before expanding (fixes #10)

**Status:** spec v2, 2026-11-24 (UTC-3), implemented by PR-A (`fix/issue-10-lexer-expander`). v1 was reviewed by
three adversarial reviewers (validator, advocate, refuter); v2 folds in the refuter's 298 cases and the maintainer's
answers to its 12 questions (§9).

**Decision rule.** PR-A keeps main's behaviour (1dbfa00; its shell code is identical to 50b0428, where the corpora were
observed) unless a change is needed for one of these reasons:
- (R1) to fix #10, whose acceptance criteria are: quoted operators, operators coming from variables, escaped `$`,
  single/double quotes and mixed fragments; "no literal argument may cause writes";
- (R2) so that the paths PR-A touches uphold I1–I5 and do not regress.

Independent semantic changes become follow-up proposals (§8). bash, zsh, dash and POSIX are references only. oxsh is a
hybrid shell with structured pipelines, not a bash clone.

Every difference from main or bash is one of: **bug fix** (B-fix), **pre-existing bug made an explicit error**
(B-err), **known limitation / debt kept** (B-debt), or **intentional difference** (K). Cases that equal main and bash
are E; R cases guard regressions earlier #28 revisions introduced.

**Case IDs:** `cNNNN` maintainer corpus, `V-*` validator, `adv-*`/`pty-*` advocate, `rf-NNNN` refuter. All of them are
versioned in `tests/corpus/*.jsonl` (§10).

## 1. Invariants and what enforces them

"Compiler" means the property follows from types or signatures; "tests" names the checks that exercise it.

| Id | Invariant | Compiler | Tests |
|---|---|---|---|
| I1 | Expansion results never become operators or syntax | `Op` and `Word` have private constructors: only `syntax::lex` builds them. `expand` returns `Field`s, which have no conversion back to tokens. Alias lookup takes the literal source text of a lexer `Word` with no expansion segment. | corpus I1 groups (rf split/trunc-I1/append/dataloss, V-O*, adv-114–118); fuzz `quoted_expansion_data_roundtrips`, `soup_is_robust_and_writes_only_with_gt` |
| I2 | Pure parsing has no side effects | `lex(&str)` and `parse_pipeline(Vec<Item>)` take no shell, environment or executor; the `syntax` module imports nothing from `shell`/`executor` [verified by reading; no automated import check]. | fuzz `substitution_runs_at_most_once`; rf-0139/0143 (lazy `:-`, quoted here-doc) |
| I3 | Invalid input never panics, hangs or writes | The lexer is total (`Result<_, SyntaxError>`); `syntax` and `expand` deny `unwrap`/`expect`/indexing/`panic` outside tests (clippy). Recursion depth is bounded (64) and brace expansion is capped. | fuzz (soup, unterminated, size and nesting ladders); refuter I3 groups. **Debt:** non-UTF-8 argv and `unset`/`export` of odd names still panic, as on main (§11). |
| I4 | Interrupted execution never continues or commits | `ExpandError::Interrupted` is a distinct error that every caller must handle before running or assigning. | `tests/interactive_signals.rs` (`ctrl_c_in_a_substitution_commits_nothing`, `ctrl_c_during_command_substitution_aborts_the_command`); refuter pty-interrupt group (notes harness) |
| I5 | Redirections keep their semantics | Redirect operators are `Op` tokens; a target is built by `RedirectTarget::from_fields` from exactly one field. | corpus I5 groups (V-R*, rf append/dataloss/empty-arg) |
| I6 | Exit codes are correct | — | corpus `rc` on every case; `tests/exit_status.rs` |

New error paths and their codes: syntax error rc 2; ambiguous redirect rc 1; builtin with a pipe or redirect rc 1 (#11);
arithmetic not supported rc 1; command list in an alias body rc 1; interrupted line rc 130.

## 2. Processing model (normative)

For each line (from `-c`, the prompt, a script or a control-flow body):

1. **History expansion** stays on the raw line, as on main (non-goal).
2. **`lex(line) -> Result<Vec<Token>, SyntaxError>`** over the whole line. A token is `Op(kind)` or `Word`; a word is a
   list of segments: `Lit{text, quoting}`, `Param{name, op, operand, quoting}`, `CmdSub{source, quoting}`,
   `Arith{source}`. Any syntax error: `oxsh: syntax error: …`, rc 2, **nothing on the line runs** (Q2).
3. **Chain split** on the `;`, `&&`, `||` tokens (Q1). Chain semantics are main's, including #12.
4. **Per chain segment**, in order:
   - a segment whose first word is `for`/`while`/`if` mid-line keeps main (the keyword runs as a command name, as on
     main; B12 debt). A line that *starts* with one of them is handled before step 3 (below);
   - assignment / env-prefix detection on tokens (D11);
   - alias resolution on a literal command word, by token splice (D10);
   - pipeline and redirect structure from `Op` tokens;
   - expansion of each word to fields (§4), then redirect targets via `RedirectTarget::from_fields` (D4);
   - dispatch to a builtin (D5), a structured stage or an external command — main's executor, unchanged.
5. **Control flow.** A line that starts with `for`, `while` or `if` is parsed by main's `scripting.rs` parsers as one
   statement; the bodies run through this same path, so I1/I4/I5 hold inside them (Q8). If the statement is not the whole
   line (`for …; done; echo x`), the result is `oxsh: syntax error: unsupported for form (the statement must be the
   whole line)`, rc 2, nothing runs. On main such lines ran the keyword pieces as commands (rc 127, sometimes writing
   files: c0158 created `c.txt;`). Proper control-flow parsing is debt B12.

## 3. Lexical rules (normative)

- **Blanks** (space, tab) separate words outside quotes, `$(...)`, backticks and `${...}`.
- **Newline:** in `-c` an unquoted newline is not a separator; it stays in the word, as on main (Q10, rf-0151, K).
- **Here-documents:** `<<` followed later on the line by a newline gives `oxsh: syntax error: here-documents are not
  supported yet`, rc 2 — so a quoted body is never executed (rf-0143) and no file named after body text is created
  (rf-0145). A `<<` with no later newline keeps main (c0163/c0164/c0463).
- **Single quotes:** everything is literal up to the next `'`.
- **Double quotes:** `$`, `` ` `` and `\` are special. `\` escapes only `` $ ` " \ `` and newline; before any other
  character both characters are kept (D-ESC).
- **Unquoted `\c`:** `c` is literal. A trailing lone `\` is dropped, as on main.
- **`$(`…`)`:** nests with quotes and parentheses balanced (iterative scanner, no recursion limit needed). The source is
  stored, never executed by the lexer.
- **`` `…` ``:** POSIX backquote unescaping (D-BQ).
- **`${`…`}`:** the operand is lexed with the same rules and the outer quote context; the closing `}` is the first
  unquoted, unescaped one (D9). `${` nesting depth is limited to 64 (deeper is a syntax error).
- **`$((`…`))`:** an `Arith` segment, never a command substitution (Q9).
- **`$'`:** a literal `$` followed by a normal single-quoted string (D-ANSI).
- **Unterminated** `'`, `"`, `$(`, `` ` ``, `${`, `$((`: syntax error, rc 2, nothing runs (Q2).
- **Operators:** `| & ; && || < > >> 2> 2>> 2>&1` (main's set), recognised only from unquoted source characters outside
  substitutions, with maximal munch. `;`, `&&` and `||` split wherever they appear unquoted, glued or not (as main's
  `split_chain_ops`). `|`, `&` and redirects follow D1. A `&` that ends a pipeline means background; any other `&` is a
  literal argument (main, c0206).
- **Word classification (D1):**
  - (a) a unit that is exactly an operator spelling is an `Op`;
  - (a') a unit made only of literal text, where the quoted parts are empty or a leading `2`, and whose text is an
    operator spelling, is an `Op` too: `"">>`, `>''`, `"2">` (main parity, c0101; refuter rf-0239/0240/0268 pin it);
  - (b) an expansion prefix followed by a run of operator characters that is exactly an operator spelling is cut:
    `$X>`, `$(cmd)>>` give `Word(prefix)` + `Op`;
  - anything else is one word (`cmd>file`, `x.bak>log`, `2>/dev/null` keep main: D1a).

## 4. Decisions

Format: decision, then rationale (R1/R2 or keep-main), then affected cases.

**D1. Glued operators** (N1, N3, N5, S1, maintainer).
- (a) **Literal words keep main:** an operator glued to unquoted literal text, or to purely quoted text, is part of the word. Examples: `cmd>file`, `x.bak>log`, `where a>2`, `2>/dev/null`, `>f`, `>>f`, `<f`, `"a"> f`, `'y'>> f`, `"2"> f`.
  - Rationale: keep-main, structured-pipeline identity. A change here is PR-D.
  - Main's `rm -v x.bak> b.log` deleting `b.log` is **pre-existing data-loss debt**. It is high priority for its own PR (PR-D), not fixed here.
  - Cases: adv-140/141/142/144/185–189/191, adv-152, V-R09/R10/R21/R34/R42, c1014, c1020.
- (b) **Expansion prefix:** if a blank-delimited unit is `prefix + oprun`, where the prefix contains at least one `Param`, `CmdSubst` or `Arith` segment, `oprun` is a complete operator spelling, and a blank or end of input follows, the lexer emits `Word(prefix)` + `Op`. Otherwise the whole unit is one word.
  - Rationale (R2: I1 and I5): on main, whether `$X>` or `$(cmd)>` redirects depends on the expansion's *value* (an empty value redirects). That is syntax decided by data. PR-A cannot keep that dependence (I1), so the lexical rule must keep main's redirect (an empty value was the common case) rather than turn it into data. Turning it into data is the #28 r3 data-loss case.
  - Visible changes vs main: `$(echo x)> f`, `$X> f` with X set, and `"$X"> f` now redirect too (main passed `x>`/`hi>` as data).
  - Cases: c0443/c0445 (`$(true)>>`: append, never truncate), c0338/c0339/c1013/c1012, V-R03/R07/R08, `$(true)>>a.txt`: data `>>a.txt`, equal to main.
- (c) **IO numbers:** `2>`, `2>>`, `2>&1` are operators only as a complete unit after a blank, as on main. In `$(true)2>> f`, the `2` is part of the expansion word and the operator is `>>`.
  - Rationale (R2: I1): main's stderr redirect there depended on the substitution being empty. V-R20 (`X=2; … $X> f` forming `2>`) is an I1 bug.
  - Cases: V-R43 (visible change: appends `x 2` to `a.txt`, bash-like), V-R20.

**D2. Field splitting** (N2, S9).
- Only **unquoted command-substitution output** is split, on space, tab and newline. Empty or blank-only output gives zero fields. `"$(...)"` is one field.
- `$VAR` and `${...}` results are **one field** each. That is main's behaviour for `$VAR`, and it is also zsh's: an intentional difference from bash.
- `${U:-a b}` gives `<a b>`. Main gave `<a><b}>` through the stray-`}` bug; the change is required because the `}` comes from #10-class loss of structure (D9).
- Cases: V-F03/F05/F09/F27 (fixes), V-F13/adv-105/adv-022 (keep), V-P02/P20/adv-124 (visible change), adv-021 (keep: split).
**D3. Empty expansions** (Q5, N7, N12, R4-B1). Main's behaviour is not one rule, so each form is pinned by case:

| Form (unquoted unless shown) | Fields | Example | Cases | Class |
|---|---|---|---|---|
| `$U`, `${U}`, `${U:-}` alone | one empty field | `printf '[%s]' a $U b` gives `[a][][b]` | rf-0216/0218/0222, c0003/c0004 | K (main) |
| `${U:-''}`, `${U:-""}`, `${U:-$U}` | one empty field | `mv a.txt ${U:-''} b.txt` fails, b.txt kept (R4-B1) | rf-0200/0201/0202/0210/0217, c1028–c1039 | E guard |
| `"$U"`, `""`, `''`, `"$(true)"`, `$U""` | one empty field | `mv a.txt "" b.txt` fails | rf-0206–0209, rf-0220 | E |
| `$(true)`, `` `true` `` | zero fields | `printf '[%s]' a $(true) b` gives `[a][b]` | rf-0211/0219/0221 | E |
| redirect target `$U`, `"$U"`, `${U:-''}` | one empty field → open fails, rc 1 | `/bin/echo x > $U` | rf-0224–0227, V-R22/R23 | E |
| D1b cut word, empty, no quoted part (`$U>>`, `$(true)>>`) | zero fields | `/bin/echo y $U>> b.log` appends `y` | rf-0233/0234/0236/0237, c1015–c1018 | E |
| quoted glued `"$U">>`, `"">>`, `''>>` | zero fields (main) | `/bin/echo y "">> b.log` appends `y` | rf-0235/0239/0240 | K (main; bash appends `y ` + empty arg) |
| alias body word that expands empty | zero fields | `alias g='printf [%s] $1'; g zz` | c0079/c0347/c0809 | E |

No empty-field removal (PR-B). `cd $U` keeps failing, which is safer.

**D4. Redirect targets** (N4).
- A target expands to fields. **Exactly one** field gives a `RedirectTarget`. **Zero or two or more** fields give `ambiguous redirect`: rc 1, nothing runs, no file is touched.
- An unquoted `$X` is never split (D2), so `X='p q'; > $X` writes to the file `p q`, as on main (V-R27).
- An empty `$UNSET` target is one empty field, so opening `""` fails as on main (V-R22/R23).
- Rationale (R2: I5): on main, `> $(printf 'p q')` wrote to `p` and turned `q` into an argument, and `> $(true) file` silently redirected to `file`.
- Cases: V-R26 (visible change), V-R27 (keep), refuter `> $(true) file` (visible change: error). **Note:** V-R22/R23/R26/R27 use the *builtin* `echo`, so D5 applies first (rc 1, nothing written; this still meets V-R22/R23's expectation). D4 itself is tested with `/bin/echo` in `tests/cli.rs` (`redirect_target_must_be_exactly_one_field`).

**D5. Builtins with redirect or pipe operators** (S5, maintainer).
- These are the builtins in builtins.rs (cd, exit/quit, export, unset, pwd, which, echo, true, false, help, type, context) and in shell.rs (alias, unalias, source/./reload, history, back, next, read).
- When one of them is the **first command word of a chain segment** (main's dispatch point: `try_builtin(&tokens)` sees the whole segment before the pipe split) and that segment contains any redirect or pipe `Op`, the result is `oxsh: <name>: redirections and pipes are not supported for builtins yet (#11)`, rc 1, and the command does not run.
- Rationale (R2: I5 and #10): on main the operators were plain text, so `echo hi > out.txt` printed `hi > out.txt`. Now that operators are typed, they would otherwise be dropped silently, and the user would believe a file was written.
- Visible change: `echo hi > f` and `pwd > f` become errors. External `/bin/echo` is unaffected. Builtin names in **later** pipeline stages (`ls | echo x`) keep main: they are resolved as external commands.
- Cases: adv-043/044/192/193, V-R44, adv-045 (`read X < a.txt`: main silently read stdin into a variable named `a.txt`), c0007/c0020/c0021/c0025/c0071–c0074 and other bare-`echo` redirect/pipe cases (now error, rc 1).

**D6. Chain operators** (N6, S2, maintainer: IN).
- `;`, `&&`, `||` and a trailing `&` are `Op` tokens from the lexer. The separators are recognised glued or not (main splits `a;b` and `a&&b` on the raw line; keep), but never inside quotes, escapes, `$(...)`, backticks or `${...}`. The chain is split on tokens, so separators inside `$(...)`, backticks, `${...}` and quotes are never split.
- Chain *semantics* stay main's, including the #12 bug where a failing `&&`/`||` skips later `;` commands (debt).
- Rationale (R2: I1): main splits the raw string, so text inside `${U:-a;b}` or `$(a; b)` **runs as a command** (V-P13: `b}`; adv-114: `echo b)`).
- Cases: V-O08/O09/O10/P13, adv-114–118, c0018.
**D7. Unterminated constructs** (Q2). Unterminated `'`, `"`, `$(`, `` ` ``, `${` or `$((` gives
`oxsh: syntax error: unterminated …`, rc 2, and nothing on the line runs. There is no literal fallback: with one, a `>`
after the opener would be an operator and write a file (main's fuzz finding `S1}`), against I3.
- Visible change: main implicitly closed quotes and ran `$(echo x` (c0060, c0900, c0901, adv-182, adv-184, V-P15/P18,
  rf I3-unterminated groups).

**D8. Glob, brace and tilde** (Q3, Q4).
- (a) Quoted or escaped `* ? [ { } ~` are literal, and **quoted expansion results are never globbed, brace- or
  tilde-expanded**: the bytes round-trip (fuzz `quoted_expansion_data_roundtrips`; main expanded braces and `~` in
  `"$FZ"`). B-fix.
- (b) Unquoted expansion results keep main: they are globbed and brace/tilde-expanded, and the results are fields (I1
  holds). So `X='*'; rm -v $X` still removes every file, as on main and in bash (rf-0264/0265, K by decision 3; PR-G
  proposes zsh's rule).
- (c) Unquoted `*` matching dotfiles is main debt, kept (D8c: c0032, c0109–c0113, c0916/c0917/c0959/c0960).

**D9. `${...}`** (N9, Q7).
- The operand uses the same lexer, so `\}` and quoted `}` do not close it: `"${U:-a\}b}"` gives `a}b`. `$VAR`,
  `$(...)` and nested `${...}` in the operand expand.
- **Lazy default:** the operand of `${X:-word}` is expanded only when X is unset or empty, so
  `X=1; ${X:-$(cmd)}` never runs `cmd` (rf-0139; main ran it).
- Operators: `${VAR}` and `${VAR:-word}`. Others (`${#X}`, `:+`, `:=`, …) keep main's result where main gave one and are
  otherwise literal (PR-E).

**D10. Aliases** (N11, S6, maintainer: minimal).
- (a) A command word is alias-eligible only if it has **no expansion segment**. Quoted or escaped literal names keep main: `\g` and `'g'` still use the alias (PR-C).
  - Rationale (R2: I1): `$(echo g)`, `X=g; $X` and `$(true)oxg` let data pick an alias whose body becomes syntax.
  - Cases: V-A08/A09, adv-052/053, c0335, c1002/c1003 (fix); V-A06, c0999–c1001 (keep).
- (b) An alias body is lexed with the PR-A lexer **at use** and its words are expanded at use. This is required (R1 regression avoidance): on main, `alias g='printf $X'` worked only because main expanded `$X` inside single quotes at definition time (the #10 bug itself).

  **Exactly what changes:**
  1. Bodies defined with the `alias` builtin inside single quotes now expand `$VAR`, `$(...)`, backticks and `~` at use; main expanded `$VAR` at definition (V-A16, adv-048/049, c0082/c0085).
  2. **Config-file aliases** now expand `$VAR`, `$(...)` and `~` at use; on main they expanded nothing (adv-057, pty-02).
  3. Pipes and redirects in a body work as on main.
  4. A body containing `;`, `&&` or `||` gives `oxsh: alias <name>: command lists in aliases are not supported yet`, rc 1, nothing runs. On main the rest was silently passed as arguments (adv-058, pty-03).
  5. Ctrl-C inside a body substitution abandons the line (pty-14, I4).

  **Mechanics that keep main** (found while classifying the corpus):
  - **Token splice.** The body's *tokens* replace the command-word token, and then the stage is parsed. So `alias r='/bin/echo R >'; r out.txt` and `alias p='/bin/echo a |'; p wc -c` keep working (c0382, c0588, c0649).
  - **Lookup order.** The alias is looked up on the literal source text of the command word, after any env-prefix assignment and before tilde, brace or glob expansion. So `FOO=1 g` and `alias ~x=…; ~x` work as on main (c0297, c0650, c0688, c0661). `g~`, `g*` and `g\ hi` are not aliases (c0318, c0660, c0666, c0645).
  - **Empty body words.** An unquoted body word whose expansion is empty yields **zero** fields, not D3's one empty field. Main expanded at definition and re-tokenised, so `alias g='printf [%s] $1'; g zz` gives `[zz]` on main and in bash (c0079, c0347, c0809). Without this rule, D10b would regress those cases.

**D11. Assignments and env-prefix** (S10, S11, maintainer: IN).
- Detection happens on tokens: a segment whose first word starts with an unquoted literal `NAME=` is an assignment. If more words follow, it is an env-prefix for the command that follows (one prefix, as on main).
- The value is the rest of that word, expanded **without** field splitting, glob or brace.
- Interrupt is checked **before** committing the variable or running the command.
- Rationale:
  - (R2: I1) ``X=`echo hi` `` ran `hi` as a command (adv-098).
  - (R2: I4) ^C clobbered X (pty-09) and `X=$(slow) cmd` still ran cmd (pty-15).
  - (R1) `FOO='$HOME' env` and `export A='$HOME'` must stay literal (c0117, adv-036, adv-132).
- Visible changes: `X=a\ b` and `X=$(printf 'a b')` keep the space (adv-134/136, c0048/c0053/c0119/c0976).
**D12. Interrupts** (Q6, Q12).
- `Err(Interrupted)` from any substitution stops expansion at that point. Later substitutions do not run, the command
  does not run, an env-prefixed command does not run, a pending assignment is **not committed**, and the rest of the
  line is abandoned with `$?=130`. The same holds inside alias bodies.
- `X=$(slow)` + Ctrl-C keeps the old X. This is an **intentional difference** from bash (which commits an empty X),
  required by I4.
- Cases: rf-0271–0286, pty-09/10/14/15/21; `tests/interactive_signals.rs`.

**D13. Structured stages** (S13).
- Typed redirect ops apply to structured stages as on main: main already took `where age > 18` as a stdout redirect and a usage error.
- **Requirement:** a structured stage validates its arguments before any output file is opened, so `where age > 18` gives a usage error and creates no file `18` (adv-005, as on main).
- Quoted `">"` and `'>'` now reach `where` as data (adv-006/007, visible improvement).
- `$(...)` results in structured arguments are split (adv-021, keep).

**D-ESC. Backslash inside double quotes** (S8, adv-073, V-P05/P06).
- One rule everywhere (D3 lexical rules): `"a\qb"` gives `a\qb`, and `"C:\path"` is kept.
- Rationale (R1): escape preservation, and one rule shared with the `${}` operand (the r3 lesson).
- Visible change: main dropped the backslash.

**D-BQ. Backticks:** POSIX unescaping; nested backticks work.
- Rationale (R1, plus the #28 regression list).
- Cases: c0734/c0755/c0756 (main-equal), V-B03/B04/B09, adv-096/099 (fixes).
**D-AR. `$((…))`** (Q9) gives `oxsh: arithmetic expansion not supported yet`, rc 1, and the command does not run. It is
never executed as a command (main ran `/bin/sh -c "(…)"`: rf-0074 `$((touch PWN))`). Cases: rf-0073–0076, adv-113, c0145.

**D-ANSI.** `$'…'` gives a literal `$` followed by single-quoted text. Main dropped characters; ANSI-C quoting is a follow-up.
- Case: adv-089.

**D-START. on_startup and the `clear` alias** (N10, S7, maintainer).
- Keep main's meaning: no expansion, aliases or chains. Lines are lexed by the PR-A lexer and words are used as their literal source text (quotes removed, `$…` kept as text).
- Rationale (R2: I1): main's tokenizer made quoted `>` and text inside `$(...)` into operators (V-S05, V-S02 wrote `ord)`).
- Visible changes: no file is written from quoted or embedded operators, and `$(echo A >> ord)` is one literal word.
- Cases: V-S01–S06, pty-01.

## 5. Behaviour kept from main on purpose (non-exhaustive, all corpus-backed)

- `$UNSET` alone as a command gives rc 127 (V-E11).
- An empty unquoted `$U` is an empty argument.
- `$VAR` is not split.
- `*.nomatch` stays literal.
- `#` is not a comment.
- auto_cd on an expanded single word (`D=sub; $D`, adv-062).
- `??`, `clear`.
- `\g` and `'g'` use the alias.
- Exit codes outside the new error paths.
- `&&`/`||`/`;` semantics including #12.
- Builtin dispatch.
- The structured commands.
## 6. Acceptance criteria for PR-A

1. **Corpus** (`tests/corpus/*.jsonl`, run by `tests/corpus_runner.rs`): every case meets its versioned expectation; a
   case with no expectation fails; skips are counted and each states its reason (§10).
2. **Fuzz** (`tests/refuter_fuzz.rs`): passes at the CI defaults (under 60 s) and at `OXSH_FUZZ_ITERS=2000` in a sandbox
   before merge.
3. **PTY** (`tests/interactive_signals.rs`): the Ctrl-C tests pass.
4. **CI**: build, `cargo test --no-fail-fast` on stable and MSRV 1.88, `clippy --all-targets -D warnings`, macOS check.
5. **No regression**: no case changes class without a decision recorded here.

## 7. Limits and non-goals

The following are not changed in PR-A:
- history `!!`/`!$`, which still re-lexes history text (pty-06);
- ANSI-C `$'…'`;
- arithmetic (explicit error, D-AR);
- here-docs (rejected when a body follows, §3);
- `[[ ]]`, `( )` and `{ }` groups;
- functions and `case`;
- control-flow parsing beyond main's whole-line forms (B12);
- job control;
- `echo -e`;
- `~user`;
- multiple env-prefix assignments;
- `#` comments;
- line continuation at the prompt (pty-19/20: PR-A must not crash; behaviour as main);
- auto_cd semantics;
- `??`;
- structured-command semantics;
- error-message wording outside the new errors;
- Windows/macOS behaviour (#27);
- performance tuning.

PR-A is not a general redesign of execution. It replaces the lexer and expansion and the call sites that feed them.
- `-c` argument handling: `$1` numbering and flag scanning of every argv word (Q11, separate PR);
- non-UTF-8 input (panics as on main).

## 8. Follow-up proposals (separate PRs, owner decision where marked)

| Id | Proposal | Notes |
|---|---|---|
| PR-B | Empty-field removal | Owner decision. Trade-off: `cd $U`, `[ -n $U ]`, R4-B1 |
| PR-C | `\name` and `'name'` bypass aliases | — |
| PR-D | Glued operators on literal words | Owner decision. **High priority:** data loss with `rm x.bak> log`. Options: bash rule; advocate's rule (operator at word start = operator, mid-word = syntax error rc 2 with a hint); keep with a warning |
| PR-E | `${}` completeness | — |
| PR-F | Syntax errors for unterminated quotes, a dangling `|`/`>` and empty pipeline stages (#13) | — |
| PR-G | No glob or brace expansion of expansion results | zsh GLOB_SUBST off; candidate rule |
| PR-H | Arithmetic `$((…))` | — |
| PR-I | ANSI-C `$'…'` | — |
| PR-J | Builtins in pipes and redirects (#11) | Then remove the D5 error |
| PR-K | Command lists in alias bodies | — |
| PR-L | on_startup expansion | Owner decision |
| PR-M | `-c` argv: stop scanning script arguments for flags; `$0`/`$1` numbering (Q11) | **High priority:** `oxsh -c '…' x --init` writes `~/.oxshrc` (rf-0184) |
| PR-N | Control-flow parser (statements inside lines, `;` after `done`/`fi`) | B12 |
| PR-O | Non-UTF-8 argv and `unset`/`export` panics | I3 debt |

## 9. Answers to the refuter's questions (FINDINGS.md)

| Q | Question | Answer (maintainer decision) | Implemented as | Cases |
|---|---|---|---|---|
| 1 | Who splits `; && \|\|`? | The lexer. Separators inside quotes, `$()`, backticks or `${}` never split. | §2 step 3; D6 | rf split group (rf-0115–0117 fixed) |
| 2 | Unterminated `$(`, `${`, backtick, quote | Syntax error rc 2, nothing runs; no literal fallback. | D7 | rf I3-unterminated* groups, c0060/c0900/c0901 |
| 3 | Glob of unquoted results | Keep main. Quoted results are never globbed. | D8 | rf-0264/0265 (K), fuzz round-trip |
| 4 | Tilde/brace of results | Quoted: never (byte round-trip). Unquoted: keep main. | D8 | c0193/c0194 (K), fuzz round-trip |
| 5 | Empty-argument forms | One table, each form pinned to main by case; the R4-B1 forms are keep-main guards. | D3 | rf-0200–0241 |
| 6 | I4 scope | Ctrl-C aborts the whole line; `X=$(slow)` does not commit (intentional, I4). | D12 | rf-0271–0286 |
| 7 | Lazy `${X:-$(cmd)}` | Lazy. | D9 | rf-0139 |
| 8 | Control-flow bodies | In scope where they reach the same path (they do); the rest is debt. | §2 step 5 | rf ctl group; rf-0161/0170/0176/0177 debt |
| 9 | `$((` | Never a command. | D-AR | rf-0073–0076 |
| 10 | Newlines, here-docs | Newline in `-c` keeps main. Here-docs out of scope, but a quoted body must never run substitutions. | §3 | rf-0151 (K), rf-0143/0145 |
| 11 | `-c` argv / `--init` scanning | Separate high-priority PR (debt). | PR-M | rf-0184–0193 (debt) |
| 12 | Alias bodies and Ctrl-C | Ctrl-C aborts alias bodies. | D12 | rf-0274, `ctrl_c_in_a_substitution_commits_nothing` |

## 10. Corpus and tests

- `tests/corpus/maintainer.jsonl` (1,039), `validator.jsonl` (155), `advocate.jsonl` (215), `refuter.jsonl` (298).
  Schema and update procedure: `tests/corpus/README.md`.
- Each case has a `class` (E, R, K, B-fix, B-err, B-debt), the `decision` that governs it, and an `expect` snapshot
  (rc, stdout, whether stderr is non-empty, every file in the work dir and HOME) plus the reviewer's own constraints.
  `expect_source` says where the expectation comes from: `main` (equal to main's recorded output), `reference` (equal to
  bash), `case` (the reviewer's constraints hold), or `spec: …`/`pinned: …` with the reason.
- Skipped (counted, with reasons): interactive cases that need a PTY (covered by `tests/interactive_signals.rs` and the
  reviewers' harnesses), one nondeterministic case (c0150), and three cases that hang or run until killed, as on main.
- bash is never run in CI; reference outputs were recorded once.
- `tests/refuter_fuzz.rs`: generative tests (knobs documented in the file); the non-UTF-8 test is ignored as debt.

## 11. Debt

Fixed by PR-A (from the main debt list): text inside `$()`/`${}`/backticks run as separate commands (B6); quoted
operators acting as operators (#10); expansion results acting as operators; `$((…))` run as a command; `${X:-$(cmd)}`
running cmd eagerly; Ctrl-C committing assignments or running the rest of the line; quoted here-doc bodies executing;
unterminated constructs executing; quoted results glob/brace/tilde-expanded.

Kept (each pinned in the corpus as B-debt or K):
- **B17, high priority:** `-c` scans every argv word for flags (`--init` writes `~/.oxshrc`, rf-0184).
- **PR-D, high priority:** `rm -v x.bak> b.log` deletes `b.log` (glued operator on a literal word, D1a; rf-0262).
- B12: control flow beyond whole-line statements; keywords mid-line run as commands.
- D8c: `*` matches dotfiles.
- #12: a failing `&&`/`||` skips later `;` commands.
- `$@`/`$#` are not positional parameters (rf-0192/0193).
- Panics on non-UTF-8 argv and on `unset ''` / `export =x` (rf-0077–0086, rf-0228–0230, c0807/c0896–c0898).
- A trailing `|` waits for input forever (c0927, #13).
