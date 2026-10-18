#!/usr/bin/env python3
"""Static, syntax-aware scanner for cancellation/error-path asymmetries.

Reads extracted crate sources as untrusted text.  It does not build, run, or
import them.  The scanner is deliberately high-recall; precision comes from the
manual review recorded in candidates.csv.

Patterns
  A  an await's error path marks a connection/session bad, but dropping the
     same future does not (no pessimistic flag, Drop guard, or take/restore).
  B  durable state (transaction depth, session flag, counter) is committed
     only after a round-trip await, with no guard before the await.
  C  pool-manager validity: has_broken is constant or only sees an unrelated
     flag; is_valid/recycle performs I/O and accepts any positive-looking reply.

Usage:
  python3 -I scanner.py --root ~/dropwise-scan-src/src --out hits.csv
  python3 -I scanner.py --root DIR --out hits.csv --only crate1,crate2
"""
import argparse
import csv
import json
import pathlib
import re
import sys

BAD_FIELD = re.compile(
    r"^(?:.*_)?(?:broken|tainted|poison(?:ed)?|closed|panic(?:ked)?|invalid|dirty|"
    r"healthy|failed|sending|in_flight|inflight|in_progress|busy|in_use|usable|"
    r"reusable|alive|ok)$",
    re.I,
)
BAD_VALUE = re.compile(r"^(?:true|Invalid|Broken|Closed|Poisoned|Tainted|Bad|Failed|Dirty|Dead)$")
GOOD_VALUE = re.compile(r"^(?:false|Invalid|Present|Valid|Healthy|Ok|Clean|Alive|Good)$")
SESSION_FIELD = re.compile(
    r"^(?:transaction_depth|transaction|in_transaction|session|session_state|"
    r"authenticated|subscribed|in_use|dirty|state|flags|.*_depth|.*_count|"
    r"count|counter|generation|epoch|sequence|seq|pending)$",
    re.I,
)
CLEAR_NAME = re.compile(r"^(?:has_broken|is_broken)$", re.I)
VALIDATE_NAME = re.compile(
    r"^(?:is_valid|validate|recycle|test_connected|check_connected|ping|health_check|"
    r"check|is_healthy|is_usable)$",
    re.I,
)
CLOSE_CALL = re.compile(r"^(?:close|abort|shutdown|set_broken|mark_broken|mark_tainted|"
                        r"taint|invalidate|kill|discard|detach)$", re.I)
GUARD_INIT = re.compile(r"(?:Rollback|Guard|Defer|ScopeGuard|UnreadyObject|Take|"
                        r"DropGuard|AbortGuard|ResetGuard)::")
GUARD_MACRO = re.compile(r"\b(?:scopeguard|defer|guard)\s*!")
BAD_EVIDENCE = re.compile(
    r"Err\s*\(|Err\s*\{|is_err\s*\(|map_err|inspect_err|unwrap_or_else|"
    r"Err\b|error\s*\(",
)

IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


class Tok:
    __slots__ = ("kind", "text", "start", "end", "line")

    def __init__(self, kind, text, start, end, line):
        self.kind, self.text, self.start, self.end, self.line = kind, text, start, end, line

    def __repr__(self):
        return f"{self.kind}:{self.text}@{self.line}"


