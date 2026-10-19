#!/usr/bin/env python3
"""Static pairing scan: pool manager adapter + underlying protocol client.

This scanner finds crates that implement one of the three pool manager traits
(bb8::ManageConnection, deadpool::managed::Manager, mobc::Manager), resolves the
managed connection type to the underlying client crate, and judges whether the
pool validation can mistake a leftover reply from an interrupted exchange for
the expected one.

It is a *text/token* analysis.  Downloaded sources are never built, imported or
executed.  The scanner is deliberately explicit about uncertainty: unresolved
connection types, missing client sources and ambiguous definitions are written
as such instead of guessed.

Suspicion rule (see PAIRING.md for the full rationale)
  * multiplexed clients are exempt (the driver routes replies by request id /
    response ordering, so a cancelled future does not leave the next user's
    reply position ambiguous);
  * a validation method with no await is a synchronous state check and is not
    suspicious by this rule;
  * if the underlying client has a cancellation-sensitive "exchange in
    progress" flag, the manager must read it (directly or through
    has_broken/is_broken/recycle/check); otherwise the pair is suspicious;
  * if the client has no such flag, validation that sends a request is
    suspicious even when the reply is checked strictly, because a reply left by
    an interrupted validation is indistinguishable from the expected one;
  * one exception is modelled: a pool core that makes the slot unusable before
    the validation await (a "shield") removes the validation-cancellation
    window.  With a shield, a strict reply check protects the pair; a loose
    check (accepts any successful reply) is still suspicious because a
    cancelled *user* exchange can leave an arbitrary reply behind.

Usage:
  python3 -I pairing.py --root ~/dropwise-scan-src/src --out pairs.csv
  python3 -I pairing.py --root ~/dropwise-scan-src/src --out pairs.csv \
      --only diesel-async,bb8-memcached
  python3 -I pairing.py --root ~/dropwise-scan-src/src --out pairs.csv \
      --calibration-dir /tmp/pairing-calib \
      --pool-patch bb8=prospective/t2-bb8/patches/bb8-invalid-until-validated.diff
"""
import argparse
import csv
import pathlib
import re
import sys

# ---------------------------------------------------------------------------
# Lexer (adapted from scan.py; doc comments are skipped, so doc examples do not
# produce fake impls).  No source is executed or imported.
# ---------------------------------------------------------------------------

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
            start_line = line
            text = src[i:k + len(close)]
            line += text.count("\n")
            toks.append(Tok("str", text, i, k + len(close), start_line))
            i = k + len(close)
            continue
        if c == '"' or (c in "bc" and i + 1 < n and src[i + 1] == '"'):
            start_line = line
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
            toks.append(Tok("str", src[i:j], i, j, start_line))
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
        m = re.match(r"[0-9][0-9A-Za-z_]*(?:\.[0-9][0-9A-Za-z_]*)?", src[i:])
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
            for k in range(len(stack) - 1, -1, -1):
                if opens[toks[stack[k]].text] == t.text:
                    j = stack.pop(k)
                    pairs[j] = i
                    pairs[i] = j
                    break
    return pairs


def angle_end(toks, i):
    """Index of the '>' matching toks[i] == '<' (or None)."""
    depth = 0
    j = i
    while j < len(toks):
        t = toks[j].text
        if t == "<":
            depth += 1
        elif t == ">":
            depth -= 1
            if depth == 0:
                return j
        elif t in {";", "{", "}"} and depth == 0:
            return None
        j += 1
    return None


def split_top(toks, lo, hi, sep=","):
    """Split [lo, hi) on sep at bracket/angle depth 0."""
    out, cur = [], []
    depth = 0
    for i in range(lo, hi):
        t = toks[i].text
        if t in {"(", "[", "{", "<"}:
            depth += 1
        elif t in {")", "]", "}", ">"}:
            depth -= 1
        if t == sep and depth == 0:
            if cur:
                out.append((cur[0], cur[-1] + 1))
            cur = []
            continue
        cur.append(i)
    if cur:
        out.append((cur[0], cur[-1] + 1))
    return out


def is_macro_body(toks, idx):
    """True if idx sits inside a macro_rules! body (no expansion)."""
    for k in range(max(0, idx - 4000), idx):
        if toks[k].text == "macro_rules" and k + 2 < len(toks) and toks[k + 1].text == "!":
            j = k + 3
            while j < idx and j < len(toks) and toks[j].text != "{":
                j += 1
            if j >= idx:
                continue
            depth = 0
            for q in range(j, len(toks)):
                if toks[q].text == "{":
                    depth += 1
                elif toks[q].text == "}":
                    depth -= 1
                    if depth == 0:
                        if q > idx:
                            return True
                        break
    return False


# ---------------------------------------------------------------------------
# Use / defs / impls / functions
# ---------------------------------------------------------------------------

def parse_use_tree(toks, lo, hi):
    """Parse a use tree in [lo,hi); return {alias: [path segments]}."""
    out = {}

    def rec(a, b, prefix):
        for (p, q) in split_top(toks, a, b, ","):
            if p >= q:
                continue
            # a nested brace group: `prefix::{...}`
            brace = None
            depth = 0
            for x in range(p, q):
                tx = toks[x].text
                if tx == "{":
                    brace = x
                    break
                if tx in {"(", "["}:
                    depth += 1
                elif tx in {")", "]"}:
                    depth -= 1
            if brace is not None:
                pre = _path_segments(toks, p, brace)
                depth = 0
                end = None
                for x in range(brace, q):
                    if toks[x].text == "{":
                        depth += 1
                    elif toks[x].text == "}":
                        depth -= 1
                        if depth == 0:
                            end = x
                            break
                if end is not None:
                    rec(brace + 1, end, prefix + pre)
                continue
            # `self` inside a group refers to the group prefix
            if p + 1 == q and toks[p].text == "self":
                if prefix:
                    out[prefix[-1]] = list(prefix)
                continue
            as_idx = None
            for x in range(p, q):
                if toks[x].text == "as":
                    as_idx = x
                    break
            if as_idx is not None:
                segs = _path_segments(toks, p, as_idx)
                alias = toks[as_idx + 1].text if as_idx + 1 < q else ""
                if alias and segs:
                    out[alias] = prefix + segs
                continue
            segs = _path_segments(toks, p, q)
            if not segs or segs[-1] == "*":
                continue
            out[segs[-1]] = prefix + segs

    rec(lo, hi, [])
    return out


def _path_segments(toks, lo, hi):
    segs = []
    i = lo
    while i < hi:
        t = toks[i]
        if t.text == "::":
            i += 1
            continue
        if t.kind == "ident":
            if t.text in {"pub", "use", "crate", "self", "super"}:
                if t.text in {"crate", "self", "super"}:
                    segs.append(t.text)
                i += 1
                continue
            segs.append(t.text)
            i += 1
            continue
        if t.text == "*":
            segs.append("*")
            i += 1
            continue
        i += 1
    return segs


def file_imports(toks, pairs):
    out = {}
    for i, t in enumerate(toks):
        if t.kind != "ident" or t.text != "use":
            continue
        j = i + 1
        depth = 0
        end = None
        while j < len(toks):
            if toks[j].text == "{":
                depth += 1
            elif toks[j].text == "}":
                depth -= 1
            elif toks[j].text == ";" and depth == 0:
                end = j
                break
            j += 1
        if end is None:
            continue
        out.update(parse_use_tree(toks, i + 1, end))
    return out


class Def:
    def __init__(self, kind, name, path, line, generics, fields, rhs, mod):
        self.kind = kind          # struct | enum | type
        self.name = name
        self.path = path          # relative source path
        self.line = line
        self.generics = generics  # set of generic parameter names
        self.fields = fields      # list of (field_name_or_None, [type token texts], line)
        self.rhs = rhs            # list of token texts for type aliases
        self.mod = mod            # module-ish path derived from the file


class Func:
    def __init__(self, name, path, line, start, body_open, body_close, toks,
                 impl_line=None):
        self.name = name
        self.path = path
        self.line = line
        self.start = start
        self.a = body_open + 1
        self.b = body_close
        self.toks = toks
        self.impl_line = impl_line

    def has_await(self):
        return any(t.text == "await" for t in self.toks[self.a:self.b])


