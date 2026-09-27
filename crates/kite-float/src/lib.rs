//! How a Kite float is written as text.
//!
//! Three backends print floats, and they printed them three ways at the edges:
//! the bytecode VM and the native runtime used Rust's `{}`, and the Wasm glue
//! used JavaScript's `Number#toString`. The same program printed `inf` or
//! `Infinity`, `-0.0` or `0.0`, `0.0000001` or `1e-7`, and a 327-character
//! decimal or `5e-324`, depending on where it ran.
//!
//! This crate is the rule for the two runtimes written in Rust — the VM and
//! `kite-rt` — so that they cannot drift from each other, and the glue's
//! `showFloat` is its transcription into JavaScript. Section 4.2 of the
//! specification states it:
//!
//! * `NaN` is `NaN`; the infinities are `inf` and `-inf`.
//! * Zero is `0.0`, and negative zero `-0.0`: it is a different value, and
//!   `1.0 / -0.0` says so.
//! * A whole number below `1e21` in magnitude is its exact digits and `.0`, so
//!   it reads back as a float rather than an `int`: `3.0`, `1e20` as
//!   `100000000000000000000.0`, `2^63` as `9223372036854775808.0`.
//! * Anything else is the shortest decimal that reads back as the same
//!   `f64` — the closest such, and the one with the even last digit when two
//!   are exactly as close, as ECMAScript chooses — laid out as ECMAScript
//!   lays out `Number#toString`: plainly from `1e-7` up to `1e21`, and in
//!   exponent form outside that — `1e-7`, `1.5e-7`, `1e+21`,
//!   `1.7976931348623157e+308`. Every one of those is also a Kite float
//!   literal.

/// `v` as Kite writes it — the text of `io.print(v)` and of `"\(v)"`.
pub fn float_text(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0.0" } else { "0.0" }.to_string();
    }
    // A whole number is written in full, which is `toFixed(1)` in JavaScript
    // and `{:.1}` here: both give the exact decimal value, not the shortest.
    if v.fract() == 0.0 && v.abs() < 1e21 {
        return format!("{:.1}", v);
    }
    let (sign, v) = if v < 0.0 { ("-", -v) } else { ("", v) };
    let (s, q) = shortest(v);
    let digits = s.to_string();
    let k = digits.len() as i32;
    // ECMAScript's `n`: the value is `0.d1d2…dk × 10^n`, and `s × 10^q`.
    let n = q + k;
    let body = if k <= n && n <= 21 {
        // A whole number, which the branch above already wrote; kept so the
        // layout is ECMAScript's in full.
        format!("{}{}.0", digits, "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        let (whole, fraction) = digits.split_at(n as usize);
        format!("{}.{}", whole, fraction)
    } else if -6 < n && n <= 0 {
        format!("0.{}{}", "0".repeat((-n) as usize), digits)
    } else {
        let e = n - 1;
        let mantissa = if k == 1 {
            digits
        } else {
            format!("{}.{}", &digits[..1], &digits[1..])
        };
        format!("{}e{}{}", mantissa, if e < 0 { '-' } else { '+' }, e.abs())
    };
    format!("{}{}", sign, body)
}

/// The digits ECMAScript chooses for a positive finite `v`, as `(s, q)` with
/// the value `s × 10^q`: the fewest digits that read back as `v`, and of
/// those the closest to it — and when two are exactly as close, the even one.
///
/// Rust's `{:e}` is shortest and closest too, but it breaks that last tie
/// upwards: `1125899906842624.25` lies exactly between `…624.2` and `…624.3`,
/// and Rust wrote `.3` where `Number#toString` writes `.2`. The Wasm glue
/// prints through JavaScript, so the VM and the native runtime printed a
/// different last digit from it, a constant folded here differed from the
/// same interpolation at run time on Wasm, and a derived `hash()` of a float,
/// which hashes its text, differed between backends. So Rust's answer is
/// taken, and then checked against the one case where it can differ.
fn shortest(v: f64) -> (u64, i32) {
    // `{:e}` writes the shortest round-tripping digits as `d.ddde<x>`: at
    // most seventeen of them, which is what lets `s` be a `u64`.
    let scientific = format!("{:e}", v);
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("`{:e}` always writes an exponent");
    let exponent: i32 = exponent.parse().expect("the exponent is an integer");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let s: u64 = digits.parse().expect("at most seventeen digits");
    let q = exponent - (digits.len() as i32 - 1);
    if s % 2 == 0 {
        return (s, q);
    }
    // An odd last digit may have an even neighbour exactly as close to `v`,
    // on either side. It is ECMAScript's answer only if it also reads back
    // as `v`, which at the bottom of a binade — where the gap below a value
    // is half the gap above it — being as close does not guarantee.
    for other in [s - 1, s + 1] {
        let reads_back = format!("{}e{}", other, q).parse::<f64>() == Ok(v);
        if halfway(v, s.min(other), q) && reads_back {
            return (other, q);
        }
    }
    (s, q)
}