def lex(src):
    toks = []
    i, n, line = 0, len(src), 1
    while i < n:
        c = src[i]
        if c == "\n":
            line += 1
            i += 1
            continue
        if c in " \t\r\f\v":
            i += 1
            continue
        if src.startswith("//", i):
            j = src.find("\n", i)
            i = n if j < 0 else j
            continue
        if src.startswith("/*", i):
            depth, i = 1, i + 2
            while i < n and depth:
                if src.startswith("/*", i):
                    depth += 1
                    i += 2
                elif src.startswith("*/", i):
                    depth -= 1
                    i += 2
                else:
                    if src[i] == "\n":
                        line += 1
                    i += 1
            continue
        # raw / byte / c strings, optionally raw
        m = re.match(r"(?:br|r)(#{0,255})\"", src[i:])
        if m:
            hashes, j = m.group(1), i + m.end()
            close = '"' + hashes
            k = src.find(close, j)
            if k < 0:
                k = n - len(close)
            text = src[i:k + len(close)]
            line += text.count("\n")
            toks.append(Tok("str", text, i, k + len(close), line))
            i = k + len(close)
            continue
        if c == '"' or (c in "bc" and i + 1 < n and src[i + 1] == '"'):
            j = i + (1 if c == '"' else 2)
            while j < n:
                if src[j] == "\\":
                    j += 2
                    continue
                if src[j] == '"':
                    j += 1
                    break
                if src[j] == "\n":
                    line += 1
                j += 1
            toks.append(Tok("str", src[i:j], i, j, line))
            i = j
            continue
        if c == "'":
            # lifetime vs char literal
            if i + 2 < n and src[i + 2] == "'":
                toks.append(Tok("char", src[i:i + 3], i, i + 3, line))
                i += 3
                continue
            if i + 3 < n and src[i + 1] == "\\" and src[i + 3] == "'":
                toks.append(Tok("char", src[i:i + 4], i, i + 4, line))
                i += 4
                continue
            m = IDENT.match(src, i + 1)
            if m:
                toks.append(Tok("life", src[i:m.end()], i, m.end(), line))
                i = m.end()
                continue
            toks.append(Tok("punct", c, i, i + 1, line))
            i += 1
            continue
        if c == "r" and src.startswith("r#", i) and i + 2 < n and (src[i + 2].isalpha() or src[i + 2] == "_"):
            m = IDENT.match(src, i + 2)
            toks.append(Tok("ident", src[i:m.end()], i, m.end(), line))
            i = m.end()
            continue
        m = IDENT.match(src, i)
        if m:
            toks.append(Tok("ident", m.group(0), i, m.end(), line))
            i = m.end()
            continue
        m = re.match(r"[0-9][0-9A-Za-z_.]*", src[i:])
        if m:
            toks.append(Tok("num", m.group(0), i, i + len(m.group(0)), line))
            i += len(m.group(0))
            continue
        three = src[i:i + 3]
        two = src[i:i + 2]
        if three in {"..="}:
            toks.append(Tok("punct", three, i, i + 3, line))
            i += 3
            continue
        if two in {"::", "->", "=>", "==", "!=", "<=", ">=", "+=", "-=", "*=", "/=",
                   "|=", "&=", "&&", "||", ".."}:
            toks.append(Tok("punct", two, i, i + 2, line))
            i += 2
            continue
        toks.append(Tok("punct", c, i, i + 1, line))
        i += 1
    return toks


def build_matches(toks):
    pairs = {}
    stack = []
    opens = {"(": ")", "[": "]", "{": "}"}
    for i, t in enumerate(toks):
        if t.kind != "punct":
            continue
        if t.text in opens:
            stack.append(i)
        elif t.text in ")]}":
            # tolerate unbalanced input; pop only a matching opener
            for k in range(len(stack) - 1, -1, -1):
                if opens[toks[stack[k]].text] == t.text:
                    j = stack.pop(k)
                    pairs[j] = i
                    pairs[i] = j
                    break
    return pairs


class Func:
    def __init__(self, name, start, body_open, body_close, toks, impl):
        self.name, self.start, self.body_open, self.body_close = name, start, body_open, body_close
        self.toks = toks
        self.impl = impl
        self.a = body_open + 1
        self.b = body_close

    def text(self, lo=None, hi=None):
        lo = self.a if lo is None else lo
        hi = self.b if hi is None else hi
        if hi <= lo:
            return ""
        return self.toks[lo]._src[lo:hi] if False else ""