def parse_defs(toks, path, skip_ranges=None, test_ranges=None):
    out = []
    mod = _mod_of(path)
    i = 0
    while i < len(toks):
        t = toks[i]
        if t.kind != "ident" or t.text not in {"struct", "enum", "type"} or is_macro_body(toks, i):
            i += 1
            continue
        if skip_ranges and any(a <= i <= b for (a, b) in skip_ranges):
            i += 1
            continue
        if test_ranges and in_ranges(t.line, test_ranges):
            i += 1
            continue
        if i + 1 >= len(toks) or toks[i + 1].kind != "ident":
            i += 1
            continue
        kind, name = t.text, toks[i + 1].text
        j = i + 2
        generics = set()
        if j < len(toks) and toks[j].text == "<":
            e = angle_end(toks, j)
            if e is not None:
                for (p, q) in split_top(toks, j + 1, e, ","):
                    ids = [x for x in range(p, q) if toks[x].kind == "ident"]
                    if ids:
                        first = toks[ids[0]].text
                        if first == "const" and len(ids) > 1:
                            generics.add(toks[ids[1]].text)
                        elif first not in {"const"}:
                            generics.add(first)
                j = e + 1
        if kind == "type":
            # find '=' then RHS to ';' at depth 0
            k = j
            while k < len(toks) and toks[k].text not in {"=", ";"}:
                k += 1
            if k < len(toks) and toks[k].text == "=":
                e = k + 1
                depth = 0
                while e < len(toks):
                    if toks[e].text in {"(", "[", "{", "<"}:
                        depth += 1
                    elif toks[e].text in {")", "]", "}", ">"}:
                        depth -= 1
                    elif toks[e].text == ";" and depth == 0:
                        break
                    e += 1
                out.append(Def("type", name, path, t.line, generics,
                               [], [x.text for x in toks[k + 1:e]], mod))
            i = k + 1
            continue
        k = j
        while k < len(toks) and toks[k].text not in {"{", "(", ";"}:
            k += 1
        if k >= len(toks):
            break
        fields = []
        if toks[k].text == "{":
            end = _brace_end(toks, k)
            if end is None:
                break
            if kind == "struct":
                fields = _named_fields(toks, k + 1, end, path)
            else:
                fields = _enum_fields(toks, k + 1, end, path)
            i = end + 1
        elif toks[k].text == "(":
            depth = 0
            end = None
            for q in range(k, len(toks)):
                if toks[q].text == "(":
                    depth += 1
                elif toks[q].text == ")":
                    depth -= 1
                    if depth == 0:
                        end = q
                        break
            if end is None:
                break
            fields = _tuple_fields(toks, k + 1, end, path)
            i = end + 1
        else:
            i = k + 1
        out.append(Def(kind, name, path, t.line, generics, fields, [], mod))
    return out


def _brace_end(toks, i):
    depth = 0
    for q in range(i, len(toks)):
        if toks[q].text == "{":
            depth += 1
        elif toks[q].text == "}":
            depth -= 1
            if depth == 0:
                return q
    return None


def _skip_attrs(toks, lo, hi):
    i = lo
    while i < hi and toks[i].text == "#":
        j = i + 1
        if j < hi and toks[j].text == "!":
            j += 1
        if j < hi and toks[j].text == "[":
            depth = 0
            while j < hi:
                if toks[j].text == "[":
                    depth += 1
                elif toks[j].text == "]":
                    depth -= 1
                    if depth == 0:
                        j += 1
                        break
                j += 1
        i = j
    while i < hi and toks[i].kind == "ident" and toks[i].text in {"pub", "crate", "super"}:
        i += 1
        if i < hi and toks[i].text == "(":
            depth = 0
            while i < hi:
                if toks[i].text == "(":
                    depth += 1
                elif toks[i].text == ")":
                    depth -= 1
                    if depth == 0:
                        i += 1
                        break
                i += 1
    return i


def _named_fields(toks, lo, hi, path):
    fields = []
    for (p, q) in split_top(toks, lo, hi, ","):
        p = _skip_attrs(toks, p, q)
        colon = None
        depth = 0
        for x in range(p, q):
            if toks[x].text in {"(", "[", "{", "<"}:
                depth += 1
            elif toks[x].text in {")", "]", "}", ">"}:
                depth -= 1
            elif toks[x].text == ":" and depth == 0:
                colon = x
                break
        if colon is None:
            continue
        ids = [x for x in range(p, colon) if toks[x].kind == "ident"]
        if not ids:
            continue
        fields.append((toks[ids[-1]].text, [x.text for x in toks[colon + 1:q]],
                       toks[ids[-1]].line))
    return fields


def _tuple_fields(toks, lo, hi, path):
    out = []
    for (p, q) in split_top(toks, lo, hi, ","):
        p = _skip_attrs(toks, p, q)
        if p < q:
            out.append((None, [x.text for x in toks[p:q]], toks[p].line))
    return out


def _enum_fields(toks, lo, hi, path):
    out = []
    for (p, q) in split_top(toks, lo, hi, ","):
        p = _skip_attrs(toks, p, q)
        if p >= q:
            continue
        name = toks[p].text
        k = p + 1
        if k < q and toks[k].text == "(":
            depth = 0
            for x in range(k, q):
                if toks[x].text == "(":
                    depth += 1
                elif toks[x].text == ")":
                    depth -= 1
                    if depth == 0:
                        for (fn, ft, fl) in _tuple_fields(toks, k + 1, x, path):
                            # variant name is not a field name
                            out.append((None, ft, fl))
                        break
        elif k < q and toks[k].text == "{":
            depth = 0
            for x in range(k, q):
                if toks[x].text == "{":
                    depth += 1
                elif toks[x].text == "}":
                    depth -= 1
                    if depth == 0:
                        for (fn, ft, fl) in _named_fields(toks, k + 1, x, path):
                            out.append((name, ft, fl))
                        break
    return out


def _mod_of(path):
    p = pathlib.PurePosixPath(path)
    parts = list(p.parts)
    if parts and parts[0] == "src":
        parts = parts[1:]
    if parts and parts[-1].endswith(".rs"):
        last = parts[-1][:-3]
        parts = parts[:-1] + ([] if last in {"lib", "mod", "main"} else [last])
    return "/".join(parts)


