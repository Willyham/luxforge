#!/usr/bin/env python3
"""Regenerate synthetic Contrast cases independently of the Rust implementations.

Run from the repository root: python3 fixtures/basic/contrast-small-cases.py
Only Python's standard library is used. Inputs are exact binary64 values; every
encoded input is also exactly representable in binary32.
"""

import json
from decimal import Decimal, localcontext
from pathlib import Path


def logistic(x, alpha):
    exponent = -alpha * (x - Decimal("0.5"))
    # Avoid constructing an enormous Decimal exponent. The omitted tail is
    # below 10^-4342, far below the 430-digit calculation's precision, including
    # normalization by the smallest admitted nonzero alpha (about 10^-325).
    if exponent > 10000:
        return Decimal(0)
    if exponent < -10000:
        return Decimal(1)
    return Decimal(1) / (Decimal(1) + exponent.exp())


def cases():
    inputs = [-0.5, 0.0, 0.25, 0.5, 0.75, 1.0, 2.0, 1024.0, 1048576.0,
              float(2**60), -float(2**60)]
    magnitudes = [0.1, 0.18, 0.3, 1.6, 5.0 / 3.0, 1.7, 1e-4, 1e-6, 1e-20, 1e-30,
                  float.fromhex("0x1p-1022"),
                  float.fromhex("0x0.0000000000001p-1022")]
    result = []
    with localcontext() as context:
        context.prec = 430
        for magnitude in magnitudes:
            for contrast in [-magnitude, magnitude]:
                alpha = Decimal(6) * Decimal.from_float(abs(contrast)) / Decimal(100)
                g0, g1 = logistic(Decimal(0), alpha), logistic(Decimal(1), alpha)
                assert g1 > g0
                kappa = (g1 - g0) / (alpha / Decimal(4))
                for value in inputs:
                    x = Decimal.from_float(value)
                    s = (logistic(x, alpha) - g0) / (g1 - g0)
                    expected = s if contrast > 0 else x - kappa * (s - x)
                    result.append({
                        "contrast": contrast,
                        "input_encoded": value,
                        "expected_encoded": float(expected),
                    })
    return result


if __name__ == "__main__":
    fixture = {
        "description": (
            "Independent 430-digit Python Decimal evaluation of the original normalized "
            "logistic and negative reflection, using exact binary64 inputs; no tanhc or "
            "production/reference implementation used. Logistic exponents beyond +/-10000 "
            "saturate, an error below 10^-4342."
        ),
        "cases": cases(),
    }
    Path(__file__).with_suffix(".json").write_text(
        json.dumps(fixture, indent=2) + "\n", encoding="utf-8"
    )