def find_functions(toks):
    out = []
    for i, t in enumerate(toks):
        if t.kind != "ident" or t.text != "fn":
            continue
        if i + 1 >= len(toks) or toks[i + 1].kind != "ident":
            continue
        name = toks[i + 1].text
        j = i + 2
        while j < len(toks) and not (toks[j].kind == "punct" and toks[j].text == "("):
            if toks[j].text in {";", "{", "}"}:
                j = -1
                break
            j += 1
        if j < 0 or j >= len(toks):
            continue
        close = None
        depth = 0
        for k in range(j, len(toks)):
            if toks[k].text == "(":
                depth += 1
            elif toks[k].text == ")":
                depth -= 1
                if depth == 0:
                    close = k
                    break
        if close is None:
            continue
        k = close + 1
        # skip return type / where clause to the body brace; a ';' means a decl
        while k < len(toks) and toks[k].text not in {"{", ";"}:
            k += 1
        if k >= len(toks) or toks[k].text != "{":
            continue
        # matching body brace
        depth = 0
        body_close = None
        for q in range(k, len(toks)):
            if toks[q].text == "{":
                depth += 1
            elif toks[q].text == "}":
                depth -= 1
                if depth == 0:
                    body_close = q
                    break
        if body_close is None:
            continue
        impl = enclosing_impl(toks, i)
        out.append(Func(name, i, k, body_close, toks, impl))
    return out


def enclosing_impl(toks, idx):
    for k in range(idx - 1, max(-1, idx - 3000), -1):
        if toks[k].kind == "ident" and toks[k].text == "impl":
            # header ends at the next '{' (the impl body) or ';'
            j = k + 1
            while j < len(toks) and toks[j].text not in {"{", ";"}:
                j += 1
            if j < len(toks) and toks[j].text == "{" and j < idx:
                # last type-ish identifier before the brace, prefer after 'for'
                ids = [t.text for t in toks[k + 1:j] if t.kind == "ident"]
                if "for" in ids:
                    ids = ids[ids.index("for") + 1:]
                return ids[-1] if ids else ""
            if j < len(toks) and toks[j].text == ";" and j < idx:
                return ""
    return ""


def block_of(toks, pairs, idx):
    """Innermost enclosing brace pair around idx."""
    for k in range(idx, -1, -1):
        if toks[k].text == "{" and k in pairs and pairs[k] > idx:
            return k, pairs[k]
    return 0, len(toks) - 1


def line_text(src, toks, lo, hi):
    if lo >= hi:
        return ""
    start, end = toks[lo].start, toks[hi - 1].end
    a = src.rfind("\n", 0, start) + 1
    b = src.find("\n", end)
    b = len(src) if b < 0 else b
    lines = src[a:b].splitlines()
    if len(lines) > 15:
        lines = lines[:15]
    return "\n".join(x.rstrip() for x in lines)


def bad_actions(toks, func, pairs):
    """Yield (idx, field_or_call, kind, value) for bad-marking actions."""
    out = []
    for i in range(func.a, func.b):
        t = toks[i]
        if t.kind != "ident":
            continue
        # field assignment: field = value / field += 1 / self.field = ...
        asg = assignment(toks, i)
        if asg:
            field, (op, ahead) = t.text, asg
            looks_bad = (BAD_FIELD.match(field) or field in
                         {"sending", "in_flight", "in_progress", "busy"})
            if not looks_bad:
                # `state = ConnectionState::Invalid`; plain booleans are too noisy here
                looks_bad = any(v in {"Invalid", "Broken", "Closed", "Poisoned",
                                      "Tainted", "Bad", "Failed", "Dirty", "Dead"}
                                for v in ahead)
            if looks_bad:
                badval = next((v for v in ahead if v in BAD_VALUES), "")
                out.append((i, field, "assign", badval))
            continue
        # .close() / .abort() / .taint() / mark_*
        if i + 1 < func.b and toks[i + 1].text == "(" and i > func.a and toks[i - 1].text == ".":
            if CLOSE_CALL.match(t.text) or re.match(
                    r"mark_(?:broken|tainted|closed|invalid|failed|unhealthy|dirty|poisoned)$",
                    t.text):
                out.append((i, t.text, "call", ""))
    return out