def cfg_test_line_ranges(src):
    """Line ranges (1-based, inclusive) of #[cfg(test)] mod bodies."""
    out = []
    for m in re.finditer(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]\s*(?:pub\s+)?mod\s+[A-Za-z_][A-Za-z0-9_]*\s*\{", src):
        line = src[:m.start()].count("\n") + 1
        brace = src.find("{", m.start(), m.end())
        depth, q = 0, brace
        while q < len(src):
            if src[q] == "{":
                depth += 1
            elif src[q] == "}":
                depth -= 1
                if depth == 0:
                    break
            q += 1
        end_line = src[:q].count("\n") + 1
        out.append((line, end_line))
    return out


def in_ranges(line, ranges):
    return any(a <= line <= b for (a, b) in ranges)


class Impl:
    def __init__(self, trait_segs, self_name, path, line, body_open, body_close,
                 toks, generics, bounds):
        self.trait_segs = trait_segs
        self.self_name = self_name
        self.path = path
        self.line = line
        self.body_open = body_open
        self.body_close = body_close
        self.toks = toks
        self.generics = generics      # set of generic param names
        self.bounds = bounds          # param -> set(bound last idents)
        self.kind = None


def parse_impls(toks, path, pairs, test_ranges=None):
    out = []
    for i, t in enumerate(toks):
        if t.kind != "ident" or t.text != "impl" or is_macro_body(toks, i):
            continue
        if test_ranges and in_ranges(t.line, test_ranges):
            continue
        j = i + 1
        # header ends at the first top-level '{'
        depth_angle = 0
        for_idx = None
        body = None
        k = j
        while k < len(toks):
            x = toks[k].text
            if x == "<":
                depth_angle += 1
            elif x == ">":
                depth_angle -= 1
            elif x == "{" and depth_angle <= 0:
                body = k
                break
            elif x == ";" and depth_angle <= 0:
                break
            elif x == "for" and depth_angle <= 0 and for_idx is None:
                for_idx = k
            k += 1
        if body is None or for_idx is None:
            continue
        # generic parameter list (if any) and bounds
        generics = set()
        bounds = {}
        g = i + 1
        if g < len(toks) and toks[g].text == "<":
            e = angle_end(toks, g)
            if e is None or e >= for_idx:
                continue
            for (p, q) in split_top(toks, g + 1, e, ","):
                ids = [x for x in range(p, q) if toks[x].kind == "ident"]
                if not ids:
                    continue
                if toks[ids[0]].text == "const" and len(ids) > 1:
                    pname = toks[ids[1]].text
                elif toks[p].kind == "life":
                    continue
                else:
                    pname = toks[ids[0]].text
                generics.add(pname)
                colon = None
                d = 0
                for x in range(p, q):
                    if toks[x].text in {"(", "[", "{", "<"}:
                        d += 1
                    elif toks[x].text in {")", "]", "}", ">"}:
                        d -= 1
                    elif toks[x].text == ":" and d == 0:
                        colon = x
                        break
                if colon is not None:
                    bs = set()
                    for x in range(colon + 1, q):
                        if toks[x].kind == "ident" and toks[x].text not in {
                                "Send", "Sync", "Sized", "Unpin", "static"}:
                            bs.add(toks[x].text)
                    bounds[pname] = bs
            g = e + 1
        trait_segs = []
        x = g
        while x < for_idx:
            if toks[x].kind == "ident":
                trait_segs.append(toks[x].text)
            x += 1
        # self type name: last ident before 'where' / '{'
        self_name = ""
        where = None
        for x in range(for_idx + 1, body):
            if toks[x].text == "where":
                where = x
                break
        end_self = where if where is not None else body
        # base type name: last identifier at angle depth 0 (skip generic args)
        self_name = ""
        depth = 0
        for x in range(for_idx + 1, end_self):
            tx = toks[x].text
            if tx == "<":
                depth += 1
            elif tx == ">":
                depth -= 1
            elif depth == 0 and toks[x].kind == "ident" and tx not in {"where", "for"}:
                self_name = tx
        if not self_name:
            ids = [toks[x].text for x in range(for_idx + 1, end_self) if toks[x].kind == "ident"]
            if ids:
                self_name = ids[-1]
        # where-clause bounds: `where C: PoolableConnection, ...`
        if where is not None:
            for (p, q) in split_top(toks, where + 1, body, ","):
                if p >= q or toks[p].kind != "ident" or toks[p].text not in generics:
                    continue
                colon = None
                d = 0
                for x in range(p, q):
                    tx = toks[x].text
                    if tx in {"(", "[", "{", "<"}:
                        d += 1
                    elif tx in {")", "]", "}", ">"}:
                        d -= 1
                    elif tx == ":" and d == 0:
                        colon = x
                        break
                if colon is None:
                    continue
                bs = bounds.setdefault(toks[p].text, set())
                for x in range(colon + 1, q):
                    if toks[x].kind == "ident" and toks[x].text not in {
                            "Send", "Sync", "Sized", "Unpin", "static"}:
                        bs.add(toks[x].text)
        close = pairs.get(body)
        if close is None:
            continue
        out.append(Impl(trait_segs, self_name, path, t.line, body, close,
                        toks, generics, bounds))
    return out


def find_functions(toks, path, pairs, impls):
    out = []
    for i, t in enumerate(toks):
        if t.kind != "ident" or t.text != "fn" or is_macro_body(toks, i):
            continue
        if i + 1 >= len(toks) or toks[i + 1].kind != "ident":
            continue
        name = toks[i + 1].text
        j = i + 2
        while j < len(toks) and toks[j].text != "(":
            if toks[j].text in {";", "{", "}"}:
                j = -1
                break
            j += 1
        if j < 0 or j >= len(toks):
            continue
        close_paren = pairs.get(j)
        if close_paren is None:
            continue
        k = close_paren + 1
        while k < len(toks) and toks[k].text not in {"{", ";"}:
            k += 1
        if k >= len(toks) or toks[k].text != "{":
            continue
        body_close = pairs.get(k)
        if body_close is None:
            continue
        impl_line = None
        for im in impls:
            if im.body_open < i < im.body_close:
                impl_line = im.line
                break
        out.append(Func(name, path, t.line, i, k, body_close, toks, impl_line))
    return out


# ---------------------------------------------------------------------------
# Crates and lazy models
# ---------------------------------------------------------------------------

DIR_RE = re.compile(r"^(.+)-(\d+\.[0-9A-Za-z.\-+]+)$")
VARIANT_RE = re.compile(r"^(.*?)(?:\+(fixed|patched|calibration))?$")


class CrateSource:
    def __init__(self, name, version, variant, path):
        self.name = name
        self.version = version
        self.variant = variant
        self.path = path
        self.files = []
        src = path / "src"
        if src.is_dir():
            self.files = sorted(str(p.relative_to(path)) for p in src.rglob("*.rs")
                                if not any(x in {"tests", "benches", "examples"} for x in p.parts))
        self._model = None
        self._file_cache = {}
        self._impls = None
        self._flags = None
        self._deps = None

    def read(self, rel):
        try:
            return (self.path / rel).read_text(errors="replace")
        except OSError:
            return ""

    def get_file(self, rel):
        f = self._file_cache.get(rel)
        if f is None:
            src = self.read(rel)
            toks = lex(src)
            pairs = build_matches(toks)
            f = (src, toks, pairs)
            if len(self._file_cache) > 12:
                self._file_cache.clear()
            self._file_cache[rel] = f
        return f

    def model(self):
        if self._model is not None:
            return self._model
        defs = {}
        imports = {}
        fns = {}
        for rel in self.files:
            src, toks, pairs = self.get_file(rel)
            imports[rel] = file_imports(toks, pairs)
            tst = cfg_test_line_ranges(src)
            impls = parse_impls(toks, rel, pairs, tst)
            skip = [(im.body_open, im.body_close) for im in impls]
            for d in parse_defs(toks, rel, skip, tst):
                defs.setdefault(d.name, []).append(d)
            for f in find_functions(toks, rel, pairs, impls):
                if in_ranges(f.line, tst):
                    continue
                fns.setdefault(f.name, []).append((rel, f.line))
        self._model = {"defs": defs, "imports": imports, "fns": fns}
        return self._model

    def manager_impls(self):
        if self._impls is not None:
            return self._impls
        out = []
        for rel in self.files:
            src = self.read(rel)
            if "ManageConnection" not in src and "Manager" not in src:
                continue
            _, toks, pairs = self.get_file(rel)
            tst = cfg_test_line_ranges(src)
            impls = parse_impls(toks, rel, pairs, tst)
            funcs = find_functions(toks, rel, pairs, impls)
            imports = file_imports(toks, pairs)
            for im in impls:
                if in_ranges(im.line, tst):
                    continue
                kind = manager_kind(im, imports)
                if kind is None:
                    continue
                im.kind = kind
                out.append((im, funcs, imports, rel, src))
        self._impls = out
        return out

    def deps(self):
        """Dependency names from Cargo.toml (best effort)."""
        if self._deps is not None:
            return self._deps
        names = set()
        text = self.read("Cargo.toml")
        section = ""
        for line in text.splitlines():
            s = line.strip()
            if s.startswith("[") and s.endswith("]"):
                section = s.strip("[]")
                # [dependencies.NAME] / [target.'cfg'.dependencies.NAME]
                parts = [x.strip().strip("'\"") for x in section.split(".")]
                if len(parts) >= 2 and parts[-2] == "dependencies":
                    names.add(parts[-1])
                continue
            if "dependencies" not in section:
                continue
            m = re.match(r"([A-Za-z0-9_\-]+)\s*=", s)
            if m:
                names.add(m.group(1))
        names.add(self.name)
        self._deps = names
        return names

    def defs_in_file(self, name, rel):
        model = self.model()
        same = [d for d in model["defs"].get(name, []) if d.path == rel]
        if same:
            return same
        return model["defs"].get(name, [])

    def find_fn(self, name):
        """Return function token bodies for a name in this crate (cached)."""
        locs = self.model()["fns"].get(name, [])
        out = []
        by_file = {}
        for (rel, line) in locs:
            by_file.setdefault(rel, set()).add(line)
        for rel, lines in by_file.items():
            _, toks, pairs = self.get_file(rel)
            impls = parse_impls(toks, rel, pairs)
            for f in find_functions(toks, rel, pairs, impls):
                if f.name == name and f.line in lines:
                    out.append(f)
        return out


def manager_kind(im, imports):
    segs = im.trait_segs
    last = segs[-1] if segs else ""
    if last == "ManageConnection":
        if "r2d2" in segs:
            return None
        if "bb8" in segs or im.trait_segs[:1] == ["bb8"]:
            return "bb8"
        alias = imports.get("ManageConnection", [])
        if alias and alias[0] == "bb8":
            return "bb8"
        if not segs[:-1] and not alias:
            return None
        return None
    if last == "Manager":
        if "deadpool" in segs or "managed" in segs:
            return "deadpool"
        if "mobc" in segs:
            return "mobc"
        alias = imports.get("Manager", [])
        if alias:
            if alias[0] == "deadpool":
                return "deadpool"
            if alias[0] == "mobc":
                return "mobc"
        return None
    return None


def load_crates(root):
    out = []
    if not root.is_dir():
        return out
    for d in sorted(root.iterdir()):
        if not d.is_dir():
            continue
        m = DIR_RE.match(d.name)
        if not m:
            continue
        crate, version = m.group(1), m.group(2)
        variant = ""
        vm = VARIANT_RE.match(version)
        if vm and vm.group(2):
            version, variant = vm.group(1), vm.group(2)
        src = d / "src"
        if not src.is_dir():
            continue
        out.append(CrateSource(crate, version, variant, d))
    return out


class Catalog:
    def __init__(self):
        self.crates = []          # all
        self.primary = {}         # name -> release CrateSource

    def add(self, crates):
        for c in crates:
            self.crates.append(c)
        for c in crates:
            old = self.primary.get(c.name)
            if old is None:
                self.primary[c.name] = c
            elif old.variant and not c.variant:
                self.primary[c.name] = c
            elif old.variant == c.variant and _vkey(c.version) > _vkey(old.version):
                self.primary[c.name] = c

    def all_named(self, name):
        return [c for c in self.crates if c.name == name]

    def get(self, name):
        if not name:
            return None
        c = self.primary.get(name)
        if c is not None:
            return c
        return self.primary.get(name.replace("_", "-"))


def _vkey(v):
    return tuple(int(x) if x.isdigit() else x for x in re.split(r"[.\-]", v))


# ---------------------------------------------------------------------------
# Type resolution
# ---------------------------------------------------------------------------

TRANSPARENT = {"Arc", "Box", "Rc", "Option", "Mutex", "RwLock", "RefCell", "Cell",
               "Pin", "Cow", "Vec", "SyncWrapper"}
NON_CLIENT_CRATES = {
    "tokio", "tokio_util", "futures", "futures_util", "futures_core", "futures_io",
    "futures_channel", "futures_sink", "futures_task", "std", "core", "alloc",
    "bytes", "serde", "serde_json", "tracing", "log", "thiserror", "anyhow",
    "async_trait", "async_std", "smol", "mio", "socket2", "parking_lot", "dashmap",
    "smallvec", "pin_project", "pin_project_lite", "url", "http", "http_body",
    "hyper", "hyper_util", "tonic", "prost", "rustls", "native_tls", "tokio_rustls",
    "tokio_native_tls", "time", "chrono", "rand", "uuid", "indexmap", "hashbrown",
    "arc_swap", "once_cell", "crossbeam", "crossbeam_channel", "flume", "async_channel",
    "tokio_stream", "tokio_util", "bytes_util", "thiserror_core",
}
KNOWN_CRATE_HINTS = {
    "redis", "tokio_postgres", "postgres", "lapin", "mysql_async", "mysql", "diesel",
    "diesel_async", "tiberius", "memcache_async", "async_memcached", "ldap3", "mongodb",
    "oracle", "rusqlite", "sqlx", "sqlx_core", "sqlx_mysql", "sqlx_postgres",
    "sqlx_sqlite", "deadpool_postgres", "deadpool_redis", "deadpool_sqlite",
    "deadpool_diesel", "bb8_redis", "bb8_postgres", "hbclient", "hdbconnect",
    "tokio", "futures", "tonic", "hyper", "reqwest", "clickhouse", "scylla",
    "cdrs_tokio", "neo4j", "bolt_client", "mobc_redis", "rustis", "fred",
}


class Resolved:
    def __init__(self, cache, client_crate, client_type, evidence, path_note="",
                 source_missing=False):
        self.cache = cache
        self.client_crate = client_crate
        self.client_type = client_type
        self.evidence = evidence
        self.path_note = path_note
        self.source_missing = source_missing

    def key(self):
        return (self.client_crate, self.client_type)


def _strip_type(toks_text):
    """Drop leading &, lifetimes, mut, dyn, impl, and trailing bounds."""
    i = 0
    while i < len(toks_text):
        t = toks_text[i]
        if t in {"&", "mut", "dyn", "impl"} or t.startswith("'"):
            i += 1
            continue
        break
    j = len(toks_text)
    depth = 0
    for k in range(i, len(toks_text)):
        t = toks_text[k]
        if t in {"(", "[", "{", "<"}:
            depth += 1
        elif t in {")", "]", "}", ">"}:
            depth -= 1
        elif t == "+" and depth == 0:
            j = k
            break
    return toks_text[i:j]


def parse_type_path(text):
    """Return (segments, generics) from a token-text list."""
    text = _strip_type(text)
    segs = []
    generics = []
    i = 0
    while i < len(text):
        t = text[i]
        if t == "::":
            i += 1
            continue
        if t in {"<", ">"}:
            break
        if REG_IDENT.match(t):
            # skip keywords that are not paths
            segs.append(t)
            i += 1
            if i < len(text) and text[i] == "<":
                depth = 0
                start = i
                while i < len(text):
                    if text[i] == "<":
                        depth += 1
                    elif text[i] == ">":
                        depth -= 1
                        if depth == 0:
                            i += 1
                            break
                    i += 1
                generics.append(text[start + 1:i - 1])
            continue
        break
    return segs, generics


REG_IDENT = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")


def split_type_args(text):
    out, cur, depth = [], [], 0
    for t in text:
        if t in {"(", "[", "{", "<"}:
            depth += 1
        elif t in {")", "]", "}", ">"}:
            depth -= 1
        if t == "," and depth == 0:
            out.append(cur)
            cur = []
        else:
            cur.append(t)
    if cur:
        out.append(cur)
    return out


class Resolver:
    def __init__(self, catalog):
        self.catalog = catalog

    def resolve_impl(self, crate, im):
        """Return [(Resolved, note)] for an impl's associated connection type."""
        kind = im.kind or ("bb8" if (im.trait_segs and im.trait_segs[-1] == "ManageConnection")
                           else ("mobc" if "mobc" in im.trait_segs else "deadpool"))
        rhs = self._associated_type(im, "Type" if kind == "deadpool" else "Connection")
        if rhs is None:
            return [], "no associated connection type"
        return self.resolve_tokens(crate, im.path, rhs, im.generics,
                                   depth=0, visited=set(), bounds=im.bounds)

    def _kind(self, im):
        if im.kind:
            return im.kind
        last = im.trait_segs[-1] if im.trait_segs else ""
        if last == "ManageConnection":
            return "bb8"
        if "mobc" in im.trait_segs:
            return "mobc"
        return "deadpool"

    def _associated_type(self, im, name):
        toks = im.toks
        depth = 0
        for i in range(im.body_open + 1, im.body_close):
            t = toks[i].text
            if t in {"(", "[", "{"}:
                depth += 1
            elif t in {")", "]", "}"}:
                depth -= 1
            elif depth == 0 and t == "type" and i + 1 < im.body_close \
                    and toks[i + 1].text == name:
                j = i + 2
                while j < im.body_close and toks[j].text not in {"=", ";"}:
                    j += 1
                if j >= im.body_close or toks[j].text != "=":
                    return None
                e = j + 1
                d = 0
                while e < im.body_close:
                    x = toks[e].text
                    if x in {"(", "[", "{", "<"}:
                        d += 1
                    elif x in {")", "]", "}", ">"}:
                        d -= 1
                    elif x == ";" and d == 0:
                        break
                    e += 1
                return [x.text for x in toks[j + 1:e]]
        return None

    def resolve_tokens(self, origin, rel, text, generic_params, depth, visited, bounds=None):
        bounds = bounds or {}
        if depth > 6:
            return [], "resolution depth limit"
        segs, generics = parse_type_path(text)
        if not segs:
            return [], "not a type path: " + " ".join(text[:8])
        base = segs[0]
        # fully transparent wrappers
        if base in TRANSPARENT and generics:
            args = split_type_args(generics[0])
            if args:
                return self.resolve_tokens(origin, rel, args[0], generic_params,
                                           depth + 1, visited, bounds)
        # qualified / multi-segment path
        if len(segs) >= 2:
            first = segs[0]
            if first in {"crate", "self", "super"}:
                return self.resolve_local(origin, rel, segs[-1], depth, visited, bounds)
            if first in generic_params:
                return self.expand_generic(origin, rel, first, depth, visited, bounds)
            imports = self._imports(origin, rel)
            if first in imports:
                target = imports[first]
                if target and target[0] in {"crate", "self", "super"}:
                    return self.resolve_local(origin, rel, target[-1], depth, visited, bounds)
                if target and (self.catalog.get(target[0]) or target[0] in origin.deps()
                               or _rust_crate_name(target[0]) in KNOWN_CRATE_HINTS):
                    cname = _rust_crate_name(target[0])
                    return self.resolve_external(cname, segs[-1], rel, depth, visited)
                # local module path: `client::Connection`
                return self.resolve_local(origin, rel, target[-1] if target else segs[-1],
                                           depth, visited, bounds)
            if self.catalog.get(first) or first in origin.deps() or first in KNOWN_CRATE_HINTS:
                return self.resolve_external(_rust_crate_name(first), segs[-1],
                                             rel, depth, visited)
            # a local module path, e.g. `rt::Client`
            local = self.resolve_local(origin, rel, segs[-1], depth, visited, bounds)
            if local[0]:
                return local
            return [], "unknown path prefix `" + first + "` in `" + "::".join(segs) + "`"
        # bare name
        name = base
        if name in generic_params:
            return self.expand_generic(origin, rel, name, depth, visited, bounds)
        imports = self._imports(origin, rel)
        if name in imports:
            target = imports[name]
            if target and target[0] in {"crate", "self", "super"}:
                return self.resolve_local(origin, rel, target[-1], depth, visited, bounds)
            if target and (self.catalog.get(target[0]) or target[0] in origin.deps()
                           or _rust_crate_name(target[0]) in KNOWN_CRATE_HINTS):
                return self.resolve_external(_rust_crate_name(target[0]), target[-1],
                                             rel, depth, visited)
            if target:
                # local module import such as `use client::Connection;`
                return self.resolve_local(origin, rel, target[-1], depth, visited, bounds)
        # local definition
        local = self.resolve_local(origin, rel, name, depth, visited, bounds)
        if local[0]:
            return local
        # unique definition elsewhere among catalog (only if unambiguous).
        # Restrict to the manager crate's declared dependencies / known client
        # crates first; scanning every downloaded crate for a bare name is both
        # slow and likely to hit an unrelated type.
        hits = []
        origin_deps = origin.deps()
        candidates = [c for c in self.catalog.crates if c.name in origin_deps]
        for c in candidates:
            for d in c.model()["defs"].get(name, []):
                hits.append((c, d))
        uniq = {}
        for (c, d) in hits:
            uniq[(c.name, name)] = (c, d)
        if len(uniq) == 1:
            c, d = next(iter(uniq.values()))
            return [Resolved(c, c.name, name, f"{d.path}:{d.line}",
                             "global-unique-definition")], ""
        if len(uniq) > 1:
            return [], "ambiguous type `" + name + "` in " + ", ".join(sorted(x[0] for x in uniq))
        return [], "unresolved type `" + name + "`"

    def _imports(self, crate, rel):
        return crate.model()["imports"].get(rel, {})

    def resolve_external(self, crate_name, type_name, rel, depth, visited):
        c = self.catalog.get(crate_name)
        if c is None:
            return [Resolved(None, crate_name, type_name, rel,
                             "client source not downloaded", source_missing=True)], ""
        crate_name = c.name
        # follow a type alias in the client crate (one level, cheap)
        if depth < 3:
            for d in c.model()["defs"].get(type_name, []):
                if d.kind == "type" and d.rhs:
                    inner, note = self.resolve_tokens(
                        c, d.path, d.rhs, d.generics, depth + 1,
                        visited | {("alias", c.name, type_name)}, {})
                    if inner:
                        for r in inner:
                            r.evidence = f"{d.path}:{d.line}; " + r.evidence
                        return inner, note
        return [Resolved(c, c.name, type_name,
                         f"{c.name}-{c.version}", "external-path")], ""

    def resolve_local(self, origin, rel, name, depth, visited, bounds=None):
        key = (origin.name, origin.version, origin.variant, name)
        if key in visited:
            return [], "recursive local type `" + name + "`"
        visited = visited | {key}
        defs = origin.defs_in_file(name, rel)
        if not defs:
            # `pub use other_crate::Connection;` re-export: the manager imports
            # `crate::Connection`, which is not a local definition.
            for rel2, imp in origin.model()["imports"].items():
                target = imp.get(name)
                if not target or target[0] in {"crate", "self", "super"}:
                    continue
                if self.catalog.get(target[0]) or target[0] in origin.deps() \
                        or _rust_crate_name(target[0]) in KNOWN_CRATE_HINTS:
                    return self.resolve_external(_rust_crate_name(target[0]),
                                                 target[-1], rel2, depth, visited)
            return [], ""
        out = []
        note = ""
        for d in defs:
            if d.kind == "type":
                if d.rhs:
                    rs, n = self.resolve_tokens(origin, d.path, d.rhs, d.generics,
                                                depth + 1, visited, {})
                    for r in rs:
                        r.evidence = f"{d.path}:{d.line}; " + r.evidence
                    out.extend(rs)
                    note = note or n
                continue
            # struct / enum: look for the field that carries the protocol
            # connection.  Do not return every nested field type: wrapper
            # structs routinely hold caches, statement handles and payload
            # structs that are not the client connection.
            external = []
            for (fname, ftype, fline) in d.fields:
                rs, n = self.resolve_tokens(origin, d.path, ftype, d.generics,
                                            depth + 1, visited, {})
                for r in rs:
                    if r.cache is not None and r.cache.name == origin.name:
                        continue
                    if r.cache is not None and r.cache.name in NON_CLIENT_CRATES:
                        continue
                    if r.cache is None and r.client_crate in NON_CLIENT_CRATES:
                        continue
                    external.append((r, d.path, fline, (fname or "").lower()))
                if not rs and n:
                    note = note or n
            # prefer connection-ish field names
            preferred = [x for x in external if any(
                k in x[3] for k in ("conn", "client", "connection", "stream", "socket",
                                    "transport", "protocol", "session", "channel",
                                    "handle", "inner", "io", "net", "sender", "receiver"))]
            chosen = preferred or external
            if chosen:
                for (r, p, ln, fn) in chosen[:1]:
                    r.evidence = f"{d.path}:{d.line} field `{fn or '?'}` at {p}:{ln}; " + r.evidence
                    out.append(r)
            else:
                out.append(Resolved(origin, origin.name, name, f"{d.path}:{d.line}",
                                    "same-crate client type"))
        # dedupe
        seen = set()
        ded = []
        for r in out:
            k = r.key()
            if k in seen:
                continue
            seen.add(k)
            ded.append(r)
        return ded, note

    def expand_generic(self, origin, rel, param, depth, visited, bounds=None):
        """Expand a generic connection parameter through wrapper-trait impls.

        Only `PoolableConnection` is expanded: that is the diesel-async wrapper
        trait whose concrete impls name the real client type.  Other generic
        parameters (TLS types, sync diesel connections) are left unresolved
        instead of being guessed.
        """
        bounds = bounds or {}
        if "PoolableConnection" not in bounds.get(param, set()):
            return [], "generic connection type `" + param + "` is not bound by a known wrapper trait"
        candidates = []
        for rel2 in origin.files:
            src = origin.read(rel2)
            if param not in src or "for" not in src:
                continue
            if not any(x in src for x in ("PoolableConnection", "Connection", "Manager")):
                continue
            _, toks, pairs = origin.get_file(rel2)
            impls = parse_impls(toks, rel2, pairs)
            for im in impls:
                # e.g. impl PoolableConnection for AsyncMysqlConnection
                if im.self_name == param:
                    continue
                if any(b in {"PoolableConnection", "Connection"} for b in im.trait_segs):
                    candidates.append(im)
        out = []
        skipped = []
        for im in candidates:
            if im.generics:
                # generic wrapper impl (for example AsyncConnectionWrapper<C>);
                # without the instantiation it is not a concrete client pair.
                skipped.append(im.self_name)
                continue
            rs, _ = self.resolve_local(origin, im.path, im.self_name, depth + 1, visited, {})
            out.extend(rs)
        if out:
            return out, ("skipped generic wrapper impls: " + ", ".join(sorted(set(skipped)))
                         if skipped else "")
        return [], ("generic connection type `" + param +
                    "` has no concrete wrapper impl in this crate" +
                    (" (generic wrappers: " + ", ".join(sorted(set(skipped))) + ")"
                     if skipped else ""))

    def resolve_pool_connection(self, origin, im):
        return self.resolve_impl(origin, im)


def _rust_crate_name(x):
    return x.replace("-", "_")


# ---------------------------------------------------------------------------
# Client analysis: in-progress flags and cancellation-invalidating guards
# ---------------------------------------------------------------------------

FLAG_FIELD = re.compile(
    r"^(?:.*_)?(?:in_flight|inflight|in_progress|busy|sending|disconnected|dirty|"
    r"pending|pending_result|in_use|writing|reading|awaiting|broken|tainted|poisoned|"
    r"closed|is_broken|is_closed|usable|reusable)$",
    re.I,
)
BAD_VALUE = {"true", "Invalid", "Broken", "Closed", "Poisoned", "Tainted", "Failed",
             "Dirty", "Dead", "Disconnected", "Error"}
GOOD_VALUE = {"false", "Present", "Valid", "Healthy", "Ok", "Clean", "Alive", "Good",
              "Connected", "None"}


def _enclosing_func(funcs, idx):
    best = None
    for f in funcs:
        if f.a <= idx <= f.b:
            if best is None or (f.b - f.a) < (best.b - best.a):
                best = f
    return best


def analyze_client(crate):
    """Return flag metadata for a client crate."""
    if crate._flags is not None:
        return crate._flags
    fields = {}
    accessors = {}
    guards = []
    for rel in crate.files:
        src, toks, pairs = crate.get_file(rel)
        if not src:
            continue
        tst = cfg_test_line_ranges(src)
        impls = parse_impls(toks, rel, pairs, tst)
        skip = [(im.body_open, im.body_close) for im in impls]
        defs = parse_defs(toks, rel, skip, tst)
        for d in defs:
            for (fname, ftype, fline) in d.fields:
                if fname and FLAG_FIELD.match(fname) and not in_ranges(fline, tst):
                    fields.setdefault(fname, []).append((rel, fline))
        funcs = find_functions(toks, rel, pairs, impls)
        # accessors: fn is_x / has_x returning a field
        for f in funcs:
            if in_ranges(f.line, tst):
                continue
            m = re.match(r"^(?:is|has)_([A-Za-z0-9_]+)$", f.name)
            if m:
                for t in toks[f.a:f.b]:
                    if t.kind == "ident" and FLAG_FIELD.match(t.text):
                        accessors.setdefault(f.name, set()).add(t.text)
        # flags assigned in methods
        for i in range(0, len(toks)):
            t = toks[i]
            if t.kind != "ident" or not FLAG_FIELD.match(t.text):
                continue
            if i + 1 >= len(toks):
                continue
            if toks[i + 1].text != "=":
                continue
            if i > 0 and toks[i - 1].text not in {".", "::"}:
                continue
            if in_ranges(t.line, tst):
                continue
            val = toks[i + 2].text if i + 2 < len(toks) else ""
            rec = fields.setdefault(t.text, [])
            if not any(p == rel for p, _ in rec):
                rec.append((rel, t.line))
            f = _enclosing_func(funcs, i)
            if f is None:
                continue
            # pessimistic: bad value before an await, good value after it
            pessimistic = False
            if val in BAD_VALUE:
                aw = next((k for k in range(i + 1, min(f.b, i + 120))
                           if toks[k].text == "await"), None)
                if aw is not None:
                    good_after = False
                    for k in range(aw + 1, f.b):
                        if toks[k].text == t.text and k + 1 < f.b and toks[k + 1].text == "=":
                            v = toks[k + 2].text if k + 2 < f.b else ""
                            if v in GOOD_VALUE:
                                good_after = True
                                break
                    pessimistic = good_after
            if pessimistic:
                fields[t.text].append(("__pessimistic__", t.line))
        # cancellation-discarding guard: a Drop impl that invalidates the
        # connection, provided the guard is actually constructed in a function
        # that reaches an await.  Normal destructors that merely close a socket
        # are not counted unless the body marks the connection dead/broken.
        for m in re.finditer(r"impl\s*(?:<[^>]*>\s*)?Drop\s+for\s+([A-Za-z_][A-Za-z0-9_]*)",
                             src):
            name = m.group(1)
            start = m.end()
            brace = src.find("{", start)
            if brace < 0:
                continue
            depth, q = 0, brace
            while q < len(src):
                if src[q] == "{":
                    depth += 1
                elif src[q] == "}":
                    depth -= 1
                    if depth == 0:
                        break
                q += 1
            body = src[brace:q]
            marks_dead = bool(re.search(
                r"\b(mark_dead|mark_connection_dead|mark_broken|set_broken|invalidate|"
                r"taint|poison)\b", body))
            guardish = bool(re.search(r"(Guard|Defer|Scope|Rollback|Reset|Abort|Cleanup|"
                                      r"Take|Operation|Stream|Result|Transaction|Txn)",
                                      name, re.I))
            closes = bool(re.search(r"\b(close|abort|shutdown|discard|detach|disconnect)\b",
                                    body))
            if not (marks_dead or (guardish and closes)):
                continue
            # is this guard constructed in a function that awaits?
            constructed = False
            for rel2 in crate.files:
                src2, toks2, pairs2 = crate.get_file(rel2)
                if name not in src2:
                    continue
                funcs2 = find_functions(toks2, rel2, pairs2,
                                        parse_impls(toks2, rel2, pairs2,
                                                    cfg_test_line_ranges(src2)))
                for k in range(len(toks2)):
                    if toks2[k].text != name:
                        continue
                    nxt = toks2[k + 1].text if k + 1 < len(toks2) else ""
                    nxt2 = toks2[k + 2].text if k + 2 < len(toks2) else ""
                    if not (nxt == "{" or (nxt == "::" and nxt2 == "new")):
                        continue
                    f = _enclosing_func(funcs2, k)
                    if f is not None and f.has_await():
                        constructed = True
                        break
                if constructed:
                    break
            if constructed:
                guards.append((name, rel, src[:m.start()].count("\n") + 1))
    # a flag is cancellation-sensitive if any assignment is pessimistic
    sensitive = {}
    for name, locs in fields.items():
        sens = [ln for (p, ln) in locs if p == "__pessimistic__"]
        if sens:
            sensitive[name] = sens
    clean_fields = {}
    for k, v in fields.items():
        seen = set()
        vals = []
        for (p, ln) in v:
            if p == "__pessimistic__":
                continue
            if (p, ln) in seen:
                continue
            seen.add((p, ln))
            vals.append((p, ln))
        clean_fields[k] = vals
    out = {
        "fields": clean_fields,
        "sensitive": sensitive,
        "accessors": {k: sorted(v) for k, v in accessors.items()},
        "guards": guards,
    }
    crate._flags = out
    return out


# ---------------------------------------------------------------------------
# Manager validation analysis
# ---------------------------------------------------------------------------

VALIDATION_NAMES = {"bb8": ["is_valid"], "deadpool": ["recycle"], "mobc": ["check"]}
HAS_BROKEN_NAMES = {"bb8": ["has_broken"], "deadpool": ["is_broken"], "mobc": ["validate"]}
STRICT_LIKE = re.compile(r"^(PONG|PONG\r\n|OK|VERSION|VERSION .*|Pong)$", re.I)


def _fn_in_impl(im, funcs, names):
    out = []
    for f in funcs:
        if f.name in names and im.body_open < f.start < im.body_close:
            out.append(f)
    return out


def _lines_detail(src, line, max_lines=4):
    lines = src.splitlines()
    lo = max(0, line - 1)
    out = []
    for x in lines[lo:lo + max_lines]:
        s = x.strip()
        if s:
            out.append(s)
    return " ".join(out)[:400]


def _strict_in_func(f):
    toks = f.toks
    for i in range(f.a, f.b):
        t = toks[i]
        if t.kind == "str" and i > f.a and i + 1 < f.b:
            if toks[i - 1].text in {"==", "!="} or toks[i + 1].text in {"==", "!="}:
                return True
        if t.text in {"starts_with", "ends_with"} and i + 2 < f.b and toks[i + 2].kind == "str":
            return True
        if t.text in {"==", "!="}:
            # equality against a literal string on either side
            if i > f.a and toks[i - 1].kind == "str":
                return True
            if i + 1 < f.b and toks[i + 1].kind == "str":
                return True
    return False


def _called_names(f):
    names = set()
    toks = f.toks
    for i in range(f.a, f.b):
        if toks[i].kind == "ident" and i + 1 < f.b and toks[i + 1].text == "(":
            names.add(toks[i].text)
    return names


def _strict_in_client_method(client, names):
    for name in names:
        for f in client.find_fn(name):
            if _strict_in_func(f):
                return True, f
    return False, None


def _reachable_flag_check(crate, seeds):
    """BFS over function names in the manager crate; look for client flag names."""
    seen_funcs = set()
    queue = []
    for f in seeds:
        queue.append(f)
    found = []
    depth = 0
    while queue and depth < 7:
        nxt = []
        for f in queue:
            key = (f.path, f.line, f.name)
            if key in seen_funcs:
                continue
            seen_funcs.add(key)
            found.append(f)
            for name in _called_names(f):
                for g in crate.find_fn(name):
                    k = (g.path, g.line, g.name)
                    if k not in seen_funcs:
                        nxt.append(g)
        queue = nxt
        depth += 1
    return found


def analyze_validation(manager, im, funcs, client_info, client):
    kind = im.kind or ("bb8" if im.trait_segs[-1] == "ManageConnection" else (
        "mobc" if "mobc" in im.trait_segs else "deadpool"))
    vnames = VALIDATION_NAMES[kind]
    hnames = HAS_BROKEN_NAMES[kind]
    vfuncs = _fn_in_impl(im, funcs, vnames)
    hfuncs = _fn_in_impl(im, funcs, hnames)
    if not vfuncs:
        return {
            "validation_io": "unknown", "validation_detail": "validation method not found",
            "validation_strict": "unknown", "manager_checks_flag": "unknown",
            "check_evidence": "", "manager_checks": "unknown",
        }
    vf = vfuncs[0]
    io = "yes" if vf.has_await() else "no"
    vbody = [t.text for t in vf.toks[vf.a:vf.b]]
    blocking = "yes" if any(x in vbody for x in
                            ("spawn_blocking", "run_blocking", "run_blocking_in_place", "interact")) else "no"
    # candidate client methods called from the validation body
    called = _called_names(vf)
    # strictness in the manager body or in a called client method
    strict = _strict_in_func(vf)
    strict_ev = ""
    if not strict and client is not None:
        strict, sf = _strict_in_client_method(client, called - {"await", "map", "ok", "err",
                                                                "unwrap", "expect", "into",
                                                                "to_string", "is_ok", "is_err"})
        if sf is not None:
            strict_ev = f"{sf.path}:{sf.line}"
    # BFS from validation + has_broken for references to a client flag/accessor
    seeds = list(vfuncs) + list(hfuncs)
    reached = _reachable_flag_check(manager, seeds)
    flag_names = list(client_info["fields"].keys()) if client_info else []
    accessors = client_info["accessors"] if client_info else {}
    check_ev = ""
    checks = "no"
    if client_info is None:
        checks = "unknown"
    else:
        for f in reached:
            for i in range(f.a, f.b):
                t = f.toks[i]
                if t.kind != "ident":
                    continue
                if t.text in flag_names:
                    checks = "yes"
                    check_ev = f"{f.path}:{t.line}"
                    break
                for acc, backs in accessors.items():
                    if t.text == acc and any(x in flag_names for x in backs):
                        checks = "yes"
                        check_ev = f"{f.path}:{t.line}"
                        break
                if checks == "yes":
                    break
            if checks == "yes":
                break
    detail = f"{vnames[0]}: " + _lines_detail(manager.read(im.path), vf.line)
    return {
        "validation_io": io,
        "validation_detail": detail,
        "validation_strict": "yes" if strict else "no",
        "validation_strict_evidence": strict_ev,
        "blocking_validation": blocking,
        "manager_checks_flag": checks,
        "check_evidence": check_ev,
        "manager_checks": checks,
    }


# ---------------------------------------------------------------------------
# Pool shield analysis
# ---------------------------------------------------------------------------

def _fn_containing(crate, needle, extra=None):
    for rel in crate.files:
        src, toks, pairs = crate.get_file(rel)
        if needle not in src:
            continue
        impls = parse_impls(toks, rel, pairs)
        for f in find_functions(toks, rel, pairs, impls):
            body = [t.text for t in toks[f.a:f.b]]
            if needle in body and (extra is None or any(x in body for x in extra)):
                return f, toks
    return None, None


def analyze_pool_shield(pool):
    """Return (yes/no/unknown, evidence, mechanism)."""
    if pool is None:
        return "unknown", "", ""
    if pool.name == "bb8":
        f, toks = _fn_containing(pool, "is_valid", {"await"})
        if f is None:
            return "unknown", "", "bb8 pool get() not located"
        # find the is_valid token index and await after it
        iv = next((i for i in range(f.a, f.b) if toks[i].text == "is_valid"), None)
        if iv is None:
            return "unknown", f"{f.path}:{f.line}", "is_valid call not located"
        pre_bad = None
        for i in range(f.a, iv):
            if toks[i].text == "state" and i + 2 < iv and toks[i + 1].text == "=":
                if any(toks[k].text == "Invalid" for k in range(i + 2, min(iv, i + 8))):
                    pre_bad = i
                    break
        post_good = False
        for i in range(iv, f.b):
            if toks[i].text == "state" and i + 2 < f.b and toks[i + 1].text == "=":
                if any(toks[k].text == "Present" for k in range(i + 2, min(f.b, i + 8))):
                    post_good = True
                    break
        if pre_bad is not None and post_good:
            return "yes", f"{f.path}:{toks[pre_bad].line}", \
                "conn.state=Invalid before is_valid await, Present after"
        return "no", f"{f.path}:{f.line}", "no pre-await invalid marker"
    if pool.name == "deadpool":
        f, toks = _fn_containing(pool, "recycle", {"await"})
        unready = False
        unready_ev = ""
        for rel in pool.files:
            src = pool.read(rel)
            if "UnreadyObject" in src and "detach" in src:
                unready = True
                unready_ev = f"{rel}:{src[:src.find('UnreadyObject')].count(chr(10)) + 1}"
                break
        if f is not None and unready:
            return "yes", (f"{f.path}:{f.line}; {unready_ev}"), \
                "object moved into UnreadyObject before recycle; Drop detaches"
        return "no", (f"{f.path}:{f.line}" if f else ""), "no unready guard"
    if pool.name == "mobc":
        for f in pool.find_fn("validate_conn"):
            body = [t.text for t in f.toks[f.a:f.b]]
            if "check" in body and "await" in body:
                return "yes", f"{f.path}:{f.line}", \
                    "check(raw).await owns the raw connection; cancelling drops it"
        return "no", "", "validate_conn check path not located"
    return "unknown", "", "pool core not recognized"


# ---------------------------------------------------------------------------
# Pair construction
# ---------------------------------------------------------------------------

MULTIPLEXED = {
    ("redis", "MultiplexedConnection"):
        "redis multiplexes commands on one socket and routes replies internally",
    ("redis", "Connection"):
        "redis aio Connection is a multiplexed handle (marker is its own)",
    ("redis", "ClusterConnection"):
        "redis cluster connection multiplexes commands and routes replies by request",
    ("tokio_postgres", "Client"):
        "tokio-postgres runs a background connection task and routes replies to per-query channels",
    ("postgres", "Client"):
        "postgres re-exports the tokio-postgres multiplexed Client",
    ("lapin", "Connection"):
        "AMQP channels are multiplexed with delivery tags / channel ids",
    ("ldap3", "Ldap"):
        "ldap3 Ldap is a handle to a background task keyed by LDAP message id",
    ("mongodb", "Client"):
        "mongodb driver is multiplexed and topology-aware",
    ("deadpool_postgres", "Client"):
        "deadpool-postgres object wraps the tokio-postgres multiplexed Client",
    ("deadpool_redis", "Connection"):
        "deadpool-redis object wraps a redis multiplexed connection",
    # Background-task clients: the driver's task owns the socket and routes
    # replies to per-request channels.  Dropping the caller future drops only
    # that channel; the driver still consumes the reply, so the next request
    # does not read a misplaced response.
    ("clickhouse_arrow", "Client"):
        "clickhouse-arrow spawns an IO task and returns oneshot replies "
        "(src/client/connection.rs:222,374)",
    ("klickhouse", "Client"):
        "klickhouse dispatches requests to a background task and returns a "
        "oneshot receiver (src/client.rs:338,248)",
    ("rustis", "ExclusiveClient"):
        "rustis uses a network task with oneshot replies "
        "(src/client/client.rs:38, src/client/command_future.rs:55)",
    ("gremlin_client", "Conn"):
        "gremlin aio Conn sends Cmd::Msg with a Uuid request id to a worker "
        "(src/aio/connection.rs:55-72)",
    ("toasty", "ConnectionHandle"):
        "toasty serializes operations through a connection task and oneshot "
        "replies (src/db/connection_task.rs:6-9,87-102)",
}

NON_PROTOCOL = {
    ("wiremock", "BareMockServer"):
        "in-process mock-server pool; reset() is not a network protocol exchange",
}


def _downloads(universe, name):
    if not name:
        return "未核实"
    if name in universe:
        return str(universe[name])
    alt = name.replace("_", "-")
    if alt in universe:
        return str(universe[alt])
    return "未核实"


def _client_choice(catalog, r):
    if r.cache is not None:
        return r.cache
    return catalog.get(r.client_crate) if r.client_crate else None


def build_pairs(catalog, universe, shield, only, debug=False):
    rows = []
    counts = {"impls": 0, "pairs": 0, "resolved": 0, "unresolved": 0}
    seen = set()
    for manager in catalog.crates:
        if only and manager.name not in only:
            continue
        impls = manager.manager_impls()
        for (im, funcs, imports, rel, src) in impls:
            kind = manager_kind(im, imports)
            if kind is None:
                continue
            if manager.name == "deadpool" and im.self_name == "Manager":
                continue
            counts["impls"] += 1
            resolver = Resolver(catalog)
            rs, note = resolver.resolve_impl(manager, im)
            if not rs:
                counts["pairs"] += 1
                counts["unresolved"] += 1
                rows.append(_row(manager, im, kind, None, note, universe, shield,
                                 None, None, debug))
                continue
            client_info_cache = {}
            for r in rs:
                counts["pairs"] += 1
                client = _client_choice(catalog, r)
                key = (manager.name, manager.version, manager.variant, kind,
                       im.line, r.client_crate, r.client_type)
                if key in seen:
                    continue
                seen.add(key)
                if client is None:
                    counts["unresolved"] += 1
                    rows.append(_row(manager, im, kind, r, "client source not downloaded",
                                     universe, shield, None, None, debug))
                    continue
                counts["resolved"] += 1
                info = client_info_cache.get(client.name)
                if info is None:
                    info = analyze_client(client)
                    client_info_cache[client.name] = info
                val = analyze_validation(manager, im, funcs, info, client)
                rows.append(_row(manager, im, kind, r, "", universe, shield,
                                 client, info, debug, val=val))
    return rows, counts


def _row(manager, im, kind, r, note, universe, shield, client, info, debug, val=None):
    suspicious = "unknown"
    reason = "unresolved connection type: " + (note or "unknown")
    flag_present = "unknown"
    flag_names = ""
    flag_sensitive = "unknown"
    cancel_guard = "unknown"
    multiplexed = "unknown"
    pool_shield, pool_ev, pool_mech, pool_name, pool_ver, pool_var = \
        shield.get(kind, ("unknown", "", "", kind, "", ""))
    strict = "unknown"
    blocking = "unknown"
    validation_io = "unknown"
    detail = ""
    checks = "unknown"
    check_ev = ""
    client_type = r.client_type if r is not None else ""
    client_crate = r.client_crate if r is not None else ""
    evidence = r.evidence if r is not None else f"{manager.name}-{manager.version}"
    if r is not None and r.source_missing:
        reason = "client crate `" + r.client_crate + "` not downloaded; cannot inspect flags"
        client_type = r.client_type
    if r is not None and client is not None and info is not None:
        flag_present = "yes" if info["fields"] else "no"
        flag_names = ",".join(sorted(info["fields"]))
        flag_sensitive = "yes" if info["sensitive"] else "no"
        cancel_guard = "yes" if info["guards"] else "no"
        mux = MULTIPLEXED.get((client.name.replace("-", "_"), r.client_type))
        if mux is None:
            mux = MULTIPLEXED.get((client.name.replace("-", "_"),
                                   r.client_type.split("::")[-1]))
        multiplexed = "yes" if mux else "no"
        validation_io = val["validation_io"]
        blocking = val.get("blocking_validation", "unknown")
        detail = val["validation_detail"]
        checks = val["manager_checks_flag"]
        check_ev = val["check_evidence"]
        strict = val["validation_strict"]
        nonproto = NON_PROTOCOL.get((client.name.replace("-", "_"), r.client_type))
        if nonproto:
            suspicious = "no"
            reason = "not a protocol client: " + nonproto
        elif validation_io == "no":
            suspicious = "no"
            reason = "validation is a synchronous state check (no await in " + \
                     (VALIDATION_NAMES[kind][0]) + ")"
        elif multiplexed == "yes":
            suspicious = "no"
            reason = "multiplexed client: " + mux
        elif blocking == "yes":
            suspicious = "unknown"
            reason = "validation waits on a blocking worker (spawn_blocking/run_blocking/" \
                     "interact); cancelling the await does not stop the worker, which is a " \
                     "different hazard from a leftover reply and is not modelled here"
        elif cancel_guard == "yes":
            suspicious = "no"
            g = info["guards"][0]
            reason = f"client has a discarding Drop guard `{g[0]}` ({g[1]}:{g[2]})"
        elif flag_sensitive == "yes":
            if checks == "yes":
                suspicious = "no"
                reason = "client has cancellation-sensitive flag(s) " + flag_names + \
                         "; manager validation reads it at " + check_ev
            elif checks == "unknown":
                suspicious = "unknown"
                reason = "client flag(s) " + flag_names + "; manager check could not be determined"
            else:
                suspicious = "yes"
                reason = "client has cancellation-sensitive flag(s) " + flag_names + \
                         " but manager validation never reads them"
        else:
            if flag_present == "yes":
                extra = " (flag present but not set before an await / not cancel-sensitive)"
            else:
                extra = ""
            if pool_shield == "yes" and strict == "yes":
                suspicious = "no"
                reason = "pool core shields the validation window (" + pool_mech + \
                         ") and the validation reply is checked strictly; " \
                         "residual user-cancel window not modelled"
            elif pool_shield == "yes" and strict == "no":
                suspicious = "yes"
                reason = "pool core shields only the validation window (" + pool_mech + \
                         "); validation accepts any successful reply, so a leftover " \
                         "reply from a cancelled user exchange can pass" + extra
            elif pool_shield == "unknown":
                suspicious = "unknown"
                reason = "no cancellation-sensitive client flag; pool shield unknown" + extra
            else:
                suspicious = "yes"
                reason = "validation does real I/O and the client has no " \
                         "cancellation-sensitive in-progress flag or discarding guard; " \
                         "a leftover reply is indistinguishable from the expected one" + extra
    elif r is not None and client is not None and info is None:
        suspicious = "unknown"
        reason = "client source missing"
    evidence_parts = [f"{im.path}:{im.line}"]
    if r is not None:
        evidence_parts.append(r.evidence)
    if check_ev:
        evidence_parts.append(check_ev)
    ev = "; ".join(x for x in evidence_parts if x)
    return {
        "manager_crate": manager.name,
        "manager_version": manager.version,
        "manager_variant": manager.variant or "release",
        "manager_kind": kind,
        "manager_downloads_90d": _downloads(universe, manager.name),
        "managed_type": im.self_name,
        "underlying_type": client_type,
        "client_crate": client_crate,
        "client_version": client.version if client is not None else "未核实",
        "client_downloads_90d": _downloads(universe, client_crate) if client_crate else "未核实",
        "flag_present": flag_present,
        "flag_names": flag_names,
        "flag_cancel_sensitive": flag_sensitive,
        "cancel_guard": cancel_guard,
        "multiplexed": multiplexed,
        "manager_checks_flag": checks,
        "validation_io": validation_io,
        "validation_strict": strict,
        "blocking_validation": blocking,
        "validation_detail": detail,
        "suspicious": suspicious,
        "reason": reason,
        "evidence": ev,
        "manager_file": im.path,
        "manager_line": im.line,
        "resolution_note": note if r is None else (r.path_note if r is not None else ""),
        "pool_crate": pool_name,
        "pool_version": pool_ver,
        "pool_variant": pool_var,
        "pool_shield": pool_shield,
        "pool_evidence": pool_ev,
        "pool_mechanism": pool_mech,
        "check_evidence": check_ev,
    }


# ---------------------------------------------------------------------------
# Unified diff application (patched-pool calibration; text only)
# ---------------------------------------------------------------------------

def apply_unified_diff(root, diff_text):
    lines = diff_text.splitlines()
    i = 0
    applied = []
    while i < len(lines):
        if not lines[i].startswith("--- "):
            i += 1
            continue
        old_hdr = lines[i][4:].strip()
        i += 1
        if i >= len(lines) or not lines[i].startswith("+++ "):
            raise ValueError("malformed diff: missing +++")
        new_hdr = lines[i][4:].split("\t")[0].strip()
        i += 1
        path = new_hdr
        if path.startswith("b/"):
            path = path[2:]
        target = root / path
        if not target.is_file():
            raise ValueError(f"diff target missing: {target}")
        content = target.read_text()
        had_nl = content.endswith("\n")
        file_lines = content.splitlines(keepends=True)
        out = []
        cursor = 0
        while i < len(lines) and lines[i].startswith("@@"):
            hdr = lines[i]
            m = re.match(r"@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@", hdr)
            if not m:
                raise ValueError(f"bad hunk header: {hdr}")
            old_start = int(m.group(1))
            i += 1
            old_block, new_block = [], []
            while i < len(lines) and not lines[i].startswith("@@"):
                ln = lines[i]
                if ln.startswith("diff ") or ln.startswith("--- "):
                    break
                tag, body = ln[:1], ln[1:]
                if tag == " ":
                    old_block.append(body)
                    new_block.append(body)
                elif tag == "-":
                    old_block.append(body)
                elif tag == "+":
                    new_block.append(body)
                elif tag == "\\":
                    pass
                else:
                    break
                i += 1
            # locate old block starting at old_start-1 (allow context shift)
            start_idx = old_start - 1
            if start_idx > len(file_lines):
                raise ValueError("hunk past end of file")
            got = [x.rstrip("\n") for x in file_lines[start_idx:start_idx + len(old_block)]]
            want = [x.rstrip("\n") for x in old_block]
            if got != want:
                raise ValueError(f"hunk does not apply to {path}: expected {want[:2]!r}, got {got[:2]!r}")
            out.extend(file_lines[cursor:start_idx])
            out.extend(x + "\n" for x in new_block)
            cursor = start_idx + len(old_block)
        out.extend(file_lines[cursor:])
        text = "".join(out)
        if not had_nl:
            text = text.rstrip("\n")
        target.write_text(text)
        applied.append(path)
    return applied


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

FIELDS = [
    "manager_crate", "manager_version", "manager_variant", "manager_kind",
    "manager_downloads_90d", "managed_type", "underlying_type", "client_crate",
    "client_version", "client_downloads_90d", "flag_present", "flag_names",
    "flag_cancel_sensitive", "cancel_guard", "multiplexed",
    "manager_checks_flag", "validation_io", "validation_strict",
    "blocking_validation", "validation_detail", "suspicious", "reason", "evidence",
    "manager_file",
    "manager_line", "resolution_note", "pool_crate", "pool_version",
    "pool_variant", "pool_shield", "pool_evidence", "pool_mechanism",
    "check_evidence",
]


def load_universe(path):
    out = {}
    if not path.is_file():
        return out
    with path.open(newline="") as f:
        for r in csv.DictReader(f):
            try:
                out[r["crate"]] = int(r["downloads_90d"])
            except (KeyError, ValueError):
                pass
    return out


def prepare_pool(kind, diff_path, catalog, workroot):
    """Copy the pool crate and apply the patch under /tmp; return new CrateSource."""
    base = catalog.get(kind)
    if base is None:
        raise SystemExit(f"pool crate `{kind}` not found in scanned roots")
    import shutil
    dest = workroot / f"{kind}-{base.version}+patched"
    if dest.exists():
        shutil.rmtree(dest)
    shutil.copytree(base.path, dest)
    diff = pathlib.Path(diff_path).expanduser().read_text()
    applied = apply_unified_diff(dest, diff)
    return CrateSource(kind, base.version, "patched", dest), applied


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", type=pathlib.Path,
                    default=pathlib.Path.home() / "dropwise-scan-src/src")
    ap.add_argument("--calibration-dir", type=pathlib.Path, action="append", default=[])
    ap.add_argument("--out", type=pathlib.Path, default=pathlib.Path("pairs.csv"))
    ap.add_argument("--only", default="")
    ap.add_argument("--pool-patch", action="append", default=[],
                    help="KIND=DIFF; apply a unified diff to a /tmp copy of the pool crate")
    ap.add_argument("--pool-source", action="append", default=[],
                    help="KIND=DIR; use a prepared pool core source")
    ap.add_argument("--universe", type=pathlib.Path,
                    default=pathlib.Path(__file__).resolve().parents[1] / "universe.csv")
    ap.add_argument("--quiet", action="store_true")
    ap.add_argument("--debug", action="store_true")
    a = ap.parse_args()
    root = a.root.expanduser().resolve()
    only = {x.strip() for x in a.only.split(",") if x.strip()}
    universe = load_universe(a.universe.expanduser())

    catalog = Catalog()
    catalog.add(load_crates(root))
    for d in a.calibration_dir:
        catalog.add(load_crates(d.expanduser().resolve()))

    import tempfile
    workroot = pathlib.Path(tempfile.mkdtemp(prefix="pairing-pool-"))
    patched = {}
    for spec in a.pool_patch:
        kind, _, diff = spec.partition("=")
        if not diff:
            raise SystemExit("--pool-patch expects KIND=DIFF")
        src, applied = prepare_pool(kind, diff, catalog, workroot)
        catalog.add([src])
        patched[kind] = src
        if not a.quiet:
            print(f"patched pool {kind}: {', '.join(applied)} -> {src.path}", flush=True)
    for spec in a.pool_source:
        kind, _, d = spec.partition("=")
        if not d:
            raise SystemExit("--pool-source expects KIND=DIR")
        p = pathlib.Path(d).expanduser().resolve()
        c = CrateSource(kind, "prepared", "patched", p)
        catalog.add([c])
        patched[kind] = c

    shield = {}
    for kind in ("bb8", "deadpool", "mobc"):
        pool = patched.get(kind) or catalog.get(kind)
        if pool is None:
            shield[kind] = ("unknown", "", "", kind, "", "")
            continue
        s, ev, mech = analyze_pool_shield(pool)
        shield[kind] = (s, ev, mech, pool.name, pool.version, pool.variant or "release")
        if not a.quiet:
            print(f"pool {kind} {pool.version}{('+' + pool.variant) if pool.variant else ''}: "
                  f"shield={s} ({mech})", flush=True)

    rows, counts = build_pairs(catalog, universe, shield, only, a.debug)
    rows.sort(key=lambda r: (r["manager_crate"], r["manager_variant"], r["manager_kind"],
                             r["client_crate"] or "", r["underlying_type"] or ""))
    a.out.parent.mkdir(parents=True, exist_ok=True)
    with a.out.open("w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=FIELDS)
        w.writeheader()
        w.writerows(rows)
    by = {}
    for r in rows:
        by[r["suspicious"]] = by.get(r["suspicious"], 0) + 1
    summary = (f"crates={len(catalog.crates)} manager_impls={counts['impls']} "
               f"pairs={counts['pairs']} resolved={counts['resolved']} "
               f"unresolved={counts['unresolved']} suspicious=yes:{by.get('yes', 0)} "
               f"no:{by.get('no', 0)} unknown:{by.get('unknown', 0)} -> {a.out}")
    print(summary)
    return 0


if __name__ == "__main__":
    sys.exit(main())
