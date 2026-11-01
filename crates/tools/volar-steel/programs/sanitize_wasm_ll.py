#!/usr/bin/env python3
"""Sanitize a whole-program (fat-LTO) wasm32 `.ll` for the importer's parser.

The fat-LTO rustc emission uses the newest float spellings and attribute
syntax that the importer's LLVM build rejects, and carries float
parse/format machinery that revm's integer semantics never reach. This
script:

1. normalizes float constant spellings (`+qnan`/`+inf`, bare `qnan`/`inf`,
   decimal literals on float/double/half lines, `f0x` forms, and 8-digit
   float hex) to the double-width hex the parser accepts; half constants
   use the `0xH` + 4-digit form;
2. strips `target_mem:` memory-attribute tokens;
3. neuters float parse/format machinery (`float_parse`, `dec2flt`,
   `flt2dec`, `core::fmt::float`, grisu, dtoa, and the `core::f16/f32/f64`
   shim crate) — their bodies become `unreachable`. revm's EVM semantics
   are integer-only, so these are unreachable from the entry.

Usage: sanitize_wasm_ll.py <in.ll> <out.ll>
"""

import re
import struct
import sys

FLOAT_TYPES = ("half", "float", "double")

# float-typed literals that are exactly representable as f64 after widening.
F32_SPECIAL = {
    "qnan": 0x7FF8000000000000,
    "inf": 0x7FF0000000000000,
    "-inf": 0xFFF0000000000000,
}
F64_SPECIAL = {
    "qnan": 0x7FF8000000000000,
    "inf": 0x7FF0000000000000,
    "-inf": 0xFFF0000000000000,
}
HALF_SPECIAL = {"qnan": 0x7E00, "inf": 0x7C00, "-inf": 0xFC00}

DECIMAL = re.compile(
    r"(?<![\w.+-])(-?\d+\.\d+(?:e[+-]?\d+)?|-?\d+e[+-]?\d+)(?![\w.])", re.I
)
FLOAT_TYPE_LINE = re.compile(r"\bhalf\b|\bfloat\b|\bdouble\b")
SPECIAL_TOKEN = re.compile(r"\b(qnan|inf)\b|-\binf\b")


def widen_f32(hex8: str) -> str:
    """f32 bit hex -> the 16-digit f64 hex of its exact widening."""
    bits = int(hex8, 16)
    f = struct.unpack("<f", struct.pack("<I", bits))[0]
    d = struct.unpack("<Q", struct.pack("<d", f))[0]
    return f"0x{d:016X}"


def double_hex(value: float) -> str:
    d = struct.unpack("<Q", struct.pack("<d", value))[0]
    return f"0x{d:016X}"


def normalize_constant_line(line: str) -> str:
    """Rewrite one line's float constants to parser-accepted spellings."""
    typed = next((t for t in FLOAT_TYPES if f" {t} " in line or f"{t} %" in line), None)
    if typed is not None:
        # Special tokens (+qnan/+inf already stripped of '+' earlier here).
        def special(m: re.Match[str]) -> str:
            tok = m.group(0)
            if typed == "half":
                return f"0x{HALF_SPECIAL[tok]:04X}"
            table = F32_SPECIAL if typed == "float" else F64_SPECIAL
            return f"0x{table[tok]:016X}"

        line = SPECIAL_TOKEN.sub(special, line)
        # Decimal literals: widen through f64 (f32 literals widen exactly).
        def decimal(m: re.Match[str]) -> str:
            try:
                v = float(m.group(1))
            except ValueError:
                return m.group(1)
            return double_hex(v)

        line = DECIMAL.sub(decimal, line)
        # `f0x` spellings (newest emission form): 16-digit doubles pass
        # through; 8-digit f32 forms widen.
        def f0x(m: re.Match[str]) -> str:
            return widen_f32(m.group(1))

        line = re.sub(r"\bf0x([0-9A-Fa-f]{8})\b", f0x, line)
        line = re.sub(r"\bf0x([0-9A-Fa-f]{16})\b", r"0x\1", line)
        # Bare 8-digit float hex in operand position: widen.
        def bare8(m: re.Match[str]) -> str:
            return widen_f32(m.group(1))

        line = re.sub(r"(?<![\wH])(0x[0-9A-Fa-f]{8})(?![0-9A-Fa-f])", bare8, line)
        # Half constants in operand position: 0xH + 4 digits.
        def half4(m: re.Match[str]) -> str:
            return f"0xH{m.group(1).upper()}"

        line = re.sub(r"(?<![\wH])(0x[0-9A-Fa-f]{4})(?![0-9A-Fa-f])", half4, line)
    return line


def neuter_float_machinery(text: str) -> tuple[str, int]:
    pat = re.compile(
        r"float_parse|dec2flt|flt2dec|3fmt5float|6dtoa|3num3flt|grisu"
        r"|Cs8hZryZq3oeI_4core3f(16|32|64)f"
    )
    names = {
        m.group(1)
        for m in re.finditer(r"^define [^@\n]*@(_[A-Za-z0-9_$]+)\(", text, re.M)
        if pat.search(m.group(1))
    }
    body = text.split("\n")
    res: list[str] = []
    i = 0
    while i < len(body):
        line = body[i]
        hit = None
        if line.startswith("define "):
            for nm in names:
                if f"@{nm}(" in line:
                    hit = nm
                    break
        if hit:
            res.append(line)
            i += 1
            while i < len(body) and body[i] != "}":
                i += 1
            res.append("  unreachable")
            res.append("}")
            i += 1
            continue
        res.append(line)
        i += 1
    return "\n".join(res), len(names)


def main() -> int:
    if len(sys.argv) != 3:
        print(f"usage: {sys.argv[0]} <in.ll> <out.ll>", file=sys.stderr)
        return 2
    inp, outp = sys.argv[1], sys.argv[2]
    try:
        with open(inp, encoding="utf-8", errors="replace") as fh:
            lines = fh.readlines()
    except OSError as e:
        print(f"cannot read {inp}: {e}", file=sys.stderr)
        return 1

    # Leading '+' on qnan/inf tokens (newest spelling) first.
    text = "".join(lines)
    text = re.sub(r"\+\b(qnan|inf)\b", r"\1", text)
    text = "\n".join(normalize_constant_line(ln) for ln in text.split("\n"))
    # Half constants: 4-digit hex -> 0xH form (operand positions).
    text = re.sub(r"(?<![\wH])(0x[0-9A-Fa-f]{4})(?![0-9A-Fa-f])",
                  lambda m: f"0xH{m.group(1).upper()}", text)

    # 2. target_mem attribute tokens (parser predates them).
    text = re.sub(r"target_mem: \w+, ", "", text)
    text = re.sub(r", target_mem: \w+", "", text)

    # 3. neuter float parse/format machinery.
    text, neutered = neuter_float_machinery(text)

    try:
        with open(outp, "w", encoding="utf-8") as fh:
            fh.write(text)
    except OSError as e:
        print(f"cannot write {outp}: {e}", file=sys.stderr)
        return 1
    print(f"neutered {neutered} float-machinery functions -> {outp}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