def value_is_bad(v):
    return bool(BAD_VALUE.match(v))


def value_is_good(v):
    return v == "false" or v in {"Present", "Valid", "Healthy", "Ok", "Clean", "Alive", "Good"}


BAD_VALUES = {"true", "Invalid", "Broken", "Closed", "Poisoned", "Tainted", "Bad",
              "Failed", "Dirty", "Dead"}
GOOD_VALUES = {"false", "Present", "Valid", "Healthy", "Clean", "Alive", "Good"}


def assignment(toks, i):
    """(op, lookahead) if toks[i] is assigned at i; else None."""
    if i + 1 >= len(toks) or toks[i + 1].text not in {"=", "+=", "-="}:
        return None
    return toks[i + 1].text, [t.text for t in toks[i + 2:i + 7]]


def guards_in_file(src):
    """Names of types with a Drop impl whose body discards, and guard-ish macros."""
    types = set()
    for m in re.finditer(r"impl\s*(?:<[^>]*>\s*)?Drop\s+for\s+([A-Za-z_][A-Za-z0-9_]*)", src):
        types.add(m.group(1))
    return types


def drop_is_discarding(src, type_name):
    for m in re.finditer(
        r"impl\s*(?:<[^>]*>\s*)?Drop\s+for\s+" + re.escape(type_name) + r"[^{]*\{", src
    ):
        # brace match on raw text
        i, depth = m.end() - 1, 0
        while i < len(src):
            if src[i] == "{":
                depth += 1
            elif src[i] == "}":
                depth -= 1
                if depth == 0:
                    break
            i += 1
        body = src[m.end():i]
        if re.search(r"\b(?:start_rollback|rollback|close|abort|shutdown|take|discard|"
                     r"detach|Invalid|Broken|remove|drop)\b", body):
            if not re.search(r"\b(?:put_back|return_to_pool|recycle)\b", body):
                return True
    return False


def pessimistic_guard(toks, func, aw, bad_idx, field, src, drop_types, any_field=False):
    """Evidence that the exchange is guarded across its await."""
    # (a) bad marker before the await and cleared after it
    for i in range(func.a, aw):
        if toks[i].kind != "ident" or (not any_field and toks[i].text != field):
            continue
        asg = assignment(toks, i)
        if not asg:
            continue
        op, ahead = asg
        if any(v in BAD_VALUES for v in ahead) or (
                toks[i].text in {"sending", "in_flight", "in_progress", "busy"} and "true" in ahead):
            for j in range(aw, func.b):
                if toks[j].kind != "ident" or (not any_field and toks[j].text != field):
                    continue
                asg2 = assignment(toks, j)
                if asg2 and any(v in GOOD_VALUES for v in asg2[1]):
                    return "pessimistic-flag"
                # a scope guard variable created before the await
    # (b) local Drop guard created before the await
    for i in range(max(func.a, aw - 60), aw):
        if toks[i].kind == "ident" and toks[i].text == "let" and i + 1 < aw:
            var = toks[i + 1].text
            seg = " ".join(t.text for t in toks[i:i + 14])
            for dt in drop_types:
                if re.search(r"\b" + re.escape(dt) + r"\b", seg) and drop_is_discarding(src, dt):
                    # must still be in scope after the await: same block
                    return "drop-guard:" + dt
            if GUARD_INIT.search(seg) or GUARD_MACRO.search(seg):
                return "guard-init"
    # (c) take-and-restore
    for i in range(max(func.a, aw - 40), aw):
        if toks[i].text == "take" and i > func.a and toks[i - 1].text == ".":
            for j in range(aw, func.b):
                if toks[j].kind == "ident" and toks[j].text == field and j + 2 < func.b \
                        and toks[j + 1].text == "=" and toks[j + 2].text in {"Some", "Ok"}:
                    return "take-and-restore"
    return ""