/// Whether `v` is exactly halfway between `lo × 10^q` and `(lo + 1) × 10^q`,
/// which is `(2·lo + 1) × 10^q / 2`.
///
/// Compared exactly, in integers. Write `v` as `m × 2^e` with `m` odd and the
/// midpoint as `(2·lo + 1) × 5^q × 2^(q − 1)`: `2·lo + 1` is odd and so is
/// any power of five, so the two are equal only when the powers of two are
/// (`e = q − 1`) and what is left over is too, `m × 5^−q = 2·lo + 1`.
///
/// That needs `q < 0`. Both neighbours read back as `v` only if the gap
/// between floats there is at least `10^q`, and a value whose lowest set bit
/// is `2^(q − 1)` has a gap no wider than that, which is below `10^q` for
/// every `q ≥ 0`: a whole number never sits on a tie. A product that
/// overflows `u128` is far larger than `2·lo + 1`, which is below `2^58`.
fn halfway(v: f64, lo: u64, q: i32) -> bool {
    if q >= 0 {
        return false;
    }
    let bits = v.to_bits();
    let biased = ((bits >> 52) & 0x7FF) as i32;
    let fraction = bits & ((1u64 << 52) - 1);
    let (mut m, mut e) = if biased == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1u64 << 52), biased - 1075)
    };
    let zeros = m.trailing_zeros();
    m >>= zeros;
    e += zeros as i32;
    if e != q - 1 {
        return false;
    }
    let Some(five) = 5u128.checked_pow(q.unsigned_abs()) else {
        return false;
    };
    (m as u128).checked_mul(five) == Some(2 * lo as u128 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every edge the three backends once disagreed about, with the text the
    /// Wasm glue's `showFloat` writes for it — generated by running that
    /// function under Node, so this table is the cross-check between the two
    /// implementations of the rule.
    #[test]
    fn floats_are_written_the_way_the_glue_writes_them() {
        let table: &[(f64, &str)] = &[
            (0.1 + 0.2, "0.30000000000000004"),
            (1.0 / 3.0, "0.3333333333333333"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000.0"),
            (123456789012345680000.0, "123456789012345683968.0"),
            (1e-7, "1e-7"),
            (0.000001, "0.000001"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
            (f64::NAN, "NaN"),
            (-0.0, "-0.0"),
            (0.0, "0.0"),
            (2.0, "2.0"),
            (-2.5, "-2.5"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (1e16, "10000000000000000.0"),
            (1e15 + 0.5, "1000000000000000.5"),
            (9007199254740993.0, "9007199254740992.0"),
            (1.5e-7, "1.5e-7"),
            (123e-20, "1.23e-18"),
            (1e100, "1e+100"),
            (0.5, "0.5"),
            (100.0, "100.0"),
            (9223372036854775808.0, "9223372036854775808.0"),
            (-9223372036854775808.0, "-9223372036854775808.0"),
            (-1e21, "-1e+21"),
            (12345.678, "12345.678"),
            (0.00001, "0.00001"),
            (0.1, "0.1"),
            (25e-8, "2.5e-7"),
            (-1e-7, "-1e-7"),
            (1.0000000000000002, "1.0000000000000002"),
            (4.35, "4.35"),
            (0.000123, "0.000123"),
            (1e-300, "1e-300"),
            (2.2250738585072014e-308, "2.2250738585072014e-308"),
            // Exactly halfway between two shortest decimals: the even one.
            // Written as sums, each exact, because the literal `…624.25` is
            // more digits than a double holds and says so.
            (1125899906842624.0 + 0.25, "1125899906842624.2"),
            (1125899906842625.0 + 0.25, "1125899906842625.2"),
            (1125899906842624.0 + 0.75, "1125899906842624.8"),
            (577411599005501.0 + 0.25, "577411599005501.2"),
            (-577411599005501.0 - 0.25, "-577411599005501.2"),
            (237061009.0 / 8192.0, "28938.111450195312"),
            (237060009.0 / 8192.0, "28937.989379882812"),
        ];
        for (v, want) in table {
            assert_eq!(float_text(*v), *want, "{:?}", v);
        }
    }
}