def error_context(toks, aw, bad_idx):
    """Is there an error arm / failure test linking the await to the marking?"""
    lo = max(0, aw - 8)
    hi = min(len(toks), bad_idx + 8)
    for i in range(lo, hi):
        if toks[i].kind == "ident" and toks[i].text in {
            "Err", "is_err", "map_err", "inspect_err", "unwrap_or_else", "expect_err"
        }:
            return True
        if toks[i].text == "?" and i > aw:
            return True
    return False


def nearest_await(toks, func, idx, limit=160):
    for i in range(idx, max(func.a, idx - limit), -1):
        if toks[i].kind == "ident" and toks[i].text == "await":
            return i
    return None


def parse_macros(src, toks):
    """name -> (start_tok, end_tok, evidence_text) for macro_rules!."""
    out = {}
    for i, t in enumerate(toks):
        if t.text != "macro_rules" or i + 2 >= len(toks):
            continue
        if toks[i + 1].text != "!" or toks[i + 2].kind != "ident":
            continue
        name = toks[i + 2].text
        j = i + 3
        while j < len(toks) and toks[j].text != "{":
            j += 1
        if j >= len(toks):
            continue
        depth, close = 0, None
        for k in range(j, len(toks)):
            if toks[k].text == "{":
                depth += 1
            elif toks[k].text == "}":
                depth -= 1
                if depth == 0:
                    close = k
                    break
        if close is None:
            continue
        body = src[toks[j].start:toks[close].end]
        if re.search(r"\bErr\b", body) and (
            re.search(r"\b(?:broken|tainted|poison\w*|closed|panic\w*|invalid|dirty|"
                      r"healthy|failed|sending|abort|close|shutdown|mark_\w*)\b", body)
            or "mark_" in body
        ):
            out[name] = (j, close, body)
    return out


TEST_ATTR = re.compile(r"#\s*\[\s*(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*test\b")


def is_test_func(src, toks, fn_idx):
    # attribute directly above the fn
    line = toks[fn_idx].line
    lines = src.splitlines()
    lo = max(0, line - 6)
    if TEST_ATTR.search("\n".join(lines[lo:line])):
        return True
    # Health checks such as lettre's `test_connected` start with `test_` but
    # are production code; keep them.
    name = toks[fn_idx + 1].text if fn_idx + 1 < len(toks) else ""
    if name in {"test_connected", "test_connection", "test_ping"}:
        return False
    if name.startswith("test_") or name.startswith("should_"):
        return True
    # #[cfg(test)] mod ... enclosing: look for the nearest mod and cfg(test) above
    prefix = "\n".join(lines[max(0, line - 80):line])
    if re.search(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]", prefix):
        return True
    return False


def scan_file(path, crate, version, src):
    toks = lex(src)
    pairs = build_matches(toks)
    funcs = find_functions(toks)
    drop_types = guards_in_file(src)
    macros = parse_macros(src, toks)
    hits = []

    def add(line, pattern, snippet, guarded, confidence, detail):
        hits.append({
            "crate": crate, "version": version,
            "file": str(path), "line": line, "pattern": pattern,
            "guarded": guarded, "confidence": confidence,
            "detail": detail, "snippet": snippet,
        })

    # ---- pattern A: error path marks bad, no cancellation guard -----------
    for f in funcs:
        fname = f.name
        if is_test_func(src, toks, f.start):
            continue
        bads = bad_actions(toks, f, pairs)
        if not bads:
            continue
        for (bi, name, kind, val) in bads:
            if kind == "assign" and not (value_is_bad(val) or name in
                                         {"sending", "in_flight", "in_progress", "busy"}):
                continue
            aw = nearest_await(toks, f, bi)
            if aw is None:
                continue
            # the await must be the exchange, not a cleanup call after the mark
            if toks[aw].start < toks[bi].start and toks[bi].line - toks[aw].line > 12:
                continue
            if not error_context(toks, aw, bi):
                continue
            g = pessimistic_guard(toks, f, aw, bi, name, src, drop_types)
            lo = max(f.a, aw - 10)
            hi = min(f.b, bi + 6)
            add(toks[aw].line, "A", line_text(src, toks, lo, hi),
                g if g else "no", "high" if g == "" and kind == "assign" else "medium",
                f"fn {fname}: {kind} `{name}` in error path after await; guard={g or 'none'}")

    # ---- macro error arms with an await at the call site ------------------
    for name, (mo, mc, body) in macros.items():
        for i, t in enumerate(toks):
            if t.kind != "ident" or t.text != name:
                continue
            if i + 1 >= len(toks) or toks[i + 1].text != "!":
                continue
            j = i + 2
            if j >= len(toks) or toks[j].text not in "([{":
                continue
            opener = toks[j].text
            closer = {"(": ")", "[": "]", "{": "}"}[opener]
            depth, close = 0, None
            for k in range(j, len(toks)):
                if toks[k].text == opener:
                    depth += 1
                elif toks[k].text == closer:
                    depth -= 1
                    if depth == 0:
                        close = k
                        break
            if close is None:
                continue
            args = toks[j:close + 1]
            if not any(x.text == "await" for x in args):
                continue
            f = next((x for x in funcs if x.a <= i <= x.b), None)
            if f is None:
                continue
            aw = next((k for k in range(j, close) if toks[k].text == "await"), j)
            g = pessimistic_guard(toks, f, aw, close, "", src, drop_types, any_field=True)
            add(toks[i].line, "A", line_text(src, toks, max(f.a, aw - 10), min(f.b, close + 3)),
                g if g else "no", "high",
                f"macro {name}! Err arm marks bad; call's await at line {toks[aw].line}; guard={g or 'none'}")

    # ---- pattern B: durable state committed after the round trip ----------
    for f in funcs:
        if is_test_func(src, toks, f.start):
            continue
        if not any(t.text == "await" for t in toks[f.a:f.b]):
            continue
        for i in range(f.a, f.b - 2):
            t = toks[i]
            if t.kind != "ident" or not SESSION_FIELD.match(t.text):
                continue
            if toks[i + 1].text not in {"=", "+=", "-="}:
                continue
            # skip obvious locals / constants
            if t.text in {"count", "state"} and i > 0 and toks[i - 1].text == "let":
                continue
            val = toks[i + 2].text
            if not (value_is_bad(val) or val in {"1", "depth", "true"} or toks[i + 1].text in {"+=", "-="}):
                continue
            aw = nearest_await(toks, f, i, limit=80)
            if aw is None:
                continue
            # state must be *after* the await, not before
            g = pessimistic_guard(toks, f, aw, i, t.text, src, drop_types)
            # a pre-await bump of the same field is the sqlx-style fix
            pre = [k for k in range(max(f.a, aw - 40), aw)
                   if toks[k].kind == "ident" and toks[k].text == t.text
                   and k + 1 < f.b and toks[k + 1].text in {"+=", "="}]
            if pre:
                g = g or "pre-await-claim"
            if not g:
                add(toks[i].line, "B", line_text(src, toks, max(f.a, aw - 8), min(f.b, i + 4)),
                    "no", "high" if t.text in {"transaction_depth", "in_transaction"} else "medium",
                    f"fn {f.name}: `{t.text} {toks[i+1].text}` after await at line {toks[aw].line}")

    # ---- pattern C: pool-manager validity --------------------------------
    for f in funcs:
        if is_test_func(src, toks, f.start):
            continue
        if CLEAR_NAME.match(f.name):
            body = " ".join(t.text for t in toks[f.a:f.b])
            if re.search(r"\breturn\s+false\b|\bfalse\s*$|\bfalse\b", body) and "false" in body:
                add(toks[f.start].line, "C", line_text(src, toks, f.a, min(f.b, f.a + 12)),
                    "no", "high", f"fn {f.name} returns false / never marks breakage")
            else:
                fields = {t.text for t in toks[f.a:f.b] if t.kind == "ident"}
                pessimistic = ""
                error_names = {"abort", "close", "shutdown", "taint", "set_broken",
                               "mark_broken", "mark_tainted", "fail", "handle_error",
                               "disconnect", "drop"}
                for g in funcs:
                    if g.name in error_names:
                        continue
                    for i in range(g.a, g.b):
                        if toks[i].text not in fields:
                            continue
                        asg = assignment(toks, i)
                        if not asg or not any(v in BAD_VALUES for v in asg[1]):
                            continue
                        if any(t.text == "await" for t in toks[i:g.b]):
                            pessimistic = g.name
                            break
                    if pessimistic:
                        break
                add(toks[f.start].line, "C", line_text(src, toks, f.a, min(f.b, f.a + 12)),
                    "pessimistic-flag:" + pessimistic if pessimistic else "no",
                    "low" if pessimistic else "medium",
                    (f"fn {f.name} reads a flag set before an await in {pessimistic}"
                     if pessimistic else
                     f"fn {f.name} reads a flag that is not pessimistically set before an await"))
        if VALIDATE_NAME.match(f.name) and any(t.text == "await" for t in toks[f.a:f.b]):
            body = " ".join(t.text for t in toks[f.a:f.b])
            broad = bool(re.search(r"is_ok\s*\(|is_positive\s*\(|Ok\s*\(\s*_\s*\)", body))
            if broad:
                add(toks[f.start].line, "C", line_text(src, toks, f.a, min(f.b, f.a + 14)),
                    "no", "high", f"fn {f.name} does I/O then accepts any positive/Ok reply")

    return hits


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", type=pathlib.Path,
                    default=pathlib.Path.home() / "dropwise-scan-src/src")
    ap.add_argument("--out", type=pathlib.Path, default=pathlib.Path("hits.csv"))
    ap.add_argument("--only", default="")
    ap.add_argument("--quiet", action="store_true")
    a = ap.parse_args()
    root = a.root.expanduser().resolve()
    only = {x.strip() for x in a.only.split(",") if x.strip()}
    all_hits = []
    crates = 0
    files = 0
    skipped = []
    dirs = sorted(root.iterdir()) if root.is_dir() else []
    for d in dirs:
        if not d.is_dir():
            continue
        m = re.match(r"(.+)-(\d+\.[0-9A-Za-z.\-+]+)$", d.name)
        if not m:
            skipped.append(d.name)
            continue
        crate, version = m.group(1), m.group(2)
        if only and crate not in only:
            continue
        srcroot = d / "src"
        if not srcroot.is_dir():
            skipped.append(d.name + " (no src)")
            continue
        crates += 1
        for p in sorted(srcroot.rglob("*.rs")):
            rel = p.relative_to(d)
            if any(part in {"tests", "benches", "examples"} for part in rel.parts):
                continue
            try:
                src = p.read_text(errors="replace")
            except OSError:
                continue
            files += 1
            for h in scan_file(rel, crate, version, src):
                all_hits.append(h)
        if not a.quiet:
            print(f"scanned {crate} {version}", flush=True)
    seen = set()
    unique = []
    for h in all_hits:
        key = (h["crate"], h["file"], h["line"], h["pattern"])
        if key in seen:
            continue
        seen.add(key)
        unique.append(h)
    all_hits = unique
    fields = ["crate", "version", "file", "line", "pattern", "guarded",
              "confidence", "detail", "snippet"]
    a.out.parent.mkdir(parents=True, exist_ok=True)
    with a.out.open("w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=fields)
        w.writeheader()
        w.writerows(all_hits)
    print(f"crates={crates} files={files} hits={len(all_hits)} -> {a.out}")
    if skipped:
        print("skipped:", " ".join(skipped[:10]), file=sys.stderr)


if __name__ == "__main__":
    main()
